// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Index the columns that point at `ip_geolocations`, and `visitor.last_seen`.
//!
//! Stored IP addresses are now deleted once nothing references them, and each
//! check for "is anything still pointing at this id?" is a scan of the
//! referencing table without an index on the column.
//!
//! The indexes are **not built here**. SeaORM runs a migration inside a
//! transaction, where only a plain `CREATE INDEX` is legal, and that holds a
//! lock that stops writes (analytics ingestion, audit logs, proxy logs) for as
//! long as the build scans the table, which on a large history is minutes.
//! They are built after the server has bound by
//! `temps_database::run_post_migration_indexes`, with `CREATE INDEX
//! CONCURRENTLY` on plain tables and TimescaleDB's per-chunk build on the
//! `proxy_logs` hypertable (which does not support `CONCURRENTLY`). Both are
//! idempotent, and an index that already exists (a database that ran the
//! previous version of this migration) is left alone.
//!
//! This migration stays registered, as a no-op, so that databases which
//! already recorded it keep a consistent history, and so that `down` still
//! removes the indexes.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Names of the indexes the post-migration runner builds, so `down` can
/// remove them.
const INDEXES: &[&str] = &[
    "idx_visitor_last_seen",
    "idx_error_events_ip_geolocation",
    "idx_proxy_logs_ip_geolocation",
    "idx_visitor_ip_address",
    "idx_performance_metrics_ip_address",
    "idx_audit_logs_ip_address",
];

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let connection = manager.get_connection();
        for index in INDEXES {
            connection
                .execute_unprepared(&format!("DROP INDEX IF EXISTS {index}"))
                .await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_names_are_unique() {
        let mut names: Vec<_> = INDEXES.to_vec();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), INDEXES.len());
    }
}
