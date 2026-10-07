//! Trusted skill and prompt-command roots for one workspace.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use harness_extensions::{
    MAX_SKILL_CATALOG_ENTRIES, SkillActivation, SkillCatalog, SkillSource, TrustedSkillRoot,
};
use harness_tools::{
    CodingToolAction, ExternalToolCatalog, ExternalToolDispatcher, ExternalTools,
    ToolDispatchAuthorization, ToolOutput,
};
use harness_types::{ErrorCode, HarnessError};
use serde_json::{Value, json};

use super::paths::LaunchEnvironment;

include!(concat!(env!("OUT_DIR"), "/bundled_skills.rs"));

const MAX_COMMANDS: usize = 256;
const MAX_COMMAND_BYTES: u64 = 256 * 1024;

pub fn roots(
    config_dir: &Path,
    workspace: &Path,
    environment: &LaunchEnvironment,
    project_trusted: bool,
) -> Vec<TrustedSkillRoot> {
    let mut roots = Vec::new();
    let user_skills = config_dir.join("skills");
    if user_skills.is_dir() {
        roots.push(TrustedSkillRoot::new(user_skills, SkillSource::User));
    }
    if let Some(home) = environment
        .value("USERPROFILE")
        .or_else(|| environment.value("HOME"))
    {
        let home = PathBuf::from(home);
        let path = home.join(".agents").join("skills");
        if path.is_dir() {
            roots.push(TrustedSkillRoot::new(path, SkillSource::User));
        }
    }
    // Extra skill directories, the way prime-agent's `skills` setting adds another
    // harness's skills (`~/.claude/skills`, `~/.codex/skills`). They are the user's
    // own choice, so they carry user trust.
    if let Some(paths) = environment.value(SKILL_PATHS_VARIABLE) {
        for path in std::env::split_paths(paths) {
            if path.is_dir() && !roots.iter().any(|root| root.path == path) {
                roots.push(TrustedSkillRoot::new(path, SkillSource::User));
            }
        }
    }
    if project_trusted {
        let mut paths = vec![workspace.join(".harness/skills")];
        // `.agents/skills` in the workspace and in each parent up to the repository
        // root, as prime-agent and pi discover them: a monorepo keeps shared skills
        // at its root and opens the agent in a package below it.
        for directory in workspace.ancestors() {
            paths.push(directory.join(".agents/skills"));
            if directory.join(".git").exists() {
                break;
            }
        }
        // Skills ha learned live in the main checkout, also when the workspace
        // is one of its linked worktrees.
        paths.push(super::learned::project_skill_root(workspace));
        for path in paths {
            if path.is_dir() && !roots.iter().any(|root| root.path == path) {
                roots.push(TrustedSkillRoot::new(path, SkillSource::TrustedProject));
            }
        }
    }
    roots
}

/// Extra skill directories, separated like `PATH` (`;` on Windows).
pub const SKILL_PATHS_VARIABLE: &str = "HA_SKILL_PATHS";

pub fn discover(
    config_dir: &Path,
    workspace: &Path,
    environment: &LaunchEnvironment,
    project_trusted: bool,
) -> Result<SkillCatalog, HarnessError> {
    let bundled = materialize_bundled_skills(config_dir)?;
    let mut roots = vec![TrustedSkillRoot::new(bundled.clone(), SkillSource::Builtin)];
    roots.extend(self::roots(
        config_dir,
        workspace,
        environment,
        project_trusted,
    ));
    // prime-agent's package manager: package skills and the `skills` settings
    // arrays add documents, and the settings turn resources off.
    let resources =
        super::resources::resolve(config_dir, workspace, project_trusted, Some(bundled));
    for (path, project) in resources.added_skills {
        if !roots.iter().any(|root| root.path == path) {
            roots.push(TrustedSkillRoot::new(
                path,
                if project {
                    SkillSource::TrustedProject
                } else {
                    SkillSource::User
                },
            ));
        }
    }
    SkillCatalog::discover_excluding(&roots, &resources.disabled_skills)
        .map_err(|error| HarnessError::new(error.code(), error.to_string()))
}

/// Where ha's bundled skills are on disk (written there on first use): the
/// package manager's built-in skills directory.
///
/// # Errors
/// The bundled files could not be written.
pub fn bundled_skills_root(config_dir: &Path) -> Result<PathBuf, HarnessError> {
    materialize_bundled_skills(config_dir)
}

fn materialize_bundled_skills(config_dir: &Path) -> Result<PathBuf, HarnessError> {
    let bundle_root = config_dir.join("bundled-skills").join(BUNDLED_SKILL_DIGEST);
    let skills_root = bundle_root.join("skills");
    for &(relative, bytes) in BUNDLED_SKILL_FILES {
        install_bundled_file(&skills_root.join(relative), bytes)?;
    }
    for &(name, bytes) in BUNDLED_NOTICES {
        install_bundled_file(&bundle_root.join(name), bytes)?;
    }
    Ok(skills_root)
}

