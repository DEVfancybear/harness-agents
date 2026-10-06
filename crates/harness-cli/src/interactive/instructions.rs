use std::collections::BTreeSet;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use harness_session::{ContextBlock, ContextBlockKind};
use harness_tools::is_sensitive_workspace_path;
use harness_types::SourceAuthority;

const MAX_INSTRUCTION_BYTES: usize = 32 * 1024;

/// What the instructions of directories below the working directory may add to
/// one conversation, all together: they arrive as the agent reaches them.
const MAX_NESTED_BYTES: usize = 16 * 1024;

/// The longest entry file the course's "map, not a manual" advice allows before
/// `ha doctor` suggests splitting it into linked documents.
pub const ENTRY_FILE_MAX_LINES: usize = 200;

/// The instruction files of one directory, in the order they apply: the shared
/// `AGENTS.md` (or `CLAUDE.md`), then the personal `AGENTS.local.md` (or
/// `CLAUDE.local.md`), whose rules come later and so win.
fn directory_files(directory: &Path, with_local: bool) -> Vec<(PathBuf, bool)> {
    let mut files = Vec::new();
    for (preferred, legacy) in [
        ("AGENTS.md", "CLAUDE.md"),
        ("AGENTS.local.md", "CLAUDE.local.md"),
    ]
    .into_iter()
    .take(if with_local { 2 } else { 1 })
    {
        let preferred = directory.join(preferred);
        let legacy = directory.join(legacy);
        if preferred.is_file() {
            files.push((preferred, false));
        } else if legacy.is_file() {
            files.push((legacy, true));
        }
    }
    files
}

/// Whether `path`, an instruction file inside the project, may be read: not a
/// protected path, and not a link out of the project.
fn admissible(path: &Path, project_root: &Path, source: &str) -> Result<(), String> {
    let Ok(relative) = path.strip_prefix(project_root) else {
        return Err("ignored instruction file outside project root".to_owned());
    };
    if is_sensitive_workspace_path(relative) {
        return Err(format!("ignored protected instruction file {source}"));
    }
    let Ok(canonical_root) = project_root.canonicalize() else {
        return Err(format!(
            "ignored instruction file because project root could not be verified: {source}"
        ));
    };
    let Ok(canonical_file) = path.canonicalize() else {
        return Err(format!(
            "ignored instruction file because its path could not be verified: {source}"
        ));
    };
    let Ok(canonical_relative) = canonical_file.strip_prefix(&canonical_root) else {
        return Err(format!(
            "ignored instruction file outside project root: {source}"
        ));
    };
    if is_sensitive_workspace_path(canonical_relative) {
        return Err(format!(
            "ignored protected instruction file {source} (resolved path)"
        ));
    }
    Ok(())
}

/// Read at most `limit` bytes of UTF-8 text, cut on a character boundary.
/// Returns the text and whether it was cut.
fn read_bounded(path: &Path, limit: usize, source: &str) -> Result<(String, bool), String> {
    let read_limit = limit.saturating_add(1);
    let mut bytes = Vec::with_capacity(read_limit.min(4096));
    File::open(path)
        .and_then(|file| {
            file.take(u64::try_from(read_limit).unwrap_or(u64::MAX))
                .read_to_end(&mut bytes)
        })
        .map_err(|_| format!("could not read instruction file {source}"))?;
    let truncated = bytes.len() > limit;
    if truncated {
        bytes.truncate(limit);
        while std::str::from_utf8(&bytes).is_err() && !bytes.is_empty() {
            bytes.pop();
        }
    }
    String::from_utf8(bytes)
        .map(|text| (text, truncated))
        .map_err(|_| format!("ignored non-UTF-8 instruction file {source}"))
}

#[derive(Clone, Debug, Default)]
pub struct InstructionLoad {
    pub blocks: Vec<ContextBlock>,
    pub files: Vec<PathBuf>,
    pub notices: Vec<String>,
}

