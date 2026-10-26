//! A bounded Markdown wiki with serialized, recoverable native mutations.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use harness_types::ContentHash;
use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

#[path = "links.rs"]
mod links;
#[path = "manage.rs"]
mod manage;

const STATE: &str = ".ha-state.json";
const PENDING: &str = ".ha-pending.json";
/// An interrupted transaction set aside because a file it would write was
/// changed since; kept for a look, never replayed.
const ABANDONED: &str = ".ha-pending.abandoned.json";
const INDEX: &str = "index.md";
const LOG: &str = "log.md";
const META: &str = "<!-- ha-knowledge: ";
const CATEGORY_MARKER: &str = ".ha-category";
const MAX_PAGE: usize = 256 * 1024;
const MAX_PAGES: usize = 2048;
const MAX_SCAN_BYTES: usize = 16 * 1024 * 1024;
const MAX_HISTORY: usize = 4 * 1024 * 1024;
const MAX_INDEX: usize = 512 * 1024;
// JSON escaping can expand the entire bounded corpus and history by 6x.
const MAX_JOURNAL: usize = (MAX_SCAN_BYTES + MAX_HISTORY + MAX_INDEX) * 6;
const MAX_READ: usize = 32 * 1024;
const MAX_LINES: usize = 200;
static NEXT_ID: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct WritePage {
    pub path: String,
    pub title: String,
    pub summary: String,
    pub content: String,
    pub sources: Vec<String>,
    pub related: Vec<String>,
    pub expected_hash: Option<String>,
}

/// A native update preserves metadata omitted by the caller. Explicit values,
/// including empty reference arrays, replace the corresponding metadata.
pub(crate) struct WritePageUpdate {
    pub path: String,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub content: String,
    pub sources: Option<Vec<String>>,
    pub related: Option<Vec<String>>,
    pub expected_hash: Option<String>,
    /// Who writes (see `Metadata::origin`); `None` keeps the page's.
    pub origin: Option<String>,
}

