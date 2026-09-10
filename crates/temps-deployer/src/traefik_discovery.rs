// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Live Traefik-label route discovery for containers Temps did not deploy.
//!
//! Points Temps' Pingora proxy at an existing docker-compose / Coolify /
//! Dokploy stack: those almost always already carry `traefik.*` labels, so the
//! operator changes nothing about their containers and gets working routes.
//!
//! # How it stays in sync
//!
//! Discovered routes are **persisted** as `traefik_discovered_routes` rows
//! rather than injected into the in-memory table. That is the whole point:
//! the existing `notify_route_table_change()` trigger →
//! `pg_notify('route_table_changes')` → `CachedPeerTable::load_routes()`
//! machinery then propagates every change in-process *and* to every other
//! control plane node, with no bespoke fan-out to maintain.
//!
//! Two drivers keep the rows fresh:
//!
//! * a **Docker events subscription** (`start`/`die`/`stop`/`destroy`/`update`)
//!   for immediate, per-container updates, and
//! * a **periodic full reconciliation** (default 30s) as the safety net for
//!   missed/dropped events, for a wedged event socket, and for picking up the
//!   world after Temps itself restarts.
//!
//! # Safety properties
//!
//! * **Temps-owned containers are skipped.** Anything carrying
//!   `sh.temps.deploy_id` already has a route from the deployment path.
//!   Re-deriving one here would let a workload rewrite its own routing through
//!   labels it controls, and would produce a second, conflicting backend for
//!   the same host.
//! * **Discovered routes never overwrite a real route.** If a discovered host
//!   already belongs to an environment domain, project custom domain, custom
//!   route, environment subdomain, or the console hostname, the discovery is
//!   *skipped* and recorded as a conflict — never written. `load_routes()`
//!   applies the same precedence again as a last line of defence.
//! * **Opt-in only.** Off unless `TEMPS_TRAEFIK_DISCOVERY_ENABLED=true`:
//!   it changes routing for containers the operator never deployed through
//!   Temps, so it must be an explicit decision. The *reader* enforces this too:
//!   `load_routes()` serves discovered rows only for the network this process
//!   is configured to adopt from, and none at all when discovery is off — so
//!   turning discovery off (or repointing it) actually stops serving what it
//!   adopted, rather than leaving orphaned rows routing forever with no
//!   reconciler left to delete them.
//! * **Ports are not taken on trust.** A `loadbalancer.server.port` label is
//!   only honoured when the container really exposes that port, and on a
//!   baremetal install the route is skipped unless the container publishes a
//!   host port. Both exist because the resolved port ends up in a backend
//!   address: without them, container-controlled labels could aim a hostname
//!   at an arbitrary loopback port on the Temps host.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use bollard::query_parameters::{EventsOptions, InspectContainerOptions, ListContainersOptions};
use bollard::Docker;
use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectionTrait, DatabaseConnection, DbErr,
    EntityTrait, IntoActiveModel, QueryFilter, Statement,
};
use temps_core::route_table::RouteTableRefresher;
use temps_core::{AppSettings, PublicHostnameStrategy};
use temps_entities::traefik_discovered_routes as discovered;
use temps_entities::traefik_route_certificates as route_certs;
// No direct dependency on temps-monitoring here: that crate depends on
// temps-deployer itself, which would create a cycle.  Instead we define a
// narrow trait that the alarm service adapter (wired in console.rs) implements.
use thiserror::Error;
use tracing::{debug, error, info, warn};

use crate::traefik_labels::{self, ResolvedRouter};

/// Narrow trait for firing a Critical alarm when certificate drift is detected.
///
/// This is the seam between `temps-deployer` and `temps-monitoring`.
/// `temps-monitoring` depends on `temps-deployer`, so we cannot depend on it
/// directly (cycle).  `console.rs` bridges the gap by creating an adapter that
/// wraps the concrete `AlarmService` and injects it via
/// [`TraefikDiscoveryService::inject_alarm_sink`].
#[async_trait::async_trait]
pub trait DriftAlarmSink: Send + Sync {
    /// Fire a Critical alarm for the given drift event.
    /// Errors are logged by the implementation; callers do not need to handle them.
    async fn notify_container_drift(
        &self,
        host: String,
        authorized_container: String,
        current_container: String,
    );
}

/// Environment variable that opts an installation into label discovery.
pub const ENABLED_ENV: &str = "TEMPS_TRAEFIK_DISCOVERY_ENABLED";
/// Environment variable overriding which Docker network is watched.
pub const NETWORK_ENV: &str = "TEMPS_TRAEFIK_DISCOVERY_NETWORK";

/// Interval of the safety-net full reconciliation pass.
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(30);
/// Backoff before re-subscribing after the Docker events stream drops.
const EVENTS_RECONNECT_BACKOFF: Duration = Duration::from_secs(5);

/// Container events that can change whether a container is routable.
const WATCHED_ACTIONS: [&str; 5] = ["start", "die", "stop", "destroy", "update"];

/// How many poll intervals a row for a *different* network must go unrefreshed
/// before startup/periodic reconciliation prunes it.
///
/// Rows for another network are almost always leftovers from a previous
/// configuration of this same node (discovery was repointed or turned off).
/// But this table has no owner column, so in a multi-node install they could
/// equally belong to a peer that is watching that other network right now — and
/// a peer refreshes `last_seen_at` on every one of its own passes. Requiring
/// the row to be stale by a wide margin is therefore the boundary that
/// distinguishes "left over from my own old config" from "someone else's live
/// row", without needing a node/owner column.
const FOREIGN_NETWORK_STALE_INTERVALS: u32 = 10;

#[derive(Debug, Error)]
pub enum TraefikDiscoveryError {
    #[error(
        "Docker API error during Traefik discovery ({operation}) on network '{network}': {reason}"
    )]
    Docker {
        operation: String,
        network: String,
        reason: String,
    },

    #[error(
        "Database error during Traefik discovery on network '{network}' ({operation}): {source}"
    )]
    Database {
        network: String,
        operation: String,
        #[source]
        source: DbErr,
    },
}

/// Runtime configuration for the discovery watcher.
///
/// These are process-wide operator/ops knobs (which Docker network this host
/// may adopt containers from, and whether adoption happens at all), not
/// per-tenant configuration — the same category as
/// `TEMPS_ALLOWED_POSTGRES_DOCKER_IMAGES`. They are read once at startup;
/// changing them requires a restart.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TraefikDiscoveryConfig {
    /// Whether discovery runs at all. Default `false`.
    pub enabled: bool,
    /// Docker network whose containers are inspected for Traefik labels.
    pub network: String,
    /// Interval of the full reconciliation safety net.
    pub poll_interval: Duration,
}

impl TraefikDiscoveryConfig {
    /// Read configuration from the environment.
    ///
    /// `default_network` should be the network Temps' own workloads already
    /// run on (the value `temps serve` passes to `DockerRuntime::new`), so an
    /// operator who only sets `TEMPS_TRAEFIK_DISCOVERY_ENABLED=true` watches a
    /// network the proxy can actually reach.
    pub fn from_env(default_network: &str) -> Self {
        Self::resolve(
            std::env::var(ENABLED_ENV).ok().as_deref(),
            std::env::var(NETWORK_ENV).ok().as_deref(),
            default_network,
        )
    }

    /// Pure resolution of the two knobs, split out from [`Self::from_env`] so
    /// the precedence rules are unit-testable without mutating process-global
    /// environment state from parallel tests.
    pub fn resolve(
        enabled_raw: Option<&str>,
        network_raw: Option<&str>,
        default_network: &str,
    ) -> Self {
        let enabled = enabled_raw.is_some_and(|v| v.trim().eq_ignore_ascii_case("true"));
        let network = network_raw
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| default_network.to_string());
        Self {
            enabled,
            network,
            poll_interval: DEFAULT_POLL_INTERVAL,
        }
    }
}

/// Why a discovered host was not adopted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConflictReason {
    /// The host already resolves to a Temps-owned route (deployment,
    /// environment domain, custom route, custom domain, or the console).
    OwnedByTempsRoute,
    /// Two discovered containers claimed the same host; the first won.
    ClaimedByAnotherContainer { winner_container_name: String },
}

impl std::fmt::Display for ConflictReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConflictReason::OwnedByTempsRoute => {
                write!(f, "host already belongs to a Temps-managed route")
            }
            ConflictReason::ClaimedByAnotherContainer {
                winner_container_name,
            } => write!(
                f,
                "host already claimed by discovered container '{winner_container_name}'"
            ),
        }
    }
}

/// A discovered host that was deliberately not adopted. Surfaced through
/// [`TraefikDiscoveryService::last_outcome`] so an operator can see *why* their
/// labelled container isn't being routed instead of guessing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostConflict {
    pub host: String,
    pub container_id: String,
    pub container_name: String,
    pub router_name: String,
    pub reason: ConflictReason,
}

/// Result of one full reconciliation pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReconcileOutcome {
    pub network: String,
    pub containers_scanned: usize,
    pub skipped_temps_managed: usize,
    pub routes_upserted: usize,
    pub routes_unchanged: usize,
    pub routes_removed: usize,
    pub conflicts: Vec<HostConflict>,
    pub completed_at: DateTime<Utc>,
}

