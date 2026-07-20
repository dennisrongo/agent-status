//! GitHub Copilot LIVE usage — the same premium-request quota the editor's
//! Copilot status panel shows. Reads the user's own Copilot/GitHub OAuth token
//! from where an editor or CLI already stored it, and calls
//! `GET https://api.github.com/copilot_internal/user`.
//!
//! This is an UNDOCUMENTED endpoint (same risk class as the Anthropic
//! `/api/oauth/usage` call this app already uses), driven by the user's own
//! locally-stored token, at the user's request. No token exchange is needed —
//! the editor OAuth token is sent directly. If no token is found we degrade to
//! `not_configured`; any fetch/parse failure degrades to `failed`, never panic.
//!
//! Response carries `quota_snapshots.premium_interactions` with
//! `entitlement` / `remaining` / `percent_remaining` / `unlimited`, plus
//! top-level `copilot_plan` and `quota_reset_date[_utc]`. Org/enterprise Business
//! and Enterprise seats are frequently `unlimited`, so the parser handles both a
//! finite quota (draw a real % meter) and an unlimited plan (render "unlimited").

use serde_json::Value;
use std::time::Duration;

use crate::process_util::SilentCommand;
use super::{short_date, KeyVal, VendorStatus};

const ENDPOINT: &str = "https://api.github.com/copilot_internal/user";

// Editor-identity headers. `/copilot_internal/*` rejects callers that don't look
// like a real editor integration (HTTP 403 "not authorized for this
// integration"), so we mirror the VS Code Copilot Chat client.
const EDITOR_VERSION: &str = "vscode/1.107.0";
const PLUGIN_VERSION: &str = "copilot-chat/0.35.0";
const INTEGRATION_ID: &str = "vscode-chat";
const USER_AGENT: &str = "GitHubCopilotChat/0.35.0";
const GH_API_VERSION: &str = "2025-04-01";

/// Classified result of a live Copilot fetch. Lets the caller hold the last good
/// reading through a transient blip instead of flipping the card to a "connect"
/// prompt — the same resilience editor Copilot clients use for cached quota.
pub enum FetchOutcome {
    /// A usable reading.
    Ok(VendorStatus),
    /// Genuinely unusable / needs user action: no token, or a durable
    /// auth/entitlement rejection (401 / 403 / 404 / other 4xx). Show the
    /// connect-or-reconnect card and drop any cached reading.
    Terminal(VendorStatus),
    /// A transient failure (network, timeout, HTTP 429, 5xx, or an unexpected
    /// 200 shape) — the caller should keep showing the last good reading.
    Transient(String),
}

/// Whether a non-success HTTP status is transient (retry, keep the cached
/// reading) rather than a durable auth/entitlement error (show the connect
/// card). 429 (rate-limited) and 5xx are transient; other 4xx are terminal.
fn http_is_transient(code: u16) -> bool {
    code == 429 || code >= 500
}

/// Fetch live Copilot usage. `stored_token` is a token the user connected via the
/// in-app device flow (tried first); otherwise we use a token an editor/CLI left
/// on disk, discovered once and cached for the process lifetime.
pub async fn fetch(stored_token: Option<String>) -> FetchOutcome {
    let stored = stored_token.filter(|t| !t.trim().is_empty());
    // Track whether the token came from auto-discovery so a 401 can invalidate
    // the cache (e.g. the `gh` CLI rotated its token) without disturbing a
    // user-connected token.
    let from_discovery = stored.is_none();
    let token = match stored {
        Some(t) => t,
        None => match discovered_token().await {
            Some(t) => t,
            None => return FetchOutcome::Terminal(VendorStatus::not_configured()),
        },
    };

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(12))
        .build()
    {
        Ok(c) => c,
        Err(e) => return FetchOutcome::Transient(format!("client init: {e}")),
    };

    let resp = client
        .get(ENDPOINT)
        // Canonical scheme for this endpoint is `token <tok>` (Bearer also works).
        .header("Authorization", format!("token {token}"))
        .header("Accept", "application/json")
        .header("Editor-Version", EDITOR_VERSION)
        .header("Editor-Plugin-Version", PLUGIN_VERSION)
        .header("Copilot-Integration-Id", INTEGRATION_ID)
        .header("User-Agent", USER_AGENT)
        .header("X-GitHub-Api-Version", GH_API_VERSION)
        .send()
        .await;

    match resp {
        Ok(r) => {
            let status = r.status();
            if !status.is_success() {
                let code = status.as_u16();
                // A discovered token that's now rejected was likely rotated by
                // its owner (gh CLI); drop it so the next refresh re-discovers.
                if code == 401 && from_discovery {
                    invalidate_discovered();
                }
                let hint = match code {
                    401 => " (Copilot token expired or revoked — reconnect)",
                    403 => " (token not authorized for Copilot)",
                    404 => " (no Copilot subscription on this account)",
                    429 => " (rate limited — will retry)",
                    _ => "",
                };
                let msg = format!("HTTP {code}{hint}");
                return if http_is_transient(code) {
                    FetchOutcome::Transient(msg)
                } else {
                    FetchOutcome::Terminal(VendorStatus::failed(msg))
                };
            }
            match r.json::<Value>().await {
                Ok(v) => {
                    let parsed = parse(&v);
                    if parsed.ok {
                        FetchOutcome::Ok(parsed)
                    } else {
                        // 200 with an unexpected shape — treat as transient so a
                        // one-off API hiccup doesn't blank a good cached reading.
                        FetchOutcome::Transient(
                            parsed.error.unwrap_or_else(|| "unexpected response".to_string()),
                        )
                    }
                }
                Err(e) => FetchOutcome::Transient(format!("invalid JSON: {e}")),
            }
        }
        Err(e) => FetchOutcome::Transient(format!("request error: {e}")),
    }
}

