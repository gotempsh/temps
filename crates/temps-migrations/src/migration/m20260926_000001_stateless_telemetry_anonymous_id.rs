// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Random anonymous telemetry identity for stateless control planes.
//!
//! Stateless replicas have no durable data directory, so they cannot keep the
//! `anonymous_id` file local installs use. Their telemetry ID used to be an
//! unsalted SHA-256 of the operator-chosen `TEMPS_INSTANCE_ID`, which anyone
//! could precompute from common names (`production`, a tenant slug, ...).
//!
//! The ID now lives in the shared `stateless_control_plane` row as a random
//! value in the same `inst_<32 hex>` format as local installs. The column
//! default is volatile, so PostgreSQL computes it per row: the existing binding
//! row (if any) gets a fresh random ID when this migration runs, and every
//! future binding gets one on insert. All replicas read the same stored value,
//! so there is no generate-on-startup race between them.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE stateless_control_plane \
                 ADD COLUMN IF NOT EXISTS telemetry_anonymous_id TEXT NOT NULL \
                 DEFAULT ('inst_' || replace(gen_random_uuid()::text, '-', ''));",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE stateless_control_plane DROP COLUMN IF EXISTS telemetry_anonymous_id;",
            )
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migration::m20260922_000001_stateless_control_plane_jobs::Migration as StatelessSchema;
    use sea_orm::{ConnectionTrait, Database, DatabaseBackend, Statement};
    use testcontainers::{core::WaitFor, runners::AsyncRunner, GenericImage, ImageExt};

    const BIND: &str = "INSERT INTO stateless_control_plane \
        (id, instance_id, management_url, storage_identity, secret_verifier) \
        VALUES (1, 'production', 'https://example.test', 'bucket/prefix', 'encrypted')";

    async fn telemetry_id(db: &sea_orm::DatabaseConnection) -> String {
        db.query_one(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT telemetry_anonymous_id FROM stateless_control_plane WHERE id = 1",
        ))
        .await
        .expect("read telemetry id")
        .expect("binding row present")
        .try_get::<String>("", "telemetry_anonymous_id")
        .expect("decode telemetry id")
    }

    fn assert_random_id_shape(id: &str) {
        let hex = id
            .strip_prefix("inst_")
            .unwrap_or_else(|| panic!("telemetry id {id:?} must start with inst_"));
        assert_eq!(hex.len(), 32, "telemetry id {id:?} must carry 32 hex chars");
        assert!(
            hex.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "telemetry id {id:?} must be lowercase hex"
        );
    }

    #[tokio::test]
    async fn existing_and_new_bindings_get_distinct_random_ids() {
        let container = match GenericImage::new("postgres", "17-alpine")
            .with_wait_for(WaitFor::message_on_stderr(
                "database system is ready to accept connections",
            ))
            .with_env_var("POSTGRES_PASSWORD", "migration-test")
            .start()
            .await
        {
            Ok(container) => container,
            Err(error) => {
                let message = error.to_string().to_lowercase();
                if message.contains("socket")
                    || message.contains("connection refused")
                    || message.contains("permission denied")
                {
                    eprintln!("Skipping stateless telemetry id migration test: Docker unavailable: {error}");
                    return;
                }
                panic!("stateless telemetry id migration database startup failed: {error}");
            }
        };
        let port = container
            .get_host_port_ipv4(5432)
            .await
            .expect("Postgres port");
        let db = Database::connect(format!(
            "postgres://postgres:migration-test@127.0.0.1:{port}/postgres"
        ))
        .await
        .expect("connect");
        let manager = SchemaManager::new(&db);
        StatelessSchema
            .up(&manager)
            .await
            .expect("apply stateless schema");

        // A binding that predates this migration gets a random id backfilled.
        db.execute_unprepared(BIND)
            .await
            .expect("bind installation");
        Migration
            .up(&manager)
            .await
            .expect("apply telemetry id migration");
        let backfilled = telemetry_id(&db).await;
        assert_random_id_shape(&backfilled);
        // Not derived from the operator-chosen instance name.
        assert!(!backfilled.contains("production"));

        // A fresh binding with the same instance name gets a different id, so
        // the id cannot be predicted from TEMPS_INSTANCE_ID.
        db.execute_unprepared("DELETE FROM stateless_control_plane")
            .await
            .expect("clear binding");
        db.execute_unprepared(BIND)
            .await
            .expect("rebind installation");
        let rebound = telemetry_id(&db).await;
        assert_random_id_shape(&rebound);
        assert_ne!(backfilled, rebound);

        // Reads are stable: every replica sees the same stored value.
        assert_eq!(rebound, telemetry_id(&db).await);

        Migration
            .down(&manager)
            .await
            .expect("reverse telemetry id migration");
        assert!(!manager
            .has_column("stateless_control_plane", "telemetry_anonymous_id")
            .await
            .expect("column absence"));
        Migration
            .up(&manager)
            .await
            .expect("reapply telemetry id migration");
        assert_random_id_shape(&telemetry_id(&db).await);
    }
}