#[cfg(test)]
impl From<WritePage> for WritePageUpdate {
    fn from(input: WritePage) -> Self {
        Self {
            path: input.path,
            title: Some(input.title),
            summary: Some(input.summary),
            content: input.content,
            sources: Some(input.sources),
            related: Some(input.related),
            expected_hash: input.expected_hash,
            origin: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct PageSummary {
    pub path: String,
    pub title: String,
    pub summary: String,
    pub hash: String,
    pub sources: Vec<String>,
    pub related: Vec<String>,
    /// When the page was last written (RFC 3339): the store's own record, else
    /// the file's modification time for a page written by hand.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct PageRead {
    pub path: String,
    pub title: String,
    pub summary: String,
    pub hash: String,
    pub content: String,
    pub start_line: usize,
    pub start_column: usize,
    pub next_line: usize,
    pub next_column: usize,
    pub total_lines: usize,
    pub backlinks: Vec<String>,
    pub sources: Vec<String>,
    pub truncated: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct PageDocument {
    #[serde(flatten)]
    pub page: PageSummary,
    pub content: String,
    pub backlinks: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct SearchHit {
    pub path: String,
    pub title: String,
    pub hash: String,
    pub sources: Vec<String>,
    pub snippet: String,
    pub start_line: usize,
    pub start_column: usize,
    pub end_line: usize,
    pub score: f64,
    pub matched_queries: Vec<usize>,
}

#[derive(Debug, Serialize)]
pub(crate) struct Stats {
    pub pages: usize,
    pub links: usize,
    pub enabled: bool,
    /// Files left out of the scan, each with why: a broken one no longer takes
    /// the whole store down with it.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct Graph {
    pub nodes: Vec<PageSummary>,
    pub edges: Vec<Edge>,
}

#[derive(Debug, Serialize)]
pub(crate) struct Edge {
    pub source: String,
    pub target: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct Metadata {
    #[serde(default)]
    title: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    sources: Vec<String>,
    #[serde(default)]
    related: Vec<String>,
    /// Who wrote the page when it was not the agent at the user's word:
    /// `review` for a page the automatic review proposed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    origin: Option<String>,
    /// When the store last wrote the page (RFC 3339), for its age in the
    /// catalog: a page that records a state goes stale like a memory entry.
    /// Absent on pages written before 0.6.3.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    updated_at: Option<String>,
}

#[derive(Deserialize, Serialize)]
struct State {
    enabled: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Change {
    path: String,
    before: Option<String>,
    after: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct Transaction {
    version: u8,
    id: String,
    changes: Vec<Change>,
}

#[derive(Clone)]
struct Page {
    summary: PageSummary,
    text: String,
}

pub(crate) struct Store {
    root: PathBuf,
    /// The library lives in the user's checkout: it keeps itself out of Git.
    self_ignoring: bool,
}

impl Store {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self {
            root,
            self_ignoring: false,
        }
    }

    /// A library inside a project checkout (`.harness/knowledge`). Its pages
    /// are ha's state, not the project's files: left untracked, they made the
    /// checkout dirty, and a coder could not be delegated until the user
    /// committed or ignored them.
    pub(crate) fn self_ignoring(mut self) -> Self {
        self.self_ignoring = true;
        self
    }

    pub(crate) fn enabled(&self) -> Result<bool, String> {
        self.with_lock(enabled_at)
    }

    pub(crate) fn set_enabled(&self, enabled: bool) -> Result<(), String> {
        self.with_lock(|root| {
            if enabled_at(root)? == enabled && root.join(STATE).exists() {
                return Ok(());
            }
            let text = serde_json::to_string(&State { enabled }).map_err(error)?;
            transact(root, vec![change(root, STATE, Some(text))?])
        })
    }

    pub(crate) fn list(&self) -> Result<Vec<PageSummary>, String> {
        self.with_lock(|root| {
            require_enabled(root)?;
            Ok(scan(root)?.into_iter().map(|page| page.summary).collect())
        })
    }

    pub(crate) fn stats(&self) -> Result<Stats, String> {
        self.with_lock(stats_at)
    }

    /// Full bounded Markdown for the local console, independent of model paging.
    pub(crate) fn document(&self, path: &str) -> Result<PageDocument, String> {
        let path = page_path(path)?;
        self.with_lock(|root| {
            require_enabled(root)?;
            let pages = scan(root)?;
            let page = find_page(&pages, &path)?;
            Ok(PageDocument {
                page: page.summary.clone(),
                content: page.text.clone(),
                backlinks: backlinks(&pages, &page.summary.path),
            })
        })
    }

    pub(crate) fn corpus(&self) -> Result<Vec<(PageSummary, String)>, String> {
        self.with_lock(|root| {
            require_enabled(root)?;
            Ok(scan(root)?
                .into_iter()
                .map(|page| (page.summary, page.text))
                .collect())
        })
    }

    #[cfg(test)]
    pub(crate) fn read(
        &self,
        path: &str,
        start_line: usize,
        max_lines: usize,
    ) -> Result<PageRead, String> {
        self.read_at(path, start_line, 1, max_lines, usize::MAX)
    }

    pub(crate) fn read_at(
        &self,
        path: &str,
        start_line: usize,
        start_column: usize,
        max_lines: usize,
        max_chars: usize,
    ) -> Result<PageRead, String> {
        let path = page_path(path)?;
        self.with_lock(|root| {
            require_enabled(root)?;
            let pages = scan(root)?;
            let page = find_page(&pages, &path)?;
            let all = page.text.lines().collect::<Vec<_>>();
            // The metadata line is the store's, not the page's: a read from
            // the top starts below it, and line numbers still count it, as
            // search's do.
            let start = if start_line <= 1 && all.first().is_some_and(|line| line.starts_with(META))
            {
                2
            } else {
                start_line.max(1)
            };
            let count = max_lines.clamp(1, MAX_LINES);
            let selected = all
                .iter()
                .skip(start - 1)
                .take(count)
                .copied()
                .collect::<Vec<_>>();
            let column = start_column.max(1).min(
                selected
                    .first()
                    .map_or(1, |line| line.chars().count().saturating_add(1)),
            );
            let full = selected
                .iter()
                .enumerate()
                .map(|(i, line)| {
                    if i == 0 {
                        line.chars().skip(column - 1).collect::<String>()
                    } else {
                        (*line).to_owned()
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            let content = bounded(&full, MAX_READ)
                .chars()
                .take(max_chars.max(1))
                .collect::<String>();
            let mut next_line =
                start.saturating_add(content.bytes().filter(|b| *b == b'\n').count());
            let mut next_column = content
                .rsplit('\n')
                .next()
                .unwrap_or_default()
                .chars()
                .count()
                .saturating_add(if next_line == start { column } else { 1 });
            if all
                .get(next_line.saturating_sub(1))
                .is_none_or(|line| next_column > line.chars().count())
            {
                next_line = next_line.saturating_add(1);
                next_column = 1;
            }
            let truncated = content.len() < full.len()
                || start.saturating_sub(1).saturating_add(selected.len()) < all.len();
            Ok(PageRead {
                path: page.summary.path.clone(),
                title: page.summary.title.clone(),
                summary: page.summary.summary.clone(),
                hash: page.summary.hash.clone(),
                sources: page.summary.sources.clone(),
                content,
                start_line: start,
                start_column: column,
                next_line,
                next_column,
                total_lines: all.len(),
                backlinks: backlinks(&pages, &page.summary.path),
                truncated,
            })
        })
    }

    #[cfg(test)]
    pub(crate) fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>, String> {
        self.search_queries(&[query.to_owned()], limit)
    }

    pub(crate) fn search_queries(
        &self,
        queries: &[String],
        limit: usize,
    ) -> Result<Vec<SearchHit>, String> {
        self.with_lock(|root| {
            require_enabled(root)?;
            let pages = scan(root)?;
            let documents = search_documents(&pages);
            let mut merged: BTreeMap<String, SearchHit> = BTreeMap::new();
            let mut unique = Vec::new();
            for query in queries {
                let query = normalized(query);
                if !unique.contains(&query) {
                    unique.push(query);
                }
            }
            for (query_index, query) in unique.iter().enumerate() {
                for (rank, mut hit) in rank_pages(&pages, &documents, query, 20)
                    .into_iter()
                    .enumerate()
                {
                    hit.score = 1.0 / (61.0 + f64::from(u32::try_from(rank).unwrap_or(u32::MAX)));
                    hit.matched_queries = vec![query_index];
                    if let Some(existing) = merged.get_mut(&hit.path) {
                        existing.score += hit.score;
                        existing.matched_queries.push(query_index);
                    } else {
                        merged.insert(hit.path.clone(), hit);
                    }
                }
            }
            let mut hits = merged.into_values().collect::<Vec<_>>();
            sort_hits(&mut hits);
            hits.truncate(limit.min(20));
            Ok(hits)
        })
    }

    #[cfg(test)]
    pub(crate) fn write(&self, input: WritePage) -> Result<PageSummary, String> {
        self.write_update(&input.into())
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one locked read, check and write of a page and its index and log"
    )]
    pub(crate) fn write_update(&self, input: &WritePageUpdate) -> Result<PageSummary, String> {
        let path = page_path(&input.path)?;
        self.with_lock(|root| {
            require_enabled(root)?;
            let mut pages = scan(root)?;
            let existing = pages
                .iter()
                .position(|p| normalized(&p.summary.path) == normalized(&path));
            // Resolve omissions from the exact page protected by this lock and
            // hash precondition, never from an earlier independent read. Keep
            // declared links separate from links discovered in the old body.
            let previous = existing.map(|i| &pages[i]);
            let declared = previous
                .map(|page| parse_metadata(&page.summary.path, &page.text))
                .transpose()?
                .unwrap_or_default();
            let title = input
                .title
                .as_deref()
                .or_else(|| previous.map(|page| page.summary.title.as_str()))
                .ok_or("title is required for a new knowledge page")?;
            let title = single_line(title, 256, "title")?;
            if title.is_empty() {
                return Err("knowledge page title is required".to_owned());
            }
            let summary = input
                .summary
                .as_deref()
                .or_else(|| previous.map(|page| page.summary.summary.as_str()))
                .ok_or("summary is required for a new knowledge page")?;
            let summary = single_line(summary, 1024, "summary")?;
            let source_values = input.sources.as_ref().unwrap_or(&declared.sources);
            let related_values = input.related.as_ref().unwrap_or(&declared.related);
            if source_values.len() > 32 || related_values.len() > 64 {
                return Err("too many knowledge sources or related pages".to_owned());
            }
            let mut sources = BTreeSet::new();
            for source in source_values {
                let source = single_line(source, 2048, "source")?;
                if !source.is_empty() {
                    sources.insert(source);
                }
            }
            let before = existing.map(|i| pages[i].summary.hash.clone());
            // What the page said before, for its history.
            let previous_text =
                existing.map(|i| (pages[i].summary.hash.clone(), pages[i].text.clone()));
            let actual_path =
                existing.map_or_else(|| path.clone(), |i| pages[i].summary.path.clone());
            let mut related = BTreeSet::new();
            for target in related_values {
                let target = page_path(target)?;
                if normalized(&target) == normalized(&actual_path) {
                    return Err("a knowledge page cannot relate to itself".to_owned());
                }
                related.insert(find_page(&pages, &target)?.summary.path.clone());
            }
            let mut metadata = Metadata {
                title: title.clone(),
                summary: summary.clone(),
                sources: sources.iter().cloned().collect(),
                related: related.into_iter().collect(),
                origin: input.origin.clone().or_else(|| declared.origin.clone()),
                updated_at: declared.updated_at.clone(),
            };
            if let Some(i) = existing {
                // Retrying a completed operation is a no-op, including its log
                // and its time.
                if pages[i].text == render_page(&actual_path, &metadata, &input.content)? {
                    return Ok(pages[i].summary.clone());
                }
            }
            metadata.updated_at =
                Some(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
            let text = render_page(&actual_path, &metadata, &input.content)?;
            if let Some(i) = existing {
                if input.expected_hash.as_deref() != Some(pages[i].summary.hash.as_str()) {
                    return Err(
                        "knowledge page changed; read it again and supply its current hash"
                            .to_owned(),
                    );
                }
            } else if input.expected_hash.is_some() {
                return Err("knowledge page no longer exists".to_owned());
            }
            let mut page = parse_page(&actual_path, text.clone())?;
            let mut admitted = pages
                .iter()
                .map(|p| (normalized(&p.summary.path), p.summary.path.clone()))
                .collect::<BTreeMap<_, _>>();
            admitted.insert(normalized(&actual_path), actual_path.clone());
            resolve_page_links(&mut page, &admitted);
            let output = page.summary.clone();
            if let Some(i) = existing {
                pages[i] = page;
            } else {
                if pages.len() >= MAX_PAGES {
                    return Err("knowledge page limit reached".to_owned());
                }
                pages.push(page);
            }
            pages.sort_by(|a, b| a.summary.path.cmp(&b.summary.path));
            if pages.iter().map(|page| page.text.len()).sum::<usize>() > MAX_SCAN_BYTES {
                return Err("knowledge scan size limit reached".to_owned());
            }
            // The precondition is the page we read, not a newer external edit.
            let mut changes = vec![Change {
                path: actual_path.clone(),
                before,
                after: Some(text.clone()),
            }];
            changes.push(change(root, INDEX, Some(render_index(&pages)?))?);
            add_log(
                root,
                &mut changes,
                "write",
                &actual_path,
                &output.hash,
                input.origin.as_deref(),
            )?;
            transact(root, changes)?;
            remember(
                root,
                &super::history::Change {
                    path: &actual_path,
                    operation: if previous_text.is_some() {
                        "update"
                    } else {
                        "insert"
                    },
                    cause: input.origin.as_deref().unwrap_or("write"),
                    actor: input.origin.as_deref().unwrap_or("agent"),
                    hash: &output.hash,
                    title: &output.title,
                    text: &text,
                    before: previous_text
                        .as_ref()
                        .map(|(hash, text)| (hash.as_str(), text.as_str())),
                },
            );
            Ok(output)
        })
    }

    /// Bring back a version `history` recorded, as the page's next version:
    /// the whole page as it then was. A deleted page comes back; a page that
    /// exists needs its current hash. A related page the version names must
    /// still exist, as for any write.
    pub(crate) fn revert(
        &self,
        path: &str,
        version: &str,
        expected_hash: Option<&str>,
        origin: Option<&str>,
    ) -> Result<PageSummary, String> {
        let path = page_path(path)?;
        let event = super::history::event(&self.root, version)?
            .ok_or("no such version of this page; history lists them")?;
        if normalized(&event.path) != normalized(&path) {
            return Err(format!(
                "version {version} is of {}, not {path}",
                event.path
            ));
        }
        let text = event
            .text
            .ok_or("that version kept no text to bring back")?;
        self.with_lock(|root| {
            require_enabled(root)?;
            let mut pages = scan(root)?;
            let existing = pages
                .iter()
                .position(|p| normalized(&p.summary.path) == normalized(&path));
            match existing {
                Some(i) if expected_hash != Some(pages[i].summary.hash.as_str()) => {
                    return Err(
                        "knowledge page changed; read it again and supply its current hash to revert it"
                            .to_owned(),
                    );
                }
                Some(i) if pages[i].text == text => return Ok(pages[i].summary.clone()),
                None if expected_hash.is_some() => {
                    return Err("knowledge page no longer exists; revert it without expected_hash".to_owned());
                }
                _ => {}
            }
            let actual_path =
                existing.map_or_else(|| path.clone(), |i| pages[i].summary.path.clone());
            for target in parse_metadata(&actual_path, &text)?.related {
                find_page(&pages, &target).map_err(|_| {
                    format!("{actual_path} relates to {target}, which no longer exists: bring that page back first")
                })?;
            }
            let mut page = parse_page(&actual_path, text.clone())?;
            let mut admitted = pages
                .iter()
                .map(|p| (normalized(&p.summary.path), p.summary.path.clone()))
                .collect::<BTreeMap<_, _>>();
            admitted.insert(normalized(&actual_path), actual_path.clone());
            resolve_page_links(&mut page, &admitted);
            let output = page.summary.clone();
            let previous = existing.map(|i| (pages[i].summary.hash.clone(), pages[i].text.clone()));
            if let Some(i) = existing {
                pages[i] = page;
            } else {
                if pages.len() >= MAX_PAGES {
                    return Err("knowledge page limit reached".to_owned());
                }
                pages.push(page);
            }
            pages.sort_by(|a, b| a.summary.path.cmp(&b.summary.path));
            let mut changes = vec![Change {
                path: actual_path.clone(),
                before: previous.as_ref().map(|(hash, _)| hash.clone()),
                after: Some(text.clone()),
            }];
            changes.push(change(root, INDEX, Some(render_index(&pages)?))?);
            add_log(root, &mut changes, "revert", &actual_path, &output.hash, origin)?;
            transact(root, changes)?;
            remember(
                root,
                &super::history::Change {
                    path: &actual_path,
                    operation: if previous.is_some() { "update" } else { "insert" },
                    cause: "revert",
                    actor: origin.unwrap_or("agent"),
                    hash: &output.hash,
                    title: &output.title,
                    text: &text,
                    before: previous
                        .as_ref()
                        .map(|(hash, text)| (hash.as_str(), text.as_str())),
                },
            );
            Ok(output)
        })
    }

    /// The versions of a page, of the pages under a folder (`path` ending in
    /// `/`), or of the library, newest first.
    pub(crate) fn history(
        &self,
        path: Option<&str>,
        since: Option<&str>,
        limit: usize,
    ) -> Result<Vec<super::history::Event>, String> {
        let path = match path {
            Some(folder) if folder.ends_with('/') => Some(folder.to_owned()),
            Some(page) => Some(page_path(page)?),
            None => None,
        };
        super::history::events(&self.root, path.as_deref(), since, limit)
    }

    pub(crate) fn delete(&self, path: &str, expected_hash: &str) -> Result<(), String> {
        let path = page_path(path)?;
        self.with_lock(|root| {
            require_enabled(root)?;
            let mut pages = scan(root)?;
            let page = find_page(&pages, &path)?;
            if page.summary.hash != expected_hash {
                return Err("knowledge page changed; deletion requires its current hash".to_owned());
            }
            let path = page.summary.path.clone();
            let referring = backlinks(&pages, &path);
            if !referring.is_empty() {
                return Err(format!(
                    "knowledge page has backlinks; update the pages that link to it before deleting it: {}",
                    referring.join(", ")
                ));
            }
            let removed = page.text.clone();
            let title = page.summary.title.clone();
            pages.retain(|p| p.summary.path != path);
            let mut changes = vec![Change {
                path: path.clone(),
                before: Some(expected_hash.to_owned()),
                after: None,
            }];
            changes.push(change(root, INDEX, Some(render_index(&pages)?))?);
            add_log(root, &mut changes, "delete", &path, expected_hash, None)?;
            transact(root, changes)?;
            // The page as it went, so revert can bring it back.
            remember(
                root,
                &super::history::Change {
                    path: &path,
                    operation: "delete",
                    cause: "delete",
                    actor: "agent",
                    hash: expected_hash,
                    title: &title,
                    text: &removed,
                    before: None,
                },
            );
            Ok(())
        })
    }

    pub(crate) fn graph(&self) -> Result<Graph, String> {
        self.graph_bounded(usize::MAX).map(|(graph, _)| graph)
    }

    pub(crate) fn graph_bounded(&self, edge_limit: usize) -> Result<(Graph, usize), String> {
        self.with_lock(|root| {
            require_enabled(root)?;
            let pages = scan(root)?;
            let total_edges = pages.iter().map(|page| page.summary.related.len()).sum();
            let edges = pages
                .iter()
                .flat_map(|page| {
                    page.summary.related.iter().map(|target| Edge {
                        source: page.summary.path.clone(),
                        target: target.clone(),
                    })
                })
                .take(edge_limit)
                .collect();
            Ok((
                Graph {
                    nodes: pages.into_iter().map(|p| p.summary).collect(),
                    edges,
                },
                total_edges,
            ))
        })
    }

    pub(crate) fn refresh(&self) -> Result<Stats, String> {
        self.with_lock(|root| {
            if !enabled_at(root)? {
                return Ok(counts(&[], false, Vec::new()));
            }
            let (pages, skipped) = scan_reporting(root)?;
            // A read-only refresh of an absent, empty store stays absent.
            if root.exists() || !pages.is_empty() {
                transact(
                    root,
                    vec![change(root, INDEX, Some(render_index(&pages)?))?],
                )?;
            }
            Ok(counts(&pages, true, skipped))
        })
    }

    /// The pages a turn is shown, the most relevant first: ranked by each of
    /// `queries` - the goal, what the user typed, where the task stands, what
    /// they said before - by its weight. Ranked by the message alone, a
    /// "continue" matched nothing and the page about the task in progress was
    /// left out for the first pages by path. A catalog that leaves pages out
    /// says how many.
    pub(crate) fn catalog(
        &self,
        queries: &[(&str, f64)],
        max_chars: usize,
    ) -> Result<String, String> {
        self.with_lock(|root| {
            if !enabled_at(root)? || max_chars == 0 {
                return Ok(String::new());
            }
            let pages = scan(root)?;
            if pages.is_empty() {
                return Ok(String::new());
            }
            let mut ordered = ranked(&pages, queries);
            ordered.truncate(40);
            let limit = max_chars.min(20_000);
            let mut output = String::new();
            let mut shown = 0;
            for page in ordered {
                // JSON escaping keeps titles from impersonating instructions or
                // fences. No hash: a page is read before it is changed, and the
                // read gives it; in the catalog it was 40% of every line.
                // Its age is: a page that records a state goes stale.
                let mut entry = serde_json::json!({
                    "path": page.summary.path,
                    "title": page.summary.title,
                    "summary": page.summary.summary,
                });
                if let Some(age) = page_age(&page.summary) {
                    entry["updated"] = serde_json::json!(age);
                }
                let line = serde_json::to_string(&entry).map_err(error)?;
                if output.chars().count() + line.chars().count() + 1 > limit.saturating_sub(80) {
                    break;
                }
                output.push_str(&line);
                output.push('\n');
                shown += 1;
            }
            let more = pages.len() - shown;
            if more > 0 {
                let _ = std::fmt::Write::write_fmt(
                    &mut output,
                    format_args!(
                        "({more} more page(s) not listed: find them with knowledge search or list)
"
                    ),
                );
            }
            Ok(output)
        })
    }

    /// The pages, the most relevant to `queries` first, then the rest by path:
    /// the list the automatic review sees, so the page it would duplicate is
    /// in it however large the library.
    pub(crate) fn ranked_list(
        &self,
        queries: &[(&str, f64)],
        limit: usize,
    ) -> Result<Vec<PageSummary>, String> {
        self.with_lock(|root| {
            require_enabled(root)?;
            let pages = scan(root)?;
            Ok(ranked(&pages, queries)
                .into_iter()
                .take(limit)
                .map(|page| page.summary.clone())
                .collect())
        })
    }

    /// The page this title and summary already describe, and how much of the
    /// new one's words it holds (weighted by how rare each word is among the
    /// pages), when that is most of them.
    pub(crate) fn same_subject(
        &self,
        title: &str,
        summary: &str,
    ) -> Result<Option<(PageSummary, f64)>, String> {
        self.with_lock(|root| {
            if !enabled_at(root)? {
                return Ok(None);
            }
            let pages = scan(root)?;
            let words = |text: &str| {
                crate::interactive::harness::query_terms(&fold(text))
                    .into_iter()
                    .collect::<BTreeSet<_>>()
            };
            let new = words(&format!("{title} {summary}"));
            if new.len() < 2 {
                return Ok(None);
            }
            let kept = pages
                .iter()
                .map(|page| words(&format!("{} {}", page.summary.title, page.summary.summary)))
                .collect::<Vec<_>>();
            #[allow(clippy::cast_precision_loss, reason = "a page count")]
            let total = pages.len() as f64;
            let weight = |word: &String| {
                #[allow(clippy::cast_precision_loss, reason = "a page count")]
                let frequency = kept.iter().filter(|words| words.contains(word)).count() as f64;
                ((total + 1.0) / (frequency + 1.0)).ln() + 1.0
            };
            let whole = new.iter().map(weight).sum::<f64>();
            let best = pages
                .iter()
                .zip(&kept)
                .map(|(page, words)| {
                    let shared = new.intersection(words).map(weight).sum::<f64>();
                    (page, shared / whole)
                })
                .max_by(|a, b| a.1.total_cmp(&b.1));
            Ok(best
                .filter(|(_, share)| *share >= SAME_SUBJECT_SHARE)
                .map(|(page, share)| (page.summary.clone(), share)))
        })
    }

    fn with_lock<T>(&self, action: impl FnOnce(&Path) -> Result<T, String>) -> Result<T, String> {
        let root = resolved_root(&self.root)?;
        let _guard = lock(&root)?;
        validate_existing(&root)?;
        recover(&root)?;
        let result = action(&root);
        if self.self_ignoring && root.is_dir() {
            let ignore = root.join(".gitignore");
            if !ignore.exists() {
                // `*` covers the file itself, so Git sees nothing here.
                let _ = fs::write(
                    ignore, "*
",
                );
            }
        }
        result
    }
}

/// How long ago a page was written, as `3d ago`.
pub(crate) fn page_age(page: &PageSummary) -> Option<String> {
    crate::interactive::harness::age_of(page.updated_at.as_deref()?)
        .map(|age| age.trim_start_matches("updated ").to_owned())
}

fn error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn hash(text: &str) -> String {
    ContentHash::from_bytes(text.as_bytes()).as_str().to_owned()
}

fn bounded(text: &str, bytes: usize) -> String {
    let mut end = text.len().min(bytes);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

fn single_line(value: &str, max: usize, name: &str) -> Result<String, String> {
    if value.len() > max || value.chars().any(char::is_control) {
        return Err(format!(
            "knowledge {name} is too long or contains control characters"
        ));
    }
    Ok(value.trim().to_owned())
}

fn page_path(path: &str) -> Result<String, String> {
    if path.is_empty() || path.len() > 512 || path.starts_with(['/', '\\']) || path.contains('\\') {
        return Err(
            "knowledge path must be a relative Markdown path with forward slashes".to_owned(),
        );
    }
    let parts = path.split('/').collect::<Vec<_>>();
    if parts.len() > 16 {
        return Err("knowledge path is too deep".to_owned());
    }
    for part in &parts {
        if part.is_empty()
            || part.starts_with('.')
            || part.ends_with([' ', '.'])
            || part.len() > 120
            || part
                .chars()
                .any(|c| c.is_control() || "<>:\"|?*".contains(c))
        {
            return Err("invalid knowledge path component".to_owned());
        }
        let base = part.split('.').next().unwrap_or_default().to_uppercase();
        if matches!(
            base.as_str(),
            "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
        ) || ["COM", "LPT"].iter().any(|prefix| {
            base.strip_prefix(prefix).is_some_and(|n| {
                matches!(
                    n,
                    "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                )
            })
        }) {
            return Err("reserved knowledge filename".to_owned());
        }
    }
    if ![".md", ".txt"]
        .iter()
        .any(|suffix| path.to_lowercase().ends_with(suffix))
        || path.eq_ignore_ascii_case(INDEX)
        || path.eq_ignore_ascii_case(LOG)
    {
        return Err("knowledge path must name a page, not an index, log or state file".to_owned());
    }
    Ok(path.to_owned())
}

fn resolved_root(root: &Path) -> Result<PathBuf, String> {
    let absolute = if root.is_absolute() {
        root.to_path_buf()
    } else {
        std::env::current_dir().map_err(error)?.join(root)
    };
    if absolute
        .components()
        .any(|c| matches!(c, Component::ParentDir))
    {
        return Err("knowledge root contains traversal".to_owned());
    }
    // The supplied root is authoritative. Canonicalize existing ancestor
    // aliases (for example macOS /var), but never accept a linked KB root.
    for path in std::iter::once(absolute.as_path()).chain(absolute.parent()) {
        if fs::symlink_metadata(path).is_ok_and(|metadata| is_link(&metadata)) {
            return Err(
                "knowledge root and its container cannot be a symlink or reparse point".to_owned(),
            );
        }
    }
    let mut nearest = absolute.as_path();
    let mut tail = Vec::new();
    while !nearest.exists() {
        tail.push(
            nearest
                .file_name()
                .ok_or("invalid knowledge root")?
                .to_owned(),
        );
        nearest = nearest.parent().ok_or("invalid knowledge root")?;
    }
    let mut resolved = nearest.canonicalize().map_err(error)?;
    for part in tail.into_iter().rev() {
        resolved.push(part);
    }
    Ok(resolved)
}

fn is_link(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return true;
        }
    }
    false
}

fn validate_existing(path: &Path) -> Result<(), String> {
    for (depth, ancestor) in path.ancestors().enumerate() {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) if is_link(&metadata) => {
                return Err("knowledge paths cannot contain symlinks or reparse points".to_owned());
            }
            Ok(metadata) if depth > 0 && !metadata.is_dir() => {
                return Err("knowledge destination conflicts with a parent file".to_owned());
            }
            Ok(_) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(error(e)),
        }
    }
    Ok(())
}

fn safe_file(root: &Path, path: &str) -> Result<PathBuf, String> {
    if let Some(category) = path.strip_suffix(&format!("/{CATEGORY_MARKER}")) {
        page_path(&format!("{category}/category-check.md"))?;
    } else if ![STATE, PENDING, INDEX, LOG].contains(&path) {
        page_path(path)?;
    }
    let full = root.join(path);
    validate_existing(&full)?;
    Ok(full)
}

fn read_bounded(path: &Path, max: usize) -> Result<Option<String>, String> {
    validate_existing(path)?;
    let file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(error(e)),
    };
    if !file.metadata().map_err(error)?.is_file() {
        return Err("knowledge entry is not a file".to_owned());
    }
    let mut content = String::new();
    file.take(u64::try_from(max).unwrap_or(u64::MAX) + 1)
        .read_to_string(&mut content)
        .map_err(error)?;
    if content.len() > max {
        return Err("knowledge file exceeds its size limit".to_owned());
    }
    Ok(Some(content))
}

fn enabled_at(root: &Path) -> Result<bool, String> {
    read_bounded(&safe_file(root, STATE)?, 2048)?.map_or(Ok(true), |text| {
        serde_json::from_str::<State>(&text)
            .map(|state| state.enabled)
            .map_err(error)
    })
}

fn require_enabled(root: &Path) -> Result<(), String> {
    if enabled_at(root)? {
        Ok(())
    } else {
        Err("knowledge is disabled; enable it before accessing pages".to_owned())
    }
}

fn scan(root: &Path) -> Result<Vec<Page>, String> {
    scan_reporting(root).map(|(pages, _)| pages)
}

/// The store's pages, and the files left out with why. A page whose metadata
/// does not parse, that is too large, is not UTF-8 or differs from another
/// only by letter case is skipped and named: one such file failed every list,
/// search, read and write of its scope. The store's own bounds (entries,
/// depth, pages, bytes) still fail the scan.
fn scan_reporting(root: &Path) -> Result<(Vec<Page>, Vec<String>), String> {
    let mut skipped = Vec::new();
    if !root.exists() {
        return Ok((Vec::new(), skipped));
    }
    let mut pending = vec![(root.to_path_buf(), 0_usize)];
    let mut pages = Vec::new();
    let mut entries = 0_usize;
    let mut bytes = 0_usize;
    let mut paths = BTreeSet::new();
    while let Some((directory, depth)) = pending.pop() {
        for entry in fs::read_dir(directory).map_err(error)? {
            let entry = entry.map_err(error)?;
            entries += 1;
            if entries > 8192 {
                return Err("knowledge directory entry limit exceeded".to_owned());
            }
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).map_err(error)?;
            if is_link(&metadata) {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            if metadata.is_dir() {
                if depth >= 15 {
                    return Err("knowledge directory depth limit exceeded".to_owned());
                }
                pending.push((path, depth + 1));
                continue;
            }
            if !metadata.is_file()
                || ![".md", ".txt"]
                    .iter()
                    .any(|suffix| name.to_lowercase().ends_with(suffix))
            {
                continue;
            }
            let relative = path
                .strip_prefix(root)
                .map_err(error)?
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            if page_path(&relative).is_err() {
                continue;
            }
            if pages.len() >= MAX_PAGES {
                return Err("knowledge page limit exceeded".to_owned());
            }
            if !paths.insert(normalized(&relative)) {
                skipped.push(format!(
                    "{relative}: its path differs from another page's only by letter case"
                ));
                continue;
            }
            let text = match read_bounded(&path, MAX_PAGE) {
                Ok(Some(text)) => text,
                Ok(None) => continue,
                Err(reason) => {
                    skipped.push(format!("{relative}: {reason}"));
                    continue;
                }
            };
            bytes += text.len();
            if bytes > MAX_SCAN_BYTES {
                return Err("knowledge scan size limit exceeded".to_owned());
            }
            match parse_page(&relative, text) {
                Ok(mut page) => {
                    // A page written by hand is as old as its file.
                    if page.summary.updated_at.is_none() {
                        page.summary.updated_at = metadata.modified().ok().map(|modified| {
                            chrono::DateTime::<chrono::Utc>::from(modified)
                                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
                        });
                    }
                    pages.push(page);
                }
                Err(reason) => skipped.push(format!("{relative}: {reason}")),
            }
        }
    }
    pages.sort_by(|a, b| a.summary.path.cmp(&b.summary.path));
    let admitted = pages
        .iter()
        .map(|p| (normalized(&p.summary.path), p.summary.path.clone()))
        .collect::<BTreeMap<_, _>>();
    for page in &mut pages {
        resolve_page_links(page, &admitted);
    }
    skipped.sort();
    Ok((pages, skipped))
}

fn resolve_page_links(page: &mut Page, admitted: &BTreeMap<String, String>) {
    let own = normalized(&page.summary.path);
    page.summary.related = page
        .summary
        .related
        .iter()
        .filter_map(|target| {
            let target = normalized(target);
            (target != own)
                .then(|| admitted.get(&target).cloned())
                .flatten()
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
}

fn parse_metadata(path: &str, text: &str) -> Result<Metadata, String> {
    let mut metadata = Metadata::default();
    if let Some(first) = text.lines().next().and_then(|line| line.strip_prefix(META)) {
        let value = first
            .strip_suffix(" -->")
            .ok_or("invalid knowledge metadata")?;
        metadata = serde_json::from_str(value)
            .map_err(|e| format!("invalid knowledge metadata in {path}: {e}"))?;
    }
    Ok(metadata)
}

fn parse_page(path: &str, text: String) -> Result<Page, String> {
    let mut metadata = parse_metadata(path, &text)?;
    metadata.title = text
        .lines()
        .take(20)
        .find_map(|line| line.strip_prefix("# "))
        .map_or(metadata.title, str::to_owned);
    if metadata.title.is_empty() {
        metadata.title = Path::new(path)
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
    }
    metadata.title = single_line(&metadata.title, 256, "title")?;
    if metadata.summary.trim().is_empty() {
        // A page written by hand has no summary: its first paragraph stands
        // in, so the index and the catalog say what it is about.
        metadata.summary = text
            .lines()
            .map(str::trim)
            .find(|line| {
                !line.is_empty()
                    && !line.starts_with('#')
                    && !line.starts_with(META)
                    && !line.starts_with('>')
            })
            .map(|line| line.chars().take(160).collect::<String>())
            .unwrap_or_default();
    }
    metadata.summary = single_line(&metadata.summary, 1024, "summary")?;
    if metadata.sources.len() > 32 || metadata.related.len() > 64 {
        return Err("knowledge metadata has too many references".to_owned());
    }
    for source in &metadata.sources {
        single_line(source, 2048, "source")?;
    }
    for target in &metadata.related {
        page_path(target)?;
    }
    metadata.related.extend(links::targets(path, &text));
    Ok(Page {
        summary: PageSummary {
            path: path.to_owned(),
            title: metadata.title,
            summary: metadata.summary,
            hash: hash(&text),
            sources: metadata.sources,
            related: metadata.related,
            updated_at: metadata.updated_at,
        },
        text,
    })
}

fn render_page(path: &str, metadata: &Metadata, content: &str) -> Result<String, String> {
    let json = serde_json::to_string(metadata)
        .map_err(error)?
        .replace("-->", "\\u002d\\u002d\\u003e");
    let mut text = format!("{META}{json} -->\n# {}\n\n", metadata.title);
    if !metadata.summary.is_empty() {
        let _ = writeln!(text, "{}\n", metadata.summary);
    }
    for source in &metadata.sources {
        let _ = writeln!(text, "> Source: {source}");
    }
    if !metadata.sources.is_empty() {
        text.push('\n');
    }
    text.push_str(content);
    if !text.ends_with('\n') {
        text.push('\n');
    }
    if !metadata.related.is_empty() {
        text.push_str("\n## Related\n\n");
        let depth = path.split('/').count().saturating_sub(1);
        for target in &metadata.related {
            let relative = format!("{}{target}", "../".repeat(depth));
            let _ = writeln!(
                text,
                "- [{}]({})",
                markdown_label(target),
                encode_path(&relative)
            );
        }
    }
    if text.len() > MAX_PAGE {
        return Err("knowledge page exceeds 256 KiB".to_owned());
    }
    Ok(text)
}

fn markdown_label(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('[', "\\[")
        .replace(']', "\\]")
}

fn encode_path(value: &str) -> String {
    value
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || b"/-._~".contains(&byte) {
                char::from(byte).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}

fn render_index(pages: &[Page]) -> Result<String, String> {
    let mut text = String::from("# Knowledge Index\n\n");
    for page in pages {
        let _ = write!(
            text,
            "- [{}]({})",
            markdown_label(&page.summary.title),
            encode_path(&page.summary.path)
        );
        if !page.summary.summary.is_empty() {
            let _ = write!(text, " — {}", page.summary.summary);
        }
        text.push('\n');
    }
    if text.len() > MAX_INDEX {
        return Err("knowledge index size limit exceeded".to_owned());
    }
    Ok(text)
}

fn find_page<'a>(pages: &'a [Page], path: &str) -> Result<&'a Page, String> {
    pages
        .iter()
        .find(|p| normalized(&p.summary.path) == normalized(path))
        .ok_or_else(|| format!("knowledge page not found: {path}"))
}

/// How much of a proposed page's title and summary an existing page must
/// hold to be the same subject.
const SAME_SUBJECT_SHARE: f64 = 0.7;

/// `pages` ranked by each of `queries` by its weight (reciprocal rank
/// fusion), then the rest by path.
fn ranked<'a>(pages: &'a [Page], queries: &[(&str, f64)]) -> Vec<&'a Page> {
    let documents = search_documents(pages);
    let mut scores: BTreeMap<&str, f64> = BTreeMap::new();
    for (query, weight) in queries {
        for (rank, hit) in rank_pages(pages, &documents, &normalized(query), 20)
            .into_iter()
            .enumerate()
        {
            let Some(page) = pages.iter().find(|page| page.summary.path == hit.path) else {
                continue;
            };
            *scores.entry(page.summary.path.as_str()).or_default() +=
                weight / (61.0 + f64::from(u32::try_from(rank).unwrap_or(u32::MAX)));
        }
    }
    let mut fused = scores.into_iter().collect::<Vec<_>>();
    fused.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    let mut ordered = fused
        .iter()
        .filter_map(|(path, _)| pages.iter().find(|p| p.summary.path == *path))
        .collect::<Vec<_>>();
    for page in pages {
        if !ordered.iter().any(|p| p.summary.path == page.summary.path) {
            ordered.push(page);
        }
    }
    ordered
}

fn backlinks(pages: &[Page], path: &str) -> Vec<String> {
    pages
        .iter()
        .filter(|p| p.summary.related.iter().any(|target| target == path))
        .map(|p| p.summary.path.clone())
        .collect()
}

fn counts(pages: &[Page], enabled: bool, skipped: Vec<String>) -> Stats {
    Stats {
        pages: pages.len(),
        links: pages.iter().map(|p| p.summary.related.len()).sum(),
        enabled,
        skipped,
    }
}

fn stats_at(root: &Path) -> Result<Stats, String> {
    if enabled_at(root)? {
        let (pages, skipped) = scan_reporting(root)?;
        Ok(counts(&pages, true, skipped))
    } else {
        Ok(counts(&[], false, Vec::new()))
    }
}

fn normalized(text: &str) -> String {
    text.nfc().collect::<String>().to_lowercase()
}

/// `text` without the accents of Latin letters (`ộ` as `o`, `đ` as `d`):
/// marks on other scripts' letters are part of their words and stay.
pub(crate) fn fold(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut latin = false;
    for character in text.replace(['đ', 'Đ'], "d").nfd() {
        if unicode_normalization::char::is_combining_mark(character) {
            if latin {
                continue;
            }
        } else {
            latin = character.is_ascii_alphabetic();
        }
        out.push(character);
    }
    out.nfc().collect()
}

fn search_documents(pages: &[Page]) -> Vec<String> {
    pages
        .iter()
        .map(|page| {
            normalized(
                &page
                    .text
                    .lines()
                    .filter(|line| !line.starts_with(META))
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
        })
        .collect()
}

fn occurrences(text: &str, term: &str) -> usize {
    let joined_script = term.chars().any(|c| matches!(u32::from(c), 0x3040..=0x30ff | 0x3400..=0x9fff | 0xac00..=0xd7af | 0xf900..=0xfaff | 0x20000..=0x323af));
    let word = |c: char| c.is_alphanumeric() || unicode_normalization::char::is_combining_mark(c);
    text.match_indices(term)
        .filter(|(start, _)| {
            joined_script
                || (text[..*start].chars().next_back().is_none_or(|c| !word(c))
                    && text[start + term.len()..]
                        .chars()
                        .next()
                        .is_none_or(|c| !word(c)))
        })
        .take(64)
        .count()
}

#[allow(
    clippy::too_many_lines,
    reason = "Keep ranking and exact source-position selection together"
)]
fn rank_pages(pages: &[Page], documents: &[String], query: &str, limit: usize) -> Vec<SearchHit> {
    let terms = super::super::harness::query_terms(&bounded(query, 2048))
        .into_iter()
        .take(32)
        .collect::<Vec<_>>();
    if terms.is_empty() || limit == 0 {
        return Vec::new();
    }
    // A query typed without accents (`bo nho dem`) reads the pages without
    // theirs, so it finds `bộ nhớ đệm`; it found nothing. One with accents
    // still matches them exactly, so words told apart by a tone stay apart.
    let plain = fold(&normalized(query)) == normalized(query);
    let folded_documents;
    let documents: &[String] = if plain {
        folded_documents = documents
            .iter()
            .map(|document| fold(document))
            .collect::<Vec<_>>();
        &folded_documents
    } else {
        documents
    };
    let prepare = |text: &str| {
        if plain {
            fold(&normalized(text))
        } else {
            normalized(text)
        }
    };
    let number = |n: usize| f64::from(u32::try_from(n).unwrap_or(u32::MAX));
    let lengths = documents
        .iter()
        .map(|doc| number(doc.chars().count()))
        .collect::<Vec<_>>();
    let average_length = (lengths.iter().sum::<f64>() / number(documents.len()).max(1.0)).max(1.0);
    let frequencies = terms
        .iter()
        .map(|term| {
            documents
                .iter()
                .filter(|doc| occurrences(doc, term) > 0)
                .count()
        })
        .collect::<Vec<_>>();
    let mut hits = Vec::new();
    for (index, (page, document)) in pages.iter().zip(documents).enumerate() {
        let title = prepare(&page.summary.title);
        let summary = prepare(&page.summary.summary);
        let mut score = 0.0;
        for (term, frequency) in terms.iter().zip(&frequencies) {
            let count = number(occurrences(document, term));
            if count > 0.0 {
                let weight = (1.0
                    + (number(pages.len()) - number(*frequency) + 0.5)
                        / (number(*frequency) + 0.5))
                    .ln();
                let bm25 =
                    count * 2.2 / (count + 1.2 * (0.25 + 0.75 * lengths[index] / average_length));
                score += weight
                    * bm25
                    * (1.0
                        + if occurrences(&title, term) > 0 {
                            3.0
                        } else {
                            0.0
                        }
                        + if occurrences(&summary, term) > 0 {
                            1.0
                        } else {
                            0.0
                        });
            }
        }
        if score <= 0.0 {
            continue;
        }
        if occurrences(document, normalized(query).trim_matches(['\'', '"'])) > 0 {
            score *= 2.0;
        }
        let lines = page.text.lines().collect::<Vec<_>>();
        let line_scores = lines
            .iter()
            .map(|line| {
                if line.starts_with(META) {
                    return 0.0;
                }
                let line = prepare(line);
                terms
                    .iter()
                    .zip(&frequencies)
                    .filter(|(term, _)| occurrences(&line, term) > 0)
                    .map(|(_, frequency)| {
                        (1.0 + (number(pages.len()) + 0.5) / (number(*frequency) + 0.5)).ln()
                    })
                    .sum::<f64>()
            })
            .collect::<Vec<_>>();
        let first = 1_usize.min(lines.len().saturating_sub(1));
        let mut start = first;
        let mut best = 0.0;
        let mut best_context = 0.0;
        for i in first..lines.len() {
            let window = &line_scores[i..(i + 4).min(lines.len())];
            let weight = window.iter().sum::<f64>();
            // On equal matches, leave room after a matching heading for its
            // explanation, rather than ending the excerpt at that heading.
            let context = window
                .iter()
                .zip([0.5, 1.0, 0.75, 0.5])
                .map(|(score, weight)| score * weight)
                .sum::<f64>();
            if weight > best || ((weight - best).abs() <= f64::EPSILON && context > best_context) {
                best = weight;
                best_context = context;
                start = i;
            }
        }
        let weights = frequencies
            .iter()
            .map(|frequency| (1.0 + (number(pages.len()) + 0.5) / (number(*frequency) + 0.5)).ln())
            .collect::<Vec<_>>();
        let (start, start_column, snippet) = excerpt(&lines, start, &terms, &weights);
        let shown = snippet.lines().count().max(1);
        hits.push(SearchHit {
            path: page.summary.path.clone(),
            title: page.summary.title.clone(),
            hash: page.summary.hash.clone(),
            sources: page.summary.sources.clone(),
            snippet,
            start_line: start + 1,
            start_column,
            end_line: start + shown,
            score,
            matched_queries: vec![0],
        });
    }
    sort_hits(&mut hits);
    hits.truncate(limit.min(20));
    hits
}

fn excerpt(
    lines: &[&str],
    start: usize,
    terms: &[String],
    weights: &[f64],
) -> (usize, usize, String) {
    let window = lines
        .iter()
        .skip(start)
        .take(4)
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    if window.len() <= 1200 {
        return (start, 1, window);
    }
    let mut best = (start, 1, bounded(&window, 1200));
    let mut best_score = 0.0;
    for (i, line) in lines.iter().enumerate().skip(start).take(4) {
        let mut cursor = 0;
        while cursor < line.len() {
            let fragment = bounded(&line[cursor..], 1200);
            let text = normalized(&fragment);
            let score = terms
                .iter()
                .zip(weights)
                .filter(|(term, _)| occurrences(&text, term) > 0)
                .map(|(_, weight)| weight)
                .sum::<f64>();
            if score > best_score {
                best = (i, line[..cursor].chars().count() + 1, fragment);
                best_score = score;
            }
            let mut next = cursor.saturating_add(600).min(line.len());
            while !line.is_char_boundary(next) {
                next += 1;
            }
            cursor = next;
        }
    }
    best
}

fn sort_hits(hits: &mut [SearchHit]) {
    hits.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.path.cmp(&b.path))
    });
}

fn lock(root: &Path) -> Result<File, String> {
    let identity = root.to_string_lossy().into_owned();
    #[cfg(windows)]
    let identity = identity.to_lowercase();
    let directory = std::env::temp_dir()
        .canonicalize()
        .map_err(error)?
        .join("harness-knowledge-locks");
    fs::create_dir_all(&directory).map_err(error)?;
    validate_existing(&directory)?;
    let path = directory.join(format!("{}.lock", hash(&identity).replace(':', "-")));
    validate_existing(&path)?;
    let file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(error)?;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(fs::TryLockError::WouldBlock) => {
                return Err("waiting for another knowledge operation timed out".to_owned());
            }
            Err(fs::TryLockError::Error(e)) => return Err(error(e)),
        }
    }
}

