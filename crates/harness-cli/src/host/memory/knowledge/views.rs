//! What the host derives from a library's notes on each call, and stores
//! nowhere: the timeline of dated statements, the newest pages, the
//! paragraphs other pages cite, the search filters, and the checks of a link
//! to a paragraph.
//!
//! "Can LLM Agents Organize Their Own Memory?" (`SoMem`, arXiv 2611.11929)
//! has the runtime derive Graph, Timeline and Recent from the documents the
//! agent writes, so a revision never has to be propagated to a second copy,
//! and found each view's effect within noise next to the evidence the
//! references resolve to (`notes.rs`, `evidence.rs`). ha derives the graph
//! on every scan and keeps nothing for it; these views are the same: a
//! timeline written into the library as files would be scanned as pages.

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{Value, json};

use super::{BTreeMap, BTreeSet, Page, Store, links, normalized, notes, require_enabled, scan};

/// How many of the newest pages the turn's catalog adds to the ranked list.
pub(crate) const RECENT_IN_CATALOG: usize = 3;

/// The most `recent` returns.
const RECENT_MAX: usize = 20;

/// The history versions read to find the newest pages: more than the pages
/// asked for, since one page can hold several of the newest versions.
const RECENT_VERSIONS: usize = 96;

/// Which pages a search looks in.
#[derive(Clone, Debug, Default)]
pub(crate) struct SearchFilter {
    /// Only pages whose path starts with this (a folder, `concepts/`, or the
    /// start of a name).
    pub path: Option<String>,
    /// Only pages with a statement dated at or after this, or - a page with
    /// no dated statement - written at or after it.
    pub since: Option<DateTime<Utc>>,
    /// The same, before this.
    pub until: Option<DateTime<Utc>>,
}

impl SearchFilter {
    fn dated(&self) -> bool {
        self.since.is_some() || self.until.is_some()
    }
}

/// Keep the pages a filter admits, and say for a date window which time let
/// each in: `event_time` when the page holds a statement dated inside it,
/// `updated_at` for a page with no dated statement, which is all a page that
/// has none can offer. A page whose dated statements are all outside is out
/// even when it was written inside.
pub(super) fn apply_filter(
    pages: &mut Vec<Page>,
    filter: &SearchFilter,
) -> BTreeMap<String, &'static str> {
    let mut matched = BTreeMap::new();
    if let Some(prefix) = &filter.path {
        let prefix = normalized(prefix.trim_start_matches("./"));
        pages.retain(|page| normalized(&page.summary.path).starts_with(&prefix));
    }
    if !filter.dated() {
        return matched;
    }
    pages.retain(|page| {
        let dated = notes::parse(&page.text).dated();
        if dated.is_empty() {
            let written = page
                .summary
                .updated_at
                .as_deref()
                .and_then(|at| DateTime::parse_from_rfc3339(at).ok())
                .map(|at| at.with_timezone(&Utc));
            let inside = written.is_some_and(|at| {
                filter.since.is_none_or(|since| at >= since)
                    && filter.until.is_none_or(|until| at < until)
            });
            if inside {
                matched.insert(page.summary.path.clone(), "updated_at");
            }
            inside
        } else if dated
            .iter()
            .any(|statement| statement.time.within(filter.since, filter.until))
        {
            matched.insert(page.summary.path.clone(), "event_time");
            true
        } else {
            false
        }
    });
    matched
}

/// The paragraphs of `path` that other pages cite, and who cites each.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct Citation {
    pub page: String,
    pub anchor: String,
    /// The start of the paragraph; empty when the page has no such anchor
    /// any more (the citing page also lists it as a dangling anchor).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub paragraph: String,
}

/// Whether a page's text can hold a link to a paragraph, cheaply.
fn may_cite(text: &str) -> bool {
    text.contains("#^") || text.contains("#%5E") || text.contains("#%5e")
}

pub(super) fn cited_by(pages: &[Page], path: &str) -> Vec<Citation> {
    let Some(target) = pages
        .iter()
        .find(|page| normalized(&page.summary.path) == normalized(path))
    else {
        return Vec::new();
    };
    let paragraphs = notes::parse(&target.text)
        .anchors
        .into_iter()
        .map(|anchor| (anchor.id, anchor.preview))
        .collect::<BTreeMap<_, _>>();
    let mut cited = Vec::new();
    for page in pages {
        if page.summary.path == target.summary.path || !may_cite(&page.text) {
            continue;
        }
        for (to, anchor) in links::citations(&page.summary.path, &page.text) {
            if normalized(&to) == normalized(&target.summary.path) {
                cited.push(Citation {
                    page: page.summary.path.clone(),
                    paragraph: paragraphs.get(&anchor).cloned().unwrap_or_default(),
                    anchor,
                });
            }
        }
    }
    cited
}

