//! The other workspace files a read names, after agentic-file-search's third
//! phase ("backtrack"): a document that says "see `09_escrow.md`" or a module
//! that imports `./upload` points at a file the model may have skipped, and
//! the read result now names it, so the model follows the reference instead
//! of answering without it.
//!
//! Only files that exist are named: a word of the text is taken as a
//! reference when it is a path of the workspace (from the root or from the
//! read file's folder, with or without its extension, as an import writes
//! it), or the name of a workspace file with its extension
//! (`settings.toml`). A name many files share (`mod.rs`) names none of them.

use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, Mutex, OnceLock, Weak},
};

use crate::walk::Walk;

/// Most files one read names.
pub(crate) const MAX_REFERENCES: usize = 8;

/// Most files a bare file name may stand for and still name them.
const MAX_SAME_NAME: usize = 3;

/// The workspace's files by the ways a text names them, kept while the walk
/// they were made from is the current one.
struct Catalog {
    walk: Weak<Walk>,
    /// Lowercase path → the path.
    paths: HashMap<String, String>,
    /// Lowercase path without its extension → the paths.
    stems: HashMap<String, Vec<String>>,
    /// Lowercase file name → the paths.
    names: HashMap<String, Vec<String>>,
}

fn kept() -> &'static Mutex<HashMap<std::path::PathBuf, Arc<Catalog>>> {
    static KEPT: OnceLock<Mutex<HashMap<std::path::PathBuf, Arc<Catalog>>>> = OnceLock::new();
    KEPT.get_or_init(Mutex::default)
}

fn names_of(root: &Path, walk: &Arc<Walk>) -> Arc<Catalog> {
    let mut kept = kept()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(names) = kept.get(root).filter(|names| {
        names
            .walk
            .upgrade()
            .is_some_and(|current| Arc::ptr_eq(&current, walk))
    }) {
        return Arc::clone(names);
    }
    let mut names = Catalog {
        walk: Arc::downgrade(walk),
        paths: HashMap::new(),
        stems: HashMap::new(),
        names: HashMap::new(),
    };
    for file in &walk.files {
        let lower = file.relative.to_lowercase();
        let name = lower.rsplit('/').next().unwrap_or(&lower).to_owned();
        if let Some((stem, _)) = lower.rsplit_once('.')
            && !stem.ends_with('/')
            && !stem.is_empty()
        {
            names
                .stems
                .entry(stem.to_owned())
                .or_default()
                .push(file.relative.clone());
        }
        names
            .names
            .entry(name)
            .or_default()
            .push(file.relative.clone());
        names.paths.insert(lower, file.relative.clone());
    }
    let names = Arc::new(names);
    kept.insert(root.to_owned(), Arc::clone(&names));
    names
}

/// `token` joined to `folder` (both `/`-separated), with `.` and `..`
/// resolved; `None` when it climbs out of the root.
fn joined(folder: &str, token: &str) -> Option<String> {
    let mut parts = folder
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    for part in token.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part),
        }
    }
    Some(parts.join("/"))
}

/// The words of `text` that may be paths or file names: runs of letters,
/// digits and `_ - . / \`, without the punctuation a sentence puts at their
/// ends.
fn path_words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|character: char| {
        !(character.is_alphanumeric() || matches!(character, '_' | '-' | '.' | '/' | '\\'))
    })
    .map(|word| {
        word.replace('\\', "/")
            .trim_end_matches(['.', '/', '-'])
            .to_owned()
    })
    .filter(|word| {
        word.len() >= 3
            && word.chars().any(char::is_alphabetic)
            && (word.contains('/') || word.trim_start_matches('.').contains('.'))
    })
}

/// The workspace files other than `file` (root-relative, `/`-separated) that
/// `text` - what a read of it showed - names, in the order it names them, at
/// most [`MAX_REFERENCES`].
pub(crate) fn named_files(root: &Path, file: &str, text: &str) -> Vec<String> {
    let Ok(walk) = crate::workspace::tool_walk(root, None, "read_file") else {
        return Vec::new();
    };
    let names = names_of(root, &walk);
    let folder = file.rsplit_once('/').map_or("", |(folder, _)| folder);
    let mut found = Vec::<String>::new();
    let push = |path: &String, found: &mut Vec<String>| {
        if !path.eq_ignore_ascii_case(file) && !found.contains(path) {
            found.push(path.clone());
        }
    };
    for word in path_words(text) {
        if found.len() >= MAX_REFERENCES {
            break;
        }
        let lower = word.to_lowercase();
        if lower.contains('/') {
            let rooted = lower.trim_start_matches('/');
            let mut hit = false;
            for candidate in [joined(folder, &lower), joined("", rooted)]
                .into_iter()
                .flatten()
            {
                if let Some(path) = names.paths.get(&candidate) {
                    push(path, &mut found);
                    hit = true;
                } else if let Some(paths) = names
                    .stems
                    .get(&candidate)
                    .filter(|paths| paths.len() <= MAX_SAME_NAME)
                {
                    for path in paths {
                        push(path, &mut found);
                    }
                    hit = true;
                }
                if hit {
                    break;
                }
            }
        } else if let Some(paths) = names
            .names
            .get(&lower)
            .filter(|paths| paths.len() <= MAX_SAME_NAME)
        {
            for path in paths {
                push(path, &mut found);
            }
        }
    }
    found.truncate(MAX_REFERENCES);
    found
}

#[cfg(test)]
mod tests {
    use super::{joined, named_files, path_words};
    use std::fs;

    #[test]
    fn a_read_names_the_workspace_files_its_text_refers_to() {
        let directory = tempfile::tempdir().expect("scratch directory");
        let root = fs::canonicalize(directory.path()).expect("root");
        for (path, text) in [
            ("docs/01_agreement.md", "x"),
            ("docs/09_escrow_agreement.md", "x"),
            ("web/src/upload.ts", "x"),
            ("web/src/app.ts", "x"),
            ("config/settings.toml", "x"),
            ("a/mod.rs", "x"),
            ("b/mod.rs", "x"),
            ("c/mod.rs", "x"),
            ("d/mod.rs", "x"),
        ] {
            fs::create_dir_all(root.join(path).parent().expect("parent")).expect("dirs");
            fs::write(root.join(path), text).expect("file");
        }
        let text = "1: The escrow terms are in 09_Escrow_Agreement.md (Section 2).\n\
                    2: See also `../config/settings.toml`, and docs/01_agreement.md itself.\n\
                    3: Version 0.8.1, std::path::Path, mod.rs, missing.md.";
        assert_eq!(
            named_files(&root, "docs/01_agreement.md", text),
            ["docs/09_escrow_agreement.md", "config/settings.toml"]
        );
        // An import names its module without the extension, from its folder.
        let code = "1: import { prepare } from './upload';\n2: import x from \"../../config/settings.toml\";";
        assert_eq!(
            named_files(&root, "web/src/app.ts", code),
            ["web/src/upload.ts", "config/settings.toml"]
        );
        assert!(named_files(&root, "web/src/app.ts", "1: nothing here").is_empty());
    }

    #[test]
    fn words_and_joins_are_read_like_paths() {
        let words = path_words("see (a/b.rs), x.y; `c\\d.md`. 1.2 ab").collect::<Vec<_>>();
        assert_eq!(words, ["a/b.rs", "x.y", "c/d.md"]);
        assert_eq!(
            joined("web/src", "./upload").as_deref(),
            Some("web/src/upload")
        );
        assert_eq!(joined("web/src", "../../a.md").as_deref(), Some("a.md"));
        assert_eq!(joined("web", "../../a.md"), None);
    }
}
