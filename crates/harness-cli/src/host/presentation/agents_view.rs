//! prime-agent's agents view (`pa-tui/src/agents_view*`): one screen over
//! every session - the agents running in the background workers and the
//! project's saved conversations - grouped Running / Idle / Inactive.
//!
//! The prompt line searches (a ranked flat list while a query is typed); the
//! arrows choose a row; Enter or Right opens it - attaching to a running
//! agent, resuming a saved conversation as one - and the session's own Left
//! on an empty prompt (or a bare `/resume`) comes back here. Space writes a
//! reply: sent to an agent, or the prompt a saved conversation resumes
//! with in the background. Ctrl+R renames an agent, Ctrl+X stops a running
//! one or deletes a saved one (press twice), Ctrl+N starts a new session, Esc
//! goes back to the session the view was opened from (`ha agents` has none:
//! there Esc leaves). The roster refreshes every second.
//!
//! prime-agent's subagent forest: an agent with subagents carries one
//! `N subagents (M running)` line; Alt+Right expands it to the subagents
//! (running first, nested by depth) and Ctrl+O shows the program each was
//! spawned with. A row shows its heartbeats as `◷N`. The mouse selects a row
//! with a click and scrolls with the wheel.
//!
//! ha's store keeps every conversation, so deleting a saved one hides it
//! from the listings rather than removing it. Not ported: the cost column.

use std::fmt::Write as _;
use std::io::{self, Write as _};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind,
};
use crossterm::{execute, terminal};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::agents::{client, protocol};
use super::bootstrap::LaunchContext;
use super::paths::LaunchEnvironment;
use super::service::SavedConversation;
use super::tui::theme::Theme;

/// The exit code a session's terminal leaves with when it asked for the
/// agents view (Left on an empty prompt, a bare `/resume`): the launch shows
/// the view instead of exiting.
pub const AGENTS_VIEW_EXIT: u8 = 254;

/// How many saved conversations the view lists.
const SAVED_LIMIT: usize = 50;

/// How often the roster is read again.
const REFRESH: Duration = Duration::from_secs(1);

/// prime-agent's sections, in their order.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Section {
    Running,
    Idle,
    Inactive,
}

impl Section {
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::Running => "Running",
            Self::Idle => "Idle",
            Self::Inactive => "Inactive",
        }
    }
}

/// What a row stands for. An agent's rows share one copy of what its worker
/// listed: a copy per row - its subagents and their spawn programs with it -
/// was most of what a frame of a large forest cost.
#[derive(Clone, Debug)]
pub enum Target {
    Live(std::sync::Arc<client::Listed>),
    Saved(SavedConversation),
}

/// prime-agent's row shapes (`RowKind`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RowKind {
    /// A top-level agent or saved session.
    Agent,
    /// The `N subagents (M running)` line under an agent.
    SubagentSummary,
    /// A nested subagent of an expanded line.
    Subagent,
    /// A subagent's spawn program, read-only.
    Code,
}

/// One session row.
#[derive(Clone, Debug)]
pub struct Row {
    pub section: Section,
    pub identity: String,
    pub title: String,
    pub model: String,
    pub age: String,
    pub target: Target,
    pub kind: RowKind,
    /// Nesting depth: 0 for an agent row.
    pub depth: usize,
    /// Why a search found the row, when the match is not in its title or
    /// model: `request: …luna…`.
    pub note: String,
}

impl Row {
    /// prime-agent's `selectable`: a program line is read-only context.
    #[must_use]
    pub fn selectable(&self) -> bool {
        self.kind != RowKind::Code
    }
}

/// What the forest shows: the expanded subagent lines and the parents whose
/// spawn programs show.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Forest {
    pub expanded: std::collections::HashSet<String>,
    pub programs: std::collections::HashSet<String>,
}

/// The agent a session's sidebar asked to switch to, for the launch to open
/// without showing the view (`request_open`, then `AGENTS_VIEW_EXIT`).
static OPEN_NEXT: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Ask the launch to open agent `id` once this terminal hands over.
pub fn request_open(id: String) {
    if let Ok(mut next) = OPEN_NEXT.lock() {
        *next = Some(id);
    }
}

/// The agent a session asked to switch to, if it is still running on this
/// build: what the view's Enter would open for it.
#[must_use]
pub fn take_requested() -> Option<Choice> {
    let id = OPEN_NEXT.lock().ok()?.take()?;
    let directory = client::user_registry().ok()?;
    client::list_all(&directory)
        .into_iter()
        .find(|listed| {
            listed.agent.id == id
                && listed.descriptor.build == super::agents::registry::build_identity()
        })
        .map(|listed| Choice::Open(Box::new(listed)))
}

/// What the user chose.
#[derive(Debug)]
pub enum Choice {
    /// Attach this terminal to a running agent.
    Open(Box<client::Listed>),
    /// Resume a saved conversation as an agent and attach to it.
    Resume(String),
    /// Start a new session.
    New,
    Quit,
}

fn age_label(seconds: u64) -> String {
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m", seconds / 60),
        3600..86_400 => format!("{}h", seconds / 3600),
        _ => format!("{}d", seconds / 86_400),
    }
}

fn first_line(text: &str) -> String {
    text.lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default()
        .trim()
        .to_owned()
}

