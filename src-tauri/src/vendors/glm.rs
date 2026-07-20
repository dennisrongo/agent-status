//! z.ai (GLM Coding Plan) usage client.
//!
//! Uses z.ai's monitor API: `GET /api/monitor/usage/quota/limit`. The coding-plan
//! token is passed in the `Authorization` header WITHOUT a `Bearer` prefix.
//! Response shape: `{ "data": { "limits": [ { type, unit, number, percentage,
//! nextResetTime, ... } ], "level": ... } }`. The live `type` values are
//! `TOKENS_LIMIT` (used for BOTH the rolling 5-hour coding window and the weekly
//! window — told apart by `(unit, number)`: 3/5 = 5-hour, 6/1 = weekly) and
//! `TIME_LIMIT` (the monthly Web Search / Reader / Zread tool quota). The names
//! don't match their windows, so `parse` maps by meaning. `nextResetTime` is a
//! Unix epoch in MILLISECONDS, present on every window (including the 5-hour
//! one), rendered as a Claude-style countdown. The base URL is configurable
//! (z.ai global vs. open.bigmodel.cn for CN).

use chrono::{DateTime, Utc};
use serde_json::Value;
use std::time::Duration;

use super::{short_date, KeyVal, VendorStatus};

pub const DEFAULT_ENDPOINT: &str = "https://api.z.ai/api/monitor/usage/quota/limit";

/// Endpoints from older builds that should be upgraded to DEFAULT_ENDPOINT.
pub const STALE_ENDPOINTS: [&str; 2] = [
    "https://api.z.ai/api/paas/v4/usage",
    "https://open.bigmodel.cn/api/paas/v4/usage",
];

pub async fn fetch(api_key: &str, endpoint: &str, now: DateTime<Utc>) -> VendorStatus {
    let url = if endpoint.is_empty() { DEFAULT_ENDPOINT } else { endpoint };
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(12))
        .build()
    {
        Ok(c) => c,
        Err(e) => return VendorStatus::failed(format!("client init: {e}")),
    };

    let resp = client
        .get(url)
        // z.ai monitor API: raw token, NO "Bearer" prefix.
        .header("Authorization", api_key)
        .header("Accept-Language", "en-US,en")
        .header("Content-Type", "application/json")
        .send()
        .await;

    match resp {
        Ok(r) => {
            let status = r.status();
            if !status.is_success() {
                let hint = match status.as_u16() {
                    401 | 403 => " (check the key — use your GLM Coding Plan token)",
                    404 => " (wrong endpoint — expected /api/monitor/usage/quota/limit)",
                    _ => "",
                };
                return VendorStatus::failed(format!("HTTP {}{hint}", status.as_u16()));
            }
            match r.json::<Value>().await {
                Ok(v) => parse(&v, now),
                Err(e) => VendorStatus::failed(format!("invalid JSON: {e}")),
            }
        }
        Err(e) => VendorStatus::failed(format!("request error: {e}")),
    }
}

