//! Web access for the agent: `web_search` and `web_fetch`.
//!
//! Measured: asked to research a topic, the agent activated `deep-research` and then
//! said it had no way to reach the internet - the skill describes a method (search
//! several angles, open the full pages, synthesise) and assumes a search tool and a
//! page-open tool the harness never had. prime-agent ships the same capability as its
//! `websearch` skill (a Serper call made from the Python kernel, pages fetched with
//! `httpx`); `ha` has no Python kernel, so the two operations are host tools here:
//!
//! - `web_search` asks Google through Serper when `SERPER_API_KEY` is set, formatted
//!   the way prime-agent formats it (knowledge graph, organic results, people also
//!   ask), and falls back to `DuckDuckGo`'s HTML endpoint, which needs no key, so search
//!   works on a fresh install;
//! - `web_fetch` downloads one http(s) page and returns readable text - scripts and
//!   styles dropped, blocks on their own lines, entities decoded - with the page's
//!   links, in windows the model pages through with `start_index`.
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
    serper_key: Option<String>,
    serper_endpoint: String,
    fallback_endpoint: String,
    allow_private_hosts: bool,
}

impl WebHost {
    /// The web tools as the environment configures them, or `None` when turned off.
    #[must_use]
    pub fn from_environment(environment: &LaunchEnvironment) -> Option<Self> {
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
        Self::build(
            value(SERPER_KEY_VARIABLE),
            SERPER_ENDPOINT.to_owned(),
            FALLBACK_ENDPOINT.to_owned(),
            false,
        )
    }

    fn build(
        serper_key: Option<String>,
        serper_endpoint: String,
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
        let client = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(REQUEST_TIMEOUT)
            .redirect(redirect)
            .build()
            .ok()?;
        Some(Self {
            inner: Arc::new(WebInner {
                client,
                serper_key,
                serper_endpoint,
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

    async fn search(&self, query: &str, results: usize) -> Result<String, HarnessError> {
        let body = match &self.inner.serper_key {
            Some(key) => self.serper(query, key, results).await?,
            None => self.fallback_search(query, results).await?,
        };
        Ok(format!("Results for query \"{query}\":\n\n{body}"))
    }

    async fn serper(&self, query: &str, key: &str, results: usize) -> Result<String, HarnessError> {
        let response = self
            .inner
            .client
            .post(&self.inner.serper_endpoint)
            .header("X-API-KEY", key)
            .json(&json!({ "q": query, "num": results }))
            .send()
            .await
            .map_err(|error| unavailable(format!("the search request failed: {error}")))?;
        let status = response.status();
        let bytes = read_bounded(response).await?;
        if !status.is_success() {
            return Err(unavailable(format!(
                "Serper answered {status}: {}",
                String::from_utf8_lossy(&bytes)
                    .chars()
                    .take(300)
                    .collect::<String>()
            )));
        }
        let data: Value = serde_json::from_slice(&bytes)
            .map_err(|_| unavailable("Serper did not answer with JSON"))?;
        Ok(format_serper(&data, query, results))
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
            .map_err(|error| unavailable(format!("the search request failed: {error}")))?;
        let status = response.status();
        let bytes = read_bounded(response).await?;
        let page = String::from_utf8_lossy(&bytes);
        let hits = parse_duckduckgo(&page, results);
        if !status.is_success() || hits.is_empty() {
            return Ok(format!(
                "No results were returned (the keyless search provider answered {status}; it may be rate-limiting). \
                 Try a different query, or ask the user to set {SERPER_KEY_VARIABLE} (a free key from https://serper.dev) for Google results."
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

    async fn fetch(&self, url: &Url, start: usize, window: usize) -> Result<String, HarnessError> {
        let response = self
            .inner
            .client
            .get(url.clone())
            .send()
            .await
            .map_err(|error| unavailable(format!("{url} could not be opened: {error}")))?;
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
            return Err(unavailable(format!("{final_url} answered {status}")));
        }
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
            let page = html_to_text(&String::from_utf8_lossy(&bytes), &final_url);
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
            format!("URL: {final_url}\n\n{}", String::from_utf8_lossy(&bytes))
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

fn unavailable(message: impl Into<String>) -> HarnessError {
    HarnessError::new(ErrorCode::ServiceUnavailable, message)
}

/// Read a response body, stopping at [`MAX_DOWNLOAD_BYTES`].
async fn read_bounded(mut response: reqwest::Response) -> Result<Vec<u8>, HarnessError> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| unavailable(format!("the response could not be read: {error}")))?
    {
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
            let text = self.run(call).await?;
            Ok(ToolOutput::ExternalTool {
                plugin_id: "web".to_owned(),
                tool_name: tool_name.to_owned(),
                payload: json!({ "text": text }),
                inflight: 1,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        WebHost, format_serper, html_to_text, is_private_host, parse_duckduckgo, window_of,
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
                    let (kind, body): (&str, Vec<u8>) = if path.starts_with("/html/") {
                        assert!(text.starts_with("POST "), "the form is posted: {text}");
                        assert!(text.contains("q=rust+book"), "{text}");
                        ("text/html; charset=utf-8", DUCK_PAGE.as_bytes().to_vec())
                    } else if path.starts_with("/article") {
                        ("text/html", ARTICLE.as_bytes().to_vec())
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
                        "HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = socket.write_all(head.as_bytes()).await;
                    let _ = socket.write_all(&body).await;
                    let _ = socket.shutdown().await;
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

    /// The whole path the agent takes: search, then open a result - without a key
    /// (the keyless fallback) and with one (Serper).
    #[tokio::test]
    async fn search_and_fetch_work_end_to_end_with_and_without_a_key() {
        let base = serve().await;
        let keyless = WebHost::build(
            None,
            format!("{base}/search"),
            format!("{base}/html/"),
            true,
        )
        .expect("client");
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
            Some("test-key".to_owned()),
            format!("{base}/search"),
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

    /// A page on the web must not steer the agent into the user's own network.
    #[tokio::test]
    async fn private_addresses_and_other_schemes_are_refused_before_any_request() {
        let host = WebHost::build(
            None,
            "https://google.serper.dev/search".to_owned(),
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
