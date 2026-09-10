// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! In-process route reload subscriber.
//!
//! Consumes [`temps_core::Job::ForceRouteReload`] events from the shared
//! in-process broadcast queue and reloads the proxy's route table, then
//! publishes a [`temps_core::Job::RouteTableUpdated`] confirmation.
//!
//! # Why this exists
//!
//! Route reloads were previously driven *only* by PostgreSQL `LISTEN/NOTIFY`
//! (see [`crate::route_table::RouteTableListener`] and
//! [`crate::project_change_listener::ProjectChangeListener`]). That path is
//! fire-and-forget: when a deployment finishes, the deploy job writes
//! `environments.current_deployment_id`, a DB trigger fires `pg_notify`, and a
//! long-lived listener connection is expected to receive it and call
//! `load_routes()`.
//!
//! In a long-lived production process that single listener connection can die
//! silently (idle timeout on a connection pooler, a NAT/load-balancer dropping
//! an idle socket, a failover) in a way that does *not* surface as a `recv()`
//! error — so the reconnect branch never runs and the notification is lost
//! forever. The proxy keeps serving stale routes and newly deployed
//! environments return "deployment not found" until the route table is
//! reloaded by hand.
//!
//! This subscriber closes that gap for the common single-node case (control
//! plane + proxy in one `temps serve` process). The deploy job and the route
//! table share one tokio broadcast channel, so the deploy job can request a
//! reload over a channel that has no database connection in its critical path
//! and therefore cannot wedge between deployments. The deploy pipeline blocks
//! on the resulting `RouteTableUpdated` confirmation, so a deployment is only
//! marked complete once the proxy's in-memory route table actually reflects
//! the new route.
//!
//! The PostgreSQL NOTIFY path is retained unchanged: it is still the mechanism
//! that propagates route changes to *remote* worker nodes, which do not share
//! this in-process queue.

use crate::route_table::CachedPeerTable;
use std::sync::Arc;
use temps_core::{Job, JobQueue, RouteTableUpdatedJob};
use tracing::{debug, error, info, warn};

/// Subscribes to the shared job queue and reloads routes on demand.
pub struct RouteReloadSubscriber {
    peer_table: Arc<CachedPeerTable>,
    queue: Arc<dyn JobQueue>,
    task_handle: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl RouteReloadSubscriber {
    /// Create a new subscriber bound to the given route table and queue.
    pub fn new(peer_table: Arc<CachedPeerTable>, queue: Arc<dyn JobQueue>) -> Self {
        Self {
            peer_table,
            queue,
            task_handle: std::sync::Mutex::new(None),
        }
    }

    /// Spawn the background task that listens for `ForceRouteReload` events.
    ///
    /// The task runs until [`Self::shutdown`] is called or the subscriber is
    /// dropped. The caller MUST keep the returned subscriber alive for the
    /// lifetime of the process — its `Drop` aborts the task.
    pub fn start(&self) {
        let mut receiver = self.queue.subscribe();
        let peer_table = self.peer_table.clone();
        let queue = self.queue.clone();

        let handle = tokio::spawn(async move {
            info!("Started in-process route reload subscriber");
            loop {
                match receiver.recv().await {
                    Ok(Job::ForceRouteReload(req)) => {
                        debug!(
                            environment_id = ?req.environment_id,
                            deployment_id = ?req.deployment_id,
                            "ForceRouteReload received — reloading route table in-process"
                        );
                        Self::reload_and_confirm(
                            &peer_table,
                            &queue,
                            req.environment_id,
                            req.deployment_id,
                        )
                        .await;
                    }
                    // Any other job type is irrelevant to this subscriber.
                    Ok(_) => continue,
                    Err(temps_core::QueueError::ChannelClosed) => {
                        // The queue is gone — the process is shutting down.
                        warn!("Route reload subscriber: queue channel closed, stopping");
                        break;
                    }
                    Err(e) => {
                        // Broadcast receiver lagged: messages were dropped but the
                        // channel is still alive. We may have missed a
                        // ForceRouteReload, so reload defensively — load_routes()
                        // is idempotent and reflects the latest DB state regardless
                        // of how many events we missed.
                        warn!(
                            "Route reload subscriber lagged ({}); reloading defensively",
                            e
                        );
                        Self::reload_and_confirm(&peer_table, &queue, None, None).await;
                    }
                }
            }
        });

        if let Ok(mut guard) = self.task_handle.lock() {
            *guard = Some(handle);
        }
    }

    /// Reload the route table and publish a `RouteTableUpdated` confirmation
    /// carrying the originating environment/deployment so the deploy pipeline
    /// can match it.
    async fn reload_and_confirm(
        peer_table: &CachedPeerTable,
        queue: &Arc<dyn JobQueue>,
        environment_id: Option<i32>,
        deployment_id: Option<i32>,
    ) {
        match peer_table.load_routes().await {
            Ok(_) => {
                let route_count = peer_table.len();
                debug!(
                    route_count,
                    environment_id = ?environment_id,
                    deployment_id = ?deployment_id,
                    "Route table reloaded in-process"
                );
                let event = Job::RouteTableUpdated(RouteTableUpdatedJob {
                    environment_id,
                    deployment_id,
                    route_count,
                });
                if let Err(e) = queue.send(event).await {
                    error!(
                        "Failed to publish RouteTableUpdated after in-process reload: {}",
                        e
                    );
                }
            }
            Err(e) => {
                error!(
                    environment_id = ?environment_id,
                    deployment_id = ?deployment_id,
                    "In-process route reload failed: {}",
                    e
                );
            }
        }
    }

    /// Stop the background task.
    pub fn shutdown(&self) {
        if let Ok(mut guard) = self.task_handle.lock() {
            if let Some(handle) = guard.take() {
                handle.abort();
                info!("Route reload subscriber stopped");
            }
        }
    }
}

impl Drop for RouteReloadSubscriber {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::broadcast;