#[must_use]
#[allow(clippy::too_many_lines)] // The chain order and cap are easiest to audit in one pass.
pub fn load(global_config_dir: &Path, project_root: &Path, cwd: &Path) -> InstructionLoad {
    let mut result = InstructionLoad::default();
    let mut remaining = MAX_INSTRUCTION_BYTES;
    let mut candidates = vec![(global_config_dir.to_path_buf(), true)];

    let mut directories = Vec::new();
    let mut current = cwd.to_path_buf();
    loop {
        directories.push(current.clone());
        if current == project_root || !current.starts_with(project_root) {
            break;
        }
        let Some(parent) = current.parent() else {
            break;
        };
        current = parent.to_path_buf();
    }
    directories.reverse();
    if directories.first().is_none_or(|path| path != project_root) {
        directories.insert(0, project_root.to_path_buf());
    }
    candidates.extend(directories.into_iter().map(|directory| (directory, false)));

    'directories: for (directory, global) in candidates {
        for (path, legacy) in directory_files(&directory, !global) {
            let source = display_source(&path, project_root, global);
            if !global && let Err(notice) = admissible(&path, project_root, &source) {
                result.notices.push(notice);
                continue;
            }
            let (text, truncated) = match read_bounded(&path, remaining, &source) {
                Ok(read) => read,
                Err(notice) => {
                    result.notices.push(notice);
                    continue;
                }
            };
            let block = ContextBlock::mandatory(
                format!("project-rule:{source}"),
                ContextBlockKind::ProjectRule,
                text,
            )
            .with_authority(SourceAuthority::User);
            remaining = remaining.saturating_sub(block.text.len());
            result.blocks.push(block);
            result.files.push(path);
            if legacy {
                result.notices.push(format!(
                    "using {} fallback for {source}; AGENTS{} takes precedence",
                    if source.ends_with(".local.md") {
                        "CLAUDE.local.md"
                    } else {
                        "CLAUDE.md"
                    },
                    if source.ends_with(".local.md") {
                        ".local.md"
                    } else {
                        ".md"
                    }
                ));
            }
            if truncated {
                result.notices.push(format!(
                    "instruction files truncated at the {MAX_INSTRUCTION_BYTES}-byte total limit"
                ));
                break 'directories;
            }
        }
    }
    result
}

/// The directories from the project root down to `cwd`: the ones [`load`]
/// already put in every turn.
fn chain(project_root: &Path, cwd: &Path) -> Vec<PathBuf> {
    let mut directories = Vec::new();
    let mut current = cwd.to_path_buf();
    loop {
        directories.push(current.clone());
        if current == project_root || !current.starts_with(project_root) {
            break;
        }
        let Some(parent) = current.parent() else {
            break;
        };
        current = parent.to_path_buf();
    }
    directories
}

/// The instructions of directories the agent reaches below its working
/// directory - an `AGENTS.md` next to the code it reads or edits - added to that
/// call's result the first time a conversation reaches them, as Claude Code
/// loads a subdirectory's `CLAUDE.md` when it reads files there.
pub struct NestedInstructions {
    project_root: PathBuf,
    conversation: String,
    /// `(conversation, directory)` pairs already handled, shared by the turns
    /// of a session.
    seen: Arc<Mutex<BTreeSet<(String, PathBuf)>>>,
    /// What this turn may still add.
    remaining: Mutex<usize>,
}

impl NestedInstructions {
    #[must_use]
    pub fn new(
        project_root: &Path,
        cwd: &Path,
        conversation: &str,
        seen: Arc<Mutex<BTreeSet<(String, PathBuf)>>>,
    ) -> Self {
        // What every turn loads already is never added again.
        if let Ok(mut seen) = seen.lock() {
            for directory in chain(project_root, cwd) {
                seen.insert((conversation.to_owned(), directory));
            }
        }
        Self {
            project_root: project_root.to_path_buf(),
            conversation: conversation.to_owned(),
            seen,
            remaining: Mutex::new(MAX_NESTED_BYTES),
        }
    }

    /// The directory a call reached, inside the project.
    fn reached(&self, action: &harness_tools::CodingToolAction) -> Option<PathBuf> {
        use harness_tools::CodingToolAction as Action;
        let hint = action.path_hint()?;
        let path = self.project_root.join(hint);
        let directory = match action {
            Action::ReadFile { .. }
            | Action::ApplyPatch { .. }
            | Action::WriteFile { .. }
            | Action::EditFile { .. } => path.parent()?.to_path_buf(),
            _ if path.is_dir() => path,
            _ => path.parent()?.to_path_buf(),
        };
        directory
            .starts_with(&self.project_root)
            .then_some(directory)
    }
}

