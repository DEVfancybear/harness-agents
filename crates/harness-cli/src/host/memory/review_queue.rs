//! The person's review queue for memory changes
//! (`docs/architecture/MEMORY_STORE.en.md` §19).
//!
//! A review, `/refine`, `/learn` and Deep Dream apply what they propose at
//! once, except the changes the person should have a say in: a fetched
//! page's words made a rule, a rule (`constraints/*`, `user/preferences`)
//! whose evidence is not the user's own words, a delegated child's proposal
//! its parent's review did not keep - or, with `HA_MEMORY_REVIEW=all`, every
//! memory change a model proposes. Those wait here, beside the store of the
//! scope they would change (`memory_queue.json` in the scope's folder),
//! until the person accepts them - applied through the review's own write
//! path, with its checks, and refused as stale when the entry changed since -
//! or rejects them: a rejection is remembered, so the next review does not
//! queue the same change again, and the reviews are told of it. An item
//! waits [`KEEP_DAYS`] days.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::paths::LaunchEnvironment;

/// `HA_MEMORY_REVIEW=all|policy|off`: which memory changes wait for the
/// person ([`ReviewMode`]).
pub const VARIABLE: &str = "HA_MEMORY_REVIEW";
/// The field of the `memory` key of `settings.json` that says the same.
pub const SETTING_FIELD: &str = "review";
/// The queue of one scope, in its folder.
pub const FILE: &str = "memory_queue.json";
/// The lock every process that changes a queue holds.
const LOCK_FILE: &str = "memory_queue.lock";
/// How long a writer waits for another process holding the lock.
const LOCK_WAIT: Duration = Duration::from_secs(5);
/// How many days an item waits before it expires.
pub const KEEP_DAYS: u64 = 30;
/// How many days a rejection is remembered.
const REJECTED_DAYS: u64 = 90;
/// How many items one scope's queue keeps, the newest.
const ITEMS_KEPT: usize = 200;
/// How many rejections one scope keeps, the newest.
const REJECTED_KEPT: usize = 200;
/// How many rejections a review is told of, the newest.
const REJECTED_SHOWN: usize = 10;
/// The share of their words two writes share to be the same change reworded.
const SAME_WORDS: f64 = 0.6;
/// How much of a rejected change's content is remembered to show a review.
const REJECTED_CONTENT_CHARS: usize = 200;
const DAY_MS: u64 = 24 * 60 * 60 * 1000;

/// Which memory changes wait for the person.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ReviewMode {
    /// The default: a page's words made a rule, a rule whose evidence is not
    /// the user's own words, and a child's proposal the review did not keep.
    #[default]
    Policy,
    /// Every memory change a review, `/refine`, `/learn` or Deep Dream
    /// proposes, and every child's proposal.
    All,
    /// Nothing waits: what 0.9.0 did (a page's words into a rule refused, a
    /// child's proposal the review did not keep dropped).
    Off,
}

impl ReviewMode {
    /// The mode `text` names: `policy` (or `on`, `default`), `all`, `off`.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "policy" | "on" | "default" | "1" | "true" | "yes" => Some(Self::Policy),
            "all" => Some(Self::All),
            "off" | "0" | "false" | "no" => Some(Self::Off),
            _ => None,
        }
    }

    /// The mode the environment and the `memory` key of `settings.json`
    /// beside `config_file` set; the environment wins, and a value that is no
    /// mode is ignored.
    #[must_use]
    pub fn load(config_file: &Path, environment: &LaunchEnvironment) -> Self {
        environment
            .value(VARIABLE)
            .and_then(|value| value.to_str())
            .and_then(Self::parse)
            .or_else(|| {
                super::config::load_setting(config_file, super::memory_switch::SETTING).and_then(
                    |setting| {
                        setting
                            .get(SETTING_FIELD)
                            .and_then(Value::as_str)
                            .and_then(Self::parse)
                    },
                )
            })
            .unwrap_or_default()
    }

    /// Its name, as `HA_MEMORY_REVIEW` takes it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Policy => "policy",
            Self::All => "all",
            Self::Off => "off",
        }
    }
}

/// A memory change waiting for the person.
#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Item {
    /// `mq-` and eight hex digits.
    pub id: String,
    /// When it was queued, in milliseconds.
    pub at_ms: u64,
    /// The scope it would change: `global` or `local`.
    pub scope: String,
    /// The conversation whose review (or child) proposed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<String>,
    /// The edit as a review's edit is applied: `action`, `kind`, `id`,
    /// `title`, `content`, `path`, `evidence`, `reason`, and where its words
    /// came from (`origin_kind`), as the host judged it.
    pub edit: Value,
    /// The entry it changes as it was when it was proposed (`version`,
    /// `title`, `content`), or `null` for an entry that was not there: an
    /// accept is refused as stale when the entry is no longer so.
    pub expected: Value,
    /// Who proposed it: `{via: review|refine|learn|deep_dream|child,
    /// refinement?, child?, role?}`.
    pub by: Value,
    /// Why it waits instead of being applied.
    pub why: String,
    /// The change's fingerprint ([`Shape`]).
    pub fingerprint: String,
    /// The item it waits on: a merge's delete waits on its survivor's change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
}

