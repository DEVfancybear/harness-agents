//! What a turn does not send twice (arXiv 2611.12122, cross-output line
//! pointers): a `read_file` result the model still holds whole, a file that
//! changed a little since it was read, and a run of one line in command output.
//!
//! Nothing here changes what a tool did or what the user is shown. The file is
//! read and hashed as before; only the text that goes into the transcript for
//! the model is shorter, and only when the model provably holds the text the
//! short form points to. That is judged from the turn's own transcript and from
//! what the runtime reports of the requests it sent (`RunRequest::
//! result_was_fitted`, `RunRequest::turn_folded_until`). A result of an earlier
//! turn is never pointed to: the runtime may summarize those or shorten them
//! when it builds the next request, and nothing the driver sees says which.
//!
//! The switch is `HA_REREAD=off`.

use std::fmt::Write as _;

use harness_providers::{MessageRole, ProviderMessage};
use similar::{Algorithm, DiffOp, capture_diff_slices};

use crate::ToolOutput;

/// A run of this many identical lines is shown once, with its count.
pub(crate) const REPEAT_RUN_MIN: usize = 4;

/// Most lines a re-read may show as changed and still be sent as a delta.
const DELTA_MAX_CHANGED: usize = 200;

/// How many earlier reads the turn keeps the text of, and how many bytes of
/// text in all: the oldest are forgotten first (and then read in full again).
const KEPT_READS: usize = 128;
const KEPT_BYTES: usize = 8 * 1024 * 1024;

/// How many of the newest earlier reads of one file a delta is tried against.
const DELTA_BASES_TRIED: usize = 4;

/// The tools whose output has its repeated lines collapsed.
const COLLAPSED_TOOLS: [&str; 3] = ["run_shell", "run_process", "read_process_output"];

/// What a turn saved by not sending text twice.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RereadTally {
    /// Re-reads answered with a pointer to the result that holds the text.
    pub unchanged: u32,
    /// Re-reads of a changed file answered with only the changed lines.
    pub delta: u32,
    /// Command results whose runs of one repeated line were collapsed.
    pub collapsed: u32,
    /// Bytes the three did not send, net of what was sent in full again.
    pub saved_bytes: u64,
}

/// Whether `HA_REREAD`'s value leaves the savings on (the default): only
/// `off` (also `0` or `false`) turns them off.
pub(crate) fn enabled_by(value: Option<&str>) -> bool {
    value.is_none_or(|value| {
        !["off", "0", "false"]
            .iter()
            .any(|off| value.trim().eq_ignore_ascii_case(off))
    })
}

/// `HA_REREAD`.
pub(crate) fn enabled_from_env() -> bool {
    enabled_by(std::env::var("HA_REREAD").ok().as_deref())
}

/// Whether a turn sends text it already sent only once (`HA_REREAD`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Mode {
    On,
    Off,
}

impl Mode {
    pub(crate) fn from_flag(on: bool) -> Self {
        if on { Self::On } else { Self::Off }
    }

    /// `HA_REREAD`: on unless `off`.
    pub(crate) fn from_env() -> Self {
        Self::from_flag(enabled_from_env())
    }

    pub(crate) fn is_on(self) -> bool {
        self == Self::On
    }
}

/// Which earlier results the model is still sent whole.
pub(crate) struct Held<'a> {
    /// The turn's transcript as the last request carried it.
    pub transcript: &'a [ProviderMessage],
    /// The results of the step being built.
    pub batch: &'a [ProviderMessage],
    /// Whether the runtime sent a result of this call shortened to fit.
    pub fitted: &'a dyn Fn(&str) -> bool,
    /// Messages of the transcript before this place were folded into a summary.
    pub folded_until: usize,
}

impl Held<'_> {
    /// Whether the result of `call_id` is in the transcript or the step being
    /// built, and not elided, shortened, dropped, fitted or folded away.
    pub(crate) fn whole(&self, call_id: &str) -> bool {
        let is_result = |message: &&ProviderMessage| {
            message.role == MessageRole::Tool && message.tool_call_id.as_deref() == Some(call_id)
        };
        if let Some(place) = self.transcript.iter().position(|m| is_result(&m)) {
            return place >= self.folded_until
                && !crate::turn_driver::is_shortened(&self.transcript[place].content)
                && !(self.fitted)(call_id);
        }
        self.batch
            .iter()
            .find(is_result)
            .is_some_and(|message| !crate::turn_driver::is_shortened(&message.content))
    }
}

/// The numbered lines a `read_file` result shows.
struct Numbered<'a> {
    /// The line number of the first row.
    first: usize,
    /// Each row as shown (`12: text`, or `12#a3: text`) and its text.
    rows: Vec<(&'a str, &'a str)>,
    /// What follows the rows: the notice that says where the range stopped.
    trailer: &'a str,
    /// Whether the rows carry hashline anchors, which name their line number.
    anchored: bool,
}

impl Numbered<'_> {
    fn last(&self) -> usize {
        self.first + self.rows.len() - 1
    }
}

/// A row's line number and text, and whether it carries an anchor.
fn split_gutter(line: &str) -> Option<(usize, &str, bool)> {
    let digits = line.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let number = line[..digits].parse().ok()?;
    let mut rest = &line[digits..];
    let mut anchored = false;
    if let Some(after) = rest.strip_prefix('#') {
        let hash = after.bytes().take_while(u8::is_ascii_alphanumeric).count();
        if hash == 0 {
            return None;
        }
        rest = &after[hash..];
        anchored = true;
    }
    let rest = rest.strip_prefix(':')?;
    Some((number, rest.strip_prefix(' ').unwrap_or(rest), anchored))
}

