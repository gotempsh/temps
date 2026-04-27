//! Integration test: reconciler-shaped DNS records survive a failover.
//!
//! Drives the DnsRegistry against a real Postgres with the same drafts the
//! reconciler would produce, simulates a primary promotion, and verifies
//! that the role records flip atomically. Skips gracefully when Docker
//! isn't available.

use std::collections::HashMap;
use std::sync::Arc;

use sea_orm::Database;
use temps_dns::{DnsRegistry, InternalOwnerKind};
use temps_migrations::{Migrator, MigratorTrait};
use temps_providers::externalsvc::postgres_role_reconciler::{drafts_for_snapshot, MonitorNode};
use testcontainers::{runners::AsyncRunner, GenericImage, ImageExt};

async fn boot_db() -> Option<Arc<sea_orm::DatabaseConnection>> {
    if std::env::var("TEMPS_TEST_DATABASE_URL").is_ok() {
        eprintln!("⏭️  TEMPS_TEST_DATABASE_URL set — skipping reconciler integration test");
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
            eprintln!("⏭️  Docker unavailable, skipping: {e}");
            return None;
        }
    };
    let port = container.get_host_port_ipv4(5432).await.ok()?;
    let db_url = format!("postgresql://postgres:postgres@localhost:{}/postgres", port);
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;

    let mut tries = 5;
    let db = loop {
        match Database::connect(&db_url).await {
            Ok(db) => break db,
            Err(_) if tries > 0 => {
                tries -= 1;
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
            Err(e) => panic!("connect failed: {e}"),
        }
    };
    Migrator::up(&db, None).await.expect("migrations");
    // Keep container alive for the duration of the connection — leaking is
    // fine in a test process that exits at the end.
    Box::leak(Box::new(container));
    Some(Arc::new(db))
}

fn node(id: i64, host: &str, state: &str) -> MonitorNode {
    MonitorNode {
        nodeid: id,
        nodename: format!("node-{id}"),
        nodehost: host.into(),
        nodeport: 6001,
        reported_state: state.into(),
    }
}

fn ip_map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[tokio::test]
async fn reconciler_failover_updates_role_records_atomically() {
    let Some(db) = boot_db().await else { return };
    let registry = DnsRegistry::new(db.clone());

    let service_id: i64 = 7;
    let service_name = "orders";

    let ips = ip_map(&[
        ("orders-1.orders.temps.local", "172.20.5.10"),
        ("orders-2.orders.temps.local", "172.20.5.11"),
        ("orders-3.orders.temps.local", "172.20.5.12"),
    ]);

    // ---- Tick 1: node-1 primary, node-2 + node-3 secondaries ----
    let monitor_t1 = vec![
        node(1, "orders-1.orders.temps.local", "primary"),
        node(2, "orders-2.orders.temps.local", "secondary"),
        node(3, "orders-3.orders.temps.local", "secondary"),
    ];
    let drafts_t1 = drafts_for_snapshot(service_id as i32, service_name, &monitor_t1, &ips);
    let g1 = registry
        .replace_endpoints_for_owner(InternalOwnerKind::ServiceRole, service_id, &drafts_t1)
        .await
        .expect("tick 1 commits");

    let snap1 = registry.get_full_zone().await.expect("zone after t1");
    let primary_t1: Vec<_> = snap1
        .records
        .iter()
        .filter(|r| r.fqdn == "primary.orders.temps.local")
        .collect();
    assert_eq!(primary_t1.len(), 1);
    assert_eq!(primary_t1[0].target_ip.as_deref(), Some("172.20.5.10"));
    let replicas_t1: Vec<_> = snap1
        .records
        .iter()
        .filter(|r| r.fqdn == "replica.orders.temps.local")
        .collect();
    assert_eq!(replicas_t1.len(), 2);

    // ---- Tick 2: failover. node-1 demoted, node-2 promoted ----
    let monitor_t2 = vec![
        node(1, "orders-1.orders.temps.local", "draining"),
        node(2, "orders-2.orders.temps.local", "primary"),
        node(3, "orders-3.orders.temps.local", "secondary"),
    ];
    let drafts_t2 = drafts_for_snapshot(service_id as i32, service_name, &monitor_t2, &ips);
    let g2 = registry
        .replace_endpoints_for_owner(InternalOwnerKind::ServiceRole, service_id, &drafts_t2)
        .await
        .expect("tick 2 commits");

    assert!(
        g2 > g1,
        "generation must monotonically increase across ticks"
    );

    let snap2 = registry.get_full_zone().await.expect("zone after t2");
    let primary_t2: Vec<_> = snap2
        .records
        .iter()
        .filter(|r| r.fqdn == "primary.orders.temps.local")
        .collect();
    assert_eq!(
        primary_t2.len(),
        1,
        "exactly one primary record after failover, got {primary_t2:?}"
    );
    assert_eq!(
        primary_t2[0].target_ip.as_deref(),
        Some("172.20.5.11"),
        "primary record must point at the promoted replica's IP after failover"
    );

    // The draining node must be gone from the VIP set.
    let vip_ips: Vec<_> = snap2
        .records
        .iter()
        .filter(|r| r.fqdn == "orders.temps.local")
        .filter_map(|r| r.target_ip.as_deref())
        .collect();
    assert!(
        !vip_ips.contains(&"172.20.5.10"),
        "draining node must be removed from VIP, got: {vip_ips:?}"
    );
    assert!(
        vip_ips.contains(&"172.20.5.11") && vip_ips.contains(&"172.20.5.12"),
        "VIP must include the new primary and the remaining replica, got: {vip_ips:?}"
    );

    // No old role records linger — the atomic replace dropped them.
    let total_role_records = snap2
        .records
        .iter()
        .filter(|r| r.owner_kind == "service_role" && r.owner_id == service_id)
        .count();
    assert_eq!(
        total_role_records,
        drafts_t2.len(),
        "after replace_endpoints_for_owner, only the new drafts should be present \
         (no leftover records from the previous tick)"
    );
}
