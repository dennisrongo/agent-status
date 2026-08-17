//! Cursor usage client (unofficial `api2.cursor.sh` dashboard API).
//!
//! Auth: the user pastes a `crsr_...` User API Key (Cursor Dashboard → API
//! Keys), stored encrypted in settings like the z.ai/Anthropic keys. As a
//! fallback the Cursor Agent CLI's local `auth.json` (`%APPDATA%\Cursor` on
//! Windows, `~/.cursor` on macOS, `~/.config/cursor` on Linux) carries an
//! `apiKey` field of the same shape. Either key is exchanged for a ~1-hour
//! access token via `POST /auth/exchange_user_api_key`; tokens are cached in
//! memory (never persisted) keyed by the sha256 of the API key and refreshed
//! 5 minutes before their JWT `exp`.
//!
//! Usage calls are Connect-RPC JSON POSTs to
//! `/aiserver.v1.DashboardService/<Method>`: `GetCurrentPeriodUsage` (plan
//! spend vs. limit, USD cents), `GetPlanInfo` (plan name/price/cycle), and
//! `GetHardLimit` (on-demand spend cap). A paged `GetFilteredUsageEvents`
//! call feeds the tab's "Last 7 days" chart (Cursor writes no clean local
//! session log, so the chart is API-sourced).
//!
//! The API is unofficial — every failure degrades to `VendorStatus::failed`,
//! never a panic.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use base64::Engine;
use chrono::{DateTime, Datelike, Local, Utc};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::Digest;

use super::{KeyVal, VendorStatus};
use crate::scanner::WeekDay;

pub const API_BASE: &str = "https://api2.cursor.sh";

/// The dashboard page caps at ~10 pages of 100 events per request; stop there
/// so a heavy user can't stall `collect()`.
const MAX_EVENT_PAGES: u32 = 10;
const EVENT_PAGE_SIZE: u32 = 100;

/// Re-exchange the access token this many seconds before its JWT `exp`.
const TOKEN_REFRESH_EARLY_SECS: i64 = 300;

// ---------- auth ----------

/// Exchange a `crsr_...` User API Key for a dashboard access token. Returns
/// the token plus its expiry (JWT `exp`, or ~1 hour from now when the token
/// isn't a parseable JWT).
async fn exchange_token(client: &reqwest::Client, api_key: &str) -> Result<(String, i64), String> {
    let resp = client
        .post(format!("{API_BASE}/auth/exchange_user_api_key"))
        .header("Authorization", format!("Bearer {api_key}"))
        .header("Content-Type", "application/json")
        .body("{}")
        .send()
        .await
        .map_err(|e| format!("request error: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        let hint = match status.as_u16() {
            401 | 403 => " (check the key — use a User API Key, crsr_…)",
            _ => "",
        };
        return Err(format!("HTTP {}{hint}", status.as_u16()));
    }
    let v: Value = resp.json().await.map_err(|e| format!("invalid JSON: {e}"))?;
    let Some(token) = v.get("accessToken").and_then(|t| t.as_str()) else {
        return Err("no `accessToken` in exchange response".to_string());
    };
    let exp = jwt_exp(token).unwrap_or_else(|| Utc::now().timestamp() + 3600);
    Ok((token.to_string(), exp))
}

/// Extract the `exp` claim (seconds) from a JWT without verifying it — we only
/// need the server-minted lifetime for cache bookkeeping.
fn jwt_exp(token: &str) -> Option<i64> {
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    let v: Value = serde_json::from_slice(&bytes).ok()?;
    v.get("exp").and_then(|e| e.as_i64().or_else(|| e.as_f64().map(|f| f as i64)))
}

