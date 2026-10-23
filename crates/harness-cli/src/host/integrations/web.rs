//! Web access for the agent: `web_search` and `web_fetch`.
//!
//! Measured: asked to research a topic, the agent had no way to reach the internet -
//! research workflows need a search tool and a page-open tool that the harness never
//! had. prime-agent ships the same capability as its
//! `websearch` skill (a Serper call made from the Python kernel, pages fetched with
//! `httpx`); here the two operations are host tools, so they work in every turn
//! whether or not the kernel is up:
//!
//! - `web_search` asks the search APIs that have a key, in the configured order
//!   (`[web] search`, `HA_WEB_SEARCH`; `DeepSeek`, then Serper, by default): `DeepSeek`'s
//!   Search API with the `DeepSeek` key, Google through Serper with `SERPER_API_KEY`,
//!   formatted the way prime-agent formats it. An API that fails hands the query to
//!   the next one, and a key an API refuses (expired, out of balance) is skipped for
//!   a while; `DuckDuckGo`'s HTML endpoint, which needs no key, comes last, so search
//!   works on a fresh install;
//! - `web_fetch` downloads one http(s) page and returns readable text - scripts and
//!   styles dropped, blocks on their own lines, entities decoded - with the page's
//!   links, in windows the model pages through with `start_index`. A site that
//!   refuses ha (a bot check, a rate limit) is read through `DeepSeek`'s Fetch API
//!   when a `DeepSeek` key is set.
//!
//! Both are bounded: a size cap on what is downloaded, a window on what is returned,
//! and a timeout. Loopback, private and link-local hosts are refused, including as
//! the target of a redirect, because a page read from the web must not be able to
//! steer the agent into the user's own network. `HA_WEB=off` removes both tools.

use std::fmt::Write as _;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use harness_tools::{
    CodingToolAction, ExternalToolCatalog, ExternalToolDispatcher, ExternalTools,
    ToolDispatchAuthorization, ToolOutput,
};
use harness_types::{ErrorCode, HarnessError};
use reqwest::Url;
use serde_json::{Value, json};

use super::paths::LaunchEnvironment;

/// `off` (or `0`, `false`, `no`) removes the web tools.
pub const WEB_VARIABLE: &str = "HA_WEB";

/// The Serper key; without it search uses the keyless fallback.
pub const SERPER_KEY_VARIABLE: &str = "SERPER_API_KEY";

const SERPER_ENDPOINT: &str = "https://google.serper.dev/search";

/// The credential-file entry `/login serper` saves, prime-agent's `serper` id.
pub const SERPER_CREDENTIAL: &str = "serper";

/// The `DeepSeek` key, the one the `DeepSeek` models use: `DEEPSEEK_API_KEY`, else
/// what `/login deepseek` saved.
pub const DEEPSEEK_KEY_VARIABLE: &str = "DEEPSEEK_API_KEY";
const DEEPSEEK_CREDENTIAL: &str = "deepseek";

/// `DeepSeek`'s Search and Fetch APIs (`/search`, `/fetch` under it); the
/// variable moves them, for a proxy or a test.
const DEEPSEEK_TOOLS_ENDPOINT: &str = "https://api.deepseek.com/v1/tools";
pub const DEEPSEEK_TOOLS_VARIABLE: &str = "HA_DEEPSEEK_TOOLS_URL";
/// `DeepSeek` Search takes a query of at most 128 characters.
const DEEPSEEK_MAX_QUERY_CHARS: usize = 128;

/// The search APIs in the order they are asked, by id (`deepseek,serper`):
/// overrides `[web] search`.
pub const SEARCH_ORDER_VARIABLE: &str = "HA_WEB_SEARCH";

/// How long a key an API refused (401, 402: invalid, expired, out of balance)
/// is skipped before it is tried again; a different key is tried at once.
const REFUSED_KEY_RETRY: Duration = Duration::from_mins(30);

/// A search API `web_search` can ask with a key.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SearchApi {
    DeepSeek,
    Serper,
}

impl SearchApi {
    /// The order when nothing sets one.
    pub const DEFAULT_ORDER: [Self; 2] = [Self::DeepSeek, Self::Serper];

    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::DeepSeek => "deepseek",
            Self::Serper => "serper",
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::DeepSeek => "DeepSeek Search",
            Self::Serper => "Google via Serper",
        }
    }

    #[must_use]
    pub fn parse(id: &str) -> Option<Self> {
        match id.trim().to_ascii_lowercase().as_str() {
            "deepseek" => Some(Self::DeepSeek),
            "serper" | "google" => Some(Self::Serper),
            _ => None,
        }
    }

    /// An order as written (`deepseek, serper`): known ids, each once.
    #[must_use]
    pub fn order(ids: &[String]) -> Vec<Self> {
        let mut order = Vec::new();
        for api in ids.iter().filter_map(|id| Self::parse(id)) {
            if !order.contains(&api) {
                order.push(api);
            }
        }
        order
    }

    /// The command that saves this API's key.
    const fn login(self) -> &'static str {
        match self {
            Self::DeepSeek => "/login deepseek",
            Self::Serper => "/login serper",
        }
    }
}

/// One search API with its key.
#[derive(Clone)]
struct Searcher {
    api: SearchApi,
    key: String,
    endpoint: String,
}

/// Keys an API refused, by API and a hash of the key, with when.
type RefusedKeys = std::collections::HashMap<(SearchApi, u64), std::time::Instant>;

fn refused_keys() -> std::sync::MutexGuard<'static, RefusedKeys> {
    static REFUSED: std::sync::LazyLock<std::sync::Mutex<RefusedKeys>> =
        std::sync::LazyLock::new(Default::default);
    REFUSED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn key_hash(key: &str) -> u64 {
    use std::hash::{Hash as _, Hasher as _};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut hasher);
    hasher.finish()
}

/// How long ago `api` refused `key`, while that still counts.
fn refused_since(api: SearchApi, key: &str) -> Option<Duration> {
    let mut refused = refused_keys();
    let entry = (api, key_hash(key));
    let since = refused.get(&entry)?.elapsed();
    if since >= REFUSED_KEY_RETRY {
        refused.remove(&entry);
        return None;
    }
    Some(since)
}

fn remember_refused(api: SearchApi, key: &str) {
    refused_keys().insert((api, key_hash(key)), std::time::Instant::now());
}

/// Why one search API did not answer.
struct SearchFailure {
    message: String,
    /// The API refused the key itself: skip it for a while.
    key_refused: bool,
}
const FALLBACK_ENDPOINT: &str = "https://html.duckduckgo.com/html/";
const USER_AGENT: &str =
    "Mozilla/5.0 (compatible; ha/0.1; +https://github.com/DEVfancybear/harness-agents)";

/// The most a fetch downloads.
const MAX_DOWNLOAD_BYTES: usize = 4 * 1024 * 1024;
/// The default and largest window one `web_fetch` returns, in characters.
const DEFAULT_WINDOW_CHARS: usize = 16_000;
const MAX_WINDOW_CHARS: usize = 20_000;
/// The most links listed under a fetched page.
const MAX_LINKS: usize = 40;
/// The most results one search returns.
const MAX_RESULTS: usize = 10;
const DEFAULT_RESULTS: usize = 6;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The web tools of one turn.
#[derive(Clone)]
pub struct WebHost {
    inner: Arc<WebInner>,
}

struct WebInner {
    client: reqwest::Client,
    /// The APIs with a key, in the order they are asked.
    searchers: Vec<Searcher>,
    /// `DeepSeek`'s Fetch API and its key, for a site that refuses ha.
    page_reader: Option<(String, String)>,
    fallback_endpoint: String,
    allow_private_hosts: bool,
}

