// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Add per-node DNS resolver health columns to the `nodes` table (ADR-024).
//!
//! Mirrors the health-tracking pattern already used on `external_services`
//! (see `m20260422_000001_external_service_health`): a nullable "current
//! state" snapshot updated on every heartbeat, rather than a full history
//! table — the resolver's own `SyncStatus` already keeps just enough
//! history (last error, consecutive failures) for an operator to act on,
//! and a full audit trail belongs in the existing `audit_logs`/logging
//! paths, not a new hypertable.
//!
//! `dns_resolver_running`, `dns_resolver_tasks_alive`, `dns_resolver_last_sync_at`,
//! and `dns_resolver_record_count` are nullable and default to `NULL`
//! ("never reported" — an agent binary older than this feature, or a
//! single-host node that never allocates a `compute_cidr` and so never
//! touches cluster DNS). That is intentionally distinct from `false`/`0`,
//! which mean a heartbeat arrived and reported those values. See the
//! `nodes::Model` field docs for the full explanation.
//!
//! `dns_resolver_consecutive_failures` is NOT NULL DEFAULT 0 — unlike the
//! other columns, "zero failures" and "never reported" are not worth
//! distinguishing for a counter; both render identically to an operator.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();

        conn.execute_unprepared(
            r#"
ALTER TABLE nodes
    ADD COLUMN dns_resolver_running BOOLEAN,
    ADD COLUMN dns_resolver_tasks_alive BOOLEAN,
    ADD COLUMN dns_resolver_last_sync_at TIMESTAMPTZ,
    ADD COLUMN dns_resolver_consecutive_failures INTEGER NOT NULL DEFAULT 0,
    ADD COLUMN dns_resolver_last_error TEXT,
    ADD COLUMN dns_resolver_record_count INTEGER;
"#,
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();

        conn.execute_unprepared(
            r#"
ALTER TABLE nodes
    DROP COLUMN IF EXISTS dns_resolver_running,
    DROP COLUMN IF EXISTS dns_resolver_tasks_alive,
    DROP COLUMN IF EXISTS dns_resolver_last_sync_at,
    DROP COLUMN IF EXISTS dns_resolver_consecutive_failures,
    DROP COLUMN IF EXISTS dns_resolver_last_error,
    DROP COLUMN IF EXISTS dns_resolver_record_count;
"#,
        )
        .await?;

        Ok(())
    }
}
