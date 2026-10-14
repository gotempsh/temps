// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Index the columns that point at `ip_geolocations`, and `visitor.last_seen`.
//!
//! Stored IP addresses are now deleted once nothing references them. Deleting
//! a row from `ip_geolocations` makes the database look for referencing rows
//! (the `ON DELETE SET NULL` foreign keys of `error_events` and `proxy_logs`
//! do it per deleted row), and the retention job itself asks "is anything
//! still pointing at this id?" for every candidate. Without an index on the
//! referencing column each of those checks is a scan of the whole table.
//!
//! The indexes are partial (`WHERE col IS NOT NULL`): most rows of the
//! high-volume tables carry no address, and a partial index costs nothing for
//! them. `events` already has `idx_events_geo`. A table or column that does
//! not exist in a given installation is skipped.
//!
//! `visitor (last_seen)` serves the visitor retention sweep, which deletes the
//! oldest rows first across every project; the existing
//! `(project_id, last_seen)` index cannot order rows globally.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// `(index, table, column)`. Every column but `last_seen` is a nullable
/// reference and gets a partial index.
const INDEXES: &[(&str, &str, &str)] = &[
    ("idx_visitor_last_seen", "visitor", "last_seen"),
    (
        "idx_error_events_ip_geolocation",
        "error_events",
        "ip_geolocation_id",
    ),
    (
        "idx_proxy_logs_ip_geolocation",
        "proxy_logs",
        "ip_geolocation_id",
    ),
    ("idx_visitor_ip_address", "visitor", "ip_address_id"),
    (
        "idx_performance_metrics_ip_address",
        "performance_metrics",
        "ip_address_id",
    ),
    ("idx_audit_logs_ip_address", "audit_logs", "ip_address_id"),
];

fn create_index_sql(index: &str, table: &str, column: &str) -> String {
    let predicate = if column == "last_seen" {
        String::new()
    } else {
        format!(" WHERE {column} IS NOT NULL")
    };
    format!(
        "DO $$ BEGIN \
           IF EXISTS (SELECT 1 FROM information_schema.columns \
                      WHERE table_schema = current_schema() \
                        AND table_name = '{table}' AND column_name = '{column}') THEN \
             CREATE INDEX IF NOT EXISTS {index} ON {table} ({column}){predicate}; \
           END IF; \
         END $$"
    )
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let connection = manager.get_connection();
        for (index, table, column) in INDEXES {
            connection
                .execute_unprepared(&create_index_sql(index, table, column))
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let connection = manager.get_connection();
        for (index, _, _) in INDEXES {
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
    fn reference_indexes_are_partial_and_every_index_is_guarded() {
        for (index, table, column) in INDEXES {
            let sql = create_index_sql(index, table, column);
            assert!(sql.contains("IF NOT EXISTS"), "{sql}");
            assert!(sql.contains("information_schema.columns"), "{sql}");
            let partial = sql.contains(&format!("WHERE {column} IS NOT NULL"));
            assert_eq!(partial, *column != "last_seen", "{sql}");
        }
    }

    #[test]
    fn index_names_are_unique() {
        let mut names: Vec<_> = INDEXES.iter().map(|(name, _, _)| *name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), INDEXES.len());
    }
}
