// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Control-plane participation in the multi-host overlay.
//!
//! The control plane is deliberately not a schedulable `nodes` row. Its
//! allocation lives in `network_config` and this module reconciles the same
//! kernel/Docker primitives workers use. Both server startup and the operator
//! CLI call the same idempotent entry point.

use std::net::IpAddr;
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
}

#[derive(Clone)]
pub struct ControlPlaneOverlay {
    pub alloc: NodeAlloc,
    pub config: NetworkConfig,
    manager: NetworkManager,
    docker: Docker,
    compute_pool: Ipv4Net,
}

impl ControlPlaneOverlay {
    pub fn spawn_peer_reconciler(&self, db: Arc<DatabaseConnection>) {
        let manager = self.manager.clone();
        let docker = self.docker.clone();
        let config = self.config.clone();
        let alloc = self.alloc.clone();
        let compute_pool = self.compute_pool;
        tokio::spawn(async move {
            let allocator = PostgresAllocator::new(db);
            loop {
                match allocator.control_plane_peer_list().await {
                    Ok(peers) => {
                        if let Some(peer) = peers.iter().find(|peer| {
                            !crate::allocator::is_private_underlay(peer.underlay_address)
                        }) {
                            warn!(
                                node_id = %peer.node_id,
                                underlay = %peer.underlay_address,
                                "refusing publicly-routable control-plane overlay peer"
                            );
                            tokio::time::sleep(PEER_RECONCILE_INTERVAL).await;
                            continue;
                        }
                        let reconcile = reconcile_peer_snapshot(
                            &manager,
                            &docker,
                            &config,
                            &alloc,
                            compute_pool,
                            peers,
                        )
                        .await;
                        if let Err(error) = reconcile {
                            warn!(error = %error, "control-plane overlay peer reconciliation failed");
                        }
                    }
                    Err(error) => {
                        warn!(error = %error, "could not load control-plane overlay peers")
                    }
                }
                tokio::time::sleep(PEER_RECONCILE_INTERVAL).await;
            }
        });
    }
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

pub async fn setup(
    db: Arc<DatabaseConnection>,
    docker: &Docker,
    underlay_address: &str,
    underlay_device: Option<&str>,
) -> Result<ControlPlaneOverlay, ControlPlaneSetupError> {
    let underlay_address: IpAddr =
        underlay_address
            .parse()
            .map_err(|error: std::net::AddrParseError| {
                ControlPlaneSetupError::InvalidUnderlayAddress {
                    value: underlay_address.to_owned(),
                    reason: error.to_string(),
                }
            })?;
    if !crate::allocator::is_private_underlay(underlay_address) {
        return Err(ControlPlaneSetupError::PublicUnderlayAddress {
            address: underlay_address,
        });
    }
    // Serialize reservation, privileged host mutation, and readiness
    // publication across server startup and operator CLI processes. Database
    // generation fencing remains the stale-writer backstop, while this lock
    // prevents two generations from concurrently reconfiguring shared kernel
    // and Docker resources.
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
    let cluster_network = reservation.cluster_config;
    let mut privileged_setup_started = false;
    let attempt = async {
        let alloc: NodeAlloc = reservation.alloc.clone().into();
        let peers = allocator.control_plane_peer_list().await?;
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
    setup_lock.commit().await?;
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
    })
}

pub async fn current_peers(
    db: Arc<DatabaseConnection>,
) -> Result<Vec<Peer>, ControlPlaneSetupError> {
    Ok(PostgresAllocator::new(db).control_plane_peer_list().await?)
}
