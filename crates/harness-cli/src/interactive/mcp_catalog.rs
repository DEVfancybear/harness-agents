//! prime-agent's MCP service catalog (`pa-core/src/mcp/service_catalog.rs`,
//! `catalog_plugin_views.rs`): the services `/plugins` and the `mcp` skill's
//! `mcp.list_plugins` / `mcp.search_plugins` offer.
//!
//! Resolution, first wins per id: the compiled built-ins (Linear, Notion),
//! the user's `mcp-services.json` beside the config, then prime's public
//! catalog (refreshed daily into the data directory; the copy compiled in
//! serves until then). Each service becomes prime's plugin card, with its
//! connection state read from ha's MCP servers and MCP sign-ins. No secret
//! ever reaches a card.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use harness_types::McpServerConfigV2;
use serde_json::{Value, json};

/// prime-agent's `MCP_SERVICE_CATALOG_URL`.
const CATALOG_URL: &str = "https://raw.githubusercontent.com/PrimeIntellect-ai/prime-agent-catalog/main/plugins/catalog.v2.json";

/// prime's packaged snapshot of that catalog.
const BUNDLED: &str = include_str!("mcp-services.bundled.json");

/// The downloaded catalog, under the data directory.
const CACHE_FILE: &str = "cache/mcp-service-catalog.v2.json";

const REFRESH_AFTER: Duration = Duration::from_hours(24);

/// prime-agent's `MAX_LOCAL_CATALOG_ENTRIES`.
const MAX_LOCAL_ENTRIES: usize = 50;

/// One catalog service, as the cards need it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Service {
    pub id: String,
    pub label: String,
    pub url: Option<String>,
    pub description: Option<String>,
    pub category: Option<String>,
    pub publisher: Option<String>,
    pub docs_url: Option<String>,
    pub aliases: Vec<String>,
    /// `oauth`, `api_key`, `none` or `unknown`.
    pub auth: String,
    pub ready: bool,
    pub setup_reason: Option<String>,
    pub metadata_reviewed: bool,
}

