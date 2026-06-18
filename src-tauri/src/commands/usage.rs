//! Usage commands: scan logs + fetch live vendor data, manage plan + API keys.

use std::sync::Mutex;

use tauri::{AppHandle, Emitter, Manager, State};

use crate::encryption::{self, EncryptedSecret};
use crate::error::ResultExt;
use crate::scanner::{self, UsageSnapshot};
use crate::settings::{self, Settings, SettingsView};
use crate::state::AppState;
use crate::vendors::{anthropic, claude, glm, Detection, VendorReport, VendorStatus};

/// Scan local logs AND fetch live vendor usage, merge into one snapshot, and
/// cache it in state. Shared by the `get_usage` command and the background
/// refresh timer.
pub async fn collect(app: &AppHandle) -> Result<UsageSnapshot, String> {
    // Serialize collects. On open, `refresh_on_open` and the frontend's
    // `get_usage` fire near-simultaneously; without this they both race the
    // rate-limited live endpoint and emit conflicting (estimate vs live)
    // snapshots. Holding this lock makes the second caller observe the first's
    // throttle/cached result and stay consistent.
    let collect_lock = app.state::<crate::state::CollectLock>();
    let _serialized = collect_lock.0.lock().await;

    let now = chrono::Utc::now();
    let (plan, glm_endpoint, zai_key, anthropic_key, live_claude, cached_live, live_due) = {
        let state = app.state::<Mutex<AppState>>();
        let guard = state.lock().map_err(|e| e.to_string())?;
        // Only hit the rate-limited /usage endpoint once per LIVE_CLAUDE_MIN_SECS,
        // even though the log scan refreshes more often. Between fetches we serve
        // the cached live meters.
        let live_due = guard.live_claude_attempted_at.is_none_or(|t| {
            (now - t).num_seconds() >= crate::state::LIVE_CLAUDE_MIN_SECS
        });
        (
            guard.settings.plan.clone(),
            guard.settings.glm_endpoint.clone(),
            guard.settings.zai_key.clone(),
            guard.settings.anthropic_key.clone(),
            guard.settings.live_claude,
            guard.live_claude_buckets.clone(),
            live_due,
        )
    };

    // Blocking file scan off the IPC runtime.
    let mut snapshot = tokio::task::spawn_blocking(move || scanner::scan_default(&plan))
        .await
        .map_err(|e| e.to_string())?
        .into_string()?;

    // Replace the estimated Claude meters with live subscription usage when
    // enabled and a Claude Code token is available.
    const LIVE_NOTE: &str = "Live from Claude — the same session / weekly utilization your /usage shows, read from your Claude Code login.";
    let mut fresh_live: Option<Vec<crate::scanner::Bucket>> = None;
    let mut live_attempted = false;
    if live_claude {
        if live_due {
            live_attempted = true;
            let live = claude::fetch(now).await;
            if live.ok && !live.buckets.is_empty() {
                snapshot.limits.buckets = live.buckets.clone();
                snapshot.limits.plan_label = "live".to_string();
                snapshot.limits.estimate_note = LIVE_NOTE.to_string();
                snapshot.limits.live = true;
                fresh_live = Some(live.buckets);
            } else if let Some(cached) = cached_live {
                // Live refresh failed (the /usage endpoint rate-limits hard when
                // polled). Reuse the last good live reading rather than swapping
                // in the local estimate — otherwise the meters flip between two
                // scales.
                snapshot.limits.buckets = cached;
                snapshot.limits.plan_label = "live".to_string();
                snapshot.limits.live = true;
                let reason = live.error.unwrap_or_else(|| "temporarily unavailable".to_string());
                snapshot.limits.estimate_note = format!(
                    "Live from Claude (last good reading) — couldn’t refresh just now ({reason})."
                );
            } else if live.configured {
                // A Claude login exists but the live read failed and we have no
                // prior reading. Don't fall back to the wrong-scale estimate —
                // show a pending state so the UI is either accurate or blank.
                let reason = live.error.unwrap_or_else(|| "temporarily unavailable".to_string());
                snapshot.limits.pending = true;
                snapshot.limits.estimate_note =
                    format!("Reading live Claude usage… (couldn’t reach it just now: {reason})");
            } else {
                // No Claude login at all → live can never work; the local
                // estimate is the legitimate, clearly-labeled fallback.
                if let Some(err) = live.error {
                    snapshot.limits.estimate_note = format!(
                        "Showing local estimate — couldn’t read live Claude usage ({err}). Limits are against an editable plan ceiling."
                    );
                }
            }
        } else if let Some(cached) = cached_live {
            // Within the throttle window — serve the cached live meters instead
            // of re-hitting the rate-limited endpoint.
            snapshot.limits.buckets = cached;
            snapshot.limits.plan_label = "live".to_string();
            snapshot.limits.live = true;
            snapshot.limits.estimate_note = LIVE_NOTE.to_string();
        } else if claude::read_token().is_some() {
            // Throttled before the first reading, but a login exists → live data
            // is still coming. Show pending rather than the estimate.
            snapshot.limits.pending = true;
            snapshot.limits.estimate_note = "Reading live Claude usage…".to_string();
        }
        // else: no login → keep the local estimate.
    }

    // Live vendor fetches (network, async).
    let glm_status = fetch_glm(zai_key, &glm_endpoint).await;
    let anthropic_status = fetch_anthropic(anthropic_key).await;

    // Decide which provider tabs to show. Claude can be detected locally (login
    // token / session logs / CLI on PATH); GLM has no readable local credential,
    // so it's only present once the API key is set in settings.
    snapshot.detection = Some(Detection {
        claude: claude::detected() || snapshot.meta.files_scanned > 0,
        glm: glm_status.configured,
    });

    snapshot.vendor = Some(VendorReport {
        glm: glm_status,
        anthropic: anthropic_status,
    });

    {
        let state = app.state::<Mutex<AppState>>();
        let mut guard = state.lock().map_err(|e| e.to_string())?;
        guard.snapshot = Some(snapshot.clone());
        if let Some(buckets) = fresh_live {
            guard.live_claude_buckets = Some(buckets);
        }
        if live_attempted {
            guard.live_claude_attempted_at = Some(now);
        }
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

/// Toggle live Claude usage (reads the Claude Code OAuth token).
#[tauri::command]
pub fn set_live_claude(
    app: AppHandle,
    state: State<'_, Mutex<AppState>>,
    enabled: bool,
) -> Result<SettingsView, String> {
    let updated = update_settings(&state, |s| s.live_claude = enabled)?;
    settings::save(&app, &updated).into_string()?;
    Ok((&updated).into())
}

/// Update the auto-refresh interval (seconds), clamped to a sane range.
#[tauri::command]
pub fn set_refresh_secs(
    app: AppHandle,
    state: State<'_, Mutex<AppState>>,
    secs: u64,
) -> Result<SettingsView, String> {
    let clamped = secs.clamp(settings::MIN_REFRESH_SECS, settings::MAX_REFRESH_SECS);
    let updated = update_settings(&state, |s| s.refresh_secs = clamped)?;
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