fn install_bundled_file(path: &Path, bytes: &[u8]) -> Result<(), HarnessError> {
    // Discovery runs every turn and every menu refresh; reading and comparing
    // all the bundled files (some 2.8 MB) each time cost more than the rest of
    // discovery. A file this process already found intact, or wrote, is
    // trusted again while its length and modification time stay what they
    // were, so a tampered or deleted file is still repaired on the next call.
    static VERIFIED: std::sync::OnceLock<Mutex<BTreeMap<PathBuf, super::config::FileStamp>>> =
        std::sync::OnceLock::new();
    let verified = VERIFIED.get_or_init(Mutex::default);
    let remember = |stamp: Option<super::config::FileStamp>| {
        if let (Some(stamp), Ok(mut verified)) = (stamp, verified.lock()) {
            verified.insert(path.to_path_buf(), stamp);
        }
    };
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                return Err(HarnessError::new(
                    ErrorCode::SkillUnavailable,
                    format!(
                        "bundled skill path is not a regular file: {}",
                        path.display()
                    ),
                ));
            }
            let stamp = metadata
                .modified()
                .ok()
                .map(|modified| (metadata.len(), modified));
            if stamp.is_some()
                && verified
                    .lock()
                    .is_ok_and(|verified| verified.get(path) == stamp.as_ref())
            {
                return Ok(());
            }
            if usize::try_from(metadata.len()).is_ok_and(|len| len == bytes.len())
                && std::fs::read(path).map_err(|error| bundled_io_error(&error))? == bytes
            {
                remember(stamp);
                return Ok(());
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(bundled_io_error(&error)),
    }
    std::fs::create_dir_all(path.parent().expect("bundled file has a parent"))
        .map_err(|error| bundled_io_error(&error))?;
    std::fs::write(path, bytes).map_err(|error| bundled_io_error(&error))?;
    remember(super::config::file_stamp(path));
    Ok(())
}

fn bundled_io_error(error: &std::io::Error) -> HarnessError {
    HarnessError::new(
        ErrorCode::SkillUnavailable,
        format!("bundled skills cannot be prepared: {error}"),
    )
}

/// The model-facing skill catalogue and digest-pinned activation dispatcher.
/// Both tools use the shared external-tool policy, approval, intent and receipt
/// path; activation returns a typed Skill-channel block for the next model step.
pub struct SkillHost {
    catalog: SkillCatalog,
    active: Arc<Mutex<BTreeMap<String, SkillActivation>>>,
}

impl SkillHost {
    #[must_use]
    pub fn new(
        catalog: SkillCatalog,
        active: Arc<Mutex<BTreeMap<String, SkillActivation>>>,
    ) -> Self {
        Self { catalog, active }
    }

    #[must_use]
    pub fn tools(&self) -> ExternalTools {
        ExternalTools::new(Arc::new(SkillToolCatalog {
            catalog: self.catalog.clone(),
        }))
    }

    #[must_use]
    pub fn dispatcher(&self) -> Arc<dyn ExternalToolDispatcher> {
        Arc::new(SkillToolDispatcher {
            catalog: self.catalog.clone(),
            active: Arc::clone(&self.active),
        })
    }
}

struct SkillToolCatalog {
    catalog: SkillCatalog,
}

impl ExternalToolCatalog for SkillToolCatalog {
    fn schemas(&self) -> Vec<Value> {
        vec![
            json!({
                "type": "function",
                "function": {
                    "name": "list_skills",
                    "description": "When a task may match a skill, list available skills and exact version digests before activating one. Bodies stay unloaded.",
                    "parameters": {"type": "object", "properties": {}, "additionalProperties": false}
                }
            }),
            json!({
                "type": "function",
                "function": {
                    "name": "activate_skill",
                    "description": "Load one matching skill by name before following its instructions. Pass the digest from list_skills to pin the exact version you listed; it may be omitted.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "name": {"type": "string", "enum": self.catalog.entries().iter().filter(|entry| entry.model_invocable).map(|entry| entry.name.clone()).collect::<Vec<_>>()},
                            "digest": {"type": "string", "description": "Optional: the sha256 digest list_skills returned for this name, with or without the sha256: prefix."}
                        },
                        "required": ["name"],
                        "additionalProperties": false
                    }
                }
            }),
            json!({
                "type": "function",
                "function": {
                    "name": "read_skill_file",
                    "description": "Read one of a skill's own files (references, prompts, templates, scripts) by its path relative to the skill directory, as listed when the skill was activated.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "name": {"type": "string", "description": "The skill's name."},
                            "path": {"type": "string", "description": "Path relative to the skill directory, e.g. references/api.md."}
                        },
                        "required": ["name", "path"],
                        "additionalProperties": false
                    }
                }
            }),
        ]
    }

    fn resolve(&self, name: &str, arguments: &Value) -> Option<CodingToolAction> {
        matches!(name, "list_skills" | "activate_skill" | "read_skill_file").then(|| {
            CodingToolAction::ExternalTool {
                plugin_id: "skill".to_owned(),
                tool_name: name.to_owned(),
                arguments: arguments.clone(),
                parent_invocation_id: None,
                timeout_ms: 5_000,
            }
        })
    }
}

/// Whether two spellings name the same sha256 digest: the `sha256:` prefix and
/// letter case do not change which content is pinned.
fn same_digest(catalog: &str, supplied: &str) -> bool {
    let bare = |digest: &str| {
        let digest = digest.trim();
        digest
            .strip_prefix("sha256:")
            .unwrap_or(digest)
            .to_ascii_lowercase()
    };
    !supplied.trim().is_empty() && bare(catalog) == bare(supplied)
}

struct SkillToolDispatcher {
    catalog: SkillCatalog,
    active: Arc<Mutex<BTreeMap<String, SkillActivation>>>,
}