/// Pure parser for the `/copilot_internal/user` response. Maps the
/// premium-interactions snapshot to the shared `VendorStatus`.
pub fn parse(v: &Value) -> VendorStatus {
    let plan = v
        .get("copilot_plan")
        .and_then(|p| p.as_str())
        .map(titleize_plan)
        .unwrap_or_else(|| "Copilot".to_string());
    let reset = v
        .get("quota_reset_date_utc")
        .and_then(|d| d.as_str())
        .or_else(|| v.get("quota_reset_date").and_then(|d| d.as_str()))
        .map(short_date);

    let premium = v
        .get("quota_snapshots")
        .and_then(|q| q.get("premium_interactions"));

    let Some(snap) = premium else {
        return shape_error("no premium-request quota in response");
    };

    let unlimited = snap.get("unlimited").and_then(|u| u.as_bool()).unwrap_or(false);

    let mut detail = vec![KeyVal::text("Plan", plan.clone())];
    if let Some(org) = seat_org(v) {
        detail.push(KeyVal::text("Organization", org));
    }
    if let Some(since) = member_since(v) {
        detail.push(KeyVal::text("Member since", since));
    }

    if unlimited {
        // Business/Enterprise seats are typically unlimited across every quota
        // category. Show each one so the card reads as a real account summary
        // rather than a bare "unlimited" — and so the rows light up with real
        // numbers automatically if the org ever switches to metered billing.
        let snapshots = v.get("quota_snapshots");
        for (key, label) in [("chat", "Chat"), ("completions", "Completions")] {
            if snapshots.and_then(|q| q.get(key)).is_some() {
                detail.push(KeyVal::text(label, "unlimited"));
            }
        }
        detail.push(KeyVal::text("Premium requests", "unlimited"));
        if let Some(caps) = capabilities(v) {
            detail.push(KeyVal::text("Capabilities", caps));
        }
        if let Some(r) = &reset {
            detail.push(KeyVal::text("Resets", r.clone()));
        }
        // "seat" is tied to org presence — individual plans have no org, so we
        // keep the legacy phrasing for them.
        let secondary = match seat_org(v) {
            Some(org) => format!("{plan} seat · {org}"),
            None => format!("{plan} · premium requests"),
        };
        return VendorStatus {
            configured: true,
            ok: true,
            error: None,
            primary: "unlimited".to_string(),
            secondary,
            detail,
            auth_expired: false,
        };
    }

    let entitlement = snap.get("entitlement").and_then(value_as_f64);
    let remaining = snap.get("remaining").and_then(value_as_f64);
    let pct_remaining = snap
        .get("percent_remaining")
        .and_then(value_as_f64)
        .or_else(|| match (entitlement, remaining) {
            (Some(e), Some(r)) if e > 0.0 => Some(r / e * 100.0),
            _ => None,
        })
        // Reject NaN/inf so a hostile/garbled payload can't render "NaN% used".
        .filter(|p| p.is_finite());

    let Some(pct_left) = pct_remaining else {
        return shape_error("premium quota present but no percentage");
    };
    let used_pct = (100.0 - pct_left).clamp(0.0, 100.0);

    let secondary = match (entitlement, remaining, &reset) {
        (Some(e), Some(r), Some(d)) => {
            format!("{} of {} left · resets {d}", fmt_count(r), fmt_count(e))
        }
        (Some(e), Some(r), None) => format!("{} of {} left", fmt_count(r), fmt_count(e)),
        _ => format!("{plan} premium requests"),
    };

    if let (Some(e), Some(r)) = (entitlement, remaining) {
        // Saturate at 0 so an over-quota reading (remaining > entitlement) can't
        // show a negative "used".
        let used = (e - r).max(0.0);
        // A quota meter (used %, status-colored bar) so the popover matches
        // Claude's and GLM's rows; the count rides in the row's faint slot.
        detail.push(KeyVal::meter(
            "Premium requests",
            format!("{} / {}", fmt_count(used), fmt_count(e)),
            used_pct,
        ));
    }
    if let Some(r) = &reset {
        detail.push(KeyVal::text("Resets", r.clone()));
    }
    if snap
        .get("overage_permitted")
        .and_then(|o| o.as_bool())
        .unwrap_or(false)
    {
        if let Some(o) = snap.get("overage_count").and_then(value_as_f64) {
            detail.push(KeyVal::text("Overage", fmt_count(o)));
        }
    }

    VendorStatus {
        configured: true,
        ok: true,
        error: None,
        primary: format!("{:.0}% used", used_pct),
        secondary,
        detail,
        auth_expired: false,
    }
}

