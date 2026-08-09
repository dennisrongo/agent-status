//! Kimi Code (Moonshot AI) LIVE usage client.
//!
//! Reads the OAuth access token the Kimi Code CLI stored at
//! `$KIMI_CODE_HOME/credentials/kimi-code.json` (default `~/.kimi-code`) and
//! calls `GET https://api.kimi.com/coding/v1/usages` — the same endpoint the
//! CLI's `/usage` panel reads. Response: a weekly plan quota (`usage`), a set
//! of rolling rate windows (`limits[]` — the 5-hour window arrives as 300
//! TIME_UNIT_MINUTE), an optional Extra Usage wallet (`boosterWallet`, money as
//! fixed-point micro-cents), and the membership tier (`user.membership.level`
//! as a LEVEL_* enum). Numbers arrive as decimal strings.
//!
//! The access token is very short-lived (`expires_in` 900s) and the CLI only
//! renews it while it runs, so a stored token dies within 15 minutes of the
//! CLI closing. To keep the login alive past that, an expired-by-clock token
//! is refreshed in place (`refresh()`) using the stored refresh token and the
//! CLI's own public OAuth client — the rotated tokens are written back to the
//! same file the CLI reads. This is race-safe by the CLI's own design: its
//! `ensureFresh()` re-reads the credentials file before AND after its refresh
//! lock and uses the on-disk token when it changed, so it picks up our rotated
//! tokens instead of refreshing with a stale copy (on Windows it takes no lock
//! at all). The refresh token is single-use, so a persistence failure is a
//! hard error rather than dropping the only-valid token — mirroring claude.rs.

use chrono::{DateTime, Utc};
use serde_json::Value;
use std::time::Duration;

use super::{KeyVal, VendorStatus};

const ENDPOINT: &str = "https://api.kimi.com/coding/v1/usages";
/// OAuth host the CLI's device/refresh flow talks to. Overridable via the same
/// env vars the CLI honors (`KIMI_CODE_OAUTH_HOST`, then `KIMI_OAUTH_HOST`).
const DEFAULT_OAUTH_HOST: &str = "https://auth.kimi.com";
/// The Kimi Code CLI's public OAuth client id (device flow; no secret), used
/// here only with the user's own stored credentials, at their request.
const CLIENT_ID: &str = "17e5f671-d194-4dfb-9706-5516cb48c098";
/// Treat the access token as expired this many seconds before its stated
/// `expires_at`, so an about-to-die token isn't used for a fetch that would
/// 401 mid-flight anyway.
const EXPIRY_SKEW_SECS: i64 = 60;
/// `boosterWallet` money amounts are fixed-point: 1_000_000 units = 1 cent.
const FIXED_POINT_CENTS: f64 = 1_000_000.0;

