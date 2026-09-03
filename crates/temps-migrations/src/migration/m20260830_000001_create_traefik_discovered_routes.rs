// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Create the table backing live Traefik-label route discovery.
//!
//! Rows here describe containers Temps did **not** deploy (existing
//! docker-compose stacks, Coolify/Dokploy leftovers) that carry Traefik
//! labels. The discovery reconciler owns every write; `load_routes()` reads
//! them alongside `custom_routes` and friends.
//!
//! The table reuses the existing `notify_route_table_change()` trigger
//! function, so a discovery write propagates to the in-memory route table of
//! *every* control plane node over the existing `route_table_changes`
//! LISTEN/NOTIFY channel — no bespoke fan-out. Unlike the older routing tables
//! the triggers here are **row-level**, so a statement that changes nothing
//! notifies nobody; see the comments on each `CREATE TRIGGER` below.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
CREATE TABLE traefik_discovered_routes (
    id SERIAL PRIMARY KEY,
    -- Normalized (lowercased) hostname from the container's Host() rule.
    -- UNIQUE: one hostname resolves to exactly one discovered backend, and the
    -- constraint is what makes the reconciler's upsert idempotent.
    host VARCHAR NOT NULL,
    -- Traefik router name the host came from, for operator diagnostics.
    router_name VARCHAR NOT NULL,
    target_container_id VARCHAR NOT NULL,
    target_container_name VARCHAR NOT NULL,
    target_port INTEGER NOT NULL,
    -- Host-published port, when the container publishes one. Needed on
    -- baremetal installs where Temps runs outside Docker and cannot resolve
    -- container names over the Docker network's internal DNS.
    target_host_port INTEGER,
    network VARCHAR NOT NULL,
    tls BOOLEAN NOT NULL DEFAULT FALSE,
    -- Operator kill-switch for one discovered route without touching the
    -- container. Disabled rows stay visible but are skipped by load_routes().
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    last_seen_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT traefik_discovered_routes_host_unique UNIQUE (host),
    CONSTRAINT traefik_discovered_routes_target_port_valid
        CHECK (target_port > 0 AND target_port <= 65535),
    CONSTRAINT traefik_discovered_routes_target_host_port_valid
        CHECK (target_host_port IS NULL OR (target_host_port > 0 AND target_host_port <= 65535))
);

-- load_routes() reads only enabled rows.
CREATE INDEX idx_traefik_discovered_routes_enabled
    ON traefik_discovered_routes (enabled);

-- Incremental container events (die/stop/destroy) delete by container ID.
CREATE INDEX idx_traefik_discovered_routes_container_id
    ON traefik_discovered_routes (target_container_id);

-- The periodic reconciliation pass diffs one network at a time.
CREATE INDEX idx_traefik_discovered_routes_network
    ON traefik_discovered_routes (network);

-- Propagates changes to all control planes over the existing
-- route_table_changes channel.
--
-- Row-level, NOT statement-level, unlike the older routing tables. A
-- statement-level trigger in Postgres fires even when the statement matched
-- ZERO rows, and this table is written by a reconciler that reacts to *every*
-- container event on the host: an unrelated container starting or dying issues
-- a `DELETE ... WHERE target_container_id = ...` that matches nothing, and a
-- statement trigger would still NOTIFY, forcing a full `load_routes()` on every
-- control plane node. Row-level firing makes a no-op statement a no-op
-- notification, which is the only correct behaviour for a table driven by
-- external events.
--
-- The tradeoff is that a multi-row statement now notifies once per row instead
-- of once per statement. That is bounded by how many containers appear or
-- disappear inside one 30s reconciliation window (normally one or two), and it
-- is the same tradeoff the UPDATE trigger below already makes; a listener that
-- reloads N times in a row is wasteful but correct, whereas the previous
-- behaviour reloaded the entire cluster's route tables on container churn that
-- has nothing to do with Temps at all.
CREATE TRIGGER traefik_discovered_routes_changes_trigger
AFTER INSERT OR DELETE ON traefik_discovered_routes
FOR EACH ROW
EXECUTE FUNCTION notify_route_table_change();

