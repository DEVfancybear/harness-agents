//! Statements with evidence: footnotes, event times and paragraph anchors
//! in a page's Markdown, and what the host derives from them.
//!
//! "Can LLM Agents Organize Their Own Memory?" (`SoMem`, arXiv 2611.11929,
//! Fu et al.) keeps memory as Markdown documents the agent revises freely,
//! where each statement carries a footnote with its event time and the
//! Source records that support it, a paragraph has a stable identifier other
//! documents link to, and the runtime - not the model - checks every edit
//! before accepting it and derives the views (Graph, Timeline, Recent) from
//! what the documents say. Its ablation found the evidence references the
//! part that matters; the views are ways to reach them. ha takes the same
//! grammar in ordinary Markdown, so a page stays a page:
//!
//! ```text
//! Joined A in May.[^e1] Moved to B in August.[^e2] ^work-2026
//!
//! [^e1]: 2026-05; src/hr.rs:12-14, event_01a0f16c-740a-7185-8a4a5bf83b00
//! [^e2]: 2026-08; https://example.com/news
//! ```
//!
//! - A marker `[^id]` follows the statement it supports. The id is 1 to 32
//!   letters, digits, `_` or `-`.
//! - Its definition is one line, `[^id]: <event time>; <reference>, ...`.
//!   The event time is optional and is `YYYY`, `YYYY-MM` or `YYYY-MM-DD`,
//!   kept at the precision it is written at. A reference is a workspace file
//!   (`path`, `path:line`, `path:start-end`), a journal source
//!   (`event_<uuid>`, as `history_search` returns it), a conversation
//!   (`task_<uuid>`) or an `http(s)://` URL (`evidence.rs`).
//! - A paragraph (or heading, or list item) is addressed by ending its last
//!   line with ` ^anchor` (3 to 32 letters, digits or `-`), the block id of
//!   Obsidian and the paper's own syntax. It is plain text, so no renderer or
//!   sanitizer can drop it, and `links.rs` leaves it alone. A link names it as
//!   `page.md#^anchor`; a fragment without the `^` is an ordinary heading
//!   link and is not checked.
//!
//! Everything here is parsing a fixed syntax; no word judges meaning.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, NaiveDate, Utc};

use super::super::evidence::{Reference, Sources};
use super::links;

/// The longest footnote id and anchor.
const MAX_ID: usize = 32;

/// The shortest anchor: a trailing `^2` is arithmetic, not an address.
const MIN_ANCHOR: usize = 3;

/// The most references one footnote holds.
const MAX_REFERENCES: usize = 16;

/// The characters of a statement or of a paragraph preview kept.
const STATEMENT_CHARS: usize = 300;
const PREVIEW_CHARS: usize = 160;

/// The most problems one refusal names.
const MAX_PROBLEMS: usize = 5;

/// What a footnote definition looks like, for a refusal to teach.
pub(crate) const GRAMMAR: &str = "a footnote is a marker [^id] after the statement and one line [^id]: <YYYY[-MM[-DD]]>; <reference>, <reference> (the time is optional); a reference is a workspace path[:line[-line]], a history source_id from history_search, or an http(s) URL";

/// A time at the precision it was written: a year, a month or a day.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct EventTime {
    year: i32,
    month: Option<u32>,
    day: Option<u32>,
}

