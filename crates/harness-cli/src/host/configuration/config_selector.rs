//! prime-agent's resource-configuration view (`prime-agent config`,
//! `pa-tui/src/config_selector.rs`): a filterable, grouped checkbox list over
//! the resolved skills, prompt templates and themes. Space (or Enter) flips a
//! resource and the flip is written to the settings at once; Esc closes.

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::{execute, terminal};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::packages::resource_config::{self, ResourceGroup, ResourceItem};
use super::packages::{PackageManager, SettingsManager};
use super::tui::theme::Theme;

/// The maximum rows the list shows at once (prime's `maxVisible`).
const MAX_VISIBLE: usize = 15;

/// One flat selector row; `Item` rows carry the index of their resource.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectorRow {
    Group(String),
    Subgroup(String),
    Item {
        key: usize,
        label: String,
        checked: bool,
        type_label: String,
        path: String,
    },
}

/// The outcome of one key press.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectorAction {
    /// Esc: close the view.
    Close,
    /// Ctrl+C: leave.
    Exit,
    /// Space/Enter on an item; the row is already flipped.
    Toggle { key: usize, enabled: bool },
}

/// The selector state: rows, the filter, and the cursor in the filtered view.
#[derive(Debug, Clone)]
pub struct ConfigSelector {
    rows: Vec<SelectorRow>,
    filtered: Vec<usize>,
    query: String,
    selected: usize,
}

impl ConfigSelector {
    #[must_use]
    pub fn new(rows: Vec<SelectorRow>) -> Self {
        let filtered = (0..rows.len()).collect();
        let mut selector = Self {
            rows,
            filtered,
            query: String::new(),
            selected: 0,
        };
        selector.select_first_item();
        selector
    }

