//! Live vendor-side usage clients. Network calls are thin; the JSON parsing is
//! pure and unit-tested. Every fetch degrades gracefully to an error string so
//! a bad key or endpoint never crashes the scan.

pub mod anthropic;
pub mod claude;
pub mod copilot;
pub mod glm;

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyVal {
    pub label: String,
    pub value: String,
    /// Percent used (0–100) when this row is a quota meter, so the UI can draw a
    /// status-colored progress bar consistent with Claude's. `None` for plain
    /// text rows, in which case it's omitted from the JSON.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pct: Option<f64>,
    /// Meter severity ("ok"/"warn"/"danger"), paired with `pct`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<&'static str>,
}

impl KeyVal {
    /// A plain labelled text row (no progress bar).
    pub fn text(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self { label: label.into(), value: value.into(), pct: None, status: None }
    }

    /// A quota meter row: the UI renders a status-colored bar at `pct`. The
    /// percent is rounded to one decimal to match Claude's buckets and clamped
    /// to 0–100 so a bad payload can't render a >100% bar or a negative fill;
    /// the severity uses the same thresholds as the local scanner's `status_for`.
    pub fn meter(label: impl Into<String>, value: impl Into<String>, pct: f64) -> Self {
        let pct = ((pct * 10.0).round() / 10.0).clamp(0.0, 100.0);
        let status = if pct < 70.0 {
            "ok"
        } else if pct < 90.0 {
            "warn"
        } else {
            "danger"
        };
        Self { label: label.into(), value: value.into(), pct: Some(pct), status: Some(status) }
    }
}

/// Trim a reset timestamp to its date part (`2026-07-01T00:00:00Z` -> `2026-07-01`).
pub(crate) fn short_date(s: &str) -> String {
    s.split(['T', ' ']).next().unwrap_or(s).to_string()
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VendorStatus {
    /// Whether an API key is stored for this vendor.
    pub configured: bool,
    /// Whether the last fetch succeeded.
    pub ok: bool,
    /// Error message when `ok` is false.
    pub error: Option<String>,
    /// Headline value (e.g. balance or cost).
    pub primary: String,
    /// Secondary line.
    pub secondary: String,
    /// Extra labelled rows.
    pub detail: Vec<KeyVal>,
}

impl VendorStatus {
    pub fn not_configured() -> Self {
        Self {
            configured: false,
            ok: false,
            error: None,
            primary: "—".to_string(),
            secondary: "no key set".to_string(),
            detail: Vec::new(),
        }
    }

    pub fn failed(msg: impl Into<String>) -> Self {
        Self {
            configured: true,
            ok: false,
            error: Some(msg.into()),
            primary: "—".to_string(),
            secondary: "fetch failed".to_string(),
            detail: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VendorReport {
    pub glm: VendorStatus,
    pub anthropic: VendorStatus,
    pub copilot: VendorStatus,
}

/// Which providers are actually present on this machine, so the UI can hide the
/// tab for a provider that isn't installed/configured.
///
/// - `claude`: a Claude Code login token exists, local session logs were found,
///   or the `claude` CLI is on PATH.
/// - `glm`: a z.ai API key is configured, or local MCP server logs exist.
/// - `copilot`: a Copilot/GitHub OAuth token was found locally (editor token
///   file or `gh` CLI) or connected in-app.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Detection {
    pub claude: bool,
    pub glm: bool,
    pub copilot: bool,
}
