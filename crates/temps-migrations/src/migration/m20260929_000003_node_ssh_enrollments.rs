// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Adding a server over SSH (ADR 048 D2c).
//!
//! One row per attempt: which server, as whom, the host key the operator
//! confirmed, the step it is on and its log, for the Worker Nodes page. The
//! SSH credentials are never stored. The pairing (D2b) it runs on the server
//! is linked, so the node it registers can be found.

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
CREATE TABLE IF NOT EXISTS node_ssh_enrollments (
    id SERIAL PRIMARY KEY,
    name TEXT NOT NULL,
    host TEXT NOT NULL,
    ssh_address TEXT NOT NULL,
    ssh_user TEXT NOT NULL,
    auth_method TEXT NOT NULL,
    host_key_fingerprint TEXT NOT NULL,
    pairing_id INTEGER REFERENCES node_pairings (id) ON DELETE SET NULL,
    status TEXT NOT NULL DEFAULT 'running',
    step TEXT NOT NULL,
    log TEXT NOT NULL DEFAULT '',
    error TEXT,
    agent_mode TEXT,
    node_id INTEGER REFERENCES nodes (id) ON DELETE SET NULL,
    created_by_user_id INTEGER,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    -- Touched by the process running the enrollment while it runs: a
    -- running row whose heartbeat is old belongs to a process that is gone.
    heartbeat_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    finished_at TIMESTAMPTZ,
    CONSTRAINT node_ssh_enrollments_status_check
        CHECK (status IN ('running', 'succeeded', 'failed'))
);

CREATE INDEX IF NOT EXISTS idx_node_ssh_enrollments_created_at
    ON node_ssh_enrollments (created_at DESC);

CREATE INDEX IF NOT EXISTS idx_node_ssh_enrollments_running
    ON node_ssh_enrollments (heartbeat_at) WHERE status = 'running';
"#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS node_ssh_enrollments;")
            .await?;
        Ok(())
    }
}