impl WebHost {
    /// The web tools as the environment configures them, or `None` when turned off.
    /// The Serper key is `SERPER_API_KEY`, else the one `/login serper` saved
    /// in the credential file under `data_dir`, as prime-agent keeps it; the
    /// `DeepSeek` key is `DEEPSEEK_API_KEY`, else the one `/login deepseek` saved.
    /// `configured` is `[web] search`, the order the APIs are asked in;
    /// `HA_WEB_SEARCH` overrides it.
    #[must_use]
    pub fn from_environment(
        environment: &LaunchEnvironment,
        data_dir: &std::path::Path,
        configured: Option<&[String]>,
    ) -> Option<Self> {
        let value = |name: &str| {
            environment
                .value(name)
                .and_then(|value| value.to_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        };
        if value(WEB_VARIABLE).is_some_and(|value| {
            matches!(
                value.to_ascii_lowercase().as_str(),
                "off" | "0" | "false" | "no"
            )
        }) {
            return None;
        }
        let credential_file = super::credentials::resolve_file(environment, data_dir);
        let saved = |provider: &str| {
            super::credentials::load(&credential_file, provider)
                .ok()
                .flatten()
                .filter(|credential| {
                    matches!(credential, super::credentials::Credential::ApiKey { .. })
                })
                .map(|credential| credential.secret().trim().to_owned())
                .filter(|key| !key.is_empty())
        };
        let order = value(SEARCH_ORDER_VARIABLE)
            .map(|ids| SearchApi::order(&ids.split(',').map(str::to_owned).collect::<Vec<_>>()))
            .or_else(|| configured.map(SearchApi::order))
            .unwrap_or_else(|| SearchApi::DEFAULT_ORDER.to_vec());
        let tools = value(DEEPSEEK_TOOLS_VARIABLE)
            .unwrap_or_else(|| DEEPSEEK_TOOLS_ENDPOINT.to_owned())
            .trim_end_matches('/')
            .to_owned();
        let deepseek_key = value(DEEPSEEK_KEY_VARIABLE).or_else(|| saved(DEEPSEEK_CREDENTIAL));
        let searchers = order
            .iter()
            .filter_map(|api| {
                let (key, endpoint) = match api {
                    SearchApi::DeepSeek => (deepseek_key.clone()?, format!("{tools}/search")),
                    SearchApi::Serper => (
                        value(SERPER_KEY_VARIABLE).or_else(|| saved(SERPER_CREDENTIAL))?,
                        SERPER_ENDPOINT.to_owned(),
                    ),
                };
                Some(Searcher {
                    api: *api,
                    key,
                    endpoint,
                })
            })
            .collect();
        Self::build(
            searchers,
            deepseek_key.map(|key| (format!("{tools}/fetch"), key)),
            FALLBACK_ENDPOINT.to_owned(),
            false,
        )
    }

    fn build(
        searchers: Vec<Searcher>,
        page_reader: Option<(String, String)>,
        fallback_endpoint: String,
        allow_private_hosts: bool,
    ) -> Option<Self> {
        let redirect = reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= 10 {
                attempt.error("too many redirects")
            } else if !allow_private_hosts && is_private_host(attempt.url()) {
                attempt.error("the page redirected to a private or local address")
            } else {
                attempt.follow()
            }
        });
        // Measured: sites behind a bot check (tradingeconomics.com) answered 403 to a
        // request without `Accept`/`Accept-Language` and 200 to the same user agent
        // sending the headers a browser sends, with compressed bodies accepted.
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::ACCEPT,
            reqwest::header::HeaderValue::from_static(
                "text/html,application/xhtml+xml,application/xml;q=0.9,text/plain;q=0.8,*/*;q=0.7",
            ),
        );
        headers.insert(
            reqwest::header::ACCEPT_LANGUAGE,
            reqwest::header::HeaderValue::from_static("en-US,en;q=0.9"),
        );
        let client = harness_providers::with_extra_roots(reqwest::Client::builder())
            .user_agent(USER_AGENT)
            .default_headers(headers)
            .timeout(REQUEST_TIMEOUT)
            .redirect(redirect)
            .build()
            .ok()?;
        Some(Self {
            inner: Arc::new(WebInner {
                client,
                searchers,
                page_reader,
                fallback_endpoint,
                allow_private_hosts,
            }),
        })
    }

    #[must_use]
    #[allow(
        clippy::unused_self,
        reason = "a method, like the other hosts' `tools`, so callers map every host the same way"
    )]
    pub fn tools(&self) -> ExternalTools {
        ExternalTools::new(Arc::new(WebCatalog))
    }

    #[must_use]
    pub fn dispatcher(&self) -> Arc<dyn ExternalToolDispatcher> {
        Arc::new(self.clone())
    }
}

struct WebCatalog;

impl ExternalToolCatalog for WebCatalog {
    fn schemas(&self) -> Vec<Value> {
        vec![
            json!({
                "type": "function",
                "function": {
                    "name": "web_search",
                    "description": "Search the web. Returns titles, URLs and snippets; open the promising results with web_fetch. Use several focused queries for research.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "query": {"type": "string", "description": "The search query."},
                            "num_results": {"type": "integer", "minimum": 1, "maximum": MAX_RESULTS, "description": "How many results (default 6)."}
                        },
                        "required": ["query"],
                        "additionalProperties": false
                    }
                }
            }),
            json!({
                "type": "function",
                "function": {
                    "name": "web_fetch",
                    "description": "Open one http(s) URL and read it as text (HTML is converted; links are listed at the end). Long pages come in windows: call again with start_index to continue.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "url": {"type": "string", "description": "The http or https URL to open."},
                            "start_index": {"type": "integer", "minimum": 0, "description": "Character offset to start reading from (default 0)."},
                            "max_chars": {"type": "integer", "minimum": 1000, "maximum": MAX_WINDOW_CHARS, "description": "Characters to return (default 16000)."}
                        },
                        "required": ["url"],
                        "additionalProperties": false
                    }
                }
            }),
        ]
    }

    fn resolve(&self, name: &str, arguments: &Value) -> Option<CodingToolAction> {
        matches!(name, "web_search" | "web_fetch").then(|| CodingToolAction::ExternalTool {
            plugin_id: "web".to_owned(),
            tool_name: name.to_owned(),
            arguments: arguments.clone(),
            parent_invocation_id: None,
            timeout_ms: 60_000,
        })
    }
}

fn invalid(message: impl Into<String>) -> HarnessError {
    HarnessError::new(ErrorCode::InvalidPayload, message)
}

/// The validated arguments of one call.
enum WebCall {
    Search {
        query: String,
        results: usize,
    },
    Fetch {
        url: Url,
        start: usize,
        window: usize,
    },
}

