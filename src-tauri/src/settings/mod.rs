//! User settings — plan tier, refresh interval, the GLM endpoint, and
//! encrypted vendor API keys. Persisted as `settings.json` in the app data dir.
//! Keys are stored ONLY as machine-bound ciphertext (see `encryption`).

use serde::{Deserialize, Serialize};
use tauri::AppHandle;

use crate::encryption::EncryptedSecret;
use crate::storage;
use crate::vendors::glm;

const SETTINGS_FILE: &str = "settings.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// One of: "pro", "max5x", "max20x", or "custom".
    pub plan: String,
    /// Background refresh interval in seconds.
    pub refresh_secs: u64,
    /// z.ai usage endpoint (account/region specific).
    pub glm_endpoint: String,
    /// Encrypted z.ai API key.
    pub zai_key: Option<EncryptedSecret>,
    /// Encrypted Anthropic admin API key.
    pub anthropic_key: Option<EncryptedSecret>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            plan: "max5x".to_string(),
            refresh_secs: 60,
            glm_endpoint: glm::DEFAULT_ENDPOINT.to_string(),
            zai_key: None,
            anthropic_key: None,
        }
    }
}

/// Frontend-facing view: never exposes ciphertext, only whether a key is set.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsView {
    pub plan: String,
    pub refresh_secs: u64,
    pub glm_endpoint: String,
    pub glm_key_set: bool,
    pub anthropic_key_set: bool,
}

impl From<&Settings> for SettingsView {
    fn from(s: &Settings) -> Self {
        Self {
            plan: s.plan.clone(),
            refresh_secs: s.refresh_secs,
            glm_endpoint: s.glm_endpoint.clone(),
            glm_key_set: s.zai_key.is_some(),
            anthropic_key_set: s.anthropic_key.is_some(),
        }
    }
}

pub fn load(app: &AppHandle) -> Settings {
    match storage::load_json::<Settings>(app, SETTINGS_FILE) {
        Ok(Some(s)) => s,
        Ok(None) => Settings::default(),
        Err(e) => {
            tracing::warn!("failed to load settings, using defaults: {e}");
            Settings::default()
        }
    }
}

pub fn save(app: &AppHandle, settings: &Settings) -> Result<(), storage::StorageError> {
    storage::save_json(app, SETTINGS_FILE, settings)
}
