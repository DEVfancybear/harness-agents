//! The session's view of prime-agent's resource resolution
//! (`pa-core/src/resources/resolution.rs`): which skills and prompt templates
//! packages and the `skills`/`prompts` settings arrays add, and which
//! resources the settings turn off.
//!
//! ha keeps its own skill roots (bundled, user, `.agents/skills`, learned and
//! `HA_SKILL_PATHS`) and adds what only the package manager knows: package
//! resources and settings-array entries. A resource the settings turn off is
//! left out wherever it comes from. Project-scope resources count only in a
//! trusted project, like every other project resource in ha.
//!
//! In a session a configured package that is not installed yet is skipped
//! rather than installed: an install writes to the terminal the TUI owns.
//! `ha package install` and `ha package update` install them.

use std::path::{Path, PathBuf};

use super::packages::{
    BundledSkillsDir, MetadataSource, MissingSourceAction, PackageManager, PackageManagerOptions,
    ResolvedResource, ResourceOrigin, SettingsManager, SourceScope,
};

/// What the package manager adds to, and removes from, a session.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionResources {
    /// Enabled package and settings-array skill documents, with whether they
    /// come from the project scope.
    pub added_skills: Vec<(PathBuf, bool)>,
    /// Canonical paths of the skill documents the settings turn off.
    pub disabled_skills: Vec<PathBuf>,
    /// Every enabled prompt template, in prime's precedence order (project
    /// settings, project directory, user settings, user directory, packages).
    pub prompts: Vec<PathBuf>,
    /// Canonical paths of the prompt templates the settings turn off.
    pub disabled_prompts: Vec<PathBuf>,
    /// Configured packages that are not installed, and other resolution
    /// problems, for the operator.
    pub diagnostics: Vec<String>,
}

/// Resolve the session resources of `workspace` with the user settings of
/// `config_dir`.
#[must_use]
pub fn resolve(
    config_dir: &Path,
    workspace: &Path,
    project_trusted: bool,
    bundled_skills: Option<PathBuf>,
) -> SessionResources {
    let settings = SettingsManager::create(workspace, config_dir);
    let mut manager = PackageManager::with_options(PackageManagerOptions {
        cwd: workspace.to_path_buf(),
        agent_dir: config_dir.to_path_buf(),
        settings,
        bundled_skills_dir: bundled_skills
            .map_or(BundledSkillsDir::Disabled, BundledSkillsDir::Directory),
        extra_builtin_skill_overrides: Vec::new(),
    });
    let mut missing = Vec::new();
    let mut skip = |source: &str| {
        missing.push(source.to_owned());
        MissingSourceAction::Skip
    };
    let resolved = match manager.resolve_with_on_missing(Some(&mut skip)) {
        Ok(resolved) => resolved,
        Err(error) => {
            return SessionResources {
                diagnostics: vec![format!("packages: {error:#}")],
                ..SessionResources::default()
            };
        }
    };
    let mut resources = SessionResources {
        diagnostics: missing
            .into_iter()
            .map(|source| {
                format!("package {source} is not installed: run `ha package install {source}`")
            })
            .collect(),
        ..SessionResources::default()
    };
    let counts = |resource: &ResolvedResource| {
        project_trusted || resource.metadata.scope != SourceScope::Project
    };
    for resource in resolved.skills.iter().filter(|resource| counts(resource)) {
        if !resource.enabled {
            resources
                .disabled_skills
                .push(super::packages::canonicalize_path(&resource.path));
        } else if resource.metadata.origin == ResourceOrigin::Package
            || resource.metadata.source == MetadataSource::Local
        {
            resources.added_skills.push((
                resource.path.clone(),
                resource.metadata.scope == SourceScope::Project,
            ));
        }
    }
    for resource in resolved.prompts.iter().filter(|resource| counts(resource)) {
        if resource.enabled {
            resources.prompts.push(resource.path.clone());
        } else {
            resources
                .disabled_prompts
                .push(super::packages::canonicalize_path(&resource.path));
        }
    }
    for diagnostic in resolved.diagnostics {
        let (super::packages::ResourceDiagnostic::Warning { message, path }
        | super::packages::ResourceDiagnostic::Error { message, path }) = diagnostic;
        resources.diagnostics.push(match path {
            Some(path) => format!("{message}: {path}"),
            None => message,
        });
    }
    resources
}