impl Item {
    /// A new item for `edit`, numbered now.
    #[must_use]
    pub fn new(
        scope: &str,
        conversation: Option<String>,
        edit: Value,
        expected: Value,
        by: Value,
        why: String,
    ) -> Self {
        let fingerprint = Shape::of(&edit).fingerprint;
        let at_ms = now_ms();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        let digest = Sha256::digest(format!("{fingerprint}:{nanos}").as_bytes());
        Self {
            id: format!("mq-{}", hex(&digest[..4])),
            at_ms,
            scope: scope.to_owned(),
            conversation,
            edit,
            expected,
            by,
            why,
            fingerprint,
            after: None,
        }
    }

    fn text(&self, field: &str) -> &str {
        self.edit[field].as_str().unwrap_or_default()
    }

    /// Who proposed it, in words.
    #[must_use]
    pub fn proposer(&self) -> String {
        let via = self.by["via"].as_str().unwrap_or("review");
        match via {
            "child" => format!(
                "delegated child {} ({})",
                self.by["child"].as_str().unwrap_or("?"),
                self.by["role"].as_str().unwrap_or("?")
            ),
            "deep_dream" => "Deep Dream".to_owned(),
            "refine" => "/refine".to_owned(),
            "learn" => "/learn".to_owned(),
            _ => "the review".to_owned(),
        }
    }

    /// One line: id, age, what it changes.
    #[must_use]
    pub fn line(&self) -> String {
        let path = self.text("path");
        let path = if path.is_empty() {
            String::new()
        } else {
            format!(" in {path}")
        };
        format!(
            "{}  {}  {} {} {} `{}`{path}: {}  (by {}, from {})",
            self.id,
            when(self.at_ms),
            self.text("action"),
            self.scope,
            self.text("kind"),
            self.text("id"),
            clip(self.text("title"), 80),
            self.proposer(),
            self.edit[ORIGIN_KIND].as_str().unwrap_or("review"),
        )
    }
}

/// The edit's field holding where its words came from, as `refine` names it.
const ORIGIN_KIND: &str = "origin_kind";

/// A change the person rejected, remembered so it is not proposed again.
#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Rejection {
    pub fingerprint: String,
    pub kind: String,
    /// `write` or `delete`.
    pub class: String,
    /// The entry it changed; `None` for a new entry.
    #[serde(default)]
    pub target: Option<String>,
    /// The words of its title and content, folded.
    pub terms: Vec<String>,
    /// The quote it rested on, its blanks one space and lowercase.
    #[serde(default)]
    pub evidence: String,
    pub title: String,
    pub content: String,
    pub at_ms: u64,
}

/// What makes two proposals the same change: the kind, whether it writes or
/// deletes, the entry it changes, and its words or the quote it rests on.
#[derive(Clone, Debug)]
struct Shape {
    kind: String,
    class: String,
    target: Option<String>,
    terms: Vec<String>,
    evidence: String,
    fingerprint: String,
}

/// The shortest quote two proposals share to rest on the same words: a
/// phrase, as a review's evidence is.
const SAME_EVIDENCE_CHARS: usize = 12;

