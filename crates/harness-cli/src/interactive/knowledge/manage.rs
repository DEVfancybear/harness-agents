//! Console wiki management, sharing the native store's lock and journal.

use super::{
    BTreeMap, BTreeSet, CATEGORY_MARKER, Change, INDEX, MAX_PAGE, MAX_PAGES, MAX_SCAN_BYTES, META,
    Metadata, Page, Path, Store, add_log, atomic_write, change, error, find_page, fs, hash,
    is_link, links, normalized, page_path, parse_page, read_bounded, render_index, require_enabled,
    resolve_page_links, safe_file, scan, single_line, transact,
};
use serde_json::{Value, json};

impl Store {
    #[allow(
        clippy::too_many_lines,
        reason = "Management actions share one lock and atomic commit boundary"
    )]
    pub(crate) fn manage(&self, request: &Value) -> Result<Value, String> {
        self.with_lock(|root| {
            require_enabled(root)?;
            let action = string(request, "action")?;
            if action == "categories" {
                return Ok(json!({"categories": categories(root)?}));
            }
            let before = scan(root)?;
            let mut pages = before.clone();
            let mut extra = Vec::new();
            let mut skipped = Vec::new();
            match action {
                "create_category" => {
                    let path = category(request)?;
                    if categories(root)?
                        .iter()
                        .any(|p| normalized(p) == normalized(&path))
                    {
                        return Err("knowledge category already exists".to_owned());
                    }
                    extra.push(change(root, &marker(&path), Some("category\n".to_owned()))?);
                }
                "rename_category" => {
                    let from = existing_category(root, &category(request)?)?;
                    let to = category_path(string(request, "new_path")?)?;
                    if within(&to, &from)
                        || within(&from, &to)
                        || categories(root)?
                            .iter()
                            .any(|p| normalized(p) == normalized(&to))
                    {
                        return Err(
                            "knowledge category destination overlaps an existing category"
                                .to_owned(),
                        );
                    }
                    let directories = checked_tree(root, &from)?;
                    let mapping = page_mapping(&pages, |path| {
                        within(path, &from).then(|| format!("{to}{}", &path[from.len()..]))
                    });
                    check_moved_hashes(request, &pages, &mapping)?;
                    pages = moved_pages(&pages, &mapping)?;
                    category_markers(root, &directories, Some((&from, &to)), &mut extra)?;
                }
                "delete_category" => {
                    if request.get("confirm").and_then(Value::as_bool) != Some(true) {
                        return Err("category deletion requires explicit confirmation".to_owned());
                    }
                    let path = existing_category(root, &category(request)?)?;
                    let directories = checked_tree(root, &path)?;
                    let selected = pages
                        .iter()
                        .filter(|p| within(&p.summary.path, &path))
                        .map(|p| p.summary.path.clone())
                        .collect::<BTreeSet<_>>();
                    delete_selected(request, &mut pages, &selected)?;
                    category_markers(root, &directories, None, &mut extra)?;
                }
                "create_document" | "update_document" => {
                    let path = page_path(string(request, "path")?)?;
                    let existing = pages
                        .iter()
                        .position(|p| normalized(&p.summary.path) == normalized(&path));
                    if action == "create_document" && existing.is_some() {
                        return Err("knowledge document already exists".to_owned());
                    }
                    if action == "update_document" && existing.is_none() {
                        return Err("knowledge document not found".to_owned());
                    }
                    let mut content = string(request, "content")?.to_owned();
                    let actual =
                        existing.map_or_else(|| path.clone(), |i| pages[i].summary.path.clone());
                    if let Some(i) = existing {
                        check_hash(request, &pages[i])?;
                        preserve_metadata(&pages[i].text, &mut content);
                    } else if !content.lines().take(20).any(|line| line.starts_with("# "))
                        && let Some(title) = request.get("title").and_then(Value::as_str)
                    {
                        content = format!("# {}\n\n{content}", single_line(title, 256, "title")?);
                    }
                    let page = bounded_page(&actual, content)?;
                    if let Some(i) = existing {
                        pages[i] = page;
                    } else {
                        pages.push(page);
                    }
                }
                "delete_documents" => {
                    let selected = selected_paths(request, &pages)?;
                    delete_selected(request, &mut pages, &selected)?;
                }
                "move_documents" => {
                    let selected = selected_paths(request, &pages)?;
                    let to = request
                        .get("target_category")
                        .or_else(|| request.get("destination"))
                        .and_then(Value::as_str)
                        .ok_or("target_category is required")?;
                    let to = if to.is_empty() {
                        String::new()
                    } else {
                        existing_category(root, &category_path(to)?)?
                    };
                    let mapping = page_mapping(&pages, |path| {
                        selected.contains(path).then(|| {
                            let name = path.rsplit('/').next().unwrap_or(path);
                            if to.is_empty() {
                                name.to_owned()
                            } else {
                                format!("{to}/{name}")
                            }
                        })
                    });
                    for page in &pages {
                        if selected.contains(&page.summary.path) {
                            check_hash(request, page)?;
                        }
                    }
                    pages = moved_pages(&pages, &mapping)?;
                }
                "import_documents" => {
                    import(request, &mut pages, &mut skipped)?;
                }
                _ => return Err("unsupported knowledge management action".to_owned()),
            }
            let changed = commit(root, &before, &mut pages, extra, action)?;
            Ok(json!({"ok": true, "changed": changed, "skipped": skipped}))
        })
    }
}

