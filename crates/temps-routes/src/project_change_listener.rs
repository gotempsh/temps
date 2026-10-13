// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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
use temps_core::log_transitions::{FailureLatch, FailureLog};
use tracing::{debug, error, info, warn};

/// How long to wait for a route-change notification before reconciling the
/// route table anyway.
///
/// This is the upper bound on how stale routing can get if the `LISTEN`
/// connection dies without reporting an error. It is deliberately short because
/// the failure it guards is a *withdrawn* public port still being served: the
/// window is the window in which a route the operator removed is still
/// reachable. The cost is one route-table load per minute on an otherwise idle
/// control plane, which is well below what an active install already does —
/// every deployment triggers a reload through this same path.
const IDLE_RECONCILE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

/// Failure state of the listener task's recurring operations.
///
/// The task reconciles the route table every [`IDLE_RECONCILE_INTERVAL`], and
/// retries the `LISTEN` connection every few seconds while PostgreSQL is
/// unreachable. Logging each failed attempt at ERROR turned one outage into a
/// stream of identical lines; these latches log when a failure starts (plus
/// an hourly reminder) and when it recovers.
#[derive(Default)]
struct ListenerHealth {
    /// The `LISTEN` connection (receive + reconnect).
    connection: FailureLatch,
    /// Reloading the route table from the database.
    reload: FailureLatch,
    /// Publishing `RouteTableUpdated` on the job queue.
    publish: FailureLatch,
}

/// Reload the route table and announce it on the queue, logging failures on
/// state transitions only. Returns whether the reload succeeded.
async fn reload_and_publish(
    peer_table: &CachedPeerTable,
    queue: &Arc<dyn temps_core::JobQueue>,
    health: &ListenerHealth,
    environment_id: Option<i32>,
    deployment_id: Option<i32>,
) -> bool {
    if let Err(e) = peer_table.load_routes().await {
        match health.reload.record_failure() {
            FailureLog::Started => error!("Failed to reload project routes: {}", e),
            FailureLog::Reminder { consecutive } => error!(
                consecutive_failures = consecutive,
                "Still failing to reload project routes: {}", e
            ),
            FailureLog::Suppressed { consecutive } => debug!(
                consecutive_failures = consecutive,
                "Failed to reload project routes: {}", e
            ),
        }
        return false;
    }
    if let Some(failures) = health.reload.record_success() {
        info!(
            previous_failures = failures,
            "Project route reload recovered"
        );
    }

    let route_count = peer_table.len();
    debug!("Project route table synchronized ({} entries)", route_count);
    let event = temps_core::Job::RouteTableUpdated(temps_core::RouteTableUpdatedJob {
        environment_id,
        deployment_id,
        route_count,
    });
    match queue.send(event).await {
        Ok(()) => {
            if let Some(failures) = health.publish.record_success() {
                info!(
                    previous_failures = failures,
                    "Publishing RouteTableUpdated events recovered"
                );
            }
        }
        Err(e) => match health.publish.record_failure() {
            FailureLog::Started => error!("Failed to send RouteTableUpdated event: {}", e),
            FailureLog::Reminder { consecutive } => error!(
                consecutive_failures = consecutive,
                "Still failing to send RouteTableUpdated events: {}", e
            ),
            FailureLog::Suppressed { consecutive } => debug!(
                consecutive_failures = consecutive,
                "Failed to send RouteTableUpdated event: {}", e
            ),
        },
    }
    true
}

