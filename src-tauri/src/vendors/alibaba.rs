//! Alibaba Cloud Model Studio (Bailian) usage client.
//!
//! Shells out to the `bl` CLI (`bailian-cli`) to read free-tier quota and
//! usage statistics. The CLI authenticates via its own config
//! (`~/.bailian/config.json`) — no API key is stored by this app. Detection
//! checks PATH, the npm global bin directory, and common install locations.

use serde_json::Value;
use std::path::PathBuf;
use std::sync::OnceLock;

use chrono::{DateTime, Utc};

use crate::process_util::SilentCommand;
use super::{KeyVal, VendorStatus};

/// Cached npm global prefix — the `npm config get prefix` subprocess is
/// expensive, so we run it at most once per process lifetime. The directory
/// itself doesn't change mid-session; we still stat for `bl` on every call so
/// a fresh install is picked up.
static NPM_PREFIX: OnceLock<Option<PathBuf>> = OnceLock::new();

fn npm_global_prefix() -> Option<PathBuf> {
    NPM_PREFIX
        .get_or_init(|| {
            let out = std::process::Command::new("npm")
                .args(["config", "get", "prefix"])
                .silent()
                .output()
                .ok()?;
            if !out.status.success() {
                return None;
            }
            let prefix = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if prefix.is_empty() {
                return None;
            }
            Some(PathBuf::from(prefix))
        })
        .clone()
}

/// Locate the `bl` (Bailian CLI) binary. Checks PATH first, then the npm
/// global prefix, then well-known install directories. Returns the path so
/// callers can invoke it directly even when it isn't on PATH.
///
/// On Windows the candidate list skips the extensionless `bl` (an sh script
/// npm ships for Git Bash) — CreateProcess can't run it and it sorts before
/// `bl.cmd`. On Unix the extensionless `bl` is the real binary.
pub fn find_cli() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    const NAMES: &[&str] = &["bl.exe", "bl.cmd"];
    #[cfg(not(target_os = "windows"))]
    const NAMES: &[&str] = &["bl"];

    // 1. PATH scan (covers most installs).
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

    // 2. npm global prefix (cached — the subprocess runs at most once).
    if let Some(base) = npm_global_prefix() {
        // npm puts binaries in <prefix>/ on Windows, <prefix>/bin/ on Unix.
        for name in NAMES {
            let candidate = base.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            let bin = base.join("bin");
            if bin.join("bl").is_file() {
                return Some(bin.join("bl"));
            }
        }
    }

    // 3. Well-known fallback: %APPDATA%\npm on Windows.
    #[cfg(target_os = "windows")]
    if let Some(appdata) = std::env::var_os("APPDATA") {
        let candidate = PathBuf::from(appdata).join("npm").join("bl.cmd");
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    None
}

/// Whether the `bl` (Bailian CLI) binary is reachable.
pub fn cli_on_path() -> bool {
    find_cli().is_some()
}

