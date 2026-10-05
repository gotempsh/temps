// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Postgres-backed integration tests for `crate::allocator::PostgresAllocator`.
//!
//! Same skip-when-Docker-unavailable convention as the migration test
//! suite. Each test is hermetic: own container, own database, no shared
//! state. Run with:
//!
//!   cargo test -p temps-network --test it_allocator --features control_plane
//!
//! These tests are deliberately NOT gated on `integration_kernel` — they
//! don't need privileged Linux, just Docker.

#![cfg(feature = "control_plane")]

use ipnet::Ipv4Net;
use sea_orm::{ActiveModelTrait, ConnectionTrait, Database, DatabaseConnection, EntityTrait, Set};
use sea_orm_migration::MigratorTrait;
use std::str::FromStr;
use std::sync::Arc;
use temps_entities::{network_config, nodes};
use temps_migrations::Migrator;
use temps_network::allocator::{AllocatorError, ComputeNetworkAllocator, PostgresAllocator};
use testcontainers::{core::WaitFor, runners::AsyncRunner, GenericImage, ImageExt};

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

struct Fixture {
    db: Arc<DatabaseConnection>,
    // Hold the container so it stays alive for the test's lifetime (none
    // when TEMPS_TEST_DATABASE_URL points at an existing server).
    _container: Option<testcontainers::ContainerAsync<GenericImage>>,
}

async fn fixture() -> Option<Fixture> {
    fixture_with("").await
}

/// A fresh database created with `create_options` (appended to `CREATE
/// DATABASE`), migrated. On TEMPS_TEST_DATABASE_URL's server when set, so
/// CI with a shared server still runs these tests, each in its own database.
async fn fixture_with(create_options: &str) -> Option<Fixture> {
    let (server_url, container) = match std::env::var("TEMPS_TEST_DATABASE_URL") {
        Ok(url) => (url, None),
        Err(_) => {
            let container = match GenericImage::new("timescale/timescaledb-ha", "pg18")
                // Without this, `start()` returns before PostgreSQL accepts
                // clients and the test races the database ("Connection
                // reset by peer"). Must match on stderr: the temporary
                // initdb server logs its "ready" line to stdout, the real
                // server to stderr.
                .with_wait_for(WaitFor::message_on_stderr(
                    "database system is ready to accept connections",
                ))
                .with_env_var("POSTGRES_DB", "postgres")
                .with_env_var("POSTGRES_USER", "postgres")
                .with_env_var("POSTGRES_PASSWORD", "postgres")
                .with_env_var("POSTGRES_HOST_AUTH_METHOD", "trust")
                .with_startup_timeout(std::time::Duration::from_secs(120))
                .start()
                .await
            {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("⏭️  skipping: docker not available: {}", e);
                    return None;
                }
            };
            let port = container
                .get_host_port_ipv4(5432)
                .await
                .expect("postgres port");
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            (
                format!("postgresql://postgres:postgres@localhost:{}/postgres", port),
                Some(container),
            )
        }
    };

    let server = connect(&server_url).await;
    let name = format!("temps_it_{}", uuid::Uuid::new_v4().simple());
    server
        .execute_unprepared(&format!("CREATE DATABASE {name} {create_options}"))
        .await
        .expect("create test database");
    let (base, query) = match server_url.split_once('?') {
        Some((base, query)) => (base, format!("?{query}")),
        None => (server_url.as_str(), String::new()),
    };
    let base = base.rsplit_once('/').map_or(base, |(root, _)| root);
    let db = connect(&format!("{base}/{name}{query}")).await;
    // A database made from template0 (to pick another locale) lacks the
    // extension template1 carries.
    db.execute_unprepared("CREATE EXTENSION IF NOT EXISTS timescaledb")
        .await
        .expect("timescaledb extension");
    Migrator::up(&db, None).await.expect("migrations");

    Some(Fixture {
        db: Arc::new(db),
        _container: container,
    })
}

async fn connect(url: &str) -> DatabaseConnection {
    let mut retries = 5;
    loop {
        match Database::connect(url).await {
            Ok(d) => break d,
            Err(e) if retries > 0 => {
                retries -= 1;
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                if retries == 0 {
                    panic!("connect failed: {}", e);
                }
            }
            Err(e) => panic!("connect failed: {}", e),
        }
    }
}

async fn insert_node(db: &DatabaseConnection, name: &str, underlay: Option<&str>) -> i32 {
    let now = chrono::Utc::now();
    let m = nodes::ActiveModel {
        name: Set(name.into()),
        token_hash: Set(format!("hash-{}", name)),
        token_encrypted: Set(None),
        address: Set("https://127.0.0.1:3100".into()),
        private_address: Set("10.0.0.1".into()),
        public_endpoint: Set(None),
        wg_public_key: Set(None),
        role: Set("worker".into()),
        status: Set("active".into()),
        labels: Set(serde_json::json!({})),
        capacity: Set(serde_json::json!({})),
        last_heartbeat: Set(None),
        edge_public_key: Set(None),
        compute_cidr: Set(None),
        underlay_address: Set(underlay.map(str::to_owned)),
        mesh_wg_public_key: Set(None),
        mesh_wg_endpoint: Set(None),
        mesh_wg_address: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    };
    m.insert(db).await.expect("insert node").id
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn pool_can_change_before_first_allocation() {
    let Some(fx) = fixture().await else { return };
    let allocator = PostgresAllocator::new(fx.db.clone());
    let configured = allocator
        .configure_pool("10.240.0.0/16".parse().unwrap(), 24)
        .await
        .unwrap();
    assert_eq!(configured.compute_pool_cidr.to_string(), "10.240.0.0/16");

    let node_id = insert_node(&fx.db, "node-custom", Some("10.0.0.1")).await;
    let allocation = allocator.allocate_for_node(node_id).await.unwrap();
    assert_eq!(allocation.compute_cidr.to_string(), "10.240.0.0/24");
}

#[tokio::test]
async fn pool_change_is_rejected_after_worker_allocation() {
    let Some(fx) = fixture().await else { return };
    let allocator = PostgresAllocator::new(fx.db.clone());
    let node_id = insert_node(&fx.db, "node-fixed", Some("10.0.0.1")).await;
    allocator.allocate_for_node(node_id).await.unwrap();

    let error = allocator
        .configure_pool("10.240.0.0/16".parse().unwrap(), 24)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        AllocatorError::PoolChangeAfterAllocation {
            allocation_count: 1,
            ..
        }
    ));
}

