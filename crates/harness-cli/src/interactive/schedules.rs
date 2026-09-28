//! Durable scheduled prompts, after prime-agent's `core/cron-jobs.ts`.
//!
//! `/schedule add <when> -- <prompt>` keeps a job in
//! `<data dir>/schedules/<task id>.json`, so it belongs to one conversation and
//! outlives the app. `<when>` takes prime-agent's forms: `in 10m` (once),
//! `every 30s` (at least ten seconds), `at <ISO date>`, a five-field cron
//! expression in local time, or `@hourly`/`@daily`/`@weekly`/`@monthly`. Jobs
//! run only while the app is open, through the same path as heartbeats; runs
//! missed while it was closed collapse into one, and the next run is counted
//! from when it ran.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, Datelike, Local, NaiveDate, NaiveDateTime, TimeZone, Timelike, Utc};
use serde::{Deserialize, Serialize};

use super::heartbeat::{Delivery, Due};

const MIN_INTERVAL_MS: u64 = 10_000;

/// prime-agent's `AgentCronJobStatus`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Active,
    Paused,
    Completed,
    Cancelled,
}

impl Status {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
        }
    }
}

/// prime-agent's `AgentCronScheduleKind`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Once,
    Cron,
    Interval,
}

/// prime-agent's `AgentCronSchedule`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Schedule {
    pub kind: Kind,
    pub expression: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval_ms: Option<u64>,
}

/// prime-agent's `AgentCronJob`, the fields a local job needs.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    pub id: String,
    pub status: Status,
    /// `steer` or `follow_up`.
    pub delivery_mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub prompt: String,
    pub schedule: Schedule,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_run_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    pub run_count: u64,
}

impl Job {
    fn delivery(&self) -> Delivery {
        if self.delivery_mode == "steer" {
            Delivery::Steer
        } else {
            Delivery::FollowUp
        }
    }

    /// prime-agent's `formatAgentCronJob`, with local times.
    fn line(&self) -> String {
        let time = |at: Option<DateTime<Utc>>| {
            at.map_or_else(
                || "-".to_owned(),
                |at| {
                    at.with_timezone(&Local)
                        .format("%Y-%m-%d %H:%M:%S")
                        .to_string()
                },
            )
        };
        let preview = self
            .prompt
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(80)
            .collect::<String>();
        let label = self
            .label
            .as_ref()
            .map(|label| format!(" label=\"{label}\""))
            .unwrap_or_default();
        let error = self
            .last_error
            .as_ref()
            .map(|error| format!(" error={error}"))
            .unwrap_or_default();
        format!(
            "{} {}{label} next={} last={} runs={} schedule=\"{}\" prompt=\"{preview}\"{error}",
            self.id,
            self.status.as_str(),
            time(self.next_run_at),
            time(self.last_run_at),
            self.run_count,
            self.schedule.expression
        )
    }
}

fn strip_matching_quotes(value: &str) -> &str {
    let bytes = value.as_bytes();
    if value.len() >= 2
        && (bytes[0] == b'"' || bytes[0] == b'\'')
        && bytes[value.len() - 1] == bytes[0]
    {
        &value[1..value.len() - 1]
    } else {
        value
    }
}

/// `<digits><unit>` or `<digits> <unit>`, the unit in lower case.
fn amount_and_unit(text: &str) -> Option<(u64, String)> {
    let digits = text
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    let unit = text[digits.len()..].trim().to_ascii_lowercase();
    Some((digits.parse().ok()?, unit))
}