/// The rows of a result's content, up to the first line that is not the next
/// numbered one. `None` for a result that shows no row, or shows only the
/// beginning of a line too long for the limit.
fn parse_numbered(content: &str) -> Option<Numbered<'_>> {
    let mut rows: Vec<(&str, &str)> = Vec::new();
    let mut first = 0;
    let mut anchored = false;
    let mut consumed = 0;
    for line in content.split('\n') {
        let Some((number, text, anchor)) = split_gutter(line) else {
            break;
        };
        if rows.is_empty() {
            first = number;
        } else if number != first + rows.len() {
            break;
        }
        anchored |= anchor;
        rows.push((line, text));
        consumed += line.len() + 1;
    }
    if rows.is_empty() {
        return None;
    }
    let trailer = content.get(consumed..).unwrap_or("");
    if trailer.trim_start().starts_with("[Line ") {
        return None;
    }
    Some(Numbered {
        first,
        rows,
        trailer,
        anchored,
    })
}

/// A read the model holds, and what the later results rest on.
struct Read {
    call_id: String,
    path: String,
    hash: String,
    first: usize,
    lines: Vec<String>,
    anchored: bool,
    /// Other results this one is only readable with (the read a delta was
    /// made against); all must be whole too.
    needs: Vec<String>,
}

/// A result sent short in place of the text, and that text, to send in its
/// place when what it points to is no longer whole.
struct Swap {
    call_id: String,
    path: String,
    text: String,
    original: String,
    needs: Vec<String>,
    saved: usize,
    unchanged: bool,
}

/// The savings of one turn.
pub(crate) struct Reread {
    on: bool,
    task: String,
    session: String,
    reads: Vec<Read>,
    swaps: Vec<Swap>,
    pub(crate) tally: RereadTally,
}

/// Whether a `read_file` call asked for the text again.
fn asks_fresh(arguments: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(arguments)
        .ok()
        .and_then(|value| value.get("fresh")?.as_bool())
        .unwrap_or(false)
}

impl Reread {
    pub(crate) fn new(on: bool, task: &str, session: &str) -> Self {
        Self {
            on,
            task: task.to_owned(),
            session: session.to_owned(),
            reads: Vec::new(),
            swaps: Vec::new(),
            tally: RereadTally::default(),
        }
    }

    fn event(&self, name: &'static str, category: &'static str, call_id: &str, saved: usize) {
        harness_types::diag::info(category, name)
            .task(&self.task)
            .session(&self.session)
            .field("call", call_id)
            .field("bytes", saved)
            .emit();
    }

    /// What the model is sent for a `read_file` result instead of its text,
    /// or `None` to send the text: a pointer to the result that holds it
    /// while the file is as it was, or the changed lines while the change is
    /// small. `rendered` is the whole text, which the user is shown.
    pub(crate) fn read_for_model(
        &mut self,
        held: &Held<'_>,
        call_id: &str,
        arguments: &str,
        output: &ToolOutput,
        rendered: &str,
    ) -> Option<String> {
        let ToolOutput::ReadFile {
            path,
            content,
            hash,
            ..
        } = output
        else {
            return None;
        };
        if !self.on || asks_fresh(arguments) {
            return None;
        }
        let new = parse_numbered(content)?;
        let key = crate::turn_driver::seen_key(path);
        let usable = (0..self.reads.len())
            .rev()
            .filter(|place| {
                let read = &self.reads[*place];
                read.path == key
                    && read.call_id != call_id
                    && held.whole(&read.call_id)
                    && read.needs.iter().all(|need| held.whole(need))
            })
            .collect::<Vec<_>>();
        // The same text, inside what an earlier read showed.
        if let Some(place) = usable.iter().copied().find(|place| {
            let read = &self.reads[*place];
            read.hash == hash.as_str()
                && read.first <= new.first
                && new.last() < read.first + read.lines.len()
        }) {
            let read = &self.reads[place];
            let short = hash.as_str().strip_prefix("sha256:").unwrap_or("");
            let mut text = format!(
                "{path}: unchanged since your read_file call {} (sha256:{}, lines {}-{}); that result above holds it. Pass \"fresh\": true to read it again.",
                read.call_id,
                &short[..short.len().min(12)],
                new.first,
                new.last()
            );
            let trailer = new.trailer.trim();
            if !trailer.is_empty() {
                text.push('\n');
                text.push_str(trailer);
            }
            if text.len() >= rendered.len() {
                return None;
            }
            let mut needs = vec![read.call_id.clone()];
            needs.extend(read.needs.iter().cloned());
            self.swapped(call_id, &key, text.clone(), rendered, needs, true);
            return Some(text);
        }
        // The file changed: the lines that did not, once.
        let mut best: Option<(usize, String, usize)> = None;
        for place in usable
            .iter()
            .copied()
            .filter(|place| self.reads[*place].hash != hash.as_str())
            .take(DELTA_BASES_TRIED)
        {
            let read = &self.reads[place];
            let Some((body, changed)) = delta_body(read, &new) else {
                continue;
            };
            if best.as_ref().is_none_or(|(_, _, fewest)| changed < *fewest) {
                best = Some((place, body, changed));
            }
        }
        let (place, body, _) = best?;
        let read = &self.reads[place];
        let mut text = format!(
            "read_file {path} [hash {}] (changed since your read_file call {}; only the changed lines are shown):\n{body}",
            hash.as_str(),
            read.call_id
        );
        let trailer = new.trailer.trim();
        if !trailer.is_empty() {
            text.push_str("\n\n");
            text.push_str(trailer);
        }
        if text.len() >= rendered.len() {
            return None;
        }
        let mut needs = vec![read.call_id.clone()];
        needs.extend(read.needs.iter().cloned());
        // The model now holds the file as it is: this result and the read it
        // changes, so a later read of the same state points here.
        let kept = Read {
            call_id: call_id.to_owned(),
            path: key.clone(),
            hash: hash.as_str().to_owned(),
            first: new.first,
            lines: new
                .rows
                .iter()
                .map(|(_, text)| (*text).to_owned())
                .collect(),
            anchored: new.anchored,
            needs: needs.clone(),
        };
        self.swapped(call_id, &key, text.clone(), rendered, needs, false);
        self.keep(kept);
        Some(text)
    }

