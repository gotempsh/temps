// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Indexes `deployments` by the columns the nightly image-retention scan
//! filters and sorts on (`recently_protected_image_names`'s window function
//! partitions by `(project_id, environment_id)` and orders by `created_at`).
//! Without this, that query's `ROW_NUMBER() OVER (...)` sorts the full table
//! every night regardless of how small `keep_recent` is.
//!
//! `deployments` grows without bound on long-lived installs — the same
//! premise this whole feature exists to address — so this follows the
//! lock_timeout/statement_timeout-guarded pattern used for
//! `idx_dns_managed_domains_normalized_domain` rather than assuming the
//! table is small. A non-concurrent `CREATE INDEX` still takes a `SHARE`
//! lock that blocks writes (new deployments) for the build's duration.
//! Installs with a very large deployment history should build this index
//! manually with `CREATE INDEX CONCURRENTLY` before upgrading — see
//! `idx_audit_logs_permission_denied_retention`'s migration for the same
//! escape hatch — rather than let this migration's bounded timeout make the
//! boot fail outright.

use sea_orm_migration::prelude::*;

const INDEX_NAME: &str = "idx_deployments_retention_scan";

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // SeaORM 1.1 wraps PostgreSQL migrations in a transaction, so
        // CREATE INDEX CONCURRENTLY is not legal here. lock_timeout bounds
        // how long we wait to acquire the lock; statement_timeout bounds the
        // whole build. Either firing fails this migration (and the boot)
        // loudly instead of hanging — an operator hitting the timeout should
        // build the index manually with CONCURRENTLY and re-run migrations.
        manager
            .get_connection()
            .execute_unprepared("SET LOCAL lock_timeout = '5s'")
            .await?;
        manager
            .get_connection()
            .execute_unprepared("SET LOCAL statement_timeout = '60s'")
            .await?;
        manager
            .get_connection()
            .execute_unprepared(&format!(
                "CREATE INDEX IF NOT EXISTS {INDEX_NAME} ON deployments (project_id, environment_id, created_at DESC)"
            ))
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("SET LOCAL lock_timeout = '5s'")
            .await?;
        manager
            .get_connection()
            .execute_unprepared("SET LOCAL statement_timeout = '30s'")
            .await?;
        manager
            .get_connection()
            .execute_unprepared(&format!("DROP INDEX IF EXISTS {INDEX_NAME}"))
            .await?;
        Ok(())
    }
}