/// Text on one line, lowercase: how two changes' words are compared.
fn squeezed(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

impl Shape {
    fn of(edit: &Value) -> Self {
        let text = |field: &str| edit[field].as_str().unwrap_or_default().trim().to_owned();
        let action = text("action");
        let class = if action == "delete" {
            "delete"
        } else {
            "write"
        };
        let target = (action != "create")
            .then(|| text("id"))
            .filter(|id| !id.is_empty());
        let terms = super::harness::query_terms(&format!("{}\n{}", text("title"), text("content")));
        let evidence = squeezed(text("evidence").trim_matches(|character: char| {
            matches!(character, '"' | '\'' | '`' | '\u{201c}' | '\u{201d}')
        }));
        let digest = Sha256::digest(
            format!(
                "{}|{class}|{}|{}|{}|{}",
                text("kind"),
                target.as_deref().unwrap_or_default(),
                squeezed(&text("path")),
                squeezed(&text("title")),
                squeezed(&text("content"))
            )
            .as_bytes(),
        );
        Self {
            kind: text("kind"),
            class: class.to_owned(),
            target,
            terms,
            evidence,
            fingerprint: hex(&digest[..8]),
        }
    }

    fn of_rejection(rejection: &Rejection) -> Self {
        Self {
            kind: rejection.kind.clone(),
            class: rejection.class.clone(),
            target: rejection.target.clone(),
            terms: rejection.terms.clone(),
            evidence: rejection.evidence.clone(),
            fingerprint: rejection.fingerprint.clone(),
        }
    }

    /// Whether `self` and `other` are the same change: of the same kind,
    /// both writes or both deletes, of the same entry (or both new), and a
    /// delete, the same fingerprint, a quote of the same words (one holding
    /// the other, a phrase long), or mostly the same words. A child's
    /// proposal and the review's edit that kept it are worded apart, and
    /// rest on the same quote.
    fn same(&self, other: &Self) -> bool {
        if self.kind != other.kind || self.class != other.class || self.target != other.target {
            return false;
        }
        if self.fingerprint == other.fingerprint || self.class == "delete" {
            return true;
        }
        let (short, long) = if self.evidence.len() <= other.evidence.len() {
            (&self.evidence, &other.evidence)
        } else {
            (&other.evidence, &self.evidence)
        };
        if short.chars().count() >= SAME_EVIDENCE_CHARS && long.contains(short.as_str()) {
            return true;
        }
        let shared = self
            .terms
            .iter()
            .filter(|term| other.terms.contains(term))
            .count();
        let all = self.terms.len() + other.terms.len() - shared;
        #[allow(clippy::cast_precision_loss, reason = "a few hundred words at most")]
        let share = if all == 0 {
            1.0
        } else {
            shared as f64 / all as f64
        };
        share >= SAME_WORDS
    }

    fn same_as_item(&self, item: &Item) -> bool {
        self.same(&Self::of(&item.edit))
    }

    fn same_as_rejection(&self, rejection: &Rejection) -> bool {
        self.same(&Self::of_rejection(rejection))
    }
}

/// One scope's queue, as its file holds it.
#[derive(Clone, Debug, Default, serde::Deserialize, serde::Serialize)]
struct Queue {
    #[serde(default)]
    items: Vec<Item>,
    #[serde(default)]
    rejected: Vec<Rejection>,
}

fn now_ms() -> u64 {
    u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap_or_default()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut text, byte| {
        let _ = write!(text, "{byte:02x}");
        text
    })
}

/// A time as the person reads it, in UTC.
fn when(ms: u64) -> String {
    i64::try_from(ms)
        .ok()
        .and_then(chrono::DateTime::from_timestamp_millis)
        .map_or_else(
            || "?".to_owned(),
            |time| time.format("%Y-%m-%d %H:%M UTC").to_string(),
        )
}

fn clip(text: &str, limit: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= limit {
        return flat;
    }
    let mut kept = flat.chars().take(limit).collect::<String>();
    kept.push('…');
    kept
}

/// The queue in `dir`, without the items past [`KEEP_DAYS`] and the
/// rejections past [`REJECTED_DAYS`]. A file that cannot be read is an
/// empty queue.
fn load(dir: &Path) -> Queue {
    let mut queue = std::fs::read_to_string(dir.join(FILE))
        .ok()
        .and_then(|text| serde_json::from_str::<Queue>(&text).ok())
        .unwrap_or_default();
    let now = now_ms();
    queue
        .items
        .retain(|item| now.saturating_sub(item.at_ms) < KEEP_DAYS * DAY_MS);
    queue
        .rejected
        .retain(|rejection| now.saturating_sub(rejection.at_ms) < REJECTED_DAYS * DAY_MS);
    queue
}

fn save(dir: &Path, queue: &mut Queue) -> Result<(), String> {
    let excess = queue.items.len().saturating_sub(ITEMS_KEPT);
    queue.items.drain(..excess);
    let excess = queue.rejected.len().saturating_sub(REJECTED_KEPT);
    queue.rejected.drain(..excess);
    let text = serde_json::to_string_pretty(queue).map_err(|error| error.to_string())?;
    super::files::write_atomic(&dir.join(FILE), &text)
        .map_err(|error| format!("the memory queue could not be written: {error}"))
}

fn hold(dir: &Path) -> Result<std::fs::File, String> {
    super::files::hold(&dir.join(LOCK_FILE), LOCK_WAIT, "memory queue")
}

/// What became of a change offered to the queue.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Offered {
    /// It waits as this item.
    Queued(String),
    /// The same change already waits as this item.
    Waiting(String),
    /// The person rejected the same change then (milliseconds).
    Rejected(u64),
}

/// Offer `item` to the queue in `dir`: it waits, unless the same change
/// waits already or the person rejected it.
///
/// # Errors
/// The queue's lock or file cannot be had.
pub fn offer(dir: &Path, item: Item) -> Result<Offered, String> {
    let _lock = hold(dir)?;
    let mut queue = load(dir);
    let shape = Shape::of(&item.edit);
    if let Some(rejection) = queue
        .rejected
        .iter()
        .rev()
        .find(|rejection| shape.same_as_rejection(rejection))
    {
        return Ok(Offered::Rejected(rejection.at_ms));
    }
    if let Some(waiting) = queue
        .items
        .iter()
        .find(|waiting| shape.same_as_item(waiting))
    {
        return Ok(Offered::Waiting(waiting.id.clone()));
    }
    let id = item.id.clone();
    queue.items.push(item);
    save(dir, &mut queue)?;
    Ok(Offered::Queued(id))
}