impl SkillToolDispatcher {
    fn validate(&self, tool_name: &str, arguments: &Value) -> Result<(), HarnessError> {
        let object = arguments.as_object().ok_or_else(|| {
            HarnessError::new(
                ErrorCode::InvalidPayload,
                "skill tool arguments must be an object",
            )
        })?;
        match tool_name {
            "list_skills" if object.is_empty() => Ok(()),
            "list_skills" => Err(HarnessError::new(
                ErrorCode::InvalidPayload,
                "list_skills accepts no arguments",
            )),
            "activate_skill" => {
                if object.keys().any(|key| key != "name" && key != "digest") {
                    return Err(HarnessError::new(
                        ErrorCode::InvalidPayload,
                        "activate_skill accepts only name and an optional digest",
                    ));
                }
                let name = object.get("name").and_then(Value::as_str).ok_or_else(|| {
                    HarnessError::new(ErrorCode::InvalidPayload, "skill name must be a string")
                })?;
                let digest = match object.get("digest") {
                    None | Some(Value::Null) => None,
                    Some(Value::String(digest)) => Some(digest.as_str()),
                    Some(_) => {
                        return Err(HarnessError::new(
                            ErrorCode::InvalidPayload,
                            "skill digest must be a string",
                        ));
                    }
                };
                let entry = self.catalog.entry(name).ok_or_else(|| {
                    let known = self
                        .catalog
                        .entries()
                        .iter()
                        .map(|entry| entry.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ");
                    HarnessError::new(
                        ErrorCode::SkillUnavailable,
                        format!(
                            "no trusted skill named {name} is in this catalogue; available: {known}"
                        ),
                    )
                })?;
                // The pin protects against a skill that changed between listing and
                // activation, so a wrong digest is still refused. How it is spelled is
                // not the point: measured, a model copied the digest without its
                // `sha256:` prefix, was refused, and gave up on the skill. The refusal
                // now names the current digest so a genuinely stale pin can be retried
                // in one step.
                // A user-only skill the user already ran with /skill:<name> is in
                // the conversation: measured, a model asked to activate it again,
                // was told the user must run it, and told the user to type the
                // command they had just typed.
                let user_ran = self
                    .active
                    .lock()
                    .is_ok_and(|active| active.contains_key(name));
                if !entry.model_invocable && !user_ran {
                    return Err(HarnessError::new(
                        ErrorCode::PolicyDenied,
                        format!("skill {name} is run only by the user, with /skill:{name}"),
                    ));
                }
                if let Some(digest) = digest
                    && !same_digest(entry.digest.as_str(), digest)
                {
                    return Err(HarnessError::new(
                        ErrorCode::SchemaVersionMismatch,
                        format!(
                            "skill {name} changed since that digest was listed; its current digest is {}",
                            entry.digest.as_str()
                        ),
                    ));
                }
                Ok(())
            }
            "read_skill_file" => {
                if object.keys().any(|key| key != "name" && key != "path") {
                    return Err(HarnessError::new(
                        ErrorCode::InvalidPayload,
                        "read_skill_file accepts only name and path",
                    ));
                }
                let name = object.get("name").and_then(Value::as_str).ok_or_else(|| {
                    HarnessError::new(ErrorCode::InvalidPayload, "skill name must be a string")
                })?;
                object.get("path").and_then(Value::as_str).ok_or_else(|| {
                    HarnessError::new(
                        ErrorCode::InvalidPayload,
                        "skill file path must be a string",
                    )
                })?;
                self.catalog.entry(name).ok_or_else(|| {
                    HarnessError::new(
                        ErrorCode::SkillUnavailable,
                        format!("no trusted skill named {name} is in this catalogue"),
                    )
                })?;
                Ok(())
            }
            _ => Err(HarnessError::new(
                ErrorCode::PolicyDenied,
                "skill tool target is unavailable",
            )),
        }
    }

    /// One of a skill's own files, as text the model reads.
    fn read_file(&self, arguments: &Value) -> Result<ToolOutput, HarnessError> {
        self.validate("read_skill_file", arguments)?;
        let name = arguments["name"].as_str().expect("validated skill name");
        let path = arguments["path"].as_str().expect("validated skill path");
        let entry = self.catalog.entry(name).expect("validated catalog entry");
        let (text, truncated) = entry
            .read_resource(path)
            .map_err(|error| HarnessError::new(error.code(), error.to_string()))?;
        Ok(ToolOutput::ExternalTool {
            plugin_id: "skill".to_owned(),
            tool_name: "read_skill_file".to_owned(),
            payload: json!({
                "name": name,
                "path": path,
                "truncated": truncated,
                "text": if truncated {
                    format!("{name}/{path} (first {} bytes):\n{text}", harness_extensions::MAX_SKILL_RESOURCE_BYTES)
                } else {
                    format!("{name}/{path}:\n{text}")
                },
            }),
            inflight: 1,
        })
    }

    fn activate(&self, arguments: &Value) -> Result<ToolOutput, HarnessError> {
        self.validate("activate_skill", arguments)?;
        let name = arguments["name"].as_str().expect("validated skill name");
        let entry = self.catalog.entry(name).expect("validated catalog entry");
        let activation = self
            .catalog
            .activate(name, Some(&entry.digest), 1)
            .map_err(|error| HarnessError::new(error.code(), error.to_string()))?;
        let block = activation.block();
        self.active
            .lock()
            .map_err(|_| {
                HarnessError::new(ErrorCode::RuntimeBlocked, "active skills are unavailable")
            })?
            .insert(name.to_owned(), activation);
        Ok(ToolOutput::SkillActivated { block })
    }
}

impl ExternalToolDispatcher for SkillToolDispatcher {
    fn validate_external<'a>(
        &'a self,
        plugin_id: &'a str,
        tool_name: &'a str,
        arguments: &'a Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), HarnessError>> + Send + 'a>>
    {
        Box::pin(async move {
            if plugin_id != "skill" {
                return Err(HarnessError::new(
                    ErrorCode::PolicyDenied,
                    "skill tool target is unavailable",
                ));
            }
            self.validate(tool_name, arguments)
        })
    }

