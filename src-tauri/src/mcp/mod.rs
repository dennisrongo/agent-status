//! MCP (Model Context Protocol) support: a read-only snapshot export that the
//! companion `agent-status-mcp` stdio server serves to AI coding agents, plus
//! per-agent registration into each agent's MCP config file.
//!
//! When `settings.mcp_enabled` is on, `collect()` writes a compact
//! `McpSnapshot` to `agent-snapshot.json` in the app data dir. The sidecar
//! binary reads that file and answers MCP tool calls — it never touches the
//! network, credentials, or this app's state. Registration is a
//! read-modify-write of the agent's own config file (`~/.claude.json`,
//! `~/.cursor/mcp.json`, `$CODEX_HOME/config.toml`, `$KIMI_CODE_HOME/config.toml`)
//! that preserves everything else in the file.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::scanner::UsageSnapshot;
use crate::vendors::VendorStatus;

/// Name of the exported snapshot file inside the app data dir. The sidecar
/// resolves the same path from the bundle identifier.
pub const SNAPSHOT_FILE: &str = "agent-snapshot.json";

/// Binary name of the MCP sidecar, used for sibling-binary resolution and to
/// recognize our own registration entries in agent configs.
pub const MCP_BIN_NAME: &str = "agent-status-mcp";

/// Env var that overrides the sidecar path (dev/testing).
pub const MCP_BIN_ENV: &str = "AGENT_STATUS_MCP_BIN";

/// Minimum seconds between background collects while the window is hidden and
/// the MCP export is on (keeps the snapshot agents read reasonably fresh
/// without the open-window refresh cadence).
pub const MCP_HIDDEN_TICK_SECS: u64 = 300;

/// Whether a hidden-window collect should run now: only when the export is
/// enabled and at least `MCP_HIDDEN_TICK_SECS` have passed since the last one.
/// Extracted so the gating logic is unit-testable without a Tauri runtime.
pub fn hidden_collect_due(mcp_enabled: bool, last_hidden_collect_ms: u64, now_ms: u64) -> bool {
    mcp_enabled && now_ms.saturating_sub(last_hidden_collect_ms) >= MCP_HIDDEN_TICK_SECS * 1000
}