/// The rows: running and idle agents, then the saved conversations no agent
/// runs. With a query, one flat list ranked by prime-agent's fuzzy match.
#[cfg(test)]
#[must_use]
pub fn rows(
    live: &[client::Listed],
    saved: &[SavedConversation],
    query: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<Row> {
    rows_with(live, saved, query, now, &Forest::default())
}

/// [`rows`] with the forest's expanded lines and shown programs.
#[must_use]
#[allow(clippy::too_many_lines)] // prime-agent's row builder, one shape per arm
pub fn rows_with(
    live: &[client::Listed],
    saved: &[SavedConversation],
    query: &str,
    now: chrono::DateTime<chrono::Utc>,
    forest: &Forest,
) -> Vec<Row> {
    let mut rows = Vec::new();
    for listed in live {
        let shared = std::sync::Arc::new(listed.clone());
        let agent = &listed.agent;
        // An active turn (the app's `has_active_run` phases, or a question
        // waiting on the user) or children still working; a ready or
        // unconfigured agent is idle.
        let running = agent.busy
            || matches!(
                agent.status.as_str(),
                "running"
                    | "waiting_approval"
                    | "waiting_input"
                    | "waiting_mcp_input"
                    | "canceling"
            );
        // The name the user gave, the session's title, the saved
        // conversation's, then the last request - unless that was a command
        // (`/compact` named a row of its own) or a scheduled run's prompt (a
        // conversation woken after a usage limit was titled
        // `[heartbeat: at ... run#1]`). Seen: a session answered "ok" was
        // listed, and searched, as "ok".
        let saved_title = agent.conversation.as_deref().and_then(|task| {
            saved
                .iter()
                .find(|conversation| conversation.task_id == task)
                .and_then(|conversation| conversation.title.clone())
        });
        let mut title = agent
            .name
            .clone()
            .or_else(|| agent.title.clone())
            .or(saved_title)
            .or_else(|| {
                agent
                    .last_request
                    .as_deref()
                    .filter(|request| {
                        let request = request.trim_start();
                        !request.starts_with('/') && !request.starts_with("[heartbeat:")
                    })
                    .map(first_line)
            })
            .filter(|title| !title.is_empty())
            .unwrap_or_else(|| agent.id.clone());
        // prime-agent's row heartbeat count.
        if agent.heartbeats > 0 {
            let _ = write!(title, " ◷{}", agent.heartbeats);
        }
        rows.push(Row {
            section: if running {
                Section::Running
            } else {
                Section::Idle
            },
            identity: format!("live:{}", agent.id),
            title,
            model: agent.model.clone(),
            // A turn at work is "now"; one waiting on the user says how long
            // it has waited - an unattended question read "now" for days.
            age: if running && !agent.status.starts_with("waiting") {
                "now".to_owned()
            } else {
                age_label(agent.idle_seconds)
            },
            target: Target::Live(std::sync::Arc::clone(&shared)),
            kind: RowKind::Agent,
            depth: 0,
            note: String::new(),
        });
        // prime-agent's subagent forest: one summary line, expanded to the
        // subagents, running first.
        if !agent.subagents.is_empty() {
            let section = rows.last().map_or(Section::Idle, |row| row.section);
            let running = agent
                .subagents
                .iter()
                .filter(|child| child.status == "running" && !child.queued)
                .count();
            let failed = agent
                .subagents
                .iter()
                .filter(|child| child.status == "error")
                .count();
            let parent = format!("live:{}", agent.id);
            let expanded = forest.expanded.contains(&parent);
            rows.push(Row {
                section,
                identity: format!("subagents:{}", agent.id),
                title: format!(
                    "{} {} subagents ({running} running{})",
                    if expanded { "▾" } else { "▸" },
                    agent.subagents.len(),
                    if failed > 0 {
                        format!(", {failed} failed")
                    } else {
                        String::new()
                    }
                ),
                model: String::new(),
                age: String::new(),
                target: Target::Live(std::sync::Arc::clone(&shared)),
                kind: RowKind::SubagentSummary,
                depth: 1,
                note: String::new(),
            });
            if expanded {
                for child in &agent.subagents {
                    let row = subagent_row(section, &shared, child);
                    let depth = row.depth;
                    rows.push(row);
                    if forest.programs.contains(&parent) && !child.program.trim().is_empty() {
                        rows.push(Row {
                            section,
                            identity: format!("code:{}:{}", agent.id, child.name),
                            title: first_line(&child.program),
                            model: String::new(),
                            age: String::new(),
                            target: Target::Live(std::sync::Arc::clone(&shared)),
                            kind: RowKind::Code,
                            depth: depth + 1,
                            note: String::new(),
                        });
                    }
                }
            }
        }
    }
    let live_tasks = live
        .iter()
        .filter_map(|listed| listed.agent.conversation.clone())
        .collect::<std::collections::HashSet<_>>();
    for conversation in saved {
        if live_tasks.contains(&conversation.task_id) {
            continue;
        }
        let age =
            chrono::NaiveDateTime::parse_from_str(&conversation.updated_at, "%Y-%m-%d %H:%M:%S")
                .ok()
                .and_then(|at| u64::try_from((now - at.and_utc()).num_seconds()).ok())
                .map_or_else(|| "-".to_owned(), age_label);
        rows.push(Row {
            section: Section::Inactive,
            identity: format!("saved:{}", conversation.task_id),
            title: conversation
                .title
                .clone()
                .unwrap_or_else(|| "untitled session".to_owned()),
            model: conversation.model.clone().unwrap_or_else(|| "-".to_owned()),
            age,
            target: Target::Saved(conversation.clone()),
            kind: RowKind::Agent,
            depth: 0,
            note: String::new(),
        });
    }
    // A stable sort: a forest stays under its agent.
    rows.sort_by_key(|row| row.section);
    let query = query.trim();
    if query.is_empty() {
        return rows;
    }
    search(rows, saved, &tokens(query))
}

/// A subagent's row under its agent: its name and state, the model and the
/// thinking level it runs at.
fn subagent_row(
    section: Section,
    shared: &std::sync::Arc<client::Listed>,
    child: &super::agents::protocol::SubagentInfo,
) -> Row {
    Row {
        section,
        identity: format!("sub:{}:{}", shared.agent.id, child.name),
        title: format!("{} · {}", child.name, child_state(child)),
        model: match child.thinking.as_deref() {
            Some(level) if level != "off" => format!("{} {level}", child.model),
            _ => child.model.clone(),
        },
        age: String::new(),
        target: Target::Live(std::sync::Arc::clone(shared)),
        kind: RowKind::Subagent,
        depth: usize::try_from(child.depth).unwrap_or(1).max(1) + 1,
        note: String::new(),
    }
}

/// A subagent's state as its row says it: `queued` while it waits for a
/// worker, else what the registry reports.
fn child_state(child: &super::agents::protocol::SubagentInfo) -> &str {
    if child.queued {
        "queued"
    } else {
        &child.status
    }
}

/// The words of a query, folded as the text they are looked for in.
#[must_use]
pub fn tokens(query: &str) -> Vec<String> {
    fold(query).split_whitespace().map(str::to_owned).collect()
}

/// One thing a search reads: its text, what to call it when the row does not
/// show it, and whether it is a long text (a request, a brief), where only
/// the word itself counts: a long request spells nearly any word's letters
/// in order somewhere.
struct Field<'a> {
    text: &'a str,
    label: &'static str,
    shown: bool,
    long: bool,
}

impl<'a> Field<'a> {
    const fn new(text: &'a str, label: &'static str, shown: bool, long: bool) -> Self {
        Self {
            text,
            label,
            shown,
            long,
        }
    }
}

/// The longest field a word may be spelled across (`gpt6` in `gpt-6-luna`):
/// in a longer one, a title taken from a request, only the word counts.
const FUZZY_CHARS: usize = 32;