-- UPDATE is handled row-wise with a WHEN filter rather than by the statement
-- trigger above. The reconciler refreshes `last_seen_at` on every 30s pass for
-- every still-present container; routing NOTHING about the route changes in
-- that case, and a statement-level UPDATE trigger would force a full
-- `load_routes()` on every control plane node twice a minute forever. Only a
-- change to a field the route table actually reads fires a reload.
CREATE TRIGGER traefik_discovered_routes_route_changes_trigger
AFTER UPDATE ON traefik_discovered_routes
FOR EACH ROW
WHEN (
    OLD.host IS DISTINCT FROM NEW.host
    OR OLD.target_container_id IS DISTINCT FROM NEW.target_container_id
    OR OLD.target_container_name IS DISTINCT FROM NEW.target_container_name
    OR OLD.target_port IS DISTINCT FROM NEW.target_port
    OR OLD.target_host_port IS DISTINCT FROM NEW.target_host_port
    OR OLD.tls IS DISTINCT FROM NEW.tls
    OR OLD.enabled IS DISTINCT FROM NEW.enabled
)
EXECUTE FUNCTION notify_route_table_change();
"#,
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
DROP TRIGGER IF EXISTS traefik_discovered_routes_route_changes_trigger ON traefik_discovered_routes;
DROP TRIGGER IF EXISTS traefik_discovered_routes_changes_trigger ON traefik_discovered_routes;
DROP TABLE IF EXISTS traefik_discovered_routes;
"#,
            )
            .await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};

    #[tokio::test]
    async fn migration_creates_unique_host_and_notify_trigger() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .into_connection();

        Migration
            .up(&SchemaManager::new(&db))
            .await
            .expect("traefik discovery migration should succeed");

        let log = format!("{:?}", db.into_transaction_log());
        assert!(
            log.contains("traefik_discovered_routes_host_unique UNIQUE (host)"),
            "host must be unique so one hostname has exactly one discovered backend"
        );
        assert!(
            log.contains("EXECUTE FUNCTION notify_route_table_change()"),
            "discovery writes must reuse the existing route_table_changes NOTIFY path"
        );
        assert!(
            log.contains("enabled BOOLEAN NOT NULL DEFAULT TRUE"),
            "discovered routes must be enabled by default with an operator kill-switch"
        );
        assert!(
            log.contains("traefik_discovered_routes_target_port_valid"),
            "target_port must be constrained to a valid TCP port"
        );
        assert!(
            log.contains("AFTER INSERT OR DELETE ON traefik_discovered_routes"),
            "insert/delete must notify through the shared route_table_changes path"
        );
        // A statement-level trigger fires even when the statement matched no
        // rows. The reconciler issues a delete-by-container for container
        // events it turns out not to care about, so statement-level firing
        // reloads every control plane node's route table on unrelated container
        // churn. Row-level firing is what makes a zero-row statement silent.
        let insert_delete = log
            .find("AFTER INSERT OR DELETE ON traefik_discovered_routes")
            .expect("the insert/delete trigger must exist");
        let after_insert_delete = &log[insert_delete..];
        assert!(
            after_insert_delete
                .starts_with("AFTER INSERT OR DELETE ON traefik_discovered_routes\\nFOR EACH ROW")
                || after_insert_delete.starts_with(
                    "AFTER INSERT OR DELETE ON traefik_discovered_routes\nFOR EACH ROW"
                ),
            "the insert/delete trigger must be FOR EACH ROW so a zero-row statement does not \
             NOTIFY (and thus does not reload every node's route table); got: {}",
            &after_insert_delete[..after_insert_delete.len().min(120)]
        );
        assert!(
            !log.contains("FOR EACH STATEMENT"),
            "no trigger on this table may be statement-level: every writer is an event-driven \
             reconciler whose statements routinely match zero rows"
        );
        assert!(
            log.contains("OLD.target_container_name IS DISTINCT FROM NEW.target_container_name"),
            "updates must only notify when a routing-relevant field changed"
        );
        assert!(
            !log.contains("OLD.last_seen_at IS DISTINCT FROM NEW.last_seen_at"),
            "a last_seen_at heartbeat must never force a route table reload"
        );
        assert!(
            log.contains("OLD.enabled IS DISTINCT FROM NEW.enabled"),
            "the operator kill-switch must fire the reload trigger: PATCH \
             /traefik-discovery/routes/{{host}}/enabled relies on this row-level \
             trigger (not a manual reload call) to reach the split-mode proxy \
             process and every other control plane node"
        );
    }

    #[tokio::test]
    async fn migration_down_drops_trigger_before_table() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .into_connection();

        Migration
            .down(&SchemaManager::new(&db))
            .await
            .expect("traefik discovery migration rollback should succeed");

        let log = format!("{:?}", db.into_transaction_log());
        let trigger = log
            .find("DROP TRIGGER IF EXISTS traefik_discovered_routes_changes_trigger")
            .expect("rollback must drop the trigger");
        let table = log
            .find("DROP TABLE IF EXISTS traefik_discovered_routes")
            .expect("rollback must drop the table");
        assert!(trigger < table, "trigger must be dropped before its table");
    }
}