impl ReconcileOutcome {
    /// Whether this pass changed anything the route table reads.
    pub fn changed(&self) -> bool {
        self.routes_upserted > 0 || self.routes_removed > 0
    }
}

/// A route candidate derived from one container's labels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RouteCandidate {
    host: String,
    router_name: String,
    container_id: String,
    container_name: String,
    port: u16,
    host_port: Option<u16>,
    tls: bool,
}

impl RouteCandidate {
    /// Compare against a stored row on the fields the route table reads.
    /// `last_seen_at`/`updated_at` are deliberately excluded — a heartbeat
    /// must not look like a route change.
    fn matches_row(&self, row: &discovered::Model, network: &str) -> bool {
        row.router_name == self.router_name
            && row.target_container_id == self.container_id
            && row.target_container_name == self.container_name
            && row.target_port == i32::from(self.port)
            && row.target_host_port == self.host_port.map(i32::from)
            && row.network == network
            && row.tls == self.tls
    }
}

/// Normalized view of a container, built from either a list summary or a full
/// inspect response so both code paths share one evaluation function.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ContainerView {
    id: String,
    name: String,
    labels: HashMap<String, String>,
    /// Distinct TCP ports the container exposes, sorted.
    exposed_ports: Vec<u16>,
    /// container port -> host-published port.
    published_ports: HashMap<u16, u16>,
    on_target_network: bool,
    running: bool,
}

/// Background service that keeps `traefik_discovered_routes` in sync with the
/// containers on a Docker network.
pub struct TraefikDiscoveryService {
    docker: Arc<Docker>,
    db: Arc<DatabaseConnection>,
    config: TraefikDiscoveryConfig,
    /// Reload hook so an in-process route table reflects a discovery write
    /// immediately, without waiting for the PG NOTIFY round trip. Optional
    /// because a console-only process may not own a route table.
    refresher: Option<Arc<dyn RouteTableRefresher>>,
    last_outcome: std::sync::RwLock<Option<ReconcileOutcome>>,
    /// Container IDs this node currently has `traefik_discovered_routes` rows
    /// for, or `None` before the first reconciliation has established the set.
    ///
    /// The Docker events subscription cannot be filtered down to "containers on
    /// the watched network" server-side, so *every* container event on the host
    /// — including every container of every unrelated stack — reaches
    /// [`TraefikDiscoveryService::handle_container_event`]. Without this set,
    /// each one issued a `DELETE ... WHERE target_container_id = ...` that
    /// matched nothing, which (before the row-level trigger fix) also NOTIFYed
    /// every control plane node into a full route-table reload. Consulting an
    /// in-memory set first turns those into zero database round trips.
    ///
    /// `None` means "not primed yet": fall back to querying, because an empty
    /// set and an unknown set must not be confused.
    tracked_containers: std::sync::RwLock<Option<HashSet<String>>>,
    /// ADR-041 §2a: alarm sink injected after construction (via
    /// [`Self::inject_alarm_sink`]) so the watcher fires a Critical alarm
    /// when certificate drift is detected — not just a log line.
    ///
    /// `OnceLock` is used because the service is constructed before the plugin
    /// system (which registers `AlarmService`) has run. The first reconcile
    /// pass starts later; by that point the sink is always set.
    alarm_sink: std::sync::OnceLock<Arc<dyn DriftAlarmSink>>,
    /// Short-lived cache for the reserved-host set so that `handle_container_event`
    /// (called on every Docker event, including events for containers on
    /// unrelated networks) does not execute 5 full-table scans per event.
    ///
    /// The cache is written at the start of each `reconcile()` pass and on the
    /// first `handle_container_event` call in a new interval. Stale for at most
    /// `RESERVED_HOSTS_CACHE_TTL`, which is bounded by the reconcile interval
    /// (default 30 s). Per the hot-loop rule (CLAUDE.md "Background loops must
    /// be O(changes), not O(total)"), event-driven paths must be O(changes).
    reserved_hosts_cache: std::sync::RwLock<Option<(Arc<ReservedHosts>, std::time::Instant)>>,
}

/// ADR-041 §2a: Free function core of drift detection. Separated from
/// `TraefikDiscoveryService` so unit tests can exercise it via `MockDatabase`
/// without needing a live Docker daemon.
///
/// Rules:
/// - Only checks hosts on `network`.
/// - Drift when a `cert_authorized = true` row's `authorized_container_id`
///   differs from the current candidate's `container_id`.
/// - When a certified host has *no* current candidate (container left the
///   network entirely), that is also drift.
/// - `cert_authorized` is NEVER cleared: auto-clearing would not remove the
///   certificate and would be a DoS primitive (ADR-041 §2a).
/// - `last_drift_alarmed_container_id` deduplicates: the alarm fires at most
///   once per unique current container so a steady-state drift is not noisy.
pub(crate) async fn check_certificate_drift_for(
    db: &DatabaseConnection,
    network: &str,
    candidates: &HashMap<String, RouteCandidate>,
    alarm_sink: Option<&dyn DriftAlarmSink>,
) -> Result<(), TraefikDiscoveryError> {
    let cert_rows = route_certs::Entity::find()
        .filter(route_certs::Column::CertAuthorized.eq(true))
        .filter(route_certs::Column::AuthorizedNetwork.eq(network.to_string()))
        .all(db)
        .await
        .map_err(|source| TraefikDiscoveryError::Database {
            network: network.to_string(),
            operation: "load cert rows for drift check".to_string(),
            source,
        })?;

    if cert_rows.is_empty() {
        return Ok(());
    }

    let now = Utc::now();

    for cert in cert_rows {
        let current_container_id = candidates.get(&cert.host).map(|c| c.container_id.as_str());

        let is_drift = match current_container_id {
            Some(cid) => cid != cert.authorized_container_id.as_str(),
            None => true,
        };

        if !is_drift {
            continue;
        }

        // Deduplicate: only alarm for a given current container ID once.
        let already_alarmed = cert
            .last_drift_alarmed_container_id
            .as_deref()
            .is_some_and(|id| Some(id) == current_container_id);
        if already_alarmed {
            continue;
        }

        let current_container_name = candidates
            .get(&cert.host)
            .map(|c| c.container_name.as_str())
            .unwrap_or("<none — container left network>");

        warn!(
            host = %cert.host,
            authorized_container = %cert.authorized_container_name,
            current_container = %current_container_name,
            "Certificate drift detected: the container serving this host is no \
             longer the one that was authorized for TLS. Operator review required."
        );

        // ADR-041 §2a: a warn! line is not unmissable. Fire a Critical alarm so
        // the operator is paged regardless of whether they are tailing logs.
        if let Some(sink) = alarm_sink {
            sink.notify_container_drift(
                cert.host.clone(),
                cert.authorized_container_name.clone(),
                current_container_name.to_string(),
            )
            .await;
        }

        let mut active = cert.into_active_model();
        active.container_drift_detected_at = Set(Some(now));
        active.last_drift_alarmed_container_id = Set(current_container_id.map(str::to_string));
        active
            .update(db)
            .await
            .map_err(|source| TraefikDiscoveryError::Database {
                network: network.to_string(),
                operation: "recording certificate drift".to_string(),
                source,
            })?;
    }

    Ok(())
}

impl TraefikDiscoveryService {
    pub fn new(
        docker: Arc<Docker>,
        db: Arc<DatabaseConnection>,
        config: TraefikDiscoveryConfig,
        refresher: Option<Arc<dyn RouteTableRefresher>>,
    ) -> Self {
        Self {
            docker,
            db,
            config,
            refresher,
            last_outcome: std::sync::RwLock::new(None),
            tracked_containers: std::sync::RwLock::new(None),
            alarm_sink: std::sync::OnceLock::new(),
            reserved_hosts_cache: std::sync::RwLock::new(None),
        }
    }

    /// ADR-041 §2a: inject the alarm sink so certificate-drift events reach the
    /// operator via the alarm system, not just as log lines. Called from
    /// `console.rs` after the plugin system has registered `AlarmService`.
    pub fn inject_alarm_sink(&self, sink: Arc<dyn DriftAlarmSink>) {
        // Ignore if already set (e.g. in tests that call inject twice).
        let _ = self.alarm_sink.set(sink);
    }

    pub fn config(&self) -> &TraefikDiscoveryConfig {
        &self.config
    }

    /// Whether this container is known to have no rows in
    /// `traefik_discovered_routes`, cheaply and without touching the database.
    ///
    /// `true` only when the tracked set has been primed by a reconciliation
    /// *and* the container is absent from it. Before priming this always
    /// returns `false`, so an event that arrives during startup still takes the
    /// (correct, slower) database path.
    fn known_untracked(&self, container_id: &str) -> bool {
        self.tracked_containers
            .read()
            .ok()
            .and_then(|guard| {
                guard
                    .as_ref()
                    .map(|tracked| !tracked.contains(container_id))
            })
            .unwrap_or(false)
    }

    /// Replace the tracked-container set with the authoritative one from a
    /// completed reconciliation.
    fn set_tracked_containers(&self, ids: HashSet<String>) {
        if let Ok(mut guard) = self.tracked_containers.write() {
            *guard = Some(ids);
        }
    }

