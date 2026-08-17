//! MCP commands: the "Expose usage data to agents" toggle and per-agent
//! registration of the `agent-status-mcp` sidecar into each agent's config.

use std::sync::Mutex;

use tauri::{AppHandle, State};

use crate::error::ResultExt;
use crate::mcp::{self, McpAgentView};
use crate::settings::{self, Settings, SettingsView};
use crate::state::AppState;
use crate::storage;

fn update_settings(
    state: &State<'_, Mutex<AppState>>,
    mutate: impl FnOnce(&mut Settings),
) -> Result<Settings, String> {
    let mut guard = state.lock().map_err(|e| e.to_string())?;
    mutate(&mut guard.settings);
    Ok(guard.settings.clone())
}

/// Toggle the MCP snapshot export. When turned on, `collect()` writes
/// `agent-snapshot.json` on every refresh; when turned off, the file is
/// deleted so agents stop seeing stale data.
#[tauri::command]
pub async fn set_mcp_enabled(
    app: AppHandle,
    state: State<'_, Mutex<AppState>>,
    enabled: bool,
) -> Result<SettingsView, String> {
    let updated = update_settings(&state, |s| s.mcp_enabled = enabled)?;
    settings::save(&app, &updated).into_string()?;
    if !enabled {
        // Best-effort delete — a missing/unwritable file must not fail the toggle.
        let app2 = app.clone();
        tokio::task::spawn_blocking(move || {
            if let Ok(path) = storage::get_storage_path(&app2, mcp::SNAPSHOT_FILE) {
                match std::fs::remove_file(&path) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => tracing::warn!("failed to delete {}: {e}", path.display()),
                }
            }
        })
        .await
        .map_err(|e| e.to_string())?;
    }
    Ok((&updated).into())
}

/// List the supported agents with their detection/registration state.
#[tauri::command]
pub async fn get_mcp_agents() -> Result<Vec<McpAgentView>, String> {
    tokio::task::spawn_blocking(mcp::list_agents)
        .await
        .map_err(|e| e.to_string())
}

/// Register the sidecar into one agent's MCP config. Returns the refreshed list.
#[tauri::command]
pub async fn register_mcp_agent(id: String) -> Result<Vec<McpAgentView>, String> {
    tokio::task::spawn_blocking(move || mcp::set_registered(&id, true))
        .await
        .map_err(|e| e.to_string())?
}

/// Remove our entry from one agent's MCP config. Returns the refreshed list.
#[tauri::command]
pub async fn unregister_mcp_agent(id: String) -> Result<Vec<McpAgentView>, String> {
    tokio::task::spawn_blocking(move || mcp::set_registered(&id, false))
        .await
        .map_err(|e| e.to_string())?
}
