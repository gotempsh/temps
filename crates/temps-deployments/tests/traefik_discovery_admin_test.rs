// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Integration tests for the Traefik discovery admin API's service layer.
//!
//! These run against a **real** Postgres (schema-isolated, full migration set)
//! rather than a mock, because the two things most worth proving here are
//! properties of the database, not of the code path:
//!
//! * `PATCH .../enabled` really persists — a subsequent read sees the new
//!   value, and `updated_at` moves so the row-level trigger's `WHEN` filter
//!   sees a routing-relevant change;
//! * the `traefik_discovered_routes_route_changes_trigger` really covers a
//!   plain `enabled` UPDATE, which is what makes the existing
//!   `route_table_changes` NOTIFY path (rather than a bespoke reload call)
//!   the propagation mechanism for a suppressed route.
//!
//! Skips gracefully — project convention, never `#[ignore]` — when Postgres or
//! the container runtime is unavailable.

use std::sync::Arc;

use sea_orm::{ActiveValue::Set, ColumnTrait, EntityTrait, QueryFilter};
use temps_database::test_utils::TestDatabase;
use temps_deployer::traefik_discovery::{TraefikDiscoveryConfig, TraefikDiscoveryHandle};
use temps_deployments::services::traefik_discovery_service::{
    DiscoveredHostTlsProvisioner, TlsProvisionerError,
};
use temps_deployments::services::TraefikDiscoveryAdminService;
use temps_entities::traefik_discovered_routes as discovered;

/// A `DiscoveredHostTlsProvisioner` that always succeeds — integration tests
/// that exercise `set_route_enabled` and `list_routes` never touch TLS paths,
/// so this is the right stub for them.
struct NoopProvisioner;

#[async_trait::async_trait]
impl DiscoveredHostTlsProvisioner for NoopProvisioner {
    async fn request_acme_cert(
        &self,
        _host: &str,
        _challenge_type: &str,
    ) -> Result<(), TlsProvisionerError> {
        Ok(())
    }

    async fn save_imported_cert(
        &self,
        _host: &str,
        _certificate_pem: &str,
        _key_pem: &str,
        _renewal_method: &str,
        _not_after: chrono::DateTime<chrono::Utc>,
    ) -> Result<i32, TlsProvisionerError> {
        Ok(1)
    }

    async fn dns_zone_is_auto_managed(&self, _host: &str) -> Result<bool, TlsProvisionerError> {
        Ok(true)
    }
}

fn noop_provisioner() -> Arc<dyn DiscoveredHostTlsProvisioner> {
    Arc::new(NoopProvisioner)
}

/// Boot a real Postgres with the full Temps schema, or `None` so the caller
/// can skip.
async fn boot_database() -> Option<TestDatabase> {
    match TestDatabase::with_migrations().await {
        Ok(db) => Some(db),
        Err(e) => {
            eprintln!("⏭️  Postgres/testcontainers unavailable, skipping: {e}");
            None
        }
    }
}

fn disabled_handle() -> Arc<TraefikDiscoveryHandle> {
    Arc::new(TraefikDiscoveryHandle::not_running(
        TraefikDiscoveryConfig::resolve(None, None, "temps"),
        "TEMPS_TRAEFIK_DISCOVERY_ENABLED is not set to 'true'",
    ))
}

/// Insert a discovered route exactly as the reconciler would.
async fn seed_route(
    db: &sea_orm::DatabaseConnection,
    host: &str,
    container: &str,
    enabled: bool,
) -> discovered::Model {
    let now = chrono::Utc::now();
    discovered::Entity::insert(discovered::ActiveModel {
        host: Set(host.to_string()),
        router_name: Set("app".to_string()),
        target_container_id: Set(format!("{container}-id")),
        target_container_name: Set(container.to_string()),
        target_port: Set(80),
        target_host_port: Set(None),
        network: Set("temps".to_string()),
        tls: Set(false),
        enabled: Set(enabled),
        last_seen_at: Set(now),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    })
    .exec_with_returning(db)
    .await
    .expect("seeding a discovered route must succeed")
}