fn unique_id() -> String {
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!(
        "{time}-{}-{}",
        std::process::id(),
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    )
}

fn atomic_write(root: &Path, path: &str, content: &str) -> Result<(), String> {
    let destination = safe_file(root, path)?;
    let parent = destination
        .parent()
        .ok_or("knowledge destination has no parent")?;
    fs::create_dir_all(parent).map_err(error)?;
    validate_existing(parent)?;
    let staging = parent.join(format!(".ha-stage-{}", unique_id()));
    let result = (|| {
        let mut file = File::options()
            .write(true)
            .create_new(true)
            .open(&staging)
            .map_err(error)?;
        file.write_all(content.as_bytes()).map_err(error)?;
        file.sync_all().map_err(error)?;
        drop(file);
        validate_existing(&destination)?;
        fs::rename(&staging, &destination).map_err(error)?;
        sync_directory(parent)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&staging);
    }
    result
}

#[cfg_attr(
    not(unix),
    allow(
        clippy::unnecessary_wraps,
        reason = "Directory fsync is fallible on Unix"
    )
)]
fn sync_directory(directory: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        File::open(directory)
            .and_then(|file| file.sync_all())
            .map_err(error)?;
    }
    #[cfg(not(unix))]
    let _ = directory;
    Ok(())
}

