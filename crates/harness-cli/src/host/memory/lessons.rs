//! The learning loop (`docs/architecture/LEARNING_LOOP.en.md`): what the user
//! corrected and what the agent assumed wrongly, gathered across
//! conversations, made a standing rule once it recurs.
//!
//! After Agnostic-AI's harvest, distill and promote (read at `aab09ff`), with
//! two changes ha's principles ask for:
//! - signals are judged by the model, not by word lists: the automatic review
//!   reads the conversation and reports each correction or wrong assumption as a
//!   lesson, written as data ("when X, Y was right"), with a verbatim quote of
//!   the conversation as its evidence; it names the open lesson a signal
//!   repeats, or contradicts, instead of a fingerprint of its words;
//! - the gate is markdown-agent-memory's promotion gate: 3 independent signals
//!   across 2 conversations, a signal older than 30 days counting half; one
//!   contrary signal holds a lesson back, two retire it.
//!
//! A lesson that passes is promoted by itself, at most two a run and eight
//! standing per scope: a lesson for every project becomes a `constraints`
//! entry in the global memory, which every turn shows; a lesson about one
//! project goes into that project's markdown store (`memory/decisions/` and
//! the index's Current Decisions) when it keeps one, else into the global
//! memory as that project's entry. `ha learning retire` undoes a promotion:
//! the entry is deleted, or the markdown lines are struck through.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The lessons, in the global state directory.
pub const FILE: &str = "lessons.json";

const LOCK_FILE: &str = "lessons.lock";

/// The weight of independent signals a lesson needs.
pub const MIN_WEIGHT: f64 = 3.0;

/// The conversations those signals must come from.
pub const MIN_CONVERSATIONS: usize = 2;

/// How old a signal is before it counts half.
pub const DECAY_DAYS: i64 = 30;

/// Contrary signals, from as many conversations, that retire a lesson.
pub const CONTRARY_RETIRES: usize = 2;

/// Promoted lessons one scope (every project, or one project) may hold.
pub const MAX_STANDING: usize = 8;

/// Promotions one run makes.
pub const MAX_PROMOTIONS_PER_RUN: usize = 2;

/// Open lessons the review is shown.
const MAX_SHOWN: usize = 30;

const MAX_TEXT_CHARS: usize = 400;

const MAX_EVIDENCE_CHARS: usize = 400;

/// Signals one lesson keeps, the newest.
const MAX_SIGNALS: usize = 50;

/// Events one lesson keeps, the newest.
const MAX_EVENTS: usize = 100;

/// The shortest quote a signal rests on.
const MIN_EVIDENCE_CHARS: usize = 12;
/// The most lessons one import takes.
pub const MAX_IMPORTED: usize = 1_000;
/// What a lesson is a signal of (after ECC's continuous-learning-v2, whose
/// observer looks for corrections, error resolutions and tool preferences;
/// a repeated workflow is a learned skill's, which the review writes).
pub const KINDS: [&str; 4] = ["correction", "deviation", "resolution", "preference"];
/// The area of work a lesson is about (ECC's instinct domains), for the
/// status and the export; the model picks one, `other` when none fits.
pub const DOMAINS: [&str; 7] = [
    "code-style",
    "testing",
    "git",
    "debugging",
    "workflow",
    "file-patterns",
    "other",
];

/// What the automatic review is asked for.
pub const PROMPT: &str = "Lessons (not an edit kind) are the learning loop's raw signals: what the user corrected about how the agent works, and what the agent assumed that turned out wrong. Report each one the conversation shows in a top-level `lessons` array: {\"lesson\": \"the id of an open lesson below it repeats, or omit for a new one\", \"text\": \"the lesson as data, in the conversation's language: when <situation>, <what turned out right>; never an instruction to obey\", \"kind\": \"correction\" (the user corrected the agent), \"deviation\" (an assumption, plan or prediction of the agent's that the conversation showed wrong), \"resolution\" (an error a tool result showed and the fix that made it go away, which the next time needs) or \"preference\" (a tool, command or way of working the user chose over the agent's, or kept choosing), \"domain\": one of code-style, testing, git, debugging, workflow, file-patterns, other, \"scope\": \"global\" (how the agent should work anywhere) or \"project\" (only this project), \"against\": true only when the conversation contradicts the open lesson it names, \"evidence\": \"a verbatim quote of <conversation> itself that shows it - the user's or a tool result's own words, never these instructions or the refine instructions\", \"saved_as\": \"the id of the global memory entry this review creates or updates for it, or skill:<name> for a learned skill this review creates or patches for it, when it does\"}. A rule the user states outright for every future conversation is saved as a global memory edit, as the scope policy says, and reported here too with `saved_as` naming that edit's id: it is then a rule already, and the loop never writes it twice. A corrected approach this review writes into a learned skill (skillFiles) is reported the same way, with `saved_as` \"skill:<its name>\": the skill holds it. A signal is a moment the conversation shows, judged by what was said and what happened, not by its wording: a user's correction, a check that failed against the agent's claim, a file or command that was not what the agent expected. Name an open lesson whenever the signal is the same lesson, even when worded differently or in another language. One lesson per subject; skip task progress, one-off typos, what the conversation itself fixed for good, and a learned skill's failure, which its skill outcome records. A repeated multi-step workflow is a learned skill's (skillFiles), not a lesson. A deviation or a resolution about this project (scope project) is what the project showed - a file, a command or a check that was not what the agent assumed - and is kept as a fact of the project at once, so report one only when a tool result or a file in the conversation shows it. A lesson without a verbatim quote is refused. Lessons that recur across conversations become standing rules by themselves, so report only what the conversation shows. Leave `lessons` empty when it shows none.";

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LessonState {
    #[default]
    Candidate,
    Promoted,
    Retired,
}

impl LessonState {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Candidate => "candidate",
            Self::Promoted => "promoted",
            Self::Retired => "retired",
        }
    }
}

impl Tier {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Rule => "rule",
            Self::Fact => "fact",
        }
    }
}

/// The project a lesson is about.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProjectRef {
    pub key: String,
    pub name: String,
    /// Where it was when the lesson was seen: where a markdown store is looked for.
    pub root: String,
}

/// One moment a conversation showed the lesson, or contradicted it.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct Signal {
    pub at: String,
    pub conversation: String,
    #[serde(default)]
    pub against: bool,
    pub evidence: String,
}

/// Where a promotion wrote the rule, so a retirement can undo it.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "target", rename_all = "snake_case")]
pub enum Placement {
    /// An entry of the global memory store.
    Memory { key: String },
    /// A decision file and an index line of a project's markdown store.
    Markdown { root: String, file: String },
    /// A memory entry that already says the rule, which the loop did not
    /// write: the review saved the user's own words, or the memory held it.
    /// Retiring the lesson leaves the entry, which is the user's.
    Covered { key: String },
    /// A fact line in a project file of a project's markdown store.
    MarkdownFact { root: String, file: String },
    /// A learned skill the review wrote or patched for the same correction:
    /// the skill is how the agent works now, so the loop writes no rule that
    /// says it again. Retiring the lesson leaves the skill, which its own
    /// outcomes and the curator judge.
    Skill { name: String },
}

/// What a promoted lesson becomes (Agnostic-AI's ladder, its middle rungs).
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// A standing rule for how the agent works: the recurrence gate holds.
    #[default]
    Rule,
    /// What one project turned out to be, seen once: an assumption of the
    /// agent's the project proved wrong (`deviation`, scope project). It is
    /// observed, not inferred, so it needs no recurrence.
    Fact,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Event {
    pub at: String,
    pub what: String,
    #[serde(default)]
    pub why: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct Lesson {
    pub id: String,
    pub text: String,
    /// One of [`KINDS`].
    pub kind: String,
    /// One of [`DOMAINS`]; empty in a lesson kept before 0.8.6, read as `other`.
    #[serde(default)]
    pub domain: String,
    /// `None` for a lesson about every project.
    #[serde(default)]
    pub project: Option<ProjectRef>,
    #[serde(default)]
    pub state: LessonState,
    #[serde(default)]
    pub signals: Vec<Signal>,
    #[serde(default)]
    pub placement: Option<Placement>,
    #[serde(default)]
    pub events: Vec<Event>,
    #[serde(default)]
    pub tier: Tier,
    /// When it was promoted: the signals after it say whether the rule works.
    #[serde(default)]
    pub promoted_at: Option<String>,
}

/// How strong a lesson's case is, on the gate's terms.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Standing {
    /// Independent supporting signals, an old one counting half.
    pub weight: f64,
    /// The conversations the supporting signals came from.
    pub conversations: usize,
    /// The conversations that contradicted it.
    pub contrary: usize,
}

fn day_of(at: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(at)
        .ok()
        .map(|at| at.with_timezone(&Utc))
}

impl Lesson {
    /// The case for the lesson at `now`. Signals are independent when they
    /// are distinct moments: the same quote from the same conversation, read
    /// by two reviews, is one.
    #[must_use]
    pub fn standing(&self, now: DateTime<Utc>) -> Standing {
        Self::standing_of(self.signals.iter(), now)
    }

    /// The case made after the lesson became a rule: a rule that works stops
    /// being corrected (Agnostic-AI: "a rule works iff its bucket stops
    /// accruing records"). Nothing before promotion counts.
    #[must_use]
    pub fn since_promotion(&self, now: DateTime<Utc>) -> Standing {
        let Some(promoted) = self.promoted_at.as_deref().and_then(day_of) else {
            return Standing::default();
        };
        Self::standing_of(
            self.signals
                .iter()
                .filter(|signal| day_of(&signal.at).is_some_and(|at| at > promoted)),
            now,
        )
    }

    /// A promoted rule the conversations keep correcting as if it were not
    /// there: by as much as it took to become one.
    #[must_use]
    pub fn ineffective(&self, now: DateTime<Utc>) -> bool {
        let since = self.since_promotion(now);
        self.state == LessonState::Promoted
            && self.tier == Tier::Rule
            && since.weight >= MIN_WEIGHT
            && since.conversations >= MIN_CONVERSATIONS
    }

    fn is_fact(&self) -> bool {
        self.project.is_some() && matches!(self.kind.as_str(), "deviation" | "resolution")
    }

    /// The area of work, `other` when the lesson names none.
    #[must_use]
    pub fn domain(&self) -> &str {
        if self.domain.is_empty() {
            "other"
        } else {
            &self.domain
        }
    }

    /// How sure the loop is of the lesson, 0 to 1, on ECC's instinct scale,
    /// read off the gate's weight (a signal older than 30 days counts half):
    /// 0.3 under 3, 0.5 from 3, 0.7 from 6, 0.85 from 11, each contradicting
    /// conversation taking 0.1 off. A rule
    /// is promoted by the gate, not by this number, which says how much the
    /// case has grown since.
    #[must_use]
    pub fn confidence(&self, now: DateTime<Utc>) -> f64 {
        let standing = self.standing(now);
        let base = if standing.weight >= 11.0 {
            0.85
        } else if standing.weight >= 6.0 {
            0.7
        } else if standing.weight >= MIN_WEIGHT {
            0.5
        } else {
            0.3
        };
        #[allow(
            clippy::cast_precision_loss,
            reason = "a handful of contradicting conversations"
        )]
        let contrary = standing.contrary as f64 * 0.1;
        (base - contrary).max(0.0)
    }

    fn standing_of<'a>(signals: impl Iterator<Item = &'a Signal>, now: DateTime<Utc>) -> Standing {
        let mut seen = BTreeSet::new();
        let mut standing = Standing::default();
        let mut supporting = BTreeSet::new();
        let mut contrary = BTreeSet::new();
        for signal in signals {
            if !seen.insert((
                signal.conversation.clone(),
                normalize(&signal.evidence),
                signal.against,
            )) {
                continue;
            }
            if signal.against {
                contrary.insert(signal.conversation.clone());
                continue;
            }
            supporting.insert(signal.conversation.clone());
            let old =
                day_of(&signal.at).is_some_and(|at| now - at > chrono::Duration::days(DECAY_DAYS));
            standing.weight += if old { 0.5 } else { 1.0 };
        }
        standing.conversations = supporting.len();
        standing.contrary = contrary.len();
        standing
    }

    fn scope(&self) -> &str {
        self.project.as_ref().map_or("global", |_| "project")
    }

    fn scope_key(&self) -> Option<&str> {
        self.project.as_ref().map(|project| project.key.as_str())
    }

    fn note(&mut self, what: &str, why: impl Into<String>) {
        self.events.push(Event {
            at: Utc::now().to_rfc3339(),
            what: what.to_owned(),
            why: why.into(),
        });
        if self.events.len() > MAX_EVENTS {
            let extra = self.events.len() - MAX_EVENTS;
            self.events.drain(..extra);
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[allow(
    clippy::struct_field_names,
    reason = "the file's own field names: `lessons` holds the lessons"
)]
pub struct Lessons {
    #[serde(default = "schema")]
    pub schema: u32,
    #[serde(default)]
    pub lessons: Vec<Lesson>,
    /// What each meditation found recurs, newest last.
    #[serde(default)]
    pub meditations: Vec<Meditation>,
}

/// One meditation's reading of the lessons as a whole.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct Meditation {
    pub at: String,
    pub synthesis: String,
    #[serde(default)]
    pub changes: Vec<String>,
}

const fn schema() -> u32 {
    1
}

impl Lessons {
    fn find_mut(&mut self, id: &str) -> Option<&mut Lesson> {
        self.lessons.iter_mut().find(|lesson| lesson.id == id)
    }

    #[must_use]
    pub fn find(&self, id: &str) -> Option<&Lesson> {
        self.lessons.iter().find(|lesson| lesson.id == id)
    }
}

/// Whitespace collapsed and case folded: how two quotes or a quote and the
/// conversation are compared.
fn normalize(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn clip(text: &str, limit: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    format!("{}...", text.chars().take(limit).collect::<String>())
}

// --- the file ---

/// Hold the lessons' lock, so a review and the CLI never interleave a
/// read-modify-write.
fn lock(dir: &Path) -> Result<std::fs::File, String> {
    std::fs::create_dir_all(dir).map_err(|error| format!("lessons directory: {error}"))?;
    let file = std::fs::File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join(LOCK_FILE))
        .map_err(|error| format!("lessons lock: {error}"))?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(std::fs::TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err("waiting for another lessons write timed out".to_owned());
            }
            Err(std::fs::TryLockError::Error(error)) => {
                return Err(format!("lessons lock: {error}"));
            }
        }
    }
}

/// The lessons kept in `dir`; none when there is no file yet.
///
/// # Errors
/// The file exists but cannot be read or parsed: a broken file is reported,
/// never overwritten as empty.
pub fn load(dir: &Path) -> Result<Lessons, String> {
    let path = dir.join(FILE);
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text)
            .map_err(|error| format!("{} is not valid: {error}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Lessons {
            schema: schema(),
            ..Lessons::default()
        }),
        Err(error) => Err(format!("cannot read {}: {error}", path.display())),
    }
}

fn save(dir: &Path, lessons: &Lessons) -> Result<(), String> {
    let path = dir.join(FILE);
    let temporary = dir.join(format!("{FILE}.tmp"));
    let text = serde_json::to_string_pretty(lessons).map_err(|error| error.to_string())?;
    std::fs::write(&temporary, text + "\n")
        .and_then(|()| std::fs::rename(&temporary, &path))
        .map_err(|error| format!("cannot write {}: {error}", path.display()))
}

/// Read, change and save the lessons under their lock.
fn update<T>(dir: &Path, change: impl FnOnce(&mut Lessons) -> T) -> Result<T, String> {
    let _lock = lock(dir)?;
    let mut lessons = load(dir)?;
    let result = change(&mut lessons);
    save(dir, &lessons)?;
    Ok(result)
}

