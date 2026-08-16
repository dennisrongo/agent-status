//! Local CLI usage scanner. Reads every agent CLI's on-disk session log and
//! aggregates them into the snapshot the frontend renders:
//!
//! | Provider | Source | Per-session tokens |
//! |---|---|---|
//! | Claude Code | `~/.claude/projects/**/*.jsonl` | exact, per message |
//! | Kimi Code | `~/.kimi-code/sessions/**/wire.jsonl` | exact, per turn |
//! | Copilot CLI | `~/.copilot/session-state/*/events.jsonl` | totals at shutdown only |
//! | GLM / z.ai | `~/.zai/*.log` | none — server lifecycle only |
//! | Grok Build | `~/.grok/sessions/**/summary.json` + `updates.jsonl` | per-turn when billed usage was recorded |
//!
//! Alibaba has no local coding-session log at all (the `bl` CLI is a one-shot
//! API client), so it contributes vendor-side quota but no rows.
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
    /// z.ai 7-day usage chart (tokens/day + per-model breakdown), filled in by
    /// the command layer from the monitor `model-usage` endpoint. Distinct from
    /// `vendor.glm`, which carries the quota windows only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub glm_week: Option<crate::vendors::glm::GlmWeek>,
    /// Local Grok CLI token totals (7-day chart + per-model), built from
    /// session logs. SuperGrok has no public % ceiling, so this is the real
    /// usage the Overview can show.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grok_week: Option<GrokWeek>,
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
    /// True when a Claude Code login exists but was rejected (HTTP 401) — the
    /// token expired. The UI shows an actionable "sign in again" state instead
    /// of an indistinguishable "loading…" spinner.
    #[serde(default)]
    pub needs_reauth: bool,
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
    pub provider: String,
    /// Pre-rendered token figure. Providers differ in what they record, so the
    /// display string is built here rather than in the UI: `"1.2M"` when the
    /// count is real, `"—"` when the provider doesn't expose one.
    pub tokens_text: String,
    /// Pre-rendered secondary figure. Dollars for Claude (estimated from
    /// standard-tier pricing), premium requests for Copilot, `"—"` where the
    /// provider bills on a flat-rate plan and a dollar figure would be a lie.
    pub cost_text: String,
    /// The ordering instant behind `when` — never serialized (the UI reads the
    /// humanized string), but carried so the command layer can interleave
    /// vendor-sourced rows (z.ai monitor activity) by recency.
    #[serde(skip_serializing)]
    pub at: Option<DateTime<Utc>>,
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

/// Local Grok CLI usage for the xAI Overview. `cost_fmt` on each day is
/// unused (flat-rate plan) and left as an em dash.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GrokWeek {
    pub days: Vec<WeekDay>,
    pub models: Vec<ModelRow>,
    /// Tokens in the last 5 hours — local spend, not a vendor quota window.
    pub session_tokens: String,
    pub week_tokens: String,
    pub total_tokens: String,
    pub sessions: usize,
    pub last: String,
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

/// Placeholder for a figure a provider doesn't record locally.
const EM_DASH: &str = "—";

/// Most-recent session rows kept per provider. The Sessions list is scrollable,
/// not paginated, so an unbounded history would render an unbounded DOM.
/// Shared with the z.ai monitor rows the command layer appends.
pub(crate) const MAX_PROVIDER_ROWS: usize = 25;

/// Roll-up of one provider's session rows for the Providers tab.
struct ProviderTotals {
    sessions: usize,
    tokens: u64,
}