#[tokio::test]
async fn allocate_assigns_lowest_free_cidr() {
    let Some(fx) = fixture().await else { return };
    let alloc = PostgresAllocator::new(fx.db.clone());

    let id = insert_node(&fx.db, "node-a", Some("10.0.0.1")).await;
    let result = alloc.allocate_for_node(id).await.unwrap();

    // Default pool 172.20.0.0/16 with /24 subnets → first free is .0.0/24.
    assert_eq!(result.compute_cidr.to_string(), "172.20.0.0/24");
    assert_eq!(result.bridge_address.to_string(), "172.20.0.1");
    assert_eq!(result.underlay_address.to_string(), "10.0.0.1");
    assert_eq!(result.node_id, id);

    // Persisted to nodes.compute_cidr.
    let row = nodes::Entity::find_by_id(id)
        .one(fx.db.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.compute_cidr.as_deref(), Some("172.20.0.0/24"));
}

#[tokio::test]
async fn second_allocation_picks_next_subnet() {
    let Some(fx) = fixture().await else { return };
    let alloc = PostgresAllocator::new(fx.db.clone());

    let a = insert_node(&fx.db, "node-a", Some("10.0.0.1")).await;
    let b = insert_node(&fx.db, "node-b", Some("10.0.0.2")).await;

    let r1 = alloc.allocate_for_node(a).await.unwrap();
    let r2 = alloc.allocate_for_node(b).await.unwrap();

    assert_eq!(r1.compute_cidr.to_string(), "172.20.0.0/24");
    assert_eq!(r2.compute_cidr.to_string(), "172.20.1.0/24");
}

#[tokio::test]
async fn legacy_public_underlay_remains_compatible_without_control_plane_peer() {
    let Some(fx) = fixture().await else { return };
    let allocator = PostgresAllocator::new(fx.db.clone());
    let node_id = insert_node(&fx.db, "public-node", Some("203.0.113.10")).await;

    let allocation = allocator.allocate_for_node(node_id).await.unwrap();
    assert_eq!(allocation.underlay_address.to_string(), "203.0.113.10");
}

#[tokio::test]
async fn public_underlay_is_rejected_after_control_plane_overlay_is_ready() {
    let Some(fx) = fixture().await else { return };
    let allocator = PostgresAllocator::new(fx.db.clone());
    let reservation = allocator
        .ensure_control_plane_reservation("10.200.4.1".parse().unwrap())
        .await
        .unwrap();
    allocator
        .set_control_plane_ready_for(&reservation, true)
        .await
        .unwrap();

    let node_id = insert_node(&fx.db, "public-node", Some("203.0.113.10")).await;
    let error = allocator.allocate_for_node(node_id).await.unwrap_err();
    assert!(matches!(
        error,
        AllocatorError::PublicUnderlayAddress { node_id: rejected, .. }
            if rejected == node_id
    ));
}

#[tokio::test]
async fn control_plane_allocation_is_stable_and_visible_to_workers() {
    let Some(fx) = fixture().await else { return };
    let allocator = PostgresAllocator::new(fx.db.clone());

    let reservation = allocator
        .ensure_control_plane_reservation("10.200.4.1".parse().unwrap())
        .await
        .unwrap();
    let control_plane = reservation.alloc.clone();
    assert_eq!(control_plane.compute_cidr.to_string(), "172.20.255.0/24");
    assert_eq!(control_plane.bridge_address.to_string(), "172.20.255.1");

    let worker_id = insert_node(&fx.db, "node-a", Some("10.200.4.2")).await;
    let worker = allocator.allocate_for_node(worker_id).await.unwrap();
    assert_eq!(worker.compute_cidr.to_string(), "172.20.0.0/24");

    let peers = allocator.peer_list(worker_id).await.unwrap();
    assert!(
        peers.is_empty(),
        "an unready reservation must not be advertised"
    );

    allocator
        .set_control_plane_ready_for(&reservation, true)
        .await
        .unwrap();
    let peers = allocator.peer_list(worker_id).await.unwrap();
    assert_eq!(peers.len(), 1);
    assert_eq!(peers[0].compute_cidr, control_plane.compute_cidr);
    assert_eq!(peers[0].underlay_address.to_string(), "10.200.4.1");

    let refreshed = allocator
        .ensure_control_plane_alloc("10.200.4.9".parse().unwrap())
        .await
        .unwrap();
    assert_eq!(refreshed.compute_cidr, control_plane.compute_cidr);
    assert_eq!(refreshed.underlay_address.to_string(), "10.200.4.9");
    assert!(allocator.peer_list(worker_id).await.unwrap().is_empty());

    let persisted = network_config::Entity::find_by_id(1)
        .one(fx.db.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        persisted.control_plane_compute_cidr.as_deref(),
        Some("172.20.255.0/24")
    );
    assert_eq!(
        persisted.control_plane_underlay_address.as_deref(),
        Some("10.200.4.9")
    );
}

#[tokio::test]
async fn stale_control_plane_setup_cannot_publish_replacement_reservation() {
    let Some(fx) = fixture().await else { return };
    let allocator = PostgresAllocator::new(fx.db.clone());
    let stale = allocator
        .ensure_control_plane_reservation("10.200.4.1".parse().unwrap())
        .await
        .unwrap();
    let current = allocator
        .ensure_control_plane_reservation("10.200.4.1".parse().unwrap())
        .await
        .unwrap();

    assert!(current.setup_generation > stale.setup_generation);

    let error = allocator
        .set_control_plane_ready_for(&stale, true)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        AllocatorError::SupersededControlPlaneSetup { .. }
    ));
    allocator
        .set_control_plane_ready_for(&current, true)
        .await
        .unwrap();
    let stale_withdrawal = allocator
        .set_control_plane_ready_for(&stale, false)
        .await
        .unwrap_err();
    assert!(matches!(
        stale_withdrawal,
        AllocatorError::SupersededControlPlaneSetup { .. }
    ));
    let persisted = network_config::Entity::find_by_id(1)
        .one(fx.db.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert!(persisted.control_plane_overlay_ready);
    assert_eq!(
        persisted.control_plane_underlay_address.as_deref(),
        Some("10.200.4.1")
    );
}

