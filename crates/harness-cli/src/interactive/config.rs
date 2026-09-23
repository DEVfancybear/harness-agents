//! Non-secret configuration loading for the interactive launch.
//!
//! The strict P0 configuration contract is unchanged: it carries no provider or
//! credential setting, so loading it can never activate runtime work. A missing
//! file is a first run rather than an error, and a corrupt file is reported with
//! its location instead of being silently replaced by defaults.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use harness_types::{ErrorCode, HarnessConfig, HarnessConfigV2, HarnessError, ProviderConfigV2};
use serde::Serialize;

/// Provider-neutral preset values for the existing default `DeepSeek` connection.
/// Adapter code consumes the resolved fields; it does not know these defaults.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProviderPreset {
    pub id: &'static str,
    pub protocol: &'static str,
    pub endpoint: &'static str,
    pub model: &'static str,
    pub api_key_env: &'static str,
    pub thinking: &'static str,
}

pub const DEEPSEEK_PRESET: ProviderPreset = ProviderPreset {
    id: "deepseek",
    protocol: "openai_chat",
    endpoint: "https://api.deepseek.com/chat/completions",
    model: "deepseek-flash",
    api_key_env: "DEEPSEEK_API_KEY",
    thinking: "off",
};
#[cfg(test)]
pub const DEEPSEEK_ENDPOINT: &str = DEEPSEEK_PRESET.endpoint;
#[cfg(test)]
pub const DEEPSEEK_MODEL: &str = DEEPSEEK_PRESET.model;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigLayer {
    Default,
    User,
    Project,
    Environment,
    Cli,
}

