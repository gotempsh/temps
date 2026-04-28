//! Long-poll client that mirrors the CP's internal-zone route table
//! into [`RouteStore`].
//!
//! Same shape as `temps-dns-resolver::sync_client`: one tokio task,
//! `since=current_generation`, the CP holds the request open until a
//! reload happens or its long-poll deadline fires, the client
//! `apply_snapshot`s the result and then ACKs. Restart-resilient via
//! the disk snapshot in [`RouteStore::load_from_disk`].
//!
//! ## Backoff
//!
//! On any error (network, 5xx, parse failure) we sleep with exponential
//! backoff capped at 30s before retrying. We don't ACK on error, so the
//! CP's view of `applied_generation` can lag during outages — that's
//! correct: it lets ops detect drift. ACK on success.
//!
//! ## CP restart
//!
//! When the CP restarts its in-memory generation resets (today). Our
//! `since` may be > server `current`, in which case the handler still
//! returns a snapshot at the new `current`. We accept whatever
//! generation the server reports and apply unconditionally — the
//! snapshot itself is the source of truth, the number is just a
//! wakeup hint.

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use tracing::{debug, info, warn};

use crate::route_store::{RouteBackend, RouteEntry, SharedRouteStore};

#[derive(Debug, Clone, Deserialize)]
struct SnapshotResponse {
    generation: u64,
    routes: Vec<SnapshotRoute>,
}

#[derive(Debug, Clone, Deserialize)]
struct SnapshotRoute {
    host: String,
    backends: Vec<SnapshotBackend>,
    #[serde(default)]
    deployment_id: Option<i32>,
    #[serde(default)]
    project_id: Option<i32>,
    #[serde(default)]
    environment_id: Option<i32>,
}

#[derive(Debug, Clone, Deserialize)]
struct SnapshotBackend {
    address: String,
    #[serde(default)]
    container_id: Option<String>,
    #[serde(default)]
    container_name: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct AckRequest {
    applied_generation: u64,
}

pub struct RouteSyncClient {
    /// Base URL of the control plane, no trailing slash.
    pub control_plane_url: String,
    /// This node's id (matches the bearer token's owner).
    pub node_id: i32,
    /// Bearer token for `/internal/...` endpoints.
    pub node_token: String,
    pub store: SharedRouteStore,
    pub shutdown: Arc<Notify>,
    /// HTTP client. Long-poll requests can take up to ~25s on the CP
    /// side, so the client timeout must be a comfortable margin
    /// above that.
    pub http: reqwest::Client,
}

impl RouteSyncClient {
    /// Create a sync client. The reqwest client is built with a
    /// 60s request timeout — long-poll on the CP is 25s, plus
    /// transport, plus a margin.
    pub fn new(
        control_plane_url: String,
        node_id: i32,
        node_token: String,
        store: SharedRouteStore,
        shutdown: Arc<Notify>,
    ) -> Result<Self, reqwest::Error> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()?;
        Ok(Self {
            control_plane_url,
            node_id,
            node_token,
            store,
            shutdown,
            http,
        })
    }

    /// Run forever. Returns when `shutdown` is notified.
    pub async fn run(self) {
        // Backoff state for error recovery.
        let mut backoff = Duration::from_secs(1);
        let max_backoff = Duration::from_secs(30);

        loop {
            // Cooperative shutdown check before each round.
            tokio::select! {
                biased;
                _ = self.shutdown.notified() => {
                    info!("route sync client shutting down");
                    return;
                }
                res = self.tick_once() => {
                    match res {
                        Ok(()) => {
                            // Reset backoff after any successful round.
                            backoff = Duration::from_secs(1);
                        }
                        Err(e) => {
                            warn!(error = %e, ?backoff, "route sync tick failed");
                            tokio::select! {
                                _ = self.shutdown.notified() => return,
                                _ = tokio::time::sleep(backoff) => {}
                            }
                            backoff = (backoff * 2).min(max_backoff);
                        }
                    }
                }
            }
        }
    }

    async fn tick_once(&self) -> Result<(), String> {
        let since = self.store.current_generation();
        let url = format!(
            "{}/api/internal/nodes/{}/routes/snapshot?since={}",
            self.control_plane_url, self.node_id, since
        );

        let resp = self
            .http
            .get(&url)
            .bearer_auth(&self.node_token)
            .send()
            .await
            .map_err(|e| format!("GET {url}: {e}"))?;

        if !resp.status().is_success() {
            return Err(format!("CP returned {} for {url}", resp.status()));
        }

        let body: SnapshotResponse = resp
            .json()
            .await
            .map_err(|e| format!("parse snapshot: {e}"))?;

        // Apply unconditionally — even when generation is unchanged
        // (long-poll timeout), this no-ops past the equality check on
        // CP, and we don't have to special-case anything here. The
        // store's apply is idempotent for an unchanged set.
        if body.generation != since || self.store.is_empty() {
            let routes: Vec<RouteEntry> = body
                .routes
                .into_iter()
                .map(|r| RouteEntry {
                    host: r.host,
                    backends: r
                        .backends
                        .into_iter()
                        .map(|b| RouteBackend {
                            address: b.address,
                            container_id: b.container_id,
                            container_name: b.container_name,
                        })
                        .collect(),
                    deployment_id: r.deployment_id,
                    project_id: r.project_id,
                    environment_id: r.environment_id,
                })
                .collect();
            let applied = self.store.apply_snapshot(body.generation, routes);
            self.ack(applied).await.ok();
        } else {
            debug!(generation = body.generation, "route snapshot unchanged");
        }

        Ok(())
    }

    async fn ack(&self, applied_generation: u64) -> Result<(), String> {
        let url = format!(
            "{}/api/internal/nodes/{}/routes/ack",
            self.control_plane_url, self.node_id
        );
        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.node_token)
            .json(&AckRequest { applied_generation })
            .send()
            .await
            .map_err(|e| format!("POST {url}: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("CP returned {} for {url}", resp.status()));
        }
        Ok(())
    }
}