#[tokio::test]
async fn pool_change_is_rejected_while_control_plane_setup_is_in_flight() {
    let Some(fx) = fixture().await else { return };
    let allocator = PostgresAllocator::new(fx.db.clone());
    let stale = allocator
        .ensure_control_plane_reservation("10.200.4.1".parse().unwrap())
        .await
        .unwrap();

    let replacement_pool: Ipv4Net = "10.240.0.0/16".parse().unwrap();
    let error = allocator
        .configure_pool(replacement_pool, 24)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        AllocatorError::PoolChangeAfterAllocation {
            allocation_count: 1,
            ..
        }
    ));
    allocator
        .set_control_plane_ready_for(&stale, true)
        .await
        .unwrap();

    let persisted = network_config::Entity::find_by_id(1)
        .one(fx.db.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert!(persisted.control_plane_overlay_ready);
    assert_eq!(persisted.compute_pool_cidr, "172.20.0.0/16");
    assert_eq!(
        persisted.control_plane_setup_generation,
        stale.setup_generation
    );
}

#[tokio::test]
async fn failed_unready_setup_can_release_reservation_and_choose_corrected_pool() {
    let Some(fx) = fixture().await else { return };
    let allocator = PostgresAllocator::new(fx.db.clone());
    let failed = allocator
        .ensure_control_plane_reservation("10.200.4.1".parse().unwrap())
        .await
        .unwrap();

    allocator
        .release_unready_control_plane_reservation(&failed)
        .await
        .unwrap();
    let corrected_pool: Ipv4Net = "10.240.0.0/16".parse().unwrap();
    let corrected = allocator.configure_pool(corrected_pool, 24).await.unwrap();
    assert_eq!(corrected.compute_pool_cidr, corrected_pool);

    let persisted = network_config::Entity::find_by_id(1)
        .one(fx.db.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert!(persisted.control_plane_compute_cidr.is_none());
    assert!(persisted.control_plane_underlay_address.is_none());
    assert!(!persisted.control_plane_overlay_ready);
    assert!(persisted.control_plane_setup_generation > failed.setup_generation);
}

#[tokio::test]
async fn stale_setup_cannot_release_newer_control_plane_reservation() {
    let Some(fx) = fixture().await else { return };
    let allocator = PostgresAllocator::new(fx.db.clone());
    let stale = allocator
        .ensure_control_plane_reservation("10.200.4.1".parse().unwrap())
        .await
        .unwrap();
    let current = allocator
        .ensure_control_plane_reservation("10.200.4.1".parse().unwrap())
        .await
        .unwrap();
    assert!(current.setup_generation > stale.setup_generation);

    let error = allocator
        .release_unready_control_plane_reservation(&stale)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        AllocatorError::SupersededControlPlaneSetup { .. }
    ));
}

#[tokio::test]
async fn ready_control_plane_reservation_cannot_be_released_by_failure_recovery() {
    let Some(fx) = fixture().await else { return };
    let allocator = PostgresAllocator::new(fx.db.clone());
    let ready = allocator
        .ensure_control_plane_reservation("10.200.4.1".parse().unwrap())
        .await
        .unwrap();
    allocator
        .set_control_plane_ready_for(&ready, true)
        .await
        .unwrap();

    let error = allocator
        .release_unready_control_plane_reservation(&ready)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        AllocatorError::ReadyControlPlaneRelease { .. }
    ));
}

#[tokio::test]
async fn allocate_twice_returns_already_allocated() {
    let Some(fx) = fixture().await else { return };
    let alloc = PostgresAllocator::new(fx.db.clone());

    let id = insert_node(&fx.db, "node-a", Some("10.0.0.1")).await;
    alloc.allocate_for_node(id).await.unwrap();

    let err = alloc.allocate_for_node(id).await.unwrap_err();
    assert!(
        matches!(err, AllocatorError::AlreadyAllocated { node_id, existing }
            if node_id == id && existing == Ipv4Net::from_str("172.20.0.0/24").unwrap()),
        "got {:?}",
        err
    );
}

#[tokio::test]
async fn allocate_without_underlay_fails() {
    let Some(fx) = fixture().await else { return };
    let alloc = PostgresAllocator::new(fx.db.clone());

    let id = insert_node(&fx.db, "node-a", None).await;
    let err = alloc.allocate_for_node(id).await.unwrap_err();
    assert!(matches!(err, AllocatorError::UnderlayMissing { node_id } if node_id == id));
}

#[tokio::test]
async fn allocate_unknown_node_returns_not_found() {
    let Some(fx) = fixture().await else { return };
    let alloc = PostgresAllocator::new(fx.db.clone());

    let err = alloc.allocate_for_node(99_999).await.unwrap_err();
    assert!(matches!(err, AllocatorError::NodeNotFound { node_id } if node_id == 99_999));
}