    fn dispatch_external<'a>(
        &'a self,
        _authorization: &'a ToolDispatchAuthorization,
        plugin_id: &'a str,
        tool_name: &'a str,
        arguments: &'a Value,
        _timeout_ms: u64,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<ToolOutput, HarnessError>> + Send + 'a>,
    > {
        Box::pin(async move {
            if plugin_id != "skill" {
                return Err(HarnessError::new(
                    ErrorCode::PolicyDenied,
                    "skill tool target is unavailable",
                ));
            }
            self.validate(tool_name, arguments)?;
            match tool_name {
                "list_skills" => Ok(ToolOutput::ExternalTool {
                    plugin_id: "skill".to_owned(),
                    tool_name: "list_skills".to_owned(),
                    payload: json!({
                        "catalog_digest": self.catalog.catalog_digest().as_str(),
                        "skills": self.catalog.entries().iter().filter(|entry| entry.model_invocable).map(|entry| json!({
                            "name": entry.name,
                            "description": entry.description,
                            "version": entry.version,
                            "digest": entry.digest.as_str(),
                        })).collect::<Vec<_>>(),
                        "limit": MAX_SKILL_CATALOG_ENTRIES,
                    }),
                    inflight: 1,
                }),
                "activate_skill" => self.activate(arguments),
                "read_skill_file" => self.read_file(arguments),
                _ => Err(HarnessError::new(
                    ErrorCode::PolicyDenied,
                    "skill tool target is unavailable",
                )),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        sync::{Arc, Mutex},
    };

    use harness_extensions::{SkillCatalog, SkillSource, TrustedSkillRoot};
    use harness_session::ContextChannel;
    use harness_types::ErrorCode;

    use super::{SkillHost, SkillToolDispatcher, commands, discover, expand};

    fn skill_catalog(root: &std::path::Path) -> SkillCatalog {
        let skill = root.join("review");
        std::fs::create_dir_all(&skill).expect("skill folder");
        std::fs::write(
            skill.join("SKILL.md"),
            "---\nname: review\nversion: 1.2\ndescription: Review Rust changes\nrequested_tools: [read_file]\n---\nUse the review checklist.\n",
        )
        .expect("skill body");
        SkillCatalog::discover(&[TrustedSkillRoot::new(root, SkillSource::User)])
            .expect("catalog scans metadata")
    }

    #[test]
    fn release_skills_are_available_without_trusting_a_project_checkout() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let config = temporary.path().join("config");
        let workspace = temporary.path().join("unrelated-project");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let catalog = discover(
            &config,
            &workspace,
            &super::LaunchEnvironment::default(),
            false,
        )
        .expect("built-in skills must be discoverable without project trust");
        for name in ["find-skills", "github-deep-research", "websearch"] {
            let entry = catalog.entry(name).expect("bundled skill");
            assert_eq!(entry.source, SkillSource::Builtin);
            assert!(catalog.activate(name, None, 1).is_ok());
        }
        assert!(catalog.entries().len() >= 15);
        assert!(
            catalog.entry("deep-research").is_none(),
            "the release must not ship deep-research"
        );
        assert!(
            catalog.entry("prime-intellect").is_none(),
            "the release must not ship prime-intellect"
        );
        assert!(
            catalog.entry("writing-skills").is_none(),
            "the release must not ship writing-skills"
        );
        for name in ["brainstorming", "using-superpowers", "writing-plans"] {
            assert!(
                catalog.entry(name).is_none(),
                "the release must not ship obra/superpowers' {name}"
            );
        }
        assert!(
            catalog
                .entry("github-deep-research")
                .expect("research skill")
                .path
                .parent()
                .expect("skill directory")
                .join("scripts/github_api.py")
                .is_file()
        );
        let bundled_document = catalog
            .entry("find-skills")
            .expect("bundled skill")
            .path
            .clone();
        let original = std::fs::read(&bundled_document).expect("bundled document");
        std::fs::write(&bundled_document, "corrupted cache").expect("tamper test cache");
        let rediscovered = discover(
            &config,
            &workspace,
            &super::LaunchEnvironment::default(),
            false,
        )
        .expect("repair bundled cache");
        assert_eq!(
            rediscovered
                .entry("find-skills")
                .expect("repaired skill")
                .source,
            SkillSource::Builtin
        );
        assert_eq!(
            std::fs::read(&bundled_document).expect("repaired document"),
            original
        );
    }

    /// A skill with its own files, as the bundled ones ship: references and scripts.
    fn skill_with_files(root: &std::path::Path) -> SkillCatalog {
        let skill = root.join("research");
        std::fs::create_dir_all(skill.join("references")).expect("references");
        std::fs::create_dir_all(skill.join("scripts")).expect("scripts");
        std::fs::write(
            skill.join("SKILL.md"),
            "---\nname: research\ndescription: Research a topic\n---\nRead references/method.md, then run scripts/search.py.\n",
        )
        .expect("skill body");
        std::fs::write(skill.join("references/method.md"), "Search three angles.\n")
            .expect("reference");
        std::fs::write(skill.join("scripts/search.py"), "print('ok')\n").expect("script");
        let hidden = root.join("release");
        std::fs::create_dir_all(&hidden).expect("hidden skill");
        std::fs::write(
            hidden.join("SKILL.md"),
            "---\nname: release\ndescription: Cut a release\ndisable-model-invocation: true\n---\nOnly when asked.\n",
        )
        .expect("hidden body");
        std::fs::write(root.join("secret.txt"), "outside the skill\n").expect("outside file");
        SkillCatalog::discover(&[TrustedSkillRoot::new(root, SkillSource::User)])
            .expect("catalog scans metadata")
    }

    /// Measured: a skill said "read references/..." and "run scripts/...", and the
    /// model had its instructions but not where they lived, while `read_file` stops at
    /// the workspace and bundled skills live outside it. The activation now names the
    /// directory and the files, and `read_skill_file` reads them - inside the skill only.
    #[test]
    fn an_activated_skill_names_its_files_and_they_can_be_read_but_nothing_else() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let catalog = skill_with_files(&temporary.path().join("skills"));
        let dispatcher = SkillToolDispatcher {
            catalog,
            active: Arc::new(Mutex::new(BTreeMap::new())),
        };
        let harness_tools::ToolOutput::SkillActivated { block } = dispatcher
            .activate(&serde_json::json!({ "name": "research" }))
            .expect("activation by name")
        else {
            panic!("activation returns a skill block");
        };
        for expected in [
            "references/method.md",
            "scripts/search.py",
            "read_skill_file",
            "research",
        ] {
            assert!(
                block.text.contains(expected),
                "{expected} in:\n{}",
                block.text
            );
        }
        assert!(
            block
                .text
                .contains("Read references/method.md, then run scripts/search.py.")
        );

        let read = dispatcher
            .read_file(&serde_json::json!({ "name": "research", "path": "references/method.md" }))
            .expect("a skill file is readable");
        let harness_tools::ToolOutput::ExternalTool { payload, .. } = read else {
            panic!("the file comes back as tool text");
        };
        assert!(
            payload["text"]
                .as_str()
                .unwrap_or_default()
                .contains("Search three angles.")
        );
        dispatcher
            .read_file(&serde_json::json!({ "name": "research", "path": "references\\method.md" }))
            .expect("a Windows-style separator is readable on every host");

        for escape in [
            "../secret.txt",
            "..\\secret.txt",
            "/etc/passwd",
            "C:\\Windows\\win.ini",
            "references/../../secret.txt",
        ] {
            let refused = dispatcher
                .read_file(&serde_json::json!({ "name": "research", "path": escape }))
                .expect_err("a path outside the skill is refused");
            assert_eq!(
                refused.code(),
                ErrorCode::PolicyDenied,
                "{escape}: {refused}"
            );
        }
    }

    /// am-will/swarms ships as five user-only skills (`/skill:<name>`), without
    /// `co-design` or `parallel-task-tmux`, and every file a `SKILL.md` links to
    /// ships with it.
    #[test]
    fn the_swarms_skills_ship_for_the_user_to_start() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let catalog = discover(
            &temporary.path().join("config"),
            temporary.path(),
            &super::LaunchEnvironment::default(),
            false,
        )
        .expect("bundled skills");
        for name in [
            "swarm-planner",
            "parallel-task",
            "parallel-task-spark",
            "super-swarm",
            "super-swarm-spark",
        ] {
            let entry = catalog.entry(name).expect("a swarms skill");
            assert_eq!(entry.source, SkillSource::Builtin, "{name}");
            assert_eq!(entry.version, "1102681", "{name}");
            assert!(
                !entry.model_invocable,
                "{name} starts only with /skill:{name}"
            );
            assert!(entry.description.contains("Use when"), "{name}");
            let activation = catalog
                .activate(name, None, 1)
                .expect("the user can run it");
            for link in activation.content.split("](").skip(1) {
                let target = link.split(')').next().expect("link target");
                if target.starts_with("references/") {
                    assert!(
                        activation
                            .resources
                            .iter()
                            .any(|file| file.ends_with(target)),
                        "{name} links {target}, which is not shipped: {:?}",
                        activation.resources
                    );
                }
            }
        }
        for name in ["co-design", "parallel-task-tmux"] {
            assert!(catalog.entry(name).is_none(), "{name} is not imported");
        }
    }