/// Build a Command for the Bailian CLI, handling Windows .cmd wrappers.
/// On Windows, npm global binaries are .cmd shims that CreateProcess can't
/// execute directly — they must go through cmd.exe /C. `.silent()` suppresses
/// the console window on every `bl` invocation (auth status, login, and the
/// usage fetches that fire on every refresh tick).
fn bl_command(cli: &std::path::Path) -> std::process::Command {
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

/// What the Settings UI shows about the Bailian CLI: installed? authenticated?
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CliStatus {
    pub installed: bool,
    pub authenticated: bool,
    /// Masked API key or console token hint (never the full credential).
    pub auth_hint: Option<String>,
}

/// Query the CLI's own auth status (`bl auth status --output json`).
pub fn auth_status() -> CliStatus {
    let Some(cli) = find_cli() else {
        return CliStatus { installed: false, authenticated: false, auth_hint: None };
    };

    let out = bl_command(&cli)
        .args(["auth", "status", "--output", "json"])
        .output();

    let Ok(out) = out else {
        return CliStatus { installed: true, authenticated: false, auth_hint: None };
    };

    let Ok(v) = serde_json::from_slice::<Value>(&out.stdout) else {
        return CliStatus { installed: true, authenticated: false, auth_hint: None };
    };

    let authenticated = v.get("authenticated").and_then(|a| a.as_bool()).unwrap_or(false);
    // Build a hint from the masked key or console token — never the real value.
    let auth_hint = v
        .get("console")
        .and_then(|c| c.get("masked"))
        .and_then(|m| m.as_str())
        .map(|m| format!("console · {m}"))
        .or_else(|| {
            v.get("api_key")
                .and_then(|k| k.get("masked"))
                .and_then(|m| m.as_str())
                .map(|m| format!("api key · {m}"))
        });

    CliStatus { installed: true, authenticated, auth_hint }
}

/// Run `bl auth login --console` to authenticate via the browser. The CLI
/// opens a browser for the user to complete the OAuth flow; this blocks until
/// they finish (or cancel). Returns a human-readable result.
pub fn login() -> Result<String, String> {
    let Some(cli) = find_cli() else {
        return Err("Bailian CLI not found — install it first.".to_string());
    };

    let out = bl_command(&cli)
        .args(["auth", "login", "--console"])
        .output()
        .map_err(|e| format!("spawn: {e}"))?;

    if out.status.success() {
        Ok("Authenticated with Alibaba Cloud. Usage will appear on the next refresh.".to_string())
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let detail = if !stderr.trim().is_empty() {
            stderr.trim().to_string()
        } else if !stdout.trim().is_empty() {
            stdout.trim().to_string()
        } else {
            format!("exit code {}", out.status.code().unwrap_or(-1))
        };
        Err(format!("Login failed: {detail}"))
    }
}

/// Install the Bailian CLI globally via npm. Returns a human-readable result.
pub fn install() -> Result<String, String> {
    // Verify npm is available first.
    let npm_ok = std::process::Command::new("npm")
        .arg("--version")
        .silent()
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !npm_ok {
        return Err(
            "npm not found — install Node.js (≥ 22.12) first: https://nodejs.org".to_string(),
        );
    }

    let out = std::process::Command::new("npm")
        .args(["install", "-g", "bailian-cli"])
        .silent()
        .output()
        .map_err(|e| format!("npm spawn failed: {e}"))?;

    if out.status.success() {
        // Verify the binary is now reachable.
        if find_cli().is_some() {
            Ok("Bailian CLI installed. Run `bl auth login --console` in a terminal to authenticate.".to_string())
        } else {
            Ok("Installed, but `bl` isn't on PATH yet — restart the app or add the npm global bin to PATH.".to_string())
        }
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr);
        Err(format!("npm install failed: {}", stderr.trim()))
    }
}

/// Run `bl usage summary`, `bl usage stats --days 1`, `bl usage free`,
/// `bl quota check`, and the Token Plan usage API (all `--output json`),
/// then merge into a single VendorStatus. Called from a blocking task.
pub fn fetch() -> VendorStatus {
    let Some(cli) = find_cli() else {
        return VendorStatus::not_configured();
    };
    let summary = run_bl(&cli, &["usage", "summary", "--output", "json"]);
    // "Today" window — best-effort; the 7-day summary still works without it.
    let today = run_bl(&cli, &["usage", "stats", "--days", "1", "--output", "json"]).ok();
    let free = run_bl(&cli, &["usage", "free", "--output", "json"]);
    // Rate-limit headroom is best-effort — a failure here shouldn't blank the
    // whole card when the usage commands succeeded.
    let quota = run_bl(&cli, &["quota", "check", "--output", "json"]).ok();
    // Token Plan 5h/7d quota — the same percentages the console website shows.
    let plan = run_bl(&cli, &[
        "console", "call",
        "--api", "zeldaHttp.apikeyMgr./tokenplan/personal/api/v2/usage",
        "--data", "{}",
        "--output", "json",
    ]).ok();

    match (summary, free) {
        (Ok(s), Ok(f)) => parse(&s, today.as_ref(), &f, quota.as_ref(), plan.as_ref(), Utc::now()),
        (Err(e), _) => VendorStatus::failed(format!("bl usage summary: {e}")),
        (_, Err(e)) => VendorStatus::failed(format!("bl usage free: {e}")),
    }
}

