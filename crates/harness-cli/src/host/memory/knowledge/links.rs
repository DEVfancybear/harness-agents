//! Relative Markdown page links, without interpreting code or remote URLs.

use std::collections::{BTreeMap, BTreeSet};

/// The pages `markdown`, a page at `source`, links to.
pub(super) fn targets(source: &str, markdown: &str) -> Vec<String> {
    link_list(source, markdown)
        .into_iter()
        .map(|(target, _)| target)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// The links that name a paragraph, `page.md#^anchor` (`notes.rs`): the
/// page and the anchor. `targets` has always dropped a fragment; a fragment
/// that begins with `^` is a paragraph's address and is kept here, so a
/// write can check that the paragraph is there and a backlink can say which
/// one a page cites.
pub(super) fn citations(source: &str, markdown: &str) -> Vec<(String, String)> {
    link_list(source, markdown)
        .into_iter()
        .filter_map(|(target, anchor)| anchor.map(|anchor| (target, anchor)))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// `markdown` with code, comments and spans masked, for `notes.rs` to read
/// footnotes and anchors from the same text the links are read from.
pub(super) fn visible(markdown: &str) -> String {
    visible_markdown(markdown)
}

/// The visible text for finding links: a footnote's definition line holds
/// references (`[^e1]: 2026-05; notes.md`), not links, and its first word
/// would otherwise read as a link to a page of that name.
fn visible_for_links(markdown: &str) -> String {
    let mut visible = visible_markdown(markdown);
    let mut offset = 0;
    for line in markdown.split_inclusive('\n') {
        let trimmed = line.trim_start_matches(' ');
        if line.len() - trimmed.len() <= 3 && trimmed.starts_with("[^") && trimmed.contains("]:") {
            let mut masked = visible.as_bytes()[offset..offset + line.len()].to_vec();
            mask(&mut masked);
            visible.replace_range(
                offset..offset + line.len(),
                std::str::from_utf8(&masked).expect("Masked lines contain only ASCII"),
            );
        }
        offset += line.len();
    }
    visible
}

/// Every link to a page, with the paragraph anchor in its fragment, if any.
fn link_list(source: &str, markdown: &str) -> Vec<(String, Option<String>)> {
    let mut visible = visible_for_links(markdown);
    let mut references = BTreeMap::new();
    let mut offset = 0;
    for line in visible.clone().split_inclusive('\n') {
        let trimmed = line.trim_start_matches(' ');
        if line.len() - trimmed.len() <= 3
            && let Some((end, label)) = label_at(trimmed, 0)
            && let Some(rest) = trimmed[end..].strip_prefix(':')
            && let Some((destination, _, _, _)) = destination(rest.trim_start(), false)
        {
            references
                .entry(reference_key(label))
                .or_insert(destination);
            let mut masked = line.as_bytes().to_vec();
            mask(&mut masked);
            visible.replace_range(
                offset..offset + line.len(),
                std::str::from_utf8(&masked).expect("Masked lines contain only ASCII"),
            );
        }
        offset += line.len();
    }
    parsed_links(&visible, &references)
        .into_iter()
        .filter_map(|link| Some((resolve(source, &link.href)?, anchor_of(&link.href))))
        .collect()
}

/// The paragraph anchor a destination's fragment names: `#^id`.
fn anchor_of(href: &str) -> Option<String> {
    let fragment = href.split_once('#')?.1.split('?').next()?;
    let id = fragment
        .strip_prefix('^')
        .or_else(|| fragment.strip_prefix("%5E"))
        .or_else(|| fragment.strip_prefix("%5e"))?;
    (!id.is_empty()).then(|| id.to_owned())
}

/// Rebase actual link destinations when pages move, leaving labels and prose intact.
pub(super) fn rewrite(
    source: &str,
    new_source: &str,
    markdown: &str,
    pages: &BTreeMap<String, String>,
) -> String {
    let mut visible = visible_for_links(markdown);
    let mut references = BTreeMap::new();
    let mut replacements = BTreeMap::new();
    let mut record = |base: usize, href: String, start: usize, end: usize| {
        if let Some(old_target) = resolve(source, &href)
            && let Some(new_target) = pages.get(&super::normalized(&old_target))
        {
            let suffix = href.find(['#', '?']).map_or("", |i| &href[i..]);
            let parent = new_source.split('/').collect::<Vec<_>>();
            let parent = &parent[..parent.len().saturating_sub(1)];
            let target = new_target.split('/').collect::<Vec<_>>();
            let common = parent
                .iter()
                .zip(&target)
                .take_while(|(a, b)| a == b)
                .count();
            let relative = format!(
                "{}{}",
                "../".repeat(parent.len() - common),
                target[common..].join("/")
            );
            replacements.insert(
                base + start,
                (
                    base + end,
                    format!("{}{suffix}", super::encode_path(&relative)),
                ),
            );
        }
    };
    let mut offset = 0;
    for line in visible.clone().split_inclusive('\n') {
        let trimmed = line.trim_start_matches(' ');
        let indent = line.len() - trimmed.len();
        if indent <= 3
            && let Some((end, label)) = label_at(trimmed, 0)
            && let Some(rest) = trimmed[end..].strip_prefix(':')
            && let Some((href, _, start, finish)) = destination(rest, false)
        {
            references
                .entry(reference_key(label))
                .or_insert_with(|| href.clone());
            record(offset + indent + end + 1, href, start, finish);
            let mut masked = line.as_bytes().to_vec();
            mask(&mut masked);
            visible.replace_range(
                offset..offset + line.len(),
                std::str::from_utf8(&masked).expect("Masked lines contain only ASCII"),
            );
        }
        offset += line.len();
    }
    for link in parsed_links(&visible, &references) {
        if let Some((start, end)) = link.destination {
            record(0, link.href, start, end);
        }
    }
    let mut result = markdown.to_owned();
    for (start, (end, target)) in replacements.into_iter().rev() {
        result.replace_range(start..end, &target);
    }
    result
}

/// Replace each inline link to one of `gone` (page paths, as `targets` gives
/// them) with its label, so the prose stays and the link goes: the wiki
/// pass's removal of a link to a page that does not exist. A reference link
/// is left as it is.
pub(super) fn unlink(source: &str, markdown: &str, gone: &BTreeSet<String>) -> String {
    let visible = visible_for_links(markdown);
    let mut references = BTreeMap::new();
    for line in visible.split_inclusive('\n') {
        let trimmed = line.trim_start_matches(' ');
        if line.len() - trimmed.len() <= 3
            && let Some((end, label)) = label_at(trimmed, 0)
            && let Some(rest) = trimmed[end..].strip_prefix(':')
            && let Some((destination, _, _, _)) = destination(rest.trim_start(), false)
        {
            references
                .entry(reference_key(label))
                .or_insert(destination);
        }
    }
    let gone = gone
        .iter()
        .map(|path| super::normalized(path))
        .collect::<BTreeSet<_>>();
    let mut result = markdown.to_owned();
    let mut spans = parsed_links(&visible, &references)
        .into_iter()
        .filter(|link| {
            link.destination.is_some()
                && resolve(source, &link.href)
                    .is_some_and(|target| gone.contains(&super::normalized(&target)))
        })
        .filter_map(|link| link.whole.map(|whole| (whole, link.label)))
        .collect::<Vec<_>>();
    spans.sort_by_key(|((start, _), _)| std::cmp::Reverse(*start));
    for ((start, end), (label_start, label_end)) in spans {
        let label = markdown[label_start..label_end].to_owned();
        result.replace_range(start..end, &label);
    }
    result
}

/// One link `parsed_links` found: where its destination is (an inline link's),
/// the whole link (`[label](destination)`) and its label, as byte ranges.
struct ParsedLink {
    href: String,
    destination: Option<(usize, usize)>,
    whole: Option<(usize, usize)>,
    label: (usize, usize),
}

/// Match brackets once so malformed or deeply nested input cannot make
/// rescanning every opener quadratic. Inner links close outer `CommonMark` links.
fn parsed_links(markdown: &str, references: &BTreeMap<String, String>) -> Vec<ParsedLink> {
    let bytes = markdown.as_bytes();
    let mut stack = Vec::new();
    let mut labels = BTreeMap::new();
    for (position, byte) in bytes.iter().enumerate() {
        if !matches!(*byte, b'[' | b']') || escaped(bytes, position) {
            continue;
        }
        match byte {
            b'[' => stack.push(position),
            b']' => {
                if let Some(open) = stack.pop() {
                    labels.insert(open, position + 1);
                }
            }
            _ => {}
        }
    }
    let mut links = BTreeMap::new();
    for (&open, &end) in labels.iter().rev() {
        let label = &markdown[open + 1..end - 1];
        let mut consumed_end = None;
        let (href, span) = if bytes.get(end) == Some(&b'(') {
            match destination(&markdown[end + 1..], true) {
                Some((href, consumed, start, finish)) => {
                    consumed_end = Some(end + 1 + consumed);
                    (Some(href), Some((end + 1 + start, end + 1 + finish)))
                }
                None => (None, None),
            }
        } else if references.is_empty() {
            (None, None)
        } else {
            let key = if bytes.get(end) == Some(&b'[') {
                labels.get(&end).and_then(|reference_end| {
                    consumed_end = Some(*reference_end);
                    let reference = &markdown[end + 1..reference_end - 1];
                    bounded_reference_key(if reference.is_empty() {
                        label
                    } else {
                        reference
                    })
                })
            } else {
                bounded_reference_key(label)
            };
            (key.and_then(|key| references.get(&key).cloned()), None)
        };
        let Some(href) = href else { continue };
        let whole = (bytes.get(end) == Some(&b'('))
            .then_some(())
            .and(consumed_end)
            .map(|finish| (open, finish));
        if let Some(consumed_end) = consumed_end {
            let suffix = links
                .range(end..consumed_end)
                .map(|(&key, _)| key)
                .collect::<Vec<_>>();
            for key in suffix {
                links.remove(&key);
            }
        }
        let image = open > 0 && bytes[open - 1] == b'!' && !escaped(bytes, open - 1);
        if image {
            let inner = links
                .range(open + 1..end)
                .map(|(&key, _)| key)
                .collect::<Vec<_>>();
            for key in inner {
                links.remove(&key);
            }
        } else if links.range(open + 1..end).next().is_none() {
            links.insert(
                open,
                ParsedLink {
                    href,
                    destination: span,
                    whole,
                    label: (open + 1, end - 1),
                },
            );
        }
    }
    links.into_values().collect()
}

fn bounded_reference_key(label: &str) -> Option<String> {
    // CommonMark reference labels contain at most 999 Unicode characters.
    (label.len() <= 3996 && label.chars().take(1000).count() <= 999).then(|| reference_key(label))
}

fn reference_key(label: &str) -> String {
    label
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn escaped(bytes: &[u8], position: usize) -> bool {
    bytes[..position]
        .iter()
        .rev()
        .take_while(|b| **b == b'\\')
        .count()
        % 2
        == 1
}

fn label_at(text: &str, start: usize) -> Option<(usize, &str)> {
    let bytes = text.as_bytes();
    if bytes.get(start) != Some(&b'[') {
        return None;
    }
    let mut depth = 1_usize;
    let mut position = start + 1;
    while position < bytes.len() {
        if matches!(bytes[position], b'[' | b']') && !escaped(bytes, position) {
            match bytes[position] {
                b'[' => depth += 1,
                b']' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some((position + 1, &text[start + 1..position]));
                    }
                }
                _ => {}
            }
        }
        position += 1;
    }
    None
}

fn destination(text: &str, inline: bool) -> Option<(String, usize, usize, usize)> {
    let bytes = text.as_bytes();
    let mut position = 0;
    while bytes.get(position).is_some_and(u8::is_ascii_whitespace) {
        position += 1;
    }
    let angle = bytes.get(position) == Some(&b'<');
    let start = position + usize::from(angle);
    position = start;
    let mut depth = 0_usize;
    while let Some(&byte) = bytes.get(position) {
        if byte != b'\\' && !escaped(bytes, position) {
            if angle {
                if byte == b'>' {
                    break;
                }
                if matches!(byte, b'<' | b'\n' | b'\r') {
                    return None;
                }
            } else {
                match byte {
                    b'(' => depth += 1,
                    b')' if depth > 0 => depth -= 1,
                    b')' => break,
                    _ if byte.is_ascii_whitespace() => break,
                    _ => {}
                }
            }
        }
        position += 1;
    }
    if angle && bytes.get(position) != Some(&b'>') || depth != 0 {
        return None;
    }
    let target = text[start..position].to_owned();
    let target_end = position;
    position += usize::from(angle);
    if !inline {
        return Some((target, position, start, target_end));
    }
    let before_whitespace = position;
    while bytes.get(position).is_some_and(u8::is_ascii_whitespace) {
        position += 1;
    }
    if position > before_whitespace
        && let Some(&opener @ (b'\'' | b'"' | b'(')) = bytes.get(position)
    {
        let closer = if opener == b'(' { b')' } else { opener };
        position += 1;
        while bytes
            .get(position)
            .is_some_and(|b| *b != closer || escaped(bytes, position))
        {
            position += 1;
        }
        if bytes.get(position) != Some(&closer) {
            return None;
        }
        position += 1;
        while bytes.get(position).is_some_and(u8::is_ascii_whitespace) {
            position += 1;
        }
    }
    (bytes.get(position) == Some(&b')')).then_some((target, position + 1, start, target_end))
}

fn resolve(source: &str, destination: &str) -> Option<String> {
    let destination = destination.split(['#', '?']).next()?;
    let mut decoded = Vec::new();
    let bytes = destination.as_bytes();
    let mut position = 0;
    while position < bytes.len() {
        if bytes[position] == b'%' {
            let hex = std::str::from_utf8(bytes.get(position + 1..position + 3)?).ok()?;
            decoded.push(u8::from_str_radix(hex, 16).ok()?);
            position += 3;
        } else if bytes[position] == b'\\' {
            let next = *bytes.get(position + 1)?;
            if !next.is_ascii_punctuation() {
                return None;
            }
            decoded.push(next);
            position += 2;
        } else {
            decoded.push(bytes[position]);
            position += 1;
        }
    }
    let destination = String::from_utf8(decoded).ok()?;
    if destination.is_empty() || destination.starts_with('/') || destination.contains(['\\', ':']) {
        return None;
    }
    let mut parts = source.split('/').collect::<Vec<_>>();
    parts.pop();
    for part in destination.split('/') {
        match part {
            "." => {}
            ".." => {
                parts.pop()?;
            }
            "" => return None,
            _ => parts.push(part),
        }
    }
    let path = parts.join("/");
    super::page_path(&path).ok()
}

fn visible_markdown(markdown: &str) -> String {
    let mut visible = markdown.as_bytes().to_vec();
    let mut fence = None;
    let mut offset = 0;
    for line in markdown.split_inclusive('\n') {
        let trimmed = line.trim_start_matches(' ');
        let indentation = line.len() - trimmed.len();
        let marker = trimmed.as_bytes().first().copied().unwrap_or_default();
        let run = trimmed.bytes().take_while(|b| *b == marker).count();
        if let Some((character, count)) = fence {
            mask(&mut visible[offset..offset + line.len()]);
            if indentation <= 3
                && marker == character
                && run >= count
                && trimmed[run..].trim().is_empty()
            {
                fence = None;
            }
        } else if indentation <= 3 && matches!(marker, b'`' | b'~') && run >= 3 {
            fence = Some((marker, run));
            mask(&mut visible[offset..offset + line.len()]);
        } else if indentation >= 4 || line.starts_with('\t') {
            mask(&mut visible[offset..offset + line.len()]);
        }
        offset += line.len();
    }
    let mut position = 0;
    while position < visible.len() {
        if visible[position..].starts_with(b"<!--") {
            let end = visible[position + 4..]
                .windows(3)
                .position(|w| w == b"-->")
                .map_or(visible.len(), |n| position + 4 + n + 3);
            mask(&mut visible[position..end]);
            position = end;
        } else if visible[position] == b'`' && !escaped(&visible, position) {
            let count = visible[position..]
                .iter()
                .take_while(|b| **b == b'`')
                .count();
            let mut end = position + count;
            let mut matched = None;
            while end < visible.len() {
                if visible[end] == b'`' {
                    let run = visible[end..].iter().take_while(|b| **b == b'`').count();
                    if run == count {
                        matched = Some(end + run);
                        break;
                    }
                    end += run;
                } else {
                    end += 1;
                }
            }
            if let Some(end) = matched {
                mask(&mut visible[position..end]);
                position = end;
            } else {
                position += count;
            }
        } else {
            position += 1;
        }
    }
    String::from_utf8(visible).expect("Masking complete UTF-8 spans preserves valid text")
}

fn mask(bytes: &mut [u8]) {
    for byte in bytes {
        if !matches!(*byte, b'\n' | b'\r') {
            *byte = b' ';
        }
    }
}

#[cfg(test)]
mod regression_tests {
    use super::*;

    #[test]
    fn malformed_openers_preserve_later_links_and_move_rewrites() {
        let markdown = "An unmatched [ bracket\n\n[Target](target.md)";
        assert_eq!(targets("source.md", markdown), ["target.md"]);
        let pages = BTreeMap::from([("target.md".to_owned(), "moved/target.md".to_owned())]);
        assert_eq!(
            rewrite("source.md", "source.md", markdown, &pages),
            "An unmatched [ bracket\n\n[Target](moved/target.md)"
        );
    }

    #[test]
    fn nested_links_close_outer_links_but_plain_brackets_remain_labels() {
        assert_eq!(
            targets("source.md", "[outer [Target](target.md)](ignored.md)"),
            ["target.md"]
        );
        assert_eq!(
            targets("source.md", "[outer [Text]](target.md)"),
            ["target.md"]
        );
        let pages = BTreeMap::from([
            ("target.md".to_owned(), "moved.md".to_owned()),
            ("ignored.md".to_owned(), "wrong.md".to_owned()),
        ]);
        assert_eq!(
            rewrite(
                "source.md",
                "source.md",
                "[outer [Target](target.md)](ignored.md)",
                &pages
            ),
            "[outer [Target](moved.md)](ignored.md)"
        );
    }

    #[test]
    fn reference_images_and_link_titles_do_not_create_extra_page_links() {
        let markdown = "![Image][ref]\n\n[Target](target.md \"[ref]\")\n\n[ref]: image.md";
        assert_eq!(targets("source.md", markdown), ["target.md"]);
    }

    #[test]
    fn unlink_keeps_the_label_and_leaves_other_links_alone() {
        let markdown =
            "See [the old page](../gone.md#part) and [Kept](kept.md).\n`[Code](../gone.md)`\n";
        let gone = BTreeSet::from(["gone.md".to_owned()]);
        assert_eq!(
            unlink("notes/a.md", markdown, &gone),
            "See the old page and [Kept](kept.md).\n`[Code](../gone.md)`\n"
        );
        assert_eq!(
            targets("notes/a.md", &unlink("notes/a.md", markdown, &gone)),
            ["notes/kept.md"]
        );
    }

    #[test]
    fn large_malformed_and_nested_bracket_inputs_preserve_real_links() {
        let unmatched = format!("{}\n\n[Target](target.md)", "[".repeat(100_000));
        assert_eq!(targets("source.md", &unmatched), ["target.md"]);
        let nested = format!(
            "{}[Target](target.md){}",
            "[".repeat(50_000),
            "]".repeat(50_000)
        );
        assert_eq!(targets("source.md", &nested), ["target.md"]);
    }

    /// `SoMem` (arXiv 2611.11929): a footnote's definition holds references,
    /// not links - `[^e1]: notes.md` is not a link to the page `notes.md` -
    /// and a fragment that begins with `^` names a paragraph, which `targets`
    /// still reads as a link to the page.
    #[test]
    fn a_footnote_definition_is_not_a_link_and_a_fragment_that_names_a_paragraph_is_a_citation() {
        let markdown = "Text.[^e1] See [it](b.md#^para-one), [again](b.md#^para-one) and [x](c.md#heading).\n\n[^e1]: 2026-05; notes.md\n[^e2]: other.md\n";
        assert_eq!(targets("a.md", markdown), ["b.md", "c.md"]);
        assert_eq!(
            citations("a.md", markdown),
            [("b.md".to_owned(), "para-one".to_owned())]
        );
        assert_eq!(
            citations(
                "a.md",
                "[p](b.md#%5Eid-x) and [q](b.md#^) and `[r](b.md#^no)`"
            ),
            [("b.md".to_owned(), "id-x".to_owned())]
        );
        let pages = BTreeMap::from([("b.md".to_owned(), "moved/b.md".to_owned())]);
        assert!(
            rewrite("a.md", "a.md", markdown, &pages).contains("[it](moved/b.md#^para-one)"),
            "the fragment survives a move"
        );
        assert!(
            rewrite("a.md", "a.md", markdown, &pages).contains("[^e1]: 2026-05; notes.md"),
            "a definition is left alone"
        );
    }
}