    /// `disable-model-invocation: true` hides a skill from the model - its list, the
    /// tool schema and the prompt - and it stays available to the user as /skill:name.
    #[test]
    fn a_user_only_skill_is_hidden_from_the_model() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let catalog = skill_with_files(&temporary.path().join("skills"));
        assert!(
            !catalog
                .entry("release")
                .expect("still in the catalogue")
                .model_invocable
        );
        let prompt =
            crate::interactive::prompt::append_skill_metadata(String::new(), catalog.entries());
        assert!(
            prompt.contains("research") && !prompt.contains("release"),
            "{prompt}"
        );
        let host = SkillHost::new(catalog.clone(), Arc::new(Mutex::new(BTreeMap::new())));
        let schemas = serde_json::to_string(&host.tools().schemas()).expect("schemas");
        assert!(schemas.contains("read_skill_file"), "{schemas}");
        assert!(!schemas.contains("\"release\""), "{schemas}");
        let dispatcher = SkillToolDispatcher {
            catalog: catalog.clone(),
            active: Arc::new(Mutex::new(BTreeMap::new())),
        };
        let refused = dispatcher
            .validate("activate_skill", &serde_json::json!({ "name": "release" }))
            .expect_err("the model cannot activate it");
        assert!(refused.to_string().contains("/skill:release"), "{refused}");
        let activation = catalog.activate("release", None, 1);
        assert!(activation.is_ok(), "the user still can");
        // Once the user ran it with /skill:release, the model may activate it.
        dispatcher
            .active
            .lock()
            .expect("active")
            .insert("release".to_owned(), activation.expect("activation"));
        dispatcher
            .validate("activate_skill", &serde_json::json!({ "name": "release" }))
            .expect("a skill the user ran is active");
    }