/// prime-agent's `parseAgentCronSchedule`: the schedule and its first run.
pub fn parse_schedule(
    input: &str,
    now: DateTime<Utc>,
) -> Result<(Schedule, DateTime<Utc>), String> {
    let text = strip_matching_quotes(input.trim());
    if text.is_empty() {
        return Err("Cron schedule cannot be empty".to_owned());
    }
    let lowered = text.to_ascii_lowercase();
    if let Some(rest) = lowered.strip_prefix("in ")
        && let Some((amount, unit)) = amount_and_unit(rest.trim())
    {
        let minutes = match unit.as_str() {
            "m" | "min" | "mins" | "minute" | "minutes" => Some(1),
            "h" | "hr" | "hrs" | "hour" | "hours" => Some(60),
            "d" | "day" | "days" => Some(24 * 60),
            _ => None,
        };
        if let Some(minutes) = minutes {
            let delay = chrono::Duration::minutes(
                i64::try_from(amount.saturating_mul(minutes)).unwrap_or(i64::MAX / 60_000),
            );
            return Ok((
                Schedule {
                    kind: Kind::Once,
                    expression: text.to_owned(),
                    interval_ms: None,
                },
                now + delay,
            ));
        }
    }
    let every = lowered
        .strip_prefix("every ")
        .or_else(|| lowered.strip_prefix("each "));
    if let Some(rest) = every
        && let Some((amount, unit)) = amount_and_unit(rest.trim())
    {
        let millis = match unit.as_str() {
            "s" | "sec" | "secs" | "second" | "seconds" => Some(1000),
            "m" | "min" | "mins" | "minute" | "minutes" => Some(60_000),
            "h" | "hr" | "hrs" | "hour" | "hours" => Some(3_600_000),
            _ => None,
        };
        if let Some(millis) = millis {
            let interval_ms = amount.saturating_mul(millis);
            if interval_ms < MIN_INTERVAL_MS {
                return Err("Recurring interval must be at least 10 seconds".to_owned());
            }
            return Ok((
                Schedule {
                    kind: Kind::Interval,
                    expression: text.to_owned(),
                    interval_ms: Some(interval_ms),
                },
                now + chrono::Duration::milliseconds(
                    i64::try_from(interval_ms).unwrap_or(i64::MAX),
                ),
            ));
        }
    }
    if lowered.starts_with("at ") {
        let when = parse_date(text[3..].trim())
            .ok_or_else(|| "Invalid one-shot schedule. Use: at <ISO date>".to_owned())?;
        if when <= now {
            return Err("One-shot schedule must be in the future".to_owned());
        }
        return Ok((
            Schedule {
                kind: Kind::Once,
                expression: text.to_owned(),
                interval_ms: None,
            },
            when,
        ));
    }
    let expression = match text {
        "@hourly" => "0 * * * *",
        "@daily" => "0 0 * * *",
        "@weekly" => "0 0 * * 0",
        "@monthly" => "0 0 1 * *",
        other => other,
    }
    .to_owned();
    let next = next_cron_after(&expression, now)?;
    Ok((
        Schedule {
            kind: Kind::Cron,
            expression,
            interval_ms: None,
        },
        next,
    ))
}

/// A date the way JavaScript's `Date` reads one: with an offset as given,
/// a date and time without one in local time, a bare date at UTC midnight.
fn parse_date(text: &str) -> Option<DateTime<Utc>> {
    if let Ok(when) = DateTime::parse_from_rfc3339(text) {
        return Some(when.with_timezone(&Utc));
    }
    for format in [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%d %H:%M",
    ] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(text, format) {
            return Local
                .from_local_datetime(&naive)
                .earliest()
                .map(|when| when.with_timezone(&Utc));
        }
    }
    NaiveDate::parse_from_str(text, "%Y-%m-%d")
        .ok()
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .map(|naive| Utc.from_utc_datetime(&naive))
}

struct CronFields {
    minute: BTreeSet<u32>,
    hour: BTreeSet<u32>,
    day_of_month: BTreeSet<u32>,
    month: BTreeSet<u32>,
    day_of_week: BTreeSet<u32>,
}

/// prime-agent's `parseCronField`: `*`, `a-b`, `/step`, lists.
fn cron_field(field: &str, min: u32, max: u32) -> Result<BTreeSet<u32>, String> {
    let number = |value: &str, low: u32| -> Result<u32, String> {
        if value.is_empty() || !value.chars().all(|char| char.is_ascii_digit()) {
            return Err(format!("Invalid cron number: {value}"));
        }
        let parsed: u32 = value
            .parse()
            .map_err(|_| format!("Cron number out of range: {value}"))?;
        if parsed < low || parsed > max {
            return Err(format!("Cron number out of range: {value}"));
        }
        Ok(parsed)
    };
    let mut values = BTreeSet::new();
    for part in field.split(',') {
        if part.is_empty() {
            return Err(format!("Invalid cron field: {field}"));
        }
        let (range, step) = match part.split_once('/') {
            Some((range, step)) => (range, number(step, 1)?),
            None => (part, 1),
        };
        let (start, end) = if range == "*" {
            (min, max)
        } else if let Some((start, end)) = range.split_once('-') {
            let (start, end) = (number(start, min)?, number(end, min)?);
            if start > end {
                return Err(format!("Invalid cron range: {range}"));
            }
            (start, end)
        } else {
            let value = number(range, min)?;
            (value, value)
        };
        values.extend((start..=end).step_by(usize::try_from(step).unwrap_or(1)));
    }
    Ok(values)
}

