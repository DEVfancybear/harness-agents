//! prime-agent's `/tree` selector (`pa-tui/src/tree_selector.rs`,
//! `tree_list.rs`, `tree_nodes.rs`): the conversation's branches as a tree,
//! the active path marked, branches folded with Ctrl/Alt+Left and unfolded
//! with Ctrl/Alt+Right, typed text filtering the rows, prime's filter modes
//! (Ctrl+D default, Ctrl+T no tools, Ctrl+U user only, Ctrl+L labeled only,
//! Ctrl+A all, Ctrl+O cycle), Shift+L editing a label, Shift+T showing label
//! times, and the "Summarize branch?" choice after a row is picked.
//!
//! ha's entries are its turns: each turn's input (`user`) and its answer
//! (`assistant`); a turn continues the answer it follows, and turns that
//! continue the same answer are branches.

use std::collections::{HashMap, HashSet};

use super::events::{Key, TreeModal, TreePrompt, TreeRow, TreeTurn};

/// prime-agent's filter modes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FilterMode {
    Default,
    NoTools,
    UserOnly,
    LabeledOnly,
    All,
}

impl FilterMode {
    const fn cycle_forward(self) -> Self {
        match self {
            Self::Default => Self::NoTools,
            Self::NoTools => Self::UserOnly,
            Self::UserOnly => Self::LabeledOnly,
            Self::LabeledOnly => Self::All,
            Self::All => Self::Default,
        }
    }

    const fn status_label(self) -> &'static str {
        match self {
            Self::Default => "",
            Self::NoTools => " [no-tools]",
            Self::UserOnly => " [user]",
            Self::LabeledOnly => " [labeled]",
            Self::All => " [all]",
        }
    }

    /// prime's `toggle`: the mode asked for, or back to default when it is on.
    fn toggle(self, requested: Self) -> Self {
        if self == requested {
            Self::Default
        } else {
            requested
        }
    }
}

/// One tree entry: a turn's input or its answer.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Entry {
    id: String,
    parent: Option<String>,
    user: bool,
    text: String,
    /// The answer of a turn that gave none.
    empty: bool,
    label: Option<String>,
    label_time: Option<String>,
    timestamp: String,
    session: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Gutter {
    position: usize,
    show: bool,
}

#[derive(Clone, Debug)]
struct Flat {
    entry: Entry,
    indent: usize,
    show_connector: bool,
    is_last: bool,
    gutters: Vec<Gutter>,
    virtual_root_child: bool,
}

/// What the caller runs after a key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TreeAction {
    None,
    Cancel,
    /// Continue from the entry, with the summary choice made.
    Navigate {
        target: String,
        summarize: bool,
        custom: Option<String>,
    },
    /// Save (or, with `None`, clear) the label of the turn of `session`.
    Label {
        session: String,
        label: Option<String>,
    },
}

enum Mode {
    Tree,
    Label { id: String, input: String },
    Summarize { target: String, selected: usize },
    Custom { target: String, input: String },
}

const SUMMARIZE_OPTIONS: [&str; 3] = ["No summary", "Summarize", "Summarize with custom prompt"];

/// The selector's state.
pub struct TreeView {
    flat: Vec<Flat>,
    filtered: Vec<usize>,
    selected: usize,
    leaf: Option<String>,
    max_visible: usize,
    filter: FilterMode,
    search: String,
    multiple_roots: bool,
    show_label_times: bool,
    active_path: HashSet<String>,
    visible_parent: HashMap<String, Option<String>>,
    visible_children: HashMap<Option<String>, Vec<String>>,
    last_selected: Option<String>,
    folded: HashSet<String>,
    mode: Mode,
    skip_summary_prompt: bool,
}

/// The entry id of a turn's input and of its answer.
#[must_use]
pub fn user_id(session: &str) -> String {
    format!("u:{session}")
}

#[must_use]
pub fn answer_id(session: &str) -> String {
    format!("a:{session}")
}

/// The session an entry id belongs to, and whether it is the turn's input.
#[must_use]
pub fn parse_id(id: &str) -> Option<(bool, &str)> {
    id.strip_prefix("u:")
        .map(|session| (true, session))
        .or_else(|| id.strip_prefix("a:").map(|session| (false, session)))
}