/// sha256 hex of the API key — the in-memory token cache key, so the plaintext
/// key never sits in the cache map itself.
fn key_id(api_key: &str) -> String {
    let digest = sha2::Sha256::digest(api_key.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

static TOKEN_CACHE: OnceLock<Mutex<HashMap<String, (String, i64)>>> = OnceLock::new();

fn token_cache() -> &'static Mutex<HashMap<String, (String, i64)>> {
    TOKEN_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Access token for `api_key`, reusing the in-memory cached one while it has
/// more than `TOKEN_REFRESH_EARLY_SECS` left and re-exchanging otherwise.
async fn cached_access_token(client: &reqwest::Client, api_key: &str) -> Result<String, String> {
    let id = key_id(api_key);
    if let Ok(map) = token_cache().lock() {
        if let Some((token, exp)) = map.get(&id) {
            if *exp - Utc::now().timestamp() > TOKEN_REFRESH_EARLY_SECS {
                return Ok(token.clone());
            }
        }
    }
    let (token, exp) = exchange_token(client, api_key).await?;
    if let Ok(mut map) = token_cache().lock() {
        map.insert(id, (token.clone(), exp));
    }
    Ok(token)
}

/// Drop a cached token (after a 401) so the next call re-exchanges.
fn evict_token(api_key: &str) {
    if let Ok(mut map) = token_cache().lock() {
        map.remove(&key_id(api_key));
    }
}

/// The Cursor Agent CLI's platform `auth.json` path.
fn cli_auth_path() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        // %APPDATA%\Cursor\auth.json
        dirs::config_dir().map(|d| d.join("Cursor").join("auth.json"))
    }
    #[cfg(target_os = "macos")]
    {
        dirs::home_dir().map(|d| d.join(".cursor").join("auth.json"))
    }
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    {
        dirs::home_dir().map(|d| d.join(".config").join("cursor").join("auth.json"))
    }
}

/// Pull the `apiKey` field out of an `auth.json` body — only when it looks
/// like a User API Key (`crsr_...`).
fn parse_cli_api_key(body: &str) -> Option<String> {
    let v: Value = serde_json::from_str(body).ok()?;
    v.get("apiKey")
        .and_then(|k| k.as_str())
        .filter(|k| k.starts_with("crsr_"))
        .map(|k| k.to_string())
}

/// Read the Cursor Agent CLI's stored User API Key, if it has one.
pub fn find_cli_api_key() -> Option<String> {
    let body = std::fs::read_to_string(cli_auth_path()?).ok()?;
    parse_cli_api_key(&body)
}

/// Resolve the effective API key: the settings key wins, else the CLI's
/// `auth.json`. `None` means Cursor simply isn't set up on this machine.
pub fn resolve_api_key(settings_key: Option<&str>) -> Option<String> {
    settings_key
        .map(|k| k.to_string())
        .filter(|k| !k.is_empty())
        .or_else(find_cli_api_key)
}

// ---------- dashboard calls ----------

/// A dashboard call failed because the access token was rejected (401) — the
/// caller evicts the cached token, re-exchanges once, and retries.
struct AuthRejected;

async fn dashboard_call(
    client: &reqwest::Client,
    token: &str,
    method: &str,
    body: Value,
) -> Result<Value, Result<AuthRejected, String>> {
    let resp = client
        .post(format!("{API_BASE}/aiserver.v1.DashboardService/{method}"))
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        // Connect-RPC JSON protocol marker — required by api2.cursor.sh.
        .header("Connect-Protocol-Version", "1")
        .json(&body)
        .send()
        .await
        .map_err(|e| Err(format!("request error: {e}")))?;
    let status = resp.status();
    if status.as_u16() == 401 {
        return Err(Ok(AuthRejected));
    }
    if !status.is_success() {
        return Err(Err(format!("HTTP {}", status.as_u16())));
    }
    resp.json::<Value>()
        .await
        .map_err(|e| Err(format!("invalid JSON: {e}")))
}

/// Run a dashboard call with one auth retry: on a 401 the cached token is
/// evicted, a fresh one exchanged, and the call retried once.
async fn call_with_retry<F, Fut>(
    client: &reqwest::Client,
    api_key: &str,
    token: &str,
    call: F,
) -> Result<Value, Result<AuthRejected, String>>
where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = Result<Value, Result<AuthRejected, String>>>,
{
    match call(token.to_string()).await {
        Err(Ok(AuthRejected)) => {
            evict_token(api_key);
            let fresh = cached_access_token(client, api_key)
                .await
                .map_err(|e| Err(format!("re-auth: {e}")))?;
            call(fresh).await
        }
        other => other,
    }
}

// ---------- status fetch ----------