    /// One key press (prime's `ResourceList.handleInput`).
    pub fn handle_key(&mut self, key: KeyEvent) -> Option<SelectorAction> {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('c') if control => Some(SelectorAction::Exit),
            KeyCode::Esc => Some(SelectorAction::Close),
            KeyCode::Up => {
                self.selected = self.find_next_item(self.selected, -1);
                None
            }
            KeyCode::Down => {
                self.selected = self.find_next_item(self.selected, 1);
                None
            }
            KeyCode::PageUp => {
                let target = self.selected.saturating_sub(MAX_VISIBLE);
                self.selected = self.nearest_item_forward(target);
                None
            }
            KeyCode::PageDown => {
                let target =
                    (self.selected + MAX_VISIBLE).min(self.filtered.len().saturating_sub(1));
                self.selected = self.nearest_item_backward(target);
                None
            }
            KeyCode::Char(' ') | KeyCode::Enter => self.toggle_selected(),
            KeyCode::Backspace => {
                if self.query.pop().is_some() {
                    self.apply_filter();
                }
                None
            }
            KeyCode::Char(character) if !control => {
                self.query.push(character);
                self.apply_filter();
                None
            }
            _ => None,
        }
    }

    /// Put a row back as it was (a settings write failed).
    pub fn set_checked(&mut self, key: usize, checked: bool) {
        for row in &mut self.rows {
            if let SelectorRow::Item {
                key: row_key,
                checked: row_checked,
                ..
            } = row
                && *row_key == key
            {
                *row_checked = checked;
                return;
            }
        }
    }

    fn toggle_selected(&mut self) -> Option<SelectorAction> {
        let index = self.filtered.get(self.selected).copied()?;
        if let Some(SelectorRow::Item { key, checked, .. }) = self.rows.get_mut(index) {
            *checked = !*checked;
            return Some(SelectorAction::Toggle {
                key: *key,
                enabled: *checked,
            });
        }
        None
    }

    fn is_item(&self, filtered_index: usize) -> bool {
        self.filtered
            .get(filtered_index)
            .is_some_and(|row_index| matches!(self.rows[*row_index], SelectorRow::Item { .. }))
    }

    fn nearest_item_forward(&self, from: usize) -> usize {
        (from..self.filtered.len())
            .find(|index| self.is_item(*index))
            .unwrap_or(self.selected)
    }

    fn nearest_item_backward(&self, from: usize) -> usize {
        (0..=from)
            .rev()
            .find(|index| self.is_item(*index))
            .unwrap_or(self.selected)
    }

    /// The next/previous item row, skipping headers; stays put at the ends.
    fn find_next_item(&self, from: usize, direction: isize) -> usize {
        let mut index = from.cast_signed() + direction;
        while index >= 0 && index.cast_unsigned() < self.filtered.len() {
            if self.is_item(index.cast_unsigned()) {
                return index.cast_unsigned();
            }
            index += direction;
        }
        from
    }

    fn select_first_item(&mut self) {
        self.selected = self
            .filtered
            .iter()
            .position(|row_index| matches!(self.rows[*row_index], SelectorRow::Item { .. }))
            .unwrap_or(0);
    }

    /// Items matching the query, with the group and subgroup rows holding
    /// them (prime's `filterItems`).
    fn apply_filter(&mut self) {
        if self.query.trim().is_empty() {
            self.filtered = (0..self.rows.len()).collect();
            self.select_first_item();
            return;
        }
        let query = self.query.to_lowercase();
        let mut kept = vec![false; self.rows.len()];
        let mut group_open = None;
        let mut subgroup_open = None;
        for (index, row) in self.rows.iter().enumerate() {
            match row {
                SelectorRow::Group(_) => {
                    group_open = Some(index);
                    subgroup_open = None;
                }
                SelectorRow::Subgroup(_) => subgroup_open = Some(index),
                SelectorRow::Item {
                    label,
                    type_label,
                    path,
                    ..
                } => {
                    if label.to_lowercase().contains(&query)
                        || type_label.to_lowercase().contains(&query)
                        || path.to_lowercase().contains(&query)
                    {
                        kept[index] = true;
                        for header in [group_open, subgroup_open].into_iter().flatten() {
                            kept[header] = true;
                        }
                    }
                }
            }
        }
        self.filtered = (0..self.rows.len()).filter(|index| kept[*index]).collect();
        self.select_first_item();
    }

    /// The filtered positions the list window shows.
    fn visible_window(&self) -> (usize, usize) {
        if self.filtered.is_empty() {
            return (0, 0);
        }
        let start = self
            .selected
            .saturating_sub(MAX_VISIBLE / 2)
            .min(self.filtered.len().saturating_sub(MAX_VISIBLE));
        (start, (start + MAX_VISIBLE).min(self.filtered.len()))
    }

    fn lines(&self, theme: &Theme) -> Vec<Line<'static>> {
        let mut lines = vec![
            Line::default(),
            Line::from(Span::styled("Resource Configuration", theme.accent)),
            Line::default(),
            Line::from(vec![
                Span::styled("› ", theme.accent),
                if self.query.is_empty() {
                    Span::styled("Type to filter resources", theme.dim)
                } else {
                    Span::raw(self.query.clone())
                },
            ]),
            Line::default(),
        ];
        if self.filtered.is_empty() {
            lines.push(Line::from(Span::styled("  No resources found", theme.dim)));
        }
        let (start, end) = self.visible_window();
        for (offset, row_index) in self.filtered[start..end].iter().enumerate() {
            let position = start + offset;
            match &self.rows[*row_index] {
                SelectorRow::Group(label) => {
                    lines.push(Line::from(Span::styled(format!("  {label}"), theme.accent)));
                }
                SelectorRow::Subgroup(label) => {
                    lines.push(Line::from(Span::styled(format!("    {label}"), theme.dim)));
                }
                SelectorRow::Item {
                    label,
                    checked,
                    type_label,
                    ..
                } => {
                    let selected = position == self.selected;
                    let marker = if selected { "› " } else { "  " };
                    let row_style = if selected {
                        theme.selection
                    } else {
                        Style::default()
                    };
                    lines.push(Line::from(vec![
                        Span::styled(marker, theme.accent),
                        Span::styled(
                            if *checked { "[x]" } else { "[ ]" },
                            if *checked { theme.tool_ok } else { theme.dim },
                        ),
                        Span::styled(format!(" {label}"), row_style),
                        Span::styled(format!("  {type_label}"), theme.muted),
                    ]));
                }
            }
        }
        if start > 0 || end < self.filtered.len() {
            let items = |rows: &[usize]| {
                rows.iter()
                    .filter(|row_index| matches!(self.rows[**row_index], SelectorRow::Item { .. }))
                    .count()
            };
            let current = items(&self.filtered[..=self.selected.min(self.filtered.len() - 1)]);
            lines.push(Line::from(Span::styled(
                format!("  ({current}/{})", items(&self.filtered)),
                theme.dim,
            )));
        }
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            "  Space toggle · Esc close",
            theme.dim,
        )));
        lines
    }
}

/// Flatten the resource groups into selector rows; an item's key is its index
/// in the returned item list.
#[must_use]
pub fn selector_rows(groups: &[ResourceGroup]) -> (Vec<SelectorRow>, Vec<ResourceItem>) {
    let mut rows = Vec::new();
    let mut items: Vec<ResourceItem> = Vec::new();
    for group in groups {
        rows.push(SelectorRow::Group(group.label.clone()));
        for subgroup in &group.subgroups {
            rows.push(SelectorRow::Subgroup(subgroup.label.to_owned()));
            for item in &subgroup.items {
                rows.push(SelectorRow::Item {
                    key: items.len(),
                    label: item.display_name.clone(),
                    checked: item.enabled,
                    type_label: resource_config::resource_type_label(item.resource_type).to_owned(),
                    path: item.path.display().to_string(),
                });
                items.push(item.clone());
            }
        }
    }
    (rows, items)
}

