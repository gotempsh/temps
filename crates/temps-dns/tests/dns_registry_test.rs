//! Integration tests for [`DnsRegistry`] (ADR-011).
//!
//! These run against a real TimescaleDB container — the schema invariants
//! we care about (monotonic generation, atomic replace, FK cascade) only
//! show up against a real Postgres engine. Skips gracefully when Docker is
//! unavailable, matching the convention from
//! `temps-migrations/tests/migration_tests.rs`.

use std::sync::Arc;

use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};
use temps_dns::services::{DnsRegistry, EndpointDraft, OwnerKind, RecordType};
use temps_migrations::{Migrator, MigratorTrait};
use testcontainers::{runners::AsyncRunner, GenericImage, ImageExt};

async fn boot_db() -> Option<Arc<DatabaseConnection>> {
    if std::env::var("TEMPS_TEST_DATABASE_URL").is_ok() {
        // External DB — pretend Docker is missing rather than mutating shared state.
        eprintln!("⏭️  TEMPS_TEST_DATABASE_URL set — skipping DnsRegistry integration test");
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
            eprintln!("⏭️  Docker unavailable, skipping DnsRegistry test: {e}");
            return None;
        }
    };

    let port = container.get_host_port_ipv4(5432).await.ok()?;
    let db_url = format!("postgresql://postgres:postgres@localhost:{}/postgres", port);
    tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

    let mut tries = 5;
    let db = loop {
        match Database::connect(&db_url).await {
            Ok(db) => break db,
            Err(_) if tries > 0 => {
                tries -= 1;
                tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
            }
            Err(e) => panic!("connect failed: {e}"),
        }
    };
    Migrator::up(&db, None).await.expect("migrations");
    // Hold the container handle alive for the lifetime of the connection
    // by leaking it. This is a test — it's fine — and the alternative
    // (returning the container) makes every caller juggle two values.
    Box::leak(Box::new(container));
    Some(Arc::new(db))
}

async fn insert_node(db: &DatabaseConnection, name: &str, token_hash: &str) -> i32 {
    db.execute(Statement::from_string(
        sea_orm::DatabaseBackend::Postgres,
        format!(
            "INSERT INTO nodes (name, token_hash, address, private_address, role, status, \
             labels, capacity) \
             VALUES ('{name}', '{token_hash}', '127.0.0.1', '10.0.0.1', \
             'worker', 'active', '{{}}', '{{}}')"
        ),
    ))
    .await
    .expect("insert node");
    let row = db
        .query_one(Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            format!("SELECT id FROM nodes WHERE name = '{name}'"),
        ))
        .await
        .expect("select node")
        .expect("node row");
    row.try_get("", "id").expect("id col")
}

fn draft(fqdn: &str, ip: &str, owner_id: i64) -> EndpointDraft {
    EndpointDraft {
        fqdn: fqdn.into(),
        record_type: RecordType::A,
        target_ip: Some(ip.into()),
        target_port: Some(5432),
        ttl: 30,
        owner_kind: OwnerKind::ServiceMember,
        owner_id,
        node_id: None,
    }
}

#[tokio::test]
async fn replace_endpoints_for_owner_handles_ip_churn() {
    let Some(db) = boot_db().await else { return };
    let registry = DnsRegistry::new(db.clone());

    // First container start — IP A.
    let g1 = registry
        .replace_endpoints_for_owner(
            OwnerKind::ServiceMember,
            42,
            &[draft(
                "pg-orders-0.pg-orders.temps.local",
                "172.20.5.10",
                42,
            )],
        )
        .await
        .expect("first replace");
    assert!(g1 >= 1, "first generation must be ≥ 1, got {g1}");

    // Container restarts → fresh IP B.
    let g2 = registry
        .replace_endpoints_for_owner(
            OwnerKind::ServiceMember,
            42,
            &[draft(
                "pg-orders-0.pg-orders.temps.local",
                "172.20.5.99",
                42,
            )],
        )
        .await
        .expect("second replace");
    assert!(
        g2 > g1,
        "generation must monotonically increase: {g1} → {g2}"
    );

    // Zone snapshot has exactly the new record — old IP is gone.
    let snap = registry.get_full_zone().await.expect("snapshot");
    let mine: Vec<_> = snap
        .records
        .iter()
        .filter(|r| r.owner_id == 42 && r.owner_kind == "service_member")
        .collect();
    assert_eq!(mine.len(), 1, "exactly one record for owner after replace");
    assert_eq!(mine[0].target_ip.as_deref(), Some("172.20.5.99"));
    assert_eq!(mine[0].generation, g2);
}

