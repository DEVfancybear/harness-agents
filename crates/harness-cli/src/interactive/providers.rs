//! Providers and their models, after prime-agent's model registry.
//!
//! The model list is prime-agent's catalog (`prime-agent-catalog`,
//! `models/catalog.v1.json`), restricted to the providers this app can log in to
//! and the wire formats it speaks. A snapshot ships inside the binary; a copy
//! refreshed at most once a day is cached in the data directory. As in prime-agent,
//! the downloaded catalog can only pick transports the snapshot already knows: a
//! `(provider, api, baseUrl)` triple that is not in the snapshot is dropped, so a
//! catalog can never redirect where a credential is sent.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

/// Where prime-agent publishes its model catalog.
pub const CATALOG_URL: &str = "https://raw.githubusercontent.com/PrimeIntellect-ai/prime-agent-catalog/main/models/catalog.v1.json";

/// The snapshot compiled into the binary.
const BUNDLED: &str = include_str!("models.bundled.json");

/// The cached download, under the data directory.
const CACHE_FILE: &str = "cache/provider-model-catalog.v1.json";

/// How old the cached catalog may get before a refresh is tried.
const REFRESH_AFTER: Duration = Duration::from_hours(24);

/// How a provider is logged in to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Login {
    /// An API key, typed into a masked prompt.
    ApiKey,
    /// A browser sign-in (OAuth authorization code with PKCE).
    OAuth,
}

/// One provider `/login` offers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Provider {
    pub id: &'static str,
    /// prime-agent's display name.
    pub name: &'static str,
    /// Environment variables that carry its key, in precedence order.
    pub env: &'static [&'static str],
    pub login: Login,
    /// The model selected right after logging in, when none is chosen yet.
    pub default_model: &'static str,
    /// Where the provider answers with the account's balance, if it has one.
    pub balance_url: Option<&'static str>,
}

/// The providers, in the order `/login` lists them: sign-in first, then by name,
/// as prime-agent sorts its selector.
pub const PROVIDERS: [Provider; 6] = [
    Provider {
        id: "openai-codex",
        name: "ChatGPT Plus/Pro (Codex Subscription)",
        env: &[],
        login: Login::OAuth,
        default_model: "gpt-5.5",
        balance_url: None,
    },
    Provider {
        id: "anthropic",
        name: "Anthropic",
        env: &["ANTHROPIC_API_KEY"],
        login: Login::ApiKey,
        default_model: "claude-fable-5",
        balance_url: None,
    },
    Provider {
        id: "deepseek",
        name: "DeepSeek",
        env: &["DEEPSEEK_API_KEY", "HA_API_KEY"],
        login: Login::ApiKey,
        default_model: "deepseek-v4-flash",
        balance_url: Some("https://api.deepseek.com/user/balance"),
    },
    Provider {
        id: "openai",
        name: "OpenAI",
        env: &["OPENAI_API_KEY"],
        login: Login::ApiKey,
        default_model: "gpt-5.5",
        balance_url: None,
    },
    Provider {
        id: "opencode",
        name: "OpenCode Zen",
        env: &["OPENCODE_API_KEY"],
        login: Login::ApiKey,
        default_model: "kimi-k2.6",
        balance_url: None,
    },
    Provider {
        id: "opencode-go",
        name: "OpenCode Go",
        env: &["OPENCODE_API_KEY"],
        login: Login::ApiKey,
        default_model: "kimi-k2.6",
        balance_url: None,
    },
];

/// The account balance a provider reports (`DeepSeek`'s `/user/balance`), one
/// line per currency.
pub async fn fetch_balance(
    url: &str,
    credential: &dyn harness_providers::CredentialResolver,
) -> Result<Vec<String>, String> {
    let token = credential.resolve().map_err(|error| error.to_string())?;
    let response = reqwest::Client::new()
        .get(url)
        .bearer_auth(token)
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .map_err(|error| error.without_url().to_string())?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("HTTP {}", status.as_u16()));
    }
    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|error| error.without_url().to_string())?;
    let mut lines = Vec::new();
    if body["is_available"] == false {
        lines.push("the balance is not enough to make requests".to_owned());
    }
    for info in body["balance_infos"].as_array().into_iter().flatten() {
        let field = |name: &str| info[name].as_str().unwrap_or("?").to_owned();
        lines.push(format!(
            "{} {} (granted {}, topped up {})",
            field("total_balance"),
            field("currency"),
            field("granted_balance"),
            field("topped_up_balance")
        ));
    }
    if lines.is_empty() {
        lines.push("the provider reported no balance".to_owned());
    }
    Ok(lines)
}

