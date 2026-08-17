//! agent-status-mcp — a tiny, read-only MCP stdio server that lets AI coding
//! agents query the AI-provider capacity snapshot written by the Agent Usage
//! Monitor app. It reads ONE cached JSON file and answers tool calls; it never
//! touches the network, credentials, or the app's state, and it writes nothing
//! but MCP protocol frames on stdout (any diagnostics go to stderr).

use std::path::PathBuf;

use anyhow::Result;
use rmcp::{
    ErrorData as McpError, ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::*,
    tool, tool_handler, tool_router,
    transport::stdio,
};
use serde::{Deserialize, Serialize};

/// App data dir name = the Tauri bundle identifier (tauri.conf.json).
const APP_DIR_NAME: &str = "com.dennisrongo.agentstatus";
const SNAPSHOT_FILE: &str = "agent-snapshot.json";

const SETUP_HINT: &str = "No usage snapshot is available yet. Open Agent Usage Monitor → Settings → enable \"Expose usage data to agents (MCP)\". The app writes a read-only snapshot (agent-snapshot.json) that this server serves; it refreshes while the app runs (up to ~5 min old while the window is hidden).";

// ── Snapshot format (mirror of src-tauri/src/mcp/mod.rs — keep in sync) ────

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct McpSnapshot {
    pub generated_ms: u64,
    #[serde(default)]
    pub providers: Vec<McpProvider>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct McpProvider {
    pub id: String,
    pub name: String,
    pub configured: bool,
    pub ok: bool,
    pub auth_expired: bool,
    pub error: Option<String>,
    #[serde(default)]
    pub windows: Vec<McpWindow>,
    #[serde(default)]
    pub detail: Vec<McpKeyVal>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct McpWindow {
    pub label: String,
    pub text: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct McpKeyVal {
    pub label: String,
    pub value: String,
}

// ── Snapshot loading + rendering (pure, unit-tested) ───────────────────────

pub fn default_snapshot_path() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join(APP_DIR_NAME).join(SNAPSHOT_FILE))
}

/// Load and parse the snapshot. Err carries a user-facing explanation.
pub fn load_snapshot(path: &std::path::Path) -> Result<McpSnapshot, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| format!("could not read {}: {e}", path.display()))?;
    serde_json::from_str(&raw).map_err(|e| format!("could not parse {}: {e}", path.display()))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn rfc3339(ms: u64) -> String {
    chrono::DateTime::from_timestamp_millis(ms as i64)
        .map(|t| t.to_rfc3339())
        .unwrap_or_else(|| "unknown".to_string())
}

fn render_provider(p: &McpProvider, out: &mut String) {
    let state = if p.auth_expired {
        "auth expired — re-login needed"
    } else if p.ok {
        "ok"
    } else if p.configured {
        "error"
    } else {
        "not configured"
    };
    out.push_str(&format!("- {} (id: {}): {state}\n", p.name, p.id));
    if let Some(err) = &p.error {
        out.push_str(&format!("  error: {err}\n"));
    }
    for w in &p.windows {
        out.push_str(&format!("  {}: {}\n", w.label, w.text));
    }
}

/// The `get_capacity` response body.
pub fn capacity_text(snap: &McpSnapshot, now_ms: u64) -> String {
    let mut out = String::new();
    let age_secs = now_ms.saturating_sub(snap.generated_ms) / 1000;
    out.push_str(&format!(
        "Snapshot generated at {} ({} seconds ago). Read-only cached data from Agent Usage Monitor.\n\n",
        rfc3339(snap.generated_ms),
        age_secs
    ));
    if snap.providers.is_empty() {
        out.push_str("No providers are configured or detected yet.\n");
        return out;
    }
    out.push_str("Providers:\n");
    for p in &snap.providers {
        render_provider(p, &mut out);
    }
    let suggested: Vec<&str> = snap
        .providers
        .iter()
        .filter(|p| p.ok && !p.auth_expired)
        .map(|p| p.id.as_str())
        .collect();
    out.push_str("\nSuggested (ok and authenticated, compare their 5-hour windows):\n");
    if suggested.is_empty() {
        out.push_str("- none right now — every provider is failing or unauthenticated\n");
    } else {
        for id in suggested {
            out.push_str(&format!("- {id}\n"));
        }
    }
    out
}