    #[test]
    fn g11_skill_activation_is_digest_pinned_and_uses_the_skill_channel() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let catalog = skill_catalog(&temporary.path().join("skills"));
        let entry = catalog.entry("review").expect("front matter name is used");
        let digest = entry.digest.as_str().to_owned();
        let active = Arc::new(Mutex::new(BTreeMap::new()));
        let host = SkillHost::new(catalog.clone(), Arc::clone(&active));
        let schema_names = host
            .tools()
            .schemas()
            .iter()
            .filter_map(|schema| schema["function"]["name"].as_str())
            .map(str::to_owned)
            .collect::<Vec<_>>();
        assert!(schema_names.contains(&"activate_skill".to_owned()));
        assert!(schema_names.contains(&"list_skills".to_owned()));

        let dispatcher = SkillToolDispatcher {
            catalog,
            active: Arc::clone(&active),
        };
        let arguments = serde_json::json!({
            "name": "review",
            "digest": digest,
        });
        assert!(dispatcher.validate("activate_skill", &arguments).is_ok());
        let activated = dispatcher.activate(&arguments).expect("pinned activation");
        let harness_tools::ToolOutput::SkillActivated { block } = activated else {
            panic!("activation returns a typed skill block");
        };
        assert_eq!(block.channel, ContextChannel::Skill);
        assert!(block.text.contains("Use the review checklist."));
        assert_eq!(active.lock().expect("active map").len(), 1);

        let stale = serde_json::json!({ "name": "review", "digest": "sha256:stale" });
        let error = dispatcher
            .validate("activate_skill", &stale)
            .expect_err("stale pin is refused");
        assert_eq!(error.code(), ErrorCode::SchemaVersionMismatch);
        assert!(
            error.to_string().contains(&digest),
            "the refusal names the current digest so the model can retry: {error}"
        );

        // Measured: the model sent the digest without its `sha256:` prefix and was
        // refused, then abandoned the skill. The same content is still the same pin.
        let bare = digest.trim_start_matches("sha256:").to_uppercase();
        assert!(
            dispatcher
                .validate(
                    "activate_skill",
                    &serde_json::json!({ "name": "review", "digest": bare })
                )
                .is_ok()
        );
        // And a name alone activates the version in the catalogue now.
        assert!(
            dispatcher
                .validate("activate_skill", &serde_json::json!({ "name": "review" }))
                .is_ok()
        );
        let unknown = dispatcher
            .validate("activate_skill", &serde_json::json!({ "name": "nope" }))
            .expect_err("unknown skill");
        assert!(unknown.to_string().contains("review"), "{unknown}");
    }

    #[test]
    fn g11_prompt_commands_expand_argument_placeholders_and_honor_project_trust() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let config = temporary.path().join("config");
        let workspace = temporary.path().join("workspace");
        std::fs::create_dir_all(config.join("commands")).expect("user command root");
        std::fs::create_dir_all(workspace.join(".harness/commands")).expect("project command root");
        std::fs::write(
            config.join("commands/review.md"),
            "---\ndescription: Review a change\nargument-hint: <path> <mode>\n---\nReview $1 in $2. Args: $ARGUMENTS; ninth=$9.\n",
        )
        .expect("command template");
        std::fs::write(
            workspace.join(".harness/commands/review.md"),
            "---\ndescription: Trusted project override\n---\nProject command.\n",
        )
        .expect("trusted project command");

        let untrusted = commands(&config, &workspace, false).expect("user commands");
        assert_eq!(untrusted.len(), 1);
        assert_eq!(untrusted[0].source, "user config");
        assert_eq!(
            expand(&untrusted[0], "src/main.rs concise"),
            "Review src/main.rs in concise. Args: src/main.rs concise; ninth=.\n"
        );

        let trusted = commands(&config, &workspace, true).expect("trusted project commands");
        assert_eq!(trusted.len(), 1);
        assert_eq!(trusted[0].description, "Trusted project override");
    }

    /// prime-agent's Python skills ship with the app: each is in the catalogue, its
    /// package is found for the kernel, and the prompt names its import.
    #[test]
    fn the_bundled_prime_agent_skills_carry_their_python_packages() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let catalog = discover(
            &temporary.path().join("config"),
            temporary.path(),
            &crate::interactive::paths::LaunchEnvironment::from_pairs([(
                "HA_HOME",
                temporary.path().join("home").into_os_string(),
            )]),
            false,
        )
        .expect("catalog");
        let imports = catalog
            .entries()
            .iter()
            .flat_map(|entry| crate::interactive::repl::python_skill_packages(&entry.path))
            .map(|skill| skill.import_name)
            .collect::<Vec<_>>();
        for name in [
            "edit",
            "goal",
            "compact",
            "refine",
            "websearch",
            "attach_image",
            "agent_message",
            "agent_observe",
            "rlm_heartbeat",
        ] {
            assert!(
                imports.iter().any(|import| import == name),
                "{name}: {imports:?}"
            );
        }
        let prompt =
            crate::interactive::prompt::append_skill_metadata(String::new(), catalog.entries());
        assert!(
            prompt.contains("<python_import>edit</python_import>"),
            "{prompt}"
        );
    }

    #[test]
    fn g11_prompt_skill_metadata_is_bounded_to_the_supplied_budget() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let catalog = skill_catalog(&temporary.path().join("skills"));
        let prompt =
            crate::interactive::prompt::append_skill_metadata(String::new(), catalog.entries());
        assert!(prompt.contains("<available_skills>"), "{prompt}");
        assert!(prompt.len() <= 16 * 1024, "{}", prompt.len());
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PromptCommand {
    pub name: String,
    pub description: String,
    pub argument_hint: String,
    pub body: String,
    pub source: String,
}

