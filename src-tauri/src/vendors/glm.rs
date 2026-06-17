//! z.ai (GLM / Zhipu) usage client.
//!
//! z.ai's billing endpoint is account/region-specific, so the base URL is
//! configurable in settings. We issue an authenticated GET and parse the JSON
//! defensively for common balance/usage field names. The parser is pure and
//! tested; the live endpoint must be confirmed against the user's account.

use serde_json::Value;
use std::time::Duration;

use super::{KeyVal, VendorStatus};

pub const DEFAULT_ENDPOINT: &str = "https://api.z.ai/api/paas/v4/usage";

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
        .bearer_auth(api_key)
        .header("Accept", "application/json")
        .send()
        .await;

    match resp {
        Ok(r) => {
            let status = r.status();
            if !status.is_success() {
                return VendorStatus::failed(format!("HTTP {} from {url}", status.as_u16()));
            }
            match r.json::<Value>().await {
                Ok(v) => parse(&v),
                Err(e) => VendorStatus::failed(format!("invalid JSON: {e}")),
            }
        }
        Err(e) => VendorStatus::failed(format!("request error: {e}")),
    }
}

/// Pure parser — pulls balance / usage from a variety of likely shapes.
pub fn parse(v: &Value) -> VendorStatus {
    // Some APIs nest under "data".
    let root = if v.get("data").map(|d| d.is_object()).unwrap_or(false) {
        &v["data"]
    } else {
        v
    };

    let num = |keys: &[&str]| -> Option<f64> {
        for k in keys {
            if let Some(n) = root.get(*k).and_then(value_as_f64) {
                return Some(n);
            }
        }
        None
    };

    let balance = num(&["balance", "remaining", "available_balance", "credit"]);
    let used = num(&["total_usage", "usage", "used", "total_cost", "amount"]);
    let tokens = num(&["total_tokens", "tokens", "prompt_tokens"]);

    if balance.is_none() && used.is_none() && tokens.is_none() {
        return VendorStatus {
            configured: true,
            ok: false,
            error: Some("no recognized balance/usage fields in response".to_string()),
            primary: "—".to_string(),
            secondary: "unexpected shape".to_string(),
            detail: Vec::new(),
        };
    }

    let mut detail = Vec::new();
    if let Some(b) = balance {
        detail.push(KeyVal { label: "Balance".to_string(), value: fmt_money(b) });
    }
    if let Some(u) = used {
        detail.push(KeyVal { label: "Used".to_string(), value: fmt_money(u) });
    }
    if let Some(t) = tokens {
        detail.push(KeyVal { label: "Tokens".to_string(), value: fmt_count(t) });
    }

    let primary = balance
        .map(fmt_money)
        .or_else(|| used.map(fmt_money))
        .or_else(|| tokens.map(fmt_count))
        .unwrap_or_else(|| "—".to_string());

    VendorStatus {
        configured: true,
        ok: true,
        error: None,
        primary,
        secondary: if balance.is_some() { "balance".to_string() } else { "usage".to_string() },
        detail,
    }
}

fn value_as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.parse::<f64>().ok(),
        _ => None,
    }
}

fn fmt_money(n: f64) -> String {
    format!("${:.2}", n)
}

fn fmt_count(n: f64) -> String {
    if n >= 1e6 {
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
    fn parses_balance_shape() {
        let v = json!({ "balance": 42.5, "total_usage": 7.25 });
        let s = parse(&v);
        assert!(s.ok);
        assert_eq!(s.primary, "$42.50");
        assert_eq!(s.detail.len(), 2);
    }

    #[test]
    fn parses_nested_data_and_string_numbers() {
        let v = json!({ "data": { "remaining": "13.00", "total_tokens": 1500000 } });
        let s = parse(&v);
        assert!(s.ok);
        assert_eq!(s.primary, "$13.00");
        assert!(s.detail.iter().any(|d| d.value == "1.5M"));
    }

    #[test]
    fn unrecognized_shape_is_not_ok() {
        let v = json!({ "foo": "bar" });
        let s = parse(&v);
        assert!(!s.ok);
        assert!(s.error.is_some());
    }
}
