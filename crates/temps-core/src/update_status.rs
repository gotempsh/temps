//! Process-wide "a newer release exists" slot.
//!
//! Written by the background update notifier that `temps serve` spawns
//! (temps-cli), read by the settings API (temps-config) so the web console
//! can render an upgrade banner. Lives in temps-core because those two
//! crates must not depend on each other.

use chrono::{DateTime, Utc};
use std::sync::RwLock;

/// Docs page operators are pointed at when a newer release is available.
pub const UPGRADE_DOCS_URL: &str = "https://temps.sh/docs/upgrade-temps";

/// A newer published release found for this install's channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvailableUpdate {
    /// Version tag of the running binary, e.g. `v0.1.0-beta.45`.
    pub current_version: String,
    /// Newest published tag on this install's channel, e.g. `v0.1.0-beta.46`.
    pub latest_version: String,
    /// Channel the install tracks: `"stable"` or `"beta"`.
    pub channel: String,
    /// Release-notes page (GitHub release) for the newer version.
    pub release_url: String,
    /// When the check that found this update ran.
    pub checked_at: DateTime<Utc>,
}

/// Shared slot holding the most recent successful update-check result.
///
/// Advisory, in-memory only: the running binary's version is a compile-time
/// constant, so the notice stays valid until the process restarts on the new
/// binary — persisting it would only risk serving a stale banner after an
/// upgrade. Empty until the first check finds a newer release; a failed
/// check never clears a previously found notice.
#[derive(Debug, Default)]
pub struct UpdateStatusSlot {
    inner: RwLock<Option<AvailableUpdate>>,
}

impl UpdateStatusSlot {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a newer release. Overwrites any earlier notice — later checks
    /// can only find the same or an even newer version.
    pub fn set(&self, update: AvailableUpdate) {
        let mut guard = match self.inner.write() {
            Ok(guard) => guard,
            // A panic while holding this lock can't corrupt an Option swap;
            // recover the guard rather than poisoning the banner forever.
            Err(poisoned) => poisoned.into_inner(),
        };
        *guard = Some(update);
    }

    /// The most recent notice, if any check has found a newer release.
    pub fn get(&self) -> Option<AvailableUpdate> {
        let guard = match self.inner.read() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notice(latest: &str) -> AvailableUpdate {
        AvailableUpdate {
            current_version: "v0.1.0".to_string(),
            latest_version: latest.to_string(),
            channel: "stable".to_string(),
            release_url: format!("https://github.com/gotempsh/temps/releases/tag/{latest}"),
            checked_at: Utc::now(),
        }
    }

    #[test]
    fn test_slot_starts_empty() {
        assert_eq!(UpdateStatusSlot::new().get(), None);
    }

    #[test]
    fn test_slot_set_then_get_roundtrips() {
        let slot = UpdateStatusSlot::new();
        let update = notice("v0.2.0");
        slot.set(update.clone());
        assert_eq!(slot.get(), Some(update));
    }

    #[test]
    fn test_slot_later_set_overwrites() {
        let slot = UpdateStatusSlot::new();
        slot.set(notice("v0.2.0"));
        slot.set(notice("v0.3.0"));
        assert_eq!(
            slot.get().map(|u| u.latest_version).as_deref(),
            Some("v0.3.0")
        );
    }
}
