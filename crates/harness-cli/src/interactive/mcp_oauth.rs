//! prime-agent's MCP OAuth login (`pa-core/src/mcp/oauth*.rs`): sign in to a
//! remote (Streamable HTTP) MCP server that asks for OAuth.
//!
//! One login discovers the server's OAuth metadata - RFC 9728 protected
//! resource metadata (the `WWW-Authenticate` `resource_metadata` pointer,
//! else its well-known location), then the authorization server's RFC 8414
//! or OpenID metadata, or the origin's own when there is no resource
//! metadata - registers a client (RFC 7591) unless one is configured, runs
//! the PKCE authorization-code flow against a loopback callback on ports
//! 53700-53709 (or a pasted redirect URL), and exchanges the code. The
//! tokens are saved in `auth.json` under `mcp:<server>`, bound to the server
//! URL, the token endpoint, the client id and the resource; a connection to
//! that server sends the access token, refreshed shortly before it expires.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use reqwest::Url;
use sha2::Digest as _;

use super::credentials::{self, Credential};

const CALLBACK_PORT_BASE: u16 = 53_700;
const CALLBACK_PORT_COUNT: u16 = 10;
const CALLBACK_PATH: &str = "/callback";
const CALLBACK_TIMEOUT: Duration = Duration::from_mins(5);
/// Refresh this long before the access token expires.
const REFRESH_MARGIN_MS: i64 = 60_000;

/// The key a server's tokens are saved under.
#[must_use]
pub fn credential_key(server: &str) -> String {
    format!("mcp:{server}")
}

fn redirect_uri(port: u16) -> String {
    format!("http://localhost:{port}{CALLBACK_PATH}")
}

/// An endpoint the flow may talk to: https, or http on a loopback host.
fn validated_url(value: &str, label: &str) -> Result<Url, String> {
    let url = Url::parse(value).map_err(|_| format!("{label} is not a valid URL"))?;
    let loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    if url.scheme() == "https" || (url.scheme() == "http" && loopback) {
        Ok(url)
    } else {
        Err(format!("{label} must use https"))
    }
}

/// prime-agent's `canonical_resource`: origin, path and query, no fragment.
fn canonical_resource(url: &Url) -> String {
    let mut resource = url.origin().ascii_serialization();
    resource.push_str(url.path());
    if let Some(query) = url.query() {
        resource.push('?');
        resource.push_str(query);
    }
    resource
}

/// The RFC 9728 well-known location for one resource.
fn resource_metadata_url(resource: &Url) -> String {
    let path = if resource.path() == "/" {
        ""
    } else {
        resource.path()
    };
    let mut url = format!(
        "{}/.well-known/oauth-protected-resource{path}",
        resource.origin().ascii_serialization()
    );
    if let Some(query) = resource.query() {
        url.push('?');
        url.push_str(query);
    }
    url
}

/// RFC 8414 and pathful OpenID metadata locations for an issuer.
fn authorization_server_metadata_urls(issuer: &Url) -> Vec<String> {
    let path = if issuer.path() == "/" {
        String::new()
    } else {
        issuer.path().trim_end_matches('/').to_owned()
    };
    let origin = issuer.origin().ascii_serialization();
    vec![
        format!("{origin}/.well-known/oauth-authorization-server{path}"),
        format!("{origin}{path}/.well-known/openid-configuration"),
    ]
}

/// The `resource_metadata` pointer of a `WWW-Authenticate` header.
fn header_resource_metadata(value: &str) -> Option<String> {
    let lower = value.to_ascii_lowercase();
    let at = lower.find("resource_metadata")?;
    let rest = value[at + "resource_metadata".len()..].trim_start();
    let rest = rest.strip_prefix('=')?.trim_start().strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(character) = chars.next() {
        match character {
            '\\' => out.extend(chars.next()),
            '"' => return Some(out),
            other => out.push(other),
        }
    }
    None
}