fn string<'a>(request: &'a Value, key: &str) -> Result<&'a str, String> {
    request
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{key} is required"))
}

fn category_path(path: &str) -> Result<String, String> {
    page_path(&format!("{path}/category-check.md"))?;
    Ok(path.to_owned())
}

fn category(request: &Value) -> Result<String, String> {
    category_path(
        request
            .get("path")
            .or_else(|| request.get("category"))
            .and_then(Value::as_str)
            .ok_or("category path is required")?,
    )
}

fn marker(path: &str) -> String {
    format!("{path}/{CATEGORY_MARKER}")
}

fn within(path: &str, category: &str) -> bool {
    let path = normalized(path);
    let category = normalized(category);
    path == category
        || path
            .strip_prefix(&category)
            .is_some_and(|tail| tail.starts_with('/'))
}

fn categories(root: &Path) -> Result<Vec<String>, String> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut queue = vec![root.to_path_buf()];
    let mut found = BTreeMap::new();
    let mut entries = 0;
    while let Some(directory) = queue.pop() {
        for entry in fs::read_dir(directory).map_err(error)? {
            entries += 1;
            if entries > 8192 {
                return Err("knowledge directory entry limit exceeded".to_owned());
            }
            let entry = entry.map_err(error)?;
            let metadata = fs::symlink_metadata(entry.path()).map_err(error)?;
            if is_link(&metadata) || !metadata.is_dir() {
                continue;
            }
            let relative = entry
                .path()
                .strip_prefix(root)
                .map_err(error)?
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            if category_path(&relative).is_err() {
                continue;
            }
            if found.insert(normalized(&relative), relative).is_some() {
                return Err(
                    "knowledge categories differ only by case or Unicode normalization".to_owned(),
                );
            }
            queue.push(entry.path());
        }
    }
    Ok(found.into_values().collect())
}

fn existing_category(root: &Path, path: &str) -> Result<String, String> {
    categories(root)?
        .into_iter()
        .find(|p| normalized(p) == normalized(path))
        .ok_or_else(|| format!("knowledge category not found: {path}"))
}