// ── Export snapshot format ─────────────────────────────────────────────────
// The sidecar crate mirrors these types (deliberately NOT shared — the sidecar
// must not depend on the Tauri app). Keep the two in sync.

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct McpSnapshot {
    /// Epoch milliseconds when the snapshot was built (from `Meta.generatedMs`).
    pub generated_ms: u64,
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
    pub windows: Vec<McpWindow>,
    /// Extra labelled rows from the vendor status (plan names, balances, …).
    #[serde(default)]
    pub detail: Vec<McpKeyVal>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct McpWindow {
    /// e.g. "5-hour" or "weekly".
    pub label: String,
    /// Human/LLM-readable usage text, exactly as the app renders it
    /// (e.g. "37% left · resets in 2h 14m").
    pub text: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct McpKeyVal {
    pub label: String,
    pub value: String,
}

/// Strings that carry no usage information and must not become "windows".
fn is_placeholder(s: &str) -> bool {
    let t = s.trim();
    t.is_empty() || t == "—" || t == "no key set" || t == "fetch failed"
}

/// Project one vendor status into an export provider, mapping the headline
/// `primary` line to the 5-hour window and `secondary` to the weekly window.
/// The strings are kept as-is — they are already human/LLM-readable.
fn vendor_provider(
    id: &str,
    name: &str,
    status: &VendorStatus,
    detected: bool,
) -> Option<McpProvider> {
    if !status.configured && !detected {
        return None;
    }
    let mut windows = Vec::new();
    // Windows are only meaningful when the last read succeeded; an error card's
    // primary/secondary are placeholders, not quota data.
    if status.ok {
        if !is_placeholder(&status.primary) {
            windows.push(McpWindow { label: "5-hour".to_string(), text: status.primary.clone() });
        }
        if !is_placeholder(&status.secondary) {
            windows.push(McpWindow { label: "weekly".to_string(), text: status.secondary.clone() });
        }
    }
    Some(McpProvider {
        id: id.to_string(),
        name: name.to_string(),
        configured: status.configured,
        ok: status.ok,
        auth_expired: status.auth_expired,
        error: status.error.clone(),
        windows,
        detail: status
            .detail
            .iter()
            .map(|kv| McpKeyVal { label: kv.label.clone(), value: kv.value.clone() })
            .collect(),
    })
}

/// Build the export snapshot from the merged usage snapshot. Includes Claude
/// (from `limits`, which carries the live meters when enabled) plus one entry
/// per detected/configured vendor. Providers that are neither configured nor
/// detected are skipped.
pub fn build_mcp_snapshot(snapshot: &UsageSnapshot) -> McpSnapshot {
    let mut providers = Vec::new();

    // Claude: the limits buckets carry the session (5-hour) and weekly meters —
    // live /usage data when enabled, the local estimate otherwise. A bucket's
    // display strings already combine "% left" and the reset countdown.
    let claude_detected = snapshot.detection.as_ref().is_some_and(|d| d.claude);
    let claude_signed_in = snapshot.detection.as_ref().is_some_and(|d| d.claude_signed_in);
    if claude_detected || claude_signed_in {
        let windows = snapshot
            .limits
            .buckets
            .iter()
            .map(|b| {
                // Bucket names look like "Session", "Week · all models",
                // "Week · Opus" — keep the suffix so two weekly windows
                // stay distinguishable to an LLM consumer.
                let label = if b.name.to_ascii_lowercase().contains("week") {
                    match b.name.split_once('·') {
                        Some((_, suffix)) => format!("weekly ({})", suffix.trim()),
                        None => "weekly".to_string(),
                    }
                } else {
                    "5-hour".to_string()
                };
                let mut text = format!("{:.0}% left", b.left_pct);
                if !b.reset.is_empty() && b.reset != "ready" && b.reset != "resetting" {
                    text.push_str(&format!(" · resets in {}", b.reset));
                }
                McpWindow { label, text }
            })
            .collect();
        providers.push(McpProvider {
            id: "claude".to_string(),
            name: "Claude (Anthropic)".to_string(),
            configured: claude_signed_in,
            // ok = the windows carry usable data. Unlike the vendor entries
            // (network fetch succeeded), Claude's meters also exist as local
            // estimates when signed out — only needs_reauth makes them unusable.
            ok: !snapshot.limits.needs_reauth,
            auth_expired: snapshot.detection.as_ref().is_some_and(|d| d.claude_expired),
            error: if snapshot.limits.needs_reauth {
                Some(snapshot.limits.estimate_note.clone())
            } else {
                None
            },
            windows,
            detail: Vec::new(),
        });
    }

    if let Some(vendor) = &snapshot.vendor {
        let det = snapshot.detection.as_ref();
        let mut push = |id: &str,
                        name: &str,
                        status: &VendorStatus,
                        detected: fn(&crate::vendors::Detection) -> bool| {
            let d = det.is_some_and(detected);
            if let Some(p) = vendor_provider(id, name, status, d) {
                providers.push(p);
            }
        };
        push("zai", "Z.ai", &vendor.glm, |d| d.glm);
        push("copilot", "GitHub Copilot", &vendor.copilot, |d| d.copilot);
        push("alibaba", "Alibaba Bailian", &vendor.alibaba, |d| d.alibaba);
        push("kimi", "Kimi (Moonshot)", &vendor.kimi, |d| d.kimi);
        push("grok", "Grok (xAI)", &vendor.grok, |d| d.grok);
        push("codex", "Codex (OpenAI)", &vendor.codex, |d| d.codex);
        // vendor.anthropic is org-level API cost (Admin API), not a coding
        // quota window — the "claude" entry above already covers Anthropic
        // capacity, so it is deliberately not exported.
    }

    McpSnapshot {
        generated_ms: snapshot.meta.generated_ms.max(0) as u64,
        providers,
    }
}

// ── Agent registration ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpAgentView {
    pub id: String,
    pub name: String,
    /// Whether the agent's config dir/file exists on this machine.
    pub detected: bool,
    /// Whether its config already points at our sidecar binary.
    pub registered: bool,
    pub config_path: String,
    /// Resolved sidecar path (None when the binary hasn't been built/found).
    pub command_path: Option<String>,
}

#[derive(Clone, Copy, PartialEq)]
enum AgentKind {
    /// JSON config with a top-level `mcpServers` object (Claude Code, Cursor).
    Json,
    /// TOML config with a `[mcp_servers]` table (Codex, Kimi).
    Toml,
}

struct AgentSpec {
    id: &'static str,
    name: &'static str,
    kind: AgentKind,
}

const AGENTS: &[AgentSpec] = &[
    AgentSpec { id: "claude-code", name: "Claude Code", kind: AgentKind::Json },
    AgentSpec { id: "cursor", name: "Cursor", kind: AgentKind::Json },
    AgentSpec { id: "codex", name: "Codex CLI", kind: AgentKind::Toml },
    AgentSpec { id: "kimi", name: "Kimi Code", kind: AgentKind::Toml },
];

/// The agent's config file path, honoring each agent's home override
/// (`$CODEX_HOME`, `$KIMI_CODE_HOME`). `None` when the home dir can't be
/// resolved (no HOME/USERPROFILE) — surfaced as an Err string by the commands.
/// `$CODEX_HOME` / `$KIMI_CODE_HOME` ARE the agent's home dir (used directly by
/// the CLIs and this app's vendors); only the home-dir fallback gets the
/// dot-dir appended. Split out pure so the env handling is unit-testable.
fn resolve_agent_home(env: Option<std::ffi::OsString>, home: Option<PathBuf>, dot: &str) -> Option<PathBuf> {
    env.and_then(|h| {
        let p = PathBuf::from(h);
        if p.as_os_str().is_empty() { None } else { Some(p) }
    })
    .or_else(|| home.map(|h| h.join(dot)))
}

fn config_path_for(id: &str) -> Option<PathBuf> {
    let home = dirs::home_dir;
    match id {
        "claude-code" => home().map(|h| h.join(".claude.json")),
        "cursor" => home().map(|h| h.join(".cursor").join("mcp.json")),
        "codex" => resolve_agent_home(std::env::var_os("CODEX_HOME"), home(), ".codex")
            .map(|h| h.join("config.toml")),
        "kimi" => resolve_agent_home(std::env::var_os("KIMI_CODE_HOME"), home(), ".kimi-code")
            .map(|h| h.join("config.toml")),
        _ => None,
    }
}

/// "Detected" = the agent's config dir or file exists (the agent is or was
/// installed). Registration can still create the file when undetected.
fn agent_detected(id: &str, config: &std::path::Path) -> bool {
    if config.exists() {
        return true;
    }
    match config.parent() {
        // ~/.claude.json lives directly in HOME — the .claude dir marks the install.
        Some(parent) if id == "claude-code" => parent.join(".claude").is_dir(),
        Some(parent) => parent.is_dir(),
        None => false,
    }
}

/// Persist the export snapshot for the sidecar, atomically (temp + rename) so
/// the server never reads a half-written file. Blocking — call via
/// `spawn_blocking`.
pub fn write_snapshot(app: &tauri::AppHandle, snap: &McpSnapshot) -> Result<(), String> {
    let path = crate::storage::get_storage_path(app, SNAPSHOT_FILE).map_err(|e| e.to_string())?;
    let contents = serde_json::to_string_pretty(snap).map_err(|e| e.to_string())?;
    write_file(&path, contents)
}

/// Resolve the sidecar binary path: `$AGENT_STATUS_MCP_BIN` override, then a
/// sibling of the running executable (bundled app), then the dev build outputs
/// under the workspace target dir.
pub fn resolve_command_path() -> Option<String> {
    let exe_name = if cfg!(windows) { "agent-status-mcp.exe" } else { MCP_BIN_NAME };
    if let Some(p) = std::env::var_os(MCP_BIN_ENV) {
        let p = PathBuf::from(p);
        if !p.as_os_str().is_empty() {
            return Some(p.to_string_lossy().into_owned());
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let sibling = dir.join(exe_name);
            if sibling.exists() {
                return Some(sibling.to_string_lossy().into_owned());
            }
        }
    }
    // Dev fallback: the workspace target dir (compile-time manifest dir is
    // src-tauri, which is also the workspace root).
    for profile in ["release", "debug"] {
        let candidate = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(profile)
            .join(exe_name);
        if candidate.exists() {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }
    None
}

fn spec_for(id: &str) -> Result<&'static AgentSpec, String> {
    AGENTS
        .iter()
        .find(|a| a.id == id)
        .ok_or_else(|| format!("unknown agent: {id} (valid: claude-code, cursor, codex, kimi)"))
}

/// Read-modify-write a JSON agent config, setting/removing
/// `mcpServers["agent-status"]`. Everything else in the file is preserved.
fn json_set_server(path: &std::path::Path, command: Option<&str>) -> Result<(), String> {
    let raw = if path.exists() {
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?
    } else {
        String::new()
    };
    let mut root: serde_json::Value = if raw.trim().is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_str(&raw).map_err(|e| format!("parse {}: {e}", path.display()))?
    };
    if !root.is_object() {
        return Err(format!("{}: expected a top-level JSON object", path.display()));
    }
    match root.get("mcpServers") {
        Some(v) if !v.is_object() => {
            return Err(format!("{}: mcpServers is not an object", path.display()))
        }
        None if command.is_none() => return Ok(()), // nothing to remove
        _ => {}
    }
    if root.get("mcpServers").is_none() {
        root["mcpServers"] = serde_json::json!({});
    }
    let servers = root
        .get_mut("mcpServers")
        .and_then(|v| v.as_object_mut())
        .ok_or_else(|| format!("{}: mcpServers is not an object", path.display()))?;
    match command {
        Some(cmd) => {
            servers.insert(
                "agent-status".to_string(),
                serde_json::json!({ "command": cmd, "args": [] }),
            );
        }
        None => {
            servers.remove("agent-status");
        }
    }
    write_file(path, serde_json::to_string_pretty(&root).map_err(|e| e.to_string())?)
}