impl WebHost {
    fn parse(&self, tool_name: &str, arguments: &Value) -> Result<WebCall, HarnessError> {
        let object = arguments
            .as_object()
            .ok_or_else(|| invalid("web tool arguments must be an object"))?;
        let unsigned = |key: &str| -> Result<Option<usize>, HarnessError> {
            match object.get(key) {
                None | Some(Value::Null) => Ok(None),
                Some(value) => value
                    .as_u64()
                    .and_then(|value| usize::try_from(value).ok())
                    .map(Some)
                    .ok_or_else(|| invalid(format!("{key} must be a non-negative integer"))),
            }
        };
        match tool_name {
            "web_search" => {
                if object
                    .keys()
                    .any(|key| key != "query" && key != "num_results")
                {
                    return Err(invalid("web_search accepts query and num_results"));
                }
                let query = object
                    .get("query")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|query| !query.is_empty())
                    .ok_or_else(|| invalid("web_search needs a non-empty query"))?;
                if query.chars().count() > 400 {
                    return Err(invalid("a search query is at most 400 characters"));
                }
                let results = unsigned("num_results")?
                    .unwrap_or(DEFAULT_RESULTS)
                    .clamp(1, MAX_RESULTS);
                Ok(WebCall::Search {
                    query: query.to_owned(),
                    results,
                })
            }
            "web_fetch" => {
                if object
                    .keys()
                    .any(|key| !matches!(key.as_str(), "url" | "start_index" | "max_chars"))
                {
                    return Err(invalid("web_fetch accepts url, start_index and max_chars"));
                }
                let raw = object
                    .get("url")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .ok_or_else(|| invalid("web_fetch needs a url"))?;
                let url = Url::parse(raw)
                    .map_err(|error| invalid(format!("{raw} is not a URL: {error}")))?;
                if !matches!(url.scheme(), "http" | "https") {
                    return Err(invalid("web_fetch opens http and https URLs only"));
                }
                if !self.inner.allow_private_hosts && is_private_host(&url) {
                    return Err(HarnessError::new(
                        ErrorCode::PolicyDenied,
                        format!(
                            "{url} is a local or private address; web_fetch reads public pages only"
                        ),
                    ));
                }
                Ok(WebCall::Fetch {
                    url,
                    start: unsigned("start_index")?.unwrap_or(0),
                    window: unsigned("max_chars")?
                        .unwrap_or(DEFAULT_WINDOW_CHARS)
                        .clamp(1_000, MAX_WINDOW_CHARS),
                })
            }
            _ => Err(HarnessError::new(
                ErrorCode::PolicyDenied,
                "web tool target is unavailable",
            )),
        }
    }

    async fn run(&self, call: WebCall) -> Result<String, HarnessError> {
        match call {
            WebCall::Search { query, results } => self.search(&query, results).await,
            WebCall::Fetch { url, start, window } => self.fetch(&url, start, window).await,
        }
    }

    /// Ask the search APIs in order until one answers: one that fails hands the
    /// query to the next, and a key an API refused is skipped for a while, so
    /// an expired key costs one failed request, not one per search. The keyless
    /// provider answers when none does, and a note says what was skipped.
    async fn search(&self, query: &str, results: usize) -> Result<String, HarnessError> {
        let mut notes = Vec::new();
        let mut renew = Vec::new();
        for searcher in &self.inner.searchers {
            let api = searcher.api;
            if let Some(since) = refused_since(api, &searcher.key) {
                notes.push(format!(
                    "{} skipped: it refused its key {} min ago",
                    api.label(),
                    since.as_secs() / 60
                ));
                renew.push(api.login());
                continue;
            }
            let answer = match api {
                SearchApi::DeepSeek => self.deepseek_search(searcher, query, results).await,
                SearchApi::Serper => self.serper(searcher, query, results).await,
            };
            match answer {
                Ok(body) => {
                    return Ok(search_answer(query, api.label(), &notes, &renew, &body));
                }
                Err(failure) => {
                    harness_types::diag::warn("web", "search_failed")
                        .field("api", api.id())
                        // Not `key_*`: diagnostics withhold fields named like a secret.
                        .field("refused", failure.key_refused)
                        .field("reason", failure.message.as_str())
                        .emit();
                    if failure.key_refused {
                        remember_refused(api, &searcher.key);
                        renew.push(api.login());
                    }
                    notes.push(format!("{} failed: {}", api.label(), failure.message));
                }
            }
        }
        let body = self.fallback_search(query, results).await?;
        Ok(search_answer(query, "DuckDuckGo", &notes, &renew, &body))
    }

    /// One search API's answer, or why it gave none.
    async fn ask_api(
        &self,
        searcher: &Searcher,
        request: reqwest::RequestBuilder,
        refuses_key: impl Fn(u16, &str) -> bool,
    ) -> Result<Value, SearchFailure> {
        let label = searcher.api.label();
        let response = request.send().await.map_err(|error| SearchFailure {
            message: format!("the request failed: {}", with_cause(&error)),
            key_refused: false,
        })?;
        let status = response.status();
        let bytes = read_bounded(response)
            .await
            .map_err(|error| SearchFailure {
                message: error.message().to_owned(),
                key_refused: false,
            })?;
        if !status.is_success() {
            // The API's own reason (`{"error": {"message": ...}}` from DeepSeek,
            // `{"message": ...}` from Serper), else the start of the body.
            let text = String::from_utf8_lossy(&bytes);
            let body = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|value| {
                    value
                        .pointer("/error/message")
                        .or_else(|| value.get("message"))
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .unwrap_or_else(|| text.into_owned())
                .chars()
                .take(300)
                .collect::<String>();
            return Err(SearchFailure {
                key_refused: refuses_key(status.as_u16(), &body),
                message: http_error(&format!("{label} ({body})"), status)
                    .message()
                    .to_owned(),
            });
        }
        serde_json::from_slice(&bytes).map_err(|_| SearchFailure {
            message: format!("{label} did not answer with JSON"),
            key_refused: false,
        })
    }

    async fn serper(
        &self,
        searcher: &Searcher,
        query: &str,
        results: usize,
    ) -> Result<String, SearchFailure> {
        let request = self
            .inner
            .client
            .post(&searcher.endpoint)
            .header("X-API-KEY", &searcher.key)
            .json(&json!({ "q": query, "num": results }));
        // Serper answers a bad key 401/403 and a key out of credits 400
        // "Not enough credits".
        let data = self
            .ask_api(searcher, request, |status, body| {
                matches!(status, 401..=403) || body.to_ascii_lowercase().contains("credits")
            })
            .await?;
        Ok(format_serper(&data, query, results))
    }

    /// `DeepSeek`'s Search API: `POST /v1/tools/search` with the `DeepSeek` key.
    async fn deepseek_search(
        &self,
        searcher: &Searcher,
        query: &str,
        results: usize,
    ) -> Result<String, SearchFailure> {
        let request = self
            .inner
            .client
            .post(&searcher.endpoint)
            .bearer_auth(&searcher.key)
            .json(&json!({
                "query": deepseek_query(query),
                "count": results,
                "need_content": false,
            }));
        // 401: no valid key; 402: the balance ran out.
        let data = self
            .ask_api(searcher, request, |status, _| matches!(status, 401 | 402))
            .await?;
        Ok(format_deepseek(&data, query, results))
    }

    async fn fallback_search(&self, query: &str, results: usize) -> Result<String, HarnessError> {
        // The HTML endpoint is a form, and it is posted like one. Measured: repeated
        // GET queries were answered `202 Accepted` with no results (the endpoint's bot
        // check) while the same query posted as its form returned the results page.
        let form = Url::parse_with_params("http://form.invalid/", &[("q", query)])
            .ok()
            .and_then(|url| url.query().map(str::to_owned))
            .unwrap_or_default();
        let response = self
            .inner
            .client
            .post(&self.inner.fallback_endpoint)
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(form)
            .send()
            .await
            .map_err(|error| {
                unavailable(format!("the search request failed: {}", with_cause(&error)))
            })?;
        let status = response.status();
        let bytes = read_bounded(response).await?;
        let page = String::from_utf8_lossy(&bytes);
        let hits = parse_duckduckgo(&page, results);
        if !status.is_success() || hits.is_empty() {
            return Ok(format!(
                "No results were returned (the keyless search provider answered {status}; it may be rate-limiting). \
                 Try a different query, or ask the user to run /login serper (a free key from https://serper.dev) for Google results."
            ));
        }
        Ok(hits
            .iter()
            .enumerate()
            .map(|(index, hit)| {
                let mut entry = format!("Result {}: {}\nURL: {}", index + 1, hit.title, hit.url);
                if !hit.snippet.is_empty() {
                    entry.push('\n');
                    entry.push_str(&hit.snippet);
                }
                entry
            })
            .collect::<Vec<_>>()
            .join("\n\n---\n\n"))
    }

    /// Open a page; a site that refuses ha or fails is read through `DeepSeek`'s
    /// Fetch API when a `DeepSeek` key is set, and the note says so.
    async fn fetch(&self, url: &Url, start: usize, window: usize) -> Result<String, HarnessError> {
        let error = match self.fetch_directly(url, start, window).await {
            Ok(page) => return Ok(page),
            Err(error) => error,
        };
        let relayed = matches!(
            error.code(),
            ErrorCode::AccessDenied | ErrorCode::RateLimited | ErrorCode::ServiceUnavailable
        );
        let Some((endpoint, key)) = self.inner.page_reader.as_ref().filter(|_| relayed) else {
            return Err(error);
        };
        if refused_since(SearchApi::DeepSeek, key).is_some() {
            return Err(error);
        }
        let searcher = Searcher {
            api: SearchApi::DeepSeek,
            key: key.clone(),
            endpoint: endpoint.clone(),
        };
        let request = self
            .inner
            .client
            .post(endpoint)
            .bearer_auth(key)
            .json(&json!({ "url": url.as_str() }));
        match self
            .ask_api(&searcher, request, |status, _| matches!(status, 401 | 402))
            .await
        {
            Ok(page) => {
                let content = page
                    .get("content")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if content.trim().is_empty() {
                    return Err(error);
                }
                let mut document = format!("[{} - read through DeepSeek Fetch]\n", error.message());
                let title = page
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if !title.trim().is_empty() {
                    let _ = writeln!(document, "Title: {}", title.trim());
                }
                let _ = write!(document, "URL: {url}\n\n{content}");
                Ok(window_of(&document, start, window))
            }
            Err(failure) => {
                if failure.key_refused {
                    remember_refused(SearchApi::DeepSeek, key);
                }
                Err(HarnessError::new(
                    error.code(),
                    format!(
                        "{} (DeepSeek Fetch could not read it either: {})",
                        error.message(),
                        failure.message
                    ),
                ))
            }
        }
    }

    async fn fetch_directly(
        &self,
        url: &Url,
        start: usize,
        window: usize,
    ) -> Result<String, HarnessError> {
        let response = self
            .inner
            .client
            .get(url.clone())
            .send()
            .await
            .map_err(|error| {
                unavailable(format!("{url} could not be opened: {}", with_cause(&error)))
            })?;
        let status = response.status();
        let final_url = response.url().clone();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        let bytes = read_bounded(response).await?;
        if !status.is_success() {
            return Err(http_error(final_url.as_str(), status));
        }
        let declared = charset_of(&content_type, &bytes);
        let looks_like_html = content_type.contains("html")
            || bytes
                .iter()
                .take(512)
                .map(u8::to_ascii_lowercase)
                .collect::<Vec<_>>()
                .windows(5)
                .any(|window| window == b"<html" || window == b"<!doc");
        let textual = content_type.is_empty()
            || content_type.starts_with("text/")
            || content_type.contains("json")
            || content_type.contains("xml")
            || content_type.contains("javascript");
        let document = if looks_like_html {
            let page = html_to_text(&decode(&bytes, declared), &final_url);
            let mut document = String::new();
            if !page.title.is_empty() {
                let _ = writeln!(document, "Title: {}", page.title);
            }
            let _ = write!(document, "URL: {final_url}\n\n{}", page.text);
            if !page.links.is_empty() {
                document.push_str("\n\nLinks:\n");
                for (text, link) in &page.links {
                    let _ = writeln!(document, "- {text} ({link})");
                }
            }
            document
        } else if textual && !bytes.contains(&0) {
            format!("URL: {final_url}\n\n{}", decode(&bytes, declared))
        } else {
            return Err(HarnessError::new(
                ErrorCode::UnsupportedTextEncoding,
                format!(
                    "{final_url} is {}; web_fetch reads HTML and text pages",
                    if content_type.is_empty() {
                        "binary"
                    } else {
                        content_type.as_str()
                    }
                ),
            ));
        };
        Ok(window_of(&document, start, window))
    }
}

