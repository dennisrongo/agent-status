//! Anthropic Admin Usage/Cost API client.
//!
//! This reports ORG-LEVEL token/cost (requires an admin key, `sk-ant-admin…`).
//! It is NOT the Pro/Max subscription "weekly % left" — that figure has no
//! public API. The cost report is real vendor-reported spend over the window.

use chrono::{Duration, Utc};
use serde_json::Value;
use std::time::Duration as StdDuration;

use super::{KeyVal, VendorStatus};

const COST_ENDPOINT: &str = "https://api.anthropic.com/v1/organizations/cost_report";
const API_VERSION: &str = "2023-06-01";

pub async fn fetch(admin_key: &str) -> VendorStatus {
    let client = match reqwest::Client::builder()
        .timeout(StdDuration::from_secs(12))
        .build()
    {
        Ok(c) => c,
        Err(e) => return VendorStatus::failed(format!("client init: {e}")),
    };

    let starting_at = (Utc::now() - Duration::days(7))
        .format("%Y-%m-%dT00:00:00Z")
        .to_string();

    let resp = client
        .get(COST_ENDPOINT)
        .query(&[("starting_at", starting_at.as_str())])
        .header("x-api-key", admin_key)
        .header("anthropic-version", API_VERSION)
        .header("Accept", "application/json")
        .send()
        .await;

    match resp {
        Ok(r) => {
            let status = r.status();
            if !status.is_success() {
                let hint = if status.as_u16() == 401 {
                    " (needs an admin key: sk-ant-admin…)"
                } else {
                    ""
                };
                return VendorStatus::failed(format!("HTTP {}{hint}", status.as_u16()));
            }
            match r.json::<Value>().await {
                Ok(v) => parse_cost(&v),
                Err(e) => VendorStatus::failed(format!("invalid JSON: {e}")),
            }
        }
        Err(e) => VendorStatus::failed(format!("request error: {e}")),
    }
}

/// Pure parser: sum `data[].results[].amount` across buckets.
pub fn parse_cost(v: &Value) -> VendorStatus {
    let Some(buckets) = v.get("data").and_then(|d| d.as_array()) else {
        return VendorStatus {
            configured: true,
            ok: false,
            error: Some("no `data` array in cost report".to_string()),
            primary: "—".to_string(),
            secondary: "unexpected shape".to_string(),
            detail: Vec::new(),
            auth_expired: false,
        };
    };

    let mut total = 0.0_f64;
    let mut currency = "USD".to_string();
    for bucket in buckets {
        if let Some(results) = bucket.get("results").and_then(|r| r.as_array()) {
            for row in results {
                if let Some(amt) = row.get("amount").and_then(value_as_f64) {
                    total += amt;
                }
                if let Some(c) = row.get("currency").and_then(|c| c.as_str()) {
                    currency = c.to_string();
                }
            }
        }
    }

    VendorStatus {
        configured: true,
        ok: true,
        error: None,
        primary: format!("${:.2}", total),
        secondary: format!("7-day org cost ({currency})"),
        detail: vec![KeyVal::text("Reported spend", format!("${:.2}", total))],
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sums_amounts_across_buckets() {
        let v = json!({
            "data": [
                { "results": [ { "amount": "1.50", "currency": "USD" } ] },
                { "results": [ { "amount": 2.25, "currency": "USD" }, { "amount": "0.25" } ] }
            ],
            "has_more": false
        });
        let s = parse_cost(&v);
        assert!(s.ok);
        assert_eq!(s.primary, "$4.00");
    }

    #[test]
    fn missing_data_is_error() {
        let s = parse_cost(&json!({ "oops": true }));
        assert!(!s.ok);
        assert!(s.error.is_some());
    }

    #[test]
    fn handles_numeric_and_string_amounts() {
        // A mix of numeric and string amounts must all be summed.
        let v = json!({
            "data": [
                { "results": [ { "amount": 10.5, "currency": "USD" }, { "amount": "0.50" } ] }
            ]
        });
        let s = parse_cost(&v);
        assert!(s.ok);
        assert_eq!(s.primary, "$11.00");
    }

    #[test]
    fn reports_currency_from_results() {
        let v = json!({
            "data": [
                { "results": [ { "amount": 5.0, "currency": "EUR" } ] }
            ]
        });
        let s = parse_cost(&v);
        assert!(s.ok);
        assert_eq!(s.secondary, "7-day org cost (EUR)");
    }

    #[test]
    fn empty_results_array_is_zero() {
        let v = json!({ "data": [{ "results": [] }] });
        let s = parse_cost(&v);
        assert!(s.ok);
        assert_eq!(s.primary, "$0.00");
    }

    #[test]
    fn bucket_without_results_is_skipped() {
        // A bucket with no `results` key should not cause a panic.
        let v = json!({
            "data": [
                { "other": true },
                { "results": [ { "amount": 3.0 } ] }
            ]
        });
        let s = parse_cost(&v);
        assert!(s.ok);
        assert_eq!(s.primary, "$3.00");
    }

    #[test]
    fn result_without_amount_is_skipped() {
        let v = json!({
            "data": [
                { "results": [ { "currency": "USD" }, { "amount": 2.0 } ] }
            ]
        });
        let s = parse_cost(&v);
        assert!(s.ok);
        assert_eq!(s.primary, "$2.00");
    }

    #[test]
    fn detail_includes_reported_spend() {
        let v = json!({
            "data": [{ "results": [ { "amount": 42.0, "currency": "USD" } ] }]
        });
        let s = parse_cost(&v);
        assert!(s.ok);
        assert_eq!(s.detail.len(), 1);
        assert_eq!(s.detail[0].label, "Reported spend");
        assert_eq!(s.detail[0].value, "$42.00");
    }
}