impl ProviderTotals {
    fn of(rows: &[(DateTime<Utc>, SessionRow)]) -> Self {
        Self {
            sessions: rows.len(),
            tokens: rows.iter().map(|(_, r)| r.tokens).sum(),
        }
    }
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

/// Compact token figure ("1.5M"), shared by session rows and the command
/// layer's z.ai monitor rows so both providers render on the same scale.
pub(crate) fn fmt_tokens(n: f64) -> String {
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

/// Relative-time label, shared by session rows and the command layer's
/// z.ai monitor rows.
pub(crate) fn humanize_when(ts: DateTime<Utc>, now: DateTime<Utc>) -> String {
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

/// Every local log root the scanner reads. Grouped into one struct so adding a
/// provider doesn't ripple through every call site.
#[derive(Debug, Clone, Default)]
pub struct ScanRoots {
    /// `~/.claude/projects` — Claude Code session JSONL.
    pub claude: PathBuf,
    /// `~/.zai` — z.ai MCP server lifecycle logs.
    pub zai: PathBuf,
    /// `$KIMI_CODE_HOME` (default `~/.kimi-code`) — Kimi Code session store.
    pub kimi: PathBuf,
    /// `~/.copilot` — GitHub Copilot CLI session state.
    pub copilot: PathBuf,
    /// `$GROK_HOME` (default `~/.grok`) — Grok Build session store.
    pub grok: PathBuf,
}

impl ScanRoots {
    /// Resolve every root under a home directory, honoring `$KIMI_CODE_HOME`
    /// and `$GROK_HOME` the same way the matching vendor clients do.
    pub fn for_home(home: &Path) -> Self {
        let kimi = std::env::var_os("KIMI_CODE_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".kimi-code"));
        let grok = std::env::var_os("GROK_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".grok"));
        Self {
            claude: home.join(".claude").join("projects"),
            zai: home.join(".zai"),
            kimi,
            copilot: home.join(".copilot"),
            grok,
        }
    }
}

/// Scan using the real home directory and the current time.
pub fn scan_default(plan: &str) -> Result<UsageSnapshot, ScanError> {
    let home = dirs::home_dir().ok_or(ScanError::NoHome)?;
    Ok(scan_roots(&ScanRoots::for_home(&home), plan, Utc::now()))
}

/// Claude + GLM only. Kept as the narrow entry point for tests that don't
/// exercise the other providers.
pub fn scan(
    claude_root: &Path,
    zai_root: &Path,
    plan: &str,
    now: DateTime<Utc>,
) -> UsageSnapshot {
    scan_roots(
        &ScanRoots {
            claude: claude_root.to_path_buf(),
            zai: zai_root.to_path_buf(),
            ..Default::default()
        },
        plan,
        now,
    )
}

/// Pure-ish scan over explicit roots and clock — used by tests.
pub fn scan_roots(roots: &ScanRoots, plan: &str, now: DateTime<Utc>) -> UsageSnapshot {
    let claude_root = roots.claude.as_path();
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

    build_snapshot(records, files_scanned, roots, plan, now)
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
    roots: &ScanRoots,
    plan: &str,
    now: DateTime<Utc>,
) -> UsageSnapshot {
    let zai_root = roots.zai.as_path();
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
        needs_reauth: false,
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
    // Every provider that keeps a local session store contributes rows, and the
    // combined set is ordered by most-recent activity (newest first). What each
    // provider can actually report differs — see `scan_kimi` / `scan_copilot`
    // for the per-provider caveats — so token and cost figures are rendered to
    // strings here and an em dash stands in for anything unavailable.
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

    // Cap the rows each provider contributes to the most-recent N. The list is
    // scrollable, not infinite — a long history would otherwise render an
    // unbounded DOM. The GLM summary row is added afterward and always kept.
    let mut claude: Vec<(&String, &SessionAgg)> = sessions.iter().collect();
    claude.sort_by(|a, b| b.1.last.cmp(&a.1.last));

    // Each row carries the instant used for ordering, so rows from different
    // providers interleave by recency. `when` is derived last.
    let mut rows: Vec<(DateTime<Utc>, SessionRow)> = Vec::new();
    for (id, s) in claude.into_iter().take(MAX_PROVIDER_ROWS) {
        rows.push((
            s.last,
            SessionRow {
                id: id.chars().take(8).collect(),
                project: clean_project(&s.project),
                model: s.family.to_string(),
                tokens: s.tokens,
                cost: (s.cost * 100.0).round() / 100.0,
                when: String::new(),
                at: None,
                provider: "claude".to_string(),
                tokens_text: fmt_tokens(s.tokens as f64),
                cost_text: fmt_cost(s.cost),
            },
        ));
    }

    // ---- GLM ----
    let (glm, glm_latest) = scan_glm(zai_root);
    if glm.sessions > 0 {
        let when_dt = glm_latest.unwrap_or(now);
        rows.push((
            when_dt,
            SessionRow {
                id: "glm".to_string(),
                project: "Z.ai".to_string(),
                model: String::new(),
                tokens: 0,
                cost: 0.0,
                when: String::new(),
                at: None,
                provider: "glm".to_string(),
                tokens_text: EM_DASH.to_string(),
                cost_text: EM_DASH.to_string(),
            },
        ));
    }

    // ---- Kimi Code, Copilot, Grok ----
    let kimi = scan_kimi(roots.kimi.as_path());
    let copilot = scan_copilot(roots.copilot.as_path());
    let (grok, grok_turns) = scan_grok(roots.grok.as_path(), now);
    let kimi_total = ProviderTotals::of(&kimi);
    let copilot_total = ProviderTotals::of(&copilot);
    let grok_total = ProviderTotals::of(&grok);
    let grok_week = if grok.is_empty() {
        None
    } else {
        Some(build_grok_week(&grok, &grok_turns, now))
    };
    rows.extend(kimi);
    rows.extend(copilot);
    rows.extend(grok);

    // Newest first, then assign humanized `when` from the ordering instant —
    // and carry the instant itself, so vendor-sourced rows appended by the
    // command layer (z.ai monitor activity) can interleave by real recency.
    rows.sort_by(|a, b| b.0.cmp(&a.0));
    let session_rows: Vec<SessionRow> = rows
        .into_iter()
        .map(|(dt, mut row)| {
            row.when = humanize_when(dt, now);
            row.at = Some(dt);
            row
        })
        .collect();

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

    let mut providers = vec![
        Provider {
            name: "Anthropic".to_string(),
            status: "connected".to_string(),
            tokens: fmt_tokens(grand_tokens as f64),
            cost: fmt_cost(grand_cost),
            sessions: session_count,
        },
        Provider {
            name: "Z.ai".to_string(),
            status: "connected".to_string(),
            tokens: EM_DASH.to_string(),
            cost: EM_DASH.to_string(),
            sessions: glm.sessions as usize,
        },
    ];
    if kimi_total.sessions > 0 {
        providers.push(Provider {
            name: "Moonshot".to_string(),
            status: "connected".to_string(),
            tokens: fmt_tokens(kimi_total.tokens as f64),
            cost: EM_DASH.to_string(),
            sessions: kimi_total.sessions,
        });
    }
    if copilot_total.sessions > 0 {
        providers.push(Provider {
            name: "GitHub".to_string(),
            status: "connected".to_string(),
            tokens: fmt_tokens(copilot_total.tokens as f64),
            cost: EM_DASH.to_string(),
            sessions: copilot_total.sessions,
        });
    }
    if grok_total.sessions > 0 {
        providers.push(Provider {
            name: "xAI".to_string(),
            status: "connected".to_string(),
            tokens: if grok_total.tokens > 0 {
                fmt_tokens(grok_total.tokens as f64)
            } else {
                EM_DASH.to_string()
            },
            cost: EM_DASH.to_string(),
            sessions: grok_total.sessions,
        });
    }

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
        // z.ai 7-day usage chart — filled in by the command layer after a live
        // fetch, like `vendor`/`detection` (the scanner sees no z.ai data).
        glm_week: None,
        grok_week,
        detection: None,
    }
}

/// Three-letter weekday label, shared with the z.ai 7-day usage chart
/// (`vendors::glm::GlmWeek` reuses the same `WeekDay` row shape).
pub(crate) fn weekday_abbr(num_from_monday: u32) -> String {
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

// ---------- Kimi Code ----------

/// Per-turn token usage the Kimi Code CLI writes to a session's wire log.
struct KimiUsage {
    tokens: u64,
    model: String,
    at: DateTime<Utc>,
}

/// Read the most-recent Kimi Code sessions from the CLI's local session store.
///
/// Layout is `<root>/sessions/wd_<name>_<hash>/session_<uuid>/` holding a small
/// `state.json` (cwd, timestamps, title) plus one `agents/<agent>/wire.jsonl`
/// per agent — the main loop and each subagent. Wire logs carry `usage.record`
/// events with a per-turn token breakdown, so Kimi rows are exact in the same
/// way Claude rows are; a subagent's usage lives only in its own wire log, so
/// summing across every agent counts each turn once.
///
/// Wire logs run to hundreds of KB, so only the newest sessions (by
/// `state.json`'s `updatedAt`) are opened — the rest are capped away anyway.
fn scan_kimi(root: &Path) -> Vec<(DateTime<Utc>, SessionRow)> {
    let pattern = format!("{}/sessions/*/session_*/state.json", root.to_string_lossy());
    let states: Vec<PathBuf> = match glob::glob(&pattern) {
        Ok(p) => p.filter_map(Result::ok).collect(),
        Err(_) => Vec::new(),
    };

    // (updated_at, session_dir, id, cwd) for each readable state file.
    let mut candidates: Vec<(DateTime<Utc>, PathBuf, String, String)> = Vec::new();
    for sp in &states {
        let Ok(raw) = std::fs::read_to_string(sp) else { continue };
        let Ok(v) = serde_json::from_str::<Value>(&raw) else { continue };
        if v["archived"].as_bool().unwrap_or(false) {
            continue;
        }
        let updated = v["updatedAt"]
            .as_i64()
            .or_else(|| v["createdAt"].as_i64())
            .and_then(DateTime::from_timestamp_millis);
        let Some(updated) = updated else { continue };
        let Some(dir) = sp.parent() else { continue };
        let id = v["id"].as_str().unwrap_or_default().to_string();
        let cwd = v["cwd"].as_str().unwrap_or_default().to_string();
        candidates.push((updated, dir.to_path_buf(), id, cwd));
    }
    candidates.sort_by(|a, b| b.0.cmp(&a.0));

    let mut rows = Vec::new();
    for (updated, dir, id, cwd) in candidates.into_iter().take(MAX_PROVIDER_ROWS) {
        let usage = read_kimi_usage(&dir);
        let tokens: u64 = usage.iter().map(|u| u.tokens).sum();
        // Order by real activity when the wire logs have any; `updatedAt` also
        // moves for non-LLM edits (title changes, archiving).
        let last = usage.iter().map(|u| u.at).max().unwrap_or(updated);
        let model = usage
            .iter()
            .max_by_key(|u| u.at)
            .map(|u| u.model.clone())
            .unwrap_or_default();
        rows.push((
            last,
            SessionRow {
                id: kimi_short_id(&id, &dir),
                project: project_from_cwd(&cwd),
                model,
                tokens,
                cost: 0.0,
                when: String::new(),
                at: None,
                provider: "kimi".to_string(),
                tokens_text: if tokens > 0 {
                    fmt_tokens(tokens as f64)
                } else {
                    EM_DASH.to_string()
                },
                // Kimi Code bills against a flat-rate coding plan, not per
                // token — a dollar figure here would be invented.
                cost_text: EM_DASH.to_string(),
            },
        ));
    }
    rows
}

/// Sum `usage.record` events across every agent's wire log in one session dir.
fn read_kimi_usage(session_dir: &Path) -> Vec<KimiUsage> {
    let pattern = format!("{}/agents/*/wire.jsonl", session_dir.to_string_lossy());
    let wires: Vec<PathBuf> = match glob::glob(&pattern) {
        Ok(p) => p.filter_map(Result::ok).collect(),
        Err(_) => Vec::new(),
    };
    let mut out = Vec::new();
    for wire in &wires {
        for line in read_lines(wire) {
            // Cheap reject before the JSON parse — usage records are a small
            // fraction of a wire log's lines.
            if !line.contains("\"usage.record\"") {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
            if v["type"].as_str() != Some("usage.record") {
                continue;
            }
            let u = &v["usage"];
            let tokens = u["inputOther"].as_u64().unwrap_or(0)
                + u["inputCacheRead"].as_u64().unwrap_or(0)
                + u["inputCacheCreation"].as_u64().unwrap_or(0)
                + u["output"].as_u64().unwrap_or(0);
            let Some(at) = v["time"].as_i64().and_then(DateTime::from_timestamp_millis) else {
                continue;
            };
            // Models are reported namespaced (`kimi-code/k3`); the badge wants
            // the bare name.
            let model = v["model"]
                .as_str()
                .unwrap_or("")
                .rsplit('/')
                .next()
                .unwrap_or("")
                .to_string();
            out.push(KimiUsage { tokens, model, at });
        }
    }
    out
}

/// `session_<uuid>` → the first 8 chars of the uuid, falling back to the
/// directory name when `state.json` has no id.
fn kimi_short_id(id: &str, dir: &Path) -> String {
    let raw = if id.is_empty() {
        dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
    } else {
        id.to_string()
    };
    raw.trim_start_matches("session_").chars().take(8).collect()
}

// ---------- GitHub Copilot ----------

/// Read the most-recent Copilot CLI sessions from `<root>/session-state`.
///
/// Each session is a directory of append-only `events.jsonl`. Project, model
/// and timing come from `session.start` / `session.resume`; **token counts only
/// appear in `session.shutdown`**, which older CLI builds omit entirely and a
/// still-running session hasn't written yet. Those rows report `—` rather than
/// a fabricated zero.
///
/// A session that is resumed emits one shutdown per process. Current builds
/// restore the running totals on resume, so those snapshots are cumulative and
/// must not be summed; older builds restart the counters from zero, so the
/// final snapshot can be *lower* than an earlier one. Taking the maximum is
/// correct for the first case and recovers the real peak in the second.
fn scan_copilot(root: &Path) -> Vec<(DateTime<Utc>, SessionRow)> {
    let pattern = format!("{}/session-state/*/events.jsonl", root.to_string_lossy());
    let mut logs: Vec<PathBuf> = match glob::glob(&pattern) {
        Ok(p) => p.filter_map(Result::ok).collect(),
        Err(_) => Vec::new(),
    };
    // Newest first by file mtime so the cap keeps the interesting sessions;
    // the authoritative instant still comes from the events themselves.
    logs.sort_by_key(|p| {
        std::cmp::Reverse(
            std::fs::metadata(p)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0),
        )
    });

    let mut rows = Vec::new();
    for log in logs.into_iter().take(MAX_PROVIDER_ROWS) {
        let mut cwd = String::new();
        let mut model = String::new();
        let mut last: Option<DateTime<Utc>> = None;
        let mut tokens: Option<u64> = None;
        let mut premium: Option<f64> = None;

        for line in read_lines(&log) {
            // Event logs reach single-digit MB and are re-read on every
            // refresh, so the timestamp — needed from every line — is pulled
            // out by substring scan and only the handful of `session.*` lines
            // are handed to the JSON parser.
            if let Some(ts) = extract_timestamp(&line) {
                last = Some(match last {
                    Some(prev) if prev > ts => prev,
                    _ => ts,
                });
            }
            if !line.contains("\"type\":\"session.") {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
            let data = &v["data"];
            match v["type"].as_str() {
                Some("session.start") | Some("session.resume") => {
                    if let Some(c) = data["context"]["cwd"].as_str() {
                        cwd = c.to_string();
                    }
                    if let Some(m) = data["selectedModel"].as_str() {
                        model = m.to_string();
                    }
                }
                Some("session.shutdown") => {
                    if let Some(t) = copilot_shutdown_tokens(data) {
                        tokens = Some(tokens.map_or(t, |prev: u64| prev.max(t)));
                    }
                    if let Some(p) = data["totalPremiumRequests"].as_f64() {
                        premium = Some(premium.map_or(p, |prev: f64| prev.max(p)));
                    }
                }
                _ => {}
            }
        }

        let Some(last) = last else { continue };
        let id = log
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().chars().take(8).collect())
            .unwrap_or_default();
        rows.push((
            last,
            SessionRow {
                id,
                project: project_from_cwd(&cwd),
                // `claude-opus-4.7` → the `opus` badge the UI already styles.
                model: if model.is_empty() {
                    String::new()
                } else {
                    family_of(&model).to_string()
                },
                tokens: tokens.unwrap_or(0),
                cost: 0.0,
                when: String::new(),
                at: None,
                provider: "copilot".to_string(),
                tokens_text: tokens.map(|t| fmt_tokens(t as f64)).unwrap_or_else(|| EM_DASH.to_string()),
                // Copilot meters premium requests, not dollars.
                cost_text: premium
                    .map(|p| format!("{} premium", trim_float(p)))
                    .unwrap_or_else(|| EM_DASH.to_string()),
            },
        ));
    }
    rows
}

/// Total tokens from one `session.shutdown` payload.
///
/// `modelMetrics[*].usage.inputTokens` already folds in cache reads and writes,
/// so input + output is the whole session. Builds that predate `modelMetrics`
/// still carry the flat `tokenDetails` breakdown; anything older reports
/// neither and yields `None`.
fn copilot_shutdown_tokens(data: &Value) -> Option<u64> {
    if let Some(metrics) = data["modelMetrics"].as_object() {
        let mut total = 0u64;
        let mut any = false;
        for m in metrics.values() {
            let usage = &m["usage"];
            if usage.is_null() {
                continue;
            }
            any = true;
            total += usage["inputTokens"].as_u64().unwrap_or(0)
                + usage["outputTokens"].as_u64().unwrap_or(0);
        }
        if any {
            return Some(total);
        }
    }
    let details = data["tokenDetails"].as_object()?;
    let mut total = 0u64;
    for d in details.values() {
        total += d["tokenCount"].as_u64().unwrap_or(0);
    }
    Some(total)
}

// ---------- Grok Build ----------

/// Read the most-recent Grok Build sessions from `$GROK_HOME/sessions`.
///
/// Layout is `<root>/sessions/<encoded-cwd>/<session-id>/summary.json` plus an
/// append-only `updates.jsonl`. Metadata (cwd, model, timestamps) comes from
/// the summary; billed tokens only appear on per-turn `usage` objects that
/// carry `input_tokens` / `inputTokens`. Bare `_meta.totalTokens` is the
/// context-window size and must not be treated as spend — a session without
/// billed usage reports `—`, matching Copilot's in-progress rows.
///
/// Grok Build is a flat-rate subscription, so the cost column is always `—`.
///
/// Also returns per-turn `(at, tokens, model)` records so the Overview can
/// draw a 7-day chart — SuperGrok publishes no % ceiling, so local tokens
/// are the real usage figure. Session *rows* stay capped at
/// [`MAX_PROVIDER_ROWS`]; week/5h turns are read from every session whose
/// summary time falls in the last 7 days so the chart is not “newest 25”.
fn scan_grok(
    root: &Path,
    now: DateTime<Utc>,
) -> (Vec<(DateTime<Utc>, SessionRow)>, Vec<(DateTime<Utc>, u64, String)>) {
    let pattern = format!("{}/sessions/*/*/summary.json", root.to_string_lossy());
    let summaries: Vec<PathBuf> = match glob::glob(&pattern) {
        Ok(p) => p.filter_map(Result::ok).collect(),
        Err(_) => Vec::new(),
    };

    let mut candidates: Vec<(DateTime<Utc>, PathBuf, Value)> = Vec::new();
    for sp in &summaries {
        let Ok(raw) = std::fs::read_to_string(sp) else { continue };
        let Ok(v) = serde_json::from_str::<Value>(&raw) else { continue };
        let last = grok_summary_time(&v);
        let Some(last) = last else { continue };
        let Some(dir) = sp.parent() else { continue };
        candidates.push((last, dir.to_path_buf(), v));
    }
    candidates.sort_by(|a, b| b.0.cmp(&a.0));

    let cut_7d = now - Duration::days(7);
    let mut rows = Vec::new();
    let mut turns: Vec<(DateTime<Utc>, u64, String)> = Vec::new();
    for (i, (updated, dir, summary)) in candidates.iter().enumerate() {
        let for_row = i < MAX_PROVIDER_ROWS;
        let for_week = *updated >= cut_7d;
        if !for_row && !for_week {
            continue;
        }
        let id = summary
            .pointer("/info/id")
            .and_then(|v| v.as_str())
            .or_else(|| summary.get("id").and_then(|v| v.as_str()))
            .unwrap_or("");
        let cwd = summary
            .pointer("/info/cwd")
            .and_then(|v| v.as_str())
            .or_else(|| summary.get("cwd").and_then(|v| v.as_str()))
            .unwrap_or("");
        let model = summary
            .get("current_model_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .rsplit('/')
            .next()
            .unwrap_or("")
            .to_string();
        let events = read_grok_usage(dir, *updated, &model);
        let tokens: u64 = events.iter().map(|e| e.1).sum();
        let usage_last = events.iter().map(|e| e.0).max();
        let last = usage_last.unwrap_or(*updated);
        turns.extend(events);
        if !for_row {
            continue;
        }
        rows.push((
            last,
            SessionRow {
                id: grok_short_id(id, dir),
                project: project_from_cwd(cwd),
                model,
                tokens,
                cost: 0.0,
                when: String::new(),
                at: None,
                provider: "grok".to_string(),
                tokens_text: if tokens > 0 {
                    fmt_tokens(tokens as f64)
                } else {
                    EM_DASH.to_string()
                },
                cost_text: EM_DASH.to_string(),
            },
        ));
    }
    (rows, turns)
}

fn build_grok_week(
    rows: &[(DateTime<Utc>, SessionRow)],
    turns: &[(DateTime<Utc>, u64, String)],
    now: DateTime<Utc>,
) -> GrokWeek {
    let cut_5h = now - Duration::hours(5);
    let cut_7d = now - Duration::days(7);
    let mut day_tokens: HashMap<String, u64> = HashMap::new();
    let mut model_tokens: HashMap<String, u64> = HashMap::new();
    let mut session_tokens = 0u64;
    let mut week_tokens = 0u64;
    let mut total_tokens = 0u64;
    for (at, n, model) in turns {
        total_tokens = total_tokens.saturating_add(*n);
        if *at >= cut_5h {
            session_tokens = session_tokens.saturating_add(*n);
        }
        if *at >= cut_7d {
            week_tokens = week_tokens.saturating_add(*n);
            let key = at.format("%Y-%m-%d").to_string();
            *day_tokens.entry(key).or_insert(0) += *n;
            if !model.is_empty() {
                *model_tokens.entry(model.clone()).or_insert(0) += *n;
            }
        }
    }
    // Sessions with no billed turns still count toward the session total,
    // but they don't move the day/model charts.
    let today = now.date_naive();
    let mut week_max = 1u64;
    let mut days: Vec<WeekDay> = Vec::with_capacity(7);
    for i in (0..7).rev() {
        let d = today - chrono::Days::new(i);
        let key = d.format("%Y-%m-%d").to_string();
        let toks = *day_tokens.get(&key).unwrap_or(&0);
        week_max = week_max.max(toks);
        days.push(WeekDay {
            day: weekday_abbr(d.weekday().num_days_from_monday()),
            date: key,
            tok_fmt: fmt_tokens(toks as f64),
            cost_fmt: EM_DASH.to_string(),
            bar_pct: 0,
        });
    }
    for w in days.iter_mut() {
        let toks = day_tokens.get(&w.date).copied().unwrap_or(0);
        w.bar_pct = ((toks as f64 / week_max as f64) * 100.0).round() as u32;
    }

    let grand = week_tokens.max(1);
    let mut model_pairs: Vec<(String, u64)> = model_tokens.into_iter().collect();
    model_pairs.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let models: Vec<ModelRow> = model_pairs
        .into_iter()
        .map(|(name, t)| ModelRow {
            key: name.clone(),
            name,
            tokens: fmt_tokens(t as f64),
            cost: EM_DASH.to_string(),
            pct: ((t as f64 / grand as f64) * 100.0).round() as u32,
        })
        .collect();

    let last = rows
        .iter()
        .map(|(dt, _)| *dt)
        .max()
        .map(|dt| humanize_when(dt, now))
        .unwrap_or_else(|| EM_DASH.to_string());

    GrokWeek {
        days,
        models,
        session_tokens: if session_tokens > 0 {
            fmt_tokens(session_tokens as f64)
        } else {
            EM_DASH.to_string()
        },
        week_tokens: if week_tokens > 0 {
            fmt_tokens(week_tokens as f64)
        } else {
            EM_DASH.to_string()
        },
        total_tokens: if total_tokens > 0 {
            fmt_tokens(total_tokens as f64)
        } else {
            EM_DASH.to_string()
        },
        sessions: rows.len(),
        last,
    }
}

fn grok_summary_time(v: &Value) -> Option<DateTime<Utc>> {
    for key in ["last_active_at", "updated_at", "created_at"] {
        if let Some(s) = v.get(key).and_then(|t| t.as_str()) {
            if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
                return Some(dt.with_timezone(&Utc));
            }
        }
    }
    None
}

/// Sum billed per-turn usage from `updates.jsonl` (and `signals.json` if
/// present). Only objects that carry an `input_tokens` / `inputTokens` field
/// count — that's the spend shape. Context-window `totalTokens` is ignored.
fn read_grok_usage(
    session_dir: &Path,
    fallback_at: DateTime<Utc>,
    fallback_model: &str,
) -> Vec<(DateTime<Utc>, u64, String)> {
    let mut events: Vec<(DateTime<Utc>, u64, String)> = Vec::new();

    let updates = session_dir.join("updates.jsonl");
    for line in read_lines(&updates) {
        if !(line.contains("\"input_tokens\"") || line.contains("\"inputTokens\"")) {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
        let parts = grok_line_events(&v, fallback_model);
        if parts.is_empty() {
            continue;
        }
        let at = grok_update_time(&v).unwrap_or(fallback_at);
        for (n, model) in parts {
            events.push((at, n, model));
        }
    }
    // signals.json is a session-end roll-up. Prefer the per-turn updates so a
    // running session (no signals file yet) still counts, and so we don't
    // double-count a completed one.
    if events.is_empty() {
        if let Some((t, at)) = read_grok_signals(&session_dir.join("signals.json")) {
            events.push((at.unwrap_or(fallback_at), t, fallback_model.to_string()));
        }
    }
    events
}

fn read_grok_signals(path: &Path) -> Option<(u64, Option<DateTime<Utc>>)> {
    let raw = std::fs::read_to_string(path).ok()?;
    let v: Value = serde_json::from_str(&raw).ok()?;
    let n = grok_turn_tokens(&v)
        .or_else(|| {
            v.get("tokens_used")
                .or_else(|| v.get("tokensUsed"))
                .and_then(|t| t.as_u64())
                .filter(|&n| n > 0)
        })?;
    Some((n, None))
}

/// Pull billed tokens from a usage object. Requires an input-token field so
/// a context-window `totalTokens` (present on every stream chunk) can't be
/// mistaken for spend.
fn grok_turn_tokens(v: &Value) -> Option<u64> {
    let parts = grok_line_events(v, "");
    if parts.is_empty() {
        None
    } else {
        Some(parts.iter().map(|(n, _)| *n).sum())
    }
}

/// One JSONL line may split spend across `modelUsage` keys (the CLI's
/// `turn_completed` shape). Fall back to a single total + the session model.
fn grok_line_events(v: &Value, fallback_model: &str) -> Vec<(u64, String)> {
    let candidates = [
        v.get("usage"),
        v.pointer("/params/update/usage"),
        v.pointer("/params/_meta/usage"),
        v.get("_meta").and_then(|m| m.get("usage")),
        Some(v),
    ];
    for u in candidates.into_iter().flatten() {
        if let Some(map) = u
            .get("modelUsage")
            .or_else(|| u.get("model_usage"))
            .and_then(|m| m.as_object())
        {
            let mut out = Vec::new();
            for (name, mu) in map {
                if let Some(n) = grok_usage_token_sum(mu) {
                    let short = name.rsplit('/').next().unwrap_or(name).to_string();
                    out.push((n, short));
                }
            }
            if !out.is_empty() {
                return out;
            }
        }
        if let Some(n) = grok_usage_token_sum(u) {
            return vec![(n, fallback_model.to_string())];
        }
    }
    Vec::new()
}

fn grok_usage_token_sum(u: &Value) -> Option<u64> {
    let input = u
        .get("input_tokens")
        .or_else(|| u.get("inputTokens"))
        .and_then(|x| x.as_u64())?;
    let output = u
        .get("output_tokens")
        .or_else(|| u.get("outputTokens"))
        .and_then(|x| x.as_u64())
        .unwrap_or(0);
    // Anthropic-style cache fields only. Grok's `cachedReadTokens` is already
    // included in `inputTokens` — adding it would double-count.
    let cache_r = u
        .get("cache_read_input_tokens")
        .or_else(|| u.get("cacheReadInputTokens"))
        .and_then(|x| x.as_u64())
        .unwrap_or(0);
    let cache_c = u
        .get("cache_creation_input_tokens")
        .or_else(|| u.get("cacheCreationInputTokens"))
        .and_then(|x| x.as_u64())
        .unwrap_or(0);
    Some(input + output + cache_r + cache_c)
}

fn grok_update_time(v: &Value) -> Option<DateTime<Utc>> {
    if let Some(ms) = v
        .pointer("/params/_meta/agentTimestampMs")
        .or_else(|| v.pointer("/_meta/agentTimestampMs"))
        .and_then(|t| t.as_i64())
    {
        return DateTime::from_timestamp_millis(ms);
    }
    if let Some(secs) = v.get("timestamp").and_then(|t| t.as_i64()) {
        return DateTime::from_timestamp(secs, 0);
    }
    None
}

fn grok_short_id(id: &str, dir: &Path) -> String {
    let raw = if id.is_empty() {
        dir.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default()
    } else {
        id.to_string()
    };
    raw.chars().take(8).collect()
}

// ---------- Shared helpers ----------

/// Read a file as lines, yielding nothing when it can't be opened. Streaming
/// keeps multi-hundred-KB wire/event logs off the heap all at once.
fn read_lines(path: &Path) -> impl Iterator<Item = String> {
    use std::io::BufRead;
    std::fs::File::open(path)
        .ok()
        .map(|f| std::io::BufReader::new(f).lines().map_while(Result::ok))
        .into_iter()
        .flatten()
}

/// Pull an event's own RFC3339 `"timestamp"` out of a JSONL line without
/// parsing it.
///
/// Searches from the right because Copilot writes the top-level `timestamp`
/// near the end of each object, after the `data` payload — which may carry
/// timestamps of its own (tool inputs record epoch millis, unquoted, so they
/// can't match this pattern anyway).
fn extract_timestamp(line: &str) -> Option<DateTime<Utc>> {
    const KEY: &str = "\"timestamp\":\"";
    let start = line.rfind(KEY)? + KEY.len();
    let rest = &line[start..];
    let end = rest.find('"')?;
    DateTime::parse_from_rfc3339(&rest[..end])
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

/// Last path component of a working directory — the project name, as opposed
/// to Claude's dash-encoded directory names that [`clean_project`] handles.
fn project_from_cwd(cwd: &str) -> String {
    let name = Path::new(cwd)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    if name.is_empty() {
        EM_DASH.to_string()
    } else {
        name.chars().take(28).collect()
    }
}

/// `7.5` → `"7.5"`, `7.0` → `"7"`. Premium-request counts are fractional but
/// usually whole, and a trailing `.0` reads like noise.
fn trim_float(v: f64) -> String {
    if (v.fract()).abs() < f64::EPSILON {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

/// Scan z.ai MCP logs. Returns the aggregate `Glm` and the most recent event
/// timestamp (parsed from the leading `[...]` bracket), so callers can merge a
/// GLM summary row into the sessions list by recency.
fn scan_glm(zai_root: &Path) -> (Glm, Option<DateTime<Utc>>) {
    let note = "Local Z.ai MCP logs record server lifecycle only — token/cost not exposed locally."
        .to_string();
    let pattern = format!("{}/zai-mcp-*.log", zai_root.to_string_lossy());
    let mut sessions = 0u32;
    let mut active_days: Vec<String> = Vec::new();
    let mut last = String::new();
    let mut latest_dt: Option<DateTime<Utc>> = None;

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
                    // Best-effort parse of the bracket timestamp; keep the newest.
                    if let Some(dt) = parse_glm_ts(&line[1..idx]) {
                        latest_dt = Some(match latest_dt {
                            Some(d) if d > dt => d,
                            _ => dt,
                        });
                    }
                }
            }
        }
        if day_has && !active_days.contains(&date) {
            active_days.push(date);
        }
    }

    let glm = Glm {
        sessions,
        active_days: active_days.len(),
        last: if last.is_empty() {
            "—".to_string()
        } else {
            last.chars().take(10).collect()
        },
        note,
    };
    (glm, latest_dt)
}

/// Parse a z.ai log bracket timestamp. The MCP server writes RFC 3339 instants
/// (e.g. `2026-01-24T06:02:14.871Z`), the same form Claude records use; the
/// trailing `Z`/offset means they're genuine UTC and compare directly against
/// the Claude record timestamps. A couple of offset-less forms are accepted as
/// best-effort fallbacks, else `None`.
///
/// `parse_from_str` must consume the *entire* string, so each accepted shape has
/// to be tried explicitly — a shorter format silently fails on a longer input.
fn parse_glm_ts(raw: &str) -> Option<DateTime<Utc>> {
    use chrono::{NaiveDate, NaiveDateTime};
    let s = raw.trim();
    // Real format: RFC 3339 with a `Z`/offset.
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Utc));
    }
    // Best-effort fallbacks for offset-less variants (treated as UTC).
    if let Ok(dt) = NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S") {
        return Some(DateTime::<Utc>::from_naive_utc_and_offset(dt, Utc));
    }
    let date_only = NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()?;
    let dt = date_only.and_hms_opt(0, 0, 0)?;
    Some(DateTime::<Utc>::from_naive_utc_and_offset(dt, Utc))
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
        // one Claude session row (no longer padded to a fixed length)
        assert_eq!(snap.sessions.len(), 1);
        assert_eq!(snap.sessions[0].provider, "claude");
        assert_eq!(snap.sessions[0].model, "opus");
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
        assert_eq!(snap.sessions.len(), 0);
        assert_eq!(snap.week.len(), 7);
    }