/// The charset a page declares: its `Content-Type`, else a `<meta charset>` near
/// the top of the document.
fn charset_of(content_type: &str, bytes: &[u8]) -> Option<&'static encoding_rs::Encoding> {
    let label_after = |text: &str| -> Option<&'static encoding_rs::Encoding> {
        let start = text.find("charset=")? + "charset=".len();
        let label = text[start..]
            .trim_start_matches(['"', '\''])
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
            .next()?;
        encoding_rs::Encoding::for_label(label.as_bytes())
    };
    label_after(content_type).or_else(|| {
        let head = String::from_utf8_lossy(&bytes[..bytes.len().min(2048)]).to_ascii_lowercase();
        label_after(&head)
    })
}

/// A body as text in its declared charset; UTF-8 when none is declared.
fn decode(bytes: &[u8], declared: Option<&'static encoding_rs::Encoding>) -> String {
    let (text, _, _) = declared.unwrap_or(encoding_rs::UTF_8).decode(bytes);
    text.into_owned()
}

/// A request error with what lies under it: reqwest's own text is only "error
/// sending request", and the reason (refused, reset, a failed TLS handshake) is
/// in its sources.
fn with_cause(error: &reqwest::Error) -> String {
    let mut text = error.to_string();
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        let cause_text = cause.to_string();
        if !text.contains(&cause_text) {
            text.push_str(": ");
            text.push_str(&cause_text);
        }
        source = cause.source();
    }
    text
}

fn unavailable(message: impl Into<String>) -> HarnessError {
    HarnessError::new(ErrorCode::ServiceUnavailable, message)
}

/// What a non-success status tells the model, by what retrying can do. Every
/// status used to be `service_unavailable` - transient - so a 404 or a 403 read
/// as "try again later" and the model fetched dead or closed pages again.
fn http_error(what: &str, status: reqwest::StatusCode) -> HarnessError {
    let (code, advice) = match status.as_u16() {
        404 | 410 => (
            ErrorCode::ResourceNotFound,
            "it does not exist; check the address or search for the page",
        ),
        401 | 403 | 407 | 451 => (
            ErrorCode::AccessDenied,
            "it refuses this reader (a login, a key, or automated access blocked); try another source",
        ),
        429 => (
            ErrorCode::RateLimited,
            "too many requests; wait before asking again, or try another source",
        ),
        408 | 500..=599 => (
            ErrorCode::ServiceUnavailable,
            "the server failed; it may answer later",
        ),
        _ => (
            ErrorCode::InvalidPayload,
            "the request was refused as it is; repeating it will not change that",
        ),
    };
    let mut message = format!("{what} answered {status}: {advice}");
    if status.as_u16() == 404
        && let Some(tree) = github_tree(what)
    {
        // A guessed path in a repository is the usual 404 there: its file
        // list says what the path is.
        let _ = write!(message, ". The repository's files: web_fetch {tree}");
    }
    HarnessError::new(code, message)
}

/// The file-list URL of the GitHub repository and branch a `blob`, `tree` or
/// raw file URL is in.
fn github_tree(url: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(url).ok()?;
    let parts = parsed.path_segments()?.collect::<Vec<_>>();
    let (owner, repo, branch) = match parsed.host_str()? {
        "github.com" if parts.len() >= 4 && matches!(parts[2], "blob" | "tree") => {
            (parts[0], parts[1], parts[3])
        }
        "raw.githubusercontent.com" if parts.len() >= 3 => (parts[0], parts[1], parts[2]),
        _ => return None,
    };
    Some(format!(
        "https://api.github.com/repos/{owner}/{repo}/git/trees/{branch}?recursive=1"
    ))
}

/// Read a response body, stopping at [`MAX_DOWNLOAD_BYTES`].
async fn read_bounded(mut response: reqwest::Response) -> Result<Vec<u8>, HarnessError> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| {
        unavailable(format!(
            "the response could not be read: {}",
            with_cause(&error)
        ))
    })? {
        let room = MAX_DOWNLOAD_BYTES.saturating_sub(bytes.len());
        bytes.extend_from_slice(&chunk[..chunk.len().min(room)]);
        if bytes.len() >= MAX_DOWNLOAD_BYTES {
            break;
        }
    }
    Ok(bytes)
}

/// One window of a document, saying where the next one starts.
fn window_of(document: &str, start: usize, window: usize) -> String {
    let total = document.chars().count();
    if start >= total && total > 0 {
        return format!("[start_index {start} is past the end: the page has {total} characters]");
    }
    let text = document
        .chars()
        .skip(start)
        .take(window)
        .collect::<String>();
    let end = start + text.chars().count();
    if end < total {
        format!(
            "{text}\n\n[showing characters {start}-{end} of {total}; call web_fetch again with start_index={end} to continue]"
        )
    } else if start > 0 {
        format!("{text}\n\n[end of page: characters {start}-{end} of {total}]")
    } else {
        text
    }
}

/// Whether a URL names this machine or a private network.
fn is_private_host(url: &Url) -> bool {
    let Some(host) = url.host_str() else {
        return true;
    };
    let host = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_ascii_lowercase();
    let last_label = host.rsplit('.').next().unwrap_or_default();
    if host == "localhost" || matches!(last_label, "localhost" | "local") {
        return true;
    }
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => {
            ip.is_loopback()
                || ip.is_private()
                || ip.is_link_local()
                || ip.is_unspecified()
                || ip.is_broadcast()
                || ip.octets()[0] == 100 && (64..128).contains(&ip.octets()[1])
        }
        Ok(IpAddr::V6(ip)) => {
            let first = ip.segments()[0];
            ip.is_loopback()
                || ip.is_unspecified()
                || (first & 0xfe00) == 0xfc00
                || (first & 0xffc0) == 0xfe80
                || ip.to_ipv4_mapped().is_some_and(|mapped| {
                    mapped.is_loopback() || mapped.is_private() || mapped.is_link_local()
                })
        }
        Err(_) => false,
    }
}

/// A search's answer: which API it came from, and a note of the ones that did
/// not answer before it.
fn search_answer(
    query: &str,
    source: &str,
    notes: &[String],
    renew: &[&str],
    body: &str,
) -> String {
    let mut answer = format!("Results for query \"{query}\" ({source}):\n\n");
    if !notes.is_empty() {
        let _ = write!(answer, "({}; these come from {source}.", notes.join("; "));
        if !renew.is_empty() {
            let mut commands = renew.to_vec();
            commands.dedup();
            let _ = write!(
                answer,
                " Ask the user to renew the key with {}.",
                commands.join(" or ")
            );
        }
        answer.push_str(")\n\n");
    }
    answer.push_str(body);
    answer
}