pub async fn fetch(now: DateTime<Utc>) -> VendorStatus {
    let Some(raw) = read_credentials() else {
        return VendorStatus::not_configured();
    };
    let creds: Value = match serde_json::from_str(raw.trim()) {
        Ok(v) => v,
        Err(e) => return VendorStatus::failed(format!("stored credentials unreadable: {e}")),
    };
    let Some(token) = creds
        .get("access_token")
        .and_then(|t| t.as_str())
        .filter(|s| !s.is_empty())
    else {
        return VendorStatus::not_configured();
    };

    // Dead by the token's own clock → don't bother the network. `collect()`
    // auto-refreshes an expired login before calling this, so reaching this
    // branch means the refresh failed (or wasn't possible) — report it.
    let expired = creds
        .get("expires_at")
        .and_then(value_as_i64)
        .map(|exp| now.timestamp() >= exp - EXPIRY_SKEW_SECS)
        .unwrap_or(false);
    if expired {
        return login_expired("Kimi Code login expired — open Kimi Code or run `kimi login` to refresh it.");
    }

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(12))
        .build()
    {
        Ok(c) => c,
        Err(e) => return VendorStatus::failed(format!("client init: {e}")),
    };

    let resp = client
        .get(ENDPOINT)
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/json")
        .send()
        .await;

    match resp {
        Ok(r) => {
            let status = r.status();
            if !status.is_success() {
                if status.as_u16() == 401 {
                    // The clock looked valid but the server rejected the token
                    // (revoked elsewhere) — same re-login state as a dead clock.
                    return login_expired(format!(
                        "Kimi Code login was rejected (HTTP 401) — open Kimi Code or run `kimi login`."
                    ));
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

/// The "stale login" status: configured (a credential exists / existed) but
/// unusable until the user re-authenticates via the Kimi Code CLI. Both the
/// Overview and Settings read `auth_expired` to show the reconnect state.
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

/// The raw credentials JSON the Kimi Code CLI stored.
fn read_credentials() -> Option<String> {
    std::fs::read_to_string(credentials_path()?).ok()
}

/// `$KIMI_CODE_HOME` when set, else `~/.kimi-code`.
fn credentials_dir() -> Option<std::path::PathBuf> {
    if let Some(home) = std::env::var_os("KIMI_CODE_HOME") {
        let p = std::path::PathBuf::from(home);
        if !p.as_os_str().is_empty() {
            return Some(p);
        }
    }
    dirs::home_dir().map(|h| h.join(".kimi-code"))
}

/// The credentials file the CLI reads and writes.
fn credentials_path() -> Option<std::path::PathBuf> {
    Some(credentials_dir()?.join("credentials").join("kimi-code.json"))
}

/// Local, network-free view of the stored login: whether an access token is
/// present, whether it's past its stated expiry (with skew), and whether a
/// refresh token is available to renew it. Lets `collect()` auto-refresh a
/// dead-by-clock login up front instead of surfacing a false "login expired"
/// 15 minutes after the CLI last ran. Mirrors claude.rs's `TokenStatus`.
pub struct TokenStatus {
    pub present: bool,
    pub expired: bool,
    pub has_refresh: bool,
}

pub fn token_status(now: DateTime<Utc>) -> TokenStatus {
    read_credentials()
        .map(|raw| parse_token_status(&raw, now))
        .unwrap_or(TokenStatus { present: false, expired: false, has_refresh: false })
}

/// Pure half of `token_status`, split out so it's unit-testable without
/// touching the real credentials file.
fn parse_token_status(raw: &str, now: DateTime<Utc>) -> TokenStatus {
    let Ok(v) = serde_json::from_str::<Value>(raw.trim()) else {
        return TokenStatus { present: false, expired: false, has_refresh: false };
    };
    let nonempty = |key: &str| {
        v.get(key)
            .and_then(|t| t.as_str())
            .map(|s| !s.is_empty())
            .unwrap_or(false)
    };
    // An empty access_token is the CLI's "revoked tombstone" — present:false,
    // so no refresh is attempted and the UI shows the sign-in state.
    let present = nonempty("access_token");
    let has_refresh = nonempty("refresh_token");
    // A missing/unparseable expires_at counts as not-expired: don't force a
    // needless refresh on a token that may still be valid — the live fetch's
    // 401 remains the backstop.
    let expired = v
        .get("expires_at")
        .and_then(value_as_i64)
        .map(|exp| now.timestamp() >= exp - EXPIRY_SKEW_SECS)
        .unwrap_or(false);
    TokenStatus { present, expired: present && expired, has_refresh }
}

/// The OAuth token endpoint for the refresh grant. Env overrides match the
/// CLI's own (`KIMI_CODE_OAUTH_HOST` wins over `KIMI_OAUTH_HOST`).
fn token_endpoint() -> String {
    let host = std::env::var("KIMI_CODE_OAUTH_HOST")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            std::env::var("KIMI_OAUTH_HOST").ok().filter(|s| !s.trim().is_empty())
        })
        .unwrap_or_else(|| DEFAULT_OAUTH_HOST.to_string());
    token_endpoint_for(&host)
}

/// Pure join of host + token path, tolerating a trailing slash on the host
/// (the CLI does the same `replace(/\/$/, "")` normalization).
fn token_endpoint_for(host: &str) -> String {
    format!("{}/api/oauth/token", host.trim().trim_end_matches('/'))
}

/// Refresh an expired Kimi Code access token using the stored refresh token,
/// then write the rotated credentials back to the same file the CLI reads.
///
/// The refresh token is SINGLE-USE: the server invalidates the old one and
/// always returns a new one. If we obtain new tokens but fail to persist them,
/// the user is locked out of Kimi Code — so persistence failures are hard
/// errors (mirrors claude.rs's `refresh`).
pub async fn refresh(now: DateTime<Utc>) -> Result<(), String> {
    let raw = read_credentials().ok_or("No Kimi Code login found to refresh.")?;
    let creds: Value = serde_json::from_str(raw.trim())
        .map_err(|e| format!("stored credentials unreadable: {e}"))?;
    let refresh_token = creds
        .get("refresh_token")
        .and_then(|t| t.as_str())
        .filter(|s| !s.is_empty())
        .ok_or("No refresh token stored — open Kimi Code or run `kimi login` to sign in again.")?
        .to_string();

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| format!("client init: {e}"))?;

    // Form-encoded, exactly the CLI's own refresh request.
    let resp = client
        .post(token_endpoint())
        .form(&[
            ("client_id", CLIENT_ID),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token.as_str()),
        ])
        .send()
        .await
        .map_err(|e| format!("request error: {e}"))?;

    let status = resp.status();
    if !status.is_success() {
        // 400 invalid_grant / 401 → the refresh token itself is dead (already
        // rotated by a racing refresh, revoked, or fully expired). Only a real
        // login can fix it.
        if status.as_u16() == 400 || status.as_u16() == 401 {
            return Err(
                "Kimi Code refresh token expired — open Kimi Code or run `kimi login` to sign in again."
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
    // Replace the refresh token (single-use rotation). If the server omitted a
    // new one, keep the prior value rather than blanking it.
    let new_refresh = tok
        .get("refresh_token")
        .and_then(|t| t.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let expires_in = tok.get("expires_in").and_then(value_as_i64).unwrap_or(900);
    let serialized = build_refreshed_credentials(
        &raw,
        now,
        &access,
        new_refresh.as_deref(),
        expires_in,
        tok.get("scope").and_then(|s| s.as_str()),
        tok.get("token_type").and_then(|s| s.as_str()),
    )?;
    write_credentials_file(&serialized)
}

/// Pure builder for the refreshed credentials JSON. Merges the new tokens into
/// the prior file (preserving every field the CLI wrote) so the on-disk shape
/// stays exactly what the CLI expects. Split from the network/file I/O so the
/// merge and the expiry clamp are unit-testable.
fn build_refreshed_credentials(
    existing: &str,
    now: DateTime<Utc>,
    access: &str,
    new_refresh: Option<&str>,
    expires_in: i64,
    scope: Option<&str>,
    token_type: Option<&str>,
) -> Result<String, String> {
    let mut root: Value = serde_json::from_str(existing.trim())
        .map_err(|e| format!("stored credentials unreadable: {e}"))?;
    if !root.is_object() {
        root = serde_json::json!({});
    }
    let obj = root.as_object_mut().expect("root coerced to object");

    obj.insert("access_token".into(), Value::String(access.to_string()));
    if let Some(r) = new_refresh {
        obj.insert("refresh_token".into(), Value::String(r.to_string()));
    }
    // expires_in is untrusted JSON; clamp it and build the deadline without
    // panicking — `panic = "abort"` would take the whole app down on overflow.
    // Mirrors the Claude merge: cap to a day, floor at 5 min, and fall back to
    // the CLI's usual 15 min if the arithmetic ever can't be represented.
    let clamped = expires_in.clamp(300, 86_400);
    let expires_at = chrono::Duration::try_seconds(clamped)
        .and_then(|d| now.checked_add_signed(d))
        .unwrap_or_else(|| now + chrono::Duration::seconds(900))
        .timestamp();
    obj.insert("expires_at".into(), Value::Number(expires_at.into()));
    obj.insert("expires_in".into(), Value::Number(clamped.into()));
    if let Some(scope) = scope.filter(|s| !s.is_empty()) {
        obj.insert("scope".into(), Value::String(scope.to_string()));
    }
    if let Some(token_type) = token_type.filter(|s| !s.is_empty()) {
        obj.insert("token_type".into(), Value::String(token_type.to_string()));
    }

    serde_json::to_string(&root).map_err(|e| format!("serialize: {e}"))
}

/// Persist refreshed credentials to the file the CLI reads.
fn write_credentials_file(json: &str) -> Result<(), String> {
    let path = credentials_path().ok_or("no home directory for Kimi Code credentials")?;
    std::fs::write(&path, json).map_err(|e| format!("write {}: {e}", path.display()))
}

/// Whether the `kimi` CLI is somewhere on PATH (drives detection when no login
/// exists yet — the user has the tool but hasn't signed in). Cheap, no spawn.
pub fn cli_on_path() -> bool {
    let Some(paths) = std::env::var_os("PATH") else { return false };
    std::env::split_paths(&paths).any(|dir| {
        let exe = dir.join("kimi");
        exe.is_file()
            || exe.with_extension("exe").is_file()
            || exe.with_extension("cmd").is_file()
    })
}

/// Pure parser for the `/usages` response. `now` lets reset timestamps be
/// rendered as Claude-style countdowns ("resets 4h 12m"), matching GLM.
pub fn parse(v: &Value, now: DateTime<Utc>) -> VendorStatus {
    // Membership tier (e.g. "LEVEL_INTERMEDIATE") — surfaced in the headline
    // secondary line, mirroring GLM's plan level.
    let level = v
        .get("user")
        .and_then(|u| u.get("membership"))
        .and_then(|m| m.get("level"))
        .and_then(|l| l.as_str())
        .map(humanize_level)
        .filter(|s| !s.is_empty());

    // Each entry carries a sort rank so the windows render in a fixed order
    // (Session, Weekly, …) regardless of payload order — the short coding
    // window always sits left of the weekly quota, matching Claude/GLM.
    let mut detail: Vec<(u8, KeyVal)> = Vec::new();
    let mut weekly_pct: Option<f64> = None;
    let mut session_pct: Option<f64> = None;

    // The summary `usage` object is the plan's weekly quota (the backend omits
    // its window). Values are percent units (limit is normally 100).
    if let Some(usage) = v.get("usage") {
        if let Some(kv) = meter_row("Weekly", usage, now) {
            weekly_pct = kv.pct;
            detail.push((1, kv));
        }
    }

    // Rolling rate windows. The 5-hour coding window arrives as
    // 300 TIME_UNIT_MINUTE; fold whole hours so it renders "Session" (this
    // app's name for the 5-hour window) rather than "300m limit".
    if let Some(limits) = v.get("limits").and_then(|l| l.as_array()) {
        for lim in limits {
            let Some(detail_obj) = lim.get("detail") else { continue };
            let window = lim.get("window");
            let label = window_label(window);
            let rank: u8 = if label == "Session" { 0 } else { 2 };
            if let Some(kv) = meter_row(&label, detail_obj, now) {
                if label == "Session" {
                    session_pct = kv.pct;
                }
                detail.push((rank, kv));
            }
        }
    }

    if detail.is_empty() {
        return shape_error("no quota windows in response");
    }

    // Extra Usage wallet (boosterWallet): pay-as-you-go balance that kicks in
    // when the plan quota runs out. Money is fixed-point micro-cents; rendered
    // as plain text rows under the meters.
    if let Some(wallet) = v.get("boosterWallet").and_then(|w| w.as_object()) {
        let balance = wallet.get("balance").and_then(|b| b.as_object());
        if let Some(balance) = balance {
            let currency = money_currency(wallet);
            if let Some(left) = balance.get("amountLeft").and_then(value_as_f64) {
                detail.push((3, KeyVal::text("Extra usage", fmt_money(left, &currency))));
            }
        }
        let cap_enabled = wallet
            .get("monthlyChargeLimitEnabled")
            .and_then(|b| b.as_bool())
            .unwrap_or(false);
        let cap = wallet.get("monthlyChargeLimit").and_then(|m| m.get("priceInCents")).and_then(value_as_f64);
        let used = wallet.get("monthlyUsed").and_then(|m| m.get("priceInCents")).and_then(value_as_f64);
        if cap_enabled {
            if let (Some(cap), Some(used)) = (cap, used) {
                if cap > 0.0 {
                    let currency = money_currency(wallet);
                    detail.push((
                        3,
                        KeyVal::text(
                            "Extra this month",
                            format!("{} / {}", fmt_money(used, &currency), fmt_money(cap, &currency)),
                        ),
                    ));
                }
            }
        }
    }

    // Stable sort keeps payload order within a rank while pinning Session
    // ahead of Weekly ahead of the Extra Usage rows.
    detail.sort_by_key(|(rank, _)| *rank);
    let detail: Vec<KeyVal> = detail.into_iter().map(|(_, kv)| kv).collect();

    // Headline: weekly plan usage if present, else the 5-hour session window.
    let (used, label) = if let Some(w) = weekly_pct {
        (w, "weekly quota used")
    } else if let Some(s) = session_pct {
        (s, "session quota used")
    } else {
        (0.0, "quota")
    };
    let secondary = match &level {
        Some(l) => format!("{l} · {label}"),
        None => label.to_string(),
    };

    VendorStatus {
        configured: true,
        ok: true,
        error: None,
        primary: format!("{:.0}% used", used.clamp(0.0, 100.0)),
        secondary,
        detail,
        auth_expired: false,
    }
}

/// Build one quota meter row from a `{used, limit, remaining, resetTime}`
/// object: pct drives the bar, the reset becomes a Claude-style countdown in
/// the faint slot. Returns `None` when the numbers are unusable (missing, or a
/// non-finite/hostile value), so a garbled window is skipped rather than
/// rendered as "NaN% used" — mirrors glm.rs.
fn meter_row(label: &str, detail: &Value, now: DateTime<Utc>) -> Option<KeyVal> {
    let used = detail.get("used").and_then(value_as_f64);
    let limit = detail.get("limit").and_then(value_as_f64);
    let pct = match (used, limit) {
        (Some(u), Some(l)) if l > 0.0 => Some(u / l * 100.0),
        _ => None,
    }
    .filter(|p| p.is_finite())?;

    // `resetTime` is an ISO timestamp on every window; render it as a
    // live-style countdown. An already-past reset renders nothing (stale data
    // must not say "resets 0m").
    let reset = detail
        .get("resetTime")
        .and_then(|r| r.as_str())
        .and_then(|ts| countdown(ts, now));
    let value = reset.map(|r| format!("resets in {r}")).unwrap_or_default();
    Some(KeyVal::meter(label, value, pct))
}

/// Map a rate window to a display label. A 5-hour window is this app's
/// "Session" (matching Claude's first bucket and GLM's coding window); other
/// durations render like the CLI does ("5h limit" style). Whole hours folded
/// out of minutes, mirroring the CLI's own normalization.
fn window_label(window: Option<&Value>) -> String {
    let Some(w) = window else { return "Limit".to_string() };
    let duration = w.get("duration").and_then(value_as_i64).unwrap_or(0);
    let unit = w.get("timeUnit").and_then(|u| u.as_str()).unwrap_or("");
    let (duration, unit) = if unit == "TIME_UNIT_MINUTE" && duration >= 60 && duration % 60 == 0 {
        (duration / 60, "TIME_UNIT_HOUR")
    } else {
        (duration, unit)
    };
    match unit {
        _ if unit == "TIME_UNIT_HOUR" && duration == 5 => "Session".to_string(),
        "TIME_UNIT_HOUR" => format!("{duration}h limit"),
        "TIME_UNIT_MINUTE" => format!("{duration}m limit"),
        "TIME_UNIT_DAY" => format!("{duration}d limit"),
        "TIME_UNIT_WEEK" => "Weekly".to_string(),
        _ => "Limit".to_string(),
    }
}

/// `LEVEL_INTERMEDIATE` → `Intermediate`; anything unrecognized falls back to a
/// title-cased de-underscored form so a new tier never renders ALL_CAPS.
fn humanize_level(level: &str) -> String {
    let stripped = level.strip_prefix("LEVEL_").unwrap_or(level);
    stripped
        .split('_')
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(first) => {
                    first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase()
                }
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Currency for the wallet rows: taken from the monthly limit/used entries,
/// defaulting to USD (mirrors the CLI's parser).
fn money_currency(wallet: &serde_json::Map<String, Value>) -> String {
    for key in ["monthlyChargeLimit", "monthlyUsed"] {
        if let Some(c) = wallet
            .get(key)
            .and_then(|m| m.get("currency"))
            .and_then(|c| c.as_str())
            .filter(|s| !s.is_empty())
        {
            return c.to_string();
        }
    }
    "USD".to_string()
}

/// Fixed-point micro-cents → a currency string. `$` for USD, `¥` for CNY,
/// otherwise a plain "<amount> <CODE>" (mirrors the CLI's formatter).
fn fmt_money(raw: f64, currency: &str) -> String {
    let amount = raw / FIXED_POINT_CENTS / 100.0;
    match currency.to_uppercase().as_str() {
        "USD" => format!("${amount:.2}"),
        "CNY" => format!("¥{amount:.2}"),
        other => format!("{amount:.2} {other}"),
    }
}

/// Format a reset ISO timestamp as a compact countdown from `now`, matching
/// Claude's "4h 12m" / "2d 3h" / "23m" style. Returns `None` for a missing,
/// unparseable, or already-past reset — never a nonsensical "resets 0m".
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-08-08T18:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    /// A reset timestamp `mins` minutes in the future of `now()`, ISO format.
    fn reset_in(mins: i64) -> String {
        (now() + chrono::Duration::minutes(mins)).to_rfc3339()
    }

    /// The real live shape, captured from `GET /coding/v1/usages`.
    fn live_payload() -> Value {
        json!({
            "user": {
                "userId": "u1",
                "region": "REGION_OVERSEA",
                "membership": { "level": "LEVEL_INTERMEDIATE" }
            },
            "usage": { "limit": "100", "used": "4", "remaining": "96", "resetTime": reset_in(7 * 24 * 60) },
            "limits": [
                { "window": { "duration": 300, "timeUnit": "TIME_UNIT_MINUTE" },
                  "detail": { "limit": "100", "used": "18", "remaining": "82", "resetTime": reset_in(297) } }
            ],
            "authentication": { "method": "METHOD_ACCESS_TOKEN", "scope": "FEATURE_CODING" }
        })
    }

    #[test]
    fn parses_live_shape_weekly_and_session() {
        let s = parse(&live_payload(), now());
        assert!(s.ok, "should parse ok: {:?}", s.error);
        // Session (5h folded from 300 minutes) pinned ahead of Weekly.
        assert_eq!(s.detail[0].label, "Session");
        assert_eq!(s.detail[0].pct, Some(18.0));
        assert_eq!(s.detail[0].value, "resets in 4h 57m");
        assert_eq!(s.detail[1].label, "Weekly");
        assert_eq!(s.detail[1].pct, Some(4.0));
        assert_eq!(s.detail[1].value, "resets in 7d 0h");
        // Weekly drives the headline; the level prefixes the secondary line.
        assert_eq!(s.primary, "4% used");
        assert_eq!(s.secondary, "Intermediate · weekly quota used");
    }

    #[test]
    fn session_only_falls_back_to_session_headline() {
        let v = json!({ "limits": [
            { "window": { "duration": 300, "timeUnit": "TIME_UNIT_MINUTE" },
              "detail": { "limit": "100", "used": "40", "resetTime": reset_in(90) } }
        ] });
        let s = parse(&v, now());
        assert!(s.ok);
        assert_eq!(s.primary, "40% used");
        assert_eq!(s.secondary, "session quota used");
        assert_eq!(s.detail[0].value, "resets in 1h 30m");
    }

    #[test]
    fn numeric_values_are_accepted_alongside_strings() {
        let v = json!({ "usage": { "limit": 100, "used": 25, "resetTime": reset_in(60) } });
        let s = parse(&v, now());
        assert!(s.ok);
        assert_eq!(s.detail[0].pct, Some(25.0));
    }

    #[test]
    fn computes_ratio_when_limit_is_not_100() {
        let v = json!({ "usage": { "limit": "1000", "used": "40", "resetTime": reset_in(60) } });
        let s = parse(&v, now());
        assert_eq!(s.detail[0].pct, Some(4.0));
        assert_eq!(s.primary, "4% used");
    }

    #[test]
    fn zero_limit_window_is_skipped() {
        // A limit of 0 would divide by zero — drop the row instead.
        let v = json!({ "usage": { "limit": "0", "used": "0" } });
        let s = parse(&v, now());
        assert!(!s.ok);
    }

    #[test]
    fn non_finite_used_is_skipped() {
        let v = json!({ "usage": { "limit": "100", "used": "NaN" } });
        let s = parse(&v, now());
        assert!(!s.ok);
    }

    #[test]
    fn meter_pct_is_clamped_by_keyval() {
        let v = json!({ "usage": { "limit": "100", "used": "250", "resetTime": reset_in(60) } });
        let s = parse(&v, now());
        assert_eq!(s.detail[0].pct, Some(100.0));
        assert_eq!(s.detail[0].status, Some("danger"));
    }

    #[test]
    fn past_reset_renders_empty_value() {
        let v = json!({ "usage": { "limit": "100", "used": "50", "resetTime": reset_in(-5) } });
        let s = parse(&v, now());
        assert_eq!(s.detail[0].value, "");
        assert_eq!(s.detail[0].pct, Some(50.0));
    }

    #[test]
    fn non_five_hour_windows_get_duration_labels() {
        let v = json!({ "limits": [
            { "window": { "duration": 1, "timeUnit": "TIME_UNIT_HOUR" },
              "detail": { "limit": "100", "used": "10" } },
            { "window": { "duration": 30, "timeUnit": "TIME_UNIT_MINUTE" },
              "detail": { "limit": "100", "used": "5" } },
            { "window": { "duration": 1, "timeUnit": "TIME_UNIT_DAY" },
              "detail": { "limit": "100", "used": "1" } }
        ] });
        let s = parse(&v, now());
        assert!(s.ok);
        let labels: Vec<_> = s.detail.iter().map(|d| d.label.as_str()).collect();
        assert!(labels.contains(&"1h limit"), "got {labels:?}");
        assert!(labels.contains(&"30m limit"), "got {labels:?}");
        assert!(labels.contains(&"1d limit"), "got {labels:?}");
    }

    #[test]
    fn week_window_in_limits_also_labels_weekly() {
        let v = json!({ "limits": [
            { "window": { "duration": 1, "timeUnit": "TIME_UNIT_WEEK" },
              "detail": { "limit": "100", "used": "12", "resetTime": reset_in(60) } }
        ] });
        let s = parse(&v, now());
        assert_eq!(s.detail[0].label, "Weekly");
    }

    #[test]
    fn level_is_humanized_and_prefixes_secondary() {
        assert_eq!(humanize_level("LEVEL_INTERMEDIATE"), "Intermediate");
        assert_eq!(humanize_level("LEVEL_ADVANCED_PRO"), "Advanced Pro");
        // Unknown shape without the prefix still title-cases.
        assert_eq!(humanize_level("vivace"), "Vivace");
    }

    #[test]
    fn missing_level_leaves_secondary_plain() {
        let v = json!({ "usage": { "limit": "100", "used": "4", "resetTime": reset_in(60) } });
        let s = parse(&v, now());
        assert_eq!(s.secondary, "weekly quota used");
    }

    #[test]
    fn booster_wallet_renders_balance_and_monthly_cap() {
        let mut p = live_payload();
        p["boosterWallet"] = json!({
            "balance": { "type": "BOOSTER", "amount": "500000000", "amountLeft": "250000000" },
            "monthlyChargeLimitEnabled": true,
            "monthlyChargeLimit": { "priceInCents": "100000000", "currency": "USD" },
            "monthlyUsed": { "priceInCents": "40000000", "currency": "USD" }
        });
        let s = parse(&p, now());
        assert!(s.ok);
        let texts: Vec<_> = s.detail.iter().filter(|d| d.pct.is_none()).collect();
        assert_eq!(texts.len(), 2);
        // 250_000_000 micro-cents = 250¢ = $2.50; wallet rows sort after the meters.
        assert_eq!(texts[0].label, "Extra usage");
        assert_eq!(texts[0].value, "$2.50");
        assert_eq!(texts[1].label, "Extra this month");
        assert_eq!(texts[1].value, "$0.40 / $1.00");
    }

    #[test]
    fn booster_wallet_cny_uses_yuan_symbol() {
        let mut p = live_payload();
        p["boosterWallet"] = json!({
            "balance": { "type": "BOOSTER", "amount": "100000000", "amountLeft": "96000000" },
            "monthlyUsed": { "priceInCents": "0", "currency": "CNY" }
        });
        let s = parse(&p, now());
        let row = s.detail.iter().find(|d| d.label == "Extra usage").unwrap();
        assert_eq!(row.value, "¥0.96");
    }

    #[test]
    fn absent_wallet_adds_no_rows() {
        let s = parse(&live_payload(), now());
        assert!(s.detail.iter().all(|d| d.pct.is_some()));
    }

    #[test]
    fn empty_payload_is_not_ok() {
        let s = parse(&json!({ "foo": "bar" }), now());
        assert!(!s.ok);
        assert!(s.error.is_some());
        assert!(!s.auth_expired);
    }

    #[test]
    fn countdown_formats_like_claude() {
        let at = |mins: i64| reset_in(mins);
        assert_eq!(countdown(&at(45), now()).as_deref(), Some("45m"));
        assert_eq!(countdown(&at(135), now()).as_deref(), Some("2h 15m"));
        assert_eq!(countdown(&at(3 * 24 * 60), now()).as_deref(), Some("3d 0h"));
        assert!(countdown(&at(-1), now()).is_none());
        assert!(countdown("not a date", now()).is_none());
    }

    // ── token status (auto-refresh pre-check) ──

    #[test]
    fn token_status_reads_present_expiry_and_refresh() {
        let future = now().timestamp() + 900;
        let raw = format!(
            r#"{{"access_token":"a","refresh_token":"r","expires_at":{future},"scope":"kimi-code","token_type":"Bearer","expires_in":900}}"#
        );
        let s = parse_token_status(&raw, now());
        assert!(s.present && s.has_refresh && !s.expired);

        let past = now().timestamp() - 10;
        let raw = format!(r#"{{"access_token":"a","refresh_token":"r","expires_at":{past}}}"#);
        let s = parse_token_status(&raw, now());
        assert!(s.present && s.has_refresh && s.expired);

        // Token present but no refresh token → can't auto-renew.
        let raw = format!(r#"{{"access_token":"a","expires_at":{past}}}"#);
        let s = parse_token_status(&raw, now());
        assert!(s.present && !s.has_refresh && s.expired);
    }

    #[test]
    fn token_status_within_skew_counts_as_expired() {
        // 30s left is inside the 60s skew — refresh now rather than 401 mid-fetch.
        let soon = now().timestamp() + 30;
        let raw = format!(r#"{{"access_token":"a","refresh_token":"r","expires_at":{soon}}}"#);
        assert!(parse_token_status(&raw, now()).expired);
    }

    #[test]
    fn token_status_revoked_tombstone_is_not_present() {
        // The CLI writes empty tokens as its "revoked" tombstone — no refresh
        // attempt, the UI shows the sign-in state.
        let raw = r#"{"access_token":"","refresh_token":"","expires_at":0,"scope":"kimi-code","token_type":"Bearer","expires_in":0}"#;
        let s = parse_token_status(raw, now());
        assert!(!s.present && !s.has_refresh && !s.expired);
    }

    #[test]
    fn token_status_missing_expiry_counts_as_valid() {
        // Don't force a needless refresh on a token that may still be valid —
        // the live fetch's 401 is the backstop.
        let s = parse_token_status(r#"{"access_token":"a","refresh_token":"r"}"#, now());
        assert!(s.present && s.has_refresh && !s.expired);
    }

    #[test]
    fn token_status_garbage_json_is_absent() {
        let s = parse_token_status("not json", now());
        assert!(!s.present && !s.has_refresh && !s.expired);
    }

    // ── refreshed-credentials merge ──

    #[test]
    fn refreshed_credentials_merge_and_rotate() {
        let existing = r#"{"access_token":"old","refresh_token":"oldR","expires_at":1,"scope":"kimi-code","token_type":"Bearer","expires_in":900,"extra_keep":true}"#;
        let out = build_refreshed_credentials(
            existing,
            now(),
            "newA",
            Some("newR"),
            900,
            Some("kimi-code"),
            Some("Bearer"),
        )
        .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["access_token"], "newA");
        assert_eq!(v["refresh_token"], "newR");
        assert_eq!(v["expires_at"], now().timestamp() + 900);
        assert_eq!(v["expires_in"], 900);
        // Unknown fields the CLI wrote are preserved.
        assert_eq!(v["extra_keep"], true);
    }

    #[test]
    fn refreshed_credentials_keep_prior_refresh_when_server_omits_one() {
        let existing = r#"{"access_token":"old","refresh_token":"keepR","expires_at":1}"#;
        let out =
            build_refreshed_credentials(existing, now(), "newA", None, 900, None, None).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["refresh_token"], "keepR");
        // Scope/token_type absent from the response keep their stored values…
        let existing = r#"{"access_token":"old","refresh_token":"r","scope":"kimi-code","token_type":"Bearer"}"#;
        let out = build_refreshed_credentials(existing, now(), "a", None, 900, None, None).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["scope"], "kimi-code");
        assert_eq!(v["token_type"], "Bearer");
    }

    #[test]
    fn refreshed_credentials_clamp_hostile_expires_in() {
        // Untrusted JSON: a huge or negative expires_in must not overflow the
        // timestamp math (`panic = "abort"` takes the whole app down).
        let out = build_refreshed_credentials("{}", now(), "a", None, i64::MAX, None, None).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["expires_at"], now().timestamp() + 86_400);
        let out = build_refreshed_credentials("{}", now(), "a", None, -5, None, None).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["expires_at"], now().timestamp() + 300);
    }

    // ── token endpoint ──

    #[test]
    fn token_endpoint_normalizes_trailing_slash() {
        assert_eq!(
            token_endpoint_for("https://auth.kimi.com"),
            "https://auth.kimi.com/api/oauth/token"
        );
        assert_eq!(
            token_endpoint_for("https://auth.kimi.com/"),
            "https://auth.kimi.com/api/oauth/token"
        );
    }
}