#[tokio::test]
async fn release_clears_compute_cidr() {
    let Some(fx) = fixture().await else { return };
    let alloc = PostgresAllocator::new(fx.db.clone());

    let id = insert_node(&fx.db, "node-a", Some("10.0.0.1")).await;
    alloc.allocate_for_node(id).await.unwrap();

    alloc.release(id).await.unwrap();
    let row = nodes::Entity::find_by_id(id)
        .one(fx.db.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert!(row.compute_cidr.is_none());

    // Reallocation after release must succeed and pick the same low slot.
    let r = alloc.allocate_for_node(id).await.unwrap();
    assert_eq!(r.compute_cidr.to_string(), "172.20.0.0/24");
}

#[tokio::test]
async fn release_is_idempotent() {
    let Some(fx) = fixture().await else { return };
    let alloc = PostgresAllocator::new(fx.db.clone());

    // Both no-op cases: no node, and node with no allocation.
    alloc.release(123_456).await.unwrap();

    let id = insert_node(&fx.db, "node-a", Some("10.0.0.1")).await;
    alloc.release(id).await.unwrap();
}

#[tokio::test]
async fn peer_list_excludes_viewer_and_unallocated() {
    let Some(fx) = fixture().await else { return };
    let alloc = PostgresAllocator::new(fx.db.clone());

    let a = insert_node(&fx.db, "node-a", Some("10.0.0.1")).await;
    let b = insert_node(&fx.db, "node-b", Some("10.0.0.2")).await;
    let c = insert_node(&fx.db, "node-c", Some("10.0.0.3")).await;
    // d has no underlay yet — must be excluded from peer lists.
    let _d = insert_node(&fx.db, "node-d", None).await;

    alloc.allocate_for_node(a).await.unwrap();
    alloc.allocate_for_node(b).await.unwrap();
    alloc.allocate_for_node(c).await.unwrap();

    let peers_seen_by_a = alloc.peer_list(a).await.unwrap();
    assert_eq!(peers_seen_by_a.len(), 2, "a sees b and c");

    let cidrs: Vec<_> = peers_seen_by_a
        .iter()
        .map(|p| p.compute_cidr.to_string())
        .collect();
    assert!(cidrs.contains(&"172.20.1.0/24".to_string()));
    assert!(cidrs.contains(&"172.20.2.0/24".to_string()));
    assert!(
        !cidrs.contains(&"172.20.0.0/24".to_string()),
        "viewer's own cidr must be excluded"
    );
}

#[tokio::test]
async fn get_alloc_returns_none_when_unallocated() {
    let Some(fx) = fixture().await else { return };
    let alloc = PostgresAllocator::new(fx.db.clone());

    let id = insert_node(&fx.db, "node-a", Some("10.0.0.1")).await;
    assert!(alloc.get_alloc(id).await.unwrap().is_none());

    alloc.allocate_for_node(id).await.unwrap();
    let got = alloc.get_alloc(id).await.unwrap().unwrap();
    assert_eq!(got.compute_cidr.to_string(), "172.20.0.0/24");
    assert_eq!(got.bridge_address.to_string(), "172.20.0.1");
}

#[tokio::test]
async fn external_id_is_stable_across_calls() {
    // The synthesized v5 UUID must be deterministic — two get_alloc calls
    // for the same node must return the same external_id.
    let Some(fx) = fixture().await else { return };
    let alloc = PostgresAllocator::new(fx.db.clone());

    let id = insert_node(&fx.db, "node-a", Some("10.0.0.1")).await;
    let first = alloc.allocate_for_node(id).await.unwrap();
    let second = alloc.get_alloc(id).await.unwrap().unwrap();
    assert_eq!(first.external_id, second.external_id);
}

#[tokio::test]
async fn pool_exhaustion_returns_typed_error() {
    let Some(fx) = fixture().await else { return };

    // Shrink the pool to two /30s to force exhaustion fast while preserving
    // the production invariant that a pool contains more than one node CIDR.
    fx.db
        .execute_unprepared(
            "UPDATE network_config SET compute_pool_cidr = '172.30.0.0/29', \
             subnet_prefix_len = 30 WHERE id = 1",
        )
        .await
        .unwrap();

    let alloc = PostgresAllocator::new(fx.db.clone());
    // /30 within /29 = exactly 2 subnets.
    let a = insert_node(&fx.db, "node-a", Some("10.0.0.1")).await;
    let b = insert_node(&fx.db, "node-b", Some("10.0.0.2")).await;
    let c = insert_node(&fx.db, "node-c", Some("10.0.0.3")).await;

    alloc.allocate_for_node(a).await.unwrap();
    alloc.allocate_for_node(b).await.unwrap();
    let err = alloc.allocate_for_node(c).await.unwrap_err();
    assert!(
        matches!(err, AllocatorError::PoolExhausted { .. }),
        "got {:?}",
        err
    );
}

// ---------------------------------------------------------------------------
// WireGuard mesh registration
// ---------------------------------------------------------------------------

fn mesh_key(seed: u8) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode([seed; 32])
}

/// One container for the whole mesh lifecycle: enabling, address assignment,
/// stable re-registration, key collisions, underlay switch, peer lists.
#[tokio::test]
async fn mesh_registration_assigns_stable_addresses_and_switches_the_underlay() {
    use temps_network::mesh::{self, MeshError};

    let Some(fx) = fixture().await else { return };
    let db = fx.db.clone();
    // Nodes that joined with public addresses: no private underlay yet.
    let node_a = insert_node(&db, "node-a", Some("203.0.113.10")).await;
    let node_b = insert_node(&db, "node-b", Some("198.51.100.20")).await;
    let endpoint_a = "203.0.113.10:51820".parse().unwrap();
    let endpoint_b = "198.51.100.20:51820".parse().unwrap();

    // Off by default: registration is refused and nobody gets mesh peers.
    assert_eq!(mesh::load_settings(&db).await.unwrap(), None);
    assert_eq!(
        mesh::register_node(&db, node_a, &mesh_key(1), endpoint_a).await,
        Err(MeshError::Disabled)
    );

    // A pool overlapping the compute pool is refused.
    assert!(matches!(
        mesh::enable(&db, Some("172.20.0.0/24"), None, None).await,
        Err(MeshError::OverlapsComputePool { .. })
    ));
    let settings = mesh::enable(&db, Some("10.201.0.0/24"), Some(51820), None)
        .await
        .unwrap();
    assert_eq!(settings.control_plane_address().to_string(), "10.201.0.1");

    let a = mesh::register_node(&db, node_a, &mesh_key(1), endpoint_a)
        .await
        .unwrap();
    let b = mesh::register_node(&db, node_b, &mesh_key(2), endpoint_b)
        .await
        .unwrap();
    assert_eq!(a.address.to_string(), "10.201.0.2");
    assert_eq!(b.address.to_string(), "10.201.0.3");
    assert_eq!((a.prefix_len, a.listen_port), (24, 51820));

    // Re-registering (agent restart, new endpoint) keeps the address.
    let moved = "203.0.113.99:51820".parse().unwrap();
    let again = mesh::register_node(&db, node_a, &mesh_key(1), moved)
        .await
        .unwrap();
    assert_eq!(again.address, a.address);

    // Another node cannot claim a key already in the mesh.
    assert_eq!(
        mesh::register_node(&db, node_b, &mesh_key(1), endpoint_b).await,
        Err(MeshError::PublicKeyInUse)
    );
    assert_eq!(
        mesh::register_node(&db, node_b, "not-a-key", endpoint_b).await,
        Err(MeshError::InvalidPublicKey)
    );

    // The mesh address is now each node's underlay, so allocation works for
    // nodes that joined with public addresses.
    let row = nodes::Entity::find_by_id(node_a)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.underlay_address.as_deref(), Some("10.201.0.2"));
    let allocator = PostgresAllocator::new(db.clone());
    allocator.allocate_for_node(node_a).await.unwrap();

    // The pool and port are frozen once nodes hold addresses; re-running
    // with the same settings is fine.
    assert!(matches!(
        mesh::enable(&db, Some("10.202.0.0/24"), None, None).await,
        Err(MeshError::InUse {
            setting: "pool",
            ..
        })
    ));
    assert!(matches!(
        mesh::enable(&db, None, Some(51821), None).await,
        Err(MeshError::InUse {
            setting: "port",
            ..
        })
    ));
    assert_eq!(
        mesh::enable(&db, Some("10.201.0.0/24"), Some(51820), None).await,
        Ok(settings.clone())
    );
    // WireGuard can't share the VXLAN port.
    assert_eq!(
        mesh::enable(&db, None, Some(4789), None).await,
        Err(MeshError::PortClashesWithVxlan(4789))
    );

    // Peer lists: a worker sees the control plane (once published) and the
    // other worker, never itself; the control plane sees every worker.
    let cp_key = mesh_key(9);
    mesh::publish_control_plane(&db, &cp_key, Some("192.0.2.1:51820".parse().unwrap()))
        .await
        .unwrap();
    let for_a = mesh::peers(&db, Some(node_a)).await.unwrap();
    assert_eq!(
        for_a
            .iter()
            .map(|p| (p.name.as_str(), p.peer.address.to_string()))
            .collect::<Vec<_>>(),
        vec![
            ("control-plane", "10.201.0.1".to_string()),
            ("node-b", "10.201.0.3".to_string()),
        ]
    );
    assert_eq!(for_a[1].peer.endpoint, Some(endpoint_b));
    let for_cp = mesh::peers(&db, None).await.unwrap();
    assert_eq!(for_cp.len(), 2);
    assert!(for_cp.iter().all(|p| p.name != "control-plane"));

    // The control plane's key cannot be registered by a node either.
    assert_eq!(
        mesh::register_node(&db, node_b, &cp_key, endpoint_b).await,
        Err(MeshError::PublicKeyInUse)
    );

    // A removed node disappears from every peer list (revocation).
    nodes::Entity::delete_by_id(node_b)
        .exec(db.as_ref())
        .await
        .unwrap();
    let for_a = mesh::peers(&db, Some(node_a)).await.unwrap();
    assert_eq!(for_a.len(), 1);
    assert_eq!(for_a[0].name, "control-plane");
}