/// Authorization-server metadata (RFC 8414).
#[derive(Debug, PartialEq)]
struct ServerMetadata {
    authorization_endpoint: String,
    token_endpoint: String,
    registration_endpoint: Option<String>,
    scopes_supported: Option<Vec<String>>,
}

fn server_metadata(value: &serde_json::Value, issuer: &str) -> Result<ServerMetadata, String> {
    let text = |key: &str| value.get(key).and_then(serde_json::Value::as_str);
    if text("issuer").is_none() {
        return Err(format!(
            "Authorization server metadata for {issuer} is missing its issuer"
        ));
    }
    let (Some(authorization), Some(token)) =
        (text("authorization_endpoint"), text("token_endpoint"))
    else {
        return Err(format!(
            "Authorization server metadata for {issuer} is missing required endpoints"
        ));
    };
    validated_url(authorization, "Authorization endpoint")?;
    validated_url(token, "Token endpoint")?;
    let registration = text("registration_endpoint")
        .map(|endpoint| {
            validated_url(endpoint, "Registration endpoint").map(|_| endpoint.to_owned())
        })
        .transpose()?;
    Ok(ServerMetadata {
        authorization_endpoint: authorization.to_owned(),
        token_endpoint: token.to_owned(),
        registration_endpoint: registration,
        scopes_supported: value
            .get("scopes_supported")
            .and_then(serde_json::Value::as_array)
            .map(|scopes| {
                scopes
                    .iter()
                    .filter_map(|scope| scope.as_str().map(str::to_owned))
                    .collect()
            }),
    })
}

/// One HTTP exchange on a private runtime: status, `WWW-Authenticate`,
/// content type and body. Errors name the host only.
fn http(
    method: &str,
    url: &str,
    content_type: Option<&str>,
    body: Option<String>,
) -> Result<(u16, Option<String>, Option<String>, String), String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    runtime.block_on(async {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|error| error.to_string())?;
        let mut request = if method == "POST" {
            client.post(url)
        } else {
            client.get(url)
        }
        .header("Accept", "application/json");
        if let Some(content_type) = content_type {
            request = request.header("Content-Type", content_type);
        }
        if let Some(body) = body {
            request = request.body(body);
        }
        let host = Url::parse(url)
            .ok()
            .and_then(|url| url.host_str().map(str::to_owned))
            .unwrap_or_default();
        let response = request
            .send()
            .await
            .map_err(|_| format!("{method} to {host} failed"))?;
        let status = response.status().as_u16();
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned)
        };
        let authenticate = header("www-authenticate");
        let kind = header("content-type");
        let text = response.text().await.unwrap_or_default();
        Ok((status, authenticate, kind, text))
    })
}

fn json_document(url: &str) -> Result<Option<serde_json::Value>, String> {
    let (status, _, kind, body) = http("GET", url, None, None)?;
    if status == 404 {
        return Ok(None);
    }
    if status != 200 || !kind.unwrap_or_default().contains("application/json") {
        return Err(format!(
            "GET {url} did not return application/json ({status})"
        ));
    }
    serde_json::from_str(&body)
        .map(Some)
        .map_err(|_| format!("GET {url} did not return application/json"))
}

fn discover_server(issuer: &str) -> Result<ServerMetadata, String> {
    let issuer_url = validated_url(issuer, "Authorization server issuer")?;
    let candidates = authorization_server_metadata_urls(&issuer_url);
    let mut last_error = String::new();
    for candidate in &candidates {
        match json_document(candidate) {
            Ok(None) => {}
            Ok(Some(value)) => match server_metadata(&value, issuer) {
                Ok(metadata) => return Ok(metadata),
                Err(error) => last_error = error,
            },
            Err(error) => last_error = error,
        }
    }
    Err(format!(
        "Could not discover OAuth metadata for {issuer}. Tried {}. Last error: {last_error}",
        candidates.join(", ")
    ))
}