impl Service {
    fn parse(entry: &Value) -> Option<Self> {
        let text = |key: &str| {
            entry
                .get(key)
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
                .map(str::to_owned)
        };
        let transport = entry.get("transport")?;
        let url = (transport.get("type").and_then(Value::as_str) == Some("http"))
            .then(|| {
                transport
                    .get("url")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .flatten();
        Some(Self {
            id: text("server")?,
            label: text("label")?,
            url,
            description: text("description"),
            category: text("category"),
            publisher: text("publisher"),
            docs_url: text("docsUrl"),
            aliases: entry
                .get("aliases")
                .and_then(Value::as_array)
                .map(|aliases| {
                    aliases
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
            auth: entry
                .pointer("/auth/strategy")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_owned(),
            ready: entry.pointer("/setup/status").and_then(Value::as_str) != Some("requires-setup"),
            setup_reason: entry
                .pointer("/setup/reason")
                .and_then(Value::as_str)
                .map(str::to_owned),
            metadata_reviewed: entry
                .pointer("/verification/status")
                .and_then(Value::as_str)
                == Some("metadata-reviewed"),
        })
    }

    /// prime-agent's `fresh_mcp_login_allowed`: a concrete https endpoint, an
    /// OAuth (or unknown) strategy and a ready setup.
    #[must_use]
    pub fn connectable(&self) -> bool {
        self.ready
            && matches!(self.auth.as_str(), "oauth" | "unknown")
            && self.url.as_deref().is_some_and(concrete_endpoint)
    }
}

/// prime-agent's `concrete_oauth_endpoint`: https (or loopback http), no
/// credentials, fragment or template placeholder.
fn concrete_endpoint(url: &str) -> bool {
    if url.contains(['{', '}', '#']) {
        return false;
    }
    let Some((scheme, rest)) = url.split_once("://") else {
        return false;
    };
    let authority = rest.split(['/', '?']).next().unwrap_or_default();
    if authority.is_empty() || authority.contains('@') {
        return false;
    }
    let host = authority
        .rsplit_once(':')
        .map_or(authority, |(host, _)| host);
    scheme.eq_ignore_ascii_case("https")
        || (scheme.eq_ignore_ascii_case("http")
            && matches!(host, "localhost" | "127.0.0.1" | "[::1]"))
}

/// prime-agent's `compiled_builtin_services`.
fn builtins() -> Vec<Service> {
    let entries = json!([
        {
            "server": "linear", "label": "Linear", "url": "https://mcp.linear.app/mcp",
            "description": "Search, create and update Linear issues, projects, and initiatives. Draft PRDs, write updates, analyze customer requests, and keep plans up to date \u{2014} all from within ChatGPT",
            "category": "Productivity", "publisher": "Linear", "aliases": ["linear-app"],
            "transport": { "type": "http", "url": "https://mcp.linear.app/mcp" },
            "auth": { "strategy": "oauth" }, "setup": { "status": "ready" },
            "verification": { "status": "metadata-reviewed" }
        },
        {
            "server": "notion", "label": "Notion", "url": "https://mcp.notion.com/mcp",
            "description": "Notion workflows for implementation planning, research synthesis, meeting preparation, and knowledge capture.",
            "category": "Productivity", "publisher": "Notion", "aliases": ["notion-workspace"],
            "transport": { "type": "http", "url": "https://mcp.notion.com/mcp" },
            "auth": { "strategy": "oauth" }, "setup": { "status": "ready" },
            "verification": { "status": "metadata-reviewed" }
        }
    ]);
    entries
        .as_array()
        .map(|entries| entries.iter().filter_map(Service::parse).collect())
        .unwrap_or_default()
}

fn entries(text: &str) -> Vec<Value> {
    serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|document| document.get("entries").and_then(Value::as_array).cloned())
        .unwrap_or_default()
}

/// The resolved catalog: built-ins, then the user's `mcp-services.json`
/// (`version: 1`, at most 50 entries; it cannot claim a built-in id or a
/// review), then prime's catalog.
#[must_use]
pub fn load(config_dir: &Path, data_dir: &Path) -> Vec<Service> {
    let mut services = builtins();
    let reserved: Vec<String> = services.iter().map(|service| service.id.clone()).collect();
    if let Ok(text) = std::fs::read_to_string(config_dir.join("mcp-services.json"))
        && serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|document| document.get("version").and_then(Value::as_u64))
            == Some(1)
    {
        for entry in entries(&text).iter().take(MAX_LOCAL_ENTRIES) {
            if let Some(mut service) = Service::parse(entry)
                && !reserved.contains(&service.id)
            {
                service.metadata_reviewed = false;
                if !services.iter().any(|known| known.id == service.id) {
                    services.push(service);
                }
            }
        }
    }
    let remote = std::fs::read_to_string(data_dir.join(CACHE_FILE))
        .ok()
        .filter(|text| !entries(text).is_empty())
        .unwrap_or_else(|| BUNDLED.to_owned());
    for entry in entries(&remote) {
        if let Some(service) = Service::parse(&entry)
            && !services.iter().any(|known| known.id == service.id)
        {
            services.push(service);
        }
    }
    services
}

/// Download prime's catalog when the cached copy is a day old (prime's fetch
/// lane); offline mode skips it.
pub fn refresh_in_background(data_dir: &Path) {
    let path = data_dir.join(CACHE_FILE);
    let fresh = std::fs::metadata(&path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_some_and(|age| age < REFRESH_AFTER);
    if fresh {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("ha-mcp-catalog".to_owned())
        .spawn(move || {
            let Some(text) = super::providers::download(CATALOG_URL) else {
                return;
            };
            if entries(&text).is_empty() {
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

/// What a card knows about the user's side: the configured MCP servers and
/// which of them hold an MCP sign-in.
pub struct Connections<'a> {
    pub servers: &'a BTreeMap<String, McpServerConfigV2>,
    pub signed_in: &'a dyn Fn(&str, &str) -> bool,
}

/// prime-agent's `build_plugin_views`: one card per catalog service, then
/// one per configured server the catalog does not name.
#[must_use]
pub fn views(services: &[Service], connections: &Connections<'_>) -> Vec<Value> {
    let mut cards: Vec<Value> = services
        .iter()
        .map(|service| catalog_card(service, connections))
        .collect();
    for (name, server) in connections.servers {
        if services.iter().any(|service| service.id == *name) {
            continue;
        }
        cards.push(json!({
            "serviceId": name,
            "label": name,
            "connectionStatus": "pending",
            "connectable": false,
            "usesOauth": false,
            "source": "user",
            "connectionIds": [name],
            "setupHint": format!(
                "Configured in ha ({}); the connection is checked when it is first used.",
                server.transport.as_deref().unwrap_or("stdio")
            ),
        }));
    }
    cards
}

fn catalog_card(service: &Service, connections: &Connections<'_>) -> Value {
    let configured = connections.servers.get(&service.id);
    let uses_oauth = matches!(service.auth.as_str(), "oauth" | "unknown");
    let (status, connection_ids, hint): (&str, Vec<String>, Option<String>) = match configured {
        // prime's `pending`: credentials exist but no verified handshake yet;
        // ha checks a server when the session first calls it.
        Some(server) => {
            let url = server.url.clone().unwrap_or_default();
            if uses_oauth
                && server.bearer_token_env.is_none()
                && !(connections.signed_in)(&service.id, &url)
            {
                (
                    "not_connected",
                    Vec::new(),
                    Some(format!("Sign in with /mcp login {}.", service.id)),
                )
            } else {
                ("pending", vec![service.id.clone()], None)
            }
        }
        None => {
            let hint = if !service.ready {
                Some(service.setup_reason.clone().unwrap_or_else(|| {
                    "This service requires manual setup before it can be connected.".to_owned()
                }))
            } else if service.url.is_none() {
                Some("This service uses a stdio adapter or a tenant URL template. Add it manually with /mcp add.".to_owned())
            } else if service.auth == "api_key" {
                Some("This service requires an API key. Add it manually with /mcp add.".to_owned())
            } else if service.auth == "none" {
                Some("No login required. Add it manually with /mcp add to use it.".to_owned())
            } else if !service.metadata_reviewed {
                Some("OAuth support has not been verified. Connect checks capabilities and asks for approval before login.".to_owned())
            } else {
                None
            };
            let status = if (!service.ready || service.url.is_none()) && service.auth != "none"
                || service.auth == "api_key"
            {
                "setup_required"
            } else {
                "not_connected"
            };
            (status, Vec::new(), hint)
        }
    };
    let mut card = json!({
        "serviceId": service.id,
        "label": service.label,
        "connectionStatus": status,
        "connectable": service.connectable(),
        "addAccountAllowed": service.connectable(),
        "usesOauth": uses_oauth,
        "source": "catalog",
        "connectionIds": connection_ids,
    });
    let mut put = |key: &str, value: Option<Value>| {
        if let Some(value) = value {
            card[key] = value;
        }
    };
    put(
        "aliases",
        (!service.aliases.is_empty()).then(|| json!(service.aliases)),
    );
    put(
        "description",
        service.description.clone().map(Value::String),
    );
    put("category", service.category.clone().map(Value::String));
    put("publisher", service.publisher.clone().map(Value::String));
    put("docsUrl", service.docs_url.clone().map(Value::String));
    put("setupHint", hint.map(Value::String));
    put(
        "unverified",
        (!service.metadata_reviewed).then_some(Value::Bool(true)),
    );
    card
}

/// prime-agent's `search_plugin_views`: ids, labels, categories,
/// descriptions, publishers, docs URLs, aliases and connection ids.
#[must_use]
pub fn search(cards: &[Value], query: &str, limit: usize) -> Vec<Value> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return cards.iter().take(limit).cloned().collect();
    }
    let field = |card: &Value, key: &str| card[key].as_str().unwrap_or_default().to_lowercase();
    cards
        .iter()
        .filter(|card| {
            [
                "serviceId",
                "label",
                "category",
                "description",
                "publisher",
                "docsUrl",
            ]
            .iter()
            .any(|key| field(card, key).contains(&needle))
                || ["aliases", "connectionIds"].iter().any(|key| {
                    card[*key].as_array().is_some_and(|values| {
                        values
                            .iter()
                            .filter_map(Value::as_str)
                            .any(|value| value.to_lowercase().contains(&needle))
                    })
                })
        })
        .take(limit)
        .cloned()
        .collect()
}

/// prime-agent's `mcp.list_plugins`: the cards of one status (when asked),
/// one page from `cursor`.
///
/// # Errors
/// prime's invalid-cursor message.
pub fn page(
    cards: &[Value],
    status: Option<&str>,
    cursor: Option<&str>,
    limit: usize,
) -> Result<Value, String> {
    let start = match cursor.filter(|cursor| !cursor.is_empty()) {
        None => 0,
        Some(cursor) if cursor.chars().all(|c| c.is_ascii_digit()) => cursor
            .parse::<usize>()
            .map_err(|_| "mcp.list_plugins received an invalid cursor".to_owned())?,
        Some(_) => return Err("mcp.list_plugins received an invalid cursor".to_owned()),
    };
    let filtered: Vec<&Value> = cards
        .iter()
        .filter(|card| status.is_none_or(|status| card["connectionStatus"] == status))
        .collect();
    if start >= filtered.len() {
        return Ok(json!({ "plugins": [], "nextCursor": Value::Null }));
    }
    let plugins: Vec<Value> = filtered
        .iter()
        .skip(start)
        .take(limit)
        .map(|card| (*card).clone())
        .collect();
    let next = (start + limit < filtered.len()).then(|| (start + limit).to_string());
    Ok(json!({ "plugins": plugins, "nextCursor": next }))
}

/// The cards for a session: the resolved catalog against the configured
/// servers and the MCP sign-ins in `auth_path`.
#[must_use]
pub fn session_cards(
    config_dir: &Path,
    data_dir: &Path,
    servers: &BTreeMap<String, McpServerConfigV2>,
    auth_path: &Path,
) -> Vec<Value> {
    let auth_path: PathBuf = auth_path.to_path_buf();
    let signed_in = move |server: &str, url: &str| {
        matches!(
            super::credentials::load(&auth_path, &super::mcp_oauth::credential_key(server)),
            Ok(Some(super::credentials::Credential::McpOauth { endpoint, .. })) if endpoint == url
        )
    };
    views(
        &load(config_dir, data_dir),
        &Connections {
            servers,
            signed_in: &signed_in,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_sign_in(_: &str, _: &str) -> bool {
        false
    }

    #[test]
    fn the_catalog_resolves_builtins_first_and_pages_like_prime() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let services = load(temporary.path(), temporary.path());
        assert_eq!(services[0].id, "linear");
        assert!(
            services.len() > 50,
            "the bundled catalog: {}",
            services.len()
        );
        assert_eq!(
            services
                .iter()
                .filter(|service| service.id == "linear")
                .count(),
            1
        );
        let servers = BTreeMap::new();
        let cards = views(
            &services,
            &Connections {
                servers: &servers,
                signed_in: &no_sign_in,
            },
        );
        let first = page(&cards, None, None, 50).expect("page");
        assert_eq!(first["plugins"].as_array().map(Vec::len), Some(50));
        assert_eq!(first["nextCursor"], json!("50"));
        assert!(page(&cards, None, Some("x"), 50).is_err());
        let found = search(&cards, "notion", 10);
        assert_eq!(found[0]["serviceId"], json!("notion"));
        assert_eq!(found[0]["connectable"], json!(true));
    }

    #[test]
    fn a_configured_signed_in_server_is_pending_and_dispatchable() {
        let services = builtins();
        let mut servers = BTreeMap::new();
        servers.insert(
            "linear".to_owned(),
            McpServerConfigV2 {
                transport: Some("streamable_http".to_owned()),
                url: Some("https://mcp.linear.app/mcp".to_owned()),
                ..McpServerConfigV2::default()
            },
        );
        let signed_in = |server: &str, url: &str| server == "linear" && url.contains("linear");
        let cards = views(
            &services,
            &Connections {
                servers: &servers,
                signed_in: &signed_in,
            },
        );
        assert_eq!(cards[0]["connectionStatus"], json!("pending"));
        assert_eq!(cards[0]["connectionIds"], json!(["linear"]));
        assert_eq!(cards[1]["connectionStatus"], json!("not_connected"));
    }
}
