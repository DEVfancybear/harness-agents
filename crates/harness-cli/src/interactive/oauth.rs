//! Browser sign-in, after prime-agent's `packages/ai/src/utils/oauth/openai-codex.ts`.
//!
//! `ChatGPT` Plus/Pro accounts sign in with OAuth 2.0 authorization code and PKCE:
//! the app opens the authorize page, a loopback listener on port 1455 receives the
//! redirect, and the code is exchanged for an access token (a JWT that names the
//! account) and a refresh token. When the browser runs on another machine, the
//! redirect URL can be pasted into the prompt instead. Tokens are saved in
//! `auth.json` and refreshed shortly before they expire.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use sha2::Digest as _;

use super::credentials::{self, Credential};

const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
const CALLBACK_ADDRESS: &str = "127.0.0.1:1455";
const CALLBACK_PATH: &str = "/auth/callback";
const SCOPE: &str = "openid profile email offline_access";

/// Sent as `originator`: which client asks for the sign-in.
pub const ORIGINATOR: &str = "ha";

/// How long the loopback listener waits for the browser.
const CALLBACK_TIMEOUT: Duration = Duration::from_mins(5);

/// Refresh this long before the token actually expires.
const REFRESH_MARGIN_MS: i64 = 60_000;

/// A sign-in that has opened the browser and waits for its code.
pub struct PendingLogin {
    pub provider: String,
    pub url: String,
    verifier: String,
    state: String,
    listener: Option<TcpListener>,
    cancel: Arc<AtomicBool>,
}

impl PendingLogin {
    /// Stop waiting for the browser.
    pub fn canceller(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancel)
    }

    /// The code in a pasted redirect, checked against this sign-in's state.
    pub fn code_from_paste(&self, input: &str) -> Result<String, String> {
        code_from_paste(input, &self.state)
    }

    /// Whether the loopback listener is up; when it is not, only pasting works.
    #[must_use]
    pub const fn listening(&self) -> bool {
        self.listener.is_some()
    }
}

/// Start a sign-in: make the PKCE pair and the state, bind the listener.
pub fn start(provider: &str) -> Result<PendingLogin, String> {
    if provider != "openai-codex" {
        return Err(format!("{provider} does not offer a browser sign-in"));
    }
    let verifier = base64url(&random_bytes());
    let challenge = base64url(&sha2::Sha256::digest(verifier.as_bytes()));
    let state = hex(&random_bytes()[..16]);
    let query = [
        ("response_type", "code"),
        ("client_id", CLIENT_ID),
        ("redirect_uri", REDIRECT_URI),
        ("scope", SCOPE),
        ("code_challenge", &challenge),
        ("code_challenge_method", "S256"),
        ("state", &state),
        ("id_token_add_organizations", "true"),
        ("codex_cli_simplified_flow", "true"),
        ("originator", ORIGINATOR),
    ]
    .iter()
    .map(|(name, value)| format!("{name}={}", encode(value)))
    .collect::<Vec<_>>()
    .join("&");
    // Port 1455 may be taken (another sign-in, another tool); pasting still works.
    let listener = TcpListener::bind(CALLBACK_ADDRESS)
        .ok()
        .filter(|listener| listener.set_nonblocking(true).is_ok());
    Ok(PendingLogin {
        provider: provider.to_owned(),
        url: format!("{AUTHORIZE_URL}?{query}"),
        verifier,
        state,
        listener,
        cancel: Arc::new(AtomicBool::new(false)),
    })
}

/// Wait for the browser's redirect and return the code; `None` when canceled,
/// timed out, or there is no listener.
pub fn wait_for_code(pending: &PendingLogin) -> Option<String> {
    let listener = pending.listener.as_ref()?;
    let deadline = Instant::now() + CALLBACK_TIMEOUT;
    while Instant::now() < deadline && !pending.cancel.load(Ordering::SeqCst) {
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
                let (status, message, code) = match parse_redirect(&target, &pending.state) {
                    Ok(code) => (
                        "200 OK",
                        "OpenAI authentication completed. You can close this window.",
                        Some(code),
                    ),
                    Err(reason) => ("400 Bad Request", reason, None),
                };
                let body = format!(
                    "<!doctype html><meta charset=utf-8><title>ha</title><p style=\"font-family:sans-serif\">{message}</p>"
                );
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                if code.is_some() {
                    return code;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(_) => return None,
        }
    }
    None
}

