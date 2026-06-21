//! Claude (Anthropic) LIVE subscription usage — the same data Claude Code's
//! status bar / `/usage` shows. Reads the OAuth token Claude Code stored
//! (macOS keychain `Claude Code-credentials`, else `~/.claude/.credentials.json`)
//! and calls `GET https://api.anthropic.com/api/oauth/usage`.
//!
//! This is an UNDOCUMENTED endpoint used with the user's own subscription token,
//! at the user's request. If the token is missing/expired we fall back silently.

use aes_gcm::aead::{rand_core::RngCore, OsRng};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{DateTime, Duration, Utc};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::time::Duration as StdDuration;

use crate::scanner::Bucket;

const ENDPOINT: &str = "https://api.anthropic.com/api/oauth/usage";
const OAUTH_BETA: &str = "oauth-2025-04-20";
/// Keychain generic-password service name Claude Code stores its login under.
/// Only referenced by the macOS keychain helpers, so it's dead on other targets.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const SERVICE: &str = "Claude Code-credentials";
/// OAuth token endpoint + public client id Claude Code uses to exchange an
/// authorization code (initial login) or a refresh token for a fresh access
/// token. Reverse-engineered from Claude Code's own flow; used here only with
/// the user's own credentials, at their request. Anthropic migrated the OAuth
/// host from `console.anthropic.com` to `platform.claude.com`; the new host is
/// authoritative for both grants.
const TOKEN_ENDPOINT: &str = "https://platform.claude.com/v1/oauth/token";
const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
/// Authorization endpoint for the Pro/Max subscription login — the same page
/// `claude /login` opens. (Console/API logins authorize at platform.claude.com,
/// but this app reads a *subscription* login, so claude.ai is the right host.)
const AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
/// Manual copy-paste callback: the authorize page renders a `CODE#STATE` string
/// the user pastes back, so no loopback HTTP server is needed. The redirect_uri
/// VALUE must match between the /authorize request and the token exchange (RFC
/// 6749 §4.1.3). It is percent-encoded in the authorize URL's query string (as
/// all query params must be) and sent raw in the exchange's JSON body; the server
/// URL-decodes the query param, so both resolve to this exact string — the same
/// pattern the reference Claude Code reimplementations use. The client id is
/// registered for this exact callback even when authorizing via claude.ai.
const REDIRECT_URI: &str = "https://platform.claude.com/oauth/code/callback";
/// Scopes Claude Code requests today. Matching them makes the consent screen and
/// the granted token look identical to a normal `claude /login`.
const SCOPES: &str = "org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";
/// Treat the access token as expired this many ms before its stated `expiresAt`,
/// so a refresh fires slightly early rather than mid-request.
const EXPIRY_SKEW_MS: i64 = 60_000;

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeLive {
    /// An OAuth token was found.
    pub configured: bool,
    /// Fetch + parse succeeded.
    pub ok: bool,
    /// A token exists but it was rejected (HTTP 401) — the Claude Code login
    /// expired and the user must sign in again. Distinct from a transient
    /// network failure so the UI can give a clear re-auth instruction.
    pub expired: bool,
    pub error: Option<String>,
    pub buckets: Vec<Bucket>,
}

impl ClaudeLive {
    fn off(configured: bool, error: Option<String>) -> Self {
        Self { configured, ok: false, expired: false, error, buckets: Vec::new() }
    }
}

/// Whether a usable Claude Code install is present: a stored login token, or
/// the `claude` CLI somewhere on PATH. Cheap, no process spawn.
pub fn detected() -> bool {
    read_token().is_some() || cli_on_path()
}

pub fn cli_on_path() -> bool {
    let Some(paths) = std::env::var_os("PATH") else { return false };
    std::env::split_paths(&paths).any(|dir| {
        let exe = dir.join("claude");
        exe.is_file()
            || exe.with_extension("exe").is_file()
            || exe.with_extension("cmd").is_file()
    })
}

/// Read the Claude Code OAuth access token (platform-specific).
pub fn read_token() -> Option<String> {
    parse_token_json(&read_raw_credentials()?)
}

/// The raw credentials JSON Claude Code stored — macOS keychain first, then the
/// `~/.claude/.credentials.json` file. Shared by token reads and the refresh.
fn read_raw_credentials() -> Option<String> {
    #[cfg(target_os = "macos")]
    if let Some(raw) = keychain_read() {
        return Some(raw);
    }
    read_credentials_file()
}

