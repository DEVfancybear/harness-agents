//! Trusted skill and prompt-command roots for one workspace.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use harness_extensions::{
    MAX_SKILL_CATALOG_ENTRIES, SkillActivation, SkillCatalog, SkillCatalogEntry, SkillSource,
    TrustedSkillRoot,
};
use harness_tools::{
    CodingToolAction, ExternalToolCatalog, ExternalToolDispatcher, ExternalTools,
    ToolDispatchAuthorization, ToolOutput,
};
use harness_types::{ErrorCode, HarnessError};
use serde_json::{Value, json};

use super::paths::LaunchEnvironment;

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
    if project_trusted {
        for path in [
            workspace.join(".agents/skills"),
            workspace.join(".harness/skills"),
        ] {
            if path.is_dir() {
                roots.push(TrustedSkillRoot::new(path, SkillSource::TrustedProject));
            }
        }
    }
    roots
}

pub fn discover(
    config_dir: &Path,
    workspace: &Path,
    environment: &LaunchEnvironment,
    project_trusted: bool,
) -> Result<SkillCatalog, HarnessError> {
    let roots = roots(config_dir, workspace, environment, project_trusted);
    SkillCatalog::discover(&roots)
        .map_err(|error| HarnessError::new(error.code(), error.to_string()))
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
                    "description": "List trusted skills and their exact version digests without loading their bodies.",
                    "parameters": {"type": "object", "properties": {}, "additionalProperties": false}
                }
            }),
            json!({
                "type": "function",
                "function": {
                    "name": "activate_skill",
                    "description": "Activate one trusted skill only when its name and exact catalog digest match.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "name": {"type": "string", "enum": self.catalog.entries().iter().map(|entry| entry.name.clone()).collect::<Vec<_>>()},
                            "digest": {"type": "string", "description": "Exact sha256 digest returned by list_skills for this name."}
                        },
                        "required": ["name", "digest"],
                        "additionalProperties": false
                    }
                }
            }),
        ]
    }

    fn resolve(&self, name: &str, arguments: &Value) -> Option<CodingToolAction> {
        matches!(name, "list_skills" | "activate_skill").then(|| CodingToolAction::ExternalTool {
            plugin_id: "skill".to_owned(),
            tool_name: name.to_owned(),
            arguments: arguments.clone(),
            parent_invocation_id: None,
            timeout_ms: 5_000,
        })
    }
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
                if object.len() != 2 {
                    return Err(HarnessError::new(
                        ErrorCode::InvalidPayload,
                        "activate_skill requires only name and digest",
                    ));
                }
                let name = object.get("name").and_then(Value::as_str).ok_or_else(|| {
                    HarnessError::new(ErrorCode::InvalidPayload, "skill name must be a string")
                })?;
                let digest = object
                    .get("digest")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        HarnessError::new(
                            ErrorCode::InvalidPayload,
                            "skill digest must be a string",
                        )
                    })?;
                let entry = self.catalog.entry(name).ok_or_else(|| {
                    HarnessError::new(
                        ErrorCode::SkillUnavailable,
                        format!("no trusted skill named {name} is in this catalogue"),
                    )
                })?;
                if entry.digest.as_str() != digest {
                    return Err(HarnessError::new(
                        ErrorCode::SchemaVersionMismatch,
                        format!("skill {name} does not match the supplied catalog digest"),
                    ));
                }
                Ok(())
            }
            _ => Err(HarnessError::new(
                ErrorCode::PolicyDenied,
                "skill tool target is unavailable",
            )),
        }
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
                        "skills": self.catalog.entries().iter().map(|entry| json!({
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

    use super::{SkillHost, SkillToolDispatcher, commands, expand, metadata_lines};

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
        assert!(metadata_lines(catalog.entries(), 24).len() <= 24);
    }
}

pub fn metadata_lines(entries: &[SkillCatalogEntry], max_bytes: usize) -> String {
    let mut output = String::new();
    for entry in entries {
        let line = format!("- {}: {}\n", entry.name, entry.description);
        if output.len().saturating_add(line.len()) > max_bytes {
            let remaining = max_bytes.saturating_sub(output.len());
            output.push_str(prefix_bytes(&line, remaining));
            break;
        }
        output.push_str(&line);
    }
    output
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

fn prefix_bytes(value: &str, limit: usize) -> &str {
    let mut end = value.len().min(limit);
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    &value[..end]
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