/// Pure parser for the monitor quota/limit response. `now` lets reset epochs be
/// rendered as a live-style countdown ("resets 4h 12m"), matching Claude.
pub fn parse(v: &Value, now: DateTime<Utc>) -> VendorStatus {
    let root = if v.get("data").map(|d| d.is_object()).unwrap_or(false) {
        &v["data"]
    } else {
        v
    };

    let Some(limits) = root.get("limits").and_then(|l| l.as_array()) else {
        return shape_error("no `data.limits` array in response");
    };

    // Each entry carries a sort rank so the windows render in a fixed order
    // (Session, Weekly, Monthly tools, …) regardless of the order z.ai lists
    // them — the short coding window then always sits left of the monthly tool
    // quota, matching Claude's Session-first overview.
    let mut detail: Vec<(u8, KeyVal)> = Vec::new();
    let mut five_h: Option<f64> = None;
    let mut weekly: Option<f64> = None;
    let mut monthly: Option<f64> = None;

    for lim in limits {
        let typ = lim.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let used = lim.get("currentValue").and_then(value_as_f64);
        let total = lim.get("total").and_then(value_as_f64);
        let pct = lim
            .get("percentage")
            .and_then(value_as_f64)
            .or_else(|| match (used, total) {
                (Some(u), Some(t)) if t > 0.0 => Some(u / t * 100.0),
                _ => None,
            })
            // Reject NaN/inf (e.g. a string "NaN") so it can't render as
            // "NaN% used" or a bogus "danger" bar — mirrors copilot.rs.
            .filter(|p| p.is_finite());

        // z.ai's live monitor uses ONE type, `TOKENS_LIMIT`, for BOTH the 5-hour
        // coding window and the weekly window — they're told apart by (unit,
        // number): unit=3/number=5 is the 5-hour quota, unit=6/number=1 is the
        // weekly quota. `TIME_LIMIT` (unit=5) is the monthly tool quota. Older or
        // synthetic shapes instead name the window in the type string
        // ("5h"/"weekly"/"mcp"); we honor both. Map everything to clean,
        // Claude-style labels; never surface the raw ALL_CAPS identifier.
        let unit = lim.get("unit").and_then(value_as_f64);
        let number = lim.get("number").and_then(value_as_f64);
        let is_tokens_type = typ.eq_ignore_ascii_case("TOKENS_LIMIT");
        let by_unit_week = is_tokens_type && unit == Some(6.0) && number == Some(1.0);
        let by_unit_5h = is_tokens_type && unit == Some(3.0) && number == Some(5.0);

        let tl = typ.to_lowercase();
        let is_week = by_unit_week || tl.contains("week");
        // Match "time" only as a whole word so "runtime"/"real-time" aren't
        // misread as the monthly window; z.ai's real type is a standalone
        // TIME_LIMIT.
        let is_month = typ.eq_ignore_ascii_case("TIME_LIMIT")
            || tl.contains("month")
            || tl.split(|c: char| !c.is_alphanumeric()).any(|w| w == "time");
        let is_5h = by_unit_5h
            || tl.contains("5h")
            || tl.contains("5 h")
            || tl.contains("5-h")
            // A bare token-count limit (no unit/number to disambiguate) is the
            // 5-hour coding window, unless the type already names a longer window.
            || (tl.contains("token") && !is_week && !is_month);

        let label: String = if is_5h {
            // "Session" mirrors Claude's first bucket; the underlying window is
            // still the rolling 5-hour coding quota.
            "Session".to_string()
        } else if is_week {
            "Weekly".to_string()
        } else if is_month {
            "Monthly tools".to_string()
        } else if tl.contains("mcp") {
            "MCP".to_string()
        } else if typ.is_empty() {
            continue;
        } else {
            humanize(typ)
        };

        // Faint right-aligned slot, parallel to Claude's "resets in 4h 12m". z.ai
        // sends `nextResetTime` as a Unix epoch in MILLISECONDS on every window
        // (including the 5-hour one), so render it as a live-style countdown. A
        // string timestamp (older/synthetic shape) is trimmed to its date unless
        // it's a numeric epoch carried as a string. The percentage drives the bar.
        let reset = match lim.get("nextResetTime") {
            // Integer epoch is the live shape; tolerate a float-encoded one too
            // (mirrors claude.rs's value_as_i64) so the countdown can't silently
            // vanish on a `…597.0`.
            Some(Value::Number(n)) => n
                .as_i64()
                .or_else(|| n.as_f64().map(|f| f as i64))
                .and_then(|ms| countdown_ms(ms, now)),
            Some(Value::String(s)) => match s.parse::<i64>() {
                Ok(ms) => countdown_ms(ms, now),
                Err(_) => Some(short_date(s)).filter(|d| !d.is_empty()),
            },
            _ => None,
        };

        let Some(p) = pct else { continue };
        let value = match (&reset, used, total) {
            (Some(r), _, _) => format!("resets in {r}"),
            (None, Some(u), Some(t)) => format!("{} / {}", fmt_count(u), fmt_count(t)),
            _ => String::new(),
        };
        let rank: u8 = if is_5h {
            0
        } else if is_week {
            1
        } else if is_month {
            2
        } else {
            3
        };
        detail.push((rank, KeyVal::meter(&label, value, p)));
        if is_5h {
            five_h = Some(p);
        } else if is_week {
            weekly = Some(p);
        } else if is_month {
            monthly = Some(p);
        }
    }

    if detail.is_empty() {
        return shape_error("no recognized quota limits in response");
    }

    // Stable sort keeps API order within a rank while pinning Session ahead of
    // Weekly ahead of Monthly tools.
    detail.sort_by_key(|(rank, _)| *rank);
    let detail: Vec<KeyVal> = detail.into_iter().map(|(_, kv)| kv).collect();

    // Headline: weekly usage if present, else the 5-hour window, else the
    // monthly tool quota.
    let (used, label) = if let Some(w) = weekly {
        (w, "weekly quota used")
    } else if let Some(f) = five_h {
        (f, "session quota used")
    } else if let Some(m) = monthly {
        (m, "monthly quota used")
    } else {
        (0.0, "quota")
    };

    VendorStatus {
        configured: true,
        ok: true,
        error: None,
        primary: format!("{:.0}% used", used.clamp(0.0, 100.0)),
        secondary: label.to_string(),
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

fn value_as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.parse::<f64>().ok(),
        _ => None,
    }
}

