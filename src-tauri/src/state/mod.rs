//! Shared application state: the most recent usage snapshot and user settings.

use chrono::{DateTime, Utc};

use crate::scanner::{Bucket, UsageSnapshot};
use crate::settings::Settings;

/// An in-progress Copilot device-flow authorization, held between
/// `copilot_device_start` and the poll that completes (or expires) it. Carries
/// the user-facing fields too so a repeat `copilot_device_start` can return the
/// same in-flight code rather than orphaning it with a new one.
#[derive(Clone)]
pub struct PendingDevice {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    /// Pre-filled verification URL, re-opened in the browser if the user
    /// re-triggers a connect for this still-valid code.
    pub verification_uri_complete: String,
    pub interval: u64,
    pub expires_at: DateTime<Utc>,
}

impl PendingDevice {
    /// Whether this device code is still within its validity window.
    pub fn is_valid(&self, now: DateTime<Utc>) -> bool {
        now < self.expires_at
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn pending_device_validity_window() {
        let p = PendingDevice {
            device_code: "dc".into(),
            user_code: "AB-CD".into(),
            verification_uri: "https://github.com/login/device".into(),
            verification_uri_complete: "https://github.com/login/device?user_code=AB-CD".into(),
            interval: 5,
            expires_at: at("2026-06-20T12:15:00Z"),
        };
        assert!(p.is_valid(at("2026-06-20T12:14:59Z")), "before expiry → valid");
        assert!(!p.is_valid(at("2026-06-20T12:15:00Z")), "at expiry → expired");
        assert!(!p.is_valid(at("2026-06-20T12:20:00Z")), "after expiry → expired");
    }
}

#[derive(Default)]
pub struct AppState {
    pub snapshot: Option<UsageSnapshot>,
    pub settings: Settings,
    /// In-flight Copilot device-flow authorization, if any.
    pub pending_copilot_device: Option<PendingDevice>,
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

/// Serializes `collect()` so concurrent callers (refresh-on-open, the frontend
/// `get_usage`, and the background loop) don't race the rate-limited live
/// endpoint and emit conflicting estimate-vs-live snapshots. Managed separately
/// from `AppState` because it must be held across `.await` points.
#[derive(Default)]
pub struct CollectLock(pub tokio::sync::Mutex<()>);

/// Minimum seconds between live `/usage` fetches, independent of the (faster)
/// log-scan refresh interval.
pub const LIVE_CLAUDE_MIN_SECS: i64 = 120;

impl AppState {
    pub fn new(settings: Settings) -> Self {
        Self {
            snapshot: None,
            settings,
            pending_copilot_device: None,
            live_claude_buckets: None,
            live_claude_attempted_at: None,
        }
    }
}