fn entries(turns: &[TreeTurn]) -> Vec<Entry> {
    let mut entries = Vec::new();
    for turn in turns {
        entries.push(Entry {
            id: user_id(&turn.session),
            parent: turn.parent.as_deref().map(answer_id),
            user: true,
            text: turn.question.clone(),
            empty: false,
            label: turn.label.clone(),
            label_time: turn.label_time.clone(),
            timestamp: turn.created_at.clone(),
            session: turn.session.clone(),
        });
        entries.push(Entry {
            id: answer_id(&turn.session),
            parent: Some(user_id(&turn.session)),
            user: false,
            text: turn.answer.clone().unwrap_or_default(),
            empty: turn
                .answer
                .as_deref()
                .is_none_or(|answer| answer.trim().is_empty()),
            label: None,
            label_time: None,
            timestamp: turn.created_at.clone(),
            session: turn.session.clone(),
        });
    }
    entries
}

struct Node {
    entry: Entry,
    children: Vec<Node>,
}

/// prime's `build_tree`: roots are entries whose parent is unknown (or
/// themselves); siblings oldest first.
fn build_tree(flat: Vec<Entry>) -> Vec<Node> {
    let by_id: HashMap<String, usize> = flat
        .iter()
        .enumerate()
        .map(|(index, entry)| (entry.id.clone(), index))
        .collect();
    let parent_of = |index: usize, flat: &[Entry]| {
        flat[index]
            .parent
            .as_ref()
            .and_then(|parent| by_id.get(parent))
            .copied()
            .filter(|parent| *parent != index)
    };
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); flat.len()];
    for index in 0..flat.len() {
        if let Some(parent) = parent_of(index, &flat) {
            children[parent].push(index);
        }
    }
    let roots: Vec<usize> = (0..flat.len())
        .filter(|index| parent_of(*index, &flat).is_none())
        .collect();
    // Iterative build: a deep conversation must not overflow the stack.
    let mut slots: Vec<Option<Entry>> = flat.into_iter().map(Some).collect();
    let mut built: Vec<Option<Node>> = (0..slots.len()).map(|_| None).collect();
    let mut work: Vec<(usize, bool)> = roots.iter().rev().map(|root| (*root, false)).collect();
    let mut seen = HashSet::new();
    while let Some((index, leaving)) = work.pop() {
        if leaving {
            let Some(entry) = slots[index].take() else {
                continue;
            };
            let mut kids: Vec<Node> = children[index]
                .iter()
                .filter_map(|child| built[*child].take())
                .collect();
            kids.sort_by(|left, right| left.entry.timestamp.cmp(&right.entry.timestamp));
            built[index] = Some(Node {
                entry,
                children: kids,
            });
        } else if seen.insert(index) {
            work.push((index, true));
            work.extend(children[index].iter().rev().map(|child| (*child, false)));
        }
    }
    roots
        .into_iter()
        .filter_map(|root| built[root].take())
        .collect()
}

impl TreeView {
    /// The selector over `turns`, the conversation's leaf the turn `leaf`
    /// ended on.
    #[must_use]
    pub fn new(
        turns: &[TreeTurn],
        leaf: Option<&str>,
        max_visible: usize,
        skip_summary_prompt: bool,
    ) -> Option<Self> {
        let tree = build_tree(entries(turns));
        if tree.is_empty() {
            return None;
        }
        let leaf = leaf.map(answer_id);
        let mut view = Self {
            flat: flatten(&tree, leaf.as_deref()),
            filtered: Vec::new(),
            selected: 0,
            leaf,
            max_visible: max_visible.max(5),
            filter: FilterMode::Default,
            search: String::new(),
            multiple_roots: tree.len() > 1,
            show_label_times: false,
            active_path: HashSet::new(),
            visible_parent: HashMap::new(),
            visible_children: HashMap::new(),
            last_selected: None,
            folded: HashSet::new(),
            mode: Mode::Tree,
            skip_summary_prompt,
        };
        view.build_active_path();
        view.apply_filter();
        view.selected = view.nearest_visible(view.leaf.clone().as_deref());
        view.last_selected = view.selected_id();
        Some(view)
    }

    fn index_of(&self, id: &str) -> Option<usize> {
        self.flat.iter().position(|node| node.entry.id == id)
    }

    fn build_active_path(&mut self) {
        self.active_path.clear();
        let mut current = self.leaf.clone();
        let mut visited = HashSet::new();
        while let Some(id) = current {
            let Some(index) = self.index_of(&id) else {
                break;
            };
            if !visited.insert(index) {
                break;
            }
            self.active_path.insert(id);
            current = self.flat[index].entry.parent.clone();
        }
    }