    /// A broadcast-backed test queue that lets us count `send` calls and drive
    /// the subscriber, mirroring the production `BroadcastQueueService`.
    struct TestQueue {
        tx: broadcast::Sender<Job>,
        route_updates: Arc<AtomicUsize>,
    }

    struct TestReceiver {
        rx: broadcast::Receiver<Job>,
    }

    #[temps_core::async_trait::async_trait]
    impl temps_core::JobReceiver for TestReceiver {
        async fn recv(&mut self) -> Result<Job, temps_core::QueueError> {
            self.rx.recv().await.map_err(|e| match e {
                broadcast::error::RecvError::Closed => temps_core::QueueError::ChannelClosed,
                broadcast::error::RecvError::Lagged(n) => {
                    temps_core::QueueError::ReceiveError(format!("lagged by {n}"))
                }
            })
        }
    }

    #[temps_core::async_trait::async_trait]
    impl JobQueue for TestQueue {
        async fn send(&self, job: Job) -> Result<(), temps_core::QueueError> {
            if matches!(job, Job::RouteTableUpdated(_)) {
                self.route_updates.fetch_add(1, Ordering::SeqCst);
            }
            // Ignore "no receivers" — tests may not always have one attached.
            let _ = self.tx.send(job);
            Ok(())
        }

        fn subscribe(&self) -> Box<dyn temps_core::JobReceiver> {
            Box::new(TestReceiver {
                rx: self.tx.subscribe(),
            })
        }
    }

    fn test_peer_table() -> Arc<CachedPeerTable> {
        // Disconnected DB: load_routes() will error, which is fine for the
        // tests below — they assert on subscriber wiring, not on route content.
        // The reload-failure path simply logs and publishes no confirmation.
        let db = Arc::new(sea_orm::DatabaseConnection::Disconnected);
        Arc::new(CachedPeerTable::new(db))
    }

    #[test]
    fn test_new_has_no_task() {
        let (tx, _rx) = broadcast::channel(16);
        let queue: Arc<dyn JobQueue> = Arc::new(TestQueue {
            tx,
            route_updates: Arc::new(AtomicUsize::new(0)),
        });
        let sub = RouteReloadSubscriber::new(test_peer_table(), queue);
        assert!(sub.task_handle.lock().unwrap().is_none());
    }

    #[test]
    fn test_shutdown_without_start_is_safe() {
        let (tx, _rx) = broadcast::channel(16);
        let queue: Arc<dyn JobQueue> = Arc::new(TestQueue {
            tx,
            route_updates: Arc::new(AtomicUsize::new(0)),
        });
        let sub = RouteReloadSubscriber::new(test_peer_table(), queue);
        sub.shutdown();
        assert!(sub.task_handle.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn test_start_registers_task_handle() {
        let (tx, _rx) = broadcast::channel(16);
        let queue: Arc<dyn JobQueue> = Arc::new(TestQueue {
            tx,
            route_updates: Arc::new(AtomicUsize::new(0)),
        });
        let sub = RouteReloadSubscriber::new(test_peer_table(), queue);
        sub.start();
        assert!(
            sub.task_handle.lock().unwrap().is_some(),
            "start() must register a task handle"
        );
        sub.shutdown();
    }

    #[tokio::test]
    async fn test_ignores_unrelated_jobs() {
        // An unrelated job must NOT trigger a reload confirmation.
        let (tx, _rx) = broadcast::channel(16);
        let route_updates = Arc::new(AtomicUsize::new(0));
        let queue: Arc<dyn JobQueue> = Arc::new(TestQueue {
            tx,
            route_updates: route_updates.clone(),
        });
        let sub = RouteReloadSubscriber::new(test_peer_table(), queue.clone());
        sub.start();

        // Publish an unrelated job and give the task a moment to observe it.
        queue
            .send(Job::CustomDomainAdded("example.com".to_string()))
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        assert_eq!(
            route_updates.load(Ordering::SeqCst),
            0,
            "unrelated jobs must not produce a RouteTableUpdated"
        );
        sub.shutdown();
    }
}