fn file_limit(path: &str) -> usize {
    if path.ends_with(&format!("/{CATEGORY_MARKER}")) {
        return 64;
    }
    match path {
        LOG => MAX_HISTORY,
        INDEX => MAX_INDEX,
        STATE => 2048,
        PENDING => MAX_JOURNAL,
        _ => MAX_PAGE,
    }
}

fn change(root: &Path, path: &str, after: Option<String>) -> Result<Change, String> {
    let before = read_bounded(&safe_file(root, path)?, file_limit(path))?.map(|text| hash(&text));
    Ok(Change {
        path: path.to_owned(),
        before,
        after,
    })
}

/// Record a committed change in the page history. The page is already
/// written, so a history that cannot be written is reported, not undone.
fn remember(root: &Path, change: &super::history::Change<'_>) {
    if let Err(error) = super::history::record(root, change) {
        harness_types::diag::warn("knowledge", "history_failed")
            .field("path", change.path.to_owned())
            .field("error", error)
            .emit();
    }
}

fn add_log(
    root: &Path,
    changes: &mut Vec<Change>,
    action: &str,
    path: &str,
    page_hash: &str,
    by: Option<&str>,
) -> Result<(), String> {
    let mut log = read_bounded(&safe_file(root, LOG)?, MAX_HISTORY)?.unwrap_or_default();
    if !log.is_empty() && !log.ends_with('\n') {
        log.push('\n');
    }
    let _ = write!(
        log,
        "- {} {action} {} {page_hash}",
        unique_id(),
        serde_json::to_string(path).map_err(error)?
    );
    if let Some(by) = by {
        let _ = write!(log, " by {by}");
    }
    log.push('\n');
    // The oldest lines go: a full history refused every later write, for good.
    if log.len() > MAX_HISTORY {
        let keep_from = log.len() - MAX_HISTORY / 4 * 3;
        let start = log[keep_from..]
            .find('\n')
            .map_or(keep_from, |offset| keep_from + offset + 1);
        log.drain(..start);
    }
    changes.push(change(root, LOG, Some(log))?);
    Ok(())
}