/// What discovery found: the server metadata, and the resource indicator
/// when the server publishes protected-resource metadata.
fn discover(url: &str) -> Result<(ServerMetadata, Option<String>), String> {
    let resource_url = validated_url(url, "MCP server URL")?;
    // The probe never carries a token.
    let pointer = http("GET", url, None, None)
        .ok()
        .and_then(|(_, authenticate, ..)| authenticate)
        .and_then(|value| header_resource_metadata(&value));
    let candidate = match &pointer {
        Some(pointer) => validated_url(pointer, "resource_metadata")?.to_string(),
        None => resource_metadata_url(&resource_url),
    };
    let resource = canonical_resource(&resource_url);
    match json_document(&candidate) {
        Ok(Some(value)) => {
            if value.get("resource").and_then(serde_json::Value::as_str) != Some(&resource) {
                return Err(format!(
                    "Protected-resource metadata resource does not exactly match {resource}"
                ));
            }
            let issuer = value
                .get("authorization_servers")
                .and_then(serde_json::Value::as_array)
                .and_then(|servers| servers.first())
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    "Protected-resource metadata has no authorization_servers".to_owned()
                })?;
            Ok((discover_server(issuer)?, Some(resource)))
        }
        Ok(None) if pointer.is_none() => Ok((
            discover_server(&resource_url.origin().ascii_serialization())?,
            None,
        )),
        Ok(None) => Err(format!("GET {candidate} failed: 404")),
        Err(error) => Err(error),
    }
}

/// RFC 7591 dynamic client registration.
fn register_client(endpoint: &str, server: &str) -> Result<String, String> {
    let body = serde_json::json!({
        "client_name": format!("ha ({server})"),
        "redirect_uris": (0..CALLBACK_PORT_COUNT)
            .map(|offset| redirect_uri(CALLBACK_PORT_BASE + offset))
            .collect::<Vec<_>>(),
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "none",
    });
    let (status, _, _, text) = http(
        "POST",
        endpoint,
        Some("application/json"),
        Some(body.to_string()),
    )?;
    if !(200..300).contains(&status) {
        return Err(format!("POST {endpoint} failed: {status}"));
    }
    serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|value| value["client_id"].as_str().map(str::to_owned))
        .filter(|id| !id.is_empty())
        .ok_or_else(|| format!("Dynamic client registration at {endpoint} returned no client_id"))
}

/// The token response's access token, refresh token and expiry.
fn token_grant(
    endpoint: &str,
    params: &[(&str, &str)],
) -> Result<(String, Option<String>, Option<i64>), String> {
    let body = params
        .iter()
        .map(|(name, value)| format!("{name}={}", super::oauth::encode(value)))
        .collect::<Vec<_>>()
        .join("&");
    let (status, _, _, text) = http(
        "POST",
        endpoint,
        Some("application/x-www-form-urlencoded"),
        Some(body),
    )?;
    if !(200..300).contains(&status) {
        return Err(format!("Token request to {endpoint} failed: {status}"));
    }
    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|_| format!("Token request to {endpoint} returned invalid JSON"))?;
    let access = value["access_token"]
        .as_str()
        .filter(|token| !token.is_empty())
        .ok_or_else(|| format!("Token request to {endpoint} returned no access_token"))?;
    Ok((
        access.to_owned(),
        value["refresh_token"].as_str().map(str::to_owned),
        value["expires_in"].as_i64(),
    ))
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn base64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// The code a redirect URL (or a pasted one) carries, checked against `state`.
fn code_from(target: &str, state: &str) -> Result<String, String> {
    let query = target.split_once('?').map_or(target, |(_, query)| query);
    let mut code = None;
    let mut returned_state = None;
    for pair in query.split('&') {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        let value = super::oauth::decode(value);
        match name {
            "code" => code = Some(value),
            "state" => returned_state = Some(value),
            "error" => return Err(format!("the authorization was refused: {value}")),
            _ => {}
        }
    }
    if returned_state
        .as_deref()
        .is_some_and(|returned| returned != state)
    {
        return Err("OAuth state mismatch".to_owned());
    }
    code.filter(|code| !code.is_empty())
        .ok_or_else(|| "Missing authorization code".to_owned())
}

