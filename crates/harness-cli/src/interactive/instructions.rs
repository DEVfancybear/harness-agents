use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use harness_session::{ContextBlock, ContextBlockKind};
use harness_tools::is_sensitive_workspace_path;
use harness_types::SourceAuthority;

const MAX_INSTRUCTION_BYTES: usize = 32 * 1024;

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

    for (directory, global) in candidates {
        let agents = directory.join("AGENTS.md");
        let claude = directory.join("CLAUDE.md");
        let (path, legacy) = if agents.is_file() {
            (agents, false)
        } else if claude.is_file() {
            (claude, true)
        } else {
            continue;
        };
        let source = display_source(&path, project_root, global);
        if !global {
            let Ok(relative) = path.strip_prefix(project_root) else {
                result
                    .notices
                    .push("ignored instruction file outside project root".to_owned());
                continue;
            };
            if is_sensitive_workspace_path(relative) {
                result
                    .notices
                    .push(format!("ignored protected instruction file {source}"));
                continue;
            }
            let Ok(canonical_root) = project_root.canonicalize() else {
                result.notices.push(format!(
                    "ignored instruction file because project root could not be verified: {source}"
                ));
                continue;
            };
            let Ok(canonical_file) = path.canonicalize() else {
                result.notices.push(format!(
                    "ignored instruction file because its path could not be verified: {source}"
                ));
                continue;
            };
            let Ok(canonical_relative) = canonical_file.strip_prefix(&canonical_root) else {
                result.notices.push(format!(
                    "ignored instruction file outside project root: {source}"
                ));
                continue;
            };
            if is_sensitive_workspace_path(canonical_relative) {
                result.notices.push(format!(
                    "ignored protected instruction file {source} (resolved path)"
                ));
                continue;
            }
        }
        let read_limit = remaining.saturating_add(1);
        let mut bytes = Vec::with_capacity(read_limit.min(4096));
        let read = File::open(&path).and_then(|file| {
            file.take(u64::try_from(read_limit).unwrap_or(u64::MAX))
                .read_to_end(&mut bytes)
        });
        if read.is_err() {
            result
                .notices
                .push(format!("could not read instruction file {source}"));
            continue;
        }
        let truncated = bytes.len() > remaining;
        if truncated {
            bytes.truncate(remaining);
            while std::str::from_utf8(&bytes).is_err() && !bytes.is_empty() {
                bytes.pop();
            }
        }
        let Ok(text) = String::from_utf8(bytes) else {
            result
                .notices
                .push(format!("ignored non-UTF-8 instruction file {source}"));
            continue;
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
                "using CLAUDE.md fallback for {source}; AGENTS.md takes precedence"
            ));
        }
        if truncated {
            result.notices.push(format!(
                "instruction files truncated at the {MAX_INSTRUCTION_BYTES}-byte total limit"
            ));
            break;
        }
    }
    result
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