/// The anchors a page's links cite in each page they point at.
pub(super) fn cited_anchors(page: &Page) -> BTreeMap<String, Vec<String>> {
    let mut cited = BTreeMap::<String, Vec<String>>::new();
    if may_cite(&page.text) {
        for (to, anchor) in links::citations(&page.summary.path, &page.text) {
            cited.entry(to).or_default().push(anchor);
        }
    }
    cited
}

/// Set each page's `dangling_anchors`: its links to a paragraph that the
/// page they name does not define. Said, not refused: the target page may
/// have been edited since the link was written.
pub(super) fn resolve_anchor_links(pages: &mut [Page]) {
    let mut anchors = BTreeMap::<String, BTreeSet<String>>::new();
    let mut missing = Vec::new();
    for (index, page) in pages.iter().enumerate() {
        if !may_cite(&page.text) {
            continue;
        }
        let mut lost = BTreeSet::new();
        for (to, anchor) in links::citations(&page.summary.path, &page.text) {
            let Some(target) = pages
                .iter()
                .find(|other| normalized(&other.summary.path) == normalized(&to))
            else {
                continue;
            };
            let held = anchors
                .entry(target.summary.path.clone())
                .or_insert_with(|| {
                    notes::parse(&target.text)
                        .anchors
                        .into_iter()
                        .map(|anchor| anchor.id)
                        .collect()
                });
            if !held.contains(&anchor) {
                lost.insert(format!("{}#^{anchor}", target.summary.path));
            }
        }
        if !lost.is_empty() {
            missing.push((index, lost));
        }
    }
    for (index, lost) in missing {
        pages[index].summary.dangling_anchors = lost.into_iter().collect();
    }
}

/// Whether `page` (at `path`) defines the anchor a fragment names.
///
/// # Errors
/// The fragment is not `^anchor`, or the page has no such paragraph.
pub(super) fn check_anchor(path: &str, text: &str, fragment: &str) -> Result<(), String> {
    let id = fragment.strip_prefix('^').ok_or_else(|| {
        format!(
            "{path}#{fragment}: a fragment that names a paragraph is #^anchor; the paragraph ends with ` ^anchor`"
        )
    })?;
    let held = notes::parse(text);
    if held.anchor_ids().contains(id) {
        return Ok(());
    }
    let have = held
        .anchors
        .iter()
        .map(|anchor| format!("^{}", anchor.id))
        .collect::<Vec<_>>();
    Err(format!(
        "{path} has no paragraph anchored ^{id} ({}); end the paragraph with ` ^{id}` in {path} first, or link the page without the fragment",
        if have.is_empty() {
            "it has no anchors".to_owned()
        } else {
            format!("it has {}", have.join(", "))
        }
    ))
}

/// The links of a page about to be written that name a paragraph: each must
/// be there, as a `related` entry's must. Only a citation the write brings in
/// is checked; one the page already held stays, and a scan reports it as a
/// dangling anchor when its paragraph is gone.
pub(super) fn check_citations(
    path: &str,
    new: &str,
    previous: &[&str],
    pages: &[Page],
) -> Result<(), String> {
    if !may_cite(new) {
        return Ok(());
    }
    let had = previous
        .iter()
        .flat_map(|text| links::citations(path, text))
        .collect::<BTreeSet<_>>();
    let mut problems = Vec::new();
    for (to, anchor) in links::citations(path, new) {
        if had.contains(&(to.clone(), anchor.clone())) {
            continue;
        }
        let text = if normalized(&to) == normalized(path) {
            new
        } else if let Some(page) = pages
            .iter()
            .find(|page| normalized(&page.summary.path) == normalized(&to))
        {
            page.text.as_str()
        } else {
            // A link to a page that is not there is a dangling link, which
            // is allowed so a page can be written before its target.
            continue;
        };
        if let Err(why) = check_anchor(&to, text, &format!("^{anchor}")) {
            problems.push(why);
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{path} was not written, a link names a paragraph that is not there: {}",
            problems.join("; ")
        ))
    }
}

/// The newest `count` pages by when they were written, the most recent
/// first: the turn's catalog shows them beside the ranked list.
pub(super) fn newest(pages: &[Page], count: usize) -> Vec<&Page> {
    let written = |page: &Page| {
        page.summary
            .updated_at
            .as_deref()
            .and_then(|at| DateTime::parse_from_rfc3339(at).ok())
            .map(|at| at.with_timezone(&Utc))
    };
    let mut dated = pages
        .iter()
        .filter_map(|page| Some((written(page)?, page)))
        .collect::<Vec<_>>();
    dated.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| a.1.summary.path.cmp(&b.1.summary.path))
    });
    dated
        .into_iter()
        .take(count)
        .map(|(_, page)| page)
        .collect()
}

/// One dated statement of the library.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct TimelineEntry {
    /// The event time as it was written: a year, a month or a day.
    pub time: String,
    pub path: String,
    pub title: String,
    pub line: usize,
    /// The paragraph's anchor, when it has one: `path#^anchor` reaches it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor: Option<String>,
    pub note: String,
    pub statement: String,
    pub references: Vec<String>,
}