/// How well the fields answer every token (lower is better), each token in
/// one field, and the note for a token found only in a field the row does
/// not show.
fn field_match(tokens: &[String], fields: &[Field<'_>]) -> Option<(f64, String)> {
    let folded: Vec<String> = fields.iter().map(|field| fold(field.text)).collect();
    let mut total = 0.0;
    let mut note = String::new();
    for token in tokens {
        let mut best: Option<(f64, &Field<'_>)> = None;
        for (field, text) in fields.iter().zip(&folded) {
            let score = if let Some(at) = text.find(token.as_str()) {
                // The word itself, nearer the field's start the better.
                Some(f64::from(u32::try_from(at.min(999)).unwrap_or(999)) / 1000.0)
            } else if field.long || text.chars().count() > FUZZY_CHARS {
                None
            } else {
                super::commands::fuzzy_score(token, text).map(|score| score + 10.0)
            };
            // What the row shows ranks above what it does not.
            let score = score.map(|score| score + if field.shown { 0.0 } else { 2.0 });
            if let Some(score) = score
                && best.is_none_or(|(kept, _)| score < kept)
            {
                best = Some((score, field));
            }
        }
        let (score, field) = best?;
        total += score;
        if !field.shown && note.is_empty() {
            note = format!("{}: {}", field.label, snippet(field.text, token));
        }
    }
    Some((total, note))
}

/// A few words of `text` around the first place it holds `token`.
fn snippet(text: &str, token: &str) -> String {
    let (folded, map) = folded_chars(text);
    let characters: Vec<char> = text.chars().collect();
    let Some(at) = folded.find(token) else {
        return first_line(text);
    };
    let begin = map[folded[..at].chars().count()];
    let from = begin.saturating_sub(16);
    let to = (begin + token.chars().count() + 24).min(characters.len());
    let mut shown: String = characters[from..to]
        .iter()
        .map(|character| {
            if character.is_whitespace() {
                ' '
            } else {
                *character
            }
        })
        .collect();
    if from > 0 {
        shown.insert(0, '…');
    }
    if to < characters.len() {
        shown.push('…');
    }
    shown
}

/// `text` folded (as [`fold`] does), and for each of its characters the
/// character of `text` it came from.
#[must_use]
pub fn folded_chars(text: &str) -> (String, Vec<usize>) {
    let mut folded = String::new();
    let mut map = Vec::new();
    let mut buffer = [0; 4];
    for (index, character) in text.chars().enumerate() {
        for each in fold(character.encode_utf8(&mut buffer)).chars() {
            folded.push(each);
            map.push(index);
        }
    }
    (folded, map)
}

/// A query's hits. An agent is found by its title, name, model or id, its
/// conversation's title or its last request; a subagent by its name (which
/// starts with its role), state, model, thinking level or brief, and is
/// listed under its agent. Groups are ranked by their best hit. Every row
/// used to be read as one text of all its fields, and subagents not at
/// all: `luna` found two sessions through their model and none of the
/// children working on gpt-6-luna.
fn search(rows: Vec<Row>, saved: &[SavedConversation], tokens: &[String]) -> Vec<Row> {
    let mut groups: Vec<(f64, Vec<Row>)> = Vec::new();
    for mut row in rows.into_iter().filter(|row| row.kind == RowKind::Agent) {
        let listed = match &row.target {
            Target::Live(listed) => Some(std::sync::Arc::clone(listed)),
            Target::Saved(_) => None,
        };
        let saved_title = listed.as_ref().and_then(|listed| {
            let task = listed.agent.conversation.as_deref()?;
            saved
                .iter()
                .find(|conversation| conversation.task_id == task)
                .and_then(|conversation| conversation.title.clone())
        });
        let own = {
            let mut fields = vec![
                Field::new(&row.title, "title", true, false),
                Field::new(&row.model, "model", true, false),
            ];
            if let Some(listed) = &listed {
                let agent = &listed.agent;
                fields.push(Field::new(&agent.id, "id", false, false));
                for (text, label) in [(&agent.name, "name"), (&agent.title, "title")] {
                    if let Some(text) = text {
                        fields.push(Field::new(text, label, false, false));
                    }
                }
                if let Some(text) = &saved_title {
                    fields.push(Field::new(text, "title", false, false));
                }
                if let Some(text) = &agent.last_request {
                    fields.push(Field::new(text, "request", false, true));
                }
            }
            field_match(tokens, &fields)
        };
        let mut children = Vec::new();
        if let Some(listed) = &listed {
            for child in &listed.agent.subagents {
                let thinking = child.thinking.clone().unwrap_or_default();
                let fields = [
                    Field::new(&child.name, "name", true, false),
                    Field::new(child_state(child), "state", true, false),
                    Field::new(&child.model, "model", true, false),
                    Field::new(&thinking, "thinking", true, false),
                    Field::new(&child.program, "brief", false, true),
                ];
                if let Some((score, note)) = field_match(tokens, &fields) {
                    let mut found = subagent_row(row.section, listed, child);
                    found.note = note;
                    children.push((score, found));
                }
            }
        }
        let best = own
            .as_ref()
            .map(|(score, _)| *score)
            .into_iter()
            .chain(children.iter().map(|(score, _)| *score))
            .min_by(f64::total_cmp);
        let Some(best) = best else {
            continue;
        };
        if let Some((_, note)) = own {
            row.note = note;
        }
        children.sort_by(|(left, _), (right, _)| left.total_cmp(right));
        let mut group = vec![row];
        group.extend(children.into_iter().map(|(_, child)| child));
        groups.push((best, group));
    }
    groups.sort_by(|(left, _), (right, _)| left.total_cmp(right));
    groups.into_iter().flat_map(|(_, group)| group).collect()
}

/// The spans with every place one of `tokens` is found (accents and case
/// aside) restyled with `style` over the span's own: why a row is a hit.
fn highlight(
    spans: Vec<Span<'static>>,
    tokens: &[String],
    style: ratatui::style::Style,
) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    for span in spans {
        let text = span.content.to_string();
        let (folded, map) = folded_chars(&text);
        let characters: Vec<char> = text.chars().collect();
        let mut marked = vec![false; characters.len()];
        for token in tokens {
            let length = token.chars().count();
            for (at, _) in folded.match_indices(token.as_str()) {
                let first = folded[..at].chars().count();
                for index in first..first + length {
                    if let Some(&original) = map.get(index) {
                        marked[original] = true;
                    }
                }
            }
        }
        if !marked.contains(&true) {
            out.push(span);
            continue;
        }
        let mut start = 0;
        while start < characters.len() {
            let on = marked[start];
            let mut end = start;
            while end < characters.len() && marked[end] == on {
                end += 1;
            }
            let piece: String = characters[start..end].iter().collect();
            out.push(Span::styled(
                piece,
                if on {
                    span.style.patch(style)
                } else {
                    span.style
                },
            ));
            start = end;
        }
    }
    out
}

/// Lowercase without the accents of Latin letters (`đ` as `d`).
fn fold(text: &str) -> String {
    use unicode_normalization::UnicodeNormalization as _;
    text.replace(['đ', 'Đ'], "d")
        .nfd()
        .filter(|character| !unicode_normalization::char::is_combining_mark(*character))
        .collect::<String>()
        .to_lowercase()
}

/// What the prompt line is doing.
#[derive(Clone, Debug, PartialEq)]
enum Composer {
    Search,
    Reply(String),
    Rename(String),
}

/// The view's state between frames.
struct View {
    registry: PathBuf,
    live: Vec<client::Listed>,
    saved: Vec<SavedConversation>,
    query: String,
    draft: String,
    composer: Composer,
    selected: Option<String>,
    status: Option<String>,
    stop_armed: Option<(String, Instant)>,
    forest: Forest,
    /// Where the list's first row was drawn, for the mouse.
    list_rows: std::cell::RefCell<Vec<(u16, String)>>,
    store_dir: PathBuf,
    /// The agent whose session opened the view: Esc goes back to it.
    came_from: Option<String>,
    /// The roster and the saved conversations are still being read.
    loading: bool,
    /// Bumped whenever the roster or the saved conversations change.
    data_version: u64,
    /// The rows of the last frame and what they were built from: a frame,
    /// and every key, read them again, and a large forest took long to build.
    rows_cache: std::cell::RefCell<Option<(RowsKey, std::rc::Rc<Vec<Row>>)>>,
}

/// What the rows are built from: the data's version, the search, the forest,
/// and the second (for the ages).
type RowsKey = (u64, String, Forest, i64);

impl View {
    /// Esc: back to the session the view was opened from while its agent
    /// still runs on this build, else out of the view.
    fn back(&self) -> Choice {
        self.came_from
            .as_ref()
            .and_then(|id| {
                self.live.iter().find(|listed| {
                    listed.agent.id == *id
                        && listed.descriptor.build == super::agents::registry::build_identity()
                })
            })
            .map_or(Choice::Quit, |listed| {
                Choice::Open(Box::new(listed.clone()))
            })
    }

    fn rows(&self) -> std::rc::Rc<Vec<Row>> {
        let now = chrono::Utc::now();
        let key = (
            self.data_version,
            self.query.clone(),
            self.forest.clone(),
            now.timestamp(),
        );
        if let Some((built, rows)) = &*self.rows_cache.borrow()
            && *built == key
        {
            return std::rc::Rc::clone(rows);
        }
        let rows = std::rc::Rc::new(rows_with(
            &self.live,
            &self.saved,
            &self.query,
            now,
            &self.forest,
        ));
        *self.rows_cache.borrow_mut() = Some((key, std::rc::Rc::clone(&rows)));
        rows
    }

    /// The selected row, kept on its identity while the roster changes.
    fn selection(&self, rows: &[Row]) -> Option<usize> {
        if rows.is_empty() {
            return None;
        }
        Some(
            self.selected
                .as_ref()
                .and_then(|identity| rows.iter().position(|row| &row.identity == identity))
                .unwrap_or(0),
        )
    }

    fn move_selection(&mut self, delta: isize) {
        let rows = self.rows();
        let Some(mut index) = self.selection(&rows) else {
            return;
        };
        // Program lines are read-only: the selection steps over them.
        loop {
            let next = index
                .saturating_add_signed(delta)
                .min(rows.len().saturating_sub(1));
            if next == index {
                break;
            }
            index = next;
            if rows[index].selectable() {
                break;
            }
        }
        if rows[index].selectable() {
            self.selected = Some(rows[index].identity.clone());
        }
    }

    /// The agent row a nested row belongs to.
    fn parent_identity(row: &Row) -> Option<String> {
        match &row.target {
            Target::Live(listed) if row.kind != RowKind::Agent => {
                Some(format!("live:{}", listed.agent.id))
            }
            _ => None,
        }
    }

    fn refresh(&mut self) {
        self.live = client::list_all(&self.registry);
        self.data_version += 1;
    }

    fn selected_row(&self) -> Option<Row> {
        let rows = self.rows();
        let index = self.selection(&rows)?;
        rows.get(index).cloned()
    }
}

/// The saved conversations of the project, read on a thread of its own: the
/// caller may already run an async runtime.
fn load_saved(context: &LaunchContext) -> Vec<SavedConversation> {
    let store_dir = context.project_store_dir();
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map(|runtime| {
                runtime.block_on(super::service::saved_conversations(store_dir, SAVED_LIMIT))
            })
            .unwrap_or_default()
    })
    .join()
    .unwrap_or_default()
}

