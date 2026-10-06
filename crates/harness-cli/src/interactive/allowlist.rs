//! prime-agent's model allowlist (settings `allowedModels`): the models a
//! session may resolve to. Read from the global `settings.json` only, so a
//! project cannot weaken it; unset (or a list that trims to empty) is
//! unrestricted. A model outside it fails loudly where it is resolved - a
//! turn's model, `/model`, a delegated child's model - and ha never falls
//! back to a different model on a refusal.

use std::path::Path;

/// The `allowedModels` patterns, `None` when unrestricted.
#[must_use]
pub fn load(config_file: &Path) -> Option<Vec<String>> {
    let patterns = super::config::load_setting(config_file, "allowedModels")?
        .as_array()?
        .iter()
        .filter_map(|pattern| pattern.as_str())
        .map(str::trim)
        .filter(|pattern| !pattern.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    (!patterns.is_empty()).then_some(patterns)
}

/// prime-agent's `model_allowed`: patterns match case-insensitively against
/// the full `provider/model-id` and the bare model id; a pattern with `*`,
/// `?` or `[` globs, a plain one must match exactly.
#[must_use]
pub fn model_allowed(selector: &str, allowlist: &[String]) -> bool {
    allowlist
        .iter()
        .any(|pattern| pattern_matches(pattern, selector))
}

fn pattern_matches(pattern: &str, selector: &str) -> bool {
    let pattern = pattern.trim().to_lowercase();
    if pattern.is_empty() {
        return false;
    }
    let selector = selector.to_lowercase();
    let bare_id = selector
        .split_once('/')
        .map_or(selector.as_str(), |(_, id)| id);
    if !pattern.contains(['*', '?', '[']) {
        return pattern == selector || pattern == bare_id;
    }
    let Ok(glob) = globset::Glob::new(&pattern) else {
        return false;
    };
    let matcher = glob.compile_matcher();
    matcher.is_match(&selector) || matcher.is_match(bare_id)
}

/// prime-agent's `ModelAllowlistRefusal` text.
#[must_use]
pub fn refusal(selector: &str) -> String {
    format!(
        "Model \"{selector}\" is blocked by the model allowlist (settings \"allowedModels\"); ha never falls back to a different model. Allow it in the settings or pick an allowed model."
    )
}

/// Refuse `provider/model` when the global allowlist leaves it out.
///
/// # Errors
/// The refusal, when the model is outside the allowlist.
pub fn check(config_file: &Path, provider: &str, model: &str) -> Result<(), String> {
    let Some(allowlist) = load(config_file) else {
        return Ok(());
    };
    let selector = format!("{provider}/{model}");
    if model_allowed(&selector, &allowlist) {
        Ok(())
    } else {
        Err(refusal(&selector))
    }
}

#[cfg(test)]
mod tests {
    use super::{check, model_allowed};

    #[test]
    fn exact_bare_and_glob_patterns_match_as_prime_matches_them() {
        let selector = "anthropic/claude-sonnet-4-5";
        assert!(model_allowed(
            selector,
            &["ANTHROPIC/claude-sonnet-4-5".to_owned()]
        ));
        assert!(model_allowed(selector, &["claude-sonnet-4-5".to_owned()]));
        assert!(model_allowed(selector, &["anthropic/*".to_owned()]));
        assert!(model_allowed(selector, &["claude-*".to_owned()]));
        assert!(!model_allowed(selector, &["openai/*".to_owned()]));
        assert!(!model_allowed(selector, &["claude".to_owned()]));
        assert!(!model_allowed(selector, &[]));
    }

    #[test]
    fn an_unset_or_empty_list_is_unrestricted() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let config = dir.path().join("config.toml");
        assert!(check(&config, "openai", "gpt-5").is_ok());
        std::fs::write(
            dir.path().join("settings.json"),
            r#"{"allowedModels":["  "]}"#,
        )
        .expect("settings");
        assert!(check(&config, "openai", "gpt-5").is_ok());
        std::fs::write(
            dir.path().join("settings.json"),
            r#"{"allowedModels":["anthropic/*"]}"#,
        )
        .expect("settings");
        assert!(check(&config, "anthropic", "claude-opus-4-1").is_ok());
        assert!(
            check(&config, "openai", "gpt-5")
                .unwrap_err()
                .contains("blocked by the model allowlist")
        );
    }
}