fn transact(root: &Path, changes: Vec<Change>) -> Result<(), String> {
    let changes = changes
        .into_iter()
        .filter(|change| change.before != change.after.as_deref().map(hash))
        .collect::<Vec<_>>();
    if changes.is_empty() {
        return Ok(());
    }
    let transaction = Transaction {
        version: 1,
        id: unique_id(),
        changes,
    };
    // Reject destination conflicts before recording an operation that cannot
    // recover (for example, a page path occupied by a category directory).
    validate_transaction(root, &transaction)?;
    validate_entry_budget(root, &transaction.changes)?;
    let journal = serde_json::to_string(&transaction).map_err(error)?;
    if journal.len() > MAX_JOURNAL {
        return Err("knowledge transaction size limit exceeded".to_owned());
    }
    atomic_write(root, PENDING, &journal)?;
    recover(root)
}

fn recover(root: &Path) -> Result<(), String> {
    let Some(journal) = read_bounded(&safe_file(root, PENDING)?, MAX_JOURNAL)? else {
        return Ok(());
    };
    let transaction: Transaction = serde_json::from_str(&journal)
        .map_err(|e| format!("invalid pending knowledge transaction: {e}"))?;
    // A file changed by someone else since the transaction stopped: their
    // version stands, and the interrupted write is set aside instead of
    // refusing every later call - `/knowledge off` included - for good. The
    // index is rebuilt from the pages as they are.
    let conflicting = transaction.changes.iter().any(|change| {
        safe_file(root, &change.path)
            .and_then(|destination| read_bounded(&destination, file_limit(&change.path)))
            .map(|current| current.map(|text| hash(&text)))
            .is_ok_and(|current| {
                current != change.after.as_deref().map(hash) && current != change.before
            })
    });
    if conflicting {
        fs::rename(safe_file(root, PENDING)?, root.join(ABANDONED)).map_err(error)?;
        let pages = scan(root)?;
        atomic_write(root, INDEX, &render_index(&pages)?)?;
        return sync_directory(root);
    }
    validate_transaction(root, &transaction)?;
    // Validate every input before touching any remaining file.
    for change in transaction.changes {
        let destination = safe_file(root, &change.path)?;
        let current = read_bounded(&destination, file_limit(&change.path))?.map(|text| hash(&text));
        if current == change.after.as_deref().map(hash) {
            if change.after.is_none() {
                cleanup_category_marker(&destination);
            }
            continue;
        }
        if current != change.before {
            return Err("knowledge changed while recovering its transaction".to_owned());
        }
        if let Some(text) = change.after {
            atomic_write(root, &change.path, &text)?;
        } else {
            fs::remove_file(&destination).map_err(error)?;
            sync_directory(
                destination
                    .parent()
                    .ok_or("invalid knowledge destination")?,
            )?;
            cleanup_category_marker(&destination);
        }
    }
    fs::remove_file(safe_file(root, PENDING)?).map_err(error)?;
    sync_directory(root)
}