/// Turn a raw API type like `"FOO_BAR_LIMIT"` into a display label `"Foo bar"`,
/// so an unrecognized window never renders as a raw ALL_CAPS identifier.
fn humanize(typ: &str) -> String {
    let words: Vec<String> = typ
        .split(|c: char| c == '_' || c == '-' || c.is_whitespace())
        .filter(|w| !w.is_empty() && !w.eq_ignore_ascii_case("limit"))
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(first) => {
                    first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase()
                }
                None => String::new(),
            }
        })
        .collect();
    if words.is_empty() {
        typ.trim().to_string()
    } else {
        words.join(" ")
    }
}

/// Format a reset epoch (ms) as a compact countdown from `now`, matching
/// Claude's "4h 12m" / "2d 3h" / "23m" style. Returns `None` for a missing or
/// already-past reset so the row falls back to used/total or nothing — never a
/// nonsensical "resets 0m" for stale data. Arithmetic is checked because
/// `panic = "abort"` would take the whole app down on overflow of a hostile ts.
fn countdown_ms(reset_ms: i64, now: DateTime<Utc>) -> Option<String> {
    let secs = reset_ms.checked_sub(now.timestamp_millis())? / 1000;
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

fn fmt_count(n: f64) -> String {
    if n >= 1e9 {
        format!("{:.1}B", n / 1e9)
    } else if n >= 1e6 {
        format!("{:.1}M", n / 1e6)
    } else if n >= 1e3 {
        format!("{:.0}K", n / 1e3)
    } else {
        format!("{}", n as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-06-21T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    /// A reset epoch `mins` minutes in the future of `now()`, in milliseconds.
    fn reset_in(mins: i64) -> i64 {
        (now() + chrono::Duration::minutes(mins)).timestamp_millis()
    }

    #[test]
    fn parses_five_hour_and_weekly_quota() {
        let v = json!({
            "data": { "limits": [
                { "type": "Token usage(5h)", "percentage": 40, "currentValue": 16000000, "total": 40000000 },
                { "type": "Token usage(Weekly)", "percentage": 12.5, "currentValue": 50000000, "total": 400000000 }
            ] }
        });
        let s = parse(&v, now());
        assert!(s.ok, "should parse ok");
        assert!(s.primary.ends_with("% used"));
        assert_eq!(s.secondary, "weekly quota used");
        assert_eq!(s.detail.len(), 2);
        assert!(s.detail[0].value.contains("16.0M / 40.0M"));
    }

    #[test]
    fn falls_back_to_five_hour_when_no_weekly() {
        let v = json!({ "data": { "limits": [
            { "type": "Token usage(5h)", "percentage": 25, "currentValue": 10000000, "total": 40000000 }
        ] } });
        let s = parse(&v, now());
        assert!(s.ok);
        assert_eq!(s.secondary, "session quota used");
    }

    #[test]
    fn computes_percentage_when_absent() {
        let v = json!({ "data": { "limits": [
            { "type": "Token usage(Weekly)", "currentValue": "100", "total": "400" }
        ] } });
        let s = parse(&v, now());
        assert!(s.ok);
        // 100/400 = 25% used; the percent now rides on `pct` (drives the bar),
        // while `value` carries the used/total amount.
        assert_eq!(s.detail[0].pct, Some(25.0));
        assert_eq!(s.detail[0].status, Some("ok"));
        assert!(s.detail[0].value.contains("100 / 400"));
    }

    #[test]
    fn maps_real_zai_token_and_time_limits() {
        // z.ai's live monitor uses TOKENS_LIMIT (5-hour coding quota) and
        // TIME_LIMIT (monthly web-search/reader/zread quota).
        let v = json!({ "data": { "limits": [
            { "type": "TIME_LIMIT", "percentage": 1 },
            { "type": "TOKENS_LIMIT", "percentage": 0 }
        ] } });
        let s = parse(&v, now());
        assert!(s.ok);
        let labels: Vec<_> = s.detail.iter().map(|d| d.label.as_str()).collect();
        assert!(labels.contains(&"Monthly tools"), "got {labels:?}");
        assert!(labels.contains(&"Session"), "got {labels:?}");
        // Even though the API lists TIME_LIMIT (monthly) first, the Session
        // window is pinned ahead of it so it renders to the left.
        assert_eq!(s.detail[0].label, "Session", "got {labels:?}");
        assert_eq!(s.detail[1].label, "Monthly tools", "got {labels:?}");
        // The session window is the coding throttle → it drives the headline.
        assert_eq!(s.secondary, "session quota used");
    }

    #[test]
    fn non_finite_percentage_is_skipped() {
        // A hostile/garbled `"NaN"` must not render as "NaN% used" or a bogus
        // "danger" meter; the only limit is dropped → no usable rows.
        let v = json!({ "data": { "limits": [
            { "type": "TOKENS_LIMIT", "percentage": "NaN" }
        ] } });
        let s = parse(&v, now());
        assert!(!s.ok);
    }

    #[test]
    fn meter_pct_is_clamped() {
        let v = json!({ "data": { "limits": [
            { "type": "TOKENS_LIMIT", "percentage": 250 }
        ] } });
        let s = parse(&v, now());
        assert_eq!(s.detail[0].pct, Some(100.0));
        assert_eq!(s.detail[0].status, Some("danger"));
    }

    #[test]
    fn time_is_matched_only_as_a_whole_word() {
        // "runtime" contains the substring "time" but must NOT be read as the
        // monthly window.
        let v = json!({ "data": { "limits": [
            { "type": "RUNTIME_LIMIT", "percentage": 50 }
        ] } });
        let s = parse(&v, now());
        assert!(s.ok);
        assert_eq!(s.detail[0].label, "Runtime");
    }

    #[test]
    fn reset_time_fills_the_faint_slot() {
        let v = json!({ "data": { "limits": [
            { "type": "TIME_LIMIT", "percentage": 1, "nextResetTime": "2026-06-24 17:13:00" }
        ] } });
        let s = parse(&v, now());
        let m = s.detail.iter().find(|d| d.label == "Monthly tools").unwrap();
        assert_eq!(m.value, "resets in 2026-06-24");
    }

    #[test]
    fn parses_real_monitor_shape_units_and_ms_reset() {
        // The actual live z.ai shape: TOKENS_LIMIT for BOTH the 5-hour (unit=3,
        // number=5) and weekly (unit=6, number=1) windows, TIME_LIMIT for the
        // monthly tool quota — with nextResetTime as a millisecond epoch on each.
        let v = json!({ "data": {
            "level": "lite",
            "limits": [
                { "type": "TOKENS_LIMIT", "unit": 3, "number": 5, "percentage": 16, "nextResetTime": reset_in(135) },
                { "type": "TOKENS_LIMIT", "unit": 6, "number": 1, "percentage": 4,  "nextResetTime": reset_in(3 * 24 * 60) },
                { "type": "TIME_LIMIT",   "unit": 5,              "percentage": 0,  "nextResetTime": reset_in(40 * 24 * 60) }
            ]
        } });
        let s = parse(&v, now());
        assert!(s.ok);
        // Both TOKENS_LIMIT rows are disambiguated by (unit, number) — the weekly
        // one is NOT collapsed into a second "5-hour".
        let five = s.detail.iter().find(|d| d.label == "Session").expect("session row");
        let week = s.detail.iter().find(|d| d.label == "Weekly").expect("weekly row");
        let month = s.detail.iter().find(|d| d.label == "Monthly tools").expect("monthly row");
        // The 5-hour window now shows a live countdown, not a date or nothing.
        assert_eq!(five.value, "resets in 2h 15m");
        assert_eq!(week.value, "resets in 3d 0h");
        assert_eq!(month.value, "resets in 40d 0h");
        assert_eq!(five.pct, Some(16.0));
        // Weekly present → it drives the headline.
        assert_eq!(s.secondary, "weekly quota used");
    }

    #[test]
    fn ms_reset_carried_as_a_string_still_counts_down() {
        let v = json!({ "data": { "limits": [
            { "type": "TOKENS_LIMIT", "unit": 3, "number": 5, "percentage": 10,
              "nextResetTime": reset_in(90).to_string() }
        ] } });
        let s = parse(&v, now());
        assert_eq!(s.detail[0].value, "resets in 1h 30m");
    }

    #[test]
    fn float_encoded_reset_epoch_still_counts_down() {
        // Defensive: a float-encoded epoch (…597.0) must not make the reset vanish.
        let ms = reset_in(45) as f64;
        let v = json!({ "data": { "limits": [
            { "type": "TOKENS_LIMIT", "unit": 3, "number": 5, "percentage": 20, "nextResetTime": ms }
        ] } });
        let s = parse(&v, now());
        assert_eq!(s.detail[0].value, "resets in 45m");
    }

    #[test]
    fn past_reset_epoch_is_dropped_not_rendered_as_zero() {
        // Stale data (reset already in the past) must not render "resets 0m".
        let v = json!({ "data": { "limits": [
            { "type": "TOKENS_LIMIT", "unit": 3, "number": 5, "percentage": 50,
              "nextResetTime": reset_in(-10) }
        ] } });
        let s = parse(&v, now());
        assert_eq!(s.detail[0].value, "");
        assert_eq!(s.detail[0].pct, Some(50.0));
    }

    #[test]
    fn humanizes_unknown_type() {
        assert_eq!(humanize("FOO_BAR_LIMIT"), "Foo Bar");
        assert_eq!(humanize("TOKENS_LIMIT"), "Tokens");
        let s = parse(&json!({ "data": { "limits": [
            { "type": "SOMETHING_NEW", "percentage": 5 }
        ] } }), now());
        assert!(s.ok);
        assert_eq!(s.detail[0].label, "Something New");
    }

    #[test]
    fn unrecognized_shape_is_not_ok() {
        let s = parse(&json!({ "foo": "bar" }), now());
        assert!(!s.ok);
        assert!(s.error.is_some());
    }
}