/// The items waiting in `dir`, oldest first.
#[must_use]
pub fn waiting(dir: &Path) -> Vec<Item> {
    load(dir).items
}

/// The changes the person rejected in `dir`, newest last.
#[must_use]
pub fn rejections(dir: &Path) -> Vec<Rejection> {
    load(dir).rejected
}

/// What a review is told of the changes the person rejected lately in the
/// queues of `dirs`: not to propose them again unless the conversation shows
/// the user wants them now. `None` when there are none.
#[must_use]
pub fn rejected_part(dirs: &[&Path]) -> Option<String> {
    let mut all = dirs
        .iter()
        .flat_map(|dir| rejections(dir))
        .collect::<Vec<_>>();
    if all.is_empty() {
        return None;
    }
    all.sort_by_key(|rejection| std::cmp::Reverse(rejection.at_ms));
    all.truncate(REJECTED_SHOWN);
    let mut part = "<rejected_by_user>\nThe person reviewed these proposed memory changes in the memory queue and rejected them. Do not propose them again, reworded or not, unless the conversation shows the user now wants them:".to_owned();
    for rejection in all {
        let target = rejection
            .target
            .as_deref()
            .map(|target| format!(" of `{target}`"))
            .unwrap_or_default();
        let _ = write!(
            part,
            "\n- {} {}{target}, {}: {}{}",
            rejection.class,
            rejection.kind,
            when(rejection.at_ms),
            rejection.title,
            if rejection.content.is_empty() {
                String::new()
            } else {
                format!(" - {}", rejection.content)
            }
        );
    }
    part.push_str("\n</rejected_by_user>");
    Some(part)
}

/// An item and the folder of the queue it waits in.
#[derive(Clone, Debug)]
pub struct Located {
    pub dir: PathBuf,
    pub item: Item,
}

/// The queues under the data directory `data`: the global store's and each
/// conversation's that has one.
fn queue_dirs(data: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![super::harness::global_dir(data)];
    let mut tasks = std::fs::read_dir(data.join("sessions"))
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    tasks.sort();
    dirs.extend(
        tasks
            .iter()
            .map(|task| super::harness::local_dir(data, task))
            .filter(|dir| dir.join(FILE).is_file()),
    );
    dirs
}

/// Every item waiting under the data directory `data`, oldest first.
#[must_use]
pub fn all(data: &Path) -> Vec<Located> {
    let mut found = queue_dirs(data)
        .into_iter()
        .flat_map(|dir| {
            waiting(&dir).into_iter().map(move |item| Located {
                dir: dir.clone(),
                item,
            })
        })
        .collect::<Vec<_>>();
    found.sort_by_key(|located| located.item.at_ms);
    found
}

/// How many items wait under the data directory, and how many of them a
/// child proposed.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Counts {
    pub waiting: usize,
    pub from_children: usize,
}

/// [`Counts`] under the data directory `data`.
#[must_use]
pub fn counts(data: &Path) -> Counts {
    let items = all(data);
    Counts {
        waiting: items.len(),
        from_children: items
            .iter()
            .filter(|located| located.item.by["via"] == "child")
            .count(),
    }
}

/// What a session says once a review queued changes: how many wait and
/// where the person reviews them.
#[must_use]
pub fn waiting_notice(queued: usize, waiting: usize) -> String {
    let plural = if queued == 1 { "" } else { "s" };
    let all = if waiting > queued {
        format!(" ({waiting} in all)")
    } else {
        String::new()
    };
    format!(
        "memory: {queued} proposed change{plural} wait{} for your review{all}: /memory queue shows them, /memory queue accept <id> or reject <id> decides (or `ha memory queue`)",
        if queued == 1 { "s" } else { "" }
    )
}

/// Take the item `id` out of the queue in `dir`, whose lock the caller holds.
fn remove(dir: &Path, id: &str) -> Result<Option<Item>, String> {
    let mut queue = load(dir);
    let Some(index) = queue.items.iter().position(|item| item.id == id) else {
        return Ok(None);
    };
    let item = queue.items.remove(index);
    save(dir, &mut queue)?;
    Ok(Some(item))
}