/// The provider with this id.
#[must_use]
pub fn provider(id: &str) -> Option<&'static Provider> {
    PROVIDERS.iter().find(|provider| provider.id == id)
}

/// The environment variables that carry a provider's key.
#[must_use]
pub fn env_variables(id: &str) -> &'static [&'static str] {
    provider(id).map_or(&[], |provider| provider.env)
}

/// One model from the catalog.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Model {
    pub id: String,
    pub name: String,
    pub api: String,
    pub provider: String,
    pub base_url: String,
    #[serde(default)]
    pub reasoning: bool,
    #[serde(default)]
    pub input: Vec<String>,
    #[serde(default)]
    pub cost: Option<Cost>,
    #[serde(default)]
    pub context_window: Option<u64>,
    #[serde(default)]
    pub max_tokens: Option<u64>,
    #[serde(default)]
    pub compat: Option<Compat>,
    /// prime-agent's per-model level map: `null` for a level the model does not
    /// offer, a string for how the provider spells it.
    #[serde(default)]
    pub thinking_level_map: Option<std::collections::BTreeMap<String, Option<String>>>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Cost {
    #[serde(default)]
    pub input: f64,
    #[serde(default)]
    pub output: f64,
    #[serde(default)]
    pub cache_read: f64,
    #[serde(default)]
    pub cache_write: f64,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Compat {
    #[serde(default)]
    pub thinking_format: Option<String>,
}

impl Model {
    /// What the model offers for reasoning, from its catalog entry, as prime-agent
    /// reads it from its registry.
    #[must_use]
    pub fn reasoning_model(&self) -> harness_providers::thinking::ReasoningModel {
        harness_providers::thinking::reasoning_from_catalog(
            self.reasoning,
            &self.thinking_level_map.clone().unwrap_or_default(),
        )
    }

    /// `provider/id`, the form `/model` takes and shows.
    #[must_use]
    pub fn reference(&self) -> String {
        format!("{}/{}", self.provider, self.id)
    }

    /// The wire protocol this app speaks to the model with.
    #[must_use]
    pub fn protocol(&self) -> Option<&'static str> {
        match self.api.as_str() {
            "openai-completions" => Some("openai_chat"),
            "anthropic-messages" => Some("anthropic_messages"),
            "openai-responses" => Some("openai_responses"),
            "openai-codex-responses" => Some("openai_codex"),
            _ => None,
        }
    }

    /// The URL requests go to, from the base URL the way prime-agent's SDKs
    /// extend it.
    #[must_use]
    pub fn endpoint(&self) -> String {
        let base = self.base_url.trim_end_matches('/');
        match self.api.as_str() {
            "openai-completions" => format!("{base}/chat/completions"),
            "anthropic-messages" => format!("{base}/v1/messages"),
            "openai-codex-responses" => format!("{base}/codex/responses"),
            _ => format!("{base}/responses"),
        }
    }
}

#[derive(Deserialize)]
struct CatalogFile {
    models: Vec<serde_json::Value>,
}

/// The models this app can use.
#[derive(Clone, Debug, Default)]
pub struct Catalog {
    models: Vec<Model>,
}

impl Catalog {
    /// The snapshot compiled into the binary.
    #[must_use]
    pub fn bundled() -> Self {
        Self {
            models: parse(BUNDLED).unwrap_or_default(),
        }
    }

    /// The cached download when it is usable, else the snapshot, plus the models
    /// the providers you are logged in to list themselves. Never touches the
    /// network; [`refresh_in_background`] does that.
    #[must_use]
    pub fn load(data_dir: &Path) -> Self {
        let bundled = Self::bundled();
        let cached = std::fs::read_to_string(cache_path(data_dir))
            .ok()
            .and_then(|text| parse(&text))
            .map(|models| bundled.admit(models))
            .filter(|models| !models.is_empty());
        let mut catalog = cached.map_or(bundled, |models| Self { models });
        for provider in LIVE_LISTS {
            let Some(ids) = std::fs::read_to_string(live_path(data_dir, provider.0))
                .ok()
                .and_then(|text| serde_json::from_str::<Vec<String>>(&text).ok())
            else {
                continue;
            };
            catalog.add_listed(provider.0, &ids);
        }
        catalog
    }