    fn passes(&self, index: usize) -> bool {
        let entry = &self.flat[index].entry;
        // prime hides an answer with no text, except at the current leaf.
        if !entry.user && entry.empty && self.leaf.as_deref() != Some(entry.id.as_str()) {
            return false;
        }
        let passes = match self.filter {
            FilterMode::UserOnly => entry.user,
            FilterMode::LabeledOnly => entry.label.is_some(),
            FilterMode::Default | FilterMode::NoTools | FilterMode::All => true,
        };
        if !passes {
            return false;
        }
        let search = self.search.to_lowercase();
        search.split_whitespace().all(|token| {
            entry.text.to_lowercase().contains(token)
                || entry
                    .label
                    .as_deref()
                    .is_some_and(|label| label.to_lowercase().contains(token))
        })
    }

    fn apply_filter(&mut self) {
        if !self.filtered.is_empty() {
            self.last_selected = self.selected_id().or_else(|| self.last_selected.clone());
        }
        self.filtered = (0..self.flat.len())
            .filter(|index| self.passes(*index))
            .collect();
        if !self.folded.is_empty() {
            let mut skip: HashSet<String> = HashSet::new();
            for node in &self.flat {
                if let Some(parent) = &node.entry.parent
                    && (self.folded.contains(parent) || skip.contains(parent))
                {
                    skip.insert(node.entry.id.clone());
                }
            }
            self.filtered
                .retain(|index| !skip.contains(&self.flat[*index].entry.id));
        }
        self.recalculate();
        let last = self.last_selected.clone();
        self.selected = match last {
            Some(last) => self.nearest_visible(Some(&last)),
            None => self.selected.min(self.filtered.len().saturating_sub(1)),
        };
        if !self.filtered.is_empty() {
            self.last_selected = self.selected_id().or_else(|| self.last_selected.clone());
        }
    }

    /// prime's `recalculate_visual_structure`: the visible parent of each
    /// visible row, and the indents and connectors over the visible tree.
    fn recalculate(&mut self) {
        self.visible_parent.clear();
        self.visible_children.clear();
        self.visible_children.insert(None, Vec::new());
        let visible: HashSet<String> = self
            .filtered
            .iter()
            .map(|index| self.flat[*index].entry.id.clone())
            .collect();
        for index in 0..self.flat.len() {
            let id = self.flat[index].entry.id.clone();
            if !visible.contains(&id) {
                continue;
            }
            let mut ancestor = None;
            let mut current = self.flat[index].entry.parent.clone();
            let mut seen = HashSet::new();
            while let Some(parent) = current {
                if visible.contains(&parent) {
                    ancestor = Some(parent);
                    break;
                }
                if !seen.insert(parent.clone()) {
                    break;
                }
                current = self
                    .index_of(&parent)
                    .and_then(|parent| self.flat[parent].entry.parent.clone());
            }
            self.visible_parent.insert(id.clone(), ancestor.clone());
            self.visible_children.entry(ancestor).or_default().push(id);
        }
        let roots = self
            .visible_children
            .get(&None)
            .cloned()
            .unwrap_or_default();
        self.multiple_roots = roots.len() > 1;
        let mut stack: Vec<(String, usize, bool, bool, bool, Vec<Gutter>, bool)> = Vec::new();
        for (index, root) in roots.iter().enumerate().rev() {
            stack.push((
                root.clone(),
                usize::from(self.multiple_roots),
                self.multiple_roots,
                self.multiple_roots,
                index == roots.len() - 1,
                Vec::new(),
                self.multiple_roots,
            ));
        }
        while let Some((
            id,
            indent,
            just_branched,
            show_connector,
            is_last,
            gutters,
            virtual_child,
        )) = stack.pop()
        {
            let Some(index) = self.index_of(&id) else {
                continue;
            };
            {
                let node = &mut self.flat[index];
                node.indent = indent;
                node.show_connector = show_connector;
                node.is_last = is_last;
                node.gutters.clone_from(&gutters);
                node.virtual_root_child = virtual_child;
            }
            let children = self
                .visible_children
                .get(&Some(id))
                .cloned()
                .unwrap_or_default();
            let multiple = children.len() > 1;
            let child_indent = if multiple || (just_branched && indent > 0) {
                indent + 1
            } else {
                indent
            };
            let display_indent = if self.multiple_roots {
                indent.saturating_sub(1)
            } else {
                indent
            };
            let child_gutters = if show_connector && !virtual_child {
                let mut gutters = gutters.clone();
                gutters.push(Gutter {
                    position: display_indent.saturating_sub(1),
                    show: !is_last,
                });
                gutters
            } else {
                gutters.clone()
            };
            for (child_index, child) in children.iter().enumerate().rev() {
                stack.push((
                    child.clone(),
                    child_indent,
                    multiple,
                    multiple,
                    child_index == children.len() - 1,
                    child_gutters.clone(),
                    false,
                ));
            }
        }
    }

