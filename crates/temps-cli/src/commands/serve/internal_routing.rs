// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The control plane serves `<environment>.<project>.temps.local` to its own
//! containers, as a worker serves its own (ADR-104).
//!
//! Nothing here decides who may call what. The control plane runs the
//! worker's code unchanged:
//!
//! - [`temps_node_agent::internal_routing::supervise`] binds the internal
//!   proxy on port 591 of the app network's gateway, redirects containers'
//!   port-80 calls to it with nftables, and falls back to the gateway's port
//!   80 when it cannot (ADR-098). It never listens on `0.0.0.0`.
//! - Its routes are a [`RouteStore`] fed from this process's route table by
//!   [`temps_routes::route_sync::internal_route_entries`], the function that
//!   builds a worker's snapshot, grants included (ADR 101 D5). The proxy
//!   identifies callers through the local Docker daemon and decides with
//!   ADR 101's `internal_access::authorize`, exactly as on a worker.
//!
//! What a worker gets over the long poll, the feeder here reads in process:
//! it rebuilds the store on every route-table generation bump (deployments,
//! and grant changes through `route_table_changes`) and at least once a
//! minute.

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use temps_node_agent::route_store::{RouteBackend, RouteEntry, RouteStore};
use temps_routes::route_sync::RouteEntryDto;
use temps_routes::CachedPeerTable;
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

/// Rebuild the store at least this often, so a lost wakeup cannot leave a
/// revoked grant in force for longer than the route table's own idle
/// reconcile would.
const SAFETY_REFRESH: Duration = Duration::from_secs(60);

/// Where the store keeps its last snapshot, so a restart serves the last
/// routes before the first rebuild.
pub(crate) fn snapshot_path(data_dir: &Path) -> PathBuf {
    data_dir.join("internal-routing").join("routes.json")
}

/// A worker's route entry from the snapshot DTO: the same fields, by name, so
/// the control plane's proxy sees what a worker's would.
pub(crate) fn route_entry(dto: RouteEntryDto) -> RouteEntry {
    RouteEntry {
        host: dto.host,
        backends: dto
            .backends
            .into_iter()
            .map(|backend| RouteBackend {
                address: backend.address,
                container_id: backend.container_id,
                container_name: backend.container_name,
            })
            .collect(),
        deployment_id: dto.deployment_id,
        project_id: dto.project_id,
        environment_id: dto.environment_id,
        // Internal routes carry neither: the gate and Shield are public
        // ingress concerns, and `internal_route_entries` never sets them.
        gate: None,
        shield: None,
        allowed_callers: dto.allowed_callers,
    }
}

/// Apply what the route table holds now to `store`. Returns the routes
/// applied.
async fn refresh(
    store: &RouteStore,
    peer_table: &CachedPeerTable,
    db: &sea_orm::DatabaseConnection,
    generation: u64,
) -> Result<usize, sea_orm::DbErr> {
    let routes: Vec<RouteEntry> = temps_routes::route_sync::internal_route_entries(peer_table, db)
        .await?
        .into_iter()
        .map(route_entry)
        .collect();
    let count = routes.len();
    store.apply_snapshot(generation, routes);
    Ok(count)
}

/// Keep `store` equal to the route table, until aborted.
async fn feed(
    store: Arc<RouteStore>,
    peer_table: Arc<CachedPeerTable>,
    db: Arc<sea_orm::DatabaseConnection>,
) {
    let notifier = peer_table.generation_notifier();
    let mut applied: Option<u64> = None;
    loop {
        // Armed before reading the generation, so a bump between the read
        // and the wait is not missed.
        let notified = notifier.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        let generation = peer_table.current_generation();
        match refresh(&store, &peer_table, &db, generation).await {
            Ok(routes) => {
                if applied != Some(generation) {
                    debug!(generation, routes, "control-plane internal routes applied");
                }
                applied = Some(generation);
            }
            Err(error) => warn!(
                error = %error,
                generation,
                "could not read internal access grants for the control plane's internal proxy; \
                 it keeps serving the last routes it applied"
            ),
        }
        tokio::select! {
            _ = &mut notified => {}
            _ = tokio::time::sleep(SAFETY_REFRESH) => {}
        }
    }
}

/// The internal proxy on one gateway, and the feeder of its routes.
pub(crate) struct ControlPlaneRouting {
    gateway: IpAddr,
    shutdown: Arc<Notify>,
    supervisor: JoinHandle<()>,
    feeder: JoinHandle<()>,
}

impl ControlPlaneRouting {
    /// Start serving on `gateway`. Must be called within a Tokio runtime.
    pub(crate) fn start(
        gateway: IpAddr,
        peer_table: Arc<CachedPeerTable>,
        db: Arc<sea_orm::DatabaseConnection>,
        docker: bollard::Docker,
        data_dir: &Path,
    ) -> Self {
        let path = snapshot_path(data_dir);
        if let Some(parent) = path.parent() {
            if let Err(error) = std::fs::create_dir_all(parent) {
                // The store still serves from memory; only the restart
                // snapshot is lost.
                warn!(path = %parent.display(), error = %error, "cannot create the internal routing snapshot directory");
            }
        }
        let store = Arc::new(RouteStore::new(path));
        store.load_from_disk();
        let feeder = tokio::spawn(feed(store.clone(), peer_table, db));
        let shutdown = Arc::new(Notify::new());
        let supervisor = tokio::spawn(temps_node_agent::internal_routing::supervise(
            gateway,
            store,
            docker,
            shutdown.clone(),
        ));
        info!(%gateway, "control-plane internal routing started (*.temps.local served to local containers)");
        Self {
            gateway,
            shutdown,
            supervisor,
            feeder,
        }
    }

    pub(crate) fn gateway(&self) -> IpAddr {
        self.gateway
    }

    /// What the supervisor's last pass found, in the heartbeat document's
    /// shape (ADR-098 D5), for node 0's DNS report.
    pub(crate) fn report() -> Option<serde_json::Value> {
        temps_node_agent::internal_routing::current_status()
            .and_then(|status| status.heartbeat_value())
    }

    /// Stop serving. The redirect is left to expire on its own (ADR-098 D3):
    /// until then calls are refused, never delivered elsewhere.
    pub(crate) async fn stop(self) {
        self.feeder.abort();
        self.shutdown.notify_one();
        if tokio::time::timeout(Duration::from_secs(10), self.supervisor)
            .await
            .is_err()
        {
            warn!(gateway = %self.gateway, "control-plane internal routing did not stop within 10 s");
        }
        temps_node_agent::internal_routing::clear_status();
        info!(gateway = %self.gateway, "control-plane internal routing stopped");
    }
}

#[cfg(test)]
mod tests;