/// A page written lately.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct Recent {
    pub path: String,
    pub title: String,
    pub summary: String,
    /// When it was last written.
    pub at: String,
    /// Who wrote it: the agent, the review, the wiki pass.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
}

impl Store {
    /// The dated statements of the library, the oldest first (a year before
    /// its months: the coarser time sorts first), then by page and line. A
    /// statement is the sentence its footnote marker follows. Only
    /// statements whose time overlaps `[since, until)`, and pages under
    /// `prefix`. Returns the first `limit` and how many there are.
    pub(crate) fn timeline(
        &self,
        prefix: Option<&str>,
        since: Option<DateTime<Utc>>,
        until: Option<DateTime<Utc>>,
        limit: usize,
    ) -> Result<(Vec<TimelineEntry>, usize), String> {
        self.with_lock(|root| {
            require_enabled(root)?;
            let mut pages = scan(root)?;
            apply_filter(
                &mut pages,
                &SearchFilter {
                    path: prefix.map(str::to_owned),
                    ..SearchFilter::default()
                },
            );
            let mut entries = Vec::new();
            for page in &pages {
                for statement in notes::parse(&page.text).dated() {
                    if !statement.time.within(since, until) {
                        continue;
                    }
                    entries.push((
                        statement.time,
                        TimelineEntry {
                            time: statement.time.to_string(),
                            path: page.summary.path.clone(),
                            title: page.summary.title.clone(),
                            line: statement.line,
                            anchor: statement.anchor,
                            note: statement.note,
                            statement: statement.statement,
                            references: statement.references,
                        },
                    ));
                }
            }
            entries.sort_by(|a, b| {
                a.0.cmp(&b.0)
                    .then_with(|| a.1.path.cmp(&b.1.path))
                    .then_with(|| a.1.line.cmp(&b.1.line))
            });
            let total = entries.len();
            Ok((
                entries
                    .into_iter()
                    .take(limit)
                    .map(|(_, entry)| entry)
                    .collect(),
                total,
            ))
        })
    }

