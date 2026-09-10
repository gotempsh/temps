// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Create severity-based notification routes.
//!
//! Routes deliberately sit between notifications and providers: providers
//! describe destinations, while routes decide which destinations receive an
//! event. Existing installations get one permissive catch-all route per
//! provider so the upgrade preserves their previous fan-out behavior while
//! keeping each destination independently configurable.

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
CREATE TABLE notification_routes (
    id SERIAL PRIMARY KEY,
    name VARCHAR NOT NULL,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    min_severity VARCHAR NOT NULL DEFAULT 'debug',
    max_severity VARCHAR NOT NULL DEFAULT 'emergency',
    -- Set only for the auto-generated catch-all route created alongside a
    -- provider (see the backfill below and NotificationRoutingService::
    -- create_catch_all_route_for_provider). NULL for routes an operator
    -- created explicitly. The FK cascade deletes the catch-all route when
    -- its provider is deleted, so no orphaned "All notifications - <name>
    -- (provider <id>)" route can be left behind; the service layer uses
    -- this column (not name parsing) to keep the route's display name in
    -- sync when the provider is renamed.
    catch_all_provider_id INTEGER REFERENCES notification_providers(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT notification_routes_name_unique UNIQUE (name),
    CONSTRAINT notification_routes_min_severity_valid
        CHECK (min_severity IN ('debug', 'info', 'warning', 'error', 'critical', 'emergency')),
    CONSTRAINT notification_routes_max_severity_valid
        CHECK (max_severity IN ('debug', 'info', 'warning', 'error', 'critical', 'emergency')),
    CONSTRAINT notification_routes_severity_range_valid
        CHECK (
            array_position(ARRAY['debug', 'info', 'warning', 'error', 'critical', 'emergency'], min_severity)
            <= array_position(ARRAY['debug', 'info', 'warning', 'error', 'critical', 'emergency'], max_severity)
        )
);

CREATE INDEX idx_notification_routes_enabled
    ON notification_routes (enabled);

-- At most one catch-all route per provider.
CREATE UNIQUE INDEX idx_notification_routes_catch_all_provider_id
    ON notification_routes (catch_all_provider_id)
    WHERE catch_all_provider_id IS NOT NULL;

CREATE TABLE notification_route_providers (
    route_id INTEGER NOT NULL REFERENCES notification_routes(id) ON DELETE CASCADE,
    provider_id INTEGER NOT NULL REFERENCES notification_providers(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (route_id, provider_id)
);

CREATE INDEX idx_notification_route_providers_provider_id
    ON notification_route_providers (provider_id);

DO $$
DECLARE
    existing_provider RECORD;
    catch_all_route_id INTEGER;
BEGIN
    FOR existing_provider IN
        SELECT id, name
        FROM notification_providers
        ORDER BY id
    LOOP
        INSERT INTO notification_routes (name, enabled, min_severity, max_severity, catch_all_provider_id)
        VALUES (
            'All notifications - ' || existing_provider.name || ' (provider ' || existing_provider.id || ')',
            TRUE,
            'debug',
            'emergency',
            existing_provider.id
        )
        RETURNING id INTO catch_all_route_id;

        INSERT INTO notification_route_providers (route_id, provider_id)
        VALUES (catch_all_route_id, existing_provider.id);
    END LOOP;
END $$;
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
DROP TABLE IF EXISTS notification_route_providers;
DROP TABLE IF EXISTS notification_routes;
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
    async fn migration_backfills_one_catch_all_route_per_existing_provider() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .into_connection();

        Migration
            .up(&SchemaManager::new(&db))
            .await
            .expect("notification routing migration should succeed");

        let log = format!("{:?}", db.into_transaction_log());
        assert!(
            log.contains("FOR existing_provider IN"),
            "migration must iterate over every existing provider"
        );
        assert!(
            log.contains("VALUES (catch_all_route_id, existing_provider.id)"),
            "each generated route must be assigned only to its provider"
        );
        assert!(
            log.contains("'debug'") && log.contains("'emergency'"),
            "backfilled routes must cover the complete severity range"
        );
        assert!(
            log.contains("catch_all_provider_id") && log.contains("existing_provider.id"),
            "each generated route must record which provider it is a catch-all for"
        );
    }
}