/// The code in a pasted redirect URL, a `code=...&state=...` string, a
/// `code#state` pair, or a bare code (prime-agent's `parseAuthorizationInput`).
pub fn code_from_paste(input: &str, state: &str) -> Result<String, String> {
    let value = input.trim();
    if value.is_empty() {
        return Err("paste the redirect URL or the code".to_owned());
    }
    if let Some((_, query)) = value.split_once('?') {
        return parse_query(query, state);
    }
    if value.contains("code=") {
        return parse_query(value, state);
    }
    if let Some((code, pasted_state)) = value.split_once('#') {
        if pasted_state != state {
            return Err("the pasted state does not match this sign-in".to_owned());
        }
        return Ok(code.to_owned());
    }
    Ok(value.to_owned())
}

fn parse_redirect(target: &str, state: &str) -> Result<String, &'static str> {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if path != CALLBACK_PATH {
        return Err("Callback route not found.");
    }
    parse_query(query, state).map_err(|_| "The sign-in did not return a matching code.")
}

fn parse_query(query: &str, state: &str) -> Result<String, String> {
    let mut code = None;
    let mut returned_state = None;
    for pair in query.split('&') {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        match name {
            "code" => code = Some(decode(value)),
            "state" => returned_state = Some(decode(value)),
            "error" => return Err(format!("the sign-in failed: {}", decode(value))),
            _ => {}
        }
    }
    if returned_state
        .as_deref()
        .is_some_and(|value| value != state)
    {
        return Err("the returned state does not match this sign-in".to_owned());
    }
    code.filter(|code| !code.is_empty())
        .ok_or_else(|| "the redirect carries no code".to_owned())
}

/// Exchange the code for tokens and save them.
pub fn finish(pending: &PendingLogin, code: &str, auth_path: &Path) -> Result<(), String> {
    let credential = token_request(&[
        ("grant_type", "authorization_code"),
        ("client_id", CLIENT_ID),
        ("code", code),
        ("code_verifier", &pending.verifier),
        ("redirect_uri", REDIRECT_URI),
    ])?;
    credentials::save(auth_path, &pending.provider, &credential)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

/// The secret a request sends: an API key as saved, a sign-in token refreshed
/// first when it is about to expire (and saved again).
pub fn current_secret(
    path: &Path,
    provider: &str,
    credential: Credential,
) -> Result<String, String> {
    let Credential::Oauth {
        access,
        refresh,
        expires,
        ..
    } = credential
    else {
        return Ok(credential.secret().to_owned());
    };
    if now_ms() + REFRESH_MARGIN_MS < expires {
        return Ok(access);
    }
    // The resolver runs inside the async runtime; the refresh runs on its own
    // thread with its own runtime, so it cannot block or re-enter the caller's.
    let refreshed = std::thread::spawn(move || {
        token_request(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", &refresh),
            ("client_id", CLIENT_ID),
        ])
    })
    .join()
    .map_err(|_| "the sign-in refresh stopped unexpectedly".to_owned())?
    .map_err(|error| format!("{error}; log in again with /login"))?;
    credentials::save(path, provider, &refreshed).map_err(|error| error.to_string())?;
    Ok(refreshed.secret().to_owned())
}