#[tokio::test]
async fn delete_by_owner_only_bumps_generation_when_something_changes() {
    let Some(db) = boot_db().await else { return };
    let registry = DnsRegistry::new(db.clone());

    // Seed.
    let g1 = registry
        .replace_endpoints_for_owner(
            OwnerKind::ServiceMember,
            7,
            &[draft("a.temps.local", "172.20.5.7", 7)],
        )
        .await
        .unwrap();

    // Real delete bumps generation.
    let removed = registry
        .delete_by_owner(OwnerKind::ServiceMember, 7)
        .await
        .unwrap();
    assert_eq!(removed, 1);
    let snap_after = registry.get_full_zone().await.unwrap();
    assert!(
        snap_after.generation > g1,
        "delete that removed rows must bump generation: {} → {}",
        g1,
        snap_after.generation
    );

    // No-op delete (owner gone) does NOT bump generation.
    let g_before = snap_after.generation;
    let removed2 = registry
        .delete_by_owner(OwnerKind::ServiceMember, 7)
        .await
        .unwrap();
    assert_eq!(removed2, 0);
    let snap_again = registry.get_full_zone().await.unwrap();
    assert_eq!(
        snap_again.generation, g_before,
        "no-op delete must not bump generation"
    );
}

#[tokio::test]
async fn get_changes_since_returns_diff_or_snapshot() {
    let Some(db) = boot_db().await else { return };
    let registry = DnsRegistry::new(db.clone());

    let _g1 = registry
        .replace_endpoints_for_owner(
            OwnerKind::ServiceMember,
            1,
            &[draft("x.temps.local", "1.1.1.1", 1)],
        )
        .await
        .unwrap();
    let g2 = registry
        .replace_endpoints_for_owner(
            OwnerKind::ServiceMember,
            2,
            &[draft("y.temps.local", "2.2.2.2", 2)],
        )
        .await
        .unwrap();

    // since=0 → snapshot.
    let snap = registry.get_changes_since(0).await.unwrap();
    assert!(snap.full_snapshot);
    assert_eq!(snap.generation, g2);
    assert!(snap.records.iter().any(|r| r.fqdn == "x.temps.local"));
    assert!(snap.records.iter().any(|r| r.fqdn == "y.temps.local"));

    // since=g2 → empty diff (caller is up to date).
    let nothing = registry.get_changes_since(g2).await.unwrap();
    assert!(!nothing.full_snapshot);
    assert!(nothing.records.is_empty());
    assert_eq!(nothing.generation, g2);

    // since=g2-1 → diff containing only the latest record.
    let diff = registry.get_changes_since(g2 - 1).await.unwrap();
    assert!(!diff.full_snapshot);
    assert!(
        diff.records.iter().all(|r| r.generation > g2 - 1),
        "diff must only contain rows with generation > since"
    );
}

#[tokio::test]
async fn ack_applied_persists_state_and_rejects_future_generations() {
    let Some(db) = boot_db().await else { return };
    let registry = DnsRegistry::new(db.clone());

    let node_id = insert_node(&db, "worker-ack", "tokenhash-ack").await;

    // Seed a record so generation > 0.
    let g = registry
        .replace_endpoints_for_owner(
            OwnerKind::ServiceMember,
            100,
            &[draft("z.temps.local", "3.3.3.3", 100)],
        )
        .await
        .unwrap();

    // Valid ACK is accepted and persisted.
    let state = registry.ack_applied(node_id, g).await.unwrap();
    assert_eq!(state.applied_generation, g);
    assert!(state.last_sync_at.is_some());
    assert_eq!(state.health, "healthy");

    // Idempotent: re-ACK with lower number doesn't roll back.
    let state2 = registry.ack_applied(node_id, g - 1).await.unwrap();
    assert_eq!(
        state2.applied_generation, g,
        "ACK lower than current must not regress applied_generation"
    );

    // ACK higher than server is rejected.
    let bogus = registry.ack_applied(node_id, g + 999).await;
    assert!(
        bogus.is_err(),
        "ACK above server generation must be rejected"
    );
}

#[tokio::test]
async fn unique_index_rejects_duplicate_fqdn_type_ip_across_owners() {
    // Two different owners trying to claim the same A record must collide
    // at the unique index — proves the schema invariant the service relies on.
    let Some(db) = boot_db().await else { return };
    let registry = DnsRegistry::new(db.clone());

    registry
        .replace_endpoints_for_owner(
            OwnerKind::ServiceMember,
            10,
            &[draft("dup.temps.local", "5.5.5.5", 10)],
        )
        .await
        .unwrap();

    let dup = registry
        .replace_endpoints_for_owner(
            OwnerKind::ServiceMember,
            11,
            &[draft("dup.temps.local", "5.5.5.5", 11)],
        )
        .await;
    assert!(
        dup.is_err(),
        "duplicate (fqdn, type, ip) for a different owner must be rejected"
    );
}