/// The prompt templates of a workspace (prime-agent's
/// `load_prompt_templates`): what the package manager resolves (the
/// `prompts` settings arrays, `.harness/prompts`, the config directory's
/// `prompts/` and package prompts), then ha's earlier `commands/` folders.
/// The first template of a name wins.
pub fn commands(
    config_dir: &Path,
    workspace: &Path,
    project_trusted: bool,
) -> Result<Vec<PromptCommand>, HarnessError> {
    let resources = super::resources::resolve(config_dir, workspace, project_trusted, None);
    let mut files: Vec<(PathBuf, &str)> = resources
        .prompts
        .iter()
        .map(|path| (path.clone(), "prompt"))
        .collect();
    // A trusted project's command overrides the user's of the same name,
    // as ha's `commands/` folders always did: the first one loaded wins.
    let mut legacy_roots = Vec::new();
    if project_trusted {
        legacy_roots.push((workspace.join(".harness/commands"), "trusted project"));
    }
    legacy_roots.push((config_dir.join("commands"), "user config"));
    for (root, source) in legacy_roots {
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        let mut paths = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.is_file() && path.extension().is_some_and(|ext| ext == "md"))
            .filter(|path| {
                !resources
                    .disabled_prompts
                    .contains(&super::packages::canonicalize_path(path))
            })
            .collect::<Vec<_>>();
        paths.sort();
        files.extend(paths.into_iter().map(|path| (path, source)));
    }
    let mut loaded: Vec<PromptCommand> = Vec::new();
    for (path, source) in files {
        if loaded.len() >= MAX_COMMANDS {
            return Err(HarnessError::new(
                ErrorCode::FrameLimitExceeded,
                "prompt command catalog is over its 256 file limit",
            ));
        }
        let metadata = std::fs::metadata(&path).map_err(|error| {
            HarnessError::new(
                ErrorCode::ConfigReadError,
                format!("{} cannot be inspected: {error}", path.display()),
            )
        })?;
        if metadata.len() > MAX_COMMAND_BYTES {
            return Err(HarnessError::new(
                ErrorCode::FrameLimitExceeded,
                format!("prompt command {} is over 256 KiB", path.display()),
            ));
        }
        let Ok(document) = std::fs::read_to_string(&path) else {
            continue;
        };
        let name = path.file_name().map_or_else(
            || String::from("command"),
            |name| name.to_string_lossy().trim_end_matches(".md").to_owned(),
        );
        if loaded.iter().any(|command| command.name == name) {
            continue;
        }
        let (front, body) = split_front_matter(&document);
        let mut description = front_value(front, "description").unwrap_or_default();
        if description.is_empty()
            && let Some(first_line) = body.lines().map(str::trim).find(|line| !line.is_empty())
        {
            // prime: the first non-empty line, cut at 60 characters.
            let characters: Vec<char> = first_line.chars().collect();
            description = if characters.len() > 60 {
                characters[..60].iter().collect::<String>() + "..."
            } else {
                first_line.to_owned()
            };
        }
        loaded.push(PromptCommand {
            name,
            description,
            argument_hint: front_value(front, "argument-hint").unwrap_or_default(),
            body: body.to_owned(),
            source: source.to_owned(),
        });
    }
    Ok(loaded)
}

/// Expand a template with prime-agent's `substitute_args`: `$1`..`$N`, `$@`,
/// `$ARGUMENTS` and `${@:N[:L]}` slices; argument values are not substituted
/// again.
pub fn expand(command: &PromptCommand, raw_arguments: &str) -> String {
    substitute_args(&command.body, &parse_command_args(raw_arguments))
}

/// Parse command arguments respecting quoted strings (bash-style); an
/// explicitly quoted token is an argument even when empty.
fn parse_command_args(args_string: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_quote: Option<char> = None;
    let mut quoted = false;
    for character in args_string.chars() {
        if let Some(quote) = in_quote {
            if character == quote {
                in_quote = None;
            } else {
                current.push(character);
            }
        } else if character == '"' || character == '\'' {
            in_quote = Some(character);
            quoted = true;
        } else if character.is_whitespace() {
            if !current.is_empty() || quoted {
                args.push(std::mem::take(&mut current));
                quoted = false;
            }
        } else {
            current.push(character);
        }
    }
    if !current.is_empty() || quoted {
        args.push(current);
    }
    args
}

