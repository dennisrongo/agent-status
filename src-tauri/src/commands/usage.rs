//! Usage commands: scan logs + fetch live vendor data, manage plan + API keys.

use std::sync::Mutex;

use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_autostart::ManagerExt;

use crate::encryption::{self, EncryptedSecret};
use crate::error::ResultExt;
use crate::scanner::{self, UsageSnapshot};
use crate::settings::{self, Settings, SettingsView};
use crate::state::AppState;
use crate::vendors::{anthropic, claude, copilot, glm, Detection, VendorReport, VendorStatus};

/// The user code + verification URL the UI shows during a Copilot device-flow
/// connect. The device code itself stays server-side in `AppState`.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CopilotDeviceCode {
    pub user_code: String,
    pub verification_uri: String,
    pub interval: u64,
}

/// What the UI shows when an in-app Claude OAuth login starts: the authorize URL
/// (already opened in the browser, repeated here so the UI can offer a manual
/// "open again" link). The PKCE verifier + state stay server-side in `AppState`.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeLoginInfo {
    pub authorize_url: String,
}

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
    let (plan, glm_endpoint, zai_key, anthropic_key, copilot_token, live_claude, cached_live, mut live_due, refresh_due) = {
        let state = app.state::<Mutex<AppState>>();
        let guard = state.lock().map_err(|e| e.to_string())?;
        // Only hit the rate-limited /usage endpoint once per LIVE_CLAUDE_MIN_SECS,
        // even though the log scan refreshes more often. Between fetches we serve
        // the cached live meters.
        let live_due = guard.live_claude_attempted_at.is_none_or(|t| {
            (now - t).num_seconds() >= crate::state::LIVE_CLAUDE_MIN_SECS
        });
        // Independently throttle automatic token refreshes so a dead refresh
        // token can't be retried against the rate-limited token endpoint on every
        // tick while the window stays open.
        let refresh_due = guard.live_claude_refresh_attempted_at.is_none_or(|t| {
            (now - t).num_seconds() >= crate::state::LIVE_CLAUDE_REFRESH_MIN_SECS
        });
        (
            guard.settings.plan.clone(),
            guard.settings.glm_endpoint.clone(),
            guard.settings.zai_key.clone(),
            guard.settings.anthropic_key.clone(),
            guard.settings.copilot_token.clone(),
            guard.settings.live_claude,
            guard.live_claude_buckets.clone(),
            live_due,
            refresh_due,
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
    let mut refresh_attempted = false;
    // Token presence/expiry, read once: the live block uses it, and so do
    // detection + the connect/reconnect controls (which must reflect the login
    // state regardless of the live toggle). Re-read after a successful auto-
    // refresh below so detection reports the post-refresh (fresh) state.
    let mut claude_ts = claude::token_status(now);
    let mut refreshed = false;
    if live_claude {
        // Detect a dead login up front, by the token's own clock, before any
        // network call. This is what stops us from serving the last cached
        // reading as "live" for up to the throttle window after the token has
        // already expired (the reported "stale data"). If a refresh token is on
        // hand we renew in place automatically; otherwise we surface a reconnect
        // prompt — and once the login is known-dead we never present the cached
        // meters again.
        let ts = &claude_ts;
        let mut force_reauth: Option<String> = None;
        if ts.present && ts.expired {
            if ts.has_refresh && refresh_due {
                refresh_attempted = true;
                match claude::refresh(now).await {
                    // Renewed in place — fetch fresh data with the new token now,
                    // bypassing the usage throttle this once.
                    Ok(()) => {
                        live_due = true;
                        refreshed = true;
                    }
                    Err(e) => force_reauth = Some(e),
                }
            } else if ts.has_refresh {
                // Refreshed too recently to retry the rate-limited token endpoint.
                force_reauth = Some("Claude login expired — reconnecting shortly…".to_string());
            } else {
                force_reauth =
                    Some("Claude login expired — sign in again to restore live usage.".to_string());
            }
        }

        if let Some(reason) = force_reauth {
            // Known-dead by the clock (and any auto-refresh failed). Surface a
            // reconnect prompt and DON'T serve the cached live meters — they'd be
            // stale. The scanner's local estimate stays in `buckets`, but the UI
            // hides it behind the reconnect card while `needs_reauth` is set.
            snapshot.limits.needs_reauth = true;
            snapshot.limits.can_refresh = ts.has_refresh;
            snapshot.limits.estimate_note = reason;
        } else if live_due {
            live_attempted = true;
            let live = claude::fetch(now).await;
            if live.expired {
                // The clock looked valid but the server rejected the token (401):
                // revoked elsewhere. Prompt a reconnect; don't show stale cache.
                snapshot.limits.needs_reauth = true;
                snapshot.limits.can_refresh = ts.has_refresh;
                snapshot.limits.estimate_note =
                    "Claude login expired — reconnect to restore live usage.".to_string();
            } else if live.ok && !live.buckets.is_empty() {
                snapshot.limits.buckets = live.buckets.clone();
                snapshot.limits.plan_label = "live".to_string();
                snapshot.limits.estimate_note = LIVE_NOTE.to_string();
                snapshot.limits.live = true;
                fresh_live = Some(live.buckets);
            } else if let Some(cached) = cached_live {
                // Live refresh failed (the /usage endpoint rate-limits hard when
                // polled), but the token is NOT expired — reuse the last good live
                // reading rather than swapping in the local estimate, which is on
                // a different scale and would make the meters flip-flop.
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
                snapshot.limits.signed_out = true;
                if let Some(err) = live.error {
                    snapshot.limits.estimate_note = format!(
                        "Showing local estimate — couldn’t read live Claude usage ({err}). Limits are against an editable plan ceiling."
                    );
                }
            }
        } else if let Some(cached) = cached_live {
            // Within the throttle window and the token is clock-valid — serve the
            // cached live meters instead of re-hitting the rate-limited endpoint.
            snapshot.limits.buckets = cached;
            snapshot.limits.plan_label = "live".to_string();
            snapshot.limits.live = true;
            snapshot.limits.estimate_note = LIVE_NOTE.to_string();
        } else if ts.present {
            // Throttled before the first reading, but a login exists → live data
            // is still coming. Show pending rather than the estimate.
            snapshot.limits.pending = true;
            snapshot.limits.estimate_note = "Reading live Claude usage…".to_string();
        } else {
            // Throttled, no cached reading, and no login → local estimate.
            snapshot.limits.signed_out = true;
        }
    }

    // A successful auto-refresh above rewrote the credential, so the token status
    // read at the top is now stale — re-read it so detection reports the fresh
    // (not-expired) state instead of flagging a just-renewed login as expired.
    if refreshed {
        claude_ts = claude::token_status(now);
    }

    // Live vendor fetches (network, async).
    let glm_status = fetch_glm(zai_key, &glm_endpoint).await;
    let anthropic_status = fetch_anthropic(anthropic_key).await;
    let copilot_status = fetch_copilot(copilot_token).await;

    // Decide which provider tabs to show. Claude can be detected locally (login
    // token / session logs / CLI on PATH); GLM has no readable local credential,
    // so it's only present once the API key is set in settings. Copilot is
    // present once a token is found locally or connected in-app — `configured`
    // captures both without a second token read.
    snapshot.detection = Some(Detection {
        claude: claude_ts.present || claude::cli_on_path() || snapshot.meta.files_scanned > 0,
        glm: glm_status.configured,
        copilot: copilot_status.configured,
        claude_signed_in: claude_ts.present,
        claude_expired: claude_ts.expired,
    });

    snapshot.vendor = Some(VendorReport {
        glm: glm_status,
        anthropic: anthropic_status,
        copilot: copilot_status,
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
        if refresh_attempted {
            guard.live_claude_refresh_attempted_at = Some(now);
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

/// Copilot reads a locally-discovered token by default; a token connected
/// in-app (device flow) is decrypted and preferred when present.
async fn fetch_copilot(token: Option<EncryptedSecret>) -> VendorStatus {
    let stored = match token {
        None => None,
        Some(secret) => match encryption::decrypt(&secret) {
            Ok(tok) => Some(tok),
            Err(e) => return VendorStatus::failed(format!("token decrypt: {e}")),
        },
    };
    copilot::fetch(stored).await
}

#[tauri::command]
pub async fn get_usage(app: AppHandle) -> Result<UsageSnapshot, String> {
    let snapshot = collect(&app).await?;
    let _ = app.emit("usage-updated", &snapshot);
    Ok(snapshot)
}

/// Begin an in-app Claude OAuth login (authorization-code + PKCE, manual
/// copy-paste). Mints a fresh PKCE verifier + state, opens the authorize page in
/// the browser, and returns the URL so the UI can re-open it. The user pastes the
/// resulting `CODE#STATE` into `claude_login_finish`.
#[tauri::command]
pub fn claude_login_start(app: AppHandle) -> Result<ClaudeLoginInfo, String> {
    let auth = claude::build_authorize();
    {
        let state = app.state::<Mutex<AppState>>();
        let mut guard = state.lock().map_err(|e| e.to_string())?;
        guard.pending_claude_login = Some(crate::state::PendingClaudeLogin {
            verifier: auth.verifier,
            state: auth.state,
            // The authorize code is short-lived; bound the pending login to ~10
            // minutes so a stale verifier can't be reused much later.
            expires_at: chrono::Utc::now() + chrono::Duration::minutes(10),
        });
    }
    // Best-effort browser open; the UI also shows the link to open manually.
    let _ = open_url(auth.url.clone());
    Ok(ClaudeLoginInfo { authorize_url: auth.url })
}

/// Finish an in-app Claude OAuth login: exchange the pasted `CODE#STATE` for
/// tokens, write them to Claude Code's credential store, and re-collect so live
/// usage comes back. Returns the refreshed snapshot.
#[tauri::command]
pub async fn claude_login_finish(app: AppHandle, code: String) -> Result<UsageSnapshot, String> {
    let pending = {
        let state = app.state::<Mutex<AppState>>();
        let guard = state.lock().map_err(|e| e.to_string())?;
        guard.pending_claude_login.clone()
    };
    let Some(pending) = pending else {
        return Err("No Claude sign-in is in progress — start it again.".to_string());
    };
    if !pending.is_valid(chrono::Utc::now()) {
        clear_pending_claude_login(&app)?;
        return Err("The sign-in timed out — start it again.".to_string());
    }

    claude::exchange_code(chrono::Utc::now(), &code, &pending.verifier, &pending.state).await?;

    clear_pending_claude_login(&app)?;
    clear_claude_throttle(&app)?;
    let snapshot = collect(&app).await?;
    let _ = app.emit("usage-updated", &snapshot);
    Ok(snapshot)
}

/// Abandon an in-progress in-app Claude login, forgetting the PKCE secrets.
#[tauri::command]
pub fn claude_login_cancel(app: AppHandle) -> Result<(), String> {
    clear_pending_claude_login(&app)
}

fn clear_pending_claude_login(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<Mutex<AppState>>();
    let mut guard = state.lock().map_err(|e| e.to_string())?;
    guard.pending_claude_login = None;
    Ok(())
}

/// Clear the live-fetch + refresh throttles and the stale cache so the very next
/// collect re-hits the usage endpoint with a freshly-minted token instead of
/// serving a cached/pending state.
fn clear_claude_throttle(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<Mutex<AppState>>();
    let mut guard = state.lock().map_err(|e| e.to_string())?;
    guard.live_claude_attempted_at = None;
    guard.live_claude_refresh_attempted_at = None;
    guard.live_claude_buckets = None;
    Ok(())
}

/// Full Claude sign-out: delete the SHARED Claude Code credential (keychain +
/// file), forget any in-flight login, clear the live throttle/cache, and
/// re-collect. Because the credential is shared, this signs the `claude` CLI out
/// too — the user signs in again (here or via `claude /login`) to use either.
/// Returns the refreshed snapshot (now the local-estimate / signed-out state).
#[tauri::command]
pub async fn claude_sign_out(app: AppHandle) -> Result<UsageSnapshot, String> {
    // Credential removal does keychain/file I/O — run it off the async runtime.
    tokio::task::spawn_blocking(claude::clear_credentials)
        .await
        .map_err(|e| e.to_string())??;
    clear_pending_claude_login(&app)?;
    clear_claude_throttle(&app)?;
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

/// Apply (or deliberately skip) the OS launch-at-login registration.
///
/// No-op in dev builds: `tauri dev` runs the binary straight out of
/// `target/debug`, so registering autostart there would write a login item
/// pointing at that throwaway path — at the next login the OS would try to
/// launch a stale/dev binary, and on macOS every dev run would re-fire the
/// "added to Login Items / can run in the background" notification. Only
/// bundled/`tauri build` builds (where `is_dev()` is false) touch the OS; the
/// stored setting is untouched here, so the UI still reflects the user's choice.
pub fn apply_autostart(app: &AppHandle, enabled: bool) -> Result<(), String> {
    if tauri::is_dev() {
        return Ok(());
    }
    let autostart = app.autolaunch();
    if enabled {
        autostart.enable().map_err(|e| e.to_string())
    } else {
        autostart.disable().map_err(|e| e.to_string())
    }
}

/// Toggle launch-at-login. Registers/unregisters the OS launch agent, then
/// persists the choice. The registration is applied before saving so a failure
/// to update the OS leaves the stored setting untouched.
#[tauri::command]
pub fn set_launch_on_startup(
    app: AppHandle,
    state: State<'_, Mutex<AppState>>,
    enabled: bool,
) -> Result<SettingsView, String> {
    apply_autostart(&app, enabled)?;
    let updated = update_settings(&state, |s| s.launch_on_startup = enabled)?;
    settings::save(&app, &updated).into_string()?;
    Ok((&updated).into())
}

/// Toggle the compact "main stats only" Overview. Pure UI preference — no
/// rescan needed, the frontend just renders less and fits the window.
#[tauri::command]
pub fn set_minimal_view(
    app: AppHandle,
    state: State<'_, Mutex<AppState>>,
    enabled: bool,
) -> Result<SettingsView, String> {
    let updated = update_settings(&state, |s| s.minimal_view = enabled)?;
    settings::save(&app, &updated).into_string()?;
    Ok((&updated).into())
}

/// Choose which provider the tray hover popover previews ("claude" or "glm").
#[tauri::command]
pub fn set_tooltip_provider(
    app: AppHandle,
    state: State<'_, Mutex<AppState>>,
    provider: String,
) -> Result<SettingsView, String> {
    match provider.as_str() {
        "claude" | "glm" | "copilot" => {}
        other => return Err(format!("unknown provider: {other}")),
    }
    let updated = update_settings(&state, |s| s.tooltip_provider = provider)?;
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

/// Resize a tray window and, on Windows, re-pin it to the bottom-right corner of
/// its monitor's work area (flush above the taskbar). Both the resize and the
/// reposition happen here, in one synchronous command, on purpose: issuing
/// `setSize` then `setPosition` as two separate calls from the webview races
/// WebView2's IPC and the second op is frequently dropped — leaving the window
/// either mis-sized (stuck at the old height) or mis-placed (bottom off-screen).
/// Native `set_size`/`set_position` are sequential Win32 calls that both apply,
/// and with no paint between them the corner re-pin is invisible. macOS keeps the
/// plain top-anchored `setSize` in the frontend, so it never calls this.
#[tauri::command]
pub fn fit_tray_window(app: AppHandle, label: String, width: f64, height: f64) {
    let Some(win) = app.get_webview_window(&label) else {
        return;
    };
    // The frontend re-asserts the size on every open (when a fresh snapshot
    // lands), so most calls request a size the window already has. Re-running
    // set_size + re-pin in that case is exactly what made the window twitch on
    // open, so skip when the size is unchanged. inner_size() is what set_size
    // sets, so compare against it.
    #[cfg(target_os = "windows")]
    {
        let scale = win.scale_factor().unwrap_or(1.0).max(0.01);
        let target_w = (width * scale).round() as u32;
        let target_h = (height * scale).round() as u32;
        let cur = win.inner_size().unwrap_or_default();
        if cur.width.abs_diff(target_w) <= 2 && cur.height.abs_diff(target_h) <= 2 {
            return;
        }
    }
    let _ = win.set_size(tauri::LogicalSize::new(width, height));
    // Re-pin through the SAME helper the tray open path uses, so a genuine resize
    // keeps the bottom-right corner exactly where it was placed (no horizontal
    // drift). current_monitor reflects the display the window is on after the
    // open placement; fall back to primary if it can't be read mid-resize.
    #[cfg(target_os = "windows")]
    if let Some(mon) = win
        .current_monitor()
        .ok()
        .flatten()
        .or_else(|| win.primary_monitor().ok().flatten())
    {
        crate::tray::pin_bottom_right(&win, &mon);
    }
}

/// Open an http(s) URL in the user's default browser. Scheme-restricted so it
/// can't be misused as a generic process launcher.
#[tauri::command]
pub fn open_url(url: String) -> Result<(), String> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(format!("refusing to open non-http url: {url}"));
    }
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = std::process::Command::new("open");
        c.arg(&url);
        c
    };
    #[cfg(target_os = "windows")]
    let mut cmd = {
        // Hand the URL to the shell's protocol handler directly. NOT `cmd /C
        // start`, whose builtin re-interprets &, |, ^, <, >, %, () — rundll32
        // receives the URL as a single argv item, so query strings (`?a=1&b=2`)
        // pass through verbatim.
        let mut c = std::process::Command::new("rundll32");
        c.arg("url.dll,FileProtocolHandler").arg(&url);
        c
    };
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    let mut cmd = {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(&url);
        c
    };
    cmd.spawn().map(drop).map_err(|e| e.to_string())
}

/// Begin the Copilot device-flow connect: get a user code from GitHub, stash the
/// device code to poll against, and open the verification page. Returns the code
/// + URL for the UI to display.
#[tauri::command]
pub async fn copilot_device_start(app: AppHandle) -> Result<CopilotDeviceCode, String> {
    let now = chrono::Utc::now();

    // Reuse a still-valid in-flight authorization rather than minting a second
    // device code — overwriting it would orphan the first (its poller would then
    // hit "no connection in progress" forever).
    {
        let state = app.state::<Mutex<AppState>>();
        let guard = state.lock().map_err(|e| e.to_string())?;
        if let Some(p) = guard.pending_copilot_device.as_ref().filter(|p| p.is_valid(now)) {
            let view = CopilotDeviceCode {
                user_code: p.user_code.clone(),
                verification_uri: p.verification_uri.clone(),
                interval: p.interval,
            };
            let complete = p.verification_uri_complete.clone();
            drop(guard);
            // Re-open the page (the user may have lost the tab).
            let _ = open_url(complete);
            return Ok(view);
        }
    }

    let start = copilot::device_start().await?;
    // `expires_in` is untrusted JSON; clamp it and build the deadline without
    // panicking. `Duration::seconds`/`DateTime + Duration` panic on overflow,
    // and with `panic = "abort"` that would take down the whole app — so cap to
    // a day and fall back to 15 min if the arithmetic ever can't represent it.
    let expires_at = chrono::Duration::try_seconds(start.expires_in.min(86_400) as i64)
        .and_then(|d| now.checked_add_signed(d))
        .unwrap_or_else(|| now + chrono::Duration::seconds(900));
    {
        let state = app.state::<Mutex<AppState>>();
        let mut guard = state.lock().map_err(|e| e.to_string())?;
        guard.pending_copilot_device = Some(crate::state::PendingDevice {
            device_code: start.device_code,
            user_code: start.user_code.clone(),
            verification_uri: start.verification_uri.clone(),
            verification_uri_complete: start.verification_uri_complete.clone(),
            interval: start.interval,
            expires_at,
        });
    }
    // Best-effort browser open with the code pre-filled; the UI also shows the
    // bare link + code to enter manually.
    let _ = open_url(start.verification_uri_complete);
    Ok(CopilotDeviceCode {
        user_code: start.user_code,
        verification_uri: start.verification_uri,
        interval: start.interval,
    })
}

/// Poll once for the Copilot device-flow token. Returns one of "pending",
/// "slow_down", "connected", "denied", "expired". On "connected" the token is
/// stored encrypted and a fresh snapshot is emitted.
#[tauri::command]
pub async fn copilot_device_poll(app: AppHandle) -> Result<String, String> {
    let pending = {
        let state = app.state::<Mutex<AppState>>();
        let guard = state.lock().map_err(|e| e.to_string())?;
        guard.pending_copilot_device.clone()
    };
    let Some(pending) = pending else {
        return Err("no Copilot connection in progress".to_string());
    };
    if !pending.is_valid(chrono::Utc::now()) {
        clear_pending_copilot(&app)?;
        return Ok("expired".to_string());
    }

    // Transient failures are mapped to `Pending` inside `device_poll`; an `Err`
    // here is a terminal OAuth protocol error, so the device code is unusable —
    // clear it (don't leak it) before surfacing the failure.
    let outcome = match copilot::device_poll(&pending.device_code).await {
        Ok(o) => o,
        Err(e) => {
            clear_pending_copilot(&app)?;
            return Err(e);
        }
    };

    match outcome {
        copilot::PollOutcome::Pending => Ok("pending".to_string()),
        copilot::PollOutcome::SlowDown => Ok("slow_down".to_string()),
        copilot::PollOutcome::Denied => {
            clear_pending_copilot(&app)?;
            Ok("denied".to_string())
        }
        copilot::PollOutcome::Expired => {
            clear_pending_copilot(&app)?;
            Ok("expired".to_string())
        }
        copilot::PollOutcome::Connected(token) => {
            let secret = encryption::encrypt(&token).into_string()?;
            // Persist to disk FIRST. If the save fails we leave the in-memory
            // settings and the pending device untouched, so the token isn't lost
            // and the still-valid device code can be retried — rather than
            // half-committing state that won't survive a restart.
            let to_save = {
                let state = app.state::<Mutex<AppState>>();
                let guard = state.lock().map_err(|e| e.to_string())?;
                let mut s = guard.settings.clone();
                s.copilot_token = Some(secret.clone());
                s
            };
            save_settings_async(&app, to_save).await?;
            {
                let state = app.state::<Mutex<AppState>>();
                let mut guard = state.lock().map_err(|e| e.to_string())?;
                guard.settings.copilot_token = Some(secret);
                guard.pending_copilot_device = None;
            }
            let snapshot = collect(&app).await?;
            let _ = app.emit("usage-updated", &snapshot);
            Ok("connected".to_string())
        }
    }
}

/// Forget the in-app Copilot token (usage falls back to a locally-discovered
/// token, if any).
#[tauri::command]
pub async fn disconnect_copilot(app: AppHandle) -> Result<SettingsView, String> {
    let updated = {
        let state = app.state::<Mutex<AppState>>();
        let mut guard = state.lock().map_err(|e| e.to_string())?;
        guard.settings.copilot_token = None;
        guard.pending_copilot_device = None;
        guard.settings.clone()
    };
    let view: SettingsView = (&updated).into();
    save_settings_async(&app, updated).await?;
    let snapshot = collect(&app).await?;
    let _ = app.emit("usage-updated", &snapshot);
    Ok(view)
}

/// Persist settings off the async runtime — `settings::save` does a blocking
/// file write, so async command paths hand it to `spawn_blocking` (the
/// synchronous command handlers already run on Tauri's blocking pool and call
/// `settings::save` directly).
async fn save_settings_async(app: &AppHandle, to_save: Settings) -> Result<(), String> {
    let app = app.clone();
    tokio::task::spawn_blocking(move || settings::save(&app, &to_save))
        .await
        .map_err(|e| e.to_string())?
        .into_string()
}

/// Abandon an in-progress device-flow connect, forgetting the pending code so
/// the next connect mints a fresh one (instead of re-handing the user a code
/// they already dismissed — e.g. after logging into the wrong account).
#[tauri::command]
pub fn copilot_device_cancel(app: AppHandle) -> Result<(), String> {
    clear_pending_copilot(&app)
}

fn clear_pending_copilot(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<Mutex<AppState>>();
    let mut guard = state.lock().map_err(|e| e.to_string())?;
    guard.pending_copilot_device = None;
    Ok(())
}

fn update_settings(
    state: &State<'_, Mutex<AppState>>,
    mutate: impl FnOnce(&mut Settings),
) -> Result<Settings, String> {
    let mut guard = state.lock().map_err(|e| e.to_string())?;
    mutate(&mut guard.settings);
    Ok(guard.settings.clone())
}