// ---------- Local token discovery ----------

/// Process-lifetime cache of the auto-discovered token. Discovery shells out to
/// `gh` and reads files, so we do it at most once (per process, or until a 401
/// invalidates it) instead of on every refresh/tray-hover.
static DISCOVERED_TOKEN: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// The auto-discovered token, cached. Runs the blocking discovery off the async
/// runtime so it can't stall a Tokio worker (or wedge other Tauri commands)
/// while `gh` cold-starts.
async fn discovered_token() -> Option<String> {
    if let Ok(guard) = DISCOVERED_TOKEN.lock() {
        if guard.is_some() {
            return guard.clone();
        }
    }
    let found = tokio::task::spawn_blocking(read_token).await.ok().flatten();
    if let (Some(t), Ok(mut guard)) = (&found, DISCOVERED_TOKEN.lock()) {
        *guard = Some(t.clone());
    }
    found
}

/// Drop the cached discovered token so the next fetch re-discovers (used when a
/// previously-good discovered token starts returning 401).
fn invalidate_discovered() {
    if let Ok(mut guard) = DISCOVERED_TOKEN.lock() {
        *guard = None;
    }
}

/// Find a usable Copilot/GitHub OAuth token from the stores an editor or CLI
/// already populated, first hit wins:
///   1. the GitHub Copilot editor token file (`~/.config/github-copilot/…` on
///      macOS/Linux, `%LOCALAPPDATA%\github-copilot\…` on Windows),
///   2. the `gh` CLI (`gh auth token`).
///
/// Blocking (file + process I/O); callers run it via `spawn_blocking`. We do NOT
/// probe the macOS keychain: `security find-internet-password -s github.com`
/// pops an interactive auth dialog on every call when the item's ACL doesn't
/// list us, and returns the first matching `github.com` secret — frequently a
/// git credential-helper PAT that 401s against `/copilot_internal/user`. Users
/// without an editor/CLI token use the explicit in-app device-flow connect.
pub fn read_token() -> Option<String> {
    editor_file_token().or_else(gh_cli_token)
}

/// Read `hosts.json` (legacy) then `apps.json` (current) from the
/// `github-copilot` config dir and pull the nested `oauth_token`.
fn editor_file_token() -> Option<String> {
    let dir = copilot_config_dir()?;
    for name in ["hosts.json", "apps.json"] {
        if let Ok(raw) = std::fs::read_to_string(dir.join(name)) {
            if let Some(tok) = parse_token_json(&raw) {
                return Some(tok);
            }
        }
    }
    None
}

