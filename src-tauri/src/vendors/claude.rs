//! Claude (Anthropic) LIVE subscription usage — the same data Claude Code's
//! status bar / `/usage` shows. Reads the OAuth token Claude Code stored
//! (macOS keychain `Claude Code-credentials`, else `~/.claude/.credentials.json`)
//! and calls `GET https://api.anthropic.com/api/oauth/usage`.
//!
//! This is an UNDOCUMENTED endpoint used with the user's own subscription token,
//! at the user's request. If the token is missing/expired we fall back silently.

use chrono::{DateTime, Utc};
use serde_json::Value;
use std::time::Duration as StdDuration;

use crate::scanner::Bucket;

const ENDPOINT: &str = "https://api.anthropic.com/api/oauth/usage";
const OAUTH_BETA: &str = "oauth-2025-04-20";

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeLive {
    /// An OAuth token was found.
    pub configured: bool,
    /// Fetch + parse succeeded.
    pub ok: bool,
    pub error: Option<String>,
    pub buckets: Vec<Bucket>,
}

impl ClaudeLive {
    fn off(configured: bool, error: Option<String>) -> Self {
        Self { configured, ok: false, error, buckets: Vec::new() }
    }
}

/// Whether a usable Claude Code install is present: a stored login token, or
/// the `claude` CLI somewhere on PATH. Cheap, no process spawn.
pub fn detected() -> bool {
    read_token().is_some() || cli_on_path()
}

fn cli_on_path() -> bool {
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
    #[cfg(target_os = "macos")]
    if let Some(t) = read_token_macos() {
        return Some(t);
    }
    read_token_file()
}

#[cfg(target_os = "macos")]
fn read_token_macos() -> Option<String> {
    let out = std::process::Command::new("security")
        .args(["find-generic-password", "-s", "Claude Code-credentials", "-w"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let raw = String::from_utf8(out.stdout).ok()?;
    parse_token_json(&raw)
}

fn read_token_file() -> Option<String> {
    let home = dirs::home_dir()?;
    let raw = std::fs::read_to_string(home.join(".claude").join(".credentials.json")).ok()?;
    parse_token_json(&raw)
}

fn parse_token_json(raw: &str) -> Option<String> {
    let v: Value = serde_json::from_str(raw.trim()).ok()?;
    v.get("claudeAiOauth")
        .and_then(|o| o.get("accessToken"))
        .and_then(|t| t.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
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
                let hint = if status.as_u16() == 401 {
                    " (Claude Code login expired — open Claude Code to re-auth)"
                } else {
                    ""
                };
                return ClaudeLive::off(true, Some(format!("HTTP {}{hint}", status.as_u16())));
            }
            match r.json::<Value>().await {
                Ok(v) => {
                    let buckets = parse(&v, now);
                    if buckets.is_empty() {
                        ClaudeLive::off(true, Some("no usage windows in response".into()))
                    } else {
                        ClaudeLive { configured: true, ok: true, error: None, buckets }
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
}