    /// Models a provider lists that the catalog does not have yet (a release
    /// newer than the catalog, such as `deepseek-v4.1-flash`). Each takes the wire
    /// format, reasoning and level map of the catalog model of the same provider
    /// whose id it shares the longest start with - its predecessor in the family.
    fn add_listed(&mut self, provider: &str, ids: &[String]) {
        let known = self
            .models
            .iter()
            .filter(|model| model.provider == provider)
            .cloned()
            .collect::<Vec<_>>();
        for id in ids {
            if known.iter().any(|model| &model.id == id) {
                continue;
            }
            let Some(sibling) = known
                .iter()
                .max_by_key(|model| common_prefix(&model.id, id))
                .filter(|model| common_prefix(&model.id, id) >= 3)
            else {
                continue;
            };
            let mut model = sibling.clone();
            model.id.clone_from(id);
            model.name.clone_from(id);
            model.cost = None;
            self.models.push(model);
        }
    }

    /// Keep the downloaded models whose transport the snapshot already knows.
    fn admit(&self, models: Vec<Model>) -> Vec<Model> {
        models
            .into_iter()
            .filter(|model| {
                model.protocol().is_some()
                    && self.models.iter().any(|known| {
                        known.provider == model.provider
                            && known.api == model.api
                            && known.base_url == model.base_url
                    })
            })
            .collect()
    }

    /// The models of one provider, in catalog order.
    pub fn for_provider<'a>(&'a self, provider: &'a str) -> impl Iterator<Item = &'a Model> {
        self.models
            .iter()
            .filter(move |model| model.provider == provider)
    }

    /// A model named `provider/id`, or by id alone when only one provider has it.
    #[must_use]
    pub fn find(&self, reference: &str) -> Option<&Model> {
        if let Some((provider, id)) = reference.split_once('/')
            && let Some(model) = self
                .models
                .iter()
                .find(|model| model.provider == provider && model.id == id)
        {
            return Some(model);
        }
        let mut matches = self.models.iter().filter(|model| model.id == reference);
        let first = matches.next()?;
        matches.next().is_none().then_some(first)
    }
}

fn parse(text: &str) -> Option<Vec<Model>> {
    let file: CatalogFile = serde_json::from_str(text).ok()?;
    Some(
        file.models
            .into_iter()
            // One malformed entry is skipped, not the whole catalog.
            .filter_map(|value| serde_json::from_value::<Model>(value).ok())
            .filter(|model| provider(&model.provider).is_some() && model.protocol().is_some())
            .collect(),
    )
}

fn cache_path(data_dir: &Path) -> PathBuf {
    data_dir.join(CACHE_FILE)
}

/// Providers whose own model list is read, and where (OpenAI-compatible
/// `GET /models`): the catalog lags behind their releases.
const LIVE_LISTS: [(&str, &str); 3] = [
    ("deepseek", "https://api.deepseek.com/models"),
    ("opencode", "https://opencode.ai/zen/v1/models"),
    ("opencode-go", "https://opencode.ai/zen/go/v1/models"),
];

fn live_path(data_dir: &Path, provider: &str) -> PathBuf {
    data_dir.join(format!("cache/models-{provider}.json"))
}

fn common_prefix(left: &str, right: &str) -> usize {
    left.chars()
        .zip(right.chars())
        .take_while(|(a, b)| a == b)
        .count()
}

/// Refresh the model list of every listing provider this launch has a key for.
pub fn refresh_listed_models_for_logins(
    environment: &super::paths::LaunchEnvironment,
    data_dir: &Path,
) {
    let auth = super::credentials::resolve_file(environment, data_dir);
    for (provider, _) in LIVE_LISTS {
        let saved = super::credentials::load(&auth, provider)
            .ok()
            .flatten()
            .map(|credential| credential.secret().to_owned());
        let from_environment = || {
            env_variables(provider).iter().find_map(|name| {
                environment
                    .value(name)
                    .filter(|value| !value.is_empty())
                    .map(|value| value.to_string_lossy().into_owned())
            })
        };
        if let Some(key) = saved.or_else(from_environment) {
            refresh_listed_models(data_dir, provider, key);
        }
    }
}

