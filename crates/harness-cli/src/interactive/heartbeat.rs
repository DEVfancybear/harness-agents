//! Agent-owned recurring prompts: prime-agent's RLM heartbeats.
//!
//! Ported from prime-agent's `core/cron-jobs.ts` and `handleRlmHeartbeatHostRequest`.
//! The `rlm_heartbeat` skill creates, lists, updates and deletes heartbeats for the
//! current session; each one re-sends its instruction on its interval as
//! `[heartbeat: every 5m run#N]`. A `steer` heartbeat reaches a running turn at once,
//! through the same inbox `/steer` uses; a `follow_up` one waits for the turn to end.
//! They live as long as the app session, like prime-agent's session-internal ones.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// prime-agent's `DEFAULT_HEARTBEAT_SCHEDULE`.
pub const DEFAULT_SCHEDULE: &str = "every 5m";
const MIN_INTERVAL: Duration = Duration::from_secs(10);

/// How a due heartbeat reaches the session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Delivery {
    /// Interrupt the running turn so the heartbeat runs promptly (the default).
    Steer,
    /// Wait for the running turn to finish.
    FollowUp,
}

impl Delivery {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Steer => "steer",
            Self::FollowUp => "follow_up",
        }
    }

    /// `normalizeHeartbeatDeliveryMode`.
    fn parse(value: &Value) -> Result<Option<Self>, String> {
        match value {
            Value::Null => Ok(None),
            Value::String(text) if text == "steer" => Ok(Some(Self::Steer)),
            Value::String(text) if text == "follow_up" => Ok(Some(Self::FollowUp)),
            _ => Err("Heartbeat delivery mode must be \"steer\" or \"follow_up\"".to_owned()),
        }
    }
}

/// A heartbeat whose time has come.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Due {
    pub text: String,
    pub delivery: Delivery,
}

#[derive(Clone, Debug)]
struct Heartbeat {
    id: String,
    paused: bool,
    label: Option<String>,
    delivery: Delivery,
    instruction: String,
    expression: String,
    interval: Duration,
    created_at: String,
    updated_at: String,
    next_run: Instant,
    last_run_at: Option<String>,
    run_count: u64,
}

impl Heartbeat {
    /// `rlmHeartbeatHostResponse`.
    fn response(&self) -> Value {
        let next = self
            .next_run
            .saturating_duration_since(Instant::now())
            .as_secs();
        json!({
            "id": self.id,
            "status": if self.paused { "paused" } else { "active" },
            "label": self.label,
            "delivery_mode": self.delivery.as_str(),
            "instruction": self.instruction,
            "schedule": {
                "kind": "interval",
                "expression": self.expression,
                "intervalMs": u64::try_from(self.interval.as_millis()).unwrap_or(u64::MAX),
            },
            "created_at": self.created_at,
            "updated_at": self.updated_at,
            "next_run_at": if self.paused { Value::Null } else { json!(format!("in {next}s")) },
            "last_run_at": self.last_run_at,
            "last_error": null,
            "run_count": self.run_count,
        })
    }
}

fn now_label() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    format!("@{seconds}")
}

/// `normalizeHeartbeatSchedule` then `parseAgentCronSchedule` for the recurring
/// forms: `5m` means `every 5m`; `every|each <n> <unit>` with seconds, minutes or
/// hours; at least ten seconds. Cron expressions and one-off `in ...` schedules are
/// refused: a heartbeat recurs, and ha has no cron parser.
pub fn parse_schedule(input: Option<&str>) -> Result<(String, Duration), String> {
    let text = input.map(str::trim).filter(|text| !text.is_empty());
    let text = match text {
        None => DEFAULT_SCHEDULE.to_owned(),
        Some(text)
            if text
                .chars()
                .next()
                .is_some_and(|first| first.is_ascii_digit()) =>
        {
            format!("every {text}")
        }
        Some(text) => text.to_owned(),
    };
    let lowered = text.to_ascii_lowercase();
    let rest = lowered
        .strip_prefix("every ")
        .or_else(|| lowered.strip_prefix("each "))
        .ok_or_else(|| {
            format!("RLM heartbeat schedule must be recurring, like \"every 5m\"; got {text:?}")
        })?
        .trim();
    let digits = rest
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    let unit = rest[digits.len()..].trim();
    let amount: u64 = digits
        .parse()
        .map_err(|_| format!("unrecognized heartbeat interval {text:?}"))?;
    let seconds = match unit {
        "s" | "sec" | "secs" | "second" | "seconds" => 1,
        "m" | "min" | "mins" | "minute" | "minutes" => 60,
        "h" | "hr" | "hrs" | "hour" | "hours" => 3600,
        _ => return Err(format!("unrecognized heartbeat interval unit in {text:?}")),
    };
    let interval = Duration::from_secs(amount.saturating_mul(seconds));
    if interval < MIN_INTERVAL {
        return Err("Recurring interval must be at least 10 seconds".to_owned());
    }
    Ok((text, interval))
}