fn new_id() -> String {
    let id = uuid::Uuid::now_v7().simple().to_string();
    // The random tail: the head is the time, alike for lessons a run makes.
    format!("les_{}", &id[id.len() - 10..])
}

// --- the review ---

/// The review's prompt part: what to report, and the open lessons a signal
/// may repeat or contradict - every project's and this project's.
#[must_use]
pub fn prompt_part(dir: &Path, project: Option<&ProjectRef>) -> String {
    let lessons = load(dir).unwrap_or_default();
    let now = Utc::now();
    let mut open = lessons
        .lessons
        .iter()
        .filter(|lesson| lesson.state != LessonState::Retired)
        .filter(|lesson| {
            lesson.project.is_none()
                || lesson.scope_key() == project.map(|project| project.key.as_str())
        })
        .collect::<Vec<_>>();
    open.sort_by(|left, right| {
        let latest = |lesson: &Lesson| lesson.signals.last().map(|signal| signal.at.clone());
        latest(right).cmp(&latest(left))
    });
    let mut listed = String::new();
    for lesson in open.iter().take(MAX_SHOWN) {
        let standing = lesson.standing(now);
        let _ = writeln!(
            listed,
            "- {} [{} {}, {}, {} from {} conversation(s){}{}] {}",
            lesson.id,
            lesson.state.as_str(),
            lesson.tier.as_str(),
            lesson.scope(),
            lesson.kind,
            standing.conversations,
            if standing.contrary > 0 {
                format!(", contradicted in {}", standing.contrary)
            } else {
                String::new()
            },
            if lesson.ineffective(now) {
                ", still corrected since it became a rule"
            } else {
                ""
            },
            lesson.text
        );
    }
    if listed.is_empty() {
        listed.push_str("(none yet)\n");
    }
    format!("<lessons>\n{PROMPT}\nOpen lessons:\n{listed}</lessons>")
}

/// Record the signals the review reported. Each comes back as a record for
/// the refinement: applied, or refused and why.
///
/// # Errors
/// The lessons file cannot be read or written.
///
/// `saved` holds the ids of the global memory entries this review writes: a
/// lesson whose `saved_as` names one is a rule already, the user's own words
/// kept as memory, and the loop never writes it again.
pub fn record(
    dir: &Path,
    reported: &[Value],
    conversation_text: &str,
    conversation: &str,
    project: Option<&ProjectRef>,
    saved: &BTreeSet<String>,
) -> Result<Vec<Value>, String> {
    if reported.is_empty() {
        return Ok(Vec::new());
    }
    let haystack = normalize(conversation_text);
    update(dir, |lessons| {
        reported
            .iter()
            .map(|item| {
                let mut record = record_one(lessons, item, &haystack, conversation, project);
                if record["applied"] == true {
                    cover(lessons, item, &mut record, saved);
                }
                record
            })
            .collect()
    })
}

/// The lessons `ids` a review covered by entries it then failed to write: candidates
/// again, so the gate can still promote them.
pub fn uncover(dir: &Path, ids: &BTreeSet<String>) {
    if ids.is_empty() {
        return;
    }
    let _ = update(dir, |lessons| {
        for lesson in &mut lessons.lessons {
            if matches!(
                lesson.placement,
                Some(Placement::Covered { .. } | Placement::Skill { .. })
            ) && ids.contains(&lesson.id)
                && lesson.state == LessonState::Promoted
            {
                lesson.placement = None;
                lesson.state = LessonState::Candidate;
                lesson.promoted_at = None;
                lesson.note(
                    "uncovered",
                    "the review did not write the memory entry or the skill it saved the lesson as",
                );
                trace(
                    "uncovered",
                    &lesson.id,
                    "the review did not write what it saved the lesson as",
                );
            }
        }
    });
}

/// A recorded lesson the review also saved as global memory, or as a learned
/// skill it wrote or patched (`skill:<name>`): promoted, as that entry or
/// skill, so the rule is kept once.
fn cover(lessons: &mut Lessons, item: &Value, record: &mut Value, saved: &BTreeSet<String>) {
    let Some(key) = item["saved_as"].as_str().filter(|key| !key.is_empty()) else {
        return;
    };
    if !saved.contains(key) {
        record["saved_as_ignored"] = json!(format!(
            "{key} is not a global memory entry or a learned skill this review writes"
        ));
        return;
    }
    let Some(lesson) = record["id"].as_str().and_then(|id| lessons.find_mut(id)) else {
        return;
    };
    if lesson.state != LessonState::Candidate {
        return;
    }
    if let Some(name) = key.strip_prefix("skill:") {
        lesson.state = LessonState::Promoted;
        lesson.promoted_at = Some(Utc::now().to_rfc3339());
        lesson.placement = Some(Placement::Skill {
            name: name.to_owned(),
        });
        lesson.note(
            "promoted",
            format!("the review wrote it into learned skill {name}"),
        );
        record["saved_as"] = json!(key);
        return;
    }
    // A project that keeps a markdown store keeps its rules there: a memory
    // entry about it is not where the rule lives.
    if lesson.project.as_ref().is_some_and(|project| {
        Path::new(&project.root)
            .join(super::markdown_memory::CONFIG_FILE)
            .is_file()
    }) {
        record["saved_as_ignored"] = json!(format!(
            "{key} is memory, and this project keeps its rules in its markdown store"
        ));
        return;
    }
    lesson.state = LessonState::Promoted;
    lesson.promoted_at = Some(Utc::now().to_rfc3339());
    lesson.placement = Some(Placement::Covered {
        key: key.to_owned(),
    });
    lesson.note(
        "promoted",
        format!("the review saved it as memory entry {key}"),
    );
    record["saved_as"] = json!(key);
}

fn refused(item: &Value, error: &str) -> Value {
    json!({"action": "lesson", "kind": "lesson", "id": item["lesson"], "applied": false, "error": error, "lesson": item})
}

fn record_one(
    lessons: &mut Lessons,
    item: &Value,
    haystack: &str,
    conversation: &str,
    project: Option<&ProjectRef>,
) -> Value {
    let text = item["text"].as_str().map(str::trim).unwrap_or_default();
    let evidence = item["evidence"].as_str().map(str::trim).unwrap_or_default();
    // A quote of a word or two is found anywhere; a moment needs a phrase.
    let quoted = normalize(evidence);
    if quoted.chars().count() < MIN_EVIDENCE_CHARS || !haystack.contains(&quoted) {
        return refused(
            item,
            "the evidence is not a verbatim quote of the conversation, a phrase long",
        );
    }
    let against = item["against"].as_bool().unwrap_or(false);
    let signal = Signal {
        at: Utc::now().to_rfc3339(),
        conversation: conversation.to_owned(),
        against,
        evidence: clip(evidence, MAX_EVIDENCE_CHARS),
    };
    if let Some(id) = item["lesson"].as_str().filter(|id| !id.is_empty()) {
        let Some(lesson) = lessons.find_mut(id) else {
            return refused(item, &format!("no open lesson {id}"));
        };
        if lesson.state == LessonState::Retired {
            return refused(item, &format!("lesson {id} is retired"));
        }
        if lesson.project.is_some()
            && lesson.scope_key() != project.map(|project| project.key.as_str())
        {
            return refused(item, &format!("lesson {id} is another project's"));
        }
        lesson.signals.push(signal);
        // The newest signals are the case; a lesson seen for years keeps a
        // bounded file.
        if lesson.signals.len() > MAX_SIGNALS {
            let extra = lesson.signals.len() - MAX_SIGNALS;
            lesson.signals.drain(..extra);
        }
        lesson.note(
            if against { "contradicted" } else { "seen" },
            format!("in {conversation}"),
        );
        return json!({"action": if against { "contradict" } else { "repeat" }, "kind": "lesson", "id": id, "applied": true, "text": lesson.text});
    }
    if against {
        return refused(
            item,
            "a contrary signal must name the lesson it contradicts",
        );
    }
    if text.is_empty() {
        return refused(item, "a new lesson needs its text");
    }
    let Some(kind) = item["kind"].as_str().filter(|kind| KINDS.contains(kind)) else {
        return refused(
            item,
            "kind must be correction, deviation, resolution or preference",
        );
    };
    let domain = item["domain"]
        .as_str()
        .filter(|domain| DOMAINS.contains(domain))
        .unwrap_or("other");
    let project = match item["scope"].as_str() {
        Some("project") => match project {
            Some(project) => Some(project.clone()),
            None => return refused(item, "there is no project for a project lesson"),
        },
        _ => None,
    };
    let mut lesson = Lesson {
        id: new_id(),
        text: clip(text, MAX_TEXT_CHARS),
        kind: kind.to_owned(),
        domain: domain.to_owned(),
        project,
        state: LessonState::Candidate,
        signals: vec![signal],
        placement: None,
        events: Vec::new(),
        tier: Tier::Rule,
        promoted_at: None,
    };
    if lesson.is_fact() {
        lesson.tier = Tier::Fact;
    }
    lesson.note("created", format!("in {conversation}"));
    let id = lesson.id.clone();
    let text = lesson.text.clone();
    lessons.lessons.push(lesson);
    json!({"action": "create", "kind": "lesson", "id": id, "applied": true, "text": text})
}

// --- distill ---

/// What one distill run changed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Distilled {
    pub promoted: Vec<(String, String)>,
    /// Facts a project showed, kept on the first sighting.
    pub facts: Vec<(String, String)>,
    pub retired: Vec<(String, String)>,
    /// Lessons that passed the gate and were held back, with why.
    pub held: Vec<(String, String)>,
}

/// One event of the learning loop in the diagnostics, for the bug report.
fn trace(event: &'static str, lesson: &str, why: &str) {
    harness_types::diag::info("learning", event)
        .field("lesson", lesson)
        .msg(clip(why, 300))
        .emit();
}

impl Distilled {
    /// Each change of a gate run in the diagnostics.
    fn trace(&self) {
        for (event, list) in [
            ("promoted", &self.promoted),
            ("fact", &self.facts),
            ("retired", &self.retired),
            ("held", &self.held),
        ] {
            for (id, why) in list {
                trace(event, id, why);
            }
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.promoted.is_empty() && self.facts.is_empty() && self.retired.is_empty()
    }

    /// One line per change, for a notice.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        for (id, text) in &self.promoted {
            lines.push(format!("learned rule {id}: {text}"));
        }
        for (id, text) in &self.facts {
            lines.push(format!("learned fact {id}: {text}"));
        }
        for (id, why) in &self.retired {
            lines.push(format!("retired lesson {id}: {why}"));
        }
        lines
    }
}

/// Where the global memory lives, and the clock: what a promotion writes with.
#[derive(Clone, Debug)]
pub struct Places {
    /// The global state directory: the lessons and the global memory store.
    pub global: PathBuf,
}

/// The key a memory entry is kept under now: its own, or the one it was
/// renamed to (the old key is among the entry's aliases). `None` when the
/// entry is gone.
fn entry_key(state: &Value, key: &str) -> Option<String> {
    let entries = state["entries"]["memory"].as_object()?;
    if entries.get(key).is_some_and(Value::is_object) {
        return Some(key.to_owned());
    }
    entries
        .iter()
        .find(|(_, entry)| {
            entry["aliases"]
                .as_array()
                .is_some_and(|aliases| aliases.iter().any(|alias| alias == key))
        })
        .map(|(id, _)| id.clone())
}

/// A promoted lesson whose memory entry is gone or renamed. Deep Dream may
/// merge the user's entry a lesson is covered by into another, and the user
/// may delete a learned rule with the memory tool: the lesson follows a
/// rename, finds the entry that now says it, and is a candidate again when
/// none does. A learned entry the user deleted retires its lesson: the user
/// did not want that rule.
fn reconcile(places: &Places, lessons: &mut Lessons, distilled: &mut Distilled) {
    // A store that cannot be read, or reads empty, says nothing about what
    // was deleted: no lesson is touched then.
    let state = match super::memory_store::load_document(&places.global) {
        Ok(state)
            if state["entries"]["memory"]
                .as_object()
                .is_some_and(|entries| !entries.is_empty()) =>
        {
            state
        }
        Ok(_) => return,
        Err(error) => {
            trace("reconcile_skipped", "", &error);
            return;
        }
    };
    for lesson in &mut lessons.lessons {
        if lesson.state != LessonState::Promoted {
            continue;
        }
        let (key, covered) = match &lesson.placement {
            Some(Placement::Covered { key }) => (key.clone(), true),
            Some(Placement::Memory { key }) => (key.clone(), false),
            _ => continue,
        };
        if let Some(now) = entry_key(&state, &key) {
            if now != key {
                let why = format!("its memory entry {key} is now {now}");
                trace("moved", &lesson.id, &why);
                lesson.note("moved", why);
                lesson.placement = Some(if covered {
                    Placement::Covered { key: now }
                } else {
                    Placement::Memory { key: now }
                });
            }
            continue;
        }
        if !covered {
            let why = format!("its memory entry {key} was deleted outside the learning loop");
            lesson.state = LessonState::Retired;
            lesson.placement = None;
            lesson.note("retired", why.clone());
            distilled.retired.push((lesson.id.clone(), why));
            continue;
        }
        let data_dir = places.global.parent().unwrap_or(&places.global);
        let mut host = super::memory::MemoryHost::new(data_dir, "learning");
        let scope = match &lesson.project {
            Some(project) => {
                host = host.with_project(Path::new(&project.root));
                "project"
            }
            None => "global",
        };
        if let Some(found) = host.covering(scope, "memory", &lesson.text, &lesson.text) {
            let why = format!("its memory entry {key} was merged into {found}");
            trace("moved", &lesson.id, &why);
            lesson.note("moved", why);
            lesson.placement = Some(Placement::Covered { key: found });
        } else {
            let why =
                format!("its memory entry {key} is gone and no entry says it: a candidate again");
            trace("unplaced", &lesson.id, &why);
            lesson.state = LessonState::Candidate;
            lesson.placement = None;
            lesson.promoted_at = None;
            lesson.note("unplaced", why);
        }
    }
}

/// A lesson kept as a learned skill that is no longer in the library (the
/// curator removed it, or the user did) is a candidate again: the gate may
/// then write its rule.
pub fn reconcile_skills(places: &Places, skills: &BTreeSet<String>) {
    let _ = update(&places.global, |lessons| {
        for lesson in &mut lessons.lessons {
            let Some(Placement::Skill { name }) = &lesson.placement else {
                continue;
            };
            if lesson.state != LessonState::Promoted || skills.contains(name) {
                continue;
            }
            let why = format!("its learned skill {name} is gone: a candidate again");
            trace("unplaced", &lesson.id, &why);
            lesson.state = LessonState::Candidate;
            lesson.placement = None;
            lesson.promoted_at = None;
            lesson.note("unplaced", why);
        }
    });
}