pub async fn fetch(api_key: Option<&str>, now: DateTime<Utc>) -> VendorStatus {
    let Some(api_key) = resolve_api_key(api_key) else {
        return VendorStatus::not_configured();
    };
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(12))
        .build()
    {
        Ok(c) => c,
        Err(e) => return VendorStatus::failed(format!("client init: {e}")),
    };
    let token = match cached_access_token(&client, &api_key).await {
        Ok(t) => t,
        Err(e) => return VendorStatus::failed(format!("key exchange: {e}")),
    };

    // The three dashboard reads only share the token, so fire them together.
    let c = &client;
    let (usage, plan, hard) = tokio::join!(
        call_with_retry(c, &api_key, &token, |t| async move {
            dashboard_call(c, &t, "GetCurrentPeriodUsage", json!({})).await
        }),
        call_with_retry(c, &api_key, &token, |t| async move {
            dashboard_call(c, &t, "GetPlanInfo", json!({})).await
        }),
        call_with_retry(c, &api_key, &token, |t| async move {
            dashboard_call(c, &t, "GetHardLimit", json!({})).await
        }),
    );

    // The usage read is the only required one — plan/hard-limit enrich the
    // detail rows but a missing one mustn't fail the whole card. A 401 that
    // survives the retry marks the login expired (re-paste / re-login).
    let usage = match usage {
        Ok(v) => v,
        Err(Ok(AuthRejected)) => {
            let mut s = VendorStatus::failed("HTTP 401 (API key rejected — re-paste your crsr_… key)");
            s.auth_expired = true;
            return s;
        }
        Err(Err(e)) => return VendorStatus::failed(e),
    };
    let plan = plan.ok().unwrap_or_else(|| json!({}));
    let hard = hard.ok().unwrap_or_else(|| json!({}));
    parse(&usage, &plan, &hard, now)
}

/// Pure parser for the three dashboard payloads. `planUsage` amounts are USD
/// CENTS (as strings or numbers); the meter clamps via `KeyVal::meter`.
pub fn parse(usage: &Value, plan: &Value, hard: &Value, now: DateTime<Utc>) -> VendorStatus {
    let Some(pu) = usage.get("planUsage").filter(|p| p.is_object()) else {
        return shape_error("no `planUsage` object in response");
    };

    let spend = pu.get("totalSpend").and_then(value_as_f64);
    let limit = pu.get("limit").and_then(value_as_f64);
    let pct = pu
        .get("totalPercentUsed")
        .and_then(value_as_f64)
        .or_else(|| match (spend, limit) {
            (Some(s), Some(l)) if l > 0.0 => Some(s / l * 100.0),
            _ => None,
        })
        .filter(|p| p.is_finite());
    let display = usage.get("displayMessage").and_then(|d| d.as_str());

    // A payload with neither a percent nor a spend figure has nothing to show.
    if pct.is_none() && spend.is_none() {
        return shape_error("no usable spend figures in `planUsage`");
    }

    let info = plan.get("planInfo").filter(|p| p.is_object());
    let plan_name = info
        .and_then(|i| i.get("planName"))
        .and_then(|n| n.as_str())
        .unwrap_or("");
    let price = info
        .and_then(|i| i.get("price"))
        .and_then(|p| p.as_str())
        .unwrap_or("");

    // Cycle end rides on the usage payload as a ms-epoch string.
    let cycle_end = usage
        .get("billingCycleEnd")
        .and_then(value_as_i64)
        .and_then(DateTime::from_timestamp_millis)
        .map(|t| t.format("%Y-%m-%d").to_string())
        .filter(|d| !d.is_empty());

    let mut detail = Vec::new();
    let meter_value = match (spend, limit) {
        (Some(s), Some(l)) => format!("{} / {}", usd_cents(s), usd_cents(l)),
        (Some(s), None) => usd_cents(s),
        _ => String::new(),
    };
    match pct {
        Some(p) => detail.push(KeyVal::meter("Included usage", meter_value, p)),
        // Spend without any percent (e.g. an unmetered plan) — a text row.
        None => detail.push(KeyVal::text("Included usage", meter_value)),
    }
    if !plan_name.is_empty() {
        let label = if price.is_empty() {
            plan_name.to_string()
        } else {
            format!("{plan_name} · {price}")
        };
        detail.push(KeyVal::text("Plan", label));
    }
    if let Some(end) = cycle_end {
        detail.push(KeyVal::text("Cycle resets", end));
    }
    // On-demand (usage-based) spend beyond the included plan amount.
    let no_overage = hard
        .get("noUsageBasedAllowed")
        .and_then(|b| b.as_bool())
        .unwrap_or(false);
    let hard_limit = hard.get("hardLimit").and_then(value_as_f64);
    if no_overage {
        detail.push(KeyVal::text("On-demand", "disabled"));
    } else if let Some(cap) = hard_limit.filter(|c| *c > 0.0) {
        detail.push(KeyVal::text("On-demand", format!("capped at {}", usd_cents(cap))));
    }

    let primary = match (spend, limit) {
        (Some(s), Some(l)) => format!("{} of {} used", usd_cents(s), usd_cents(l)),
        (Some(s), None) => format!("{} used", usd_cents(s)),
        _ => format!("{:.0}% used", pct.unwrap_or(0.0).clamp(0.0, 100.0)),
    };
    let secondary = match display {
        Some(d) if !d.is_empty() => d.to_string(),
        _ => match pct {
            Some(p) => format!("{:.0}% of included usage", p.clamp(0.0, 100.0)),
            None => "included usage".to_string(),
        },
    };

    let _ = now; // no countdown rows today — the cycle end renders as a date
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

// ---------- 7-day usage chart (GetFilteredUsageEvents) ----------

/// The Cursor tab's "Last 7 days" view, built from paged
/// `GetFilteredUsageEvents` calls (Cursor keeps no usable local session log).
/// `days` reuses the scanner's `WeekDay` shape so the frontend renders it with
/// the same chart component as Claude/GLM.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CursorWeek {
    /// One row per calendar day, oldest first, always 7 ending today.
    pub days: Vec<WeekDay>,
    /// Per-model spend over the window ("claude-sonnet-4" -> "$12.40 · 63%").
    pub models: Vec<KeyVal>,
    /// Window spend, pre-rendered ("$19.65").
    pub week_spend: String,
    /// Usage events seen in the window.
    pub events: usize,
    /// Most recent event, humanized ("2h ago"), "—" when the window is empty.
    pub last: String,
}