/// The `github-copilot` config directory. macOS uses XDG-style `~/.config`
/// (NOT `~/Library/Application Support`), so we resolve it explicitly rather
/// than via `dirs::config_dir()`.
fn copilot_config_dir() -> Option<std::path::PathBuf> {
    #[cfg(windows)]
    {
        dirs::data_local_dir().map(|d| d.join("github-copilot"))
    }
    #[cfg(not(windows))]
    {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(std::path::PathBuf::from)
            .filter(|p| !p.as_os_str().is_empty())
            .or_else(|| dirs::home_dir().map(|h| h.join(".config")))?;
        Some(base.join("github-copilot"))
    }
}

/// Extract the `oauth_token` from a Copilot token file. `hosts.json` keys are a
/// plain host (`"github.com"`); `apps.json` keys are composite
/// (`"github.com:Iv1.<clientId>"`) and the clientId varies, so we match any key
/// that starts with a GitHub host rather than hardcoding it.
pub fn parse_token_json(raw: &str) -> Option<String> {
    let v: Value = serde_json::from_str(raw.trim()).ok()?;
    let obj = v.as_object()?;
    obj.iter()
        .filter(|(k, _)| is_github_host(k))
        .find_map(|(_, val)| {
            val.get("oauth_token")
                .and_then(|t| t.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
        })
}

/// Whether a Copilot token-file key names a GitHub host. Keys are either a bare
/// host (`github.com`) or `host:clientId`. Enterprise seats use any `*.ghe.com`
/// (e.g. `mycorp.ghe.com`) or a GHEC subdomain of `github.com`, so we match on
/// the host suffix rather than an exact `github.com`/`ghe.com`.
fn is_github_host(key: &str) -> bool {
    let host = key.split(':').next().unwrap_or(key);
    host == "github.com"
        || host == "ghe.com"
        || host.ends_with(".github.com")
        || host.ends_with(".ghe.com")
}

/// `gh auth token` — works on macOS and Windows when the GitHub CLI is signed
/// in, regardless of whether it stored the token in a file or the OS keyring.
fn gh_cli_token() -> Option<String> {
    let out = std::process::Command::new("gh")
        .args(["auth", "token"])
        .silent()
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| s.starts_with("gh") && s.len() > 8)
}

// ---------- Device flow (fallback when no local token is found) ----------
//
// GitHub OAuth device flow with the well-known VS Code Copilot client id. The
// user authorizes once in a browser; the minted token is stored encrypted and
// preferred by `fetch`. Mirrors the editor's own sign-in, so the resulting token
// is accepted by `/copilot_internal/user`.

const CLIENT_ID: &str = "Iv1.b507a08c87ecfe98";
const DEVICE_CODE_URL: &str = "https://github.com/login/device/code";
const TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
const SCOPE: &str = "read:user";

/// Result of starting the device flow.
pub struct DeviceStart {
    pub device_code: String,
    pub user_code: String,
    /// Bare verification page (`…/login/device`) — shown to the user as text.
    pub verification_uri: String,
    /// Same page with the user code pre-filled (`…?user_code=ABCD-1234`) — what
    /// we actually open in the browser so the user needn't retype the code.
    /// Falls back to `verification_uri` when GitHub omits it.
    pub verification_uri_complete: String,
    pub interval: u64,
    pub expires_in: u64,
}

/// Outcome of a single poll for the access token.
pub enum PollOutcome {
    /// Still waiting for the user to authorize.
    Pending,
    /// Polling too fast — back off by adding 5s to the interval, per the OAuth
    /// device-flow spec. Distinct from `Pending` so the caller can bump it.
    SlowDown,
    /// Authorized — here is the access token.
    Connected(String),
    /// The user denied the request.
    Denied,
    /// The device code expired before authorization.
    Expired,
}

/// Begin the device flow: request a device + user code from GitHub.
pub async fn device_start() -> Result<DeviceStart, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| format!("client init: {e}"))?;
    let resp = client
        .post(DEVICE_CODE_URL)
        .header("Accept", "application/json")
        .header("User-Agent", USER_AGENT)
        .form(&[("client_id", CLIENT_ID), ("scope", SCOPE)])
        .send()
        .await
        .map_err(|e| format!("request error: {e}"))?;
    let status = resp.status();
    let v: Value = resp
        .json()
        .await
        .map_err(|e| format!("invalid JSON: {e}"))?;
    if !status.is_success() {
        let msg = v
            .get("error_description")
            .and_then(|x| x.as_str())
            .unwrap_or("device code request failed");
        return Err(msg.to_string());
    }
    let verification_uri = v
        .get("verification_uri")
        .and_then(|x| x.as_str())
        .unwrap_or("https://github.com/login/device")
        .to_string();
    Ok(DeviceStart {
        device_code: v.get("device_code").and_then(|x| x.as_str()).ok_or("no device_code")?.to_string(),
        user_code: v.get("user_code").and_then(|x| x.as_str()).ok_or("no user_code")?.to_string(),
        verification_uri_complete: v
            .get("verification_uri_complete")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| verification_uri.clone()),
        verification_uri,
        interval: v.get("interval").and_then(|x| x.as_u64()).unwrap_or(5),
        expires_in: v.get("expires_in").and_then(|x| x.as_u64()).unwrap_or(900),
    })
}

