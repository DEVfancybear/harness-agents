//! prime-agent's agents view (`pa-tui/src/agents_view*`): one screen over
//! every session - the agents running in the background workers and the
//! project's saved conversations - grouped Running / Idle / Inactive.
//!
//! The prompt line searches (a ranked flat list while a query is typed); the
//! arrows choose a row; Enter or Right opens it - attaching to a running
//! agent, resuming a saved conversation as one - and the session's own Left
//! on an empty prompt (or a bare `/resume`) comes back here. Space writes a
//! reply: sent to an agent, or the prompt a saved conversation resumes
//! with in the background. Ctrl+R renames an agent, Ctrl+X stops one (press
//! twice), Ctrl+N starts a new session, Esc leaves. The roster refreshes
//! every second.
//!
//! Not ported: prime-agent's scoped subagent tree (ha's children live inside
//! one conversation, not as sessions of their own), the cost column, mouse
//! hover, and deleting a saved session file.

use std::io::{self, Write as _};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::{execute, terminal};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
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

/// What a row stands for.
#[derive(Clone, Debug)]
pub enum Target {
    Live(Box<client::Listed>),
    Saved(SavedConversation),
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
#[must_use]
pub fn rows(
    live: &[client::Listed],
    saved: &[SavedConversation],
    query: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<Row> {
    let mut rows = Vec::new();
    for listed in live {
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
        let title = agent
            .name
            .clone()
            .or_else(|| agent.last_request.as_deref().map(first_line))
            .filter(|title| !title.is_empty())
            .unwrap_or_else(|| agent.id.clone());
        rows.push(Row {
            section: if running {
                Section::Running
            } else {
                Section::Idle
            },
            identity: format!("live:{}", agent.id),
            title,
            model: agent.model.clone(),
            age: if running {
                "now".to_owned()
            } else {
                age_label(agent.idle_seconds)
            },
            target: Target::Live(Box::new(listed.clone())),
        });
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
        });
    }
    rows.sort_by_key(|row| row.section);
    let query = query.trim();
    if query.is_empty() {
        return rows;
    }
    let mut ranked = rows
        .into_iter()
        .filter_map(|row| {
            let haystack = format!("{} {} {}", row.title, row.model, row.identity);
            super::commands::fuzzy_score(query, &haystack.to_lowercase()).map(|score| (score, row))
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|(left, _), (right, _)| left.total_cmp(right));
    ranked.into_iter().map(|(_, row)| row).collect()
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
}

impl View {
    fn rows(&self) -> Vec<Row> {
        rows(&self.live, &self.saved, &self.query, chrono::Utc::now())
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
        let Some(index) = self.selection(&rows) else {
            return;
        };
        let next = index
            .saturating_add_signed(delta)
            .min(rows.len().saturating_sub(1));
        self.selected = Some(rows[next].identity.clone());
    }

    fn refresh(&mut self) {
        self.live = client::list_all(&self.registry);
    }

