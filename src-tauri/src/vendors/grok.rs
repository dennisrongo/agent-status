//! Grok Build (xAI) LIVE usage client.
//!
//! Reads the SpaceXAI OIDC login the Grok CLI stores at `$GROK_HOME/auth.json`
//! (default `~/.grok`) and calls `GET https://cli-chat-proxy.grok.com/v1/billing?format=credits`
//! — the same endpoint the CLI's `/usage` panel reads. SuperGrok / Grok Build
//! subscribers typically get a weekly window (no percent-used ceiling); API-key
//! / credits accounts may carry `creditUsagePercent` or a monthly `used` /
//! `monthlyLimit` pair. `val` fields are `{ "val": N }` wrappers.
//!
//! Access tokens last a few hours. The CLI refreshes them only while it runs,
//! so an expired-by-clock login is renewed in place (`refresh()`) using the
//! stored refresh token and the CLI's public OIDC client, then written back
//! to the same `auth.json` the CLI hot-reloads. The refresh token is
//! single-use, so a persistence failure is a hard error rather than dropping
//! the only-valid token — mirroring kimi.rs / claude.rs.

use chrono::{DateTime, Utc};
use serde_json::Value;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use crate::process_util::SilentCommand;
use super::{KeyVal, VendorStatus};

const BILLING_ENDPOINT: &str = "https://cli-chat-proxy.grok.com/v1/billing?format=credits";
const TOKEN_ENDPOINT: &str = "https://auth.x.ai/oauth2/token";
/// The Grok CLI's public SpaceXAI OIDC client id (authorization-code + PKCE;
/// no secret). Used only with the user's own stored credentials.
const DEFAULT_CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
const USER_AGENT: &str = "xai-grok-cli";
/// Treat the access token as expired this many seconds before its stated
/// `expires_at`, so an about-to-die token isn't used for a fetch that would
/// 401 mid-flight anyway.
const EXPIRY_SKEW_SECS: i64 = 60;

