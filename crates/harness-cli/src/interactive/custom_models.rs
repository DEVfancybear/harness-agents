//! Models you add yourself, after prime-agent's `models.json`.
//!
//! The file sits beside the user config. Its schema is prime-agent's:
//! `{"providers": {"<id>": {"name", "baseUrl", "apiKey", "api", "models": [..],
//! "modelOverrides": {"<model id>": {..}}}}}`. Comments (`//` and `/* */`) are
//! allowed. `apiKey` is the name of the environment variable that holds the key,
//! never the key itself, so the file can be shared without leaking one.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde::Deserialize;

use super::providers::{Compat, Cost, Model};

/// The file name, beside the user config.
pub const FILE_NAME: &str = "models.json";

/// Defaults prime-agent gives a model that leaves them out.
const DEFAULT_CONTEXT_WINDOW: u64 = 128_000;
const DEFAULT_MAX_TOKENS: u64 = 16_384;

fn location() -> &'static Mutex<Option<PathBuf>> {
    static LOCATION: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();
    LOCATION.get_or_init(|| Mutex::new(None))
}

/// Read `models.json` from beside this user config from now on.
pub fn use_beside(user_config: &Path) {
    let path = user_config.with_file_name(FILE_NAME);
    if let Ok(mut location) = location().lock() {
        *location = Some(path);
    }
}

