import { invoke } from "@tauri-apps/api/core";
import { LogicalSize, type Window } from "@tauri-apps/api/window";

// Reliable in the webview without the @tauri-apps/plugin-os dependency. Used to
// flip the tray-window anchor: macOS hangs windows down from the top menu bar,
// Windows grows them up from the bottom taskbar.
export const isWindows =
  typeof navigator !== "undefined" && navigator.userAgent.includes("Windows");

/**
 * Resize a tray window to a new height, fitting its content.
 *
 * On macOS the window is anchored top-under-the-menu-bar, so the OS default
 * (top-left fixed, grows downward) is exactly right — a plain setSize.
 *
 * On Windows the window is anchored bottom-on-the-taskbar (see place_window in
 * tray.rs), so the resize must keep the bottom edge pinned. We hand that to the
 * `fit_tray_window` Rust command, which resizes AND re-pins the bottom-right
 * corner in one synchronous step. Doing it as two webview calls (setSize then
 * setPosition) races WebView2's IPC — the second op is frequently dropped,
 * which left the window stuck at the wrong size or position.
 *
 * In float mode the window is free-floating (user-dragged), so both platforms
 * use a plain setSize — no re-pinning to any corner.
 */
export async function fitWindowHeight(
  win: Window,
  width: number,
  height: number,
  floating = false,
): Promise<void> {
  if (floating || !isWindows) {
    await win.setSize(new LogicalSize(width, height));
    return;
  }
  await invoke("fit_tray_window", { label: win.label, width, height });
}
