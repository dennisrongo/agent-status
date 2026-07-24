import { useCallback, useEffect, useState } from "react";
import { check as checkForUpdate, type Update } from "@tauri-apps/plugin-updater";
import { invoke } from "@tauri-apps/api/core";

// How often a running app re-checks the update endpoint after its initial
// mount-time check. Long enough to avoid pointless polling, short enough that
// a release published while the app is open surfaces within a few hours.
const UPDATE_INTERVAL_MS = 4 * 60 * 60 * 1000; // 4 hours

type Phase =
  | "idle"
  | "checking"
  | "uptodate"
  | "available"
  | "downloading"
  | "ready"
  | "error";

interface UpdaterState {
  phase: Phase;
  version: string | null;
  error: string | null;
}

/**
 * Update checking against the configured endpoint.
 *
 * `auto: true` (default) checks once on mount and stays silent on failure —
 * suitable for the passive banner. `auto: false` only checks when `check()` is
 * called, and surfaces errors / an explicit "up to date" result — suitable for
 * the manual button in Settings. `install()` downloads, applies, and relaunches.
 */
export function useUpdater(opts: { auto?: boolean } = {}) {
  const { auto = true } = opts;
  const [state, setState] = useState<UpdaterState>({
    phase: "idle",
    version: null,
    error: null,
  });
  const [update, setUpdate] = useState<Update | null>(null);

  const check = useCallback(async () => {
    setState({ phase: "checking", version: null, error: null });
    try {
      const found = await checkForUpdate();
      if (found) {
        setUpdate(found);
        setState({ phase: "available", version: found.version, error: null });
      } else {
        setState({ phase: "uptodate", version: null, error: null });
      }
    } catch (e) {
      const error = e instanceof Error ? e.message : String(e);
      setState({ phase: "error", version: null, error });
    }
  }, []);

  useEffect(() => {
    if (!auto) return;
    let cancelled = false;

    // Silent auto-check: a passive banner shouldn't show dev/offline errors.
    // Runs once on mount and again every UPDATE_INTERVAL_MS, so a long-running
    // app learns about a release published after it started. Each definitive
    // result (available / up-to-date) also toggles the tray-icon badge via the
    // Rust command; transient failures are swallowed so a brief offline blip
    // doesn't drop the indicator.
    const run = () => {
      checkForUpdate()
        .then((found) => {
          if (cancelled) return;
          if (found) {
            setUpdate(found);
            setState({ phase: "available", version: found.version, error: null });
          } else {
            setState({ phase: "uptodate", version: null, error: null });
          }
          invoke("set_update_available", { available: !!found });
        })
        .catch(() => {});
    };
    run();
    const id = setInterval(run, UPDATE_INTERVAL_MS);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [auto]);

  const install = useCallback(async () => {
    if (!update) return;
    try {
      setState((s) => ({ ...s, phase: "downloading" }));
      await update.downloadAndInstall();
      setState((s) => ({ ...s, phase: "ready" }));
      // Relaunch via our own command, which clears the single-instance lock
      // before spawning the new binary — the plugin's restart races the lock
      // on macOS and the relaunched child silently exits as a "duplicate".
      await invoke("restart_after_update");
    } catch (e) {
      const error = e instanceof Error ? e.message : String(e);
      setState((s) => ({ ...s, phase: "error", error }));
    }
  }, [update]);

  return { ...state, check, install };
}
