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
//!
//! A second endpoint, `GET /api/monitor/usage/model-usage`, serves hourly usage
//! buckets over an arbitrary `startTime`/`endTime` window. `fetch_week` pulls a
//! 7-day window from it for the GLM tab's "Last 7 days" chart — real usage
//! (tokens/day + per-model totals), since the weekly *quota* percentage isn't
//! served by the limits endpoint for every account.

use std::collections::HashMap;

use chrono::{DateTime, Datelike, Local, Utc};
use serde::Serialize;
use serde_json::Value;
use std::time::Duration;

use super::{short_date, KeyVal, VendorStatus};
use crate::scanner::WeekDay;

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

    // Plan tier (e.g. "pro", "lite") — surfaced in the headline secondary line.
    let level = root
        .get("level")
        .and_then(|l| l.as_str())
        .map(|s| {
            let mut chars = s.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .filter(|s| !s.is_empty());

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
        // The weekly quota arrived as `1 week` (unit=6) before z.ai's July 2026
        // credits revision; community parsers disagree on the day-unit code the
        // restored window may use (Raycast reads `7 days` as unit=1, oh-my-pi as
        // unit=4). Accept all three so the meter lights up correctly — under the
        // right label, not misfiled as "Session" — whichever encoding returns.
        let by_unit_week = is_tokens_type
            && ((unit == Some(6.0) && number == Some(1.0))
                || ((unit == Some(1.0) || unit == Some(4.0)) && number == Some(7.0)));
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

        // z.ai's live monitor puts the monthly tool quota's TOTAL in a field
        // literally named `usage` (e.g. 1,000) rather than `total` — verified
        // against the live API. Fall back to it so the row can render
        // "133 / 1,000" alongside the reset countdown.
        let total = total.or_else(|| {
            if is_month {
                lim.get("usage").and_then(value_as_f64)
            } else {
                None
            }
        });

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
        // Counts plus a reset render together ("133 / 1,000 · resets in 10d 3h"),
        // matching how the tool quota reads in z.ai's own dashboard. The 5-hour
        // window carries no counts in the live shape, so it stays a plain
        // countdown.
        let value = match (used, total, &reset) {
            (Some(u), Some(t), Some(r)) => {
                format!("{} · resets in {r}", fmt_pair(u, t))
            }
            (Some(u), Some(t), None) => fmt_pair(u, t),
            (_, _, Some(r)) => format!("resets in {r}"),
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

        // Per-tool breakdown (e.g. search-prime 989, web-reader 11) — z.ai
        // nests this inside the TIME_LIMIT entry as `usageDetails`. Rendered
        // as plain text rows right after their parent meter; zero-usage tools
        // are skipped to keep the list tight.
        if let Some(items) = lim.get("usageDetails").and_then(|d| d.as_array()) {
            for item in items {
                let code = item.get("modelCode").and_then(|c| c.as_str()).unwrap_or("");
                let usage = item.get("usage").and_then(value_as_f64).unwrap_or(0.0);
                if code.is_empty() || usage == 0.0 {
                    continue;
                }
                detail.push((rank, KeyVal::text(&friendly_tool_name(code), fmt_count(usage))));
            }
        }

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
    // monthly tool quota. The plan tier (e.g. "Pro") prefixes the secondary
    // line so the provider card shows "Pro · session quota used".
    let (used, label) = if let Some(w) = weekly {
        (w, "weekly quota used")
    } else if let Some(f) = five_h {
        (f, "session quota used")
    } else if let Some(m) = monthly {
        (m, "monthly quota used")
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

// ---------- 7-day usage (monitor `model-usage` endpoint) ----------

/// The GLM tab's "Last 7 days" view, built from the monitor `model-usage`
/// endpoint. The weekly *quota* isn't served by the limits payload for every
/// account (verified live: only the 5-hour window and the monthly tool quota
/// come back), so this is real usage — tokens per day plus a per-model
/// breakdown — rather than a percentage.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GlmWeek {
    /// One row per calendar day, oldest first, always 7 ending today — the same
    /// shape as the Claude week chart, so the frontend renders it with the same
    /// component. `cost_fmt` carries the day's call count ("1.2K calls") because
    /// z.ai reports no cost.
    pub days: Vec<WeekDay>,
    /// Per-model token totals over the window ("GLM-5.2" -> "515.2M · 69%").
    pub models: Vec<KeyVal>,
    /// Window totals, pre-rendered ("504.5M").
    pub total_tokens: String,
    /// Window call count, pre-rendered ("5.5K").
    pub total_calls: String,
    /// Active hours, newest first, capped to the sessions-tab per-provider row
    /// limit — converted into "Recent activity" rows by the command layer.
    pub recent: Vec<GlmHour>,
}

/// One active hour of z.ai usage, from the monitor `model-usage` hourly
/// buckets. Unlike the local logs (MCP server lifecycle only), this is real
/// server-side usage — including activity from other machines/CLIs.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GlmHour {
    /// The hour bucket's start instant, in UTC.
    pub at: DateTime<Utc>,
    /// Model that consumed the most tokens this hour ("" when the payload
    /// carries no per-model series — the badge then simply hides).
    pub model: String,
    pub tokens: u64,
    pub calls: u64,
}

/// "2026-08-15 09:00" (local, the same convention the dashboard renders) ->
/// the hour's start instant in UTC. Ambiguous DST folds pick the later offset;
/// unresolvable labels (a gap hour) are skipped rather than guessed.
fn parse_hour_label(label: &str) -> Option<DateTime<Utc>> {
    use chrono::TimeZone;
    let ndt = chrono::NaiveDateTime::parse_from_str(label, "%Y-%m-%d %H:%M").ok()?;
    match Local.from_local_datetime(&ndt) {
        chrono::LocalResult::Single(dt) => Some(dt.with_timezone(&Utc)),
        chrono::LocalResult::Ambiguous(_, later) => Some(later.with_timezone(&Utc)),
        chrono::LocalResult::None => None,
    }
}

/// Fetch the 7-day model usage. `endpoint` is the configured quota/limit URL;
/// its base (scheme + host) is reused so a CN `open.bigmodel.cn` endpoint routes
/// the usage call to the same platform. The window is expressed as
/// `startTime`/`endTime` in `yyyy-MM-dd HH:mm:ss` local time with the space
/// URL-encoded as `%20` — the format z.ai's own dashboard sends.
pub async fn fetch_week(
    api_key: &str,
    endpoint: &str,
    now: DateTime<Utc>,
) -> Result<GlmWeek, String> {
    let end = now.with_timezone(&Local);
    let start = end - chrono::Duration::hours(7 * 24);
    let fmt = |t: DateTime<Local>| t.format("%Y-%m-%d %H:%M:%S").to_string().replace(' ', "%20");
    let url = format!(
        "{}/api/monitor/usage/model-usage?startTime={}&endTime={}",
        base_url(endpoint),
        fmt(start),
        fmt(end),
    );

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(12))
        .build()
        .map_err(|e| format!("client init: {e}"))?;
    let resp = client
        .get(&url)
        // Same monitor-API auth as `fetch`: raw token, no "Bearer" prefix.
        .header("Authorization", api_key)
        .header("Accept-Language", "en-US,en")
        .header("Content-Type", "application/json")
        .send()
        .await
        .map_err(|e| format!("request error: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("HTTP {}", status.as_u16()));
    }
    let v: Value = resp.json().await.map_err(|e| format!("invalid JSON: {e}"))?;
    parse_model_usage(&v, now)
}

/// `https://api.z.ai/api/monitor/usage/quota/limit` -> `https://api.z.ai` —
/// everything before the API path. Falls back to the global API host when the
/// configured endpoint doesn't match the expected shape.
fn base_url(endpoint: &str) -> String {
    endpoint
        .split_once("/api/")
        .map(|(base, _)| base.to_string())
        .unwrap_or_else(|| "https://api.z.ai".to_string())
}

/// Pure parser for the monitor `model-usage` response: hourly `x_time` buckets
/// aggregated into 7 calendar days (local, matching how the labels are
/// rendered), plus the per-model summary and window totals. `now` anchors
/// "today" so tests can pin the calendar.
pub fn parse_model_usage(v: &Value, now: DateTime<Utc>) -> Result<GlmWeek, String> {
    let root = if v.get("data").map(|d| d.is_object()).unwrap_or(false) {
        &v["data"]
    } else {
        v
    };

    let Some(times) = root.get("x_time").and_then(|t| t.as_array()) else {
        return Err("no `data.x_time` array in response".to_string());
    };
    let series = |key: &str| -> Vec<f64> {
        root.get(key)
            .and_then(|t| t.as_array())
            .map(|a| a.iter().filter_map(value_as_f64).collect())
            .unwrap_or_default()
    };
    let tokens = series("tokensUsage");
    let calls = series("modelCallCount");

    // Bucket the hourly series by calendar day — the `yyyy-MM-dd` prefix of each
    // label. Mismatched array lengths (a truncated payload) just contribute
    // zeros for the missing tail rather than failing the whole chart.
    let mut by_day: HashMap<String, (f64, f64)> = HashMap::new();
    for (i, t) in times.iter().enumerate() {
        let Some(label) = t.as_str() else { continue };
        if label.len() < 10 {
            continue;
        }
        let entry = by_day.entry(label[..10].to_string()).or_insert((0.0, 0.0));
        entry.0 += tokens.get(i).copied().unwrap_or(0.0);
        entry.1 += calls.get(i).copied().unwrap_or(0.0);
    }

    // Seven calendar days ending today, mirroring the scanner's Claude week
    // chart; `bar_pct` is normalized to the busiest day.
    let today = now.with_timezone(&Local).date_naive();
    let mut rows: Vec<(String, String, f64, f64)> = Vec::with_capacity(7);
    let mut max_tokens = 0.0f64;
    for i in (0..7).rev() {
        let d = today - chrono::Days::new(i);
        let key = d.format("%Y-%m-%d").to_string();
        let (toks, calls) = by_day.get(&key).copied().unwrap_or((0.0, 0.0));
        max_tokens = max_tokens.max(toks);
        rows.push((
            crate::scanner::weekday_abbr(d.weekday().num_days_from_monday()),
            key,
            toks,
            calls,
        ));
    }
    let days: Vec<WeekDay> = rows
        .iter()
        .map(|(day, date, toks, calls)| WeekDay {
            day: day.clone(),
            date: date.clone(),
            tok_fmt: fmt_count(*toks),
            cost_fmt: format!("{} calls", fmt_count(*calls)),
            bar_pct: if max_tokens > 0.0 {
                ((toks / max_tokens) * 100.0).round() as u32
            } else {
                0
            },
        })
        .collect();

    let total_usage = |key: &str| root.get("totalUsage").and_then(|t| t.get(key)).and_then(value_as_f64);
    // Prefer the API's window totals; a payload without them falls back to the
    // bucket sums (identical for a well-formed response).
    let summed_tokens: f64 = rows.iter().map(|r| r.2).sum();
    let summed_calls: f64 = rows.iter().map(|r| r.3).sum();
    let total_tokens = total_usage("totalTokensUsage").unwrap_or(summed_tokens);
    let total_calls = total_usage("totalModelCallCount").unwrap_or(summed_calls);

    // Per-model totals. The summary list is the primary shape; older payloads
    // may carry only the per-model series with the same totals attached.
    let models_src: &[Value] = root
        .get("modelSummaryList")
        .and_then(|m| m.as_array())
        .or_else(|| root.get("modelDataList").and_then(|m| m.as_array()))
        .map(|a| a.as_slice())
        .unwrap_or(&[]);
    let model_tokens = |m: &Value| m.get("totalTokens").and_then(value_as_f64).unwrap_or(0.0);
    // Shares are relative to the SUM of the per-model totals, not
    // `totalUsage.totalTokensUsage` — the live API's own lists disagree by a few
    // percent (they appear to aggregate slightly differently), and a
    // relative-to-models sum keeps the rows adding up to 100% instead of
    // rendering a "515.2M · 101%" row.
    let models_sum: f64 = models_src.iter().map(|m| model_tokens(m)).sum();
    let mut models = Vec::new();
    for m in models_src {
        let name = m.get("modelName").and_then(|n| n.as_str()).unwrap_or("");
        let toks = model_tokens(m);
        if name.is_empty() || toks <= 0.0 {
            continue;
        }
        let share = if models_sum > 0.0 {
            toks / models_sum * 100.0
        } else {
            0.0
        };
        models.push(KeyVal::text(name, format!("{} · {:.0}%", fmt_count(toks), share)));
    }

    // Dominant model per hour, from the per-model series ("GLM-5.2" etc.). The
    // aggregate series drives the counts; this only labels the hour.
    let mut hour_best: Vec<(f64, &str)> = vec![(0.0, ""); times.len()];
    if let Some(models_series) = root.get("modelDataList").and_then(|m| m.as_array()) {
        for m in models_series {
            let name = m.get("modelName").and_then(|n| n.as_str()).unwrap_or("");
            if name.is_empty() {
                continue;
            }
            let per_hour: Vec<f64> = m
                .get("tokensUsage")
                .and_then(|t| t.as_array())
                .map(|a| a.iter().filter_map(value_as_f64).collect())
                .unwrap_or_default();
            for (i, v) in per_hour.iter().enumerate() {
                if i < hour_best.len() && *v > hour_best[i].0 {
                    hour_best[i] = (*v, name);
                }
            }
        }
    }

    // Active hours for the Sessions tab: one row per hour with usage, newest
    // first, capped like every other provider's row contribution.
    let mut recent: Vec<GlmHour> = Vec::new();
    for (i, t) in times.iter().enumerate() {
        let Some(label) = t.as_str() else { continue };
        let Some(at) = parse_hour_label(label) else { continue };
        let toks = tokens.get(i).copied().unwrap_or(0.0);
        let calls = calls.get(i).copied().unwrap_or(0.0);
        if toks <= 0.0 && calls <= 0.0 {
            continue;
        }
        recent.push(GlmHour {
            at,
            model: hour_best.get(i).map(|(_, n)| n.to_string()).unwrap_or_default(),
            tokens: toks as u64,
            calls: calls as u64,
        });
    }
    recent.sort_by(|a, b| b.at.cmp(&a.at));
    recent.truncate(crate::scanner::MAX_PROVIDER_ROWS);

    Ok(GlmWeek {
        days,
        models,
        total_tokens: fmt_count(total_tokens),
        total_calls: fmt_count(total_calls),
        recent,
    })
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

/// Map a z.ai `modelCode` to a human-friendly label. Known codes get a
/// curated name; anything new falls back to `humanize` (title-case, de-hyphen).
fn friendly_tool_name(code: &str) -> String {
    match code {
        "search-prime" => "Search".into(),
        "web-reader" => "Web Reader".into(),
        "zread" => "ZRead".into(),
        "code-interpreter" => "Code Interpreter".into(),
        _ => humanize(code),
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

/// Integer with thousands separators ("1,000"), for human-scale counts where
/// "1.0K" would read worse than the exact number (the monthly tool quota).
fn fmt_grouped(n: f64) -> String {
    let s = (n as u64).to_string();
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, b) in bytes.iter().enumerate() {
        // Insert a comma before every 3rd digit from the right.
        if i > 0 && (bytes.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
}

/// Format a used/total pair: exact grouped integers while the scale is
/// human-countable ("133 / 1,000"), compact units at token scale
/// ("16.0M / 40.0M", matching the pre-existing quota rows).
fn fmt_pair(used: f64, total: f64) -> String {
    if total < 10_000.0 {
        format!("{} / {}", fmt_grouped(used), fmt_grouped(total))
    } else {
        format!("{} / {}", fmt_count(used), fmt_count(total))
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
        // Weekly present → it drives the headline; level prefixes it.
        assert_eq!(s.secondary, "Lite · weekly quota used");
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

    #[test]
    fn level_prefixes_secondary() {
        let v = json!({ "data": { "level": "pro", "limits": [
            { "type": "TOKENS_LIMIT", "unit": 3, "number": 5, "percentage": 10 }
        ] } });
        let s = parse(&v, now());
        assert_eq!(s.secondary, "Pro · session quota used");
    }

    #[test]
    fn no_level_leaves_secondary_plain() {
        let v = json!({ "data": { "limits": [
            { "type": "TOKENS_LIMIT", "unit": 3, "number": 5, "percentage": 10 }
        ] } });
        let s = parse(&v, now());
        assert_eq!(s.secondary, "session quota used");
    }

    #[test]
    fn usage_details_render_as_text_rows() {
        let v = json!({ "data": { "level": "pro", "limits": [
            { "type": "TIME_LIMIT", "unit": 5, "number": 1, "percentage": 100,
              "usage": 1000, "currentValue": 1000, "remaining": 0,
              "usageDetails": [
                  { "modelCode": "search-prime", "usage": 989 },
                  { "modelCode": "web-reader", "usage": 11 },
                  { "modelCode": "zread", "usage": 0 }
              ] }
        ] } });
        let s = parse(&v, now());
        assert!(s.ok);
        // The meter row comes first, then the non-zero tool breakdown rows.
        let meter = s.detail.iter().find(|d| d.label == "Monthly tools").unwrap();
        assert_eq!(meter.pct, Some(100.0));
        // zread (0 usage) is skipped; search-prime and web-reader remain,
        // rendered with human-friendly labels.
        let texts: Vec<_> = s.detail.iter().filter(|d| d.pct.is_none()).collect();
        assert_eq!(texts.len(), 2);
        assert_eq!(texts[0].label, "Search");
        assert_eq!(texts[0].value, "989");
        assert_eq!(texts[1].label, "Web Reader");
        assert_eq!(texts[1].value, "11");
    }

    #[test]
    fn usage_details_absent_is_fine() {
        let v = json!({ "data": { "limits": [
            { "type": "TOKENS_LIMIT", "unit": 3, "number": 5, "percentage": 5 }
        ] } });
        let s = parse(&v, now());
        assert!(s.ok);
        assert!(s.detail.iter().all(|d| d.pct.is_some()));
    }

    #[test]
    fn monthly_total_falls_back_to_usage_field() {
        // Live shape: the tool quota's total rides in `usage`, not `total`.
        let v = json!({ "data": { "level": "pro", "limits": [
            { "type": "TIME_LIMIT", "unit": 5, "number": 1, "usage": 1000,
              "currentValue": 133, "remaining": 867, "percentage": 13,
              "nextResetTime": reset_in(10 * 24 * 60) }
        ] } });
        let s = parse(&v, now());
        assert!(s.ok);
        let meter = s.detail.iter().find(|d| d.label == "Monthly tools").unwrap();
        assert_eq!(meter.pct, Some(13.0));
        assert_eq!(meter.value, "133 / 1,000 · resets in 10d 0h");
    }

    #[test]
    fn pair_formatting_scales_by_magnitude() {
        // Human-scale counts render exactly; token scale stays compact.
        assert_eq!(fmt_pair(133.0, 1000.0), "133 / 1,000");
        assert_eq!(fmt_pair(16000000.0, 40000000.0), "16.0M / 40.0M");
        assert_eq!(fmt_grouped(1234567.0), "1,234,567");
        assert_eq!(fmt_grouped(989.0), "989");
    }

    #[test]
    fn weekly_window_recognized_in_every_known_encoding() {
        // The weekly quota predates z.ai's July 2026 credits revision as
        // `1 week` (6/1); community parsers expect the restored window as
        // `7 days` under either day-unit code. All three must map to Weekly —
        // never misfiled as "Session" via the bare-token fallback.
        for (unit, number) in [(6.0, 1.0), (1.0, 7.0), (4.0, 7.0)] {
            let v = json!({ "data": { "limits": [
                { "type": "TOKENS_LIMIT", "unit": unit, "number": number, "percentage": 30 }
            ] } });
            let s = parse(&v, now());
            assert!(s.ok, "unit={unit} number={number} should parse");
            assert_eq!(s.detail[0].label, "Weekly", "unit={unit} number={number}");
            assert_eq!(s.secondary, "weekly quota used", "unit={unit} number={number}");
        }
    }

    #[test]
    fn base_url_strips_the_api_path() {
        assert_eq!(
            base_url("https://api.z.ai/api/monitor/usage/quota/limit"),
            "https://api.z.ai"
        );
        assert_eq!(
            base_url("https://open.bigmodel.cn/api/monitor/usage/quota/limit"),
            "https://open.bigmodel.cn"
        );
        // Unparseable/empty endpoint falls back to the global API host.
        assert_eq!(base_url(""), "https://api.z.ai");
        assert_eq!(base_url("not-a-url"), "https://api.z.ai");
    }

    /// Today's local date for the fixed `now()` — `x_time` labels are local
    /// time, so the fixtures must be anchored the same way.
    fn today_local() -> chrono::NaiveDate {
        now().with_timezone(&Local).date_naive()
    }

    #[test]
    fn parses_model_usage_into_seven_days() {
        let today = today_local();
        let label = |d: chrono::NaiveDate, h: u32| format!("{} {:02}:00", d.format("%Y-%m-%d"), h);
        let yesterday = today - chrono::Days::new(1);
        let three_days_ago = today - chrono::Days::new(3);
        let v = json!({ "data": {
            // One straggler before the window starts — must be ignored.
            "x_time": [
                label(three_days_ago - chrono::Days::new(7), 9),
                label(three_days_ago, 9),
                label(yesterday, 23),
                label(today, 10),
                label(today, 11)
            ],
            "tokensUsage": [999999, 50, 200, 100, 300],
            "modelCallCount": [99, 1, 1, 2, 3],
            "totalUsage": { "totalTokensUsage": 650, "totalModelCallCount": 7 }
        } });
        let w = parse_model_usage(&v, now()).unwrap();

        assert_eq!(w.days.len(), 7, "always seven calendar days");
        assert_eq!(w.days.last().unwrap().date, today.format("%Y-%m-%d").to_string());
        assert_eq!(w.days.first().unwrap().date, (today - chrono::Days::new(6)).format("%Y-%m-%d").to_string());

        // Bars normalize to the busiest day (today: 100+300 tokens).
        let today_row = w.days.last().unwrap();
        assert_eq!(today_row.tok_fmt, "400");
        assert_eq!(today_row.cost_fmt, "5 calls");
        assert_eq!(today_row.bar_pct, 100);
        let yesterday_row = w.days.iter().rev().nth(1).unwrap();
        assert_eq!(yesterday_row.tok_fmt, "200");
        assert_eq!(yesterday_row.bar_pct, 50);
        let three_row = w.days.iter().rev().nth(3).unwrap();
        assert_eq!(three_row.tok_fmt, "50");
        assert_eq!(three_row.bar_pct, 13); // 50/400 → 12.5 rounds to 13
        // Empty days still render (zeroed bar), and pre-window data is ignored.
        let empty_row = w.days.iter().rev().nth(2).unwrap();
        assert_eq!(empty_row.tok_fmt, "0");
        assert_eq!(empty_row.bar_pct, 0);

        assert_eq!(w.total_tokens, "650");
        assert_eq!(w.total_calls, "7");
    }

    #[test]
    fn model_usage_model_rows_and_totals() {
        let today = today_local();
        let v = json!({ "data": {
            "x_time": [format!("{} 09:00", today.format("%Y-%m-%d"))],
            "tokensUsage": [1000],
            "modelCallCount": [4],
            "totalUsage": { "totalTokensUsage": 1000, "totalModelCallCount": 4 },
            "modelSummaryList": [
                { "modelName": "GLM-5.2", "totalTokens": 750, "sortOrder": 1 },
                { "modelName": "GLM-5.3", "totalTokens": 250, "sortOrder": 2 },
                { "modelName": "GLM-4.6", "totalTokens": 0, "sortOrder": 3 }
            ]
        } });
        let w = parse_model_usage(&v, now()).unwrap();
        // Zero-token models are skipped; shares are relative to the window total.
        assert_eq!(w.models.len(), 2);
        assert_eq!(w.models[0].label, "GLM-5.2");
        assert_eq!(w.models[0].value, "750 · 75%");
        assert_eq!(w.models[1].label, "GLM-5.3");
        assert_eq!(w.models[1].value, "250 · 25%");
        assert_eq!(w.total_tokens, "1K");
        assert_eq!(w.total_calls, "4");
    }

    #[test]
    fn model_usage_falls_back_to_model_data_list_and_bucket_sums() {
        let today = today_local();
        let v = json!({ "data": {
            "x_time": [format!("{} 09:00", today.format("%Y-%m-%d"))],
            "tokensUsage": [2500],
            "modelCallCount": [9],
            // No modelSummaryList / totalUsage — derive from the series.
            "modelDataList": [
                { "modelName": "GLM-5.3", "totalTokens": 2500, "sortOrder": 1 }
            ]
        } });
        let w = parse_model_usage(&v, now()).unwrap();
        assert_eq!(w.models.len(), 1);
        assert_eq!(w.models[0].value, "2K · 100%");
        assert_eq!(w.total_tokens, "2K");
        assert_eq!(w.total_calls, "9");
    }

    #[test]
    fn model_usage_without_x_time_is_err() {
        assert!(parse_model_usage(&json!({ "data": { "tokensUsage": [1] } }), now()).is_err());
        assert!(parse_model_usage(&json!({ "data": {} }), now()).is_err());
    }

    #[test]
    fn model_usage_builds_recent_active_hours() {
        use chrono::TimeZone;
        let today = today_local();
        let label = |d: chrono::NaiveDate, h: u32| format!("{} {:02}:00", d.format("%Y-%m-%d"), h);
        let v = json!({ "data": {
            "x_time": [label(today, 9), label(today, 10), label(today, 11)],
            "tokensUsage": [100, 0, 50],
            "modelCallCount": [4, 0, 1],
            "modelDataList": [
                { "modelName": "GLM-5.2", "tokensUsage": [100, 0, 10], "totalTokens": 110 },
                { "modelName": "GLM-5.3", "tokensUsage": [0, 0, 40], "totalTokens": 40 }
            ]
        } });
        let w = parse_model_usage(&v, now()).unwrap();

        // Zero-activity hours are skipped; the rest are newest first, each
        // labeled with the hour's dominant model.
        assert_eq!(w.recent.len(), 2);
        let (y, m, d) = (today.year(), today.month(), today.day());
        let hour_utc = |h: u32| {
            Local
                .with_ymd_and_hms(y, m, d, h, 0, 0)
                .single()
                .unwrap()
                .with_timezone(&Utc)
        };
        assert_eq!(w.recent[0].at, hour_utc(11), "newest hour first");
        assert_eq!(w.recent[0].model, "GLM-5.3", "dominant model that hour");
        assert_eq!(w.recent[0].tokens, 50);
        assert_eq!(w.recent[0].calls, 1);
        assert_eq!(w.recent[1].at, hour_utc(9));
        assert_eq!(w.recent[1].model, "GLM-5.2");
        assert_eq!(w.recent[1].tokens, 100);
        assert_eq!(w.recent[1].calls, 4);
    }

    #[test]
    fn model_usage_caps_recent_hours_at_the_sessions_row_limit() {
        let base = today_local() - chrono::Days::new(2);
        let labels: Vec<String> = (0..30)
            .map(|i| format!("{} {:02}:00", (base + chrono::Days::new(i / 24)).format("%Y-%m-%d"), i % 24))
            .collect();
        let tokens: Vec<u64> = (0..30).map(|i| 10 + i as u64).collect();
        let calls: Vec<u64> = vec![1; 30];
        let v = json!({ "data": {
            "x_time": labels,
            "tokensUsage": tokens,
            "modelCallCount": calls
        } });
        let w = parse_model_usage(&v, now()).unwrap();

        assert_eq!(w.recent.len(), crate::scanner::MAX_PROVIDER_ROWS);
        // The cap keeps the NEWEST hours — the last generated bucket.
        assert_eq!(w.recent[0].tokens, 39, "newest hour (i=29) survives the cap");
        assert!(w.recent.iter().all(|h| h.model.is_empty()), "no model series -> unlabeled");
    }
}
