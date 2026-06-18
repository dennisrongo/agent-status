//! Local CLI usage scanner. Reads Claude Code session logs
//! (`~/.claude/projects/**/*.jsonl`) and GLM/z.ai logs (`~/.zai/*.log`),
//! aggregates token usage into the snapshot the frontend renders.
//!
//! All file I/O here is synchronous; callers must run it via
//! `tokio::task::spawn_blocking` from async commands.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Datelike, Duration, Utc};
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    #[error("could not resolve home directory")]
    NoHome,
}

// ---------- Output types (serialize to camelCase for the frontend) ----------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSnapshot {
    pub meta: Meta,
    pub limits: Limits,
    pub kpi: Kpi,
    pub week: Vec<WeekDay>,
    pub models: Vec<ModelRow>,
    pub sessions: Vec<SessionRow>,
    pub providers: Vec<Provider>,
    pub glm: Glm,
    /// Live vendor-side usage, filled in by the command layer after the scan.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vendor: Option<crate::vendors::VendorReport>,
    /// Which providers are present locally, filled in by the command layer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detection: Option<crate::vendors::Detection>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Meta {
    pub generated: String,
    /// Epoch milliseconds the snapshot was built. Lets the frontend drop any
    /// out-of-order snapshot (several emitters can race) instead of flipping.
    pub generated_ms: i64,
    pub window_first: String,
    pub window_last: String,
    pub files_scanned: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Limits {
    pub plan_label: String,
    pub estimate_note: String,
    pub buckets: Vec<Bucket>,
    /// True when the meters are real live data from Claude's usage API rather
    /// than the local estimate.
    #[serde(default)]
    pub live: bool,
    /// True when live data is the chosen source but isn't available yet (still
    /// fetching / throttled before the first reading). The UI shows a loading
    /// state instead of the wrong-scale local estimate.
    #[serde(default)]
    pub pending: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Bucket {
    pub name: String,
    pub sub: String,
    pub used_fmt: String,
    pub used_pct: f64,
    pub left_pct: f64,
    pub left_fmt: String,
    pub limit_fmt: String,
    pub reset: String,
    pub status: String,
    pub status_label: String,
    /// True when sourced from Claude's live usage API rather than the local estimate.
    #[serde(default)]
    pub live: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Kpi {
    pub session_tokens: String,
    pub session_cost: String,
    pub week_tokens: String,
    pub week_cost: String,
    pub total_tokens: String,
    pub total_cost: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WeekDay {
    pub day: String,
    pub date: String,
    pub tok_fmt: String,
    pub cost_fmt: String,
    pub bar_pct: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRow {
    pub name: String,
    pub key: String,
    pub tokens: String,
    pub cost: String,
    pub pct: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRow {
    pub id: String,
    pub project: String,
    pub model: String,
    pub tokens: u64,
    pub cost: f64,
    pub when: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Provider {
    pub name: String,
    pub status: String,
    pub tokens: String,
    pub cost: String,
    pub sessions: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Glm {
    pub sessions: u32,
    pub active_days: usize,
    pub last: String,
    pub note: String,
}

// ---------- Internal record ----------

#[derive(Clone)]
struct Record {
    dt: DateTime<Utc>,
    tokens: u64,
    cost: f64,
    is_opus: bool,
    family: &'static str,
    session_id: String,
    project: String,
}

struct SessionAgg {
    tokens: u64,
    cost: f64,
    project: String,
    last: DateTime<Utc>,
    family: &'static str,
}

// ---------- Pricing (USD per 1M tokens, standard tier) ----------

struct Price {
    input: f64,
    output: f64,
    cache_write: f64,
    cache_read: f64,
}

fn price(family: &str) -> Price {
    match family {
        "opus" => Price { input: 15.0, output: 75.0, cache_write: 18.75, cache_read: 1.50 },
        "haiku" => Price { input: 0.80, output: 4.0, cache_write: 1.0, cache_read: 0.08 },
        // sonnet and fallback
        _ => Price { input: 3.0, output: 15.0, cache_write: 3.75, cache_read: 0.30 },
    }
}

fn family_of(model: &str) -> &'static str {
    let m = model.to_lowercase();
    if m.contains("opus") {
        "opus"
    } else if m.contains("haiku") {
        "haiku"
    } else {
        "sonnet"
    }
}

// ---------- Plan ceilings (editable estimates) ----------

/// Returns (session_5h, week_all, week_opus) ceilings in tokens.
fn ceilings(plan: &str) -> (u64, u64, u64) {
    match plan {
        "pro" => (30_000_000, 200_000_000, 0),
        "max20x" => (600_000_000, 4_000_000_000, 1_000_000_000),
        // max5x and custom fallback
        _ => (150_000_000, 1_000_000_000, 250_000_000),
    }
}

fn plan_label(plan: &str) -> &'static str {
    match plan {
        "pro" => "Pro",
        "max20x" => "Max 20×",
        "custom" => "Custom",
        _ => "Max 5×",
    }
}

// ---------- Formatting helpers ----------

fn fmt_tokens(n: f64) -> String {
    if n >= 1e9 {
        format!("{:.2}B", n / 1e9)
    } else if n >= 1e6 {
        format!("{:.1}M", n / 1e6)
    } else if n >= 1e3 {
        format!("{:.0}K", n / 1e3)
    } else {
        format!("{}", n as u64)
    }
}

fn fmt_cost(c: f64) -> String {
    format!("${:.2}", c)
}

fn countdown(reset: Option<DateTime<Utc>>, now: DateTime<Utc>) -> String {
    let Some(reset) = reset else { return "ready".to_string() };
    let secs = (reset - now).num_seconds();
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

fn humanize_when(ts: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let delta = now - ts;
    let secs = delta.num_seconds().max(0);
    let days = delta.num_days();
    if days == 0 {
        let h = secs / 3_600;
        if h > 0 {
            format!("{h}h ago")
        } else {
            format!("{}m ago", secs / 60)
        }
    } else if days == 1 {
        "yesterday".to_string()
    } else {
        format!("{days}d ago")
    }
}

fn clean_project(raw: &str) -> String {
    let s = raw
        .replace("-Volumes-CrucialX10-projects-", "")
        .replace("-Users-dennisrongo-", "")
        .replace("-Volumes-CrucialX10-", "");
    let s = s.trim_matches('-');
    if s.is_empty() {
        "—".to_string()
    } else {
        let trimmed: String = s.chars().take(28).collect();
        trimmed
    }
}

fn status_for(pct: f64) -> (&'static str, &'static str) {
    if pct < 70.0 {
        ("ok", "Healthy")
    } else if pct < 90.0 {
        ("warn", "Watch")
    } else {
        ("danger", "Near limit")
    }
}

// ---------- Public entry ----------

/// Scan using the real home directory and the current time.
pub fn scan_default(plan: &str) -> Result<UsageSnapshot, ScanError> {
    let home = dirs::home_dir().ok_or(ScanError::NoHome)?;
    let claude_root = home.join(".claude").join("projects");
    let zai_root = home.join(".zai");
    Ok(scan(&claude_root, &zai_root, plan, Utc::now()))
}

/// Pure-ish scan over explicit roots and clock — used by tests.
pub fn scan(
    claude_root: &Path,
    zai_root: &Path,
    plan: &str,
    now: DateTime<Utc>,
) -> UsageSnapshot {
    let mut records: Vec<Record> = Vec::new();
    let files = find_jsonl(claude_root);
    let files_scanned = files.len();

    // Claude Code writes the same assistant message into multiple JSONL files
    // when a session is resumed, compacted, or forked into a sidechain. Counting
    // every line double-counts those tokens (≈40% inflation in practice), so we
    // dedupe on the API's stable identity — `message.id` + `requestId` — the same
    // key ccusage uses. Records missing both ids are always kept.
    let mut seen: HashSet<(String, String)> = HashSet::new();

    for fp in &files {
        let project = fp
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let Ok(content) = std::fs::read_to_string(fp) else { continue };
        for line in content.lines() {
            if !line.contains("\"usage\"") {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
            let msg = &v["message"];
            let usage = &msg["usage"];
            if usage.is_null() {
                continue;
            }
            let Some(ts) = v["timestamp"].as_str() else { continue };
            let Ok(dt) = DateTime::parse_from_rfc3339(ts) else { continue };
            let dt = dt.with_timezone(&Utc);

            // Skip a record we've already counted under a different file. Only
            // dedupe when both ids are present, mirroring ccusage.
            if let (Some(id), Some(req)) = (msg["id"].as_str(), v["requestId"].as_str()) {
                if !seen.insert((id.to_string(), req.to_string())) {
                    continue;
                }
            }

            let model = msg["model"].as_str().unwrap_or("");
            let family = family_of(model);
            let tin = usage["input_tokens"].as_u64().unwrap_or(0);
            let tout = usage["output_tokens"].as_u64().unwrap_or(0);
            let tcw = usage["cache_creation_input_tokens"].as_u64().unwrap_or(0);
            let tcr = usage["cache_read_input_tokens"].as_u64().unwrap_or(0);
            let tokens = tin + tout + tcw + tcr;

            let p = price(family);
            let cost = (tin as f64 * p.input
                + tout as f64 * p.output
                + tcw as f64 * p.cache_write
                + tcr as f64 * p.cache_read)
                / 1e6;

            let session_id = v["sessionId"].as_str().unwrap_or("?").to_string();

            records.push(Record {
                dt,
                tokens,
                cost,
                is_opus: family == "opus",
                family,
                session_id,
                project: project.clone(),
            });
        }
    }

    build_snapshot(records, files_scanned, zai_root, plan, now)
}

fn find_jsonl(root: &Path) -> Vec<PathBuf> {
    let pattern = format!("{}/**/*.jsonl", root.to_string_lossy());
    match glob::glob(&pattern) {
        Ok(paths) => paths.filter_map(Result::ok).collect(),
        Err(_) => Vec::new(),
    }
}

fn build_snapshot(
    records: Vec<Record>,
    files_scanned: usize,
    zai_root: &Path,
    plan: &str,
    now: DateTime<Utc>,
) -> UsageSnapshot {
    // ---- windows ----
    let cut_5h = now - Duration::hours(5);
    let cut_7d = now - Duration::days(7);

    let window = |opus_only: bool, cut: DateTime<Utc>| -> (u64, Option<DateTime<Utc>>) {
        let mut used = 0u64;
        let mut earliest: Option<DateTime<Utc>> = None;
        for r in &records {
            if r.dt >= cut && (!opus_only || r.is_opus) {
                used += r.tokens;
                earliest = Some(match earliest {
                    Some(e) if e < r.dt => e,
                    _ => r.dt,
                });
            }
        }
        (used, earliest)
    };

    let (s_used, s_anchor) = window(false, cut_5h);
    let s_reset = s_anchor.map(|a| a + Duration::hours(5));
    let (wa_used, wa_anchor) = window(false, cut_7d);
    let wa_reset = wa_anchor.map(|a| a + Duration::days(7));
    let (wo_used, wo_anchor) = window(true, cut_7d);
    let wo_reset = wo_anchor.map(|a| a + Duration::days(7));

    let (c_session, c_week_all, c_week_opus) = ceilings(plan);

    let make_bucket = |name: &str, sub: &str, used: u64, ceil: u64, reset: Option<DateTime<Utc>>| {
        let pct = if ceil > 0 {
            ((used as f64 / ceil as f64) * 100.0).min(100.0)
        } else {
            0.0
        };
        let pct = (pct * 10.0).round() / 10.0;
        let left = ceil.saturating_sub(used);
        let (status, status_label) = status_for(pct);
        Bucket {
            name: name.to_string(),
            sub: sub.to_string(),
            used_fmt: fmt_tokens(used as f64),
            used_pct: pct,
            left_pct: ((100.0 - pct) * 10.0).round() / 10.0,
            left_fmt: fmt_tokens(left as f64),
            limit_fmt: if ceil > 0 { fmt_tokens(ceil as f64) } else { "—".to_string() },
            reset: countdown(reset, now),
            status: status.to_string(),
            status_label: status_label.to_string(),
            live: false,
        }
    };

    let buckets = vec![
        make_bucket("Session", "5-hour window", s_used, c_session, s_reset),
        make_bucket("Week · all models", "rolling 7 days", wa_used, c_week_all, wa_reset),
        make_bucket("Week · Opus", "rolling 7 days", wo_used, c_week_opus, wo_reset),
    ];

    let label = plan_label(plan);
    let limits = Limits {
        plan_label: label.to_string(),
        estimate_note: format!(
            "Limits estimated for the {label} plan — usage and reset times are read from your local logs; the % left is against an editable ceiling."
        ),
        buckets,
        live: false,
        pending: false,
    };

    // ---- per-day week chart (7 days incl. today) ----
    let mut day_tokens: HashMap<String, u64> = HashMap::new();
    let mut day_cost: HashMap<String, f64> = HashMap::new();
    for r in &records {
        let key = r.dt.format("%Y-%m-%d").to_string();
        *day_tokens.entry(key.clone()).or_insert(0) += r.tokens;
        *day_cost.entry(key).or_insert(0.0) += r.cost;
    }
    let today = now.date_naive();
    let mut week: Vec<WeekDay> = Vec::with_capacity(7);
    let mut week_max = 1u64;
    let mut week_tokens_total = 0u64;
    let mut week_cost_total = 0.0;
    for i in (0..7).rev() {
        let d = today - chrono::Days::new(i);
        let key = d.format("%Y-%m-%d").to_string();
        let toks = *day_tokens.get(&key).unwrap_or(&0);
        let cost = *day_cost.get(&key).unwrap_or(&0.0);
        week_tokens_total += toks;
        week_cost_total += cost;
        week_max = week_max.max(toks);
        week.push(WeekDay {
            day: weekday_abbr(d.weekday().num_days_from_monday()),
            date: key,
            tok_fmt: fmt_tokens(toks as f64),
            cost_fmt: fmt_cost(cost),
            bar_pct: 0, // filled below once max is known
        });
    }
    for w in week.iter_mut() {
        let toks = day_tokens.get(&w.date).copied().unwrap_or(0);
        w.bar_pct = ((toks as f64 / week_max as f64) * 100.0).round() as u32;
    }

    // ---- by model (all-time) ----
    let mut model_tokens: HashMap<&str, u64> = HashMap::new();
    let mut model_cost: HashMap<&str, f64> = HashMap::new();
    let mut grand_tokens = 0u64;
    let mut grand_cost = 0.0;
    for r in &records {
        *model_tokens.entry(r.family).or_insert(0) += r.tokens;
        *model_cost.entry(r.family).or_insert(0.0) += r.cost;
        grand_tokens += r.tokens;
        grand_cost += r.cost;
    }
    let models: Vec<ModelRow> = [("opus", "Opus"), ("sonnet", "Sonnet"), ("haiku", "Haiku")]
        .iter()
        .map(|(key, name)| {
            let t = *model_tokens.get(key).unwrap_or(&0);
            let c = *model_cost.get(key).unwrap_or(&0.0);
            ModelRow {
                name: name.to_string(),
                key: key.to_string(),
                tokens: fmt_tokens(t as f64),
                cost: fmt_cost(c),
                pct: if grand_tokens > 0 {
                    ((t as f64 / grand_tokens as f64) * 100.0).round() as u32
                } else {
                    0
                },
            }
        })
        .collect();

    // ---- sessions ----
    let mut sessions: HashMap<String, SessionAgg> = HashMap::new();
    for r in &records {
        let agg = sessions.entry(r.session_id.clone()).or_insert(SessionAgg {
            tokens: 0,
            cost: 0.0,
            project: r.project.clone(),
            last: r.dt,
            family: r.family,
        });
        agg.tokens += r.tokens;
        agg.cost += r.cost;
        if r.dt > agg.last {
            agg.last = r.dt;
            agg.family = r.family;
        }
    }
    let session_count = sessions.len();
    let mut sorted: Vec<(String, SessionAgg)> = sessions.into_iter().collect();
    sorted.sort_by(|a, b| b.1.last.cmp(&a.1.last));
    let mut session_rows: Vec<SessionRow> = sorted
        .iter()
        .take(6)
        .map(|(id, s)| SessionRow {
            id: id.chars().take(8).collect(),
            project: clean_project(&s.project),
            model: s.family.to_string(),
            tokens: s.tokens,
            cost: (s.cost * 100.0).round() / 100.0,
            when: humanize_when(s.last, now),
        })
        .collect();
    while session_rows.len() < 6 {
        session_rows.push(SessionRow {
            id: "—".to_string(),
            project: "—".to_string(),
            model: String::new(),
            tokens: 0,
            cost: 0.0,
            when: "—".to_string(),
        });
    }

    // ---- window bounds for meta ----
    let first = records.iter().map(|r| r.dt).min();
    let last = records.iter().map(|r| r.dt).max();
    let meta = Meta {
        generated: now.format("%Y-%m-%d %H:%M UTC").to_string(),
        generated_ms: now.timestamp_millis(),
        window_first: first.map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_default(),
        window_last: last.map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_default(),
        files_scanned,
    };

    // ---- GLM ----
    let glm = scan_glm(zai_root);

    let providers = vec![
        Provider {
            name: "Claude Code".to_string(),
            status: "connected".to_string(),
            tokens: fmt_tokens(grand_tokens as f64),
            cost: fmt_cost(grand_cost),
            sessions: session_count,
        },
        Provider {
            name: "GLM / z.ai".to_string(),
            status: "connected".to_string(),
            tokens: "—".to_string(),
            cost: "—".to_string(),
            sessions: glm.sessions as usize,
        },
    ];

    let kpi = Kpi {
        session_tokens: fmt_tokens(s_used as f64),
        session_cost: fmt_cost(records.iter().filter(|r| r.dt >= cut_5h).map(|r| r.cost).sum()),
        week_tokens: fmt_tokens(week_tokens_total as f64),
        week_cost: fmt_cost(week_cost_total),
        total_tokens: fmt_tokens(grand_tokens as f64),
        total_cost: fmt_cost(grand_cost),
    };

    UsageSnapshot {
        meta,
        limits,
        kpi,
        week,
        models,
        sessions: session_rows,
        providers,
        glm,
        vendor: None,
        detection: None,
    }
}

fn weekday_abbr(num_from_monday: u32) -> String {
    match num_from_monday {
        0 => "Mon",
        1 => "Tue",
        2 => "Wed",
        3 => "Thu",
        4 => "Fri",
        5 => "Sat",
        _ => "Sun",
    }
    .to_string()
}

fn scan_glm(zai_root: &Path) -> Glm {
    let note = "Local z.ai MCP logs record server lifecycle only — token/cost not exposed locally."
        .to_string();
    let pattern = format!("{}/zai-mcp-*.log", zai_root.to_string_lossy());
    let mut sessions = 0u32;
    let mut active_days: Vec<String> = Vec::new();
    let mut last = String::new();

    let paths: Vec<PathBuf> = match glob::glob(&pattern) {
        Ok(p) => p.filter_map(Result::ok).collect(),
        Err(_) => Vec::new(),
    };
    for fp in &paths {
        let date = fp
            .file_name()
            .map(|n| {
                n.to_string_lossy()
                    .replace("zai-mcp-", "")
                    .replace(".log", "")
            })
            .unwrap_or_default();
        let Ok(content) = std::fs::read_to_string(fp) else { continue };
        let mut day_has = false;
        for line in content.lines() {
            if line.contains("MCP Server started successfully") {
                sessions += 1;
                day_has = true;
                if let Some(idx) = line.find(']') {
                    let ts = line[1..idx].to_string();
                    if ts > last {
                        last = ts;
                    }
                }
            }
        }
        if day_has && !active_days.contains(&date) {
            active_days.push(date);
        }
    }

    Glm {
        sessions,
        active_days: active_days.len(),
        last: if last.is_empty() {
            "—".to_string()
        } else {
            last.chars().take(10).collect()
        },
        note,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_jsonl(dir: &Path, project: &str, lines: &[&str]) {
        let pdir = dir.join(project);
        std::fs::create_dir_all(&pdir).unwrap();
        let mut f = std::fs::File::create(pdir.join("session.jsonl")).unwrap();
        for l in lines {
            writeln!(f, "{l}").unwrap();
        }
    }

    #[test]
    fn aggregates_tokens_and_models() {
        let tmp = tempfile::tempdir().unwrap();
        let claude = tmp.path().join("claude");
        let zai = tmp.path().join("zai");
        std::fs::create_dir_all(&zai).unwrap();
        let now = DateTime::parse_from_rfc3339("2026-06-17T20:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let recent = "2026-06-17T19:00:00.000Z";

        let line = format!(
            r#"{{"timestamp":"{recent}","sessionId":"abc12345","message":{{"model":"claude-opus-4-7","usage":{{"input_tokens":100,"output_tokens":200,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}}}}"#
        );
        write_jsonl(&claude, "proj-a", &[&line]);

        let snap = scan(&claude, &zai, "max5x", now);
        assert_eq!(snap.meta.files_scanned, 1);
        // 300 tokens total this session
        assert_eq!(snap.kpi.session_tokens, "300");
        // opus family present with non-zero
        let opus = snap.models.iter().find(|m| m.key == "opus").unwrap();
        assert_eq!(opus.tokens, "300");
        // session bucket should be healthy and have a 5h reset
        assert_eq!(snap.limits.buckets[0].status, "ok");
        assert!(snap.limits.buckets[0].reset.contains('h') || snap.limits.buckets[0].reset.contains('m'));
        // three model rows always present
        assert_eq!(snap.models.len(), 3);
        // six session rows (padded)
        assert_eq!(snap.sessions.len(), 6);
    }

    #[test]
    fn dedupes_repeated_message_request_ids() {
        let tmp = tempfile::tempdir().unwrap();
        let claude = tmp.path().join("claude");
        let zai = tmp.path().join("zai");
        std::fs::create_dir_all(&zai).unwrap();
        let now = DateTime::parse_from_rfc3339("2026-06-17T20:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let recent = "2026-06-17T19:00:00.000Z";

        // Same (message.id, requestId) appearing twice — e.g. a resumed session
        // copied into a second file — must be counted once.
        let line = format!(
            r#"{{"timestamp":"{recent}","sessionId":"abc12345","requestId":"req_1","message":{{"id":"msg_1","model":"claude-opus-4-7","usage":{{"input_tokens":100,"output_tokens":200,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}}}}"#
        );
        write_jsonl(&claude, "proj-a", &[&line]);
        write_jsonl(&claude, "proj-b", &[&line]);

        let snap = scan(&claude, &zai, "max5x", now);
        assert_eq!(snap.meta.files_scanned, 2);
        // 300 tokens counted once, not 600.
        assert_eq!(snap.kpi.session_tokens, "300");
    }

    #[test]
    fn empty_roots_do_not_panic() {
        let tmp = tempfile::tempdir().unwrap();
        let now = Utc::now();
        let snap = scan(&tmp.path().join("none"), &tmp.path().join("nozai"), "pro", now);
        assert_eq!(snap.meta.files_scanned, 0);
        assert_eq!(snap.sessions.len(), 6);
        assert_eq!(snap.week.len(), 7);
    }

    #[test]
    fn token_formatting() {
        assert_eq!(fmt_tokens(500.0), "500");
        assert_eq!(fmt_tokens(1_500.0), "2K");
        assert_eq!(fmt_tokens(1_500_000.0), "1.5M");
        assert_eq!(fmt_tokens(2_500_000_000.0), "2.50B");
    }
}
