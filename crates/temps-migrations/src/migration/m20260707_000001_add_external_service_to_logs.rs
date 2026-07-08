//! Add an `external_service_id` dimension to the log-aggregator tables so that
//! logs from **imported/managed external-service containers** (Postgres,
//! MariaDB, Redis, MongoDB, MinIO, …) can be persisted, retained, and searched
//! exactly like deployment/application logs.
//!
//! # Why a new dimension instead of reusing `project_id`
//!
//! The whole log pipeline is keyed on a mandatory `project_id: i32`
//! (`log_chunks.project_id NOT NULL`, storage keys, search filters, indexes).
//! An external service is **not** owned by a single project — it can be linked
//! to zero or many. So there is no honest `project_id` to file its logs under.
//!
//! This migration keeps `project_id` as-is (external-service chunks store the
//! sentinel `0`) and adds a nullable `external_service_id`:
//!   * deployment/application chunks: `external_service_id IS NULL`, keyed by
//!     `project_id` (unchanged behaviour).
//!   * external-service chunks: `external_service_id = <external_services.id>`,
//!     `project_id = 0`, keyed by service.
//!
//! `log_events` is an optional/legacy table (see
//! `m20260611_000001_change_log_deploy_id_to_integer`), so its ALTER is guarded
//! by `to_regclass`.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260707_000001_add_external_service_to_logs"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // log_chunks always exists.
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                ALTER TABLE log_chunks
                    ADD COLUMN IF NOT EXISTS external_service_id integer;

                -- Service-scoped lookup: mirrors the (project_id, service,
                -- started_at) primary index but leads with external_service_id
                -- so the external-service logs view is an index range scan.
                CREATE INDEX IF NOT EXISTS idx_log_chunks_extsvc_time
                    ON log_chunks (external_service_id, started_at)
                    WHERE external_service_id IS NOT NULL;
                "#,
            )
            .await?;

        // log_events is optional — only touch it if it exists.
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                DO $$
                BEGIN
                    IF to_regclass('public.log_events') IS NOT NULL THEN
                        ALTER TABLE log_events
                            ADD COLUMN IF NOT EXISTS external_service_id integer;
                        CREATE INDEX IF NOT EXISTS idx_log_events_extsvc_time
                            ON log_events (external_service_id, time)
                            WHERE external_service_id IS NOT NULL;
                    END IF;
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
                DROP INDEX IF EXISTS idx_log_chunks_extsvc_time;
                ALTER TABLE log_chunks DROP COLUMN IF EXISTS external_service_id;

                DO $$
                BEGIN
                    IF to_regclass('public.log_events') IS NOT NULL THEN
                        DROP INDEX IF EXISTS idx_log_events_extsvc_time;
                        ALTER TABLE log_events DROP COLUMN IF EXISTS external_service_id;
                    END IF;
                END $$;
                "#,
            )
            .await?;
        Ok(())
    }
}
