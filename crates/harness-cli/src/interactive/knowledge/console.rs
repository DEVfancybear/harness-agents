//! Loopback-only Knowledge console. Model tools never start or control it.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use harness_types::{ContentHash, TaskId};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::KnowledgeHost;

const MAX_HEAD: usize = 16 * 1024;
const MAX_BODY: usize = 34 * 1024 * 1024;
const ASSETS: &str = "text/javascript; charset=utf-8";

struct Request {
    method: String,
    target: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

struct Server {
    host: KnowledgeHost,
    address: SocketAddr,
    token: String,
    cookie: String,
}

/// The caller explicitly starts this foreground server; binding is always local.
pub fn serve(host: KnowledgeHost, port: u16, json_output: bool) -> Result<(), String> {
    // The CLI already runs inside Tokio; keep this foreground service on its
    // own thread so both terminal and synchronous callers can start it safely.
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|error| error.to_string())?
            .block_on(run(host, port, json_output))
    })
    .join()
    .map_err(|_| "Knowledge console worker failed".to_owned())?
}

async fn run(host: KnowledgeHost, port: u16, json_output: bool) -> Result<(), String> {
    let listener = match TcpListener::bind((std::net::Ipv6Addr::LOCALHOST, port)).await {
        Ok(listener) => listener,
        Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
            return Err(format!("cannot start Knowledge console: {error}"));
        }
        Err(_) => TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port))
            .await
            .map_err(|error| format!("cannot start Knowledge console: {error}"))?,
    };
    let address = listener.local_addr().map_err(|error| error.to_string())?;
    let entropy = format!("{}:{}", TaskId::generate(), TaskId::generate());
    let server = Arc::new(Server {
        host,
        address,
        token: ContentHash::from_bytes(entropy.as_bytes())
            .as_str()
            .to_owned(),
        cookie: format!("ha_knowledge_{}", address.port()),
    });
    let url = format!("http://{address}/");
    if json_output {
        println!(
            "{}",
            json!({"schema_version":1,"url":url,"pid":std::process::id()})
        );
    } else {
        println!("Knowledge console: {url}\nPress Ctrl+C to stop.");
    }
    let slots = Arc::new(tokio::sync::Semaphore::new(8));
    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);
    loop {
        let permit = tokio::select! {
            _ = &mut shutdown => break,
            permit = Arc::clone(&slots).acquire_owned() => permit.map_err(|error| error.to_string())?,
        };
        let socket = tokio::select! {
            _ = &mut shutdown => break,
            accepted = listener.accept() => accepted.map_err(|error| error.to_string())?.0,
        };
        let server = Arc::clone(&server);
        tokio::spawn(async move {
            let _permit = permit;
            let _ = tokio::time::timeout(Duration::from_mins(1), connection(socket, server)).await;
        });
    }
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "A single bounded HTTP exchange keeps authorization before dispatch explicit"
)]
async fn connection(mut socket: TcpStream, server: Arc<Server>) -> Result<(), String> {
    let request =
        match tokio::time::timeout(Duration::from_secs(15), read_request(&mut socket)).await {
            Ok(Ok(request)) => request,
            Ok(Err(error)) => {
                return respond(
                    &mut socket,
                    400,
                    "application/json",
                    &json!({"error":error}).to_string(),
                    None,
                )
                .await;
            }
            Err(_) => {
                return respond(
                    &mut socket,
                    408,
                    "application/json",
                    r#"{"error":"request timed out"}"#,
                    None,
                )
                .await;
            }
        };
    let expected_host = server.address.to_string();
    if request.headers.get("host") != Some(&expected_host) {
        return respond(
            &mut socket,
            403,
            "application/json",
            r#"{"error":"invalid console host"}"#,
            None,
        )
        .await;
    }
    let origin = format!("http://{}", server.address);
    let external_navigation = request.method == "GET"
        && request.target.split('?').next() == Some("/")
        && request
            .headers
            .get("sec-fetch-mode")
            .is_some_and(|mode| mode == "navigate")
        && request
            .headers
            .get("sec-fetch-dest")
            .is_some_and(|destination| destination == "document");
    if request
        .headers
        .get("origin")
        .is_some_and(|value| value != &origin)
        || request
            .headers
            .get("sec-fetch-site")
            .is_some_and(|value| value == "cross-site" && !external_navigation)
    {
        return respond(
            &mut socket,
            403,
            "application/json",
            r#"{"error":"cross-origin access is refused"}"#,
            None,
        )
        .await;
    }
    if request.method == "GET" {
        let path = request.target.split('?').next().unwrap_or_default();
        if let Some((mime, body)) = asset(path) {
            let cookie = (path == "/").then(|| {
                format!(
                    "{}={}; HttpOnly; SameSite=Strict; Path=/",
                    server.cookie, server.token
                )
            });
            return respond(&mut socket, 200, mime, body, cookie.as_deref()).await;
        }
    }
    if !authorized(&request, &server.cookie, &server.token) {
        return respond(
            &mut socket,
            403,
            "application/json",
            r#"{"error":"open the console page to authenticate"}"#,
            None,
        )
        .await;
    }
    let (route, args) = match route(&request) {
        Ok(route) => route,
        Err(error) => {
            return respond(
                &mut socket,
                400,
                "application/json",
                &json!({"error":error}).to_string(),
                None,
            )
            .await;
        }
    };
    let host = server.host.clone();
    let result = tokio::task::spawn_blocking(move || host.console_request(&route, &args))
        .await
        .map_err(|error| format!("console operation failed: {error}"))?;
    let (status, body) = match result {
        Ok(value) => (200, value.to_string()),
        Err(error) => (error_status(&error), json!({"error":error}).to_string()),
    };
    respond(
        &mut socket,
        status,
        "application/json; charset=utf-8",
        &body,
        None,
    )
    .await
}