    /// The visible row of `id`, else of its nearest visible ancestor, else
    /// the last row.
    fn nearest_visible(&self, id: Option<&str>) -> usize {
        if self.filtered.is_empty() {
            return 0;
        }
        let mut current = id.map(str::to_owned);
        let mut seen = HashSet::new();
        while let Some(id) = current {
            if let Some(position) = self
                .filtered
                .iter()
                .position(|index| self.flat[*index].entry.id == id)
            {
                return position;
            }
            if !seen.insert(id.clone()) {
                break;
            }
            current = self
                .index_of(&id)
                .and_then(|index| self.flat[index].entry.parent.clone());
        }
        self.filtered.len() - 1
    }

    fn selected_id(&self) -> Option<String> {
        self.filtered
            .get(self.selected)
            .map(|index| self.flat[*index].entry.id.clone())
    }

    fn foldable(&self, id: &str) -> bool {
        if self
            .visible_children
            .get(&Some(id.to_owned()))
            .is_none_or(Vec::is_empty)
        {
            return false;
        }
        match self.visible_parent.get(id).cloned().flatten() {
            None => true,
            Some(parent) => self
                .visible_children
                .get(&Some(parent))
                .is_some_and(|siblings| siblings.len() > 1),
        }
    }

    /// prime's `find_branch_segment_start`: the start of the next (or this)
    /// branch segment.
    fn segment_start(&self, down: bool) -> usize {
        let Some(mut current) = self.selected_id() else {
            return self.selected;
        };
        let position = |id: &str| {
            self.filtered
                .iter()
                .position(|index| self.flat[*index].entry.id == id)
        };
        if down {
            loop {
                let children = self
                    .visible_children
                    .get(&Some(current.clone()))
                    .cloned()
                    .unwrap_or_default();
                match children.len() {
                    0 => return position(&current).unwrap_or(self.selected),
                    1 => current.clone_from(&children[0]),
                    _ => return position(&children[0]).unwrap_or(self.selected),
                }
            }
        }
        loop {
            let Some(parent) = self.visible_parent.get(&current).cloned().flatten() else {
                return position(&current).unwrap_or(self.selected);
            };
            let siblings = self
                .visible_children
                .get(&Some(parent.clone()))
                .map_or(0, Vec::len);
            if siblings > 1
                && let Some(start) = position(&current)
                && start < self.selected
            {
                return start;
            }
            current = parent;
        }
    }

    fn set_filter(&mut self, filter: FilterMode) {
        self.filter = filter;
        self.folded.clear();
        self.apply_filter();
    }