    /// Record that a container now has (or no longer has) rows, so incremental
    /// event handling stays consistent between full reconciliations.
    fn mark_tracked(&self, container_id: &str, tracked: bool) {
        if let Ok(mut guard) = self.tracked_containers.write() {
            if let Some(set) = guard.as_mut() {
                if tracked {
                    set.insert(container_id.to_string());
                } else {
                    set.remove(container_id);
                }
            }
        }
    }

    /// Snapshot of the most recent reconciliation, including host conflicts.
    /// Read by the (follow-up) status endpoint; also useful in tests.
    pub fn last_outcome(&self) -> Option<ReconcileOutcome> {
        self.last_outcome
            .read()
            .ok()
            .and_then(|guard| guard.clone())
    }

    /// Start the background tasks: an immediate reconciliation, then the
    /// Docker events subscription and the periodic safety-net loop.
    ///
    /// The immediate reconciliation (the ticker's first tick fires without
    /// delay) is also what purges rows this node adopted under a *previous*
    /// discovery network — see
    /// [`TraefikDiscoveryService::prune_foreign_network_rows`]. Repointing
    /// `TEMPS_TRAEFIK_DISCOVERY_NETWORK` and restarting therefore converges,
    /// rather than leaving the old network's adopted rows behind forever.
    ///
    /// No-op when discovery is disabled. Both loops are detached; the caller
    /// keeps the returned handles only if it wants to abort them.
    pub fn start(
        self: Arc<Self>,
        runtime: &tokio::runtime::Handle,
    ) -> Vec<tokio::task::JoinHandle<()>> {
        if !self.config.enabled {
            debug!(
                "Traefik label discovery disabled (set {}=true to enable)",
                ENABLED_ENV
            );
            return Vec::new();
        }

        info!(
            network = %self.config.network,
            poll_interval_secs = self.config.poll_interval.as_secs(),
            "Starting Traefik label discovery — containers on this network carrying \
             traefik.enable=true will be routed by Temps"
        );

        let poll_service = Arc::clone(&self);
        let poll_handle = runtime.spawn(async move {
            let mut ticker = tokio::time::interval(poll_service.config.poll_interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                if let Err(e) = poll_service.reconcile().await {
                    error!(
                        network = %poll_service.config.network,
                        error = %e,
                        "Traefik discovery reconciliation failed; will retry on the next tick"
                    );
                }
            }
        });

        let events_service = Arc::clone(&self);
        let events_handle = runtime.spawn(async move {
            events_service.run_events_loop().await;
        });

        vec![poll_handle, events_handle]
    }