fn checked_tree(root: &Path, path: &str) -> Result<Vec<String>, String> {
    let mut directories = categories(root)?
        .into_iter()
        .filter(|p| within(p, path))
        .collect::<Vec<_>>();
    for directory in &directories {
        for entry in fs::read_dir(root.join(directory)).map_err(error)? {
            let entry = entry.map_err(error)?;
            let metadata = fs::symlink_metadata(entry.path()).map_err(error)?;
            if is_link(&metadata) {
                return Err("knowledge category contains a symlink or reparse point".to_owned());
            }
            if metadata.is_dir() {
                let relative = format!("{directory}/{}", entry.file_name().to_string_lossy());
                if !directories.contains(&relative) {
                    return Err("knowledge category contains an unsupported directory".to_owned());
                }
            } else if !metadata.is_file() {
                return Err("knowledge category contains unsupported entries".to_owned());
            } else if entry.file_name() != CATEGORY_MARKER {
                page_path(&format!(
                    "{directory}/{}",
                    entry.file_name().to_string_lossy()
                ))
                .map_err(|_| "knowledge category contains unsupported files".to_owned())?;
            }
        }
    }
    // Delete deepest markers last in the transaction's file-change list.
    directories.sort_by_key(|p| std::cmp::Reverse(p.split('/').count()));
    Ok(directories)
}

fn category_markers(
    root: &Path,
    directories: &[String],
    renamed: Option<(&str, &str)>,
    changes: &mut Vec<Change>,
) -> Result<(), String> {
    for directory in directories {
        let old = marker(directory);
        if read_bounded(&safe_file(root, &old)?, 64)?.is_none() {
            // Adopt external empty directories so removal is recoverable too.
            atomic_write(root, &old, "category\n")?;
        }
        changes.push(change(root, &old, None)?);
        if let Some((from, to)) = renamed {
            changes.push(change(
                root,
                &marker(&format!("{to}{}", &directory[from.len()..])),
                Some("category\n".to_owned()),
            )?);
        }
    }
    Ok(())
}

fn check_hash(request: &Value, page: &Page) -> Result<(), String> {
    let expected = request
        .get("expected_hashes")
        .and_then(|hashes| {
            hashes
                .as_object()?
                .iter()
                .find(|(path, _)| normalized(path) == normalized(&page.summary.path))?
                .1
                .as_str()
        })
        .or_else(|| request.get("expected_hash").and_then(Value::as_str));
    if expected != Some(page.summary.hash.as_str()) {
        return Err(format!(
            "knowledge document changed; read {} again and supply its current hash",
            page.summary.path
        ));
    }
    Ok(())
}

fn selected_paths(request: &Value, pages: &[Page]) -> Result<BTreeSet<String>, String> {
    let paths = request
        .get("paths")
        .and_then(Value::as_array)
        .ok_or("paths are required")?;
    if paths.is_empty() || paths.len() > MAX_PAGES {
        return Err("invalid knowledge document selection".to_owned());
    }
    paths
        .iter()
        .map(|path| {
            let path = page_path(path.as_str().ok_or("document paths must be strings")?)?;
            Ok(find_page(pages, &path)?.summary.path.clone())
        })
        .collect()
}

fn delete_selected(
    request: &Value,
    pages: &mut Vec<Page>,
    selected: &BTreeSet<String>,
) -> Result<(), String> {
    for page in pages.iter() {
        if selected.contains(&page.summary.path) {
            check_hash(request, page)?;
        } else if page
            .summary
            .related
            .iter()
            .any(|target| selected.contains(target))
        {
            return Err(format!(
                "knowledge documents have backlinks from {}; update referring pages before deleting",
                page.summary.path
            ));
        }
    }
    pages.retain(|p| !selected.contains(&p.summary.path));
    Ok(())
}

fn bounded_page(path: &str, content: String) -> Result<Page, String> {
    if content.len() > MAX_PAGE {
        return Err("knowledge document exceeds 256 KiB".to_owned());
    }
    parse_page(path, content)
}

fn preserve_metadata(previous: &str, content: &mut String) {
    if !content.starts_with(META)
        && let Some(line) = previous
            .lines()
            .next()
            .filter(|line| line.starts_with(META))
    {
        *content = format!("{line}\n{content}");
    }
}

fn page_mapping(
    pages: &[Page],
    moved: impl Fn(&str) -> Option<String>,
) -> BTreeMap<String, String> {
    pages
        .iter()
        .map(|p| {
            (
                normalized(&p.summary.path),
                moved(&p.summary.path).unwrap_or_else(|| p.summary.path.clone()),
            )
        })
        .collect()
}

