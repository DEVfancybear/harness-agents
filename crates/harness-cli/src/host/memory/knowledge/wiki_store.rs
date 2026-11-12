//! The wiki pass's edits of a library (`wiki.rs` decides them and checks
//! what the model proposed): each runs under the store's lock, against the
//! hash of the page the pass read, in one transaction with the index and the
//! log, and every page it changes or removes is recorded in the page history
//! as the wiki's, so `history` shows it and `revert` brings the old one back.

use super::{
    BTreeMap, BTreeSet, Change, INDEX, MAX_PAGES, MAX_SCAN_BYTES, META, Metadata, Page,
    PageSummary, Path, Store, add_log, change, error, find_page, hash, links, normalized, notes,
    page_path, parse_metadata, parse_page, remember, render_index, render_page, require_enabled,
    resolve_page_links, scan, single_line, transact,
};

/// Who the wiki pass writes as: in a page's metadata, the log and the history.
pub(crate) const WIKI: &str = "wiki";

/// A page as the wiki pass reads it: what the store says of it, its whole
/// text and who wrote it (`review`, `wiki`, or nobody for the agent).
#[derive(Clone, Debug)]
pub(crate) struct WikiPage {
    pub summary: PageSummary,
    pub text: String,
    pub origin: Option<String>,
}

impl Store {
    /// Run `action` holding the library's lock: what the wiki pass keeps
    /// beside the pages is written under it too.
    pub(crate) fn locked<T>(
        &self,
        action: impl FnOnce(&Path) -> Result<T, String>,
    ) -> Result<T, String> {
        self.with_lock(action)
    }

    /// Every page with its text, for the wiki pass to read.
    pub(crate) fn wiki_pages(&self) -> Result<Vec<WikiPage>, String> {
        self.with_lock(|root| {
            require_enabled(root)?;
            Ok(scan(root)?
                .into_iter()
                .map(|page| WikiPage {
                    origin: parse_metadata(&page.summary.path, &page.text)
                        .ok()
                        .and_then(|metadata| metadata.origin),
                    summary: page.summary,
                    text: page.text,
                })
                .collect())
        })
    }

    /// Point `path`'s links to pages that do not exist at an existing page
    /// (`repoint`: missing target to existing page), or take them out leaving
    /// their label (`unlink`). Only dangling links: a link to a page that
    /// exists is the author's.
    pub(crate) fn wiki_relink(
        &self,
        path: &str,
        expected_hash: &str,
        repoint: &BTreeMap<String, String>,
        unlink: &BTreeSet<String>,
    ) -> Result<PageSummary, String> {
        let path = page_path(path)?;
        self.with_lock(|root| {
            require_enabled(root)?;
            let before = scan(root)?;
            let mut pages = before.clone();
            let index = position(&pages, &path)?;
            current(&pages[index], expected_hash)?;
            let page = &pages[index];
            let actual = page.summary.path.clone();
            let dangling = page
                .summary
                .dangling
                .iter()
                .map(|target| normalized(target))
                .collect::<BTreeSet<_>>();
            let mut mapping = BTreeMap::new();
            for (from, to) in repoint {
                if !dangling.contains(&normalized(from)) {
                    return Err(format!("{actual} has no link to a missing {from}"));
                }
                let target = find_page(&pages, &page_path(to)?)?.summary.path.clone();
                if normalized(&target) == normalized(&actual) {
                    return Err("a link cannot be repointed to its own page".to_owned());
                }
                mapping.insert(normalized(from), target);
            }
            for target in unlink {
                if !dangling.contains(&normalized(target)) {
                    return Err(format!("{actual} has no link to a missing {target}"));
                }
            }
            let mut text = links::rewrite(&actual, &actual, &page.text, &mapping);
            text = links::unlink(&actual, &text, unlink);
            text = with_metadata(&actual, &text, |metadata| {
                let mut related = BTreeSet::new();
                for target in &metadata.related {
                    if unlink
                        .iter()
                        .any(|gone| normalized(gone) == normalized(target))
                    {
                        continue;
                    }
                    related.insert(
                        mapping
                            .get(&normalized(target))
                            .cloned()
                            .unwrap_or_else(|| target.clone()),
                    );
                }
                metadata.related = related.into_iter().collect();
            })?;
            if text == page.text {
                return Err(format!("{actual} would not change"));
            }
            pages[index] = parse_page(&actual, text)?;
            commit_wiki(root, &before, &mut pages, "wiki relink")?;
            Ok(find_page(&pages, &actual)?.summary.clone())
        })
    }