/// Reject the item `id` in `dir`: it leaves the queue, and the change is
/// remembered so it is not queued again. The items that waited on it (a
/// merge's delete on its survivor) leave with it; their ids are returned.
///
/// # Errors
/// The item is not in that queue, or the queue cannot be written.
pub fn reject(dir: &Path, id: &str) -> Result<(Item, Vec<String>), String> {
    let _lock = hold(dir)?;
    let mut queue = load(dir);
    let Some(index) = queue.items.iter().position(|item| item.id == id) else {
        return Err(format!("{id} is not waiting in the memory queue"));
    };
    let item = queue.items.remove(index);
    let shape = Shape::of(&item.edit);
    queue.rejected.push(Rejection {
        fingerprint: shape.fingerprint,
        kind: shape.kind,
        class: shape.class,
        target: shape.target,
        terms: shape.terms,
        evidence: shape.evidence,
        title: clip(item.text("title"), 120),
        content: clip(item.text("content"), REJECTED_CONTENT_CHARS),
        at_ms: now_ms(),
    });
    let dropped = queue
        .items
        .iter()
        .filter(|other| other.after.as_deref() == Some(id))
        .map(|other| other.id.clone())
        .collect::<Vec<_>>();
    queue
        .items
        .retain(|other| other.after.as_deref() != Some(id));
    save(dir, &mut queue)?;
    Ok((item, dropped))
}

/// The scopes an item of the queue in `dir` is applied to.
fn scopes_for(data: &Path, located: &Located) -> super::refine::HarnessScopes {
    let global = super::harness::global_dir(data);
    let local = if located.item.scope == "local" {
        located.dir.clone()
    } else {
        located.item.conversation.as_deref().map_or_else(
            || global.clone(),
            |task| super::harness::local_dir(data, task),
        )
    };
    super::refine::HarnessScopes {
        global,
        local,
        review: ReviewMode::default(),
    }
}

/// What an accept did.
#[derive(Clone, Debug)]
pub enum Accepted {
    /// Applied, by this refinement, which `/refine --rollback` undoes.
    Applied(super::refine::Refinement),
    /// Refused as stale and taken out of the queue: the entry changed since.
    Stale(String),
    /// Refused for another reason; the item still waits.
    Refused(String),
}

/// Accept the item `id`: applied through the write path a review uses, with
/// its checks, as the person's (`origin.kind` `user`). One whose entry
/// changed since it was proposed is refused as stale and leaves the queue.
/// Its queue is held from the look until it leaves: a reject meanwhile is
/// not overridden, and nothing is applied that would stay waiting.
///
/// # Errors
/// No item `id` waits, it waits on another item, or the queue cannot be
/// had or written.
pub fn accept(data: &Path, id: &str) -> Result<Accepted, String> {
    let items = all(data);
    let Some(found) = items.iter().find(|located| located.item.id == id) else {
        return Err(format!("{id} is not waiting in the memory queue"));
    };
    let _lock = hold(&found.dir)?;
    // As the queue holds it now: rejected or accepted since, it is gone.
    let Some(item) = load(&found.dir)
        .items
        .into_iter()
        .find(|item| item.id == id)
    else {
        return Err(format!("{id} is not waiting in the memory queue"));
    };
    let located = &Located {
        dir: found.dir.clone(),
        item,
    };
    if let Some(after) = &located.item.after
        && items.iter().any(|other| &other.item.id == after)
    {
        return Err(format!(
            "{id} deletes an entry merged into what {after} writes: accept {after} first, or reject it (which takes {id} with it)"
        ));
    }
    let scopes = scopes_for(data, located);
    let refinement = super::refine::accept_queued(&scopes, &located.item)?;
    let edit = refinement.record["appliedEdits"]
        .as_array()
        .and_then(|edits| edits.first())
        .cloned()
        .unwrap_or(Value::Null);
    if edit["applied"] == true {
        remove(&located.dir, id)?;
        return Ok(Accepted::Applied(refinement));
    }
    let error = edit["error"]
        .as_str()
        .unwrap_or("the change was not applied")
        .to_owned();
    if edit["stale"] == true {
        remove(&located.dir, id)?;
        return Ok(Accepted::Stale(error));
    }
    Ok(Accepted::Refused(error))
}

/// The entry an item changes, as the store holds it now.
fn current(located: &Located) -> Value {
    let kind = located.item.text("kind");
    let id = located.item.text("id");
    super::refine::load_raw(&located.dir)["entries"][kind][id].clone()
}

/// The lines of `before` and `after` as a diff: what goes, `-`, and what
/// comes, `+`; a line both hold is shown once, indented.
fn diff(before: &str, after: &str) -> Vec<String> {
    let old = before.lines().collect::<Vec<_>>();
    let new = after.lines().collect::<Vec<_>>();
    let mut lines = old
        .iter()
        .filter(|line| !new.contains(line))
        .map(|line| format!("- {line}"))
        .collect::<Vec<_>>();
    for line in &new {
        if old.contains(line) {
            lines.push(format!("  {line}"));
        } else {
            lines.push(format!("+ {line}"));
        }
    }
    lines
}