/// Show the view until the user opens a session, starts one, or leaves. The
/// caller holds raw mode; the view takes the alternate screen and gives it
/// back.
///
/// # Errors
/// The console refused the alternate screen.
#[allow(
    clippy::too_many_lines,
    reason = "the view's loop: load, draw, refresh and keys in one place"
)]
pub fn run(
    context: &LaunchContext,
    environment: &LaunchEnvironment,
    notice: Option<String>,
    came_from: Option<String>,
) -> Result<Choice, String> {
    let registry = client::user_registry()?;
    // The view is drawn at once and filled when the roster and the saved
    // conversations are read: reading them first (each ~0.4 s on a large
    // store) left the session's last frame, spinner and all, on screen.
    let (loaded_sender, arrived) = std::sync::mpsc::channel();
    {
        let loaded = loaded_sender.clone();
        let registry = registry.clone();
        let context = context.clone();
        std::thread::spawn(move || {
            let live = std::thread::spawn({
                let registry = registry.clone();
                move || client::list_all(&registry)
            });
            let saved = load_saved(&context);
            let _ = loaded.send((live.join().unwrap_or_default(), Some(saved)));
        });
    }
    let mut view = View {
        live: Vec::new(),
        registry,
        saved: Vec::new(),
        query: String::new(),
        draft: String::new(),
        composer: Composer::Search,
        selected: None,
        status: notice,
        stop_armed: None,
        forest: Forest::default(),
        list_rows: std::cell::RefCell::new(Vec::new()),
        store_dir: context.project_store_dir(),
        came_from,
        loading: true,
        data_version: 0,
        rows_cache: std::cell::RefCell::new(None),
    };
    let mut stdout = io::stdout();
    // Cleared as well as entered: a terminal that keeps the alternate
    // screen's last contents showed the session under the view's rows, as
    // the view draws only the cells it fills.
    execute!(
        stdout,
        terminal::EnterAlternateScreen,
        terminal::Clear(terminal::ClearType::All),
        event::EnableMouseCapture
    )
    .map_err(|error| error.to_string())?;
    let result = (|| -> Result<Choice, String> {
        let mut screen = Terminal::new(CrosstermBackend::new(io::stdout()))
            .map_err(|error| error.to_string())?;
        let theme = Theme::detect();
        let mut refreshed = Instant::now();
        // A roster read on the screen's own thread stalled every key for it.
        let mut refreshing = true;
        // A frame is drawn when something changed - a key, the roster - and
        // once a second for the ages; drawing every quarter second redrew a
        // large forest for nothing.
        let mut dirty = true;
        let mut drawn = Instant::now();
        loop {
            if let Ok((live, saved)) = arrived.try_recv() {
                // A roster the same as the last one changes nothing on screen.
                let changed = live_changed(&view.live, &live) || saved.is_some();
                view.live = live;
                if let Some(saved) = saved {
                    view.saved = saved;
                }
                if changed {
                    view.data_version += 1;
                    dirty = true;
                }
                view.loading = false;
                refreshing = false;
                refreshed = Instant::now();
            }
            if dirty || drawn.elapsed() >= REFRESH {
                screen
                    .draw(|frame| draw(frame, &view, &theme))
                    .map_err(|error| error.to_string())?;
                dirty = false;
                drawn = Instant::now();
            }
            if !refreshing && refreshed.elapsed() >= REFRESH {
                refreshing = true;
                let registry = view.registry.clone();
                let loaded = loaded_sender.clone();
                std::thread::spawn(move || {
                    let _ = loaded.send((client::list_all_quick(&registry), None));
                });
            }
            if !event::poll(Duration::from_millis(250)).map_err(|error| error.to_string())? {
                continue;
            }
            dirty = true;
            match event::read().map_err(|error| error.to_string())? {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    if let Some(choice) = handle_key(&mut view, key, context, environment) {
                        return Ok(choice);
                    }
                }
                // prime-agent's mouse: a click selects a row, the wheel moves.
                Event::Mouse(mouse) => match mouse.kind {
                    MouseEventKind::Down(_) => {
                        let hit = view
                            .list_rows
                            .borrow()
                            .iter()
                            .find(|(y, _)| *y == mouse.row)
                            .map(|(_, identity)| identity.clone());
                        if let Some(identity) = hit {
                            view.selected = Some(identity);
                        }
                    }
                    MouseEventKind::ScrollUp => view.move_selection(-1),
                    MouseEventKind::ScrollDown => view.move_selection(1),
                    _ => {}
                },
                _ => {}
            }
        }
    })();
    let _ = execute!(
        stdout,
        event::DisableMouseCapture,
        terminal::LeaveAlternateScreen
    );
    let _ = stdout.flush();
    result
}

#[allow(clippy::too_many_lines)] // prime-agent's key table, one arm per action
fn handle_key(
    view: &mut View,
    key: KeyEvent,
    context: &LaunchContext,
    environment: &LaunchEnvironment,
) -> Option<Choice> {
    let control = key.modifiers.contains(KeyModifiers::CONTROL);
    let plain = !control && !key.modifiers.contains(KeyModifiers::ALT);
    if control && matches!(key.code, KeyCode::Char('c')) {
        return Some(Choice::Quit);
    }
    // The reply and rename composers own the prompt line until Enter or Esc.
    if view.composer != Composer::Search {
        match key.code {
            KeyCode::Esc => {
                view.composer = Composer::Search;
                view.draft.clear();
            }
            KeyCode::Enter => {
                let text = std::mem::take(&mut view.draft);
                let composer = std::mem::replace(&mut view.composer, Composer::Search);
                if !text.trim().is_empty() {
                    view.status = Some(match composer {
                        Composer::Reply(identity) => {
                            reply(view, &identity, &text, context, environment)
                        }
                        Composer::Rename(identity) => rename(view, &identity, text.trim()),
                        Composer::Search => String::new(),
                    });
                    view.refresh();
                }
            }
            KeyCode::Backspace => {
                view.draft.pop();
            }
            KeyCode::Char(character) if plain => view.draft.push(character),
            _ => {}
        }
        return None;
    }
    let empty = view.query.is_empty();
    match key.code {
        KeyCode::Esc if !empty => view.query.clear(),
        // Back needs the roster to find the session it came from.
        KeyCode::Esc if view.loading && view.came_from.is_some() => {}
        // Seen: Esc in a view opened from a session ended ha instead of going
        // back to that session.
        KeyCode::Esc => return Some(view.back()),
        KeyCode::Char('d') if control && empty => return Some(Choice::Quit),
        KeyCode::Up => view.move_selection(-1),
        KeyCode::Down => view.move_selection(1),
        KeyCode::Home => view.selected = view.rows().first().map(|row| row.identity.clone()),
        KeyCode::End => view.selected = view.rows().last().map(|row| row.identity.clone()),
        // prime-agent's `app.agents.expand` (Alt+Right): open or close the
        // selected agent's subagent line.
        KeyCode::Right if key.modifiers.contains(KeyModifiers::ALT) => {
            if let Some(row) = view.selected_row() {
                let parent = View::parent_identity(&row).unwrap_or(row.identity.clone());
                if !view.forest.expanded.remove(&parent) {
                    view.forest.expanded.insert(parent);
                }
            }
        }
        // prime-agent's `app.agents.program` (Ctrl+O): show the programs the
        // subagents were spawned with.
        KeyCode::Char('o') if control => {
            if let Some(row) = view.selected_row() {
                let parent = View::parent_identity(&row).unwrap_or(row.identity.clone());
                view.forest.expanded.insert(parent.clone());
                if !view.forest.programs.remove(&parent) {
                    view.forest.programs.insert(parent);
                }
            }
        }
        KeyCode::Enter | KeyCode::Right => {
            let row = view.selected_row()?;
            // The subagent line opens like Alt+Right; a subagent opens its agent.
            if row.kind == RowKind::SubagentSummary {
                let parent = View::parent_identity(&row).unwrap_or(row.identity.clone());
                if !view.forest.expanded.remove(&parent) {
                    view.forest.expanded.insert(parent);
                }
                return None;
            }
            match row.target {
                Target::Live(listed) => {
                    if listed.descriptor.build != super::agents::registry::build_identity() {
                        view.status = Some(
                            "the agent's worker runs another build of ha; `ha shutdown` replaces it"
                                .to_owned(),
                        );
                        return None;
                    }
                    return Some(Choice::Open(Box::new((*listed).clone())));
                }
                Target::Saved(conversation) => {
                    return Some(Choice::Resume(conversation.session_id));
                }
            }
        }
        KeyCode::Char('n') if control => return Some(Choice::New),
        KeyCode::Char(' ') if empty => {
            if let Some(row) = view.selected_row() {
                view.composer = Composer::Reply(row.identity);
                view.draft.clear();
            }
        }
        KeyCode::Char('r') if control && empty => match view.selected_row() {
            Some(Row {
                target: Target::Live(listed),
                identity,
                ..
            }) => {
                view.draft = listed.agent.name.clone().unwrap_or_default();
                view.composer = Composer::Rename(identity);
            }
            Some(_) => {
                view.status = Some(
                    "open the conversation and name it with /name; a saved one has no agent to rename"
                        .to_owned(),
                );
            }
            None => {}
        },
        KeyCode::Char('x') if control && empty => match view.selected_row() {
            Some(Row {
                target: Target::Live(listed),
                identity,
                ..
            }) => {
                let armed = view.stop_armed.take().is_some_and(|(armed, at)| {
                    armed == identity && at.elapsed() < Duration::from_secs(3)
                });
                if armed {
                    view.status = Some(stop(&listed));
                    view.refresh();
                } else {
                    view.status = Some(format!(
                        "Press ctrl+x again to stop {}",
                        listed.agent.display_name()
                    ));
                    view.stop_armed = Some((identity, Instant::now()));
                }
            }
            // prime-agent's delete of a saved session (press twice).
            Some(Row {
                target: Target::Saved(conversation),
                identity,
                title,
                ..
            }) => {
                let armed = view.stop_armed.take().is_some_and(|(armed, at)| {
                    armed == identity && at.elapsed() < Duration::from_secs(3)
                });
                if armed {
                    view.status = Some(
                        match super::service::hide_conversation(
                            &view.store_dir,
                            &conversation.task_id,
                        ) {
                            Ok(()) => {
                                view.saved
                                    .retain(|saved| saved.task_id != conversation.task_id);
                                view.data_version += 1;
                                format!("deleted {title}")
                            }
                            Err(error) => format!("could not delete {title}: {error}"),
                        },
                    );
                } else {
                    view.status = Some(format!("Press ctrl+x again to delete {title}"));
                    view.stop_armed = Some((identity, Instant::now()));
                }
            }
            None => {}
        },
        KeyCode::Backspace => {
            view.query.pop();
            view.selected = None;
        }
        KeyCode::Char(character) if plain => {
            view.query.push(character);
            view.selected = None;
        }
        _ => {}
    }
    None
}