fn run_bl(cli: &std::path::Path, args: &[&str]) -> Result<Value, String> {
    let out = bl_command(cli)
        .args(args)
        .output()
        .map_err(|e| format!("spawn: {e}"))?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        // The CLI writes structured JSON errors to stdout even on failure.
        let stdout = String::from_utf8_lossy(&out.stdout);
        let hint = if let Ok(v) = serde_json::from_str::<Value>(&stdout) {
            v.get("error")
                .and_then(|e| e.get("hint"))
                .and_then(|h| h.as_str())
                .map(|h| format!(" ({h})"))
                .unwrap_or_default()
        } else {
            String::new()
        };
        return Err(format!(
            "exit {}{hint}: {}",
            out.status.code().unwrap_or(-1),
            stderr.trim()
        ));
    }

    serde_json::from_str(&String::from_utf8_lossy(&out.stdout))
        .map_err(|e| format!("invalid JSON: {e}"))
}

/// Snapshot of one time window's headline numbers.
struct WindowStats {
    total_tokens: Option<f64>,
    input_tokens: Option<f64>,
    output_tokens: Option<f64>,
    calls: Option<f64>,
    models: Option<f64>,
}

/// Extract headline stats from a `usage stats` or `usage summary` response.
/// `usage stats` puts fields at the top level; `usage summary` nests them
/// under `"usage"`.
fn extract_window(v: &Value) -> WindowStats {
    let usage = v.get("usage").unwrap_or(v);
    let usages = usage.get("usages").and_then(|u| u.as_array());

    let mut total_tokens = None;
    let mut input_tokens = None;
    let mut output_tokens = None;

    if let Some(arr) = usages {
        for item in arr {
            let key = item.get("key").and_then(|k| k.as_str()).unwrap_or("");
            let val = item.get("value").and_then(value_as_f64);
            match key {
                "total_token" => total_tokens = val,
                "input_token" => input_tokens = val,
                "output_token" => output_tokens = val,
                _ => {}
            }
        }
    }
    // Fallback for older CLI builds that put totalTokens at the top level.
    if total_tokens.is_none() {
        total_tokens = usage.get("totalTokens").and_then(value_as_f64);
    }

    WindowStats {
        total_tokens,
        input_tokens,
        output_tokens,
        calls: v
            .get("successfulCalls")
            .or_else(|| usage.get("successfulCalls"))
            .and_then(value_as_f64),
        models: v
            .get("modelsCalled")
            .or_else(|| usage.get("modelsCalled"))
            .and_then(value_as_f64),
    }
}