    /// Full reconciliation: list the containers on the target network, derive
    /// routes, and converge the `traefik_discovered_routes` rows for this
    /// network onto that set.
    pub async fn reconcile(&self) -> Result<ReconcileOutcome, TraefikDiscoveryError> {
        // Invalidate the reserved-host cache so this reconcile sees current DB
        // state, not a stale cache left over from incremental event handling.
        // The cache is refreshed when `reserved_hosts()` is called below, and
        // subsequent `handle_container_event` calls within the TTL window reuse it.
        self.invalidate_reserved_hosts_cache();

        // Rows this node adopted under a *previous* discovery configuration
        // (a different `TEMPS_TRAEFIK_DISCOVERY_NETWORK`) are nobody's job to
        // clean up otherwise: the reconciler only ever diffs its own network,
        // so they would sit in the table forever. `load_routes()` already
        // refuses to serve them, this makes them actually go away.
        self.prune_foreign_network_rows().await?;

        let views = self.list_network_containers().await?;
        let containers_scanned = views.len();
        let skipped_temps_managed = views
            .iter()
            .filter(|v| traefik_labels::is_temps_managed(&v.labels))
            .count();

        let reserved = self.reserved_hosts().await?;

        // Deterministic order: whoever sorts first by container name wins a
        // contested host, so a restart can't flip which container serves it.
        let mut ordered: Vec<&ContainerView> = views.iter().collect();
        ordered.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));

        let mut candidates: HashMap<String, RouteCandidate> = HashMap::new();
        let mut conflicts: Vec<HostConflict> = Vec::new();

        for view in ordered {
            for candidate in evaluate_container(view) {
                if reserved.is_reserved(&candidate.host) {
                    warn!(
                        host = %candidate.host,
                        container = %candidate.container_name,
                        router = %candidate.router_name,
                        "Skipping Traefik-labelled container: host already belongs to a \
                         Temps-managed route. The existing route is kept."
                    );
                    conflicts.push(HostConflict {
                        host: candidate.host,
                        container_id: candidate.container_id,
                        container_name: candidate.container_name,
                        router_name: candidate.router_name,
                        reason: ConflictReason::OwnedByTempsRoute,
                    });
                    continue;
                }
                match candidates.entry(candidate.host.clone()) {
                    std::collections::hash_map::Entry::Occupied(existing) => {
                        let winner = existing.get().container_name.clone();
                        warn!(
                            host = %candidate.host,
                            container = %candidate.container_name,
                            winner = %winner,
                            "Two Traefik-labelled containers claim the same host; keeping the first"
                        );
                        conflicts.push(HostConflict {
                            host: candidate.host,
                            container_id: candidate.container_id,
                            container_name: candidate.container_name,
                            router_name: candidate.router_name,
                            reason: ConflictReason::ClaimedByAnotherContainer {
                                winner_container_name: winner,
                            },
                        });
                    }
                    std::collections::hash_map::Entry::Vacant(slot) => {
                        slot.insert(candidate);
                    }
                }
            }
        }

        let existing = discovered::Entity::find()
            .filter(discovered::Column::Network.eq(self.config.network.clone()))
            .all(self.db.as_ref())
            .await
            .map_err(|e| self.db_err("load existing discovered routes", e))?;
        let existing_by_host: HashMap<&str, &discovered::Model> = existing
            .iter()
            .map(|row| (row.host.as_str(), row))
            .collect();

        // Split into "route actually changed" (must write + notify) and
        // "still there, unchanged" (heartbeat only, must NOT notify).
        let mut to_upsert: Vec<&RouteCandidate> = Vec::new();
        let mut unchanged_hosts: Vec<String> = Vec::new();
        for candidate in candidates.values() {
            match existing_by_host.get(candidate.host.as_str()) {
                Some(row) if candidate.matches_row(row, &self.config.network) => {
                    unchanged_hosts.push(candidate.host.clone());
                }
                _ => to_upsert.push(candidate),
            }
        }

        let stale_hosts: Vec<String> = existing
            .iter()
            .filter(|row| !candidates.contains_key(&row.host))
            .map(|row| row.host.clone())
            .collect();

        for candidate in &to_upsert {
            self.upsert_candidate(candidate).await?;
        }
        if !unchanged_hosts.is_empty() {
            self.touch_last_seen(&unchanged_hosts).await?;
        }
        if !stale_hosts.is_empty() {
            self.delete_hosts(&stale_hosts).await?;
        }

        // Authoritative refresh of the event-loop fast path: after this pass,
        // exactly these containers have rows on this network.
        self.set_tracked_containers(
            candidates
                .values()
                .map(|candidate| candidate.container_id.clone())
                .collect(),
        );

        // ADR-041 §2a: Check certificate drift for every host with cert_authorized=true
        // on this network. When the current container differs from the authorized one,
        // record `container_drift_detected_at`. This is a best-effort pass; drift
        // detection failures are logged but do not fail the reconciliation.
        if let Err(e) = self.check_certificate_drift(&candidates).await {
            error!(
                network = %self.config.network,
                error = %e,
                "Certificate drift check failed; will retry on the next reconciliation pass"
            );
        }

        let outcome = ReconcileOutcome {
            network: self.config.network.clone(),
            containers_scanned,
            skipped_temps_managed,
            routes_upserted: to_upsert.len(),
            routes_unchanged: unchanged_hosts.len(),
            routes_removed: stale_hosts.len(),
            conflicts,
            completed_at: Utc::now(),
        };

        if outcome.changed() {
            info!(
                network = %outcome.network,
                upserted = outcome.routes_upserted,
                removed = outcome.routes_removed,
                conflicts = outcome.conflicts.len(),
                "Traefik discovery updated the route table"
            );
            self.trigger_route_reload().await;
        } else {
            debug!(
                network = %outcome.network,
                scanned = outcome.containers_scanned,
                unchanged = outcome.routes_unchanged,
                "Traefik discovery reconciliation: no changes"
            );
        }

        if let Ok(mut guard) = self.last_outcome.write() {
            *guard = Some(outcome.clone());
        }
        Ok(outcome)
    }

    /// ADR-041 §2a: Compare `traefik_route_certificates` rows against the
    /// current candidate set and record drift when the running container no
    /// longer matches the one that was authorized.
    async fn check_certificate_drift(
        &self,
        candidates: &HashMap<String, RouteCandidate>,
    ) -> Result<(), TraefikDiscoveryError> {
        check_certificate_drift_for(
            self.db.as_ref(),
            &self.config.network,
            candidates,
            self.alarm_sink.get().map(|a| a.as_ref()),
        )
        .await
    }

    /// Incrementally handle a single container event.
    ///
    /// `start`/`update` re-inspect and upsert; `die`/`stop`/`destroy` remove
    /// every row pointing at the container. Anything ambiguous falls through
    /// to the next full reconciliation.
    ///
    /// The Docker events stream is host-wide — it carries every container of
    /// every unrelated stack — so the first thing this does for a teardown
    /// event is check the in-memory tracked-container set. A container this
    /// node never adopted has nothing to delete, and must not cost a database
    /// round trip (nor, before the row-level trigger fix, a cluster-wide route
    /// reload) just because it happened to stop.
    pub async fn handle_container_event(
        &self,
        container_id: &str,
        action: &str,
    ) -> Result<bool, TraefikDiscoveryError> {
        match action {
            "die" | "stop" | "destroy" => {
                if self.known_untracked(container_id) {
                    debug!(
                        container_id = %container_id,
                        action = %action,
                        "Ignoring container event: no discovered route points at this container"
                    );
                    return Ok(false);
                }
                let removed = self.delete_by_container(container_id).await?;
                if removed > 0 {
                    info!(
                        container_id = %container_id,
                        action = %action,
                        removed,
                        "Removed discovered Traefik routes for a container that went away"
                    );
                    self.trigger_route_reload().await;
                }
                Ok(removed > 0)
            }
            "start" | "update" => {
                let Some(view) = self.inspect_container(container_id).await? else {
                    return Ok(false);
                };
                if !view.on_target_network || !view.running {
                    // Left the network (or isn't up): treat like a removal.
                    // Overwhelmingly this is a container that was never on the
                    // watched network in the first place, so the tracked set
                    // spares the delete entirely.
                    let removed = self.delete_by_container_if_tracked(container_id).await?;
                    if removed > 0 {
                        self.trigger_route_reload().await;
                    }
                    return Ok(removed > 0);
                }

                let candidates = evaluate_container(&view);
                if candidates.is_empty() {
                    // On the network but carrying no usable Traefik labels —
                    // the common case for an operator's own stack. Same
                    // reasoning: nothing to delete unless we adopted it before.
                    let removed = self.delete_by_container_if_tracked(container_id).await?;
                    if removed > 0 {
                        self.trigger_route_reload().await;
                    }
                    return Ok(removed > 0);
                }

                let reserved = self.reserved_hosts().await?;
                let mut changed = false;

                // Batch-fetch all discovered rows for this candidate set in a
                // single query to avoid an N+1 round trip per host.
                let candidate_hosts: Vec<String> =
                    candidates.iter().map(|c| c.host.clone()).collect();
                let existing_rows: std::collections::HashMap<String, discovered::Model> =
                    if candidate_hosts.is_empty() {
                        std::collections::HashMap::new()
                    } else {
                        discovered::Entity::find()
                            .filter(discovered::Column::Host.is_in(candidate_hosts))
                            .all(self.db.as_ref())
                            .await
                            .map_err(|e| self.db_err("batch look up discovered routes by host", e))?
                            .into_iter()
                            .map(|row| (row.host.clone(), row))
                            .collect()
                    };

                for candidate in &candidates {
                    if reserved.is_reserved(&candidate.host) {
                        warn!(
                            host = %candidate.host,
                            container = %candidate.container_name,
                            "Skipping Traefik-labelled container: host already belongs to a \
                             Temps-managed route. The existing route is kept."
                        );
                        continue;
                    }
                    // Never steal a host another container already holds; the
                    // full reconciliation owns tie-breaking.
                    if let Some(row) = existing_rows.get(&candidate.host) {
                        if row.target_container_id != candidate.container_id {
                            warn!(
                                host = %candidate.host,
                                container = %candidate.container_name,
                                existing = %row.target_container_name,
                                "Host already claimed by another discovered container; skipping"
                            );
                            continue;
                        }
                        if candidate.matches_row(row, &self.config.network) {
                            self.touch_last_seen(std::slice::from_ref(&candidate.host))
                                .await?;
                            continue;
                        }
                    }
                    self.upsert_candidate(candidate).await?;
                    changed = true;
                }

                // Hosts this container used to own but no longer claims.
                let claimed: HashSet<&str> = candidates.iter().map(|c| c.host.as_str()).collect();
                let orphaned: Vec<String> = discovered::Entity::find()
                    .filter(discovered::Column::TargetContainerId.eq(container_id.to_string()))
                    .all(self.db.as_ref())
                    .await
                    .map_err(|e| self.db_err("load discovered routes for container", e))?
                    .into_iter()
                    .filter(|row| !claimed.contains(row.host.as_str()))
                    .map(|row| row.host)
                    .collect();
                if !orphaned.is_empty() {
                    self.delete_hosts(&orphaned).await?;
                    changed = true;
                }

                if changed {
                    self.trigger_route_reload().await;
                }
                Ok(changed)
            }
            _ => Ok(false),
        }
    }

    // ── Docker ───────────────────────────────────────────────────────

    async fn list_network_containers(&self) -> Result<Vec<ContainerView>, TraefikDiscoveryError> {
        let mut filters: HashMap<String, Vec<String>> = HashMap::new();
        filters.insert("network".to_string(), vec![self.config.network.clone()]);
        filters.insert("status".to_string(), vec!["running".to_string()]);

        let summaries = self
            .docker
            .list_containers(Some(ListContainersOptions {
                all: false,
                filters: Some(filters),
                ..Default::default()
            }))
            .await
            .map_err(|e| self.docker_err("list containers", e))?;

        Ok(summaries
            .into_iter()
            .filter_map(|summary| ContainerView::from_summary(summary, &self.config.network))
            .filter(|view| view.on_target_network && view.running)
            .collect())
    }

    async fn inspect_container(
        &self,
        container_id: &str,
    ) -> Result<Option<ContainerView>, TraefikDiscoveryError> {
        match self
            .docker
            .inspect_container(container_id, None::<InspectContainerOptions>)
            .await
        {
            Ok(response) => Ok(ContainerView::from_inspect(response, &self.config.network)),
            // A container that vanished between the event and the inspect is
            // the normal race on `destroy`, not an error.
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => Ok(None),
            Err(e) => Err(self.docker_err("inspect container", e)),
        }
    }

    async fn run_events_loop(self: Arc<Self>) {
        loop {
            // Docker cannot filter container events down to "attached to
            // network X", so this stream carries every container on the host.
            // Relevance is decided in `handle_container_event`, which consults
            // the in-memory tracked-container set before doing any database
            // work — see the `tracked_containers` field docs.
            let mut filters: HashMap<String, Vec<String>> = HashMap::new();
            filters.insert("type".to_string(), vec!["container".to_string()]);
            filters.insert(
                "event".to_string(),
                WATCHED_ACTIONS.iter().map(|a| a.to_string()).collect(),
            );

            let mut stream = self.docker.events(Some(EventsOptions {
                filters: Some(filters),
                ..Default::default()
            }));

            debug!(
                network = %self.config.network,
                "Subscribed to Docker container events for Traefik discovery"
            );

            while let Some(event) = stream.next().await {
                match event {
                    Ok(message) => {
                        let Some(action) = message.action.as_deref() else {
                            continue;
                        };
                        // Docker emits qualified actions like `exec_start: ls`;
                        // only the bare actions we asked for are interesting.
                        if !WATCHED_ACTIONS.contains(&action) {
                            continue;
                        }
                        let Some(container_id) =
                            message.actor.as_ref().and_then(|a| a.id.as_deref())
                        else {
                            continue;
                        };
                        if let Err(e) = self.handle_container_event(container_id, action).await {
                            error!(
                                container_id = %container_id,
                                action = %action,
                                error = %e,
                                "Failed to apply Docker event to Traefik discovery; the \
                                 periodic reconciliation will correct this"
                            );
                        }
                    }
                    Err(e) => {
                        warn!(
                            error = %e,
                            "Docker events stream error during Traefik discovery; resubscribing"
                        );
                        break;
                    }
                }
            }

            warn!(
                "Docker events stream for Traefik discovery ended; resubscribing in {}s",
                EVENTS_RECONNECT_BACKOFF.as_secs()
            );
            tokio::time::sleep(EVENTS_RECONNECT_BACKOFF).await;
            // Events may have been missed while disconnected — resync fully
            // before trusting the incremental path again.
            if let Err(e) = self.reconcile().await {
                error!(error = %e, "Resync after Docker events reconnect failed");
            }
        }
    }

    // ── Database ─────────────────────────────────────────────────────

    async fn upsert_candidate(
        &self,
        candidate: &RouteCandidate,
    ) -> Result<(), TraefikDiscoveryError> {
        let now = Utc::now();
        let model = discovered::ActiveModel {
            host: Set(candidate.host.clone()),
            router_name: Set(candidate.router_name.clone()),
            target_container_id: Set(candidate.container_id.clone()),
            target_container_name: Set(candidate.container_name.clone()),
            target_port: Set(i32::from(candidate.port)),
            target_host_port: Set(candidate.host_port.map(i32::from)),
            network: Set(self.config.network.clone()),
            tls: Set(candidate.tls),
            enabled: Set(true),
            last_seen_at: Set(now),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        };

        discovered::Entity::insert(model)
            .on_conflict(
                OnConflict::column(discovered::Column::Host)
                    .update_columns([
                        discovered::Column::RouterName,
                        discovered::Column::TargetContainerId,
                        discovered::Column::TargetContainerName,
                        discovered::Column::TargetPort,
                        discovered::Column::TargetHostPort,
                        discovered::Column::Network,
                        discovered::Column::Tls,
                        discovered::Column::LastSeenAt,
                        discovered::Column::UpdatedAt,
                    ])
                    .to_owned(),
            )
            .exec(self.db.as_ref())
            .await
            .map_err(|e| self.db_err("upsert discovered route", e))?;

        self.mark_tracked(&candidate.container_id, true);
        debug!(
            host = %candidate.host,
            container = %candidate.container_name,
            port = candidate.port,
            tls = candidate.tls,
            "Upserted discovered Traefik route"
        );
        Ok(())
    }

    /// Delete rows adopted under a *different* discovery network that have gone
    /// unrefreshed long enough that no live watcher can still own them.
    ///
    /// See [`FOREIGN_NETWORK_STALE_INTERVALS`] for why staleness — rather than
    /// "network is not mine" alone — is the safe boundary: this table has no
    /// owner column, and in a multi-node install another node may be watching
    /// that other network right now and refreshing its rows every pass.
    async fn prune_foreign_network_rows(&self) -> Result<(), TraefikDiscoveryError> {
        let result = self
            .foreign_network_prune_query(Utc::now())
            .exec(self.db.as_ref())
            .await
            .map_err(|e| self.db_err("prune discovered routes from other networks", e))?;

        if result.rows_affected > 0 {
            info!(
                network = %self.config.network,
                removed = result.rows_affected,
                "Removed discovered Traefik routes left over from a previous discovery network. \
                 They were already excluded from the route table; this clears the rows."
            );
        }
        Ok(())
    }

    /// The prune's query, split out so its two filters — "not my network" AND
    /// "unrefreshed for long enough that no live watcher owns it" — are
    /// assertable without a database.
    fn foreign_network_prune_query(
        &self,
        now: DateTime<Utc>,
    ) -> sea_orm::DeleteMany<discovered::Entity> {
        let stale_after = self
            .config
            .poll_interval
            .saturating_mul(FOREIGN_NETWORK_STALE_INTERVALS);
        let cutoff = now
            - chrono::Duration::from_std(stale_after)
                .unwrap_or_else(|_| chrono::Duration::hours(1));

        discovered::Entity::delete_many()
            .filter(discovered::Column::Network.ne(self.config.network.clone()))
            .filter(discovered::Column::LastSeenAt.lt(cutoff))
    }

    /// `delete_by_container`, skipped entirely when the in-memory tracked set
    /// proves there is nothing to delete.
    async fn delete_by_container_if_tracked(
        &self,
        container_id: &str,
    ) -> Result<u64, TraefikDiscoveryError> {
        if self.known_untracked(container_id) {
            return Ok(0);
        }
        self.delete_by_container(container_id).await
    }

    /// Refresh `last_seen_at` without touching any routing field.
    ///
    /// Issued as a raw statement so it can never disturb `updated_at` or any
    /// other column: the update trigger's `WHEN` filter only fires on routing
    /// fields, so this heartbeat does not cause a route table reload.
    async fn touch_last_seen(&self, hosts: &[String]) -> Result<(), TraefikDiscoveryError> {
        if hosts.is_empty() {
            return Ok(());
        }
        let placeholders: Vec<String> = (2..=hosts.len() + 1).map(|i| format!("${i}")).collect();
        let sql = format!(
            "UPDATE traefik_discovered_routes SET last_seen_at = $1 WHERE host IN ({})",
            placeholders.join(", ")
        );
        let mut values: Vec<sea_orm::Value> = Vec::with_capacity(hosts.len() + 1);
        values.push(Utc::now().into());
        values.extend(hosts.iter().map(|h| h.clone().into()));

        self.db
            .execute(Statement::from_sql_and_values(
                sea_orm::DatabaseBackend::Postgres,
                &sql,
                values,
            ))
            .await
            .map_err(|e| self.db_err("refresh discovered route last_seen_at", e))?;
        Ok(())
    }

    async fn delete_hosts(&self, hosts: &[String]) -> Result<u64, TraefikDiscoveryError> {
        if hosts.is_empty() {
            return Ok(0);
        }
        let result = discovered::Entity::delete_many()
            .filter(discovered::Column::Network.eq(self.config.network.clone()))
            .filter(discovered::Column::Host.is_in(hosts.to_vec()))
            .exec(self.db.as_ref())
            .await
            .map_err(|e| self.db_err("delete stale discovered routes", e))?;
        Ok(result.rows_affected)
    }

    async fn delete_by_container(&self, container_id: &str) -> Result<u64, TraefikDiscoveryError> {
        let result = discovered::Entity::delete_many()
            .filter(discovered::Column::Network.eq(self.config.network.clone()))
            .filter(discovered::Column::TargetContainerId.eq(container_id.to_string()))
            .exec(self.db.as_ref())
            .await
            .map_err(|e| self.db_err("delete discovered routes for container", e))?;
        self.mark_tracked(container_id, false);
        Ok(result.rows_affected)
    }

    /// Every hostname a Temps-managed route already owns.
    ///
    /// A discovered container must never take one of these over — that is how
    /// a workload on a shared Docker host would otherwise hijack a real
    /// deployment's domain (or lock the operator out of the console) by
    /// setting a label.
    async fn reserved_hosts(&self) -> Result<ReservedHosts, TraefikDiscoveryError> {
        use temps_entities::{
            custom_routes, environment_domains, environments, project_custom_domains, settings,
        };

        // Cache TTL: 30 s matches the default reconcile interval. Container
        // events during a burst reuse the cached set so the full-table scan does
        // not run O(events); the reconcile pass invalidates it explicitly via
        // `invalidate_reserved_hosts_cache` so correctness is not traded away.
        const RESERVED_HOSTS_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(30);

        if let Ok(guard) = self.reserved_hosts_cache.read() {
            if let Some((cached, primed_at)) = guard.as_ref() {
                if primed_at.elapsed() < RESERVED_HOSTS_CACHE_TTL {
                    return Ok((**cached).clone());
                }
            }
        }

        let mut reserved = ReservedHosts::default();

        for row in environment_domains::Entity::find()
            .all(self.db.as_ref())
            .await
            .map_err(|e| self.db_err("load environment domains", e))?
        {
            reserved.add(&row.domain);
        }
        for row in project_custom_domains::Entity::find()
            .all(self.db.as_ref())
            .await
            .map_err(|e| self.db_err("load project custom domains", e))?
        {
            reserved.add(&row.domain);
        }
        for row in custom_routes::Entity::find()
            .all(self.db.as_ref())
            .await
            .map_err(|e| self.db_err("load custom routes", e))?
        {
            reserved.add(&row.domain);
        }

        let app_settings = settings::Entity::find()
            .one(self.db.as_ref())
            .await
            .map_err(|e| self.db_err("load settings", e))?
            .map(|s| AppSettings::from_json(s.data))
            .unwrap_or_default();

        if let Some(console_host) = app_settings.console_hostname() {
            reserved.add(&console_host);
        }

        let preview_domain = app_settings.preview_domain.clone();
        for env in environments::Entity::find()
            .filter(environments::Column::DeletedAt.is_null())
            .all(self.db.as_ref())
            .await
            .map_err(|e| self.db_err("load environments", e))?
        {
            if env.subdomain.trim().is_empty() {
                continue;
            }
            reserved.add(&env.subdomain);
            reserved.add(
                &PublicHostnameStrategy::Standard
                    .environment_hostname(&preview_domain, &env.subdomain),
            );
        }

        // Write to cache so subsequent event-path calls within the TTL window
        // skip the five full-table scans.
        if let Ok(mut guard) = self.reserved_hosts_cache.write() {
            *guard = Some((Arc::new(reserved.clone()), std::time::Instant::now()));
        }

        Ok(reserved)
    }

    /// Invalidate the reserved-host cache so the next call to `reserved_hosts()`
    /// performs a fresh database read. Called at the start of each reconcile pass
    /// so the full-reconcile path always sees current state.
    fn invalidate_reserved_hosts_cache(&self) {
        if let Ok(mut guard) = self.reserved_hosts_cache.write() {
            *guard = None;
        }
    }

    async fn trigger_route_reload(&self) {
        // The DB trigger already fired PG NOTIFY, which is what reaches other
        // control planes. This call makes the local table reflect the change
        // without waiting for that round trip; it is idempotent.
        if let Some(refresher) = &self.refresher {
            match refresher.refresh_routes().await {
                Ok(count) => debug!(
                    routes = count,
                    "Reloaded route table after Traefik discovery change"
                ),
                Err(e) => warn!(
                    error = %e,
                    "Route table reload after a Traefik discovery change failed; the \
                     PG NOTIFY path will still converge"
                ),
            }
        }
    }

    fn db_err(&self, operation: &str, source: DbErr) -> TraefikDiscoveryError {
        TraefikDiscoveryError::Database {
            network: self.config.network.clone(),
            operation: operation.to_string(),
            source,
        }
    }

    fn docker_err(&self, operation: &str, e: bollard::errors::Error) -> TraefikDiscoveryError {
        TraefikDiscoveryError::Docker {
            operation: operation.to_string(),
            network: self.config.network.clone(),
            reason: e.to_string(),
        }
    }
}