/// Read-modify-write a TOML agent config, setting/removing
/// `[mcp_servers.agent-status]`. `toml_edit` keeps unrelated sections,
/// comments, and formatting intact.
fn toml_set_server(path: &std::path::Path, command: Option<&str>) -> Result<(), String> {
    let raw = if path.exists() {
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?
    } else {
        String::new()
    };
    let mut doc = if raw.trim().is_empty() {
        toml_edit::DocumentMut::new()
    } else {
        raw.parse::<toml_edit::DocumentMut>()
            .map_err(|e| format!("parse {}: {e}", path.display()))?
    };
    if doc.get("mcp_servers").is_some_and(|v| !v.is_table_like()) {
        return Err(format!("{}: mcp_servers is not a table", path.display()));
    }
    match command {
        Some(cmd) => {
            if doc.get("mcp_servers").is_none() {
                doc["mcp_servers"] = toml_edit::Item::Table(toml_edit::Table::new());
            }
            let servers = doc["mcp_servers"]
                .as_table_like_mut()
                .ok_or_else(|| format!("{}: mcp_servers is not a table", path.display()))?;
            let mut entry = toml_edit::Table::new();
            entry["command"] = toml_edit::value(cmd);
            entry["args"] = toml_edit::value(toml_edit::Array::new());
            servers.insert("agent-status", toml_edit::Item::Table(entry));
        }
        None => {
            if let Some(servers) = doc["mcp_servers"].as_table_like_mut() {
                servers.remove("agent-status");
            } else {
                return Ok(());
            }
        }
    }
    write_file(path, doc.to_string())
}