/// Pure parser: merges the `usage summary` (7-day), optional `usage stats
/// --days 1` (today), `usage free`, optional `quota check`, and optional
/// Token Plan usage JSON outputs into a single VendorStatus. `now` is passed
/// in so reset countdowns are deterministic (matching GLM's pattern).
pub fn parse(
    summary: &Value,
    today: Option<&Value>,
    free: &Value,
    quota: Option<&Value>,
    plan: Option<&Value>,
    now: DateTime<Utc>,
) -> VendorStatus {
    let mut detail: Vec<KeyVal> = Vec::new();

    // ── Token Plan quota (5-hour / 7-day windows) — the same percentages
    //    the Alibaba Cloud console shows. These are the primary meters.
    let mut has_plan_quota = false;
    if let Some(plan) = plan {
        // Response: data.DataV2.data.data.{per5HourPercentage, per5HourResetTime, …}
        let d = plan
            .get("data")
            .and_then(|d| d.get("DataV2"))
            .and_then(|d| d.get("data"))
            .and_then(|d| d.get("data"));
        if let Some(d) = d {
            let pct_5h = d.get("per5HourPercentage").and_then(value_as_f64);
            let pct_7d = d.get("per1WeekPercentage").and_then(value_as_f64);
            let reset_5h = d.get("per5HourResetTime").and_then(value_as_f64);
            let reset_7d = d.get("per1WeekResetTime").and_then(value_as_f64);

            if let Some(p) = pct_5h {
                let reset_label = reset_5h
                    .and_then(|ms| countdown_ms(ms as i64, now))
                    .map(|r| format!("resets in {r}"))
                    .unwrap_or_default();
                detail.push(KeyVal::meter("5 hours", reset_label, p * 100.0));
                has_plan_quota = true;
            }
            if let Some(p) = pct_7d {
                let reset_label = reset_7d
                    .and_then(|ms| countdown_ms(ms as i64, now))
                    .map(|r| format!("resets in {r}"))
                    .unwrap_or_default();
                detail.push(KeyVal::meter("7 days", reset_label, p * 100.0));
                has_plan_quota = true;
            }
        }
    }

    // ── Free-tier quota rows — each entry is a model with used/total tokens.
    let empty: Vec<Value> = Vec::new();
    let free_arr = free.as_array().unwrap_or(&empty);
    let mut has_quota = false;
    for item in free_arr {
        let model = item
            .get("model")
            .or_else(|| item.get("modelName"))
            .and_then(|m| m.as_str())
            .unwrap_or("unknown");
        let used = item.get("used").or_else(|| item.get("usedTokens")).and_then(value_as_f64);
        let total = item.get("total").or_else(|| item.get("totalTokens")).and_then(value_as_f64);
        let pct = item
            .get("percentage")
            .or_else(|| item.get("pct"))
            .and_then(value_as_f64)
            .or_else(|| match (used, total) {
                (Some(u), Some(t)) if t > 0.0 => Some(u / t * 100.0),
                _ => None,
            })
            .filter(|p| p.is_finite());

        if let Some(p) = pct {
            let value = match (used, total) {
                (Some(u), Some(t)) => format!("{} / {}", fmt_count(u), fmt_count(t)),
                _ => String::new(),
            };
            detail.push(KeyVal::meter(model, value, p));
            has_quota = true;
        } else if let (Some(u), Some(t)) = (used, total) {
            detail.push(KeyVal::text(model, format!("{} / {}", fmt_count(u), fmt_count(t))));
        }
    }

    // ── Time-windowed usage (Today + 7 days).
    let week = extract_window(summary);
    let today_win = today.map(extract_window);

    // Window summary rows — only when there's no plan quota (the plan meters
    // already cover the 5h/7d windows with real percentages).
    if !has_plan_quota {
        if let Some(tw) = &today_win {
            let tok = tw.total_tokens.unwrap_or(0.0);
            let calls = tw.calls.unwrap_or(0.0) as u64;
            detail.push(KeyVal::text("Today", format!("{} tokens · {} calls", fmt_count(tok), calls)));
        }
        {
            let tok = week.total_tokens.unwrap_or(0.0);
            let calls = week.calls.unwrap_or(0.0) as u64;
            detail.push(KeyVal::text("7 days", format!("{} tokens · {} calls", fmt_count(tok), calls)));
        }
    }

    // Token breakdown from the 7-day window.
    if let Some(inp) = week.input_tokens {
        detail.push(KeyVal::text("Input tokens", fmt_count(inp)));
    }
    if let Some(outp) = week.output_tokens {
        detail.push(KeyVal::text("Output tokens", fmt_count(outp)));
    }
    if let Some(mc) = week.models {
        detail.push(KeyVal::text("Models called", format!("{}", mc as u64)));
    }

    // Cost (only present in some CLI builds).
    let usage_obj = summary.get("usage");
    let total_cost = usage_obj.and_then(|u| u.get("totalCost")).and_then(value_as_f64);
    if let Some(c) = total_cost {
        detail.push(KeyVal::text("Cost", format!("¥{:.2}", c)));
    }

    // ── Rate-limit headroom from `quota check` (best-effort).
    let mut models_with_usage = 0u64;
    let mut models_total = 0u64;
    if let Some(q) = quota.and_then(|q| q.as_array()) {
        for item in q {
            models_total += 1;
            let model = item.get("model").and_then(|m| m.as_str()).unwrap_or("unknown");
            let tpm_usage = item.get("tpmUsage").and_then(value_as_f64).unwrap_or(0.0);
            let tpm_limit = item.get("tpmLimit").and_then(value_as_f64).unwrap_or(0.0);
            let rpm_usage = item.get("rpmUsage").and_then(value_as_f64).unwrap_or(0.0);
            let rpm_limit = item.get("rpmLimit").and_then(value_as_f64).unwrap_or(0.0);

            if tpm_usage > 0.0 || rpm_usage > 0.0 {
                models_with_usage += 1;
                // Plain text rows — these are per-minute throughput rates, not
                // cumulative quota windows, so they must NOT carry a pct (which
                // would promote them into the frontend's KPI tiles alongside the
                // plan-quota meters and skew the headline percentage).
                let tpm_val = format!("{} / {} TPM", fmt_count(tpm_usage), fmt_count(tpm_limit));
                detail.push(KeyVal::text(format!("{model} TPM"), tpm_val));

                if rpm_usage > 0.0 && rpm_limit > 0.0 {
                    let rpm_val = format!("{} / {} RPM", fmt_count(rpm_usage), fmt_count(rpm_limit));
                    detail.push(KeyVal::text(format!("{model} RPM"), rpm_val));
                }
            }
        }
        if models_total > 0 && models_with_usage == 0 {
            detail.push(KeyVal::text(
                "Rate limits",
                format!("{models_total} models · all within limits"),
            ));
        }
    }

    // ── Period label.
    let period = summary.get("period");
    let period_label = period
        .and_then(|p| {
            let s = p.get("start")?.as_str()?;
            let e = p.get("end")?.as_str()?;
            Some(format!("{s} → {e}"))
        })
        .unwrap_or_else(|| "last 7 days".to_string());

    if detail.is_empty() {
        return VendorStatus {
            configured: true,
            ok: true,
            error: None,
            primary: "0".to_string(),
            secondary: format!("no usage · {period_label}"),
            detail: Vec::new(),
        };
    }

    let primary = if has_plan_quota || has_quota {
        let max_pct = detail
            .iter()
            .filter_map(|d| d.pct)
            .fold(0.0_f64, f64::max);
        format!("{:.0}% used", max_pct)
    } else if let Some(t) = week.total_tokens {
        format!("{} tokens", fmt_count(t))
    } else if let Some(sc) = week.calls {
        format!("{} calls", sc as u64)
    } else {
        "active".to_string()
    };

    VendorStatus {
        configured: true,
        ok: true,
        error: None,
        primary,
        secondary: period_label,
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

/// Format a reset epoch (ms) as a compact countdown from `now`, matching
/// Claude's "4h 12m" / "2d 3h" style. Returns `None` for a past reset.
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
        Utc::now()
    }

    #[test]
    fn parses_empty_usage() {
        let summary = json!({
            "period": { "start": "2026-07-12", "end": "2026-07-19", "days": 7 },
            "freeTier": [],
            "usage": null
        });
        let free = json!([]);
        let s = parse(&summary, None, &free, None, None, now());
        assert!(s.ok);
        assert!(s.detail.iter().any(|d| d.label == "7 days"));
    }

    #[test]
    fn parses_free_tier_quota() {
        let summary = json!({
            "period": { "start": "2026-07-12", "end": "2026-07-19", "days": 7 },
            "freeTier": [],
            "usage": null
        });
        let free = json!([
            { "model": "qwen-turbo", "used": 500000, "total": 1000000, "percentage": 50 },
            { "model": "qwen-plus", "used": 100000, "total": 500000 }
        ]);
        let s = parse(&summary, None, &free, None, None, now());
        assert!(s.ok);
        assert_eq!(s.primary, "50% used");
        assert_eq!(s.detail[0].label, "qwen-turbo");
        assert_eq!(s.detail[0].pct, Some(50.0));
        assert_eq!(s.detail[1].label, "qwen-plus");
        assert_eq!(s.detail[1].pct, Some(20.0));
    }

    #[test]
    fn parses_usage_stats_top_level() {
        let summary = json!({
            "period": { "start": "2026-06-19", "end": "2026-07-19", "days": 30 },
            "modelsCalled": 3,
            "successfulCalls": 142
        });
        let free = json!([]);
        let s = parse(&summary, None, &free, None, None, now());
        assert!(s.ok);
        assert_eq!(s.primary, "142 calls");
        assert!(s.detail.iter().any(|d| d.label == "7 days" && d.value.contains("142 calls")));
        assert!(s.detail.iter().any(|d| d.label == "Models called" && d.value == "3"));
    }

    #[test]
    fn parses_usage_with_tokens_and_cost() {
        let summary = json!({
            "period": { "start": "2026-07-12", "end": "2026-07-19", "days": 7 },
            "usage": {
                "modelsCalled": 2,
                "successfulCalls": 50,
                "totalTokens": 1234567,
                "totalCost": 1.23
            }
        });
        let free = json!([]);
        let s = parse(&summary, None, &free, None, None, now());
        assert!(s.ok);
        assert_eq!(s.primary, "1.2M tokens");
        assert!(s.detail.iter().any(|d| d.label == "7 days" && d.value.contains("1.2M tokens")));
        assert!(s.detail.iter().any(|d| d.label == "Cost" && d.value == "¥1.23"));
    }

    #[test]
    fn computes_pct_when_absent() {
        let summary = json!({ "period": { "start": "a", "end": "b" } });
        let free = json!([
            { "model": "qwen-max", "used": 250, "total": 1000 }
        ]);
        let s = parse(&summary, None, &free, None, None, now());
        assert!(s.ok);
        assert_eq!(s.detail[0].pct, Some(25.0));
    }

    #[test]
    fn parses_usages_array_with_windows() {
        let summary = json!({
            "period": { "start": "2026-07-12", "end": "2026-07-19", "days": 7 },
            "usage": {
                "modelsCalled": 1,
                "successfulCalls": 1,
                "usages": [
                    { "key": "input_token", "value": 13, "unit": "tokens", "label": "Input Tokens" },
                    { "key": "output_token", "value": 1, "unit": "tokens", "label": "Output Tokens" },
                    { "key": "total_token", "value": 14, "unit": "tokens", "label": "Total Tokens" }
                ]
            }
        });
        let today = json!({
            "period": { "start": "2026-07-18", "end": "2026-07-19", "days": 1 },
            "modelsCalled": 1,
            "successfulCalls": 1,
            "usages": [
                { "key": "total_token", "value": 14, "unit": "tokens", "label": "Total Tokens" }
            ]
        });
        let free = json!([]);
        let s = parse(&summary, Some(&today), &free, None, None, now());
        assert!(s.ok);
        assert_eq!(s.primary, "14 tokens");
        let today_row = s.detail.iter().find(|d| d.label == "Today").unwrap();
        assert_eq!(today_row.value, "14 tokens · 1 calls");
        assert!(s.detail.iter().any(|d| d.label == "Input tokens" && d.value == "13"));
    }

    #[test]
    fn parses_plan_quota() {
        let summary = json!({
            "period": { "start": "2026-07-12", "end": "2026-07-19", "days": 7 },
            "usage": {
                "modelsCalled": 1,
                "successfulCalls": 1,
                "usages": [
                    { "key": "input_token", "value": 13, "unit": "tokens", "label": "Input Tokens" },
                    { "key": "output_token", "value": 1, "unit": "tokens", "label": "Output Tokens" },
                    { "key": "total_token", "value": 14, "unit": "tokens", "label": "Total Tokens" }
                ]
            }
        });
        let free = json!([]);
        let n = now();
        let now_ms = n.timestamp_millis();
        let plan = json!({
            "data": {
                "DataV2": {
                    "data": {
                        "data": {
                            "per5HourPercentage": 0.047,
                            "per5HourResetTime": now_ms + 4 * 3600 * 1000 + 30 * 60 * 1000,
                            "per1WeekPercentage": 0.014,
                            "per1WeekResetTime": now_ms + 6 * 24 * 3600 * 1000
                        }
                    }
                }
            }
        });
        let s = parse(&summary, None, &free, None, Some(&plan), n);
        assert!(s.ok);
        assert_eq!(s.primary, "5% used");
        let five_h = s.detail.iter().find(|d| d.label == "5 hours").unwrap();
        assert_eq!(five_h.pct, Some(4.7));
        assert!(five_h.value.starts_with("resets in"));
        let seven_d = s.detail.iter().find(|d| d.label == "7 days").unwrap();
        assert_eq!(seven_d.pct, Some(1.4));
        assert!(seven_d.value.starts_with("resets in"));
        assert!(!s.detail.iter().any(|d| d.label == "Today"));
        assert!(s.detail.iter().any(|d| d.label == "Input tokens"));
    }

    #[test]
    fn parses_quota_check_with_usage() {
        let summary = json!({
            "period": { "start": "2026-07-12", "end": "2026-07-19", "days": 7 },
            "usage": {
                "modelsCalled": 1,
                "successfulCalls": 5,
                "usages": [
                    { "key": "total_token", "value": 50000, "unit": "tokens", "label": "Total Tokens" }
                ]
            }
        });
        let free = json!([]);
        let quota = json!([
            {
                "model": "qwen3.6-plus",
                "rpmUsage": 10, "rpmLimit": 15000,
                "tpmUsage": 50000, "tpmLimit": 5000000
            },
            {
                "model": "qwen-flash",
                "rpmUsage": 0, "rpmLimit": 600,
                "tpmUsage": 0, "tpmLimit": 5000000
            }
        ]);
        let s = parse(&summary, None, &free, Some(&quota), None, now());
        assert!(s.ok);
        // Rate-limit rows are plain text (no pct) — they're per-minute
        // throughput, not quota windows.
        let tpm = s.detail.iter().find(|d| d.label == "qwen3.6-plus TPM").unwrap();
        assert_eq!(tpm.pct, None);
        assert!(tpm.value.contains("50K"));
        assert!(!s.detail.iter().any(|d| d.label.starts_with("qwen-flash")));
    }

    #[test]
    fn parses_quota_check_all_idle() {
        let summary = json!({
            "period": { "start": "2026-07-12", "end": "2026-07-19", "days": 7 },
            "usage": null
        });
        let free = json!([]);
        let quota = json!([
            { "model": "qwen3.6-plus", "rpmUsage": 0, "rpmLimit": 15000, "tpmUsage": 0, "tpmLimit": 5000000 },
            { "model": "qwen-flash", "rpmUsage": 0, "rpmLimit": 600, "tpmUsage": 0, "tpmLimit": 5000000 }
        ]);
        let s = parse(&summary, None, &free, Some(&quota), None, now());
        assert!(s.ok);
        let rl = s.detail.iter().find(|d| d.label == "Rate limits").unwrap();
        assert_eq!(rl.value, "2 models · all within limits");
    }
}