/// Send a reply: to a running agent, or the prompt a saved conversation
/// resumes with in the background. The status line it leaves.
fn reply(
    view: &View,
    identity: &str,
    text: &str,
    context: &LaunchContext,
    environment: &LaunchEnvironment,
) -> String {
    let rows = view.rows();
    let Some(row) = rows.iter().find(|row| row.identity == identity).cloned() else {
        return "the session is gone".to_owned();
    };
    match row.target {
        Target::Live(listed) => {
            let sent = client::open(&listed).and_then(|mut connection| {
                connection.call(&protocol::Request::Send {
                    agent: listed.agent.id.clone(),
                    text: text.to_owned(),
                    from: None,
                    mode: protocol::SendMode::Auto,
                })
            });
            match sent {
                Ok(value) => format!(
                    "{} to {}",
                    value["status"].as_str().unwrap_or("delivered"),
                    listed.agent.display_name()
                ),
                Err(error) => format!("could not reach {}: {error}", listed.agent.display_name()),
            }
        }
        Target::Saved(conversation) => {
            let overrides = super::config::ConfigOverrides {
                initial_prompt: Some(text.to_owned()),
                ..super::config::ConfigOverrides::default()
            };
            let spec = super::agents::spec_for_launch(
                context,
                environment,
                None,
                Some(conversation.session_id),
                false,
                &overrides,
            );
            match client::create_in(context, spec) {
                Ok(agent) => format!(
                    "{} resumed as {} with the reply",
                    row.title,
                    agent.display_name()
                ),
                Err(error) => format!("the conversation could not resume: {error}"),
            }
        }
    }
}

fn rename(view: &View, identity: &str, name: &str) -> String {
    let Some(Row {
        target: Target::Live(listed),
        ..
    }) = view
        .rows()
        .iter()
        .find(|row| row.identity == identity)
        .cloned()
    else {
        return "the agent is gone".to_owned();
    };
    let renamed = client::open(&listed).and_then(|mut connection| {
        connection.call(&protocol::Request::Rename {
            agent: listed.agent.id.clone(),
            name: name.to_owned(),
        })
    });
    match renamed {
        Ok(_) => format!("renamed {} to {name}", listed.agent.id),
        Err(error) => error,
    }
}

fn stop(listed: &client::Listed) -> String {
    let stopped = client::open(listed).and_then(|mut connection| {
        connection.call(&protocol::Request::Stop {
            agent: listed.agent.id.clone(),
        })
    });
    match stopped {
        Ok(_) => format!("stopped {}", listed.agent.display_name()),
        Err(error) => error,
    }
}

/// Whether a roster read differs from the one on screen.
fn live_changed(old: &[client::Listed], new: &[client::Listed]) -> bool {
    old.len() != new.len()
        || old
            .iter()
            .zip(new)
            .any(|(old, new)| old.agent != new.agent || old.descriptor != new.descriptor)
}

/// A cell of `width` columns: cut with `…`, padded with spaces.
fn cell(text: &str, width: usize) -> String {
    let count = text.chars().count();
    if count > width {
        let mut cut = text
            .chars()
            .take(width.saturating_sub(1))
            .collect::<String>();
        cut.push('…');
        cut
    } else {
        format!("{text}{}", " ".repeat(width - count))
    }
}

/// One item of the list area.
enum Item<'a> {
    Heading(Section, usize),
    Spacer,
    Row(&'a Row),
}

/// The mark before a row: what the agent, subagent or saved session is doing.
fn status_mark(row: &Row, theme: &Theme) -> (&'static str, ratatui::style::Style) {
    match (&row.target, row.kind) {
        (_, RowKind::Code | RowKind::SubagentSummary) => (" ", theme.dim),
        (_, RowKind::Subagent) => {
            let status = row.title.rsplit(" · ").next().unwrap_or_default();
            match status {
                "running" => ("●", theme.tool_ok),
                "queued" => ("◌", theme.dim),
                "error" => ("✗", theme.error),
                "stopped" => ("■", theme.muted),
                _ => ("✓", theme.dim),
            }
        }
        (Target::Live(listed), _) => {
            let status = listed.agent.status.as_str();
            if status.starts_with("waiting") {
                ("◐", theme.warning)
            } else if row.section == Section::Running {
                ("●", theme.tool_ok)
            } else if status == "setup_required" {
                ("!", theme.warning)
            } else {
                ("○", theme.muted)
            }
        }
        (Target::Saved(_), _) => ("·", theme.dim),
    }
}

/// The model without the endpoint it is reached through.
fn short_model(model: &str) -> &str {
    model.split(" via ").next().unwrap_or(model)
}