    fn swapped(
        &mut self,
        call_id: &str,
        key: &str,
        text: String,
        original: &str,
        needs: Vec<String>,
        unchanged: bool,
    ) {
        let saved = original.len() - text.len();
        if unchanged {
            self.tally.unchanged += 1;
        } else {
            self.tally.delta += 1;
        }
        self.tally.saved_bytes += saved as u64;
        if unchanged {
            self.event("unchanged", "read", call_id, saved);
        } else {
            self.event("delta", "read", call_id, saved);
        }
        self.swaps.push(Swap {
            call_id: call_id.to_owned(),
            path: key.to_owned(),
            text,
            original: original.to_owned(),
            needs,
            saved,
            unchanged,
        });
    }

    /// Remember a `read_file` result the model was sent whole.
    pub(crate) fn keep_read(&mut self, call_id: &str, output: &ToolOutput) {
        let ToolOutput::ReadFile {
            path,
            content,
            hash,
            ..
        } = output
        else {
            return;
        };
        if !self.on {
            return;
        }
        let Some(new) = parse_numbered(content) else {
            return;
        };
        self.keep(Read {
            call_id: call_id.to_owned(),
            path: crate::turn_driver::seen_key(path),
            hash: hash.as_str().to_owned(),
            first: new.first,
            lines: new
                .rows
                .iter()
                .map(|(_, text)| (*text).to_owned())
                .collect(),
            anchored: new.anchored,
            needs: Vec::new(),
        });
    }

    fn keep(&mut self, read: Read) {
        self.reads.push(read);
        let mut bytes = self
            .reads
            .iter()
            .flat_map(|read| read.lines.iter())
            .map(String::len)
            .sum::<usize>();
        while self.reads.len() > KEPT_READS || (bytes > KEPT_BYTES && self.reads.len() > 1) {
            let gone = self.reads.remove(0);
            bytes -= gone.lines.iter().map(String::len).sum::<usize>();
        }
    }

    /// Command output with each run of [`REPEAT_RUN_MIN`] or more identical
    /// lines shown once, or `None` to send it as it is.
    pub(crate) fn output_for_model(
        &mut self,
        tool: &str,
        call_id: &str,
        rendered: &str,
    ) -> Option<String> {
        if !self.on || !COLLAPSED_TOOLS.contains(&tool) {
            return None;
        }
        let (text, runs) = collapse_repeated_lines(rendered)?;
        // A run of short lines is not worth its note.
        if text.len() >= rendered.len() {
            return None;
        }
        let saved = rendered.len() - text.len();
        self.tally.collapsed += 1;
        self.tally.saved_bytes += saved as u64;
        harness_types::diag::info("output", "repeats_collapsed")
            .task(&self.task)
            .session(&self.session)
            .field("call", call_id)
            .field("tool", tool)
            .field("runs", runs)
            .field("bytes", saved)
            .emit();
        Some(text)
    }

    /// Put back the text of each short answer whose pointed-to result is no
    /// longer whole, so the model is never pointed at text it does not have.
    /// Called before a request is sent, after the transcript was trimmed.
    pub(crate) fn reconcile(
        &mut self,
        transcript: &mut [ProviderMessage],
        fitted: &dyn Fn(&str) -> bool,
        folded_until: usize,
    ) {
        if self.swaps.is_empty() {
            return;
        }
        let mut stale = Vec::new();
        let mut gone = Vec::new();
        {
            let held = Held {
                transcript,
                batch: &[],
                fitted,
                folded_until,
            };
            for (index, swap) in self.swaps.iter().enumerate() {
                let Some(place) = transcript.iter().position(|message| {
                    message.role == MessageRole::Tool
                        && message.tool_call_id.as_deref() == Some(swap.call_id.as_str())
                }) else {
                    continue;
                };
                if !transcript[place].content.contains(&swap.text) {
                    gone.push(index);
                } else if !swap.needs.iter().all(|need| held.whole(need)) {
                    stale.push((index, place));
                }
            }
        }
        for (index, place) in &stale {
            let swap = &self.swaps[*index];
            let content = &mut transcript[*place].content;
            *content = content.replacen(&swap.text, &swap.original, 1);
            if swap.unchanged {
                self.tally.unchanged = self.tally.unchanged.saturating_sub(1);
            } else {
                self.tally.delta = self.tally.delta.saturating_sub(1);
            }
            self.tally.saved_bytes = self.tally.saved_bytes.saturating_sub(swap.saved as u64);
            self.event("restored", "read", &swap.call_id, swap.original.len());
            // What was read in full again no longer rests on what went.
            let (call_id, path) = (swap.call_id.clone(), swap.path.clone());
            for read in &mut self.reads {
                if read.call_id == call_id && read.path == path {
                    read.needs.clear();
                }
            }
        }
        let mut done = stale
            .iter()
            .map(|(index, _)| *index)
            .chain(gone)
            .collect::<Vec<_>>();
        done.sort_unstable_by(|a, b| b.cmp(a));
        for index in done {
            self.swaps.remove(index);
        }
    }
}