    /// One key, prime's `TreeList.handle_key` and the selector's modes.
    pub fn handle_key(&mut self, key: &Key) -> TreeAction {
        match &mut self.mode {
            Mode::Label { id, input } => {
                match key {
                    Key::Enter => {
                        let id = id.clone();
                        let label = Some(input.trim().to_owned()).filter(|label| !label.is_empty());
                        self.mode = Mode::Tree;
                        if let Some(index) = self.index_of(&id) {
                            self.flat[index].entry.label.clone_from(&label);
                            self.flat[index].entry.label_time =
                                label.as_ref().map(|_| chrono::Utc::now().to_rfc3339());
                            let session = self.flat[index].entry.session.clone();
                            if self.filter == FilterMode::LabeledOnly {
                                self.apply_filter();
                            }
                            return TreeAction::Label { session, label };
                        }
                    }
                    Key::Esc => self.mode = Mode::Tree,
                    Key::Backspace => {
                        input.pop();
                    }
                    Key::Char(character) => input.push(*character),
                    Key::Paste(text) => input.extend(text.chars().filter(|c| !c.is_control())),
                    _ => {}
                }
                return TreeAction::None;
            }
            Mode::Summarize { target, selected } => {
                match key {
                    Key::Up => {
                        *selected = selected
                            .checked_sub(1)
                            .unwrap_or(SUMMARIZE_OPTIONS.len() - 1)
                    }
                    Key::Down => *selected = (*selected + 1) % SUMMARIZE_OPTIONS.len(),
                    Key::Esc => self.mode = Mode::Tree,
                    Key::Enter => {
                        let target = target.clone();
                        match *selected {
                            0 | 1 => {
                                let summarize = *selected == 1;
                                self.mode = Mode::Tree;
                                return TreeAction::Navigate {
                                    target,
                                    summarize,
                                    custom: None,
                                };
                            }
                            _ => {
                                self.mode = Mode::Custom {
                                    target,
                                    input: String::new(),
                                };
                            }
                        }
                    }
                    _ => {}
                }
                return TreeAction::None;
            }
            Mode::Custom { target, input } => {
                match key {
                    Key::Enter => {
                        let target = target.clone();
                        let custom = Some(input.trim().to_owned()).filter(|text| !text.is_empty());
                        self.mode = Mode::Tree;
                        return TreeAction::Navigate {
                            target,
                            summarize: true,
                            custom,
                        };
                    }
                    Key::Esc => {
                        let target = target.clone();
                        self.mode = Mode::Summarize {
                            target,
                            selected: 2,
                        };
                    }
                    Key::Backspace => {
                        input.pop();
                    }
                    Key::Char(character) => input.push(*character),
                    Key::Newline => input.push('\n'),
                    Key::Paste(text) => input.push_str(text),
                    _ => {}
                }
                return TreeAction::None;
            }
            Mode::Tree => {}
        }
        match key {
            Key::Up => {
                self.selected = self
                    .selected
                    .checked_sub(1)
                    .unwrap_or_else(|| self.filtered.len().saturating_sub(1));
            }
            Key::Down => self.selected = (self.selected + 1) % self.filtered.len().max(1),
            // `app.tree.foldOrUp` / `app.tree.unfoldOrDown` (Ctrl/Alt+Left/Right).
            Key::WordLeft => {
                let current = self.selected_id();
                match current.filter(|id| self.foldable(id) && !self.folded.contains(id)) {
                    Some(id) => {
                        self.folded.insert(id);
                        self.apply_filter();
                    }
                    None => self.selected = self.segment_start(false),
                }
            }
            Key::WordRight => match self.selected_id().filter(|id| self.folded.contains(id)) {
                Some(id) => {
                    self.folded.remove(&id);
                    self.apply_filter();
                }
                None => self.selected = self.segment_start(true),
            },
            Key::PageUp | Key::Left => {
                self.selected = self.selected.saturating_sub(self.max_visible)
            }
            Key::PageDown | Key::Right => {
                if !self.filtered.is_empty() {
                    self.selected = (self.selected + self.max_visible).min(self.filtered.len() - 1);
                }
            }
            Key::Enter => {
                let Some(id) = self.selected_id() else {
                    return TreeAction::None;
                };
                if self.skip_summary_prompt || self.leaf.as_deref() == Some(id.as_str()) {
                    return TreeAction::Navigate {
                        target: id,
                        summarize: false,
                        custom: None,
                    };
                }
                self.mode = Mode::Summarize {
                    target: id,
                    selected: 0,
                };
            }
            Key::Esc => {
                if self.search.is_empty() {
                    return TreeAction::Cancel;
                }
                self.search.clear();
                self.folded.clear();
                self.apply_filter();
            }
            // prime's filter keys, as ha's terminal reads them: Ctrl+D,
            // Ctrl+T, Ctrl+U, Ctrl+L, Ctrl+A and Ctrl+O.
            Key::EndOfInput => self.set_filter(FilterMode::Default),
            Key::Transpose => self.set_filter(self.filter.toggle(FilterMode::NoTools)),
            Key::EraseToLineStart => self.set_filter(self.filter.toggle(FilterMode::UserOnly)),
            Key::Redraw => self.set_filter(self.filter.toggle(FilterMode::LabeledOnly)),
            Key::LineStart => self.set_filter(self.filter.toggle(FilterMode::All)),
            Key::CycleDetail => self.set_filter(self.filter.cycle_forward()),
            Key::Backspace => {
                if self.search.pop().is_some() {
                    self.folded.clear();
                    self.apply_filter();
                }
            }
            // `app.tree.editLabel` (Shift+L) and `app.tree.toggleLabelTimestamp`
            // (Shift+T).
            Key::Char('L') => {
                if let Some(id) = self.selected_id() {
                    let target = self
                        .index_of(&id)
                        .and_then(|index| {
                            let entry = &self.flat[index].entry;
                            // A turn's label lives on its input.
                            if entry.user {
                                Some(entry.id.clone())
                            } else {
                                entry.parent.clone()
                            }
                        })
                        .unwrap_or(id);
                    let input = self
                        .index_of(&target)
                        .and_then(|index| self.flat[index].entry.label.clone())
                        .unwrap_or_default();
                    self.mode = Mode::Label { id: target, input };
                }
            }
            Key::Char('T') => self.show_label_times = !self.show_label_times,
            Key::Char(character) if !character.is_control() => {
                self.search.push(*character);
                self.folded.clear();
                self.apply_filter();
            }
            Key::Paste(text) => {
                self.search
                    .extend(text.chars().filter(|character| !character.is_control()));
                self.folded.clear();
                self.apply_filter();
            }
            _ => {}
        }
        TreeAction::None
    }