/// prime-agent's `discover_system_prompt_file`: `.harness/SYSTEM.md` of a
/// trusted project, else `SYSTEM.md` in the config directory. Its text
/// replaces the static layers of the system prompt.
#[must_use]
pub fn system_prompt(workspace: &Path, config_dir: &Path, project_trusted: bool) -> Option<String> {
    prompt_file(workspace, config_dir, project_trusted, "SYSTEM.md")
}

/// prime-agent's `discover_append_system_prompt_file`:
/// `.harness/APPEND_SYSTEM.md` of a trusted project, else `APPEND_SYSTEM.md`
/// in the config directory. Its text is added at the end of the system
/// prompt, where `--append-system-prompt` goes; the flag replaces it.
#[must_use]
pub fn append_system_prompt(
    workspace: &Path,
    config_dir: &Path,
    project_trusted: bool,
) -> Option<String> {
    prompt_file(workspace, config_dir, project_trusted, "APPEND_SYSTEM.md")
}

fn prompt_file(
    workspace: &Path,
    config_dir: &Path,
    project_trusted: bool,
    name: &str,
) -> Option<String> {
    let project = workspace.join(super::packages::CONFIG_DIR_NAME).join(name);
    [
        project_trusted.then_some(project),
        Some(config_dir.join(name)),
    ]
    .into_iter()
    .flatten()
    .find(|path| path.exists())
    .and_then(|path| std::fs::read_to_string(path).ok())
    .filter(|text| !text.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_project_system_md_wins_once_trusted() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let config = temporary.path().join("config");
        let workspace = temporary.path().join("workspace");
        std::fs::create_dir_all(&config).expect("config");
        std::fs::create_dir_all(workspace.join(".harness")).expect("project");
        std::fs::write(config.join("SYSTEM.md"), "global system").expect("global");
        std::fs::write(workspace.join(".harness/SYSTEM.md"), "project system").expect("project");
        std::fs::write(config.join("APPEND_SYSTEM.md"), "extra").expect("append");
        assert_eq!(
            system_prompt(&workspace, &config, true).as_deref(),
            Some("project system")
        );
        assert_eq!(
            system_prompt(&workspace, &config, false).as_deref(),
            Some("global system")
        );
        assert_eq!(
            append_system_prompt(&workspace, &config, true).as_deref(),
            Some("extra")
        );
    }

    fn skill(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name).join("SKILL.md");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("skill dir");
        std::fs::write(
            &path,
            format!("---\nname: {name}\ndescription: {name}\n---\nbody"),
        )
        .expect("skill");
        path
    }

    #[test]
    fn a_local_package_adds_its_skills_and_prompts_and_settings_turn_one_off() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let config = temporary.path().join("config");
        let workspace = temporary.path().join("workspace");
        let package = temporary.path().join("pkg");
        skill(&package.join("skills"), "from-package");
        let off = skill(&package.join("skills"), "turned-off");
        std::fs::create_dir_all(package.join("prompts")).expect("prompts");
        std::fs::write(package.join("prompts/review.md"), "Review $1").expect("prompt");
        std::fs::create_dir_all(&config).expect("config");
        std::fs::create_dir_all(&workspace).expect("workspace");
        std::fs::write(
            config.join("settings.json"),
            serde_json::json!({"packages": [{
                "source": package.display().to_string(),
                "skills": ["skills/from-package/SKILL.md"],
            }]})
            .to_string(),
        )
        .expect("settings");
        let resources = resolve(&config, &workspace, false, None);
        assert_eq!(resources.added_skills.len(), 1, "{resources:?}");
        assert!(
            resources.added_skills[0]
                .0
                .ends_with("from-package/SKILL.md")
        );
        assert_eq!(
            resources.disabled_skills,
            vec![super::super::packages::canonicalize_path(&off)]
        );
        assert!(
            resources
                .prompts
                .iter()
                .any(|path| path.ends_with("review.md")),
            "{resources:?}"
        );
    }

    #[test]
    fn project_resources_wait_for_trust() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let config = temporary.path().join("config");
        let workspace = temporary.path().join("workspace");
        std::fs::create_dir_all(workspace.join(".harness/prompts")).expect("prompts");
        std::fs::write(workspace.join(".harness/prompts/fix.md"), "Fix $@").expect("prompt");
        assert!(resolve(&config, &workspace, false, None).prompts.is_empty());
        assert_eq!(resolve(&config, &workspace, true, None).prompts.len(), 1);
    }
}