    fn selected_row(&self) -> Option<Row> {
        let rows = self.rows();
        let index = self.selection(&rows)?;
        rows.into_iter().nth(index)
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
pub fn run(
    context: &LaunchContext,
    environment: &LaunchEnvironment,
    notice: Option<String>,
) -> Result<Choice, String> {
    let registry = client::user_registry()?;
    let mut view = View {
        live: client::list_all(&registry),
        registry,
        saved: load_saved(context),
        query: String::new(),
        draft: String::new(),
        composer: Composer::Search,
        selected: None,
        status: notice,
        stop_armed: None,
    };
    let mut stdout = io::stdout();
    execute!(stdout, terminal::EnterAlternateScreen).map_err(|error| error.to_string())?;
    let result = (|| -> Result<Choice, String> {
        let mut screen = Terminal::new(CrosstermBackend::new(io::stdout()))
            .map_err(|error| error.to_string())?;
        let theme = Theme::detect();
        let mut refreshed = Instant::now();
        loop {
            screen
                .draw(|frame| draw(frame, &view, &theme))
                .map_err(|error| error.to_string())?;
            if refreshed.elapsed() >= REFRESH {
                view.refresh();
                refreshed = Instant::now();
            }
            if !event::poll(Duration::from_millis(250)).map_err(|error| error.to_string())? {
                continue;
            }
            match event::read().map_err(|error| error.to_string())? {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    if let Some(choice) = handle_key(&mut view, key, context, environment) {
                        return Ok(choice);
                    }
                }
                _ => {}
            }
        }
    })();
    let _ = execute!(stdout, terminal::LeaveAlternateScreen);
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
        KeyCode::Esc => return Some(Choice::Quit),
        KeyCode::Char('d') if control && empty => return Some(Choice::Quit),
        KeyCode::Up => view.move_selection(-1),
        KeyCode::Down => view.move_selection(1),
        KeyCode::Home => view.selected = view.rows().first().map(|row| row.identity.clone()),
        KeyCode::End => view.selected = view.rows().last().map(|row| row.identity.clone()),
        KeyCode::Enter | KeyCode::Right => {
            let row = view.selected_row()?;
            match row.target {
                Target::Live(listed) => {
                    if listed.descriptor.build != super::agents::registry::build_identity() {
                        view.status = Some(
                            "the agent's worker runs another build of ha; `ha shutdown` replaces it"
                                .to_owned(),
                        );
                        return None;
                    }
                    return Some(Choice::Open(listed));
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
            Some(_) => {
                view.status = Some(
                    "a saved conversation is not running; there is nothing to stop".to_owned(),
                );
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
    let Some(row) = rows.into_iter().find(|row| row.identity == identity) else {
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
    }) = view.rows().into_iter().find(|row| row.identity == identity)
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

#[allow(clippy::too_many_lines)] // one frame, top to bottom
fn draw(frame: &mut ratatui::Frame, view: &View, theme: &Theme) {
    let area = frame.area();
    if area.height < 6 || area.width < 20 {
        return;
    }
    let width = usize::from(area.width);
    let line = |y: u16, content: Line<'static>, frame: &mut ratatui::Frame| {
        frame.render_widget(
            Paragraph::new(content),
            Rect::new(area.x, area.y + y, area.width, 1),
        );
    };
    line(0, Line::from(Span::styled(" Agents", theme.title)), frame);
    // The prompt line: the search, or the reply / rename being written.
    let rows = view.rows();
    let selected = view.selection(&rows);
    let (prefix, text, placeholder) = match &view.composer {
        Composer::Search => (" › ", view.query.as_str(), "Search sessions"),
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
    line(1, Line::from(prompt), frame);
    let cursor_x =
        area.x + u16::try_from(prefix.chars().count() + text.chars().count()).unwrap_or(0);
    frame.set_cursor_position((cursor_x.min(area.x + area.width - 1), area.y + 1));
    // The list: a legend, then the sections (or the ranked hits).
    let list_top = 3_u16;
    let list_bottom = area.height - 2;
    let model_width = rows
        .iter()
        .map(|row| row.model.chars().count())
        .max()
        .unwrap_or(0)
        .clamp(12, 32);
    let age_width = 5;
    let name_width = width.saturating_sub(model_width + age_width + 8).max(10);
    let legend = format!(
        "  {}  {}  {:>age_width$}",
        cell("Session", name_width),
        cell("Model", model_width),
        "Age"
    );
    line(
        list_top,
        Line::from(Span::styled(
            legend,
            theme.title.add_modifier(Modifier::BOLD),
        )),
        frame,
    );
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
            items.push(Item::Heading(section, members.len()));
            items.extend(members.into_iter().map(Item::Row));
        }
    } else {
        items.extend(rows.iter().map(Item::Row));
    }
    let visible = usize::from(list_bottom.saturating_sub(list_top + 1));
    if items.is_empty() {
        let empty = if view.query.trim().is_empty() {
            "No sessions yet."
        } else {
            "No sessions match your search."
        };
        line(
            list_top + 2,
            Line::from(Span::styled(format!("  {empty}"), theme.dim)),
            frame,
        );
    } else {
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
                Item::Heading(section, count) => Line::from(Span::styled(
                    format!("{} ({count})", section.title()),
                    theme.muted,
                )),
                Item::Row(row) => {
                    let is_selected = Some(row.identity.as_str()) == selected_identity;
                    let marker = if is_selected { "❯ " } else { "  " };
                    let text = format!(
                        "{marker}{}  {}  {:>age_width$}",
                        cell(&row.title, name_width),
                        cell(&row.model, model_width),
                        row.age
                    );
                    let style = if is_selected {
                        theme.selection
                    } else if row.section == Section::Inactive {
                        theme.dim
                    } else {
                        ratatui::style::Style::default()
                    };
                    Line::from(Span::styled(cell(&text, width), style))
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
    let mut hints = vec!["↑/↓ navigate".to_owned(), "enter/→ open".to_owned()];
    if view.query.is_empty()
        && let Some(row) = selected.map(|index| &rows[index])
    {
        let running = matches!(row.target, Target::Live(_));
        if running {
            hints.push("ctrl+r rename".to_owned());
        }
        hints.push("space reply".to_owned());
        if running {
            hints.push("ctrl+x stop".to_owned());
        }
    }
    hints.push("ctrl+n new".to_owned());
    hints.push("esc quit".to_owned());
    line(
        area.height - 1,
        Line::from(Span::styled(format!(" {}", hints.join("   ")), theme.muted)),
        frame,
    );
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
                idle_seconds: 120,
                worker_pid: 1,
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
                (Section::Idle, "fix the parser", "2m"),
                (Section::Inactive, "old work", "2h"),
            ]
        );
        // A query is a flat ranked list.
        let hits = rows(&live, &saved, "old", now);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "old work");
    }
}