fn cron_fields(expression: &str) -> Result<CronFields, String> {
    let parts = expression.split_whitespace().collect::<Vec<_>>();
    if parts.len() != 5 {
        return Err("Unsupported cron schedule. Use 'in 10m', 'at <ISO date>', @hourly, or five fields: minute hour day month weekday".to_owned());
    }
    Ok(CronFields {
        minute: cron_field(parts[0], 0, 59)?,
        hour: cron_field(parts[1], 0, 23)?,
        day_of_month: cron_field(parts[2], 1, 31)?,
        month: cron_field(parts[3], 1, 12)?,
        day_of_week: cron_field(parts[4], 0, 7)?,
    })
}

/// prime-agent's `nextCronRunAfter`: the first local minute after `after` that
/// matches, looking a year ahead.
pub fn next_cron_after(expression: &str, after: DateTime<Utc>) -> Result<DateTime<Utc>, String> {
    let fields = cron_fields(expression)?;
    let local = after.with_timezone(&Local);
    let mut candidate = local
        .with_second(0)
        .and_then(|time| time.with_nanosecond(0))
        .unwrap_or(local)
        + chrono::Duration::minutes(1);
    let deadline = candidate + chrono::Duration::days(366);
    while candidate <= deadline {
        let day = candidate.weekday().num_days_from_sunday();
        let day_matches =
            fields.day_of_week.contains(&day) || (day == 0 && fields.day_of_week.contains(&7));
        if fields.minute.contains(&candidate.minute())
            && fields.hour.contains(&candidate.hour())
            && fields.day_of_month.contains(&candidate.day())
            && fields.month.contains(&candidate.month())
            && day_matches
        {
            return Ok(candidate.with_timezone(&Utc));
        }
        candidate += chrono::Duration::minutes(1);
    }
    Err(format!(
        "Cron schedule did not match within one year: {expression}"
    ))
}

/// prime-agent's `nextRunAtForSchedule`: none for a one-shot job.
fn next_run(schedule: &Schedule, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
    match schedule.kind {
        Kind::Once => None,
        Kind::Interval => schedule.interval_ms.map(|interval| {
            after + chrono::Duration::milliseconds(i64::try_from(interval).unwrap_or(i64::MAX))
        }),
        Kind::Cron => next_cron_after(&schedule.expression, after).ok(),
    }
}

#[derive(Default)]
struct Inner {
    path: Option<PathBuf>,
    jobs: Vec<Job>,
}

/// The jobs of the conversation the session is in.
#[derive(Default)]
pub struct Schedules {
    inner: Mutex<Inner>,
}

/// Where a conversation's jobs are kept.
#[must_use]
pub fn path_for(data_dir: &Path, task: &str) -> PathBuf {
    data_dir.join("schedules").join(format!("{task}.json"))
}

fn load(path: &Path) -> Vec<Job> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<Vec<Job>>(&text).ok())
        .unwrap_or_default()
}

/// Write the file whole, through a staged copy, so a crash leaves the old one.
fn save(path: &Path, jobs: &[Job]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let text = serde_json::to_string_pretty(jobs).map_err(|error| error.to_string())?;
    let staged = path.with_extension("json.staged");
    std::fs::write(&staged, text).map_err(|error| error.to_string())?;
    std::fs::rename(&staged, path).map_err(|error| error.to_string())
}