/// Run the gate over every lesson: promote what passes, within the ceilings,
/// and retire what two conversations contradicted.
///
/// # Errors
/// The lessons file cannot be read or written. A promotion or retirement that
/// fails is held back and said so; it does not stop the others.
#[allow(
    clippy::too_many_lines,
    reason = "one pass of the gate: retire, then facts, then rules within their ceilings"
)]
pub fn distill(places: &Places, now: DateTime<Utc>) -> Result<Distilled, String> {
    update(&places.global, |lessons| {
        let mut distilled = Distilled::default();
        reconcile(places, lessons, &mut distilled);
        // Retire first: a lesson two conversations contradicted is no rule,
        // and a promoted one gives its place back.
        for lesson in &mut lessons.lessons {
            if lesson.state == LessonState::Retired {
                continue;
            }
            let standing = lesson.standing(now);
            if standing.contrary >= CONTRARY_RETIRES {
                let mut why = format!("contradicted in {} conversations", standing.contrary);
                // An entry the loop did not write stays: it is the user's
                // words, and the review updates it when they change.
                if let Some(Placement::Covered { key }) = &lesson.placement {
                    let _ = write!(why, "; memory entry {key} still says it");
                }
                match unplace(places, lesson) {
                    Ok(()) => {
                        lesson.state = LessonState::Retired;
                        lesson.note("retired", why.clone());
                        distilled.retired.push((lesson.id.clone(), why));
                    }
                    Err(error) => distilled.held.push((lesson.id.clone(), error)),
                }
            }
        }
        // A fact is what the project showed, once is enough; a rule is an
        // inference, and waits for recurrence.
        let mut ready = lessons
            .lessons
            .iter()
            .enumerate()
            .filter(|(_, lesson)| lesson.state == LessonState::Candidate)
            .map(|(index, lesson)| (index, lesson.standing(now), lesson.tier))
            .filter(|(_, standing, tier)| match tier {
                Tier::Fact => standing.weight > 0.0,
                Tier::Rule => {
                    standing.weight >= MIN_WEIGHT && standing.conversations >= MIN_CONVERSATIONS
                }
            })
            .collect::<Vec<_>>();
        ready.sort_by(|(_, left, _), (_, right, _)| right.weight.total_cmp(&left.weight));
        let mut rules_promoted = 0;
        for (index, standing, tier) in ready {
            let id = lessons.lessons[index].id.clone();
            if standing.contrary > 0 {
                distilled
                    .held
                    .push((id, "a conversation contradicted it".to_owned()));
                continue;
            }
            if tier == Tier::Fact {
                let lesson = &mut lessons.lessons[index];
                match place_fact(places, lesson) {
                    Ok(placement) => {
                        lesson.placement = Some(placement);
                        lesson.state = LessonState::Promoted;
                        lesson.promoted_at = Some(now.to_rfc3339());
                        lesson.note("promoted", "a fact the project showed");
                        distilled
                            .facts
                            .push((lesson.id.clone(), lesson.text.clone()));
                    }
                    Err(error) => {
                        lesson.note("held", error.clone());
                        distilled.held.push((lesson.id.clone(), error));
                    }
                }
                continue;
            }
            if rules_promoted >= MAX_PROMOTIONS_PER_RUN {
                distilled.held.push((
                    id,
                    format!("{MAX_PROMOTIONS_PER_RUN} lessons were promoted this run already"),
                ));
                continue;
            }
            let scope = lessons.lessons[index].scope_key().map(str::to_owned);
            let standing_in_scope = lessons
                .lessons
                .iter()
                .filter(|lesson| {
                    lesson.state == LessonState::Promoted
                        && lesson.tier == Tier::Rule
                        && lesson.scope_key().map(str::to_owned) == scope
                })
                .count();
            if standing_in_scope >= MAX_STANDING {
                distilled.held.push((
                    id,
                    format!(
                        "its scope holds {MAX_STANDING} learned rules: retire one that no longer helps"
                    ),
                ));
                continue;
            }
            let lesson = &mut lessons.lessons[index];
            match place(places, lesson, &standing) {
                Ok(placement) => {
                    let covered = matches!(placement, Placement::Covered { .. });
                    lesson.placement = Some(placement);
                    lesson.state = LessonState::Promoted;
                    lesson.promoted_at = Some(now.to_rfc3339());
                    lesson.note(
                        "promoted",
                        format!(
                            "weight {} from {} conversations",
                            standing.weight, standing.conversations
                        ),
                    );
                    let text = match (&lesson.placement, covered) {
                        (Some(Placement::Covered { key }), true) => {
                            format!("{} (the memory holds it as {key})", lesson.text)
                        }
                        _ => lesson.text.clone(),
                    };
                    distilled.promoted.push((lesson.id.clone(), text));
                    rules_promoted += 1;
                }
                Err(error) => {
                    lesson.note("held", error.clone());
                    distilled.held.push((lesson.id.clone(), error));
                }
            }
        }
        distilled.trace();
        distilled
    })
}

/// Retire one lesson by hand, undoing its promotion.
///
/// # Errors
/// There is no such lesson, it is retired already, or its promotion cannot be
/// undone.
pub fn retire(places: &Places, id: &str, why: &str) -> Result<Lesson, String> {
    update(&places.global, |lessons| {
        let lesson = lessons
            .find_mut(id)
            .ok_or_else(|| format!("no lesson {id}"))?;
        if lesson.state == LessonState::Retired {
            return Err(format!("lesson {id} is retired already"));
        }
        unplace(places, lesson)?;
        lesson.state = LessonState::Retired;
        lesson.note("retired", why);
        Ok(lesson.clone())
    })?
}

// --- where a rule goes ---

fn date(now: DateTime<Utc>) -> String {
    now.format("%Y-%m-%d").to_string()
}

/// The trail a promoted rule carries: why it is a rule.
fn trail(lesson: &Lesson, standing: &Standing) -> String {
    let days = lesson
        .signals
        .iter()
        .filter(|signal| !signal.against)
        .filter_map(|signal| signal.at.get(..10))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "learned by ha from {} {} signals in {} conversations ({days}); lesson {}",
        standing.weight, lesson.kind, standing.conversations, lesson.id
    )
}

fn place(places: &Places, lesson: &Lesson, standing: &Standing) -> Result<Placement, String> {
    // A rule the memory already says - the review kept the user's words, or
    // an earlier rule says it - is that entry, by the measures the memory
    // tool refuses a duplicate with.
    let data_dir = places.global.parent().unwrap_or(&places.global);
    let mut host = super::memory::MemoryHost::new(data_dir, "learning");
    let scope = match &lesson.project {
        Some(project) => {
            host = host.with_project(Path::new(&project.root));
            "project"
        }
        None => "global",
    };
    if let Some(key) = host.covering(
        scope,
        "memory",
        &format!("Learned: {}", clip(&lesson.text, 60)),
        &lesson.text,
    ) {
        return Ok(Placement::Covered { key });
    }
    if let Some(project) = &lesson.project {
        let root = Path::new(&project.root);
        if root.join(super::markdown_memory::CONFIG_FILE).is_file() {
            return place_markdown(root, lesson, standing);
        }
    }
    place_memory(&places.global, lesson, standing)
}

fn memory_key(lesson: &Lesson) -> String {
    format!("learned_{}", lesson.id.trim_start_matches("les_"))
}

fn writer(lesson: &Lesson, cause: &str) -> super::memory_store::Writer {
    super::memory_store::Writer::new("learning", &json!({"lesson": lesson.id}), cause)
}

/// A `constraints` entry of the global memory: every turn shows it, before
/// the ranked entries; a project's is shown in that project's turns only.
fn place_memory(global: &Path, lesson: &Lesson, standing: &Standing) -> Result<Placement, String> {
    write_entry(
        global,
        lesson,
        &format!("Learned: {}", clip(&lesson.text, 60)),
        &format!("{}\n({})", lesson.text, trail(lesson, standing)),
        &format!("{}/learned", super::harness::CONSTRAINTS_PATH),
        "learn",
    )
}

/// A fact of the project, as a ranked memory entry of that project: a fact is
/// recalled when the work touches it, not put in every turn.
fn place_fact_memory(global: &Path, lesson: &Lesson) -> Result<Placement, String> {
    let seen = lesson
        .signals
        .iter()
        .find(|signal| !signal.against)
        .map(|signal| {
            format!(
                "seen in {}, {}",
                signal.conversation,
                signal.at.get(..10).unwrap_or(&signal.at)
            )
        })
        .unwrap_or_default();
    write_entry(
        global,
        lesson,
        &format!("Fact: {}", clip(&lesson.text, 60)),
        &format!("{}\n(observed: {seen}; lesson {})", lesson.text, lesson.id),
        "facts/learned",
        "learn",
    )
}

/// Write the lesson's own memory entry, a new version when it has one.
fn write_entry(
    global: &Path,
    lesson: &Lesson,
    title: &str,
    content: &str,
    path: &str,
    cause: &str,
) -> Result<Placement, String> {
    let key = memory_key(lesson);
    let _lock = super::harness::lock_state(global)?;
    let mut state = super::refine::load_raw(global);
    let before = &state["entries"]["memory"][&key];
    let version = before["version"].as_i64().map_or(1, |version| version + 1);
    let created = before
        .get("created_at")
        .cloned()
        .unwrap_or_else(|| json!(super::refine::now()));
    let mut entry = json!({
        "id": key,
        "kind": "memory",
        "title": title,
        "content": content,
        "path": path,
        "scope": "global",
        "reference": {},
        "arguments": {},
        "metadata": {"lesson": lesson.id},
        "source": "learning",
        "created_at": created,
        "updated_at": super::refine::now(),
        "version": version,
        "origin": {"via": "learning loop", "lesson": lesson.id},
    });
    if let Some(project) = &lesson.project {
        entry["project"] = json!({"key": project.key, "name": project.name});
    }
    state["entries"]["memory"][&key] = entry;
    super::refine::save_raw(global, &state, &writer(lesson, cause))?;
    Ok(Placement::Memory { key })
}

/// Where a fact goes: the project's markdown store, in its project file, when
/// it keeps one; else the global memory as that project's entry. A fact the
/// memory already holds is that entry.
fn place_fact(places: &Places, lesson: &Lesson) -> Result<Placement, String> {
    let Some(project) = &lesson.project else {
        return Err("a fact needs its project".to_owned());
    };
    let data_dir = places.global.parent().unwrap_or(&places.global);
    let host =
        super::memory::MemoryHost::new(data_dir, "learning").with_project(Path::new(&project.root));
    if let Some(key) = host.covering(
        "project",
        "memory",
        &format!("Fact: {}", clip(&lesson.text, 60)),
        &lesson.text,
    ) {
        return Ok(Placement::Covered { key });
    }
    let root = Path::new(&project.root);
    if root.join(super::markdown_memory::CONFIG_FILE).is_file() {
        return place_markdown_fact(root, &project.name, lesson);
    }
    place_fact_memory(&places.global, lesson)
}

/// An `[observed]` line under `## Facts` of the project's file, and the
/// project in the index's Projects table when it is not there yet.
fn place_markdown_fact(root: &Path, name: &str, lesson: &Lesson) -> Result<Placement, String> {
    let today = date(Utc::now());
    let file = format!("memory/projects/{}.md", slug(name));
    let path = root.join(&file);
    let current =
        std::fs::read_to_string(&path).unwrap_or_else(|_| format!("# {name}\n\n## Facts\n"));
    let line = format!(
        "- [observed] {} ({today}, {})",
        lesson.text.replace('\n', " "),
        lesson.id
    );
    std::fs::create_dir_all(path.parent().unwrap_or(root))
        .and_then(|()| std::fs::write(&path, under_heading(&current, "## Facts", &line)))
        .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
    let index = root.join("MEMORY.md");
    let text = std::fs::read_to_string(&index).unwrap_or_default();
    if !text.contains(&file) {
        let row = format!("| {name} | `{file}` | {name} |");
        std::fs::write(&index, with_project_row(&text, &row))
            .map_err(|error| format!("cannot write {}: {error}", index.display()))?;
    }
    Ok(Placement::MarkdownFact {
        root: root.display().to_string(),
        file,
    })
}

/// `text` with `line` at the end of the section `heading` opens, which is
/// added at the end when the text has none.
fn under_heading(text: &str, heading: &str, line: &str) -> String {
    let lines = text.lines().collect::<Vec<_>>();
    let Some(start) = lines.iter().position(|text| text.trim() == heading) else {
        let mut out = text.trim_end().to_owned();
        let _ = write!(out, "\n\n{heading}\n\n{line}\n");
        return out;
    };
    let end = lines[start + 1..]
        .iter()
        .position(|text| text.starts_with("## "))
        .map_or(lines.len(), |offset| start + 1 + offset);
    let mut out = lines[..end]
        .iter()
        .map(|text| (*text).to_owned())
        .collect::<Vec<_>>();
    while out.last().is_some_and(|text| text.trim().is_empty()) {
        out.pop();
    }
    if out.len() == start + 1 {
        out.push(String::new());
    }
    out.push(line.to_owned());
    if end < lines.len() {
        out.push(String::new());
    }
    out.extend(lines[end..].iter().map(|text| (*text).to_owned()));
    let mut text = out.join("\n");
    text.push('\n');
    text
}

/// The index with `row` added to its Projects table, or a Projects section
/// when it has none.
fn with_project_row(index: &str, row: &str) -> String {
    let lines = index.lines().collect::<Vec<_>>();
    let table = lines
        .iter()
        .position(|text| text.trim() == "## Projects")
        .and_then(|heading| {
            lines[heading..]
                .iter()
                .position(|text| {
                    text.trim_start().starts_with("|-") || text.trim_start().starts_with("| -")
                })
                .map(|offset| heading + offset)
        });
    let Some(separator) = table else {
        let mut out = index.trim_end().to_owned();
        let _ = write!(
            out,
            "\n\n## Projects\n\n| Project | Detail file | Trigger keywords |\n|---------|-------------|------------------|\n{row}\n"
        );
        return out;
    };
    // After the table's last row.
    let last = lines[separator + 1..]
        .iter()
        .position(|text| !text.trim_start().starts_with('|'))
        .map_or(lines.len(), |offset| separator + 1 + offset);
    let mut out = lines[..last]
        .iter()
        .map(|text| (*text).to_owned())
        .collect::<Vec<_>>();
    out.push(row.to_owned());
    out.extend(lines[last..].iter().map(|text| (*text).to_owned()));
    let mut text = out.join("\n");
    text.push('\n');
    text
}

