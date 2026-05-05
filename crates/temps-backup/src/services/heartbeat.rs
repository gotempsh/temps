//! Heartbeat guard for long-running backups.
//!
//! While a backup engine is running, the worker spawns a background task
//! that updates `backups.last_heartbeat_at` every 30 seconds. The UI uses
//! the heartbeat to detect stalled backups: if the row's state is still
//! `running` but the heartbeat is older than 5 minutes, the worker is
//! presumed dead.
//!
//! The guard cancels the heartbeat task when dropped, so the
//! happy-path (backup completes / fails normally) doesn't need any
//! explicit teardown.

use std::sync::Arc;
use std::time::Duration;

use sea_orm::{ActiveModelTrait, DatabaseConnection, Set};
use tokio::task::JoinHandle;
use tracing::{debug, warn};

/// How often the worker pings the row. Short enough that the UI's stall
/// detector (5 minutes) never false-positives a healthy backup, long
/// enough that the database write rate stays trivial.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

/// Holds a background task that keeps a `backups` row's
/// `last_heartbeat_at` fresh. Drop the guard to stop the task — useful in
/// success and failure paths alike, since neither has to remember to
/// teardown.
pub struct HeartbeatGuard {
    handle: Option<JoinHandle<()>>,
}

impl HeartbeatGuard {
    /// Start a heartbeat task for the given backup row. The task runs
    /// until dropped; failures to update the row are logged at warn level
    /// but don't propagate (a transient DB blip shouldn't kill the
    /// backup).
    pub fn spawn(db: Arc<DatabaseConnection>, backup_id: i32) -> Self {
        let handle = tokio::spawn(async move {
            // First tick fires immediately, so we don't have to write a
            // separate "kick the heartbeat now" step at construction.
            let mut interval = tokio::time::interval(HEARTBEAT_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

            loop {
                interval.tick().await;

                let now = chrono::Utc::now();
                let update = temps_entities::backups::ActiveModel {
                    id: Set(backup_id),
                    last_heartbeat_at: Set(Some(now)),
                    ..Default::default()
                };

                if let Err(e) = update.update(db.as_ref()).await {
                    warn!("Failed to update heartbeat for backup {}: {}", backup_id, e);
                } else {
                    debug!("heartbeat refreshed for backup {}", backup_id);
                }
            }
        });

        Self {
            handle: Some(handle),
        }
    }
}

impl Drop for HeartbeatGuard {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}
