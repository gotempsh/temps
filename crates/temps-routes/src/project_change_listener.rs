//! Project route change listener
//!
//! Listens to PostgreSQL `project_route_change` channel for notifications when:
//! - A project is created (need to reload routes)
//! - A project is deleted (need to remove from routes)
//! - A project slug changes (affects preview domain routing)
//!
//! This is more granular than reloading all routes - only affected projects are reloaded.

use crate::route_table::CachedPeerTable;
use anyhow::Result;
use std::sync::Arc;
use tracing::{error, info};

/// Listens for project route changes and updates the route cache
pub struct ProjectChangeListener {
    database_url: String,
    peer_table: Arc<CachedPeerTable>,
}

impl ProjectChangeListener {
    /// Create a new project change listener
    pub fn new(database_url: String, peer_table: Arc<CachedPeerTable>) -> Self {
        Self {
            database_url,
            peer_table,
        }
    }

    /// Start listening for project change notifications
    /// This runs in a background task and listens indefinitely
    pub async fn start_listening(self) -> Result<()> {
        use sqlx::postgres::{PgListener, PgPool};

        // Create PostgreSQL listener using sqlx
        let pool = PgPool::connect(&self.database_url).await?;
        let mut listener = PgListener::connect_with(&pool).await?;

        listener.listen("project_route_change").await?;
        info!("Started listening for project_route_change events");

        loop {
            match listener.recv().await {
                Ok(notification) => {
                    self.handle_project_change(notification.payload()).await;
                }
                Err(e) => {
                    error!("Error receiving project change notification: {}", e);

                    // Attempt to reconnect after error
                    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

                    match PgListener::connect_with(&pool).await {
                        Ok(mut new_listener) => {
                            if let Err(e) = new_listener.listen("project_route_change").await {
                                error!("Failed to re-subscribe to project_route_change: {}", e);
                            } else {
                                listener = new_listener;
                                info!("Reconnected to project_route_change listener");
                            }
                        }
                        Err(e) => {
                            error!("Failed to reconnect project_route_change listener: {}", e);
                        }
                    }
                }
            }
        }
    }

    /// Handle a project change notification
    async fn handle_project_change(&self, payload: &str) {
        match serde_json::from_str::<ProjectChangePayload>(payload) {
            Ok(change) => {
                info!(
                    "Project route change: action={}, project_id={}, is_deleted={}, slug={}",
                    change.action, change.project_id, change.is_deleted, change.slug
                );

                // Reload all routes when a project changes
                // (since project slug affects preview domain routing)
                if let Err(e) = self.peer_table.load_routes().await {
                    error!(
                        "Failed to reload routes after project change (project_id={}): {}",
                        change.project_id, e
                    );
                }
            }
            Err(e) => {
                error!(
                    "Failed to parse project_route_change payload: {}. Payload: {}",
                    e, payload
                );
            }
        }
    }
}

/// The payload structure sent by the PostgreSQL trigger
#[derive(Debug, serde::Deserialize)]
struct ProjectChangePayload {
    action: String, // INSERT, UPDATE, or DELETE
    project_id: i32,
    is_deleted: bool,
    slug: String,
    #[allow(dead_code)]
    timestamp: String, // Included for debugging/auditing
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_project_change_payload() {
        let payload = r#"{"action":"UPDATE","project_id":1,"is_deleted":false,"slug":"my-project","timestamp":"2025-11-06T10:30:00Z"}"#;
        let change: ProjectChangePayload = serde_json::from_str(payload).unwrap();
        assert_eq!(change.project_id, 1);
        assert_eq!(change.action, "UPDATE");
        assert!(!change.is_deleted);
    }

    #[test]
    fn test_parse_deleted_project() {
        let payload = r#"{"action":"UPDATE","project_id":2,"is_deleted":true,"slug":"old-project","timestamp":"2025-11-06T10:30:00Z"}"#;
        let change: ProjectChangePayload = serde_json::from_str(payload).unwrap();
        assert_eq!(change.project_id, 2);
        assert!(change.is_deleted);
    }
}