/// Where `models.json` is read from, once [`use_beside`] has been called.
#[must_use]
pub fn path() -> Option<PathBuf> {
    location().lock().ok().and_then(|location| location.clone())
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ModelsFile {
    #[serde(default)]
    providers: BTreeMap<String, ProviderEntry>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ProviderEntry {
    #[serde(default)]
    #[allow(
        dead_code,
        reason = "prime-agent's display name, accepted for compatibility"
    )]
    name: Option<String>,
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    api: Option<String>,
    #[serde(default)]
    models: Vec<ModelDef>,
    #[serde(default)]
    model_overrides: BTreeMap<String, ModelOverride>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ModelDef {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    api: Option<String>,
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    reasoning: Option<bool>,
    #[serde(default)]
    input: Option<Vec<String>>,
    #[serde(default)]
    cost: Option<CostDef>,
    #[serde(default)]
    context_window: Option<u64>,
    #[serde(default)]
    max_tokens: Option<u64>,
    #[serde(default)]
    compat: Option<Compat>,
}

/// A full cost: all four prices, as prime-agent requires.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct CostDef {
    input: f64,
    output: f64,
    cache_read: f64,
    cache_write: f64,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct CostOverride {
    input: Option<f64>,
    output: Option<f64>,
    cache_read: Option<f64>,
    cache_write: Option<f64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ModelOverride {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    reasoning: Option<bool>,
    #[serde(default)]
    input: Option<Vec<String>>,
    #[serde(default)]
    cost: Option<CostOverride>,
    #[serde(default)]
    context_window: Option<u64>,
    #[serde(default)]
    max_tokens: Option<u64>,
    #[serde(default)]
    compat: Option<Compat>,
}

/// What `models.json` adds and changes.
#[derive(Debug, Default)]
pub struct CustomModels {
    models: Vec<Model>,
    /// `(provider, model id)` → override.
    overrides: Vec<(String, String, ModelOverride)>,
    /// provider → the variable its key is read from.
    keys: BTreeMap<String, String>,
}

/// Read `models.json`. A missing file is `Ok(None)`; a file that cannot be used
/// is an error naming the problem, and none of it applies.
pub fn load(path: &Path, builtin: &[Model]) -> Result<Option<CustomModels>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    parse(&text, builtin).map(Some)
}

fn parse(text: &str, builtin: &[Model]) -> Result<CustomModels, String> {
    let file: ModelsFile =
        serde_json::from_str(&strip_comments(text)).map_err(|error| error.to_string())?;
    let mut custom = CustomModels::default();
    for (provider, entry) in file.providers {
        let known = builtin.iter().any(|model| model.provider == provider);
        if let Some(variable) = entry.api_key.as_deref() {
            if !is_variable_name(variable) {
                return Err(format!(
                    "provider \"{provider}\": apiKey must name the environment variable that holds the key"
                ));
            }
            custom.keys.insert(provider.clone(), variable.to_owned());
        }
        if !known && !entry.models.is_empty() {
            if entry.base_url.is_none() {
                return Err(format!(
                    "provider \"{provider}\": \"baseUrl\" is required when defining custom models"
                ));
            }
            if entry.api_key.is_none() {
                return Err(format!(
                    "provider \"{provider}\": \"apiKey\" is required when defining custom models"
                ));
            }
        }
        for definition in entry.models {
            custom.models.push(build(
                &provider,
                entry.base_url.as_ref(),
                entry.api.as_ref(),
                definition,
                builtin,
            )?);
        }
        for (id, change) in entry.model_overrides {
            check_limits(&provider, &id, change.context_window, change.max_tokens)?;
            custom.overrides.push((provider.clone(), id, change));
        }
    }
    Ok(custom)
}

fn build(
    provider: &str,
    base_url: Option<&String>,
    api: Option<&String>,
    definition: ModelDef,
    builtin: &[Model],
) -> Result<Model, String> {
    let id = definition.id;
    if id.trim().is_empty() {
        return Err(format!(
            "provider \"{provider}\": a model has an empty \"id\""
        ));
    }
    check_limits(
        provider,
        &id,
        definition.context_window,
        definition.max_tokens,
    )?;
    // A built-in provider lends its transport to a model that leaves it out.
    let sibling = builtin.iter().find(|model| model.provider == provider);
    let api = definition
        .api
        .or_else(|| api.cloned())
        .or_else(|| sibling.map(|model| model.api.clone()))
        .ok_or_else(|| format!("provider \"{provider}\", model \"{id}\": no \"api\""))?;
    let base_url = definition
        .base_url
        .or_else(|| base_url.cloned())
        .or_else(|| sibling.map(|model| model.base_url.clone()))
        .ok_or_else(|| format!("provider \"{provider}\", model \"{id}\": no \"baseUrl\""))?;
    let model = Model {
        name: definition.name.unwrap_or_else(|| id.clone()),
        id,
        api,
        provider: provider.to_owned(),
        base_url,
        reasoning: definition.reasoning.unwrap_or(false),
        input: definition.input.unwrap_or_else(|| vec!["text".to_owned()]),
        cost: Some(definition.cost.map_or_else(Cost::default, |cost| Cost {
            input: cost.input,
            output: cost.output,
            cache_read: cost.cache_read,
            cache_write: cost.cache_write,
        })),
        context_window: Some(definition.context_window.unwrap_or(DEFAULT_CONTEXT_WINDOW)),
        max_tokens: Some(definition.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS)),
        compat: definition.compat,
        thinking_level_map: None,
        key_variable: None,
    };
    if model.protocol().is_none() {
        return Err(format!(
            "provider \"{provider}\", model \"{}\": api \"{}\" is not one of openai-completions, anthropic-messages, openai-responses, openai-codex-responses",
            model.id, model.api
        ));
    }
    Ok(model)
}

fn check_limits(
    provider: &str,
    id: &str,
    context_window: Option<u64>,
    max_tokens: Option<u64>,
) -> Result<(), String> {
    if context_window == Some(0) {
        return Err(format!(
            "provider \"{provider}\", model \"{id}\": invalid contextWindow"
        ));
    }
    if max_tokens == Some(0) {
        return Err(format!(
            "provider \"{provider}\", model \"{id}\": invalid maxTokens"
        ));
    }
    Ok(())
}

fn is_variable_name(value: &str) -> bool {
    let mut chars = value.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|char| char.is_ascii_alphanumeric() || char == '_')
}

