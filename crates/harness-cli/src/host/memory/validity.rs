//! When an entry holds and where its words came from (0.9.0,
//! `docs/architecture/MEMORY_STORE.en.md` §16).
//!
//! The fields 0.9.0 adds to an entry's document are all named here:
//!
//! - `origin.kind`: where the entry's words came from, as the host judged it
//!   (never the model): `user`, `tool`, `web`, `file`, `agent`, `child` or
//!   `review` ([`ORIGIN_KINDS`]). An entry written before reads as `agent`.
//! - `supersedes`: the memory ids of the entries this one corrected or
//!   replaced.
//! - `superseded_by`: on an entry a later one replaced, that one's memory id.
//! - `expired`: why the expiry pass closed it; `unused` is the one reason.
//!
//! An entry's validity is its `temporal` value: a period `[start, end)`, or a
//! point it holds from. Without one it holds from its creation on. A
//! correction (`supersedes`) or the expiry pass closes an entry by giving its
//! period an end, now: nothing is deleted, the digest stops showing it, and
//! search reads it when asked about that time (`as_of`, `include_ended`).

use std::path::Path;

use serde_json::{Value, json};

use super::memory_store;

/// The field of a new entry naming the entries it closes.
pub const SUPERSEDES: &str = "supersedes";
/// The field of a closed entry naming the one that replaced it.
pub const SUPERSEDED_BY: &str = "superseded_by";
/// The field of an entry the expiry pass closed, saying why.
pub const EXPIRED: &str = "expired";
/// The reason the expiry pass gives: nobody read or found the entry.
pub const UNUSED: &str = "unused";
/// Where an entry's words came from: the user's turns, a tool's result, a
/// fetched web page, a file read, the agent itself, a delegated child, or a
/// review that inferred it.
pub const ORIGIN_KINDS: [&str; 7] = ["user", "tool", "web", "file", "agent", "child", "review"];
/// What an entry without a kind reads as.
pub const AGENT: &str = "agent";
/// The variable that sets after how many days unused memory is closed.
pub const EXPIRE_VARIABLE: &str = "HA_MEMORY_EXPIRE_DAYS";
/// Unused this long, an entry is closed, unless [`EXPIRE_VARIABLE`] says
/// otherwise (Copilot drops memory unused for 28 days).
pub const DEFAULT_EXPIRE_DAYS: f64 = 28.0;
/// The longest period [`EXPIRE_VARIABLE`] sets, a century: a longer one
/// would not fit the times the store keeps.
pub const MAX_EXPIRE_DAYS: f64 = 36_500.0;
/// The expiry pass runs at most this often (or as often as its period, when
/// that is shorter, so a test can set seconds).
const EXPIRY_EVERY_SECONDS: f64 = 86_400.0;
/// The store meta key the last pass is recorded under.
pub const EXPIRY_META: &str = "expiry";
/// The store meta key of when a store's first pass ran: no entry's disuse
/// is counted from before it.
pub(crate) const EXPIRY_SINCE: &str = "expiry_since";

/// What ranking and the digest read of these fields beside an entry's words.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Standing {
    /// Its `origin.kind` ([`ORIGIN_KINDS`]).
    pub kind: &'static str,
    /// The memory id of the entry that replaced it.
    pub superseded_by: Option<String>,
    /// Why the expiry pass closed it.
    pub expired: Option<String>,
}

impl Standing {
    /// The standing an entry's document records.
    #[must_use]
    pub fn of(doc: &Value) -> Self {
        Self {
            kind: origin_kind(doc),
            superseded_by: doc[SUPERSEDED_BY].as_str().map(str::to_owned),
            expired: doc[EXPIRED].as_str().map(str::to_owned),
        }
    }

    /// Shown as unverified, and ranked below an otherwise equal entry.
    #[must_use]
    pub fn unverified(&self) -> bool {
        unverified(self.kind)
    }

    /// A correction or the expiry pass closed it.
    #[must_use]
    pub const fn closed(&self) -> bool {
        self.superseded_by.is_some() || self.expired.is_some()
    }
}