#[allow(clippy::too_many_lines)] // one frame, top to bottom
fn draw(frame: &mut ratatui::Frame, view: &View, theme: &Theme) {
    let area = frame.area();
    if area.height < 8 || area.width < 30 {
        return;
    }
    let width = usize::from(area.width);
    let line = |y: u16, content: Line<'static>, frame: &mut ratatui::Frame| {
        frame.render_widget(
            Paragraph::new(content),
            Rect::new(area.x, area.y + y, area.width, 1),
        );
    };
    let rows = view.rows();
    let selected = view.selection(&rows);
    // The title bar: what the view holds.
    let count = |section: Section| {
        rows.iter()
            .filter(|row| row.section == section && row.kind == RowKind::Agent)
            .count()
    };
    let mut title = vec![
        Span::styled(" ✦ Agents ".to_owned(), theme.badge_accent),
        Span::raw("  "),
    ];
    for (index, (section, label, style)) in [
        (Section::Running, "running", theme.tool_ok),
        (Section::Idle, "idle", theme.muted),
        (Section::Inactive, "saved", theme.dim),
    ]
    .into_iter()
    .enumerate()
    {
        if index > 0 {
            title.push(Span::styled(" · ".to_owned(), theme.dim));
        }
        title.push(Span::styled(format!("{} {label}", count(section)), style));
    }
    line(0, Line::from(title), frame);
    // The prompt line: the search, or the reply / rename being written.
    let (prefix, text, placeholder) = match &view.composer {
        Composer::Search => (
            " ⌕ ",
            view.query.as_str(),
            "Search sessions, requests, models",
        ),
        Composer::Reply(identity) => (
            " reply › ",
            view.draft.as_str(),
            if identity.starts_with("live:") {
                "Write a reply to this agent"
            } else {
                "Write a prompt to resume this session"
            },
        ),
        Composer::Rename(_) => (" name › ", view.draft.as_str(), "A one-word name"),
    };
    let mut prompt = vec![Span::styled(prefix.to_owned(), theme.accent)];
    if text.is_empty() {
        prompt.push(Span::styled(placeholder.to_owned(), theme.dim));
    } else {
        prompt.push(Span::raw(text.to_owned()));
    }
    line(2, Line::from(prompt), frame);
    let cursor_x = area.x
        + u16::try_from(
            super::tui::widgets::composer::display_width(prefix)
                + super::tui::widgets::composer::display_width(text),
        )
        .unwrap_or(0);
    frame.set_cursor_position((cursor_x.min(area.x + area.width - 1), area.y + 2));
    line(
        3,
        Line::from(Span::styled("─".repeat(width), theme.rule)),
        frame,
    );
    // The list: a legend, then the sections (or the ranked hits).
    let list_top = 4_u16;
    let list_bottom = area.height - 2;
    let model_width = rows
        .iter()
        .map(|row| short_model(&row.model).chars().count())
        .max()
        .unwrap_or(0)
        .clamp(12, 32);
    let age_width = 5;
    let name_width = width.saturating_sub(model_width + age_width + 10).max(10);
    let legend = format!(
        "      {}  {}  {:>age_width$}",
        cell("SESSION", name_width),
        cell("MODEL", model_width),
        "AGE"
    );
    line(list_top, Line::from(Span::styled(legend, theme.dim)), frame);
    let query_tokens = tokens(view.query.trim());
    let find_style = super::tui::find::style(theme, false);
    let mut items: Vec<Item> = Vec::new();
    if view.query.trim().is_empty() {
        for section in [Section::Running, Section::Idle, Section::Inactive] {
            let members = rows
                .iter()
                .filter(|row| row.section == section)
                .collect::<Vec<_>>();
            if members.is_empty() {
                continue;
            }
            if !items.is_empty() {
                items.push(Item::Spacer);
            }
            let agents = members
                .iter()
                .filter(|row| row.kind == RowKind::Agent)
                .count();
            items.push(Item::Heading(section, agents));
            items.extend(members.into_iter().map(Item::Row));
        }
    } else {
        items.extend(rows.iter().map(Item::Row));
    }
    let visible = usize::from(list_bottom.saturating_sub(list_top + 1));
    if items.is_empty() {
        let empty = if view.loading {
            "Loading sessions…"
        } else if view.query.trim().is_empty() {
            "No sessions yet. Ctrl+N starts one."
        } else {
            "No sessions match your search."
        };
        line(
            list_top + 2,
            Line::from(Span::styled(format!("   {empty}"), theme.dim)),
            frame,
        );
    } else {
        view.list_rows.borrow_mut().clear();
        // Keep the selected row in view.
        let selected_identity = selected.map(|index| rows[index].identity.as_str());
        let selected_item = items
            .iter()
            .position(|item| matches!(item, Item::Row(row) if Some(row.identity.as_str()) == selected_identity))
            .unwrap_or(0);
        let start = selected_item
            .saturating_sub(visible.saturating_sub(1))
            .min(items.len().saturating_sub(visible));
        for (offset, item) in items.iter().skip(start).take(visible).enumerate() {
            let y = list_top + 1 + u16::try_from(offset).unwrap_or(0);
            let content = match item {
                Item::Spacer => Line::default(),
                Item::Heading(section, count) => {
                    let label = format!(" {} ", section.title().to_uppercase());
                    let tally = format!(" {count} ");
                    let used = label.chars().count() + tally.chars().count() + 2;
                    Line::from(vec![
                        Span::styled(label, theme.md_heading),
                        Span::styled(tally, theme.chip),
                        Span::styled(
                            format!(" {}", "─".repeat(width.saturating_sub(used))),
                            theme.rule,
                        ),
                    ])
                }
                Item::Row(row) => {
                    if row.selectable() {
                        view.list_rows
                            .borrow_mut()
                            .push((area.y + y, row.identity.clone()));
                    }
                    let is_selected = Some(row.identity.as_str()) == selected_identity;
                    let marker = if is_selected { " ❯ " } else { "   " };
                    let (mark, mark_style) = status_mark(row, theme);
                    let indent = "  ".repeat(row.depth);
                    let mut title = format!("{indent}{}", row.title);
                    // Where in the name cell the title ends and the note on
                    // why a search found the row begins.
                    let titled = title.chars().count();
                    if !row.note.is_empty() {
                        let _ = write!(title, "  ↳ {}", row.note);
                    }
                    let base = if is_selected {
                        theme.selection
                    } else if row.kind == RowKind::Code {
                        theme.muted
                    } else if row.section == Section::Inactive || row.kind != RowKind::Agent {
                        theme.dim
                    } else {
                        theme.strong
                    };
                    let rest = format!(
                        "{}  {}  {:>age_width$}",
                        cell(&title, name_width),
                        cell(short_model(&row.model), model_width),
                        row.age
                    );
                    let mut spans = vec![
                        Span::styled(
                            marker.to_owned(),
                            if is_selected {
                                theme.selection
                            } else {
                                theme.accent
                            },
                        ),
                        Span::styled(
                            format!("{mark} "),
                            if is_selected {
                                theme.selection
                            } else {
                                mark_style
                            },
                        ),
                    ];
                    let rest = cell(&rest, width.saturating_sub(5));
                    if is_selected {
                        spans.push(Span::styled(rest, base));
                    } else {
                        // The model and the age read quieter than the title.
                        let split = rest
                            .char_indices()
                            .nth(name_width)
                            .map_or(rest.len(), |(index, _)| index);
                        let (name, tail) = rest.split_at(split);
                        let noted = name
                            .char_indices()
                            .nth(titled)
                            .map_or(name.len(), |(index, _)| index);
                        let (name, note) = name.split_at(noted);
                        spans.push(Span::styled(name.to_owned(), base));
                        spans.push(Span::styled(note.to_owned(), theme.dim));
                        spans.push(Span::styled(tail.to_owned(), theme.muted));
                    }
                    // What the search found, marked as Ctrl+F marks a match.
                    if !query_tokens.is_empty() {
                        spans = highlight(spans, &query_tokens, find_style);
                    }
                    Line::from(spans)
                }
            };
            line(y, content, frame);
        }
    }
    if let Some(status) = &view.status {
        line(
            area.height - 2,
            Line::from(Span::styled(format!(" {status}"), theme.warning)),
            frame,
        );
    }
    // prime-agent's hint bar: the keys that act on the selected row.
    let mut hints = vec![("↑↓", "navigate"), ("enter", "open")];
    if view.query.is_empty()
        && let Some(row) = selected.map(|index| &rows[index])
    {
        let running = matches!(row.target, Target::Live(_));
        if running {
            hints.push(("ctrl+r", "rename"));
        }
        hints.push(("space", "reply"));
        hints.push(("ctrl+x", if running { "stop" } else { "delete" }));
        if running
            && rows
                .iter()
                .any(|other| other.kind == RowKind::SubagentSummary)
        {
            hints.push(("alt+→", "subagents"));
            hints.push(("ctrl+o", "program"));
        }
    }
    hints.push(("ctrl+n", "new"));
    hints.push((
        "esc",
        if view.came_from.is_some() {
            "back"
        } else {
            "quit"
        },
    ));
    // Too long for the width, the keys between the first two and the last
    // two go first: Esc stays in view.
    let length = |hints: &[(&str, &str)]| -> usize {
        hints
            .iter()
            .map(|(key, action)| key.chars().count() + action.chars().count() + 3)
            .sum::<usize>()
            + 1
    };
    while length(&hints) > width && hints.len() > 4 {
        hints.remove(2);
    }
    let mut bar = vec![Span::raw(" ")];
    for (index, (key, action)) in hints.into_iter().enumerate() {
        if index > 0 {
            bar.push(Span::styled("  ".to_owned(), theme.dim));
        }
        bar.push(Span::styled(key.to_owned(), theme.accent));
        bar.push(Span::styled(format!(" {action}"), theme.muted));
    }
    line(area.height - 1, Line::from(bar), frame);
}

#[cfg(test)]
mod tests {
    use super::{Section, rows};
    use crate::interactive::agents::client::Listed;
    use crate::interactive::agents::protocol::AgentInfo;
    use crate::interactive::agents::registry::Descriptor;
    use crate::interactive::service::SavedConversation;