/// Poll once for the access token using the device code.
///
/// Transient failures (client build, network blip, captive portal, partial
/// body) map to `Pending` — a single failed poll in a ~15-minute flow must not
/// kill the connect; the caller just polls again next tick (and the device
/// code's own expiry still bounds the flow). Only a genuine terminal OAuth
/// protocol error surfaces as `Err`.
pub async fn device_poll(device_code: &str) -> Result<PollOutcome, String> {
    let client = match reqwest::Client::builder().timeout(Duration::from_secs(15)).build() {
        Ok(c) => c,
        Err(_) => return Ok(PollOutcome::Pending),
    };
    let resp = match client
        .post(TOKEN_URL)
        .header("Accept", "application/json")
        .header("User-Agent", USER_AGENT)
        .form(&[
            ("client_id", CLIENT_ID),
            ("device_code", device_code),
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
        ])
        .send()
        .await
    {
        Ok(r) => r,
        Err(_) => return Ok(PollOutcome::Pending),
    };
    match resp.json::<Value>().await {
        Ok(v) => parse_poll(&v),
        Err(_) => Ok(PollOutcome::Pending),
    }
}

/// Pure classification of a device-flow token-endpoint response, split out so it
/// can be unit-tested without a live call.
pub fn parse_poll(v: &Value) -> Result<PollOutcome, String> {
    if let Some(tok) = v
        .get("access_token")
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
    {
        return Ok(PollOutcome::Connected(tok.to_string()));
    }
    match v.get("error").and_then(|x| x.as_str()).unwrap_or("") {
        "authorization_pending" => Ok(PollOutcome::Pending),
        // The spec requires the client to slow down (add 5s to its interval);
        // surfaced distinctly so the caller actually does so.
        "slow_down" => Ok(PollOutcome::SlowDown),
        "access_denied" => Ok(PollOutcome::Denied),
        "expired_token" => Ok(PollOutcome::Expired),
        other if !other.is_empty() => Err(other.to_string()),
        _ => Ok(PollOutcome::Pending),
    }
}

// ---------- helpers ----------

fn shape_error(msg: &str) -> VendorStatus {
    VendorStatus {
        configured: true,
        ok: false,
        error: Some(msg.to_string()),
        primary: "—".to_string(),
        secondary: "unexpected shape".to_string(),
        detail: Vec::new(),
        auth_expired: false,
    }
}

fn value_as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.parse::<f64>().ok(),
        _ => None,
    }
}

