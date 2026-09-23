//! Resolve G05 settings into the existing P3 `ToolPolicy`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::{io::Write, path::Path};

use harness_tools::{PolicyMode, ToolPatternRule, ToolPolicy, validate_tool_pattern};
use harness_types::{ErrorCode, HarnessConfigV2, HarnessError, PermissionsConfigV2};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub fn build_tool_policy(
    approval: &str,
    allow_patterns: &[String],
    deny_patterns: &[String],
    session_mode: Option<PolicyMode>,
) -> Result<ToolPolicy, HarnessError> {
    let mode = match session_mode {
        Some(mode) => mode,
        None => approval.parse::<PolicyMode>()?,
    };
    let mut rules = Vec::with_capacity(allow_patterns.len() + deny_patterns.len());
    for pattern in allow_patterns {
        validate_tool_pattern(pattern)?;
        rules.push(ToolPatternRule::allow(pattern, "configured allow rule"));
    }
    for pattern in deny_patterns {
        validate_tool_pattern(pattern)?;
        rules.push(ToolPatternRule::deny(pattern, "configured deny rule"));
    }
    Ok(ToolPolicy::default().with_mode(mode).with_tool_rules(rules))
}

/// Save a user-confirmed allow pattern to this checkout's ignored local file.
/// The caller must obtain a second explicit Enter confirmation first.
pub fn persist_allow_rule(workspace_root: &Path, pattern: &str) -> Result<(), HarnessError> {
    validate_tool_pattern(pattern)?;
    let directory = workspace_root.join(".harness");
    std::fs::create_dir_all(&directory).map_err(|error| {
        HarnessError::new(
            ErrorCode::StorageWriteFailed,
            format!("cannot create local config directory: {error}"),
        )
    })?;
    let local_path = directory.join("config.local.toml");
    reject_symlink(&local_path)?;
    let mut config = if local_path.exists() {
        let metadata = std::fs::metadata(&local_path).map_err(|error| config_read_error(&error))?;
        if metadata.len() > 1024 * 1024 {
            return Err(HarnessError::new(
                ErrorCode::ConfigReadError,
                "local config exceeds the 1 MiB safety limit",
            ));
        }
        let text =
            std::fs::read_to_string(&local_path).map_err(|error| config_read_error(&error))?;
        toml::from_str::<HarnessConfigV2>(&text).map_err(|_| {
            HarnessError::new(
                ErrorCode::ConfigParseError,
                "local config is not a valid v2 file; it was left unchanged",
            )
        })?
    } else {
        HarnessConfigV2 {
            schema_version: 2,
            ..HarnessConfigV2::default()
        }
    };
    if config.schema_version != 2 {
        return Err(HarnessError::new(
            ErrorCode::UnsupportedSchemaVersion,
            "local config schema must be version 2; it was left unchanged",
        ));
    }
    let permissions = config
        .permissions
        .get_or_insert_with(PermissionsConfigV2::default);
    if !permissions.allow.iter().any(|current| current == pattern) {
        permissions.allow.push(pattern.to_owned());
    }
    config.validate()?;
    let serialized = toml::to_string_pretty(&config).map_err(|_| {
        HarnessError::new(
            ErrorCode::StorageWriteFailed,
            "local config could not be serialized; it was left unchanged",
        )
    })?;
    replace_file(&local_path, serialized.as_bytes())?;

    let gitignore = directory.join(".gitignore");
    reject_symlink(&gitignore)?;
    let current = if gitignore.exists() {
        std::fs::read_to_string(&gitignore).map_err(|error| config_read_error(&error))?
    } else {
        String::new()
    };
    if !current
        .lines()
        .any(|line| line.trim() == "config.local.toml")
    {
        let mut updated = current;
        if !updated.is_empty() && !updated.ends_with(['\n', '\r']) {
            updated.push('\n');
        }
        updated.push_str("config.local.toml\n");
        replace_file(&gitignore, updated.as_bytes())?;
    }
    Ok(())
}

fn reject_symlink(path: &Path) -> Result<(), HarnessError> {
    if std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err(HarnessError::new(
            ErrorCode::StorageWriteFailed,
            "refusing to follow a symlink while saving local permissions",
        ));
    }
    Ok(())
}

fn config_read_error(error: &std::io::Error) -> HarnessError {
    HarnessError::new(
        ErrorCode::ConfigReadError,
        format!("local config could not be read: {error}"),
    )
}

