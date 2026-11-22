// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Give AI threads a kind: an AI harness conversation (`ai`) or an interactive
//! terminal in the workspace sandbox (`terminal`).
//!
//! Every existing row is an AI conversation, so the column defaults to `ai`
//! and no backfill is needed. A binary that predates the column keeps working
//! against the migrated table: it never writes `kind`, so the default applies.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const ADD_KIND_COLUMN: &str = "ALTER TABLE ai_conversations \
     ADD COLUMN IF NOT EXISTS kind VARCHAR(16) NOT NULL DEFAULT 'ai'";
const ADD_KIND_CHECK: &str = "ALTER TABLE ai_conversations \
     ADD CONSTRAINT ai_conversations_kind_check CHECK (kind IN ('ai', 'terminal'))";
const DROP_KIND_CHECK: &str =
    "ALTER TABLE ai_conversations DROP CONSTRAINT IF EXISTS ai_conversations_kind_check";
const DROP_KIND_COLUMN: &str = "ALTER TABLE ai_conversations DROP COLUMN IF EXISTS kind";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let connection = manager.get_connection();
        connection.execute_unprepared(ADD_KIND_COLUMN).await?;
        // Guarded so a re-run after a partial failure does not trip over the
        // constraint that the first run already created.
        connection.execute_unprepared(DROP_KIND_CHECK).await?;
        connection.execute_unprepared(ADD_KIND_CHECK).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let connection = manager.get_connection();
        connection.execute_unprepared(DROP_KIND_CHECK).await?;
        connection.execute_unprepared(DROP_KIND_COLUMN).await?;
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
    async fn up_defaults_existing_threads_to_ai_before_constraining_the_kind() {
        let db = mock_db(3);
        Migration
            .up(&SchemaManager::new(&db))
            .await
            .expect("migration up should succeed");
        let log = format!("{:?}", db.into_transaction_log());
        let add = log
            .find("ADD COLUMN IF NOT EXISTS kind VARCHAR(16) NOT NULL DEFAULT 'ai'")
            .expect("kind column must default existing rows to ai");
        let check = log
            .find("CHECK (kind IN ('ai', 'terminal'))")
            .expect("kind must be constrained to the known kinds");
        assert!(
            add < check,
            "the column must exist before it is constrained"
        );
    }

    #[tokio::test]
    async fn down_drops_the_constraint_before_the_column() {
        let db = mock_db(2);
        Migration
            .down(&SchemaManager::new(&db))
            .await
            .expect("migration down should succeed");
        let log = format!("{:?}", db.into_transaction_log());
        let check = log
            .find("DROP CONSTRAINT IF EXISTS ai_conversations_kind_check")
            .expect("constraint must be dropped");
        let column = log
            .find("DROP COLUMN IF EXISTS kind")
            .expect("column must be dropped");
        assert!(check < column);
    }
}
