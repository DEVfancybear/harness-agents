//! Non-secret configuration loading for the interactive launch.
//!
//! The strict P0 configuration contract is unchanged: it carries no provider or
//! credential setting, so loading it can never activate runtime work. A missing
//! file is a first run rather than an error, and a corrupt file is reported with
//! its location instead of being silently replaced by defaults.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use harness_tools::ConfiguredToolHook;
use harness_types::{
    ErrorCode, HarnessConfig, HarnessConfigV2, HarnessError, McpServerConfigV2, ProviderConfigV2,
};
use serde::{Deserialize, Serialize};

/// The context window assumed for a model no table knows, in tokens.
///
/// prime-agent's model registry gives a custom model `contextWindow: 128000`; ha used
/// 8192, which is smaller than almost any model still served and pushed the first
/// request of a turn over budget and into compaction.
pub const UNKNOWN_MODEL_CONTEXT_WINDOW: u64 = 128_000;

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
    model: "deepseek-v4-flash",
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
    /// The model `/model` chose.
    Selection,
    Project,
    Local,
    Environment,
    Cli,
}

impl ConfigLayer {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::User => "user",
            Self::Selection => "selection",
            Self::Project => "project(trust)",
            Self::Local => "project(local)",
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
    pub allowed_tools: Vec<String>,
    pub disallowed_tools: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ResolvedProviderConfig {
    pub id: String,
    pub protocol: String,
    pub endpoint: String,
    pub model: String,
    pub api_key_env: String,
    pub thinking: String,
    /// How the model takes a thinking level, from the catalog (`deepseek`,
    /// `qwen`); `None` lets the adapter decide from the provider.
    pub thinking_format: Option<String>,
}

/// The model `/model` chose, as prime-agent keeps its default model in its
/// settings: saved beside the user config, applied right after it, and still
/// overridden by a trusted project, a profile, the environment and `--model`.
///
/// It carries the transport resolved from the catalog when it was chosen, so
/// resolving the configuration never needs the catalog.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Selection {
    pub provider: String,
    pub model: String,
    pub protocol: String,
    pub endpoint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_format: Option<String>,
}

/// The file the selection lives in, beside the user config.
#[must_use]
pub fn selection_path(user_path: &Path) -> PathBuf {
    user_path.with_file_name("selection.json")
}

