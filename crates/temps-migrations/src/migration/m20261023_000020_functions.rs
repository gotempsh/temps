// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! ADR-055 Phase 10: functions.
//!
//! A function is a product object: an artifact (a WASM module stored by
//! digest under the data directory, or an OCI image reference) that runs as
//! a `ScaleToZero` workload on the workload plane, reached on its own
//! hostname through the activator. Nothing existing changes: the table is
//! new, and the function's ingress endpoint is a row of the existing
//! `ingress_endpoints` table (`owner_kind = 'function'`, already allowed).
//!
//! `workload_id` is the plane's workload, once applied; `NULL` while the
//! plane is not running in this installation (the function is stored and
//! reported as waiting for it).

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
CREATE TABLE functions (
    id BIGSERIAL PRIMARY KEY,
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    artifact_kind TEXT NOT NULL,
    image_reference TEXT,
    wasm_digest TEXT,
    wasm_size_bytes BIGINT,
    port INTEGER NOT NULL DEFAULT 8080,
    idle_timeout_seconds INTEGER NOT NULL DEFAULT 300,
    wake_timeout_seconds INTEGER NOT NULL DEFAULT 30,
    memory_limit_mb INTEGER,
    hostname TEXT NOT NULL,
    workload_id BIGINT REFERENCES workloads(id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT functions_artifact_kind_valid
        CHECK (artifact_kind IN ('wasm', 'oci')),
    CONSTRAINT functions_artifact_matches_kind
        CHECK ((artifact_kind = 'wasm'
                AND wasm_digest IS NOT NULL AND image_reference IS NULL)
            OR (artifact_kind = 'oci'
                AND image_reference IS NOT NULL AND wasm_digest IS NULL)),
    CONSTRAINT functions_port_valid CHECK (port BETWEEN 1 AND 65535),
    CONSTRAINT functions_idle_timeout_valid
        CHECK (idle_timeout_seconds BETWEEN 10 AND 86400),
    CONSTRAINT functions_wake_timeout_valid
        CHECK (wake_timeout_seconds BETWEEN 1 AND 300),
    CONSTRAINT functions_memory_limit_valid
        CHECK (memory_limit_mb IS NULL OR memory_limit_mb BETWEEN 1 AND 4096)
);

CREATE UNIQUE INDEX functions_project_name_unique ON functions (project_id, name);
CREATE UNIQUE INDEX functions_hostname_unique ON functions (hostname);
CREATE INDEX functions_project_created_at ON functions (project_id, created_at DESC);
CREATE INDEX functions_wasm_digest ON functions (wasm_digest) WHERE wasm_digest IS NOT NULL;
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
DELETE FROM ingress_endpoints WHERE owner_kind = 'function';
DROP TABLE IF EXISTS functions;
"#,
            )
            .await?;
        Ok(())
    }
}