impl ConfigLayer {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::User => "user",
            Self::Project => "project(trust)",
            Self::Environment => "env",
            Self::Cli => "cli",
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ConfigOverrides {
    pub model: Option<String>,
    pub profile: Option<String>,
    pub approval: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ResolvedProviderConfig {
    pub id: String,
    pub protocol: String,
    pub endpoint: String,
    pub model: String,
    pub api_key_env: String,
    pub thinking: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ConfigExplainEntry {
    pub key: String,
    pub value: String,
    pub layer: ConfigLayer,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedConfig {
    pub provider: ResolvedProviderConfig,
    pub profile: Option<String>,
    pub approval: String,
    pub model_prices: BTreeMap<String, super::cost::ModelPrice>,
    pub retry_after_max_seconds: u64,
    pub explain: Vec<ConfigExplainEntry>,
    pub project_config_reason: Option<String>,
}

#[allow(clippy::too_many_lines)] // Keeps the explicit layer precedence auditable in one place.
pub fn resolve_layers(
    user_path: &Path,
    project_root: &Path,
    environment: &super::paths::LaunchEnvironment,
    overrides: &ConfigOverrides,
) -> Result<ResolvedConfig, HarnessError> {
    let mut provider = ResolvedProviderConfig {
        id: DEEPSEEK_PRESET.id.to_owned(),
        protocol: DEEPSEEK_PRESET.protocol.to_owned(),
        endpoint: DEEPSEEK_PRESET.endpoint.to_owned(),
        model: DEEPSEEK_PRESET.model.to_owned(),
        api_key_env: DEEPSEEK_PRESET.api_key_env.to_owned(),
        thinking: DEEPSEEK_PRESET.thinking.to_owned(),
    };
    let mut entries = BTreeMap::new();
    let mut model_prices = BTreeMap::new();
    let mut retry_after_max_seconds = 30_u64;
    explain_provider(&mut entries, &provider, ConfigLayer::Default, None);
    set_explain(&mut entries, "profile", "none", ConfigLayer::Default, None);
    set_explain(&mut entries, "approval", "ask", ConfigLayer::Default, None);
    set_explain(
        &mut entries,
        "limits.max_retry_after_seconds",
        "30",
        ConfigLayer::Default,
        None,
    );

    let user = load_v2_layer(user_path)?;
    if let Some(config) = &user {
        if let Some(provider_patch) = &config.provider {
            apply_provider_patch(
                &mut provider,
                provider_patch,
                ConfigLayer::User,
                &mut entries,
            );
        }
        apply_models(&mut model_prices, config, ConfigLayer::User, &mut entries);
        apply_retry_limit(
            &mut retry_after_max_seconds,
            config,
            ConfigLayer::User,
            &mut entries,
        );
    }

    let canonical_root = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    let trusted = user
        .as_ref()
        .and_then(|config| config.trust.as_ref())
        .is_some_and(|trust| {
            trust.projects.iter().any(|trusted| {
                PathBuf::from(trusted)
                    .canonicalize()
                    .is_ok_and(|path| same_path(&path, &canonical_root))
            })
        });
    let project_path = project_root.join(".harness").join("config.toml");
    let local_path = project_root.join(".harness").join("config.local.toml");
    let mut project_reason = None;
    if trusted {
        if let Some(config) = load_v2_layer(&project_path)? {
            if let Some(provider_patch) = &config.provider {
                apply_provider_patch(
                    &mut provider,
                    provider_patch,
                    ConfigLayer::Project,
                    &mut entries,
                );
            }
            apply_models(
                &mut model_prices,
                &config,
                ConfigLayer::Project,
                &mut entries,
            );
            apply_retry_limit(
                &mut retry_after_max_seconds,
                &config,
                ConfigLayer::Project,
                &mut entries,
            );
        }
        if let Some(config) = load_v2_layer(&local_path)? {
            if let Some(provider_patch) = &config.provider {
                apply_provider_patch(
                    &mut provider,
                    provider_patch,
                    ConfigLayer::Project,
                    &mut entries,
                );
            }
            apply_models(
                &mut model_prices,
                &config,
                ConfigLayer::Project,
                &mut entries,
            );
            apply_retry_limit(
                &mut retry_after_max_seconds,
                &config,
                ConfigLayer::Project,
                &mut entries,
            );
        }
    } else if project_path.exists() || local_path.exists() {
        project_reason = Some(
            "ignored project config because this canonical root is not in trust.projects"
                .to_owned(),
        );
        for key in [
            "provider.id",
            "provider.protocol",
            "provider.endpoint",
            "provider.model",
            "provider.api_key_env",
            "provider.thinking",
        ] {
            if let Some(entry) = entries.get_mut(key) {
                entry.reason.clone_from(&project_reason);
            }
        }
    }

    let mut profile = overrides.profile.clone().or_else(|| {
        environment
            .value("HA_PROFILE")
            .map(|value| value.to_string_lossy().into_owned())
    });
    if let Some(selected) = &profile {
        let mut profile_patch = None;
        if let Some(config) = &user {
            profile_patch = config.profiles.get(selected).cloned();
        }
        let mut profile_layer = ConfigLayer::User;
        if trusted {
            if let Some(config) = load_v2_layer(&project_path)?
                && let Some(value) = config.profiles.get(selected)
            {
                profile_patch = Some(value.clone());
                profile_layer = ConfigLayer::Project;
            }
            if let Some(config) = load_v2_layer(&local_path)?
                && let Some(value) = config.profiles.get(selected)
            {
                profile_patch = Some(value.clone());
                profile_layer = ConfigLayer::Project;
            }
        }
        if let Some(patch) = profile_patch
            && let Some(provider_patch) = patch.provider
        {
            apply_provider_patch(&mut provider, &provider_patch, profile_layer, &mut entries);
        }
        set_explain(
            &mut entries,
            "profile",
            selected,
            if overrides.profile.is_some() {
                ConfigLayer::Cli
            } else if environment.value("HA_PROFILE").is_some() {
                ConfigLayer::Environment
            } else {
                profile_layer
            },
            None,
        );
    } else {
        profile = None;
    }

    apply_environment_provider(&mut provider, environment, &mut entries);
    if let Some(seconds) = environment
        .value("HA_MAX_RETRY_AFTER_SECONDS")
        .and_then(|value| value.to_string_lossy().parse::<u64>().ok())
    {
        retry_after_max_seconds = seconds.min(30);
        set_explain(
            &mut entries,
            "limits.max_retry_after_seconds",
            &retry_after_max_seconds.to_string(),
            ConfigLayer::Environment,
            None,
        );
    }
    if let Some(model) = &overrides.model {
        provider.model.clone_from(model);
        set_explain(
            &mut entries,
            "provider.model",
            model,
            ConfigLayer::Cli,
            None,
        );
    }
    let env_approval = environment
        .value("HA_APPROVAL")
        .map(|value| value.to_string_lossy().into_owned());
    let approval = overrides
        .approval
        .clone()
        .or(env_approval)
        .unwrap_or_else(|| "ask".to_owned());
    if approval != "ask" {
        return Err(HarnessError::new(
            ErrorCode::ConfigParseError,
            "only approval mode 'ask' is available before G05",
        ));
    }
    let approval_layer = if overrides.approval.is_some() {
        ConfigLayer::Cli
    } else if environment.value("HA_APPROVAL").is_some() {
        ConfigLayer::Environment
    } else {
        ConfigLayer::Default
    };
    set_explain(&mut entries, "approval", &approval, approval_layer, None);

    let explain = entries.into_values().collect();
    Ok(ResolvedConfig {
        provider,
        profile,
        approval,
        model_prices,
        retry_after_max_seconds,
        explain,
        project_config_reason: project_reason,
    })
}

fn load_v2_layer(path: &Path) -> Result<Option<HarnessConfigV2>, HarnessError> {
    if !path.exists() {
        return Ok(None);
    }
    match load(path)? {
        ConfigState::LoadedV2 { config, .. } => Ok(Some(*config)),
        ConfigState::Loaded { .. } | ConfigState::FirstRun { .. } => Ok(None),
    }
}

fn apply_models(
    prices: &mut BTreeMap<String, super::cost::ModelPrice>,
    config: &HarnessConfigV2,
    layer: ConfigLayer,
    entries: &mut BTreeMap<String, ConfigExplainEntry>,
) {
    for (model, value) in &config.models {
        if let (Some(input), Some(output)) =
            (value.input_price_per_mtok, value.output_price_per_mtok)
        {
            prices.insert(
                model.clone(),
                super::cost::ModelPrice {
                    input_per_mtok: input,
                    output_per_mtok: output,
                },
            );
            set_explain(
                entries,
                &format!("models.{model}.input_price_per_mtok"),
                &input.to_string(),
                layer,
                None,
            );
            set_explain(
                entries,
                &format!("models.{model}.output_price_per_mtok"),
                &output.to_string(),
                layer,
                None,
            );
        }
    }
}

fn apply_retry_limit(
    seconds: &mut u64,
    config: &HarnessConfigV2,
    layer: ConfigLayer,
    entries: &mut BTreeMap<String, ConfigExplainEntry>,
) {
    if let Some(value) = config
        .limits
        .as_ref()
        .and_then(|limits| limits.max_retry_after_seconds)
    {
        *seconds = value.min(30);
        set_explain(
            entries,
            "limits.max_retry_after_seconds",
            &seconds.to_string(),
            layer,
            None,
        );
    }
}

fn same_path(left: &Path, right: &Path) -> bool {
    #[cfg(windows)]
    {
        left.to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy())
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

fn apply_provider_patch(
    provider: &mut ResolvedProviderConfig,
    patch: &ProviderConfigV2,
    layer: ConfigLayer,
    entries: &mut BTreeMap<String, ConfigExplainEntry>,
) {
    macro_rules! apply {
        ($field:ident, $key:literal) => {
            if let Some(value) = &patch.$field {
                provider.$field.clone_from(value);
                set_explain(entries, $key, value, layer, None);
            }
        };
    }
    apply!(id, "provider.id");
    apply!(protocol, "provider.protocol");
    apply!(endpoint, "provider.endpoint");
    apply!(model, "provider.model");
    apply!(api_key_env, "provider.api_key_env");
    apply!(thinking, "provider.thinking");
}

fn apply_environment_provider(
    provider: &mut ResolvedProviderConfig,
    environment: &super::paths::LaunchEnvironment,
    entries: &mut BTreeMap<String, ConfigExplainEntry>,
) {
    for (variable, field, key) in [
        ("HA_PROVIDER_ID", &mut provider.id, "provider.id"),
        (
            "HA_PROVIDER_PROTOCOL",
            &mut provider.protocol,
            "provider.protocol",
        ),
        (
            "HA_PROVIDER_ENDPOINT",
            &mut provider.endpoint,
            "provider.endpoint",
        ),
        ("HA_PROVIDER_MODEL", &mut provider.model, "provider.model"),
        (
            "HA_PROVIDER_API_KEY_ENV",
            &mut provider.api_key_env,
            "provider.api_key_env",
        ),
        (
            "HA_PROVIDER_THINKING",
            &mut provider.thinking,
            "provider.thinking",
        ),
    ] {
        if let Some(value) = environment
            .value(variable)
            .filter(|value| !value.is_empty())
        {
            let value = value.to_string_lossy().into_owned();
            field.clone_from(&value);
            set_explain(entries, key, &value, ConfigLayer::Environment, None);
        }
    }
}

fn explain_provider(
    entries: &mut BTreeMap<String, ConfigExplainEntry>,
    provider: &ResolvedProviderConfig,
    layer: ConfigLayer,
    reason: Option<String>,
) {
    set_explain(entries, "provider.id", &provider.id, layer, reason.clone());
    set_explain(
        entries,
        "provider.protocol",
        &provider.protocol,
        layer,
        reason.clone(),
    );
    set_explain(
        entries,
        "provider.endpoint",
        &provider.endpoint,
        layer,
        reason.clone(),
    );
    set_explain(
        entries,
        "provider.model",
        &provider.model,
        layer,
        reason.clone(),
    );
    set_explain(
        entries,
        "provider.api_key_env",
        &provider.api_key_env,
        layer,
        reason.clone(),
    );
    set_explain(
        entries,
        "provider.thinking",
        &provider.thinking,
        layer,
        reason,
    );
}

fn set_explain(
    entries: &mut BTreeMap<String, ConfigExplainEntry>,
    key: &str,
    value: &str,
    layer: ConfigLayer,
    reason: Option<String>,
) {
    entries.insert(
        key.to_owned(),
        ConfigExplainEntry {
            key: key.to_owned(),
            value: value.to_owned(),
            layer,
            reason,
        },
    );
}

/// Configuration state visible to the app before the first request.
#[derive(Clone, Debug, PartialEq)]
pub enum ConfigState {
    /// No file yet: defaults apply and the app opens in setup state.
    FirstRun { path: PathBuf },
    /// Parsed, validated, and still free of provider or credential settings.
    Loaded {
        path: PathBuf,
        config: HarnessConfig,
    },
    /// Parsed schema v2 agent configuration.
    LoadedV2 {
        path: PathBuf,
        config: Box<HarnessConfigV2>,
    },
}

impl ConfigState {
    #[must_use]
    pub const fn is_first_run(&self) -> bool {
        matches!(self, Self::FirstRun { .. })
    }

    /// One-line description for the launch header.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::FirstRun { path } => format!("first run, defaults ({})", path.display()),
            Self::Loaded { path, config } => {
                format!(
                    "schema_version={} ({})",
                    config.schema_version,
                    path.display()
                )
            }
            Self::LoadedV2 { path, config } => {
                format!(
                    "schema_version={} ({})",
                    config.schema_version,
                    path.display()
                )
            }
        }
    }
}

/// Load the user configuration file.
pub fn load(path: &Path) -> Result<ConfigState, HarnessError> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ConfigState::FirstRun {
                path: path.to_path_buf(),
            });
        }
        Err(error) => {
            return Err(HarnessError::new(
                ErrorCode::ConfigReadError,
                format!(
                    "configuration file {} could not be read: {error}",
                    path.display()
                ),
            ));
        }
    };

    let document: toml::Value = toml::from_str(&contents)
        .map_err(|error| invalid(path, error.to_string().contains("unknown field")))?;
    let version = document
        .get("schema_version")
        .and_then(toml::Value::as_integer)
        .and_then(|value| u16::try_from(value).ok())
        .ok_or_else(|| invalid(path, false))?;
    if version == 2 {
        let config: HarnessConfigV2 =
            toml::Value::try_into(document).map_err(|error: toml::de::Error| {
                invalid(path, error.to_string().contains("unknown field"))
            })?;
        config.validate().map_err(|error| {
            HarnessError::new(
                error.code(),
                format!("configuration file {} is invalid: {error}", path.display()),
            )
        })?;
        return Ok(ConfigState::LoadedV2 {
            path: path.to_path_buf(),
            config: Box::new(config),
        });
    }
    let config: HarnessConfig =
        toml::Value::try_into(document).map_err(|error: toml::de::Error| {
            invalid(path, error.to_string().contains("unknown field"))
        })?;
    if let Err(error) = config.validate() {
        return Err(HarnessError::new(
            error.code(),
            format!("configuration file {} is invalid: {error}", path.display()),
        ));
    }
    Ok(ConfigState::Loaded {
        path: path.to_path_buf(),
        config,
    })
}