/// The changed lines of `new` against what `old` showed, with one line for
/// each run that did not change; and how many lines changed. `None` when
/// more than half of `new`'s lines changed, or more than
/// [`DELTA_MAX_CHANGED`], or the line numbers named by hashline anchors moved
/// (the anchors of lines the model holds would no longer name them).
fn delta_body(old: &Read, new: &Numbered<'_>) -> Option<(String, usize)> {
    let before = old.lines.iter().map(String::as_str).collect::<Vec<_>>();
    let after = new.rows.iter().map(|(_, text)| *text).collect::<Vec<_>>();
    let ops = capture_diff_slices(Algorithm::Myers, &before, &after);
    let mut changed = 0;
    for op in &ops {
        if let DiffOp::Insert { new_len, .. } | DiffOp::Replace { new_len, .. } = op {
            changed += new_len;
        }
    }
    if changed * 2 > after.len() || changed > DELTA_MAX_CHANGED {
        return None;
    }
    // The new read stopped before the end of the file (its trailer says where
    // to read on), so old lines past its last row were cut, not removed.
    let cut = !new.trailer.trim().is_empty();
    let mut body = String::new();
    let mut line = |text: &str| {
        if !body.is_empty() {
            body.push('\n');
        }
        body.push_str(text);
    };
    for op in ops {
        match op {
            DiffOp::Equal {
                old_index,
                new_index,
                len,
            } => {
                let (from, to) = (new.first + new_index, new.first + new_index + len - 1);
                let (old_from, old_to) = (old.first + old_index, old.first + old_index + len - 1);
                let moved = old_from != from;
                if moved && (old.anchored || new.anchored) {
                    return None;
                }
                let lines = if from == to {
                    format!("line {from}")
                } else {
                    format!("lines {from}-{to}")
                };
                let was = if !moved {
                    String::new()
                } else if old_from == old_to {
                    format!(" (line {old_from} there)")
                } else {
                    format!(" (lines {old_from}-{old_to} there)")
                };
                line(&format!(
                    "{lines} unchanged since call {}{was}",
                    old.call_id
                ));
            }
            DiffOp::Insert {
                new_index, new_len, ..
            }
            | DiffOp::Replace {
                new_index, new_len, ..
            } => {
                for (row, _) in &new.rows[new_index..new_index + new_len] {
                    line(row);
                }
            }
            DiffOp::Delete {
                old_len, new_index, ..
            } => {
                let mut note = String::new();
                if new_index > 0 && new_index < new.rows.len() {
                    let _ = write!(
                        note,
                        "[{old_len} line(s) removed after line {}]",
                        new.first + new_index - 1
                    );
                } else if new_index >= new.rows.len() {
                    // At the end: lines past a read that was cut off are not a
                    // change; a read that reached the end of the file says the
                    // file got shorter.
                    if !cut {
                        let _ = write!(
                            note,
                            "[{old_len} line(s) removed after line {}]",
                            new.last()
                        );
                    }
                } else if new.first == old.first {
                    // At the start: a read that began later did not show them.
                    let _ = write!(
                        note,
                        "[{old_len} line(s) removed before line {}]",
                        new.first
                    );
                }
                if !note.is_empty() {
                    line(&note);
                }
            }
        }
    }
    Some((body, changed))
}

/// `text` with each run of [`REPEAT_RUN_MIN`] or more consecutive identical
/// lines (compared without trailing whitespace) shown as its first line and
/// `[previous line repeated N more times]`, and how many runs that was;
/// `None` when no run is long enough.
pub(crate) fn collapse_repeated_lines(text: &str) -> Option<(String, usize)> {
    let lines = text.split('\n').collect::<Vec<_>>();
    let mut out = String::with_capacity(text.len());
    let mut runs = 0;
    let mut start = 0;
    while start < lines.len() {
        let key = lines[start].trim_end();
        let mut end = start + 1;
        while end < lines.len() && lines[end].trim_end() == key {
            end += 1;
        }
        if start > 0 {
            out.push('\n');
        }
        out.push_str(lines[start]);
        if end - start >= REPEAT_RUN_MIN {
            let _ = write!(
                out,
                "\n[previous line repeated {} more times]",
                end - start - 1
            );
            runs += 1;
        } else {
            for repeated in &lines[start + 1..end] {
                out.push('\n');
                out.push_str(repeated);
            }
        }
        start = end;
    }
    (runs > 0).then_some((out, runs))
}

/// Helpers the tests of this module and of the turn driver share.
#[cfg(test)]
pub(crate) mod testing {
    use std::fmt::Write as _;

    use harness_providers::ProviderMessage;
    use harness_types::ContentHash;

    use super::{Held, Reread};
    use crate::ToolOutput;
    use crate::turn_driver::render_tool_output;

    /// A file of `count` lines, `line 1` to `line N`.
    pub(crate) fn file(count: usize) -> Vec<String> {
        (1..=count).map(|number| format!("line {number}")).collect()
    }

    /// What `read_file` returns for `limit` lines of `lines` from `offset`.
    pub(crate) fn output(path: &str, lines: &[String], offset: usize, limit: usize) -> ToolOutput {
        let end = (offset + limit).min(lines.len());
        let mut content = (offset..end)
            .map(|index| format!("{}: {}", index + 1, lines[index]))
            .collect::<Vec<_>>()
            .join("\n");
        let truncated = end < lines.len();
        if truncated {
            let _ = write!(
                content,
                "\n\n[Showing lines {}-{end} of {}. Use offset={end} to continue.]",
                offset + 1,
                lines.len()
            );
        }
        ToolOutput::ReadFile {
            path: path.to_owned(),
            content,
            truncated,
            hash: ContentHash::from_bytes(lines.join("\n").as_bytes()),
            references: Vec::new(),
        }
    }

    /// One `read_file` call as the turn driver handles it: the text the model
    /// is sent, which is also put in the transcript.
    pub(crate) fn ask(
        reread: &mut Reread,
        transcript: &mut Vec<ProviderMessage>,
        call_id: &str,
        arguments: &str,
        output: &ToolOutput,
    ) -> String {
        let rendered = render_tool_output("read_file", output);
        let held = Held {
            transcript: transcript.as_slice(),
            batch: &[],
            fitted: &|_| false,
            folded_until: 0,
        };
        let text = if let Some(text) =
            reread.read_for_model(&held, call_id, arguments, output, &rendered)
        {
            text
        } else {
            reread.keep_read(call_id, output);
            rendered
        };
        transcript.push(ProviderMessage::tool_result(call_id, text.clone()));
        text
    }