impl CustomModels {
    /// Add the custom models to a catalog: one with the provider and id of a
    /// catalog model replaces it. Then apply the overrides, field by field.
    pub fn apply(&self, models: &mut Vec<Model>) {
        for custom in &self.models {
            let mut custom = custom.clone();
            custom.key_variable = self.keys.get(&custom.provider).cloned();
            match models
                .iter_mut()
                .find(|model| model.provider == custom.provider && model.id == custom.id)
            {
                Some(model) => *model = custom,
                None => models.push(custom),
            }
        }
        for (provider, id, change) in &self.overrides {
            let Some(model) = models
                .iter_mut()
                .find(|model| &model.provider == provider && &model.id == id)
            else {
                continue;
            };
            if let Some(name) = &change.name {
                model.name.clone_from(name);
            }
            if let Some(reasoning) = change.reasoning {
                model.reasoning = reasoning;
            }
            if let Some(input) = &change.input {
                model.input.clone_from(input);
            }
            if let Some(cost) = &change.cost {
                let mut merged = model.cost.unwrap_or_default();
                merged.input = cost.input.unwrap_or(merged.input);
                merged.output = cost.output.unwrap_or(merged.output);
                merged.cache_read = cost.cache_read.unwrap_or(merged.cache_read);
                merged.cache_write = cost.cache_write.unwrap_or(merged.cache_write);
                model.cost = Some(merged);
            }
            if change.context_window.is_some() {
                model.context_window = change.context_window;
            }
            if change.max_tokens.is_some() {
                model.max_tokens = change.max_tokens;
            }
            if change.compat.is_some() {
                model.compat.clone_from(&change.compat);
            }
        }
        for model in models.iter_mut() {
            if model.key_variable.is_none() {
                model.key_variable = self.keys.get(&model.provider).cloned();
            }
        }
    }
}

