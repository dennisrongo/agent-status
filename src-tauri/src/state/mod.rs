//! Shared application state: the most recent usage snapshot and user settings.

use crate::scanner::UsageSnapshot;
use crate::settings::Settings;

#[derive(Default)]
pub struct AppState {
    pub snapshot: Option<UsageSnapshot>,
    pub settings: Settings,
}

impl AppState {
    pub fn new(settings: Settings) -> Self {
        Self { snapshot: None, settings }
    }
}