impl Schedules {
    /// Follow the conversation whose jobs live at `path`.
    pub fn bind(&self, path: PathBuf) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        if inner.path.as_ref() == Some(&path) {
            return;
        }
        inner.jobs = load(&path);
        inner.path = Some(path);
    }

    /// Change the jobs and write them back.
    fn change<T>(
        &self,
        change: impl FnOnce(&mut Vec<Job>) -> Result<T, String>,
    ) -> Result<T, String> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| "schedules are unavailable".to_owned())?;
        let path = inner
            .path
            .clone()
            .ok_or_else(|| "no conversation to schedule in yet".to_owned())?;
        let result = change(&mut inner.jobs)?;
        save(&path, &inner.jobs)?;
        Ok(result)
    }

    /// Add a job.
    pub fn add(
        &self,
        when: &str,
        prompt: &str,
        delivery: Delivery,
        now: DateTime<Utc>,
    ) -> Result<Job, String> {
        let prompt = prompt.trim();
        if prompt.is_empty() {
            return Err(
                "a scheduled job needs a prompt: /schedule add <when> -- <prompt>".to_owned(),
            );
        }
        let (schedule, next) = parse_schedule(when, now)?;
        self.change(|jobs| {
            let number = jobs
                .iter()
                .filter_map(|job| job.id.strip_prefix("job-")?.parse::<u64>().ok())
                .max()
                .unwrap_or(0)
                + 1;
            let job = Job {
                id: format!("job-{number}"),
                status: Status::Active,
                delivery_mode: match delivery {
                    Delivery::Steer => "steer",
                    Delivery::FollowUp => "follow_up",
                }
                .to_owned(),
                label: None,
                prompt: prompt.to_owned(),
                schedule,
                created_at: now,
                updated_at: now,
                next_run_at: Some(next),
                last_run_at: None,
                last_error: None,
                run_count: 0,
            };
            jobs.push(job.clone());
            Ok(job)
        })
    }

    /// Pause, resume or cancel a job.
    pub fn set_status(&self, id: &str, action: &str, now: DateTime<Utc>) -> Result<Job, String> {
        self.change(|jobs| {
            let job = jobs
                .iter_mut()
                .find(|job| job.id == id)
                .ok_or_else(|| format!("no scheduled job {id}"))?;
            if matches!(job.status, Status::Completed | Status::Cancelled) {
                return Err(format!("{id} is {}", job.status.as_str()));
            }
            match action {
                "pause" => {
                    job.status = Status::Paused;
                    job.next_run_at = None;
                }
                "resume" => {
                    job.status = Status::Active;
                    job.next_run_at = match job.schedule.kind {
                        // A one-shot job that came due while paused runs now.
                        Kind::Once => Some(job.next_run_at.unwrap_or(now).max(now)),
                        _ => next_run(&job.schedule, now),
                    };
                }
                _ => {
                    job.status = Status::Cancelled;
                    job.next_run_at = None;
                }
            }
            job.updated_at = now;
            Ok(job.clone())
        })
    }

    /// One line per job.
    #[must_use]
    pub fn list(&self) -> Vec<String> {
        self.inner
            .lock()
            .map(|inner| inner.jobs.iter().map(Job::line).collect())
            .unwrap_or_default()
    }

    /// The jobs due at `now`, each run once however many runs it missed, with
    /// its next run counted from now, as prime-agent's heartbeat prompt.
    pub fn due(&self, now: DateTime<Utc>) -> Vec<Due> {
        let mut due = Vec::new();
        let Ok(mut inner) = self.inner.lock() else {
            return due;
        };
        for job in inner
            .jobs
            .iter_mut()
            .filter(|job| job.status == Status::Active)
        {
            if job.next_run_at.is_none_or(|next| next > now) {
                continue;
            }
            job.run_count += 1;
            job.last_run_at = Some(now);
            job.updated_at = now;
            job.next_run_at = next_run(&job.schedule, now);
            if job.next_run_at.is_none() {
                job.status = Status::Completed;
            }
            due.push(Due {
                text: format!(
                    "[heartbeat: {} run#{}]\n\n{}",
                    job.schedule.expression.replace(['\n', '[', ']'], " "),
                    job.run_count,
                    job.prompt
                ),
                delivery: job.delivery(),
            });
        }
        if !due.is_empty()
            && let Some(path) = inner.path.clone()
        {
            let _ = save(&path, &inner.jobs);
        }
        due
    }
}