    /// A command's output with `text` as its whole stdout.
    pub(crate) fn process(text: &str) -> ToolOutput {
        let stream = |text: &str| crate::capture::StreamTail {
            text: text.to_owned(),
            total_bytes: text.len() as u64,
            total_lines: text.split('\n').count() as u64,
            last_line_bytes: 0,
        };
        ToolOutput::Process {
            executable: "sh".to_owned(),
            shell: None,
            exit_code: Some(0),
            timed_out: false,
            canceled: false,
            queued: false,
            tree_cleanup_confirmed: true,
            tree_cleanup: "nothing_to_clean".to_owned(),
            stdout: String::new(),
            stderr: String::new(),
            stdout_truncated: false,
            stderr_truncated: false,
            artifact_id: Some("artifact-1".to_owned()),
            captured_bytes: text.len() as u64,
            capture_hash: None,
            capture_truncated: false,
            capture_tail: String::new(),
            stdout_tail: stream(text),
            stderr_tail: stream(""),
        }
    }
}

#[cfg(test)]
mod tests {
    use harness_providers::ProviderMessage;

    use super::testing::{ask, file, output, process};
    use super::{Held, Reread, RereadTally, collapse_repeated_lines, enabled_by, parse_numbered};
    use crate::ToolOutput;
    use crate::turn_driver::render_tool_output;

    fn reread() -> Reread {
        Reread::new(true, "task", "session")
    }

    fn hash_of(output: &ToolOutput) -> String {
        match output {
            ToolOutput::ReadFile { hash, .. } => hash.as_str().to_owned(),
            _ => unreachable!("a read"),
        }
    }

    const PATH: &str = "src/a.rs";
    const ARGS: &str = r#"{"path":"src/a.rs"}"#;

    #[test]
    fn an_unchanged_second_read_is_a_pointer_that_saves_bytes() {
        let lines = file(80);
        let read = output(PATH, &lines, 0, 200);
        let whole = render_tool_output("read_file", &read);
        let (mut reread, mut transcript) = (reread(), Vec::new());
        assert_eq!(ask(&mut reread, &mut transcript, "c1", ARGS, &read), whole);
        let mut second = String::new();
        let records = harness_types::diag::capture(|| {
            second = ask(&mut reread, &mut transcript, "c2", ARGS, &read);
        });
        let hash = hash_of(&read);
        assert_eq!(
            second,
            format!(
                "src/a.rs: unchanged since your read_file call c1 (sha256:{}, lines 1-80); that result above holds it. Pass \"fresh\": true to read it again.",
                &hash["sha256:".len()..][..12]
            )
        );
        assert!(second.len() < whole.len());
        assert_eq!(reread.tally.unchanged, 1);
        assert_eq!(
            reread.tally.saved_bytes,
            (whole.len() - second.len()) as u64
        );
        let event = records
            .iter()
            .find(|record| record.kind == "read" && record.event == "unchanged")
            .expect("an event");
        assert_eq!(event.fields["bytes"], whole.len() - second.len());
        // The transcript holds the pointer; the first result is untouched.
        assert_eq!(transcript[0].content, whole);
        assert_eq!(transcript[1].content, second);
    }

    #[test]
    fn fresh_true_reads_the_file_in_full() {
        let read = output(PATH, &file(80), 0, 200);
        let whole = render_tool_output("read_file", &read);
        let (mut reread, mut transcript) = (reread(), Vec::new());
        ask(&mut reread, &mut transcript, "c1", ARGS, &read);
        let again = ask(
            &mut reread,
            &mut transcript,
            "c2",
            r#"{"path":"src/a.rs","fresh":true}"#,
            &read,
        );
        assert_eq!(again, whole);
        assert_eq!(reread.tally, RereadTally::default());
        // Asking without it again is a pointer: fresh does not forget the read.
        let third = ask(&mut reread, &mut transcript, "c3", ARGS, &read);
        assert!(
            third.contains("unchanged since your read_file call"),
            "{third}"
        );
    }

    #[test]
    fn a_range_inside_an_earlier_whole_range_of_an_unchanged_file_is_a_pointer() {
        let lines = file(100);
        let (mut reread, mut transcript) = (reread(), Vec::new());
        ask(
            &mut reread,
            &mut transcript,
            "c1",
            ARGS,
            &output(PATH, &lines, 0, 100),
        );
        let inside = ask(
            &mut reread,
            &mut transcript,
            "c2",
            r#"{"path":"src/a.rs","offset":20,"limit":10}"#,
            &output(PATH, &lines, 20, 10),
        );
        assert!(
            inside.contains("unchanged since your read_file call c1"),
            "{inside}"
        );
        assert!(inside.contains("lines 21-30)"), "{inside}");
        // The notice that says where the range stopped is still there.
        assert!(
            inside.ends_with("[Showing lines 21-30 of 100. Use offset=30 to continue.]"),
            "{inside}"
        );
    }

    #[test]
    fn a_range_the_earlier_read_did_not_show_is_read_in_full() {
        let lines = file(100);
        let (mut reread, mut transcript) = (reread(), Vec::new());
        ask(
            &mut reread,
            &mut transcript,
            "c1",
            ARGS,
            &output(PATH, &lines, 0, 50),
        );
        let wider = output(PATH, &lines, 40, 40);
        let text = ask(&mut reread, &mut transcript, "c2", ARGS, &wider);
        assert_eq!(text, render_tool_output("read_file", &wider));
        assert_eq!(reread.tally.unchanged, 0);
    }

    #[test]
    fn a_read_after_a_one_line_edit_is_only_the_changed_lines() {
        let before = file(100);
        let mut after = before.clone();
        after[49] = "line 50, changed".to_owned();
        let (mut reread, mut transcript) = (reread(), Vec::new());
        ask(
            &mut reread,
            &mut transcript,
            "c1",
            ARGS,
            &output(PATH, &before, 0, 200),
        );
        let changed = output(PATH, &after, 0, 200);
        let mut text = String::new();
        let records = harness_types::diag::capture(|| {
            text = ask(&mut reread, &mut transcript, "c2", ARGS, &changed);
        });
        let hash = hash_of(&changed);
        assert_eq!(
            text,
            format!(
                "read_file src/a.rs [hash {hash}] (changed since your read_file call c1; only the changed lines are shown):\nlines 1-49 unchanged since call c1\n50: line 50, changed\nlines 51-100 unchanged since call c1"
            )
        );
        assert_eq!(reread.tally.delta, 1);
        assert!(
            records
                .iter()
                .any(|record| record.kind == "read" && record.event == "delta")
        );
        // The model now holds the file as it is: a read of that is a pointer.
        let third = ask(&mut reread, &mut transcript, "c3", ARGS, &changed);
        assert!(
            third.contains("unchanged since your read_file call c2"),
            "{third}"
        );
    }