/// A query `DeepSeek` Search takes: at most 128 characters, cut at a word.
fn deepseek_query(query: &str) -> String {
    if query.chars().count() <= DEEPSEEK_MAX_QUERY_CHARS {
        return query.to_owned();
    }
    let cut = query
        .chars()
        .take(DEEPSEEK_MAX_QUERY_CHARS)
        .collect::<String>();
    match cut.rfind(char::is_whitespace) {
        Some(end) if end > DEEPSEEK_MAX_QUERY_CHARS / 2 => cut[..end].trim_end().to_owned(),
        _ => cut,
    }
}

/// Format a `DeepSeek` Search answer like the other providers': title, URL, the
/// site and date when known, then its model-oriented summary.
fn format_deepseek(data: &Value, query: &str, results: usize) -> String {
    let text = |item: &Value, key: &str| {
        item.get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    let entries = data
        .get("results")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .take(results)
                .enumerate()
                .map(|(index, item)| {
                    let mut entry = format!(
                        "Result {}: {}",
                        index + 1,
                        text(item, "title").unwrap_or_else(|| "Untitled".to_owned())
                    );
                    if let Some(url) = text(item, "url") {
                        let _ = write!(entry, "\nURL: {url}");
                    }
                    let about = [text(item, "site_name"), text(item, "date")]
                        .into_iter()
                        .flatten()
                        .collect::<Vec<_>>();
                    if !about.is_empty() {
                        let _ = write!(entry, "\n{}", about.join(" - "));
                    }
                    if let Some(summary) = text(item, "summary").or_else(|| text(item, "snippet")) {
                        let _ = write!(entry, "\n{summary}");
                    }
                    entry
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if entries.is_empty() {
        return format!("No results returned for query: {query}");
    }
    entries.join("\n\n---\n\n")
}

/// Format a Serper answer as prime-agent's websearch skill does.
fn format_serper(data: &Value, query: &str, results: usize) -> String {
    let text = |value: Option<&Value>| {
        value
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default()
            .to_owned()
    };
    let mut sections = Vec::new();
    if let Some(graph) = data.get("knowledgeGraph") {
        let mut lines = Vec::new();
        let title = text(graph.get("title"));
        if !title.is_empty() {
            lines.push(format!("Knowledge Graph: {title}"));
        }
        let description = text(graph.get("description"));
        if !description.is_empty() {
            lines.push(description);
        }
        if let Some(attributes) = graph.get("attributes").and_then(Value::as_object) {
            for (key, value) in attributes {
                let value = value
                    .as_str()
                    .map_or_else(|| value.to_string(), str::to_owned);
                if !value.trim().is_empty() {
                    lines.push(format!("{key}: {}", value.trim()));
                }
            }
        }
        if !lines.is_empty() {
            sections.push(lines.join("\n"));
        }
    }
    if let Some(organic) = data.get("organic").and_then(Value::as_array) {
        for (index, result) in organic.iter().take(results).enumerate() {
            let title = text(result.get("title"));
            let mut lines = vec![format!(
                "Result {}: {}",
                index + 1,
                if title.is_empty() {
                    "Untitled"
                } else {
                    title.as_str()
                }
            )];
            let link = text(result.get("link"));
            if !link.is_empty() {
                lines.push(format!("URL: {link}"));
            }
            let snippet = text(result.get("snippet"));
            if !snippet.is_empty() {
                lines.push(snippet);
            }
            sections.push(lines.join("\n"));
        }
    }
    if let Some(questions) = data.get("peopleAlsoAsk").and_then(Value::as_array) {
        let asked = questions
            .iter()
            .take(3)
            .filter_map(|item| {
                let question = text(item.get("question"));
                if question.is_empty() {
                    return None;
                }
                let answer = text(item.get("snippet"));
                Some(if answer.is_empty() {
                    format!("Q: {question}")
                } else {
                    format!("Q: {question}\nA: {answer}")
                })
            })
            .collect::<Vec<_>>();
        if !asked.is_empty() {
            sections.push(format!("People Also Ask:\n{}", asked.join("\n")));
        }
    }
    if sections.is_empty() {
        format!("No results returned for query: {query}")
    } else {
        sections.join("\n\n---\n\n")
    }
}

#[derive(Debug, PartialEq, Eq)]
struct SearchHit {
    title: String,
    url: String,
    snippet: String,
}

/// The results of a `DuckDuckGo` HTML page, in order.
fn parse_duckduckgo(page: &str, limit: usize) -> Vec<SearchHit> {
    let lower = page.to_ascii_lowercase();
    let mut hits = Vec::new();
    let mut cursor = 0;
    while hits.len() < limit {
        let Some(found) = lower[cursor..].find("class=\"result__a\"") else {
            break;
        };
        let at = cursor + found;
        let tag_start = lower[..at].rfind('<').unwrap_or(at);
        let Some(tag_end) = lower[at..].find('>').map(|offset| at + offset) else {
            break;
        };
        let tag = &page[tag_start..tag_end];
        let title_end = lower[tag_end..]
            .find("</a>")
            .map_or(page.len(), |offset| tag_end + offset);
        let title = clean_inline(&page[tag_end + 1..title_end]);
        let next = lower[title_end..]
            .find("class=\"result__a\"")
            .map_or(page.len(), |offset| title_end + offset);
        let snippet = lower[title_end..next]
            .find("class=\"result__snippet\"")
            .and_then(|offset| {
                let start = title_end + offset;
                let open = start + lower[start..].find('>')? + 1;
                let close = ["</a>", "</td>", "</div>"]
                    .iter()
                    .filter_map(|end| lower[open..].find(end))
                    .min()
                    .map_or(next, |offset| open + offset);
                Some(clean_inline(&page[open..close]))
            })
            .unwrap_or_default();
        cursor = title_end;
        let Some(href) = attribute(tag, "href") else {
            continue;
        };
        let href = decode_entities(&href);
        // Sponsored results go through an ad redirect; they are not search results.
        if href.contains("duckduckgo.com/y.js") {
            continue;
        }
        let absolute = if href.starts_with("//") {
            format!("https:{href}")
        } else {
            href.clone()
        };
        let url = Url::parse(&absolute)
            .ok()
            .and_then(|parsed| {
                parsed
                    .query_pairs()
                    .find(|(key, _)| key == "uddg")
                    .map(|(_, value)| value.into_owned())
            })
            .unwrap_or(absolute);
        // Sponsored results come through the same redirect as organic ones, with the
        // ad endpoint encoded inside `uddg`, so they are recognised after decoding.
        if title.is_empty()
            || !url.starts_with("http")
            || url.contains("duckduckgo.com/y.js")
            || url.contains("bing.com/aclick")
        {
            continue;
        }
        hits.push(SearchHit {
            title,
            url,
            snippet,
        });
    }
    hits
}

/// The value of one attribute in an HTML start tag.
fn attribute(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(found) = lower[from..].find(name) {
        let at = from + found;
        from = at + name.len();
        let before = lower[..at].chars().last();
        if !before.is_some_and(char::is_whitespace) {
            continue;
        }
        let rest = tag[at + name.len()..].trim_start();
        let Some(rest) = rest.strip_prefix('=') else {
            continue;
        };
        let rest = rest.trim_start();
        let value = match rest.chars().next() {
            Some(quote @ ('"' | '\'')) => rest[1..].split(quote).next().unwrap_or_default(),
            _ => rest
                .split(|character: char| character.is_whitespace() || character == '>')
                .next()
                .unwrap_or_default(),
        };
        return Some(value.to_owned());
    }
    None
}

/// Inline HTML as one line of text.
fn clean_inline(fragment: &str) -> String {
    let mut text = String::new();
    let mut in_tag = false;
    for character in fragment.chars() {
        match character {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => text.push(character),
            _ => {}
        }
    }
    decode_entities(&text)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Decode the entities that matter for reading: the named basics and numeric ones.
fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        // An entity is short; look for its `;` within the next twelve characters,
        // by character and not by byte, so a cut never lands inside one.
        let Some(end) = rest
            .char_indices()
            .take(12)
            .find(|(_, character)| *character == ';')
            .map(|(index, _)| index)
        else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" | "#39" => Some('\''),
            "nbsp" => Some(' '),
            "ndash" => Some('–'),
            "mdash" => Some('—'),
            "hellip" => Some('…'),
            "copy" => Some('©'),
            _ => entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
                .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                .or_else(|| {
                    entity
                        .strip_prefix('#')
                        .and_then(|decimal| decimal.parse().ok())
                })
                .and_then(char::from_u32),
        };
        if let Some(character) = decoded {
            out.push(character);
            rest = &rest[end + 1..];
        } else {
            out.push('&');
            rest = &rest[1..];
        }
    }
    out.push_str(rest);
    out
}

/// A page read as text.
struct PageText {
    title: String,
    text: String,
    links: Vec<(String, String)>,
}

/// Elements whose content is not read.
const SKIPPED: &[&str] = &[
    "script", "style", "noscript", "svg", "template", "iframe", "head", "canvas",
];

/// Elements that start a new line.
const BLOCKS: &[&str] = &[
    "p",
    "div",
    "br",
    "li",
    "ul",
    "ol",
    "tr",
    "table",
    "section",
    "article",
    "header",
    "footer",
    "nav",
    "aside",
    "main",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "pre",
    "blockquote",
    "figure",
    "figcaption",
    "dt",
    "dd",
    "hr",
    "form",
    "details",
    "summary",
];

/// Convert HTML to readable text, keeping the title and the links.
#[allow(
    clippy::too_many_lines,
    reason = "one pass over the markup, told in order"
)]
fn html_to_text(html: &str, base: &Url) -> PageText {
    let lower = html.to_ascii_lowercase();
    let title = lower
        .find("<title")
        .and_then(|start| {
            let open = start + lower[start..].find('>')? + 1;
            let close = open + lower[open..].find("</title")?;
            Some(clean_inline(&html[open..close]))
        })
        .unwrap_or_default();
    let mut text = String::new();
    let mut links: Vec<(String, String)> = Vec::new();
    let mut open_link: Option<(String, usize)> = None;
    let mut index = 0;
    while index < html.len() {
        let Some(offset) = html[index..].find('<') else {
            text.push_str(&html[index..]);
            break;
        };
        text.push_str(&html[index..index + offset]);
        let start = index + offset;
        if lower[start..].starts_with("<!--") {
            index = lower[start..]
                .find("-->")
                .map_or(html.len(), |end| start + end + 3);
            continue;
        }
        let Some(end) = lower[start..].find('>').map(|end| start + end) else {
            break;
        };
        let tag = &lower[start + 1..end];
        let closing = tag.starts_with('/');
        let name = tag
            .trim_start_matches('/')
            .split(|character: char| character.is_whitespace() || character == '/')
            .next()
            .unwrap_or_default()
            .to_owned();
        index = end + 1;
        if !closing && SKIPPED.contains(&name.as_str()) {
            let close = format!("</{name}");
            index = lower[index..]
                .find(&close)
                .and_then(|found| {
                    let after = index + found;
                    lower[after..].find('>').map(|gt| after + gt + 1)
                })
                .unwrap_or(html.len());
            continue;
        }
        if BLOCKS.contains(&name.as_str()) {
            text.push('\n');
            if name == "li" && !closing {
                text.push_str("- ");
            }
        } else if name == "td" || name == "th" {
            text.push(' ');
        }
        if name == "a" {
            if closing {
                if let Some((href, from)) = open_link.take() {
                    let label = clean_inline(&text[from.min(text.len())..]);
                    if !label.is_empty()
                        && links.len() < MAX_LINKS
                        && !links.iter().any(|(_, known)| known == &href)
                    {
                        links.push((label, href));
                    }
                }
            } else if let Some(href) = attribute(&html[start + 1..end], "href") {
                let href = decode_entities(&href);
                if let Ok(resolved) = base.join(&href)
                    && matches!(resolved.scheme(), "http" | "https")
                {
                    open_link = Some((resolved.to_string(), text.len()));
                }
            }
        }
    }
    let decoded = decode_entities(&text);
    let mut rows = Vec::new();
    let mut blank = 0;
    for row in decoded.lines() {
        let row = row.split_whitespace().collect::<Vec<_>>().join(" ");
        if row.is_empty() {
            blank += 1;
            if blank == 1 && !rows.is_empty() {
                rows.push(String::new());
            }
        } else {
            blank = 0;
            rows.push(row);
        }
    }
    while rows.last().is_some_and(String::is_empty) {
        rows.pop();
    }
    PageText {
        title,
        text: rows.join("\n"),
        links,
    }
}

