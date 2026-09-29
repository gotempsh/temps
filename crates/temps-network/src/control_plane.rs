// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Control-plane participation in the multi-host overlay.
//!
//! The control plane is deliberately not a schedulable `nodes` row. Its
//! allocation lives in `network_config` and this module reconciles the same
//! kernel/Docker primitives workers use. Both server startup and the operator
//! CLI call the same idempotent entry point.

use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use bollard::Docker;
use ipnet::Ipv4Net;
use sea_orm::{
    ConnectionTrait, DatabaseBackend, DatabaseConnection, EntityTrait, Statement, TransactionTrait,
};
use temps_entities::network_config;
use thiserror::Error;
use tracing::{info, warn};

use crate::allocator::{AllocatorError, PostgresAllocator};
use crate::{NetworkConfig, NetworkError, NetworkManager, NodeAlloc, Peer, Transport};

const PEER_RECONCILE_INTERVAL: Duration = Duration::from_secs(5);

/// Why the last attempt to bring up this process's control-plane overlay
/// failed, if it did. Set by whoever drives [`setup`] in the background (the
/// server's watcher) so status endpoints in the same process can show the
/// operator the actual error instead of "check the logs".
static LAST_SETUP_FAILURE: std::sync::RwLock<Option<String>> = std::sync::RwLock::new(None);

/// Record the outcome of a background [`setup`]: `None` on success.
pub fn record_setup_failure(failure: Option<String>) {
    if let Ok(mut slot) = LAST_SETUP_FAILURE.write() {
        *slot = failure;
    }
}

/// The last recorded background [`setup`] failure in this process.
pub fn last_setup_failure() -> Option<String> {
    LAST_SETUP_FAILURE.read().ok().and_then(|slot| slot.clone())
}

/// When the cluster network settings last changed (`network_config.updated_at`),
/// so a setup parked on a configuration error can retry once they do.
pub async fn network_config_revision(
    db: &DatabaseConnection,
) -> Result<Option<chrono::DateTime<chrono::Utc>>, sea_orm::DbErr> {
    Ok(network_config::Entity::find_by_id(1)
        .one(db)
        .await?
        .map(|cfg| cfg.updated_at))
}
// "TEMPSNET" as a stable signed 64-bit PostgreSQL advisory-lock key.
const CONTROL_PLANE_SETUP_LOCK_KEY: i64 = 0x5445_4D50_534E_4554;