/// The `get_provider_status` response body. Unknown ids list the valid ones.
pub fn provider_status_text(snap: &McpSnapshot, provider: &str) -> String {
    let id = provider.trim().to_ascii_lowercase();
    match snap.providers.iter().find(|p| p.id == id) {
        Some(p) => {
            let mut out = String::new();
            render_provider(p, &mut out);
            if !p.detail.is_empty() {
                out.push_str("  detail:\n");
                for kv in &p.detail {
                    out.push_str(&format!("    {}: {}\n", kv.label, kv.value));
                }
            }
            out
        }
        None => {
            let valid: Vec<&str> = snap.providers.iter().map(|p| p.id.as_str()).collect();
            format!(
                "Unknown provider id: {provider}. Valid ids in the current snapshot: {}\n",
                if valid.is_empty() { "(none)".to_string() } else { valid.join(", ") }
            )
        }
    }
}

// ── MCP server ─────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
pub struct ProviderRequest {
    /// Provider id: claude, zai, copilot, alibaba, kimi, grok, or codex.
    pub provider: String,
}

#[derive(Clone)]
pub struct AgentStatus {
    snapshot_path: PathBuf,
    #[allow(dead_code)] // consumed by the #[tool_handler] ServerHandler wiring
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl AgentStatus {
    pub fn new(snapshot_path: PathBuf) -> Self {
        Self { snapshot_path, tool_router: Self::tool_router() }
    }

    /// Show AI coding-provider capacity (5-hour and weekly windows) across
    /// every authenticated provider. Use before starting a long task to pick
    /// the provider with the most 5-hour headroom. Data is a read-only cached
    /// snapshot written by the Agent Usage Monitor app.
    #[tool(description = "Show AI coding-provider capacity (5-hour and weekly windows) across every authenticated provider. Use before starting a long task to pick the provider with the most 5-hour headroom. Data is a read-only cached snapshot written by the Agent Usage Monitor app.")]
    fn get_capacity(&self) -> Result<CallToolResult, McpError> {
        match load_snapshot(&self.snapshot_path) {
            Ok(snap) => Ok(CallToolResult::success(vec![ContentBlock::text(
                capacity_text(&snap, now_ms()),
            )])),
            // is_error=true so clients can tell "no data" from a real answer;
            // the text still carries the setup hint for the user.
            Err(e) => Ok(CallToolResult::error(vec![ContentBlock::text(
                format!("{e}\n\n{SETUP_HINT}"),
            )])),
        }
    }

    /// Show detailed status for one provider id (claude, zai, copilot,
    /// alibaba, kimi, grok, codex), including extra detail rows.
    #[tool(description = "Show detailed status for one AI coding provider by id (claude, zai, copilot, alibaba, kimi, grok, codex): configured/ok/auth state, 5-hour and weekly windows, and extra detail rows.")]
    fn get_provider_status(
        &self,
        Parameters(req): Parameters<ProviderRequest>,
    ) -> Result<CallToolResult, McpError> {
        match load_snapshot(&self.snapshot_path) {
            Ok(snap) => Ok(CallToolResult::success(vec![ContentBlock::text(
                provider_status_text(&snap, &req.provider),
            )])),
            Err(e) => Ok(CallToolResult::error(vec![ContentBlock::text(
                format!("{e}\n\n{SETUP_HINT}"),
            )])),
        }
    }
}

#[tool_handler]
impl ServerHandler for AgentStatus {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info({
                let mut info = Implementation::from_build_env();
                info.name = "agent-status".to_string();
                info.version = env!("CARGO_PKG_VERSION").to_string();
                info
            })
            .with_instructions(
                "Read-only access to the Agent Usage Monitor cached snapshot: \
                 per-provider AI coding capacity (5-hour / weekly windows). \
                 Call get_capacity before long tasks to pick the provider with \
                 the most headroom."
                    .to_string(),
            )
    }
}

fn parse_args() -> Option<PathBuf> {
    let mut args = std::env::args().skip(1);
    let mut path = None;
    while let Some(arg) = args.next() {
        if arg == "--snapshot-path" {
            path = args.next().map(PathBuf::from);
        }
    }
    path
}