#[tokio::test]
async fn set_route_enabled_persists_and_is_visible_to_a_later_read() {
    let Some(test_db) = boot_database().await else {
        return;
    };
    let db = test_db.connection_arc();
    let seeded = seed_route(db.as_ref(), "app.example.com", "whoami", true).await;

    let service =
        TraefikDiscoveryAdminService::new(db.clone(), disabled_handle(), noop_provisioner());

    // Suppress it.
    let updated = service
        .set_route_enabled("APP.Example.com", false)
        .await
        .expect("suppressing a discovered route must succeed");
    assert!(!updated.enabled, "the response must reflect the new value");
    assert!(!updated.active);

    // ...and the change must be in the database, not just in the response.
    let reread = discovered::Entity::find()
        .filter(discovered::Column::Host.eq("app.example.com"))
        .one(db.as_ref())
        .await
        .expect("re-reading the row must succeed")
        .expect("the row must still exist — suppression never deletes it");
    assert!(
        !reread.enabled,
        "the enabled flag must be persisted, not only returned"
    );
    assert!(
        reread.updated_at >= seeded.updated_at,
        "updated_at must advance so the row-level trigger sees a real change"
    );

    // Restoring it must work the same way in reverse.
    let restored = service
        .set_route_enabled("app.example.com", true)
        .await
        .expect("restoring a discovered route must succeed");
    assert!(restored.enabled);
    assert!(restored.active);
    assert!(restored.inactive_reason.is_none());

    let reread = discovered::Entity::find()
        .filter(discovered::Column::Host.eq("app.example.com"))
        .one(db.as_ref())
        .await
        .expect("re-reading the row must succeed")
        .expect("the row must still exist");
    assert!(reread.enabled);
}

#[tokio::test]
async fn list_and_status_reflect_the_rows_in_the_database() {
    let Some(test_db) = boot_database().await else {
        return;
    };
    let db = test_db.connection_arc();
    seed_route(db.as_ref(), "one.example.com", "one", true).await;
    seed_route(db.as_ref(), "two.example.com", "two", false).await;

    let service =
        TraefikDiscoveryAdminService::new(db.clone(), disabled_handle(), noop_provisioner());

    let list = service
        .list_routes(None, None)
        .await
        .expect("listing discovered routes must succeed");
    assert_eq!(list.total, 2);
    assert_eq!(list.routes.len(), 2);
    assert!(
        !list.discovery_running,
        "the watcher is not running in this test process"
    );
    let suppressed = list
        .routes
        .iter()
        .find(|r| r.host == "two.example.com")
        .expect("the suppressed row must still be listed so the operator can see it");
    assert!(!suppressed.active);
    assert!(suppressed.inactive_reason.is_some());

    let status = service.status().await.expect("status must succeed");
    assert!(
        !status.configured,
        "discovery is off in this process, so it must report configured=false"
    );
    assert!(
        status.reason.is_some(),
        "an unconfigured feature must say why"
    );
    assert_eq!(status.discovered_route_count, 2);
    assert_eq!(
        status.enabled_route_count, 1,
        "a suppressed row counts as discovered but not as enabled"
    );
}

/// The whole propagation design rests on the migration's row-level UPDATE
/// trigger firing for an `enabled` change: that is what carries a suppressed
/// route to the split-mode `temps proxy` process and to every other control
/// plane node. Assert the deployed trigger really has that clause, rather than
/// trusting a manual reload call that the API deliberately does not make.
#[tokio::test]
async fn the_deployed_update_trigger_fires_on_an_enabled_change() {
    let Some(test_db) = boot_database().await else {
        return;
    };

    let rows = test_db
        .query_sql(
            "SELECT pg_get_triggerdef(t.oid) AS def \
             FROM pg_trigger t \
             JOIN pg_class c ON c.oid = t.tgrelid \
             WHERE c.relname = 'traefik_discovered_routes' AND NOT t.tgisinternal",
        )
        .await
        .expect("reading trigger definitions must succeed");

    let definitions: Vec<String> = rows
        .iter()
        .map(|row| {
            row.try_get::<String>("", "def")
                .expect("pg_get_triggerdef must return text")
        })
        .collect();

    assert!(
        definitions
            .iter()
            .any(|d| d.contains("enabled IS DISTINCT FROM")),
        "the UPDATE trigger must fire on an enabled change, otherwise a \
         suppressed route never reaches the other nodes' route tables; got: {definitions:?}"
    );
    assert!(
        definitions
            .iter()
            .all(|d| !d.contains("last_seen_at IS DISTINCT FROM")),
        "a last_seen_at heartbeat must never force a route table reload; got: {definitions:?}"
    );
}