impl EventTime {
    /// `YYYY`, `YYYY-MM` or `YYYY-MM-DD`, a date that exists.
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let parts = text.split('-').collect::<Vec<_>>();
        if parts.len() > 3 {
            return None;
        }
        let digits = |part: &str, width: usize| {
            (part.len() == width && part.bytes().all(|byte| byte.is_ascii_digit()))
                .then(|| part.parse::<u32>().ok())
                .flatten()
        };
        let year = i32::try_from(digits(parts.first()?, 4)?).ok()?;
        let month = match parts.get(1) {
            None => None,
            Some(part) => Some(digits(part, 2)?),
        };
        let day = match parts.get(2) {
            None => None,
            Some(part) => Some(digits(part, 2)?),
        };
        NaiveDate::from_ymd_opt(year, month.unwrap_or(1), day.unwrap_or(1))?;
        Some(Self { year, month, day })
    }

    /// The span of time it names, `[start, end)`: a year is all of it.
    pub(crate) fn span(self) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
        let start =
            NaiveDate::from_ymd_opt(self.year, self.month.unwrap_or(1), self.day.unwrap_or(1))?;
        let end = match (self.month, self.day) {
            (_, Some(_)) => start.succ_opt()?,
            (Some(month), None) => {
                if month == 12 {
                    NaiveDate::from_ymd_opt(self.year.checked_add(1)?, 1, 1)?
                } else {
                    NaiveDate::from_ymd_opt(self.year, month + 1, 1)?
                }
            }
            (None, None) => NaiveDate::from_ymd_opt(self.year.checked_add(1)?, 1, 1)?,
        };
        Some((
            start.and_hms_opt(0, 0, 0)?.and_utc(),
            end.and_hms_opt(0, 0, 0)?.and_utc(),
        ))
    }

    /// Whether the time overlaps `[since, until)`: a statement dated to a
    /// year is in any window that touches that year.
    pub(crate) fn within(self, since: Option<DateTime<Utc>>, until: Option<DateTime<Utc>>) -> bool {
        let Some((start, end)) = self.span() else {
            return false;
        };
        since.is_none_or(|since| end > since) && until.is_none_or(|until| start < until)
    }
}

impl std::fmt::Display for EventTime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{:04}", self.year)?;
        if let Some(month) = self.month {
            write!(formatter, "-{month:02}")?;
        }
        if let Some(day) = self.day {
            write!(formatter, "-{day:02}")?;
        }
        Ok(())
    }
}

/// Whether `text` is shaped like a time (four digits, then digits and
/// dashes): a first segment that is shaped like one but is not a date is a
/// mistake to name, not a reference.
fn looks_like_time(text: &str) -> bool {
    text.len() >= 4
        && text.bytes().take(4).all(|byte| byte.is_ascii_digit())
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'-')
}

/// A footnote's definition line.
#[derive(Clone, Debug)]
pub(crate) struct Definition {
    pub id: String,
    pub line: usize,
    /// The text after the colon, as written: what an unchanged definition
    /// is recognized by.
    pub text: String,
    pub time: Option<EventTime>,
    pub references: Vec<String>,
    /// Why the definition cannot be read, when it cannot.
    pub problem: Option<String>,
}

/// A footnote marker: the statement it follows.
#[derive(Clone, Debug)]
pub(crate) struct Marker {
    pub id: String,
    pub line: usize,
    pub statement: String,
    unit: (usize, usize),
}

/// A paragraph anchor.
#[derive(Clone, Debug)]
pub(crate) struct Anchor {
    pub id: String,
    pub line: usize,
    /// The start of the paragraph, for a backlink to say what it cites.
    pub preview: String,
    unit: (usize, usize),
}

/// What a page's Markdown holds of statements, evidence and anchors.
#[derive(Clone, Debug, Default)]
pub(crate) struct Notes {
    pub markers: Vec<Marker>,
    pub definitions: Vec<Definition>,
    pub anchors: Vec<Anchor>,
}

/// A statement with an event time, for the timeline and the date filters.
#[derive(Clone, Debug)]
pub(crate) struct Dated {
    pub time: EventTime,
    pub note: String,
    pub statement: String,
    pub line: usize,
    pub anchor: Option<String>,
    pub references: Vec<String>,
}