impl ExternalToolDispatcher for WebHost {
    fn validate_external<'a>(
        &'a self,
        plugin_id: &'a str,
        tool_name: &'a str,
        arguments: &'a Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), HarnessError>> + Send + 'a>>
    {
        Box::pin(async move {
            if plugin_id != "web" {
                return Err(HarnessError::new(
                    ErrorCode::PolicyDenied,
                    "web tool target is unavailable",
                ));
            }
            self.parse(tool_name, arguments).map(|_| ())
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
            if plugin_id != "web" {
                return Err(HarnessError::new(
                    ErrorCode::PolicyDenied,
                    "web tool target is unavailable",
                ));
            }
            let call = self.parse(tool_name, arguments)?;
            // Both tools only read, so a failed request changed nothing: it is
            // reported as a failure the model can act on (try another source),
            // not as a run whose side effects are uncertain.
            Ok(match self.run(call).await {
                Ok(text) => ToolOutput::ExternalTool {
                    plugin_id: "web".to_owned(),
                    tool_name: tool_name.to_owned(),
                    payload: json!({ "text": text }),
                    inflight: 1,
                },
                Err(error) => ToolOutput::Denied {
                    code: error.code().as_str().to_owned(),
                    reason: error.message().to_owned(),
                },
            })
        })
    }
}

#[cfg(test)]
mod tests {
    /// A 404 on a GitHub file names the repository's file list.
    #[test]
    fn a_missing_github_file_points_at_the_file_list() {
        let error = super::http_error(
            "https://github.com/owner/repo/blob/main/docs/invariants.md",
            reqwest::StatusCode::NOT_FOUND,
        );
        assert!(
            error.to_string().contains(
                "web_fetch https://api.github.com/repos/owner/repo/git/trees/main?recursive=1"
            ),
            "{error}"
        );
        assert_eq!(
            super::github_tree("https://raw.githubusercontent.com/o/r/dev/a.md").as_deref(),
            Some("https://api.github.com/repos/o/r/git/trees/dev?recursive=1")
        );
        assert!(super::github_tree("https://example.com/a/b/blob/c/d").is_none());
    }

    use super::{
        SearchApi, Searcher, WebHost, deepseek_query, format_serper, html_to_text, is_private_host,
        parse_duckduckgo, window_of,
    };
    use harness_tools::ExternalToolDispatcher;
    use reqwest::Url;
    use serde_json::json;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const DUCK_PAGE: &str = r#"<html><body>
        <div class="result results_links"><a rel="nofollow" class="result__a" href="//duckduckgo.com/y.js?ad_provider=x&amp;u3=ad">Sponsored thing</a></div>
        <div class="result results_links"><a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fduckduckgo.com%2Fy.js%3Fad_domain%3Dudemy.com&amp;rut=1">Encoded ad</a></div>
        <div class="result"><h2><a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fwww.rust-lang.org%2Flearn&amp;rut=abc">Learn <b>Rust</b></a></h2>
        <a class="result__snippet" href="x">The <b>Rust</b> book &amp; more.</a></div>
        <div class="result"><h2><a rel="nofollow" class="result__a" href="https://doc.rust-lang.org/std/">std - Rust</a></h2>
        <a class="result__snippet" href="y">The standard library.</a></div>
    </body></html>"#;

    const ARTICLE: &str = r#"<!DOCTYPE html><html><head><title>Memory &amp; agents</title>
        <style>body{color:red}</style><script>alert("x")</script></head>
        <body><nav><a href="/">Home</a></nav>
        <h1>Agent memory</h1><p>Facts are extracted&nbsp;after each turn.</p>
        <ul><li>L0 conversation</li><li>L1 atoms</li></ul>
        <p>See <a href="https://example.org/paper">the paper</a> and <a href="/docs/intro">intro</a>.</p>
        <!-- hidden --><noscript>enable js</noscript></body></html>"#;

    #[test]
    fn html_becomes_readable_text_with_its_title_and_links() {
        let base = Url::parse("https://site.test/blog/post").expect("base");
        let page = html_to_text(ARTICLE, &base);
        assert_eq!(page.title, "Memory & agents");
        for expected in [
            "Agent memory",
            "Facts are extracted after each turn.",
            "- L0 conversation",
            "- L1 atoms",
        ] {
            assert!(
                page.text.contains(expected),
                "{expected:?} in:\n{}",
                page.text
            );
        }
        for absent in ["alert", "color:red", "hidden", "enable js"] {
            assert!(
                !page.text.contains(absent),
                "{absent:?} leaked:\n{}",
                page.text
            );
        }
        assert!(page.links.contains(&(
            "the paper".to_owned(),
            "https://example.org/paper".to_owned()
        )));
        assert!(
            page.links
                .iter()
                .any(|(_, link)| link == "https://site.test/docs/intro"),
            "relative links resolve against the page: {:?}",
            page.links
        );
    }