fn slug(text: &str) -> String {
    let mut slug = String::new();
    for character in text.chars().flat_map(char::to_lowercase) {
        if character.is_ascii_alphanumeric() {
            slug.push(character);
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
        if slug.len() >= 40 {
            break;
        }
    }
    let slug = slug.trim_end_matches('-').to_owned();
    if slug.is_empty() {
        "lesson".to_owned()
    } else {
        slug
    }
}

/// The heading the index lists behavior-changing defaults under.
const DECISIONS_HEADING: &str = "## Current Decisions";

/// A decision file whose every line carries its tag, and a line in the
/// index's Current Decisions: the index is in every turn, so the rule is.
fn place_markdown(root: &Path, lesson: &Lesson, standing: &Standing) -> Result<Placement, String> {
    let now = Utc::now();
    let today = date(now);
    let file = format!("memory/decisions/{today}-learned-{}.md", slug(&lesson.text));
    let path = root.join(&file);
    let mut text = format!(
        "# Decision: {}\n\n- [inferred] Rule: {} ({today})\n- [inferred] Why: {}.\n- [inferred] Review trigger: retire it with `ha learning retire {}` once it no longer holds.\n\n## Evidence\n\n",
        clip(&lesson.text, 80),
        lesson.text,
        trail(lesson, standing),
        lesson.id
    );
    for signal in lesson.signals.iter().filter(|signal| !signal.against) {
        let _ = writeln!(
            text,
            "- [observed] \"{}\" ({}, {})",
            signal.evidence.replace('\n', " "),
            signal.conversation,
            signal.at.get(..10).unwrap_or(&signal.at)
        );
    }
    std::fs::create_dir_all(path.parent().unwrap_or(root))
        .and_then(|()| std::fs::write(&path, text))
        .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
    let index = root.join("MEMORY.md");
    let line = format!(
        "- {} (learned {today}, {}) - `{file}`",
        clip(&lesson.text, 160).replace('\n', " "),
        lesson.id
    );
    let current = std::fs::read_to_string(&index).unwrap_or_default();
    std::fs::write(&index, with_decision(&current, &line))
        .map_err(|error| format!("cannot write {}: {error}", index.display()))?;
    Ok(Placement::Markdown {
        root: root.display().to_string(),
        file,
    })
}

/// The index with `line` listed under Current Decisions, the placeholder it
/// replaces gone; a section is added when the index has none.
fn with_decision(index: &str, line: &str) -> String {
    let lines = index.lines().collect::<Vec<_>>();
    let Some(heading) = lines
        .iter()
        .position(|text| text.trim() == DECISIONS_HEADING)
    else {
        let mut out = index.trim_end().to_owned();
        let _ = write!(out, "\n\n{DECISIONS_HEADING}\n\n{line}\n");
        return out;
    };
    let end = lines[heading + 1..]
        .iter()
        .position(|text| text.starts_with("## ") || text.trim() == "---")
        .map_or(lines.len(), |offset| heading + 1 + offset);
    let mut out = lines[..=heading]
        .iter()
        .map(|text| (*text).to_owned())
        .collect::<Vec<_>>();
    out.extend(
        lines[heading + 1..end]
            .iter()
            .filter(|text| text.trim() != "- (none yet)")
            .map(|text| (*text).to_owned()),
    );
    while out.last().is_some_and(|text| text.trim().is_empty()) {
        out.pop();
    }
    out.push(line.to_owned());
    out.push(String::new());
    out.extend(lines[end..].iter().map(|text| (*text).to_owned()));
    let mut text = out.join("\n");
    text.push('\n');
    text
}

/// Strike `marker`'s lines through, dated: supersession is an edit, and the
/// store keeps its history.
fn strike(text: &str, marker: &str, today: &str) -> String {
    let mut out = text
        .lines()
        .map(|line| {
            let Some(rest) = line.strip_prefix("- ") else {
                return line.to_owned();
            };
            if line.contains(marker) && !line.contains("~~") {
                format!("- ~~{rest}~~ retired {today}")
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    if text.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Undo a lesson's promotion, if it has one.
fn unplace(places: &Places, lesson: &Lesson) -> Result<(), String> {
    match &lesson.placement {
        None | Some(Placement::Covered { .. } | Placement::Skill { .. }) => Ok(()),
        Some(Placement::MarkdownFact { root, file }) => {
            let today = date(Utc::now());
            let path = Path::new(root).join(file);
            if let Ok(text) = std::fs::read_to_string(&path) {
                std::fs::write(&path, strike(&text, &lesson.id, &today))
                    .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
            }
            Ok(())
        }
        Some(Placement::Memory { key }) => {
            let _lock = super::harness::lock_state(&places.global)?;
            let mut state = super::refine::load_raw(&places.global);
            if let Some(entries) = state["entries"]["memory"].as_object_mut() {
                entries.remove(key);
            }
            super::refine::save_raw(&places.global, &state, &writer(lesson, "retire"))?;
            Ok(())
        }
        Some(Placement::Markdown { root, file }) => {
            let today = date(Utc::now());
            let root = Path::new(root);
            let decision = root.join(file);
            if let Ok(text) = std::fs::read_to_string(&decision) {
                std::fs::write(&decision, strike(&text, "[inferred] Rule:", &today))
                    .map_err(|error| format!("cannot write {}: {error}", decision.display()))?;
            }
            let index = root.join("MEMORY.md");
            if let Ok(text) = std::fs::read_to_string(&index) {
                std::fs::write(&index, strike(&text, file, &today))
                    .map_err(|error| format!("cannot write {}: {error}", index.display()))?;
            }
            Ok(())
        }
    }
}

/// One lesson in a line: id, state, scope, standing and text.
#[must_use]
pub fn line(lesson: &Lesson, now: DateTime<Utc>) -> String {
    let standing = lesson.standing(now);
    format!(
        "{}  {:<9}  {:<4}  {:<7}  {:<10}  {:<13}  weight {:.1}, {} conversation(s), confidence {:.2}{}{}  {}",
        lesson.id,
        lesson.state.as_str(),
        lesson.tier.as_str(),
        lesson
            .project
            .as_ref()
            .map_or_else(|| "global".to_owned(), |project| project.name.clone()),
        lesson.kind,
        lesson.domain(),
        standing.weight,
        standing.conversations,
        lesson.confidence(now),
        if standing.contrary > 0 {
            format!(", contradicted in {}", standing.contrary)
        } else {
            String::new()
        },
        effect(lesson, now),
        clip(&lesson.text, 120)
    )
}

/// What a promoted rule did: whether the conversations stopped correcting it.
fn effect(lesson: &Lesson, now: DateTime<Utc>) -> String {
    if lesson.state != LessonState::Promoted || lesson.tier != Tier::Rule {
        return String::new();
    }
    let since = lesson.since_promotion(now);
    if lesson.ineffective(now) {
        format!(
            "; STILL CORRECTED since it became a rule ({} signals in {} conversations)",
            since.weight, since.conversations
        )
    } else if since.weight > 0.0 {
        format!(
            "; corrected {} time(s) since it became a rule",
            since.weight
        )
    } else {
        "; not corrected since it became a rule".to_owned()
    }
}

/// One part of the report: its title, and which lessons it holds.
type Section<'a> = (&'a str, Box<dyn Fn(&Lesson) -> bool + 'a>);

/// `list`, the lesson with the strongest case first.
fn by_weight(mut list: Vec<&Lesson>, now: DateTime<Utc>) -> Vec<&Lesson> {
    list.sort_by(|left, right| {
        right
            .standing(now)
            .weight
            .total_cmp(&left.standing(now).weight)
    });
    list
}

/// The recurring mistakes, most recurring first: Agnostic-AI's error log, read
/// from the lessons. Rules first, with what they did; then the candidates
/// that recur; then the facts.
#[must_use]
pub fn report(lessons: &Lessons, now: DateTime<Utc>, all: bool) -> String {
    let shown = |lesson: &&Lesson| all || lesson.state != LessonState::Retired;
    let mut out = String::new();
    let sections: [Section<'_>; 4] = [
        (
            "Rules still corrected (reword or retire them)",
            Box::new(|lesson: &Lesson| lesson.ineffective(now)),
        ),
        (
            "Rules",
            Box::new(|lesson: &Lesson| {
                lesson.state == LessonState::Promoted
                    && lesson.tier == Tier::Rule
                    && !lesson.ineffective(now)
            }),
        ),
        (
            "Recurring, not a rule yet",
            Box::new(|lesson: &Lesson| lesson.state == LessonState::Candidate),
        ),
        (
            "Facts",
            Box::new(|lesson: &Lesson| lesson.tier == Tier::Fact),
        ),
    ];
    for (title, belongs) in &sections {
        let list = by_weight(
            lessons
                .lessons
                .iter()
                .filter(shown)
                .filter(|lesson| belongs(lesson))
                .collect(),
            now,
        );
        let _ = writeln!(out, "## {title} ({})", list.len());
        for lesson in list {
            let _ = writeln!(out, "{}", line(lesson, now));
        }
        out.push('\n');
    }
    let count = |kind: &str| {
        lessons
            .lessons
            .iter()
            .filter(shown)
            .filter(|lesson| lesson.kind == kind)
            .count()
    };
    let _ = writeln!(
        out,
        "{} correction(s), {} deviation(s); the gate: weight {MIN_WEIGHT} from {MIN_CONVERSATIONS} conversations, signals older than {DECAY_DAYS} days count half",
        count("correction"),
        count("deviation")
    );
    if let Some(meditation) = lessons.meditations.last() {
        let _ = writeln!(
            out,
            "\nLast meditation, {}: {}",
            meditation.at.get(..10).unwrap_or(&meditation.at),
            if meditation.synthesis.is_empty() {
                "(no synthesis)"
            } else {
                &meditation.synthesis
            }
        );
        for change in &meditation.changes {
            let _ = writeln!(out, "  {change}");
        }
    }
    out
}

/// [`report`] as one standalone page.
#[must_use]
pub fn report_html(lessons: &Lessons, now: DateTime<Utc>, all: bool) -> String {
    let escape = |text: &str| {
        text.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    let mut body = String::new();
    for block in report(lessons, now, all).split("\n\n") {
        let mut lines = block.lines();
        let Some(first) = lines.next() else {
            continue;
        };
        if let Some(title) = first.strip_prefix("## ") {
            let _ = writeln!(body, "<h2>{}</h2>\n<ul>", escape(title));
            for line in lines {
                let _ = writeln!(body, "<li><code>{}</code></li>", escape(line));
            }
            body.push_str("</ul>\n");
        } else {
            let _ = writeln!(body, "<p>{}</p>", escape(first));
        }
    }
    format!(
        "<!doctype html>\n<html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\"><title>ha lessons</title>\n<style>body{{font-family:system-ui,sans-serif;margin:2rem;max-width:72rem;background:#fff;color:#111}}code{{white-space:pre-wrap}}li{{margin:.3rem 0}}@media (prefers-color-scheme:dark){{body{{background:#111;color:#eee}}}}</style></head>\n<body><h1>What recurs - ha's learning loop</h1>\n<p>{}</p>\n{body}</body></html>\n",
        now.format("%Y-%m-%d %H:%M UTC")
    )
}

// --- status, export, import and promotion (ECC's instinct CLI) ---

/// The open lessons by area of work, each with a bar of its confidence:
/// ECC's `instinct-cli status`.
#[must_use]
pub fn status(lessons: &Lessons, now: DateTime<Utc>) -> String {
    let open = lessons
        .lessons
        .iter()
        .filter(|lesson| lesson.state != LessonState::Retired)
        .collect::<Vec<_>>();
    if open.is_empty() {
        return "no open lessons yet: the automatic review records them as conversations show them\n"
            .to_owned();
    }
    let rules = open
        .iter()
        .filter(|lesson| lesson.state == LessonState::Promoted)
        .count();
    let conversations = open
        .iter()
        .flat_map(|lesson| lesson.signals.iter())
        .filter(|signal| !signal.against)
        .map(|signal| signal.conversation.as_str())
        .collect::<BTreeSet<_>>()
        .len();
    let mut out = format!(
        "{} open lesson(s), {rules} of them rules, from {conversations} conversation(s)\n",
        open.len()
    );
    let mut by_domain = std::collections::BTreeMap::<&str, Vec<&Lesson>>::new();
    for lesson in &open {
        by_domain.entry(lesson.domain()).or_default().push(lesson);
    }
    for (domain, mut list) in by_domain {
        list.sort_by(|left, right| right.confidence(now).total_cmp(&left.confidence(now)));
        let _ = writeln!(out, "\n## {domain} ({})", list.len());
        for lesson in list {
            let confidence = lesson.confidence(now);
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a confidence between 0 and 1, in tenths"
            )]
            let filled = (confidence * 10.0).round().clamp(0.0, 10.0) as usize;
            let _ = writeln!(
                out,
                "{}{} {:>3.0}%  {}  {:<9}  {:<10}  {:<7}  {}",
                "█".repeat(filled),
                "░".repeat(10 - filled),
                confidence * 100.0,
                lesson.id,
                lesson.state.as_str(),
                lesson.kind,
                lesson
                    .project
                    .as_ref()
                    .map_or("global", |project| project.name.as_str()),
                clip(&lesson.text, 100)
            );
        }
    }
    out
}

/// Which lessons an export or an import takes: every project's, one
/// project's, or both.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScopeFilter {
    Global,
    Project,
    All,
}

impl ScopeFilter {
    fn takes(self, lesson: &Lesson) -> bool {
        match self {
            Self::Global => lesson.project.is_none(),
            Self::Project => lesson.project.is_some(),
            Self::All => true,
        }
    }
}

/// The open lessons as a portable document: ECC's `instinct-cli export`. A
/// lesson keeps its quotes and dates, so an import makes the same case; its
/// rule's placement stays behind, as it is this machine's.
#[must_use]
pub fn export(
    lessons: &Lessons,
    now: DateTime<Utc>,
    min_confidence: f64,
    scope: ScopeFilter,
) -> Value {
    let exported = lessons
        .lessons
        .iter()
        .filter(|lesson| lesson.state != LessonState::Retired)
        .filter(|lesson| scope.takes(lesson))
        .filter(|lesson| lesson.confidence(now) >= min_confidence)
        .map(|lesson| {
            json!({
                "id": lesson.id,
                "text": lesson.text,
                "kind": lesson.kind,
                "domain": lesson.domain(),
                "scope": lesson.scope(),
                "project": lesson.project.as_ref().map(|project| project.name.clone()),
                "confidence": lesson.confidence(now),
                "signals": lesson.signals,
            })
        })
        .collect::<Vec<_>>();
    json!({"schema": 1, "kind": "ha-lessons", "exported_at": now.to_rfc3339(), "lessons": exported})
}

/// What an import did, lesson by lesson.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Imported {
    pub added: Vec<String>,
    pub merged: Vec<String>,
    pub skipped: Vec<(String, String)>,
}

/// Bring in lessons another machine exported: ECC's `instinct-cli import`.
/// A lesson that is already here (the same id, or the same text and scope)
/// gains the signals it did not have; a new one is a candidate, which the
/// gate promotes like any other. Their signals keep their conversations,
/// marked as imported, so the same file imported twice adds nothing. A
/// project lesson becomes `project`'s; without a project here it is skipped.
///
/// # Errors
/// The document is not a lessons export, or the lessons cannot be written.
pub fn import(
    dir: &Path,
    document: &Value,
    min_confidence: f64,
    scope: ScopeFilter,
    project: Option<&ProjectRef>,
    dry_run: bool,
) -> Result<Imported, String> {
    if document["kind"] != "ha-lessons" {
        return Err("not a lessons export (`ha learning export` writes one)".to_owned());
    }
    let items = document["lessons"].as_array().cloned().unwrap_or_default();
    if items.len() > MAX_IMPORTED {
        return Err(format!(
            "the export holds {} lessons, more than the {MAX_IMPORTED} an import takes",
            items.len()
        ));
    }
    let run = |lessons: &mut Lessons| import_into(lessons, &items, min_confidence, scope, project);
    if dry_run {
        let mut lessons = load(dir)?;
        return Ok(run(&mut lessons));
    }
    let imported = update(dir, run)?;
    harness_types::diag::info("learning", "imported")
        .field("added", imported.added.len())
        .field("merged", imported.merged.len())
        .field("skipped", imported.skipped.len())
        .emit();
    Ok(imported)
}

/// The signals an exported lesson carries, as this machine keeps them: no
/// secret, a bounded quote, and the conversation marked as imported.
fn imported_signals(item: &Value) -> Vec<Signal> {
    let now = Utc::now();
    let mut signals = item["signals"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|signal| serde_json::from_value::<Signal>(signal.clone()).ok())
        // The review's own measures: a phrase, no secret, a time that parses.
        .filter(|signal| normalize(&signal.evidence).chars().count() >= MIN_EVIDENCE_CHARS)
        .filter(|signal| harness_tools::secret_values::first(&signal.evidence).is_none())
        .filter(|signal| !signal.conversation.trim().is_empty())
        .filter_map(|mut signal| {
            let at = day_of(&signal.at)?;
            // A signal from the future would never age.
            signal.at = at.min(now).to_rfc3339();
            if !signal.conversation.starts_with("import:") {
                signal.conversation = format!("import:{}", signal.conversation);
            }
            signal.evidence = clip(&signal.evidence, MAX_EVIDENCE_CHARS);
            Some(signal)
        })
        .collect::<Vec<_>>();
    signals.sort_by(|left, right| left.at.cmp(&right.at));
    let excess = signals.len().saturating_sub(MAX_SIGNALS);
    signals.drain(..excess);
    signals
}

/// A conversation as an import marks it, and as this machine wrote it: the
/// same moment either way.
fn bare_conversation(conversation: &str) -> &str {
    conversation.strip_prefix("import:").unwrap_or(conversation)
}

/// Add to `kept` the signals of `lesson` it lacks; how many were added.
fn merge_signals(kept: &mut Lesson, lesson: Lesson) -> usize {
    let mut added = 0;
    for signal in lesson.signals {
        let known = kept.signals.iter().any(|have| {
            bare_conversation(&have.conversation) == bare_conversation(&signal.conversation)
                && have.against == signal.against
                && normalize(&have.evidence) == normalize(&signal.evidence)
        });
        if !known {
            kept.signals.push(signal);
            added += 1;
        }
    }
    if added > 0 {
        kept.signals.sort_by(|left, right| left.at.cmp(&right.at));
        if kept.signals.len() > MAX_SIGNALS {
            let extra = kept.signals.len() - MAX_SIGNALS;
            kept.signals.drain(..extra);
        }
    }
    added
}

#[allow(
    clippy::too_many_lines,
    reason = "one exported lesson's checks, in the order an import applies them"
)]
fn import_into(
    lessons: &mut Lessons,
    items: &[Value],
    min_confidence: f64,
    scope: ScopeFilter,
    project: Option<&ProjectRef>,
) -> Imported {
    let now = Utc::now();
    let mut imported = Imported::default();
    for item in items {
        let id = item["id"].as_str().unwrap_or("(no id)").to_owned();
        let text = clip(
            item["text"].as_str().unwrap_or_default().trim(),
            MAX_TEXT_CHARS,
        );
        let Some(kind) = item["kind"].as_str().filter(|kind| KINDS.contains(kind)) else {
            imported
                .skipped
                .push((id, "its kind is not a lesson's".to_owned()));
            continue;
        };
        if text.is_empty() {
            imported.skipped.push((id, "it has no text".to_owned()));
            continue;
        }
        if let Some(shape) = harness_tools::secret_values::first(&text) {
            imported
                .skipped
                .push((id, format!("its text holds what looks like a {shape}")));
            continue;
        }
        // A project's lesson lands only in that project: run the import there.
        let target = if item["scope"] == "project" {
            let from = item["project"].as_str().unwrap_or_default();
            match project {
                Some(project) if project.name == from => Some(project.clone()),
                _ => {
                    imported.skipped.push((
                        id,
                        format!("a lesson of project {from:?}: import it inside that project"),
                    ));
                    continue;
                }
            }
        } else {
            None
        };
        let mut lesson = Lesson {
            id: id.clone(),
            text,
            kind: kind.to_owned(),
            domain: item["domain"]
                .as_str()
                .filter(|domain| DOMAINS.contains(domain))
                .unwrap_or("other")
                .to_owned(),
            project: target,
            signals: imported_signals(item),
            ..Lesson::default()
        };
        if !scope.takes(&lesson) {
            imported
                .skipped
                .push((id, "outside the scope asked for".to_owned()));
            continue;
        }
        let confidence = lesson.confidence(now);
        if confidence < min_confidence {
            imported.skipped.push((
                id,
                format!("confidence {confidence:.2} is under the minimum"),
            ));
            continue;
        }
        let text_key = normalize(&lesson.text);
        let scope_key = lesson.scope_key().map(str::to_owned);
        let same = |kept: &Lesson| {
            kept.id == lesson.id
                || (normalize(&kept.text) == text_key
                    && kept.scope_key().map(str::to_owned) == scope_key)
        };
        // A lesson retired here stays retired: an export does not bring it back.
        if lessons
            .lessons
            .iter()
            .any(|kept| kept.state == LessonState::Retired && same(kept))
        {
            imported
                .skipped
                .push((id, "it was retired here".to_owned()));
            continue;
        }
        if let Some(kept) = lessons
            .lessons
            .iter_mut()
            .find(|kept| kept.state != LessonState::Retired && same(kept))
        {
            let added = merge_signals(kept, lesson);
            if added > 0 {
                kept.note("imported", format!("{added} signal(s) from an export"));
                imported.merged.push(kept.id.clone());
            } else {
                imported.skipped.push((
                    kept.id.clone(),
                    "it brought no signal this machine lacks".to_owned(),
                ));
            }
            continue;
        }
        if lesson.signals.is_empty() {
            imported
                .skipped
                .push((id, "it has no signal to make its case".to_owned()));
            continue;
        }
        // The id is this machine's to give: an exported one may be any text.
        lesson.id = new_id();
        lesson.tier = if lesson.is_fact() {
            Tier::Fact
        } else {
            Tier::Rule
        };
        lesson.note("imported", format!("from an export, as {id}"));
        imported.added.push(lesson.id.clone());
        lessons.lessons.push(lesson);
    }
    imported
}