/// `/schedule` and its subcommands.
pub fn command(schedules: &Schedules, argument: Option<&str>) -> Result<Vec<String>, String> {
    const USAGE: &str = "Usage: /schedule [list] | add [--steer|--follow-up] <when> -- <prompt> | pause <id> | resume <id> | cancel <id>";
    let argument = argument.map(str::trim).unwrap_or_default();
    let (verb, rest) = argument
        .split_once(char::is_whitespace)
        .map_or((argument, ""), |(verb, rest)| (verb, rest.trim()));
    let now = Utc::now();
    match verb {
        "" | "list" => {
            let lines = schedules.list();
            Ok(if lines.is_empty() {
                vec!["no scheduled jobs in this conversation; /schedule add <when> -- <prompt> adds one".to_owned()]
            } else {
                lines
            })
        }
        "add" => {
            let (when, prompt) = rest
                .split_once(" -- ")
                .or_else(|| rest.strip_suffix(" --").map(|when| (when, "")))
                .ok_or_else(|| USAGE.to_owned())?;
            let mut delivery = Delivery::FollowUp;
            let mut when = when.trim();
            loop {
                if let Some(rest) = when.strip_prefix("--steer") {
                    delivery = Delivery::Steer;
                    when = rest.trim_start();
                } else if let Some(rest) = when
                    .strip_prefix("--follow-up")
                    .or_else(|| when.strip_prefix("--follow_up"))
                {
                    delivery = Delivery::FollowUp;
                    when = rest.trim_start();
                } else {
                    break;
                }
            }
            let job = schedules.add(when, prompt, delivery, now)?;
            Ok(vec![format!("scheduled {}", job.line())])
        }
        "pause" | "resume" | "cancel" | "delete" if !rest.is_empty() => {
            let action = if verb == "delete" { "cancel" } else { verb };
            let job = schedules.set_status(rest, action, now)?;
            Ok(vec![job.line()])
        }
        _ => Err(USAGE.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::{Kind, Schedules, Status, command, next_cron_after, parse_schedule, path_for};
    use crate::interactive::heartbeat::Delivery;
    use chrono::{Local, TimeZone, Timelike, Utc};

    #[test]
    fn q15_parser_accepts_prime_forms() {
        let now = Utc.with_ymd_and_hms(2026, 9, 28, 8, 0, 0).unwrap();
        let (once, next) = parse_schedule("in 10m", now).unwrap();
        assert_eq!(once.kind, Kind::Once);
        assert_eq!(next, now + chrono::Duration::minutes(10));
        assert_eq!(
            parse_schedule("IN 2 days", now).unwrap().1,
            now + chrono::Duration::days(2)
        );
        let (every, next) = parse_schedule("every 30s", now).unwrap();
        assert_eq!(
            (every.kind, every.interval_ms),
            (Kind::Interval, Some(30_000))
        );
        assert_eq!(next, now + chrono::Duration::seconds(30));
        assert_eq!(
            parse_schedule("each 2 hours", now).unwrap().0.interval_ms,
            Some(7_200_000)
        );
        let (at, next) = parse_schedule("at 2030-01-01T09:00:00Z", now).unwrap();
        assert_eq!(at.kind, Kind::Once);
        assert_eq!(next, Utc.with_ymd_and_hms(2030, 1, 1, 9, 0, 0).unwrap());
        let (cron, _) = parse_schedule("0 9 * * 1-5", now).unwrap();
        assert_eq!(cron.kind, Kind::Cron);
        assert_eq!(
            parse_schedule("@daily", now).unwrap().0.expression,
            "0 0 * * *"
        );
        assert_eq!(
            parse_schedule("\"every 1m\"", now).unwrap().0.interval_ms,
            Some(60_000)
        );
        // The wrong forms, with prime-agent's reasons.
        assert_eq!(
            parse_schedule("every 5s", now).unwrap_err(),
            "Recurring interval must be at least 10 seconds"
        );
        assert_eq!(
            parse_schedule("at 2020-01-01T00:00:00Z", now).unwrap_err(),
            "One-shot schedule must be in the future"
        );
        assert!(
            parse_schedule("at soon", now)
                .unwrap_err()
                .starts_with("Invalid one-shot schedule")
        );
        assert!(
            parse_schedule("in 5s", now)
                .unwrap_err()
                .starts_with("Unsupported cron schedule"),
            "`in` takes minutes, hours or days"
        );
        assert!(
            parse_schedule("61 * * * *", now)
                .unwrap_err()
                .starts_with("Cron number out of range")
        );
        assert!(parse_schedule("", now).is_err());
    }

    #[test]
    fn q15_cron_next_run_is_local_time() {
        let after = Local
            .with_ymd_and_hms(2026, 9, 28, 8, 30, 15)
            .unwrap()
            .with_timezone(&Utc);
        let next = next_cron_after("0 9 * * *", after)
            .unwrap()
            .with_timezone(&Local);
        assert_eq!((next.hour(), next.minute()), (9, 0));
        assert_eq!(next.date_naive(), after.with_timezone(&Local).date_naive());
        // Sunday is 0 or 7.
        let sunday = next_cron_after("0 12 * * 7", after)
            .unwrap()
            .with_timezone(&Local);
        assert_eq!(chrono::Datelike::weekday(&sunday), chrono::Weekday::Sun);
        let every = next_cron_after("*/15 * * * *", after)
            .unwrap()
            .with_timezone(&Local);
        assert_eq!((every.hour(), every.minute()), (8, 45));
    }

    #[test]
    fn q15_missed_runs_collapse_into_one() {
        let dir = tempfile::tempdir().unwrap();
        let schedules = Schedules::default();
        schedules.bind(path_for(dir.path(), "task-a"));
        let start = Utc.with_ymd_and_hms(2026, 9, 28, 8, 0, 0).unwrap();
        schedules
            .add("every 1m", "check", Delivery::FollowUp, start)
            .unwrap();
        // Closed for an hour: sixty runs missed, one delivered.
        let later = start + chrono::Duration::hours(1);
        let due = schedules.due(later);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].text, "[heartbeat: every 1m run#1]\n\ncheck");
        assert_eq!(due[0].delivery, Delivery::FollowUp);
        assert!(schedules.due(later).is_empty());
        // The next run is counted from when it ran.
        assert_eq!(
            schedules.due(later + chrono::Duration::seconds(59)).len(),
            0
        );
        assert_eq!(schedules.due(later + chrono::Duration::minutes(1)).len(), 1);
        // A one-shot job completes.
        schedules
            .add("in 1m", "once", Delivery::Steer, start)
            .unwrap();
        let due = schedules.due(later + chrono::Duration::minutes(5));
        assert!(due.iter().any(|due| due.delivery == Delivery::Steer));
        assert!(
            schedules
                .list()
                .iter()
                .any(|line| line.starts_with("job-2 completed"))
        );
    }

    #[test]
    fn q15_jobs_survive_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = path_for(dir.path(), "task-a");
        let first = Schedules::default();
        first.bind(path.clone());
        let lines = command(&first, Some("add --steer every 10m -- look at the build")).unwrap();
        assert!(lines[0].starts_with("scheduled job-1 active"), "{lines:?}");
        command(&first, Some("add 0 9 * * 1-5 -- standup")).unwrap();
        command(&first, Some("pause job-2")).unwrap();
        drop(first);
        // A new app reads the same conversation's jobs back.
        let second = Schedules::default();
        second.bind(path);
        let listed = command(&second, Some("list")).unwrap();
        assert_eq!(listed.len(), 2);
        assert!(listed[0].contains("schedule=\"every 10m\" prompt=\"look at the build\""));
        assert!(listed[1].starts_with("job-2 paused"));
        // Another conversation has its own.
        let other = Schedules::default();
        other.bind(path_for(dir.path(), "task-b"));
        assert!(command(&other, None).unwrap()[0].starts_with("no scheduled jobs"));
        assert!(
            command(&second, Some("cancel job-9"))
                .unwrap_err()
                .contains("no scheduled job job-9")
        );
        assert!(
            command(&second, Some("add every 1m"))
                .unwrap_err()
                .starts_with("Usage: /schedule")
        );
        let cancelled = command(&second, Some("cancel job-1")).unwrap();
        assert!(cancelled[0].starts_with("job-1 cancelled"));
        let _ = Status::Active;
    }
}