/// An entry's kind of origin: its `origin.kind`, or [`AGENT`].
#[must_use]
pub fn origin_kind(doc: &Value) -> &'static str {
    let kind = doc["origin"]["kind"].as_str().unwrap_or(AGENT);
    ORIGIN_KINDS
        .iter()
        .copied()
        .find(|known| *known == kind)
        .unwrap_or(AGENT)
}

/// Whether entries of `kind` are shown as unverified and rank below an
/// otherwise equal entry: a fetched page and a delegated child's report are
/// not what the user said or what the agent checked.
#[must_use]
pub fn unverified(kind: &str) -> bool {
    matches!(kind, "web" | "child")
}

/// The grouping an entry is in decides whether a web page may set it: a
/// constraint (`constraints`, `constraints/<topic>`) or the user's
/// preferences (`user/preferences` and below) steer every later turn.
#[must_use]
pub fn steering_path(path: &str) -> bool {
    let labels = path
        .split('/')
        .map(str::trim)
        .filter(|label| !label.is_empty())
        .collect::<Vec<_>>();
    labels
        .first()
        .is_some_and(|first| first.eq_ignore_ascii_case(super::harness::CONSTRAINTS_PATH))
        || (labels.len() >= 2
            && labels[0].eq_ignore_ascii_case("user")
            && labels[1].eq_ignore_ascii_case("preferences"))
}

/// Why an entry of origin `kind` may not be kept at `path`, when it may not.
#[must_use]
pub fn steering_refusal(kind: &str, path: &str) -> Option<String> {
    (kind == "web" && steering_path(path)).then(|| {
        format!(
            "{path} holds rules every later turn follows, and these words came from a fetched web page, not from the user: a page cannot set a constraint or a user preference. Keep it as a fact under another path, or ask the user to state the rule in their own words"
        )
    })
}

/// Whether a correction or the expiry pass closed the entry.
#[must_use]
pub fn closed(doc: &Value) -> bool {
    doc[SUPERSEDED_BY].is_string() || doc[EXPIRED].is_string()
}

/// When the entry was created, as the store keeps times.
fn created(doc: &Value) -> Option<String> {
    doc["created_at"]
        .as_str()
        .and_then(|time| memory_store::instant(time).ok())
}

/// The entry's validity `[start, end)` as store times: its temporal period,
/// else from its point on, else from its creation on. `None` is unbounded.
#[must_use]
pub fn validity(doc: &Value) -> (Option<String>, Option<String>) {
    match memory_store::temporal_bounds(doc.get("temporal").unwrap_or(&Value::Null)) {
        Ok(Some((start, end))) if end > start => (Some(start), Some(end)),
        Ok(Some((start, _))) => (Some(start), None),
        _ => (created(doc), None),
    }
}

/// Whether the entry held at `at` (a store time): its validity covers it,
/// and it had been written by then.
#[must_use]
pub fn valid_at(doc: &Value, at: &str) -> bool {
    let (start, end) = validity(doc);
    created(doc).is_none_or(|created| created.as_str() <= at)
        && start.is_none_or(|start| start.as_str() <= at)
        && end.is_none_or(|end| at < end.as_str())
}

/// Close an entry's validity at `at` (a store time): its period ends then,
/// from the start it had (its creation, when it had none). An end that would
/// not come after the start is the start's next millisecond, so the period
/// is never read as a point.
pub fn close(doc: &mut Value, at: &str) {
    let start = validity(doc).0.unwrap_or_else(|| at.to_owned());
    let end = if at > start.as_str() {
        at.to_owned()
    } else {
        chrono::DateTime::parse_from_rfc3339(&start).map_or_else(
            |_| at.to_owned(),
            |start| {
                (start.with_timezone(&chrono::Utc) + chrono::TimeDelta::milliseconds(1))
                    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
            },
        )
    };
    doc["temporal"] = json!({"start": start, "end": end});
}