    fn agent(id: &str, status: &str, conversation: &str) -> Listed {
        Listed {
            descriptor: Descriptor {
                schema_version: 1,
                worker_id: "w".to_owned(),
                pid: 1,
                port: 1,
                token: String::new(),
                store_dir: std::path::PathBuf::from("/s"),
                project_root: std::path::PathBuf::from("/p"),
                started_at_unix_ms: 0,
                build: String::new(),
            },
            agent: AgentInfo {
                id: id.to_owned(),
                name: None,
                status: status.to_owned(),
                busy: false,
                scheduled: false,
                attached: false,
                project_root: std::path::PathBuf::from("/p"),
                model: "openai/gpt-5".to_owned(),
                conversation: Some(conversation.to_owned()),
                last_request: Some("fix the parser\nplease".to_owned()),
                title: None,
                idle_seconds: 120,
                worker_pid: 1,
                subagents: Vec::new(),
                heartbeats: 0,
            },
        }
    }

    fn saved(task: &str, title: &str) -> SavedConversation {
        SavedConversation {
            session_id: format!("session_{task}"),
            task_id: task.to_owned(),
            title: Some(title.to_owned()),
            model: None,
            updated_at: "2026-10-06 10:00:00".to_owned(),
            turns: 2,
        }
    }

    /// Seen: the view lagged on a conversation with many subagents. Its rows
    /// share one copy of the agent (not one per row, briefs and all), are
    /// built once until the roster, the search or the forest changes, and a
    /// subagent row names its model with its thinking level.
    #[test]
    fn a_forest_is_built_once_and_its_rows_share_the_agent() {
        let mut parent = agent("a1", "running", "t1");
        parent.agent.subagents = (0..20)
            .map(|index| super::super::agents::protocol::SubagentInfo {
                queued: false,
                name: format!("explorer-{index:08x}"),
                status: "running".to_owned(),
                model: "opencode-go/deepseek-v4.1-flash".to_owned(),
                thinking: Some("high".to_owned()),
                depth: 1,
                program: "read the README ".repeat(200),
            })
            .collect();
        let mut view = super::View {
            registry: std::path::PathBuf::new(),
            live: vec![parent],
            saved: Vec::new(),
            query: String::new(),
            draft: String::new(),
            composer: super::Composer::Search,
            selected: None,
            status: None,
            stop_armed: None,
            forest: {
                let mut forest = super::Forest::default();
                forest.expanded.insert("live:a1".to_owned());
                forest
            },
            list_rows: std::cell::RefCell::new(Vec::new()),
            store_dir: std::path::PathBuf::new(),
            came_from: None,
            loading: false,
            data_version: 0,
            rows_cache: std::cell::RefCell::new(None),
        };
        let rows = view.rows();
        assert_eq!(rows.len(), 22, "the agent, its summary, twenty subagents");
        let shared = rows
            .iter()
            .map(|row| match &row.target {
                super::Target::Live(listed) => std::sync::Arc::as_ptr(listed),
                super::Target::Saved(_) => std::ptr::null(),
            })
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(shared.len(), 1, "one copy of the agent for all its rows");
        assert_eq!(rows[2].model, "opencode-go/deepseek-v4.1-flash high");
        // Two reads in the same second (the ages change by the second).
        assert!(
            (0..3).any(|_| {
                let first = view.rows();
                std::rc::Rc::ptr_eq(&first, &view.rows())
            }),
            "a second read is the same rows"
        );
        let rows = view.rows();
        view.query.push('x');
        assert!(
            !std::rc::Rc::ptr_eq(&rows, &view.rows()),
            "a search rebuilds"
        );
        view.query.clear();
        view.data_version += 1;
        assert!(
            !std::rc::Rc::ptr_eq(&rows, &view.rows()),
            "a new roster rebuilds"
        );
    }