/// Process-wide handle to the discovery watcher, shared with the API layer.
///
/// `temps serve` decides at startup whether the watcher runs (opt-in env var,
/// Docker reachable) and registers one of these in the plugin service registry.
/// `GET /traefik-discovery/status` reads it so the console/CLI can tell
/// "this build has no discovery" apart from "discovery is not turned on" — the
/// capability-endpoint rule in CLAUDE.md's *Feature Discoverability*.
///
/// It deliberately carries the resolved [`TraefikDiscoveryConfig`] even when
/// the watcher is NOT running, so a disabled instance can still answer *which
/// network it would watch* and *why it is off*.
pub struct TraefikDiscoveryHandle {
    config: TraefikDiscoveryConfig,
    /// `None` when the watcher isn't running in this process.
    service: Option<Arc<TraefikDiscoveryService>>,
    /// Operator-facing explanation of why the watcher isn't running. Always
    /// `Some` when `service` is `None`.
    unavailable_reason: Option<String>,
}

impl TraefikDiscoveryHandle {
    /// The watcher is running in this process.
    pub fn running(service: Arc<TraefikDiscoveryService>) -> Self {
        Self {
            config: service.config().clone(),
            service: Some(service),
            unavailable_reason: None,
        }
    }

    /// The watcher is not running; `reason` explains why in operator terms
    /// (not enabled, Docker unreachable, wrong process role, ...).
    pub fn not_running(config: TraefikDiscoveryConfig, reason: impl Into<String>) -> Self {
        Self {
            config,
            service: None,
            unavailable_reason: Some(reason.into()),
        }
    }