    #[test]
    fn a_delta_says_where_the_lines_it_leaves_out_were() {
        let before = file(100);
        let mut after = before.clone();
        after.insert(9, "an inserted line".to_owned());
        after.remove(60);
        let (mut reread, mut transcript) = (reread(), Vec::new());
        ask(
            &mut reread,
            &mut transcript,
            "c1",
            ARGS,
            &output(PATH, &before, 0, 200),
        );
        let text = ask(
            &mut reread,
            &mut transcript,
            "c2",
            ARGS,
            &output(PATH, &after, 0, 200),
        );
        let body = text.split_once(":\n").expect("a header").1;
        assert_eq!(
            body,
            "lines 1-9 unchanged since call c1\n10: an inserted line\nlines 11-60 unchanged since call c1 (lines 10-59 there)\n[1 line(s) removed after line 60]\nlines 61-100 unchanged since call c1"
        );
    }

    /// The body of the delta a second read of `after` is answered with, after
    /// a first read of `before`; `limit` is how many lines the second asks for.
    fn delta_of(before: &[String], after: &[String], limit: usize) -> String {
        let (mut reread, mut transcript) = (reread(), Vec::new());
        ask(
            &mut reread,
            &mut transcript,
            "c1",
            ARGS,
            &output(PATH, before, 0, 200),
        );
        let text = ask(
            &mut reread,
            &mut transcript,
            "c2",
            ARGS,
            &output(PATH, after, 0, limit),
        );
        assert_eq!(reread.tally.delta, 1, "{text}");
        text.split_once(":\n").expect("a header").1.to_owned()
    }

    #[test]
    fn a_read_after_the_last_lines_were_deleted_says_so() {
        let before = file(100);
        let after = before[..97].to_vec();
        assert_eq!(
            delta_of(&before, &after, 200),
            "lines 1-97 unchanged since call c1\n[3 line(s) removed after line 97]"
        );
    }

    #[test]
    fn a_read_after_the_first_lines_were_deleted_says_so() {
        let before = file(100);
        let after = before[3..].to_vec();
        assert_eq!(
            delta_of(&before, &after, 200),
            "[3 line(s) removed before line 1]\nlines 1-97 unchanged since call c1 (lines 4-100 there)"
        );
    }

    #[test]
    fn a_read_cut_off_before_a_deletion_does_not_mention_it() {
        let before = file(100);
        // Lines 91-100 are gone and the file goes on, but the read stops at 80.
        let mut after = before[..90].to_vec();
        after.extend((91..=200).map(|number| format!("new {number}")));
        assert_eq!(
            delta_of(&before, &after, 80),
            "lines 1-80 unchanged since call c1\n\n[Showing lines 1-80 of 200. Use offset=80 to continue.]"
        );
    }

    #[test]
    fn a_large_change_reads_the_whole_file() {
        let before = file(100);
        let after = before
            .iter()
            .enumerate()
            .map(|(index, line)| {
                if index < 60 {
                    format!("{line} changed")
                } else {
                    line.clone()
                }
            })
            .collect::<Vec<_>>();
        let (mut reread, mut transcript) = (reread(), Vec::new());
        ask(
            &mut reread,
            &mut transcript,
            "c1",
            ARGS,
            &output(PATH, &before, 0, 200),
        );
        let read = output(PATH, &after, 0, 200);
        let text = ask(&mut reread, &mut transcript, "c2", ARGS, &read);
        assert_eq!(text, render_tool_output("read_file", &read));
        assert_eq!(reread.tally.delta, 0);
    }

    #[test]
    fn more_than_two_hundred_changed_lines_read_the_whole_file() {
        let before = file(500);
        let mut after = before.clone();
        for line in after.iter_mut().take(240) {
            line.push_str(" changed");
        }
        let (mut reread, mut transcript) = (reread(), Vec::new());
        ask(
            &mut reread,
            &mut transcript,
            "c1",
            ARGS,
            &output(PATH, &before, 0, 500),
        );
        let read = output(PATH, &after, 0, 500);
        let text = ask(&mut reread, &mut transcript, "c2", ARGS, &read);
        assert_eq!(text, render_tool_output("read_file", &read));
        // The same file with fewer changed lines is a delta.
        let mut fewer = before.clone();
        for line in fewer.iter_mut().take(150) {
            line.push_str(" changed");
        }
        let read = output(PATH, &fewer, 0, 500);
        let text = ask(&mut reread, &mut transcript, "c3", ARGS, &read);
        assert!(text.contains("only the changed lines are shown"), "{text}");
    }

    #[test]
    fn a_change_outside_the_requested_range_leaves_a_single_unchanged_line() {
        let before = file(100);
        let mut after = before.clone();
        after[90] = "line 91, changed".to_owned();
        let (mut reread, mut transcript) = (reread(), Vec::new());
        ask(
            &mut reread,
            &mut transcript,
            "c1",
            ARGS,
            &output(PATH, &before, 0, 100),
        );
        let text = ask(
            &mut reread,
            &mut transcript,
            "c2",
            r#"{"path":"src/a.rs","limit":20}"#,
            &output(PATH, &after, 0, 20),
        );
        assert!(
            text.ends_with(
                "lines 1-20 unchanged since call c1\n\n[Showing lines 1-20 of 100. Use offset=20 to continue.]"
            ),
            "{text}"
        );
    }