/// An item whole: who proposed it and why it waits, its evidence, and what
/// it changes against the entry as it is now.
fn shown(located: &Located) -> Vec<String> {
    let item = &located.item;
    let mut lines = vec![item.line()];
    lines.push(format!("  waits because: {}", item.why));
    if let Some(reason) = item.edit["reason"].as_str().filter(|text| !text.is_empty()) {
        lines.push(format!("  reason: {reason}"));
    }
    if let Some(evidence) = item.edit["evidence"]
        .as_str()
        .filter(|text| !text.is_empty())
    {
        lines.push(format!("  evidence: \"{}\"", clip(evidence, 400)));
    }
    if let Some(conversation) = &item.conversation {
        lines.push(format!("  conversation: {conversation}"));
    }
    if let Some(after) = &item.after {
        lines.push(format!("  waits on: {after}"));
    }
    let now = current(located);
    if item.text("action") == "delete" {
        lines.push("  deletes:".to_owned());
        lines.extend(
            now["content"]
                .as_str()
                .unwrap_or_default()
                .lines()
                .map(|line| format!("  - {line}")),
        );
    } else {
        let before_title = now["title"].as_str().unwrap_or_default();
        let after_title = item.edit["title"].as_str().unwrap_or(before_title);
        if now.is_object() {
            lines.push(format!(
                "  against `{}` version {}{}:",
                item.text("id"),
                now["version"],
                if item.text("action") == "supersede" {
                    ", which it closes and replaces"
                } else {
                    ""
                }
            ));
        } else {
            lines.push("  a new entry:".to_owned());
        }
        if before_title != after_title {
            if !before_title.is_empty() {
                lines.push(format!("  - title: {before_title}"));
            }
            lines.push(format!("  + title: {after_title}"));
        }
        let path_now = now["path"].as_str().unwrap_or_default();
        let path = item.edit["path"].as_str().unwrap_or(path_now);
        if path != path_now && !path.is_empty() {
            if !path_now.is_empty() {
                lines.push(format!("  - path: {path_now}"));
            }
            lines.push(format!("  + path: {path}"));
        }
        let content = item.edit["content"]
            .as_str()
            .or_else(|| now["content"].as_str())
            .unwrap_or_default();
        lines.extend(
            diff(now["content"].as_str().unwrap_or_default(), content)
                .into_iter()
                .map(|line| format!("  {line}")),
        );
    }
    lines
}

/// An item as `--json` prints it: the item and the entry as it is now.
fn as_json(located: &Located) -> Value {
    let mut value = serde_json::to_value(&located.item).unwrap_or(Value::Null);
    value["current"] = current(located);
    value["proposed_by"] = json!(located.item.proposer());
    value
}

/// The answer of a queue command: its lines, its JSON, and whether every
/// part of it did what it was asked.
#[derive(Clone, Debug)]
pub struct Answer {
    pub lines: Vec<String>,
    pub json: Value,
    pub ok: bool,
}

/// `ha memory queue` and `/memory queue`: `list` (the default), `show <id>`,
/// `accept <id>...`, `reject <id>...`, over every queue under the data
/// directory `data`.
#[must_use]
pub fn command(data: &Path, words: &[&str]) -> Answer {
    let (action, ids) = words
        .split_first()
        .map_or(("list", &[][..]), |(action, ids)| (*action, ids));
    match action {
        "list" | "ls" => list(data),
        "show" => show(data, ids),
        "accept" => decide(data, ids, true),
        "reject" => decide(data, ids, false),
        other => Answer {
            lines: vec![format!(
                "unknown memory queue action {other:?}: list, show <id>, accept <id>..., reject <id>..."
            )],
            json: json!({"error": format!("unknown action {other}")}),
            ok: false,
        },
    }
}

fn list(data: &Path) -> Answer {
    let items = all(data);
    let mut lines = if items.is_empty() {
        vec![
            "The memory queue is empty: no proposed memory change waits for your review."
                .to_owned(),
        ]
    } else {
        vec![format!(
            "{} proposed memory change(s) wait for your review (each for {KEEP_DAYS} days); `show <id>` shows one with its evidence and diff, `accept <id>` applies it, `reject <id>` keeps it out:",
            items.len()
        )]
    };
    for located in &items {
        lines.push(format!("  {}", located.item.line()));
        lines.push(format!("    waits because: {}", located.item.why));
    }
    Answer {
        lines,
        json: json!({"items": items.iter().map(as_json).collect::<Vec<_>>()}),
        ok: true,
    }
}

fn show(data: &Path, ids: &[&str]) -> Answer {
    let items = all(data);
    if ids.is_empty() {
        return Answer {
            lines: vec!["show <id>: name the item to show".to_owned()],
            json: json!({"error": "no id"}),
            ok: false,
        };
    }
    let mut lines = Vec::new();
    let mut shown_items = Vec::new();
    let mut ok = true;
    for id in ids {
        if let Some(located) = items.iter().find(|located| located.item.id == *id) {
            lines.extend(shown(located));
            shown_items.push(as_json(located));
        } else {
            ok = false;
            lines.push(format!("{id} is not waiting in the memory queue"));
        }
    }
    Answer {
        lines,
        json: json!({"items": shown_items}),
        ok,
    }
}