fn check_moved_hashes(
    request: &Value,
    pages: &[Page],
    mapping: &BTreeMap<String, String>,
) -> Result<(), String> {
    for page in pages {
        if mapping
            .get(&normalized(&page.summary.path))
            .is_some_and(|target| *target != page.summary.path)
        {
            check_hash(request, page)?;
        }
    }
    Ok(())
}

fn moved_pages(pages: &[Page], mapping: &BTreeMap<String, String>) -> Result<Vec<Page>, String> {
    let mut destinations = BTreeSet::new();
    for target in mapping.values() {
        page_path(target)?;
        if !destinations.insert(normalized(target)) {
            return Err("knowledge destination document already exists".to_owned());
        }
    }
    pages
        .iter()
        .map(|page| {
            let path = mapping
                .get(&normalized(&page.summary.path))
                .ok_or("missing document move mapping")?;
            let mut text = links::rewrite(&page.summary.path, path, &page.text, mapping);
            if let Some(first) = text.lines().next().and_then(|line| line.strip_prefix(META)) {
                let mut metadata: Metadata = serde_json::from_str(
                    first
                        .strip_suffix(" -->")
                        .ok_or("invalid knowledge metadata")?,
                )
                .map_err(error)?;
                for related in &mut metadata.related {
                    if let Some(target) = mapping.get(&normalized(related)) {
                        related.clone_from(target);
                    }
                }
                let rest = text.split_once('\n').map_or("", |(_, rest)| rest);
                text = format!(
                    "{META}{} -->\n{rest}",
                    serde_json::to_string(&metadata)
                        .map_err(error)?
                        .replace("-->", "\\u002d\\u002d\\u003e")
                );
            }
            bounded_page(path, text)
        })
        .collect()
}

fn import(request: &Value, pages: &mut Vec<Page>, skipped: &mut Vec<String>) -> Result<(), String> {
    let documents = request
        .get("files")
        .or_else(|| request.get("documents"))
        .and_then(Value::as_array)
        .ok_or("files are required")?;
    if documents.is_empty() || documents.len() > MAX_PAGES {
        return Err("invalid knowledge import count".to_owned());
    }
    let category = request
        .get("category")
        .and_then(Value::as_str)
        .unwrap_or("");
    if !category.is_empty() {
        category_path(category)?;
    }
    let conflict = request
        .get("on_conflict")
        .or_else(|| request.get("conflict"))
        .and_then(Value::as_str)
        .unwrap_or("skip");
    if !["skip", "rename", "overwrite"].contains(&conflict) {
        return Err("invalid knowledge import conflict mode".to_owned());
    }
    for document in documents {
        let name = document
            .get("name")
            .or_else(|| document.get("path"))
            .and_then(Value::as_str)
            .ok_or("import filename is required")?;
        if request.get("files").is_some() && name.contains('/') {
            return Err("import filenames cannot contain directories".to_owned());
        }
        let mut path = page_path(&if category.is_empty() {
            name.to_owned()
        } else {
            format!("{category}/{name}")
        })?;
        let existing = pages
            .iter()
            .position(|p| normalized(&p.summary.path) == normalized(&path));
        if let Some(index) = existing {
            match conflict {
                "skip" => {
                    skipped.push(path);
                    continue;
                }
                "rename" => {
                    let (stem, extension) = path
                        .rsplit_once('.')
                        .ok_or("import extension is required")?;
                    let (stem, extension) = (stem.to_owned(), extension.to_owned());
                    let mut number = 1;
                    loop {
                        path = page_path(&format!("{stem}_{number}.{extension}"))?;
                        if !pages
                            .iter()
                            .any(|p| normalized(&p.summary.path) == normalized(&path))
                        {
                            break;
                        }
                        number += 1;
                        if number > MAX_PAGES {
                            return Err("knowledge import rename limit reached".to_owned());
                        }
                    }
                }
                "overwrite" => {
                    check_hash(request, &pages[index])?;
                    path.clone_from(&pages[index].summary.path);
                    pages[index] = bounded_page(&path, string(document, "content")?.to_owned())?;
                    continue;
                }
                _ => unreachable!(),
            }
        }
        pages.push(bounded_page(
            &path,
            string(document, "content")?.to_owned(),
        )?);
    }
    Ok(())
}