/// The first free callback port of prime-agent's range.
fn callback_listener() -> Result<(TcpListener, u16), String> {
    for offset in 0..CALLBACK_PORT_COUNT {
        let port = CALLBACK_PORT_BASE + offset;
        if let Ok(listener) = TcpListener::bind(("127.0.0.1", port)) {
            let _ = listener.set_nonblocking(true);
            return Ok((listener, port));
        }
    }
    Err(format!(
        "Could not start the OAuth callback server: ports {CALLBACK_PORT_BASE}-{} are all in use",
        CALLBACK_PORT_BASE + CALLBACK_PORT_COUNT - 1
    ))
}

/// Wait for the browser's redirect, or a pasted URL on stdin.
fn wait_for_code(listener: &TcpListener, state: &str) -> Result<String, String> {
    let (pasted_in, pasted) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).is_ok() && !line.trim().is_empty() {
            let _ = pasted_in.send(line);
        }
    });
    let deadline = Instant::now() + CALLBACK_TIMEOUT;
    while Instant::now() < deadline {
        if let Ok(line) = pasted.try_recv() {
            return code_from(line.trim(), state);
        }
        match listener.accept() {
            Ok((mut stream, _)) => {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                let mut buffer = [0_u8; 8192];
                let read = stream.read(&mut buffer).unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..read]).into_owned();
                let target = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap_or_default()
                    .to_owned();
                if !target.starts_with(CALLBACK_PATH) {
                    continue;
                }
                let outcome = code_from(&target, state);
                let message = match &outcome {
                    Ok(_) => "MCP authentication completed. You can close this window.",
                    Err(_) => "MCP authentication failed. Return to the terminal.",
                };
                let body = format!(
                    "<!doctype html><meta charset=utf-8><title>ha</title><p style=\"font-family:sans-serif\">{message}</p>"
                );
                let _ = write!(
                    stream,
                    "HTTP/1.1 {}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    if outcome.is_ok() {
                        "200 OK"
                    } else {
                        "400 Bad Request"
                    },
                    body.len()
                );
                return outcome;
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(error) => return Err(format!("the OAuth callback failed: {error}")),
        }
    }
    Err("the sign-in timed out".to_owned())
}

