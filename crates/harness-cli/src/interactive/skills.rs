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
    let mut roots = vec![TrustedSkillRoot::new(bundled, SkillSource::Builtin)];
    roots.extend(self::roots(
        config_dir,
        workspace,
        environment,
        project_trusted,
    ));
    SkillCatalog::discover(&roots)
        .map_err(|error| HarnessError::new(error.code(), error.to_string()))
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
            if std::fs::read(path).map_err(|error| bundled_io_error(&error))? == bytes {
                return Ok(());
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(bundled_io_error(&error)),
    }
    std::fs::create_dir_all(path.parent().expect("bundled file has a parent"))
        .map_err(|error| bundled_io_error(&error))?;
    std::fs::write(path, bytes).map_err(|error| bundled_io_error(&error))
}

fn bundled_io_error(error: &std::io::Error) -> HarnessError {
    HarnessError::new(
        ErrorCode::SkillUnavailable,
        format!("bundled skills cannot be prepared: {error}"),
    )
}

pub fn activate(
    catalog: &SkillCatalog,
    name: &str,
    sequence: u64,
) -> Result<SkillActivation, HarnessError> {
    catalog
        .activate(name, None, sequence)
        .map_err(|error| HarnessError::new(error.code(), error.to_string()))
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
                if !entry.model_invocable {
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
        for name in [
            "brainstorming",
            "deep-research",
            "find-skills",
            "github-deep-research",
        ] {
            let entry = catalog.entry(name).expect("bundled skill");
            assert_eq!(entry.source, SkillSource::Builtin);
            assert!(catalog.activate(name, None, 1).is_ok());
        }
        assert!(catalog.entries().len() >= 18);
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
        assert!(
            catalog.activate("release", None, 1).is_ok(),
            "the user still can"
        );
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

pub fn commands(
    config_dir: &Path,
    workspace: &Path,
    project_trusted: bool,
) -> Result<Vec<PromptCommand>, HarnessError> {
    let mut roots = vec![(config_dir.join("commands"), "user config")];
    if project_trusted {
        roots.push((workspace.join(".harness/commands"), "trusted project"));
    }
    let mut loaded = Vec::new();
    for (root, source) in roots {
        if !root.is_dir() {
            continue;
        }
        let mut files = std::fs::read_dir(&root)
            .map_err(|error| {
                HarnessError::new(
                    ErrorCode::ConfigReadError,
                    format!("{} cannot be read: {error}", root.display()),
                )
            })?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.is_file() && path.extension().is_some_and(|ext| ext == "md"))
            .collect::<Vec<_>>();
        files.sort();
        for path in files {
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
            let document = std::fs::read_to_string(&path).map_err(|error| {
                HarnessError::new(
                    ErrorCode::ConfigReadError,
                    format!("{} cannot be read: {error}", path.display()),
                )
            })?;
            let name = path.file_stem().map_or_else(
                || String::from("command"),
                |stem| stem.to_string_lossy().into_owned(),
            );
            let (front, body) = split_front_matter(&document);
            let description = front_value(front, "description").unwrap_or_default();
            let argument_hint = front_value(front, "argument-hint").unwrap_or_default();
            if !name
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
            {
                continue;
            }
            loaded.push(PromptCommand {
                name,
                description,
                argument_hint,
                body: body.to_owned(),
                source: source.to_owned(),
            });
        }
    }
    loaded.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then(left.source.cmp(&right.source))
    });
    loaded.dedup_by(|left, right| left.name == right.name);
    Ok(loaded)
}

pub fn expand(command: &PromptCommand, raw_arguments: &str) -> String {
    let values = split_arguments(raw_arguments);
    let mut result = command.body.replace("$ARGUMENTS", raw_arguments);
    for index in 1..=9 {
        let value = values.get(index - 1).map_or("", String::as_str);
        result = result.replace(&format!("${index}"), value);
    }
    result
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

fn split_arguments(raw: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    for character in raw.chars() {
        match (quote, character) {
            (Some(active), ch) if ch == active => quote = None,
            (None, '"' | '\'') => quote = Some(character),
            (None, ch) if ch.is_whitespace() => {
                if !current.is_empty() {
                    values.push(std::mem::take(&mut current));
                }
            }
            (_, ch) => current.push(ch),
        }
    }
    if !current.is_empty() {
        values.push(current);
    }
    values
}