/// `ha config`: resolve the resources, then run the selector until Esc.
///
/// # Errors
/// The resolution failed or the terminal refused the view.
pub fn run(cwd: &Path, agent_dir: &Path, bundled_skills: Option<PathBuf>) -> Result<(), String> {
    let settings = SettingsManager::create(cwd, agent_dir);
    let mut manager = PackageManager::with_options(super::packages::PackageManagerOptions {
        cwd: cwd.to_path_buf(),
        agent_dir: agent_dir.to_path_buf(),
        settings,
        bundled_skills_dir: bundled_skills.map_or(
            super::packages::BundledSkillsDir::Disabled,
            super::packages::BundledSkillsDir::Directory,
        ),
        extra_builtin_skill_overrides: Vec::new(),
    });
    let resolved = manager
        .resolve()
        .map_err(|error| format!("Error: {error:#}"))?;
    let groups = resource_config::build_groups(&resolved);
    let (rows, items) = selector_rows(&groups);
    let mut selector = ConfigSelector::new(rows);
    let mut toggle_settings = SettingsManager::create(cwd, agent_dir);

    terminal::enable_raw_mode().map_err(|error| error.to_string())?;
    let mut stdout = io::stdout();
    let entered = execute!(stdout, terminal::EnterAlternateScreen);
    let result = entered.map_err(|error| error.to_string()).and_then(|()| {
        let mut screen = Terminal::new(CrosstermBackend::new(io::stdout()))
            .map_err(|error| error.to_string())?;
        let theme = Theme::detect();
        loop {
            screen
                .draw(|frame| {
                    let area: Rect = frame.area();
                    frame.render_widget(Paragraph::new(selector.lines(&theme)), area);
                })
                .map_err(|error| error.to_string())?;
            if !event::poll(Duration::from_millis(250)).map_err(|error| error.to_string())? {
                continue;
            }
            let Event::Key(key) = event::read().map_err(|error| error.to_string())? else {
                continue;
            };
            if key.kind == KeyEventKind::Release {
                continue;
            }
            match selector.handle_key(key) {
                Some(SelectorAction::Close | SelectorAction::Exit) => return Ok(()),
                Some(SelectorAction::Toggle { key, enabled }) => {
                    let Some(item) = items.get(key) else {
                        continue;
                    };
                    if resource_config::toggle_resource(
                        &mut toggle_settings,
                        cwd,
                        agent_dir,
                        item,
                        enabled,
                    )
                    .is_err()
                    {
                        selector.set_checked(key, !enabled);
                    }
                }
                None => {}
            }
        }
    });
    let _ = execute!(stdout, terminal::LeaveAlternateScreen);
    let _ = terminal::disable_raw_mode();
    let _ = stdout.flush();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows() -> Vec<SelectorRow> {
        vec![
            SelectorRow::Group("User".into()),
            SelectorRow::Subgroup("Skills".into()),
            SelectorRow::Item {
                key: 0,
                label: "alpha".into(),
                checked: true,
                type_label: "Skills".into(),
                path: "/a/alpha/SKILL.md".into(),
            },
            SelectorRow::Subgroup("Prompts".into()),
            SelectorRow::Item {
                key: 1,
                label: "review.md".into(),
                checked: false,
                type_label: "Prompts".into(),
                path: "/a/prompts/review.md".into(),
            },
        ]
    }

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn space_toggles_the_selected_item_and_arrows_skip_headers() {
        let mut selector = ConfigSelector::new(rows());
        assert_eq!(
            selector.handle_key(press(KeyCode::Char(' '))),
            Some(SelectorAction::Toggle {
                key: 0,
                enabled: false
            })
        );
        assert_eq!(selector.handle_key(press(KeyCode::Down)), None);
        assert_eq!(
            selector.handle_key(press(KeyCode::Enter)),
            Some(SelectorAction::Toggle {
                key: 1,
                enabled: true
            })
        );
        assert_eq!(
            selector.handle_key(press(KeyCode::Esc)),
            Some(SelectorAction::Close)
        );
    }

    #[test]
    fn the_filter_keeps_matching_items_and_their_headers() {
        let mut selector = ConfigSelector::new(rows());
        for character in "review".chars() {
            selector.handle_key(press(KeyCode::Char(character)));
        }
        assert_eq!(selector.filtered, vec![0, 3, 4]);
        assert_eq!(
            selector.handle_key(press(KeyCode::Enter)),
            Some(SelectorAction::Toggle {
                key: 1,
                enabled: true
            })
        );
    }
}