    /// Seen: a conversation woken after a usage limit was listed as
    /// `[heartbeat: at ... run#1]`, "now" for days, its failed children unseen.
    /// The frame names what each row is doing and the keys that act on it.
    #[test]
    fn the_view_draws_marks_sections_and_keys() {
        let mut running = agent("a1", "running", "t1");
        running.agent.busy = true;
        running.agent.model = "deepseek-v4.1-flash via https://api.example/v1".to_owned();
        running.agent.subagents = vec![super::super::agents::protocol::SubagentInfo {
            queued: false,
            name: "verifier-1".to_owned(),
            status: "error".to_owned(),
            model: "m".to_owned(),
            thinking: None,
            depth: 1,
            program: String::new(),
        }];
        let view = super::View {
            registry: std::path::PathBuf::new(),
            live: vec![running, agent("a2", "waiting_input", "t2")],
            saved: vec![saved("t1", "ninja school"), saved("t3", "old work")],
            query: String::new(),
            draft: String::new(),
            composer: super::Composer::Search,
            selected: None,
            status: None,
            stop_armed: None,
            forest: {
                let mut forest = super::Forest::default();
                forest.expanded.insert("live:a1".to_owned());
                forest
            },
            list_rows: std::cell::RefCell::new(Vec::new()),
            store_dir: std::path::PathBuf::new(),
            came_from: Some("a1".to_owned()),
            loading: false,
            data_version: 0,
            rows_cache: std::cell::RefCell::new(None),
        };
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 18)).expect("terminal");
        terminal
            .draw(|frame| {
                super::draw(
                    frame,
                    &view,
                    &crate::interactive::tui::theme::Theme::plain(),
                );
            })
            .expect("drawn");
        let buffer = terminal.backend().buffer().clone();
        let text = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol().to_owned())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        println!("{text}");
        for expected in [
            "✦ Agents",
            "2 running",
            "RUNNING",
            "● ninja school",
            "◐ ",
            "✗",
            "deepseek-v4.1-flash ",
            "esc back",
        ] {
            assert!(text.contains(expected), "{expected:?} in\n{text}");
        }
        assert!(!text.contains("via https"), "{text}");
    }

    /// Seen: a session last answered "ok" was listed as "ok", and searching for
    /// its project found nothing.
    #[test]
    fn a_running_session_is_named_and_found_by_its_conversation() {
        let now = chrono::Utc::now();
        let mut answered = agent("a1", "running", "t1");
        answered.agent.last_request = Some("ok".to_owned());
        let saved = [saved("t1", "tạo dự án game ninja school online")];
        let listed = rows(std::slice::from_ref(&answered), &saved, "", now);
        assert_eq!(listed[0].title, "tạo dự án game ninja school online");
        for query in ["ninja", "du an", "Dự Án", "ok"] {
            let hits = rows(std::slice::from_ref(&answered), &saved, query, now);
            assert_eq!(hits.len(), 1, "{query}");
        }
        assert!(rows(std::slice::from_ref(&answered), &saved, "zzz", now).is_empty());
    }

    #[test]
    fn a_woken_conversation_keeps_its_title_and_says_how_long_it_waited() {
        let now = chrono::Utc::now();
        let mut woken = agent("a1", "waiting_input", "t1");
        woken.agent.last_request =
            Some("[heartbeat: at 2026-10-11T04:03:00Z run#1]\nresume".to_owned());
        woken.agent.idle_seconds = 4 * 86_400;
        woken.agent.subagents = ["running", "error", "error", "completed"]
            .iter()
            .enumerate()
            .map(
                |(index, status)| super::super::agents::protocol::SubagentInfo {
                    queued: false,
                    name: format!("coder-{index}"),
                    status: (*status).to_owned(),
                    model: "m".to_owned(),
                    thinking: None,
                    depth: 1,
                    program: String::new(),
                },
            )
            .collect();
        let listed = rows(
            std::slice::from_ref(&woken),
            &[saved("t1", "ninja school project")],
            "",
            now,
        );
        assert_eq!(listed[0].title, "ninja school project");
        assert_eq!(listed[0].age, "4d");
        assert!(
            listed[1]
                .title
                .ends_with("4 subagents (1 running, 2 failed)"),
            "{}",
            listed[1].title
        );
        // Without a saved title the scheduled prompt is not a title either.
        let listed = rows(std::slice::from_ref(&woken), &[], "", now);
        assert_eq!(listed[0].title, "a1");
    }

    /// Esc goes back to the agent the view was opened from while it runs on
    /// this build; opened by `ha agents`, or with that agent gone, it leaves.
    #[test]
    fn esc_goes_back_to_the_session_the_view_was_opened_from() {
        let mut current = agent("a1", "ready", "t1");
        current.descriptor.build = super::super::agents::registry::build_identity();
        let mut other_build = agent("a2", "ready", "t2");
        other_build.descriptor.build = "older".to_owned();
        let view = |came_from: Option<&str>| super::View {
            registry: std::path::PathBuf::new(),
            live: vec![current.clone(), other_build.clone()],
            saved: Vec::new(),
            query: String::new(),
            draft: String::new(),
            composer: super::Composer::Search,
            selected: None,
            status: None,
            stop_armed: None,
            forest: super::Forest::default(),
            list_rows: std::cell::RefCell::new(Vec::new()),
            store_dir: std::path::PathBuf::new(),
            came_from: came_from.map(str::to_owned),
            loading: false,
            data_version: 0,
            rows_cache: std::cell::RefCell::new(None),
        };
        assert!(matches!(
            view(Some("a1")).back(),
            super::Choice::Open(listed) if listed.agent.id == "a1"
        ));
        for came_from in [None, Some("a2"), Some("gone")] {
            assert!(
                matches!(view(came_from).back(), super::Choice::Quit),
                "{came_from:?}"
            );
        }
    }

    #[test]
    fn sessions_group_as_prime_groups_them() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-10-06T12:00:00Z")
            .expect("time")
            .to_utc();
        let live = [
            agent("a1", "running", "t1"),
            agent("a2", "setup_required", "t2"),
        ];
        let saved = [saved("t2", "already live"), saved("t3", "old work")];
        let listed = rows(&live, &saved, "", now);
        assert_eq!(
            listed
                .iter()
                .map(|row| (row.section, row.title.as_str(), row.age.as_str()))
                .collect::<Vec<_>>(),
            [
                (Section::Running, "fix the parser", "now"),
                (Section::Idle, "already live", "2m"),
                (Section::Inactive, "old work", "2h"),
            ]
        );
        // A query is a flat ranked list.
        let hits = rows(&live, &saved, "old work", now);
        assert_eq!(hits[0].title, "old work");
    }

    /// Seen in the agents view: a session that had just run `/compact` was
    /// listed as `/compact`. Its title (the goal the summary named) comes
    /// first, and a command is never a row's name.
    #[test]
    fn a_row_is_named_by_the_session_title_never_by_a_command() {
        let now = chrono::Utc::now();
        let mut compacted = agent("a1", "ready", "t1");
        compacted.agent.last_request = Some("/compact".to_owned());
        let untitled = rows(&[compacted.clone()], &[], "", now);
        assert_eq!(untitled[0].title, "a1", "the id, not the command");
        compacted.agent.title = Some("Build a Flappy Bird game".to_owned());
        let titled = rows(&[compacted], &[], "", now);
        assert_eq!(titled[0].title, "Build a Flappy Bird game");
    }

    #[test]
    fn subagents_nest_under_their_agent_as_prime_nests_them() {
        use super::{Forest, RowKind, rows_with};
        use crate::interactive::agents::protocol::SubagentInfo;
        let now = chrono::Utc::now();
        let mut parent = agent("a1", "running", "t1");
        parent.agent.heartbeats = 2;
        parent.agent.subagents = vec![
            SubagentInfo {
                queued: false,
                name: "scout".to_owned(),
                status: "running".to_owned(),
                model: "m".to_owned(),
                thinking: None,
                depth: 1,
                program: "look at the parser".to_owned(),
            },
            SubagentInfo {
                queued: false,
                name: "done".to_owned(),
                status: "completed".to_owned(),
                model: "m".to_owned(),
                thinking: None,
                depth: 1,
                program: String::new(),
            },
        ];
        let live = [parent];
        let closed = rows_with(&live, &[], "", now, &Forest::default());
        assert_eq!(closed.len(), 2);
        assert!(closed[0].title.ends_with("◷2"), "{}", closed[0].title);
        assert_eq!(closed[1].kind, RowKind::SubagentSummary);
        assert!(closed[1].title.contains("2 subagents (1 running)"));
        let mut forest = Forest::default();
        forest.expanded.insert("live:a1".to_owned());
        forest.programs.insert("live:a1".to_owned());
        let open = rows_with(&live, &[], "", now, &forest);
        let kinds = open.iter().map(|row| row.kind).collect::<Vec<_>>();
        assert_eq!(
            kinds,
            [
                RowKind::Agent,
                RowKind::SubagentSummary,
                RowKind::Subagent,
                RowKind::Code,
                RowKind::Subagent,
            ]
        );
        assert!(!open[3].selectable());
    }

    /// Seen: `luna` listed two sessions (their model) and none of the
    /// children at work on gpt-6-luna. A search reads each field on its own,
    /// finds children by name, role, model or brief and lists them under their
    /// agent, and says why a row matched when its title and model do not.
    #[test]
    fn a_search_finds_subagents_by_name_role_and_model_under_their_agent() {
        use super::{Forest, RowKind, rows_with};
        use crate::interactive::agents::protocol::SubagentInfo;
        let now = chrono::Utc::now();
        let child = |name: &str, model: &str, thinking: Option<&str>, program: &str| SubagentInfo {
            name: name.to_owned(),
            status: "running".to_owned(),
            model: model.to_owned(),
            thinking: thinking.map(str::to_owned),
            depth: 1,
            program: program.to_owned(),
            queued: false,
        };
        let mut game = agent("a1", "running", "t1");
        game.agent.name = Some("làm game full chức năng".to_owned());
        game.agent.model = "gpt-6-luna".to_owned();
        game.agent.subagents = vec![
            child(
                "coder-cd196007",
                "deepseek-v4.1-flash",
                Some("high"),
                "Sửa arena service",
            ),
            child(
                "coder-bbd2acd3",
                "deepseek-v4.1-flash",
                None,
                "Viết test cho shop",
            ),
            child(
                "explorer-181def05",
                "gpt-6-luna",
                Some("xhigh"),
                "Map the world service",
            ),
        ];
        let mut other = agent("b2", "ready", "t2");
        other.agent.model = "deepseek-v4-flash".to_owned();
        other.agent.name = Some("ci-runner".to_owned());
        other.agent.last_request =
            Some("Hãy chạy lại toàn bộ bộ test của arena rồi báo kết quả".to_owned());
        let live = [game, other];
        let found = |query: &str| {
            rows_with(&live, &[], query, now, &Forest::default())
                .into_iter()
                .map(|row| (row.kind, row.title, row.note))
                .collect::<Vec<_>>()
        };

        // A child by its name: under its agent, collapsed forest or not.
        let hits = found("cd196007");
        assert_eq!(hits.len(), 2, "{hits:?}");
        assert_eq!(hits[0].0, RowKind::Agent);
        assert_eq!(
            hits[1],
            (
                RowKind::Subagent,
                "coder-cd196007 · running".to_owned(),
                String::new()
            )
        );

        // By role: both coders, not the explorer.
        let coders = found("coder");
        let children: Vec<&str> = coders
            .iter()
            .filter(|hit| hit.0 == RowKind::Subagent)
            .map(|hit| hit.1.as_str())
            .collect();
        assert_eq!(
            children,
            ["coder-cd196007 · running", "coder-bbd2acd3 · running"]
        );

        // By model: the agent on it and the child on it, not the coders.
        let luna = found("luna");
        let titles: Vec<&str> = luna.iter().map(|hit| hit.1.as_str()).collect();
        assert_eq!(
            titles,
            ["làm game full chức năng", "explorer-181def05 · running"],
            "{luna:?}"
        );

        // A word only a brief or a request holds says where it was found.
        let arena = found("arena");
        assert!(
            arena.iter().any(|hit| hit.0 == RowKind::Subagent
                && hit.2.starts_with("brief: ")
                && hit.2.contains("arena")),
            "{arena:?}"
        );
        assert!(
            arena
                .iter()
                .any(|hit| hit.0 == RowKind::Agent && hit.2.starts_with("request: ")),
            "{arena:?}"
        );

        // A long request no longer matches every word spelled across it.
        assert!(
            found("lnua").is_empty()
                || found("lnua")
                    .iter()
                    .all(|hit| !hit.2.starts_with("request"))
        );
    }

    /// The matched words are marked in the row, as Ctrl+F marks them.
    #[test]
    fn a_hit_is_marked_where_it_matched() {
        let spans = vec![ratatui::text::Span::raw(
            "explorer-181def05 · running".to_owned(),
        )];
        let style = ratatui::style::Style::new().add_modifier(ratatui::style::Modifier::UNDERLINED);
        let marked = super::highlight(spans, &super::tokens("EXPLORER"), style);
        assert_eq!(marked[0].content, "explorer");
        assert_eq!(marked[0].style, style);
        assert_eq!(marked[1].content, "-181def05 · running");
        let accents = super::highlight(
            vec![ratatui::text::Span::raw("làm game".to_owned())],
            &super::tokens("lam"),
            style,
        );
        assert_eq!(accents[0].content, "làm");
    }
}