fn validate_transaction(root: &Path, transaction: &Transaction) -> Result<(), String> {
    if transaction.version != 1
        || transaction.changes.is_empty()
        || transaction.changes.len() > 16_387
    {
        return Err("unsupported pending knowledge transaction".to_owned());
    }
    let written_paths = transaction
        .changes
        .iter()
        .filter(|change| change.after.is_some())
        .map(|change| normalized(&change.path))
        .collect::<BTreeSet<_>>();
    for path in &written_paths {
        let mut ancestor = path.as_str();
        while let Some((parent, _)) = ancestor.rsplit_once('/') {
            if written_paths.contains(parent) {
                return Err("knowledge destination conflicts with a parent file".to_owned());
            }
            ancestor = parent;
        }
    }
    let mut paths = BTreeSet::new();
    for change in &transaction.changes {
        if change.path == PENDING || !paths.insert(change.path.clone()) {
            return Err("invalid knowledge transaction path".to_owned());
        }
        let destination = safe_file(root, &change.path)?;
        if change
            .after
            .as_ref()
            .is_some_and(|text| text.len() > file_limit(&change.path))
        {
            return Err("pending knowledge value exceeds limit".to_owned());
        }
        let current = read_bounded(&destination, file_limit(&change.path))?.map(|text| hash(&text));
        let after = change.after.as_deref().map(hash);
        if current != after && current != change.before {
            return Err(format!(
                "knowledge transaction conflicts with an external edit: {}",
                change.path
            ));
        }
    }
    Ok(())
}