fn authorized(request: &Request, cookie: &str, token: &str) -> bool {
    request.headers.get("cookie").is_some_and(|cookies| {
        cookies
            .split(';')
            .filter_map(|value| value.trim().split_once('='))
            .any(|(name, value)| name == cookie && value == token)
    })
}

fn error_status(error: &str) -> u16 {
    if error.contains("is off") || error.contains("read-only") {
        403
    } else if error.contains("not found") {
        404
    } else if error.contains("hash")
        || error.contains("conflict")
        || error.contains("already exists")
        || error.contains("backlinks")
    {
        409
    } else {
        400
    }
}

fn route(request: &Request) -> Result<(String, Value), String> {
    let (path, query) = request
        .target
        .split_once('?')
        .unwrap_or((&request.target, ""));
    let mut args = serde_json::Map::new();
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let key = decode(key)?;
        if args.insert(key, Value::String(decode(value)?)).is_some() {
            return Err("duplicate query parameter".to_owned());
        }
    }
    let route = match (request.method.as_str(), path) {
        ("GET", "/api/state") => "state",
        ("GET", "/api/pages") => "pages",
        ("GET", "/api/page") => "page",
        ("GET", "/api/graph") => "graph",
        ("GET", "/api/search") => "search",
        ("POST", "/api/control") => "control",
        ("POST", "/api/manage") => "manage",
        _ => return Err("unknown console endpoint or method".to_owned()),
    };
    if request.method == "POST" {
        if !request.headers.get("content-type").is_some_and(|value| {
            value
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .eq_ignore_ascii_case("application/json")
        }) {
            return Err("console mutations require application/json".to_owned());
        }
        if !args.is_empty() {
            return Err("mutations take a JSON body, not query parameters".to_owned());
        }
        let body: Value =
            serde_json::from_slice(&request.body).map_err(|_| "invalid JSON request".to_owned())?;
        if !body.is_object() {
            return Err("console takes a JSON object".to_owned());
        }
        return Ok((route.to_owned(), body));
    }
    Ok((route.to_owned(), Value::Object(args)))
}