/// prime-agent's `mcp_login`: sign in to `server` at `url` and save its
/// tokens. `progress` narrates the steps and shows the authorization URL.
///
/// # Errors
/// Discovery, registration, the authorization or the token exchange failed.
pub fn login(
    server: &str,
    url: &str,
    client_id: Option<&str>,
    scopes: Option<&str>,
    auth_path: &Path,
    progress: &dyn Fn(&str),
) -> Result<(), String> {
    progress(&format!("Discovering OAuth metadata for {server}..."));
    let (metadata, resource) = discover(url)?;
    let client_id = match client_id {
        Some(client_id) => client_id.to_owned(),
        None => {
            let endpoint = metadata.registration_endpoint.as_deref().ok_or_else(|| {
                format!(
                    "{server} does not support dynamic client registration and no client id was configured; pass --client-id"
                )
            })?;
            progress("Registering OAuth client...");
            register_client(endpoint, server)?
        }
    };
    let verifier = base64url(&super::oauth::random_bytes());
    let challenge = base64url(&sha2::Sha256::digest(verifier.as_bytes()));
    let state = base64url(&super::oauth::random_bytes());
    let (listener, port) = callback_listener()?;
    let redirect = redirect_uri(port);
    let scope = scopes
        .map(str::to_owned)
        .or_else(|| {
            metadata
                .scopes_supported
                .as_ref()
                .map(|scopes| scopes.join(" "))
        })
        .unwrap_or_default();
    let mut params = vec![
        ("client_id", client_id.as_str()),
        ("response_type", "code"),
        ("redirect_uri", redirect.as_str()),
        ("code_challenge", challenge.as_str()),
        ("code_challenge_method", "S256"),
        ("state", state.as_str()),
    ];
    if !scope.is_empty() {
        params.push(("scope", scope.as_str()));
    }
    if let Some(resource) = &resource {
        params.push(("resource", resource.as_str()));
    }
    let separator = if metadata.authorization_endpoint.contains('?') {
        '&'
    } else {
        '?'
    };
    let authorize = format!(
        "{}{separator}{}",
        metadata.authorization_endpoint,
        params
            .iter()
            .map(|(name, value)| format!("{name}={}", super::oauth::encode(value)))
            .collect::<Vec<_>>()
            .join("&")
    );
    progress(&format!(
        "Open this URL to sign in (opening your browser):\n{authorize}\nIf the browser is on another machine, paste the final redirect URL here."
    ));
    super::oauth::open_browser(&authorize);
    let code = wait_for_code(&listener, &state)?;
    progress("Exchanging authorization code for tokens...");
    let mut grant = vec![
        ("grant_type", "authorization_code"),
        ("code", code.as_str()),
        ("redirect_uri", redirect.as_str()),
        ("client_id", client_id.as_str()),
        ("code_verifier", verifier.as_str()),
    ];
    if let Some(resource) = &resource {
        grant.push(("resource", resource.as_str()));
    }
    let (access, refresh, expires_in) = token_grant(&metadata.token_endpoint, &grant)?;
    let credential = Credential::McpOauth {
        access,
        refresh,
        expires: expires_in.map(|seconds| now_ms() + seconds * 1000),
        endpoint: url.to_owned(),
        token_endpoint: metadata.token_endpoint,
        client_id,
        resource,
    };
    credentials::save(auth_path, &credential_key(server), &credential)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

/// The access token a connection to `server` at `url` sends, refreshed
/// first when it is about to expire; `None` when there is no sign-in for that
/// server at that URL.
///
/// # Errors
/// The refresh failed: sign in again.
pub fn bearer(auth_path: &Path, server: &str, url: &str) -> Result<Option<String>, String> {
    let key = credential_key(server);
    let Ok(Some(Credential::McpOauth {
        access,
        refresh,
        expires,
        endpoint,
        token_endpoint,
        client_id,
        resource,
    })) = credentials::load(auth_path, &key)
    else {
        return Ok(None);
    };
    // A sign-in is bound to the URL it was made for.
    if endpoint != url {
        return Ok(None);
    }
    if expires.is_none_or(|expires| now_ms() + REFRESH_MARGIN_MS < expires) {
        return Ok(Some(access));
    }
    let Some(refresh_token) = refresh.clone() else {
        return Err(format!(
            "the MCP sign-in for {server} expired; run `ha mcp login {server}`"
        ));
    };
    let refreshed = std::thread::spawn(move || {
        let mut grant = vec![
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token.as_str()),
            ("client_id", client_id.as_str()),
        ];
        if let Some(resource) = &resource {
            grant.push(("resource", resource.as_str()));
        }
        token_grant(&token_endpoint, &grant)
            .map(|token| (token, token_endpoint, client_id, resource))
    })
    .join()
    .map_err(|_| "the MCP sign-in refresh stopped unexpectedly".to_owned())?
    .map_err(|error| format!("{error}; run `ha mcp login {server}`"))?;
    let ((access, new_refresh, expires_in), token_endpoint, client_id, resource) = refreshed;
    let credential = Credential::McpOauth {
        access: access.clone(),
        refresh: new_refresh.or(refresh),
        expires: expires_in.map(|seconds| now_ms() + seconds * 1000),
        endpoint,
        token_endpoint,
        client_id,
        resource,
    };
    credentials::save(auth_path, &key, &credential).map_err(|error| error.to_string())?;
    Ok(Some(access))
}

#[cfg(test)]
mod tests {
    use super::{
        authorization_server_metadata_urls, canonical_resource, code_from,
        header_resource_metadata, resource_metadata_url, server_metadata, validated_url,
    };
    use reqwest::Url;