#[cfg(target_os = "macos")]
fn keychain_read() -> Option<String> {
    let out = std::process::Command::new("security")
        .args(["find-generic-password", "-s", SERVICE, "-w"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn read_credentials_file() -> Option<String> {
    let home = dirs::home_dir()?;
    std::fs::read_to_string(home.join(".claude").join(".credentials.json")).ok()
}

fn parse_token_json(raw: &str) -> Option<String> {
    let v: Value = serde_json::from_str(raw.trim()).ok()?;
    v.get("claudeAiOauth")
        .and_then(|o| o.get("accessToken"))
        .and_then(|t| t.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

/// Refresh an expired Claude Code access token using the stored refresh token,
/// then write the rotated credentials back to the same store Claude Code reads.
///
/// The refresh token is SINGLE-USE: the server invalidates the old one and
/// returns a new one. If we obtain new tokens but fail to persist them, the
/// user is locked out of Claude Code — so persistence failures are hard errors
/// and we fall back to the credentials file rather than dropping the new token.
pub async fn refresh(now: DateTime<Utc>) -> Result<(), String> {
    let raw = read_raw_credentials().ok_or("No Claude Code login found to refresh.")?;
    let root: Value = serde_json::from_str(raw.trim())
        .map_err(|e| format!("stored credentials unreadable: {e}"))?;
    let refresh_token = root
        .get("claudeAiOauth")
        .and_then(|v| v.get("refreshToken"))
        .and_then(|t| t.as_str())
        .filter(|s| !s.is_empty())
        .ok_or("No refresh token stored — sign in to Claude again.")?
        .to_string();

    let client = reqwest::Client::builder()
        .timeout(StdDuration::from_secs(15))
        .build()
        .map_err(|e| format!("client init: {e}"))?;

    let resp = client
        .post(TOKEN_ENDPOINT)
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({
            "grant_type": "refresh_token",
            "refresh_token": refresh_token,
            "client_id": CLIENT_ID,
        }))
        .send()
        .await
        .map_err(|e| format!("request error: {e}"))?;

    let status = resp.status();
    if !status.is_success() {
        // 400 invalid_grant / 401 → the refresh token itself is dead (already
        // rotated by another Claude Code session, revoked, or fully expired).
        // Only a real login can fix it.
        if status.as_u16() == 400 || status.as_u16() == 401 {
            return Err("Refresh token expired — sign in to Claude again.".into());
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
    // Replace the refresh token (single-use rotation). If the server omitted a
    // new one, keep the prior value rather than blanking it.
    let new_refresh = tok
        .get("refresh_token")
        .and_then(|t| t.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let expires_in = tok.get("expires_in").and_then(|v| v.as_i64()).unwrap_or(28_800);
    let scope = tok.get("scope").and_then(|s| s.as_str());
    persist_oauth_tokens(now, &access, new_refresh.as_deref(), expires_in, scope)
}

// ---------- In-app OAuth login (authorization-code + PKCE) ----------
//
// Mirrors `claude /login`'s manual copy-paste flow: open the authorize page,
// the user approves and pastes back a `CODE#STATE` string, we exchange it for
// tokens and write them into the same credential store Claude Code reads. The
// PKCE verifier + state are held in AppState between start and finish so they
// never round-trip through the UI.

/// A started login: the URL to open and the PKCE secrets to carry until the
/// pasted code is exchanged.
pub struct AuthStart {
    pub url: String,
    pub verifier: String,
    pub state: String,
}

fn random_b64url(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    OsRng.fill_bytes(&mut buf);
    URL_SAFE_NO_PAD.encode(buf)
}

fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    OsRng.fill_bytes(&mut buf);
    let mut s = String::with_capacity(bytes * 2);
    for b in buf {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// PKCE S256 challenge: base64url(SHA256(verifier)), no padding.
fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// Percent-encode a query-parameter value (RFC 3986 unreserved set kept as-is).
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

/// Build the authorize URL with a fresh PKCE verifier + state for an in-app
/// Claude subscription login.
pub fn build_authorize() -> AuthStart {
    let verifier = random_b64url(64);
    let state = random_hex(32);
    let challenge = pkce_challenge(&verifier);
    // challenge (base64url) and state (hex) are already URL-safe; only the
    // redirect_uri and scope need percent-encoding.
    let url = format!(
        "{AUTHORIZE_URL}?code=true&response_type=code&client_id={CLIENT_ID}\
&redirect_uri={redirect}&scope={scope}&code_challenge={challenge}\
&code_challenge_method=S256&state={state}",
        redirect = urlencode(REDIRECT_URI),
        scope = urlencode(SCOPES),
    );
    AuthStart { url, verifier, state }
}

/// Split a pasted login result into (code, optional state). The authorize page
/// hands the user `CODE#STATE`; we also accept a whole pasted callback URL or
/// `code=…&state=…` query string for the user who copies more than the code.
pub fn parse_pasted_code(input: &str) -> (String, Option<String>) {
    let t = input.trim();
    if let Some(idx) = t.find("code=") {
        let code: String = t[idx + 5..]
            .chars()
            .take_while(|c| *c != '&' && *c != '#')
            .collect();
        let state = t.find("state=").map(|i| {
            t[i + 6..]
                .chars()
                .take_while(|c| *c != '&' && *c != '#')
                .collect::<String>()
        });
        if !code.is_empty() {
            return (code, state.filter(|s| !s.is_empty()));
        }
    }
    if let Some((code, state)) = t.split_once('#') {
        return (code.trim().to_string(), Some(state.trim().to_string()));
    }
    (t.to_string(), None)
}

/// Exchange the pasted authorization code for tokens and persist them in Claude
/// Code's credential store. `expected_state` is the state we generated and sent
/// to /authorize; if the pasted value carried a state we verify it matches
/// (CSRF guard) before exchanging.
pub async fn exchange_code(
    now: DateTime<Utc>,
    pasted: &str,
    verifier: &str,
    expected_state: &str,
) -> Result<(), String> {
    let (code, pasted_state) = parse_pasted_code(pasted);
    if code.is_empty() {
        return Err("No authorization code found in what you pasted.".into());
    }
    // The authorize page hands back a single `CODE#STATE` blob, so require the
    // state half and verify it (CSRF guard) — a code lured from a different
    // sign-in must not be accepted. PKCE already binds the code to this client;
    // validating state is the standard defense-in-depth and costs nothing.
    let pasted_state = pasted_state
        .ok_or("Incomplete code — copy the whole value (it has a # and a second half).")?;
    if pasted_state != expected_state {
        return Err("That code is from a different sign-in — start again.".into());
    }

    let client = reqwest::Client::builder()
        .timeout(StdDuration::from_secs(30))
        .build()
        .map_err(|e| format!("client init: {e}"))?;

    let resp = client
        .post(TOKEN_ENDPOINT)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .json(&serde_json::json!({
            "grant_type": "authorization_code",
            "code": code,
            "state": expected_state,
            "client_id": CLIENT_ID,
            "redirect_uri": REDIRECT_URI,
            "code_verifier": verifier,
        }))
        .send()
        .await
        .map_err(|e| format!("request error: {e}"))?;

    let status = resp.status();
    if !status.is_success() {
        return Err(match status.as_u16() {
            400 | 401 => "Sign-in failed — the code may have expired. Try again.".into(),
            429 => "Anthropic is rate-limiting sign-in right now — wait a moment and retry.".into(),
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
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let expires_in = tok.get("expires_in").and_then(|v| v.as_i64()).unwrap_or(28_800);
    let scope = tok.get("scope").and_then(|s| s.as_str());
    persist_oauth_tokens(now, &access, refresh.as_deref(), expires_in, scope)
}

/// Merge fresh OAuth tokens into the `claudeAiOauth` object Claude Code reads,
/// preserving every field it wrote (subscriptionType, organizationUuid, email,
/// …), and persist. Creates the object from scratch when no prior login exists
/// (a first in-app sign-in). The refresh token is single-use, so persistence
/// failures are hard errors with a file fallback rather than dropping the only
/// valid token (see `write_credentials`).
fn persist_oauth_tokens(
    now: DateTime<Utc>,
    access: &str,
    new_refresh: Option<&str>,
    expires_in: i64,
    scope: Option<&str>,
) -> Result<(), String> {
    let existing = read_raw_credentials();
    let serialized =
        build_credentials_json(existing.as_deref(), now, access, new_refresh, expires_in, scope)?;
    write_credentials(&serialized)
}

/// Pure builder for the `claudeAiOauth` credentials JSON Claude Code reads.
/// Merges into the prior credentials (preserving every field Claude Code wrote —
/// subscriptionType, organizationUuid, email, …) or creates the object from
/// scratch on a first sign-in. Split from the keychain/file I/O so the merge and
/// the expiry clamp are unit-testable without touching the real stores.
fn build_credentials_json(
    existing: Option<&str>,
    now: DateTime<Utc>,
    access: &str,
    new_refresh: Option<&str>,
    expires_in: i64,
    scope: Option<&str>,
) -> Result<String, String> {
    let mut root: Value = existing
        .and_then(|raw| serde_json::from_str(raw.trim()).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    if !root.is_object() {
        root = serde_json::json!({});
    }
    let obj = root.as_object_mut().expect("root coerced to object");
    let oauth_entry = obj
        .entry("claudeAiOauth")
        .or_insert_with(|| serde_json::json!({}));
    if !oauth_entry.is_object() {
        *oauth_entry = serde_json::json!({});
    }
    let oauth = oauth_entry.as_object_mut().expect("oauth coerced to object");

    oauth.insert("accessToken".into(), Value::String(access.to_string()));
    if let Some(r) = new_refresh {
        oauth.insert("refreshToken".into(), Value::String(r.to_string()));
    }
    // expires_in is untrusted JSON; clamp it and build the deadline without
    // panicking — `panic = "abort"` would take the whole app down on overflow.
    // Mirrors the Copilot device-flow guard: cap to a day, floor at 5 min, and
    // fall back to 8h if the arithmetic ever can't be represented.
    let expires_at = Duration::try_seconds(expires_in.clamp(300, 86_400))
        .and_then(|d| now.checked_add_signed(d))
        .unwrap_or_else(|| now + Duration::seconds(28_800))
        .timestamp_millis();
    oauth.insert("expiresAt".into(), Value::Number(expires_at.into()));
    if let Some(scope) = scope {
        let scopes: Vec<Value> = scope
            .split_whitespace()
            .map(|s| Value::String(s.to_string()))
            .collect();
        if !scopes.is_empty() {
            oauth.insert("scopes".into(), Value::Array(scopes));
        }
    }
    // Claude Code expects this key; default it to null on a fresh login (it fills
    // the real plan in separately) without clobbering an existing value.
    oauth.entry("subscriptionType").or_insert(Value::Null);

    serde_json::to_string(&root).map_err(|e| format!("serialize: {e}"))
}

/// Local, network-free view of the stored login: whether an access token is
/// present, whether it's past its stated expiry (with skew), and whether a
/// refresh token is available to renew it. Lets `collect()` detect a dead login
/// up front instead of serving a stale cached reading until the next 401.
pub struct TokenStatus {
    pub present: bool,
    pub expired: bool,
    pub has_refresh: bool,
}

pub fn token_status(now: DateTime<Utc>) -> TokenStatus {
    read_raw_credentials()
        .map(|raw| parse_token_status(&raw, now))
        .unwrap_or(TokenStatus { present: false, expired: false, has_refresh: false })
}

/// Pure half of `token_status`, split out so it's unit-testable without touching
/// the keychain / credentials file.
fn parse_token_status(raw: &str, now: DateTime<Utc>) -> TokenStatus {
    let Ok(v) = serde_json::from_str::<Value>(raw.trim()) else {
        return TokenStatus { present: false, expired: false, has_refresh: false };
    };
    let oauth = v.get("claudeAiOauth");
    let nonempty = |key: &str| {
        oauth
            .and_then(|o| o.get(key))
            .and_then(|t| t.as_str())
            .map(|s| !s.is_empty())
            .unwrap_or(false)
    };
    let present = nonempty("accessToken");
    let has_refresh = nonempty("refreshToken");
    // A missing/unparseable expiresAt counts as not-expired: don't force a
    // needless reauth on a token that may still be valid — the live fetch's 401
    // remains the backstop.
    let expired = oauth
        .and_then(|o| o.get("expiresAt"))
        .and_then(value_as_i64)
        .map(|exp| now.timestamp_millis() >= exp - EXPIRY_SKEW_MS)
        .unwrap_or(false);
    TokenStatus { present, expired: present && expired, has_refresh }
}

/// Persist refreshed credentials to the store Claude Code reads.
fn write_credentials(json: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        match keychain_write(json) {
            // Keep a co-existing credentials file in sync if one is present so
            // the two stores can't diverge.
            Ok(()) => {
                let _ = write_credentials_file_if_exists(json);
                Ok(())
            }
            // Keychain write failed AFTER the server already rotated the refresh
            // token — persist to the file so the only-valid tokens aren't lost.
            Err(e) => write_credentials_file(json)
                .map_err(|fe| format!("keychain update failed ({e}) and file fallback failed ({fe})")),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        write_credentials_file(json)
    }
}

/// Remove the Claude Code login from this machine entirely — both the macOS
/// keychain item and `~/.claude/.credentials.json`. This is a SHARED store, so
/// it signs the Claude Code CLI out too; the user must sign in again (here or via
/// `claude /login`) to use either. Best-effort on each store, then verifies that
/// nothing still resolves so the "signed out" post-condition holds.
pub fn clear_credentials() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        // Delete the keychain item(s). Target the exact account the item was
        // created under (mirrors `keychain_write`) so we remove the right one —
        // `-s SERVICE` alone deletes only the first match — and loop in case an
        // older build/another machine left duplicates under the service. Stop as
        // soon as none remain or a delete can't make progress (the post-condition
        // below reports anything still left).
        while keychain_read().is_some() {
            let mut cmd = std::process::Command::new("security");
            cmd.arg("delete-generic-password");
            if let Some(account) = keychain_account() {
                cmd.args(["-a", &account]);
            }
            let deleted = cmd
                .args(["-s", SERVICE])
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            if !deleted {
                break;
            }
        }
    }

    if let Some(home) = dirs::home_dir() {
        let path = home.join(".claude").join(".credentials.json");
        if path.exists() {
            std::fs::remove_file(&path).map_err(|e| format!("remove credentials file: {e}"))?;
        }
    }

    // Verify each store separately so the error names what's left for the user to
    // remove by hand (the keychain item vs. the file), rather than a vague "some
    // of it is still here".
    let mut left: Vec<&str> = Vec::new();
    #[cfg(target_os = "macos")]
    if keychain_read().is_some() {
        left.push("the macOS keychain item “Claude Code-credentials”");
    }
    if dirs::home_dir()
        .map(|h| h.join(".claude").join(".credentials.json").exists())
        .unwrap_or(false)
    {
        left.push("~/.claude/.credentials.json");
    }
    if !left.is_empty() {
        return Err(format!(
            "Couldn’t remove {} — remove it manually, then try again.",
            left.join(" and ")
        ));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn keychain_write(json: &str) -> Result<(), String> {
    let account = keychain_account()
        .or_else(|| std::env::var("USER").ok())
        .ok_or("could not determine keychain account")?;
    // `-U` updates the existing item in place (preserving its access control),
    // so Claude Code keeps reading it without a new keychain prompt.
    let status = std::process::Command::new("security")
        .args(["add-generic-password", "-U", "-a", &account, "-s", SERVICE, "-w", json])
        .status()
        .map_err(|e| format!("spawn security: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("security add-generic-password returned non-zero".into())
    }
}

/// Read the account name on the existing keychain item so we update that exact
/// item rather than creating a divergent one under a different account.
#[cfg(target_os = "macos")]
fn keychain_account() -> Option<String> {
    let out = std::process::Command::new("security")
        .args(["find-generic-password", "-s", SERVICE])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        // Format: `    "acct"<blob>="dennisrongo"`
        if let Some(idx) = line.find("\"acct\"") {
            if let Some(eq) = line[idx..].find('=') {
                let val = line[idx + eq + 1..].trim().trim_matches('"');
                if !val.is_empty() {
                    return Some(val.to_string());
                }
            }
        }
    }
    None
}

fn write_credentials_file(json: &str) -> Result<(), String> {
    let home = dirs::home_dir().ok_or("no home directory")?;
    let dir = home.join(".claude");
    std::fs::create_dir_all(&dir).map_err(|e| format!("create ~/.claude: {e}"))?;
    let path = dir.join(".credentials.json");
    std::fs::write(&path, json).map_err(|e| format!("write credentials file: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn write_credentials_file_if_exists(json: &str) -> Result<(), String> {
    let home = dirs::home_dir().ok_or("no home directory")?;
    let path = home.join(".claude").join(".credentials.json");
    if path.exists() {
        write_credentials_file(json)
    } else {
        Ok(())
    }
}

pub async fn fetch(now: DateTime<Utc>) -> ClaudeLive {
    let Some(token) = read_token() else {
        return ClaudeLive::off(false, None);
    };

    let client = match reqwest::Client::builder()
        .timeout(StdDuration::from_secs(12))
        .build()
    {
        Ok(c) => c,
        Err(e) => return ClaudeLive::off(true, Some(format!("client init: {e}"))),
    };

    let resp = client
        .get(ENDPOINT)
        .header("Authorization", format!("Bearer {token}"))
        .header("anthropic-beta", OAUTH_BETA)
        .header("anthropic-version", "2023-06-01")
        .header("Content-Type", "application/json")
        .send()
        .await;

    match resp {
        Ok(r) => {
            let status = r.status();
            if !status.is_success() {
                let expired = status.as_u16() == 401;
                let hint = if expired {
                    " (Claude Code login expired — open Claude Code to re-auth)"
                } else {
                    ""
                };
                let mut off =
                    ClaudeLive::off(true, Some(format!("HTTP {}{hint}", status.as_u16())));
                off.expired = expired;
                return off;
            }
            match r.json::<Value>().await {
                Ok(v) => {
                    let buckets = parse(&v, now);
                    if buckets.is_empty() {
                        ClaudeLive::off(true, Some("no usage windows in response".into()))
                    } else {
                        ClaudeLive { configured: true, ok: true, expired: false, error: None, buckets }
                    }
                }
                Err(e) => ClaudeLive::off(true, Some(format!("invalid JSON: {e}"))),
            }
        }
        Err(e) => ClaudeLive::off(true, Some(format!("request error: {e}"))),
    }
}

/// Parse the normalized `limits[]` array into display buckets.
pub fn parse(v: &Value, now: DateTime<Utc>) -> Vec<Bucket> {
    let Some(limits) = v.get("limits").and_then(|l| l.as_array()) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for lim in limits {
        let kind = lim.get("kind").and_then(|k| k.as_str()).unwrap_or("");
        let pct = lim.get("percent").and_then(value_as_f64).unwrap_or(0.0);
        let severity = lim.get("severity").and_then(|s| s.as_str()).unwrap_or("normal");
        let resets_at = lim.get("resets_at").and_then(|r| r.as_str());
        let scope_model = lim
            .get("scope")
            .and_then(|s| s.get("model"))
            .and_then(|m| m.get("display_name"))
            .and_then(|d| d.as_str());

        let (name, sub) = match kind {
            "session" => ("Session".to_string(), "5-hour window".to_string()),
            "weekly_all" => ("Week · all models".to_string(), "resets weekly".to_string()),
            "weekly_scoped" => (
                format!("Week · {}", scope_model.unwrap_or("scoped")),
                "resets weekly".to_string(),
            ),
            other if !other.is_empty() => (titleize(other), String::new()),
            _ => continue,
        };

        let (status, status_label) = severity_status(severity);
        out.push(Bucket {
            name,
            sub,
            used_fmt: String::new(),
            used_pct: (pct * 10.0).round() / 10.0,
            left_pct: ((100.0 - pct) * 10.0).round() / 10.0,
            left_fmt: String::new(),
            limit_fmt: String::new(),
            reset: countdown(resets_at, now),
            status: status.to_string(),
            status_label: status_label.to_string(),
            live: true,
        });
    }
    out
}

fn severity_status(sev: &str) -> (&'static str, &'static str) {
    match sev {
        "normal" | "ok" | "low" => ("ok", "Healthy"),
        "warning" | "warn" | "approaching" | "medium" => ("warn", "Watch"),
        _ => ("danger", "Near limit"),
    }
}

fn countdown(resets_at: Option<&str>, now: DateTime<Utc>) -> String {
    let Some(ts) = resets_at else { return "—".to_string() };
    let Ok(reset) = DateTime::parse_from_rfc3339(ts) else { return "—".to_string() };
    let secs = (reset.with_timezone(&Utc) - now).num_seconds();
    if secs <= 0 {
        return "resetting".to_string();
    }
    let d = secs / 86_400;
    let h = (secs % 86_400) / 3_600;
    let m = (secs % 3_600) / 60;
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else {
        format!("{m}m")
    }
}

fn titleize(s: &str) -> String {
    s.replace('_', " ")
        .split(' ')
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
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

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-06-17T20:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn parses_real_limits_array() {
        let v = json!({
            "limits": [
                { "kind": "session", "percent": 55, "severity": "normal", "resets_at": "2026-06-18T00:49:59+00:00" },
                { "kind": "weekly_all", "percent": 22, "severity": "normal", "resets_at": "2026-06-22T23:59:59+00:00" },
                { "kind": "weekly_scoped", "percent": 0, "severity": "normal", "resets_at": null,
                  "scope": { "model": { "display_name": "Sonnet" } } }
            ]
        });
        let b = parse(&v, now());
        assert_eq!(b.len(), 3);
        assert_eq!(b[0].name, "Session");
        assert_eq!(b[0].used_pct, 55.0);
        assert!(b[0].live);
        assert_eq!(b[0].reset, "4h 49m");
        assert_eq!(b[1].name, "Week · all models");
        assert_eq!(b[2].name, "Week · Sonnet");
        assert_eq!(b[2].reset, "—");
    }

    #[test]
    fn missing_limits_is_empty() {
        assert!(parse(&json!({ "foo": 1 }), now()).is_empty());
    }

    #[test]
    fn severity_maps_to_status() {
        let v = json!({ "limits": [
            { "kind": "session", "percent": 95, "severity": "critical", "resets_at": null }
        ]});
        let b = parse(&v, now());
        assert_eq!(b[0].status, "danger");
    }

    #[test]
    fn pkce_challenge_matches_rfc7636_vector() {
        // RFC 7636 Appendix B worked example.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            pkce_challenge(verifier),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn authorize_url_has_required_params() {
        let a = build_authorize();
        assert!(a.url.starts_with("https://claude.ai/oauth/authorize?"));
        assert!(a.url.contains("response_type=code"));
        assert!(a.url.contains(&format!("client_id={CLIENT_ID}")));
        assert!(a.url.contains("code_challenge_method=S256"));
        assert!(a.url.contains("redirect_uri=https%3A%2F%2Fplatform.claude.com%2Foauth%2Fcode%2Fcallback"));
        // scope is space-joined → percent-encoded spaces.
        assert!(a.url.contains("scope=org%3Acreate_api_key%20user%3Aprofile"));
        assert!(a.url.contains(&format!("state={}", a.state)));
        // The challenge in the URL is the S256 hash of the returned verifier.
        assert!(a.url.contains(&format!("code_challenge={}", pkce_challenge(&a.verifier))));
        // verifier (64 bytes b64url) ≈ 86 chars; state (32 bytes hex) = 64 chars.
        assert_eq!(a.state.len(), 64);
        assert!(a.verifier.len() >= 80);
    }

    #[test]
    fn parses_pasted_code_forms() {
        // The canonical CODE#STATE the authorize page shows.
        assert_eq!(
            parse_pasted_code("ac_abc123#st_xyz789"),
            ("ac_abc123".to_string(), Some("st_xyz789".to_string()))
        );
        // A bare code (user copied only the first half).
        assert_eq!(parse_pasted_code("  ac_abc123  "), ("ac_abc123".to_string(), None));
        // A whole callback URL pasted in.
        assert_eq!(
            parse_pasted_code("https://platform.claude.com/oauth/code/callback?code=ac_1&state=st_2"),
            ("ac_1".to_string(), Some("st_2".to_string()))
        );
    }

    #[test]
    fn build_credentials_merges_into_existing_preserving_fields() {
        let existing = r#"{"claudeAiOauth":{
            "accessToken":"old","refreshToken":"oldR","subscriptionType":"max",
            "organizationUuid":"org-1","email":"a@b.co"
        },"otherTool":{"x":1}}"#;
        let out = build_credentials_json(
            Some(existing), now(), "newAccess", Some("newR"), 28_800, Some("user:profile user:inference"),
        )
        .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        let o = v.get("claudeAiOauth").unwrap();
        assert_eq!(o.get("accessToken").unwrap(), "newAccess");
        assert_eq!(o.get("refreshToken").unwrap(), "newR");
        // Fields Claude Code wrote are preserved.
        assert_eq!(o.get("subscriptionType").unwrap(), "max");
        assert_eq!(o.get("organizationUuid").unwrap(), "org-1");
        assert_eq!(o.get("email").unwrap(), "a@b.co");
        // Sibling keys untouched.
        assert_eq!(v.get("otherTool").unwrap().get("x").unwrap(), 1);
        // Scopes parsed from the space-joined string.
        let scopes = o.get("scopes").unwrap().as_array().unwrap();
        assert_eq!(scopes.len(), 2);
        assert_eq!(scopes[0], "user:profile");
        // expiresAt is now + 8h in ms.
        let exp = o.get("expiresAt").unwrap().as_i64().unwrap();
        assert_eq!(exp, (now() + Duration::seconds(28_800)).timestamp_millis());
    }

    #[test]
    fn build_credentials_creates_fresh_when_no_prior_login() {
        // First in-app sign-in: no existing credentials file/keychain.
        let out =
            build_credentials_json(None, now(), "acc", Some("ref"), 28_800, Some("user:profile")).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        let o = v.get("claudeAiOauth").unwrap();
        assert_eq!(o.get("accessToken").unwrap(), "acc");
        assert_eq!(o.get("refreshToken").unwrap(), "ref");
        // subscriptionType defaults to null (Claude Code fills the real plan in).
        assert!(o.get("subscriptionType").unwrap().is_null());
        // A garbage/non-object prior value is replaced, not merged into.
        let out2 = build_credentials_json(Some("not json"), now(), "a", None, 28_800, None).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&out2).unwrap()["claudeAiOauth"]["accessToken"],
            "a"
        );
    }

    #[test]
    fn build_credentials_clamps_hostile_expires_in_without_panic() {
        // panic = "abort": a malicious/garbled expires_in must not overflow-panic.
        let huge = build_credentials_json(None, now(), "a", None, i64::MAX, None).unwrap();
        let exp = serde_json::from_str::<Value>(&huge).unwrap()["claudeAiOauth"]["expiresAt"]
            .as_i64()
            .unwrap();
        // Clamped to +1 day.
        assert_eq!(exp, (now() + Duration::seconds(86_400)).timestamp_millis());

        // Negative/zero floors to +5 min rather than producing an already-expired token.
        let neg = build_credentials_json(None, now(), "a", None, -100, None).unwrap();
        let exp2 = serde_json::from_str::<Value>(&neg).unwrap()["claudeAiOauth"]["expiresAt"]
            .as_i64()
            .unwrap();
        assert_eq!(exp2, (now() + Duration::seconds(300)).timestamp_millis());
    }

    #[test]
    fn build_credentials_keeps_prior_refresh_token_when_server_omits_one() {
        let existing = r#"{"claudeAiOauth":{"accessToken":"old","refreshToken":"keepR"}}"#;
        let out = build_credentials_json(Some(existing), now(), "new", None, 28_800, None).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["claudeAiOauth"]["refreshToken"], "keepR");
        assert_eq!(v["claudeAiOauth"]["accessToken"], "new");
    }

    #[test]
    fn token_status_reads_presence_and_expiry() {
        let now = now(); // 2026-06-17T20:00:00Z
        let future = (now + Duration::hours(2)).timestamp_millis();
        let past = (now - Duration::hours(1)).timestamp_millis();

        let live = format!(
            r#"{{"claudeAiOauth":{{"accessToken":"a","refreshToken":"r","expiresAt":{future}}}}}"#
        );
        let s = parse_token_status(&live, now);
        assert!(s.present && s.has_refresh && !s.expired);

        let expired = format!(
            r#"{{"claudeAiOauth":{{"accessToken":"a","refreshToken":"r","expiresAt":{past}}}}}"#
        );
        let s = parse_token_status(&expired, now);
        assert!(s.present && s.has_refresh && s.expired);

        // No token at all.
        let s = parse_token_status("{}", now);
        assert!(!s.present && !s.has_refresh && !s.expired);

        // Token present but no refresh token → can't auto-renew.
        let s = parse_token_status(
            &format!(r#"{{"claudeAiOauth":{{"accessToken":"a","expiresAt":{past}}}}}"#),
            now,
        );
        assert!(s.present && !s.has_refresh && s.expired);
    }
}