    /// Found by a live fetch: an `&` followed by multi-byte text was sliced at a byte
    /// offset inside a character and the tool panicked.
    #[test]
    fn entities_decode_next_to_multibyte_text_without_panicking() {
        assert_eq!(
            super::decode_entities("Tôi & bạn &amp; đồng đội &#x1F980; — mã &nbsp;xong"),
            "Tôi & bạn & đồng đội 🦀 — mã  xong"
        );
        assert_eq!(
            super::decode_entities("&đường dài không có dấu chấm phẩy"),
            "&đường dài không có dấu chấm phẩy"
        );
    }

    #[test]
    fn duckduckgo_results_are_parsed_in_order_without_ads() {
        let hits = parse_duckduckgo(DUCK_PAGE, 5);
        assert_eq!(hits.len(), 2, "{hits:?}");
        assert_eq!(hits[0].title, "Learn Rust");
        assert_eq!(hits[0].url, "https://www.rust-lang.org/learn");
        assert_eq!(hits[0].snippet, "The Rust book & more.");
        assert_eq!(hits[1].url, "https://doc.rust-lang.org/std/");
        assert_eq!(parse_duckduckgo(DUCK_PAGE, 1).len(), 1);
    }

    #[test]
    fn serper_answers_are_formatted_like_prime_agent() {
        let data = json!({
            "knowledgeGraph": {"title": "Rust", "description": "A language.", "attributes": {"Designed by": "Graydon Hoare"}},
            "organic": [
                {"title": "Rust", "link": "https://rust-lang.org", "snippet": "Fast and safe."},
                {"title": "Book", "link": "https://doc.rust-lang.org/book", "snippet": "Learn it."}
            ],
            "peopleAlsoAsk": [{"question": "Is Rust hard?", "snippet": "At first."}]
        });
        let text = format_serper(&data, "rust", 1);
        for expected in [
            "Knowledge Graph: Rust",
            "Designed by: Graydon Hoare",
            "Result 1: Rust\nURL: https://rust-lang.org\nFast and safe.",
            "Q: Is Rust hard?\nA: At first.",
        ] {
            assert!(text.contains(expected), "{expected:?} in:\n{text}");
        }
        assert!(
            !text.contains("doc.rust-lang.org/book"),
            "num_results is honoured"
        );
        assert_eq!(
            format_serper(&json!({}), "nothing", 5),
            "No results returned for query: nothing"
        );
    }

    #[test]
    fn local_and_private_addresses_are_recognised() {
        for private in [
            "http://localhost:8080/",
            "http://127.0.0.1/",
            "http://10.1.2.3/",
            "http://192.168.1.1/",
            "http://172.16.0.5/",
            "http://169.254.169.254/latest/meta-data",
            "http://[::1]/",
            "http://[fd00::1]/",
            "http://printer.local/",
            "http://0.0.0.0/",
        ] {
            assert!(
                is_private_host(&Url::parse(private).expect(private)),
                "{private}"
            );
        }
        for public in [
            "https://example.org/",
            "https://8.8.8.8/",
            "https://[2606:4700::1111]/",
        ] {
            assert!(
                !is_private_host(&Url::parse(public).expect(public)),
                "{public}"
            );
        }
    }

    #[test]
    fn long_pages_come_in_windows_that_say_where_to_continue() {
        let document = "a".repeat(2_500);
        let first = window_of(&document, 0, 1_000);
        assert!(first.contains("start_index=1000"), "{first}");
        let last = window_of(&document, 2_000, 1_000);
        assert!(last.contains("end of page"), "{last}");
        assert!(window_of(&document, 9_000, 1_000).contains("past the end"));
        assert_eq!(window_of("short", 0, 1_000), "short");
    }