/// Make a project's lesson one for every project: ECC's `instinct-cli
/// promote <id>`. A rule written for the project is taken back; the gate's
/// next run writes it where a rule for every project goes. A fact is what
/// one project showed, and stays that project's.
///
/// # Errors
/// There is no such open lesson, it is already for every project, it is a
/// fact, or its project rule cannot be taken back.
pub fn promote_global(places: &Places, id: &str) -> Result<Lesson, String> {
    update(&places.global, |lessons| {
        let lesson = lessons
            .find_mut(id)
            .ok_or_else(|| format!("no lesson {id}"))?;
        if lesson.state == LessonState::Retired {
            return Err(format!("lesson {id} is retired"));
        }
        let Some(project) = lesson.project.clone() else {
            return Err(format!("lesson {id} is for every project already"));
        };
        if lesson.tier == Tier::Fact {
            return Err(format!(
                "lesson {id} is a fact of project {}: it holds there only",
                project.name
            ));
        }
        match &lesson.placement {
            Some(Placement::Covered { key }) => {
                return Err(format!(
                    "lesson {id} is memory entry {key}, the user's own words: move that entry to every project instead"
                ));
            }
            Some(Placement::Skill { name }) => {
                return Err(format!(
                    "lesson {id} is learned skill {name}: make that skill global instead"
                ));
            }
            _ => {}
        }
        unplace(places, lesson)?;
        lesson.project = None;
        lesson.state = LessonState::Candidate;
        lesson.placement = None;
        lesson.promoted_at = None;
        let why = format!(
            "made a lesson for every project (it was project {}'s)",
            project.name
        );
        trace("generalized", &lesson.id, &why);
        lesson.note("generalized", why);
        Ok(lesson.clone())
    })?
}

// --- meditation ---

/// When the last meditation ran, beside the lessons.
const MEDITATED_FILE: &str = "lessons_meditated";

/// How often the lessons are read as a whole (Agnostic-AI's nightly
/// meditation): at most once a day, after a review.
pub const MEDITATE_EVERY: chrono::Duration = chrono::Duration::hours(24);

/// Open lessons one meditation reads, the most recently seen.
const MEDITATION_LESSONS: usize = 60;

/// Meditations the file keeps.
const MAX_MEDITATIONS: usize = 30;

const MEDITATION_SYSTEM: &str = "You are the reflection pass of ha's learning loop. You read every open lesson - what users corrected and what the agent assumed wrongly, gathered across conversations - as a whole, as Agnostic-AI's nightly meditation does, and you keep the set honest. Judge by what each lesson's quotes show, never by its wording alone. Change only what the quotes justify; when in doubt, change nothing. Answer with one JSON object.";

/// Whether a meditation is due: two or more open lessons, and none for a day.
#[must_use]
pub fn meditation_due(dir: &Path, now: DateTime<Utc>) -> bool {
    let open = load(dir).map_or(0, |lessons| {
        lessons
            .lessons
            .iter()
            .filter(|lesson| lesson.state != LessonState::Retired)
            .count()
    });
    if open < 2 {
        return false;
    }
    std::fs::read_to_string(dir.join(MEDITATED_FILE))
        .ok()
        .and_then(|at| day_of(at.trim()))
        .is_none_or(|at| now - at >= MEDITATE_EVERY)
}

/// The meditation's prompt: what to do, and each open lesson with its case.
fn meditation_prompt(lessons: &Lessons, now: DateTime<Utc>, skills: bool) -> String {
    let mut open = lessons
        .lessons
        .iter()
        .filter(|lesson| lesson.state != LessonState::Retired)
        .collect::<Vec<_>>();
    open.sort_by(|left, right| {
        let latest = |lesson: &Lesson| lesson.signals.last().map(|signal| signal.at.clone());
        latest(right).cmp(&latest(left))
    });
    let mut listed = String::new();
    for lesson in open.into_iter().take(MEDITATION_LESSONS) {
        let standing = lesson.standing(now);
        let _ = writeln!(
            listed,
            "- {} [{} {}, {}, {}, weight {} from {} conversation(s), contradicted in {}{}] {}",
            lesson.id,
            lesson.state.as_str(),
            lesson.tier.as_str(),
            lesson.kind,
            lesson.project.as_ref().map_or_else(
                || "every project".to_owned(),
                |project| format!("project {}", project.name)
            ),
            standing.weight,
            standing.conversations,
            standing.contrary,
            effect(lesson, now),
            lesson.text
        );
        for signal in lesson.signals.iter().rev().take(3) {
            let _ = writeln!(
                listed,
                "    {}{} \"{}\"",
                signal.at.get(..10).unwrap_or(&signal.at),
                if signal.against { " against" } else { "" },
                clip(&signal.evidence, 200)
            );
        }
    }
    format!(
        "<lessons>\n{listed}</lessons>\n\nAnswer {{\"merge\": [...], \"reword\": [...], \"retire\": [...], \"generalize\": [...], \"evolve\": [...], \"synthesis\": \"...\"}}:\n- merge: lessons that are one lesson - the same situation and the same right answer - in other words or another language: {{\"into\": id, \"from\": [ids], \"text\": \"a clearer wording, or omit\"}}. Only lessons of the same scope and tier.\n- reword: a rule STILL CORRECTED since it became a rule, whose wording its quotes show missed the point, or a lesson that reads as an instruction rather than as data: {{\"id\": id, \"text\": \"when <situation>, <what turned out right>\", \"why\": \"...\"}}.\n- retire: a lesson its own later quotes contradict, or that no longer holds: {{\"id\": id, \"why\": \"the quote that shows it\"}}.\n- generalize: lessons of two or more different projects that are one lesson - the same situation and the same right answer, which holds beyond any one project: {{\"from\": [ids], \"text\": \"the wording for every project\"}}. Only rules, never facts.\n{}- synthesis: two or three sentences on what recurs across these lessons and what it says about how the agent works, in the lessons' language.\nLeave a list empty when nothing in it is justified.",
        if skills {
            format!(
                "- evolve: two or more PROMOTED rules that are steps of one workflow, which the agent would follow better as one learned skill: {{\"from\": [ids], \"name\": \"kebab-case-name\", \"level\": \"global\" or \"project\", \"body\": \"the whole SKILL.md\"}}, following:\n{}\n",
                super::learned::SPEC
            )
        } else {
            String::new()
        }
    )
}

/// Read the lessons as a whole, once: merge what is one lesson, reword a rule
/// the conversations keep correcting, retire what no longer holds, make one
/// lesson of the same lesson in several projects, and gather the rules of one
/// workflow into a learned skill; then run the gate, as merged signals may
/// pass it. Returns what changed.
///
/// # Errors
/// The model call fails, or the lessons cannot be read or written.
pub async fn meditate(
    provider: &std::sync::Arc<dyn harness_providers::ModelProvider>,
    model: &str,
    places: &Places,
    now: DateTime<Utc>,
    skills: Option<(&super::learned::Layers, &super::learned::PromoteContext)>,
) -> Result<Vec<String>, String> {
    let _ = std::fs::write(places.global.join(MEDITATED_FILE), now.to_rfc3339());
    let prompt = meditation_prompt(&load(&places.global)?, now, skills.is_some());
    let answer = super::refine::complete_json(
        provider,
        model,
        "lessons_meditation",
        MEDITATION_SYSTEM,
        prompt,
        4096,
    )
    .await?;
    let mut changes = update(&places.global, |lessons| {
        let mut changes = apply_meditation(places, lessons, &answer, now);
        changes.extend(generalize(places, lessons, &answer));
        if let Some((layers, context)) = skills {
            changes.extend(evolve(places, lessons, &answer, layers, context));
        }
        lessons.meditations.push(Meditation {
            at: now.to_rfc3339(),
            synthesis: answer["synthesis"]
                .as_str()
                .unwrap_or_default()
                .trim()
                .to_owned(),
            changes: changes.clone(),
        });
        if lessons.meditations.len() > MAX_MEDITATIONS {
            let extra = lessons.meditations.len() - MAX_MEDITATIONS;
            lessons.meditations.drain(..extra);
        }
        changes
    })?;
    changes.extend(distill(places, now)?.lines());
    Ok(changes)
}

fn apply_meditation(
    places: &Places,
    lessons: &mut Lessons,
    answer: &Value,
    now: DateTime<Utc>,
) -> Vec<String> {
    let mut changes = Vec::new();
    let list = |field: &str| answer[field].as_array().cloned().unwrap_or_default();
    for merge in list("merge") {
        let Some(into) = merge["into"].as_str() else {
            continue;
        };
        let Some(target) = lessons
            .find(into)
            .filter(|lesson| lesson.state != LessonState::Retired)
            .cloned()
        else {
            continue;
        };
        for from in merge["from"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            if from == into {
                continue;
            }
            let Some(source) = lessons.find_mut(from) else {
                continue;
            };
            if source.state == LessonState::Retired
                || source.scope_key() != target.scope_key()
                || source.tier != target.tier
            {
                continue;
            }
            if unplace(places, source).is_err() {
                continue;
            }
            let moved = std::mem::take(&mut source.signals);
            source.state = LessonState::Retired;
            source.note("merged", format!("into {into}"));
            if let Some(target) = lessons.find_mut(into) {
                target.signals.extend(moved);
                target.signals.sort_by(|left, right| left.at.cmp(&right.at));
                target.note("merged", format!("{from} came into it"));
            }
            changes.push(format!("merged lesson {from} into {into}"));
        }
        if let Some(text) = merge["text"]
            .as_str()
            .filter(|text| !text.trim().is_empty())
            && let Some(target) = lessons.find_mut(into)
            && target.state == LessonState::Candidate
        {
            target.text = clip(text, MAX_TEXT_CHARS);
        }
    }
    for reword in list("reword") {
        let (Some(id), Some(text)) = (reword["id"].as_str(), reword["text"].as_str()) else {
            continue;
        };
        let why = reword["why"].as_str().unwrap_or_default().to_owned();
        let Some(lesson) = lessons
            .find_mut(id)
            .filter(|lesson| lesson.state != LessonState::Retired)
        else {
            continue;
        };
        let text = clip(text, MAX_TEXT_CHARS);
        if text.is_empty() || text == lesson.text {
            continue;
        }
        match reword_placement(places, lesson, &text, now) {
            Ok(()) => {
                lesson.note("reworded", format!("{}: {why}", lesson.text));
                lesson.text = text;
                changes.push(format!("reworded lesson {id}: {}", lesson.text));
            }
            Err(error) => lesson.note("not reworded", error),
        }
    }
    for retire in list("retire") {
        let Some(id) = retire["id"].as_str() else {
            continue;
        };
        let why = retire["why"]
            .as_str()
            .unwrap_or("the meditation found it no longer holds");
        let Some(lesson) = lessons
            .find_mut(id)
            .filter(|lesson| lesson.state != LessonState::Retired)
        else {
            continue;
        };
        if unplace(places, lesson).is_ok() {
            lesson.state = LessonState::Retired;
            lesson.note("retired", format!("meditation: {why}"));
            changes.push(format!("retired lesson {id}: {why}"));
        }
    }
    changes
}

