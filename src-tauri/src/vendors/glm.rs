//! z.ai (GLM Coding Plan) usage client.
//!
//! Uses z.ai's monitor API: `GET /api/monitor/usage/quota/limit`. The coding-plan
//! token is passed in the `Authorization` header WITHOUT a `Bearer` prefix.
//! Response shape: `{ "data": { "limits": [ { type, percentage, currentValue,
//! total, nextResetTime, ... } ] } }`. The live `type` values are `TOKENS_LIMIT`
//! (z.ai's "5 Hours Quota" — the rolling 5-hour coding window) and `TIME_LIMIT`
//! (z.ai's "Total Monthly Web Search / Reader / Zread Quota" — the monthly tool
//! quota); the names don't match their windows, so `parse` maps by meaning.
//! The base URL is configurable (z.ai global vs. open.bigmodel.cn for CN).

use serde_json::Value;
use std::time::Duration;

use super::{short_date, KeyVal, VendorStatus};

pub const DEFAULT_ENDPOINT: &str = "https://api.z.ai/api/monitor/usage/quota/limit";

/// Endpoints from older builds that should be upgraded to DEFAULT_ENDPOINT.
pub const STALE_ENDPOINTS: [&str; 2] = [
    "https://api.z.ai/api/paas/v4/usage",
    "https://open.bigmodel.cn/api/paas/v4/usage",
];

pub async fn fetch(api_key: &str, endpoint: &str) -> VendorStatus {
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
                Ok(v) => parse(&v),
                Err(e) => VendorStatus::failed(format!("invalid JSON: {e}")),
            }
        }
        Err(e) => VendorStatus::failed(format!("request error: {e}")),
    }
}

/// Pure parser for the monitor quota/limit response.
pub fn parse(v: &Value) -> VendorStatus {
    let root = if v.get("data").map(|d| d.is_object()).unwrap_or(false) {
        &v["data"]
    } else {
        v
    };

    let Some(limits) = root.get("limits").and_then(|l| l.as_array()) else {
        return shape_error("no `data.limits` array in response");
    };

    let mut detail = Vec::new();
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

        // z.ai's live monitor returns `TOKENS_LIMIT` for the 5-hour coding quota
        // (z.ai's "5 Hours Quota") and `TIME_LIMIT` for the monthly tool quota
        // (z.ai's "Total Monthly Web Search / Reader / Zread Quota"). Older/other
        // shapes may use "5h"/"weekly"/"mcp". Map them all to clean, Claude-style
        // labels; never surface the raw ALL_CAPS identifier.
        let tl = typ.to_lowercase();
        let is_week = tl.contains("week");
        // Match "time" only as a whole word so "runtime"/"real-time" aren't
        // misread as the monthly window; z.ai's real type is a standalone
        // TIME_LIMIT.
        let is_month = tl.contains("month")
            || tl.split(|c: char| !c.is_alphanumeric()).any(|w| w == "time");
        let is_5h = tl.contains("5h")
            || tl.contains("5 h")
            || tl.contains("5-h")
            // A bare token-count limit is the 5-hour coding window, unless the
            // type already names a longer window.
            || (tl.contains("token") && !is_week && !is_month);

        let label: String = if is_5h {
            "5-hour".to_string()
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

        // Faint right-aligned slot, parallel to Claude's "resets X". Prefer the
        // reset date (z.ai gives one on the monthly window); else the used/total
        // amount. Only string timestamps are formatted — an epoch number is left
        // unshown rather than risk a wrong date. The percentage drives the bar.
        let reset = lim
            .get("nextResetTime")
            .and_then(|d| d.as_str())
            .map(short_date)
            .filter(|s| !s.is_empty());

        let Some(p) = pct else { continue };
        let value = match (&reset, used, total) {
            (Some(r), _, _) => format!("resets {r}"),
            (None, Some(u), Some(t)) => format!("{} / {}", fmt_count(u), fmt_count(t)),
            _ => String::new(),
        };
        detail.push(KeyVal::meter(&label, value, p));
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

    // Headline: weekly usage if present, else the 5-hour window, else the
    // monthly tool quota.
    let (used, label) = if let Some(w) = weekly {
        (w, "weekly quota used")
    } else if let Some(f) = five_h {
        (f, "5-hour quota used")
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

    #[test]
    fn parses_five_hour_and_weekly_quota() {
        let v = json!({
            "data": { "limits": [
                { "type": "Token usage(5h)", "percentage": 40, "currentValue": 16000000, "total": 40000000 },
                { "type": "Token usage(Weekly)", "percentage": 12.5, "currentValue": 50000000, "total": 400000000 }
            ] }
        });
        let s = parse(&v);
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
        let s = parse(&v);
        assert!(s.ok);
        assert_eq!(s.secondary, "5-hour quota used");
    }

    #[test]
    fn computes_percentage_when_absent() {
        let v = json!({ "data": { "limits": [
            { "type": "Token usage(Weekly)", "currentValue": "100", "total": "400" }
        ] } });
        let s = parse(&v);
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
        let s = parse(&v);
        assert!(s.ok);
        let labels: Vec<_> = s.detail.iter().map(|d| d.label.as_str()).collect();
        assert!(labels.contains(&"Monthly tools"), "got {labels:?}");
        assert!(labels.contains(&"5-hour"), "got {labels:?}");
        // 5-hour is the coding throttle → it drives the headline.
        assert_eq!(s.secondary, "5-hour quota used");
    }

    #[test]
    fn non_finite_percentage_is_skipped() {
        // A hostile/garbled `"NaN"` must not render as "NaN% used" or a bogus
        // "danger" meter; the only limit is dropped → no usable rows.
        let v = json!({ "data": { "limits": [
            { "type": "TOKENS_LIMIT", "percentage": "NaN" }
        ] } });
        let s = parse(&v);
        assert!(!s.ok);
    }

    #[test]
    fn meter_pct_is_clamped() {
        let v = json!({ "data": { "limits": [
            { "type": "TOKENS_LIMIT", "percentage": 250 }
        ] } });
        let s = parse(&v);
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
        let s = parse(&v);
        assert!(s.ok);
        assert_eq!(s.detail[0].label, "Runtime");
    }

    #[test]
    fn reset_time_fills_the_faint_slot() {
        let v = json!({ "data": { "limits": [
            { "type": "TIME_LIMIT", "percentage": 1, "nextResetTime": "2026-06-24 17:13:00" }
        ] } });
        let s = parse(&v);
        let m = s.detail.iter().find(|d| d.label == "Monthly tools").unwrap();
        assert_eq!(m.value, "resets 2026-06-24");
    }

    #[test]
    fn humanizes_unknown_type() {
        assert_eq!(humanize("FOO_BAR_LIMIT"), "Foo Bar");
        assert_eq!(humanize("TOKENS_LIMIT"), "Tokens");
        let s = parse(&json!({ "data": { "limits": [
            { "type": "SOMETHING_NEW", "percentage": 5 }
        ] } }));
        assert!(s.ok);
        assert_eq!(s.detail[0].label, "Something New");
    }

    #[test]
    fn unrecognized_shape_is_not_ok() {
        let s = parse(&json!({ "foo": "bar" }));
        assert!(!s.ok);
        assert!(s.error.is_some());
    }
}