/// `individual_pro` -> `Individual Pro`, `business` -> `Business`.
fn titleize_plan(s: &str) -> String {
    s.split(['_', ' ', '-'])
        .filter(|w| !w.is_empty())
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

fn fmt_count(n: f64) -> String {
    // Non-finite input (e.g. a `"NaN"` string in the payload) → em dash rather
    // than a misleading "0".
    if !n.is_finite() {
        return "—".to_string();
    }
    // Intentional: 1K–9.9K keeps one decimal (premium-request counts are small,
    // so "1.5K" reads better than "2K"), but 10K+ drops it ("999K", not
    // "999.0K"). The 1e4-before-1e3 ordering is what produces that — don't
    // "tidy" the branches into a single threshold.
    if n >= 1e6 {
        format!("{:.1}M", n / 1e6)
    } else if n >= 1e4 {
        format!("{:.0}K", n / 1e3)
    } else if n >= 1e3 {
        format!("{:.1}K", n / 1e3)
    } else {
        format!("{}", n.round() as u64)
    }
}

/// The seat-providing org's canonical handle, e.g. `"CodeStackSJ"`. Prefers the
/// login (always present, recognizable) over the display name (frequently
/// unset), and reads the richer `organization_list` before the bare
/// `organization_login_list` fallback. `None` for individual plans.
fn seat_org(v: &Value) -> Option<String> {
    let by_login = |o: &Value| o.get("login").and_then(|s| s.as_str()).map(str::to_string);
    let by_name = |o: &Value| o.get("name").and_then(|s| s.as_str()).map(str::to_string);

    if let Some(orgs) = v.get("organization_list").and_then(|o| o.as_array()) {
        if let Some(first) = orgs.first() {
            // Login is canonical; display name is a last resort (often empty).
            return by_login(first).or_else(|| by_name(first));
        }
    }
    v.get("organization_login_list")
        .and_then(|l| l.as_array())
        .and_then(|a| a.first())
        .and_then(|s| s.as_str())
        .map(str::to_string)
}

/// The seat assignment date as `YYYY-MM-DD`, e.g. `assigned_date`
/// `"2023-09-07T13:15:02-07:00"` -> `"2023-09-07"`. `None` when the field is
/// absent (older responses).
fn member_since(v: &Value) -> Option<String> {
    v.get("assigned_date")
        .and_then(|d| d.as_str())
        .map(short_date)
}

/// Enabled add-on capabilities joined with `" · "`, e.g. `"MCP · CLI · Copilot
/// app"`. Returns `None` when nothing is enabled so the row is omitted rather
/// than rendered empty. The join glyph mirrors the rest of the card
/// (`"{plan} · premium requests"`).
fn capabilities(v: &Value) -> Option<String> {
    let flag = |k: &str| v.get(k).and_then(|b| b.as_bool()).unwrap_or(false);
    let mut caps: Vec<&str> = Vec::new();
    if flag("is_mcp_enabled") {
        caps.push("MCP");
    }
    if flag("cli_enabled") {
        caps.push("CLI");
    }
    if flag("copilot_app_enabled") {
        caps.push("Copilot app");
    }
    if caps.is_empty() {
        None
    } else {
        Some(caps.join(" · "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn http_status_classification() {
        // Transient: rate-limit + server errors → keep the cached reading.
        for c in [429, 500, 502, 503, 504] {
            assert!(http_is_transient(c), "HTTP {c} should be transient");
        }
        // Terminal: durable auth/entitlement errors → show the connect card.
        for c in [400, 401, 403, 404, 422] {
            assert!(!http_is_transient(c), "HTTP {c} should be terminal");
        }
    }

    #[test]
    fn parses_finite_premium_quota() {
        let v = json!({
            "copilot_plan": "individual_pro",
            "quota_reset_date": "2026-07-01",
            "quota_snapshots": {
                "premium_interactions": {
                    "entitlement": 1500, "remaining": 1327,
                    "percent_remaining": 88.5, "unlimited": false
                },
                "chat": { "unlimited": true }
            }
        });
        let s = parse(&v);
        assert!(s.ok);
        // 100 - 88.5 = 11.5 -> "12% used"
        assert_eq!(s.primary, "12% used");
        assert!(s.secondary.contains("resets 2026-07-01"));
        assert!(s.secondary.contains("of"));
        // Plan + used + resets rows.
        assert!(s.detail.iter().any(|d| d.label == "Plan" && d.value == "Individual Pro"));
        // Premium-requests row is now a meter (carries pct + status for the bar).
        assert!(s
            .detail
            .iter()
            .any(|d| d.label == "Premium requests" && d.pct == Some(11.5) && d.status == Some("ok")));
    }

    #[test]
    fn parses_unlimited_business_plan() {
        // Mirrors the live shape observed for a Business seat: unlimited quota
        // across every category, plus the org / member-since / capability
        // metadata the enriched card surfaces.
        let v = json!({
            "copilot_plan": "business",
            "access_type_sku": "copilot_for_business_seat_quota",
            "assigned_date": "2023-09-07T13:15:02-07:00",
            "organization_login_list": ["CodeStackSJ"],
            "organization_list": [{ "login": "CodeStackSJ", "name": "CodeStack" }],
            "is_mcp_enabled": false,
            "cli_enabled": true,
            "copilot_app_enabled": true,
            "quota_reset_date_utc": "2026-07-01T00:00:00Z",
            "quota_snapshots": {
                "chat": { "unlimited": true, "credits_used": 0 },
                "completions": { "unlimited": true, "credits_used": 0 },
                "premium_interactions": {
                    "entitlement": 0, "remaining": 0,
                    "percent_remaining": 100.0, "unlimited": true,
                    "overage_permitted": true, "overage_count": 0
                }
            }
        });
        let s = parse(&v);
        assert!(s.ok);
        assert_eq!(s.primary, "unlimited");
        assert_eq!(s.secondary, "Business seat · CodeStackSJ");
        // Plan + org + member-since + three quota rows + caps + resets.
        assert!(s.detail.iter().any(|d| d.label == "Plan" && d.value == "Business"));
        assert!(s.detail.iter().any(|d| d.label == "Organization" && d.value == "CodeStackSJ"));
        assert!(s.detail.iter().any(|d| d.label == "Member since" && d.value == "2023-09-07"));
        // Every quota category renders as an unlimited row.
        assert_eq!(
            s.detail.iter().filter(|d| d.value == "unlimited").count(),
            3,
            "chat, completions, and premium requests should each be unlimited"
        );
        assert!(s.detail.iter().any(|d| d.label == "Chat" && d.value == "unlimited"));
        assert!(s.detail.iter().any(|d| d.label == "Completions" && d.value == "unlimited"));
        assert!(s.detail.iter().any(|d| d.label == "Premium requests" && d.value == "unlimited"));
        // MCP is off, so it's omitted from the caps row.
        assert!(s.detail.iter().any(|d| d.label == "Capabilities" && d.value == "CLI · Copilot app"));
        assert!(s.detail.iter().any(|d| d.label == "Resets" && d.value == "2026-07-01"));
    }

    #[test]
    fn unlimited_omits_optional_rows_when_absent() {
        // A minimal response (no org, no assigned_date, no chat/completions, no
        // caps) must render exactly the legacy rows and fall back to the legacy
        // secondary — backward compat for responses that predate the richer
        // fields, or for individual Pro seats with nothing extra.
        let v = json!({
            "copilot_plan": "individual",
            "quota_snapshots": {
                "premium_interactions": { "unlimited": true, "percent_remaining": 100.0 }
            }
        });
        let s = parse(&v);
        assert!(s.ok);
        assert_eq!(s.primary, "unlimited");
        assert_eq!(s.secondary, "Individual · premium requests");
        // Only Plan + Premium requests; no org/member-since/caps/chat/completions.
        assert!(s.detail.iter().all(|d| d.label == "Plan" || d.label == "Premium requests"));
        assert_eq!(s.detail.iter().filter(|d| d.value == "unlimited").count(), 1);
    }

    #[test]
    fn unlimited_lists_all_capabilities() {
        let v = json!({
            "copilot_plan": "enterprise",
            "organization_list": [{ "login": "mycorp" }],
            "is_mcp_enabled": true,
            "cli_enabled": true,
            "copilot_app_enabled": true,
            "quota_snapshots": { "premium_interactions": { "unlimited": true } }
        });
        let s = parse(&v);
        assert!(s.ok);
        assert!(s
            .detail
            .iter()
            .any(|d| d.label == "Capabilities" && d.value == "MCP · CLI · Copilot app"));
    }

    #[test]
    fn finite_includes_org_row() {
        // A finite Pro seat with an org should show the org alongside the meter.
        let v = json!({
            "copilot_plan": "individual_pro",
            "assigned_date": "2024-01-15T00:00:00Z",
            "organization_login_list": ["acme"],
            "quota_snapshots": { "premium_interactions": {
                "entitlement": 1500, "remaining": 1327,
                "percent_remaining": 88.5, "unlimited": false
            }}
        });
        let s = parse(&v);
        assert!(s.ok);
        assert!(s.detail.iter().any(|d| d.label == "Organization" && d.value == "acme"));
        assert!(s.detail.iter().any(|d| d.label == "Member since" && d.value == "2024-01-15"));
        assert!(s
            .detail
            .iter()
            .any(|d| d.label == "Premium requests" && d.pct == Some(11.5)));
    }

    #[test]
    fn computes_percent_when_absent() {
        let v = json!({
            "copilot_plan": "individual",
            "quota_snapshots": { "premium_interactions": {
                "entitlement": 300, "remaining": 75, "unlimited": false
            }}
        });
        let s = parse(&v);
        assert!(s.ok);
        // 75/300 = 25% left -> 75% used
        assert_eq!(s.primary, "75% used");
    }

    #[test]
    fn missing_premium_snapshot_is_not_ok() {
        let v = json!({ "copilot_plan": "individual", "quota_snapshots": { "chat": { "unlimited": true } } });
        let s = parse(&v);
        assert!(!s.ok);
        assert!(s.error.is_some());
    }

    #[test]
    fn non_finite_percentage_is_rejected() {
        // A hostile/garbled `"NaN"` must not render as "NaN% used".
        let v = json!({ "copilot_plan": "individual", "quota_snapshots": { "premium_interactions": {
            "entitlement": 0, "remaining": 0, "percent_remaining": "NaN", "unlimited": false
        }}});
        let s = parse(&v);
        assert!(!s.ok, "non-finite percentage must not produce a usage figure");
    }

    #[test]
    fn over_quota_used_saturates_to_zero() {
        // remaining > entitlement (overage) → "used" floors at 0, never negative.
        let v = json!({ "copilot_plan": "business", "quota_snapshots": { "premium_interactions": {
            "entitlement": 1500, "remaining": 1600, "percent_remaining": 106.0, "unlimited": false
        }}});
        let s = parse(&v);
        assert!(s.ok);
        assert_eq!(s.primary, "0% used");
        assert!(s
            .detail
            .iter()
            .any(|d| d.label == "Premium requests" && d.value == "0 / 1.5K" && d.pct == Some(0.0)));
    }

    #[test]
    fn token_from_legacy_hosts_json() {
        let raw = r#"{ "github.com": { "user": "octocat", "oauth_token": "gho_abcdefghijklmnop" } }"#;
        assert_eq!(parse_token_json(raw).as_deref(), Some("gho_abcdefghijklmnop"));
    }

    #[test]
    fn token_from_composite_apps_json_key() {
        let raw = r#"{ "github.com:Iv1.b507a08c87ecfe98": { "user": "octocat", "oauth_token": "ghu_zyxwvutsrqponml", "githubAppId": "Iv1.b507a08c87ecfe98" } }"#;
        assert_eq!(parse_token_json(raw).as_deref(), Some("ghu_zyxwvutsrqponml"));
    }

    #[test]
    fn token_parse_rejects_unrelated_or_empty() {
        assert!(parse_token_json(r#"{ "gitlab.com": { "oauth_token": "x" } }"#).is_none());
        assert!(parse_token_json(r#"{ "github.com": { "oauth_token": "" } }"#).is_none());
        assert!(parse_token_json("not json").is_none());
    }

    #[test]
    fn token_from_enterprise_ghe_host() {
        // GHEC data-residency / Enterprise hosts key on the full host.
        let raw = r#"{ "mycorp.ghe.com:Iv1.abc": { "oauth_token": "ghu_enterprise1234" } }"#;
        assert_eq!(parse_token_json(raw).as_deref(), Some("ghu_enterprise1234"));
    }

    #[test]
    fn rejects_lookalike_hosts() {
        // Spoofy hosts that merely contain/abut a GitHub host must NOT match.
        for host in ["evil-github.com", "github.com.evil.com", "notghe.com", "ghe.com.evil.io"] {
            let raw = format!(r#"{{ "{host}": {{ "oauth_token": "ghu_should_not_match" }} }}"#);
            assert!(parse_token_json(&raw).is_none(), "{host} must not be treated as GitHub");
        }
        // Direct helper checks for clarity.
        assert!(is_github_host("github.com"));
        assert!(is_github_host("github.com:Iv1.abc"));
        assert!(is_github_host("tenant.ghe.com"));
        assert!(!is_github_host("evil-github.com"));
        assert!(!is_github_host("github.com.evil.com"));
    }

    #[test]
    fn poll_classifies_each_outcome() {
        assert!(matches!(
            parse_poll(&json!({ "access_token": "ghu_tok1234" })),
            Ok(PollOutcome::Connected(t)) if t == "ghu_tok1234"
        ));
        assert!(matches!(
            parse_poll(&json!({ "error": "authorization_pending" })),
            Ok(PollOutcome::Pending)
        ));
        assert!(matches!(
            parse_poll(&json!({ "error": "slow_down" })),
            Ok(PollOutcome::SlowDown)
        ));
        assert!(matches!(
            parse_poll(&json!({ "error": "access_denied" })),
            Ok(PollOutcome::Denied)
        ));
        assert!(matches!(
            parse_poll(&json!({ "error": "expired_token" })),
            Ok(PollOutcome::Expired)
        ));
        // An unrecognized error is a hard failure, not a poll-again.
        assert!(parse_poll(&json!({ "error": "unsupported_grant_type" })).is_err());
        // An empty/transient body is treated as pending so we keep polling.
        assert!(matches!(parse_poll(&json!({})), Ok(PollOutcome::Pending)));
    }
}