/// Make one lesson for every project of the same lesson seen in two or more
/// projects (ECC's automatic `promote`: an instinct in 2+ projects becomes
/// global). The sources give their signals to it and are retired; the gate
/// then decides on the merged case, as on any other. Facts stay with their
/// project, and a lesson the user's entry or a learned skill holds is not
/// moved. When a lesson for every project already says it, the signals go
/// to that one.
#[allow(
    clippy::too_many_lines,
    reason = "take the sources back, then merge them into one lesson or its twin"
)]
fn generalize(places: &Places, lessons: &mut Lessons, answer: &Value) -> Vec<String> {
    let mut changes = Vec::new();
    for item in answer["generalize"].as_array().into_iter().flatten() {
        let ids = item["from"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
        let sources = lessons
            .lessons
            .iter()
            .filter(|lesson| ids.contains(&lesson.id))
            .filter(|lesson| lesson.state != LessonState::Retired && lesson.tier == Tier::Rule)
            .filter(|lesson| lesson.project.is_some())
            .filter(|lesson| {
                !matches!(
                    lesson.placement,
                    Some(Placement::Covered { .. } | Placement::Skill { .. })
                )
            })
            .cloned()
            .collect::<Vec<_>>();
        let projects = |lessons: &[&Lesson]| {
            lessons
                .iter()
                .filter_map(|lesson| lesson.scope_key())
                .collect::<BTreeSet<_>>()
                .len()
        };
        if projects(&sources.iter().collect::<Vec<_>>()) < 2 {
            continue;
        }
        let text = item["text"]
            .as_str()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map_or_else(
                || sources[0].text.clone(),
                |text| clip(text, MAX_TEXT_CHARS),
            );
        // Take each source's rule back; only those taken back move.
        let mut moved = Vec::new();
        for source in &sources {
            if let Some(lesson) = lessons.find_mut(&source.id)
                && unplace(places, lesson).is_ok()
            {
                moved.push(source.clone());
            }
        }
        let projects_moved = projects(&moved.iter().collect::<Vec<_>>());
        if projects_moved < 2 {
            // Too few to make the case: the ones taken back are candidates,
            // and the gate writes their rules again.
            for source in &moved {
                if let Some(lesson) = lessons.find_mut(&source.id) {
                    lesson.state = LessonState::Candidate;
                    lesson.placement = None;
                    lesson.promoted_at = None;
                }
            }
            continue;
        }
        let mut signals = moved
            .iter()
            .flat_map(|source| source.signals.iter().cloned())
            .collect::<Vec<_>>();
        signals.sort_by(|left, right| left.at.cmp(&right.at));
        let excess = signals.len().saturating_sub(MAX_SIGNALS);
        signals.drain(..excess);
        let from = moved
            .iter()
            .map(|source| source.id.clone())
            .collect::<Vec<_>>();
        for id in &from {
            if let Some(lesson) = lessons.find_mut(id) {
                lesson.signals.clear();
                lesson.state = LessonState::Retired;
                lesson.placement = None;
            }
        }
        let key = normalize(&text);
        let into = if let Some(twin) = lessons.lessons.iter_mut().find(|lesson| {
            lesson.project.is_none()
                && lesson.state != LessonState::Retired
                && normalize(&lesson.text) == key
        }) {
            twin.signals.extend(signals);
            twin.signals.sort_by(|left, right| left.at.cmp(&right.at));
            let excess = twin.signals.len().saturating_sub(MAX_SIGNALS);
            twin.signals.drain(..excess);
            twin.note(
                "generalized",
                format!(
                    "{} came into it from {projects_moved} projects",
                    from.join(", ")
                ),
            );
            twin.id.clone()
        } else {
            let mut general = Lesson {
                id: new_id(),
                text: text.clone(),
                kind: moved[0].kind.clone(),
                domain: moved[0].domain.clone(),
                signals,
                ..Lesson::default()
            };
            general.note(
                "generalized",
                format!("from {} in {projects_moved} projects", from.join(", ")),
            );
            let id = general.id.clone();
            lessons.lessons.push(general);
            id
        };
        for id in &from {
            if let Some(lesson) = lessons.find_mut(id) {
                lesson.note("generalized", format!("into {into}"));
            }
        }
        let change = format!(
            "generalized {} into lesson {into} for every project: {text}",
            from.join(", ")
        );
        trace("generalized", &into, &change);
        changes.push(change);
    }
    changes
}

/// Gather promoted rules that are steps of one workflow into a learned skill
/// (ECC's `/evolve`, whose skills are written where nothing loads them; this
/// one goes through the learned-skill promoter, with its checks, into the
/// library the agent loads skills from). The rules must hold where the skill
/// will: all for every project (a global skill), or all one project's (that
/// project's skill). Each rule then is the skill: its own entry or decision
/// line is taken back, so the workflow is kept in one place.
fn evolve(
    places: &Places,
    lessons: &mut Lessons,
    answer: &Value,
    layers: &super::learned::Layers,
    context: &super::learned::PromoteContext,
) -> Vec<String> {
    let mut changes = Vec::new();
    for item in answer["evolve"].as_array().into_iter().flatten() {
        let rules = item["from"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter_map(|id| lessons.find(id))
            .filter(|lesson| lesson.state == LessonState::Promoted && lesson.tier == Tier::Rule)
            .filter(|lesson| {
                !matches!(
                    lesson.placement,
                    Some(Placement::Covered { .. } | Placement::Skill { .. })
                )
            })
            .cloned()
            .collect::<Vec<_>>();
        let (Some(name), Some(body)) = (item["name"].as_str(), item["body"].as_str()) else {
            continue;
        };
        if rules.len() < 2 {
            continue;
        }
        let scopes = rules
            .iter()
            .map(|lesson| lesson.scope_key().map(str::to_owned))
            .collect::<BTreeSet<_>>();
        if scopes.len() != 1 {
            changes.push(format!(
                "evolve into skill {name} refused: its rules hold in different scopes"
            ));
            continue;
        }
        let level = if rules[0].project.is_some() {
            if layers.project.is_none() {
                changes.push(format!(
                    "evolve into skill {name} refused: its rules are a project's, and the project's skills are not available"
                ));
                continue;
            }
            "project"
        } else {
            "global"
        };
        let ids = rules
            .iter()
            .map(|lesson| lesson.id.clone())
            .collect::<Vec<_>>();
        let intent = super::learned::Intent::from_value(&json!({
            "action": "create",
            "name": name,
            "level": level,
            "body": body,
            "reason": format!("the learning loop's rules {} are one workflow", ids.join(", ")),
            "evidence": rules.iter().map(|lesson| lesson.text.clone()).collect::<Vec<_>>().join(" / "),
        }));
        let verdict = super::learned::promote(layers, &intent, context);
        if !verdict.ok {
            changes.push(format!(
                "evolve into skill {name} refused: {}",
                verdict.findings.join("; ")
            ));
            continue;
        }
        for id in &ids {
            if let Some(lesson) = lessons.find_mut(id) {
                // Its own rule is taken back: the skill says it now.
                if let Err(error) = unplace(places, lesson) {
                    lesson.note("not evolved", error);
                    continue;
                }
                lesson.placement = Some(Placement::Skill {
                    name: name.to_owned(),
                });
                lesson.note("evolved", format!("into learned skill {name}"));
            }
        }
        let change = format!("evolved rules {} into learned skill {name}", ids.join(", "));
        trace("evolved", &ids.join(","), &change);
        changes.push(change);
    }
    changes
}

/// Carry a new wording to where the lesson's rule or fact is written; the old
/// one is superseded, never deleted. An entry the loop did not write stays
/// as the user wrote it.
fn reword_placement(
    places: &Places,
    lesson: &Lesson,
    text: &str,
    now: DateTime<Utc>,
) -> Result<(), String> {
    let mut renamed = lesson.clone();
    text.clone_into(&mut renamed.text);
    let today = date(now);
    match &lesson.placement {
        None => Ok(()),
        Some(Placement::Covered { key }) => Err(format!(
            "memory entry {key} is the user's own words; the review updates it"
        )),
        Some(Placement::Skill { name }) => Err(format!(
            "learned skill {name} holds it; the review patches the skill"
        )),
        // A new version of the lesson's own entry: the store keeps the old.
        Some(Placement::Memory { .. }) => match lesson.tier {
            Tier::Rule => place_memory(&places.global, &renamed, &renamed.standing(now)),
            Tier::Fact => place_fact_memory(&places.global, &renamed),
        }
        .map(|_| ()),
        Some(Placement::Markdown { root, file }) => {
            let path = Path::new(root).join(file);
            let current = std::fs::read_to_string(&path)
                .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
            let struck = strike(&current, "[inferred] Rule:", &today);
            let line = format!("- [inferred] Rule: {text} ({today})");
            std::fs::write(
                &path,
                insert_after_struck(&struck, "[inferred] Rule:", &line),
            )
            .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
            let index = Path::new(root).join("MEMORY.md");
            if let Ok(current) = std::fs::read_to_string(&index) {
                let struck = strike(&current, file, &today);
                let line = format!(
                    "- {} (learned {today}, {}) - `{file}`",
                    clip(text, 160).replace('\n', " "),
                    lesson.id
                );
                std::fs::write(&index, with_decision(&struck, &line))
                    .map_err(|error| format!("cannot write {}: {error}", index.display()))?;
            }
            Ok(())
        }
        Some(Placement::MarkdownFact { root, file }) => {
            let path = Path::new(root).join(file);
            let current = std::fs::read_to_string(&path)
                .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
            let struck = strike(&current, &lesson.id, &today);
            let line = format!(
                "- [observed] {} ({today}, {})",
                text.replace('\n', " "),
                lesson.id
            );
            std::fs::write(&path, under_heading(&struck, "## Facts", &line))
                .map_err(|error| format!("cannot write {}: {error}", path.display()))
        }
    }
}

/// `text` with `line` right after the last struck line holding `marker`.
fn insert_after_struck(text: &str, marker: &str, line: &str) -> String {
    let lines = text.lines().collect::<Vec<_>>();
    let at = lines
        .iter()
        .rposition(|text| text.contains(marker) && text.contains("~~"))
        .map_or(lines.len(), |index| index + 1);
    let mut out = lines[..at]
        .iter()
        .map(|text| (*text).to_owned())
        .collect::<Vec<_>>();
    out.push(line.to_owned());
    out.extend(lines[at..].iter().map(|text| (*text).to_owned()));
    let mut text = out.join("\n");
    text.push('\n');
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signal(conversation: &str, evidence: &str, days_ago: i64, against: bool) -> Signal {
        Signal {
            at: (Utc::now() - chrono::Duration::days(days_ago)).to_rfc3339(),
            conversation: conversation.to_owned(),
            against,
            evidence: evidence.to_owned(),
        }
    }

    fn lesson(id: &str, signals: Vec<Signal>) -> Lesson {
        Lesson {
            id: id.to_owned(),
            text: "when the user asks a question, answer it before changing code".to_owned(),
            kind: "correction".to_owned(),
            signals,
            ..Lesson::default()
        }
    }

    fn seed(dir: &Path, lessons: Vec<Lesson>) {
        std::fs::create_dir_all(dir).expect("dir");
        save(
            dir,
            &Lessons {
                schema: 1,
                lessons,
                ..Lessons::default()
            },
        )
        .expect("seed");
    }

    #[test]
    fn the_gate_counts_moments_conversations_and_age() {
        let now = Utc::now();
        let same = lesson(
            "les_a",
            vec![
                signal("c1", "just answer my question", 1, false),
                signal("c1", "Just answer  my question", 1, false),
                signal("c1", "I only asked", 1, false),
            ],
        );
        let standing = same.standing(now);
        assert!(
            (standing.weight - 2.0).abs() < f64::EPSILON,
            "one quote read twice is one"
        );
        assert_eq!(standing.conversations, 1);
        let old = lesson(
            "les_b",
            vec![
                signal("c1", "a", 40, false),
                signal("c2", "b", 1, false),
                signal("c3", "c", 1, true),
            ],
        );
        let standing = old.standing(now);
        assert!(
            (standing.weight - 1.5).abs() < f64::EPSILON,
            "the old one counts half"
        );
        assert_eq!((standing.conversations, standing.contrary), (2, 1));
    }

    #[test]
    fn a_signal_needs_a_verbatim_quote_and_names_its_lesson() {
        let dir = tempfile::tempdir().expect("dir");
        let conversation =
            "user: you changed the file again.\nuser: Just answer my question, do not edit.";
        let records = record(
            dir.path(),
            &[
                json!({"text": "when asked a question, answer without editing", "kind": "correction", "scope": "global", "evidence": "just answer my   question"}),
                json!({"text": "invented", "kind": "correction", "evidence": "words nobody said"}),
                json!({"lesson": "les_missing", "evidence": "you changed the file again"}),
                json!({"text": "x", "kind": "mood", "evidence": "you changed the file again"}),
                json!({"text": "a word is no moment", "kind": "correction", "evidence": "again"}),
            ],
            conversation,
            "c1",
            None,
            &BTreeSet::new(),
        )
        .expect("record");
        assert_eq!(records[0]["applied"], true, "{records:?}");
        assert_eq!(
            records[1]["error"],
            "the evidence is not a verbatim quote of the conversation, a phrase long"
        );
        assert_eq!(records[2]["error"], "no open lesson les_missing");
        assert_eq!(
            records[3]["error"],
            "kind must be correction, deviation, resolution or preference"
        );
        assert_eq!(
            records[4]["applied"], false,
            "a quote of one word: {:?}",
            records[4]
        );
        let id = records[0]["id"].as_str().expect("an id").to_owned();
        let again = record(
            dir.path(),
            &[json!({"lesson": id, "evidence": "you changed the file again"})],
            conversation,
            "c2",
            None,
            &BTreeSet::new(),
        )
        .expect("record");
        assert_eq!(again[0]["action"], "repeat");
        let lessons = load(dir.path()).expect("load");
        assert_eq!(lessons.lessons.len(), 1);
        assert_eq!(lessons.lessons[0].signals.len(), 2);
        assert!(prompt_part(dir.path(), None).contains(&id));
    }

    /// ECC's confidence scale: one or two signals 0.3, three to five 0.5, six
    /// to ten 0.7, beyond 0.85, each contradicting conversation 0.1 off.
    #[test]
    fn confidence_grows_with_the_case_and_falls_with_contradiction() {
        let signals = |count: usize| {
            (0..count)
                .map(|index| signal(&format!("c{index}"), &format!("quote {index}"), 1, false))
                .collect::<Vec<_>>()
        };
        let now = Utc::now();
        assert!((lesson("a", signals(2)).confidence(now) - 0.3).abs() < 1e-9);
        assert!((lesson("b", signals(4)).confidence(now) - 0.5).abs() < 1e-9);
        assert!((lesson("c", signals(7)).confidence(now) - 0.7).abs() < 1e-9);
        assert!((lesson("d", signals(12)).confidence(now) - 0.85).abs() < 1e-9);
        let mut contradicted = signals(4);
        contradicted.push(signal("x", "not so", 1, true));
        assert!((lesson("e", contradicted).confidence(now) - 0.4).abs() < 1e-9);
    }

    /// The review may report an error's fix and a preference, with an area of
    /// work; a project's resolution is what it showed, a fact; an unknown
    /// domain reads as other.
    #[test]
    fn a_resolution_and_a_preference_are_recorded_with_their_domain() {
        let dir = tempfile::tempdir().expect("dir");
        let project = ProjectRef {
            key: "p1".to_owned(),
            name: "orion".to_owned(),
            root: dir.path().display().to_string(),
        };
        let conversation =
            "tool: error: linker `link.exe` not found\nuser: always use rg instead of grep here";
        let records = record(
            dir.path(),
            &[
                json!({"text": "when the linker is missing, run vcvars64 first", "kind": "resolution", "scope": "project", "domain": "debugging", "evidence": "linker `link.exe` not found"}),
                json!({"text": "when searching code, use rg", "kind": "preference", "scope": "global", "domain": "made-up", "evidence": "always use rg instead of grep"}),
                json!({"text": "x", "kind": "guess", "scope": "global", "evidence": "always use rg instead of grep"}),
            ],
            conversation,
            "c1",
            Some(&project),
            &BTreeSet::new(),
        )
        .expect("record");
        assert_eq!(records[0]["applied"], true, "{records:?}");
        assert_eq!(records[1]["applied"], true, "{records:?}");
        assert_eq!(records[2]["applied"], false, "{records:?}");
        let lessons = load(dir.path()).expect("load");
        assert_eq!(lessons.lessons[0].tier, Tier::Fact);
        assert_eq!(lessons.lessons[0].domain(), "debugging");
        assert_eq!(lessons.lessons[1].tier, Tier::Rule);
        assert_eq!(lessons.lessons[1].domain(), "other");
        assert!(status(&lessons, Utc::now()).contains("## debugging (1)"));
    }

    /// An export carries the case; importing it on another machine adds the
    /// lesson, importing it again adds nothing, and a project lesson needs a
    /// project to land in.
    #[test]
    #[allow(clippy::too_many_lines, reason = "one export, imported four ways")]
    fn an_exported_lesson_is_imported_once() {
        let source = tempfile::tempdir().expect("source");
        let target = tempfile::tempdir().expect("target");
        let root = tempfile::tempdir().expect("root");
        seed(
            source.path(),
            vec![
                lesson(
                    "les_shared",
                    vec![
                        signal("c1", "a long enough quote", 2, false),
                        signal("c2", "another quote here", 1, false),
                        signal("c2", "short", 1, false),
                    ],
                ),
                project_lesson(
                    "les_local",
                    root.path(),
                    "correction",
                    vec![signal("c1", "make ci, not cargo", 1, false)],
                ),
            ],
        );
        let document = export(
            &load(source.path()).expect("load"),
            Utc::now(),
            0.0,
            ScopeFilter::All,
        );
        assert_eq!(document["lessons"].as_array().map(Vec::len), Some(2));
        let dry =
            import(target.path(), &document, 0.0, ScopeFilter::All, None, true).expect("dry run");
        assert_eq!(dry.added.len(), 1, "{dry:?}");
        assert!(load(target.path()).expect("load").lessons.is_empty());
        // Another project's lesson does not land here.
        let elsewhere = ProjectRef {
            key: "p9".to_owned(),
            name: "elsewhere".to_owned(),
            root: root.path().display().to_string(),
        };
        let first = import(
            target.path(),
            &document,
            0.0,
            ScopeFilter::All,
            Some(&elsewhere),
            false,
        )
        .expect("import");
        assert_eq!(first.added.len(), 1, "{first:?}");
        assert_ne!(first.added[0], "les_shared", "the id is this machine's");
        assert!(
            first
                .skipped
                .iter()
                .any(|(id, why)| id == "les_local" && why.contains("project \"Orion\"")),
            "{first:?}"
        );
        let kept = load(target.path()).expect("load");
        assert!(
            kept.lessons[0]
                .signals
                .iter()
                .all(|signal| signal.conversation.starts_with("import:")),
        );
        // The quote under a phrase was dropped, as the review drops it.
        assert_eq!(kept.lessons[0].signals.len(), 2);
        let again =
            import(target.path(), &document, 0.0, ScopeFilter::All, None, false).expect("again");
        assert!(
            again.added.is_empty() && again.merged.is_empty(),
            "{again:?}"
        );
        assert_eq!(
            load(target.path()).expect("load").lessons[0].signals.len(),
            2
        );
        // An export of these, imported back where they came from, adds no case.
        let back = export(
            &load(target.path()).expect("load"),
            Utc::now(),
            0.0,
            ScopeFilter::All,
        );
        let home =
            import(source.path(), &back, 0.0, ScopeFilter::All, None, false).expect("back home");
        assert!(home.added.is_empty() && home.merged.is_empty(), "{home:?}");
        assert_eq!(
            load(source.path())
                .expect("load")
                .find("les_shared")
                .expect("lesson")
                .signals
                .len(),
            3
        );
        // A lesson retired here stays retired.
        let places = Places {
            global: target.path().to_path_buf(),
        };
        retire(&places, &kept.lessons[0].id, "not wanted").expect("retire");
        let retired =
            import(target.path(), &document, 0.0, ScopeFilter::All, None, false).expect("retired");
        assert!(retired.added.is_empty(), "{retired:?}");
        assert!(
            retired
                .skipped
                .iter()
                .any(|(_, why)| why.contains("retired here"))
        );
    }

    /// A project's rule made one for every project gives its project rule
    /// back and waits for the gate; a fact stays with its project.
    #[test]
    fn a_project_lesson_is_promoted_to_every_project() {
        let dir = tempfile::tempdir().expect("dir");
        let root = tempfile::tempdir().expect("root");
        seed(
            dir.path(),
            vec![
                project_lesson(
                    "les_rule",
                    root.path(),
                    "correction",
                    vec![signal("c1", "make ci, not cargo", 1, false)],
                ),
                project_lesson(
                    "les_fact",
                    root.path(),
                    "deviation",
                    vec![signal("c1", "no such file", 1, false)],
                ),
            ],
        );
        let places = Places {
            global: dir.path().to_path_buf(),
        };
        let promoted = promote_global(&places, "les_rule").expect("promote");
        assert!(promoted.project.is_none());
        assert_eq!(promoted.state, LessonState::Candidate);
        assert!(promote_global(&places, "les_rule").is_err());
        assert!(
            promote_global(&places, "les_fact")
                .expect_err("a fact")
                .contains("fact")
        );
    }

    /// The meditation makes one lesson for every project of the same rule in
    /// two projects; one project's lesson alone is left as it is.
    #[test]
    fn the_same_rule_in_two_projects_is_generalized() {
        let dir = tempfile::tempdir().expect("dir");
        let root = tempfile::tempdir().expect("root");
        let mut other = project_lesson(
            "les_two",
            root.path(),
            "correction",
            vec![signal("c2", "run the tests first", 1, false)],
        );
        if let Some(project) = other.project.as_mut() {
            project.key = "p2".to_owned();
            project.name = "Vega".to_owned();
        }
        let mut lessons = Lessons {
            schema: 1,
            lessons: vec![
                project_lesson(
                    "les_one",
                    root.path(),
                    "correction",
                    vec![signal("c1", "run the tests first", 1, false)],
                ),
                other,
                project_lesson(
                    "les_alone",
                    root.path(),
                    "correction",
                    vec![signal("c3", "other quote", 1, false)],
                ),
            ],
            ..Lessons::default()
        };
        let places = Places {
            global: dir.path().to_path_buf(),
        };
        let answer = json!({"generalize": [
            {"from": ["les_one", "les_two"], "text": "when a change is done, run the tests first"},
            {"from": ["les_alone"]},
        ]});
        let changes = generalize(&places, &mut lessons, &answer);
        assert_eq!(changes.len(), 1, "{changes:?}");
        let general = lessons
            .lessons
            .iter()
            .find(|lesson| lesson.text == "when a change is done, run the tests first")
            .expect("the general lesson");
        assert!(general.project.is_none());
        assert_eq!(general.signals.len(), 2);
        let state = |id: &str| lessons.find(id).expect("lesson").state;
        assert_eq!(state("les_one"), LessonState::Retired);
        assert_eq!(state("les_two"), LessonState::Retired);
        assert_eq!(state("les_alone"), LessonState::Candidate);
    }

    /// A correction the review wrote into a learned skill is that skill: the
    /// loop writes no rule for it, and retiring it leaves the skill.
    #[test]
    fn a_lesson_the_review_wrote_into_a_skill_is_that_skill() {
        let dir = tempfile::tempdir().expect("dir");
        let conversation = "user: no, run the migration in a transaction";
        let item = json!({"text": "when migrating, run it in a transaction", "kind": "correction", "scope": "global", "evidence": "run the migration in a transaction", "saved_as": "skill:db-migrations"});
        let saved = BTreeSet::from(["skill:db-migrations".to_owned()]);
        let records =
            record(dir.path(), &[item], conversation, "c1", None, &saved).expect("record");
        assert_eq!(records[0]["saved_as"], "skill:db-migrations", "{records:?}");
        let lessons = load(dir.path()).expect("load");
        assert_eq!(
            lessons.lessons[0].placement,
            Some(Placement::Skill {
                name: "db-migrations".to_owned()
            })
        );
        let places = Places {
            global: dir.path().to_path_buf(),
        };
        // A lesson whose skill is gone is a candidate again; one whose skill
        // is there stays the skill.
        reconcile_skills(&places, &BTreeSet::from(["db-migrations".to_owned()]));
        assert_eq!(
            load(dir.path()).expect("load").lessons[0].state,
            LessonState::Promoted
        );
        reconcile_skills(&places, &BTreeSet::new());
        let lessons = load(dir.path()).expect("load");
        assert_eq!(lessons.lessons[0].state, LessonState::Candidate);
        assert_eq!(lessons.lessons[0].placement, None);
        // Retiring a lesson a skill held leaves nothing to undo.
        retire(&places, &lessons.lessons[0].id, "test").expect("retire");
    }

    /// A lesson follows its entry: a learned rule the user deleted retires
    /// it; a covering entry merged away is found again by what it says, and
    /// with nothing saying it the lesson is a candidate again.
    #[test]
    fn a_lesson_whose_entry_is_gone_is_reconciled() {
        // As in ha: the global store is `<data>/harness`, where the memory
        // host looks for an entry that says the lesson.
        let data = tempfile::tempdir().expect("dir");
        let global = super::super::harness::global_dir(data.path());
        std::fs::create_dir_all(&global).expect("global");
        let places = Places {
            global: global.clone(),
        };
        let mut covered = lesson("les_cov", vec![signal("c1", "a", 1, false)]);
        covered.state = LessonState::Promoted;
        covered.placement = Some(Placement::Covered {
            key: "merged_away".to_owned(),
        });
        let mut orphan = lesson("les_orphan", vec![signal("c1", "a", 1, false)]);
        orphan.text = "when a migration runs, back the table up first".to_owned();
        orphan.state = LessonState::Promoted;
        orphan.placement = Some(Placement::Covered {
            key: "gone_too".to_owned(),
        });
        let mut ready = lesson(
            "les_ready",
            vec![
                signal("c1", "a", 2, false),
                signal("c1", "b", 2, false),
                signal("c2", "c", 1, false),
            ],
        );
        ready.text = "when a test is flaky, rerun it alone before changing it".to_owned();
        seed(&global, vec![ready, covered, orphan]);
        let mut state = super::super::refine::load_raw(&global);
        state["entries"]["memory"]["answer_first"] = json!({
            "id": "answer_first", "kind": "memory", "title": "Answer before changing code",
            "content": "when the user asks a question, answer it before changing code",
            "path": "general", "scope": "global", "version": 1,
        });
        super::super::refine::save_raw(
            &global,
            &state,
            &super::super::memory_store::Writer::new("test", &json!({}), "test"),
        )
        .expect("save");
        distill(&places, Utc::now()).expect("distill");
        let mut state = super::super::refine::load_raw(&global);
        state["entries"]["memory"]
            .as_object_mut()
            .expect("entries")
            .remove("learned_ready");
        super::super::refine::save_raw(
            &global,
            &state,
            &super::super::memory_store::Writer::new("test", &json!({}), "test"),
        )
        .expect("save");
        let distilled = distill(&places, Utc::now()).expect("distill");
        assert!(
            distilled
                .retired
                .iter()
                .any(|(id, why)| id == "les_ready" && why.contains("deleted outside")),
            "{distilled:?}"
        );
        let lessons = load(&global).expect("load");
        let find = |id: &str| {
            lessons
                .lessons
                .iter()
                .find(|lesson| lesson.id == id)
                .expect("lesson")
                .clone()
        };
        assert_eq!(
            find("les_cov").placement,
            Some(Placement::Covered {
                key: "answer_first".to_owned()
            })
        );
        assert_eq!(find("les_orphan").state, LessonState::Candidate);
        assert_eq!(find("les_orphan").placement, None);
    }

    #[test]
    fn a_global_lesson_that_passes_becomes_a_constraints_entry_and_retires_cleanly() {
        let dir = tempfile::tempdir().expect("dir");
        seed(
            dir.path(),
            vec![
                lesson(
                    "les_ready",
                    vec![
                        signal("c1", "a", 2, false),
                        signal("c1", "b", 2, false),
                        signal("c2", "c", 1, false),
                    ],
                ),
                lesson(
                    "les_one",
                    vec![
                        signal("c1", "a", 1, false),
                        signal("c1", "b", 1, false),
                        signal("c1", "c", 1, false),
                    ],
                ),
            ],
        );
        let places = Places {
            global: dir.path().to_path_buf(),
        };
        let distilled = distill(&places, Utc::now()).expect("distill");
        assert_eq!(distilled.promoted.len(), 1, "{distilled:?}");
        assert_eq!(distilled.promoted[0].0, "les_ready");
        let state = super::super::refine::load_raw(dir.path());
        let entry = &state["entries"]["memory"]["learned_ready"];
        assert_eq!(entry["path"], "constraints/learned");
        assert!(
            entry["content"]
                .as_str()
                .expect("content")
                .contains("lesson les_ready")
        );
        // A second run promotes nothing new.
        assert!(distill(&places, Utc::now()).expect("distill").is_empty());
        retire(&places, "les_ready", "no longer holds").expect("retire");
        let state = super::super::refine::load_raw(dir.path());
        assert!(state["entries"]["memory"]["learned_ready"].is_null());
        assert_eq!(
            load(dir.path())
                .expect("load")
                .find("les_ready")
                .expect("kept")
                .state,
            LessonState::Retired
        );
    }

    #[test]
    fn contradiction_holds_a_lesson_back_and_two_retire_it() {
        let dir = tempfile::tempdir().expect("dir");
        let mut signals = vec![
            signal("c1", "a", 1, false),
            signal("c2", "b", 1, false),
            signal("c3", "c", 1, false),
            signal("c4", "no", 1, true),
        ];
        seed(dir.path(), vec![lesson("les_x", signals.clone())]);
        let places = Places {
            global: dir.path().to_path_buf(),
        };
        let distilled = distill(&places, Utc::now()).expect("distill");
        assert!(distilled.promoted.is_empty());
        assert_eq!(distilled.held[0].1, "a conversation contradicted it");
        signals.push(signal("c5", "no again", 1, true));
        seed(dir.path(), vec![lesson("les_x", signals)]);
        let distilled = distill(&places, Utc::now()).expect("distill");
        assert_eq!(distilled.retired[0].0, "les_x");
    }

    #[test]
    fn the_ceilings_hold() {
        let dir = tempfile::tempdir().expect("dir");
        let ready = |id: &str| {
            lesson(
                id,
                vec![
                    signal("c1", "a", 1, false),
                    signal("c2", "b", 1, false),
                    signal("c3", "c", 1, false),
                ],
            )
        };
        seed(
            dir.path(),
            vec![ready("les_1"), ready("les_2"), ready("les_3")],
        );
        let places = Places {
            global: dir.path().to_path_buf(),
        };
        let first = distill(&places, Utc::now()).expect("distill");
        assert_eq!(first.promoted.len(), MAX_PROMOTIONS_PER_RUN);
        assert_eq!(first.held.len(), 1);
        let second = distill(&places, Utc::now()).expect("distill");
        assert_eq!(second.promoted.len(), 1, "held only for this run");
    }

    #[test]
    fn a_project_lesson_goes_to_its_markdown_store_and_is_struck_on_retirement() {
        let dir = tempfile::tempdir().expect("dir");
        let project = tempfile::tempdir().expect("project");
        super::super::markdown_memory::init(project.path(), false).expect("init");
        let mut ready = lesson(
            "les_md",
            vec![
                signal("c1", "the build is make ci", 1, false),
                signal("c2", "use make ci", 1, false),
                signal("c3", "make ci again", 1, false),
            ],
        );
        ready.text =
            "when building this project, the build is `make ci`, not cargo build".to_owned();
        ready.project = Some(ProjectRef {
            key: "p1".to_owned(),
            name: "proj".to_owned(),
            root: project.path().display().to_string(),
        });
        seed(dir.path(), vec![ready]);
        let places = Places {
            global: dir.path().to_path_buf(),
        };
        distill(&places, Utc::now()).expect("distill");
        let lessons = load(dir.path()).expect("load");
        let Some(Placement::Markdown { file, .. }) = &lessons.lessons[0].placement else {
            panic!("placed in markdown: {:?}", lessons.lessons[0].placement);
        };
        let index = std::fs::read_to_string(project.path().join("MEMORY.md")).expect("index");
        assert!(index.contains(&format!("`{file}`")), "{index}");
        let decisions = index
            .split("## Current Decisions")
            .nth(1)
            .and_then(|rest| rest.split("\n---").next())
            .expect("the section");
        assert!(!decisions.contains("(none yet)"), "{decisions}");
        assert!(decisions.contains("les_md"), "{decisions}");
        assert!(
            index
                .split("## Current Decisions")
                .next()
                .expect("before")
                .contains("(none yet)"),
            "Active Context keeps its placeholder: {index}"
        );
        let report =
            super::super::markdown_memory::lint(&super::super::markdown_memory::LintOptions {
                root: project.path().to_path_buf(),
                config: None,
                mode: super::super::markdown_memory::DiffMode::None,
            })
            .expect("lint");
        assert!(report.passed(), "{}", report.text(false));
        let decision = std::fs::read_to_string(project.path().join(file)).expect("decision");
        assert!(
            decision
                .lines()
                .filter(|line| line.starts_with("- "))
                .all(|line| line.starts_with("- [")),
            "{decision}"
        );
        retire(&places, "les_md", "the build changed").expect("retire");
        let decision = std::fs::read_to_string(project.path().join(file)).expect("decision");
        assert!(decision.contains("- ~~[inferred] Rule:"), "{decision}");
        let index = std::fs::read_to_string(project.path().join("MEMORY.md")).expect("index");
        assert!(index.contains("~~ retired "), "{index}");
    }

    #[test]
    fn a_broken_lessons_file_is_reported_not_overwritten() {
        let dir = tempfile::tempdir().expect("dir");
        std::fs::write(dir.path().join(FILE), "{broken").expect("write");
        assert!(load(dir.path()).is_err());
        assert!(
            record(
                dir.path(),
                &[json!({"text": "t", "kind": "correction", "evidence": "x"})],
                "x",
                "c",
                None,
                &BTreeSet::new()
            )
            .is_err()
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join(FILE)).expect("kept"),
            "{broken"
        );
    }

    /// A project with a markdown store keeps its rules in it: a memory entry
    /// the review wrote about the project does not cover the lesson.
    #[test]
    fn a_project_lesson_with_a_store_is_not_covered_by_memory() {
        let dir = tempfile::tempdir().expect("dir");
        let root = tempfile::tempdir().expect("root");
        std::fs::write(
            root.path().join(super::super::markdown_memory::CONFIG_FILE),
            "{}",
        )
        .expect("config");
        let project = ProjectRef {
            key: "p1".to_owned(),
            name: "q".to_owned(),
            root: root.path().display().to_string(),
        };
        let conversation = "user: this project builds with make ci, not cargo build";
        let item = json!({"text": "when building this project, use make ci", "kind": "correction", "scope": "project", "evidence": "builds with make ci, not cargo build", "saved_as": "q_build"});
        let saved = BTreeSet::from(["q_build".to_owned()]);
        let records = record(
            dir.path(),
            &[item],
            conversation,
            "c1",
            Some(&project),
            &saved,
        )
        .expect("record");
        assert!(records[0]["saved_as_ignored"].is_string(), "{records:?}");
        let lessons = load(dir.path()).expect("load");
        assert_eq!(lessons.lessons[0].state, LessonState::Candidate);
        assert_eq!(lessons.lessons[0].placement, None);
    }

    /// The user's own rule, saved as memory by the review, is kept once: the
    /// lesson is that entry, and a `saved_as` the review does not write is
    /// not believed.
    #[test]
    fn a_lesson_the_review_saved_as_memory_is_that_entry() {
        let dir = tempfile::tempdir().expect("dir");
        let conversation = "user: from now on, only answer when I ask a question";
        let item = |saved_as: &str| json!({"text": "when the user asks, answer without editing", "kind": "correction", "scope": "global", "evidence": "only answer when I ask a question", "saved_as": saved_as});
        let saved = BTreeSet::from(["answer_only".to_owned()]);
        let records = record(
            dir.path(),
            &[item("answer_only"), item("not_written")],
            conversation,
            "c1",
            None,
            &saved,
        )
        .expect("record");
        assert_eq!(records[0]["saved_as"], "answer_only", "{records:?}");
        assert!(records[1]["saved_as_ignored"].is_string(), "{records:?}");
        let lessons = load(dir.path()).expect("load");
        assert_eq!(lessons.lessons[0].state, LessonState::Promoted);
        assert_eq!(
            lessons.lessons[0].placement,
            Some(Placement::Covered {
                key: "answer_only".to_owned()
            })
        );
        assert_eq!(lessons.lessons[1].state, LessonState::Candidate);
        // A covered lesson is never written again, and retiring it leaves the
        // user's entry alone.
        let places = Places {
            global: dir.path().to_path_buf(),
        };
        assert!(
            distill(&places, Utc::now())
                .expect("distill")
                .promoted
                .is_empty()
        );
        retire(&places, &lessons.lessons[0].id, "test").expect("retire");
        // A failed global part makes the covered lesson a candidate again.
        let records = record(
            dir.path(),
            &[item("answer_only")],
            conversation,
            "c2",
            None,
            &saved,
        )
        .expect("record");
        let id = records[0]["id"].as_str().expect("id").to_owned();
        uncover(dir.path(), &BTreeSet::from([id.clone()]));
        let lessons = load(dir.path()).expect("load");
        assert_eq!(
            lessons.find(&id).expect("kept").state,
            LessonState::Candidate
        );
    }

    /// A lesson that passes while the memory already says it is promoted as
    /// that entry: no second entry is written.
    #[test]
    fn a_rule_the_memory_holds_is_not_written_twice() {
        let data = tempfile::tempdir().expect("data");
        let global = data.path().join("harness");
        let mut state = super::super::refine::load_raw(&global);
        for (key, title, content) in [
            (
                "answer_only",
                "Answer questions",
                "when the user asks a question, answer it without editing any file",
            ),
            (
                "tabs",
                "Indentation",
                "the user indents with tabs in every project",
            ),
            ("lang", "Language", "the user writes in Vietnamese"),
        ] {
            state["entries"]["memory"][key] = json!({"id": key, "kind": "memory", "title": title, "content": content, "path": "general", "scope": "global", "version": 1, "metadata": {}});
        }
        super::super::refine::save_raw(
            &global,
            &state,
            &super::super::memory_store::Writer::new("cli", &json!({}), "create"),
        )
        .expect("seed memory");
        let mut ready = lesson(
            "les_dup",
            vec![
                signal("c1", "a", 1, false),
                signal("c2", "b", 1, false),
                signal("c3", "c", 1, false),
            ],
        );
        ready.text = "when the user asks a question, answer it without editing any file".to_owned();
        seed(&global, vec![ready]);
        let places = Places {
            global: global.clone(),
        };
        let distilled = distill(&places, Utc::now()).expect("distill");
        assert_eq!(distilled.promoted.len(), 1, "{distilled:?}");
        assert!(
            distilled.promoted[0]
                .1
                .contains("the memory holds it as answer_only"),
            "{distilled:?}"
        );
        let state = super::super::refine::load_raw(&global);
        assert!(
            state["entries"]["memory"]["learned_dup"].is_null(),
            "no second entry"
        );
    }

    fn project_lesson(id: &str, root: &Path, kind: &str, signals: Vec<Signal>) -> Lesson {
        Lesson {
            id: id.to_owned(),
            text: "when building this project, the build command is `make ci`".to_owned(),
            kind: kind.to_owned(),
            project: Some(ProjectRef {
                key: "p1".to_owned(),
                name: "Orion".to_owned(),
                root: root.display().to_string(),
            }),
            tier: if kind == "deviation" {
                Tier::Fact
            } else {
                Tier::Rule
            },
            signals,
            ..Lesson::default()
        }
    }

    /// A project's wrong assumption is a fact seen once: kept at once, in the
    /// project's file, where the lint passes it; a retirement strikes it.
    #[test]
    fn a_fact_is_kept_on_its_first_sighting_and_struck_when_retired() {
        let dir = tempfile::tempdir().expect("dir");
        let project = tempfile::tempdir().expect("project");
        super::super::markdown_memory::init(project.path(), false).expect("init");
        seed(
            dir.path(),
            vec![project_lesson(
                "les_fact",
                project.path(),
                "deviation",
                vec![signal("c1", "the build failed: no Cargo.toml", 1, false)],
            )],
        );
        let places = Places {
            global: dir.path().to_path_buf(),
        };
        let distilled = distill(&places, Utc::now()).expect("distill");
        assert_eq!(distilled.facts.len(), 1, "{distilled:?}");
        assert!(distilled.lines()[0].starts_with("learned fact les_fact"));
        let file = project.path().join("memory/projects/orion.md");
        let text = std::fs::read_to_string(&file).expect("project file");
        assert!(
            text.contains("## Facts") && text.contains("- [observed] when building this project"),
            "{text}"
        );
        let index = std::fs::read_to_string(project.path().join("MEMORY.md")).expect("index");
        assert!(
            index.contains("| Orion | `memory/projects/orion.md` | Orion |"),
            "{index}"
        );
        let report =
            super::super::markdown_memory::lint(&super::super::markdown_memory::LintOptions {
                root: project.path().to_path_buf(),
                config: None,
                mode: super::super::markdown_memory::DiffMode::None,
            })
            .expect("lint");
        assert!(report.passed(), "{}", report.text(false));
        retire(&places, "les_fact", "the project moved to cargo").expect("retire");
        let text = std::fs::read_to_string(&file).expect("project file");
        assert!(text.contains("- ~~[observed] when building"), "{text}");
    }

    #[test]
    fn a_fact_without_a_store_is_a_ranked_entry_of_its_project() {
        let dir = tempfile::tempdir().expect("dir");
        let project = tempfile::tempdir().expect("project");
        let global = dir.path().join("harness");
        seed(
            &global,
            vec![project_lesson(
                "les_bare",
                project.path(),
                "deviation",
                vec![signal("c1", "the build failed: no Cargo.toml", 1, false)],
            )],
        );
        distill(
            &Places {
                global: global.clone(),
            },
            Utc::now(),
        )
        .expect("distill");
        let state = super::super::refine::load_raw(&global);
        let entry = &state["entries"]["memory"]["learned_bare"];
        assert_eq!(entry["path"], "facts/learned", "not in every turn: {entry}");
        assert_eq!(entry["project"]["key"], "p1");
        // A correction about the project still waits for the gate.
        let mut rule = project_lesson(
            "les_rule",
            project.path(),
            "correction",
            vec![signal("c1", "use make ci", 1, false)],
        );
        rule.text = "when the user asks for a build, use make ci".to_owned();
        seed(&global, vec![rule]);
        assert!(
            distill(&Places { global }, Utc::now())
                .expect("distill")
                .is_empty()
        );
    }

    /// A rule the conversations keep correcting after it became one is
    /// named: the rule does not work.
    #[test]
    fn a_rule_still_corrected_after_promotion_is_named() {
        let now = Utc::now();
        let mut rule = lesson(
            "les_r",
            vec![
                signal("c1", "a", 9, false),
                signal("c2", "b", 8, false),
                signal("c3", "c", 7, false),
            ],
        );
        rule.state = LessonState::Promoted;
        rule.promoted_at = Some((now - chrono::Duration::days(6)).to_rfc3339());
        assert!(!rule.ineffective(now));
        assert!(effect(&rule, now).contains("not corrected since it became a rule"));
        rule.signals.extend([
            signal("c4", "d", 2, false),
            signal("c5", "e", 1, false),
            signal("c5", "f", 1, false),
        ]);
        assert!(rule.ineffective(now));
        let lessons = Lessons {
            lessons: vec![rule],
            ..Lessons::default()
        };
        let text = report(&lessons, now, false);
        assert!(
            text.starts_with("## Rules still corrected (reword or retire them) (1)"),
            "{text}"
        );
        assert!(text.contains("STILL CORRECTED"), "{text}");
        let page = report_html(&lessons, now, false);
        assert!(
            page.starts_with("<!doctype html>") && page.contains("<h2>Rules still corrected"),
            "{page}"
        );
    }

    /// The meditation's answer applied: one lesson in two wordings becomes
    /// one, a rule is reworded in a new version of its entry, and a lesson is
    /// retired; a rule the user wrote is not reworded.
    #[test]
    fn a_meditation_merges_rewords_and_retires() {
        let dir = tempfile::tempdir().expect("dir");
        let places = Places {
            global: dir.path().to_path_buf(),
        };
        let original = "when the user asks a question, answer it before changing code".to_owned();
        let mut covered = lesson("les_user", vec![signal("c1", "a", 1, false)]);
        covered.state = LessonState::Promoted;
        covered.placement = Some(Placement::Covered {
            key: "answer_only".to_owned(),
        });
        let mut written = lesson("les_rule", vec![signal("c1", "a", 1, false)]);
        written.state = LessonState::Promoted;
        written.placement = Some(
            place_memory(dir.path(), &written, &written.standing(Utc::now())).expect("placed"),
        );
        seed(
            dir.path(),
            vec![
                lesson(
                    "les_a",
                    vec![signal("c1", "answer only, do not edit", 2, false)],
                ),
                lesson(
                    "les_b",
                    vec![signal("c2", "chỉ trả lời, đừng sửa", 1, false)],
                ),
                lesson("les_old", vec![signal("c3", "old", 1, false)]),
                covered,
                written,
            ],
        );
        assert!(meditation_due(dir.path(), Utc::now()));
        let answer = json!({
            "merge": [{"into": "les_a", "from": ["les_b"], "text": "when the user only asks, answer without editing"}],
            "reword": [
                {"id": "les_rule", "text": "when the user asks a question, answer first and edit only when asked", "why": "still corrected"},
                {"id": "les_user", "text": "anything", "why": "x"}
            ],
            "retire": [{"id": "les_old", "why": "the later quote contradicts it"}],
            "synthesis": "The user wants answers before edits."
        });
        let mut lessons = load(dir.path()).expect("load");
        let changes = apply_meditation(&places, &mut lessons, &answer, Utc::now());
        assert_eq!(changes.len(), 3, "{changes:?}");
        let merged = lessons.find("les_a").expect("kept");
        assert_eq!(merged.signals.len(), 2);
        assert_eq!(
            merged.text,
            "when the user only asks, answer without editing"
        );
        assert_eq!(
            lessons.find("les_b").expect("kept").state,
            LessonState::Retired
        );
        assert_eq!(
            lessons.find("les_old").expect("kept").state,
            LessonState::Retired
        );
        assert_eq!(lessons.find("les_user").expect("kept").text, original);
        let state = super::super::refine::load_raw(dir.path());
        let entry = &state["entries"]["memory"]["learned_rule"];
        assert_eq!(entry["version"], 2, "a new version: {entry}");
        assert!(
            entry["content"]
                .as_str()
                .expect("content")
                .starts_with("when the user asks a question, answer first")
        );
    }
}