/// The saved selection; a missing or unreadable file is no selection.
#[must_use]
pub fn load_selection(user_path: &Path) -> Option<Selection> {
    let text = std::fs::read_to_string(selection_path(user_path)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Save the selection for this and later launches.
pub fn save_selection(user_path: &Path, selection: &Selection) -> Result<(), HarnessError> {
    let path = selection_path(user_path);
    let failed = |error: &dyn std::fmt::Display| {
        HarnessError::new(
            ErrorCode::StorageOpenFailed,
            format!(
                "model selection {} could not be saved: {error}",
                path.display()
            ),
        )
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| failed(&error))?;
    }
    let text = serde_json::to_string_pretty(selection).map_err(|error| failed(&error))?;
    let staged = path.with_extension("json.staged");
    std::fs::write(&staged, text).map_err(|error| failed(&error))?;
    std::fs::rename(&staged, &path).map_err(|error| failed(&error))
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
    pub allow_rules: Vec<String>,
    pub deny_rules: Vec<String>,
    pub model_prices: BTreeMap<String, super::cost::ModelPrice>,
    pub context_window_tokens: u64,
    pub context_window_notice: Option<String>,
    pub output_reservation_tokens: u64,
    pub compaction_reserve_tokens: u64,
    pub retry_after_max_seconds: u64,
    pub hooks: Vec<ConfiguredToolHook>,
    pub mcp_servers: BTreeMap<String, McpServerConfigV2>,
    pub project_trusted: bool,
    pub bell: bool,
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
        thinking_format: None,
    };
    let mut entries = BTreeMap::new();
    let mut model_prices = BTreeMap::new();
    let mut model_context_windows = BTreeMap::new();
    let mut retry_after_max_seconds = 30_u64;
    let mut output_reservation_tokens = 1024_u64;
    let mut compaction_reserve_tokens = 16_384_u64;
    let mut hooks = Vec::new();
    let mut mcp_servers = BTreeMap::new();
    let mut notify_command = None;
    let mut bell = false;
    let mut approval = "ask".to_owned();
    let mut approval_layer = ConfigLayer::Default;
    let mut allow_rules = Vec::new();
    let mut deny_rules = Vec::new();
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
    set_explain(
        &mut entries,
        "limits.output_reservation_tokens",
        &output_reservation_tokens.to_string(),
        ConfigLayer::Default,
        None,
    );
    set_explain(
        &mut entries,
        "limits.compaction_reserve_tokens",
        &compaction_reserve_tokens.to_string(),
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
        apply_models(
            &mut model_prices,
            &mut model_context_windows,
            config,
            ConfigLayer::User,
            &mut entries,
        );
        apply_context_limits(
            &mut output_reservation_tokens,
            &mut compaction_reserve_tokens,
            config,
            ConfigLayer::User,
            &mut entries,
        );
        apply_retry_limit(
            &mut retry_after_max_seconds,
            config,
            ConfigLayer::User,
            &mut entries,
        );
        apply_permissions(
            &mut approval,
            &mut approval_layer,
            &mut allow_rules,
            &mut deny_rules,
            config,
            ConfigLayer::User,
            &mut entries,
        )?;
        apply_hooks(
            &mut hooks,
            &mut notify_command,
            &mut bell,
            config,
            ConfigLayer::User,
        )?;
        mcp_servers.extend(config.mcp_servers.clone());
    }
    // The servers `/mcp add` saved beside the user config, at the user layer: a
    // trusted project's servers still override them.
    mcp_servers.extend(super::mcp_config::load(user_path));
    if let Some(selection) = load_selection(user_path) {
        apply_selection(
            &mut provider,
            &mut model_context_windows,
            &selection,
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
                &mut model_context_windows,
                &config,
                ConfigLayer::Project,
                &mut entries,
            );
            apply_context_limits(
                &mut output_reservation_tokens,
                &mut compaction_reserve_tokens,
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
            apply_permissions(
                &mut approval,
                &mut approval_layer,
                &mut allow_rules,
                &mut deny_rules,
                &config,
                ConfigLayer::Project,
                &mut entries,
            )?;
            apply_hooks(
                &mut hooks,
                &mut notify_command,
                &mut bell,
                &config,
                ConfigLayer::Project,
            )?;
            mcp_servers.extend(config.mcp_servers.clone());
        }
    } else if project_path.exists() {
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

    // `config.local.toml` is an explicit per-user file inside this checkout.
    // Shared `config.toml` remains gated by trust; local choices are loaded
    // regardless so a confirmed always-allow rule works on the next turn.
    if let Some(config) = load_v2_layer(&local_path)? {
        if let Some(provider_patch) = &config.provider {
            apply_provider_patch(
                &mut provider,
                provider_patch,
                ConfigLayer::Local,
                &mut entries,
            );
        }
        apply_models(
            &mut model_prices,
            &mut model_context_windows,
            &config,
            ConfigLayer::Local,
            &mut entries,
        );
        apply_context_limits(
            &mut output_reservation_tokens,
            &mut compaction_reserve_tokens,
            &config,
            ConfigLayer::Local,
            &mut entries,
        );
        apply_retry_limit(
            &mut retry_after_max_seconds,
            &config,
            ConfigLayer::Local,
            &mut entries,
        );
        apply_permissions(
            &mut approval,
            &mut approval_layer,
            &mut allow_rules,
            &mut deny_rules,
            &config,
            ConfigLayer::Local,
            &mut entries,
        )?;
        mcp_servers.extend(config.mcp_servers.clone());
        if let Some(value) = config.ui.as_ref().and_then(|ui| ui.bell) {
            bell = value;
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
        if trusted
            && let Some(config) = load_v2_layer(&project_path)?
            && let Some(value) = config.profiles.get(selected)
        {
            profile_patch = Some(value.clone());
            profile_layer = ConfigLayer::Project;
        }
        if let Some(config) = load_v2_layer(&local_path)?
            && let Some(value) = config.profiles.get(selected)
        {
            profile_patch = Some(value.clone());
            profile_layer = ConfigLayer::Local;
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
    if let Some(value) = env_approval {
        approval = value;
        approval_layer = ConfigLayer::Environment;
    }
    if let Some(value) = &overrides.approval {
        approval.clone_from(value);
        approval_layer = ConfigLayer::Cli;
    }
    if !matches!(approval.as_str(), "ask" | "auto-edit" | "full-auto") {
        return Err(HarnessError::new(
            ErrorCode::ConfigParseError,
            "approval mode must be ask, auto-edit, or full-auto",
        ));
    }
    set_explain(&mut entries, "approval", &approval, approval_layer, None);
    if !overrides.allowed_tools.is_empty() {
        allow_rules.extend(overrides.allowed_tools.iter().cloned());
        explain_permission_rules(
            &mut entries,
            "permissions.allow",
            &overrides.allowed_tools,
            ConfigLayer::Cli,
        );
        set_explain(
            &mut entries,
            "permissions.allow",
            &format!("{} (CLI temporary)", overrides.allowed_tools.join(", ")),
            ConfigLayer::Cli,
            None,
        );
    }
    if !overrides.disallowed_tools.is_empty() {
        deny_rules.extend(overrides.disallowed_tools.iter().cloned());
        explain_permission_rules(
            &mut entries,
            "permissions.deny",
            &overrides.disallowed_tools,
            ConfigLayer::Cli,
        );
        set_explain(
            &mut entries,
            "permissions.deny",
            &format!("{} (CLI temporary)", overrides.disallowed_tools.join(", ")),
            ConfigLayer::Cli,
            None,
        );
    }

    let (context_window_tokens, context_window_layer, context_window_reason) = if let Some((
        value,
        layer,
    )) =
        model_context_windows.get(&provider.model).copied()
    {
        (value, layer, None)
    } else {
        (
            UNKNOWN_MODEL_CONTEXT_WINDOW,
            ConfigLayer::Default,
            Some(format!(
                "notice: no context window is known for {}; using {UNKNOWN_MODEL_CONTEXT_WINDOW}",
                provider.model
            )),
        )
    };
    let context_window_notice = context_window_reason
        .as_deref()
        .filter(|reason| reason.contains("notice:"))
        .map(str::to_owned);
    set_explain(
        &mut entries,
        "runtime.context_window_tokens",
        &context_window_tokens.to_string(),
        context_window_layer,
        context_window_reason,
    );

    if let Some(command) = notify_command {
        hooks.push(ConfiguredToolHook {
            event: "notification".to_owned(),
            matcher: None,
            command,
            args: Vec::new(),
            timeout_seconds: 60,
            source: "user/trusted project [ui].notify_command".to_owned(),
        });
    }

    let explain = entries.into_values().collect();
    Ok(ResolvedConfig {
        provider,
        profile,
        approval,
        allow_rules,
        deny_rules,
        model_prices,
        context_window_tokens,
        context_window_notice,
        output_reservation_tokens,
        compaction_reserve_tokens,
        retry_after_max_seconds,
        hooks,
        mcp_servers,
        project_trusted: trusted,
        bell,
        explain,
        project_config_reason: project_reason,
    })
}

fn apply_permissions(
    approval: &mut String,
    approval_layer: &mut ConfigLayer,
    allow_rules: &mut Vec<String>,
    deny_rules: &mut Vec<String>,
    config: &HarnessConfigV2,
    layer: ConfigLayer,
    entries: &mut BTreeMap<String, ConfigExplainEntry>,
) -> Result<(), HarnessError> {
    let Some(permissions) = &config.permissions else {
        return Ok(());
    };
    if let Some(mode) = &permissions.mode {
        if !matches!(mode.as_str(), "ask" | "auto-edit" | "full-auto") {
            return Err(HarnessError::new(
                ErrorCode::ConfigParseError,
                "permissions.mode must be ask, auto-edit, or full-auto",
            ));
        }
        approval.clone_from(mode);
        *approval_layer = layer;
        set_explain(entries, "permissions.mode", mode, layer, None);
    }
    allow_rules.extend(
        permissions
            .allow
            .iter()
            .filter(|rule| !rule.trim().is_empty())
            .cloned(),
    );
    deny_rules.extend(
        permissions
            .deny
            .iter()
            .filter(|rule| !rule.trim().is_empty())
            .cloned(),
    );
    if !permissions.allow.is_empty() {
        set_explain(
            entries,
            "permissions.allow",
            &allow_rules.join(", "),
            layer,
            None,
        );
    }
    explain_permission_rules(entries, "permissions.allow", &permissions.allow, layer);
    if !permissions.deny.is_empty() {
        set_explain(
            entries,
            "permissions.deny",
            &deny_rules.join(", "),
            layer,
            None,
        );
    }
    explain_permission_rules(entries, "permissions.deny", &permissions.deny, layer);
    Ok(())
}

fn apply_hooks(
    hooks: &mut Vec<ConfiguredToolHook>,
    notify_command: &mut Option<String>,
    bell: &mut bool,
    config: &HarnessConfigV2,
    layer: ConfigLayer,
) -> Result<(), HarnessError> {
    for (event, value) in &config.hooks {
        if !matches!(
            event.as_str(),
            "pre_tool_use" | "post_tool_use" | "stop" | "notification"
        ) {
            return Err(HarnessError::new(
                ErrorCode::ConfigParseError,
                format!("unsupported hook event {event:?}"),
            ));
        }
        for entry in value {
            hooks.push(ConfiguredToolHook {
                event: event.clone(),
                matcher: entry.matcher.clone(),
                command: entry.command.clone(),
                args: entry.args.clone(),
                timeout_seconds: entry.timeout_seconds.unwrap_or(60),
                source: layer.as_str().to_owned(),
            });
        }
    }
    if let Some(ui) = &config.ui {
        if let Some(value) = ui.bell {
            *bell = value;
        }
        if let Some(command) = &ui.notify_command {
            if command.trim().is_empty() {
                return Err(HarnessError::new(
                    ErrorCode::ConfigParseError,
                    "ui.notify_command must name an executable",
                ));
            }
            *notify_command = Some(command.clone());
        }
    }
    Ok(())
}

fn explain_permission_rules(
    entries: &mut BTreeMap<String, ConfigExplainEntry>,
    key: &str,
    rules: &[String],
    layer: ConfigLayer,
) {
    let prefix = format!("{key}.rule.");
    let first_index = entries
        .keys()
        .filter(|key| key.starts_with(&prefix))
        .count();
    for (offset, rule) in rules
        .iter()
        .filter(|rule| !rule.trim().is_empty())
        .enumerate()
    {
        let index = first_index + offset;
        set_explain(entries, &format!("{prefix}{index:04}"), rule, layer, None);
    }
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
    context_windows: &mut BTreeMap<String, (u64, ConfigLayer)>,
    config: &HarnessConfigV2,
    layer: ConfigLayer,
    entries: &mut BTreeMap<String, ConfigExplainEntry>,
) {
    for (model, value) in &config.models {
        if let Some(window) = value.context_window {
            context_windows.insert(model.clone(), (window, layer));
            set_explain(
                entries,
                &format!("models.{model}.context_window"),
                &window.to_string(),
                layer,
                None,
            );
        }
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

fn apply_context_limits(
    output_reservation_tokens: &mut u64,
    compaction_reserve_tokens: &mut u64,
    config: &HarnessConfigV2,
    layer: ConfigLayer,
    entries: &mut BTreeMap<String, ConfigExplainEntry>,
) {
    if let Some(value) = config
        .limits
        .as_ref()
        .and_then(|limits| limits.output_reservation_tokens)
    {
        *output_reservation_tokens = value;
        set_explain(
            entries,
            "limits.output_reservation_tokens",
            &value.to_string(),
            layer,
            None,
        );
    }
    if let Some(value) = config
        .limits
        .as_ref()
        .and_then(|limits| limits.compaction_reserve_tokens)
    {
        *compaction_reserve_tokens = value;
        set_explain(
            entries,
            "limits.compaction_reserve_tokens",
            &value.to_string(),
            layer,
            None,
        );
    }
}

/// A model's context window from the model catalog, when the configuration did
/// not set one: the notice a missing window leaves is how that is told apart.
pub fn apply_catalog_context_window(resolved: &mut ResolvedConfig, data_dir: &Path) {
    if resolved.context_window_notice.is_none() {
        return;
    }
    let reference = format!("{}/{}", resolved.provider.id, resolved.provider.model);
    if let Some(window) = super::providers::Catalog::load(data_dir)
        .find(&reference)
        .and_then(|model| model.context_window)
    {
        resolved.context_window_tokens = window;
        resolved.context_window_notice = None;
        if let Some(entry) = resolved
            .explain
            .iter_mut()
            .find(|entry| entry.key == "runtime.context_window_tokens")
        {
            entry.value = window.to_string();
            entry.reason = Some("from the model catalog".to_owned());
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

fn apply_selection(
    provider: &mut ResolvedProviderConfig,
    model_context_windows: &mut BTreeMap<String, (u64, ConfigLayer)>,
    selection: &Selection,
    entries: &mut BTreeMap<String, ConfigExplainEntry>,
) {
    provider.id.clone_from(&selection.provider);
    provider.protocol.clone_from(&selection.protocol);
    provider.endpoint.clone_from(&selection.endpoint);
    provider.model.clone_from(&selection.model);
    provider.api_key_env = super::providers::env_variables(&selection.provider)
        .first()
        .map_or_else(String::new, |variable| (*variable).to_owned());
    provider
        .thinking_format
        .clone_from(&selection.thinking_format);
    if let Some(window) = selection.context_window {
        model_context_windows.insert(selection.model.clone(), (window, ConfigLayer::Selection));
    }
    explain_provider(
        entries,
        provider,
        ConfigLayer::Selection,
        Some("chosen with /model".to_owned()),
    );
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
    fn g07_context_window_comes_from_config_then_catalog_then_default_with_notice() {
        let temp = tempfile::tempdir().expect("temp root");
        let root = temp.path().join("project");
        std::fs::create_dir_all(&root).expect("project root");
        let user = temp.path().join("user.toml");
        std::fs::write(
            &user,
            "schema_version = 2\n[models.deepseek-v4-flash]\ncontext_window = 24576\n",
        )
        .expect("configured model window");
        let configured = resolve_layers(
            &user,
            &root,
            &super::super::paths::LaunchEnvironment::default(),
            &ConfigOverrides::default(),
        )
        .expect("configured window resolves");
        let source = configured
            .explain
            .iter()
            .find(|entry| entry.key == "runtime.context_window_tokens")
            .expect("context window is explained");
        assert_eq!(source.value, "24576");
        assert_eq!(source.layer, super::ConfigLayer::User);

        std::fs::write(
            &user,
            "schema_version = 2\n[provider]\nmodel = 'deepseek-v4-pro'\n",
        )
        .expect("known model");
        let mut known = resolve_layers(
            &user,
            &root,
            &super::super::paths::LaunchEnvironment::default(),
            &ConfigOverrides::default(),
        )
        .expect("known model window resolves");
        // The window of a model the configuration does not size is the catalog's.
        super::apply_catalog_context_window(&mut known, &temp.path().join("data"));
        let source = known
            .explain
            .iter()
            .find(|entry| entry.key == "runtime.context_window_tokens")
            .expect("known model window is explained");
        assert_eq!(source.value, "1000000");
        assert!(known.context_window_notice.is_none());
        assert!(
            source
                .reason
                .as_deref()
                .is_some_and(|reason| reason.contains("catalog"))
        );

        let unknown = resolve_layers(
            &user,
            &root,
            &super::super::paths::LaunchEnvironment::default(),
            &ConfigOverrides {
                model: Some("local-unknown-model".to_owned()),
                ..ConfigOverrides::default()
            },
        )
        .expect("unknown model uses the safe fallback");
        let source = unknown
            .explain
            .iter()
            .find(|entry| entry.key == "runtime.context_window_tokens")
            .expect("fallback window is explained");
        assert_eq!(source.value, "128000");
        assert!(
            source
                .reason
                .as_deref()
                .is_some_and(|reason| reason.contains("notice"))
        );
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
    fn g09_untrusted_project_hooks_do_not_run() {
        let temp = tempfile::tempdir().expect("temp root");
        let root = temp.path().join("project");
        std::fs::create_dir_all(root.join(".harness")).expect("project config dir");
        let user = temp.path().join("user.toml");
        std::fs::write(&user, "schema_version = 2\n").expect("user config");
        std::fs::write(
            root.join(".harness/config.toml"),
            "schema_version = 2\n[[hooks.pre_tool_use]]\nmatcher = 'run_shell'\ncommand = 'must-not-run'\n",
        ).expect("untrusted hook config");
        std::fs::write(
            root.join(".harness/config.local.toml"),
            "schema_version = 2\n[[hooks.pre_tool_use]]\nmatcher = 'run_shell'\ncommand = 'local-must-not-run'\n",
        ).expect("local hook config");

        let resolved = resolve_layers(
            &user,
            &root,
            &super::super::paths::LaunchEnvironment::default(),
            &ConfigOverrides::default(),
        )
        .expect("untrusted project hook is ignored");

        assert!(
            resolved.hooks.is_empty(),
            "untrusted commands must never execute"
        );
        assert!(
            resolved
                .project_config_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("trust"))
        );
    }

    #[test]
    fn g09_trusted_project_hooks_load_but_local_hooks_do_not() {
        let temp = tempfile::tempdir().expect("temp root");
        let root = temp.path().join("project");
        std::fs::create_dir_all(root.join(".harness")).expect("project config dir");
        let user = temp.path().join("user.toml");
        let trusted_root = root.canonicalize().expect("canonical root");
        std::fs::write(
            &user,
            format!(
                "schema_version = 2\n[trust]\nprojects = [{:?}]\n[[hooks.stop]]\ncommand = 'user-stop'\n",
                trusted_root.to_string_lossy()
            ),
        ).expect("trusted user config");
        std::fs::write(
            root.join(".harness/config.toml"),
            "schema_version = 2\n[[hooks.pre_tool_use]]\nmatcher = 'run_shell|run_process'\ncommand = 'trusted-hook'\nargs = ['--quiet']\ntimeout_seconds = 3\n[ui]\nbell = true\nnotify_command = 'notify-user'\n",
        ).expect("trusted project config");
        std::fs::write(
            root.join(".harness/config.local.toml"),
            "schema_version = 2\n[[hooks.stop]]\ncommand = 'local-stop'\n[ui]\nbell = false\nnotify_command = 'local-notify'\n",
        ).expect("local config");

        let resolved = resolve_layers(
            &user,
            &root,
            &super::super::paths::LaunchEnvironment::default(),
            &ConfigOverrides::default(),
        )
        .expect("trusted hook config resolves");

        assert!(
            resolved
                .hooks
                .iter()
                .any(|hook| hook.command == "trusted-hook" && hook.source == "project(trust)")
        );
        assert!(
            resolved
                .hooks
                .iter()
                .any(|hook| hook.command == "user-stop" && hook.source == "user")
        );
        assert!(
            resolved
                .hooks
                .iter()
                .any(|hook| hook.command == "notify-user")
        );
        assert!(
            !resolved
                .hooks
                .iter()
                .any(|hook| hook.command.starts_with("local-"))
        );
        assert!(
            !resolved.bell,
            "the explicit local UI preference wins for this machine"
        );
    }

    #[test]
    fn g09_invalid_hook_config_is_rejected() {
        let temp = tempfile::tempdir().expect("temp root");
        let root = temp.path().join("project");
        std::fs::create_dir_all(&root).expect("project root");
        let user = temp.path().join("user.toml");
        std::fs::write(
            &user,
            "schema_version = 2\n[[hooks.pre_tool_use]]\ncommand = 'bad-timeout'\ntimeout_seconds = 61\n",
        ).expect("invalid hook config");

        let error = resolve_layers(
            &user,
            &root,
            &super::super::paths::LaunchEnvironment::default(),
            &ConfigOverrides::default(),
        )
        .expect_err("hook timeout above 60 seconds is rejected");

        assert_eq!(error.code(), ErrorCode::ConfigParseError);
    }

    #[test]
    fn g05_local_permission_rules_load_without_project_trust_and_explain_the_layer() {
        let temp = tempfile::tempdir().expect("temp root");
        let root = temp.path().join("project");
        std::fs::create_dir_all(root.join(".harness")).expect("project config dir");
        let user = temp.path().join("user.toml");
        std::fs::write(&user, "schema_version = 2\n").expect("user config");
        std::fs::write(
            root.join(".harness/config.toml"),
            "schema_version = 2\n[permissions]\nallow = ['run_shell(never trusted)']\n",
        )
        .expect("untrusted shared config");
        std::fs::write(
            root.join(".harness/config.local.toml"),
            "schema_version = 2\n[permissions]\nallow = ['run_shell(cargo test *)']\n",
        )
        .expect("local per-user config");

        let resolved = resolve_layers(
            &user,
            &root,
            &super::super::paths::LaunchEnvironment::default(),
            &ConfigOverrides::default(),
        )
        .expect("local preferences load without trusting shared project config");
        assert_eq!(resolved.allow_rules, ["run_shell(cargo test *)"]);
        assert!(
            resolved.explain.iter().any(|entry| {
                entry.key.starts_with("permissions.allow.rule.")
                    && entry.value == "run_shell(cargo test *)"
                    && entry.layer == super::ConfigLayer::Local
            }),
            "the overlay can show the rule's true source layer: {:?}",
            resolved.explain
        );
        assert!(
            !resolved
                .allow_rules
                .iter()
                .any(|rule| rule.contains("never trusted")),
            "an untrusted shared config never contributes allow rules"
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