fn decode(value: &str) -> Result<String, String> {
    let mut bytes = Vec::with_capacity(value.len());
    let mut input = value.bytes();
    while let Some(byte) = input.next() {
        bytes.push(match byte {
            b'+' => b' ',
            b'%' => {
                let a = input.next().and_then(hex).ok_or("invalid URL encoding")?;
                let b = input.next().and_then(hex).ok_or("invalid URL encoding")?;
                a * 16 + b
            }
            value => value,
        });
    }
    let text = String::from_utf8(bytes).map_err(|_| "URL must be UTF-8".to_owned())?;
    if text.chars().any(char::is_control) {
        return Err("URL contains a control character".to_owned());
    }
    Ok(text)
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

async fn read_request(socket: &mut TcpStream) -> Result<Request, String> {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 4096];
    let head_end = loop {
        let count = socket
            .read(&mut buffer)
            .await
            .map_err(|error| error.to_string())?;
        if count == 0 {
            return Err("incomplete request".to_owned());
        }
        bytes.extend_from_slice(&buffer[..count]);
        if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            if end > MAX_HEAD {
                return Err("request headers exceed limit".to_owned());
            }
            break end + 4;
        }
        if bytes.len() > MAX_HEAD {
            return Err("request headers exceed limit".to_owned());
        }
    };
    let head = std::str::from_utf8(&bytes[..head_end])
        .map_err(|_| "invalid request headers".to_owned())?;
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next().unwrap_or_default().split_whitespace();
    let method = request_line
        .next()
        .ok_or("missing request method")?
        .to_owned();
    let target = request_line
        .next()
        .ok_or("missing request target")?
        .to_owned();
    if request_line.next() != Some("HTTP/1.1")
        || request_line.next().is_some()
        || !target.starts_with('/')
        || target.len() > 8192
        || target.contains('#')
    {
        return Err("invalid HTTP request line".to_owned());
    }
    let mut headers = BTreeMap::new();
    for line in lines.filter(|line| !line.is_empty()) {
        let (key, value) = line.split_once(':').ok_or("invalid request header")?;
        if key.is_empty()
            || !key
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err("invalid request header name".to_owned());
        }
        if headers
            .insert(key.to_ascii_lowercase(), value.trim().to_owned())
            .is_some()
        {
            return Err("duplicate request header".to_owned());
        }
    }
    if headers.contains_key("transfer-encoding") {
        return Err("chunked requests are not supported".to_owned());
    }
    let length = headers
        .get("content-length")
        .map(|value| {
            value
                .parse::<usize>()
                .map_err(|_| "invalid content length".to_owned())
        })
        .transpose()?
        .unwrap_or(0);
    if length > MAX_BODY {
        return Err("request body exceeds limit".to_owned());
    }
    if method == "GET" && length != 0 {
        return Err("GET request cannot have a body".to_owned());
    }
    while bytes.len() < head_end + length {
        let remaining = (head_end + length - bytes.len()).min(buffer.len());
        let count = socket
            .read(&mut buffer[..remaining])
            .await
            .map_err(|error| error.to_string())?;
        if count == 0 {
            return Err("incomplete request body".to_owned());
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    Ok(Request {
        method,
        target,
        headers,
        body: bytes[head_end..head_end + length].to_vec(),
    })
}

async fn respond(
    socket: &mut TcpStream,
    status: u16,
    mime: &str,
    body: &str,
    cookie: Option<&str>,
) -> Result<(), String> {
    let label = match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        408 => "Request Timeout",
        409 => "Conflict",
        _ => "Error",
    };
    let mut head = format!(
        "HTTP/1.1 {status} {label}\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nContent-Security-Policy: default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data: https:; connect-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'\r\n",
        body.len()
    );
    if let Some(cookie) = cookie {
        let _ = write!(head, "Set-Cookie: {cookie}\r\n");
    }
    head.push_str("\r\n");
    socket
        .write_all(head.as_bytes())
        .await
        .map_err(|error| error.to_string())?;
    socket
        .write_all(body.as_bytes())
        .await
        .map_err(|error| error.to_string())?;
    socket.shutdown().await.map_err(|error| error.to_string())?;
    // Complete the half-close before dropping the socket. Network filters on
    // Windows can otherwise reset an otherwise complete local response.
    let mut remaining = [0; 256];
    let _ = tokio::time::timeout(Duration::from_secs(1), socket.read(&mut remaining)).await;
    Ok(())
}

fn asset(path: &str) -> Option<(&'static str, &'static str)> {
    const ROOT: &str = include_str!("../../../assets/knowledge-console/index.html");
    const JS: &str = include_str!("../../../assets/knowledge-console/app.js");
    const CSS: &str = include_str!("../../../assets/knowledge-console/app.css");
    match path {
        "/" => Some(("text/html; charset=utf-8", ROOT)),
        "/app.js" => Some((ASSETS, JS)),
        "/app.css" => Some(("text/css; charset=utf-8", CSS)),
        "/vendor/marked.min.js" => Some((
            ASSETS,
            include_str!("../../../assets/knowledge-console/vendor/marked.min.js"),
        )),
        "/vendor/purify.min.js" => Some((
            ASSETS,
            include_str!("../../../assets/knowledge-console/vendor/purify.min.js"),
        )),
        "/vendor/LICENSE-marked.txt" => Some((
            "text/plain; charset=utf-8",
            include_str!("../../../assets/knowledge-console/vendor/LICENSE-marked.txt"),
        )),
        "/vendor/LICENSE-DOMPurify.txt" => Some((
            "text/plain; charset=utf-8",
            include_str!("../../../assets/knowledge-console/vendor/LICENSE-DOMPurify.txt"),
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(method: &str, target: &str) -> Request {
        Request {
            method: method.to_owned(),
            target: target.to_owned(),
            headers: BTreeMap::new(),
            body: Vec::new(),
        }
    }

    #[test]
    fn urls_preserve_unicode_and_reject_ambiguous_or_invalid_inputs() {
        assert_eq!(decode("concepts%2Fa%20b.md").unwrap(), "concepts/a b.md");
        assert_eq!(decode("%E7%9F%A5%E8%AF%86.md").unwrap(), "知识.md");
        for value in ["%", "%zz", "%ff", "%00", "%0a"] {
            assert!(decode(value).is_err());
        }
        assert!(route(&request("GET", "/api/page?scope=project&scope=global")).is_err());
        assert!(route(&request("GET", "/api/not-real")).is_err());
    }

    #[test]
    fn cookies_and_mutation_content_type_are_required() {
        let mut req = request("POST", "/api/manage");
        assert!(!authorized(&req, "ha_knowledge_1", "secret"));
        req.headers.insert(
            "cookie".to_owned(),
            "x=other; ha_knowledge_1=secret".to_owned(),
        );
        assert!(authorized(&req, "ha_knowledge_1", "secret"));
        assert!(route(&req).is_err());
        req.headers
            .insert("content-type".to_owned(), "application/json".to_owned());
        req.body = br#"{"action":"create_category","path":"notes"}"#.to_vec();
        assert_eq!(route(&req).unwrap().0, "manage");
        assert!(!authorized(&req, "ha_knowledge_2", "secret"));
    }
}
