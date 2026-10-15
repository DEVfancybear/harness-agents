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

const STATE: &str = ".ha-state.json";
const PENDING: &str = ".ha-pending.json";
const INDEX: &str = "index.md";
const LOG: &str = "log.md";
const META: &str = "<!-- ha-knowledge: ";
const MAX_PAGE: usize = 256 * 1024;
const MAX_PAGES: usize = 2048;
const MAX_SCAN_BYTES: usize = 16 * 1024 * 1024;
const MAX_HISTORY: usize = 4 * 1024 * 1024;
const MAX_INDEX: usize = 512 * 1024;
const MAX_JOURNAL: usize = MAX_HISTORY + MAX_INDEX + MAX_PAGE * 2;
const MAX_READ: usize = 32 * 1024;
const MAX_LINES: usize = 200;
static NEXT_ID: AtomicU64 = AtomicU64::new(0);

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

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct PageSummary {
    pub path: String,
    pub title: String,
    pub summary: String,
    pub hash: String,
    pub sources: Vec<String>,
    pub related: Vec<String>,
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
}

impl Store {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self { root }
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
            let start = start_line.max(1);
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

    pub(crate) fn write(&self, input: WritePage) -> Result<PageSummary, String> {
        let path = page_path(&input.path)?;
        let title = single_line(&input.title, 256, "title")?;
        if title.is_empty() {
            return Err("knowledge page title is required".to_owned());
        }
        let summary = single_line(&input.summary, 1024, "summary")?;
        if input.sources.len() > 32 || input.related.len() > 64 {
            return Err("too many knowledge sources or related pages".to_owned());
        }
        let mut sources = BTreeSet::new();
        for source in input.sources {
            let source = single_line(&source, 2048, "source")?;
            if !source.is_empty() {
                sources.insert(source);
            }
        }
        self.with_lock(|root| {
            require_enabled(root)?;
            let mut pages = scan(root)?;
            let existing = pages
                .iter()
                .position(|p| normalized(&p.summary.path) == normalized(&path));
            let before = existing.map(|i| pages[i].summary.hash.clone());
            let actual_path =
                existing.map_or_else(|| path.clone(), |i| pages[i].summary.path.clone());
            let mut related = BTreeSet::new();
            for target in &input.related {
                let target = page_path(target)?;
                if normalized(&target) == normalized(&actual_path) {
                    return Err("a knowledge page cannot relate to itself".to_owned());
                }
                related.insert(find_page(&pages, &target)?.summary.path.clone());
            }
            let metadata = Metadata {
                title: title.clone(),
                summary: summary.clone(),
                sources: sources.iter().cloned().collect(),
                related: related.into_iter().collect(),
            };
            let text = render_page(&actual_path, &metadata, &input.content)?;
            if let Some(i) = existing {
                // Retrying a completed operation is a no-op, including its log.
                if pages[i].text == text {
                    return Ok(pages[i].summary.clone());
                }
                if input.expected_hash.as_deref() != Some(pages[i].summary.hash.as_str()) {
                    return Err(
                        "knowledge page changed; read it again and supply its current hash"
                            .to_owned(),
                    );
                }
            } else if input.expected_hash.is_some() {
                return Err("knowledge page no longer exists".to_owned());
            }
            let page = parse_page(&actual_path, text.clone())?;
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
                after: Some(text),
            }];
            changes.push(change(root, INDEX, Some(render_index(&pages)?))?);
            add_log(root, &mut changes, "write", &actual_path, &output.hash)?;
            transact(root, changes)?;
            Ok(output)
        })
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
            if !backlinks(&pages, &path).is_empty() {
                return Err(
                    "knowledge page has backlinks; update referring pages before deleting it"
                        .to_owned(),
                );
            }
            pages.retain(|p| p.summary.path != path);
            let mut changes = vec![Change {
                path: path.clone(),
                before: Some(expected_hash.to_owned()),
                after: None,
            }];
            changes.push(change(root, INDEX, Some(render_index(&pages)?))?);
            add_log(root, &mut changes, "delete", &path, expected_hash)?;
            transact(root, changes)
        })
    }

    pub(crate) fn graph(&self) -> Result<Graph, String> {
        self.with_lock(|root| {
            require_enabled(root)?;
            let pages = scan(root)?;
            let edges = pages
                .iter()
                .flat_map(|page| {
                    page.summary.related.iter().map(|target| Edge {
                        source: page.summary.path.clone(),
                        target: target.clone(),
                    })
                })
                .collect();
            Ok(Graph {
                nodes: pages.into_iter().map(|p| p.summary).collect(),
                edges,
            })
        })
    }

    pub(crate) fn refresh(&self) -> Result<Stats, String> {
        self.with_lock(|root| {
            if !enabled_at(root)? {
                return Ok(Stats {
                    pages: 0,
                    links: 0,
                    enabled: false,
                });
            }
            let pages = scan(root)?;
            // A read-only refresh of an absent, empty store stays absent.
            if root.exists() || !pages.is_empty() {
                transact(
                    root,
                    vec![change(root, INDEX, Some(render_index(&pages)?))?],
                )?;
            }
            Ok(counts(&pages, true))
        })
    }

    pub(crate) fn catalog(&self, query: &str, max_chars: usize) -> Result<String, String> {
        self.with_lock(|root| {
            if !enabled_at(root)? || max_chars == 0 {
                return Ok(String::new());
            }
            let pages = scan(root)?;
            if pages.is_empty() {
                return Ok(String::new());
            }
            let hits = search_pages(&pages, query, 20);
            let mut ordered = hits
                .iter()
                .filter_map(|hit| pages.iter().find(|p| p.summary.path == hit.path))
                .collect::<Vec<_>>();
            for page in &pages {
                if !ordered.iter().any(|p| p.summary.path == page.summary.path) {
                    ordered.push(page);
                }
                if ordered.len() >= 40 {
                    break;
                }
            }
            let limit = max_chars.min(20_000);
            let mut output = String::new();
            for page in ordered {
                // JSON escaping keeps titles from impersonating instructions or fences.
                let line = serde_json::to_string(&serde_json::json!({
                    "path": page.summary.path,
                    "title": page.summary.title,
                    "summary": page.summary.summary,
                    "hash": page.summary.hash,
                }))
                .map_err(error)?;
                if output.chars().count() + line.chars().count() + 1 > limit {
                    break;
                }
                output.push_str(&line);
                output.push('\n');
            }
            Ok(output)
        })
    }

    fn with_lock<T>(&self, action: impl FnOnce(&Path) -> Result<T, String>) -> Result<T, String> {
        let root = resolved_root(&self.root)?;
        let _guard = lock(&root)?;
        validate_existing(&root)?;
        recover(&root)?;
        action(&root)
    }
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
    if !path.to_lowercase().ends_with(".md")
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
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) if is_link(&metadata) => {
                return Err("knowledge paths cannot contain symlinks or reparse points".to_owned());
            }
            Ok(_) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(error(e)),
        }
    }
    Ok(())
}

