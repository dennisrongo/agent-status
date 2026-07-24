//! Relaunch the app after an auto-update.
//!
//! The Tauri process plugin's `restart` (what `relaunch()` from
//! `@tauri-apps/plugin-process` calls) spawns the new binary and then exits the
//! old process. On macOS that races `tauri-plugin-single-instance`: the freshly
//! spawned binary runs its single-instance setup and calls
//! `UnixStream::connect()` on the lock socket while the dying parent's listener
//! can still be accepting connections. The connect succeeds, so the new binary
//! treats itself as a duplicate instance and **silently `exit(0)`s** — leaving
//! no process running after an update.
//!
//! This command fixes the race by controlling the ordering ourselves: remove
//! the single-instance socket file *first*, then spawn the new binary detached,
//! then exit. With the socket gone, the child's `connect()` fails with
//! `ENOENT`/`ConnectionRefused` (as it does on a normal cold start) and it
//! claims the singleton instead of bailing out.
//!
//! On Windows the single-instance plugin uses a named pipe rather than a socket
//! file, so the socket removal is a no-op there — the spawn+exit still applies.

use std::path::PathBuf;
use std::process::Command;

use tauri::{Manager, Runtime};

/// Compute the single-instance lock socket path the same way
/// `tauri-plugin-single-instance` does on macOS/Linux: the app identifier with
/// `.` and `-` rewritten to `_`, suffixed with `_si.sock` under `/tmp`.
///
/// Extracted as a pure function so the path computation — the exact thing that
/// must match the plugin for the fix to work — can be unit-tested in isolation.
fn single_instance_socket_path(identifier: &str) -> PathBuf {
    let normalized = identifier.replace(['.', '-'], "_");
    PathBuf::from(format!("/tmp/{normalized}_si.sock"))
}

/// Remove the single-instance lock socket so the relaunched child doesn't
/// detect the still-shutting-down parent and silently exit as a "duplicate".
/// No-op on platforms that don't use a socket file (e.g. Windows named pipes).
fn clear_single_instance_lock<R: Runtime>(app: &tauri::AppHandle<R>) {
    #[cfg(unix)]
    {
        let identifier = app.config().identifier.clone();
        let socket = single_instance_socket_path(&identifier);
        // Best-effort: if it's already gone (clean shutdown) this is a no-op.
        if let Err(e) = std::fs::remove_file(&socket) {
            tracing::debug!("single-instance socket {:?} not removed: {e}", socket);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = app;
    }
}

/// Resolve the executable to relaunch. This is the current running binary —
/// after an auto-update it already points at the freshly installed bundle's
/// executable (the updater replaces the `.app` in place before we're invoked).
/// Our binary name is fixed by the Cargo `lib.name` (`agent-status`) and does
/// not change across versions, so `current_binary()` is sufficient and we avoid
/// pulling in `plist` just to re-read `CFBundleExecutable`.
fn relaunch_target(env: &tauri::Env) -> Result<PathBuf, String> {
    tauri::process::current_binary(env).map_err(|e| e.to_string())
}

/// IPC command invoked from the frontend after `update.downloadAndInstall()`
/// resolves. Removes the single-instance lock, spawns the new binary detached,
/// and exits the current process — in that strict order so the relaunched child
/// sees no stale singleton and comes up cleanly.
#[tauri::command]
pub fn restart_after_update<R: Runtime>(app: tauri::AppHandle<R>) -> Result<(), String> {
    // 1. Drop the single-instance lock so the child's connect() finds nothing.
    clear_single_instance_lock(&app);

    // 2. Spawn the new binary fully detached.
    let target = relaunch_target(&app.env())?;
    if let Err(e) = Command::new(&target)
        .args(app.env().args_os.iter().skip(1))
        .spawn()
    {
        tracing::error!("failed to relaunch {}: {e}", target.display());
        return Err(format!("failed to relaunch: {e}"));
    }

    // 3. Exit the current (old) process. The detached child now becomes the
    //    singleton. We use request_restart's exit so Tauri runs its normal
    //    Exit-event cleanup (tray teardown, etc.) — but crucially we already
    //    cleared the lock, so the post-exit spawn path can't reintroduce the
    //    race. Using app.exit (not request_restart) avoids spawning twice.
    app.exit(0);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression test for the path that must exactly match
    /// `tauri-plugin-single-instance`'s macOS/Linux socket location. If this
    /// path is wrong, the fix silently does nothing (wrong file removed) and
    /// the relaunch race returns.
    #[test]
    fn single_instance_socket_path_matches_plugin() {
        let path = single_instance_socket_path("com.dennisrongo.agentstatus");
        assert_eq!(
            path,
            PathBuf::from("/tmp/com_dennisrongo_agentstatus_si.sock")
        );
    }

    /// The plugin rewrites both `.` and `-` to `_`. Hyphenated identifiers
    /// must normalize the same way or the lock wouldn't be cleared.
    #[test]
    fn single_instance_socket_path_normalizes_dashes() {
        let path = single_instance_socket_path("com.example.my-app");
        assert_eq!(
            path,
            PathBuf::from("/tmp/com_example_my_app_si.sock")
        );
    }
}
