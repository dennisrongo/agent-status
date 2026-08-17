//! Codex (OpenAI) LIVE usage client.
//!
//! Reads the ChatGPT OAuth login the Codex CLI stores at `$CODEX_HOME/auth.json`
//! (default `~/.codex`) and calls `GET https://chatgpt.com/backend-api/wham/usage`
//! — the same quota endpoint the Codex CLI / dashboard reads. Response:
//! `rate_limit.primary_window` / `secondary_window` (5-hour + weekly, as
//! `used_percent` + unix `reset_at`), optional `credits`, optional
//! `additional_rate_limits[]` (model-specific lanes such as Codex Spark), and
//! a `plan_type`.
//!
//! Access tokens last hours; the CLI only renews them while it runs. An
//! expired-by-clock login is renewed in place (`refresh()`) using the stored
//! refresh token and the CLI's public OAuth client, then written back to the
//! same `auth.json` the CLI re-reads. The refresh token is single-use, so a
//! persistence failure is a hard error — mirroring kimi.rs / grok.rs.

use aes_gcm::aead::{rand_core::RngCore, OsRng};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{DateTime, Utc};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::process_util::SilentCommand;
use super::{KeyVal, VendorStatus};

const USAGE_ENDPOINT: &str = "https://chatgpt.com/backend-api/wham/usage";
const TOKEN_ENDPOINT: &str = "https://auth.openai.com/oauth/token";
const AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
/// The Codex CLI's public ChatGPT OAuth client id (authorization-code + PKCE;
/// no secret). Used only with the user's own stored credentials.
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
/// Same originator the official CLI sends so the consent screen matches
/// `codex login`.
const ORIGINATOR: &str = "codex_cli_rs";
const OAUTH_SCOPES: &str = "openid profile email offline_access api.connectors.read api.connectors.invoke";
const CALLBACK_PORT: u16 = 1455;
const CALLBACK_PORT_FALLBACK: u16 = 1457;
const USER_AGENT: &str = "agent-status";
/// Treat the access token as expired this many seconds before its stated
/// `exp`, so an about-to-die token isn't used for a fetch that would 401.
const EXPIRY_SKEW_SECS: i64 = 60;
/// When `auth.json` has no JWT `exp`, fall back to `last_refresh` age. Codex
/// access tokens are rotated well before this; an 8-day-old stamp is stale.
const LAST_REFRESH_MAX_SECS: i64 = 8 * 24 * 60 * 60;

static NPM_PREFIX: OnceLock<Option<PathBuf>> = OnceLock::new();

pub async fn fetch(now: DateTime<Utc>) -> VendorStatus {
    let Some(raw) = read_credentials() else {
        return VendorStatus::not_configured();
    };
    let Ok(root) = serde_json::from_str::<Value>(raw.trim()) else {
        return VendorStatus::failed("stored credentials unreadable");
    };
    let Some(creds) = token_credentials(&root) else {
        return VendorStatus::not_configured();
    };
    if creds.access_token.is_empty() {
        return VendorStatus::not_configured();
    }

    if credential_expired(&creds, now) && creds.refresh_token.is_empty() {
        return login_expired(
            "Codex login expired — sign in again from Settings (or run `codex login`).",
        );
    }

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(12))
        .build()
    {
        Ok(c) => c,
        Err(e) => return VendorStatus::failed(format!("client init: {e}")),
    };

    let resp = client
        .get(USAGE_ENDPOINT)
        .header("Authorization", format!("Bearer {}", creds.access_token))
        .header("Accept", "application/json")
        .header("User-Agent", USER_AGENT)
        .headers(account_headers(&creds.account_id))
        .send()
        .await;

    match resp {
        Ok(r) => {
            let status = r.status();
            if !status.is_success() {
                if status.as_u16() == 401 || status.as_u16() == 403 {
                    return login_expired(
                        "Codex login was rejected — sign in again from Settings (or run `codex login`)."
                            .to_string(),
                    );
                }
                return VendorStatus::failed(format!("HTTP {}", status.as_u16()));
            }
            match r.json::<Value>().await {
                Ok(v) => parse(&v, now),
                Err(e) => VendorStatus::failed(format!("invalid JSON: {e}")),
            }
        }
        Err(e) => VendorStatus::failed(format!("request error: {e}")),
    }
}

fn account_headers(account_id: &Option<String>) -> reqwest::header::HeaderMap {
    let mut headers = reqwest::header::HeaderMap::new();
    if let Some(id) = account_id.as_deref().filter(|s| !s.is_empty()) {
        if let Ok(val) = reqwest::header::HeaderValue::from_str(id) {
            headers.insert("ChatGPT-Account-Id", val);
        }
    }
    headers
}

fn login_expired(msg: impl Into<String>) -> VendorStatus {
    VendorStatus {
        configured: true,
        ok: false,
        error: Some(msg.into()),
        primary: "—".to_string(),
        secondary: "login expired".to_string(),
        detail: Vec::new(),
        auth_expired: true,
    }
}

fn codex_home() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("CODEX_HOME") {
        let p = PathBuf::from(home);
        if !p.as_os_str().is_empty() {
            return Some(p);
        }
    }
    dirs::home_dir().map(|h| h.join(".codex"))
}

fn credentials_path() -> Option<PathBuf> {
    Some(codex_home()?.join("auth.json"))
}

fn read_credentials() -> Option<String> {
    std::fs::read_to_string(credentials_path()?).ok()
}

struct CodexCreds {
    access_token: String,
    refresh_token: String,
    id_token: Option<String>,
    account_id: Option<String>,
    last_refresh: Option<DateTime<Utc>>,
}

fn token_credentials(root: &Value) -> Option<CodexCreds> {
    parse_token_credentials(root)
}

fn parse_token_credentials(root: &Value) -> Option<CodexCreds> {
    let tokens = root.get("tokens")?;
    let access = string_field(tokens, "access_token", "accessToken")?;
    if access.is_empty() {
        return None;
    }
    let refresh = string_field(tokens, "refresh_token", "refreshToken").unwrap_or_default();
    let id_token = string_field(tokens, "id_token", "idToken");
    let account_id = string_field(tokens, "account_id", "accountId")
        .or_else(|| jwt_claim(id_token.as_deref(), &access, "chatgpt_account_id"))
        .or_else(|| jwt_auth_claim(id_token.as_deref(), &access, "chatgpt_account_id"));
    Some(CodexCreds {
        access_token: access,
        refresh_token: refresh,
        id_token,
        account_id,
        last_refresh: parse_last_refresh(root.get("last_refresh")),
    })
}