/// Something wrong in a page's notes. `key` names it without its line
/// numbers, so a page that already had it is not refused for it again.
#[derive(Clone, Debug)]
pub(crate) struct Fault {
    pub key: String,
    pub message: String,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Kind {
    Blank,
    Definition,
    Text,
}

fn valid_id(id: &str) -> bool {
    (1..=MAX_ID).contains(&id.len())
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn valid_anchor(id: &str) -> bool {
    (MIN_ANCHOR..=MAX_ID).contains(&id.len())
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

/// `[^id]:` at the start of a line (up to three spaces in): the id and the
/// byte where the text after the colon starts.
fn definition_head(visible: &str) -> Option<(&str, usize)> {
    let trimmed = visible.trim_start_matches(' ');
    if visible.len() - trimmed.len() > 3 {
        return None;
    }
    let rest = trimmed.strip_prefix("[^")?;
    let (id, after) = rest.split_once("]:")?;
    valid_id(id).then(|| (id, visible.len() - after.len()))
}

/// Whether a line starts a block of its own: a heading or a list item.
fn starts_block(visible: &str) -> bool {
    let trimmed = visible.trim_start();
    let hashes = trimmed.bytes().take_while(|byte| *byte == b'#').count();
    if (1..=6).contains(&hashes) && trimmed[hashes..].starts_with(' ') {
        return true;
    }
    if ["- ", "* ", "+ "]
        .iter()
        .any(|bullet| trimmed.starts_with(bullet))
    {
        return true;
    }
    let digits = trimmed.bytes().take_while(u8::is_ascii_digit).count();
    digits > 0
        && digits <= 9
        && (trimmed[digits..].starts_with(". ") || trimmed[digits..].starts_with(") "))
}

/// A line without its heading or list marker.
fn without_block_mark(line: &str) -> &str {
    let trimmed = line.trim_start();
    let hashes = trimmed.bytes().take_while(|byte| *byte == b'#').count();
    if (1..=6).contains(&hashes) && trimmed[hashes..].starts_with(' ') {
        return trimmed[hashes..].trim_start();
    }
    for bullet in ["- ", "* ", "+ "] {
        if let Some(rest) = trimmed.strip_prefix(bullet) {
            return rest.trim_start();
        }
    }
    let digits = trimmed.bytes().take_while(u8::is_ascii_digit).count();
    if digits > 0
        && digits <= 9
        && (trimmed[digits..].starts_with(". ") || trimmed[digits..].starts_with(") "))
    {
        return trimmed[digits + 2..].trim_start();
    }
    trimmed
}

/// The anchor a line ends with: ` ^id` after some text.
fn trailing_anchor(visible: &str) -> Option<&str> {
    let trimmed = visible.trim_end();
    let (head, id) = trimmed.rsplit_once(char::is_whitespace)?;
    let id = id.strip_prefix('^')?;
    (valid_anchor(id) && !head.trim().is_empty()).then_some(id)
}

/// A line without the anchor it ends with.
fn without_anchor(line: &str) -> &str {
    trailing_anchor(line).map_or(line, |_| {
        line.trim_end()
            .rsplit_once(char::is_whitespace)
            .map_or(line, |(head, _)| head)
    })
}

fn clipped(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        text.to_owned()
    } else {
        format!("{}...", text.chars().take(limit).collect::<String>())
    }
}

/// Where the markers `[^id]` are in a visible line: their start, end and id.
fn markers_in(visible: &str) -> Vec<(usize, usize, &str)> {
    let mut found = Vec::new();
    let bytes = visible.as_bytes();
    for (start, _) in visible.match_indices("[^") {
        let Some(close) = visible[start + 2..].find(']') else {
            continue;
        };
        let id = &visible[start + 2..start + 2 + close];
        let end = start + 2 + close + 1;
        let backslashed = start > 0 && bytes[start - 1] == b'\\';
        if valid_id(id) && bytes.get(end) != Some(&b'(') && !backslashed {
            found.push((start, end, id));
        }
    }
    found
}

/// The text of a paragraph before a marker, from the sentence it ends: the
/// statement the footnote supports. A sentence ends at `.`, `!`, `?` (or the
/// CJK forms) followed by a space; a marker straight after one belongs to
/// the sentence it ends, and the previous marker starts a new one.
fn statement_of(before: &str) -> String {
    let mut text = before;
    // The text since the previous marker.
    if let Some((_, end, _)) = markers_in(before).last() {
        text = &before[*end..];
    }
    let text = text.trim_end();
    let body_end = text
        .trim_end_matches([
            '.', '!', '?', '\u{3002}', '\u{ff01}', '\u{ff1f}', '\u{2026}',
        ])
        .len();
    let mut start = 0;
    for (at, character) in text[..body_end].char_indices() {
        if matches!(
            character,
            '.' | '!' | '?' | '\u{3002}' | '\u{ff01}' | '\u{ff1f}'
        ) && text[at + character.len_utf8()..]
            .chars()
            .next()
            .is_some_and(char::is_whitespace)
        {
            start = at + character.len_utf8();
        }
    }
    clipped(text[start..].trim(), STATEMENT_CHARS)
}

fn definition_of(id: &str, line: usize, rest: &str) -> Definition {
    let rest = rest.trim();
    let (head, tail) = match rest.split_once(';') {
        Some((head, tail)) if looks_like_time(head.trim()) => (Some(head.trim()), tail),
        _ if looks_like_time(rest) => (Some(rest), ""),
        _ => (None, rest),
    };
    let mut problem = None;
    let time = head.and_then(|head| {
        EventTime::parse(head).or_else(|| {
            problem = Some(format!(
                "the event time {head:?} is not YYYY, YYYY-MM or YYYY-MM-DD (a date that exists)"
            ));
            None
        })
    });
    let mut references = Vec::new();
    let mut start = 0;
    let bytes = tail.as_bytes();
    for (at, _) in tail.match_indices(',') {
        if bytes.get(at + 1).is_none_or(u8::is_ascii_whitespace) {
            references.push(&tail[start..at]);
            start = at + 1;
        }
    }
    references.push(&tail[start..]);
    let references = references
        .into_iter()
        .map(|reference| reference.trim().trim_matches('`').trim().to_owned())
        .filter(|reference| !reference.is_empty())
        .collect::<Vec<_>>();
    if problem.is_none() && time.is_none() && references.is_empty() {
        problem = Some("it holds neither an event time nor a reference".to_owned());
    }
    if problem.is_none() && references.len() > MAX_REFERENCES {
        problem = Some(format!("it has more than {MAX_REFERENCES} references"));
    }
    Definition {
        id: id.to_owned(),
        line,
        text: rest.to_owned(),
        time,
        references,
        problem,
    }
}

/// The text of a paragraph (lines `unit.0..=unit.1`), without its block mark
/// and its anchor.
fn paragraph_text(originals: &[&str], unit: (usize, usize)) -> String {
    let mut parts = Vec::new();
    for (index, line) in originals.iter().enumerate().take(unit.1 + 1).skip(unit.0) {
        let line = line.trim_end_matches(['\n', '\r']);
        let line = if index == unit.0 {
            without_block_mark(line)
        } else {
            line
        };
        let line = if index == unit.1 {
            without_anchor(line)
        } else {
            line
        };
        parts.push(line.trim().to_owned());
    }
    parts.join(" ")
}

/// The paragraph `unit` up to byte `at` of line `index`: where a marker is.
/// `at` is the same byte in the visible and the original line.
fn text_before(originals: &[&str], unit: (usize, usize), index: usize, at: usize) -> String {
    let mut before = String::new();
    for earlier in &originals[unit.0..index] {
        before.push_str(earlier.trim());
        before.push(' ');
    }
    before.push_str(&originals[index][..at]);
    without_block_mark(&before).to_owned()
}

/// Read the notes of `text`, a page as stored: its metadata line counts as
/// line 1, as read and search count it. Code, comments and spans are not
/// read (`links::visible`).
pub(crate) fn parse(text: &str) -> Notes {
    if !text.contains('^') {
        return Notes::default();
    }
    let visible = links::visible(text);
    let originals = text.split_inclusive('\n').collect::<Vec<_>>();
    let visibles = visible.split_inclusive('\n').collect::<Vec<_>>();
    let trim = |line: &str| line.trim_end_matches(['\n', '\r']).to_owned();
    let lines = visibles.iter().map(|line| trim(line)).collect::<Vec<_>>();
    let kinds = lines
        .iter()
        .map(|line| {
            if line.trim().is_empty() {
                Kind::Blank
            } else if definition_head(line).is_some() {
                Kind::Definition
            } else {
                Kind::Text
            }
        })
        .collect::<Vec<_>>();
    // The paragraph each text line belongs to: it runs to the next blank
    // line, or to the next heading or list item.
    let mut starts = vec![None; lines.len()];
    let mut current: Option<usize> = None;
    for (index, kind) in kinds.iter().enumerate() {
        if *kind == Kind::Text {
            if current.is_none() || starts_block(&lines[index]) {
                current = Some(index);
            }
            starts[index] = current;
        } else {
            current = None;
        }
    }
    let mut ends = BTreeMap::new();
    for (index, start) in starts.iter().enumerate() {
        if let Some(start) = start {
            ends.insert(*start, index);
        }
    }
    let unit_of = |index: usize| starts[index].map(|start| (start, ends[&start]));
    let paragraph = |unit: (usize, usize)| paragraph_text(&originals, unit);
    let mut notes = Notes::default();
    for (index, kind) in kinds.iter().enumerate() {
        let number = index + 1;
        match kind {
            Kind::Blank => {}
            Kind::Definition => {
                if let Some((id, after)) = definition_head(&lines[index]) {
                    let original = originals[index].trim_end_matches(['\n', '\r']);
                    notes.definitions.push(definition_of(
                        id,
                        number,
                        original.get(after..).unwrap_or_default(),
                    ));
                }
            }
            Kind::Text => {
                let Some(unit) = unit_of(index) else { continue };
                if let Some(id) = trailing_anchor(&lines[index]) {
                    notes.anchors.push(Anchor {
                        id: id.to_owned(),
                        line: number,
                        preview: clipped(&paragraph(unit), PREVIEW_CHARS),
                        unit,
                    });
                }
                for (at, _, id) in markers_in(&lines[index]) {
                    let mut statement = statement_of(&text_before(&originals, unit, index, at));
                    if statement.is_empty() {
                        statement = clipped(&paragraph(unit), STATEMENT_CHARS);
                    }
                    notes.markers.push(Marker {
                        id: id.to_owned(),
                        line: number,
                        statement,
                        unit,
                    });
                }
            }
        }
    }
    notes
}

/// Whether `line` is a footnote definition: `[^id]: ...`.
pub(crate) fn is_definition(line: &str) -> bool {
    definition_head(line).is_some()
}

/// `text` without its footnote definition lines and markers: the claim
/// itself. The wiki pass weighs the words a page brings in against its
/// sources; an id and a path are not a claim.
pub(crate) fn without_notes(text: &str) -> String {
    let kept = text
        .split_inclusive('\n')
        .filter(|line| definition_head(line.trim_end_matches(['\n', '\r'])).is_none())
        .collect::<String>();
    let mut claim = String::with_capacity(kept.len());
    let mut last = 0;
    for (start, end, _) in markers_in(&kept) {
        claim.push_str(&kept[last..start]);
        last = end;
    }
    claim.push_str(&kept[last..]);
    claim
}

impl Notes {
    /// What is wrong with the notes themselves (not with what they cite).
    pub(crate) fn faults(&self) -> Vec<Fault> {
        let mut faults = Vec::new();
        let defined = self
            .definitions
            .iter()
            .map(|definition| definition.id.as_str())
            .collect::<BTreeSet<_>>();
        let used = self
            .markers
            .iter()
            .map(|marker| marker.id.as_str())
            .collect::<BTreeSet<_>>();
        let mut reported = BTreeSet::new();
        for marker in &self.markers {
            if !defined.contains(marker.id.as_str()) && reported.insert(marker.id.as_str()) {
                faults.push(Fault {
                    key: format!("marker:{}", marker.id),
                    message: format!(
                        "[^{id}] (line {line}) has no definition: add the line `[^{id}]: <YYYY[-MM[-DD]]>; <reference>, ...` or take the marker out; if it is code, not a footnote, put it in backticks",
                        id = marker.id,
                        line = marker.line
                    ),
                });
            }
        }
        let mut seen = BTreeSet::new();
        for definition in &self.definitions {
            if !seen.insert(definition.id.as_str()) {
                faults.push(Fault {
                    key: format!("twice:{}", definition.id),
                    message: format!(
                        "[^{}] is defined twice (line {} and an earlier one): a footnote has one definition",
                        definition.id, definition.line
                    ),
                });
                continue;
            }
            if !used.contains(definition.id.as_str()) {
                faults.push(Fault {
                    key: format!("unused:{}", definition.id),
                    message: format!(
                        "[^{id}] is defined (line {line}) but no statement uses it: put [^{id}] after the statement it supports or delete the line",
                        id = definition.id,
                        line = definition.line
                    ),
                });
            }
            if let Some(problem) = &definition.problem {
                faults.push(Fault {
                    key: format!("definition:{}:{problem}", definition.id),
                    message: format!("[^{}] (line {}): {problem}", definition.id, definition.line),
                });
            }
        }
        let mut anchors = BTreeMap::<&str, usize>::new();
        for anchor in &self.anchors {
            if let Some(first) = anchors.insert(anchor.id.as_str(), anchor.line) {
                faults.push(Fault {
                    key: format!("anchor:{}", anchor.id),
                    message: format!(
                        "the anchor ^{} is on lines {first} and {}: an anchor names one paragraph",
                        anchor.id, anchor.line
                    ),
                });
            }
        }
        faults
    }

    /// The definition of footnote `id`.
    pub(crate) fn definition(&self, id: &str) -> Option<&Definition> {
        self.definitions
            .iter()
            .find(|definition| definition.id == id)
    }

    /// The statements that have an event time, in the page's order.
    pub(crate) fn dated(&self) -> Vec<Dated> {
        self.markers
            .iter()
            .filter_map(|marker| {
                let definition = self.definition(&marker.id)?;
                let time = definition.time?;
                Some(Dated {
                    time,
                    note: marker.id.clone(),
                    statement: marker.statement.clone(),
                    line: marker.line,
                    anchor: self
                        .anchors
                        .iter()
                        .find(|anchor| anchor.unit == marker.unit)
                        .map(|anchor| anchor.id.clone()),
                    references: definition.references.clone(),
                })
            })
            .collect()
    }

    /// The anchors the page defines.
    pub(crate) fn anchor_ids(&self) -> BTreeSet<&str> {
        self.anchors
            .iter()
            .map(|anchor| anchor.id.as_str())
            .collect()
    }
}

/// Check the notes of a page about to be written. Only what the write brings
/// in is checked: a fault the page already had is not raised again, and a
/// definition the page already held is not resolved again, because the file
/// it cites may have changed since and an unrelated edit must not be refused
/// for it (the wiki pass is what finds a claim a source no longer holds).
///
/// With `sources` the references are looked up; without, only their syntax is
/// read - a store the host did not equip (a test, a tool that writes no
/// footnotes) cannot say where a file is.
///
/// # Errors
/// What is wrong, each problem naming the footnote, the reference and what
/// would resolve. Nothing is written for it.
pub(crate) fn check(
    page: &str,
    new: &str,
    previous: &[&str],
    sources: Option<&Sources>,
) -> Result<(), String> {
    let now = parse(new);
    if now.markers.is_empty() && now.definitions.is_empty() && now.anchors.is_empty() {
        return Ok(());
    }
    // Several earlier texts when pages are merged: a note either page held.
    let before = previous.iter().map(|text| parse(text)).collect::<Vec<_>>();
    let had = before
        .iter()
        .flat_map(Notes::faults)
        .map(|fault| fault.key)
        .collect::<BTreeSet<_>>();
    let mut problems = now
        .faults()
        .into_iter()
        .filter(|fault| !had.contains(&fault.key))
        .map(|fault| fault.message)
        .collect::<Vec<_>>();
    for definition in &now.definitions {
        let unchanged = before.iter().any(|earlier| {
            earlier
                .definition(&definition.id)
                .is_some_and(|earlier| earlier.text == definition.text)
        });
        if unchanged || definition.problem.is_some() {
            continue;
        }
        for token in &definition.references {
            match Reference::parse(token) {
                Err(why) => problems.push(format!("[^{}]: {why}", definition.id)),
                Ok(reference) => {
                    if let Some(why) = sources.and_then(|sources| sources.check(&reference).err()) {
                        problems.push(format!("[^{}]: reference {token}: {why}", definition.id));
                    }
                }
            }
        }
    }
    if problems.is_empty() {
        return Ok(());
    }
    let more = problems.len().saturating_sub(MAX_PROBLEMS);
    problems.truncate(MAX_PROBLEMS);
    Err(format!(
        "{page} was not written, its footnotes do not hold: {}{}. Rule: {GRAMMAR}",
        problems.join("; "),
        if more > 0 {
            format!("; and {more} more")
        } else {
            String::new()
        }
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_time_keeps_the_precision_it_was_written_at_and_a_year_sorts_before_its_months() {
        let year = EventTime::parse("2026").unwrap();
        let month = EventTime::parse("2026-05").unwrap();
        let day = EventTime::parse("2026-05-03").unwrap();
        assert_eq!(
            [year.to_string(), month.to_string(), day.to_string()],
            ["2026", "2026-05", "2026-05-03"]
        );
        let mut times = [day, EventTime::parse("2026-04-30").unwrap(), month, year];
        times.sort();
        assert_eq!(
            times.iter().map(ToString::to_string).collect::<Vec<_>>(),
            ["2026", "2026-04-30", "2026-05", "2026-05-03"]
        );
        for bad in [
            "26",
            "2026-13",
            "2026-02-30",
            "2026-5",
            "2026-05-03-01",
            "2026/05",
            "abcd",
        ] {
            assert!(EventTime::parse(bad).is_none(), "{bad} was a time");
        }
    }

    #[test]
    fn a_time_overlaps_a_window_by_its_whole_span() {
        let at = |text: &str| {
            DateTime::parse_from_rfc3339(text)
                .unwrap()
                .with_timezone(&Utc)
        };
        let may = EventTime::parse("2026-05").unwrap();
        assert!(may.within(Some(at("2026-05-31T00:00:00Z")), None));
        assert!(!may.within(Some(at("2026-06-01T00:00:00Z")), None));
        assert!(!may.within(None, Some(at("2026-05-01T00:00:00Z"))));
        assert!(EventTime::parse("2026").unwrap().within(
            Some(at("2026-09-01T00:00:00Z")),
            Some(at("2026-10-01T00:00:00Z"))
        ));
    }

    #[test]
    fn a_marker_takes_the_sentence_it_follows_and_its_definition_gives_time_and_references() {
        let text = "<!-- meta -->\n# Work\n\nJoined A in May.[^e1] Moved to B in August.[^e2] ^work-2026\n\n- A list item says one thing.[^e3]\n\n[^e1]: 2026-05; src/hr.rs:12-14, https://example.com/a\n[^e2]: 2026-08-12\n[^e3]: src/hr.rs\n";
        let notes = parse(text);
        let statements = notes
            .markers
            .iter()
            .map(|marker| (marker.id.as_str(), marker.statement.as_str(), marker.line))
            .collect::<Vec<_>>();
        assert_eq!(
            statements,
            [
                ("e1", "Joined A in May.", 4),
                ("e2", "Moved to B in August.", 4),
                ("e3", "A list item says one thing.", 6),
            ]
        );
        let first = notes.definition("e1").unwrap();
        assert_eq!(first.time.unwrap().to_string(), "2026-05");
        assert_eq!(
            first.references,
            ["src/hr.rs:12-14", "https://example.com/a"]
        );
        assert_eq!(notes.definition("e2").unwrap().references.len(), 0);
        assert_eq!(notes.definition("e3").unwrap().time, None);
        assert!(notes.faults().is_empty(), "{:?}", notes.faults());
        let dated = notes.dated();
        assert_eq!(dated.len(), 2);
        assert_eq!(dated[0].anchor.as_deref(), Some("work-2026"));
        assert_eq!(notes.anchors[0].id, "work-2026");
        assert!(notes.anchors[0].preview.starts_with("Joined A in May."));
    }

    #[test]
    fn markers_in_code_are_not_read_and_a_page_without_notes_has_none() {
        let text =
            "# P\n\n`[^x]` and\n\n```\n[^y]: nothing\n```\n\nPlain text with a caret ^2 and x ^2\n";
        let notes = parse(text);
        assert!(notes.markers.is_empty() && notes.definitions.is_empty());
        assert!(notes.anchors.is_empty(), "{:?}", notes.anchors);
        assert!(check("p.md", text, &[], None).is_ok());
    }

    #[test]
    fn faults_name_an_undefined_marker_an_unused_definition_a_bad_time_and_a_repeated_anchor() {
        let text =
            "# P\n\nOne.[^a]\n\nTwo. ^same-id\n\nThree. ^same-id\n\n[^b]: 2026-13; x.md\n[^c]: \n";
        let faults = parse(text).faults();
        let messages = faults
            .iter()
            .map(|fault| fault.message.as_str())
            .collect::<Vec<_>>();
        assert!(
            messages
                .iter()
                .any(|m| m.contains("[^a]") && m.contains("no definition"))
        );
        assert!(
            messages
                .iter()
                .any(|m| m.contains("[^b]") && m.contains("2026-13"))
        );
        assert!(
            messages
                .iter()
                .any(|m| m.contains("[^b]") && m.contains("no statement uses it"))
        );
        assert!(
            messages
                .iter()
                .any(|m| m.contains("[^c]") && m.contains("neither"))
        );
        assert!(
            messages
                .iter()
                .any(|m| m.contains("^same-id") && m.contains("one paragraph"))
        );
    }

    /// A regular-expression class such as `[^abc]` in prose reads as a footnote
    /// marker; the refusal says the way out, and a page that already held one
    /// can still be edited.
    #[test]
    fn a_class_in_prose_is_refused_with_the_way_out_and_passes_in_backticks() {
        let bare = "# P\n\nThe pattern [^abc] skips them.\n";
        let why = check("p.md", bare, &[], None).unwrap_err();
        assert!(why.contains("[^abc]") && why.contains("backticks"), "{why}");
        let quoted = "# P\n\nThe pattern `[^abc]` skips them.\n";
        assert!(check("p.md", quoted, &[], None).is_ok());
        let edited = format!("{bare}\nMore text.\n");
        assert!(check("p.md", &edited, &[bare], None).is_ok());
    }

    #[test]
    fn only_what_a_write_brings_in_is_checked() {
        let old = "# P\n\nOne.[^a]\n\n[^a]: src/gone.rs:99\n[^dup]: 2026\n";
        // The old page's faults and its unresolvable reference are not raised
        // again by an edit elsewhere.
        let edited = format!("{old}\nMore text.\n");
        assert!(check("p.md", &edited, &[old], Some(&Sources::default())).is_ok());
        // A new footnote that cites what does not exist is.
        let worse = format!("{old}\nNew.[^n]\n\n[^n]: nowhere/at-all.rs:1\n");
        let why = check("p.md", &worse, &[old], Some(&Sources::default())).unwrap_err();
        assert!(
            why.contains("[^n]") && why.contains("nowhere/at-all.rs:1"),
            "{why}"
        );
        // A changed definition is checked again.
        let changed = old.replace("src/gone.rs:99", "src/gone.rs:98");
        assert!(check("p.md", &changed, &[old], Some(&Sources::default())).is_err());
    }
}