impl super::repl::HostRequests for Heartbeats {
    fn handle<'a>(&'a self, request: &'a Value) -> super::repl::HostReply<'a> {
        Box::pin(async move {
            let kind = request["type"].as_str().unwrap_or_default();
            kind.starts_with("rlm_heartbeat.")
                .then(|| Heartbeats::handle(self, kind, request))
        })
    }
}

/// The heartbeats of one app session.
#[derive(Default)]
pub struct Heartbeats {
    items: Mutex<Vec<Heartbeat>>,
    next_id: Mutex<u64>,
}

impl Heartbeats {
    fn id(&self) -> String {
        let mut next = self
            .next_id
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *next += 1;
        format!("heartbeat-{next}")
    }

    fn optional_text(
        request: &Value,
        field: &str,
        operation: &str,
    ) -> Result<Option<String>, String> {
        match &request[field] {
            Value::Null => Ok(None),
            Value::String(text) => Ok(Some(text.clone())),
            _ => Err(format!(
                "rlm_heartbeat.{operation} {field} must be a string when provided"
            )),
        }
    }

    /// Answer one `rlm_heartbeat.*` request.
    #[allow(
        clippy::too_many_lines,
        reason = "one arm per request type, as prime-agent handles them"
    )]
    pub fn handle(&self, kind: &str, request: &Value) -> Result<Value, String> {
        let mut items = self
            .items
            .lock()
            .map_err(|_| "heartbeats are unavailable".to_owned())?;
        match kind {
            "rlm_heartbeat.list" => {
                // Deleted heartbeats are removed, so "inactive" has nothing more to add.
                Ok(
                    json!({ "heartbeats": items.iter().map(Heartbeat::response).collect::<Vec<_>>() }),
                )
            }
            "rlm_heartbeat.create" => {
                let instruction = request["instruction"]
                    .as_str()
                    .ok_or("rlm_heartbeat.create instruction must be a string")?
                    .trim()
                    .to_owned();
                if instruction.is_empty() {
                    return Err("RLM heartbeat instruction cannot be empty".to_owned());
                }
                let (expression, interval) =
                    parse_schedule(Self::optional_text(request, "interval", "create")?.as_deref())?;
                let label = Self::optional_text(request, "label", "create")?
                    .map(|label| label.trim().to_owned())
                    .filter(|label| !label.is_empty());
                let delivery =
                    Delivery::parse(&request["delivery_mode"])?.unwrap_or(Delivery::Steer);
                drop(items);
                let id = self.id();
                let mut items = self
                    .items
                    .lock()
                    .map_err(|_| "heartbeats are unavailable".to_owned())?;
                let now = now_label();
                let heartbeat = Heartbeat {
                    id,
                    paused: false,
                    label,
                    delivery,
                    instruction,
                    expression,
                    interval,
                    created_at: now.clone(),
                    updated_at: now,
                    next_run: Instant::now() + interval,
                    last_run_at: None,
                    run_count: 0,
                };
                let response = heartbeat.response();
                items.push(heartbeat);
                Ok(json!({ "heartbeat": response }))
            }
            "rlm_heartbeat.update" => {
                let id = request["id"]
                    .as_str()
                    .ok_or("rlm_heartbeat.update id must be a string")?;
                let instruction = Self::optional_text(request, "instruction", "update")?;
                let interval = Self::optional_text(request, "interval", "update")?;
                let label = Self::optional_text(request, "label", "update")?;
                let status = match &request["status"] {
                    Value::Null => None,
                    Value::String(status) if status == "pause" || status == "resume" => {
                        Some(status.clone())
                    }
                    _ => return Err(
                        "rlm_heartbeat.update status must be \"pause\" or \"resume\" when provided"
                            .to_owned(),
                    ),
                };
                let delivery = Delivery::parse(&request["delivery_mode"])?;
                if instruction.is_none()
                    && interval.is_none()
                    && label.is_none()
                    && status.is_none()
                    && delivery.is_none()
                {
                    return Err(
                        "rlm_heartbeat.update requires at least one field to update".to_owned()
                    );
                }
                let Some(heartbeat) = items.iter_mut().find(|heartbeat| heartbeat.id == id) else {
                    return Ok(json!({ "heartbeat": null }));
                };
                if let Some(instruction) = instruction.map(|text| text.trim().to_owned()) {
                    if instruction.is_empty() {
                        return Err("RLM heartbeat instruction cannot be empty".to_owned());
                    }
                    heartbeat.instruction = instruction;
                }
                if let Some(interval) = interval {
                    let (expression, every) = parse_schedule(Some(&interval))?;
                    heartbeat.expression = expression;
                    heartbeat.interval = every;
                    heartbeat.next_run = Instant::now() + every;
                }
                if let Some(label) = label {
                    heartbeat.label =
                        Some(label.trim().to_owned()).filter(|label| !label.is_empty());
                }
                if let Some(delivery) = delivery {
                    heartbeat.delivery = delivery;
                }
                match status.as_deref() {
                    Some("pause") => heartbeat.paused = true,
                    Some("resume") => {
                        heartbeat.paused = false;
                        heartbeat.next_run = Instant::now() + heartbeat.interval;
                    }
                    _ => {}
                }
                heartbeat.updated_at = now_label();
                Ok(json!({ "heartbeat": heartbeat.response() }))
            }
            "rlm_heartbeat.delete" => {
                let id = request["id"]
                    .as_str()
                    .ok_or("rlm_heartbeat.delete id must be a string")?;
                let removed = items
                    .iter()
                    .position(|heartbeat| heartbeat.id == id)
                    .map(|index| items.remove(index));
                Ok(json!({ "heartbeat": removed.map(|heartbeat| heartbeat.response()) }))
            }
            _ => Err(format!("unknown RLM heartbeat request type \"{kind}\"")),
        }
    }

    /// The heartbeats due at `now`, each advanced to its next run, as the message
    /// prime-agent's `createHeartbeatPromptMessage` writes.
    pub fn due(&self, now: Instant) -> Vec<Due> {
        let Ok(mut items) = self.items.lock() else {
            return Vec::new();
        };
        let mut due = Vec::new();
        for heartbeat in items.iter_mut().filter(|heartbeat| !heartbeat.paused) {
            if heartbeat.next_run > now {
                continue;
            }
            heartbeat.run_count += 1;
            heartbeat.last_run_at = Some(now_label());
            // A session that was busy for several intervals runs once, not once per miss.
            heartbeat.next_run = now + heartbeat.interval;
            due.push(Due {
                text: format!(
                    "[heartbeat: {} run#{}]\n\n{}",
                    heartbeat.expression.replace(['\n', '[', ']'], " "),
                    heartbeat.run_count,
                    heartbeat.instruction
                ),
                delivery: heartbeat.delivery,
            });
        }
        due
    }
}