/// Close an entry a later one replaced: valid until `at` (a store time),
/// `superseded_by` the memory id `by`, as its next version.
pub fn supersede(doc: &mut Value, by: &str, at: &str) {
    close(doc, at);
    doc[SUPERSEDED_BY] = json!(by);
    doc["updated_at"] = json!(at);
    doc["version"] = json!(doc["version"].as_i64().unwrap_or(1) + 1);
}

/// What a new entry that replaces the entries with the memory ids `closed`
/// records: their ids, and that it holds from now unless it says from when.
pub fn superseding(doc: &mut Value, closed: &[String], at: &str) {
    doc[SUPERSEDES] = json!(closed);
    if doc.get("temporal").is_none_or(Value::is_null) {
        doc["temporal"] = json!({"start": at});
    }
}

/// A review's `supersede` edit on the document `state` of the store at
/// `dir`: a new entry of `kind` from the edit's title, content, path and
/// metadata, written by `origin`, that replaces the entry `old` - which is
/// closed, not deleted ([`supersede`]). The new entry is keyed by its title,
/// next to its old entry's place and project. Returns the record of the new
/// entry as an applied edit (a rollback deletes it) and the memory id the
/// save gives it (`Writer::reviving`), which the closed entry names.
///
/// # Errors
/// The old entry is not there, was superseded already, or has no memory id.
pub fn supersede_edit(
    dir: &Path,
    state: &mut Value,
    kind: &str,
    old: &str,
    edit: &serde_json::Map<String, Value>,
    origin: &Value,
) -> Result<(Value, String), String> {
    let before = state["entries"][kind][old].clone();
    if !before.is_object() {
        return Err("entry not found".to_owned());
    }
    if let Some(by) = before[SUPERSEDED_BY].as_str() {
        return Err(format!("entry was superseded already (by {by})"));
    }
    let closed = memory_store::ids(dir)?
        .get(&(kind.to_owned(), old.to_owned()))
        .cloned()
        .ok_or("the entry has no memory id yet")?;
    let text = |field: &str| edit.get(field).and_then(Value::as_str).unwrap_or_default();
    let slug = super::refine::slug(text("title"), kind);
    let taken = |key: &str| state["entries"][kind][key].is_object();
    let key = std::iter::once(slug.clone())
        .chain((2..=100_000).map(|number| format!("{slug}-{number}")))
        .find(|key| !taken(key))
        .unwrap_or_else(|| format!("{slug}-{}", uuid::Uuid::now_v7().simple()));
    let at = memory_store::now();
    let mut after = json!({
        "id": key,
        "kind": kind,
        "title": text("title"),
        "content": text("content"),
        "path": edit.get("path").filter(|path| path.is_string()).cloned()
            .unwrap_or_else(|| before.get("path").cloned().unwrap_or_else(|| json!("general"))),
        "scope": before.get("scope").cloned().unwrap_or_else(|| json!("global")),
        "reference": {},
        "arguments": {},
        "metadata": edit.get("metadata").filter(|meta| meta.is_object()).cloned().unwrap_or_else(|| json!({})),
        "source": "refine",
        "origin": origin,
        "created_at": at,
        "updated_at": at,
        "version": 1,
    });
    if let Some(project) = before.get("project").filter(|project| !project.is_null()) {
        after["project"] = project.clone();
    }
    // The correction holds where the old fact did: kept for the same files,
    // else it would join every turn. The code the old fact cited is not
    // what the new one rests on, and a review cannot cite: none is kept.
    if let Some(paths) = before
        .get(super::memory_citations::PATHS)
        .filter(|paths| paths.is_array())
    {
        after[super::memory_citations::PATHS] = paths.clone();
    }
    let memory = uuid::Uuid::now_v7().to_string();
    superseding(&mut after, std::slice::from_ref(&closed), &at);
    state["entries"][kind][&key] = after.clone();
    supersede(&mut state["entries"][kind][old], &memory, &at);
    let mut record = json!({
        "action": "create", "kind": kind, "id": key, "after": after, "applied": true,
        "supersedes": old,
    });
    if let Some(reason) = edit.get("reason") {
        record["reason"] = reason.clone();
    }
    Ok((record, memory))
}

