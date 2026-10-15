//! Relative Markdown page links, without interpreting code or remote URLs.

use std::collections::{BTreeMap, BTreeSet};

pub(super) fn targets(source: &str, markdown: &str) -> Vec<String> {
    let mut visible = visible_markdown(markdown);
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
        .filter_map(|(href, _)| resolve(source, &href))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Rebase actual link destinations when pages move, leaving labels and prose intact.
pub(super) fn rewrite(
    source: &str,
    new_source: &str,
    markdown: &str,
    pages: &BTreeMap<String, String>,
) -> String {
    let mut visible = visible_markdown(markdown);
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
    for (href, span) in parsed_links(&visible, &references) {
        if let Some((start, end)) = span {
            record(0, href, start, end);
        }
    }
    let mut result = markdown.to_owned();
    for (start, (end, target)) in replacements.into_iter().rev() {
        result.replace_range(start..end, &target);
    }
    result
}

/// Match brackets once so malformed or deeply nested input cannot make
/// rescanning every opener quadratic. Inner links close outer `CommonMark` links.
fn parsed_links(
    markdown: &str,
    references: &BTreeMap<String, String>,
) -> Vec<(String, Option<(usize, usize)>)> {
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
            links.insert(open, (href, span));
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
}