fn token_request(form: &[(&str, &str)]) -> Result<Credential, String> {
    let body = form
        .iter()
        .map(|(name, value)| format!("{name}={}", encode(value)))
        .collect::<Vec<_>>()
        .join("&");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("the sign-in could not start: {error}"))?;
    let text = runtime.block_on(async {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|error| error.to_string())?;
        let response = client
            .post(TOKEN_URL)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(body)
            .send()
            .await
            .map_err(|error| format!("the token request failed: {}", error.without_url()))?;
        let status = response.status();
        let text = response.text().await.map_err(|error| error.to_string())?;
        if status.is_success() {
            Ok(text)
        } else {
            Err(format!("the token request was refused ({status})"))
        }
    })?;
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|_| "the token response is not JSON".to_owned())?;
    let access = value["access_token"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let refresh = value["refresh_token"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let Some(expires_in) = value["expires_in"].as_i64() else {
        return Err("the token response has no expiry".to_owned());
    };
    if access.is_empty() || refresh.is_empty() {
        return Err("the token response is missing a token".to_owned());
    }
    let account_id = harness_providers::responses::account_id(&access);
    Ok(Credential::Oauth {
        access,
        refresh,
        expires: now_ms() + expires_in * 1000,
        account_id,
    })
}

/// Open a URL in the default browser; a failure only means the user opens it.
pub fn open_browser(url: &str) {
    #[cfg(windows)]
    let command = std::process::Command::new("rundll32")
        .args(["url.dll,FileProtocolHandler", url])
        .spawn();
    #[cfg(target_os = "macos")]
    let command = std::process::Command::new("open").arg(url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let command = std::process::Command::new("xdg-open").arg(url).spawn();
    let _ = command;
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        })
}

/// 32 unpredictable bytes without another dependency: the standard library's
/// hasher keys are drawn from the operating system's random source, and each
/// `RandomState` gets fresh keys.
fn random_bytes() -> [u8; 32] {
    use std::hash::{BuildHasher, Hasher};
    let mut digest = sha2::Sha256::new();
    for index in 0..8_u64 {
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u64(index);
        hasher.write_u128(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_nanos()),
        );
        digest.update(hasher.finish().to_le_bytes());
    }
    digest.update(std::process::id().to_le_bytes());
    digest.finalize().into()
}

fn base64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut text, byte| {
        let _ = write!(text, "{byte:02x}");
        text
    })
}

/// Percent-encode everything but the unreserved characters.
fn encode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
                char::from(byte).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}

fn decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => out.push(b' '),
            b'%' if index + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap_or("");
                if let Ok(byte) = u8::from_str_radix(hex, 16) {
                    out.push(byte);
                    index += 3;
                    continue;
                }
                out.push(b'%');
            }
            other => out.push(other),
        }
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::{code_from_paste, decode, encode, start};

    #[test]
    fn the_authorize_url_carries_pkce_and_the_state() {
        let pending = start("openai-codex").expect("started");
        assert!(
            pending
                .url
                .starts_with("https://auth.openai.com/oauth/authorize?")
        );
        assert!(pending.url.contains("code_challenge_method=S256"));
        assert!(pending.url.contains(&format!("state={}", pending.state)));
        assert!(
            pending
                .url
                .contains("redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback")
        );
        assert_ne!(
            pending.verifier,
            start("openai-codex").expect("again").verifier
        );
        assert!(start("deepseek").is_err());
    }

    #[test]
    fn a_pasted_redirect_yields_its_code_only_for_this_sign_in() {
        let url = "http://localhost:1455/auth/callback?code=abc%2B1&state=s1";
        assert_eq!(code_from_paste(url, "s1").as_deref(), Ok("abc+1"));
        assert!(code_from_paste(url, "other").is_err());
        assert_eq!(code_from_paste("code=xyz", "s1").as_deref(), Ok("xyz"));
        assert_eq!(
            code_from_paste("bare-code", "s1").as_deref(),
            Ok("bare-code")
        );
        assert!(code_from_paste("c#wrong", "s1").is_err());
    }

    #[test]
    fn percent_encoding_round_trips() {
        let value = "a b/c:d?e=f&g";
        assert_eq!(decode(&encode(value)), value);
    }
}