fn string_field(obj: &Value, snake: &str, camel: &str) -> Option<String> {
    obj.get(snake)
        .or_else(|| obj.get(camel))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn parse_last_refresh(raw: Option<&Value>) -> Option<DateTime<Utc>> {
    let s = raw.and_then(|v| v.as_str()).filter(|s| !s.is_empty())?;
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

pub struct TokenStatus {
    pub present: bool,
    pub expired: bool,
    pub has_refresh: bool,
}

pub fn token_status(now: DateTime<Utc>) -> TokenStatus {
    read_credentials()
        .map(|raw| parse_token_status(&raw, now))
        .unwrap_or(TokenStatus {
            present: false,
            expired: false,
            has_refresh: false,
        })
}

fn parse_token_status(raw: &str, now: DateTime<Utc>) -> TokenStatus {
    let Ok(root) = serde_json::from_str::<Value>(raw.trim()) else {
        return TokenStatus {
            present: false,
            expired: false,
            has_refresh: false,
        };
    };
    let Some(creds) = parse_token_credentials(&root) else {
        return TokenStatus {
            present: false,
            expired: false,
            has_refresh: false,
        };
    };
    TokenStatus {
        present: true,
        expired: credential_expired(&creds, now),
        has_refresh: !creds.refresh_token.is_empty(),
    }
}

fn credential_expired(creds: &CodexCreds, now: DateTime<Utc>) -> bool {
    if let Some(exp) = jwt_exp(Some(creds.access_token.as_str())).or_else(|| jwt_exp(creds.id_token.as_deref()))
    {
        return now >= exp - chrono::Duration::seconds(EXPIRY_SKEW_SECS);
    }
    match creds.last_refresh {
        Some(at) => (now - at).num_seconds() > LAST_REFRESH_MAX_SECS,
        None => true,
    }
}

/// Decode a JWT payload claim without verifying the signature — we only read
/// expiry / account id from a token the user already stored locally.
fn jwt_payload(token: &str) -> Option<Value> {
    let mut parts = token.split('.');
    let _ = parts.next()?;
    let payload = parts.next()?;
    let _sig = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    let mut encoded = payload.replace('-', "+").replace('_', "/");
    match encoded.len() % 4 {
        2 => encoded.push_str("=="),
        3 => encoded.push_str("="),
        1 => return None,
        _ => {}
    }
    let bytes = base64_decode(&encoded)?;
    serde_json::from_slice(&bytes).ok()
}

fn jwt_exp(token: Option<&str>) -> Option<DateTime<Utc>> {
    let payload = jwt_payload(token?)?;
    let exp = payload.get("exp").and_then(value_as_i64)?;
    DateTime::from_timestamp(exp, 0)
}

fn jwt_claim(id_token: Option<&str>, access: &str, key: &str) -> Option<String> {
    for token in [id_token, Some(access)].into_iter().flatten() {
        if let Some(payload) = jwt_payload(token) {
            if let Some(v) = payload.get(key).and_then(|s| s.as_str()).filter(|s| !s.is_empty()) {
                return Some(v.to_string());
            }
        }
    }
    None
}

fn jwt_auth_claim(id_token: Option<&str>, access: &str, key: &str) -> Option<String> {
    for token in [id_token, Some(access)].into_iter().flatten() {
        if let Some(payload) = jwt_payload(token) {
            if let Some(v) = payload
                .pointer("/https:~1~1api.openai.com~1auth")
                .or_else(|| payload.get("https://api.openai.com/auth"))
                .and_then(|a| a.get(key))
                .and_then(|s| s.as_str())
                .filter(|s| !s.is_empty())
            {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// Minimal base64 decoder for JWT payloads (standard alphabet after url-safe
/// rewrite). Returns None on invalid input instead of panicking.
fn base64_decode(s: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    let mut buf = 0u32;
    let mut n = 0u32;
    for &c in bytes {
        if c == b'=' {
            break;
        }
        buf = (buf << 6) | u32::from(val(c)?);
        n += 1;
        if n == 4 {
            out.push((buf >> 16) as u8);
            out.push((buf >> 8) as u8);
            out.push(buf as u8);
            buf = 0;
            n = 0;
        }
    }
    match n {
        0 => {}
        2 => out.push((buf >> 4) as u8),
        3 => {
            out.push((buf >> 10) as u8);
            out.push((buf >> 2) as u8);
        }
        _ => return None,
    }
    Some(out)
}

pub async fn refresh(now: DateTime<Utc>) -> Result<(), String> {
    let raw = read_credentials().ok_or("No Codex login found to refresh.")?;
    let root: Value = serde_json::from_str(raw.trim())
        .map_err(|e| format!("stored credentials unreadable: {e}"))?;
    let creds = parse_token_credentials(&root).ok_or("No Codex login found to refresh.")?;
    if creds.refresh_token.is_empty() {
        return Err(
            "No refresh token stored — sign in again from Settings (or run `codex login`)."
                .into(),
        );
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| format!("client init: {e}"))?;

    let resp = client
        .post(TOKEN_ENDPOINT)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .body(form_encode(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", creds.refresh_token.as_str()),
            ("client_id", CLIENT_ID),
            ("scope", "openid profile email offline_access"),
        ]))
        .send()
        .await
        .map_err(|e| format!("request error: {e}"))?;

    let status = resp.status();
    if !status.is_success() {
        if status.as_u16() == 400 || status.as_u16() == 401 {
            return Err(
                "Codex refresh token expired — sign in again from Settings (or run `codex login`)."
                    .into(),
            );
        }
        return Err(format!("token endpoint returned HTTP {}", status.as_u16()));
    }

    let tok: Value = resp
        .json()
        .await
        .map_err(|e| format!("invalid token response: {e}"))?;
    let access = tok
        .get("access_token")
        .and_then(|t| t.as_str())
        .filter(|s| !s.is_empty())
        .ok_or("token response had no access_token")?
        .to_string();
    let new_refresh = tok
        .get("refresh_token")
        .and_then(|t| t.as_str())
        .filter(|s| !s.is_empty());
    let new_id = tok
        .get("id_token")
        .and_then(|t| t.as_str())
        .filter(|s| !s.is_empty());
    let serialized = build_refreshed_credentials(&raw, now, &access, new_refresh, new_id)?;
    write_credentials_file(&serialized)
}

fn build_refreshed_credentials(
    existing: &str,
    now: DateTime<Utc>,
    access: &str,
    new_refresh: Option<&str>,
    new_id: Option<&str>,
) -> Result<String, String> {
    let mut root: Value = serde_json::from_str(existing.trim())
        .map_err(|e| format!("stored credentials unreadable: {e}"))?;
    let tokens = root
        .as_object_mut()
        .ok_or("stored credentials are not an object")?
        .entry("tokens")
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    let obj = tokens
        .as_object_mut()
        .ok_or("tokens is not an object")?;
    obj.insert("access_token".into(), Value::String(access.to_string()));
    if let Some(r) = new_refresh {
        obj.insert("refresh_token".into(), Value::String(r.to_string()));
    }
    if let Some(id) = new_id {
        obj.insert("id_token".into(), Value::String(id.to_string()));
    }
    root.as_object_mut()
        .unwrap()
        .insert(
            "last_refresh".into(),
            Value::String(now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)),
        );
    serde_json::to_string(&root).map_err(|e| format!("serialize: {e}"))
}

fn write_credentials_file(json: &str) -> Result<(), String> {
    let path = credentials_path().ok_or("no home directory for Codex credentials")?;
    std::fs::write(&path, json).map_err(|e| format!("write {}: {e}", path.display()))
}

fn npm_command() -> std::process::Command {
    #[cfg(target_os = "windows")]
    {
        let mut cmd = std::process::Command::new("cmd");
        cmd.arg("/C").arg("npm").silent();
        return cmd;
    }
    #[cfg(not(target_os = "windows"))]
    {
        let mut cmd = std::process::Command::new("npm");
        cmd.silent();
        cmd
    }
}

fn npm_global_prefix() -> Option<PathBuf> {
    NPM_PREFIX
        .get_or_init(|| {
            let out = npm_command().args(["config", "get", "prefix"]).output().ok()?;
            if !out.status.success() {
                return None;
            }
            let prefix = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if prefix.is_empty() {
                return None;
            }
            Some(PathBuf::from(prefix))
        })
        .clone()
}

/// Locate the `codex` CLI. PATH first, then `$CODEX_HOME/bin`, then the npm
/// global prefix (the usual `npm i -g @openai/codex` install).
pub fn find_cli() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    const NAMES: &[&str] = &["codex.exe", "codex.cmd"];
    #[cfg(not(target_os = "windows"))]
    const NAMES: &[&str] = &["codex"];

    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            for name in NAMES {
                let candidate = dir.join(name);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }

    if let Some(home) = codex_home() {
        for name in NAMES {
            let candidate = home.join("bin").join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    if let Some(base) = npm_global_prefix() {
        for name in NAMES {
            let candidate = base.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            let bin = base.join("bin");
            if bin.join("codex").is_file() {
                return Some(bin.join("codex"));
            }
        }
    }
    None
}

pub fn cli_on_path() -> bool {
    find_cli().is_some()
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CliStatus {
    pub installed: bool,
    pub authenticated: bool,
}

pub fn cli_status() -> CliStatus {
    CliStatus {
        installed: find_cli().is_some(),
        authenticated: token_status(Utc::now()).present,
    }
}

/// Install the official Codex CLI globally via npm. Optional — live quota
/// uses in-app ChatGPT OAuth and does not need the binary. The CLI is only
/// needed for local session-log rows.
pub fn install() -> Result<String, String> {
    let npm_ok = npm_command()
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !npm_ok {
        return Err(
            "npm not found — install Node.js first: https://nodejs.org".to_string(),
        );
    }

    let out = npm_command()
        .args(["install", "-g", "--include=optional", "@openai/codex"])
        .output()
        .map_err(|e| format!("npm spawn failed: {e}"))?;

    if out.status.success() {
        if find_cli().is_some() {
            Ok("Codex CLI installed. Sign in from Settings to connect (or run `codex login`).".to_string())
        } else {
            Ok("Installed, but `codex` isn’t on PATH yet — restart the app.".to_string())
        }
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr);
        Err(format!("npm install failed: {}", stderr.trim()))
    }
}

// ---------- In-app ChatGPT OAuth (authorization-code + PKCE) ----------
//
// Same flow as `codex login`: bind localhost:1455 (or 1457), open the ChatGPT
// authorize page, catch the redirect, exchange the code, write `auth.json`.
// The CLI is not required.

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginInfo {
    pub authorize_url: String,
}

pub struct LoginStart {
    pub url: String,
    pub verifier: String,
    pub state: String,
    pub redirect_uri: String,
    pub listener: TcpListener,
    pub cancel: Arc<AtomicBool>,
}

fn random_b64url(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    OsRng.fill_bytes(&mut buf);
    URL_SAFE_NO_PAD.encode(buf)
}

fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn form_encode(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", urlencode(k), urlencode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn urldecode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = &s[i + 1..i + 3];
            if let Ok(v) = u8::from_str_radix(hex, 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(if bytes[i] == b'+' { b' ' } else { bytes[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Bind the Codex-registered loopback port and build the authorize URL.
pub fn begin_login() -> Result<LoginStart, String> {
    let listener = bind_callback_port()?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("listener addr: {e}"))?
        .port();
    let redirect_uri = format!("http://localhost:{port}/auth/callback");
    let verifier = random_b64url(64);
    let state = random_b64url(32);
    let challenge = pkce_challenge(&verifier);
    let url = format!(
        "{AUTHORIZE_URL}?response_type=code&client_id={CLIENT_ID}\
&redirect_uri={redirect}&scope={scope}&code_challenge={challenge}\
&code_challenge_method=S256&state={state}\
&id_token_add_organizations=true&codex_cli_simplified_flow=true\
&originator={ORIGINATOR}",
        redirect = urlencode(&redirect_uri),
        scope = urlencode(OAUTH_SCOPES),
    );
    Ok(LoginStart {
        url,
        verifier,
        state,
        redirect_uri,
        listener,
        cancel: Arc::new(AtomicBool::new(false)),
    })
}

fn bind_callback_port() -> Result<TcpListener, String> {
    for port in [CALLBACK_PORT, CALLBACK_PORT_FALLBACK] {
        if let Ok(l) = TcpListener::bind(("127.0.0.1", port)) {
            let _ = l.set_nonblocking(true);
            return Ok(l);
        }
    }
    Err("Could not bind the Codex login port (1455 / 1457). Close another Codex login and try again.".into())
}

#[derive(Debug, PartialEq, Eq)]
enum CallbackReq {
    Code { code: String, state: String },
    Cancel,
    Ignore,
}

fn parse_callback_request(first_line: &str) -> CallbackReq {
    let path = first_line
        .split_whitespace()
        .nth(1)
        .unwrap_or("");
    let (route, query) = path
        .split_once('?')
        .map(|(p, q)| (p, q))
        .unwrap_or((path, ""));
    if route == "/cancel" {
        return CallbackReq::Cancel;
    }
    if route != "/auth/callback" {
        return CallbackReq::Ignore;
    }
    let mut code = String::new();
    let mut state = String::new();
    for part in query.split('&') {
        if let Some((k, v)) = part.split_once('=') {
            match k {
                "code" => code = urldecode(v),
                "state" => state = urldecode(v),
                _ => {}
            }
        }
    }
    if code.is_empty() {
        CallbackReq::Ignore
    } else {
        CallbackReq::Code { code, state }
    }
}

/// Block until the browser hits the loopback callback (or cancel / timeout).
pub fn wait_for_callback(
    listener: TcpListener,
    expected_state: &str,
    cancel: &AtomicBool,
    timeout: Duration,
) -> Result<String, String> {
    let start = Instant::now();
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err("Sign-in cancelled.".into());
        }
        if start.elapsed() >= timeout {
            return Err("Sign-in timed out — try again.".into());
        }
        match listener.accept() {
            Ok((mut stream, _)) => {
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let first = req.lines().next().unwrap_or("");
                match parse_callback_request(first) {
                    CallbackReq::Code { code, state } => {
                        let body = b"<html><body><p>Signed in to Codex. You can close this tab.</p></body></html>";
                        let _ = write_http(&mut stream, 200, "text/html; charset=utf-8", body);
                        if state != expected_state {
                            return Err("That sign-in is from a different attempt — start again.".into());
                        }
                        if code.is_empty() {
                            return Err("No authorization code in the callback.".into());
                        }
                        return Ok(code);
                    }
                    CallbackReq::Cancel => {
                        let _ = write_http(&mut stream, 200, "text/plain", b"cancelled");
                        return Err("Sign-in cancelled.".into());
                    }
                    CallbackReq::Ignore => {
                        let _ = write_http(&mut stream, 404, "text/plain", b"not found");
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(150));
            }
            Err(e) => return Err(format!("login listener: {e}")),
        }
    }
}

fn write_http(stream: &mut impl Write, status: u16, ctype: &str, body: &[u8]) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        _ => "OK",
    };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)?;
    stream.flush()
}

/// Wake a waiting listener so cancel returns promptly.
pub fn cancel_login(port: u16) {
    if let Ok(mut s) = std::net::TcpStream::connect(("127.0.0.1", port)) {
        let _ = s.set_write_timeout(Some(Duration::from_secs(1)));
        let _ = s.write_all(b"GET /cancel HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
    }
}

/// Exchange the authorization code and persist tokens to `auth.json`.
pub async fn exchange_code(
    now: DateTime<Utc>,
    code: &str,
    verifier: &str,
    redirect_uri: &str,
) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| format!("client init: {e}"))?;

    let resp = client
        .post(TOKEN_ENDPOINT)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .body(form_encode(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("client_id", CLIENT_ID),
            ("code_verifier", verifier),
        ]))
        .send()
        .await
        .map_err(|e| format!("request error: {e}"))?;

    let status = resp.status();
    if !status.is_success() {
        return Err(match status.as_u16() {
            400 | 401 => "Sign-in failed — the code may have expired. Try again.".into(),
            429 => "OpenAI is rate-limiting sign-in right now — wait a moment and retry.".into(),
            other => format!("token endpoint returned HTTP {other}"),
        });
    }

    let tok: Value = resp
        .json()
        .await
        .map_err(|e| format!("invalid token response: {e}"))?;
    let access = tok
        .get("access_token")
        .and_then(|t| t.as_str())
        .filter(|s| !s.is_empty())
        .ok_or("token response had no access_token")?
        .to_string();
    let refresh = tok
        .get("refresh_token")
        .and_then(|t| t.as_str())
        .filter(|s| !s.is_empty());
    let id_token = tok
        .get("id_token")
        .and_then(|t| t.as_str())
        .filter(|s| !s.is_empty());
    persist_oauth_tokens(now, &access, refresh, id_token)
}

fn persist_oauth_tokens(
    now: DateTime<Utc>,
    access: &str,
    refresh: Option<&str>,
    id_token: Option<&str>,
) -> Result<(), String> {
    let existing = read_credentials().unwrap_or_else(|| "{}".into());
    let account_id = id_token
        .and_then(|t| jwt_auth_claim(Some(t), access, "chatgpt_account_id"))
        .or_else(|| jwt_claim(id_token, access, "chatgpt_account_id"));
    let serialized = build_login_credentials(&existing, now, access, refresh, id_token, account_id.as_deref())?;
    if let Some(dir) = credentials_path().and_then(|p| p.parent().map(|d| d.to_path_buf())) {
        std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    }
    write_credentials_file(&serialized)
}

fn build_login_credentials(
    existing: &str,
    now: DateTime<Utc>,
    access: &str,
    refresh: Option<&str>,
    id_token: Option<&str>,
    account_id: Option<&str>,
) -> Result<String, String> {
    let mut root: Value = serde_json::from_str(existing.trim()).unwrap_or_else(|_| json_object());
    if !root.is_object() {
        root = json_object();
    }
    let obj = root.as_object_mut().unwrap();
    obj.insert("auth_mode".into(), Value::String("chatgpt".into()));
    let tokens = obj
        .entry("tokens")
        .or_insert_with(json_object);
    let t = tokens
        .as_object_mut()
        .ok_or("tokens is not an object")?;
    t.insert("access_token".into(), Value::String(access.to_string()));
    if let Some(r) = refresh {
        t.insert("refresh_token".into(), Value::String(r.to_string()));
    }
    if let Some(id) = id_token {
        t.insert("id_token".into(), Value::String(id.to_string()));
    }
    if let Some(acc) = account_id.filter(|s| !s.is_empty()) {
        t.insert("account_id".into(), Value::String(acc.to_string()));
    }
    obj.insert(
        "last_refresh".into(),
        Value::String(now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)),
    );
    serde_json::to_string(&root).map_err(|e| format!("serialize: {e}"))
}

fn json_object() -> Value {
    Value::Object(serde_json::Map::new())
}

/// Sign out of Codex by blanking the shared `auth.json`. The Codex CLI
/// re-reads that file, so this signs it out too.
pub fn logout() -> Result<String, String> {
    let path = credentials_path().ok_or("no home directory for Codex credentials")?;
    let raw = std::fs::read_to_string(&path)
        .map_err(|_| "No Codex login found — already signed out.".to_string())?;
    let tombstone = build_logout_tombstone(&raw)?;
    std::fs::write(&path, tombstone).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok("Disconnected from Codex.".to_string())
}

fn build_logout_tombstone(existing: &str) -> Result<String, String> {
    let mut root: Value = serde_json::from_str(existing.trim())
        .map_err(|e| format!("stored credentials unreadable: {e}"))?;
    let tokens = root
        .get_mut("tokens")
        .and_then(|v| v.as_object_mut())
        .ok_or("No Codex login found — already signed out.")?;
    tokens.insert("access_token".into(), Value::String(String::new()));
    tokens.insert("refresh_token".into(), Value::String(String::new()));
    serde_json::to_string(&root).map_err(|e| format!("serialize: {e}"))
}

/// Pure parser for the `/wham/usage` response.
pub fn parse(v: &Value, now: DateTime<Utc>) -> VendorStatus {
    let rate = v.get("rate_limit").or_else(|| v.get("rateLimit"));
    let mut primary = window_of(rate, "primary_window", "primaryWindow")
        .or_else(|| window_of(rate, "primary", "primary"));
    let mut secondary = window_of(rate, "secondary_window", "secondaryWindow")
        .or_else(|| window_of(rate, "secondary", "secondary"));
    normalize_windows(&mut primary, &mut secondary);

    let credits = v.get("credits");
    let individual = v
        .get("individual_limit")
        .or_else(|| v.get("individualLimit"))
        .or_else(|| rate.and_then(|r| r.get("individual_limit").or_else(|| r.get("individualLimit"))))
        .or_else(|| {
            v.get("spend_control")
                .or_else(|| v.get("spendControl"))
                .and_then(|s| s.get("individual_limit").or_else(|| s.get("individualLimit")))
        });

    let extras = v
        .get("additional_rate_limits")
        .or_else(|| v.get("additionalRateLimits"))
        .and_then(|a| a.as_array())
        .cloned()
        .unwrap_or_default();

    let plan = v
        .get("plan_type")
        .or_else(|| v.get("planType"))
        .and_then(|p| p.as_str())
        .map(humanize_plan)
        .filter(|s| !s.is_empty());

    let recognized = primary.is_some()
        || secondary.is_some()
        || credits.is_some()
        || individual.is_some()
        || !extras.is_empty()
        || plan.is_some();
    if !recognized {
        return VendorStatus::failed("unrecognized usage shape");
    }

    let mut detail: Vec<KeyVal> = Vec::new();
    let mut headline_pct: Option<f64> = None;

    if let Some(w) = &primary {
        detail.push(window_meter("Session", "5-hour window", w, now));
        headline_pct = Some(w.used_percent);
    }
    if let Some(w) = &secondary {
        detail.push(window_meter("Week", "rolling 7 days", w, now));
        if headline_pct.is_none() {
            headline_pct = Some(w.used_percent);
        }
    }

    for extra in &extras {
        for meter in extra_meters(extra, now) {
            detail.push(meter);
        }
    }

    if let Some(limit) = individual {
        if let Some(row) = spend_control_row(limit, now) {
            detail.push(row);
        }
    }

    if let Some(c) = credits {
        if let Some(row) = credits_row(c) {
            detail.push(row);
        }
    }

    if let Some(p) = &plan {
        detail.push(KeyVal::text("Plan", p.clone()));
    }

    let primary_text = match headline_pct {
        Some(pct) => format!("{:.0}% used", pct),
        None => plan.clone().unwrap_or_else(|| "Codex".to_string()),
    };
    let secondary_text = primary
        .as_ref()
        .and_then(|w| reset_countdown(w.reset_at, now))
        .or_else(|| secondary.as_ref().and_then(|w| reset_countdown(w.reset_at, now)))
        .map(|r| format!("resets in {r}"))
        .unwrap_or_else(|| "live".to_string());

    VendorStatus {
        configured: true,
        ok: true,
        error: None,
        primary: primary_text,
        secondary: secondary_text,
        detail,
        auth_expired: false,
    }
}

struct RateWin {
    used_percent: f64,
    reset_at: Option<i64>,
    window_minutes: Option<i64>,
}

fn window_of(parent: Option<&Value>, snake: &str, camel: &str) -> Option<RateWin> {
    let w = parent?.get(snake).or_else(|| parent?.get(camel))?;
    if w.is_null() {
        return None;
    }
    let used = w
        .get("used_percent")
        .or_else(|| w.get("usedPercent"))
        .and_then(value_as_f64)
        .or_else(|| {
            w.get("remaining_percent")
                .or_else(|| w.get("remainingPercent"))
                .and_then(value_as_f64)
                .map(|r| (100.0 - r).clamp(0.0, 100.0))
        })?;
    let reset = w
        .get("reset_at")
        .or_else(|| w.get("resetAt"))
        .or_else(|| w.get("resets_at"))
        .or_else(|| w.get("resetsAt"))
        .and_then(value_as_i64)
        .filter(|&n| n > 0);
    let secs = w
        .get("limit_window_seconds")
        .or_else(|| w.get("limitWindowSeconds"))
        .and_then(value_as_i64)
        .filter(|&n| n > 0);
    let mins = w
        .get("window_duration_mins")
        .or_else(|| w.get("windowDurationMins"))
        .and_then(value_as_i64)
        .or_else(|| secs.map(|s| s / 60));
    Some(RateWin {
        used_percent: used,
        reset_at: reset,
        window_minutes: mins,
    })
}

/// Keep the 5-hour lane in `primary` and the weekly lane in `secondary`,
/// even if the API swaps them.
fn normalize_windows(primary: &mut Option<RateWin>, secondary: &mut Option<RateWin>) {
    let role = |w: &RateWin| match w.window_minutes {
        Some(300) => 0,   // session
        Some(10080) => 1, // weekly
        _ => 2,
    };
    match (primary.as_ref().map(role), secondary.as_ref().map(role)) {
        (Some(1), Some(0)) | (Some(1), Some(2)) | (Some(1), None) => {
            std::mem::swap(primary, secondary);
        }
        (None, Some(0)) | (None, Some(2)) => {
            std::mem::swap(primary, secondary);
        }
        _ => {}
    }
}

fn window_meter(label: &str, fallback: &str, w: &RateWin, now: DateTime<Utc>) -> KeyVal {
    let value = reset_countdown(w.reset_at, now)
        .map(|r| format!("resets in {r}"))
        .unwrap_or_else(|| fallback.to_string());
    KeyVal::meter(label, value, w.used_percent)
}

fn extra_meters(entry: &Value, now: DateTime<Utc>) -> Vec<KeyVal> {
    let name = entry
        .get("limit_name")
        .or_else(|| entry.get("limitName"))
        .or_else(|| entry.get("metered_feature"))
        .or_else(|| entry.get("meteredFeature"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let spark = name.to_lowercase().contains("spark");
    let rate = entry.get("rate_limit").or_else(|| entry.get("rateLimit"));
    let p = window_of(rate, "primary_window", "primaryWindow")
        .or_else(|| window_of(rate, "primary", "primary"));
    let s = window_of(rate, "secondary_window", "secondaryWindow")
        .or_else(|| window_of(rate, "secondary", "secondary"));
    let mut out = Vec::new();
    if spark {
        if let Some(w) = p {
            out.push(window_meter("Codex Spark 5-hour", "5-hour window", &w, now));
        }
        if let Some(w) = s {
            out.push(window_meter("Codex Spark Weekly", "rolling 7 days", &w, now));
        }
    } else if let Some(w) = p.or(s) {
        let label = if name.is_empty() { "Codex extra limit" } else { name };
        out.push(window_meter(label, "extra limit", &w, now));
    }
    out
}

fn spend_control_row(limit: &Value, now: DateTime<Utc>) -> Option<KeyVal> {
    let cap = limit.get("limit").and_then(value_as_f64).filter(|n| *n > 0.0)?;
    let used = limit.get("used").and_then(value_as_f64).unwrap_or(0.0);
    let remaining = limit
        .get("remaining_percent")
        .or_else(|| limit.get("remainingPercent"))
        .and_then(value_as_f64);
    let pct = remaining
        .map(|r| (100.0 - r).clamp(0.0, 100.0))
        .unwrap_or_else(|| ((used / cap) * 100.0).clamp(0.0, 100.0));
    let reset = limit
        .get("resets_at")
        .or_else(|| limit.get("resetsAt"))
        .or_else(|| limit.get("reset_at"))
        .or_else(|| limit.get("resetAt"))
        .and_then(value_as_i64);
    let value = reset_countdown(reset, now)
        .map(|r| format!("resets in {r}"))
        .unwrap_or_else(|| format!("{} / {}", fmt_credits(used), fmt_credits(cap)));
    Some(KeyVal::meter("Monthly credits", value, pct))
}

fn credits_row(c: &Value) -> Option<KeyVal> {
    if c.get("unlimited").and_then(|b| b.as_bool()).unwrap_or(false) {
        return Some(KeyVal::text("Credits", "unlimited"));
    }
    let balance = c.get("balance").and_then(value_as_f64);
    let has = c.get("has_credits").or_else(|| c.get("hasCredits")).and_then(|b| b.as_bool());
    match (balance, has) {
        (Some(n), _) => Some(KeyVal::text("Credits", fmt_credits(n))),
        (None, Some(true)) => Some(KeyVal::text("Credits", "available")),
        (None, Some(false)) => Some(KeyVal::text("Credits", "none")),
        _ => None,
    }
}

fn humanize_plan(raw: &str) -> String {
    match raw {
        "guest" => "Guest".into(),
        "free" => "Free".into(),
        "go" => "Go".into(),
        "plus" => "Plus".into(),
        "pro" => "Pro".into(),
        "free_workspace" => "Free workspace".into(),
        "team" => "Team".into(),
        "business" => "Business".into(),
        "education" => "Education".into(),
        "enterprise" => "Enterprise".into(),
        "edu" => "Edu".into(),
        "k12" => "K-12".into(),
        "quorum" => "Quorum".into(),
        other => other
            .split('_')
            .map(|w| {
                let mut c = w.chars();
                match c.next() {
                    Some(f) => format!("{}{}", f.to_uppercase(), c.as_str().to_lowercase()),
                    None => String::new(),
                }
            })
            .collect::<Vec<_>>()
            .join(" "),
    }
}

fn fmt_credits(n: f64) -> String {
    if n.abs() >= 1_000.0 {
        format!("{:.1}k", n / 1_000.0)
    } else if n.fract().abs() < f64::EPSILON {
        format!("{}", n as i64)
    } else {
        format!("{n:.2}")
    }
}

fn reset_countdown(reset_at: Option<i64>, now: DateTime<Utc>) -> Option<String> {
    let ts = reset_at.filter(|&n| n > 0)?;
    let reset = DateTime::from_timestamp(ts, 0)?;
    let secs = reset.signed_duration_since(now).num_seconds();
    if secs <= 0 {
        return None;
    }
    let d = secs / 86_400;
    let h = (secs % 86_400) / 3_600;
    let m = (secs % 3_600) / 60;
    Some(if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else {
        format!("{m}m")
    })
}

fn value_as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.parse::<f64>().ok(),
        _ => None,
    }
}

fn value_as_i64(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Value::String(s) => s.parse::<i64>().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    /// Unsigned JWT with the given payload JSON. Signature is dummy — we never verify.
    fn jwt(payload: &Value) -> String {
        fn b64url(bytes: &[u8]) -> String {
            const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
            let mut out = String::new();
            let mut i = 0;
            while i < bytes.len() {
                let b0 = bytes[i];
                let b1 = if i + 1 < bytes.len() { bytes[i + 1] } else { 0 };
                let b2 = if i + 2 < bytes.len() { bytes[i + 2] } else { 0 };
                out.push(T[(b0 >> 2) as usize] as char);
                out.push(T[(((b0 & 3) << 4) | (b1 >> 4)) as usize] as char);
                if i + 1 < bytes.len() {
                    out.push(T[(((b1 & 15) << 2) | (b2 >> 6)) as usize] as char);
                }
                if i + 2 < bytes.len() {
                    out.push(T[(b2 & 63) as usize] as char);
                }
                i += 3;
            }
            out.replace('+', "-").replace('/', "_").trim_end_matches('=').to_string()
        }
        let header = b64url(br#"{"alg":"none"}"#);
        let body = b64url(payload.to_string().as_bytes());
        format!("{header}.{body}.sig")
    }

    fn sample_auth(access: &str, refresh: &str, last_refresh: &str) -> String {
        json!({
            "tokens": {
                "access_token": access,
                "refresh_token": refresh,
                "id_token": "id",
                "account_id": "acct-1"
            },
            "last_refresh": last_refresh
        })
        .to_string()
    }

    #[test]
    fn token_status_reads_auth_tokens() {
        let now = at("2026-08-16T20:00:00Z");
        let raw = sample_auth("tok", "ref", "2026-08-16T12:00:00Z");
        let ts = parse_token_status(&raw, now);
        assert!(ts.present);
        assert!(!ts.expired);
        assert!(ts.has_refresh);
    }

    #[test]
    fn token_status_expired_by_last_refresh_age() {
        let now = at("2026-08-16T20:00:00Z");
        let raw = sample_auth("tok", "ref", "2026-08-01T12:00:00Z");
        let ts = parse_token_status(&raw, now);
        assert!(ts.present);
        assert!(ts.expired);
        assert!(ts.has_refresh);
    }

    #[test]
    fn token_status_expired_by_jwt_exp_with_skew() {
        let now = at("2026-08-16T20:00:00Z");
        let exp = now.timestamp() + 30; // within 60s skew
        let token = jwt(&json!({"exp": exp}));
        let raw = sample_auth(&token, "ref", "2026-08-16T19:00:00Z");
        let ts = parse_token_status(&raw, now);
        assert!(ts.present);
        assert!(ts.expired, "within {EXPIRY_SKEW_SECS}s of JWT exp counts as expired");
    }

    #[test]
    fn token_status_missing_tokens_is_absent() {
        let now = at("2026-08-16T20:00:00Z");
        let ts = parse_token_status(r#"{"OPENAI_API_KEY":"sk-x"}"#, now);
        assert!(!ts.present);
        assert!(!ts.has_refresh);
    }

    #[test]
    fn token_status_empty_access_is_absent() {
        let now = at("2026-08-16T20:00:00Z");
        let raw = sample_auth("", "ref", "2026-08-16T12:00:00Z");
        let ts = parse_token_status(&raw, now);
        assert!(!ts.present);
    }

    #[test]
    fn parse_plus_plan_windows() {
        let now = at("2026-08-16T20:00:00Z");
        let session_reset = now.timestamp() + 3 * 3600 + 15 * 60;
        let week_reset = now.timestamp() + 2 * 86_400 + 4 * 3600;
        let status = parse(
            &json!({
                "plan_type": "plus",
                "rate_limit": {
                    "primary_window": {
                        "used_percent": 42,
                        "reset_at": session_reset,
                        "limit_window_seconds": 18000
                    },
                    "secondary_window": {
                        "used_percent": 18,
                        "reset_at": week_reset,
                        "limit_window_seconds": 604800
                    }
                },
                "credits": { "has_credits": true, "unlimited": false, "balance": 12.5 }
            }),
            now,
        );
        assert!(status.ok);
        assert!(status.configured);
        assert_eq!(status.primary, "42% used");
        assert_eq!(status.secondary, "resets in 3h 15m");
        assert_eq!(status.detail[0].label, "Session");
        assert_eq!(status.detail[0].pct, Some(42.0));
        assert_eq!(status.detail[1].label, "Week");
        assert_eq!(status.detail[1].pct, Some(18.0));
        assert!(status.detail.iter().any(|d| d.label == "Credits" && d.value == "12.50"));
        assert!(status.detail.iter().any(|d| d.label == "Plan" && d.value == "Plus"));
    }

    #[test]
    fn parse_compact_primary_secondary_keys() {
        let now = at("2026-08-16T20:00:00Z");
        let status = parse(
            &json!({
                "rate_limit": {
                    "primary": { "used_percent": 22, "reset_at": now.timestamp() + 1800 },
                    "secondary": { "usedPercent": 8, "resetsAt": now.timestamp() + 86400 }
                }
            }),
            now,
        );
        assert!(status.ok);
        assert_eq!(status.detail[0].label, "Session");
        assert_eq!(status.detail[0].pct, Some(22.0));
        assert_eq!(status.detail[1].label, "Week");
        assert_eq!(status.detail[1].pct, Some(8.0));
    }

    #[test]
    fn parse_swaps_weekly_primary_into_session_lane() {
        let now = at("2026-08-16T20:00:00Z");
        let status = parse(
            &json!({
                "rate_limit": {
                    "primary_window": {
                        "used_percent": 10,
                        "reset_at": now.timestamp() + 86400,
                        "limit_window_seconds": 604800
                    },
                    "secondary_window": {
                        "used_percent": 55,
                        "reset_at": now.timestamp() + 1800,
                        "limit_window_seconds": 18000
                    }
                }
            }),
            now,
        );
        assert!(status.ok);
        assert_eq!(status.detail[0].label, "Session");
        assert_eq!(status.detail[0].pct, Some(55.0));
        assert_eq!(status.detail[1].label, "Week");
        assert_eq!(status.detail[1].pct, Some(10.0));
    }

    #[test]
    fn parse_spark_additional_limits() {
        let now = at("2026-08-16T20:00:00Z");
        let status = parse(
            &json!({
                "plan_type": "pro",
                "rate_limit": {
                    "primary_window": {
                        "used_percent": 5,
                        "reset_at": now.timestamp() + 3600,
                        "limit_window_seconds": 18000
                    }
                },
                "additional_rate_limits": [{
                    "limit_name": "GPT-5.3-Codex-Spark",
                    "metered_feature": "spark",
                    "rate_limit": {
                        "primary_window": {
                            "used_percent": 80,
                            "reset_at": now.timestamp() + 1200,
                            "limit_window_seconds": 18000
                        },
                        "secondary_window": {
                            "used_percent": 30,
                            "reset_at": now.timestamp() + 86400,
                            "limit_window_seconds": 604800
                        }
                    }
                }]
            }),
            now,
        );
        assert!(status.ok);
        assert!(status.detail.iter().any(|d| d.label == "Codex Spark 5-hour" && d.pct == Some(80.0)));
        assert!(status.detail.iter().any(|d| d.label == "Codex Spark Weekly" && d.pct == Some(30.0)));
    }

    #[test]
    fn parse_unrecognized_shape_fails() {
        let now = at("2026-08-16T20:00:00Z");
        let status = parse(&json!({"foo": 1}), now);
        assert!(!status.ok);
        assert_eq!(status.error.as_deref(), Some("unrecognized usage shape"));
    }

    #[test]
    fn parse_monthly_credit_limit() {
        let now = at("2026-08-16T20:00:00Z");
        let status = parse(
            &json!({
                "plan_type": "team",
                "individual_limit": {
                    "limit": 100.0,
                    "used": 25.0,
                    "remaining_percent": 75.0,
                    "resets_at": now.timestamp() + 10 * 86_400
                }
            }),
            now,
        );
        assert!(status.ok);
        let row = status.detail.iter().find(|d| d.label == "Monthly credits").unwrap();
        assert_eq!(row.pct, Some(25.0));
        assert_eq!(status.detail.iter().find(|d| d.label == "Plan").unwrap().value, "Team");
    }

    #[test]
    fn logout_tombstone_blanks_tokens() {
        let raw = sample_auth("tok", "ref", "2026-08-16T12:00:00Z");
        let out = build_logout_tombstone(&raw).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["tokens"]["access_token"], "");
        assert_eq!(v["tokens"]["refresh_token"], "");
        assert_eq!(v["tokens"]["account_id"], "acct-1");
    }

    #[test]
    fn logout_tombstone_rejects_garbage() {
        assert!(build_logout_tombstone("not-json").is_err());
        assert!(build_logout_tombstone("{}").is_err());
    }

    #[test]
    fn refresh_preserves_account_and_writes_last_refresh() {
        let now = at("2026-08-16T21:00:00Z");
        let raw = sample_auth("old", "ref", "2026-08-16T12:00:00Z");
        let out = build_refreshed_credentials(&raw, now, "new-access", Some("new-ref"), Some("new-id")).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["tokens"]["access_token"], "new-access");
        assert_eq!(v["tokens"]["refresh_token"], "new-ref");
        assert_eq!(v["tokens"]["id_token"], "new-id");
        assert_eq!(v["tokens"]["account_id"], "acct-1");
        assert_eq!(v["last_refresh"], "2026-08-16T21:00:00.000Z");
    }

    #[test]
    fn jwt_payload_reads_exp() {
        let token = jwt(&json!({"exp": 1_786_903_800, "email": "a@b.com"}));
        let exp = jwt_exp(Some(&token)).unwrap();
        assert_eq!(exp.timestamp(), 1_786_903_800);
    }

    #[test]
    fn jwt_payload_rejects_malformed() {
        assert!(jwt_payload("not.a").is_none());
        assert!(jwt_payload("a.b.c.d").is_none());
    }

    #[test]
    fn parse_callback_extracts_code_and_state() {
        let got = parse_callback_request(
            "GET /auth/callback?code=abc%2Fdef&state=xyz HTTP/1.1",
        );
        assert_eq!(
            got,
            CallbackReq::Code {
                code: "abc/def".into(),
                state: "xyz".into()
            }
        );
    }

    #[test]
    fn parse_callback_cancel_and_ignore() {
        assert_eq!(
            parse_callback_request("GET /cancel HTTP/1.1"),
            CallbackReq::Cancel
        );
        assert_eq!(
            parse_callback_request("GET /favicon.ico HTTP/1.1"),
            CallbackReq::Ignore
        );
    }

    #[test]
    fn build_login_credentials_creates_auth_json() {
        let now = at("2026-08-16T21:00:00Z");
        let out = build_login_credentials("{}", now, "acc", Some("ref"), Some("id"), Some("acct"))
            .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["auth_mode"], "chatgpt");
        assert_eq!(v["tokens"]["access_token"], "acc");
        assert_eq!(v["tokens"]["refresh_token"], "ref");
        assert_eq!(v["tokens"]["id_token"], "id");
        assert_eq!(v["tokens"]["account_id"], "acct");
        assert_eq!(v["last_refresh"], "2026-08-16T21:00:00.000Z");
    }

    #[test]
    fn authorize_url_has_required_oauth_params() {
        let start = begin_login().expect("bind loopback");
        assert!(start.url.starts_with("https://auth.openai.com/oauth/authorize?"));
        assert!(start.url.contains(&format!("client_id={CLIENT_ID}")));
        assert!(start.url.contains("response_type=code"));
        assert!(start.url.contains("code_challenge_method=S256"));
        assert!(start.url.contains("redirect_uri="));
        assert!(start.redirect_uri.contains("/auth/callback"));
        assert!(!start.verifier.is_empty());
        assert!(!start.state.is_empty());
    }
}
