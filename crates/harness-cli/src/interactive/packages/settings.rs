//! The slice of prime-agent's `SettingsManager` the package manager reads and
//! writes: the `packages` arrays and the `skills`/`prompts`/`themes` resource
//! arrays of the user settings (`<config dir>/settings.json`, beside ha's
//! other prime settings) and the project settings
//! (`<workspace>/.harness/settings.json`). Every other key in those files is
//! kept as it is.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::Value;

/// prime-agent's `CONFIG_DIR_NAME` (`.prime/agent`): ha's project directory.
pub const CONFIG_DIR_NAME: &str = ".harness";

/// `bundledSkills` (prime's per-bundled-skill switches).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BundledSkillsSettings {
    pub websearch: Option<bool>,
}

/// The package-manager keys of one settings scope.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Settings {
    pub packages: Option<Vec<Value>>,
    pub skills: Option<Vec<String>>,
    pub prompts: Option<Vec<String>>,
    pub themes: Option<Vec<String>>,
    pub npm_command: Option<Vec<String>>,
    pub enable_builtin_skills: Option<bool>,
    pub bundled_skills: Option<BundledSkillsSettings>,
}

impl Settings {
    fn from_document(document: &Value) -> Self {
        let strings = |key: &str| {
            document.get(key).and_then(Value::as_array).map(|entries| {
                entries
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
        };
        Self {
            packages: document.get("packages").and_then(Value::as_array).cloned(),
            skills: strings("skills"),
            prompts: strings("prompts"),
            themes: strings("themes"),
            npm_command: strings("npmCommand"),
            enable_builtin_skills: document.get("enableBuiltinSkills").and_then(Value::as_bool),
            bundled_skills: document
                .get("bundledSkills")
                .and_then(Value::as_object)
                .map(|bundled| BundledSkillsSettings {
                    websearch: bundled.get("websearch").and_then(Value::as_bool),
                }),
        }
    }

    /// prime's `deepMerge(global, project)` for these keys: a value the
    /// project sets wins.
    fn merged(global: &Self, project: &Self) -> Self {
        Self {
            packages: project.packages.clone().or_else(|| global.packages.clone()),
            skills: project.skills.clone().or_else(|| global.skills.clone()),
            prompts: project.prompts.clone().or_else(|| global.prompts.clone()),
            themes: project.themes.clone().or_else(|| global.themes.clone()),
            npm_command: project
                .npm_command
                .clone()
                .or_else(|| global.npm_command.clone()),
            enable_builtin_skills: project
                .enable_builtin_skills
                .or(global.enable_builtin_skills),
            bundled_skills: match (&global.bundled_skills, &project.bundled_skills) {
                (_, Some(project)) if project.websearch.is_some() => Some(project.clone()),
                (global, _) => global.clone(),
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scope {
    Global,
    Project,
}

/// Both settings scopes, loaded from and written to their files.
#[derive(Clone, Debug)]
pub struct SettingsManager {
    global_path: PathBuf,
    project_path: PathBuf,
    global: Settings,
    project: Settings,
    merged: Settings,
}

impl SettingsManager {
    /// The settings of `agent_dir` (the user scope) and of `cwd`'s
    /// `.harness` directory (the project scope).
    pub fn create(cwd: impl AsRef<Path>, agent_dir: impl AsRef<Path>) -> Self {
        let mut manager = Self {
            global_path: agent_dir.as_ref().join("settings.json"),
            project_path: cwd.as_ref().join(CONFIG_DIR_NAME).join("settings.json"),
            global: Settings::default(),
            project: Settings::default(),
            merged: Settings::default(),
        };
        let _ = manager.reload();
        manager
    }

    /// Read both scopes again. A missing or unreadable file is an empty scope.
    ///
    /// # Errors
    ///
    /// None today; the signature keeps prime's.
    pub fn reload(&mut self) -> Result<()> {
        self.global = Settings::from_document(&read_document(&self.global_path));
        self.project = Settings::from_document(&read_document(&self.project_path));
        self.merged = Settings::merged(&self.global, &self.project);
        Ok(())
    }

    #[must_use]
    pub fn settings(&self) -> &Settings {
        &self.merged
    }

    #[must_use]
    pub fn global_settings(&self) -> &Settings {
        &self.global
    }

    #[must_use]
    pub fn project_settings(&self) -> &Settings {
        &self.project
    }

    /// Replace the `packages` array in the user settings file.
    pub fn set_packages(&mut self, packages: Vec<Value>) {
        self.global.packages = Some(packages.clone());
        self.persist(Scope::Global, "packages", Value::Array(packages));
    }

    /// Replace the `packages` array in the project settings file.
    pub fn set_project_packages(&mut self, packages: Vec<Value>) {
        self.project.packages = Some(packages.clone());
        self.persist(Scope::Project, "packages", Value::Array(packages));
    }

    /// Replace one resource array (`skills`/`prompts`/`themes`) in the user
    /// settings file.
    pub fn set_global_resource_array(&mut self, field: &str, values: Vec<String>) {
        if let Some(slot) = resource_slot(&mut self.global, field) {
            *slot = Some(values.clone());
            self.persist(Scope::Global, field, strings_value(values));
        }
    }

    /// Replace one resource array in the project settings file.
    pub fn set_project_resource_array(&mut self, field: &str, values: Vec<String>) {
        if let Some(slot) = resource_slot(&mut self.project, field) {
            *slot = Some(values.clone());
            self.persist(Scope::Project, field, strings_value(values));
        }
    }

    fn persist(&mut self, scope: Scope, field: &str, value: Value) {
        let path = match scope {
            Scope::Global => &self.global_path,
            Scope::Project => &self.project_path,
        };
        if let Err(error) = write_field(path, field, value) {
            // prime records a settings write error and carries on; ha has no
            // settings error channel, so it goes to the log.
            eprintln!("settings: {error:#}");
        }
        self.merged = Settings::merged(&self.global, &self.project);
    }
}

fn resource_slot<'a>(
    settings: &'a mut Settings,
    field: &str,
) -> Option<&'a mut Option<Vec<String>>> {
    match field {
        "skills" => Some(&mut settings.skills),
        "prompts" => Some(&mut settings.prompts),
        "themes" => Some(&mut settings.themes),
        _ => None,
    }
}

fn strings_value(values: Vec<String>) -> Value {
    Value::Array(values.into_iter().map(Value::String).collect())
}

fn read_document(path: &Path) -> Value {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| Value::Object(serde_json::Map::new()))
}

fn write_field(path: &Path, field: &str, value: Value) -> Result<()> {
    let mut document = read_document(path);
    if let Some(object) = document.as_object_mut() {
        object.insert(field.to_owned(), value);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let text = serde_json::to_string_pretty(&document)?;
    let staged = path.with_extension("json.staged");
    std::fs::write(&staged, text).with_context(|| format!("writing {}", staged.display()))?;
    std::fs::rename(&staged, path).with_context(|| format!("replacing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_keep_the_other_keys_and_the_project_wins() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let agent = temporary.path().join("agent");
        let cwd = temporary.path().join("cwd");
        std::fs::create_dir_all(&agent).expect("agent dir");
        std::fs::write(
            agent.join("settings.json"),
            r#"{"rlmMaxDepth": 2, "npmCommand": ["pnpm"]}"#,
        )
        .expect("settings");
        let mut manager = SettingsManager::create(&cwd, &agent);
        manager.set_packages(vec![Value::String("npm:demo".into())]);
        manager.set_project_resource_array("skills", vec!["-skills/x/SKILL.md".into()]);
        let global: Value = serde_json::from_str(
            &std::fs::read_to_string(agent.join("settings.json")).expect("read"),
        )
        .expect("json");
        assert_eq!(global["rlmMaxDepth"], 2);
        assert_eq!(global["packages"], serde_json::json!(["npm:demo"]));
        assert_eq!(
            manager.settings().npm_command,
            Some(vec!["pnpm".to_owned()])
        );
        assert_eq!(
            manager.settings().skills,
            Some(vec!["-skills/x/SKILL.md".to_owned()])
        );
        assert!(cwd.join(".harness/settings.json").is_file());
    }
}