/// Page `GetFilteredUsageEvents` over the last 7 days and aggregate client-side.
pub async fn fetch_week(api_key: Option<&str>, now: DateTime<Utc>) -> Result<CursorWeek, String> {
    let api_key = resolve_api_key(api_key).ok_or_else(|| "no Cursor API key".to_string())?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(12))
        .build()
        .map_err(|e| format!("client init: {e}"))?;
    let token = cached_access_token(&client, &api_key).await?;

    let start_ms = (now - chrono::Duration::days(7)).timestamp_millis().to_string();
    let end_ms = now.timestamp_millis().to_string();

    let mut events: Vec<Value> = Vec::new();
    let mut page: u32 = 1;
    loop {
        let body = json!({
            "startDate": start_ms,
            "endDate": end_ms,
            "page": page,
            "pageSize": EVENT_PAGE_SIZE,
        });
        let c = &client;
        let result = call_with_retry(c, &api_key, &token, |t| {
            let b = body.clone();
            async move { dashboard_call(c, &t, "GetFilteredUsageEvents", b).await }
        })
        .await;
        let v = match result {
            Ok(v) => v,
            Err(Ok(AuthRejected)) => {
                return Err("HTTP 401 (API key rejected)".to_string());
            }
            Err(Err(e)) => return Err(e),
        };
        let Some(batch) = v.get("usageEventsDisplay").and_then(|e| e.as_array()) else {
            break;
        };
        if batch.is_empty() {
            break;
        }
        events.extend(batch.iter().cloned());
        page += 1;
        if page > MAX_EVENT_PAGES {
            break;
        }
    }

    Ok(parse_week(&json!({ "usageEventsDisplay": events }), now))
}