/// Read one provider's model list with its key and cache the ids, on a thread of
/// its own. A failure leaves the previous list in place.
fn refresh_listed_models(data_dir: &Path, provider: &str, key: String) {
    let Some((_, url)) = LIVE_LISTS.iter().find(|(id, _)| *id == provider) else {
        return;
    };
    let url = (*url).to_owned();
    let path = live_path(data_dir, provider);
    let _ = std::thread::Builder::new()
        .name("ha-model-list".to_owned())
        .spawn(move || {
            let Some(text) = download_with_key(&url, &key) else {
                return;
            };
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
                return;
            };
            let ids = value["data"]
                .as_array()
                .map(|models| {
                    models
                        .iter()
                        .filter_map(|model| model["id"].as_str().map(str::to_owned))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if ids.is_empty() {
                return;
            }
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(text) = serde_json::to_string(&ids) {
                let _ = std::fs::write(path, text);
            }
        });
}

fn download_with_key(url: &str, key: &str) -> Option<String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?;
    runtime.block_on(async {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .ok()?;
        let response = client
            .get(url)
            .bearer_auth(key)
            .header("User-Agent", format!("ha/{}", env!("CARGO_PKG_VERSION")))
            .header("x-opencode-session", "ha-model-list")
            .send()
            .await
            .ok()?;
        if !response.status().is_success() {
            return None;
        }
        response.text().await.ok()
    })
}

/// Download the catalog when the cached copy is missing or older than a day.
///
/// Runs on its own thread and reports nothing: the snapshot keeps working if the
/// download fails, and the next launch reads what this one saved.
pub fn refresh_in_background(data_dir: &Path) {
    let path = cache_path(data_dir);
    let fresh = std::fs::metadata(&path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_some_and(|age| age < REFRESH_AFTER);
    if fresh {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("ha-model-catalog".to_owned())
        .spawn(move || {
            let Some(text) = download(CATALOG_URL) else {
                return;
            };
            if parse(&text).is_none_or(|models| Catalog::bundled().admit(models).is_empty()) {
                return;
            }
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let staged = path.with_extension("json.staged");
            if std::fs::write(&staged, text).is_ok() {
                let _ = std::fs::rename(&staged, &path);
            }
        });
}

/// One GET on a private runtime, so the caller's thread needs none.
fn download(url: &str) -> Option<String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?;
    runtime.block_on(async {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .ok()?;
        let response = client.get(url).send().await.ok()?;
        if !response.status().is_success() {
            return None;
        }
        response.text().await.ok()
    })
}

#[cfg(test)]
mod tests {
    use super::{Catalog, PROVIDERS, provider};

    #[test]
    fn the_snapshot_covers_every_provider_login_offers() {
        let catalog = Catalog::bundled();
        for entry in PROVIDERS {
            assert!(
                catalog.for_provider(entry.id).next().is_some(),
                "no model for {}",
                entry.id
            );
        }
    }

    #[test]
    fn each_wire_format_gets_the_url_its_sdk_would_call() {
        let catalog = Catalog::bundled();
        let anthropic = catalog
            .for_provider("opencode")
            .find(|model| model.api == "anthropic-messages")
            .expect("an anthropic-shaped opencode model");
        assert_eq!(anthropic.endpoint(), "https://opencode.ai/zen/v1/messages");
        let chat = catalog
            .for_provider("opencode")
            .find(|model| model.api == "openai-completions")
            .expect("a chat-shaped opencode model");
        assert_eq!(
            chat.endpoint(),
            "https://opencode.ai/zen/v1/chat/completions"
        );
        let deepseek = catalog
            .find("deepseek/deepseek-v4-flash")
            .expect("deepseek");
        assert_eq!(
            deepseek.endpoint(),
            "https://api.deepseek.com/chat/completions"
        );
        assert_eq!(deepseek.protocol(), Some("openai_chat"));
    }

    #[test]
    fn a_download_cannot_send_a_key_somewhere_new() {
        let catalog = Catalog::bundled();
        let mut evil = catalog.find("deepseek/deepseek-v4-flash").cloned().unwrap();
        evil.base_url = "https://attacker.example".to_owned();
        assert!(catalog.admit(vec![evil]).is_empty());
    }

    #[test]
    fn a_listed_model_the_catalog_lacks_follows_its_family() {
        let mut catalog = Catalog::bundled();
        catalog.add_listed("opencode-go", &["deepseek-v4.1-flash".to_owned()]);
        let model = catalog
            .find("opencode-go/deepseek-v4.1-flash")
            .expect("the listed model is offered");
        assert_eq!(model.protocol(), Some("openai_chat"));
        assert_eq!(
            model
                .compat
                .as_ref()
                .and_then(|compat| compat.thinking_format.as_deref()),
            Some("deepseek")
        );
        assert!(model.thinking_level_map.is_some());
    }

    #[test]
    fn a_bare_id_resolves_only_when_it_is_unambiguous() {
        let catalog = Catalog::bundled();
        assert!(catalog.find("deepseek/deepseek-v4-flash").is_some());
        // deepseek-v4-flash is served by deepseek, opencode and opencode-go.
        assert!(catalog.find("deepseek-v4-flash").is_none());
        assert!(provider("opencode").is_some());
    }
}