/// A corrupt file is reported with its location and a safe next step.
///
/// The raw parser message is deliberately not echoed: a TOML type error can
/// quote the value it rejected, and a value in this file could be a secret a
/// user placed in the wrong file. The explicit validate command prints details
/// on demand instead.
fn invalid(path: &Path, unknown_field: bool) -> HarnessError {
    let code = if unknown_field {
        ErrorCode::ConfigUnknownField
    } else {
        ErrorCode::ConfigParseError
    };
    let detail = if unknown_field {
        "it has a key this schema does not allow"
    } else {
        "it is not valid TOML for this schema version"
    };
    HarnessError::new(
        code,
        format!(
            "configuration file {} is invalid: {detail}; run ha config validate --config {} for details",
            path.display(),
            path.display()
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::{ConfigOverrides, ConfigState, DEEPSEEK_MODEL, load, resolve_layers};
    use harness_types::ErrorCode;
    use std::path::PathBuf;

    fn fixture(relative: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(relative)
    }

    #[test]
    fn h02_missing_configuration_is_a_first_run_not_an_error() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("config.toml");
        let state = load(&path).expect("a missing file is a first run");
        assert!(state.is_first_run());
        assert!(state.describe().contains(&path.display().to_string()));
        assert!(state.describe().contains("first run"));
    }

    #[test]
    fn h02_valid_configuration_loads_and_is_described_with_its_schema_version() {
        let state = load(&fixture("tests/fixtures/p0/config/valid.toml")).expect("valid config");
        let ConfigState::Loaded { config, .. } = &state else {
            panic!("a valid file must load, got {state:?}");
        };
        assert_eq!(config.schema_version, 1);
        assert!(state.describe().contains("schema_version=1"));
    }

    #[test]
    fn h02_unknown_field_and_unsupported_schema_keep_their_codes_and_name_the_path() {
        let unknown = load(&fixture("tests/fixtures/p0/config/unknown-field.toml"))
            .expect_err("an unknown field is rejected");
        assert_eq!(unknown.code(), ErrorCode::ConfigUnknownField);
        assert!(unknown.to_string().contains("unknown-field.toml"));

        let unsupported = load(&fixture("tests/fixtures/p0/config/unsupported-schema.toml"))
            .expect_err("an unsupported schema version is rejected");
        assert_eq!(unsupported.code(), ErrorCode::UnsupportedSchemaVersion);
        assert!(unsupported.to_string().contains("unsupported-schema.toml"));
    }

    #[test]
    fn h02_corrupt_configuration_is_actionable_and_never_replaced_by_defaults() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("config.toml");
        std::fs::write(&path, "schema_version = \"not-a-number\"\n").expect("fixture write");
        let error = load(&path).expect_err("corrupt config is an error");
        assert_eq!(error.code(), ErrorCode::ConfigParseError);
        let message = error.to_string();
        assert!(message.contains("config.toml"), "{message}");
        assert!(message.contains("ha config validate"), "{message}");
        assert!(
            !message.contains("not-a-number"),
            "a rejected value must not be echoed back: {message}"
        );
        assert_eq!(
            std::fs::read_to_string(&path).expect("file survives"),
            "schema_version = \"not-a-number\"\n",
            "a corrupt file is never overwritten"
        );
    }

    #[test]
    fn g02_v1_config_still_loads_unchanged() {
        let state =
            load(&fixture("tests/fixtures/p0/config/valid.toml")).expect("v1 remains valid");
        let ConfigState::Loaded { config, .. } = state else {
            panic!("v1 must keep its original load shape");
        };
        assert_eq!(config.schema_version, 1);
        assert_eq!(config.cli.output, harness_types::CliOutputFormat::Json);
    }

    #[test]
    fn g02_precedence_cli_over_env_over_project_over_user() {
        let temp = tempfile::tempdir().expect("temp root");
        let root = temp.path().join("project");
        std::fs::create_dir_all(root.join(".harness")).expect("project config dir");
        let user = temp.path().join("user.toml");
        let trusted =
            serde_json::to_string(&root.to_string_lossy().to_string()).expect("path serializes");
        std::fs::write(
            &user,
            format!("schema_version = 2\n[provider]\nmodel = 'user-model'\n[trust]\nprojects = [{trusted}]\n"),
        )
        .expect("user config");
        std::fs::write(
            root.join(".harness/config.toml"),
            "schema_version = 2\n[provider]\nmodel = 'project-model'\n",
        )
        .expect("project config");
        let env = super::super::paths::LaunchEnvironment::from_pairs([(
            "HA_PROVIDER_MODEL",
            "env-model",
        )]);
        let resolved = resolve_layers(
            &user,
            &root,
            &env,
            &ConfigOverrides {
                model: Some("cli-model".to_owned()),
                ..ConfigOverrides::default()
            },
        )
        .expect("configuration resolves");
        assert_eq!(resolved.provider.model, "cli-model");
        assert_eq!(
            resolved
                .explain
                .iter()
                .find(|entry| entry.key == "provider.model")
                .map(|entry| entry.layer),
            Some(super::ConfigLayer::Cli)
        );
        let env_only = resolve_layers(&user, &root, &env, &ConfigOverrides::default())
            .expect("environment layer resolves");
        assert_eq!(env_only.provider.model, "env-model");
        let project_only = resolve_layers(
            &user,
            &root,
            &super::super::paths::LaunchEnvironment::default(),
            &ConfigOverrides::default(),
        )
        .expect("project layer resolves");
        assert_eq!(project_only.provider.model, "project-model");
    }

    #[test]
    fn g02_untrusted_project_config_is_ignored_with_reason() {
        let temp = tempfile::tempdir().expect("temp root");
        let root = temp.path().join("project");
        std::fs::create_dir_all(root.join(".harness")).expect("project config dir");
        let user = temp.path().join("user.toml");
        std::fs::write(&user, "schema_version = 2\n").expect("user config");
        std::fs::write(
            root.join(".harness/config.toml"),
            "schema_version = 2\n[provider]\nmodel = 'untrusted-model'\n",
        )
        .expect("project config");
        let resolved = resolve_layers(
            &user,
            &root,
            &super::super::paths::LaunchEnvironment::default(),
            &ConfigOverrides::default(),
        )
        .expect("untrusted project is ignored");
        assert_eq!(resolved.provider.model, DEEPSEEK_MODEL);
        assert!(
            resolved
                .project_config_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("trust"))
        );
    }

    #[test]
    fn g02_config_explain_names_the_layer_for_every_key() {
        let temp = tempfile::tempdir().expect("temp root");
        let root = temp.path().join("project");
        std::fs::create_dir_all(&root).expect("project root");
        let resolved = resolve_layers(
            &temp.path().join("missing.toml"),
            &root,
            &super::super::paths::LaunchEnvironment::default(),
            &ConfigOverrides::default(),
        )
        .expect("defaults explain");
        assert!(
            !resolved.explain.is_empty(),
            "all effective keys have explain entries"
        );
        assert!(
            resolved
                .explain
                .iter()
                .all(|entry| !entry.layer.as_str().is_empty())
        );
    }

    #[test]
    fn g02_unknown_key_is_rejected_with_path() {
        let temp = tempfile::tempdir().expect("temp root");
        let root = temp.path().join("project");
        std::fs::create_dir_all(&root).expect("project root");
        let user = temp.path().join("config.toml");
        std::fs::write(
            &user,
            "schema_version = 2\nunknown_secret_key = 'do-not-echo'\n",
        )
        .expect("bad config");
        let error = resolve_layers(
            &user,
            &root,
            &super::super::paths::LaunchEnvironment::default(),
            &ConfigOverrides::default(),
        )
        .expect_err("unknown field is rejected");
        assert!(error.to_string().contains("config.toml"), "{error}");
        assert!(!error.to_string().contains("do-not-echo"), "{error}");
    }
}