/// Keep successful writes within the same entry budget used by later reads.
fn validate_entry_budget(root: &Path, changes: &[Change]) -> Result<(), String> {
    let mut entries = BTreeSet::new();
    let mut duplicate_entries = 0;
    let mut directories = BTreeSet::new();
    let mut queue = if root.exists() {
        vec![root.to_path_buf()]
    } else {
        Vec::new()
    };
    while let Some(directory) = queue.pop() {
        for entry in fs::read_dir(directory).map_err(error)? {
            let entry = entry.map_err(error)?;
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .map_err(error)?
                .components()
                .map(|part| part.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            // The recovery journal is removed before the next store read.
            if relative == PENDING {
                continue;
            }
            if !entries.insert(normalized(&relative)) {
                duplicate_entries += 1;
            }
            if entries.len() + duplicate_entries > 8192 {
                return Err("knowledge directory entry limit exceeded".to_owned());
            }
            let metadata = fs::symlink_metadata(&path).map_err(error)?;
            if metadata.is_dir()
                && !is_link(&metadata)
                && !entry.file_name().to_string_lossy().starts_with('.')
            {
                directories.insert(normalized(&relative));
                queue.push(path);
            }
        }
    }
    for change in changes {
        let path = normalized(&change.path);
        if change.after.is_none() {
            entries.remove(&path);
        } else {
            entries.insert(path.clone());
            let mut ancestor = path.as_str();
            while let Some((parent, _)) = ancestor.rsplit_once('/') {
                entries.insert(parent.to_owned());
                directories.insert(parent.to_owned());
                ancestor = parent;
            }
        }
    }
    let mut pruned = changes
        .iter()
        .filter(|change| change.after.is_none())
        .filter_map(|change| change.path.strip_suffix(&format!("/{CATEGORY_MARKER}")))
        .map(normalized)
        .collect::<Vec<_>>();
    pruned.sort_by_key(|path| std::cmp::Reverse(path.split('/').count()));
    for directory in pruned {
        let prefix = format!("{directory}/");
        if directories.contains(&directory) && !entries.iter().any(|path| path.starts_with(&prefix))
        {
            entries.remove(&directory);
        }
    }
    if entries.len() + duplicate_entries > 8192 {
        return Err("knowledge directory entry limit exceeded".to_owned());
    }
    Ok(())
}

fn cleanup_category_marker(destination: &Path) {
    if destination
        .file_name()
        .is_some_and(|name| name == CATEGORY_MARKER)
    {
        // Never recurse or remove a directory that still contains data.
        if let Some(parent) = destination.parent() {
            let _ = fs::remove_dir(parent);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(path: &str, content: &str) -> WritePage {
        WritePage {
            path: path.to_owned(),
            title: path.to_owned(),
            summary: "A durable reference".to_owned(),
            content: content.to_owned(),
            sources: vec!["https://example.com/source".to_owned()],
            related: Vec::new(),
            expected_hash: None,
        }
    }

    fn store() -> (tempfile::TempDir, Store) {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::new(directory.path().join("knowledge"));
        (directory, store)
    }

    /// Found testing a release build: one page with broken metadata, one too
    /// large and one not UTF-8 each failed every list, search and status of
    /// the scope; an unaccented query found nothing in accented pages; and a
    /// hand-written page had no summary in the index.
    #[test]
    fn broken_files_are_skipped_and_named_and_unaccented_queries_find_accents() {
        let (dir, store) = store();
        store.write(input("concepts/a.md", "A reference")).unwrap();
        let root = dir.path().join("knowledge");
        fs::write(
            root.join("cache.md"),
            "# Chính sách bộ nhớ đệm\n\nCache giữ 5 phút cho mỗi phiên.\n",
        )
        .unwrap();
        fs::write(
            root.join("broken.md"),
            "<!-- ha-knowledge: {not json} -->\n# B\n",
        )
        .unwrap();
        fs::write(root.join("huge.md"), "a".repeat(MAX_PAGE + 1)).unwrap();
        fs::write(root.join("bad.md"), b"# Bad\n\xff\xfe\n").unwrap();
        let stats = store.stats().unwrap();
        assert_eq!(stats.pages, 2, "{stats:?}");
        let skipped = stats.skipped.join("\n");
        for name in ["bad.md", "broken.md", "huge.md"] {
            assert!(skipped.contains(name), "{skipped}");
        }
        assert_eq!(store.list().unwrap().len(), 2);
        let hits = store.search("bo nho dem", 5).unwrap();
        assert_eq!(hits.first().map(|hit| hit.path.as_str()), Some("cache.md"));
        let cache = store
            .list()
            .unwrap()
            .into_iter()
            .find(|page| page.path == "cache.md")
            .unwrap();
        assert_eq!(cache.summary, "Cache giữ 5 phút cho mỗi phiên.");
    }

    #[test]
    fn crud_provenance_directed_links_and_backlinks() {
        let (_dir, store) = store();
        let a = store.write(input("concepts/a.md", "A reference")).unwrap();
        let mut b = input("notes/b.md", "B reference");
        b.related.push(a.path.clone());
        let b = store.write(b).unwrap();
        let read = store.read(&a.path, 1, 200).unwrap();
        assert_eq!(read.backlinks.as_slice(), std::slice::from_ref(&b.path));
        assert_eq!(read.sources, ["https://example.com/source"]);
        assert!(
            store
                .read(&b.path, 1, 200)
                .unwrap()
                .content
                .contains("../concepts/a.md")
        );
        let graph = store.graph().unwrap();
        assert_eq!(graph.nodes.len(), 2);
        assert_eq!(graph.edges.len(), 1);
        assert_eq!(graph.edges[0].source, b.path);
        assert!(store.delete(&a.path, &a.hash).is_err());
        store.delete(&b.path, &b.hash).unwrap();
        store.delete(&a.path, &a.hash).unwrap();
        assert_eq!(store.stats().unwrap().pages, 0);
        assert!(store.search("reference", 10).unwrap().is_empty());
    }

    #[test]
    fn expected_hash_and_identical_retries_do_not_duplicate_history() {
        let (_dir, store) = store();
        let a = store.write(input("a.md", "first")).unwrap();
        let log = fs::read_to_string(store.root.join(LOG)).unwrap();
        assert_eq!(store.write(input("a.md", "first")).unwrap().hash, a.hash);
        assert_eq!(fs::read_to_string(store.root.join(LOG)).unwrap(), log);
        assert!(store.write(input("a.md", "changed")).is_err());
        let mut update = input("a.md", "changed");
        update.expected_hash = Some(a.hash);
        let changed = store.write(update.clone()).unwrap();
        let log = fs::read_to_string(store.root.join(LOG)).unwrap();
        assert_eq!(store.write(update).unwrap().hash, changed.hash);
        assert_eq!(fs::read_to_string(store.root.join(LOG)).unwrap(), log);
        assert!(store.delete("a.md", "stale").is_err());
    }

    /// Every write and delete is a version with the whole page, and revert
    /// brings one back: an earlier text of a page that exists, or a page that
    /// was deleted.
    #[test]
    fn a_pages_history_reverts_an_edit_and_a_delete() {
        let (_dir, store) = store();
        let first = store.write(input("a.md", "first text")).unwrap();
        let mut update = input("a.md", "second text");
        update.expected_hash = Some(first.hash.clone());
        let second = store.write(update).unwrap();
        let history = store.history(Some("a.md"), None, 10).unwrap();
        assert_eq!(
            history
                .iter()
                .map(|event| event.operation.as_str())
                .collect::<Vec<_>>(),
            ["update", "insert"]
        );
        let first_version = history[1].id.clone();
        // An edit taken back needs the page's current hash.
        assert!(store.revert("a.md", &first_version, None, None).is_err());
        let reverted = store
            .revert("a.md", &first_version, Some(&second.hash), Some("review"))
            .unwrap();
        assert_eq!(reverted.hash, first.hash);
        assert!(
            store
                .read("a.md", 1, 50)
                .unwrap()
                .content
                .contains("first text")
        );
        // A deleted page comes back as it was.
        store.delete("a.md", &first.hash).unwrap();
        let gone = store.history(Some("a.md"), None, 10).unwrap();
        assert_eq!(gone[0].operation, "delete");
        assert_eq!(gone[1].cause.as_deref(), Some("revert"));
        let back = store.revert("a.md", &gone[0].id, None, None).unwrap();
        assert_eq!(back.hash, first.hash);
        assert_eq!(store.history(None, None, 50).unwrap().len(), 5);
        assert!(store.revert("a.md", "no-such-version", None, None).is_err());
    }

    /// A page written before the history began still has a version to go
    /// back to: its first recorded change keeps the text it replaced.
    #[test]
    fn the_first_change_keeps_the_page_it_replaced() {
        let (_dir, store) = store();
        let old = store
            .write(input("b.md", "written before history"))
            .unwrap();
        std::fs::remove_file(super::super::history::path(&store.root)).unwrap();
        let mut update = input("b.md", "rewritten");
        update.expected_hash = Some(old.hash.clone());
        store.write(update).unwrap();
        let history = store.history(Some("b.md"), None, 10).unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[1].cause.as_deref(), Some("baseline"));
        assert_eq!(history[1].hash, old.hash);
    }

    #[test]
    fn concurrent_writes_preserve_every_page_index_and_log() {
        let (dir, store) = store();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
        let jobs = (0..4)
            .map(|worker| {
                let root = store.root.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    for n in 0..8 {
                        Store::new(root.clone())
                            .write(input(&format!("w{worker}/p{n}.md"), "parallel"))
                            .unwrap();
                    }
                })
            })
            .collect::<Vec<_>>();
        for job in jobs {
            job.join().unwrap();
        }
        assert_eq!(store.list().unwrap().len(), 32);
        assert_eq!(
            fs::read_to_string(store.root.join(LOG))
                .unwrap()
                .lines()
                .count(),
            32
        );
        assert_eq!(
            fs::read_to_string(store.root.join(INDEX))
                .unwrap()
                .lines()
                .filter(|line| line.starts_with("- "))
                .count(),
            32
        );
        assert!(dir.path().exists());
    }

    #[test]
    fn disabled_store_gates_all_access_and_preserves_pages() {
        let (_dir, store) = store();
        let a = store.write(input("a.md", "private knowledge")).unwrap();
        store.set_enabled(false).unwrap();
        assert!(!store.enabled().unwrap());
        assert!(store.list().is_err());
        assert!(store.read("a.md", 1, 20).is_err());
        assert!(store.search("private", 10).is_err());
        assert!(store.write(input("b.md", "disabled")).is_err());
        assert!(store.delete("a.md", &a.hash).is_err());
        assert!(store.graph().is_err());
        assert_eq!(store.refresh().unwrap().pages, 0);
        assert!(store.catalog(&[("private", 1.0)], 2000).unwrap().is_empty());
        store.set_enabled(true).unwrap();
        assert_eq!(store.list().unwrap().len(), 1);
    }

    #[test]
    fn missing_store_is_lazy_and_scopes_are_isolated() {
        let (dir, first) = store();
        assert!(first.enabled().unwrap());
        assert!(first.list().unwrap().is_empty());
        first.refresh().unwrap();
        assert!(!first.root.exists());
        let second = Store::new(dir.path().join("other"));
        first.write(input("a.md", "one scope")).unwrap();
        assert!(second.search("scope", 20).unwrap().is_empty());
        second.set_enabled(false).unwrap();
        assert!(first.enabled().unwrap());
    }

    #[test]
    fn external_edits_additions_and_deletions_are_seen_without_restart() {
        let (_dir, store) = store();
        let a = store.write(input("a.md", "original")).unwrap();
        fs::write(store.root.join("a.md"), "# Edited\nexternal fact").unwrap();
        assert_ne!(store.read("a.md", 1, 20).unwrap().hash, a.hash);
        assert_eq!(store.search("external", 10).unwrap()[0].title, "Edited");
        fs::write(store.root.join("added.MD"), "# Added\nexternal more").unwrap();
        assert_eq!(store.refresh().unwrap().pages, 2);
        fs::remove_file(store.root.join("a.md")).unwrap();
        assert_eq!(store.search("external", 10).unwrap().len(), 1);
        assert_eq!(store.refresh().unwrap().pages, 1);
        assert!(
            !fs::read_to_string(store.root.join(INDEX))
                .unwrap()
                .contains("a.md")
        );
    }

    #[test]
    fn external_markdown_links_update_graph_backlinks_and_deletion_guards() {
        let (_dir, store) = store();
        let target = store
            .write(input("concepts/École notes.md", "target fact"))
            .unwrap();
        fs::create_dir_all(store.root.join("notes")).unwrap();
        fs::write(
            store.root.join("notes/ref.md"),
            "# Reference\n[Read](../concepts/%C3%89cole%20notes.md#details \"title\")\n",
        )
        .unwrap();
        let graph = store.graph().unwrap();
        assert_eq!(graph.edges.len(), 1);
        assert_eq!(graph.edges[0].target, target.path);
        assert_eq!(
            store.document(&target.path).unwrap().backlinks,
            ["notes/ref.md"]
        );
        assert!(
            store
                .delete(&target.path, &target.hash)
                .unwrap_err()
                .contains("backlinks")
        );
        fs::write(store.root.join("notes/ref.md"), "# Reference\nlink removed").unwrap();
        assert!(store.document(&target.path).unwrap().backlinks.is_empty());
        fs::write(
            store.root.join("notes/ref.md"),
            "# Reference\n[Read](<../concepts/École notes.md>)",
        )
        .unwrap();
        assert_eq!(store.graph().unwrap().edges.len(), 1);
        fs::remove_file(store.root.join("notes/ref.md")).unwrap();
        store.delete(&target.path, &target.hash).unwrap();
    }

    #[test]
    fn markdown_graph_excludes_code_images_remote_self_and_escaping_links() {
        let (_dir, store) = store();
        store.write(input("target.md", "target")).unwrap();
        fs::create_dir_all(store.root.join("notes")).unwrap();
        fs::write(
            store.root.join("notes/ref.md"),
            concat!(
                "# Reference\n[Valid](../target.md#part)\n[Duplicate](<../target.md> 'title')\n",
                "![Image](../target.md)\n`[Code](../target.md)`\n",
                "```markdown\n[Fenced](../target.md)\n```\n",
                "~~~\n[Tilde](../target.md)\n~~~\n    [Indented](../target.md)\n",
                "<!-- [Comment](../target.md) -->\n[Remote](https://example.com/target.md)\n",
                "[Absolute](/target.md)\n[Drive](C:/target.md)\n[Outside](../../target.md)\n",
                "[EncodedOutside](%2e%2e/%2e%2e/target.md)\n[Missing](../missing.md)\n",
                "[Self](ref.md#anchor)\n[Anchor](#anchor)\n"
            ),
        )
        .unwrap();
        let graph = store.graph().unwrap();
        assert_eq!(graph.edges.len(), 1);
        assert_eq!(graph.edges[0].source, "notes/ref.md");
        assert_eq!(graph.edges[0].target, "target.md");
        fs::write(
            store.root.join("notes/ref.md"),
            concat!(
                "# Reference\n![Image](../target.md)\n`[Code](../target.md)`\n",
                "```\n[Fenced](../target.md)\n```\n<!-- [Comment](../target.md) -->\n",
                "[Outside](../../target.md)\n[Self](ref.md)\n"
            ),
        )
        .unwrap();
        assert!(store.graph().unwrap().edges.is_empty());
    }

    #[test]
    fn native_content_links_and_reference_links_use_actual_page_paths() {
        let (_dir, store) = store();
        store.write(input("Target (one).md", "fact")).unwrap();
        let value = store
            .write(input(
                "notes/ref.md",
                concat!(
                    "[Inline](../Target%20%28one%29.md)\n[Reference][ID]\n",
                    "[ID]: <../Target (one).md#part> \"title\"\n",
                    "[Unused]: ../missing.md\n"
                ),
            ))
            .unwrap();
        assert_eq!(value.related, ["Target (one).md"]);
        assert_eq!(store.graph().unwrap().edges.len(), 1);
    }

    #[test]
    fn console_document_is_full_bounded_and_respects_off_and_safe_paths() {
        let (_dir, store) = store();
        let value = store
            .write(input("long.md", &"long reference\n".repeat(5000)))
            .unwrap();
        assert!(store.read("long.md", 1, 200).unwrap().truncated);
        let document = store.document("long.md").unwrap();
        assert_eq!(document.page.hash, value.hash);
        assert_eq!(
            document.content,
            fs::read_to_string(store.root.join("long.md")).unwrap()
        );
        assert!(store.document("../long.md").is_err());
        store.set_enabled(false).unwrap();
        assert!(store.document("long.md").is_err());
        assert!(store.corpus().is_err());
    }

    #[test]
    fn invalid_paths_and_missing_related_pages_are_refused() {
        let (_dir, store) = store();
        for path in [
            "../a.md",
            "/a.md",
            "C:/a.md",
            "a\\b.md",
            "notes//a.md",
            "a/./b.md",
            "index.md",
            "LOG.MD",
            ".ha-pending.json",
            "CON.md",
            "a/NUL.txt.md",
            "COM1.md",
            "bad .md/next.md",
            "a.md:stream",
            "a.pdf",
        ] {
            // A space before the extension is valid; trailing component spaces are not.
            if path == "bad .md/next.md" {
                continue;
            }
            assert!(store.write(input(path, "bad")).is_err(), "accepted {path}");
        }
        let mut value = input("valid.md", "body");
        value.related.push("missing.md".to_owned());
        assert!(store.write(value).is_err());
        assert!(!store.root.exists());
    }

    /// Seen: with sixty pages, a "continue" matched nothing and the catalog
    /// showed the first pages by path, without the one about the task. Ranked
    /// by the goal too, it leads; and a catalog that leaves pages out says so.
    #[test]
    fn the_catalog_is_ranked_by_the_goal_and_says_what_it_leaves_out() {
        let (_dir, store) = store();
        for index in 0..60 {
            let mut filler = input(
                &format!("analysis/a-{index:02}.md"),
                "Báo cáo phân tích chung\n",
            );
            filler.title = format!("Phân tích {index}");
            store.write(filler).unwrap();
        }
        let mut cart = input(
            "concepts/cart-retention.md",
            "Giỏ hàng giữ sản phẩm 24 giờ.\n",
        );
        cart.title = "Chính sách giỏ hàng".to_owned();
        cart.summary = "Giỏ hàng giữ sản phẩm trong 24 giờ".to_owned();
        store.write(cart).unwrap();
        let by_message = store.catalog(&[("tiếp tục", 2.0)], 3_000).unwrap();
        assert!(!by_message.contains("cart-retention"), "{by_message}");
        let ranked = store
            .catalog(
                &[("Thiết kế chính sách giỏ hàng", 3.0), ("tiếp tục", 2.0)],
                3_000,
            )
            .unwrap();
        assert!(
            ranked
                .lines()
                .next()
                .unwrap()
                .contains("concepts/cart-retention.md"),
            "{ranked}"
        );
        assert!(ranked.contains("more page(s) not listed"), "{ranked}");
        assert!(
            !ranked.contains("sha256:"),
            "no hash in the catalog: {ranked}"
        );
    }

    /// A page carries when the store last wrote it, and the catalog says how
    /// old it is: a page that records a state goes stale. A retry that changes
    /// nothing keeps the time; a page written by hand, or before 0.6.3, is as
    /// old as its file.
    #[test]
    fn a_page_knows_its_age_and_the_catalog_shows_it() {
        let (_dir, store) = store();
        let written = store
            .write(input("ops/deploy.md", "Deploy on Fridays.\n"))
            .unwrap();
        let stamp = written
            .updated_at
            .clone()
            .expect("a written page has a time");
        assert!(
            chrono::DateTime::parse_from_rfc3339(&stamp).is_ok(),
            "{stamp}"
        );
        let catalog = store.catalog(&[("deploy", 1.0)], 3_000).unwrap();
        assert!(catalog.contains("\"updated\":\"<1h ago\""), "{catalog}");
        // The same write again is a no-op, time included.
        let again = store
            .write(input("ops/deploy.md", "Deploy on Fridays.\n"))
            .unwrap();
        assert_eq!(again.updated_at.as_deref(), Some(stamp.as_str()));
        assert_eq!(again.hash, written.hash);
        // An old page: metadata without a time, written long ago.
        let old = store.root.join("ops/old.md");
        fs::write(
            &old,
            format!("{META}{{\"title\":\"Old\",\"summary\":\"from 0.6.2\"}} -->\n# Old\n\nkept\n"),
        )
        .unwrap();
        let long_ago = std::time::SystemTime::now() - std::time::Duration::from_hours(9 * 24);
        fs::File::options()
            .write(true)
            .open(&old)
            .unwrap()
            .set_modified(long_ago)
            .unwrap();
        let catalog = store.catalog(&[("old", 1.0)], 3_000).unwrap();
        assert!(catalog.contains("\"updated\":\"9d ago\""), "{catalog}");
    }

    #[test]
    fn unicode_search_and_outputs_remain_bounded() {
        let (_dir, store) = store();
        let mut value = input("概念/ghi-nhớ.md", &"Dữ liệu 学习知识\n".repeat(3000));
        value.title = "Bộ nhớ học tập".to_owned();
        store.write(value).unwrap();
        assert_eq!(store.search("Dữ liệu", 50).unwrap().len(), 1);
        assert_eq!(store.search("学习知识", 50).unwrap().len(), 1);
        let read = store.read("概念/ghi-nhớ.md", 1, usize::MAX).unwrap();
        assert!(read.content.len() <= MAX_READ);
        assert!(read.content.lines().count() <= MAX_LINES);
        assert!(read.truncated);
        let catalog = store.catalog(&[("học", 1.0)], 190).unwrap();
        assert!(catalog.chars().count() <= 190);
        assert_eq!(store.search("", 10).unwrap().len(), 0);
    }

    #[test]
    fn recovery_rolls_forward_partial_commit_once_without_duplicate_log() {
        let (_dir, store) = store();
        let root = resolved_root(&store.root).unwrap();
        let page = "# Recovered\nDurable fact\n".to_owned();
        let changes = vec![
            change(&root, "a.md", Some(page.clone())).unwrap(),
            change(
                &root,
                INDEX,
                Some("# Knowledge Index\n- [Recovered](a.md)\n".to_owned()),
            )
            .unwrap(),
            change(
                &root,
                LOG,
                Some("- transaction-one write a.md\n".to_owned()),
            )
            .unwrap(),
        ];
        let transaction = Transaction {
            version: 1,
            id: "one".to_owned(),
            changes,
        };
        atomic_write(
            &root,
            PENDING,
            &serde_json::to_string(&transaction).unwrap(),
        )
        .unwrap();
        atomic_write(&root, "a.md", &page).unwrap();
        assert_eq!(store.list().unwrap().len(), 1);
        assert!(!root.join(PENDING).exists());
        store.list().unwrap();
        assert_eq!(
            fs::read_to_string(root.join(LOG)).unwrap().lines().count(),
            1
        );
    }

    #[test]
    fn search_preserves_word_boundaries_and_ignores_internal_metadata() {
        let (_dir, store) = store();
        store
            .write(input("capital.md", "Capital allocation policy"))
            .unwrap();
        assert!(store.search("api", 10).unwrap().is_empty());
        assert!(store.search("ha-knowledge", 10).unwrap().is_empty());
        let mut page = input("resume.md", "Caf\u{e9} r\u{e9}sum\u{e9} facts");
        page.title = "Unicode".to_owned();
        store.write(page).unwrap();
        assert_eq!(
            store.search("cafe\u{301}", 10).unwrap()[0].path,
            "resume.md"
        );
    }

    #[test]
    fn multiple_query_variants_merge_results_without_duplicate_votes() {
        let (_dir, store) = store();
        store
            .write(input(
                "tls.md",
                "TLS services rotate certificates before expiry.",
            ))
            .unwrap();
        store
            .write(input("other.md", "TLS diagnostic reference."))
            .unwrap();
        let queries = vec![
            "TLS".to_owned(),
            "certificates".to_owned(),
            "tls".to_owned(),
        ];
        let found = store.search_queries(&queries, 10).unwrap();
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].path, "tls.md");
        assert_eq!(found[0].matched_queries, [0, 1]);
        assert!(found[0].score > found[1].score);
    }

    #[test]
    fn search_snippet_finds_the_relevant_passage_late_in_a_page() {
        let (_dir, store) = store();
        let mut page = input(
            "handbook.md",
            &format!(
                "{}\n## Certificate renewal\nRotate certificates before expiry to avoid TLS outages. QA_PASSAGE_739\n",
                "Ordinary unrelated instructions.\n".repeat(220)
            ),
        );
        page.title = "Operations handbook".to_owned();
        store.write(page).unwrap();
        let found = store.search("certificate renewal", 10).unwrap();
        assert!(
            found[0].snippet.contains("QA_PASSAGE_739"),
            "{}",
            found[0].snippet
        );
        assert!(found[0].start_line > 200);
        let read = store
            .read(
                "handbook.md",
                found[0].start_line,
                found[0].end_line - found[0].start_line + 1,
            )
            .unwrap();
        assert_eq!(found[0].snippet, read.content);
    }

    #[test]
    fn a_match_deep_inside_a_unicode_line_can_be_read_and_continued() {
        let (_dir, store) = store();
        let body = format!(
            "{} IMPORTANT_X739 rotation policy {}",
            "界 ".repeat(6000),
            "tail ".repeat(4000)
        );
        store.write(input("long.md", &body)).unwrap();
        let hit = store.search("IMPORTANT_X739", 5).unwrap().remove(0);
        assert!(hit.snippet.contains("IMPORTANT_X739"));
        assert!(hit.start_column > 1000);
        let read = store
            .read_at("long.md", hit.start_line, hit.start_column, 1, 800)
            .unwrap();
        assert!(read.content.contains("IMPORTANT_X739"));
        assert!(read.truncated);
        let next = store
            .read_at("long.md", read.next_line, read.next_column, 1, 800)
            .unwrap();
        assert_eq!(
            next.start_column,
            read.start_column + read.content.chars().count()
        );
        let original = std::fs::read_to_string(store.root.join("long.md")).unwrap();
        let original = original.lines().nth(hit.start_line - 1).unwrap();
        assert_eq!(
            format!("{}{}", read.content, next.content),
            original
                .chars()
                .skip(hit.start_column - 1)
                .take(1600)
                .collect::<String>()
        );
    }

    #[test]
    fn complete_read_with_a_trailing_blank_line_is_not_truncated() {
        let (_dir, store) = store();
        store.write(input("blank.md", "Body\n\n")).unwrap();
        assert!(!store.read("blank.md", 1, 200).unwrap().truncated);
    }

    #[test]
    fn unicode_case_and_normalization_do_not_create_colliding_pages_or_a_stuck_journal() {
        let (_dir, store) = store();
        let first = store
            .write(input("concepts/École.md", "original fact"))
            .unwrap();
        let mut update = input("CONCEPTS/E\u{301}COLE.md", "updated fact");
        update.expected_hash = Some(first.hash);
        let changed = store.write(update).unwrap();
        assert_eq!(changed.path, first.path);
        assert_eq!(store.list().unwrap().len(), 1);
        assert!(
            store
                .read("concepts/école.md", 1, 20)
                .unwrap()
                .content
                .contains("updated fact")
        );
        let mut related = input("other.md", "linked fact");
        related.related = vec!["CONCEPTS/e\u{301}cole.md".to_owned()];
        store.write(related).unwrap();
        assert_eq!(store.graph().unwrap().edges[0].target, first.path);
        assert!(!store.root.join(PENDING).exists());
    }

    #[test]
    /// An interrupted write whose file someone changed since: their version
    /// stands, and the write is set aside rather than refusing every later
    /// call - `/knowledge off` included - for good.
    fn recovery_preserves_conflicting_external_changes() {
        let (_dir, store) = store();
        let root = resolved_root(&store.root).unwrap();
        let transaction = Transaction {
            version: 1,
            id: "conflict".to_owned(),
            changes: vec![
                change(&root, "a.md", Some("# intended".to_owned())).unwrap(),
                change(&root, INDEX, Some("# intended index".to_owned())).unwrap(),
            ],
        };
        atomic_write(
            &root,
            PENDING,
            &serde_json::to_string(&transaction).unwrap(),
        )
        .unwrap();
        fs::write(root.join("a.md"), "external edit").unwrap();
        store.list().expect("the store is usable again");
        assert_eq!(
            fs::read_to_string(root.join("a.md")).unwrap(),
            "external edit"
        );
        assert!(!root.join(PENDING).exists());
        assert!(root.join(ABANDONED).exists(), "kept for a look");
        assert!(
            fs::read_to_string(root.join(INDEX))
                .unwrap()
                .contains("a.md"),
            "the index is rebuilt from the pages as they are"
        );
    }

    /// A full history no longer refuses every later write: its oldest lines go.
    #[test]
    fn a_full_history_drops_its_oldest_lines() {
        let (_dir, store) = store();
        store.write(input("a.md", "first\n")).unwrap();
        let root = resolved_root(&store.root).unwrap();
        let line = "- old write \"x.md\" sha256:0\n";
        // Just under the limit, so the next line takes it over.
        let old = line.repeat((MAX_HISTORY - 40) / line.len());
        fs::write(root.join(LOG), &old).unwrap();
        store.write(input("b.md", "second\n")).unwrap();
        let log = fs::read_to_string(root.join(LOG)).unwrap();
        assert!(log.len() <= MAX_HISTORY, "{}", log.len());
        assert!(log.lines().last().unwrap().contains("\"b.md\""));
        assert!(log.lines().next().unwrap().starts_with("- old write"));
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_cannot_escape_or_be_indexed() {
        use std::os::unix::fs::symlink;
        let (dir, store) = store();
        let outside = dir.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("secret.md"), "secret").unwrap();
        fs::create_dir_all(&store.root).unwrap();
        symlink(&outside, store.root.join("link")).unwrap();
        symlink(outside.join("secret.md"), store.root.join("secret.md")).unwrap();
        assert!(store.list().unwrap().is_empty());
        assert!(store.read("secret.md", 1, 20).is_err());
        assert!(store.write(input("link/new.md", "escape")).is_err());
        assert!(!outside.join("new.md").exists());
    }
}