    /// Merge `absorb` into `keep`: `keep` becomes `title`, `summary` and
    /// `content` with the sources of both and the related pages of both; each
    /// page that linked to `absorb` links to `keep` instead; `absorb` goes.
    /// The history keeps both as they were. Returns the pages changed.
    #[allow(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "the two pages with the hashes read, and what the merged page says; one locked read, check and write"
    )]
    pub(crate) fn wiki_merge(
        &self,
        keep: &str,
        keep_hash: &str,
        absorb: &str,
        absorb_hash: &str,
        title: &str,
        summary: &str,
        content: &str,
    ) -> Result<Vec<String>, String> {
        let (keep, absorb) = (page_path(keep)?, page_path(absorb)?);
        if normalized(&keep) == normalized(&absorb) {
            return Err("a page cannot be merged into itself".to_owned());
        }
        let title = single_line(title, 256, "title")?;
        let summary = single_line(summary, 1024, "summary")?;
        if title.is_empty() {
            return Err("the merged page needs a title".to_owned());
        }
        self.with_lock(|root| {
            require_enabled(root)?;
            let before = scan(root)?;
            let mut pages = before.clone();
            let kept = position(&pages, &keep)?;
            let gone = position(&pages, &absorb)?;
            current(&pages[kept], keep_hash)?;
            current(&pages[gone], absorb_hash)?;
            let keep = pages[kept].summary.path.clone();
            let absorb = pages[gone].summary.path.clone();
            let first = parse_metadata(&keep, &pages[kept].text)?;
            let second = parse_metadata(&absorb, &pages[gone].text)?;
            let mut sources = Vec::new();
            for source in first
                .sources
                .iter()
                .chain(&second.sources)
                .chain(&pages[kept].summary.sources)
                .chain(&pages[gone].summary.sources)
            {
                if !sources.contains(source) {
                    sources.push(source.clone());
                }
            }
            if sources.len() > 32 {
                return Err("the merged page would have more than 32 sources".to_owned());
            }
            let related = first
                .related
                .iter()
                .chain(&second.related)
                .filter(|target| {
                    ![&keep, &absorb]
                        .iter()
                        .any(|path| normalized(path) == normalized(target))
                })
                .filter(|target| find_page(&pages, target).is_ok())
                .cloned()
                .collect::<BTreeSet<_>>();
            let metadata = Metadata {
                title,
                summary,
                sources,
                related: related.into_iter().collect(),
                origin: Some(WIKI.to_owned()),
                updated_at: Some(now()),
            };
            let moved = BTreeMap::from([(normalized(&absorb), keep.clone())]);
            let text = links::rewrite(
                &keep,
                &keep,
                &render_page(&keep, &metadata, content)?,
                &moved,
            );
            // The merged text holds the footnotes of both pages: a note
            // either page already held is not looked up again, a new fault is
            // refused (the ids of two pages can collide).
            notes::check(
                &keep,
                &text,
                &[&pages[kept].text, &pages[gone].text],
                self.sources.as_ref(),
            )?;
            pages[kept] = parse_page(&keep, text)?;
            for page in &mut pages {
                let path = page.summary.path.clone();
                if path == keep
                    || path == absorb
                    || !page
                        .summary
                        .related
                        .iter()
                        .any(|target| normalized(target) == normalized(&absorb))
                {
                    continue;
                }
                let text = links::rewrite(&path, &path, &page.text, &moved);
                let text = with_metadata(&path, &text, |metadata| {
                    let related = metadata
                        .related
                        .iter()
                        .map(|target| {
                            moved
                                .get(&normalized(target))
                                .cloned()
                                .unwrap_or_else(|| target.clone())
                        })
                        .filter(|target| normalized(target) != normalized(&path))
                        .collect::<BTreeSet<_>>();
                    metadata.related = related.into_iter().collect();
                })?;
                *page = parse_page(&path, text)?;
            }
            pages.retain(|page| page.summary.path != absorb);
            commit_wiki(root, &before, &mut pages, "wiki merge")
        })
    }

    /// Replace the one place `path` says `old` with `new`: a claim the wiki
    /// pass checked against the page's source. `old` must occur exactly once
    /// in the page, outside its metadata line. `note` is a footnote
    /// definition line (`[^id]: 2026-05; path:line`) that `new` cites: the
    /// pass adds it to the page, and the page's notes are checked as for any
    /// write (`notes::check`).
    pub(crate) fn wiki_replace(
        &self,
        path: &str,
        expected_hash: &str,
        old: &str,
        new: &str,
        note: Option<&str>,
    ) -> Result<PageSummary, String> {
        let path = page_path(path)?;
        if old.is_empty() {
            return Err("a fix names the text it replaces".to_owned());
        }
        self.with_lock(|root| {
            require_enabled(root)?;
            let before = scan(root)?;
            let mut pages = before.clone();
            let index = position(&pages, &path)?;
            current(&pages[index], expected_hash)?;
            let actual = pages[index].summary.path.clone();
            let text = &pages[index].text;
            let (head, body) = match text.split_once('\n') {
                Some((first, rest)) if first.starts_with(META) => (format!("{first}\n"), rest),
                _ => (String::new(), text.as_str()),
            };
            match body.matches(old).count() {
                0 => return Err(format!("{actual} does not say the text the fix replaces")),
                1 => {}
                _ => {
                    return Err(format!(
                        "{actual} says the text the fix replaces more than once; quote more of it"
                    ));
                }
            }
            let mut replaced = format!("{head}{}", body.replacen(old, new, 1));
            if let Some(note) = note {
                replaced = with_definition(&replaced, note);
            }
            notes::check(&actual, &replaced, &[text], self.sources.as_ref())?;
            let replaced = with_metadata(&actual, &replaced, |metadata| {
                metadata.updated_at = Some(now());
            })?;
            pages[index] = parse_page(&actual, replaced)?;
            commit_wiki(root, &before, &mut pages, "wiki fix")?;
            Ok(find_page(&pages, &actual)?.summary.clone())
        })
    }
}

