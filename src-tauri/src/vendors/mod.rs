//! Live vendor-side usage clients. Network calls are thin; the JSON parsing is
//! pure and unit-tested. Every fetch degrades gracefully to an error string so
//! a bad key or endpoint never crashes the scan.

pub mod anthropic;
pub mod claude;
pub mod glm;

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyVal {
    pub label: String,
    pub value: String,
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
}

/// Which providers are actually present on this machine, so the UI can hide the
/// tab for a provider that isn't installed/configured.
///
/// - `claude`: a Claude Code login token exists, local session logs were found,
///   or the `claude` CLI is on PATH.
/// - `glm`: a z.ai API key is configured, or local MCP server logs exist.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Detection {
    pub claude: bool,
    pub glm: bool,
}