#[cfg(test)]
mod tests {
    use super::{Delivery, Heartbeats, parse_schedule};
    use serde_json::json;
    use std::time::{Duration, Instant};

    #[test]
    fn schedules_follow_prime_agent() {
        assert_eq!(
            parse_schedule(None).expect("default").1,
            Duration::from_mins(5)
        );
        assert_eq!(parse_schedule(Some("5m")).expect("bare").0, "every 5m");
        assert_eq!(
            parse_schedule(Some("each 2 hours")).expect("each").1,
            Duration::from_hours(2)
        );
        assert!(
            parse_schedule(Some("every 5s")).is_err(),
            "at least ten seconds"
        );
        assert!(parse_schedule(Some("in 5m")).is_err(), "a heartbeat recurs");
    }

    #[test]
    fn heartbeats_are_managed_and_come_due() {
        let heartbeats = Heartbeats::default();
        let created = heartbeats
            .handle(
                "rlm_heartbeat.create",
                &json!({"instruction": "check the tests", "interval": "10s", "label": "tests", "delivery_mode": "follow_up"}),
            )
            .expect("created");
        let id = created["heartbeat"]["id"].as_str().expect("id").to_owned();
        assert_eq!(created["heartbeat"]["delivery_mode"], "follow_up");
        assert!(heartbeats.due(Instant::now()).is_empty(), "not due yet");
        let later = Instant::now() + Duration::from_secs(11);
        let due = heartbeats.due(later);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].delivery, Delivery::FollowUp);
        assert!(
            due[0]
                .text
                .starts_with("[heartbeat: every 10s run#1]\n\ncheck the tests"),
            "{}",
            due[0].text
        );
        assert!(heartbeats.due(later).is_empty(), "advanced to the next run");

        heartbeats
            .handle(
                "rlm_heartbeat.update",
                &json!({"id": id, "status": "pause"}),
            )
            .expect("paused");
        assert!(heartbeats.due(later + Duration::from_mins(1)).is_empty());
        let listed = heartbeats
            .handle("rlm_heartbeat.list", &json!({}))
            .expect("list");
        assert_eq!(listed["heartbeats"][0]["status"], "paused");
        assert!(
            heartbeats
                .handle("rlm_heartbeat.update", &json!({"id": id}))
                .is_err(),
            "an update changes something"
        );
        let deleted = heartbeats
            .handle("rlm_heartbeat.delete", &json!({"id": id}))
            .expect("deleted");
        assert_eq!(deleted["heartbeat"]["id"], id);
        let listed = heartbeats
            .handle("rlm_heartbeat.list", &json!({}))
            .expect("list");
        assert_eq!(listed["heartbeats"], json!([]));
    }
}