/// Pure aggregator: events -> 7 calendar days (local, matching how the chart
/// labels render) + a per-model spend breakdown. Token-less events still count
/// toward spend; the window is always 7 days even when empty.
pub fn parse_week(v: &Value, now: DateTime<Utc>) -> CursorWeek {
    let events: &[Value] = v
        .get("usageEventsDisplay")
        .and_then(|e| e.as_array())
        .map(|a| a.as_slice())
        .unwrap_or(&[]);

    let mut by_day: HashMap<String, (f64, f64)> = HashMap::new(); // date -> (cents, tokens)
    let mut by_model: HashMap<String, f64> = HashMap::new(); // model -> cents
    let mut newest_ms: Option<i64> = None;

    for ev in events {
        let cents = ev
            .get("chargedCents")
            .and_then(value_as_f64)
            .or_else(|| {
                ev.get("tokenUsage")
                    .and_then(|t| t.get("totalCents"))
                    .and_then(value_as_f64)
            })
            .unwrap_or(0.0);
        let toks = ev
            .get("tokenUsage")
            .map(|t| {
                ["inputTokens", "outputTokens", "cacheReadTokens"]
                    .iter()
                    .filter_map(|k| t.get(k).and_then(value_as_f64))
                    .sum()
            })
            .unwrap_or(0.0);
        let ms = ev.get("timestamp").and_then(value_as_i64);
        if let Some(ms) = ms {
            newest_ms = Some(newest_ms.map_or(ms, |n| n.max(ms)));
            if let Some(day) = DateTime::from_timestamp_millis(ms)
                .map(|t| t.with_timezone(&Local).format("%Y-%m-%d").to_string())
            {
                let entry = by_day.entry(day).or_insert((0.0, 0.0));
                entry.0 += cents;
                entry.1 += toks;
            }
        }
        let model = ev.get("model").and_then(|m| m.as_str()).unwrap_or("");
        if !model.is_empty() && cents > 0.0 {
            *by_model.entry(model.to_string()).or_insert(0.0) += cents;
        }
    }

    // Seven calendar days ending today, mirroring the GLM week chart; the bar
    // normalizes to the highest-spend day (Cursor is dollar-metered).
    let today = now.with_timezone(&Local).date_naive();
    let mut max_cents = 0.0f64;
    let mut rows: Vec<(String, String, f64, f64)> = Vec::with_capacity(7);
    for i in (0..7).rev() {
        let d = today - chrono::Days::new(i);
        let key = d.format("%Y-%m-%d").to_string();
        let (cents, toks) = by_day.get(&key).copied().unwrap_or((0.0, 0.0));
        max_cents = max_cents.max(cents);
        rows.push((
            crate::scanner::weekday_abbr(d.weekday().num_days_from_monday()),
            key,
            cents,
            toks,
        ));
    }
    let days: Vec<WeekDay> = rows
        .iter()
        .map(|(day, date, cents, toks)| WeekDay {
            day: day.clone(),
            date: date.clone(),
            tok_fmt: crate::scanner::fmt_tokens(*toks),
            cost_fmt: usd_cents(*cents),
            bar_pct: if max_cents > 0.0 {
                ((cents / max_cents) * 100.0).round() as u32
            } else {
                0
            },
        })
        .collect();

    let week_cents: f64 = rows.iter().map(|r| r.2).sum();
    let models_sum: f64 = by_model.values().sum();
    let mut models: Vec<(String, f64)> = by_model.into_iter().collect();
    models.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let models: Vec<KeyVal> = models
        .into_iter()
        .map(|(name, cents)| {
            let share = if models_sum > 0.0 {
                cents / models_sum * 100.0
            } else {
                0.0
            };
            KeyVal::text(name, format!("{} · {:.0}%", usd_cents(cents), share))
        })
        .collect();

    let last = newest_ms
        .and_then(DateTime::from_timestamp_millis)
        .map(|t| crate::scanner::humanize_when(t, now))
        .unwrap_or_else(|| "—".to_string());

    CursorWeek {
        days,
        models,
        week_spend: usd_cents(week_cents),
        events: events.len(),
        last,
    }
}

// ---------- helpers ----------