fn commit(
    root: &Path,
    before: &[Page],
    pages: &mut [Page],
    extra: Vec<Change>,
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
    if admitted.len() != pages.len() {
        return Err("knowledge document paths collide".to_owned());
    }
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
    let paths = previous
        .keys()
        .chain(next.keys())
        .copied()
        .collect::<BTreeSet<_>>();
    for path in paths {
        let old = previous.get(path).map(|p| p.summary.hash.clone());
        let new = next.get(path).map(|p| p.text.clone());
        if old != new.as_deref().map(hash) {
            changes.push(Change {
                path: path.to_owned(),
                before: old,
                after: new,
            });
        }
    }
    changes.extend(extra);
    let affected = changes
        .iter()
        .filter(|c| c.before != c.after.as_deref().map(hash))
        .map(|c| c.path.clone())
        .collect::<Vec<_>>();
    if affected.is_empty() {
        return Ok(affected);
    }
    changes.push(change(root, INDEX, Some(render_index(pages)?))?);
    add_log(
        root,
        &mut changes,
        action,
        &affected.join(", "),
        &hash(&affected.join("\n")),
        None,
    )?;
    transact(root, changes)?;
    Ok(affected)
}

#[cfg(test)]
mod tests {
    use super::super::{LOG, PENDING, render_page};
    use super::*;

    fn store() -> (tempfile::TempDir, Store) {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::new(directory.path().join("knowledge"));
        (directory, store)
    }

    fn hashes(store: &Store) -> Value {
        Value::Object(
            store
                .list()
                .unwrap()
                .into_iter()
                .map(|page| (page.path, Value::String(page.hash)))
                .collect(),
        )
    }

    fn create(store: &Store, path: &str, content: &str) {
        store
            .manage(&json!({"action":"create_document","path":path,"content":content}))
            .unwrap();
    }

    #[test]
    fn console_crud_preserves_metadata_and_rejects_stale_updates() {
        let (_dir, store) = store();
        create(&store, "reference.md", "# Reference\nFact");
        let hash = store.document("reference.md").unwrap().page.hash;
        assert!(store.manage(&json!({"action":"update_document","path":"reference.md","content":"changed","expected_hash":"stale"})).is_err());
        store.manage(&json!({"action":"update_document","path":"reference.md","content":"# Updated\nNew fact","expected_hash":hash})).unwrap();
        assert_eq!(
            store.document("reference.md").unwrap().page.title,
            "Updated"
        );
        let mut metadata = Metadata::default();
        metadata
            .sources
            .push("https://example.com/provenance".to_owned());
        let original = render_page("native.md", &metadata, "original").unwrap();
        create(&store, "native.md", &original);
        let hash = store.document("native.md").unwrap().page.hash;
        store.manage(&json!({"action":"update_document","path":"native.md","content":"# Native\nEdited","expected_hash":hash})).unwrap();
        assert_eq!(
            store.document("native.md").unwrap().page.sources,
            ["https://example.com/provenance"]
        );
        assert!(
            store
                .manage(
                    &json!({"action":"create_document","path":"../outside.md","content":"escape"})
                )
                .is_err()
        );
    }