    #[test]
    fn glm_row_orders_by_parsed_event_time() {
        let tmp = tempfile::tempdir().unwrap();
        let claude = tmp.path().join("claude");
        let zai = tmp.path().join("zai");
        std::fs::create_dir_all(&zai).unwrap();
        let now = DateTime::parse_from_rfc3339("2026-06-17T20:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        // A Claude session at 15:00 — the SAME day as the GLM event but LATER.
        let claude_line =
            r#"{"timestamp":"2026-06-17T15:00:00.000Z","sessionId":"latest12","message":{"model":"claude-sonnet-4-5","usage":{"input_tokens":100,"output_tokens":200,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#;
        write_jsonl(&claude, "proj-a", &[claude_line]);

        // GLM server-start earlier the same day (09:00). The bracket carries a
        // real RFC 3339 instant (matching actual z.ai logs), which must be
        // parsed for ordering — not silently dropped to a `now` fallback (which
        // would wrongly pin GLM to the top).
        std::fs::write(
            zai.join("zai-mcp-2026-06-17.log"),
            "[2026-06-17T09:00:00.000Z] INFO: MCP Server started successfully\n",
        )
        .unwrap();

        let snap = scan(&claude, &zai, "max5x", now);
        // Claude row + one GLM summary row.
        assert_eq!(snap.sessions.len(), 2);
        // Claude (15:00) is newer than the GLM event (09:00), so it sorts first.
        // This ordering only holds if parse_glm_ts read the GLM event time
        // instead of falling back to `now` — i.e. it is the discriminating
        // assertion that the timestamp parse actually works.
        assert_eq!(snap.sessions[0].provider, "claude");
        assert_eq!(snap.sessions[1].provider, "glm");
        assert_eq!(snap.sessions[1].project, "Z.ai");
        assert_eq!(snap.sessions[1].model, ""); // no model badge for GLM
    }

    #[test]
    fn parse_glm_ts_parses_rfc3339_and_fallbacks() {
        // Real MCP log format: RFC 3339 with milliseconds and a `Z`.
        let real = parse_glm_ts("2026-01-24T06:02:14.871Z").expect("rfc3339 should parse");
        assert_eq!(real.format("%Y-%m-%d %H:%M:%S").to_string(), "2026-01-24 06:02:14");
        // A non-UTC offset is normalized to UTC.
        let offset = parse_glm_ts("2026-01-24T06:02:14+02:00").expect("offset should parse");
        assert_eq!(offset.format("%Y-%m-%d %H:%M:%S").to_string(), "2026-01-24 04:02:14");
        // Offset-less fallbacks still work.
        let full = parse_glm_ts("2026-06-17 09:00:00").expect("space form should parse");
        assert_eq!(full.format("%Y-%m-%d %H:%M:%S").to_string(), "2026-06-17 09:00:00");
        let date_only = parse_glm_ts("2026-06-17").expect("date-only should parse");
        assert_eq!(date_only.format("%Y-%m-%d %H:%M:%S").to_string(), "2026-06-17 00:00:00");
        // Garbage returns None rather than panicking.
        assert!(parse_glm_ts("not a timestamp").is_none());
    }

    #[test]
    fn token_formatting() {
        assert_eq!(fmt_tokens(500.0), "500");
        assert_eq!(fmt_tokens(1_500.0), "2K");
        assert_eq!(fmt_tokens(1_500_000.0), "1.5M");
        assert_eq!(fmt_tokens(2_500_000_000.0), "2.50B");
    }

    // ── family_of: model name → pricing family ──

    #[test]
    fn family_of_maps_model_names() {
        assert_eq!(family_of("claude-opus-4-7"), "opus");
        assert_eq!(family_of("claude-opus-4-1-20250805"), "opus");
        assert_eq!(family_of("claude-haiku-4-5"), "haiku");
        assert_eq!(family_of("claude-sonnet-4-5"), "sonnet");
        assert_eq!(family_of("claude-sonnet-4-5-20250929"), "sonnet");
        // Unknown model defaults to sonnet.
        assert_eq!(family_of("gpt-4o"), "sonnet");
        assert_eq!(family_of(""), "sonnet");
    }

    // ── price: per-family token rates ──

    #[test]
    fn price_returns_expected_rates() {
        let opus = price("opus");
        assert_eq!(opus.input, 15.0);
        assert_eq!(opus.output, 75.0);
        assert_eq!(opus.cache_write, 18.75);
        assert_eq!(opus.cache_read, 1.50);

        let sonnet = price("sonnet");
        assert_eq!(sonnet.input, 3.0);
        assert_eq!(sonnet.output, 15.0);

        let haiku = price("haiku");
        assert_eq!(haiku.input, 0.80);
        assert_eq!(haiku.output, 4.0);

        // Fallback to sonnet pricing for unknown families.
        let unknown = price("unknown");
        assert_eq!(unknown.input, sonnet.input);
        assert_eq!(unknown.output, sonnet.output);
    }

    // ── ceilings: plan tier token limits ──

    #[test]
    fn ceilings_per_plan_tier() {
        let (s, w, wo) = ceilings("pro");
        assert_eq!(s, 30_000_000);
        assert_eq!(w, 200_000_000);
        assert_eq!(wo, 0);

        let (s, w, wo) = ceilings("max5x");
        assert_eq!(s, 150_000_000);
        assert_eq!(w, 1_000_000_000);
        assert_eq!(wo, 250_000_000);

        let (s, w, wo) = ceilings("max20x");
        assert_eq!(s, 600_000_000);
        assert_eq!(w, 4_000_000_000);
        assert_eq!(wo, 1_000_000_000);

        // Custom and unknown strings fall back to max5x.
        let (s, _, _) = ceilings("custom");
        assert_eq!(s, 150_000_000);
        let (s, _, _) = ceilings("garbage");
        assert_eq!(s, 150_000_000);
    }

    #[test]
    fn plan_label_maps_tier_names() {
        assert_eq!(plan_label("pro"), "Pro");
        assert_eq!(plan_label("max20x"), "Max 20×");
        assert_eq!(plan_label("custom"), "Custom");
        assert_eq!(plan_label("max5x"), "Max 5×");
        assert_eq!(plan_label("unknown"), "Max 5×");
    }

    // ── fmt_cost ──

    #[test]
    fn cost_formatting() {
        assert_eq!(fmt_cost(0.0), "$0.00");
        assert_eq!(fmt_cost(1.5), "$1.50");
        assert_eq!(fmt_cost(123.456), "$123.46");
    }

    // ── countdown: reset time remaining ──

    #[test]
    fn countdown_no_reset_is_ready() {
        let now = Utc::now();
        assert_eq!(countdown(None, now), "ready");
    }

    #[test]
    fn countdown_past_reset_is_resetting() {
        let now = Utc::now();
        let past = now - Duration::hours(1);
        assert_eq!(countdown(Some(past), now), "resetting");
    }

    #[test]
    fn countdown_formats_minutes_hours_and_days() {
        let now = DateTime::parse_from_rfc3339("2026-06-17T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        // 30 minutes ahead
        let r = now + Duration::minutes(30);
        assert_eq!(countdown(Some(r), now), "30m");

        // 4h 15m ahead
        let r = now + Duration::hours(4) + Duration::minutes(15);
        assert_eq!(countdown(Some(r), now), "4h 15m");

        // 2d 3h ahead
        let r = now + Duration::days(2) + Duration::hours(3);
        assert_eq!(countdown(Some(r), now), "2d 3h");
    }

    // ── humanize_when ──

    #[test]
    fn humanize_when_minutes_ago() {
        let now = DateTime::parse_from_rfc3339("2026-06-17T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let ts = now - Duration::minutes(45);
        assert_eq!(humanize_when(ts, now), "45m ago");
    }

    #[test]
    fn humanize_when_hours_ago() {
        let now = DateTime::parse_from_rfc3339("2026-06-17T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let ts = now - Duration::hours(3);
        assert_eq!(humanize_when(ts, now), "3h ago");
    }

    #[test]
    fn humanize_when_yesterday() {
        let now = DateTime::parse_from_rfc3339("2026-06-17T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let ts = now - Duration::days(1);
        assert_eq!(humanize_when(ts, now), "yesterday");
    }

    #[test]
    fn humanize_when_days_ago() {
        let now = DateTime::parse_from_rfc3339("2026-06-17T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let ts = now - Duration::days(5);
        assert_eq!(humanize_when(ts, now), "5d ago");
    }

    // ── clean_project ──

    #[test]
    fn clean_project_strips_known_prefixes() {
        assert_eq!(
            clean_project("-Volumes-CrucialX10-projects-myproj"),
            "myproj"
        );
        assert_eq!(
            clean_project("-Users-dennisrongo-myproj"),
            "myproj"
        );
        assert_eq!(
            clean_project("-Volumes-CrucialX10-myproj"),
            "myproj"
        );
    }

    #[test]
    fn clean_project_truncates_long_names() {
        let long = "x".repeat(50);
        let cleaned = clean_project(&long);
        assert_eq!(cleaned.len(), 28);
    }

    #[test]
    fn clean_project_empty_returns_em_dash() {
        assert_eq!(clean_project(""), "—");
        assert_eq!(clean_project("---"), "—");
    }

    // ── status_for: threshold classification ──

    #[test]
    fn status_for_thresholds() {
        assert_eq!(status_for(0.0), ("ok", "Healthy"));
        assert_eq!(status_for(69.9), ("ok", "Healthy"));
        assert_eq!(status_for(70.0), ("warn", "Watch"));
        assert_eq!(status_for(89.9), ("warn", "Watch"));
        assert_eq!(status_for(90.0), ("danger", "Near limit"));
        assert_eq!(status_for(100.0), ("danger", "Near limit"));
    }

    // ── weekday_abbr ──

    #[test]
    fn weekday_abbr_all_days() {
        assert_eq!(weekday_abbr(0), "Mon");
        assert_eq!(weekday_abbr(1), "Tue");
        assert_eq!(weekday_abbr(2), "Wed");
        assert_eq!(weekday_abbr(3), "Thu");
        assert_eq!(weekday_abbr(4), "Fri");
        assert_eq!(weekday_abbr(5), "Sat");
        assert_eq!(weekday_abbr(6), "Sun");
    }

    // ── Multi-model scan (opus + sonnet + haiku) ──

    #[test]
    fn multi_model_aggregates_per_family() {
        let tmp = tempfile::tempdir().unwrap();
        let claude = tmp.path().join("claude");
        let zai = tmp.path().join("zai");
        std::fs::create_dir_all(&zai).unwrap();
        let now = DateTime::parse_from_rfc3339("2026-06-17T20:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let recent = "2026-06-17T19:00:00.000Z";

        let mk = |model: &str, tin, tout| {
            format!(
                r#"{{"timestamp":"{recent}","sessionId":"s-{model}","message":{{"model":"{model}","usage":{{"input_tokens":{tin},"output_tokens":{tout},"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}}}}"#
            )
        };
        write_jsonl(&claude, "multi", &[
            &mk("claude-opus-4-7", 100, 200),
            &mk("claude-sonnet-4-5", 1000, 2000),
            &mk("claude-haiku-4-5", 500, 100),
        ]);

        let snap = scan(&claude, &zai, "max5x", now);
        // All three model rows have non-zero tokens (the families actually used).
        let opus = snap.models.iter().find(|m| m.key == "opus").unwrap();
        assert_eq!(opus.tokens, "300");
        let sonnet = snap.models.iter().find(|m| m.key == "sonnet").unwrap();
        assert_eq!(sonnet.tokens, "3K");
        let haiku = snap.models.iter().find(|m| m.key == "haiku").unwrap();
        assert_eq!(haiku.tokens, "600");

        // Three distinct sessions → three session rows.
        assert_eq!(snap.sessions.len(), 3);
        // Total tokens = 300 + 3000 + 600 = 3900 → fmt_tokens rounds to 4K.
        assert_eq!(snap.kpi.session_tokens, "4K");
        // Cost: opus = (100*15 + 200*75)/1e6 = 0.0165
        //       sonnet = (1000*3 + 2000*15)/1e6 = 0.033
        //       haiku = (500*0.8 + 100*4)/1e6 = 0.0008
        // Total ≈ $0.05
        assert_eq!(snap.kpi.session_cost, "$0.05");
    }

    // ── Session cap: MAX_PROVIDER_ROWS = 25 ──

    #[test]
    fn session_rows_capped_at_max() {
        let tmp = tempfile::tempdir().unwrap();
        let claude = tmp.path().join("claude");
        let zai = tmp.path().join("zai");
        std::fs::create_dir_all(&zai).unwrap();
        let now = DateTime::parse_from_rfc3339("2026-06-17T20:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        // Write 30 sessions, each in its own project dir so they don't merge.
        for i in 0..30 {
            let ts = format!("2026-06-17T{:02}:00:00.000Z", i % 24);
            let line = format!(
                r#"{{"timestamp":"{ts}","sessionId":"sess{i:02}","message":{{"model":"claude-sonnet-4-5","usage":{{"input_tokens":100,"output_tokens":200,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}}}}"#
            );
            write_jsonl(&claude, &format!("proj-{i:02}"), &[&line]);
        }

        let snap = scan(&claude, &zai, "max5x", now);
        // MAX_PROVIDER_ROWS = 25 → 25 Claude rows (no GLM row since zai is empty).
        assert_eq!(snap.sessions.len(), 25);
        // Session IDs are "sess00".."sess29" → 6 chars after take(8) truncation.
        assert_eq!(snap.sessions[0].id.len(), 6);
    }

    // ── KPI calculations across the session/week/total windows ──

    #[test]
    fn kpi_session_and_week_and_total() {
        let tmp = tempfile::tempdir().unwrap();
        let claude = tmp.path().join("claude");
        let zai = tmp.path().join("zai");
        std::fs::create_dir_all(&zai).unwrap();
        let now = DateTime::parse_from_rfc3339("2026-06-17T20:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        // A session entry from 2 hours ago (within 5h window and 7d window).
        let recent_ts = "2026-06-17T18:00:00.000Z";
        // An entry from 4 days ago (outside 5h, inside 7d).
        let old_ts = "2026-06-13T12:00:00.000Z";
        // An entry from 10 days ago (outside 7d, counted only in total).
        let ancient_ts = "2026-06-07T12:00:00.000Z";

        let mk = |ts: &str, tin, tout| {
            format!(
                r#"{{"timestamp":"{ts}","sessionId":"s1","message":{{"model":"claude-sonnet-4-5","usage":{{"input_tokens":{tin},"output_tokens":{tout},"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}}}}"#
            )
        };
        write_jsonl(&claude, "proj-a", &[
            &mk(recent_ts, 100, 200),     // 300 tokens
            &mk(old_ts, 1000, 2000),     // 3000 tokens
            &mk(ancient_ts, 10000, 20000), // 30000 tokens
        ]);

        let snap = scan(&claude, &zai, "max5x", now);

        // Session (5h): only the recent entry → 300 tokens.
        assert_eq!(snap.kpi.session_tokens, "300");
        // Week (7d): recent + old → 300 + 3000 = 3300 → fmt_tokens rounds to 3K.
        assert_eq!(snap.kpi.week_tokens, "3K");
        // Total: 300 + 3000 + 30000 = 33300 → fmt_tokens rounds to 33K.
        assert_eq!(snap.kpi.total_tokens, "33K");
    }

    // ── Week chart: 7 days always present, today included ──

    #[test]
    fn week_chart_has_seven_days_ending_today() {
        let tmp = tempfile::tempdir().unwrap();
        let now = DateTime::parse_from_rfc3339("2026-06-17T20:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let snap = scan(
            &tmp.path().join("none"),
            &tmp.path().join("nozai"),
            "pro",
            now,
        );
        assert_eq!(snap.week.len(), 7);
        // Last entry is today.
        let today_key = now.format("%Y-%m-%d").to_string();
        assert_eq!(snap.week.last().unwrap().date, today_key);
        // Bar percentages are 0 when there's no data (all days equal 0, max = 1).
        for w in &snap.week {
            assert_eq!(w.bar_pct, 0);
        }
    }

    // ── Plan tier changes affect bucket percentages ──

    #[test]
    fn plan_tier_changes_bucket_usage_percentage() {
        let tmp = tempfile::tempdir().unwrap();
        let claude = tmp.path().join("claude");
        let zai = tmp.path().join("zai");
        std::fs::create_dir_all(&zai).unwrap();
        let now = DateTime::parse_from_rfc3339("2026-06-17T20:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let recent = "2026-06-17T19:00:00.000Z";

        // 30M tokens — hits 100% on Pro (30M ceiling), 20% on Max 5× (150M).
        let line = format!(
            r#"{{"timestamp":"{recent}","sessionId":"s1","message":{{"model":"claude-sonnet-4-5","usage":{{"input_tokens":30000000,"output_tokens":0,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}}}}"#
        );
        write_jsonl(&claude, "proj-a", &[&line]);

        let pro_snap = scan(&claude, &zai, "pro", now);
        let pro_pct = pro_snap.limits.buckets[0].used_pct;
        assert!((pro_pct - 100.0).abs() < 0.1, "pro should be ~100%, got {pro_pct}");

        let max5x_snap = scan(&claude, &zai, "max5x", now);
        let max5x_pct = max5x_snap.limits.buckets[0].used_pct;
        assert!((max5x_pct - 20.0).abs() < 0.1, "max5x should be ~20%, got {max5x_pct}");
    }

    // ── Cache tokens are counted in the total ──

    #[test]
    fn cache_tokens_counted_in_total() {
        let tmp = tempfile::tempdir().unwrap();
        let claude = tmp.path().join("claude");
        let zai = tmp.path().join("zai");
        std::fs::create_dir_all(&zai).unwrap();
        let now = DateTime::parse_from_rfc3339("2026-06-17T20:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let recent = "2026-06-17T19:00:00.000Z";

        // input=100, output=200, cache_write=300, cache_read=400 → 1000 total.
        let line = format!(
            r#"{{"timestamp":"{recent}","sessionId":"s1","message":{{"model":"claude-sonnet-4-5","usage":{{"input_tokens":100,"output_tokens":200,"cache_creation_input_tokens":300,"cache_read_input_tokens":400}}}}}}"#
        );
        write_jsonl(&claude, "proj-a", &[&line]);

        let snap = scan(&claude, &zai, "max5x", now);
        assert_eq!(snap.kpi.session_tokens, "1K");
    }

    // ── GLM scan: counts server-start events and active days ──

    #[test]
    fn glm_scan_counts_sessions_and_active_days() {
        let tmp = tempfile::tempdir().unwrap();
        let claude = tmp.path().join("claude");
        let zai = tmp.path().join("zai");
        std::fs::create_dir_all(&claude).unwrap();
        std::fs::create_dir_all(&zai).unwrap();

        std::fs::write(
            zai.join("zai-mcp-2026-06-15.log"),
            "[2026-06-15T08:00:00.000Z] INFO: MCP Server started successfully\n",
        ).unwrap();
        std::fs::write(
            zai.join("zai-mcp-2026-06-16.log"),
            "[2026-06-16T10:00:00.000Z] INFO: MCP Server started successfully\n[2026-06-16T14:00:00.000Z] INFO: MCP Server started successfully\n",
        ).unwrap();

        let now = DateTime::parse_from_rfc3339("2026-06-17T20:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let snap = scan(&claude, &zai, "max5x", now);
        assert_eq!(snap.glm.sessions, 3); // 1 + 2 start events
        assert_eq!(snap.glm.active_days, 2);
    }

    // ── Lines without "usage" are skipped ──

    #[test]
    fn skips_lines_without_usage_key() {
        let tmp = tempfile::tempdir().unwrap();
        let claude = tmp.path().join("claude");
        let zai = tmp.path().join("zai");
        std::fs::create_dir_all(&zai).unwrap();
        let now = DateTime::parse_from_rfc3339("2026-06-17T20:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let recent = "2026-06-17T19:00:00.000Z";

        // A line with no "usage" field, plus a real usage line.
        let no_usage = r#"{"timestamp":"recent","sessionId":"s1","message":{"model":"claude-sonnet-4-5"}}"#;
        let with_usage = format!(
            r#"{{"timestamp":"{recent}","sessionId":"s1","message":{{"model":"claude-sonnet-4-5","usage":{{"input_tokens":100,"output_tokens":200,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}}}}"#
        );
        write_jsonl(&claude, "proj-a", &[no_usage, &with_usage]);

        let snap = scan(&claude, &zai, "max5x", now);
        // Only the usage-bearing line counted → 300 tokens.
        assert_eq!(snap.kpi.session_tokens, "300");
    }

    // ── Kimi Code session store ──

    /// Write a Kimi session: `state.json` plus one wire log per agent.
    fn write_kimi_session(
        kimi: &Path,
        workdir: &str,
        session: &str,
        state: &str,
        agents: &[(&str, &[&str])],
    ) {
        let dir = kimi.join("sessions").join(workdir).join(session);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("state.json"), state).unwrap();
        for (agent, lines) in agents {
            let adir = dir.join("agents").join(agent);
            std::fs::create_dir_all(&adir).unwrap();
            let mut f = std::fs::File::create(adir.join("wire.jsonl")).unwrap();
            for l in *lines {
                writeln!(f, "{l}").unwrap();
            }
        }
    }

    fn kimi_usage_line(model: &str, other: u64, out: u64, cache: u64, time_ms: i64) -> String {
        format!(
            r#"{{"type":"usage.record","model":"{model}","usage":{{"inputOther":{other},"output":{out},"inputCacheRead":{cache},"inputCacheCreation":0}},"usageScope":"turn","time":{time_ms}}}"#
        )
    }

    fn roots_with(kimi: &Path, copilot: &Path) -> ScanRoots {
        ScanRoots {
            claude: kimi.join("__no_claude__"),
            zai: kimi.join("__no_zai__"),
            kimi: kimi.to_path_buf(),
            copilot: copilot.to_path_buf(),
            grok: kimi.join("__no_grok__"),
        }
    }

    #[test]
    fn kimi_session_sums_usage_across_agents() {
        let tmp = tempfile::tempdir().unwrap();
        let kimi = tmp.path().join("kimi");
        let now = DateTime::parse_from_rfc3339("2026-06-17T20:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        // 2026-06-17T19:00:00Z and 19:30:00Z.
        let t1 = 1_781_722_800_000i64;
        let t2 = 1_781_724_600_000i64;

        write_kimi_session(
            &kimi,
            "wd_myproj_abc123",
            "session_11112222-3333-4444-5555-666677778888",
            r#"{"id":"session_11112222-3333-4444-5555-666677778888","cwd":"/Volumes/dev/myproj","createdAt":1781722000000,"updatedAt":1781724600000,"archived":false}"#,
            &[
                ("main", &[&kimi_usage_line("kimi-code/k3", 1000, 200, 800, t1)]),
                // A subagent's usage lives only in its own wire log.
                ("agent-0", &[&kimi_usage_line("kimi-code/k3", 500, 100, 400, t2)]),
            ],
        );

        let snap = scan_roots(&roots_with(&kimi, &tmp.path().join("nocopilot")), "max5x", now);
        assert_eq!(snap.sessions.len(), 1);
        let row = &snap.sessions[0];
        assert_eq!(row.provider, "kimi");
        assert_eq!(row.project, "myproj");
        // main (1000+200+800) + agent-0 (500+100+400) = 3000.
        assert_eq!(row.tokens, 3000);
        assert_eq!(row.tokens_text, "3K");
        // Namespaced model name is reduced to the badge-friendly bare name.
        assert_eq!(row.model, "k3");
        // Flat-rate plan — no invented dollar figure.
        assert_eq!(row.cost_text, "—");
        // Ordered by the newest usage record, not `updatedAt`.
        assert_eq!(row.when, "30m ago");
        assert_eq!(row.id, "11112222");
    }

    #[test]
    fn kimi_skips_archived_sessions() {
        let tmp = tempfile::tempdir().unwrap();
        let kimi = tmp.path().join("kimi");
        let now = DateTime::parse_from_rfc3339("2026-06-17T20:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        write_kimi_session(
            &kimi,
            "wd_gone_abc123",
            "session_deadbeef-0000-0000-0000-000000000000",
            r#"{"id":"session_deadbeef-0000-0000-0000-000000000000","cwd":"/tmp/gone","updatedAt":1781724600000,"archived":true}"#,
            &[("main", &[&kimi_usage_line("kimi-code/k3", 10, 10, 0, 1781724600000)])],
        );

        let snap = scan_roots(&roots_with(&kimi, &tmp.path().join("nocopilot")), "max5x", now);
        assert!(snap.sessions.is_empty(), "archived sessions must not surface");
    }

    #[test]
    fn kimi_session_without_usage_records_reports_em_dash() {
        let tmp = tempfile::tempdir().unwrap();
        let kimi = tmp.path().join("kimi");
        let now = DateTime::parse_from_rfc3339("2026-06-17T20:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        // A freshly-started session: state written, no LLM turn yet.
        write_kimi_session(
            &kimi,
            "wd_fresh_abc123",
            "session_aaaabbbb-0000-0000-0000-000000000000",
            r#"{"id":"session_aaaabbbb-0000-0000-0000-000000000000","cwd":"/tmp/fresh","updatedAt":1781724600000}"#,
            &[("main", &[r#"{"type":"metadata","protocol_version":"1.5"}"#])],
        );

        let snap = scan_roots(&roots_with(&kimi, &tmp.path().join("nocopilot")), "max5x", now);
        assert_eq!(snap.sessions.len(), 1);
        assert_eq!(snap.sessions[0].tokens_text, "—");
        // Falls back to `updatedAt` for ordering.
        assert_eq!(snap.sessions[0].when, "30m ago");
    }

    // ── Copilot CLI session state ──

    fn write_copilot_session(copilot: &Path, id: &str, lines: &[&str]) {
        let dir = copilot.join("session-state").join(id);
        std::fs::create_dir_all(&dir).unwrap();
        let mut f = std::fs::File::create(dir.join("events.jsonl")).unwrap();
        for l in lines {
            writeln!(f, "{l}").unwrap();
        }
    }

    #[test]
    fn copilot_session_uses_highest_cumulative_shutdown() {
        let tmp = tempfile::tempdir().unwrap();
        let copilot = tmp.path().join("copilot");
        let now = DateTime::parse_from_rfc3339("2026-06-17T20:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        write_copilot_session(
            &copilot,
            "64a3ccbe-57bf-481b-b3d1-d40a010bc927",
            &[
                r#"{"type":"session.start","data":{"selectedModel":"claude-opus-4.7","context":{"cwd":"/Volumes/dev/agent-status"}},"timestamp":"2026-06-17T18:00:00.000Z"}"#,
                // First run's snapshot…
                r#"{"type":"session.shutdown","data":{"totalPremiumRequests":3,"modelMetrics":{"claude-opus-4.7":{"usage":{"inputTokens":1000,"outputTokens":100}}}},"timestamp":"2026-06-17T18:30:00.000Z"}"#,
                r#"{"type":"session.resume","data":{"selectedModel":"claude-opus-4.7","context":{"cwd":"/Volumes/dev/agent-status"}},"timestamp":"2026-06-17T18:40:00.000Z"}"#,
                // …superseded by the second, which is cumulative, not additive.
                r#"{"type":"session.shutdown","data":{"totalPremiumRequests":7.5,"modelMetrics":{"claude-opus-4.7":{"usage":{"inputTokens":1500,"outputTokens":300}}}},"timestamp":"2026-06-17T19:00:00.000Z"}"#,
            ],
        );

        let snap = scan_roots(&roots_with(&tmp.path().join("nokimi"), &copilot), "max5x", now);
        assert_eq!(snap.sessions.len(), 1);
        let row = &snap.sessions[0];
        assert_eq!(row.provider, "copilot");
        assert_eq!(row.project, "agent-status");
        // Highest shutdown only: 1500 + 300. `inputTokens` already folds in cache.
        assert_eq!(row.tokens, 1800);
        // The badge reuses the Claude family styling.
        assert_eq!(row.model, "opus");
        assert_eq!(row.cost_text, "7.5 premium");
        assert_eq!(row.when, "1h ago");
    }

    #[test]
    fn copilot_ignores_a_shutdown_whose_counters_reset() {
        let tmp = tempfile::tempdir().unwrap();
        let copilot = tmp.path().join("copilot");
        let now = DateTime::parse_from_rfc3339("2026-06-17T20:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        // An older build: the session is reopened and closed without doing any
        // work, and its final snapshot reports zero rather than the real total.
        write_copilot_session(
            &copilot,
            "dddddddd-bbbb-cccc-dddd-eeeeeeeeeeee",
            &[
                r#"{"type":"session.start","data":{"selectedModel":"claude-sonnet-4.6","context":{"cwd":"/Volumes/dev/reset"}},"timestamp":"2026-06-17T17:00:00.000Z"}"#,
                r#"{"type":"session.shutdown","data":{"totalPremiumRequests":1,"tokenDetails":{"input":{"tokenCount":900},"output":{"tokenCount":100}}},"timestamp":"2026-06-17T18:00:00.000Z"}"#,
                r#"{"type":"session.shutdown","data":{"totalPremiumRequests":0,"tokenDetails":{"input":{"tokenCount":0},"output":{"tokenCount":0}}},"timestamp":"2026-06-17T19:00:00.000Z"}"#,
            ],
        );

        let snap = scan_roots(&roots_with(&tmp.path().join("nokimi"), &copilot), "max5x", now);
        assert_eq!(snap.sessions[0].tokens, 1000);
        assert_eq!(snap.sessions[0].cost_text, "1 premium");
    }

    #[test]
    fn copilot_running_session_reports_em_dash_tokens() {
        let tmp = tempfile::tempdir().unwrap();
        let copilot = tmp.path().join("copilot");
        let now = DateTime::parse_from_rfc3339("2026-06-17T20:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        // No shutdown event yet — tokens are genuinely unknown, not zero.
        write_copilot_session(
            &copilot,
            "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
            &[
                r#"{"type":"session.start","data":{"selectedModel":"claude-sonnet-4.6","context":{"cwd":"/Volumes/dev/live"}},"timestamp":"2026-06-17T19:00:00.000Z"}"#,
                r#"{"type":"assistant.turn_start","data":{"turnId":"0"},"timestamp":"2026-06-17T19:30:00.000Z"}"#,
            ],
        );

        let snap = scan_roots(&roots_with(&tmp.path().join("nokimi"), &copilot), "max5x", now);
        assert_eq!(snap.sessions.len(), 1);
        assert_eq!(snap.sessions[0].tokens_text, "—");
        assert_eq!(snap.sessions[0].cost_text, "—");
        assert_eq!(snap.sessions[0].model, "sonnet");
        // Ordered by the newest event, not the start.
        assert_eq!(snap.sessions[0].when, "30m ago");
    }

    #[test]
    fn copilot_falls_back_to_token_details() {
        let tmp = tempfile::tempdir().unwrap();
        let copilot = tmp.path().join("copilot");
        let now = DateTime::parse_from_rfc3339("2026-06-17T20:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        // An older CLI build: `tokenDetails` present, `modelMetrics.usage` absent.
        write_copilot_session(
            &copilot,
            "cccccccc-bbbb-cccc-dddd-eeeeeeeeeeee",
            &[
                r#"{"type":"session.start","data":{"selectedModel":"claude-sonnet-4.6","context":{"cwd":"/Volumes/dev/old"}},"timestamp":"2026-06-17T19:00:00.000Z"}"#,
                r#"{"type":"session.shutdown","data":{"modelMetrics":{"claude-sonnet-4.6":{"requests":{"count":1}}},"tokenDetails":{"input":{"tokenCount":100},"cache_read":{"tokenCount":50},"output":{"tokenCount":25}}},"timestamp":"2026-06-17T19:30:00.000Z"}"#,
            ],
        );

        let snap = scan_roots(&roots_with(&tmp.path().join("nokimi"), &copilot), "max5x", now);
        assert_eq!(snap.sessions[0].tokens, 175);
        // No premium-request figure in this build.
        assert_eq!(snap.sessions[0].cost_text, "—");
    }

    // ── Cross-provider ordering ──

    #[test]
    fn rows_from_every_provider_interleave_by_recency() {
        let tmp = tempfile::tempdir().unwrap();
        let claude = tmp.path().join("claude");
        let zai = tmp.path().join("zai");
        let kimi = tmp.path().join("kimi");
        let copilot = tmp.path().join("copilot");
        std::fs::create_dir_all(&zai).unwrap();
        let now = DateTime::parse_from_rfc3339("2026-06-17T20:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        // Claude at 19:00, Kimi at 18:00, Copilot at 17:00.
        write_jsonl(
            &claude,
            "proj-a",
            &[r#"{"timestamp":"2026-06-17T19:00:00.000Z","sessionId":"claude01","message":{"model":"claude-sonnet-4-5","usage":{"input_tokens":100,"output_tokens":200,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#],
        );
        write_kimi_session(
            &kimi,
            "wd_kproj_abc123",
            "session_kkkkkkkk-0000-0000-0000-000000000000",
            r#"{"id":"session_kkkkkkkk-0000-0000-0000-000000000000","cwd":"/tmp/kproj","updatedAt":1781719200000}"#,
            &[("main", &[&kimi_usage_line("kimi-code/k3", 100, 50, 0, 1_781_719_200_000)])],
        );
        write_copilot_session(
            &copilot,
            "cccccccc-0000-0000-0000-000000000000",
            &[r#"{"type":"session.start","data":{"selectedModel":"claude-opus-4.7","context":{"cwd":"/tmp/cproj"}},"timestamp":"2026-06-17T17:00:00.000Z"}"#],
        );

        let snap = scan_roots(&ScanRoots { claude, zai, kimi, copilot, grok: tmp.path().join("nogrok") }, "max5x", now);
        let order: Vec<&str> = snap.sessions.iter().map(|s| s.provider.as_str()).collect();
        assert_eq!(order, vec!["claude", "kimi", "copilot"]);

        // Each contributing provider gets a Providers-tab row.
        let names: Vec<&str> = snap.providers.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"Moonshot"), "got {names:?}");
        assert!(names.contains(&"GitHub"), "got {names:?}");
    }

    #[test]
    fn missing_provider_roots_are_silently_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let now = DateTime::parse_from_rfc3339("2026-06-17T20:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let snap = scan_roots(
            &ScanRoots {
                claude: tmp.path().join("nope"),
                zai: tmp.path().join("nope"),
                kimi: tmp.path().join("nope"),
                copilot: tmp.path().join("nope"),
                grok: tmp.path().join("nope"),
            },
            "pro",
            now,
        );
        assert!(snap.sessions.is_empty());
        assert_eq!(snap.providers.len(), 2, "only the always-present rows");
    }

    // ── project_from_cwd / trim_float ──

    #[test]
    fn project_from_cwd_takes_last_component() {
        assert_eq!(project_from_cwd("/Volumes/dev/projects/myproj"), "myproj");
        assert_eq!(project_from_cwd(""), "—");
        assert_eq!(project_from_cwd(&format!("/tmp/{}", "x".repeat(50))).len(), 28);
    }

    #[test]
    fn extract_timestamp_prefers_the_trailing_top_level_key() {
        // `data` carries an unquoted epoch-millis timestamp; the event's own
        // RFC3339 one trails it.
        let line = r#"{"type":"hook.start","data":{"timestamp":1781974216372},"id":"x","timestamp":"2026-06-17T19:30:00.000Z","parentId":"y"}"#;
        let got = extract_timestamp(line).unwrap();
        assert_eq!(got.to_rfc3339(), "2026-06-17T19:30:00+00:00");
    }

    #[test]
    fn extract_timestamp_returns_none_without_one() {
        assert!(extract_timestamp(r#"{"type":"x","data":{}}"#).is_none());
        // Present but unparseable — not a panic, just no timestamp.
        assert!(extract_timestamp(r#"{"timestamp":"not-a-date"}"#).is_none());
    }

    #[test]
    fn trim_float_drops_trailing_zero() {
        assert_eq!(trim_float(7.0), "7");
        assert_eq!(trim_float(7.5), "7.5");
        assert_eq!(trim_float(0.0), "0");
    }

    // ── Grok Build session store ──

    fn write_grok_session(
        grok: &Path,
        cwd_enc: &str,
        id: &str,
        summary: &str,
        updates: &[&str],
    ) {
        let dir = grok.join("sessions").join(cwd_enc).join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("summary.json"), summary).unwrap();
        let mut f = std::fs::File::create(dir.join("updates.jsonl")).unwrap();
        for l in updates {
            writeln!(f, "{l}").unwrap();
        }
    }

    fn grok_usage_line(input: u64, output: u64, ts: i64) -> String {
        format!(
            r#"{{"timestamp":{ts},"method":"session/update","params":{{"update":{{"sessionUpdate":"usage_update","usage":{{"input_tokens":{input},"output_tokens":{output},"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}}}}}}"#
        )
    }

    fn grok_roots(grok: &Path) -> ScanRoots {
        ScanRoots {
            claude: grok.join("__no_claude__"),
            zai: grok.join("__no_zai__"),
            kimi: grok.join("__no_kimi__"),
            copilot: grok.join("__no_copilot__"),
            grok: grok.to_path_buf(),
        }
    }

    #[test]
    fn grok_session_sums_billed_usage() {
        let tmp = tempfile::tempdir().unwrap();
        let grok = tmp.path().join("grok");
        let now = DateTime::parse_from_rfc3339("2026-08-16T20:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        write_grok_session(
            &grok,
            "C%3A%5Cproj",
            "01a00bbe-460b-7cc0-b3c7-3f1326845eba",
            r#"{"info":{"id":"01a00bbe-460b-7cc0-b3c7-3f1326845eba","cwd":"C:\\Users\\denni\\Documents\\GitHub\\agent-status"},"current_model_id":"grok-4.6","updated_at":"2026-08-16T19:30:00Z","last_active_at":"2026-08-16T19:30:00Z"}"#,
            &[
                &grok_usage_line(1000, 200, 1_786_903_800),
                // Context-window totalTokens must not be counted as spend.
                r#"{"timestamp":1786903801,"method":"session/update","params":{"update":{"sessionUpdate":"agent_message_chunk"},"_meta":{"totalTokens":88009}}}"#,
                &grok_usage_line(500, 100, 1_786_904_000),
            ],
        );

        let snap = scan_roots(&grok_roots(&grok), "max5x", now);
        assert_eq!(snap.sessions.len(), 1);
        let row = &snap.sessions[0];
        assert_eq!(row.provider, "grok");
        assert_eq!(row.project, "agent-status");
        assert_eq!(row.model, "grok-4.6");
        assert_eq!(row.tokens, 1800);
        assert_eq!(row.tokens_text, "2K");
        assert_eq!(row.cost_text, "—");
        assert_eq!(row.id, "01a00bbe");
        assert!(snap.providers.iter().any(|p| p.name == "xAI"));
        let week = snap.grok_week.expect("local xAI week stats");
        assert_eq!(week.sessions, 1);
        assert_eq!(week.total_tokens, "2K");
        assert_eq!(week.week_tokens, "2K");
        assert_eq!(week.session_tokens, "2K");
        assert_eq!(week.days.len(), 7);
        assert!(week.models.iter().any(|m| m.name == "grok-4.6" && m.tokens == "2K"));
    }

    #[test]
    fn grok_session_without_billed_usage_reports_em_dash() {
        let tmp = tempfile::tempdir().unwrap();
        let grok = tmp.path().join("grok");
        let now = DateTime::parse_from_rfc3339("2026-08-16T20:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        write_grok_session(
            &grok,
            "home",
            "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
            r#"{"info":{"id":"aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee","cwd":"/tmp/fresh"},"current_model_id":"grok-4.6","updated_at":"2026-08-16T19:00:00Z"}"#,
            &[r#"{"timestamp":1786903200,"method":"session/update","params":{"update":{"sessionUpdate":"agent_thought_chunk"},"_meta":{"totalTokens":19786}}}"#],
        );
        let snap = scan_roots(&grok_roots(&grok), "max5x", now);
        assert_eq!(snap.sessions.len(), 1);
        assert_eq!(snap.sessions[0].tokens, 0);
        assert_eq!(snap.sessions[0].tokens_text, "—");
    }

    fn grok_turn_completed(input: u64, output: u64, ts: i64, model: &str) -> String {
        let mut model_usage = serde_json::Map::new();
        model_usage.insert(
            model.to_string(),
            serde_json::json!({ "inputTokens": input, "outputTokens": output }),
        );
        serde_json::json!({
            "timestamp": ts,
            "method": "_x.ai/session/update",
            "params": {
                "update": {
                    "sessionUpdate": "turn_completed",
                    "usage": {
                        "inputTokens": input,
                        "outputTokens": output,
                        "totalTokens": input + output,
                        "cachedReadTokens": input / 2,
                        "cacheCreationTokens": 0,
                        "modelUsage": serde_json::Value::Object(model_usage)
                    }
                }
            },
            "_meta": { "agentTimestampMs": ts * 1000 }
        })
        .to_string()
    }

    #[test]
    fn grok_turn_completed_camelcase_is_billed_spend() {
        let tmp = tempfile::tempdir().unwrap();
        let grok = tmp.path().join("grok");
        let now = DateTime::parse_from_rfc3339("2026-08-16T20:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let ts = (now - Duration::hours(1)).timestamp();
        write_grok_session(
            &grok,
            "proj",
            "bbbbbbbb-bbbb-cccc-dddd-eeeeeeeeeeee",
            r#"{"info":{"id":"bbbbbbbb-bbbb-cccc-dddd-eeeeeeeeeeee","cwd":"/tmp/p"},"current_model_id":"grok-4.6","last_active_at":"2026-08-16T19:00:00Z"}"#,
            &[&grok_turn_completed(1000, 200, ts, "grok-4.6-build")],
        );
        let snap = scan_roots(&grok_roots(&grok), "max5x", now);
        assert_eq!(snap.sessions[0].tokens, 1200);
        let week = snap.grok_week.expect("week");
        assert!(
            week.models.iter().any(|m| m.name == "grok-4.6-build" && m.tokens == "1K"),
            "got {:?}",
            week.models
        );
    }

    #[test]
    fn grok_week_cuts_5h_and_7d() {
        let tmp = tempfile::tempdir().unwrap();
        let grok = tmp.path().join("grok");
        let now = DateTime::parse_from_rfc3339("2026-08-16T20:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        write_grok_session(
            &grok,
            "proj",
            "cccccccc-bbbb-cccc-dddd-eeeeeeeeeeee",
            r#"{"info":{"id":"cccccccc-bbbb-cccc-dddd-eeeeeeeeeeee","cwd":"/tmp/p"},"current_model_id":"grok-4.6","last_active_at":"2026-08-16T19:00:00Z"}"#,
            &[
                &grok_usage_line(1000, 0, (now - Duration::hours(2)).timestamp()),
                &grok_usage_line(2000, 0, (now - Duration::hours(6)).timestamp()),
                &grok_usage_line(4000, 0, (now - Duration::days(8)).timestamp()),
            ],
        );
        let week = scan_roots(&grok_roots(&grok), "max5x", now)
            .grok_week
            .expect("week");
        assert_eq!(week.session_tokens, "1K");
        assert_eq!(week.week_tokens, "3K");
        assert_eq!(week.total_tokens, "7K");
        assert!(
            week.models.iter().any(|m| m.name == "grok-4.6" && m.tokens == "3K"),
            "7d model mix must exclude the 8-day-old turn, got {:?}",
            week.models
        );
    }

    #[test]
    fn grok_untimed_spend_uses_session_last_active() {
        let tmp = tempfile::tempdir().unwrap();
        let grok = tmp.path().join("grok");
        let now = DateTime::parse_from_rfc3339("2026-08-16T20:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        write_grok_session(
            &grok,
            "old",
            "dddddddd-bbbb-cccc-dddd-eeeeeeeeeeee",
            r#"{"info":{"id":"dddddddd-bbbb-cccc-dddd-eeeeeeeeeeee","cwd":"/tmp/old"},"current_model_id":"grok-4.6","last_active_at":"2026-08-08T20:00:00Z"}"#,
            &[r#"{"method":"session/update","params":{"update":{"sessionUpdate":"usage_update","usage":{"inputTokens":9000,"outputTokens":0}}}}"#],
        );
        let week = scan_roots(&grok_roots(&grok), "max5x", now)
            .grok_week
            .expect("week");
        assert_eq!(week.session_tokens, "—");
        assert_eq!(week.week_tokens, "—");
        assert_eq!(week.total_tokens, "9K");
    }

    #[test]
    fn grok_week_includes_sessions_past_the_row_cap() {
        let tmp = tempfile::tempdir().unwrap();
        let grok = tmp.path().join("grok");
        let now = DateTime::parse_from_rfc3339("2026-08-16T20:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let ts = (now - Duration::hours(1)).timestamp();
        let summary_at = "2026-08-16T19:00:00Z";
        for i in 0..26 {
            let id = format!("eeeeeeee-bbbb-cccc-dddd-eeeeeeeeee{i:02}");
            write_grok_session(
                &grok,
                "many",
                &id,
                &format!(
                    r#"{{"info":{{"id":"{id}","cwd":"/tmp/many"}},"current_model_id":"grok-4.6","last_active_at":"{summary_at}"}}"#
                ),
                &[&grok_usage_line(1000, 0, ts)],
            );
        }
        let snap = scan_roots(&grok_roots(&grok), "max5x", now);
        assert_eq!(snap.sessions.len(), MAX_PROVIDER_ROWS);
        let week = snap.grok_week.expect("week");
        assert_eq!(week.week_tokens, "26K");
        assert_eq!(week.sessions, MAX_PROVIDER_ROWS);
    }
}