/// `text` with the footnote definition `line` as a block of its own, above
/// the host's `## Related` section when the page has one.
fn with_definition(text: &str, line: &str) -> String {
    let line = line.trim();
    match text.rfind("\n## Related\n") {
        Some(at) => format!("{}\n{line}\n{}", &text[..at], &text[at..]),
        None => format!("{}\n\n{line}\n", text.trim_end()),
    }
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn position(pages: &[Page], path: &str) -> Result<usize, String> {
    pages
        .iter()
        .position(|page| normalized(&page.summary.path) == normalized(path))
        .ok_or_else(|| format!("knowledge page not found: {path}"))
}

/// The page is as the pass read it.
fn current(page: &Page, expected_hash: &str) -> Result<(), String> {
    if page.summary.hash == expected_hash {
        Ok(())
    } else {
        Err(format!(
            "{} changed since the wiki pass read it",
            page.summary.path
        ))
    }
}

/// `text` with its metadata line changed by `change`; a page written by hand
/// has none, and is returned as it is.
fn with_metadata(
    path: &str,
    text: &str,
    change: impl FnOnce(&mut Metadata),
) -> Result<String, String> {
    let Some((first, rest)) = text.split_once('\n') else {
        return Ok(text.to_owned());
    };
    if !first.starts_with(META) {
        return Ok(text.to_owned());
    }
    let mut metadata = parse_metadata(path, text)?;
    change(&mut metadata);
    Ok(format!(
        "{META}{} -->\n{rest}",
        serde_json::to_string(&metadata)
            .map_err(error)?
            .replace("-->", "\\u002d\\u002d\\u003e")
    ))
}

/// Write the pages the pass changed, with the index and one log line, and
/// record each change in the page history as the wiki's. Returns the paths
/// changed.
fn commit_wiki(
    root: &Path,
    before: &[Page],
    pages: &mut [Page],
    action: &str,
) -> Result<Vec<String>, String> {
    if pages.len() > MAX_PAGES || pages.iter().map(|p| p.text.len()).sum::<usize>() > MAX_SCAN_BYTES
    {
        return Err("knowledge corpus limit reached".to_owned());
    }
    pages.sort_by(|a, b| a.summary.path.cmp(&b.summary.path));
    let admitted = pages
        .iter()
        .map(|p| (normalized(&p.summary.path), p.summary.path.clone()))
        .collect::<BTreeMap<_, _>>();
    for page in pages.iter_mut() {
        resolve_page_links(page, &admitted);
    }
    let previous = before
        .iter()
        .map(|p| (p.summary.path.as_str(), p))
        .collect::<BTreeMap<_, _>>();
    let next = pages
        .iter()
        .map(|p| (p.summary.path.as_str(), p))
        .collect::<BTreeMap<_, _>>();
    let mut changes = Vec::new();
    let mut recorded = Vec::new();
    for path in previous
        .keys()
        .chain(next.keys())
        .copied()
        .collect::<BTreeSet<_>>()
    {
        let old = previous.get(path).copied();
        let new = next.get(path).copied();
        if old.map(|page| page.summary.hash.as_str()) == new.map(|page| page.summary.hash.as_str())
        {
            continue;
        }
        changes.push(Change {
            path: path.to_owned(),
            before: old.map(|page| page.summary.hash.clone()),
            after: new.map(|page| page.text.clone()),
        });
        recorded.push((path.to_owned(), old.cloned(), new.cloned()));
    }
    if changes.is_empty() {
        return Ok(Vec::new());
    }
    let affected = recorded
        .iter()
        .map(|(path, _, _)| path.clone())
        .collect::<Vec<_>>();
    changes.push(change(root, INDEX, Some(render_index(pages)?))?);
    add_log(
        root,
        &mut changes,
        action,
        &affected.join(", "),
        &hash(&affected.join("\n")),
        Some(WIKI),
    )?;
    transact(root, changes)?;
    for (path, old, new) in &recorded {
        let (operation, page) = match (old, new) {
            (None, Some(page)) => ("insert", page),
            (Some(_), Some(page)) => ("update", page),
            (Some(page), None) => ("delete", page),
            (None, None) => continue,
        };
        remember(
            root,
            &super::super::history::Change {
                path,
                operation,
                cause: WIKI,
                actor: WIKI,
                hash: &page.summary.hash,
                title: &page.summary.title,
                text: &page.text,
                before: match (old, new) {
                    (Some(old), Some(_)) => Some((old.summary.hash.as_str(), old.text.as_str())),
                    _ => None,
                },
            },
        );
    }
    Ok(affected)
}
