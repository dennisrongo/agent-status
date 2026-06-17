//! z.ai (GLM Coding Plan) usage client.
//!
//! Uses z.ai's monitor API: `GET /api/monitor/usage/quota/limit`. The coding-plan
//! token is passed in the `Authorization` header WITHOUT a `Bearer` prefix.
//! Response shape: `{ "data": { "limits": [ { type, percentage, currentValue,
//! total, nextResetTime, ... } ] } }` with 5-hour and weekly token windows.
//! The base URL is configurable (z.ai global vs. open.bigmodel.cn for CN).

use serde_json::Value;
use std::time::Duration;

use super::{KeyVal, VendorStatus};

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
            });

        let tl = typ.to_lowercase();
        let (label, is_5h, is_week) = if tl.contains("5h") || tl.contains("5 h") || tl.contains("5-h")
        {
            ("5-hour", true, false)
        } else if tl.contains("week") {
            ("Weekly", false, true)
        } else if tl.contains("mcp") {
            ("MCP", false, false)
        } else if typ.is_empty() {
            continue;
        } else {
            (typ, false, false)
        };

        let Some(p) = pct else { continue };
        let value = match (used, total) {
            (Some(u), Some(t)) => format!("{} / {} · {:.0}%", fmt_count(u), fmt_count(t), p),
            _ => format!("{:.0}% used", p),
        };
        detail.push(KeyVal { label: label.to_string(), value });
        if is_5h {
            five_h = Some(p);
        }
        if is_week {
            weekly = Some(p);
        }
    }

    if detail.is_empty() {
        return shape_error("no recognized quota limits in response");
    }

    // Headline: weekly usage if present, else the 5-hour window.
    let (used, label) = match (weekly, five_h) {
        (Some(w), _) => (w, "weekly quota used"),
        (None, Some(f)) => (f, "5-hour quota used"),
        _ => (0.0, "quota"),
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
        // 100/400 = 25% used -> 75% left
        assert!(s.detail[0].value.contains("25%"));
    }

    #[test]
    fn unrecognized_shape_is_not_ok() {
        let s = parse(&json!({ "foo": "bar" }));
        assert!(!s.ok);
        assert!(s.error.is_some());
    }
}