/// USD cents -> "$X.XX".
fn usd_cents(c: f64) -> String {
    format!("${:.2}", c / 100.0)
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
        DateTime::parse_from_rfc3339("2026-08-17T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn usage_json() -> Value {
        json!({
            "billingCycleStart": "1754870400000",
            "billingCycleEnd": "1757548800000",
            "planUsage": {
                "totalSpend": "1234",
                "limit": "2000",
                "remaining": "766",
                "totalPercentUsed": "61.7"
            },
            "displayMessage": "You've used 62% of your included usage"
        })
    }

    fn plan_json() -> Value {
        json!({ "planInfo": {
            "planName": "Pro",
            "includedAmountCents": "2000",
            "price": "$20/mo",
            "billingCycleEnd": "1757548800000"
        } })
    }

    // ── parse ──

    #[test]
    fn parses_full_payload() {
        let hard = json!({ "hardLimit": "5000", "noUsageBasedAllowed": false });
        let s = parse(&usage_json(), &plan_json(), &hard, now());
        assert!(s.ok, "should parse ok: {:?}", s.error);
        assert_eq!(s.primary, "$12.34 of $20.00 used");
        assert_eq!(s.secondary, "You've used 62% of your included usage");
        let meter = &s.detail[0];
        assert_eq!(meter.label, "Included usage");
        assert_eq!(meter.value, "$12.34 / $20.00");
        assert_eq!(meter.pct, Some(61.7));
        assert!(s.detail.iter().any(|d| d.label == "Plan" && d.value == "Pro · $20/mo"));
        assert!(s.detail.iter().any(|d| d.label == "Cycle resets" && d.value == "2025-09-11"));
        assert!(s.detail.iter().any(|d| d.label == "On-demand" && d.value == "capped at $50.00"));
    }

    #[test]
    fn on_demand_disabled_row() {
        let hard = json!({ "hardLimit": "0", "noUsageBasedAllowed": true });
        let s = parse(&usage_json(), &plan_json(), &hard, now());
        assert!(s.detail.iter().any(|d| d.label == "On-demand" && d.value == "disabled"));
    }

    #[test]
    fn computes_percent_when_absent() {
        let v = json!({ "planUsage": { "totalSpend": 500, "limit": 2000 } });
        let s = parse(&v, &json!({}), &json!({}), now());
        assert!(s.ok);
        assert_eq!(s.detail[0].pct, Some(25.0));
        assert_eq!(s.secondary, "25% of included usage");
    }

    #[test]
    fn meter_pct_is_clamped() {
        let v = json!({ "planUsage": { "totalSpend": 9000, "limit": 2000, "totalPercentUsed": 450 } });
        let s = parse(&v, &json!({}), &json!({}), now());
        assert!(s.ok);
        assert_eq!(s.detail[0].pct, Some(100.0));
        assert_eq!(s.detail[0].status, Some("danger"));
    }

    #[test]
    fn missing_plan_usage_is_error() {
        let s = parse(&json!({ "oops": true }), &json!({}), &json!({}), now());
        assert!(!s.ok);
        assert!(s.error.is_some());
    }

    #[test]
    fn empty_plan_and_hard_still_parse() {
        let s = parse(&usage_json(), &json!({}), &json!({}), now());
        assert!(s.ok);
        assert_eq!(s.detail.len(), 2); // meter + cycle resets only
        assert!(s.detail.iter().all(|d| d.label != "Plan" && d.label != "On-demand"));
    }

    #[test]
    fn spend_without_limit_headlines_spend_only() {
        let v = json!({ "planUsage": { "totalSpend": 1750 } });
        let s = parse(&v, &json!({}), &json!({}), now());
        assert!(s.ok);
        assert_eq!(s.primary, "$17.50 used");
    }

    // ── jwt_exp ──

    #[test]
    fn jwt_exp_reads_payload_claim() {
        // header {"alg":"none"}, payload {"exp":2000000000}, no signature.
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r#"{"exp":2000000000}"#);
        let token = format!("e30.{payload}.");
        assert_eq!(jwt_exp(&token), Some(2_000_000_000));
    }

    #[test]
    fn jwt_exp_rejects_garbage() {
        assert_eq!(jwt_exp("not-a-jwt"), None);
        assert_eq!(jwt_exp("a.!!!.c"), None);
        assert_eq!(jwt_exp("a.e30.c"), None); // payload without exp
    }

    // ── CLI auth.json ──

    #[test]
    fn cli_auth_parses_user_api_key() {
        let body = r#"{"accessToken":"jwt…","apiKey":"crsr_abc123"}"#;
        assert_eq!(parse_cli_api_key(body), Some("crsr_abc123".to_string()));
    }

    #[test]
    fn cli_auth_rejects_non_crsr_or_missing_key() {
        assert_eq!(parse_cli_api_key(r#"{"apiKey":"other"}"#), None);
        assert_eq!(parse_cli_api_key(r#"{"accessToken":"jwt…"}"#), None);
        assert_eq!(parse_cli_api_key("not json"), None);
    }

    #[test]
    fn settings_key_wins_over_cli() {
        assert_eq!(resolve_api_key(Some("crsr_settings")), Some("crsr_settings".to_string()));
        assert_eq!(resolve_api_key(Some("")), find_cli_api_key());
    }

    // ── parse_week ──

    fn today_local() -> chrono::NaiveDate {
        now().with_timezone(&Local).date_naive()
    }

    fn ms_of(d: chrono::NaiveDate, h: u32) -> i64 {
        d.and_hms_opt(h, 0, 0)
            .unwrap()
            .and_local_timezone(Local)
            .single()
            .unwrap()
            .with_timezone(&Utc)
            .timestamp_millis()
    }

    #[test]
    fn aggregates_events_into_seven_days_and_models() {
        let today = today_local();
        let yesterday = today - chrono::Days::new(1);
        let v = json!({ "usageEventsDisplay": [
            { "timestamp": ms_of(today, 10).to_string(), "model": "claude-sonnet-4",
              "tokenUsage": { "inputTokens": 1000, "outputTokens": 500, "cacheReadTokens": 200 },
              "chargedCents": 150 },
            { "timestamp": ms_of(today, 11).to_string(), "model": "claude-sonnet-4",
              "tokenUsage": { "inputTokens": 2000, "outputTokens": 0 },
              "chargedCents": 50 },
            { "timestamp": ms_of(yesterday, 9).to_string(), "model": "gpt-5",
              "tokenUsage": { "totalCents": 300 } }
        ] });
        let w = parse_week(&v, now());
        assert_eq!(w.days.len(), 7);
        let today_row = w.days.last().unwrap();
        assert_eq!(today_row.date, today.format("%Y-%m-%d").to_string());
        assert_eq!(today_row.cost_fmt, "$2.00");
        assert_eq!(today_row.tok_fmt, "4K");
        assert_eq!(today_row.bar_pct, 67); // $2.00 vs. yesterday's $3.00 peak
        assert_eq!(w.days[w.days.len() - 2].bar_pct, 100); // busiest day
        assert_eq!(w.week_spend, "$5.00");
        assert_eq!(w.events, 3);
        assert_ne!(w.last, "—");
        // Per-model: sonnet $2.00 (40%), gpt-5 $3.00 (60%) — sorted desc.
        assert_eq!(w.models.len(), 2);
        assert_eq!(w.models[0].label, "gpt-5");
        assert_eq!(w.models[0].value, "$3.00 · 60%");
        assert_eq!(w.models[1].value, "$2.00 · 40%");
    }

    #[test]
    fn empty_window_still_returns_seven_zero_days() {
        let w = parse_week(&json!({ "usageEventsDisplay": [] }), now());
        assert_eq!(w.days.len(), 7);
        assert!(w.days.iter().all(|d| d.cost_fmt == "$0.00" && d.bar_pct == 0));
        assert_eq!(w.week_spend, "$0.00");
        assert_eq!(w.events, 0);
        assert_eq!(w.last, "—");
        assert!(w.models.is_empty());
    }

    #[test]
    fn token_less_event_counts_spend_only() {
        let today = today_local();
        let v = json!({ "usageEventsDisplay": [
            { "timestamp": ms_of(today, 8).to_string(), "model": "tab-completion",
              "chargedCents": 5 }
        ] });
        let w = parse_week(&v, now());
        let row = w.days.last().unwrap();
        assert_eq!(row.cost_fmt, "$0.05");
        assert_eq!(row.tok_fmt, "0");
        assert_eq!(w.events, 1);
    }

    #[test]
    fn event_without_timestamp_is_skipped_for_days_but_kept_in_count() {
        let v = json!({ "usageEventsDisplay": [
            { "model": "gpt-5", "chargedCents": 100 }
        ] });
        let w = parse_week(&v, now());
        assert_eq!(w.events, 1);
        assert_eq!(w.week_spend, "$0.00"); // no day to bucket into
        assert_eq!(w.models[0].value, "$1.00 · 100%");
    }

    #[test]
    fn missing_events_array_is_empty_week() {
        let w = parse_week(&json!({ "foo": "bar" }), now());
        assert_eq!(w.days.len(), 7);
        assert_eq!(w.events, 0);
    }
}
