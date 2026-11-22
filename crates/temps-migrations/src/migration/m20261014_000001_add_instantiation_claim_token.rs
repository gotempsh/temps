// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Give each claim of a template instantiation its own ownership token.
//!
//! A runner claims an instantiation by marking the row `running` with a
//! fresh heartbeat; once the heartbeat is older than the lease, another
//! process may claim it. Until now nothing told the two runners apart: a
//! server that lost its database connection for longer than the lease kept
//! executing its steps, and when it reconnected both runners wrote progress
//! and the final result over each other.
//!
//! `claim_token` is set to a new random value by every claim (including a
//! takeover) and every later write of the runner is conditional on it, so a
//! runner that lost the claim can no longer change the row and learns that it
//! lost it. The column is nullable: rows that predate it, and rows nobody
//! holds, carry no token, which no runner's token matches.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const ADD_CLAIM_TOKEN: &str =
    "ALTER TABLE template_instantiations ADD COLUMN IF NOT EXISTS claim_token TEXT";
const DROP_CLAIM_TOKEN: &str =
    "ALTER TABLE template_instantiations DROP COLUMN IF EXISTS claim_token";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(ADD_CLAIM_TOKEN)
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(DROP_CLAIM_TOKEN)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};

    fn mock_db(statement_count: usize) -> sea_orm::DatabaseConnection {
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results((0..statement_count).map(|_| MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }))
            .into_connection()
    }

    #[tokio::test]
    async fn up_adds_a_nullable_token_column_so_existing_rows_need_no_backfill() {
        let db = mock_db(1);
        Migration
            .up(&SchemaManager::new(&db))
            .await
            .expect("migration up should succeed");
        let log = format!("{:?}", db.into_transaction_log());
        assert!(
            log.contains("ADD COLUMN IF NOT EXISTS claim_token TEXT"),
            "got {log}"
        );
        assert!(!log.contains("NOT NULL"), "the token must be nullable");
    }

    #[tokio::test]
    async fn down_drops_the_token_column() {
        let db = mock_db(1);
        Migration
            .down(&SchemaManager::new(&db))
            .await
            .expect("migration down should succeed");
        let log = format!("{:?}", db.into_transaction_log());
        assert!(
            log.contains("DROP COLUMN IF EXISTS claim_token"),
            "got {log}"
        );
    }
}
