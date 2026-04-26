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
use temps_entities::nodes;
use temps_migrations::Migrator;
use temps_network::allocator::{AllocatorError, ComputeNetworkAllocator, PostgresAllocator};
use testcontainers::{runners::AsyncRunner, GenericImage, ImageExt};

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

struct Fixture {
    db: Arc<DatabaseConnection>,
    // Hold the container so it stays alive for the test's lifetime.
    _container: testcontainers::ContainerAsync<GenericImage>,
}

async fn fixture() -> Option<Fixture> {
    if std::env::var("TEMPS_TEST_DATABASE_URL").is_ok() {
        eprintln!("⏭️  skipping: external database in use");
        return None;
    }
    let container = match GenericImage::new("timescale/timescaledb-ha", "pg18")
        .with_env_var("POSTGRES_DB", "postgres")
        .with_env_var("POSTGRES_USER", "postgres")
        .with_env_var("POSTGRES_PASSWORD", "postgres")
        .with_env_var("POSTGRES_HOST_AUTH_METHOD", "trust")
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
    let db_url = format!("postgresql://postgres:postgres@localhost:{}/postgres", port);
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;

    let mut retries = 5;
    let db = loop {
        match Database::connect(&db_url).await {
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
    };
    Migrator::up(&db, None).await.expect("migrations");

    Some(Fixture {
        db: Arc::new(db),
        _container: container,
    })
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

    // Shrink the pool to a single /30 to force exhaustion fast.
    fx.db
        .execute_unprepared(
            "UPDATE network_config SET compute_pool_cidr = '172.30.0.0/30', \
             subnet_prefix_len = 30 WHERE id = 1",
        )
        .await
        .unwrap();

    let alloc = PostgresAllocator::new(fx.db.clone());
    // /30 within /30 = exactly 1 subnet.
    let a = insert_node(&fx.db, "node-a", Some("10.0.0.1")).await;
    let b = insert_node(&fx.db, "node-b", Some("10.0.0.2")).await;

    alloc.allocate_for_node(a).await.unwrap();
    let err = alloc.allocate_for_node(b).await.unwrap_err();
    assert!(
        matches!(err, AllocatorError::PoolExhausted { .. }),
        "got {:?}",
        err
    );
}