    /// The panel, as data the TUI draws.
    #[must_use]
    pub fn modal(&self) -> TreeModal {
        let mut rows = Vec::new();
        let max = self.max_visible;
        let start = self
            .selected
            .saturating_sub(max / 2)
            .min(self.filtered.len().saturating_sub(max));
        let end = (start + max).min(self.filtered.len());
        for position in start..end {
            let node = &self.flat[self.filtered[position]];
            rows.push(TreeRow {
                selected: position == self.selected,
                prefix: self.prefix(node),
                folded: self.folded.contains(&node.entry.id)
                    && !(node.show_connector && !node.virtual_root_child),
                active: self.active_path.contains(&node.entry.id),
                label: node.entry.label.clone(),
                label_time: if self.show_label_times {
                    node.entry.label_time.clone()
                } else {
                    None
                },
                user: node.entry.user,
                text: node
                    .entry
                    .text
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" "),
                empty: node.entry.empty,
            });
        }
        let footer = if self.filtered.is_empty() {
            format!("(0/0){}", self.filter.status_label())
        } else {
            format!(
                "({}/{}){}",
                self.selected + 1,
                self.filtered.len(),
                self.filter.status_label()
            )
        };
        let prompt = match &self.mode {
            Mode::Tree => None,
            Mode::Label { input, .. } => Some(TreePrompt::Label {
                input: input.clone(),
            }),
            Mode::Summarize { selected, .. } => Some(TreePrompt::Summarize {
                options: SUMMARIZE_OPTIONS
                    .iter()
                    .map(|option| (*option).to_owned())
                    .collect(),
                selected: *selected,
            }),
            Mode::Custom { input, .. } => Some(TreePrompt::Custom {
                input: input.clone(),
            }),
        };
        TreeModal {
            rows,
            footer,
            search: self.search.clone(),
            prompt,
        }
    }

    /// prime's row prefix: the gutters, the `├─`/`└─` connector and the
    /// fold markers (`⊞` folded, `⊟` foldable).
    fn prefix(&self, node: &Flat) -> String {
        let display_indent = if self.multiple_roots {
            node.indent.saturating_sub(1)
        } else {
            node.indent
        };
        let connector = node.show_connector && !node.virtual_root_child;
        let connector_position = if connector {
            display_indent.saturating_sub(1)
        } else {
            usize::MAX
        };
        let mut prefix = String::new();
        for column in 0..display_indent * 3 {
            let level = column / 3;
            let offset = column % 3;
            if let Some(gutter) = node.gutters.iter().find(|gutter| gutter.position == level) {
                prefix.push(if offset == 0 && gutter.show {
                    '│'
                } else {
                    ' '
                });
            } else if connector && level == connector_position {
                prefix.push(match offset {
                    0 if node.is_last => '└',
                    0 => '├',
                    1 if self.folded.contains(&node.entry.id) => '⊞',
                    1 if self.foldable(&node.entry.id) => '⊟',
                    1 => '─',
                    _ => ' ',
                });
            } else {
                prefix.push(' ');
            }
        }
        prefix
    }
}

