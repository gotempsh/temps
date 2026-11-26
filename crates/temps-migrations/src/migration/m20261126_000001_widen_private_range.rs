// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `private_range` on `project_custom_domains` and `environment_domains` holds
//! every range a private address admits, comma-separated in canonical form
//! (`100.64.0.0/10,fd7a:115c:a1e0::/48`). `VARCHAR(64)` fits one or two; an
//! operator who admits six `/24` subnets has a valid list of 89 characters,
//! and applying a plan for it failed at the write. `TEXT` holds any list the
//! consent accepts.
//!
//! Widening `varchar` to `text` is binary-compatible in PostgreSQL: no table
//! rewrite, no backfill, and the previous binary reads the column as before.
//! `down` narrows it back, and refuses while a stored list is longer than 64
//! characters instead of truncating a range a visitor is checked against.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const TABLES: [&str; 2] = ["project_custom_domains", "environment_domains"];

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        for table in TABLES {
            db.execute_unprepared(&format!(
                "ALTER TABLE {table} ALTER COLUMN private_range TYPE TEXT"
            ))
            .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        for table in TABLES {
            let longer = db
                .query_one(sea_orm::Statement::from_string(
                    manager.get_database_backend(),
                    format!(
                        "SELECT count(*)::bigint AS n FROM {table} WHERE length(private_range) > 64"
                    ),
                ))
                .await?
                .map(|row| row.try_get::<i64>("", "n"))
                .transpose()?
                .unwrap_or(0);
            if longer > 0 {
                return Err(DbErr::Migration(format!(
                    "{longer} row(s) of {table} admit a private_range longer than 64 characters; \
                     narrowing the column would cut a range visitors are checked against. Make \
                     those addresses admit fewer ranges first"
                )));
            }
        }
        for table in TABLES {
            db.execute_unprepared(&format!(
                "ALTER TABLE {table} ALTER COLUMN private_range TYPE VARCHAR(64)"
            ))
            .await?;
        }
        Ok(())
    }
}