    #[test]
    fn hashline_anchors_name_their_line_so_a_shift_reads_in_full() {
        let anchored = |lines: &[&str]| {
            let content = lines
                .iter()
                .enumerate()
                .map(|(index, line)| {
                    format!("{}: {line}", crate::edit_diff::line_anchor(index + 1, line))
                })
                .collect::<Vec<_>>()
                .join("\n");
            ToolOutput::ReadFile {
                path: PATH.to_owned(),
                content,
                truncated: false,
                hash: harness_types::ContentHash::from_bytes(lines.join("\n").as_bytes()),
                references: Vec::new(),
            }
        };
        let before = (1..=40).map(|n| format!("row {n}")).collect::<Vec<_>>();
        let first = before.iter().map(String::as_str).collect::<Vec<_>>();
        let mut shifted = first.clone();
        shifted.insert(2, "new row");
        let mut edited = first.clone();
        edited[2] = "row three, edited";
        let (mut reread, mut transcript) = (reread(), Vec::new());
        ask(&mut reread, &mut transcript, "c1", ARGS, &anchored(&first));
        // Nothing moved: only the changed line is sent, anchor and all.
        let text = ask(&mut reread, &mut transcript, "c2", ARGS, &anchored(&edited));
        assert!(text.contains("\n3#"), "{text}");
        assert!(text.contains("lines 4-40 unchanged"), "{text}");
        // A line inserted moves the lines under it, and their anchors with it.
        let read = anchored(&shifted);
        let text = ask(&mut reread, &mut transcript, "c3", ARGS, &read);
        assert_eq!(text, render_tool_output("read_file", &read));
    }

    #[test]
    fn a_result_that_is_no_longer_whole_is_not_pointed_to() {
        let read = output(PATH, &file(80), 0, 200);
        let whole = render_tool_output("read_file", &read);
        let mut reread = reread();
        let first = ProviderMessage::tool_result("c1", whole.clone());
        reread.keep_read("c1", &read);
        let ask_with =
            |reread: &mut Reread, transcript: &[ProviderMessage], fitted: bool, folded: usize| {
                let held = Held {
                    transcript,
                    batch: &[],
                    fitted: &move |_: &str| fitted,
                    folded_until: folded,
                };
                reread.read_for_model(&held, "c2", ARGS, &read, &whole)
            };
        // Whole.
        assert!(ask_with(&mut reread, std::slice::from_ref(&first), false, 0).is_some());
        // The runtime sent it shortened to fit the window.
        assert!(ask_with(&mut reread, std::slice::from_ref(&first), true, 0).is_none());
        // The turn's early steps were folded into a summary.
        assert!(ask_with(&mut reread, std::slice::from_ref(&first), false, 1).is_none());
        // The driver shortened it, elided it, or it left the transcript.
        for content in [
            "[earlier tool result shortened to save context; call the tool again if you need it]",
            "[earlier tool result shortened to its start to save context; call the tool again for all of it]\nread_file",
        ] {
            let gone = ProviderMessage::tool_result("c1", content);
            assert!(
                ask_with(&mut reread, &[gone], false, 0).is_none(),
                "{content}"
            );
        }
        assert!(ask_with(&mut reread, &[], false, 0).is_none());
        // A read of the step being built is whole.
        let held = Held {
            transcript: &[],
            batch: std::slice::from_ref(&first),
            fitted: &|_| false,
            folded_until: 0,
        };
        assert!(
            reread
                .read_for_model(&held, "c2", ARGS, &read, &whole)
                .is_some()
        );
    }

    #[test]
    fn a_call_naming_several_files_points_only_at_the_ones_it_holds() {
        let a = output("src/a.rs", &file(60), 0, 200);
        let b = output("src/b.rs", &file(70), 0, 200);
        let (mut reread, mut transcript) = (reread(), Vec::new());
        ask(
            &mut reread,
            &mut transcript,
            "c1",
            r#"{"path":"src/a.rs"}"#,
            &a,
        );
        // One call, two files: the first the model holds, the second it does not.
        let args = r#"{"paths":["src/a.rs","src/b.rs"]}"#;
        let mut parts = Vec::new();
        for read in [&a, &b] {
            let rendered = render_tool_output("read_file", read);
            let held = Held {
                transcript: &transcript,
                batch: &[],
                fitted: &|_| false,
                folded_until: 0,
            };
            let sent = reread.read_for_model(&held, "c2", args, read, &rendered);
            if sent.is_none() {
                reread.keep_read("c2", read);
            }
            parts.push((sent, rendered));
        }
        let pointer = parts[0].0.clone().expect("the held file is a pointer");
        assert!(
            pointer.contains("unchanged since your read_file call c1"),
            "{pointer}"
        );
        assert!(parts[1].0.is_none(), "the new file is sent in full");
        transcript.push(ProviderMessage::tool_result(
            "c2",
            format!("{pointer}\n\n{}", parts[1].1),
        ));
        // A later read of the new file points into that one result.
        let third = ask(
            &mut reread,
            &mut transcript,
            "c3",
            r#"{"path":"src/b.rs"}"#,
            &b,
        );
        assert!(
            third.contains("unchanged since your read_file call c2"),
            "{third}"
        );
        // And a read of the first is a pointer to the first read, not to a pointer.
        let fourth = ask(
            &mut reread,
            &mut transcript,
            "c4",
            r#"{"path":"src/a.rs"}"#,
            &a,
        );
        assert!(
            fourth.contains("unchanged since your read_file call c1"),
            "{fourth}"
        );
    }

    #[test]
    fn a_pointer_is_replaced_by_its_text_when_what_it_points_to_is_gone() {
        let read = output(PATH, &file(80), 0, 200);
        let whole = render_tool_output("read_file", &read);
        let (mut reread, mut transcript) = (reread(), Vec::new());
        ask(&mut reread, &mut transcript, "c1", ARGS, &read);
        let pointer = ask(&mut reread, &mut transcript, "c2", ARGS, &read);
        assert_ne!(pointer, whole);
        // Still whole: nothing changes.
        reread.reconcile(&mut transcript, &|_| false, 0);
        assert_eq!(transcript[1].content, pointer);
        assert_eq!(reread.tally.unchanged, 1);
        // The runtime fitted c1: the second result is the text again.
        reread.reconcile(&mut transcript, &|id| id == "c1", 0);
        assert_eq!(transcript[1].content, whole);
        assert_eq!(reread.tally, RereadTally::default());
        // A pointer whose own result was elided is forgotten, not restored.
        let again = ask(&mut reread, &mut transcript, "c3", ARGS, &read);
        assert!(again.contains("unchanged"), "{again}");
        transcript[2].content = "[earlier tool result shortened to save context]".to_owned();
        reread.reconcile(&mut transcript, &|id| id == "c1", 0);
        assert!(transcript[2].content.starts_with("[earlier tool result"));
    }