    /// Handle for a process that reads the environment but never starts a
    /// watcher (tests, embedded bootstraps that skip `temps serve`'s wiring).
    pub fn disabled_from_env(default_network: &str) -> Self {
        let config = TraefikDiscoveryConfig::from_env(default_network);
        let reason = if config.enabled {
            format!(
                "{ENABLED_ENV} is set, but the discovery watcher is not running in this process"
            )
        } else {
            format!("{ENABLED_ENV} is not set to 'true'")
        };
        Self::not_running(config, reason)
    }

    pub fn config(&self) -> &TraefikDiscoveryConfig {
        &self.config
    }

    /// Whether the watcher is actually reconciling in this process.
    pub fn is_running(&self) -> bool {
        self.service.is_some()
    }

    pub fn unavailable_reason(&self) -> Option<&str> {
        self.unavailable_reason.as_deref()
    }

    /// Most recent reconciliation, or `None` when the watcher isn't running or
    /// hasn't completed its first pass yet.
    pub fn last_outcome(&self) -> Option<ReconcileOutcome> {
        self.service.as_ref().and_then(|s| s.last_outcome())
    }

    /// ADR-041 §2a: wire the alarm sink into the underlying watcher so
    /// certificate-drift events reach the operator via the alarm system.
    /// No-op when the watcher isn't running in this process.
    pub fn inject_alarm_sink(&self, sink: Arc<dyn DriftAlarmSink>) {
        if let Some(svc) = &self.service {
            svc.inject_alarm_sink(sink);
        }
    }
}

/// Hostnames owned by Temps-managed routes, including wildcard patterns.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct ReservedHosts {
    exact: HashSet<String>,
    /// Suffixes from `*.example.com` patterns (stored as `example.com`).
    wildcard_suffixes: HashSet<String>,
}

impl ReservedHosts {
    fn add(&mut self, domain: &str) {
        let domain = domain.trim().trim_end_matches('.').to_ascii_lowercase();
        if domain.is_empty() {
            return;
        }
        match domain.strip_prefix("*.") {
            Some(suffix) if !suffix.is_empty() => {
                self.wildcard_suffixes.insert(suffix.to_string());
            }
            _ => {
                self.exact.insert(domain);
            }
        }
    }

    /// Mirrors `WildcardMatcher` semantics: `*.example.com` covers
    /// `api.example.com` but not `example.com` or `a.b.example.com`.
    fn is_reserved(&self, host: &str) -> bool {
        let host = host.trim().to_ascii_lowercase();
        if self.exact.contains(&host) {
            return true;
        }
        match host.split_once('.') {
            Some((label, parent)) if !label.is_empty() && !parent.is_empty() => {
                self.wildcard_suffixes.contains(parent)
            }
            _ => false,
        }
    }
}

impl ContainerView {
    fn from_summary(summary: bollard::models::ContainerSummary, network: &str) -> Option<Self> {
        let id = summary.id?;
        let name = summary
            .names
            .as_ref()
            .and_then(|names| names.first())
            .map(|n| n.trim_start_matches('/').to_string())
            .unwrap_or_else(|| id.clone());

        let mut exposed_ports = Vec::new();
        let mut published_ports = HashMap::new();
        for port in summary.ports.iter().flatten() {
            if !matches!(
                port.typ,
                None | Some(bollard::models::PortSummaryTypeEnum::TCP)
                    | Some(bollard::models::PortSummaryTypeEnum::EMPTY)
            ) {
                continue;
            }
            exposed_ports.push(port.private_port);
            if let Some(public) = port.public_port {
                published_ports.insert(port.private_port, public);
            }
        }
        exposed_ports.sort_unstable();
        exposed_ports.dedup();

        let on_target_network = summary
            .network_settings
            .as_ref()
            .and_then(|ns| ns.networks.as_ref())
            .is_some_and(|networks| networks.contains_key(network));

        Some(Self {
            id,
            name,
            labels: summary.labels.unwrap_or_default(),
            exposed_ports,
            published_ports,
            on_target_network,
            running: matches!(
                summary.state,
                Some(bollard::models::ContainerSummaryStateEnum::RUNNING)
            ),
        })
    }

    fn from_inspect(
        response: bollard::models::ContainerInspectResponse,
        network: &str,
    ) -> Option<Self> {
        let id = response.id?;
        let name = response
            .name
            .as_deref()
            .map(|n| n.trim_start_matches('/').to_string())
            .unwrap_or_else(|| id.clone());

        let config = response.config.unwrap_or_default();
        let mut exposed_ports: Vec<u16> = config
            .exposed_ports
            .unwrap_or_default()
            .iter()
            .filter_map(|spec| parse_port_spec(spec))
            .collect();
        exposed_ports.sort_unstable();
        exposed_ports.dedup();

        let network_settings = response.network_settings.unwrap_or_default();

        let mut published_ports = HashMap::new();
        for (spec, bindings) in network_settings.ports.iter().flatten() {
            let Some(container_port) = parse_port_spec(spec) else {
                continue;
            };
            // Docker reports a port here even when it isn't exposed in the
            // image, so fold it into the exposed set too.
            if !exposed_ports.contains(&container_port) {
                exposed_ports.push(container_port);
            }
            if let Some(host_port) = bindings
                .iter()
                .flatten()
                .filter_map(|b| b.host_port.as_deref())
                .filter_map(|p| p.parse::<u16>().ok())
                .next()
            {
                published_ports.insert(container_port, host_port);
            }
        }
        exposed_ports.sort_unstable();
        exposed_ports.dedup();

        let on_target_network = network_settings
            .networks
            .as_ref()
            .is_some_and(|networks| networks.contains_key(network));

        Some(Self {
            id,
            name,
            labels: config.labels.unwrap_or_default(),
            exposed_ports,
            published_ports,
            on_target_network,
            running: response
                .state
                .and_then(|state| state.running)
                .unwrap_or(false),
        })
    }
}

/// Parse a Docker port spec such as `"3000/tcp"`. UDP/SCTP are ignored — HTTP
/// routing is TCP only.
fn parse_port_spec(spec: &str) -> Option<u16> {
    let (port, proto) = match spec.split_once('/') {
        Some((port, proto)) => (port, proto),
        None => (spec, "tcp"),
    };
    if !proto.eq_ignore_ascii_case("tcp") {
        return None;
    }
    port.trim().parse::<u16>().ok().filter(|p| *p != 0)
}