    #[test]
    fn category_rename_and_document_move_preserve_links_and_empty_categories() {
        let (_dir, store) = store();
        store
            .manage(&json!({"action":"create_category","path":"old/empty"}))
            .unwrap();
        create(&store, "old/target.md", "# Target\n[Other](../other.md)");
        create(
            &store,
            "other.md",
            "# Other\n[Target](old/target.md#anchor \"title\")",
        );
        store.manage(&json!({"action":"rename_category","path":"old","new_path":"renamed","expected_hashes":hashes(&store)})).unwrap();
        assert!(!store.root.join("old").exists());
        assert!(store.root.join("renamed/empty").is_dir());
        assert!(
            store
                .document("other.md")
                .unwrap()
                .content
                .contains("renamed/target.md#anchor \"title\"")
        );
        assert_eq!(
            store.document("renamed/target.md").unwrap().backlinks,
            ["other.md"]
        );
        store
            .manage(&json!({"action":"create_category","path":"moved"}))
            .unwrap();
        store.manage(&json!({"action":"move_documents","paths":["renamed/target.md"],"target_category":"moved","expected_hashes":hashes(&store)})).unwrap();
        assert!(
            store
                .document("other.md")
                .unwrap()
                .content
                .contains("moved/target.md#anchor")
        );
        assert!(
            store
                .document("moved/target.md")
                .unwrap()
                .content
                .contains("../other.md")
        );
        assert_eq!(store.graph().unwrap().edges.len(), 2);
    }

    #[test]
    fn bulk_delete_allows_internal_links_but_refuses_surviving_backlinks() {
        let (_dir, store) = store();
        create(&store, "a.md", "# A\n[B](b.md)");
        create(&store, "b.md", "# B\n[A](a.md)");
        let before = hashes(&store);
        assert!(
            store
                .manage(
                    &json!({"action":"delete_documents","paths":["a.md"],"expected_hashes":before})
                )
                .is_err()
        );
        assert_eq!(store.list().unwrap().len(), 2);
        store.manage(&json!({"action":"delete_documents","paths":["a.md","b.md"],"expected_hashes":before})).unwrap();
        assert!(store.list().unwrap().is_empty());
    }

    #[test]
    fn category_deletion_requires_confirmation_and_preserves_unrecognized_files() {
        let (_dir, store) = store();
        store
            .manage(&json!({"action":"create_category","path":"category/empty"}))
            .unwrap();
        create(&store, "category/page.md", "# Page");
        let request =
            json!({"action":"delete_category","path":"category","expected_hashes":hashes(&store)});
        assert!(store.manage(&request).is_err());
        fs::write(store.root.join("category/keep.bin"), "unrecognized").unwrap();
        let mut request = request;
        request["confirm"] = json!(true);
        assert!(store.manage(&request).is_err());
        assert!(store.root.join("category/page.md").exists());
        fs::remove_file(store.root.join("category/keep.bin")).unwrap();
        store.manage(&request).unwrap();
        assert!(!store.root.join("category").exists());
    }

