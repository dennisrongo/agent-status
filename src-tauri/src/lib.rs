pub mod commands;
pub mod encryption;
pub mod error;
pub mod scanner;
pub mod settings;
pub mod state;
pub mod storage;
pub mod tray;
pub mod vendors;

use std::sync::Mutex;
use std::time::Duration;

use tauri::{Emitter, Manager};
use tauri_plugin_autostart::{ManagerExt, MacosLauncher};
use tracing::Level;
use tracing_subscriber::EnvFilter;

use crate::state::AppState;

pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::default().add_directive(Level::INFO.into())),
        )
        .init();

    tauri::Builder::default()
        // Single-instance MUST be registered first so a second launch focuses
        // the existing window instead of spawning a rival process.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
                tray::refresh_on_open(app);
            }
        }))
        .plugin(tauri_plugin_positioner::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec![]),
        ))
        .setup(|app| {
            let handle = app.handle().clone();

            // Load settings and seed managed state.
            let loaded = settings::load(&handle);
            app.manage(Mutex::new(AppState::new(loaded)));
            app.manage(crate::state::CollectLock::default());

            // Tray + dropdown.
            tray::build(&handle)?;

            // Launch at login (menubar widgets are expected to persist).
            let _ = app.autolaunch().enable();

            // Background refresh loop: re-scan the logs on an interval and push
            // the fresh snapshot to the frontend.
            let bg = handle.clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    // Only poll while the dropdown is open — no background API
                    // calls when the window is hidden. Opening the window
                    // triggers its own immediate refresh (see tray.rs).
                    let visible = bg
                        .get_webview_window("main")
                        .and_then(|w| w.is_visible().ok())
                        .unwrap_or(false);
                    if visible {
                        match commands::usage::collect(&bg).await {
                            Ok(snapshot) => {
                                let _ = bg.emit("usage-updated", &snapshot);
                            }
                            Err(e) => tracing::warn!("background refresh failed: {e}"),
                        }
                    }
                    // Re-read the interval each tick so changes from Settings
                    // take effect on the next cycle without a restart.
                    let secs = bg
                        .state::<Mutex<AppState>>()
                        .lock()
                        .map(|g| g.settings.refresh_secs)
                        .unwrap_or(30)
                        .clamp(settings::MIN_REFRESH_SECS, settings::MAX_REFRESH_SECS);
                    tokio::time::sleep(Duration::from_secs(secs)).await;
                }
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_usage,
            commands::get_settings,
            commands::set_plan,
            commands::set_live_claude,
            commands::set_refresh_secs,
            commands::set_glm_endpoint,
            commands::set_api_key,
            commands::clear_api_key,
        ])
        .run(tauri::generate_context!())
        .expect("error while running agent-status");
}