/// Listens for project route changes and updates the route cache
pub struct ProjectChangeListener {
    database_url: String,
    peer_table: Arc<CachedPeerTable>,
    queue: Arc<dyn temps_core::JobQueue>,
    task_handle: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl ProjectChangeListener {
    /// Create a new project change listener
    pub fn new(
        database_url: String,
        peer_table: Arc<CachedPeerTable>,
        queue: Arc<dyn temps_core::JobQueue>,
    ) -> Self {
        Self {
            database_url,
            peer_table,
            queue,
            task_handle: std::sync::Mutex::new(None),
        }
    }

    /// Start listening for project change notifications in a background task.
    /// The task runs until `shutdown()` is called or the listener is dropped.
    pub async fn start_listening(&self) -> Result<()> {
        use sqlx::postgres::{PgListener, PgPool};

        // Create PostgreSQL listener using sqlx
        let pool = PgPool::connect(&self.database_url).await?;
        let mut pg_listener = PgListener::connect_with(&pool).await?;

        pg_listener.listen("project_route_change").await?;
        info!("Started listening for project_route_change events");

        let peer_table = self.peer_table.clone();
        let queue = self.queue.clone();

        let handle = tokio::spawn(async move {
            let health = ListenerHealth::default();
            // Event-driven loop with a bounded wait. Reacting to PG NOTIFY
            // alone is not sufficient: a `LISTEN` connection dropped by a NAT,
            // firewall, or load balancer idle-timeout leaves `recv()` parked on
            // a half-open socket forever, so no error is ever observed, the
            // reconnect path never runs, and the route table stays stale
            // indefinitely. That fails open — a withdrawn public port would
            // remain reachable. Capping the wait converts that unbounded
            // staleness into a reconcile that is at worst
            // `IDLE_RECONCILE_INTERVAL` behind the database.
            //
            // sqlx 0.8 exposes no TCP keepalive setting, so this timeout is the
            // only mechanism available to bound a silently dead listener.
            loop {
                match tokio::time::timeout(IDLE_RECONCILE_INTERVAL, pg_listener.recv()).await {
                    Ok(Ok(n)) => {
                        // handle_project_change_static parses the payload,
                        // calls load_routes, and broadcasts the event.
                        Self::handle_project_change_static(
                            &peer_table,
                            &queue,
                            &health,
                            n.payload(),
                        )
                        .await;
                        continue;
                    }
                    Err(_elapsed) => {
                        // No notification arrived within the window. Either the
                        // system is genuinely idle (the reload is a cheap no-op
                        // resync) or the listener is silently dead (the reload
                        // is the only thing keeping routes correct). We cannot
                        // tell the two apart — sqlx gives no way to probe the
                        // listener's own connection — so reconcile either way
                        // and fall through to the reload below.
                        debug!(
                            "No project route change within {}s; reconciling route table",
                            IDLE_RECONCILE_INTERVAL.as_secs()
                        );
                    }
                    Ok(Err(e)) => {
                        // A dropped LISTEN connection (PostgreSQL restart,
                        // idle-connection reaper, network blip) is recovered
                        // right below, so the first drop is a WARN. Failing to
                        // reconnect means the database itself is unreachable:
                        // that is reported at ERROR, once per outage plus an
                        // hourly reminder, not once per 5-second retry.
                        let outcome = health.connection.record_failure();
                        match outcome {
                            FailureLog::Started => warn!(
                                "Lost project_route_change listener connection: {}; reconnecting",
                                e
                            ),
                            FailureLog::Reminder { consecutive } => error!(
                                consecutive_failures = consecutive,
                                "project_route_change listener is still disconnected: {}", e
                            ),
                            FailureLog::Suppressed { consecutive } => debug!(
                                consecutive_failures = consecutive,
                                "Error receiving project change notification: {}", e
                            ),
                        }

                        // Attempt to reconnect after error
                        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

                        let reconnect = match PgListener::connect_with(&pool).await {
                            Ok(mut new_listener) => {
                                match new_listener.listen("project_route_change").await {
                                    Ok(()) => {
                                        pg_listener = new_listener;
                                        Ok(())
                                    }
                                    Err(e) => Err(format!(
                                        "failed to re-subscribe to project_route_change: {e}"
                                    )),
                                }
                            }
                            Err(e) => Err(format!(
                                "failed to reconnect project_route_change listener: {e}"
                            )),
                        };
                        match reconnect {
                            Ok(()) => {
                                let failures = health.connection.record_success().unwrap_or(0);
                                info!(
                                    failed_attempts = failures,
                                    "Reconnected to project_route_change listener"
                                );
                            }
                            Err(reason) if matches!(outcome, FailureLog::Suppressed { .. }) => {
                                debug!("{}", reason);
                            }
                            Err(reason) => {
                                error!("{}; will keep retrying", reason);
                            }
                        }
                        // Fall through to reload once after reconnect.
                    }
                }

                // Safety reload: reached after a listener error/reconnect, or
                // after an idle window elapsed without a notification.
                reload_and_publish(&peer_table, &queue, &health, None, None).await;
            }
        });

        if let Ok(mut guard) = self.task_handle.lock() {
            *guard = Some(handle);
        }

        Ok(())
    }

    /// Stop the background listener task
    pub fn shutdown(&self) {
        if let Ok(mut guard) = self.task_handle.lock() {
            if let Some(handle) = guard.take() {
                handle.abort();
                info!("Project change listener stopped");
            }
        }
    }

    /// Handle a route change notification (project or environment)
    async fn handle_project_change_static(
        peer_table: &CachedPeerTable,
        queue: &Arc<dyn temps_core::JobQueue>,
        health: &ListenerHealth,
        payload: &str,
    ) {
        // Try to parse as RouteChangePayload which handles both project and environment changes
        match serde_json::from_str::<RouteChangePayload>(payload) {
            Ok(change) => {
                // Extract environment/deployment context before we move into load_routes
                let (environment_id, deployment_id) = match &change {
                    RouteChangePayload::Project(project_change) => {
                        info!(
                            "Project route change: action={}, project_id={}, is_deleted={}, slug={}",
                            project_change.action,
                            project_change.project_id,
                            project_change.is_deleted,
                            project_change.slug
                        );
                        (None, None)
                    }
                    RouteChangePayload::Environment(env_change) => {
                        info!(
                            "Environment route change: action={}, environment_id={}, project_id={}, deployment_id={:?}",
                            env_change.action,
                            env_change.environment_id,
                            env_change.project_id,
                            env_change.deployment_id
                        );
                        (Some(env_change.environment_id), env_change.deployment_id)
                    }
                };

                // Reload all routes when any change happens.
                //
                // NOTE: The environment_id and deployment_id in the event come from
                // the PG NOTIFY payload, not from what load_routes() actually loaded.
                // With concurrent deployments, the deployment_id may not match what
                // the route table actually resolved to. Consumers (e.g. mark_complete)
                // should verify the actual DB state rather than trusting this field.
                reload_and_publish(peer_table, queue, health, environment_id, deployment_id).await;
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

impl Drop for ProjectChangeListener {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Unified payload structure for route changes (project or environment)
///
/// IMPORTANT: `Environment` must be listed before `Project` because with
/// `#[serde(untagged)]`, serde tries variants in order. `EnvironmentChangePayload`
/// has a required `environment_id` field that acts as a discriminator.
/// `ProjectChangePayload` uses `#[serde(default)]` on most fields, so it would
/// greedily match environment payloads too if listed first.
#[derive(Debug, serde::Deserialize)]
#[serde(untagged)]
enum RouteChangePayload {
    Environment(EnvironmentChangePayload),
    Project(ProjectChangePayload),
}

/// Payload from project triggers
///
/// Fields are optional with defaults because the INSERT/DELETE trigger sends a
/// minimal payload (`action`, `project_id`, `field`) that lacks `is_deleted`,
/// `slug`, and `timestamp`. Only UPDATEEs send the full payload.
#[derive(Debug, serde::Deserialize)]
struct ProjectChangePayload {
    action: String, // INSERT, UPDATE, or DELETE
    project_id: i32,
    #[serde(default)]
    is_deleted: bool,
    #[serde(default)]
    slug: String,
    #[serde(default)]
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

    #[test]
    fn test_parse_project_insert_payload_minimal() {
        // INSERT/DELETE triggers send minimal payloads without is_deleted, slug, timestamp.
        // These must parse successfully with defaults — this was a bug that caused
        // route reloads to be silently skipped for new project deployments.
        let payload = r#"{"action":"INSERT","project_id":42,"field":"project"}"#;
        let change: RouteChangePayload = serde_json::from_str(payload).unwrap();
        match change {
            RouteChangePayload::Project(project) => {
                assert_eq!(project.project_id, 42);
                assert_eq!(project.action, "INSERT");
                assert!(!project.is_deleted); // default
                assert_eq!(project.slug, ""); // default
            }
            _ => panic!("Expected Project payload, got {:?}", change),
        }
    }

    #[test]
    fn test_parse_project_delete_payload_minimal() {
        let payload = r#"{"action":"DELETE","project_id":7,"field":"project"}"#;
        let change: RouteChangePayload = serde_json::from_str(payload).unwrap();
        match change {
            RouteChangePayload::Project(project) => {
                assert_eq!(project.project_id, 7);
                assert_eq!(project.action, "DELETE");
            }
            _ => panic!("Expected Project payload, got {:?}", change),
        }
    }

    // ========================================================================
    // ProjectChangeListener lifecycle tests
    // ========================================================================

    /// Create a no-op queue for tests that don't need queue functionality
    fn test_queue() -> Arc<dyn temps_core::JobQueue> {
        struct NoOpQueue;
        #[temps_core::async_trait::async_trait]
        impl temps_core::JobQueue for NoOpQueue {
            async fn send(&self, _job: temps_core::Job) -> Result<(), temps_core::QueueError> {
                Ok(())
            }
            fn subscribe(&self) -> Box<dyn temps_core::JobReceiver> {
                unimplemented!("not needed in tests")
            }
        }
        Arc::new(NoOpQueue)
    }

    /// The reconcile interval is the upper bound on how long a public port the
    /// operator removed can still be served if the `LISTEN` connection dies
    /// silently. The loop that consumes it needs a live PostgreSQL listener to
    /// exercise, so this pins the one part that can be checked in isolation:
    /// that the bound stays a bound. Widening it to tens of minutes would
    /// quietly restore the fail-open window it exists to close.
    #[test]
    fn test_idle_reconcile_interval_bounds_route_staleness() {
        assert!(
            IDLE_RECONCILE_INTERVAL <= std::time::Duration::from_secs(120),
            "reconcile interval must stay short enough to bound a withdrawn \
             route's reachability, got {:?}",
            IDLE_RECONCILE_INTERVAL
        );
        assert!(
            IDLE_RECONCILE_INTERVAL >= std::time::Duration::from_secs(15),
            "reconcile interval must not be so short that idle control planes \
             reload the route table constantly, got {:?}",
            IDLE_RECONCILE_INTERVAL
        );
    }

    /// A queue whose sends always fail, counting attempts.
    struct FailingQueue(std::sync::atomic::AtomicUsize);

    #[temps_core::async_trait::async_trait]
    impl temps_core::JobQueue for FailingQueue {
        async fn send(&self, _job: temps_core::Job) -> Result<(), temps_core::QueueError> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err(temps_core::QueueError::SendError("no receivers".into()))
        }
        fn subscribe(&self) -> Box<dyn temps_core::JobReceiver> {
            unimplemented!("not needed in tests")
        }
    }

    #[tokio::test]
    async fn reload_failures_are_tracked_as_one_ongoing_outage() {
        // Model failed database queries rather than a connection with no
        // backend, whose query builder panics before returning a DB error.
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
                .append_query_errors(
                    (0..5).map(|_| sea_orm::DbErr::Custom("database unavailable".into())),
                )
                .into_connection(),
        );
        let peer_table = CachedPeerTable::new(db);
        let health = ListenerHealth::default();
        let queue = test_queue();

        for _ in 0..5 {
            assert!(!reload_and_publish(&peer_table, &queue, &health, None, None).await);
        }
        assert!(health.reload.is_failing());
        assert_eq!(
            health.reload.record_failure(),
            FailureLog::Suppressed { consecutive: 6 },
            "every reload after the first stays below the logged level"
        );
        // Nothing is published while routes cannot be loaded.
        assert!(!health.publish.is_failing());
    }

    #[tokio::test]
    async fn handle_change_with_unparsable_payload_does_not_reload() {
        let db = Arc::new(sea_orm::DatabaseConnection::Disconnected);
        let peer_table = CachedPeerTable::new(db);
        let health = ListenerHealth::default();
        let queue: Arc<dyn temps_core::JobQueue> =
            Arc::new(FailingQueue(std::sync::atomic::AtomicUsize::new(0)));

        ProjectChangeListener::handle_project_change_static(
            &peer_table,
            &queue,
            &health,
            "not json",
        )
        .await;
        assert!(!health.reload.is_failing());
        assert!(!health.publish.is_failing());
    }

    #[test]
    fn test_project_change_listener_new_has_no_task() {
        let db = Arc::new(sea_orm::DatabaseConnection::Disconnected);
        let peer_table = Arc::new(CachedPeerTable::new(db));
        let listener = ProjectChangeListener::new(
            "postgresql://fake:fake@localhost/fake".to_string(),
            peer_table,
            test_queue(),
        );

        let guard = listener.task_handle.lock().unwrap();
        assert!(guard.is_none(), "New listener should have no task handle");
    }

    #[test]
    fn test_project_change_listener_shutdown_without_start_is_safe() {
        let db = Arc::new(sea_orm::DatabaseConnection::Disconnected);
        let peer_table = Arc::new(CachedPeerTable::new(db));
        let listener = ProjectChangeListener::new(
            "postgresql://fake:fake@localhost/fake".to_string(),
            peer_table,
            test_queue(),
        );

        // Calling shutdown before start should not panic
        listener.shutdown();

        let guard = listener.task_handle.lock().unwrap();
        assert!(guard.is_none());
    }

    #[test]
    fn test_project_change_listener_drop_without_start_is_safe() {
        let db = Arc::new(sea_orm::DatabaseConnection::Disconnected);
        let peer_table = Arc::new(CachedPeerTable::new(db));
        let listener = ProjectChangeListener::new(
            "postgresql://fake:fake@localhost/fake".to_string(),
            peer_table,
            test_queue(),
        );

        // Dropping without starting should not panic
        drop(listener);
    }
}