/// JSON with `//` and `/* */` comments taken out; strings are left alone.
fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_string = false;
    while let Some(char) = chars.next() {
        if in_string {
            out.push(char);
            if char == '\\' {
                if let Some(next) = chars.next() {
                    out.push(next);
                }
            } else if char == '"' {
                in_string = false;
            }
            continue;
        }
        match (char, chars.peek()) {
            ('"', _) => {
                in_string = true;
                out.push(char);
            }
            ('/', Some('/')) => {
                for next in chars.by_ref() {
                    if next == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            ('/', Some('*')) => {
                chars.next();
                let mut previous = ' ';
                for next in chars.by_ref() {
                    if previous == '*' && next == '/' {
                        break;
                    }
                    if next == '\n' {
                        out.push('\n');
                    }
                    previous = next;
                }
            }
            _ => out.push(char),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::parse;
    use crate::interactive::providers::Catalog;

    fn builtin() -> Vec<crate::interactive::providers::Model> {
        Catalog::bundled().models().to_vec()
    }

    #[test]
    fn q10_custom_model_is_added_with_defaults() {
        let custom = parse(
            r#"{
              // a local server
              "providers": {
                "local": {
                  "baseUrl": "http://127.0.0.1:8080/v1",
                  "apiKey": "LOCAL_KEY",
                  "api": "openai-completions",
                  "models": [{ "id": "tiny" }]
                }
              }
            }"#,
            &builtin(),
        )
        .expect("the file is valid");
        let mut models = builtin();
        custom.apply(&mut models);
        let model = models
            .iter()
            .find(|model| model.reference() == "local/tiny")
            .expect("the custom model is added");
        assert_eq!(model.name, "tiny");
        assert!(!model.reasoning);
        assert_eq!(model.input, ["text"]);
        assert_eq!(model.context_window, Some(128_000));
        assert_eq!(model.max_tokens, Some(16_384));
        assert_eq!(model.cost.map(|cost| cost.input), Some(0.0));
        assert_eq!(model.key_variable.as_deref(), Some("LOCAL_KEY"));
        assert_eq!(
            model.endpoint(),
            "http://127.0.0.1:8080/v1/chat/completions"
        );
    }

    #[test]
    fn q10_override_replaces_only_given_fields() {
        let before = builtin()
            .into_iter()
            .find(|model| model.reference() == "deepseek/deepseek-v4-flash")
            .expect("a built-in model");
        let custom = parse(
            r#"{"providers": {"deepseek": {"modelOverrides": {
                "deepseek-v4-flash": {"contextWindow": 64000, "cost": {"output": 9}}
            }}}}"#,
            &builtin(),
        )
        .expect("the file is valid");
        let mut models = builtin();
        custom.apply(&mut models);
        let after = models
            .iter()
            .find(|model| model.reference() == "deepseek/deepseek-v4-flash")
            .expect("still there");
        assert_eq!(after.context_window, Some(64_000));
        assert_eq!(after.cost.map(|cost| cost.output), Some(9.0));
        assert_eq!(
            after.cost.map(|cost| cost.input),
            before.cost.map(|cost| cost.input)
        );
        assert_eq!(after.max_tokens, before.max_tokens);
        assert_eq!(after.base_url, before.base_url);
        assert_eq!(after.thinking_level_map, before.thinking_level_map);
    }

    #[test]
    fn q10_same_id_replaces_the_builtin_model() {
        let custom = parse(
            r#"{"providers": {"deepseek": {"models": [{"id": "deepseek-v4-flash", "name": "Mine"}]}}}"#,
            &builtin(),
        )
        .expect("a built-in provider needs no baseUrl");
        let mut models = builtin();
        let count = models.len();
        custom.apply(&mut models);
        assert_eq!(models.len(), count);
        let model = models
            .iter()
            .find(|model| model.reference() == "deepseek/deepseek-v4-flash")
            .expect("replaced");
        assert_eq!(model.name, "Mine");
        assert_eq!(model.base_url, "https://api.deepseek.com");
    }

    #[test]
    fn q10_custom_provider_needs_base_url_and_key() {
        let no_url = parse(
            r#"{"providers": {"local": {"apiKey": "K", "api": "openai-completions", "models": [{"id": "m"}]}}}"#,
            &builtin(),
        )
        .expect_err("baseUrl is required");
        assert!(no_url.contains("baseUrl"), "{no_url}");
        let no_key = parse(
            r#"{"providers": {"local": {"baseUrl": "http://x", "api": "openai-completions", "models": [{"id": "m"}]}}}"#,
            &builtin(),
        )
        .expect_err("apiKey is required");
        assert!(no_key.contains("apiKey"), "{no_key}");
        let literal = parse(
            r#"{"providers": {"local": {"baseUrl": "http://x", "apiKey": "sk-abc.def", "api": "openai-completions", "models": [{"id": "m"}]}}}"#,
            &builtin(),
        )
        .expect_err("a key value is refused");
        assert!(literal.contains("environment variable"), "{literal}");
        assert!(!literal.contains("sk-abc"), "the key must not be echoed");
    }

    #[test]
    fn q10_invalid_file_is_an_error_that_names_the_problem() {
        let error = parse(r#"{"providers": {"local": {"bogus": 1}}}"#, &builtin())
            .expect_err("unknown field");
        assert!(error.contains("bogus"), "{error}");
        let error = parse(
            r#"{"providers": {"local": {"baseUrl": "http://x", "apiKey": "K", "api": "grpc", "models": [{"id": "m"}]}}}"#,
            &builtin(),
        )
        .expect_err("unsupported api");
        assert!(error.contains("grpc"), "{error}");
        let error = parse(
            r#"{"providers": {"deepseek": {"modelOverrides": {"deepseek-v4-flash": {"maxTokens": 0}}}}}"#,
            &builtin(),
        )
        .expect_err("zero max tokens");
        assert!(error.contains("maxTokens"), "{error}");
    }

    #[test]
    fn comments_inside_strings_are_kept() {
        assert_eq!(
            super::strip_comments("{\"a\": \"http://x\" /* c */} // end"),
            "{\"a\": \"http://x\" } "
        );
    }
}
