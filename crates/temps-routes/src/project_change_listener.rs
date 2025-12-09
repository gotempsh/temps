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

    /// Handle a route change notification (project or environment)
    async fn handle_project_change(&self, payload: &str) {
        // Try to parse as RouteChangePayload which handles both project and environment changes
        match serde_json::from_str::<RouteChangePayload>(payload) {
            Ok(change) => {
                match change {
                    RouteChangePayload::Project(project_change) => {
                        info!(
                            "Project route change: action={}, project_id={}, is_deleted={}, slug={}",
                            project_change.action,
                            project_change.project_id,
                            project_change.is_deleted,
                            project_change.slug
                        );
                    }
                    RouteChangePayload::Environment(env_change) => {
                        info!(
                            "Environment route change: action={}, environment_id={}, project_id={}, deployment_id={:?}",
                            env_change.action,
                            env_change.environment_id,
                            env_change.project_id,
                            env_change.deployment_id
                        );
                    }
                }

                // Reload all routes when any change happens
                if let Err(e) = self.peer_table.load_routes().await {
                    error!("Failed to reload routes after change: {}", e);
                }
            }
            Err(e) => {
                error!(
                    "Failed to parse route change payload: {}. Payload: {}",
                    e, payload
                );
            }
        }
    }
}

/// Unified payload structure for route changes (project or environment)
#[derive(Debug, serde::Deserialize)]
#[serde(untagged)]
enum RouteChangePayload {
    Project(ProjectChangePayload),
    Environment(EnvironmentChangePayload),
}

/// Payload from project triggers
#[derive(Debug, serde::Deserialize)]
struct ProjectChangePayload {
    action: String, // INSERT, UPDATE, or DELETE
    project_id: i32,
    is_deleted: bool,
    slug: String,
    #[allow(dead_code)]
    timestamp: String, // Included for debugging/auditing
}

/// Payload from environment triggers (when current_deployment_id changes)
#[derive(Debug, serde::Deserialize)]
struct EnvironmentChangePayload {
    action: String, // ENVIRONMENT_UPDATE
    environment_id: i32,
    project_id: i32,
    deployment_id: Option<i32>,
    #[allow(dead_code)]
    timestamp: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_project_change_payload() {
        let payload = r#"{"action":"UPDATE","project_id":1,"is_deleted":false,"slug":"my-project","timestamp":"2025-11-06T10:30:00Z"}"#;
        let change: RouteChangePayload = serde_json::from_str(payload).unwrap();
        match change {
            RouteChangePayload::Project(project) => {
                assert_eq!(project.project_id, 1);
                assert_eq!(project.action, "UPDATE");
                assert!(!project.is_deleted);
            }
            _ => panic!("Expected Project payload"),
        }
    }

    #[test]
    fn test_parse_deleted_project() {
        let payload = r#"{"action":"UPDATE","project_id":2,"is_deleted":true,"slug":"old-project","timestamp":"2025-11-06T10:30:00Z"}"#;
        let change: RouteChangePayload = serde_json::from_str(payload).unwrap();
        match change {
            RouteChangePayload::Project(project) => {
                assert_eq!(project.project_id, 2);
                assert!(project.is_deleted);
            }
            _ => panic!("Expected Project payload"),
        }
    }

    #[test]
    fn test_parse_environment_change_payload() {
        let payload = r#"{"action":"ENVIRONMENT_UPDATE","environment_id":5,"project_id":1,"deployment_id":42,"timestamp":"2025-12-09T12:00:00Z"}"#;
        let change: RouteChangePayload = serde_json::from_str(payload).unwrap();
        match change {
            RouteChangePayload::Environment(env) => {
                assert_eq!(env.action, "ENVIRONMENT_UPDATE");
                assert_eq!(env.environment_id, 5);
                assert_eq!(env.project_id, 1);
                assert_eq!(env.deployment_id, Some(42));
            }
            _ => panic!("Expected Environment payload"),
        }
    }

    #[test]
    fn test_parse_environment_change_null_deployment() {
        let payload = r#"{"action":"ENVIRONMENT_UPDATE","environment_id":5,"project_id":1,"deployment_id":null,"timestamp":"2025-12-09T12:00:00Z"}"#;
        let change: RouteChangePayload = serde_json::from_str(payload).unwrap();
        match change {
            RouteChangePayload::Environment(env) => {
                assert_eq!(env.environment_id, 5);
                assert_eq!(env.deployment_id, None);
            }
            _ => panic!("Expected Environment payload"),
        }
    }
}
