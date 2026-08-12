//! Tie resumable AI CLI sessions to the effective provider/tool contract.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const ADD_FINGERPRINT_COLUMN: &str =
    "ALTER TABLE ai_conversations ADD COLUMN IF NOT EXISTS cli_session_fingerprint VARCHAR(80)";
const CLEAR_RESUMABLE_SESSIONS: &str = "UPDATE ai_conversations \
     SET cli_session_id = NULL, cli_session_fingerprint = NULL";
const DROP_FINGERPRINT_COLUMN: &str =
    "ALTER TABLE ai_conversations DROP COLUMN IF EXISTS cli_session_fingerprint";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(ADD_FINGERPRINT_COLUMN)
            .await?;
        // Existing session ids predate the contract fingerprint. Clearing both
        // fields at migration time makes legacy rows fail closed even if an old
        // binary is briefly restarted during a rolling upgrade.
        manager
            .get_connection()
            .execute_unprepared(CLEAR_RESUMABLE_SESSIONS)
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(CLEAR_RESUMABLE_SESSIONS)
            .await?;
        // Never leave a fingerprint-less session id behind for an older binary
        // to resume after rollback.
        manager
            .get_connection()
            .execute_unprepared(DROP_FINGERPRINT_COLUMN)
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
    async fn up_adds_fingerprint_before_invalidating_all_legacy_sessions() {
        let db = mock_db(2);
        Migration
            .up(&SchemaManager::new(&db))
            .await
            .expect("migration up should succeed");
        let log = format!("{:?}", db.into_transaction_log());
        let add = log
            .find("ADD COLUMN IF NOT EXISTS cli_session_fingerprint")
            .expect("fingerprint column must be added");
        let clear = log
            .find("SET cli_session_id = NULL, cli_session_fingerprint = NULL")
            .expect("legacy sessions must be invalidated");
        assert!(
            add < clear,
            "legacy rows can be cleared only after the column exists"
        );
    }

    #[tokio::test]
    async fn down_invalidates_sessions_before_dropping_their_fingerprint() {
        let db = mock_db(2);
        Migration
            .down(&SchemaManager::new(&db))
            .await
            .expect("migration down should succeed");
        let log = format!("{:?}", db.into_transaction_log());
        let clear = log
            .find("SET cli_session_id = NULL, cli_session_fingerprint = NULL")
            .expect("sessions must be invalidated before rollback");
        let drop_column = log
            .find("DROP COLUMN IF EXISTS cli_session_fingerprint")
            .expect("fingerprint column must be dropped");
        assert!(
            clear < drop_column,
            "rollback must not expose a fingerprint-less resumable session"
        );
    }
}