fn decide(data: &Path, ids: &[&str], accepting: bool) -> Answer {
    let verb = if accepting { "accept" } else { "reject" };
    if ids.is_empty() {
        return Answer {
            lines: vec![format!("{verb} <id>...: name the items")],
            json: json!({"error": "no id"}),
            ok: false,
        };
    }
    let mut lines = Vec::new();
    let mut done = Vec::new();
    let mut refused = Vec::new();
    for id in ids {
        if accepting {
            match accept(data, id) {
                Ok(Accepted::Applied(refinement)) => {
                    let edit = &refinement.record["appliedEdits"][0];
                    lines.push(format!(
                        "accepted {id}: {} {} {} `{}` (refinement {}, which `/refine --rollback` undoes)",
                        edit["action"].as_str().unwrap_or("?"),
                        refinement.record["scope"].as_str().unwrap_or("?"),
                        edit["kind"].as_str().unwrap_or("?"),
                        edit["id"].as_str().unwrap_or("?"),
                        refinement.id()
                    ));
                    done.push(json!({"id": id, "refinement": refinement.id(), "entry": edit["id"], "scope": refinement.record["scope"]}));
                }
                Ok(Accepted::Stale(error)) => {
                    lines.push(format!(
                        "refused {id} as stale, and took it out of the queue: {error}"
                    ));
                    refused.push(json!({"id": id, "stale": true, "error": error}));
                }
                Ok(Accepted::Refused(error)) => {
                    lines.push(format!("refused {id} (it still waits): {error}"));
                    refused.push(json!({"id": id, "stale": false, "error": error}));
                }
                Err(error) => {
                    lines.push(format!("refused {id}: {error}"));
                    refused.push(json!({"id": id, "stale": false, "error": error}));
                }
            }
            continue;
        }
        let located = all(data).into_iter().find(|located| located.item.id == *id);
        match located.map(|located| reject(&located.dir, id)) {
            Some(Ok((item, dropped))) => {
                let with = if dropped.is_empty() {
                    String::new()
                } else {
                    format!(", with {} that waited on it", dropped.join(", "))
                };
                lines.push(format!(
                    "rejected {id}{with}: {} `{}` stays out, and a review does not propose it again",
                    item.text("action"),
                    item.text("id")
                ));
                done.push(json!({"id": id, "dropped": dropped}));
            }
            Some(Err(error)) => {
                lines.push(format!("{id}: {error}"));
                refused.push(json!({"id": id, "error": error}));
            }
            None => {
                lines.push(format!("{id} is not waiting in the memory queue"));
                refused.push(json!({"id": id, "error": "not waiting"}));
            }
        }
    }
    let key = if accepting { "accepted" } else { "rejected" };
    Answer {
        ok: refused.is_empty(),
        lines,
        json: json!({key: done, "refused": refused}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(action: &str, id: &str, title: &str, content: &str) -> Value {
        json!({"action": action, "kind": "memory", "id": id, "title": title, "content": content, "path": "constraints"})
    }

    fn item(edit: Value) -> Item {
        Item::new(
            "global",
            Some("task_1".to_owned()),
            edit,
            Value::Null,
            json!({"via": "review"}),
            "test".to_owned(),
        )
    }

    #[test]
    fn the_mode_is_read_from_the_environment_over_the_setting() {
        assert_eq!(ReviewMode::parse("ALL"), Some(ReviewMode::All));
        assert_eq!(ReviewMode::parse("off"), Some(ReviewMode::Off));
        assert_eq!(ReviewMode::parse("policy"), Some(ReviewMode::Policy));
        assert_eq!(ReviewMode::parse("maybe"), None);
        let home = tempfile::tempdir().expect("home");
        let config = home.path().join("config.toml");
        std::fs::write(
            home.path().join("settings.json"),
            r#"{"memory": {"review": "all"}}"#,
        )
        .expect("settings");
        let none = LaunchEnvironment::from_pairs(Vec::<(String, String)>::new());
        assert_eq!(ReviewMode::load(&config, &none), ReviewMode::All);
        let off = LaunchEnvironment::from_pairs([(VARIABLE, "off")]);
        assert_eq!(ReviewMode::load(&config, &off), ReviewMode::Off);
    }

    #[test]
    fn a_change_waits_once_and_a_rejected_one_is_not_queued_again() {
        let dir = tempfile::tempdir().expect("dir");
        let first = item(edit(
            "create",
            "deploy-rule",
            "Deploy rule",
            "Never deploy on a Friday afternoon without the release board",
        ));
        let Offered::Queued(id) = offer(dir.path(), first).expect("offer") else {
            panic!("queued");
        };
        // The same change reworded a little waits already.
        let again = item(edit(
            "create",
            "deploy-rule-2",
            "Deploy rule",
            "Never deploy on a Friday afternoon without the release board's sign-off",
        ));
        assert_eq!(
            offer(dir.path(), again.clone()).expect("offer"),
            Offered::Waiting(id.clone())
        );
        assert_eq!(waiting(dir.path()).len(), 1);
        let (rejected, dropped) = reject(dir.path(), &id).expect("reject");
        assert_eq!(rejected.id, id);
        assert!(dropped.is_empty());
        assert!(waiting(dir.path()).is_empty());
        assert!(matches!(
            offer(dir.path(), again).expect("offer"),
            Offered::Rejected(_)
        ));
        // Another change is no rejected one.
        let other = item(edit(
            "create",
            "editor",
            "Editor",
            "The user edits in Helix with tabs of four",
        ));
        assert!(matches!(
            offer(dir.path(), other).expect("offer"),
            Offered::Queued(_)
        ));
        let part = rejected_part(&[dir.path()]).expect("told");
        assert!(part.contains("Never deploy on a Friday"), "{part}");
    }

    #[test]
    fn an_item_past_its_days_expires_and_a_merge_leaves_with_its_survivor() {
        let dir = tempfile::tempdir().expect("dir");
        let mut old = item(edit(
            "create",
            "old",
            "Old",
            "an old proposal nobody looked at",
        ));
        old.at_ms = now_ms() - (KEEP_DAYS + 1) * DAY_MS;
        let survivor = item(edit(
            "update",
            "keep",
            "Keep",
            "what both entries said, merged into one",
        ));
        let mut merged = item(edit("delete", "gone", "", ""));
        merged.after = Some(survivor.id.clone());
        let mut queue = Queue {
            items: vec![old, survivor.clone(), merged.clone()],
            rejected: Vec::new(),
        };
        save(dir.path(), &mut queue).expect("save");
        assert_eq!(waiting(dir.path()).len(), 2, "the expired one is gone");
        let (_, dropped) = reject(dir.path(), &survivor.id).expect("reject");
        assert_eq!(dropped, vec![merged.id]);
        assert!(waiting(dir.path()).is_empty());
    }

    /// A child's proposal worded in its language and the review's edit that
    /// kept it in another rest on the same quote: rejecting one keeps the
    /// other out too.
    #[test]
    fn a_change_resting_on_a_rejected_quote_is_the_same_change() {
        let dir = tempfile::tempdir().expect("dir");
        let mut review = edit(
            "create",
            "deploy-window",
            "Deploy window",
            "The project deploys in the quartz-lynx-608 window (ops/deploy.ini)",
        );
        review["evidence"] = json!("deploy_window = quartz-lynx-608");
        let Offered::Queued(id) = offer(dir.path(), item(review)).expect("offer") else {
            panic!("queued");
        };
        reject(dir.path(), &id).expect("reject");
        let mut child = edit(
            "create",
            "khung-gio-deploy",
            "Khung giờ deploy",
            "Khung giờ deploy của project là quartz-lynx-608",
        );
        child["evidence"] = json!("\"deploy_window = quartz-lynx-608\"");
        assert!(matches!(
            offer(dir.path(), item(child)).expect("offer"),
            Offered::Rejected(_)
        ));
    }

    /// An accept holds the queue while it applies the change: one that cannot
    /// have it applies nothing, so a reject made meanwhile is never
    /// overridden and an item applied never stays waiting.
    #[test]
    fn an_accept_applies_nothing_without_the_queue() {
        let data = tempfile::tempdir().expect("data");
        let global = super::super::harness::global_dir(data.path());
        let mut change = edit(
            "create",
            "",
            "Editor",
            "The user edits in Helix with tabs of four",
        );
        change["path"] = json!("notes");
        if let Some(fields) = change.as_object_mut() {
            fields.remove("id");
        }
        let Offered::Queued(id) = offer(&global, item(change)).expect("offer") else {
            panic!("queued");
        };
        let held = hold(&global).expect("held");
        assert!(accept(data.path(), &id).is_err(), "the queue is held");
        assert!(
            super::super::refine::load_raw(&global)["entries"]["memory"]
                .as_object()
                .is_none_or(serde_json::Map::is_empty),
            "nothing applied"
        );
        drop(held);
        assert!(matches!(
            accept(data.path(), &id).expect("accepted"),
            Accepted::Applied(_)
        ));
        assert!(waiting(&global).is_empty());
    }

    #[test]
    fn a_diff_shows_what_goes_and_what_comes() {
        assert_eq!(
            diff("a\nb", "a\nc"),
            vec!["- b".to_owned(), "  a".to_owned(), "+ c".to_owned()]
        );
    }
}