#[tokio::test]
async fn gc_orphan_records_removes_records_for_missing_members() {
    let Some(db) = boot_db().await else { return };
    let registry = DnsRegistry::new(db.clone());

    // Insert a parent external_services row first — service_members.service_id
    // has a FK to it.
    db.execute(sea_orm::Statement::from_string(
        sea_orm::DatabaseBackend::Postgres,
        "INSERT INTO external_services \
         (name, service_type, status, topology, created_at, updated_at) \
         VALUES ('gc-test-svc', 'postgres', 'running', 'cluster', now(), now())"
            .to_string(),
    ))
    .await
    .expect("insert external_service");
    let svc_id: i32 = db
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT id FROM external_services WHERE name = 'gc-test-svc'".to_string(),
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get("", "id")
        .unwrap();

    db.execute(sea_orm::Statement::from_string(
        sea_orm::DatabaseBackend::Postgres,
        format!(
            "INSERT INTO service_members \
             (service_id, role, container_name, status, ordinal, created_at, updated_at) \
             VALUES ({svc_id}, 'primary', 'gc-test-member', 'running', 0, now(), now())"
        ),
    ))
    .await
    .expect("insert member");
    let member_id_row = db
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT id FROM service_members WHERE container_name = 'gc-test-member'".to_string(),
        ))
        .await
        .unwrap()
        .unwrap();
    let member_id: i32 = member_id_row.try_get("", "id").unwrap();

    registry
        .replace_endpoints_for_owner(
            OwnerKind::ServiceMember,
            member_id as i64,
            &[draft(
                "gc-member.temps.local",
                "172.20.5.99",
                member_id as i64,
            )],
        )
        .await
        .unwrap();

    // First GC pass: nothing orphaned, should be a no-op.
    let removed_first = registry.gc_orphan_records().await.unwrap();
    assert_eq!(removed_first, 0, "no orphans before member is deleted");

    // Now delete the member out from under the record.
    db.execute(sea_orm::Statement::from_string(
        sea_orm::DatabaseBackend::Postgres,
        format!("DELETE FROM service_members WHERE id = {member_id}"),
    ))
    .await
    .unwrap();

    // Second GC pass: should reap the orphan.
    let removed_second = registry.gc_orphan_records().await.unwrap();
    assert_eq!(removed_second, 1, "exactly one orphan record reaped");

    // The record is gone.
    let snap = registry.get_full_zone().await.unwrap();
    assert!(
        !snap
            .records
            .iter()
            .any(|r| r.fqdn == "gc-member.temps.local"),
        "orphan record must be deleted"
    );
}

#[tokio::test]
async fn list_stale_resolvers_returns_lagging_nodes() {
    let Some(db) = boot_db().await else { return };
    let registry = DnsRegistry::new(db.clone());

    // Two nodes: one fresh (ACK'd just now), one stale (last_sync_at long ago).
    let fresh_id = insert_node(&db, "fresh-resolver", "tok-fresh").await;
    let stale_id = insert_node(&db, "stale-resolver", "tok-stale").await;

    // Bump the generation so we have something for the resolvers to be
    // behind on. Use OwnerKind::Static for the anchor — there's no member
    // table to satisfy and we just need any record.
    let static_anchor = EndpointDraft {
        fqdn: "anchor.temps.local".into(),
        record_type: RecordType::A,
        target_ip: Some("10.0.0.99".into()),
        target_port: None,
        ttl: 30,
        owner_kind: OwnerKind::Static,
        owner_id: 999,
        node_id: None,
    };
    let _ = registry
        .replace_endpoints_for_owner(OwnerKind::Static, 999, &[static_anchor])
        .await
        .unwrap();

    // Fresh: ACK now.
    let g_now = registry.get_full_zone().await.unwrap().generation;
    registry.ack_applied(fresh_id, g_now).await.unwrap();

    // Stale: insert state row and backdate last_sync_at to 5 minutes ago.
    db.execute(sea_orm::Statement::from_string(
        sea_orm::DatabaseBackend::Postgres,
        format!(
            "INSERT INTO node_dns_state (node_id, applied_generation, last_sync_at, health) \
             VALUES ({stale_id}, 0, now() - interval '5 minutes', 'unknown')"
        ),
    ))
    .await
    .unwrap();

    // Threshold: 60s. Should pick up `stale_id` only.
    let stale = registry.list_stale_resolvers(60).await.unwrap();
    let stale_ids: Vec<i32> = stale.iter().map(|s| s.node_id).collect();
    assert!(
        stale_ids.contains(&stale_id),
        "stale node must appear in the report, got: {stale_ids:?}"
    );
    assert!(
        !stale_ids.contains(&fresh_id),
        "freshly-ACK'd node must NOT appear in the report, got: {stale_ids:?}"
    );

    // The reported lag matches what we expect.
    let stale_row = stale.iter().find(|s| s.node_id == stale_id).unwrap();
    assert!(stale_row.lag() > 0, "stale resolver must have positive lag");
    assert_eq!(stale_row.applied_generation, 0);

    // Validation: zero / negative thresholds rejected.
    assert!(registry.list_stale_resolvers(0).await.is_err());
}