pub async fn fetch(now: DateTime<Utc>) -> VendorStatus {
    let Some(raw) = read_credentials() else {
        return VendorStatus::not_configured();
    };
    let Ok(root) = serde_json::from_str::<Value>(raw.trim()) else {
        return VendorStatus::failed("stored credentials unreadable");
    };
    let Some((_, creds)) = selected_credential(&root) else {
        return VendorStatus::not_configured();
    };
    let Some(token) = creds
        .get("key")
        .and_then(|t| t.as_str())
        .filter(|s| !s.is_empty())
    else {
        return VendorStatus::not_configured();
    };

    if credential_expired(creds, now) {
        return login_expired(
            "Grok login expired — sign in again from Settings (or run `grok login`).",
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
        .get(BILLING_ENDPOINT)
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/json")
        .header("User-Agent", USER_AGENT)
        .send()
        .await;

    match resp {
        Ok(r) => {
            let status = r.status();
            if !status.is_success() {
                if status.as_u16() == 401 {
                    return login_expired(
                        "Grok login was rejected (HTTP 401) — sign in again from Settings (or run `grok login`)."
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

fn grok_home() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("GROK_HOME") {
        let p = PathBuf::from(home);
        if !p.as_os_str().is_empty() {
            return Some(p);
        }
    }
    dirs::home_dir().map(|h| h.join(".grok"))
}

fn credentials_path() -> Option<PathBuf> {
    Some(grok_home()?.join("auth.json"))
}

fn read_credentials() -> Option<String> {
    std::fs::read_to_string(credentials_path()?).ok()
}

/// Pick the stored login the CLI will actually use. `auth.json` is a map of
/// `"<issuer>::<client_id>" → credential`. Prefer the SpaceXAI (`auth.x.ai`)
/// entry with a non-empty access token; fall back to any other non-empty
/// `key` so a custom OIDC login still works for status/fetch. Refresh refuses
/// a non-xAI issuer — that token must not be posted to auth.x.ai.
fn selected_credential(root: &Value) -> Option<(&str, &Value)> {
    let obj = root.as_object()?;
    let mut fallback = None;
    for (k, v) in obj {
        let nonempty = v
            .get("key")
            .and_then(|t| t.as_str())
            .map(|s| !s.is_empty())
            .unwrap_or(false);
        if !nonempty {
            continue;
        }
        let issuer = v
            .get("oidc_issuer")
            .and_then(|s| s.as_str())
            .unwrap_or(k.as_str());
        if issuer_is_xai(issuer, k) {
            return Some((k.as_str(), v));
        }
        if fallback.is_none() {
            fallback = Some((k.as_str(), v));
        }
    }
    fallback
}

/// True when the issuer (or the `auth.json` map key) is hosted at `auth.x.ai`.
/// Host-only: a path like `https://evil.example/auth.x.ai` must not match.
fn issuer_is_xai(issuer: &str, map_key: &str) -> bool {
    host_is_auth_xai(issuer) || host_is_auth_xai(map_key)
}

fn host_is_auth_xai(s: &str) -> bool {
    let head = s.split("::").next().unwrap_or(s).trim();
    let after_scheme = match head.find("://") {
        Some(i) => &head[i + 3..],
        None => head,
    };
    let host = after_scheme
        .split('/')
        .next()
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("");
    host.eq_ignore_ascii_case("auth.x.ai")
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
    let Some((_, creds)) = selected_credential(&root) else {
        return TokenStatus {
            present: false,
            expired: false,
            has_refresh: false,
        };
    };
    let present = creds
        .get("key")
        .and_then(|t| t.as_str())
        .map(|s| !s.is_empty())
        .unwrap_or(false);
    let has_refresh = creds
        .get("refresh_token")
        .and_then(|t| t.as_str())
        .map(|s| !s.is_empty())
        .unwrap_or(false);
    TokenStatus {
        present,
        expired: present && credential_expired(creds, now),
        has_refresh,
    }
}

fn credential_expired(creds: &Value, now: DateTime<Utc>) -> bool {
    parse_expires_at(creds)
        .map(|exp| now >= exp - chrono::Duration::seconds(EXPIRY_SKEW_SECS))
        .unwrap_or(false)
}

/// `expires_at` arrives as an RFC3339 string (the CLI's shape). Accept a unix
/// timestamp too so a refresh we wrote as either form still parses.
fn parse_expires_at(creds: &Value) -> Option<DateTime<Utc>> {
    let v = creds.get("expires_at")?;
    if let Some(s) = v.as_str() {
        return DateTime::parse_from_rfc3339(s)
            .ok()
            .map(|d| d.with_timezone(&Utc));
    }
    value_as_i64(v).and_then(|secs| DateTime::from_timestamp(secs, 0))
}

pub async fn refresh(now: DateTime<Utc>) -> Result<(), String> {
    let raw = read_credentials().ok_or("No Grok login found to refresh.")?;
    let root: Value = serde_json::from_str(raw.trim())
        .map_err(|e| format!("stored credentials unreadable: {e}"))?;
    let (key, creds) = selected_credential(&root).ok_or("No Grok login found to refresh.")?;
    let issuer = creds
        .get("oidc_issuer")
        .and_then(|s| s.as_str())
        .unwrap_or(key);
    if !issuer_is_xai(issuer, key) {
        return Err(
            "Grok login is not a SpaceXAI account — sign in again from Settings (or run `grok login`)."
                .into(),
        );
    }
    let refresh_token = creds
        .get("refresh_token")
        .and_then(|t| t.as_str())
        .filter(|s| !s.is_empty())
        .ok_or("No refresh token stored — sign in again from Settings (or run `grok login`).")?
        .to_string();
    let client_id = creds
        .get("oidc_client_id")
        .and_then(|t| t.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_CLIENT_ID)
        .to_string();

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| format!("client init: {e}"))?;

    let resp = client
        .post(TOKEN_ENDPOINT)
        .form(&[
            ("client_id", client_id.as_str()),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token.as_str()),
        ])
        .send()
        .await
        .map_err(|e| format!("request error: {e}"))?;

    let status = resp.status();
    if !status.is_success() {
        if status.as_u16() == 400 || status.as_u16() == 401 {
            return Err(
                "Grok refresh token expired — sign in again from Settings (or run `grok login`)."
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
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let expires_in = tok.get("expires_in").and_then(value_as_i64).unwrap_or(21_600);
    let serialized = build_refreshed_credentials(
        &raw,
        now,
        &access,
        new_refresh.as_deref(),
        expires_in,
    )?;
    write_credentials_file(&serialized)
}

fn build_refreshed_credentials(
    existing: &str,
    now: DateTime<Utc>,
    access: &str,
    new_refresh: Option<&str>,
    expires_in: i64,
) -> Result<String, String> {
    let mut root: Value = serde_json::from_str(existing.trim())
        .map_err(|e| format!("stored credentials unreadable: {e}"))?;
    let key = {
        let selected = selected_credential(&root).ok_or("No Grok login found to refresh.")?;
        selected.0.to_string()
    };
    let obj = root
        .as_object_mut()
        .ok_or("stored credentials are not an object")?
        .get_mut(&key)
        .and_then(|v| v.as_object_mut())
        .ok_or("selected credential vanished")?;

    obj.insert("key".into(), Value::String(access.to_string()));
    if let Some(r) = new_refresh {
        obj.insert("refresh_token".into(), Value::String(r.to_string()));
    }
    let clamped = expires_in.clamp(300, 86_400);
    let expires_at = chrono::Duration::try_seconds(clamped)
        .and_then(|d| now.checked_add_signed(d))
        .unwrap_or_else(|| now + chrono::Duration::seconds(21_600));
    obj.insert(
        "expires_at".into(),
        Value::String(expires_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
    );
    serde_json::to_string(&root).map_err(|e| format!("serialize: {e}"))
}

fn write_credentials_file(json: &str) -> Result<(), String> {
    let path = credentials_path().ok_or("no home directory for Grok credentials")?;
    std::fs::write(&path, json).map_err(|e| format!("write {}: {e}", path.display()))
}

const INSTALL_PRIMARY: &str = "https://x.ai/cli";
const INSTALL_FALLBACK: &str = "https://storage.googleapis.com/grok-build-public-artifacts/cli";
const INSTALL_CHANNEL: &str = "stable";

/// Install the official Grok CLI binary into `$GROK_HOME/bin` (default
/// `~/.grok/bin`) — the same artifact the `install.sh` / `install.ps1`
/// scripts fetch. Cross-platform: macOS and Windows (x86_64 / arm64).
/// After this, [`find_cli`] sees the binary without requiring a PATH change.
pub async fn install() -> Result<String, String> {
    let home = grok_home().ok_or("no home directory for Grok install")?;
    let bin_dir = std::env::var_os("GROK_BIN_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join("bin"));
    let download_dir = home.join("downloads");
    std::fs::create_dir_all(&bin_dir)
        .map_err(|e| format!("create {}: {e}", bin_dir.display()))?;
    std::fs::create_dir_all(&download_dir)
        .map_err(|e| format!("create {}: {e}", download_dir.display()))?;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(300))
        .user_agent(USER_AGENT)
        .build()
        .map_err(|e| format!("client init: {e}"))?;

    let (base, version) = resolve_install_version(&client).await?;
    let platform = install_platform()?;
    let dest = bin_dir.join(cli_bin_name());
    let tmp = download_dir.join(format!("grok-{platform}.tmp"));

    let mut last_err = String::from("no download URL tried");
    for url in artifact_urls(&base, &version, platform) {
        match download_binary(&client, &url, &tmp).await {
            Ok(()) => {
                last_err.clear();
                break;
            }
            Err(e) => last_err = e,
        }
    }
    if !last_err.is_empty() {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("Grok CLI download failed: {last_err}"));
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&tmp)
            .map_err(|e| format!("stat {}: {e}", tmp.display()))?
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&tmp, perms)
            .map_err(|e| format!("chmod {}: {e}", tmp.display()))?;
    }

    std::fs::copy(&tmp, &dest).map_err(|e| format!("install {}: {e}", dest.display()))?;
    let _ = std::fs::remove_file(&tmp);

    if find_cli().is_some() {
        Ok(format!(
            "Grok CLI {version} installed. Sign in from Settings to connect."
        ))
    } else {
        Ok(format!(
            "Installed to {}, but grok isn’t on PATH yet — restart the app.",
            dest.display()
        ))
    }
}

fn cli_bin_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "grok.exe"
    } else {
        "grok"
    }
}

fn install_platform() -> Result<&'static str, String> {
    let os = if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        return Err("Grok CLI install is not supported on this OS.".into());
    };
    let arch = if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else {
        return Err("Grok CLI install is not supported on this architecture.".into());
    };
    Ok(match (os, arch) {
        ("macos", "aarch64") => "macos-aarch64",
        ("macos", "x86_64") => "macos-x86_64",
        ("windows", "aarch64") => "windows-aarch64",
        ("windows", "x86_64") => "windows-x86_64",
        ("linux", "aarch64") => "linux-aarch64",
        ("linux", "x86_64") => "linux-x86_64",
        _ => return Err(format!("unsupported platform {os}-{arch}")),
    })
}

fn parse_channel_version(raw: &str) -> Result<String, String> {
    let version = raw
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .trim_start_matches('\u{feff}')
        .to_string();
    if !is_semver_tag(&version) {
        return Err(format!("invalid version from channel pointer: {version:?}"));
    }
    Ok(version)
}

fn is_semver_tag(s: &str) -> bool {
    let mut parts = s.splitn(2, '-');
    let core = parts.next().unwrap_or("");
    let nums: Vec<&str> = core.split('.').collect();
    nums.len() == 3 && nums.iter().all(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}

fn artifact_urls(base: &str, version: &str, platform: &str) -> Vec<String> {
    let stem = format!("{base}/grok-{version}-{platform}");
    if platform.starts_with("windows-") {
        vec![format!("{stem}.exe"), stem]
    } else {
        vec![stem]
    }
}

async fn resolve_install_version(client: &reqwest::Client) -> Result<(String, String), String> {
    let mut last = String::from("no endpoint tried");
    for base in [INSTALL_PRIMARY, INSTALL_FALLBACK] {
        let url = format!("{base}/{INSTALL_CHANNEL}");
        match client.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => match resp.text().await {
                Ok(body) => match parse_channel_version(&body) {
                    Ok(v) => return Ok((base.to_string(), v)),
                    Err(e) => last = e,
                },
                Err(e) => last = format!("read {url}: {e}"),
            },
            Ok(resp) => last = format!("{url} HTTP {}", resp.status().as_u16()),
            Err(e) => last = format!("{url}: {e}"),
        }
    }
    Err(format!("Couldn’t fetch the latest Grok CLI version ({last})."))
}

async fn download_binary(client: &reqwest::Client, url: &str, dest: &PathBuf) -> Result<(), String> {
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("{url}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("{url} HTTP {}", resp.status().as_u16()));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("read {url}: {e}"))?;
    if bytes.len() < 1_024 {
        return Err(format!("{url}: response too small ({} bytes)", bytes.len()));
    }
    std::fs::write(dest, &bytes).map_err(|e| format!("write {}: {e}", dest.display()))
}

/// Locate the `grok` CLI. PATH first, then the installer's well-known
/// `$GROK_HOME/bin` (default `~/.grok/bin`).
pub fn find_cli() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    const NAMES: &[&str] = &["grok.exe", "grok.cmd"];
    #[cfg(not(target_os = "windows"))]
    const NAMES: &[&str] = &["grok"];

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

    if let Some(home) = grok_home() {
        for name in NAMES {
            let candidate = home.join("bin").join(name);
            if candidate.is_file() {
                return Some(candidate);
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

fn grok_command(cli: &std::path::Path) -> std::process::Command {
    #[cfg(target_os = "windows")]
    {
        let ext = cli.extension().and_then(|e| e.to_str()).unwrap_or("");
        if ext.eq_ignore_ascii_case("cmd") || ext.eq_ignore_ascii_case("bat") {
            let mut cmd = std::process::Command::new("cmd");
            cmd.arg("/C").arg(cli).silent();
            return cmd;
        }
    }
    let mut cmd = std::process::Command::new(cli);
    cmd.silent();
    cmd
}

/// Run `grok login` — the CLI's SpaceXAI OAuth browser flow (default). The
/// CLI opens the user's browser and blocks until authorization completes.
pub fn login() -> Result<String, String> {
    let Some(cli) = find_cli() else {
        return Err("Grok CLI not found — install it first.".to_string());
    };

    let out = grok_command(&cli)
        .arg("login")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("spawn: {e}"))?;
    if out.status.success() {
        Ok("Authenticated with Grok. Usage will appear on the next refresh.".to_string())
    } else {
        let detail = String::from_utf8_lossy(&out.stderr);
        let last = detail
            .lines()
            .rfind(|l| !l.trim().is_empty())
            .map(|l| l.trim().to_string())
            .unwrap_or_else(|| format!("exit code {}", out.status.code().unwrap_or(-1)));
        Err(format!("Login failed: {last}"))
    }
}

/// Sign out of Grok. Prefers `grok logout` so the CLI clears its own cache;
/// falls back to blanking the stored tokens if the binary isn't reachable.
pub fn logout() -> Result<String, String> {
    if let Some(cli) = find_cli() {
        let out = grok_command(&cli)
            .arg("logout")
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("spawn: {e}"))?;
        if out.status.success() {
            return Ok("Disconnected from Grok — the CLI is signed out too.".to_string());
        }
        // Fall through to a local tombstone if the CLI rejected the logout
        // (older builds, or a half-written credentials file).
    }
    let path = credentials_path().ok_or("no home directory for Grok credentials")?;
    let raw = std::fs::read_to_string(&path)
        .map_err(|_| "No Grok login found — already signed out.".to_string())?;
    let tombstone = build_logout_tombstone(&raw)?;
    std::fs::write(&path, tombstone).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok("Disconnected from Grok.".to_string())
}

fn build_logout_tombstone(existing: &str) -> Result<String, String> {
    let mut root: Value = serde_json::from_str(existing.trim())
        .map_err(|e| format!("stored credentials unreadable: {e}"))?;
    let key = selected_credential(&root)
        .map(|(k, _)| k.to_string())
        .ok_or("No Grok login found — already signed out.")?;
    let obj = root
        .as_object_mut()
        .ok_or("stored credentials are not an object")?
        .get_mut(&key)
        .and_then(|v| v.as_object_mut())
        .ok_or("selected credential vanished")?;
    obj.insert("key".into(), Value::String(String::new()));
    obj.insert("refresh_token".into(), Value::String(String::new()));
    obj.insert("expires_at".into(), Value::String("1970-01-01T00:00:00Z".into()));
    serde_json::to_string(&root).map_err(|e| format!("serialize: {e}"))
}

/// Pure parser for the `/v1/billing?format=credits` response.
pub fn parse(v: &Value, now: DateTime<Utc>) -> VendorStatus {
    let config = v.get("config").unwrap_or(v);

    let period = config.get("currentPeriod");
    let period_type = period
        .and_then(|p| p.get("type"))
        .and_then(|t| t.as_str())
        .map(humanize_period)
        .unwrap_or_else(|| "Weekly".to_string());
    let period_end = period
        .and_then(|p| p.get("end"))
        .and_then(|e| e.as_str())
        .or_else(|| config.get("billingPeriodEnd").and_then(|e| e.as_str()));
    let reset = period_end.and_then(|s| countdown(s, now));

    let credit_pct = config
        .get("creditUsagePercent")
        .and_then(value_as_f64)
        .filter(|n| n.is_finite());

    let monthly_limit = nested_val(config, "monthlyLimit").unwrap_or(0.0);
    let monthly_used = nested_val(config, "used").unwrap_or(0.0);
    let on_demand_cap = nested_val(config, "onDemandCap").unwrap_or(0.0);
    let on_demand_used = nested_val(config, "onDemandUsed").unwrap_or(0.0);
    let prepaid = nested_val(config, "prepaidBalance").unwrap_or(0.0);

    let recognized = credit_pct.is_some()
        || monthly_limit > 0.0
        || on_demand_cap > 0.0
        || period
            .and_then(|p| p.get("type"))
            .and_then(|t| t.as_str())
            .is_some()
        || period_end.is_some();
    if !recognized {
        return VendorStatus::failed("unrecognized billing shape");
    }

    let tier = config
        .get("subscriptionTier")
        .and_then(|t| t.as_str())
        .map(humanize_tier)
        .filter(|s| !s.is_empty());
    let unified = config
        .get("isUnifiedBillingUser")
        .and_then(|b| b.as_bool())
        .unwrap_or(false);
    let plan = tier.unwrap_or_else(|| {
        if unified {
            "Grok Build".to_string()
        } else {
            "Grok".to_string()
        }
    });

    let mut detail: Vec<KeyVal> = Vec::new();
    let mut headline_pct: Option<f64> = None;

    if let Some(pct) = credit_pct {
        let value = match &reset {
            Some(r) => format!("resets in {r}"),
            None => format!("{pct:.0}% used"),
        };
        // Short window label like other providers ("Session", "Week"); the
        // per-product meters below name the pool (Grok Build, Grok Imagine).
        detail.push(KeyVal::meter(period_type.clone(), value, pct));
        headline_pct = Some(pct);
    } else if monthly_limit > 0.0 {
        let pct = ((monthly_used / monthly_limit) * 100.0).clamp(0.0, 100.0);
        let value = match &reset {
            Some(r) => format!("resets in {r}"),
            None => format!("{} / {}", fmt_credits(monthly_used), fmt_credits(monthly_limit)),
        };
        detail.push(KeyVal::meter("Monthly", value, pct));
        headline_pct = Some(pct);
    } else {
        // SuperGrok / Grok Build: a weekly window with no published ceiling.
        // Don't invent a percent — a text row + reset countdown is honest.
        let value = match &reset {
            Some(r) => format!("resets in {r}"),
            None => "included".to_string(),
        };
        detail.push(KeyVal::text(period_type, value));
    }

    // Per-product breakdown of the blended `creditUsagePercent` (e.g.
    // GrokBuild 16% / GrokImagine 1%). Entries without a `usagePercent`
    // (GrokChat — the grok.com web pool) are skipped rather than guessed.
    if let Some(products) = config.get("productUsage").and_then(|p| p.as_array()) {
        for p in products {
            let Some(name) = p.get("product").and_then(|n| n.as_str()) else {
                continue;
            };
            let Some(pct) = p
                .get("usagePercent")
                .and_then(value_as_f64)
                .filter(|n| n.is_finite())
            else {
                continue;
            };
            // No per-product reset exists in the payload — products share the
            // account's weekly window, so reuse its countdown.
            let value = match &reset {
                Some(r) => format!("resets in {r}"),
                None => format!("{pct:.0}% used"),
            };
            detail.push(KeyVal::meter(humanize_product(name), value, pct));
        }
    }

    if on_demand_cap > 0.0 {
        let pct = ((on_demand_used / on_demand_cap) * 100.0).clamp(0.0, 100.0);
        detail.push(KeyVal::meter(
            "On-demand",
            format!("{} / {}", fmt_credits(on_demand_used), fmt_credits(on_demand_cap)),
            pct,
        ));
    }
    if prepaid > 0.0 {
        detail.push(KeyVal::text("Prepaid", fmt_credits(prepaid)));
    }
    detail.push(KeyVal::text("Plan", plan.clone()));

    let primary = match headline_pct {
        Some(pct) => format!("{:.0}% used", pct),
        None => plan.clone(),
    };
    let secondary = reset
        .as_ref()
        .map(|r| format!("resets in {r}"))
        .unwrap_or_else(|| "live".to_string());

    VendorStatus {
        configured: true,
        ok: true,
        error: None,
        primary,
        secondary,
        detail,
        auth_expired: false,
    }
}

fn nested_val(obj: &Value, key: &str) -> Option<f64> {
    let v = obj.get(key)?;
    v.get("val").and_then(value_as_f64).or_else(|| value_as_f64(v))
}

fn humanize_period(raw: &str) -> String {
    match raw {
        "USAGE_PERIOD_TYPE_WEEKLY" => "Weekly".to_string(),
        "USAGE_PERIOD_TYPE_MONTHLY" => "Monthly".to_string(),
        "USAGE_PERIOD_TYPE_DAILY" => "Daily".to_string(),
        other => other
            .strip_prefix("USAGE_PERIOD_TYPE_")
            .unwrap_or(other)
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

fn humanize_tier(raw: &str) -> String {
    let trimmed = raw
        .strip_prefix("SUBSCRIPTION_TIER_")
        .or_else(|| raw.strip_prefix("TIER_"))
        .unwrap_or(raw);
    match trimmed {
        "SUPERGROK" | "SuperGrok" => "SuperGrok".to_string(),
        "PREMIUM_PLUS" | "PremiumPlus" => "Premium+".to_string(),
        other if other.is_empty() => String::new(),
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

fn humanize_product(raw: &str) -> String {
    // "GrokBuild" / "GrokImagine" → "Grok Build" / "Grok Imagine". Insert a
    // space at each lower→Upper boundary; non-CamelCase names pass through.
    let mut out = String::with_capacity(raw.len() + 4);
    let mut prev_lower = false;
    for ch in raw.chars() {
        if prev_lower && ch.is_ascii_uppercase() {
            out.push(' ');
        }
        prev_lower = ch.is_ascii_lowercase();
        out.push(ch);
    }
    out
}

fn fmt_credits(n: f64) -> String {
    if n.abs() >= 1_000.0 {
        format!("{:.1}k", n / 1_000.0)
    } else if (n.fract()).abs() < f64::EPSILON {
        format!("{}", n as i64)
    } else {
        format!("{n:.2}")
    }
}

fn countdown(reset: &str, now: DateTime<Utc>) -> Option<String> {
    let reset = DateTime::parse_from_rfc3339(reset).ok()?.with_timezone(&Utc);
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

    fn sample_auth(key: &str, refresh: &str, expires_at: &str) -> String {
        json!({
            "https://auth.x.ai::b1a00492-073a-47ea-816f-4c329264a828": {
                "key": key,
                "auth_mode": "oidc",
                "refresh_token": refresh,
                "expires_at": expires_at,
                "oidc_issuer": "https://auth.x.ai",
                "oidc_client_id": "b1a00492-073a-47ea-816f-4c329264a828"
            }
        })
        .to_string()
    }

    #[test]
    fn token_status_reads_auth_xai_entry() {
        let now = at("2026-08-16T20:00:00Z");
        let raw = sample_auth("tok", "ref", "2026-08-17T00:00:00Z");
        let ts = parse_token_status(&raw, now);
        assert!(ts.present);
        assert!(!ts.expired);
        assert!(ts.has_refresh);
    }

    #[test]
    fn token_status_expired_by_clock_with_skew() {
        let now = at("2026-08-17T00:00:30Z");
        let raw = sample_auth("tok", "ref", "2026-08-17T00:01:00Z");
        let ts = parse_token_status(&raw, now);
        assert!(ts.present);
        assert!(ts.expired, "within {EXPIRY_SKEW_SECS}s of expiry counts as expired");
        assert!(ts.has_refresh);
    }

    #[test]
    fn token_status_empty_key_is_signed_out() {
        let now = at("2026-08-16T20:00:00Z");
        let raw = sample_auth("", "ref", "2026-08-17T00:00:00Z");
        let ts = parse_token_status(&raw, now);
        assert!(!ts.present);
        assert!(!ts.expired);
    }

    #[test]
    fn token_status_unreadable_is_absent() {
        let ts = parse_token_status("not-json", at("2026-08-16T20:00:00Z"));
        assert!(!ts.present);
        assert!(!ts.has_refresh);
    }

    #[test]
    fn parse_expires_at_accepts_rfc3339_and_unix() {
        let rfc = json!({ "expires_at": "2026-08-17T00:01:20.127308500Z" });
        assert_eq!(
            parse_expires_at(&rfc).unwrap(),
            at("2026-08-17T00:01:20.127308500Z")
        );
        let unix = json!({ "expires_at": 1_786_924_880i64 });
        assert_eq!(
            parse_expires_at(&unix).unwrap(),
            DateTime::from_timestamp(1_786_924_880, 0).unwrap()
        );
    }

    #[test]
    fn selected_credential_prefers_auth_xai() {
        let root = json!({
            "https://idp.example.com::other": {
                "key": "other-tok",
                "refresh_token": "r",
                "oidc_issuer": "https://idp.example.com"
            },
            "https://auth.x.ai::b1a00492-073a-47ea-816f-4c329264a828": {
                "key": "xai-tok",
                "refresh_token": "r",
                "oidc_issuer": "https://auth.x.ai"
            }
        });
        let (k, v) = selected_credential(&root).unwrap();
        assert!(k.contains("auth.x.ai"));
        assert_eq!(v["key"], "xai-tok");
    }

    #[test]
    fn issuer_is_xai_matches_host_not_path() {
        assert!(issuer_is_xai("https://auth.x.ai", "https://auth.x.ai::abc"));
        assert!(issuer_is_xai("https://auth.x.ai/oauth2", ""));
        assert!(!issuer_is_xai("https://evil.example/auth.x.ai", "https://evil.example/auth.x.ai::abc"));
        assert!(!issuer_is_xai("https://idp.example.com", "https://idp.example.com::other"));
    }

    #[test]
    fn parse_empty_payload_is_unrecognized() {
        let s = parse(&json!({}), at("2026-08-16T18:00:00Z"));
        assert!(!s.ok);
        assert_eq!(s.error.as_deref(), Some("unrecognized billing shape"));
        let s2 = parse(&json!({ "config": { "isUnifiedBillingUser": true } }), at("2026-08-16T18:00:00Z"));
        assert!(!s2.ok);
    }

    #[test]
    fn parse_channel_version_accepts_semver() {
        assert_eq!(parse_channel_version("0.14.1\n").unwrap(), "0.14.1");
        assert_eq!(parse_channel_version("1.2.3-beta.1\r\n").unwrap(), "1.2.3-beta.1");
        assert!(parse_channel_version("latest").is_err());
        assert!(parse_channel_version("").is_err());
    }

    #[test]
    fn artifact_urls_windows_tries_exe_first() {
        let urls = artifact_urls(INSTALL_PRIMARY, "0.14.1", "windows-x86_64");
        assert_eq!(
            urls,
            vec![
                "https://x.ai/cli/grok-0.14.1-windows-x86_64.exe",
                "https://x.ai/cli/grok-0.14.1-windows-x86_64",
            ]
        );
        let mac = artifact_urls(INSTALL_PRIMARY, "0.14.1", "macos-aarch64");
        assert_eq!(mac, vec!["https://x.ai/cli/grok-0.14.1-macos-aarch64"]);
    }

    #[test]
    fn install_platform_is_known() {
        let p = install_platform().expect("host should be a supported Grok target");
        assert!(
            p.starts_with("macos-") || p.starts_with("windows-") || p.starts_with("linux-"),
            "{p}"
        );
    }

    #[test]
    fn refresh_merge_preserves_other_fields() {
        let now = at("2026-08-16T20:00:00Z");
        let existing = sample_auth("old", "oldR", "2026-08-16T18:00:00Z");
        let out = build_refreshed_credentials(&existing, now, "new", Some("newR"), 21_600).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        let creds = selected_credential(&v).unwrap().1;
        assert_eq!(creds["key"], "new");
        assert_eq!(creds["refresh_token"], "newR");
        assert_eq!(creds["oidc_issuer"], "https://auth.x.ai");
        assert_eq!(creds["expires_at"], "2026-08-17T02:00:00Z");
    }

    #[test]
    fn logout_tombstone_blanks_tokens() {
        let existing = sample_auth("tok", "ref", "2026-08-17T00:00:00Z");
        let out = build_logout_tombstone(&existing).unwrap();
        let ts = parse_token_status(&out, at("2026-08-16T20:00:00Z"));
        assert!(!ts.present);
        assert!(!ts.has_refresh);
    }

    #[test]
    fn parse_supergrok_weekly_has_no_invented_percent() {
        let now = at("2026-08-16T18:00:00Z");
        let v = json!({
            "config": {
                "currentPeriod": {
                    "type": "USAGE_PERIOD_TYPE_WEEKLY",
                    "start": "2026-08-16T17:45:09.558527+00:00",
                    "end": "2026-08-23T17:45:09.558527+00:00"
                },
                "onDemandCap": { "val": 0 },
                "onDemandUsed": { "val": 0 },
                "isUnifiedBillingUser": true,
                "prepaidBalance": { "val": 0 },
                "billingPeriodStart": "2026-08-16T17:45:09.558527+00:00",
                "billingPeriodEnd": "2026-08-23T17:45:09.558527+00:00"
            }
        });
        let s = parse(&v, now);
        assert!(s.ok && s.configured);
        assert_eq!(s.primary, "Grok Build");
        assert_eq!(s.secondary, "resets in 6d 23h");
        assert!(s.detail.iter().any(|d| d.label == "Weekly" && d.pct.is_none()));
        assert!(s.detail.iter().any(|d| d.label == "Plan" && d.value == "Grok Build"));
        assert!(
            s.detail.iter().all(|d| d.label != "On-demand"),
            "zero on-demand cap must not render a meter"
        );
    }

    #[test]
    fn parse_credit_percent_is_a_meter() {
        let now = at("2026-08-16T18:00:00Z");
        let v = json!({
            "config": {
                "currentPeriod": {
                    "type": "USAGE_PERIOD_TYPE_WEEKLY",
                    "end": "2026-08-17T18:00:00Z"
                },
                "creditUsagePercent": 42.4,
                "subscriptionTier": "SUPERGROK"
            }
        });
        let s = parse(&v, now);
        assert_eq!(s.primary, "42% used");
        let weekly = s.detail.iter().find(|d| d.label == "Weekly").unwrap();
        assert_eq!(weekly.pct, Some(42.4));
        assert_eq!(weekly.status, Some("ok"));
        assert!(s.detail.iter().any(|d| d.label == "Plan" && d.value == "SuperGrok"));
    }

    #[test]
    fn parse_product_usage_adds_per_product_meters() {
        let now = at("2026-08-16T18:00:00Z");
        let v = json!({
            "config": {
                "currentPeriod": {
                    "type": "USAGE_PERIOD_TYPE_WEEKLY",
                    "end": "2026-08-17T18:00:00Z"
                },
                "creditUsagePercent": 17,
                "productUsage": [
                    { "product": "GrokBuild", "usagePercent": 16 },
                    { "product": "GrokImagine", "usagePercent": 1 },
                    { "product": "GrokChat" }
                ],
                "isUnifiedBillingUser": true
            }
        });
        let s = parse(&v, now);
        assert!(s
            .detail
            .iter()
            .any(|d| d.label == "Weekly" && d.pct == Some(17.0)));
        let build = s
            .detail
            .iter()
            .find(|d| d.label == "Grok Build")
            .expect("Grok Build meter");
        assert_eq!(build.pct, Some(16.0));
        assert_eq!(build.value, "resets in 1d 0h", "products share the window's reset");
        assert!(s
            .detail
            .iter()
            .any(|d| d.label == "Grok Imagine" && d.pct == Some(1.0)));
        assert!(
            !s.detail.iter().any(|d| d.label == "Grok Chat"),
            "products without a usagePercent must not render a meter"
        );
    }

    #[test]
    fn parse_monthly_limit_is_a_meter() {
        let now = at("2026-08-16T18:00:00Z");
        let v = json!({
            "config": {
                "monthlyLimit": { "val": 100 },
                "used": { "val": 80 },
                "billingPeriodEnd": "2026-09-01T00:00:00+00:00"
            }
        });
        let s = parse(&v, now);
        assert_eq!(s.primary, "80% used");
        let monthly = s.detail.iter().find(|d| d.label == "Monthly").unwrap();
        assert_eq!(monthly.pct, Some(80.0));
        assert_eq!(monthly.status, Some("warn"));
    }

    #[test]
    fn parse_on_demand_and_prepaid_rows() {
        let now = at("2026-08-16T18:00:00Z");
        let v = json!({
            "config": {
                "isUnifiedBillingUser": true,
                "onDemandCap": { "val": 50 },
                "onDemandUsed": { "val": 10 },
                "prepaidBalance": { "val": 12.5 }
            }
        });
        let s = parse(&v, now);
        let od = s.detail.iter().find(|d| d.label == "On-demand").unwrap();
        assert_eq!(od.pct, Some(20.0));
        assert!(s.detail.iter().any(|d| d.label == "Prepaid" && d.value == "12.50"));
    }

    #[test]
    fn humanize_period_and_tier() {
        assert_eq!(humanize_period("USAGE_PERIOD_TYPE_WEEKLY"), "Weekly");
        assert_eq!(humanize_period("USAGE_PERIOD_TYPE_CUSTOM_WINDOW"), "Custom Window");
        assert_eq!(humanize_tier("SUPERGROK"), "SuperGrok");
        assert_eq!(humanize_tier("PREMIUM_PLUS"), "Premium+");
    }

    #[test]
    fn countdown_formats_like_kimi() {
        let now = at("2026-08-16T18:00:00Z");
        assert_eq!(countdown("2026-08-16T18:23:00Z", now).as_deref(), Some("23m"));
        assert_eq!(countdown("2026-08-16T20:12:00Z", now).as_deref(), Some("2h 12m"));
        assert_eq!(countdown("2026-08-18T21:00:00Z", now).as_deref(), Some("2d 3h"));
        assert!(countdown("2026-08-16T17:00:00Z", now).is_none());
    }
}