    #[test]
    fn discovery_locations_follow_the_rfcs() {
        let resource = Url::parse("https://mcp.example/mcp?tenant=a#x").expect("url");
        assert_eq!(
            canonical_resource(&resource),
            "https://mcp.example/mcp?tenant=a"
        );
        assert_eq!(
            resource_metadata_url(&resource),
            "https://mcp.example/.well-known/oauth-protected-resource/mcp?tenant=a"
        );
        assert_eq!(
            authorization_server_metadata_urls(
                &Url::parse("https://login.example/tenant/").expect("url")
            ),
            [
                "https://login.example/.well-known/oauth-authorization-server/tenant",
                "https://login.example/tenant/.well-known/openid-configuration",
            ]
        );
        assert_eq!(
            header_resource_metadata(
                r#"Bearer realm="mcp", resource_metadata="https://metadata.example/rm""#
            )
            .as_deref(),
            Some("https://metadata.example/rm")
        );
        assert_eq!(header_resource_metadata("Basic realm=\"x\""), None);
        assert!(validated_url("http://example.com/x", "x").is_err());
        assert!(validated_url("http://localhost:9/x", "x").is_ok());
    }

    /// An expiring sign-in is refreshed against its token endpoint and saved;
    /// a sign-in made for another URL is not used.
    #[test]
    fn an_expiring_token_is_refreshed_and_saved() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
        let port = listener.local_addr().expect("address").port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("token request");
            let mut buffer = [0_u8; 4096];
            let read = stream.read(&mut buffer).expect("request");
            let request = String::from_utf8_lossy(&buffer[..read]).into_owned();
            let body = r#"{"access_token":"fresh","expires_in":3600}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .expect("response");
            request
        });
        let dir = tempfile::tempdir().expect("temporary directory");
        let auth = dir.path().join("auth.json");
        crate::interactive::credentials::save(
            &auth,
            "mcp:linear",
            &crate::interactive::credentials::Credential::McpOauth {
                access: "stale".to_owned(),
                refresh: Some("r1".to_owned()),
                expires: Some(0),
                endpoint: "https://mcp.example/mcp".to_owned(),
                token_endpoint: format!("http://127.0.0.1:{port}/token"),
                client_id: "c1".to_owned(),
                resource: Some("https://mcp.example/mcp".to_owned()),
            },
        )
        .expect("saved");
        assert_eq!(
            super::bearer(&auth, "linear", "https://other.example/mcp"),
            Ok(None)
        );
        assert_eq!(
            super::bearer(&auth, "linear", "https://mcp.example/mcp"),
            Ok(Some("fresh".to_owned()))
        );
        let request = server.join().expect("server");
        assert!(request.contains("grant_type=refresh_token"), "{request}");
        assert!(request.contains("refresh_token=r1"), "{request}");
        assert!(request.contains("client_id=c1"), "{request}");
        // Saved: the next connection reads the fresh token without asking.
        assert_eq!(
            super::bearer(&auth, "linear", "https://mcp.example/mcp"),
            Ok(Some("fresh".to_owned()))
        );
    }

    #[test]
    fn metadata_and_redirects_are_checked() {
        let metadata = server_metadata(
            &serde_json::json!({
                "issuer": "https://login.example",
                "authorization_endpoint": "https://login.example/authorize",
                "token_endpoint": "https://login.example/token",
                "scopes_supported": ["read", "write"],
            }),
            "https://login.example",
        )
        .expect("metadata");
        assert_eq!(metadata.token_endpoint, "https://login.example/token");
        assert_eq!(
            metadata.scopes_supported,
            Some(vec!["read".to_owned(), "write".to_owned()])
        );
        assert!(server_metadata(&serde_json::json!({ "issuer": "x" }), "x").is_err());
        assert_eq!(
            code_from("/callback?code=abc&state=s1", "s1").as_deref(),
            Ok("abc")
        );
        assert_eq!(
            code_from("http://localhost:53700/callback?code=abc&state=other", "s1"),
            Err("OAuth state mismatch".to_owned())
        );
    }
}