/// prime's `flatten_tree`: depth-first, the branch holding the leaf first.
fn flatten(roots: &[Node], leaf: Option<&str>) -> Vec<Flat> {
    fn holds_leaf(node: &Node, leaf: Option<&str>) -> bool {
        let mut stack = vec![node];
        while let Some(current) = stack.pop() {
            if leaf == Some(current.entry.id.as_str()) {
                return true;
            }
            stack.extend(current.children.iter());
        }
        false
    }
    let mut ordered: Vec<&Node> = roots.iter().collect();
    ordered.sort_by_key(|node| !holds_leaf(node, leaf));
    let mut result = Vec::new();
    let mut stack: Vec<&Node> = ordered.into_iter().rev().collect();
    while let Some(node) = stack.pop() {
        result.push(Flat {
            entry: node.entry.clone(),
            indent: 0,
            show_connector: false,
            is_last: false,
            gutters: Vec::new(),
            virtual_root_child: false,
        });
        let mut children: Vec<&Node> = node.children.iter().collect();
        children.sort_by_key(|child| !holds_leaf(child, leaf));
        stack.extend(children.into_iter().rev());
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(session: &str, parent: Option<&str>, question: &str, at: &str) -> TreeTurn {
        TreeTurn {
            session: session.to_owned(),
            parent: parent.map(str::to_owned),
            question: question.to_owned(),
            answer: Some(format!("answer to {question}")),
            label: None,
            label_time: None,
            created_at: at.to_owned(),
        }
    }

    fn branched() -> Vec<TreeTurn> {
        vec![
            turn("s1", None, "first", "2026-01-01T00:00:01"),
            turn("s2", Some("s1"), "second", "2026-01-01T00:00:02"),
            turn("s3", Some("s1"), "other branch", "2026-01-01T00:00:03"),
        ]
    }

    #[test]
    fn a_branch_shows_connectors_and_the_active_path() {
        let view = TreeView::new(&branched(), Some("s3"), 20, false).expect("view");
        let modal = view.modal();
        assert_eq!(modal.rows.len(), 6);
        // The branch holding the leaf comes first, and the leaf is selected.
        assert!(modal.rows[2].prefix.contains('├'), "{:?}", modal.rows[2]);
        assert_eq!(modal.rows[2].text, "other branch");
        assert!(modal.rows.iter().filter(|row| row.active).count() >= 4);
        assert!(modal.rows[3].selected);
        assert_eq!(modal.footer, "(4/6)");
    }

    #[test]
    fn filters_fold_and_search_like_prime() {
        let mut view = TreeView::new(&branched(), Some("s3"), 20, false).expect("view");
        view.handle_key(&Key::EraseToLineStart);
        assert_eq!(view.modal().footer, "(2/3) [user]");
        view.handle_key(&Key::EndOfInput);
        for character in "second".chars() {
            view.handle_key(&Key::Char(character));
        }
        // The turn's input and its answer both match.
        assert_eq!(view.modal().rows.len(), 2);
        view.handle_key(&Key::Esc);
        assert_eq!(view.modal().rows.len(), 6);
        // A branch's first entry folds: the branch below it is hidden.
        view.selected = 2;
        view.handle_key(&Key::WordLeft);
        assert_eq!(view.modal().rows.len(), 5);
        assert!(view.modal().rows[2].prefix.contains('⊞'));
        view.handle_key(&Key::WordRight);
        assert_eq!(view.modal().rows.len(), 6);
    }

    #[test]
    fn picking_a_row_asks_whether_to_summarize_and_labels_save() {
        let mut view = TreeView::new(&branched(), Some("s3"), 20, false).expect("view");
        view.handle_key(&Key::Up);
        view.handle_key(&Key::Up);
        assert_eq!(view.handle_key(&Key::Enter), TreeAction::None);
        assert!(matches!(
            view.modal().prompt,
            Some(TreePrompt::Summarize { .. })
        ));
        view.handle_key(&Key::Down);
        assert_eq!(
            view.handle_key(&Key::Enter),
            TreeAction::Navigate {
                target: answer_id("s1"),
                summarize: true,
                custom: None,
            }
        );
        view.handle_key(&Key::Char('L'));
        for character in "keep".chars() {
            view.handle_key(&Key::Char(character));
        }
        assert_eq!(
            view.handle_key(&Key::Enter),
            TreeAction::Label {
                session: "s1".to_owned(),
                label: Some("keep".to_owned()),
            }
        );
    }
}