/// Open again an entry a correction or the expiry pass closed: its
/// `superseded_by` and `expired` go, and its period the end they gave it.
pub fn reopen(doc: &mut Value, before: &Value) {
    if let Some(fields) = doc.as_object_mut() {
        fields.remove(SUPERSEDED_BY);
        fields.remove(EXPIRED);
        match before.get("temporal") {
            Some(temporal) if !temporal.is_null() => {
                fields.insert("temporal".to_owned(), temporal.clone());
            }
            _ => {
                fields.remove("temporal");
            }
        }
    }
}

/// A time a search names, as a window of store times: `as_of` is a point;
/// `since`/`until` and the temporal filter's modes are a window, open where
/// they give no bound.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Window {
    pub from: Option<String>,
    pub to: Option<String>,
}

impl Window {
    /// The window narrowed to `other`.
    #[must_use]
    pub fn and(self, other: &Self) -> Self {
        Self {
            from: self.from.max(other.from.clone()),
            to: match (self.to, &other.to) {
                (Some(a), Some(b)) => Some(a.min(b.clone())),
                (a, b) => a.or_else(|| b.clone()),
            },
        }
    }
}

/// Whether an entry held at some time in a window: its validity and the
/// window share a moment (a point window: it held then).
#[must_use]
pub fn overlaps(doc: &Value, window: &Window) -> bool {
    if window.from.is_some() && window.from == window.to {
        return window.from.as_deref().is_some_and(|at| valid_at(doc, at));
    }
    let (start, end) = validity(doc);
    let starts_in_time = match (&start, &window.to) {
        (Some(start), Some(to)) => start < to,
        _ => true,
    };
    let lasts_into_it = match (&end, &window.from) {
        (Some(end), Some(from)) => end > from,
        _ => true,
    };
    starts_in_time && lasts_into_it
}

/// Milliseconds since the epoch of a store time.
fn millis(time: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(time)
        .ok()
        .map(|time| time.timestamp_millis())
}

/// How well an entry's validity matches a window, from 0 to 1: for a point,
/// whether it held then; for a period, the share of the window it held.
/// What a search that names a time ranks by before the words.
#[must_use]
#[allow(clippy::cast_precision_loss, reason = "spans in milliseconds")]
pub fn fit(doc: &Value, window: &Window, now: i64) -> f64 {
    if window.from.is_some() && window.from == window.to {
        return f64::from(u8::from(
            window.from.as_deref().is_some_and(|at| valid_at(doc, at)),
        ));
    }
    let (start, end) = validity(doc);
    let start = start.as_deref().and_then(millis).unwrap_or(i64::MIN);
    let end = end.as_deref().and_then(millis).unwrap_or(i64::MAX);
    let to = window.to.as_deref().and_then(millis).unwrap_or(now);
    // A window open at its start has no share to measure: it fits when the
    // entry held at some time in it.
    let Some(from) = window.from.as_deref().and_then(millis) else {
        return f64::from(u8::from(start < to));
    };
    let overlap = end.min(to).saturating_sub(start.max(from)).max(0);
    (overlap as f64 / (to - from).max(1) as f64).min(1.0)
}

/// After how many days unused memory is closed: [`DEFAULT_EXPIRE_DAYS`], or
/// [`EXPIRE_VARIABLE`] (fractional days allowed), at most
/// [`MAX_EXPIRE_DAYS`]; `None` when it is 0 (off).
#[must_use]
pub fn expire_days() -> Option<f64> {
    expire_days_of(std::env::var(EXPIRE_VARIABLE).ok().as_deref())
}

/// [`expire_days`] of the variable's value.
fn expire_days_of(value: Option<&str>) -> Option<f64> {
    let days = value
        .and_then(|value| value.trim().parse::<f64>().ok())
        .filter(|days| days.is_finite() && *days >= 0.0)
        .unwrap_or(DEFAULT_EXPIRE_DAYS)
        .min(MAX_EXPIRE_DAYS);
    (days > 0.0).then_some(days)
}

