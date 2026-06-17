//! JSON-on-disk utilities scoped to the app data directory.

use serde::{de::DeserializeOwned, Serialize};
use std::path::PathBuf;
use tauri::{AppHandle, Manager};

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("could not resolve app data dir: {0}")]
    AppDir(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

/// Resolve (and create) the app data directory.
pub fn get_app_data_dir(app: &AppHandle) -> Result<PathBuf, StorageError> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| StorageError::AppDir(e.to_string()))?;
    if !dir.exists() {
        std::fs::create_dir_all(&dir)?;
    }
    Ok(dir)
}

/// Path to a named JSON file inside the app data dir.
pub fn get_storage_path(app: &AppHandle, name: &str) -> Result<PathBuf, StorageError> {
    Ok(get_app_data_dir(app)?.join(name))
}

/// Load JSON, returning `None` if the file does not exist.
pub fn load_json<T: DeserializeOwned>(
    app: &AppHandle,
    name: &str,
) -> Result<Option<T>, StorageError> {
    let path = get_storage_path(app, name)?;
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(path)?;
    let value = serde_json::from_str(&raw)?;
    Ok(Some(value))
}

/// Persist a value as pretty JSON.
pub fn save_json<T: Serialize>(app: &AppHandle, name: &str, value: &T) -> Result<(), StorageError> {
    let path = get_storage_path(app, name)?;
    let raw = serde_json::to_string_pretty(value)?;
    std::fs::write(path, raw)?;
    Ok(())
}