    #[test]
    fn nothing_is_shortened_while_the_switch_is_off() {
        let read = output(PATH, &file(80), 0, 200);
        let whole = render_tool_output("read_file", &read);
        let mut reread = Reread::new(false, "task", "session");
        let mut transcript = Vec::new();
        ask(&mut reread, &mut transcript, "c1", ARGS, &read);
        assert_eq!(ask(&mut reread, &mut transcript, "c2", ARGS, &read), whole);
        let noisy = format!("start\n{}\nend", "same\n".repeat(10));
        assert!(reread.output_for_model("run_shell", "c3", &noisy).is_none());
        assert_eq!(reread.tally, RereadTally::default());
        assert!(enabled_by(None));
        assert!(enabled_by(Some("on")));
        assert!(!enabled_by(Some("off")));
        assert!(!enabled_by(Some(" OFF ")));
        assert!(!enabled_by(Some("0")));
    }

    #[test]
    fn a_line_too_long_for_the_limit_is_not_kept() {
        let content = "1: aaaa\n\n[Line 1 is 80.0KB, exceeds 50.0KB limit; showing its first 50.0KB. Use offset=1 to continue after it.]";
        assert!(parse_numbered(content).is_none());
        let rows = parse_numbered(
            "5: a\n6: \n7: c\n\n[Showing lines 5-7 of 90. Use offset=7 to continue.]",
        )
        .expect("rows");
        assert_eq!((rows.first, rows.last(), rows.rows.len()), (5, 7, 3));
        assert_eq!(rows.rows[1].1, "");
        assert!(rows.trailer.contains("[Showing lines 5-7"));
    }

    #[test]
    fn four_identical_lines_are_shown_once_with_the_count() {
        let (text, runs) = collapse_repeated_lines("a\nb\nb\nb\nb\nc").expect("a run of four");
        assert_eq!(text, "a\nb\n[previous line repeated 3 more times]\nc");
        assert_eq!(runs, 1);
        let (text, _) =
            collapse_repeated_lines(&format!("x\n{}y", "tick\n".repeat(1000))).expect("a long run");
        assert_eq!(text, "x\ntick\n[previous line repeated 999 more times]\ny");
    }

    #[test]
    fn three_identical_lines_are_not_collapsed() {
        assert!(collapse_repeated_lines("a\nb\nb\nb\nc").is_none());
        assert!(collapse_repeated_lines("b\nb\nb").is_none());
        // Not consecutive.
        assert!(collapse_repeated_lines("b\nb\nc\nb\nb\nc\nb\nb").is_none());
    }

    #[test]
    fn trailing_whitespace_does_not_separate_repeated_lines() {
        let (text, runs) =
            collapse_repeated_lines("go \ngo\ngo  \r\ngo\nend\n\n\n\n\n").expect("two runs");
        assert_eq!(
            text,
            "go \n[previous line repeated 3 more times]\nend\n\n[previous line repeated 4 more times]"
        );
        assert_eq!(runs, 2);
    }

    #[test]
    fn a_command_result_is_collapsed_and_the_output_it_came_from_is_kept() {
        let noisy = format!("compiling\n{}done", "retrying connection\n".repeat(500));
        let output = process(&noisy);
        let kept = output.clone();
        let rendered = render_tool_output("run_shell", &output);
        let mut reread = reread();
        let mut text = String::new();
        let records = harness_types::diag::capture(|| {
            text = reread
                .output_for_model("run_shell", "c1", &rendered)
                .expect("collapsed");
        });
        assert!(
            text.contains("retrying connection\n[previous line repeated 499 more times]\ndone"),
            "{text}"
        );
        assert!(text.len() < rendered.len() / 10);
        assert_eq!(reread.tally.collapsed, 1);
        assert_eq!(
            reread.tally.saved_bytes,
            (rendered.len() - text.len()) as u64
        );
        let event = records
            .iter()
            .find(|record| record.kind == "output" && record.event == "repeats_collapsed")
            .expect("an event");
        assert_eq!(event.fields["bytes"], rendered.len() - text.len());
        // The output itself, which the capture and the artifact come from, is
        // not touched.
        assert_eq!(output, kept);
        // The other tools that run commands, and no others.
        for tool in ["run_process", "read_process_output"] {
            assert!(reread.output_for_model(tool, "c2", &rendered).is_some());
        }
        assert!(
            reread
                .output_for_model("search_text", "c3", &rendered)
                .is_none()
        );
        // A run whose note would be longer than its lines is left alone.
        assert!(
            reread
                .output_for_model("run_shell", "c4", "a\na\na\na\na\n")
                .is_none()
        );
    }

    #[test]
    fn a_cut_stream_keeps_its_notice_and_where_to_read_the_rest_when_collapsed() {
        let many = format!("{}tail", "same line\n".repeat(5_000));
        let rendered = render_tool_output("run_shell", &process(&many));
        assert!(rendered.contains("[Showing lines"), "{}", rendered.len());
        assert!(rendered.contains("read_process_output artifact_id=artifact-1"));
        let (collapsed, runs) = collapse_repeated_lines(&rendered).expect("collapsed");
        assert_eq!(runs, 1);
        assert!(collapsed.contains("[Showing lines"), "{collapsed}");
        assert!(
            collapsed.contains("read_process_output artifact_id=artifact-1"),
            "{collapsed}"
        );
        assert!(collapsed.len() < 800, "{}", collapsed.len());
    }
}