impl harness_tools::ToolResultContext for NestedInstructions {
    fn context_for(&self, action: &harness_tools::CodingToolAction) -> Vec<String> {
        let Some(reached) = self.reached(action) else {
            return Vec::new();
        };
        // The unseen directories from the root down to the one reached.
        let mut directories = Vec::new();
        let mut current = reached;
        while current.starts_with(&self.project_root) {
            directories.push(current.clone());
            if current == self.project_root {
                break;
            }
            let Some(parent) = current.parent() else {
                break;
            };
            current = parent.to_path_buf();
        }
        directories.reverse();
        let Ok(mut seen) = self.seen.lock() else {
            return Vec::new();
        };
        let Ok(mut remaining) = self.remaining.lock() else {
            return Vec::new();
        };
        let mut added = Vec::new();
        for directory in directories {
            if !seen.insert((self.conversation.clone(), directory.clone())) {
                continue;
            }
            for (path, _) in directory_files(&directory, true) {
                let source = display_source(&path, &self.project_root, false);
                if admissible(&path, &self.project_root, &source).is_err() || *remaining == 0 {
                    continue;
                }
                let Ok((text, truncated)) = read_bounded(&path, *remaining, &source) else {
                    continue;
                };
                *remaining = remaining.saturating_sub(text.len());
                let scope = source
                    .rsplit_once('/')
                    .map_or_else(|| "./".to_owned(), |(folder, _)| format!("{folder}/"));
                added.push(format!(
                    "[Instructions from {source}, which apply to files under {scope}; the call above reached that directory for the first time]\n{}{}",
                    text.trim_end(),
                    if truncated {
                        "\n[truncated: read the file for the rest]"
                    } else {
                        ""
                    }
                ));
            }
        }
        added
    }
}

/// Instruction files longer than the entry-file guideline, as
/// `(source, lines)`, for `ha doctor`.
#[must_use]
pub fn oversized(
    global_config_dir: &Path,
    project_root: &Path,
    cwd: &Path,
) -> Vec<(String, usize)> {
    load(global_config_dir, project_root, cwd)
        .blocks
        .iter()
        .filter_map(|block| {
            let lines = block.text.lines().count();
            (lines > ENTRY_FILE_MAX_LINES).then(|| {
                (
                    block
                        .id
                        .strip_prefix("project-rule:")
                        .unwrap_or(&block.id)
                        .to_owned(),
                    lines,
                )
            })
        })
        .collect()
}