/// Entries the expiry pass never closes: the rules the user set
/// (`constraints/*`), their preferences (`user/preferences`), what the
/// learning loop learned from corrections, and a pinned entry, which the user
/// or the agent chose to show in every turn.
fn exempt(doc: &Value) -> bool {
    steering_path(doc["path"].as_str().unwrap_or_default())
        || doc["source"] == "learning"
        || doc["metadata"]["pinned"] == true
}

/// Close, in the store of `dir`, the memory and prompt entries nobody used
/// for `days`: no read and no search hit since they were last written or
/// used. Each is closed, not deleted (`temporal.end`, `expired: unused`), as
/// one event of the pass, so `revert` (or `restore` with its id alone) brings
/// it back. A digest showing an entry is not a use. Returns how many closed.
///
/// # Errors
/// The store cannot be locked, read or written.
#[allow(clippy::cast_possible_truncation, reason = "days as milliseconds")]
pub fn expire_unused(
    dir: &Path,
    days: f64,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<usize, String> {
    if !memory_store::db_path(dir).is_file() {
        return Ok(0);
    }
    // A period past what a time can hold reaches back before any entry:
    // nothing is that old.
    let Some(cutoff) =
        chrono::TimeDelta::try_milliseconds((days.min(MAX_EXPIRE_DAYS) * 86_400_000.0) as i64)
            .and_then(|span| now.checked_sub_signed(span))
    else {
        return Ok(0);
    };
    let _lock = super::harness::lock_state(dir)?;
    let (mut document, rows) = memory_store::load_with_usage(dir)?;
    let cutoff = cutoff.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let at = now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    // The clock starts with the store's first pass: before 0.8.8 no read
    // was counted, and an upgrade must not close a store's whole memory at
    // once for a disuse nobody measured.
    let watched = if let Some(since) = memory_store::read_meta(dir, EXPIRY_SINCE) {
        since.as_str().map_or_else(|| at.clone(), str::to_owned)
    } else {
        memory_store::write_meta(dir, EXPIRY_SINCE, &json!(at))?;
        at.clone()
    };
    let mut closing = 0;
    for kind in ["memory", "prompt"] {
        let Some(records) = document["entries"][kind].as_object_mut() else {
            continue;
        };
        for (key, doc) in records.iter_mut() {
            if exempt(doc) || closed(doc) || memory_store::ended(doc) {
                continue;
            }
            let usage = rows
                .get(&(kind.to_owned(), key.clone()))
                .map(|row| row.usage.clone())
                .unwrap_or_default();
            let last = [
                doc["created_at"].as_str(),
                doc["updated_at"].as_str(),
                usage.last_read_at.as_deref(),
                usage.last_hit_at.as_deref(),
                Some(watched.as_str()),
            ]
            .into_iter()
            .flatten()
            .filter_map(|time| memory_store::instant(time).ok())
            .max();
            if last.is_none_or(|last| last > cutoff) {
                continue;
            }
            close(doc, &at);
            doc[EXPIRED] = json!(UNUSED);
            doc["updated_at"] = json!(at);
            doc["version"] = json!(doc["version"].as_i64().unwrap_or(1) + 1);
            closing += 1;
        }
    }
    if closing > 0 {
        memory_store::save_document(
            dir,
            &document,
            &memory_store::Writer::new("sleep", &json!({"via": "expiry"}), "expire"),
        )?;
    }
    Ok(closing)
}

/// The last expiry pass over the global store: when it ran and how many it
/// closed (across the stores it read), for `ha doctor`.
#[must_use]
pub fn last_expiry(global: &Path) -> Option<(String, u64)> {
    let recorded = memory_store::read_meta(global, EXPIRY_META)?;
    Some((
        recorded["at"].as_str()?.to_owned(),
        recorded["closed"].as_u64().unwrap_or(0),
    ))
}

/// Whether the expiry pass is due: it runs at most daily, or once per its
/// period when that is shorter.
#[must_use]
pub fn expiry_due(global: &Path, days: f64, now: chrono::DateTime<chrono::Utc>) -> bool {
    let every = (days * 86_400.0).min(EXPIRY_EVERY_SECONDS);
    last_expiry(global)
        .and_then(|(at, _)| chrono::DateTime::parse_from_rfc3339(&at).ok())
        .is_none_or(|last| {
            #[allow(clippy::cast_precision_loss, reason = "seconds")]
            let since =
                (now - last.with_timezone(&chrono::Utc)).num_milliseconds() as f64 / 1_000.0;
            since >= every
        })
}

/// The expiry pass, as a job of the Sleep queue: when it is on and due, it
/// closes the unused entries of the global store and of the conversation's
/// store at `local`, and records the run. Another process running it, or a
/// pass not due, leaves it. Fast and local: no model call. A home with no
/// global store has nothing to expire, and the pass creates none.
pub fn expire_due(data_dir: &Path, local: &Path) {
    let Some(days) = expire_days() else {
        return;
    };
    let global = super::harness::global_dir(data_dir);
    let now = chrono::Utc::now();
    if !memory_store::db_path(&global).is_file() || !expiry_due(&global, days, now) {
        return;
    }
    let Some(claimed) = super::sleep::claim_upkeep(data_dir, super::sleep::Job::expiry(&global))
    else {
        return;
    };
    let mut total = 0_usize;
    let mut failure = None;
    for dir in [global.as_path(), local] {
        // A pass that panicked fails its job: dropped, the claim would leave
        // it to run again with no attempt counted, for ever.
        let pass = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            expire_unused(dir, days, now)
        }))
        .unwrap_or_else(|panic| {
            let reason = panic
                .downcast_ref::<&str>()
                .map(|reason| (*reason).to_owned())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_default();
            Err(format!("the expiry pass panicked: {reason}"))
        });
        match pass {
            Ok(count) => total += count,
            Err(error) => failure = Some(error),
        }
    }
    let recorded = json!({
        "at": now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        "closed": total,
        "days": days,
    });
    if let Err(error) = memory_store::write_meta(&global, EXPIRY_META, &recorded) {
        failure.get_or_insert(error);
    }
    harness_types::diag::info("memory", "expired")
        .field("closed", total)
        .field("days", days)
        .emit();
    match failure {
        None => claimed.done(),
        Some(error) => claimed.failed(&error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_entry_holds_from_its_creation_until_it_is_closed() {
        let mut doc = json!({"created_at": "2026-10-01T00:00:00Z", "title": "t", "content": "c"});
        assert!(valid_at(&doc, "2026-10-02T00:00:00.000Z"));
        assert!(!valid_at(&doc, "2026-09-30T00:00:00.000Z"));
        close(&mut doc, "2026-10-05T00:00:00.000Z");
        assert_eq!(
            doc["temporal"],
            json!({"start": "2026-10-01T00:00:00.000Z", "end": "2026-10-05T00:00:00.000Z"})
        );
        assert!(valid_at(&doc, "2026-10-04T23:00:00.000Z"));
        assert!(!valid_at(&doc, "2026-10-05T00:00:00.000Z"));
        // A point it held from, then closed before it: a period all the same.
        let mut point = json!({"temporal": {"start": "2026-12-01T00:00:00.000Z"}});
        close(&mut point, "2026-11-01T00:00:00.000Z");
        assert_eq!(point["temporal"]["end"], "2026-12-01T00:00:00.001Z");
    }

    /// An enormous `HA_MEMORY_EXPIRE_DAYS` is capped, and a pass given more
    /// days than a time holds closes nothing instead of panicking: the panic
    /// left its Sleep job claimed, neither done nor failed.
    #[test]
    fn an_enormous_expiry_period_is_capped_and_never_panics() {
        assert_eq!(expire_days_of(Some("1e300")), Some(MAX_EXPIRE_DAYS));
        assert_eq!(expire_days_of(Some("3")), Some(3.0));
        assert_eq!(expire_days_of(Some("0")), None);
        assert_eq!(expire_days_of(None), Some(DEFAULT_EXPIRE_DAYS));
        let directory = tempfile::tempdir().expect("dir");
        let dir = directory.path().join("harness");
        let mut state = memory_store::empty_document();
        state["entries"]["memory"]["old"] = json!({"id": "old", "kind": "memory", "title": "Old", "content": "An old fact",
            "path": "notes", "version": 1, "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-01T00:00:00Z"});
        memory_store::save_document(
            &dir,
            &state,
            &memory_store::Writer::new("model", &json!({}), "create"),
        )
        .expect("save");
        memory_store::write_meta(&dir, EXPIRY_SINCE, &json!("2026-01-01T00:00:00.000Z"))
            .expect("watched");
        let now = chrono::Utc::now();
        assert_eq!(expire_unused(&dir, 1e300, now).expect("no panic"), 0);
        assert_eq!(expire_unused(&dir, f64::MAX, now).expect("no panic"), 0);
        assert_eq!(expire_unused(&dir, 1.0, now).expect("expired"), 1);
    }

    #[test]
    fn a_web_page_cannot_set_a_constraint_or_a_preference() {
        assert!(steering_refusal("web", "constraints").is_some());
        assert!(steering_refusal("web", "Constraints/deploys").is_some());
        assert!(steering_refusal("web", "user/preferences").is_some());
        assert!(steering_refusal("web", "user/preferences/editor").is_some());
        assert!(steering_refusal("web", "notes/web").is_none());
        assert!(steering_refusal("user", "constraints").is_none());
        assert!(steering_refusal("agent", "user/preferences").is_none());
    }

    #[test]
    fn the_fit_of_a_validity_is_the_share_of_the_window_it_held() {
        let now = millis("2026-11-01T00:00:00.000Z").expect("now");
        let doc = json!({"temporal": {"start": "2026-10-01T00:00:00.000Z", "end": "2026-10-11T00:00:00.000Z"}});
        let window = Window {
            from: Some("2026-10-06T00:00:00.000Z".to_owned()),
            to: Some("2026-10-16T00:00:00.000Z".to_owned()),
        };
        assert!((fit(&doc, &window, now) - 0.5).abs() < 1e-9);
        let point = Window {
            from: Some("2026-10-05T00:00:00.000Z".to_owned()),
            to: Some("2026-10-05T00:00:00.000Z".to_owned()),
        };
        assert!((fit(&doc, &point, now) - 1.0).abs() < 1e-9);
        let later = Window {
            from: Some("2026-10-20T00:00:00.000Z".to_owned()),
            to: None,
        };
        assert!(fit(&doc, &later, now).abs() < 1e-9);
    }

    /// A review's `supersede`: a new entry, and the old one closed in the same
    /// save, naming the new one's memory id, which the save keeps.
    #[test]
    fn a_reviews_supersede_closes_the_old_entry_and_names_the_new_one() {
        let directory = tempfile::tempdir().expect("dir");
        let dir = directory.path().join("harness");
        let mut state = memory_store::empty_document();
        state["entries"]["memory"]["deploy_day"] = json!({"id": "deploy_day", "kind": "memory", "title": "Deploy day",
            "content": "Deploys go out on Thursdays.", "path": "ops", "version": 1,
            "paths": ["ops/deploy/**"],
            "citations": [{"path": "ops/deploy/day.toml", "quote": "day = \"thursday\"", "lines": [1, 1]}],
            "created_at": "2026-10-01T00:00:00Z", "updated_at": "2026-10-01T00:00:00Z"});
        memory_store::save_document(
            &dir,
            &state,
            &memory_store::Writer::new("model", &json!({}), "create"),
        )
        .expect("save");
        let old = memory_store::ids(&dir).expect("ids")
            [&("memory".to_owned(), "deploy_day".to_owned())]
            .clone();
        let edit = json!({"action": "supersede", "kind": "memory", "id": "deploy_day", "title": "Deploy day moved",
            "content": "Deploys go out on Fridays now.", "reason": "the user moved it"});
        let (created, memory) = supersede_edit(
            &dir,
            &mut state,
            "memory",
            "deploy_day",
            edit.as_object().expect("edit"),
            &json!({"via": "refine", "kind": "user"}),
        )
        .expect("supersede");
        let key = created["id"].as_str().expect("key").to_owned();
        assert_eq!(created["action"], "create");
        assert_eq!(created["after"]["supersedes"], json!([old]));
        assert_eq!(created["after"]["path"], "ops");
        // The correction is kept for the files the old fact was; the code
        // the old fact cited is not the new one's.
        assert_eq!(created["after"]["paths"], json!(["ops/deploy/**"]));
        assert!(created["after"].get("citations").is_none());
        assert_eq!(
            state["entries"]["memory"]["deploy_day"][SUPERSEDED_BY],
            memory
        );
        memory_store::save_document(
            &dir,
            &state,
            &memory_store::Writer::new("refine", &json!({}), "refine")
                .reviving("memory", &key, &memory),
        )
        .expect("save");
        let ids = memory_store::ids(&dir).expect("ids");
        assert_eq!(ids[&("memory".to_owned(), key)], memory);
        let after = memory_store::load_document(&dir).expect("load");
        assert!(memory_store::ended(
            &after["entries"]["memory"]["deploy_day"]
        ));
        // Superseded once, it is not superseded again.
        assert!(
            supersede_edit(
                &dir,
                &mut state,
                "memory",
                "deploy_day",
                edit.as_object().expect("edit"),
                &json!({})
            )
            .is_err()
        );
    }

    #[test]
    fn unused_entries_are_closed_and_the_exempt_ones_stay() {
        let directory = tempfile::tempdir().expect("dir");
        let dir = directory.path().join("harness");
        let old = "2026-01-01T00:00:00Z";
        let entry = |title: &str, path: &str, source: &str| {
            json!({"id": title, "kind": "memory", "title": title, "content": format!("{title} content"),
                   "path": path, "source": source, "version": 1, "created_at": old, "updated_at": old})
        };
        let mut state = memory_store::empty_document();
        state["entries"]["memory"]["stale"] = entry("stale", "notes", "agent");
        state["entries"]["memory"]["rule"] = entry("rule", "constraints", "agent");
        state["entries"]["memory"]["taste"] = entry("taste", "user/preferences", "agent");
        state["entries"]["memory"]["learned"] = entry("learned", "notes", "learning");
        state["entries"]["memory"]["read"] = entry("read", "notes", "agent");
        memory_store::save_document(
            &dir,
            &state,
            &memory_store::Writer::new("model", &json!({}), "create"),
        )
        .expect("save");
        let ids = memory_store::ids(&dir).expect("ids");
        let read = ids[&("memory".to_owned(), "read".to_owned())].clone();
        memory_store::record_use(&dir, memory_store::Use::Read, &[read]).expect("use");
        let now = chrono::Utc::now();
        // The first pass starts the clock: an upgraded store is not closed
        // at once for a disuse nobody measured.
        assert_eq!(expire_unused(&dir, 28.0, now).expect("first pass"), 0);
        memory_store::write_meta(&dir, EXPIRY_SINCE, &json!("2026-01-01T00:00:00.000Z"))
            .expect("watched");
        assert_eq!(expire_unused(&dir, 28.0, now).expect("expire"), 1);
        let after = memory_store::load_document(&dir).expect("load");
        let unused = &after["entries"]["memory"]["stale"];
        assert_eq!(unused[EXPIRED], UNUSED);
        assert!(memory_store::ended(unused), "{unused}");
        assert_eq!(unused["version"], 2);
        for kept in ["rule", "taste", "learned", "read"] {
            assert!(!closed(&after["entries"]["memory"][kept]), "{kept}");
        }
        // Closed already, it is not closed again.
        assert_eq!(expire_unused(&dir, 28.0, now).expect("again"), 0);
    }
}