/// Write via a sibling temp file + rename so a crash mid-write can never
/// leave a user's agent config (e.g. the whole ~/.claude.json) truncated.
fn write_file(path: &std::path::Path, contents: String) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    let tmp = path.with_extension("agent-status-tmp");
    std::fs::write(&tmp, contents).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("rename {} → {}: {e}", tmp.display(), path.display()))
}

/// Whether the config already registers a server whose command points at our
/// sidecar binary.
fn is_registered(kind: AgentKind, path: &std::path::Path) -> bool {
    let Ok(raw) = std::fs::read_to_string(path) else { return false };
    match kind {
        AgentKind::Json => serde_json::from_str::<serde_json::Value>(&raw)
            .ok()
            .and_then(|root| {
                root["mcpServers"]["agent-status"]["command"]
                    .as_str()
                    .map(str::to_string)
            })
            .is_some_and(|cmd| cmd.contains(MCP_BIN_NAME)),
        AgentKind::Toml => raw
            .parse::<toml_edit::DocumentMut>()
            .ok()
            .and_then(|doc| {
                doc.get("mcp_servers")?
                    .get("agent-status")?
                    .get("command")?
                    .as_str()
                    .map(str::to_string)
            })
            .is_some_and(|cmd| cmd.contains(MCP_BIN_NAME)),
    }
}

