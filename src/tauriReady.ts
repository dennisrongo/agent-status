// Guard for code that may run before the Tauri IPC bridge is wired.
export function isTauriReady(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}