fn safe_file(root: &Path, path: &str) -> Result<PathBuf, String> {
    if ![STATE, PENDING, INDEX, LOG].contains(&path) {
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
    if !root.exists() {
        return Ok(Vec::new());
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
            if !metadata.is_file() || !name.to_lowercase().ends_with(".md") {
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
                return Err("knowledge paths differ only by letter case".to_owned());
            }
            let Some(text) = read_bounded(&path, MAX_PAGE)? else {
                continue;
            };
            bytes += text.len();
            if bytes > MAX_SCAN_BYTES {
                return Err("knowledge scan size limit exceeded".to_owned());
            }
            pages.push(parse_page(&relative, text)?);
        }
    }
    pages.sort_by(|a, b| a.summary.path.cmp(&b.summary.path));
    let admitted = pages
        .iter()
        .map(|p| (normalized(&p.summary.path), p.summary.path.clone()))
        .collect::<BTreeMap<_, _>>();
    for page in &mut pages {
        let own = normalized(&page.summary.path);
        page.summary.related = page
            .summary
            .related
            .iter()
            .filter_map(|target| {
                if normalized(target) == own {
                    None
                } else {
                    admitted.get(&normalized(target)).cloned()
                }
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
    }
    Ok(pages)
}

fn parse_page(path: &str, text: String) -> Result<Page, String> {
    let mut metadata = Metadata::default();
    if let Some(first) = text.lines().next().and_then(|line| line.strip_prefix(META)) {
        let value = first
            .strip_suffix(" -->")
            .ok_or("invalid knowledge metadata")?;
        metadata = serde_json::from_str(value)
            .map_err(|e| format!("invalid knowledge metadata in {path}: {e}"))?;
    }
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
    Ok(Page {
        summary: PageSummary {
            path: path.to_owned(),
            title: metadata.title,
            summary: metadata.summary,
            hash: hash(&text),
            sources: metadata.sources,
            related: metadata.related,
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

fn backlinks(pages: &[Page], path: &str) -> Vec<String> {
    pages
        .iter()
        .filter(|p| p.summary.related.iter().any(|target| target == path))
        .map(|p| p.summary.path.clone())
        .collect()
}

fn counts(pages: &[Page], enabled: bool) -> Stats {
    Stats {
        pages: pages.len(),
        links: pages.iter().map(|p| p.summary.related.len()).sum(),
        enabled,
    }
}

fn stats_at(root: &Path) -> Result<Stats, String> {
    if enabled_at(root)? {
        Ok(counts(&scan(root)?, true))
    } else {
        Ok(Stats {
            pages: 0,
            links: 0,
            enabled: false,
        })
    }
}

fn normalized(text: &str) -> String {
    text.nfc().collect::<String>().to_lowercase()
}

fn search_pages(pages: &[Page], query: &str, limit: usize) -> Vec<SearchHit> {
    rank_pages(pages, &search_documents(pages), query, limit)
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
        let title = normalized(&page.summary.title);
        let summary = normalized(&page.summary.summary);
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
                let line = normalized(line);
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

fn add_log(
    root: &Path,
    changes: &mut Vec<Change>,
    action: &str,
    path: &str,
    page_hash: &str,
) -> Result<(), String> {
    let mut log = read_bounded(&safe_file(root, LOG)?, MAX_HISTORY)?.unwrap_or_default();
    if !log.is_empty() && !log.ends_with('\n') {
        log.push('\n');
    }
    let _ = writeln!(
        log,
        "- {} {action} {} {page_hash}",
        unique_id(),
        serde_json::to_string(path).map_err(error)?
    );
    if log.len() > MAX_HISTORY {
        return Err("knowledge history size limit reached".to_owned());
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
    if transaction.version != 1 || transaction.changes.is_empty() || transaction.changes.len() > 4 {
        return Err("unsupported pending knowledge transaction".to_owned());
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
    // Validate every input before touching any remaining file.
    for change in transaction.changes {
        let destination = safe_file(root, &change.path)?;
        let current = read_bounded(&destination, file_limit(&change.path))?.map(|text| hash(&text));
        if current == change.after.as_deref().map(hash) {
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
        }
    }
    fs::remove_file(safe_file(root, PENDING)?).map_err(error)?;
    sync_directory(root)
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
        assert!(store.catalog("private", 2000).unwrap().is_empty());
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
            "a.txt",
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
        let catalog = store.catalog("học", 190).unwrap();
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
        assert!(store.list().is_err());
        assert_eq!(
            fs::read_to_string(root.join("a.md")).unwrap(),
            "external edit"
        );
        assert!(!root.join(INDEX).exists());
        assert!(root.join(PENDING).exists());
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