    /// Serve fixed answers on loopback: `/html/` is a `DuckDuckGo` results page,
    /// `/article` an HTML page, `/search` a Serper answer (POST), `/pdf` a binary file.
    #[allow(clippy::too_many_lines)]
    async fn serve() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let address = listener.local_addr().expect("address");
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut buffer = [0_u8; 4096];
                    loop {
                        let Ok(read) = socket.read(&mut buffer).await else {
                            return;
                        };
                        if read == 0 {
                            break;
                        }
                        request.extend_from_slice(&buffer[..read]);
                        let text = String::from_utf8_lossy(&request);
                        if let Some(end) = text.find("\r\n\r\n") {
                            let length = text[..end]
                                .lines()
                                .find_map(|line| {
                                    let (name, value) = line.split_once(':')?;
                                    name.eq_ignore_ascii_case("content-length")
                                        .then(|| value.trim().parse::<usize>().ok())?
                                })
                                .unwrap_or(0);
                            if request.len() >= end + 4 + length {
                                break;
                            }
                        }
                    }
                    let text = String::from_utf8_lossy(&request).into_owned();
                    let path = text.split_whitespace().nth(1).unwrap_or("/").to_owned();
                    let mut status = "200 OK".to_owned();
                    let (kind, body): (&str, Vec<u8>) = if let Some(code) =
                        path.strip_prefix("/status/")
                    {
                        status = format!("{code} Status");
                        ("text/plain", b"no".to_vec())
                    } else if path.starts_with("/denied") {
                        status = "403 Forbidden".to_owned();
                        (
                            "application/json",
                            br#"{"message":"Unauthorized."}"#.to_vec(),
                        )
                    } else if path.starts_with("/html/") {
                        assert!(text.starts_with("POST "), "the form is posted: {text}");
                        assert!(text.contains("q=rust+book"), "{text}");
                        ("text/html; charset=utf-8", DUCK_PAGE.as_bytes().to_vec())
                    } else if path.starts_with("/article") {
                        ("text/html", ARTICLE.as_bytes().to_vec())
                    } else if path.starts_with("/ds/search") {
                        let lower = text.to_ascii_lowercase();
                        assert!(lower.contains("\"need_content\":false"), "{text}");
                        if lower.contains("bearer ds-good") {
                            (
                                "application/json",
                                serde_json::to_vec(&json!({"results": [{
                                    "rerank_score": 2.1,
                                    "title": "The Rust Book",
                                    "url": "https://doc.rust-lang.org/book/",
                                    "snippet": "short",
                                    "summary": "The Rust Programming Language, the official book.",
                                    "content": "",
                                    "site_name": "Rust",
                                    "site_icon": null,
                                    "date": "2026-01-02T00:00:00Z",
                                    "authority": 4
                                }]}))
                                .expect("json"),
                            )
                        } else {
                            status = "402 Payment Required".to_owned();
                            (
                                "application/json",
                                br#"{"error":{"message":"Insufficient Balance","type":"payment","param":null,"code":"insufficient_balance"}}"#.to_vec(),
                            )
                        }
                    } else if path.starts_with("/ds/fetch") {
                        assert!(
                            text.to_ascii_lowercase().contains("bearer ds-good"),
                            "{text}"
                        );
                        (
                            "application/json",
                            serde_json::to_vec(&json!({"title": "Relayed page", "content": "# Hello\n\nRead through the Fetch API."}))
                                .expect("json"),
                        )
                    } else if path.starts_with("/search") {
                        assert!(
                            text.contains("x-api-key: test-key")
                                || text.contains("X-API-KEY: test-key"),
                            "{text}"
                        );
                        (
                            "application/json",
                            serde_json::to_vec(&json!({"organic": [{"title": "Rust", "link": "https://rust-lang.org", "snippet": "Fast."}]})).expect("json"),
                        )
                    } else {
                        ("application/pdf", vec![0x25, 0x50, 0x44, 0x46, 0, 1, 2, 3])
                    };
                    let head = format!(
                        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    socket.write_all(head.as_bytes()).await.expect("headers");
                    socket.write_all(&body).await.expect("body");
                    socket.shutdown().await.expect("response shutdown");
                    // Keep the read half alive until the client has consumed the
                    // response and closed. Dropping the socket immediately can
                    // reset a still-active connection on Windows.
                    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                        while let Ok(read) = socket.read(&mut buffer).await {
                            if read == 0 {
                                break;
                            }
                        }
                    })
                    .await;
                });
            }
        });
        format!("http://{address}")
    }

    /// The dispatch path without its authorization token, which only the tool gate
    /// can mint: validate the arguments, then run the call.
    async fn call(
        host: &WebHost,
        tool: &str,
        arguments: serde_json::Value,
    ) -> Result<String, String> {
        let call = host
            .parse(tool, &arguments)
            .map_err(|error| error.to_string())?;
        host.run(call).await.map_err(|error| error.to_string())
    }

    fn searcher(api: SearchApi, key: &str, endpoint: String) -> Searcher {
        Searcher {
            api,
            key: key.to_owned(),
            endpoint,
        }
    }

    /// The whole path the agent takes: search, then open a result - without a key
    /// (the keyless fallback) and with one (Serper).
    #[tokio::test]
    async fn search_and_fetch_work_end_to_end_with_and_without_a_key() {
        let base = serve().await;
        let keyless =
            WebHost::build(Vec::new(), None, format!("{base}/html/"), true).expect("client");
        let found = call(&keyless, "web_search", json!({"query": "rust book"}))
            .await
            .expect("fallback search");
        assert!(found.contains("Result 1: Learn Rust"), "{found}");
        assert!(
            found.contains("URL: https://www.rust-lang.org/learn"),
            "{found}"
        );

        let page = call(
            &keyless,
            "web_fetch",
            json!({"url": format!("{base}/article")}),
        )
        .await
        .expect("fetch");
        assert!(page.contains("Title: Memory & agents"), "{page}");
        assert!(
            page.contains("Facts are extracted after each turn."),
            "{page}"
        );
        assert!(page.contains("Links:"), "{page}");

        let binary = call(
            &keyless,
            "web_fetch",
            json!({"url": format!("{base}/report.pdf")}),
        )
        .await
        .expect_err("a binary file is refused");
        assert!(binary.contains("application/pdf"), "{binary}");

        let keyed = WebHost::build(
            vec![searcher(
                SearchApi::Serper,
                "test-key",
                format!("{base}/search"),
            )],
            None,
            format!("{base}/html/"),
            true,
        )
        .expect("client");
        let google = call(
            &keyed,
            "web_search",
            json!({"query": "rust", "num_results": 3}),
        )
        .await
        .expect("serper search");
        assert!(
            google.contains("Result 1: Rust\nURL: https://rust-lang.org\nFast."),
            "{google}"
        );
    }

    /// A failed page says what retrying can do: every status read as
    /// `service_unavailable` (transient), so dead and closed pages were fetched
    /// again. A Serper key it refuses falls back to the keyless search.
    #[tokio::test]
    async fn a_failed_page_says_whether_asking_again_can_help() {
        let base = serve().await;
        let host = WebHost::build(
            vec![searcher(
                SearchApi::Serper,
                "denied-key",
                format!("{base}/denied"),
            )],
            None,
            format!("{base}/html/"),
            true,
        )
        .expect("client");
        for (status, code) in [
            (404, super::ErrorCode::ResourceNotFound),
            (410, super::ErrorCode::ResourceNotFound),
            (403, super::ErrorCode::AccessDenied),
            (401, super::ErrorCode::AccessDenied),
            (429, super::ErrorCode::RateLimited),
            (503, super::ErrorCode::ServiceUnavailable),
            (400, super::ErrorCode::InvalidPayload),
        ] {
            let parsed = host
                .parse(
                    "web_fetch",
                    &json!({"url": format!("{base}/status/{status}")}),
                )
                .expect("arguments");
            let error = host.run(parsed).await.expect_err("a failed page");
            assert_eq!(error.code(), code, "{status}: {}", error.message());
            assert!(
                error.message().contains(&status.to_string()),
                "{}",
                error.message()
            );
        }
        let found = call(&host, "web_search", json!({"query": "rust book"}))
            .await
            .expect("the keyless search answers");
        assert!(found.contains("Result 1: Learn Rust"), "{found}");
        assert!(found.contains("Google via Serper failed"), "{found}");
        assert!(found.contains("/login serper"), "{found}");
        assert!(found.contains("(DuckDuckGo)"), "{found}");
    }

    /// The first API with a key answers; one whose key is refused (here out of
    /// balance) hands the query to the next and is skipped by later searches,
    /// and a site that refuses ha is read through `DeepSeek` Fetch.
    #[tokio::test]
    async fn search_moves_to_the_next_api_when_a_key_is_refused() {
        let base = serve().await;
        let deepseek = WebHost::build(
            vec![
                searcher(SearchApi::DeepSeek, "ds-good", format!("{base}/ds/search")),
                searcher(SearchApi::Serper, "test-key", format!("{base}/search")),
            ],
            Some((format!("{base}/ds/fetch"), "ds-good".to_owned())),
            format!("{base}/html/"),
            true,
        )
        .expect("client");
        let found = call(&deepseek, "web_search", json!({"query": "rust book"}))
            .await
            .expect("deepseek search");
        assert!(found.contains("(DeepSeek Search)"), "{found}");
        assert!(
            found.contains(
                "Result 1: The Rust Book\nURL: https://doc.rust-lang.org/book/\nRust - 2026-01-02T00:00:00Z\nThe Rust Programming Language, the official book."
            ),
            "{found}"
        );
        let relayed = call(
            &deepseek,
            "web_fetch",
            json!({"url": format!("{base}/denied/page")}),
        )
        .await
        .expect("read through DeepSeek Fetch");
        assert!(relayed.contains("read through DeepSeek Fetch"), "{relayed}");
        assert!(relayed.contains("Title: Relayed page"), "{relayed}");
        assert!(relayed.contains("Read through the Fetch API."), "{relayed}");
        let missing = call(
            &deepseek,
            "web_fetch",
            json!({"url": format!("{base}/status/404")}),
        )
        .await
        .expect_err("a page that does not exist is not relayed");
        assert!(missing.contains("404"), "{missing}");

        let expired = WebHost::build(
            vec![
                searcher(
                    SearchApi::DeepSeek,
                    "ds-expired",
                    format!("{base}/ds/search"),
                ),
                searcher(SearchApi::Serper, "test-key", format!("{base}/search")),
            ],
            None,
            format!("{base}/html/"),
            true,
        )
        .expect("client");
        let first = call(&expired, "web_search", json!({"query": "rust"}))
            .await
            .expect("serper answers");
        assert!(first.contains("(Google via Serper)"), "{first}");
        assert!(first.contains("DeepSeek Search failed"), "{first}");
        assert!(first.contains("402"), "{first}");
        assert!(
            first.contains("Insufficient Balance") && !first.contains("\"type\""),
            "the API's own reason, not its JSON: {first}"
        );
        assert!(first.contains("/login deepseek"), "{first}");
        assert!(first.contains("Result 1: Rust"), "{first}");
        let second = call(&expired, "web_search", json!({"query": "rust"}))
            .await
            .expect("serper answers again");
        assert!(
            second.contains("DeepSeek Search skipped: it refused its key"),
            "{second}"
        );
        assert!(second.contains("(Google via Serper)"), "{second}");
    }

    #[test]
    fn the_search_order_is_read_from_ids_and_long_queries_fit_deepseek() {
        assert_eq!(
            SearchApi::order(&[
                "serper".to_owned(),
                "DeepSeek".to_owned(),
                "serper".to_owned(),
                "bing".to_owned()
            ]),
            vec![SearchApi::Serper, SearchApi::DeepSeek]
        );
        let long = "word ".repeat(40);
        let cut = deepseek_query(&long);
        assert!(cut.chars().count() <= 128, "{cut}");
        assert!(cut.ends_with("word"), "{cut:?}");
        assert_eq!(deepseek_query("short query"), "short query");
    }

    /// A page on the web must not steer the agent into the user's own network.
    #[tokio::test]
    async fn private_addresses_and_other_schemes_are_refused_before_any_request() {
        let host = WebHost::build(
            Vec::new(),
            None,
            "https://html.duckduckgo.com/html/".to_owned(),
            false,
        )
        .expect("client");
        for url in [
            "http://127.0.0.1:9/secret",
            "http://169.254.169.254/latest/meta-data",
            "http://localhost/admin",
        ] {
            let refused = host
                .validate_external("web", "web_fetch", &json!({ "url": url }))
                .await
                .expect_err(url);
            assert!(refused.to_string().contains("private"), "{url}: {refused}");
        }
        for url in ["file:///etc/passwd", "ftp://example.org/x"] {
            assert!(
                host.validate_external("web", "web_fetch", &json!({ "url": url }))
                    .await
                    .is_err(),
                "{url}"
            );
        }
        assert!(
            host.validate_external("web", "web_search", &json!({ "query": "  " }))
                .await
                .is_err()
        );
    }
}