fn display_source(path: &Path, project_root: &Path, global: bool) -> String {
    if global {
        path.file_name().map_or_else(
            || "global instruction".to_owned(),
            |name| name.to_string_lossy().into_owned(),
        )
    } else {
        path.strip_prefix(project_root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/")
    }
}

#[cfg(test)]
mod tests {
    use super::load;
    use std::path::PathBuf;

    #[test]
    fn g01_agents_md_chain_is_loaded_root_to_cwd_in_order() {
        let temp = tempfile::tempdir().expect("fixture directory");
        let global = temp.path().join("user-config");
        let root = temp.path().join("project");
        let cwd = root.join("src").join("nested");
        std::fs::create_dir_all(&global).expect("global directory");
        std::fs::create_dir_all(&cwd).expect("nested cwd");
        std::fs::write(global.join("AGENTS.md"), "global rule").expect("global rule");
        std::fs::write(root.join("AGENTS.md"), "root rule").expect("root rule");
        std::fs::write(root.join("src/AGENTS.md"), "src rule").expect("src rule");
        std::fs::write(cwd.join("AGENTS.md"), "cwd rule").expect("cwd rule");

        let loaded = load(&global, &root, &cwd);
        let text = loaded
            .blocks
            .iter()
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>();
        assert_eq!(text, ["global rule", "root rule", "src rule", "cwd rule"]);
        assert_eq!(loaded.files.len(), 4);
        assert!(
            loaded
                .blocks
                .iter()
                .all(|block| block.digest.as_str().starts_with("sha256:"))
        );
        assert!(loaded.blocks.iter().all(|block| {
            block.channel == harness_session::ContextChannel::ProjectRule
                && block.authority == harness_types::SourceAuthority::User
        }));
    }

    #[test]
    fn g01_agents_md_over_32k_is_truncated_with_notice() {
        let temp = tempfile::tempdir().expect("fixture directory");
        let root = temp.path().join("project");
        std::fs::create_dir_all(&root).expect("project directory");
        std::fs::write(root.join("AGENTS.md"), "x".repeat(40 * 1024)).expect("rule file");

        let loaded = load(&PathBuf::new(), &root, &root);
        let bytes = loaded
            .blocks
            .iter()
            .map(|block| block.text.len())
            .sum::<usize>();
        assert!(bytes <= 32 * 1024, "loaded {bytes} bytes");
        assert!(
            loaded
                .notices
                .iter()
                .any(|notice| notice.to_lowercase().contains("truncated")),
            "{:?}",
            loaded.notices
        );
    }

    #[test]
    fn g01_claude_md_is_fallback_only() {
        let temp = tempfile::tempdir().expect("fixture directory");
        let root = temp.path().join("project");
        std::fs::create_dir_all(&root).expect("project directory");
        std::fs::write(root.join("CLAUDE.md"), "legacy").expect("legacy rule");
        let legacy = load(&PathBuf::new(), &root, &root);
        assert_eq!(legacy.blocks.len(), 1);
        assert_eq!(legacy.blocks[0].text, "legacy");

        std::fs::write(root.join("AGENTS.md"), "preferred").expect("preferred rule");
        let preferred = load(&PathBuf::new(), &root, &root);
        assert_eq!(preferred.blocks.len(), 1);
        assert_eq!(preferred.blocks[0].text, "preferred");
    }

    /// A directory's personal `AGENTS.local.md` follows its shared file, so its
    /// rules come later and win.
    #[test]
    fn local_instructions_follow_the_shared_file_of_their_directory() {
        let temp = tempfile::tempdir().expect("fixture directory");
        let root = temp.path().join("project");
        std::fs::create_dir_all(&root).expect("project directory");
        std::fs::write(root.join("AGENTS.md"), "shared").expect("shared");
        std::fs::write(root.join("AGENTS.local.md"), "mine").expect("local");
        let loaded = load(&PathBuf::new(), &root, &root);
        let text = loaded
            .blocks
            .iter()
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>();
        assert_eq!(text, ["shared", "mine"]);
    }

    /// The instructions of a directory the agent reaches below its working
    /// directory come with that call's result, once per conversation.
    #[test]
    fn a_subdirectory_s_instructions_arrive_when_a_call_reaches_it() {
        use harness_tools::ToolResultContext as _;
        let temp = tempfile::tempdir().expect("fixture directory");
        let root = temp.path().join("project");
        std::fs::create_dir_all(root.join("src/api")).expect("dirs");
        std::fs::write(root.join("AGENTS.md"), "root rule").expect("root");
        std::fs::write(root.join("src/api/AGENTS.md"), "every endpoint checks auth").expect("api");
        std::fs::write(root.join("src/api/AGENTS.local.md"), "my api note").expect("api local");
        std::fs::write(root.join("src/api/users.rs"), "fn x() {}").expect("code");
        let seen = std::sync::Arc::new(std::sync::Mutex::new(std::collections::BTreeSet::new()));
        let nested = super::NestedInstructions::new(&root, &root, "conversation", seen.clone());
        let read = |path: &str| harness_tools::CodingToolAction::ReadFile {
            path: path.to_owned(),
            offset: None,
            limit: None,
        };
        let added = nested.context_for(&read("src/api/users.rs"));
        assert_eq!(added.len(), 2, "{added:?}");
        assert!(added[0].contains("src/api/AGENTS.md"), "{added:?}");
        assert!(added[0].contains("every endpoint checks auth"), "{added:?}");
        assert!(added[1].contains("my api note"), "{added:?}");
        assert!(
            !added.iter().any(|text| text.contains("root rule")),
            "the root's file is in every turn already"
        );
        assert!(
            nested.context_for(&read("src/api/users.rs")).is_empty(),
            "once per conversation"
        );
        let next_turn = super::NestedInstructions::new(&root, &root, "conversation", seen.clone());
        assert!(next_turn.context_for(&read("src/api/users.rs")).is_empty());
        let other = super::NestedInstructions::new(&root, &root, "another", seen);
        assert_eq!(other.context_for(&read("src/api/users.rs")).len(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn g01_agents_md_symlink_to_protected_path_is_ignored() {
        let temp = tempfile::tempdir().expect("fixture directory");
        let root = temp.path().join("project");
        let protected = root.join(".git");
        std::fs::create_dir_all(&protected).expect("protected directory");
        std::fs::write(protected.join("AGENTS.md"), "secret project rule").expect("protected rule");
        std::os::unix::fs::symlink(protected.join("AGENTS.md"), root.join("AGENTS.md"))
            .expect("instruction symlink");

        let loaded = load(&PathBuf::new(), &root, &root);
        assert!(loaded.blocks.is_empty());
        assert!(
            loaded
                .notices
                .iter()
                .any(|notice| notice.contains("protected instruction file"))
        );
    }
}