/// Snapshot every agent's view (sync file IO — call via `spawn_blocking`).
pub fn list_agents() -> Vec<McpAgentView> {
    let command_path = resolve_command_path();
    AGENTS
        .iter()
        .map(|spec| {
            let path = config_path_for(spec.id).unwrap_or_default();
            McpAgentView {
                id: spec.id.to_string(),
                name: spec.name.to_string(),
                detected: agent_detected(spec.id, &path),
                registered: is_registered(spec.kind, &path),
                config_path: path.to_string_lossy().into_owned(),
                command_path: command_path.clone(),
            }
        })
        .collect()
}

/// Register or unregister one agent (sync file IO — call via
/// `spawn_blocking`). Returns the refreshed list on success.
pub fn set_registered(id: &str, register: bool) -> Result<Vec<McpAgentView>, String> {
    let spec = spec_for(id)?;
    let path = config_path_for(id)
        .ok_or_else(|| format!("could not resolve the home directory for {id}"))?;
    let command = if register {
        Some(resolve_command_path().ok_or_else(|| {
            format!(
                "the {MCP_BIN_NAME} binary was not found — build it first (npm run build:mcp) or set ${MCP_BIN_ENV}"
            )
        })?)
    } else {
        None
    };
    match spec.kind {
        AgentKind::Json => json_set_server(&path, command.as_deref())?,
        AgentKind::Toml => toml_set_server(&path, command.as_deref())?,
    }
    Ok(list_agents())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── hidden_collect_due ──

    #[test]
    fn hidden_tick_requires_toggle_on() {
        assert!(!hidden_collect_due(false, 0, 9_999_999));
        assert!(hidden_collect_due(true, 0, MCP_HIDDEN_TICK_SECS * 1000));
    }

    #[test]
    fn hidden_tick_respects_interval() {
        let t = 1_000_000u64;
        assert!(!hidden_collect_due(true, t, t + MCP_HIDDEN_TICK_SECS * 1000 - 1));
        assert!(hidden_collect_due(true, t, t + MCP_HIDDEN_TICK_SECS * 1000));
    }

    #[test]
    fn hidden_tick_first_run_collects_immediately() {
        // last=0: a fresh app with MCP on writes the snapshot on the first
        // hidden tick rather than waiting 5 minutes.
        assert!(hidden_collect_due(true, 0, MCP_HIDDEN_TICK_SECS * 1000 + 1));
    }

    // ── resolve_agent_home ──

    #[test]
    fn agent_home_env_is_used_directly() {
        // $CODEX_HOME IS the codex home — no ".codex" appended to it.
        let env = Some(std::ffi::OsString::from("/data/codex"));
        let home = Some(PathBuf::from("/home/u"));
        assert_eq!(
            resolve_agent_home(env, home, ".codex"),
            Some(PathBuf::from("/data/codex"))
        );
    }

    #[test]
    fn agent_home_fallback_appends_dot_dir() {
        let home = Some(PathBuf::from("/home/u"));
        assert_eq!(
            resolve_agent_home(None, home, ".codex"),
            Some(PathBuf::from("/home/u/.codex"))
        );
    }

    #[test]
    fn agent_home_empty_env_falls_back() {
        let env = Some(std::ffi::OsString::new());
        let home = Some(PathBuf::from("/home/u"));
        assert_eq!(
            resolve_agent_home(env, home, ".codex"),
            Some(PathBuf::from("/home/u/.codex"))
        );
        assert_eq!(resolve_agent_home(None, None, ".codex"), None);
    }

    // ── JSON merge (claude-code / cursor) ──

    #[test]
    fn json_register_preserves_other_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        std::fs::write(
            &path,
            r#"{"theme":"dark","mcpServers":{"other":{"command":"x","args":["--y"]}}}"#,
        )
        .unwrap();
        json_set_server(&path, Some("/usr/local/bin/agent-status-mcp")).unwrap();
        let root: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(root["theme"], "dark");
        assert_eq!(root["mcpServers"]["other"]["command"], "x");
        assert_eq!(root["mcpServers"]["agent-status"]["command"], "/usr/local/bin/agent-status-mcp");
        assert_eq!(root["mcpServers"]["agent-status"]["args"], serde_json::json!([]));
        assert!(is_registered(AgentKind::Json, &path));
    }

    #[test]
    fn json_register_creates_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join(".claude.json");
        json_set_server(&path, Some("/opt/agent-status-mcp")).unwrap();
        let root: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(root["mcpServers"]["agent-status"]["command"], "/opt/agent-status-mcp");
    }

    #[test]
    fn json_unregister_removes_only_our_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        std::fs::write(
            &path,
            r#"{"mcpServers":{"other":{"command":"x"},"agent-status":{"command":"/a/agent-status-mcp","args":[]}}}"#,
        )
        .unwrap();
        json_set_server(&path, None).unwrap();
        let root: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(root["mcpServers"]["other"]["command"].is_string());
        assert!(root["mcpServers"]["agent-status"].is_null());
        assert!(!is_registered(AgentKind::Json, &path));
    }

    #[test]
    fn json_register_rejects_corrupt_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        std::fs::write(&path, "{not json").unwrap();
        let err = json_set_server(&path, Some("/x/agent-status-mcp")).unwrap_err();
        assert!(err.contains("parse"), "got: {err}");
        // The corrupt file is left untouched.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{not json");
    }

    // ── TOML merge (codex / kimi) ──

    #[test]
    fn toml_register_preserves_unrelated_sections() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "model = \"gpt-5\"\n\n[mcp_servers.pencil]\ncommand = \"pencil-mcp\"\nargs = [\"--fast\"]\n",
        )
        .unwrap();
        toml_set_server(&path, Some("/opt/agent-status-mcp")).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("model = \"gpt-5\""), "got:\n{text}");
        assert!(text.contains("[mcp_servers.pencil]"), "got:\n{text}");
        assert!(text.contains("pencil-mcp"), "got:\n{text}");
        let doc = text.parse::<toml_edit::DocumentMut>().unwrap();
        assert_eq!(
            doc["mcp_servers"]["agent-status"]["command"].as_str(),
            Some("/opt/agent-status-mcp")
        );
        assert_eq!(
            doc["mcp_servers"]["agent-status"]["args"].as_array().map(|a| a.len()),
            Some(0)
        );
        assert!(is_registered(AgentKind::Toml, &path));
    }

    #[test]
    fn toml_register_creates_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("config.toml");
        toml_set_server(&path, Some("/opt/agent-status-mcp")).unwrap();
        let doc = std::fs::read_to_string(&path)
            .unwrap()
            .parse::<toml_edit::DocumentMut>()
            .unwrap();
        assert_eq!(
            doc["mcp_servers"]["agent-status"]["command"].as_str(),
            Some("/opt/agent-status-mcp")
        );
    }

    #[test]
    fn toml_unregister_removes_only_our_table() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[mcp_servers.agent-status]\ncommand = \"/a/agent-status-mcp\"\nargs = []\n\n[mcp_servers.pencil]\ncommand = \"pencil-mcp\"\n",
        )
        .unwrap();
        toml_set_server(&path, None).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("agent-status"), "got:\n{text}");
        assert!(text.contains("[mcp_servers.pencil]"), "got:\n{text}");
        assert!(!is_registered(AgentKind::Toml, &path));
    }

    #[test]
    fn toml_register_rejects_corrupt_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[unclosed").unwrap();
        let err = toml_set_server(&path, Some("/x/agent-status-mcp")).unwrap_err();
        assert!(err.contains("parse"), "got: {err}");
    }

    #[test]
    fn is_registered_requires_our_binary_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        std::fs::write(&path, r#"{"mcpServers":{"agent-status":{"command":"/other/tool"}}}"#)
            .unwrap();
        assert!(!is_registered(AgentKind::Json, &path));
    }

    #[test]
    fn unknown_agent_is_an_error_not_a_panic() {
        assert!(spec_for("windsurf").is_err());
    }
}
