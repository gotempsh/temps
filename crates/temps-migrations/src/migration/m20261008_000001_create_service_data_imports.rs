// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Create `service_data_imports`: one row per "copy an external database into
//! a database of a managed service" run.
//!
//! A run is independent from `restore_runs` because it has no backup as its
//! source and it targets a single logical database inside a service rather
//! than the whole service. The partial unique index is the authoritative lock
//! that keeps two imports from writing into the same database at once; the
//! service layer pre-checks it only to return a readable conflict.

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
CREATE TABLE IF NOT EXISTS service_data_imports (
    id SERIAL PRIMARY KEY,
    service_id INTEGER NOT NULL REFERENCES external_services(id) ON DELETE CASCADE,
    service_type VARCHAR(32) NOT NULL,
    target_database VARCHAR(128) NOT NULL,
    source_display TEXT NOT NULL,
    source_database VARCHAR(128) NOT NULL,
    replace_existing BOOLEAN NOT NULL DEFAULT FALSE,
    atomic_transfer BOOLEAN NOT NULL DEFAULT FALSE,
    status VARCHAR(16) NOT NULL,
    phase VARCHAR(32) NOT NULL,
    helper_container VARCHAR(128),
    error_message TEXT,
    helper_output TEXT,
    target_object_count BIGINT,
    target_size_bytes BIGINT,
    timeout_seconds INTEGER NOT NULL,
    created_by INTEGER REFERENCES users(id) ON DELETE SET NULL,
    cancel_requested_at TIMESTAMPTZ,
    started_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    finished_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT service_data_imports_status_check
        CHECK (status IN ('running', 'succeeded', 'failed', 'cancelled', 'interrupted')),
    CONSTRAINT service_data_imports_phase_check
        CHECK (phase IN ('preparing_target', 'transferring', 'verifying', 'finished')),
    CONSTRAINT service_data_imports_timeout_check
        CHECK (timeout_seconds > 0)
);

CREATE INDEX IF NOT EXISTS idx_service_data_imports_service_created
    ON service_data_imports (service_id, created_at DESC, id DESC);

CREATE UNIQUE INDEX IF NOT EXISTS uq_service_data_imports_one_running_per_database
    ON service_data_imports (service_id, target_database)
    WHERE status = 'running';
"#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS service_data_imports;")
            .await?;
        Ok(())
    }
}