#[derive(Debug, Error)]
pub enum ControlPlaneSetupError {
    #[error("control-plane underlay address {value:?} is invalid: {reason}")]
    InvalidUnderlayAddress { value: String, reason: String },
    #[error("VXLAN requires a private underlay address; {address} is publicly routable")]
    PublicUnderlayAddress { address: IpAddr },
    #[error("control-plane overlay allocation failed: {0}")]
    Allocation(#[from] AllocatorError),
    #[error("control-plane overlay network failed: {0}")]
    Network(#[from] NetworkError),
    #[error("network_config singleton row is missing")]
    MissingNetworkConfig,
    #[error("network_config transport {value:?} is unsupported")]
    InvalidTransport { value: String },
    #[error("network_config contains an invalid VXLAN value: {reason}")]
    InvalidVxlanConfig { reason: String },
    #[error("database error while loading network_config: {0}")]
    Database(#[from] sea_orm::DbErr),
    #[error("WireGuard mesh: {0}")]
    Mesh(#[from] crate::mesh::MeshError),
    #[error("WireGuard mesh interface: {0}")]
    WireGuard(#[from] temps_wireguard::WireGuardError),
    #[error("the WireGuard mesh is enabled but no data directory was given for its key")]
    MeshKeyDirMissing,
}

#[derive(Clone)]
pub struct ControlPlaneOverlay {
    pub alloc: NodeAlloc,
    pub config: NetworkConfig,
    manager: NetworkManager,
    docker: Docker,
    compute_pool: Ipv4Net,
    /// The control plane's end of the managed WireGuard mesh, when that is
    /// the underlay; the reconciler keeps its peers in step with the cluster.
    mesh: Option<MeshEnd>,
}

/// What the control plane needs to (re)create its mesh interface.
#[derive(Clone)]
struct MeshEnd {
    interface: temps_wireguard::mesh::MeshInterface,
    key: temps_wireguard::mesh::MeshKey,
    /// Where workers dial this end; published once setup succeeds. `None`
    /// when nobody can dial it (no reachable address configured): it dials
    /// the nodes that publish an endpoint instead.
    endpoint: Option<SocketAddr>,
    /// The pool it was built for: a change means setup must run again.
    settings: crate::mesh::MeshSettings,
    vxlan_port: u16,
}

impl MeshEnd {
    fn lockdown(&self) -> crate::mesh::MeshLockdown {
        crate::mesh::MeshLockdown {
            vxlan_port: self.vxlan_port,
            mesh: self.settings.cidr,
            node_api_port: Some(self.settings.node_api_port),
        }
    }
}

/// Create or repair the mesh interface, lockdown first. Returns whether the
/// interface changed, in which case the overlay on it must be rebuilt (a
/// recreated interface takes its VXLAN device with it).
async fn ensure_mesh_interface(end: &MeshEnd) -> Result<bool, ControlPlaneSetupError> {
    crate::mesh::ensure_lockdown(&end.lockdown()).await?;
    let end = end.clone();
    let changed = tokio::task::spawn_blocking(move || {
        temps_wireguard::mesh::ensure_interface(&end.interface, &end.key)
    })
    .await
    .map_err(|error| temps_wireguard::WireGuardError::OperationFailed {
        operation: "configure WireGuard interface".into(),
        reason: error.to_string(),
    })??;
    Ok(changed)
}

impl ControlPlaneOverlay {
    /// Keep the overlay (and the mesh, when on) in step with the cluster.
    /// The task ends when the cluster's mesh setting no longer matches what
    /// this overlay was built for; the caller then runs [`setup`] again.
    pub fn spawn_peer_reconciler(
        &self,
        db: Arc<DatabaseConnection>,
    ) -> tokio::task::JoinHandle<()> {
        let manager = self.manager.clone();
        let docker = self.docker.clone();
        let config = self.config.clone();
        let alloc = self.alloc.clone();
        let compute_pool = self.compute_pool;
        let mesh = self.mesh.clone();
        tokio::spawn(async move {
            let allocator = PostgresAllocator::new(db.clone());
            let mut tick: u32 = 0;
            // Set when the mesh interface was recreated or reconfigured: the
            // overlay must be bootstrapped again, not just reconciled.
            let mut rebootstrap = false;
            loop {
                tick = tick.wrapping_add(1);
                match crate::mesh::load_settings(db.as_ref()).await {
                    Ok(settings) if settings.as_ref() != mesh.as_ref().map(|end| &end.settings) => {
                        info!(
                            mesh = settings.is_some(),
                            "the cluster's WireGuard mesh setting changed; setting the control-plane overlay up again"
                        );
                        return;
                    }
                    Ok(_) => {}
                    Err(error) => {
                        warn!(error = %error, "could not read the WireGuard mesh settings")
                    }
                }
                // WireGuard first: a new node's VXLAN peer is useless until
                // its tunnel exists.
                if let Some(end) = &mesh {
                    rebootstrap |= tend_mesh(end, &db).await;
                }
                match allocator.control_plane_peer_list().await {
                    Ok(peers) => {
                        let peers: Vec<_> = peers
                            .into_iter()
                            .filter(|peer| {
                                let allowed =
                                    crate::allocator::is_private_underlay(peer.underlay_address);
                                if !allowed {
                                    // Expected on the mesh until the node's
                                    // agent registers and the mesh address
                                    // becomes its underlay.
                                    tracing::debug!(
                                        node_id = %peer.node_id,
                                        underlay = %peer.underlay_address,
                                        "skipping publicly-routable control-plane overlay peer"
                                    );
                                }
                                allowed
                            })
                            .collect();
                        let result = if rebootstrap {
                            manager
                                .bootstrap(alloc.clone(), peers)
                                .await
                                .map(|()| {
                                    info!("control-plane overlay rebuilt on the recreated WireGuard interface");
                                    true
                                })
                        } else {
                            reconcile_peer_snapshot(
                                &manager,
                                &docker,
                                &config,
                                &alloc,
                                compute_pool,
                                peers,
                            )
                            .await
                        };
                        match result {
                            Ok(_) => rebootstrap = false,
                            Err(error) => {
                                warn!(error = %error, "control-plane overlay peer reconciliation failed")
                            }
                        }
                    }
                    Err(error) => {
                        warn!(error = %error, "could not load control-plane overlay peers")
                    }
                }
                tokio::time::sleep(PEER_RECONCILE_INTERVAL).await;
            }
        })
    }
}

/// One reconcile tick for the control plane's mesh end: lockdown, then
/// peers, recreating the interface when it is gone. Returns whether the
/// interface was recreated or reconfigured.
async fn tend_mesh(end: &MeshEnd, db: &DatabaseConnection) -> bool {
    // Every tick: another tool flushing the ruleset (a firewalld reload,
    // `nft flush ruleset`) must not leave the mesh open for long. Checking
    // is one `nft list`.
    if let Err(error) = crate::mesh::ensure_lockdown(&end.lockdown()).await {
        warn!(error = %error, "could not verify the WireGuard mesh lockdown");
    }
    let Err(error) = reconcile_mesh_peers(db).await else {
        return false;
    };
    warn!(error = %error, "control-plane WireGuard peer reconciliation failed");
    // The interface may be gone (deleted, module reloaded); recreate it so
    // the next tick can repopulate its peers.
    match ensure_mesh_interface(end).await {
        Ok(changed) => changed,
        Err(error) => {
            warn!(error = %error, "could not restore the control-plane WireGuard interface");
            false
        }
    }
}

/// The control plane's end of the mesh on a server that runs no workloads:
/// there is no local container network, so no overlay, but nodes still
/// reach the control plane (and it reaches their published ports) over the
/// mesh.
pub struct ControlPlaneMesh {
    end: MeshEnd,
}

impl ControlPlaneMesh {
    /// Keep the mesh end in step with the cluster. The task ends when the
    /// cluster's mesh setting no longer matches what it was set up for; the
    /// caller then runs [`setup_mesh_only`] again.
    pub fn spawn_reconciler(self, db: Arc<DatabaseConnection>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                match crate::mesh::load_settings(db.as_ref()).await {
                    Ok(settings) if settings.as_ref() != Some(&self.end.settings) => {
                        info!(
                            mesh = settings.is_some(),
                            "the cluster's WireGuard mesh setting changed; setting the control-plane mesh up again"
                        );
                        return;
                    }
                    Ok(_) => {}
                    Err(error) => {
                        warn!(error = %error, "could not read the WireGuard mesh settings")
                    }
                }
                tend_mesh(&self.end, db.as_ref()).await;
                tokio::time::sleep(PEER_RECONCILE_INTERVAL).await;
            }
        })
    }
}

/// Bring up the control plane's mesh end without an overlay (a server that
/// runs no workloads). `None` while the mesh is off.
///
/// `configured_address` is the operator's `--private-address`, where nodes
/// dial WireGuard; without one the control plane publishes no endpoint.
pub async fn setup_mesh_only(
    db: &DatabaseConnection,
    configured_address: Option<&str>,
    mesh_key_dir: &Path,
) -> Result<Option<ControlPlaneMesh>, ControlPlaneSetupError> {
    let Some(settings) = crate::mesh::load_settings(db).await? else {
        return Ok(None);
    };
    let end = setup_mesh(db, &settings, configured_address, mesh_key_dir).await?;
    crate::mesh::publish_control_plane(db, end.key.public_key(), end.endpoint).await?;
    Ok(Some(ControlPlaneMesh { end }))
}

/// Reconcile one authoritative peer snapshot after repeating all local
/// collision checks. Public for the privileged DinD lifecycle test; normal
/// callers should use [`ControlPlaneOverlay::spawn_peer_reconciler`].
#[doc(hidden)]
pub async fn reconcile_peer_snapshot(
    manager: &NetworkManager,
    docker: &Docker,
    config: &NetworkConfig,
    alloc: &NodeAlloc,
    compute_pool: Ipv4Net,
    peers: Vec<Peer>,
) -> Result<bool, NetworkError> {
    crate::preflight_compute_pool_routes(config, compute_pool).await?;
    crate::docker::ensure_network_for_pool(docker, config, alloc, compute_pool).await?;
    manager.reconcile_peers(peers).await
}

/// Make the control plane's WireGuard peers exactly the registered nodes.
async fn reconcile_mesh_peers(db: &DatabaseConnection) -> Result<(), ControlPlaneSetupError> {
    let desired: Vec<_> = crate::mesh::peers(db, None)
        .await?
        .into_iter()
        .map(|named| named.peer)
        .collect();
    let changes =
        tokio::task::spawn_blocking(move || temps_wireguard::mesh::reconcile_peers(&desired))
            .await
            .map_err(|error| temps_wireguard::WireGuardError::OperationFailed {
                operation: "reconcile WireGuard peers".into(),
                reason: error.to_string(),
            })??;
    if !changes.is_empty() {
        info!(
            added = changes.added,
            updated = changes.updated,
            removed = changes.removed,
            "control-plane WireGuard peers reconciled"
        );
    }
    Ok(())
}

/// Bring up the control plane's end of the WireGuard mesh. Its address is the
/// overlay underlay. Workers are only told about it (the key is published)
/// once the whole overlay setup has succeeded, see [`setup`].
///
/// `configured_address` is the operator's `--private-address`: with the mesh
/// on it is where workers dial WireGuard, and may be public. On a restart
/// without it, the caller passes the persisted underlay (already the mesh
/// address) or nothing, and the previously published endpoint, if any, is
/// reused. With no endpoint at all the control plane is still on the mesh:
/// it dials every node that publishes one.
async fn setup_mesh(
    db: &DatabaseConnection,
    settings: &crate::mesh::MeshSettings,
    configured_address: Option<&str>,
    key_dir: &Path,
) -> Result<MeshEnd, ControlPlaneSetupError> {
    let cfg = network_config::Entity::find_by_id(1)
        .one(db)
        .await?
        .ok_or(ControlPlaneSetupError::MissingNetworkConfig)?;
    let mesh_address = settings.control_plane_address();
    let endpoint = match configured_address
        .map(str::trim)
        .filter(|address| !address.is_empty() && *address != mesh_address.to_string())
    {
        Some(address) => Some(crate::mesh::default_endpoint(address, settings.port)?),
        None => cfg
            .control_plane_wg_endpoint
            .as_deref()
            .and_then(|endpoint| endpoint.parse().ok()),
    };
    let vxlan_port =
        u16::try_from(cfg.vxlan_port).map_err(|_| ControlPlaneSetupError::InvalidVxlanConfig {
            reason: format!("vxlan_port {} is outside 0..=65535", cfg.vxlan_port),
        })?;
    let key_dir = key_dir.to_path_buf();
    let key = tokio::task::spawn_blocking(move || {
        temps_wireguard::mesh::MeshKey::load_or_create(&key_dir)
    })
    .await
    .map_err(|error| temps_wireguard::WireGuardError::OperationFailed {
        operation: "load the WireGuard key".into(),
        reason: error.to_string(),
    })??;
    let mtu = crate::mesh::detect_mtu(u32::try_from(cfg.underlay_mtu).ok()).await?;
    crate::mesh::preflight_routes(settings.cidr).await?;
    let end = MeshEnd {
        interface: temps_wireguard::mesh::MeshInterface {
            address: mesh_address,
            prefix_len: settings.cidr.prefix_len(),
            listen_port: settings.port,
            mtu,
        },
        key,
        endpoint,
        settings: settings.clone(),
        vxlan_port,
    };
    ensure_mesh_interface(&end).await?;
    reconcile_mesh_peers(db).await?;
    info!(
        interface = crate::mesh::MESH_INTERFACE,
        address = %mesh_address,
        endpoint = ?endpoint,
        mtu,
        "control-plane WireGuard mesh is up"
    );
    Ok(end)
}

/// `mesh_key_dir` holds the control plane's WireGuard private key; it is only
/// read when `network_config.wireguard_enabled` is set.
/// `underlay_address` is the operator's `--private-address` (or the persisted
/// underlay); it may only be absent while the mesh is on, whose address is
/// then the underlay.
pub async fn setup(
    db: Arc<DatabaseConnection>,
    docker: &Docker,
    underlay_address: Option<&str>,
    underlay_device: Option<&str>,
    mesh_key_dir: Option<&Path>,
) -> Result<ControlPlaneOverlay, ControlPlaneSetupError> {
    let mesh_settings = crate::mesh::load_settings(db.as_ref()).await?;
    let mut mesh_end = None;
    let (underlay_address, underlay_device) = match &mesh_settings {
        Some(settings) => {
            let key_dir = mesh_key_dir.ok_or(ControlPlaneSetupError::MeshKeyDirMissing)?;
            let end = setup_mesh(db.as_ref(), settings, underlay_address, key_dir).await?;
            let address = IpAddr::V4(end.interface.address);
            mesh_end = Some(end);
            (address, Some(crate::mesh::MESH_INTERFACE))
        }
        None => {
            let underlay_address = underlay_address.unwrap_or_default();
            let address: IpAddr =
                underlay_address
                    .parse()
                    .map_err(|error: std::net::AddrParseError| {
                        ControlPlaneSetupError::InvalidUnderlayAddress {
                            value: underlay_address.to_owned(),
                            reason: error.to_string(),
                        }
                    })?;
            (address, underlay_device)
        }
    };
    if !crate::allocator::is_private_underlay(underlay_address) {
        return Err(ControlPlaneSetupError::PublicUnderlayAddress {
            address: underlay_address,
        });
    }
    // Take the advisory lock in a short transaction to serialize concurrent
    // reservation attempts, then commit immediately. The generation counter
    // written by ensure_control_plane_reservation fences any concurrent
    // completion in set_control_plane_ready_for, so there is no need to hold
    // a DB transaction (and a connection-pool slot) open across the slow
    // privileged I/O below (subprocess ip/nft, Docker network create/inspect,
    // manager.bootstrap). Holding it open risks pool exhaustion under load.
    let setup_lock = db.begin().await?;
    setup_lock
        .execute(Statement::from_string(
            DatabaseBackend::Postgres,
            format!("SELECT pg_advisory_xact_lock({CONTROL_PLANE_SETUP_LOCK_KEY})"),
        ))
        .await?;
    let allocator = PostgresAllocator::new(db.clone());
    let reservation = allocator
        .ensure_control_plane_reservation(underlay_address)
        .await?;
    // Release the advisory lock and the connection-pool slot before the slow
    // privileged I/O begins. The generation counter in the reservation now
    // acts as the serialization backstop.
    setup_lock.commit().await?;
    let cluster_network = reservation.cluster_config;
    let mut privileged_setup_started = false;
    let attempt = async {
        let alloc: NodeAlloc = reservation.alloc.clone().into();
        let mut peers = allocator.control_plane_peer_list().await?;
        if mesh_settings.is_some() {
            // A worker that has not registered its mesh key yet still has its
            // public join address as underlay. It joins the overlay once its
            // agent registers (the reconciler picks it up); it must not block
            // the control plane.
            peers.retain(|peer| crate::allocator::is_private_underlay(peer.underlay_address));
        }
        let persisted = network_config::Entity::find_by_id(1)
            .one(db.as_ref())
            .await?
            .ok_or(ControlPlaneSetupError::MissingNetworkConfig)?;

        let underlay_dev = match underlay_device
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            Some(value) => value.to_owned(),
            None => crate::detect_device_for_address(underlay_address).await?,
        };
        let detected_mtu = crate::detect_underlay_mtu(&underlay_dev).await?;
        let configured_mtu = u32::try_from(persisted.underlay_mtu).map_err(|_| {
            ControlPlaneSetupError::InvalidVxlanConfig {
                reason: format!("underlay_mtu {} is negative", persisted.underlay_mtu),
            }
        })?;
        let transport = match persisted.transport.as_str() {
            "vxlan" => Transport::Vxlan {
                vni: u32::try_from(persisted.vxlan_vni).map_err(|_| {
                    ControlPlaneSetupError::InvalidVxlanConfig {
                        reason: format!("vxlan_vni {} is negative", persisted.vxlan_vni),
                    }
                })?,
                port: u16::try_from(persisted.vxlan_port).map_err(|_| {
                    ControlPlaneSetupError::InvalidVxlanConfig {
                        reason: format!("vxlan_port {} is outside 0..=65535", persisted.vxlan_port),
                    }
                })?,
            },
            "native" => Transport::Native,
            value => {
                return Err(ControlPlaneSetupError::InvalidTransport {
                    value: value.into(),
                });
            }
        };
        if matches!(transport, Transport::Vxlan { .. }) {
            if let Some(peer) = peers
                .iter()
                .find(|peer| !crate::allocator::is_private_underlay(peer.underlay_address))
            {
                return Err(ControlPlaneSetupError::PublicUnderlayAddress {
                    address: peer.underlay_address,
                });
            }
        }
        let config = NetworkConfig {
            transport,
            underlay_mtu: detected_mtu.min(configured_mtu),
            underlay_dev,
            ..NetworkConfig::default()
        };
        crate::preflight_compute_pool_routes(&config, cluster_network.compute_pool_cidr).await?;
        crate::docker::preflight_network_for_pool(
            docker,
            &config,
            &alloc,
            cluster_network.compute_pool_cidr,
        )
        .await?;
        let manager = NetworkManager::new(config.clone())?;
        // All operations from here are idempotent for this topology, but they
        // mutate shared host state. A failed attempt must not tear that state
        // down because a newer generation may already be using it.
        privileged_setup_started = true;
        manager.bootstrap(alloc.clone(), peers.clone()).await?;
        crate::docker::ensure_network_for_pool(
            docker,
            &config,
            &alloc,
            cluster_network.compute_pool_cidr,
        )
        .await?;
        Ok::<_, ControlPlaneSetupError>((alloc, peers, config, manager))
    }
    .await;

    let outcome = match attempt {
        Ok(overlay) => {
            allocator
                .set_control_plane_ready_for(&reservation, true)
                .await?;
            // Only now tell workers the mesh has a hub: a setup that failed
            // above must never move them onto it.
            if let Some(end) = &mesh_end {
                crate::mesh::publish_control_plane(db.as_ref(), end.key.public_key(), end.endpoint)
                    .await?;
            }
            Ok(overlay)
        }
        Err(setup_error) => {
            if !reservation.was_ready && !privileged_setup_started {
                // Route/Docker collision checks happen before privileged
                // mutation. Their failure can safely release this exact
                // unpublished generation so the operator may choose a
                // corrected pool. Once mutation starts, retain the
                // reservation and rely on idempotent retry: teardown here
                // could destroy a newer concurrent attempt's healthy overlay.
                match allocator
                    .release_unready_control_plane_reservation(&reservation)
                    .await
                {
                    Ok(()) | Err(AllocatorError::SupersededControlPlaneSetup { .. }) => {}
                    Err(error) => return Err(error.into()),
                }
            }
            Err(setup_error)
        }
    };
    let (alloc, peers, config, manager) = outcome?;
    info!(
        cidr = %alloc.compute_cidr,
        bridge = %alloc.bridge_address,
        underlay = %alloc.underlay_address,
        peers = peers.len(),
        "control-plane overlay is ready"
    );
    Ok(ControlPlaneOverlay {
        alloc,
        config,
        manager,
        docker: docker.clone(),
        compute_pool: cluster_network.compute_pool_cidr,
        mesh: mesh_end,
    })
}