#[tokio::main]
async fn main() -> Result<()> {
    let snapshot_path = parse_args()
        .or_else(default_snapshot_path)
        .ok_or_else(|| anyhow::anyhow!("could not resolve the app data directory"))?;
    eprintln!("agent-status-mcp: serving snapshot {}", snapshot_path.display());

    let service = AgentStatus::new(snapshot_path).serve(stdio()).await.inspect_err(|e| {
        eprintln!("agent-status-mcp: serving error: {e:?}");
    })?;
    service.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"{
        "generatedMs": 1755400000000,
        "providers": [
            {
                "id": "claude",
                "name": "Claude (Anthropic)",
                "configured": true,
                "ok": true,
                "authExpired": false,
                "error": null,
                "windows": [
                    { "label": "5-hour", "text": "37% left · resets in 2h 14m" },
                    { "label": "weekly", "text": "82% left · resets in 4d 3h" }
                ],
                "detail": []
            },
            {
                "id": "kimi",
                "name": "Kimi (Moonshot)",
                "configured": true,
                "ok": false,
                "authExpired": true,
                "error": "login expired",
                "windows": [],
                "detail": [
                    { "label": "Plan", "value": "Pro" }
                ]
            }
        ]
    }"#;

    fn fixture_snapshot() -> McpSnapshot {
        serde_json::from_str(FIXTURE).expect("fixture must parse")
    }

    #[test]
    fn fixture_parses_camel_case() {
        let snap = fixture_snapshot();
        assert_eq!(snap.generated_ms, 1_755_400_000_000);
        assert_eq!(snap.providers.len(), 2);
        assert_eq!(snap.providers[0].windows[0].label, "5-hour");
        assert!(snap.providers[1].auth_expired);
    }

    #[test]
    fn load_snapshot_reads_fixture_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent-snapshot.json");
        std::fs::write(&path, FIXTURE).unwrap();
        let snap = load_snapshot(&path).unwrap();
        assert_eq!(snap.providers[0].id, "claude");
    }

    #[test]
    fn load_snapshot_missing_file_is_an_error_string() {
        let dir = tempfile::tempdir().unwrap();
        let err = load_snapshot(&dir.path().join("nope.json")).unwrap_err();
        assert!(err.contains("could not read"), "got: {err}");
    }

    #[test]
    fn load_snapshot_corrupt_file_is_an_error_string() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.json");
        std::fs::write(&path, "{nope").unwrap();
        let err = load_snapshot(&path).unwrap_err();
        assert!(err.contains("could not parse"), "got: {err}");
    }

    #[test]
    fn capacity_text_reports_staleness_and_suggested() {
        let snap = fixture_snapshot();
        let now = snap.generated_ms + 42_000;
        let text = capacity_text(&snap, now);
        assert!(text.contains("42 seconds ago"), "got:\n{text}");
        assert!(text.contains("37% left · resets in 2h 14m"), "got:\n{text}");
        // claude is ok → suggested; kimi is auth-expired → not suggested.
        assert!(text.contains("- claude"), "got:\n{text}");
        let suggested = text.split("Suggested").nth(1).unwrap();
        assert!(!suggested.contains("- kimi"), "got:\n{text}");
    }

    #[test]
    fn capacity_text_empty_snapshot_suggests_nothing() {
        let snap = McpSnapshot { generated_ms: 0, providers: vec![] };
        let text = capacity_text(&snap, 10_000);
        assert!(text.contains("No providers"), "got:\n{text}");
    }

    #[test]
    fn provider_status_unknown_id_lists_valid_ids() {
        let snap = fixture_snapshot();
        let text = provider_status_text(&snap, "windsurf");
        assert!(text.contains("Unknown provider id"), "got:\n{text}");
        assert!(text.contains("claude") && text.contains("kimi"), "got:\n{text}");
    }

    #[test]
    fn provider_status_includes_detail_rows() {
        let snap = fixture_snapshot();
        let text = provider_status_text(&snap, "kimi");
        assert!(text.contains("Plan: Pro"), "got:\n{text}");
        assert!(text.contains("auth expired"), "got:\n{text}");
    }
}