/// prime-agent's `substitute_args`.
fn substitute_args(content: &str, args: &[String]) -> String {
    let all_args = args.join(" ");
    let mut out = String::with_capacity(content.len());
    let characters: Vec<char> = content.chars().collect();
    let mut index = 0;
    while index < characters.len() {
        if characters[index] != '$' {
            out.push(characters[index]);
            index += 1;
            continue;
        }
        let Some(&next) = characters.get(index + 1) else {
            out.push('$');
            break;
        };
        if next == '{' {
            if let Some(close) = characters[index + 2..].iter().position(|c| *c == '}') {
                let inner: String = characters[index + 2..index + 2 + close].iter().collect();
                if let Some(rest) = inner.strip_prefix("@:") {
                    let mut parts = rest.splitn(2, ':');
                    let start = parts
                        .next()
                        .unwrap_or("0")
                        .parse::<usize>()
                        .unwrap_or(1)
                        .saturating_sub(1);
                    let length = parts.next().and_then(|length| length.parse::<usize>().ok());
                    let slice: Vec<&str> = args
                        .iter()
                        .skip(start)
                        .take(length.unwrap_or(usize::MAX))
                        .map(String::as_str)
                        .collect();
                    out.push_str(&slice.join(" "));
                    index += 2 + close + 1;
                    continue;
                }
            }
            out.push('$');
            index += 1;
            continue;
        }
        if characters[index..].starts_with(&['$', 'A', 'R', 'G', 'U', 'M', 'E', 'N', 'T', 'S']) {
            out.push_str(&all_args);
            index += "$ARGUMENTS".len();
            continue;
        }
        if next == '@' {
            out.push_str(&all_args);
            index += 2;
            continue;
        }
        if next.is_ascii_digit() {
            let mut end = index + 1;
            while end < characters.len() && characters[end].is_ascii_digit() {
                end += 1;
            }
            let number: String = characters[index + 1..end].iter().collect();
            let position: usize = number.parse().unwrap_or(0);
            if let Some(value) = position
                .checked_sub(1)
                .and_then(|position| args.get(position))
            {
                out.push_str(value);
            }
            index = end;
            continue;
        }
        out.push('$');
        index += 1;
    }
    out
}

#[cfg(test)]
mod prompt_template_tests {
    use super::*;

    #[test]
    fn command_args_quoting() {
        assert_eq!(
            parse_command_args("a \"b c\" 'd e' \"\""),
            vec!["a", "b c", "d e", ""]
        );
        assert_eq!(parse_command_args("  "), Vec::<String>::new());
    }

    #[test]
    fn substitutes_positional_and_slices() {
        let args = vec!["one".to_string(), "two".to_string(), "three".to_string()];
        assert_eq!(substitute_args("$1 and $2", &args), "one and two");
        assert_eq!(substitute_args("$@", &args), "one two three");
        assert_eq!(substitute_args("$ARGUMENTS", &args), "one two three");
        assert_eq!(substitute_args("${@:2}", &args), "two three");
        assert_eq!(substitute_args("${@:2:1}", &args), "two");
        assert_eq!(substitute_args("${@:9}", &args), "");
        assert_eq!(substitute_args("$5", &args), "");
        assert_eq!(substitute_args("plain text", &args), "plain text");
        assert_eq!(substitute_args("cost is $10", &args), "cost is ");
    }

    #[test]
    fn prompts_load_from_the_prompts_folders_before_commands() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let config = temporary.path().join("config");
        let workspace = temporary.path().join("workspace");
        std::fs::create_dir_all(config.join("prompts")).expect("prompts");
        std::fs::create_dir_all(config.join("commands")).expect("commands");
        std::fs::create_dir_all(&workspace).expect("workspace");
        std::fs::write(config.join("prompts/fix.md"), "Fix ${@:2} in $1").expect("fix");
        std::fs::write(config.join("commands/fix.md"), "older fix").expect("old fix");
        std::fs::write(config.join("commands/legacy.md"), "Legacy $ARGUMENTS").expect("legacy");
        let commands = commands(&config, &workspace, false).expect("commands");
        let fix = commands
            .iter()
            .find(|command| command.name == "fix")
            .expect("fix");
        assert_eq!(fix.description, "Fix ${@:2} in $1");
        assert_eq!(expand(fix, "src a b"), "Fix a b in src");
        assert!(commands.iter().any(|command| command.name == "legacy"));
        assert_eq!(
            commands
                .iter()
                .filter(|command| command.name == "fix")
                .count(),
            1
        );
    }
}

fn split_front_matter(document: &str) -> (&str, &str) {
    let mut lines = document.split_inclusive('\n');
    if lines.next().is_none_or(|line| line.trim() != "---") {
        return ("", document);
    }
    let mut offset = 4;
    for line in lines {
        if line.trim() == "---" {
            let front = &document[4..offset];
            let body = &document[offset + line.len()..];
            return (front, body);
        }
        offset += line.len();
    }
    ("", document)
}

fn front_value(front: &str, key: &str) -> Option<String> {
    front.lines().find_map(|line| {
        let (candidate, value) = line.trim().split_once(':')?;
        (candidate.trim() == key).then(|| value.trim().trim_matches(['"', '\'']).to_owned())
    })
}