/// Derive route candidates for a single container.
fn evaluate_container(view: &ContainerView) -> Vec<RouteCandidate> {
    traefik_labels::resolve_routers(&view.labels, &view.exposed_ports)
        .into_iter()
        .map(|router: ResolvedRouter| RouteCandidate {
            host: router.host,
            router_name: router.router_name,
            container_id: view.id.clone(),
            container_name: view.name.clone(),
            port: router.port,
            host_port: view.published_ports.get(&router.port).copied(),
            tls: router.tls,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(name: &str, labels: &[(&str, &str)], exposed: &[u16]) -> ContainerView {
        ContainerView {
            id: format!("id-{name}"),
            name: name.to_string(),
            labels: labels
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
            exposed_ports: exposed.to_vec(),
            published_ports: HashMap::new(),
            on_target_network: true,
            running: true,
        }
    }

    #[test]
    fn config_defaults_to_disabled_and_the_workload_network() {
        let cfg = TraefikDiscoveryConfig::resolve(None, None, "temps");
        assert!(!cfg.enabled, "discovery must be opt-in");
        assert_eq!(cfg.network, "temps");
        assert_eq!(cfg.poll_interval, DEFAULT_POLL_INTERVAL);
    }

    #[test]
    fn config_reads_enable_and_network_overrides() {
        let cfg =
            TraefikDiscoveryConfig::resolve(Some("TRUE"), Some("  my-stack_default "), "temps");
        assert!(cfg.enabled);
        assert_eq!(cfg.network, "my-stack_default");
    }

    #[test]
    fn config_non_true_values_leave_discovery_off() {
        for value in ["1", "yes", "", "off", "false", "t", "  "] {
            assert!(
                !TraefikDiscoveryConfig::resolve(Some(value), None, "temps").enabled,
                "value {value:?} must not enable discovery"
            );
        }
    }

    #[test]
    fn config_blank_network_falls_back_to_the_default() {
        assert_eq!(
            TraefikDiscoveryConfig::resolve(None, Some("   "), "temps").network,
            "temps"
        );
    }

    #[test]
    fn evaluate_container_uses_explicit_port_label() {
        let v = view(
            "app",
            &[
                ("traefik.enable", "true"),
                ("traefik.http.routers.app.rule", "Host(`app.example.com`)"),
                ("traefik.http.services.app.loadbalancer.server.port", "3000"),
            ],
            &[3000, 9229],
        );
        let candidates = evaluate_container(&v);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].host, "app.example.com");
        assert_eq!(candidates[0].port, 3000);
        assert_eq!(candidates[0].container_name, "app");
        assert_eq!(candidates[0].host_port, None);
    }

    #[test]
    fn evaluate_container_records_the_published_host_port() {
        let mut v = view(
            "app",
            &[
                ("traefik.enable", "true"),
                ("traefik.http.routers.app.rule", "Host(`app.example.com`)"),
                ("traefik.http.services.app.loadbalancer.server.port", "3000"),
            ],
            &[3000],
        );
        v.published_ports.insert(3000, 18080);
        assert_eq!(evaluate_container(&v)[0].host_port, Some(18080));
    }

    #[test]
    fn evaluate_container_skips_temps_managed_containers() {
        let v = view(
            "temps-app",
            &[
                ("sh.temps.deploy_id", "17"),
                ("traefik.enable", "true"),
                (
                    "traefik.http.routers.app.rule",
                    "Host(`hijack.example.com`)",
                ),
                ("traefik.http.services.app.loadbalancer.server.port", "80"),
            ],
            &[80],
        );
        assert!(
            evaluate_container(&v).is_empty(),
            "a Temps-deployed container must never be adopted by label discovery"
        );
    }

    #[test]
    fn candidate_matches_row_ignores_timestamps() {
        let candidate = RouteCandidate {
            host: "app.example.com".into(),
            router_name: "app".into(),
            container_id: "abc".into(),
            container_name: "app".into(),
            port: 3000,
            host_port: None,
            tls: false,
        };
        let now = Utc::now();
        let row = discovered::Model {
            id: 1,
            host: "app.example.com".into(),
            router_name: "app".into(),
            target_container_id: "abc".into(),
            target_container_name: "app".into(),
            target_port: 3000,
            target_host_port: None,
            network: "temps".into(),
            tls: false,
            enabled: true,
            last_seen_at: now - chrono::Duration::hours(3),
            created_at: now - chrono::Duration::days(2),
            updated_at: now - chrono::Duration::days(2),
        };
        assert!(candidate.matches_row(&row, "temps"));

        let mut moved = row.clone();
        moved.target_container_id = "def".into();
        assert!(!candidate.matches_row(&moved, "temps"));

        let mut reported = row.clone();
        reported.target_port = 8080;
        assert!(!candidate.matches_row(&reported, "temps"));

        let mut secured = row.clone();
        secured.tls = true;
        assert!(!candidate.matches_row(&secured, "temps"));

        assert!(
            !candidate.matches_row(&row, "other-network"),
            "a row from another network must not count as a match"
        );
    }

    #[test]
    fn reserved_hosts_match_exactly_and_by_single_label_wildcard() {
        let mut reserved = ReservedHosts::default();
        reserved.add("App.Example.COM");
        reserved.add("*.preview.temps.dev");
        reserved.add("  ");
        reserved.add("*.");

        assert!(reserved.is_reserved("app.example.com"));
        assert!(reserved.is_reserved("APP.EXAMPLE.COM"));
        assert!(!reserved.is_reserved("other.example.com"));

        assert!(reserved.is_reserved("my-env.preview.temps.dev"));
        assert!(
            !reserved.is_reserved("a.b.preview.temps.dev"),
            "wildcards cover exactly one label, matching WildcardMatcher"
        );
        assert!(
            !reserved.is_reserved("preview.temps.dev"),
            "*.x does not cover the apex"
        );
        assert!(!reserved.is_reserved(""));
    }

    #[test]
    fn parse_port_spec_accepts_tcp_and_rejects_udp() {
        assert_eq!(parse_port_spec("3000/tcp"), Some(3000));
        assert_eq!(parse_port_spec("3000"), Some(3000));
        assert_eq!(parse_port_spec("53/udp"), None);
        assert_eq!(parse_port_spec("132/sctp"), None);
        assert_eq!(parse_port_spec("0/tcp"), None);
        assert_eq!(parse_port_spec("notaport/tcp"), None);
        assert_eq!(parse_port_spec(""), None);
    }

    #[test]
    fn container_view_from_summary_reads_labels_ports_and_network() {
        let summary = bollard::models::ContainerSummary {
            id: Some("deadbeef".into()),
            names: Some(vec!["/whoami".into()]),
            labels: Some(
                [("traefik.enable".to_string(), "true".to_string())]
                    .into_iter()
                    .collect(),
            ),
            state: Some(bollard::models::ContainerSummaryStateEnum::RUNNING),
            ports: Some(vec![
                bollard::models::PortSummary {
                    private_port: 80,
                    public_port: Some(18080),
                    typ: Some(bollard::models::PortSummaryTypeEnum::TCP),
                    ip: None,
                },
                bollard::models::PortSummary {
                    private_port: 53,
                    public_port: None,
                    typ: Some(bollard::models::PortSummaryTypeEnum::UDP),
                    ip: None,
                },
            ]),
            network_settings: Some(bollard::models::ContainerSummaryNetworkSettings {
                networks: Some(
                    [(
                        "stack_default".to_string(),
                        bollard::models::EndpointSettings::default(),
                    )]
                    .into_iter()
                    .collect(),
                ),
            }),
            ..Default::default()
        };

        let view = ContainerView::from_summary(summary.clone(), "stack_default")
            .expect("summary with an id must produce a view");
        assert_eq!(view.name, "whoami");
        assert_eq!(view.exposed_ports, vec![80]);
        assert_eq!(view.published_ports.get(&80), Some(&18080));
        assert!(view.on_target_network);
        assert!(view.running);

        let other = ContainerView::from_summary(summary, "some-other-network")
            .expect("view is still built for other networks");
        assert!(!other.on_target_network);
    }

    #[test]
    fn handle_reports_a_reason_when_the_watcher_is_not_running() {
        let config = TraefikDiscoveryConfig::resolve(None, None, "temps");
        let handle = TraefikDiscoveryHandle::not_running(
            config,
            "TEMPS_TRAEFIK_DISCOVERY_ENABLED is not set to 'true'",
        );

        assert!(!handle.is_running());
        assert_eq!(
            handle.unavailable_reason(),
            Some("TEMPS_TRAEFIK_DISCOVERY_ENABLED is not set to 'true'")
        );
        assert!(
            handle.last_outcome().is_none(),
            "a watcher that never ran cannot have an outcome"
        );
        assert_eq!(
            handle.config().network,
            "temps",
            "a disabled instance must still report which network it would watch"
        );
    }

    // ── event fast path / foreign-network prune ──────────────────────

    /// A service whose Docker client points at a port nothing listens on.
    /// Every test below exercises the database and in-memory paths only; any
    /// accidental Docker call fails loudly rather than silently passing.
    fn test_service(db: DatabaseConnection) -> TraefikDiscoveryService {
        let docker = bollard::Docker::connect_with_http(
            "http://127.0.0.1:1",
            5,
            bollard::API_DEFAULT_VERSION,
        )
        .expect("constructing a bollard HTTP client must not require a live daemon");
        TraefikDiscoveryService::new(
            Arc::new(docker),
            Arc::new(db),
            TraefikDiscoveryConfig::resolve(Some("true"), None, "temps"),
            None,
        )
    }

    #[test]
    fn untracked_containers_are_only_known_once_the_set_is_primed() {
        let service = test_service(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
        );

        assert!(
            !service.known_untracked("whatever"),
            "before the first reconciliation the set is unknown, so nothing may be skipped"
        );

        service.set_tracked_containers(HashSet::from(["adopted".to_string()]));
        assert!(service.known_untracked("stranger"));
        assert!(!service.known_untracked("adopted"));

        service.mark_tracked("stranger", true);
        assert!(!service.known_untracked("stranger"));
        service.mark_tracked("stranger", false);
        assert!(service.known_untracked("stranger"));
    }

    /// The whole point of the tracked set: a container this node never adopted
    /// stopping must cost **zero** database round trips. The mock has no
    /// results queued, so any query at all fails the test.
    #[tokio::test]
    async fn teardown_event_for_an_untracked_container_touches_no_database() {
        let service = test_service(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
        );
        service.set_tracked_containers(HashSet::from(["adopted".to_string()]));

        for action in ["die", "stop", "destroy"] {
            let changed = service
                .handle_container_event("some-unrelated-container", action)
                .await
                .unwrap_or_else(|e| panic!("{action} must not hit the database: {e}"));
            assert!(
                !changed,
                "{action} on an untracked container changes nothing"
            );
        }
    }

    /// ...but a teardown event for a container we *did* adopt must still go to
    /// the database. Queuing an exec error proves the delete was issued.
    #[tokio::test]
    async fn teardown_event_for_a_tracked_container_still_deletes() {
        let service = test_service(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
                .append_exec_errors([DbErr::Custom("delete reached the database".to_string())])
                .into_connection(),
        );
        service.set_tracked_containers(HashSet::from(["adopted".to_string()]));

        let err = service
            .handle_container_event("adopted", "die")
            .await
            .expect_err("the delete must be issued for a tracked container");
        assert!(
            matches!(err, TraefikDiscoveryError::Database { .. }),
            "expected a Database error proving the delete ran, got {err:?}"
        );
    }

    /// Before the first reconciliation the set is unknown, so correctness wins
    /// over the fast path: the delete is still issued.
    #[tokio::test]
    async fn teardown_event_before_priming_still_deletes() {
        let service = test_service(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
                .append_exec_errors([DbErr::Custom("delete reached the database".to_string())])
                .into_connection(),
        );

        let err = service
            .handle_container_event("anything", "die")
            .await
            .expect_err("an unprimed set must not suppress the delete");
        assert!(matches!(err, TraefikDiscoveryError::Database { .. }));
    }

    #[test]
    fn prune_targets_other_networks_and_only_long_unrefreshed_rows() {
        use sea_orm::QueryTrait;

        let service = test_service(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
        );
        let now = DateTime::parse_from_rfc3339("2026-01-01T12:00:00Z")
            .expect("static timestamp parses")
            .with_timezone(&Utc);

        let sql = service
            .foreign_network_prune_query(now)
            .into_query()
            .to_string(sea_orm::sea_query::PostgresQueryBuilder);

        assert!(
            sql.contains("DELETE FROM \"traefik_discovered_routes\""),
            "the prune must delete from the discovery table, got {sql}"
        );
        assert!(
            sql.contains("\"network\" <> 'temps'"),
            "the prune must only ever touch networks other than the configured one, got {sql}"
        );
        // 30s poll interval * 10 = 5 minutes before noon.
        assert!(
            sql.contains("\"last_seen_at\" < '2026-01-01 11:55:00"),
            "the prune must only remove rows no live watcher is refreshing: without the staleness \
             bound it would delete a peer node's live rows for that node's own network, which \
             this table has no owner column to distinguish. Got {sql}"
        );
    }

    #[tokio::test]
    async fn prune_surfaces_a_database_failure_with_context() {
        let service = test_service(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
                .append_exec_errors([DbErr::Custom("connection reset".to_string())])
                .into_connection(),
        );

        let err = service
            .prune_foreign_network_rows()
            .await
            .expect_err("a failed prune must surface, not be swallowed");
        assert!(
            err.to_string()
                .contains("prune discovered routes from other networks"),
            "the error must name the operation, got {err}"
        );
    }

    #[test]
    fn container_view_from_summary_requires_an_id() {
        let summary = bollard::models::ContainerSummary {
            id: None,
            ..Default::default()
        };
        assert!(ContainerView::from_summary(summary, "temps").is_none());
    }

    // ── ADR-041 §2a: Container drift detection tests ─────────────────────────
    //
    // These tests exercise `check_certificate_drift_for` (the extracted free
    // function) directly with a `MockDatabase` so no live Docker daemon is
    // needed. The three cases below correspond to the three findings from the
    // security review: initial drift alarm, deduplication, and re-alarm.

    fn cert_row(
        host: &str,
        authorized_container_id: &str,
        last_drift_alarmed: Option<&str>,
    ) -> route_certs::Model {
        let now = Utc::now();
        route_certs::Model {
            id: 1,
            host: host.to_string(),
            cert_authorized: true,
            authorized_at: Some(now),
            authorized_by_user_id: Some(1),
            authorized_network: "temps".to_string(),
            authorized_container_id: authorized_container_id.to_string(),
            authorized_container_name: "authorized-container".to_string(),
            container_drift_detected_at: None,
            last_drift_alarmed_container_id: last_drift_alarmed.map(str::to_string),
            renewal_method: "http-01".to_string(),
            source: "acme".to_string(),
            certificate_id: None,
            imported_at: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn candidate(host: &str, container_id: &str) -> (String, RouteCandidate) {
        (
            host.to_string(),
            RouteCandidate {
                host: host.to_string(),
                router_name: "app".to_string(),
                container_id: container_id.to_string(),
                container_name: format!("container-{container_id}"),
                port: 80,
                host_port: None,
                tls: false,
            },
        )
    }

    /// First detection: the container serving the host changed.
    /// `cert_authorized` must remain `true`; `container_drift_detected_at` and
    /// `last_drift_alarmed_container_id` must be updated.
    #[tokio::test]
    async fn drift_sets_detected_at_on_first_container_change() {
        let row = cert_row("drift.example.com", "old-id", None);
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            // SELECT: returns one cert_authorized row with old container ID
            .append_query_results([vec![row.clone()]])
            // UPDATE ... RETURNING * (Postgres returns the updated row as a query result)
            .append_query_results([vec![row]])
            .into_connection();

        let candidates = HashMap::from([candidate("drift.example.com", "new-id")]);

        check_certificate_drift_for(&db, "temps", &candidates, None)
            .await
            .expect("drift detection must not fail");

        let log = db.into_transaction_log();
        // SELECT + UPDATE
        assert_eq!(log.len(), 2, "expected SELECT + UPDATE, got {log:?}");

        // The UPDATE must NOT touch cert_authorized in the SET clause — clearing it
        // would be a DoS primitive. Note: RETURNING lists all columns (including
        // cert_authorized), so we check only the SET portion of the SQL.
        let update_stmts = log[1].statements();
        assert!(
            !update_stmts.is_empty(),
            "UPDATE transaction must have statements"
        );
        for stmt in update_stmts {
            // Extract just the SET ... WHERE portion to avoid false positives from RETURNING.
            let set_portion = stmt.sql.split("WHERE").next().unwrap_or(&stmt.sql);
            assert!(
                !set_portion.contains("cert_authorized"),
                "cert_authorized must never appear in the SET clause of a drift update: {}",
                stmt.sql
            );
        }
    }

    /// Deduplication: the same drifting container fires the alarm at most once.
    /// When `last_drift_alarmed_container_id` already equals the current container
    /// ID, the UPDATE must be suppressed.
    #[tokio::test]
    async fn drift_deduplication_suppresses_repeat_alarm_for_same_container() {
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            // SELECT: cert already alarmed for "new-id"
            .append_query_results([vec![cert_row(
                "drift.example.com",
                "old-id",
                Some("new-id"),
            )]])
            // No UPDATE expected — dedup must fire
            .into_connection();

        let candidates = HashMap::from([candidate("drift.example.com", "new-id")]);

        check_certificate_drift_for(&db, "temps", &candidates, None)
            .await
            .expect("drift detection must not fail");

        let log = db.into_transaction_log();
        // Only the SELECT; no UPDATE.
        assert_eq!(
            log.len(),
            1,
            "expected only SELECT (dedup suppressed UPDATE), got {log:?}"
        );
    }

    /// Re-alarm: when a *third* container takes over (different from the one
    /// already recorded in `last_drift_alarmed_container_id`), the alarm must
    /// fire again. This proves the dedup is per-container-ID, not permanent.
    #[tokio::test]
    async fn drift_re_alarms_when_yet_another_container_takes_over() {
        let row = cert_row("drift.example.com", "original-id", Some("second-id"));
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            // SELECT: previously alarmed for "second-id"; now "third-id" is serving
            .append_query_results([vec![row.clone()]])
            // UPDATE ... RETURNING * (Postgres returns the updated row as a query result)
            .append_query_results([vec![row]])
            .into_connection();

        let candidates = HashMap::from([candidate("drift.example.com", "third-id")]);

        check_certificate_drift_for(&db, "temps", &candidates, None)
            .await
            .expect("drift detection must not fail");

        let log = db.into_transaction_log();
        // SELECT + UPDATE (new container triggers a fresh alarm)
        assert_eq!(
            log.len(),
            2,
            "expected SELECT + UPDATE for third container, got {log:?}"
        );
    }
}
