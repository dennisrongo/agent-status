//! User settings — plan tier, refresh interval, the GLM endpoint, and
//! encrypted vendor API keys. Persisted as `settings.json` in the app data dir.
//! Keys are stored ONLY as machine-bound ciphertext (see `encryption`).

use serde::{Deserialize, Serialize};
use tauri::AppHandle;

use crate::encryption::EncryptedSecret;
use crate::storage;
use crate::vendors::glm;

const SETTINGS_FILE: &str = "settings.json";

/// Allowed bounds for the auto-refresh interval (seconds).
pub const MIN_REFRESH_SECS: u64 = 10;
pub const MAX_REFRESH_SECS: u64 = 3600;

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
    /// Encrypted GitHub Copilot OAuth token, minted via the in-app device flow.
    /// Tried before any editor/CLI token. Absent when usage is read from a
    /// locally-discovered token instead.
    pub copilot_token: Option<EncryptedSecret>,
    /// Use Claude's live usage API (reads the Claude Code OAuth token) for the
    /// Overview meters instead of the local token estimate.
    pub live_claude: bool,
    /// Register the app to launch automatically at login. Menubar widgets are
    /// expected to persist, so this defaults on.
    pub launch_on_startup: bool,
    /// Compact "main stats only" Overview that fits the window without
    /// scrolling. Defaults off — the full view is the standard layout.
    pub minimal_view: bool,
    /// Which provider the tray hover popover previews: "claude" or "glm".
    /// Defaults to Claude (the local-by-default provider).
    pub tooltip_provider: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            plan: "max5x".to_string(),
            refresh_secs: 30,
            glm_endpoint: glm::DEFAULT_ENDPOINT.to_string(),
            zai_key: None,
            anthropic_key: None,
            copilot_token: None,
            live_claude: true,
            launch_on_startup: true,
            minimal_view: false,
            tooltip_provider: "claude".to_string(),
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
    /// Whether a Copilot token was connected in-app via the device flow.
    pub copilot_connected: bool,
    pub live_claude: bool,
    pub launch_on_startup: bool,
    pub minimal_view: bool,
    pub tooltip_provider: String,
}

impl From<&Settings> for SettingsView {
    fn from(s: &Settings) -> Self {
        Self {
            plan: s.plan.clone(),
            refresh_secs: s.refresh_secs,
            glm_endpoint: s.glm_endpoint.clone(),
            glm_key_set: s.zai_key.is_some(),
            anthropic_key_set: s.anthropic_key.is_some(),
            copilot_connected: s.copilot_token.is_some(),
            live_claude: s.live_claude,
            launch_on_startup: s.launch_on_startup,
            minimal_view: s.minimal_view,
            tooltip_provider: s.tooltip_provider.clone(),
        }
    }
}

pub fn load(app: &AppHandle) -> Settings {
    let mut settings = match storage::load_json::<Settings>(app, SETTINGS_FILE) {
        Ok(Some(s)) => s,
        Ok(None) => Settings::default(),
        Err(e) => {
            tracing::warn!("failed to load settings, using defaults: {e}");
            Settings::default()
        }
    };
    migrate(&mut settings);
    settings
}

/// Upgrade settings written by older builds (e.g. the stale z.ai endpoint).
fn migrate(s: &mut Settings) {
    if s.glm_endpoint.trim().is_empty() || glm::STALE_ENDPOINTS.contains(&s.glm_endpoint.as_str()) {
        s.glm_endpoint = glm::DEFAULT_ENDPOINT.to_string();
    }
}

pub fn save(app: &AppHandle, settings: &Settings) -> Result<(), storage::StorageError> {
    storage::save_json(app, SETTINGS_FILE, settings)
}
