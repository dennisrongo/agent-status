//! Shared application state: the most recent usage snapshot and user settings.

use chrono::{DateTime, Utc};

use crate::scanner::{Bucket, UsageSnapshot};
use crate::settings::Settings;

#[derive(Default)]
pub struct AppState {
    pub snapshot: Option<UsageSnapshot>,
    pub settings: Settings,
    /// Last *successful* live Claude meters. The `/usage` endpoint rate-limits
    /// aggressively when polled, so a failed refresh reuses this instead of
    /// swapping in the local estimate — which is on a different scale and would
    /// make the meters visibly flip-flop between two number systems.
    pub live_claude_buckets: Option<Vec<Bucket>>,
    /// When the live Claude `/usage` endpoint was last *attempted*. Used to
    /// throttle it well below the log-scan cadence (see `LIVE_CLAUDE_MIN_SECS`),
    /// since session/weekly windows move slowly and the endpoint throttles hard.
    pub live_claude_attempted_at: Option<DateTime<Utc>>,
}

/// Minimum seconds between live `/usage` fetches, independent of the (faster)
/// log-scan refresh interval.
pub const LIVE_CLAUDE_MIN_SECS: i64 = 120;

impl AppState {
    pub fn new(settings: Settings) -> Self {
        Self {
            snapshot: None,
            settings,
            live_claude_buckets: None,
            live_claude_attempted_at: None,
        }
    }
}