    /// The `count` pages written most recently, the newest first, with the
    /// title and summary of each. Page-level: the library's history keeps
    /// whole versions of a page for 30 days (ten for each page beyond that),
    /// so what a version added could be derived only for a page changed in
    /// that time, and only by comparing it with the version before. When
    /// the history knows a page (it records who wrote it), `by` says who.
    pub(crate) fn recent(&self, count: usize) -> Result<Vec<Recent>, String> {
        let count = count.clamp(1, RECENT_MAX);
        self.with_lock(|root| {
            require_enabled(root)?;
            let pages = scan(root)?;
            let mut writers = BTreeMap::<String, (DateTime<Utc>, String)>::new();
            for event in super::super::history::events(
                root,
                &super::super::history::Window::default(),
                RECENT_VERSIONS,
            )? {
                if event.operation == "delete" || event.cause.as_deref() == Some("baseline") {
                    continue;
                }
                let Ok(at) = DateTime::parse_from_rfc3339(&event.at) else {
                    continue;
                };
                writers
                    .entry(event.path.clone())
                    .or_insert((at.with_timezone(&Utc), event.actor));
            }
            let mut written = pages
                .iter()
                .filter_map(|page| {
                    let recorded = writers.get(&page.summary.path);
                    let at = recorded.map(|(at, _)| *at).or_else(|| {
                        DateTime::parse_from_rfc3339(page.summary.updated_at.as_deref()?)
                            .ok()
                            .map(|at| at.with_timezone(&Utc))
                    })?;
                    Some((
                        at,
                        Recent {
                            path: page.summary.path.clone(),
                            title: page.summary.title.clone(),
                            summary: page.summary.summary.clone(),
                            at: at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                            by: recorded.map(|(_, actor)| actor.clone()),
                        },
                    ))
                })
                .collect::<Vec<_>>();
            written.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.path.cmp(&b.1.path)));
            Ok(written
                .into_iter()
                .take(count)
                .map(|(_, recent)| recent)
                .collect())
        })
    }

    /// The source behind a statement: the footnote `note` of `path`, its
    /// statement and event time, and what each of its references holds now
    /// (`Sources::open`: a file's lines with a little around them, the
    /// journal source's text, a URL as it is). A reference that no longer
    /// resolves says why instead of failing the call: the file may have
    /// changed since the footnote was written.
    pub(crate) fn evidence(&self, path: &str, note: &str) -> Result<Value, String> {
        let path = super::page_path(path)?;
        let note = note.trim().trim_start_matches("[^").trim_end_matches(']');
        self.with_lock(|root| {
            require_enabled(root)?;
            let pages = scan(root)?;
            let page = super::find_page(&pages, &path)?;
            let notes = notes::parse(&page.text);
            let Some(definition) = notes.definition(note) else {
                let known = notes
                    .definitions
                    .iter()
                    .map(|definition| format!("[^{}]", definition.id))
                    .collect::<Vec<_>>();
                return Err(format!(
                    "{} has no footnote [^{note}] ({})",
                    page.summary.path,
                    if known.is_empty() {
                        "it has no footnotes".to_owned()
                    } else {
                        format!("it has {}", known.join(", "))
                    }
                ));
            };
            let sources = self.sources.clone().unwrap_or_default();
            let statements = notes
                .markers
                .iter()
                .filter(|marker| marker.id == definition.id)
                .map(|marker| json!({"line": marker.line, "statement": marker.statement}))
                .collect::<Vec<_>>();
            let references = definition
                .references
                .iter()
                .map(|token| {
                    super::super::evidence::Reference::parse(token)
                        .and_then(|reference| sources.open(&reference))
                        .unwrap_or_else(|why| json!({"ref": token, "error": why}))
                })
                .collect::<Vec<_>>();
            Ok(json!({
                "path": page.summary.path,
                "note": definition.id,
                "event_time": definition.time.map(|time| time.to_string()),
                "statements": statements,
                "references": references,
            }))
        })
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use super::super::super::evidence::Sources;
    use super::super::{Store, WritePageUpdate};
    use super::*;

    /// A library whose footnotes resolve against a workspace with one file
    /// (`src/hr.rs`, five lines) and a journal with one source.
    struct Fixture {
        _directory: tempfile::TempDir,
        store: Store,
        root: PathBuf,
        source: String,
    }

    fn fixture() -> Fixture {
        let directory = tempfile::tempdir().expect("directory");
        let workspace = directory.path().join("workspace");
        fs::create_dir_all(workspace.join("src")).expect("workspace");
        fs::write(
            workspace.join("src").join("hr.rs"),
            "fn one() {}\nfn two() {}\nfn three() {}\nfn four() {}\nfn five() {}\n",
        )
        .expect("file");
        let journal = directory.path().join("journal");
        fs::create_dir_all(&journal).expect("journal");
        let source = format!("event_{}", uuid::Uuid::now_v7());
        let connection =
            rusqlite::Connection::open(journal.join("harness.sqlite3")).expect("database");
        connection
            .execute(
                "CREATE TABLE history_sources (source_id TEXT PRIMARY KEY, session_id TEXT NOT NULL, task_id TEXT NOT NULL, project_id TEXT NOT NULL, sequence INTEGER NOT NULL, kind TEXT NOT NULL, content TEXT NOT NULL, content_hash TEXT NOT NULL, availability TEXT NOT NULL)",
                [],
            )
            .expect("table");
        connection
            .execute(
                "INSERT INTO history_sources VALUES (?1, 's', 't', 'p', 1, 'user_message', 'We moved to B in August.', 'h', 'available')",
                [&source],
            )
            .expect("row");
        let root = directory.path().join("knowledge");
        let store = Store::new(root.clone())
            .with_sources(Sources::in_workspace(workspace).with_journal(journal));
        Fixture {
            _directory: directory,
            store,
            root,
            source,
        }
    }

    /// Write a page as the agent does: the current hash when it exists.
    fn put(store: &Store, path: &str, content: &str) -> Result<super::super::PageSummary, String> {
        let hash = store
            .list()
            .expect("list")
            .into_iter()
            .find(|page| page.path == path)
            .map(|page| page.hash);
        store.write_update(&WritePageUpdate {
            path: path.to_owned(),
            title: Some("Work history".to_owned()),
            summary: Some("Where people worked".to_owned()),
            content: content.to_owned(),
            sources: Some(Vec::new()),
            related: Some(Vec::new()),
            expected_hash: hash,
            origin: None,
        })
    }

    /// A page as a person would have saved it, with the time it was written.
    fn by_hand(root: &Path, path: &str, updated_at: &str, body: &str) {
        let title = path.trim_end_matches(".md");
        let file = root.join(path);
        fs::create_dir_all(file.parent().expect("parent")).expect("folder");
        fs::write(
            file,
            format!(
                "<!-- ha-knowledge: {{\"title\":\"{title}\",\"summary\":\"About {title}\",\"updated_at\":\"{updated_at}\"}} -->\n# {title}\n\n{body}\n"
            ),
        )
        .expect("page");
    }

    fn file_bytes(root: &Path, path: &str) -> Vec<u8> {
        fs::read(root.join(path)).expect("page file")
    }

    /// `SoMem` (arXiv 2611.11929) validates every evidence reference before an
    /// edit is accepted, and a rejected edit leaves memory as it was: a
    /// footnote that cites a file that is not there, a line past the end, or a
    /// source the journal does not hold is refused, and nothing is written.
    #[test]
    fn a_footnote_that_cites_a_missing_file_a_line_out_of_range_or_an_unknown_source_is_refused_and_nothing_is_written()
     {
        let f = fixture();
        let unknown = format!("event_{}", uuid::Uuid::now_v7());
        for (reference, expected) in [
            ("src/nowhere.rs:1", "no such file"),
            ("src/hr.rs:9", "has 5 line(s)"),
            ("src/hr.rs:4-9", "out of range"),
            (unknown.as_str(), "not a source of this project's journal"),
            ("src/hr.rs:0", "lines start at 1"),
            ("ftp://example.test/x", "http:// or https://"),
        ] {
            let content = format!("Joined A in May.[^e1]\n\n[^e1]: 2026-05; {reference}\n");
            let why = put(&f.store, "work.md", &content).expect_err(reference);
            assert!(
                why.contains("[^e1]") && why.contains(expected),
                "{reference}: {why}"
            );
            assert!(why.contains("Rule: a footnote is"), "{why}");
        }
        assert!(
            !f.root.join("work.md").exists(),
            "a refused write leaves no page"
        );
        // A marker with no definition, and a definition no statement uses.
        let why = put(&f.store, "work.md", "Joined A.[^lost]\n").unwrap_err();
        assert!(
            why.contains("[^lost]") && why.contains("no definition"),
            "{why}"
        );
        let why = put(&f.store, "work.md", "Joined A.\n\n[^idle]: 2026\n").unwrap_err();
        assert!(
            why.contains("[^idle]") && why.contains("no statement uses it"),
            "{why}"
        );
        // A page that held a good footnote stays as it was when an update's is bad.
        put(
            &f.store,
            "work.md",
            "Joined A in May.[^e1]\n\n[^e1]: 2026-05; src/hr.rs:2\n",
        )
        .expect("a footnote that resolves");
        let before = file_bytes(&f.root, "work.md");
        put(
            &f.store,
            "work.md",
            "Joined A in May.[^e1]\n\n[^e1]: 2026-05; src/hr.rs:2-80\n",
        )
        .expect_err("a changed definition is looked up again");
        assert_eq!(file_bytes(&f.root, "work.md"), before);
    }

    #[test]
    fn a_valid_footnote_is_written_and_evidence_returns_the_cited_lines_and_the_journal_source() {
        let f = fixture();
        let content = format!(
            "Joined A in May.[^e1] Moved to B in August.[^e2]\n\n[^e1]: 2026-05; src/hr.rs:2-3, https://example.test/a\n[^e2]: 2026-08; {}\n",
            f.source
        );
        put(&f.store, "work.md", &content).expect("every reference resolves");
        let shown = f.store.evidence("work.md", "e1").expect("evidence");
        assert_eq!(shown["event_time"], "2026-05");
        assert_eq!(shown["statements"][0]["statement"], "Joined A in May.");
        let lines = shown["references"][0]["text"].as_str().expect("lines");
        assert!(
            lines.contains(">2| fn two() {}") && lines.contains(">3| fn three() {}"),
            "{lines}"
        );
        assert!(
            lines.contains(" 1| fn one() {}") && lines.contains(" 4| fn four() {}"),
            "{lines}"
        );
        assert!(
            lines.contains(" 5| fn five() {}"),
            "two lines of context after: {lines}"
        );
        assert_eq!(shown["references"][1]["kind"], "url");
        let moved = f.store.evidence("work.md", "[^e2]").expect("evidence");
        assert_eq!(moved["references"][0]["kind"], "history");
        assert_eq!(moved["references"][0]["text"], "We moved to B in August.");
        // The file moves on: the call says so for that reference, not for the others.
        fs::write(f.root.join("../workspace/src/hr.rs"), "fn only() {}\n").expect("edit");
        let stale = f.store.evidence("work.md", "e1").expect("evidence");
        assert!(
            stale["references"][0]["error"]
                .as_str()
                .is_some_and(|why| why.contains("has 1 line(s)")),
            "{stale}"
        );
        assert_eq!(stale["references"][1]["kind"], "url");
        // The page is still editable elsewhere though its citation went stale.
        put(&f.store, "work.md", &format!("{content}\nAnd more.\n"))
            .expect("a definition the page already held is not looked up again");
        let why = f.store.evidence("work.md", "nope").unwrap_err();
        assert!(
            why.contains("no footnote [^nope]") && why.contains("[^e1]"),
            "{why}"
        );
    }

    /// A page without footnotes is exactly what 0.9.7 wrote, and one with
    /// free-text footnotes of its own keeps being editable.
    #[test]
    fn a_page_without_footnotes_round_trips_byte_for_byte_and_old_free_text_footnotes_do_not_block_an_edit()
     {
        let f = fixture();
        let content = "Plain text, a link to [other](other.md), 2 ^ 3 and x ^2\n\n- one\n- two\n";
        let written = put(&f.store, "plain.md", content).expect("a plain page");
        let first = file_bytes(&f.root, "plain.md");
        let text = String::from_utf8(first.clone()).expect("utf8");
        assert!(
            text.ends_with(&format!(
                "{content}\n## Related\n\n- [other.md](../other.md)\n"
            )) || text.contains(content),
            "{text}"
        );
        let same = put(&f.store, "plain.md", content).expect("the same write again");
        assert_eq!(same.hash, written.hash);
        assert_eq!(
            file_bytes(&f.root, "plain.md"),
            first,
            "a retry changes nothing"
        );
        // A page a person wrote with a footnote of free text.
        by_hand(
            &f.root,
            "rfc.md",
            "2026-01-01T00:00:00Z",
            "The protocol retries.[^1]\n\n[^1]: See the RFC, section 4.\n",
        );
        put(
            &f.store,
            "rfc.md",
            "The protocol retries.[^1] It backs off.\n\n[^1]: See the RFC, section 4.\n",
        )
        .expect("an old footnote that is kept as it is is not checked again");
        put(
            &f.store,
            "rfc.md",
            "The protocol retries.[^1] It backs off.[^2]\n\n[^1]: See the RFC, section 4.\n[^2]: See the RFC, section 5.\n",
        )
        .expect_err("a new footnote is checked, and free text is not a reference");
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one story: the link, the related entry, the rewrite and the scan"
    )]
    fn a_link_or_related_entry_to_a_missing_anchor_is_refused_and_a_scan_reports_one_that_went_missing()
     {
        let f = fixture();
        put(
            &f.store,
            "project.md",
            "Orion is a project at B. ^orion-1\n\nOther text.\n",
        )
        .expect("target");
        let why = put(
            &f.store,
            "work.md",
            "Moved to B for [Orion](project.md#^orion-9).\n",
        )
        .unwrap_err();
        assert!(
            why.contains("project.md") && why.contains("^orion-9") && why.contains("^orion-1"),
            "{why}"
        );
        assert!(!f.root.join("work.md").exists());
        put(
            &f.store,
            "work.md",
            "Moved to B for [Orion](project.md#^orion-1).\n",
        )
        .expect("anchor is there");
        // A heading fragment is an ordinary link and is not checked.
        put(
            &f.store,
            "notes.md",
            "See [Orion](project.md#some-heading).\n",
        )
        .expect("heading fragment");
        // Two paragraphs cannot share an anchor.
        let why = put(&f.store, "twice.md", "One. ^same-id\n\nTwo. ^same-id\n").unwrap_err();
        assert!(
            why.contains("^same-id") && why.contains("one paragraph"),
            "{why}"
        );
        // A related entry may name a paragraph; it is checked, the page is kept.
        let hash = f
            .store
            .list()
            .expect("list")
            .into_iter()
            .find(|p| p.path == "work.md")
            .map(|p| p.hash);
        let mut update = WritePageUpdate {
            path: "work.md".to_owned(),
            title: None,
            summary: None,
            content: "Moved to B for [Orion](project.md#^orion-1).\n".to_owned(),
            sources: None,
            related: Some(vec!["project.md#^nope".to_owned()]),
            expected_hash: hash.clone(),
            origin: None,
        };
        assert!(f.store.write_update(&update).unwrap_err().contains("^nope"));
        update.related = Some(vec!["project.md#^orion-1".to_owned()]);
        update.content = "Moved to B.\n".to_owned();
        let kept = f
            .store
            .write_update(&update)
            .expect("related with an anchor");
        assert_eq!(kept.related, ["project.md"]);
        // The paragraph is rewritten away: the link stays, and the scan names it.
        let target = f
            .store
            .list()
            .expect("list")
            .into_iter()
            .find(|p| p.path == "project.md")
            .map(|p| p.hash);
        f.store
            .write_update(&WritePageUpdate {
                path: "project.md".to_owned(),
                title: None,
                summary: None,
                content: "Orion is a project at B.\n".to_owned(),
                sources: None,
                related: None,
                expected_hash: target,
                origin: None,
            })
            .expect("the target page is the author's to rewrite");
        let stats = f.store.stats().expect("stats");
        assert_eq!(stats.dangling, 0);
        assert_eq!(stats.dangling_anchors, 0, "work.md now links the page only");
        by_hand(
            &f.root,
            "old.md",
            "2026-01-01T00:00:00Z",
            "From [Orion](project.md#^orion-1) once.",
        );
        let stats = f.store.stats().expect("stats");
        assert_eq!(stats.dangling_anchors, 1);
        assert_eq!(
            stats.dangling_anchors_first,
            ["old.md -> project.md#^orion-1"]
        );
        // It is still an edge of the page-level graph, with no anchor to name.
        let (graph, _) = f.store.graph_bounded(100).expect("graph");
        assert!(
            graph
                .edges
                .iter()
                .any(|edge| edge.source == "old.md" && edge.target == "project.md")
        );
    }

    #[test]
    fn a_backlink_names_the_paragraph_it_cites_and_the_graph_edge_carries_the_anchor() {
        let f = fixture();
        put(
            &f.store,
            "project.md",
            "Orion is a project at B. ^orion-1\n\nOther text.\n",
        )
        .expect("target");
        put(
            &f.store,
            "work.md",
            "Moved to B for [Orion](project.md#^orion-1) and [the page](project.md).\n",
        )
        .expect("citing page");
        let read = f.store.read("project.md", 1, 50).expect("read");
        assert_eq!(read.backlinks, ["work.md"]);
        assert_eq!(read.anchors, ["orion-1"]);
        assert_eq!(read.cited_by.len(), 1);
        assert_eq!(read.cited_by[0].page, "work.md");
        assert_eq!(read.cited_by[0].anchor, "orion-1");
        assert_eq!(read.cited_by[0].paragraph, "Orion is a project at B.");
        let (graph, _) = f.store.graph_bounded(100).expect("graph");
        let edge = graph
            .edges
            .iter()
            .find(|edge| edge.source == "work.md" && edge.target == "project.md")
            .expect("the page-level edge");
        assert_eq!(edge.anchors, ["orion-1"]);
    }

    #[test]
    fn the_timeline_orders_statements_by_event_time_across_precisions_and_filters_by_window_and_path()
     {
        let f = fixture();
        by_hand(
            &f.root,
            "people/ann.md",
            "2026-06-01T00:00:00Z",
            "Ann was born.[^a] Ann moved in May.[^b] Ann left on the third.[^c]\n\nAnn stayed to the end of April. ^end\u{2d}april\n\n[^a]: 2026\n[^b]: 2026-05\n[^c]: 2026-05-03\n",
        );
        by_hand(
            &f.root,
            "people/bob.md",
            "2026-06-02T00:00:00Z",
            "Bob arrived at the end of April.[^d] Bob has a note with no date.[^e]\n\n[^d]: 2026-04-30\n[^e]: src/hr.rs\n",
        );
        by_hand(
            &f.root,
            "places.md",
            "2026-06-03T00:00:00Z",
            "Paris was visited.[^p]\n\n[^p]: 2025-12-24\n",
        );
        let (all, total) = f.store.timeline(None, None, None, 50).expect("timeline");
        assert_eq!(total, 5);
        assert_eq!(
            all.iter()
                .map(|entry| entry.time.as_str())
                .collect::<Vec<_>>(),
            ["2025-12-24", "2026", "2026-04-30", "2026-05", "2026-05-03"],
            "oldest first, a year before its months"
        );
        assert_eq!(all[1].statement, "Ann was born.");
        assert_eq!(all[1].path, "people/ann.md");
        assert_eq!(all[1].note, "a");
        assert_eq!(all[1].line, 4);
        assert!(all.iter().all(|entry| entry.references.len() <= 1));
        let at = |text: &str| {
            Some(
                chrono::DateTime::parse_from_rfc3339(text)
                    .unwrap()
                    .with_timezone(&Utc),
            )
        };
        let (may, _) = f
            .store
            .timeline(
                None,
                at("2026-05-01T00:00:00Z"),
                at("2026-06-01T00:00:00Z"),
                50,
            )
            .expect("window");
        assert_eq!(
            may.iter()
                .map(|entry| entry.time.as_str())
                .collect::<Vec<_>>(),
            ["2026", "2026-05", "2026-05-03"],
            "a statement dated to the year is in any window that touches it"
        );
        let (people, _) = f
            .store
            .timeline(Some("people/"), None, None, 2)
            .expect("prefix");
        assert_eq!(people.len(), 2);
        let (none, total) = f
            .store
            .timeline(Some("nowhere/"), None, None, 5)
            .expect("none");
        assert!(none.is_empty() && total == 0);
    }

    #[test]
    fn a_search_keeps_pages_under_a_path_and_in_a_window_and_says_which_time_matched() {
        let f = fixture();
        by_hand(
            &f.root,
            "people/ann.md",
            "2026-01-01T00:00:00Z",
            "Ann moved to Lyon.[^a]\n\n[^a]: 2026-05\n",
        );
        by_hand(
            &f.root,
            "people/bob.md",
            "2026-05-10T00:00:00Z",
            "Bob moved to Lyon.[^b]\n\n[^b]: 2024-02-02\n",
        );
        by_hand(
            &f.root,
            "notes/lyon.md",
            "2026-05-20T00:00:00Z",
            "Lyon is a city. Everyone moved to Lyon.",
        );
        by_hand(
            &f.root,
            "notes/old.md",
            "2025-01-01T00:00:00Z",
            "Old moved to Lyon.",
        );
        let at = |text: &str| {
            Some(
                chrono::DateTime::parse_from_rfc3339(text)
                    .unwrap()
                    .with_timezone(&Utc),
            )
        };
        let search = |filter: &SearchFilter| {
            f.store
                .search_filtered(&["moved Lyon".to_owned()], 10, filter)
                .expect("search")
                .into_iter()
                .map(|hit| (hit.path, hit.time_match))
                .collect::<std::collections::BTreeMap<_, _>>()
        };
        let all = search(&SearchFilter::default());
        assert_eq!(all.len(), 4, "{all:?}");
        assert!(all.values().all(Option::is_none));
        let may = search(&SearchFilter {
            since: at("2026-05-01T00:00:00Z"),
            until: at("2026-06-01T00:00:00Z"),
            ..SearchFilter::default()
        });
        assert_eq!(
            may,
            std::collections::BTreeMap::from([
                ("people/ann.md".to_owned(), Some("event_time")),
                ("notes/lyon.md".to_owned(), Some("updated_at")),
            ]),
            "ann by the month her statement is dated to; bob was written in May but his statement is dated 2024; old.md is out"
        );
        let under = search(&SearchFilter {
            path: Some("people/".to_owned()),
            ..SearchFilter::default()
        });
        assert_eq!(
            under.keys().cloned().collect::<Vec<_>>(),
            ["people/ann.md", "people/bob.md"]
        );
        let early = search(&SearchFilter {
            until: at("2024-12-31T00:00:00Z"),
            ..SearchFilter::default()
        });
        assert_eq!(
            early,
            std::collections::BTreeMap::from([("people/bob.md".to_owned(), Some("event_time"))])
        );
    }

    /// `SoMem` puts Recent in the initial context; its ablation found the
    /// effect small, so the catalog adds three lines and keeps room for them.
    #[test]
    fn the_catalog_adds_the_newest_pages_the_ranking_did_not_show_within_its_bound() {
        let f = fixture();
        for (day, name) in ["a", "b", "c", "d", "e", "f", "g", "h"].iter().enumerate() {
            by_hand(
                &f.root,
                &format!("{name}.md"),
                &format!("2026-11-0{}T00:00:00Z", day + 1),
                &format!(
                    "Page {name} says something about deploys and {}.",
                    if *name == "a" { "zebra" } else { "other" }
                ),
            );
        }
        let catalog = f.store.catalog(&[("zebra", 1.0)], 520).expect("catalog");
        assert!(
            catalog.chars().count() <= 520,
            "{}\n{catalog}",
            catalog.chars().count()
        );
        let (listed, recent) = catalog
            .split_once("recently written (not in the list above):")
            .expect("a recent part");
        assert!(
            listed.contains("\"path\":\"a.md\""),
            "the ranked page first: {catalog}"
        );
        for newest in ["h.md", "g.md", "f.md"] {
            assert!(
                recent.contains(&format!("\"path\":\"{newest}\"")),
                "{newest}: {catalog}"
            );
            assert!(
                !listed.contains(&format!("\"path\":\"{newest}\"")),
                "{catalog}"
            );
        }
        assert_eq!(recent.matches("\"path\"").count(), 3);
        assert!(catalog.contains("more page(s) not listed"), "{catalog}");
        // When the ranked list already shows the newest pages, nothing repeats.
        let all = f
            .store
            .catalog(&[("deploys", 1.0)], 20_000)
            .expect("catalog");
        assert!(!all.contains("recently written"), "{all}");
    }

    #[test]
    fn recent_lists_the_newest_pages_first_and_names_who_wrote_the_one_the_store_wrote() {
        let f = fixture();
        by_hand(&f.root, "old.md", "2020-01-01T00:00:00Z", "Old text.");
        by_hand(&f.root, "mid.md", "2021-01-01T00:00:00Z", "Mid text.");
        f.store
            .write_update(&WritePageUpdate {
                path: "fresh.md".to_owned(),
                title: Some("Fresh".to_owned()),
                summary: Some("Just written".to_owned()),
                content: "Fresh text.\n".to_owned(),
                sources: Some(Vec::new()),
                related: Some(Vec::new()),
                expected_hash: None,
                origin: Some("review".to_owned()),
            })
            .expect("write");
        let recent = f.store.recent(2).expect("recent");
        assert_eq!(
            recent
                .iter()
                .map(|page| page.path.as_str())
                .collect::<Vec<_>>(),
            ["fresh.md", "mid.md"]
        );
        assert_eq!(recent[0].by.as_deref(), Some("review"));
        assert_eq!(recent[0].summary, "Just written");
        assert_eq!(recent[1].by, None);
        assert_eq!(f.store.recent(8).expect("recent").len(), 3);
    }

    #[test]
    fn a_link_to_a_paragraph_is_checked_against_the_page_it_names() {
        let target = page("b.md", "# B\n\nThe paragraph. ^para-one\n");
        let ok = "# A\n\nSee [it](b.md#^para-one).\n";
        let missing = "# A\n\nSee [it](b.md#^nope).\n";
        let heading = "# A\n\nSee [it](b.md#some-heading).\n";
        assert!(check_citations("a.md", ok, &[], std::slice::from_ref(&target)).is_ok());
        assert!(check_citations("a.md", heading, &[], std::slice::from_ref(&target)).is_ok());
        let why = check_citations("a.md", missing, &[], std::slice::from_ref(&target)).unwrap_err();
        assert!(
            why.contains("b.md") && why.contains("^nope") && why.contains("^para-one"),
            "{why}"
        );
        // A citation the page already held stays, so an unrelated edit goes through.
        assert!(check_citations("a.md", missing, &[missing], &[target]).is_ok());
    }

    fn page(path: &str, text: &str) -> Page {
        super::super::parse_page(path, text.to_owned()).unwrap()
    }
}
