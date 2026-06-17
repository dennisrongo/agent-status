//! Shared error helpers. Internal APIs use `thiserror` enums; only the
//! `#[tauri::command]` boundary converts to `Result<T, String>`.

/// Convert any `Result<T, E: Display>` into `Result<T, String>` at the IPC boundary.
pub trait ResultExt<T> {
    fn into_string(self) -> Result<T, String>;
}

impl<T, E: std::fmt::Display> ResultExt<T> for Result<T, E> {
    fn into_string(self) -> Result<T, String> {
        self.map_err(|e| e.to_string())
    }
}
