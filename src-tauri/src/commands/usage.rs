//! Usage commands: scan logs + fetch live vendor data, manage plan + API keys.

use std::sync::Mutex;

use tauri::{AppHandle, Emitter, Manager, State};

use crate::encryption::{self, EncryptedSecret};
use crate::error::ResultExt;
use crate::scanner::{self, UsageSnapshot};
use crate::settings::{self, Settings, SettingsView};
use crate::state::AppState;
use crate::vendors::{anthropic, glm, VendorReport, VendorStatus};

/// Scan local logs AND fetch live vendor usage, merge into one snapshot, and
/// cache it in state. Shared by the `get_usage` command and the background
/// refresh timer.
pub async fn collect(app: &AppHandle) -> Result<UsageSnapshot, String> {
    let (plan, glm_endpoint, zai_key, anthropic_key) = {
        let state = app.state::<Mutex<AppState>>();
        let guard = state.lock().map_err(|e| e.to_string())?;
        (
            guard.settings.plan.clone(),
            guard.settings.glm_endpoint.clone(),
            guard.settings.zai_key.clone(),
            guard.settings.anthropic_key.clone(),
        )
    };

    // Blocking file scan off the IPC runtime.
    let mut snapshot = tokio::task::spawn_blocking(move || scanner::scan_default(&plan))
        .await
        .map_err(|e| e.to_string())?
        .into_string()?;

    // Live vendor fetches (network, async).
    let glm_status = fetch_glm(zai_key, &glm_endpoint).await;
    let anthropic_status = fetch_anthropic(anthropic_key).await;
    snapshot.vendor = Some(VendorReport {
        glm: glm_status,
        anthropic: anthropic_status,
    });

    {
        let state = app.state::<Mutex<AppState>>();
        let mut guard = state.lock().map_err(|e| e.to_string())?;
        guard.snapshot = Some(snapshot.clone());
    }

    Ok(snapshot)
}

async fn fetch_glm(key: Option<EncryptedSecret>, endpoint: &str) -> VendorStatus {
    match key {
        None => VendorStatus::not_configured(),
        Some(secret) => match encryption::decrypt(&secret) {
            Ok(api_key) => glm::fetch(&api_key, endpoint).await,
            Err(e) => VendorStatus::failed(format!("key decrypt: {e}")),
        },
    }
}

async fn fetch_anthropic(key: Option<EncryptedSecret>) -> VendorStatus {
    match key {
        None => VendorStatus::not_configured(),
        Some(secret) => match encryption::decrypt(&secret) {
            Ok(api_key) => anthropic::fetch(&api_key).await,
            Err(e) => VendorStatus::failed(format!("key decrypt: {e}")),
        },
    }
}

#[tauri::command]
pub async fn get_usage(app: AppHandle) -> Result<UsageSnapshot, String> {
    let snapshot = collect(&app).await?;
    let _ = app.emit("usage-updated", &snapshot);
    Ok(snapshot)
}

#[tauri::command]
pub fn get_settings(state: State<'_, Mutex<AppState>>) -> Result<SettingsView, String> {
    let guard = state.lock().map_err(|e| e.to_string())?;
    Ok((&guard.settings).into())
}

#[tauri::command]
pub fn set_plan(
    app: AppHandle,
    state: State<'_, Mutex<AppState>>,
    plan: String,
) -> Result<SettingsView, String> {
    let updated = update_settings(&state, |s| s.plan = plan)?;
    settings::save(&app, &updated).into_string()?;
    Ok((&updated).into())
}

#[tauri::command]
pub fn set_glm_endpoint(
    app: AppHandle,
    state: State<'_, Mutex<AppState>>,
    endpoint: String,
) -> Result<SettingsView, String> {
    let updated = update_settings(&state, |s| s.glm_endpoint = endpoint)?;
    settings::save(&app, &updated).into_string()?;
    Ok((&updated).into())
}

/// Encrypt and store an API key. `provider` is "glm" (or "zai") or "anthropic".
#[tauri::command]
pub fn set_api_key(
    app: AppHandle,
    state: State<'_, Mutex<AppState>>,
    provider: String,
    key: String,
) -> Result<SettingsView, String> {
    let trimmed = key.trim();
    if trimmed.is_empty() {
        return Err("key is empty".to_string());
    }
    let secret = encryption::encrypt(trimmed).into_string()?;
    let updated = match provider.as_str() {
        "glm" | "zai" => update_settings(&state, |s| s.zai_key = Some(secret))?,
        "anthropic" => update_settings(&state, |s| s.anthropic_key = Some(secret))?,
        other => return Err(format!("unknown provider: {other}")),
    };
    settings::save(&app, &updated).into_string()?;
    Ok((&updated).into())
}

/// Remove a stored API key.
#[tauri::command]
pub fn clear_api_key(
    app: AppHandle,
    state: State<'_, Mutex<AppState>>,
    provider: String,
) -> Result<SettingsView, String> {
    let updated = match provider.as_str() {
        "glm" | "zai" => update_settings(&state, |s| s.zai_key = None)?,
        "anthropic" => update_settings(&state, |s| s.anthropic_key = None)?,
        other => return Err(format!("unknown provider: {other}")),
    };
    settings::save(&app, &updated).into_string()?;
    Ok((&updated).into())
}

fn update_settings(
    state: &State<'_, Mutex<AppState>>,
    mutate: impl FnOnce(&mut Settings),
) -> Result<Settings, String> {
    let mut guard = state.lock().map_err(|e| e.to_string())?;
    mutate(&mut guard.settings);
    Ok(guard.settings.clone())
}