/// One-paste pairing (ADR 048 D2b): a pairing reserves an address, the
/// control plane peers with it once the node's key arrives, and the node
/// registering with the pairing's token takes over key and address.
#[tokio::test]
async fn a_pairing_reserves_an_address_and_hands_it_to_the_node_it_enrolls() {
    use temps_entities::node_enrollment_tokens;
    use temps_network::{
        mesh::{self, MeshError},
        pairing::{self, NewPairing},
    };

    let Some(fx) = fixture().await else { return };
    let db = fx.db.clone();
    let now = chrono::Utc::now();
    let token = |id: &str| node_enrollment_tokens::ActiveModel {
        token_hash: Set(format!("hash-{id}")),
        max_uses: Set(1),
        used_count: Set(0),
        expires_at: Set(now + chrono::Duration::minutes(30)),
        bound_node_name: Set(Some(id.to_string())),
        bound_labels: Set(None),
        created_by_user_id: Set(None),
        revoked_at: Set(None),
        ca_fingerprint: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    };
    let token_a = token("paired-a").insert(db.as_ref()).await.unwrap().id;
    let token_b = token("paired-b").insert(db.as_ref()).await.unwrap().id;
    let new = |name: &str, token: i32, endpoint: &str| NewPairing {
        pairing_id: format!("id-{name}"),
        name: name.to_string(),
        node_endpoint: endpoint.parse().unwrap(),
        secret_encrypted: "encrypted".into(),
        enrollment_token_id: token,
        expires_at: now + chrono::Duration::minutes(30),
        created_by_user_id: None,
    };

    // Pairing needs the mesh.
    assert!(matches!(
        pairing::create(&db, new("paired-a", token_a, "198.51.100.7:51820")).await,
        Err(MeshError::Disabled)
    ));
    mesh::enable(&db, Some("10.203.0.0/24"), Some(51820), None)
        .await
        .unwrap();
    mesh::publish_control_plane(&db, &mesh_key(90), None)
        .await
        .unwrap();

    let a = pairing::create(&db, new("paired-a", token_a, "198.51.100.7:51820"))
        .await
        .unwrap();
    let b = pairing::create(&db, new("paired-b", token_b, "198.51.100.8:51820"))
        .await
        .unwrap();
    assert_eq!(a.mesh_address, "10.203.0.2");
    assert_eq!(
        b.mesh_address, "10.203.0.3",
        "pending pairings hold their address"
    );

    // A node registering on its own does not take a reserved address.
    let other = insert_node(&db, "other", Some("203.0.113.5")).await;
    let other_reg = mesh::register_node(
        &db,
        other,
        &mesh_key(3),
        "203.0.113.5:51820".parse().unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(other_reg.address.to_string(), "10.203.0.4");

    // Both are dialed until the node answers; keys must be unique.
    assert_eq!(pairing::due(&db).await.unwrap().len(), 2);
    assert_eq!(
        pairing::record_key(&db, a.id, &mesh_key(3)).await,
        Err(MeshError::PublicKeyInUse)
    );
    pairing::record_key(&db, a.id, &mesh_key(1)).await.unwrap();
    assert_eq!(pairing::due(&db).await.unwrap().len(), 1);
    // A pairing that stopped waiting (here: it already has its key) takes
    // no other key, so the node is not confirmed for it.
    assert_eq!(
        pairing::record_key(&db, a.id, &mesh_key(4)).await,
        Err(MeshError::PairingClosed)
    );

    // A refusal outlives later attempts' errors, until a key is accepted.
    pairing::record_rejection(&db, b.id, "its key belongs to another node")
        .await
        .unwrap();
    pairing::record_attempt(&db, b.id, Some("no answer yet"))
        .await
        .unwrap();
    let refused = pairing::get(&db, b.id).await.unwrap().unwrap();
    assert_eq!(refused.last_error.as_deref(), Some("no answer yet"));
    assert_eq!(
        refused.last_rejection.as_deref(),
        Some("its key belongs to another node")
    );
    pairing::record_key(&db, b.id, &mesh_key(5)).await.unwrap();
    let accepted = pairing::get(&db, b.id).await.unwrap().unwrap();
    assert_eq!(accepted.last_rejection, None);
    assert_eq!(accepted.status, pairing::STATUS_KEY_RECEIVED);

    // The control plane peers with the pairing; workers do not see it.
    let cp_view = mesh::peers(&db, None).await.unwrap();
    let paired = cp_view
        .iter()
        .find(|peer| peer.peer.public_key == mesh_key(1))
        .expect("the control plane peers with the pairing");
    assert_eq!(paired.peer.address.to_string(), "10.203.0.2");
    assert_eq!(
        paired.peer.endpoint,
        Some("198.51.100.7:51820".parse().unwrap())
    );
    assert!(mesh::peers(&db, Some(other))
        .await
        .unwrap()
        .iter()
        .all(|peer| peer.peer.public_key != mesh_key(1)));

    // Registering with the pairing's token hands the node its key and
    // reserved address, and completes the pairing.
    let node_a = insert_node(&db, "paired-a", Some("198.51.100.7")).await;
    pairing::link_node(&db, token_a, node_a)
        .await
        .unwrap()
        .unwrap();
    let row = nodes::Entity::find_by_id(node_a)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.mesh_wg_address.as_deref(), Some("10.203.0.2"));
    assert_eq!(row.mesh_wg_public_key, Some(mesh_key(1)));
    assert_eq!(row.underlay_address.as_deref(), Some("10.203.0.2"));
    assert_eq!(
        pairing::get(&db, a.id).await.unwrap().unwrap().status,
        pairing::STATUS_COMPLETED
    );
    // The agent registering the same key afterwards changes nothing.
    let again = mesh::register_node(
        &db,
        node_a,
        &mesh_key(1),
        "198.51.100.7:51820".parse().unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(again.address.to_string(), "10.203.0.2");
    // One peer entry for it, from the node row now.
    assert_eq!(
        mesh::peers(&db, None)
            .await
            .unwrap()
            .iter()
            .filter(|peer| peer.peer.public_key == mesh_key(1))
            .count(),
        1
    );

    // Cancelling releases the address; a finished pairing cannot be cancelled.
    assert!(pairing::cancel(&db, b.id).await.unwrap());
    assert!(!pairing::cancel(&db, b.id).await.unwrap());
    assert!(!pairing::cancel(&db, a.id).await.unwrap());
    let token_c = token("paired-c").insert(db.as_ref()).await.unwrap().id;
    let c = pairing::create(&db, new("paired-c", token_c, "198.51.100.9:51820"))
        .await
        .unwrap();
    assert_eq!(
        c.mesh_address, "10.203.0.3",
        "a cancelled pairing's address is reused"
    );

    // Expired pairings stop being dialed and release their address.
    use sea_orm::{ColumnTrait, QueryFilter};
    temps_entities::node_pairings::Entity::update_many()
        .col_expr(
            temps_entities::node_pairings::Column::ExpiresAt,
            sea_orm::sea_query::Expr::value(now - chrono::Duration::minutes(1)),
        )
        .filter(temps_entities::node_pairings::Column::Id.eq(c.id))
        .exec(db.as_ref())
        .await
        .unwrap();
    assert!(pairing::due(&db).await.unwrap().is_empty());
    assert_eq!(pairing::expire_stale(&db).await.unwrap(), 1);
    assert_eq!(
        pairing::get(&db, c.id).await.unwrap().unwrap().status,
        pairing::STATUS_EXPIRED
    );
}

#[tokio::test]
async fn pairings_in_progress_are_capped_until_one_finishes() {
    use temps_entities::node_enrollment_tokens;
    use temps_network::{
        mesh::{self, MeshError},
        pairing::{self, NewPairing, MAX_PENDING},
    };

    let Some(fx) = fixture().await else { return };
    let db = fx.db.clone();
    let now = chrono::Utc::now();
    mesh::enable(&db, Some("10.203.0.0/24"), Some(51820), None)
        .await
        .unwrap();
    let create = |index: usize| {
        let db = db.clone();
        async move {
            let token = node_enrollment_tokens::ActiveModel {
                token_hash: Set(format!("hash-{index}")),
                max_uses: Set(1),
                used_count: Set(0),
                expires_at: Set(now + chrono::Duration::minutes(30)),
                bound_node_name: Set(Some(format!("node-{index}"))),
                created_at: Set(now),
                updated_at: Set(now),
                ..Default::default()
            }
            .insert(db.as_ref())
            .await
            .unwrap();
            pairing::create(
                &db,
                NewPairing {
                    pairing_id: format!("id-{index}"),
                    name: format!("node-{index}"),
                    node_endpoint: format!("198.51.100.{}:51820", index + 1).parse().unwrap(),
                    secret_encrypted: "encrypted".into(),
                    enrollment_token_id: token.id,
                    expires_at: now + chrono::Duration::minutes(30),
                    created_by_user_id: None,
                },
            )
            .await
        }
    };
    let mut first = None;
    for index in 0..MAX_PENDING {
        let created = create(index).await.unwrap();
        first.get_or_insert(created.id);
    }
    assert_eq!(
        create(MAX_PENDING).await,
        Err(MeshError::TooManyPairings { limit: MAX_PENDING })
    );
    pairing::cancel(&db, first.unwrap()).await.unwrap();
    create(MAX_PENDING + 1).await.unwrap();
}

fn enrollment_token(name: &str) -> temps_entities::node_enrollment_tokens::ActiveModel {
    let now = chrono::Utc::now();
    temps_entities::node_enrollment_tokens::ActiveModel {
        token_hash: Set(format!("hash-{name}")),
        max_uses: Set(1),
        used_count: Set(0),
        expires_at: Set(now + chrono::Duration::minutes(30)),
        bound_node_name: Set(Some(name.to_string())),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
}

fn new_pairing(
    name: &str,
    token: i32,
    endpoint: &str,
    expires_in: chrono::Duration,
) -> temps_network::pairing::NewPairing {
    temps_network::pairing::NewPairing {
        pairing_id: format!("id-{name}"),
        name: name.to_string(),
        node_endpoint: endpoint.parse().unwrap(),
        secret_encrypted: "encrypted".into(),
        enrollment_token_id: token,
        expires_at: chrono::Utc::now() + expires_in,
        created_by_user_id: None,
    }
}

/// Under a locale collation, base64 keys differing in case sort unlike
/// their bytes ("O…" > "c…" in en-US, "O…" < "c…" in byte order). The
/// routing table must still accept every pair the control plane writes.
#[tokio::test]
async fn mesh_links_keep_byte_order_under_a_locale_collation() {
    use std::collections::HashMap;
    use temps_network::{mesh, mesh_links};

    let Some(fx) =
        fixture_with("TEMPLATE template0 LOCALE_PROVIDER icu ICU_LOCALE 'en-US' LOCALE 'C.UTF-8'")
            .await
    else {
        return;
    };
    let db = fx.db.clone();
    // The database really sorts these two the other way round.
    let locale_says: bool = db
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT 'ODg' < 'cHB' AS lt".to_string(),
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get("", "lt")
        .unwrap();
    assert!(
        !locale_says,
        "the test database must use a locale collation"
    );

    mesh::enable(&db, Some("10.201.0.0/24"), Some(51820), None)
        .await
        .unwrap();
    // 90 → "Wl…", 56 → "ODg…", 112 → "cHB…".
    mesh::publish_control_plane(&db, &mesh_key(90), None)
        .await
        .unwrap();
    for (name, seed, ip) in [
        ("node-o", 56, "203.0.113.10"),
        ("node-c", 112, "203.0.113.11"),
    ] {
        let node = insert_node(&db, name, Some(ip)).await;
        mesh::register_node(
            &db,
            node,
            &mesh_key(seed),
            format!("{ip}:51820").parse().unwrap(),
        )
        .await
        .unwrap();
    }

    mesh_links::evaluate(&db, &HashMap::new()).await.unwrap();
    assert_eq!(mesh_links::load_links(&db).await.unwrap().len(), 3);
}

/// The hub lifecycle through the database: who can be the hub, which
/// reports count, rerouting a pair that never connected, the routed peer
/// lists, and pruning a member that left.
#[tokio::test]
async fn a_hub_carries_pairs_that_never_connected_until_it_is_removed() {
    use std::collections::HashMap;
    use temps_entities::{mesh_links as links_table, node_mesh_reports};
    use temps_network::{
        mesh::{self, MeshError},
        mesh_links::{self, Hub},
    };

    let Some(fx) = fixture().await else { return };
    let db = fx.db.clone();
    assert_eq!(
        mesh_links::set_hub(&db, Some(Hub::ControlPlane)).await,
        Err(MeshError::Disabled)
    );
    mesh::enable(&db, Some("10.201.0.0/24"), Some(51820), None)
        .await
        .unwrap();
    mesh::publish_control_plane(&db, &mesh_key(90), None)
        .await
        .unwrap();
    let a = insert_node(&db, "node-a", Some("203.0.113.10")).await;
    let b = insert_node(&db, "node-b", Some("198.51.100.20")).await;
    let off_mesh = insert_node(&db, "node-off", Some("198.51.100.30")).await;
    let reg_a = mesh::register_node(&db, a, &mesh_key(1), "203.0.113.10:51820".parse().unwrap())
        .await
        .unwrap();
    let reg_b = mesh::register_node(&db, b, &mesh_key(2), "198.51.100.20:51820".parse().unwrap())
        .await
        .unwrap();

    assert_eq!(
        mesh_links::set_hub(&db, Some(Hub::Node(9999))).await,
        Err(MeshError::NodeNotFound(9999))
    );
    assert_eq!(
        mesh_links::set_hub(&db, Some(Hub::Node(off_mesh))).await,
        Err(MeshError::NotOnMesh("node-off".into()))
    );

    // Reports keep only members' keys, and drop ages that do not fit.
    let cp = mesh_key(90);
    mesh_links::record_report(
        &db,
        a,
        &HashMap::from([
            (cp.clone(), 5),
            ("not-a-member".to_string(), 5),
            (mesh_key(2), u64::MAX),
        ]),
    )
    .await
    .unwrap();
    mesh_links::record_report(&db, b, &HashMap::from([(cp.clone(), 5)]))
        .await
        .unwrap();
    let report = node_mesh_reports::Entity::find_by_id(a)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();
    let keys: Vec<_> = report
        .handshakes
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    assert_eq!(keys, vec![cp.clone()]);

    // The control plane reaches both; a and b never handshook.
    let now = chrono::Utc::now();
    let cp_view = HashMap::from([(mesh_key(1), now), (mesh_key(2), now)]);
    mesh_links::evaluate(&db, &cp_view).await.unwrap();
    let pair = if mesh_key(1) < mesh_key(2) {
        (mesh_key(1), mesh_key(2))
    } else {
        (mesh_key(2), mesh_key(1))
    };
    let links = mesh_links::load_links(&db).await.unwrap();
    assert!(!links[&pair].via_hub, "every pair starts direct");

    // Past the grace period, with no hub: nothing moves.
    db.execute_unprepared("UPDATE mesh_links SET since = NOW() - INTERVAL '10 minutes'")
        .await
        .unwrap();
    mesh_links::evaluate(&db, &cp_view).await.unwrap();
    assert!(!mesh_links::load_links(&db).await.unwrap()[&pair].via_hub);

    // With the control plane as hub the pair moves onto it...
    mesh_links::set_hub(&db, Some(Hub::ControlPlane))
        .await
        .unwrap();
    mesh_links::evaluate(&db, &cp_view).await.unwrap();
    assert!(mesh_links::load_links(&db).await.unwrap()[&pair].via_hub);

    // ...so a reaches b through the control plane's entry.
    let peers_of_a = mesh::peers(&db, Some(a)).await.unwrap();
    let names: Vec<_> = peers_of_a.iter().map(|peer| peer.name.as_str()).collect();
    assert_eq!(names, vec!["control-plane"]);
    assert_eq!(peers_of_a[0].peer.relayed, vec![reg_b.address]);
    let peers_of_b = mesh::peers(&db, Some(b)).await.unwrap();
    assert_eq!(peers_of_b[0].peer.relayed, vec![reg_a.address]);

    // Removing the hub sends the pair back to the direct path.
    mesh_links::set_hub(&db, None).await.unwrap();
    mesh_links::evaluate(&db, &cp_view).await.unwrap();
    assert!(!mesh_links::load_links(&db).await.unwrap()[&pair].via_hub);

    // A member that leaves takes its pairs with it.
    nodes::Entity::delete_by_id(b)
        .exec(db.as_ref())
        .await
        .unwrap();
    mesh_links::evaluate(&db, &cp_view).await.unwrap();
    let left = links_table::Entity::find().all(db.as_ref()).await.unwrap();
    assert!(left
        .iter()
        .all(|link| link.key_a != mesh_key(2) && link.key_b != mesh_key(2)));
    assert_eq!(left.len(), 1, "control plane and node-a remain");
}

/// A pairing that cannot complete must not half-register its node, and a
/// key is refused wherever another member or pending pairing holds it.

#[tokio::test]
async fn a_pairing_cancelled_after_the_link_check_never_links() {
    use temps_entities::node_pairings;
    use temps_network::{
        mesh::{self, MeshError},
        pairing,
    };

    let Some(fx) = fixture().await else { return };
    let db = fx.db.clone();
    mesh::enable(&db, Some("10.204.0.0/24"), Some(51820), None)
        .await
        .unwrap();
    mesh::publish_control_plane(&db, &mesh_key(90), None)
        .await
        .unwrap();
    let token = enrollment_token("late-cancel")
        .insert(db.as_ref())
        .await
        .unwrap()
        .id;
    let created = pairing::create(
        &db,
        new_pairing(
            "late-cancel",
            token,
            "198.51.100.9:51820",
            chrono::Duration::minutes(30),
        ),
    )
    .await
    .unwrap();
    pairing::record_key(&db, created.id, &mesh_key(11))
        .await
        .unwrap();

    // The node passed the check before it was created...
    assert_eq!(pairing::check_linkable(&db, token).await, Ok(()));
    // ...then the pairing was cancelled (an operator, or recovery of an
    // interrupted SSH enrollment) before the link ran.
    assert!(pairing::cancel(&db, created.id).await.unwrap());

    let node = insert_node(&db, "late-cancel", Some("198.51.100.9")).await;
    assert_eq!(
        pairing::link_node(&db, token, node).await.map(|_| ()),
        Err(MeshError::PairingClosed)
    );
    let row = node_pairings::Entity::find_by_id(created.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.status, pairing::STATUS_CANCELLED);
    assert_eq!(row.node_id, None, "a cancelled pairing is never linked");
    let registered = nodes::Entity::find_by_id(node)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(registered.mesh_wg_public_key, None);
    assert_eq!(registered.mesh_wg_address, None);
}
#[tokio::test]
async fn pairings_refuse_taken_keys_and_link_atomically() {
    use temps_entities::node_pairings;
    use temps_network::{
        mesh::{self, MeshError},
        pairing,
    };

    let Some(fx) = fixture().await else { return };
    let db = fx.db.clone();
    mesh::enable(&db, Some("10.203.0.0/24"), Some(51820), None)
        .await
        .unwrap();
    mesh::publish_control_plane(&db, &mesh_key(90), None)
        .await
        .unwrap();
    let minutes = chrono::Duration::minutes;
    let token_a = enrollment_token("paired-a")
        .insert(db.as_ref())
        .await
        .unwrap()
        .id;
    let token_b = enrollment_token("paired-b")
        .insert(db.as_ref())
        .await
        .unwrap()
        .id;
    let token_c = enrollment_token("paired-c")
        .insert(db.as_ref())
        .await
        .unwrap()
        .id;

    // An endpoint inside the mesh pool is not a node's public address.
    assert!(pairing::create(
        &db,
        new_pairing("paired-a", token_a, "10.203.0.50:51820", minutes(30))
    )
    .await
    .is_err());
    let a = pairing::create(
        &db,
        new_pairing("paired-a", token_a, "198.51.100.7:51820", minutes(30)),
    )
    .await
    .unwrap();
    let b = pairing::create(
        &db,
        new_pairing("paired-b", token_b, "198.51.100.8:51820", minutes(30)),
    )
    .await
    .unwrap();

    // One dialer per pairing across control-plane processes.
    let lease = std::time::Duration::from_secs(30);
    assert!(pairing::claim(&db, a.id, lease).await.unwrap());
    assert!(!pairing::claim(&db, a.id, lease).await.unwrap());
    pairing::release(&db, a.id).await.unwrap();
    assert!(pairing::claim(&db, a.id, lease).await.unwrap());

    // Keys: malformed, the control plane's, another pending pairing's.
    assert_eq!(
        pairing::record_key(&db, a.id, "not-a-key").await,
        Err(MeshError::InvalidPublicKey)
    );
    assert_eq!(
        pairing::record_key(&db, a.id, &mesh_key(90)).await,
        Err(MeshError::PublicKeyInUse)
    );
    pairing::record_key(&db, a.id, &mesh_key(5)).await.unwrap();
    assert_eq!(
        pairing::record_key(&db, b.id, &mesh_key(5)).await,
        Err(MeshError::PublicKeyInUse)
    );
    // A node registering on its own cannot take a key a pairing holds.
    let other = insert_node(&db, "other", Some("203.0.113.5")).await;
    assert_eq!(
        mesh::register_node(
            &db,
            other,
            &mesh_key(5),
            "203.0.113.5:51820".parse().unwrap()
        )
        .await,
        Err(MeshError::PublicKeyInUse)
    );

    // Past its deadline, a pairing takes no key even before the sweep.
    let late = pairing::create(
        &db,
        new_pairing("paired-c", token_c, "198.51.100.9:51820", minutes(-1)),
    )
    .await
    .unwrap();
    assert_eq!(
        pairing::record_key(&db, late.id, &mesh_key(7)).await,
        Err(MeshError::PairingClosed)
    );

    // The pairing can complete: the node registers and adopts its address.
    assert_eq!(pairing::check_linkable(&db, token_a).await, Ok(()));
    assert_eq!(
        pairing::check_linkable(&db, token_b).await,
        Err(MeshError::PairingClosed)
    );

    // If another node took the key meanwhile, nothing is linked.
    db.execute_unprepared(&format!(
        "UPDATE nodes SET mesh_wg_public_key = '{}' WHERE id = {other}",
        mesh_key(5)
    ))
    .await
    .unwrap();
    assert_eq!(
        pairing::check_linkable(&db, token_a).await,
        Err(MeshError::PublicKeyInUse)
    );
    let paired = insert_node(&db, "paired-a", Some("198.51.100.7")).await;
    assert_eq!(
        pairing::link_node(&db, token_a, paired).await.map(|_| ()),
        Err(MeshError::PublicKeyInUse)
    );
    let row = node_pairings::Entity::find_by_id(a.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.node_id, None, "the failed link was rolled back");

    // With the key free again, linking completes the pairing.
    db.execute_unprepared(&format!(
        "UPDATE nodes SET mesh_wg_public_key = NULL WHERE id = {other}"
    ))
    .await
    .unwrap();
    pairing::link_node(&db, token_a, paired).await.unwrap();
    let node = nodes::Entity::find_by_id(paired)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        node.mesh_wg_address.as_deref(),
        Some(a.mesh_address.as_str())
    );
    assert_eq!(
        node.mesh_wg_public_key.as_deref(),
        Some(mesh_key(5).as_str())
    );
}