fn replace_file(path: &Path, content: &[u8]) -> Result<(), HarnessError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(
        ".{}.{}.{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id(),
        sequence
    ));
    let backup = parent.join(format!(
        ".{}.{}.{}.bak",
        path.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id(),
        sequence
    ));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|error| {
            HarnessError::new(
                ErrorCode::StorageWriteFailed,
                format!("local config temporary could not be created: {error}"),
            )
        })?;
    file.write_all(content).map_err(|error| {
        HarnessError::new(
            ErrorCode::StorageWriteFailed,
            format!("local config temporary could not be written: {error}"),
        )
    })?;
    file.sync_all().map_err(|error| {
        HarnessError::new(
            ErrorCode::StorageWriteFailed,
            format!("local config temporary could not be synchronized: {error}"),
        )
    })?;
    drop(file);

    if path.exists() {
        std::fs::rename(path, &backup).map_err(|error| {
            HarnessError::new(
                ErrorCode::StorageWriteFailed,
                format!("local config could not be staged for replacement: {error}"),
            )
        })?;
        if let Err(error) = std::fs::rename(&temporary, path) {
            let _ = std::fs::rename(&backup, path);
            let _ = std::fs::remove_file(&temporary);
            return Err(HarnessError::new(
                ErrorCode::StorageWriteFailed,
                format!("local config could not be replaced: {error}"),
            ));
        }
        std::fs::remove_file(&backup).map_err(|error| {
            HarnessError::new(
                ErrorCode::StorageWriteFailed,
                format!("local config was replaced but its backup remains: {error}"),
            )
        })?;
    } else {
        std::fs::rename(&temporary, path).map_err(|error| {
            let _ = std::fs::remove_file(&temporary);
            HarnessError::new(
                ErrorCode::StorageWriteFailed,
                format!("local config could not be installed: {error}"),
            )
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{build_tool_policy, persist_allow_rule};
    use crate::interactive::config::{
        ConfigExplainEntry, ConfigLayer, ResolvedConfig, ResolvedProviderConfig,
    };
    use harness_tools::PolicyEffect;
    use std::collections::BTreeMap;

    fn config(approval: &str) -> ResolvedConfig {
        ResolvedConfig {
            provider: ResolvedProviderConfig {
                id: "test".to_owned(),
                protocol: "openai_chat".to_owned(),
                endpoint: "http://localhost".to_owned(),
                model: "test".to_owned(),
                api_key_env: "TEST_KEY".to_owned(),
                thinking: "off".to_owned(),
            },
            profile: None,
            approval: approval.to_owned(),
            allow_rules: vec!["run_shell(cargo test *)".to_owned()],
            deny_rules: vec!["run_shell(cargo test secrets *)".to_owned()],
            model_prices: BTreeMap::new(),
            context_window_tokens: 8192,
            context_window_notice: None,
            output_reservation_tokens: 1024,
            compaction_reserve_tokens: 16_384,
            retry_after_max_seconds: 30,
            hooks: Vec::new(),
            mcp_servers: BTreeMap::new(),
            project_trusted: false,
            bell: false,
            explain: vec![ConfigExplainEntry {
                key: "approval".to_owned(),
                value: approval.to_owned(),
                layer: ConfigLayer::User,
                reason: None,
            }],
            project_config_reason: None,
        }
    }

    #[test]
    fn g05_permissions_resolve_rules_inside_the_single_tool_policy() {
        let config = config("full-auto");
        let policy = build_tool_policy(
            &config.approval,
            &config.allow_rules,
            &config.deny_rules,
            None,
        )
        .expect("valid policy");
        assert_eq!(policy.mode().as_str(), "full-auto");
        assert_eq!(policy.tool_rules().len(), 2);
        assert_eq!(policy.tool_rules()[1].effect, PolicyEffect::Deny);
    }

    #[test]
    fn g05_always_allow_persistence_creates_local_file_and_gitignore() {
        let root = tempfile::tempdir().expect("workspace");
        persist_allow_rule(root.path(), "run_shell(cargo test *)").expect("rule persists");
        let local = std::fs::read_to_string(root.path().join(".harness/config.local.toml"))
            .expect("local config exists");
        assert!(local.contains("run_shell(cargo test *)"), "{local}");
        assert_eq!(
            std::fs::read_to_string(root.path().join(".harness/.gitignore"))
                .expect("gitignore exists"),
            "config.local.toml\n"
        );
    }

    #[test]
    fn g05_invalid_rule_patterns_fail_closed() {
        let mut invalid = config("ask");
        invalid.allow_rules = vec!["run_shell".to_owned()];
        assert!(
            build_tool_policy(
                &invalid.approval,
                &invalid.allow_rules,
                &invalid.deny_rules,
                None,
            )
            .is_err()
        );
    }
}