    #[test]
    fn import_modes_support_markdown_and_text_and_reject_unsafe_names() {
        let (_dir, store) = store();
        let mut request = json!({"action":"import_documents","category":"imports","files":[{"name":"a.txt","content":"plain fact"},{"name":"b.md","content":"# B\nReference"}],"on_conflict":"skip"});
        store.manage(&request).unwrap();
        assert_eq!(store.list().unwrap().len(), 2);
        assert_eq!(
            store.manage(&request).unwrap()["skipped"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        request["on_conflict"] = json!("rename");
        store.manage(&request).unwrap();
        assert!(store.document("imports/a_1.txt").is_ok());
        request["on_conflict"] = json!("overwrite");
        request["files"][0]["content"] = json!("changed plain fact");
        assert!(store.manage(&request).is_err());
        request["expected_hashes"] = hashes(&store);
        store.manage(&request).unwrap();
        assert_eq!(
            store.document("imports/a.txt").unwrap().content,
            "changed plain fact"
        );
        assert!(store.manage(&json!({"action":"import_documents","files":[{"name":"../escape.md","content":"escape"}]})).is_err());
        store.set_enabled(false).unwrap();
        assert!(store.manage(&json!({"action":"categories"})).is_err());
    }

    #[test]
    fn multi_document_transactions_recover_without_duplicate_history() {
        let (_dir, store) = store();
        let files = (0..12)
            .map(|n| json!({"name":format!("p{n}.md"),"content":format!("# Page {n}")}))
            .collect::<Vec<_>>();
        store
            .manage(&json!({"action":"import_documents","files":files}))
            .unwrap();
        assert_eq!(store.list().unwrap().len(), 12);
        assert_eq!(
            fs::read_to_string(store.root.join(LOG))
                .unwrap()
                .lines()
                .count(),
            1
        );
        assert!(!store.root.join(PENDING).exists());
        store.refresh().unwrap();
        assert_eq!(
            fs::read_to_string(store.root.join(LOG))
                .unwrap()
                .lines()
                .count(),
            1
        );
    }

    #[test]
    fn destination_conflicts_do_not_leave_an_unrecoverable_journal() {
        let (_dir, store) = store();
        create(&store, "existing.md", "# Existing\nKeep this page");
        store
            .manage(&json!({"action":"create_category","path":"folder.md"}))
            .unwrap();
        let requests = [
            json!({"action":"create_document","path":"folder.md","content":"# Collision"}),
            json!({"action":"create_document","path":"existing.md/child.md","content":"# Collision"}),
            json!({"action":"import_documents","documents":[{"path":"valid.md","content":"# Valid"},{"path":"folder.md","content":"# Collision"}]}),
            json!({"action":"import_documents","documents":[{"path":"new.md","content":"# Parent"},{"path":"new.md/child.md","content":"# Child"}]}),
        ];
        for request in requests {
            assert!(store.manage(&request).is_err());
            assert!(!store.root.join(PENDING).exists());
            assert_eq!(store.list().unwrap().len(), 1);
            assert!(
                store
                    .document("existing.md")
                    .unwrap()
                    .content
                    .contains("Keep this page")
            );
            assert!(!store.root.join("valid.md").exists());
        }
    }

    #[test]
    fn move_to_a_directory_destination_keeps_original_pages_readable() {
        let (_dir, store) = store();
        create(&store, "source.md", "# Source\nKeep this page");
        store
            .manage(&json!({"action":"create_category","path":"destination/source.md"}))
            .unwrap();
        assert!(
            store
                .manage(&json!({"action":"move_documents","paths":["source.md"],"destination":"destination","expected_hashes":hashes(&store)}))
                .is_err()
        );
        assert!(!store.root.join(PENDING).exists());
        assert_eq!(store.list().unwrap().len(), 1);
        assert!(store.document("source.md").is_ok());
        assert!(store.root.join("destination/source.md").is_dir());
    }

    #[test]
    fn imports_cannot_exceed_the_filesystem_entry_budget() {
        let (_dir, store) = store();
        create(&store, "existing.md", "# Existing");
        let documents = (0..MAX_PAGES - 1)
            .map(
                |index| json!({"path":format!("category{index}/a/b/c/page.md"),"content":"# Page"}),
            )
            .collect::<Vec<_>>();
        let error = store
            .manage(&json!({"action":"import_documents","documents":documents}))
            .unwrap_err();
        assert!(error.contains("directory entry limit"));
        assert!(!store.root.join(PENDING).exists());
        assert!(!store.root.join("category0").exists());
        assert_eq!(store.list().unwrap().len(), 1);
        create(&store, "next.md", "# Next");
        assert_eq!(store.list().unwrap().len(), 2);
    }

    #[test]
    fn graph_bounds_allocated_edges_and_preserves_total_count() {
        let (_dir, store) = store();
        create(&store, "a.md", "# A\n[B](b.md)\n[C](c.md)");
        create(&store, "b.md", "# B\n[C](c.md)");
        create(&store, "c.md", "# C");
        let (graph, total_edges) = store.graph_bounded(1).unwrap();
        assert_eq!(graph.nodes.len(), 3);
        assert_eq!(graph.edges.len(), 1);
        assert_eq!(total_edges, 3);
        let (graph, total_edges) = store.graph_bounded(0).unwrap();
        assert!(graph.edges.is_empty());
        assert_eq!(total_edges, 3);
        assert_eq!(store.graph().unwrap().edges.len(), 3);
    }
}
