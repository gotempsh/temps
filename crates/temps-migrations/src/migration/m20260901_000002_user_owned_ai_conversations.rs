// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Makes AI conversations private, user-owned resources.
//!
//! A project is optional execution context, not the authorization boundary.
//! Legacy rows must already carry the user that created them. Projects are
//! team-scoped resources and do not have a single deterministic owner, so rows
//! without an owner abort the migration with an actionable error instead of
//! guessing an identity or silently deleting conversation history.
//! Pending actions follow the conversation owner so a global operation can be
//! proposed without inventing a project scope.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "UPDATE ai_pending_actions AS pending
                    SET created_by = conversation.created_by
                   FROM ai_conversations AS conversation
                  WHERE conversation.id = pending.conversation_id
                    AND conversation.created_by IS NOT NULL
                    AND pending.created_by IS DISTINCT FROM conversation.created_by;

                 DO $$
                 BEGIN
                    IF EXISTS (
                        SELECT 1 FROM ai_conversations AS conversation
                         WHERE conversation.created_by IS NULL
                            OR NOT EXISTS (
                                SELECT 1 FROM users
                                 WHERE users.id = conversation.created_by
                            )
                    ) THEN
                        RAISE EXCEPTION 'cannot make AI conversations user-owned: legacy rows have no valid owner; assign created_by to a valid user before retrying the migration';
                    END IF;
                    IF EXISTS (
                        SELECT 1 FROM ai_pending_actions AS pending
                         WHERE pending.created_by IS NULL
                            OR NOT EXISTS (
                                SELECT 1 FROM users
                                 WHERE users.id = pending.created_by
                            )
                            OR NOT EXISTS (
                                SELECT 1 FROM ai_conversations AS conversation
                                 WHERE conversation.id = pending.conversation_id
                                   AND conversation.created_by = pending.created_by
                            )
                    ) THEN
                        RAISE EXCEPTION 'cannot make AI pending actions user-owned: rows do not have a valid matching conversation owner; repair them before retrying the migration';
                    END IF;
                 END $$;

                 ALTER TABLE ai_conversations
                   ALTER COLUMN project_id DROP NOT NULL,
                   ALTER COLUMN created_by SET NOT NULL;
                 ALTER TABLE ai_pending_actions
                   ALTER COLUMN project_id DROP NOT NULL,
                   ALTER COLUMN created_by SET NOT NULL;

                 UPDATE ai_conversations
                    SET project_id = NULL
                  WHERE application_id IS NOT NULL;

                 ALTER TABLE ai_conversations
                   ADD CONSTRAINT fk_ai_conversations_owner
                   FOREIGN KEY (created_by) REFERENCES users (id) ON DELETE CASCADE;
                 ALTER TABLE ai_conversations
                   ADD CONSTRAINT chk_ai_conversations_single_context
                   CHECK (project_id IS NULL OR application_id IS NULL);
                 ALTER TABLE ai_pending_actions
                   ADD CONSTRAINT fk_ai_pending_actions_owner
                   FOREIGN KEY (created_by) REFERENCES users (id) ON DELETE CASCADE;

                 DROP INDEX IF EXISTS idx_ai_conversations_context;
                 CREATE INDEX idx_ai_conversations_owner_activity
                   ON ai_conversations (created_by, last_activity_at DESC);
                 CREATE INDEX idx_ai_conversations_owner_context
                   ON ai_conversations (created_by, context_type, context_id);
                 CREATE INDEX idx_ai_conversations_project_context
                   ON ai_conversations (project_id, context_type, context_id)
                   WHERE project_id IS NOT NULL;
                 CREATE INDEX idx_ai_pending_actions_owner_conversation
                   ON ai_pending_actions (created_by, conversation_id, created_at DESC);",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Global and application conversations cannot satisfy the historical
        // NOT NULL project_id constraint. Refuse the rollback while such rows
        // exist rather than silently destroying conversation history.
        manager
            .get_connection()
            .execute_unprepared(
                r#"DO $$
                BEGIN
                    IF EXISTS (
                        SELECT 1 FROM ai_conversations WHERE project_id IS NULL
                    ) OR EXISTS (
                        SELECT 1 FROM ai_pending_actions WHERE project_id IS NULL
                    ) THEN
                        RAISE EXCEPTION 'cannot roll back user-owned AI conversations while global or application history exists; archive/export or reassign those rows first';
                    END IF;
                END $$;"#,
            )
            .await?;
        manager
            .get_connection()
            .execute_unprepared(
                "DROP INDEX IF EXISTS idx_ai_pending_actions_owner_conversation;
                 DROP INDEX IF EXISTS idx_ai_conversations_project_context;
                 DROP INDEX IF EXISTS idx_ai_conversations_owner_context;
                 DROP INDEX IF EXISTS idx_ai_conversations_owner_activity;
                 ALTER TABLE ai_pending_actions
                   DROP CONSTRAINT IF EXISTS fk_ai_pending_actions_owner;
                 ALTER TABLE ai_conversations
                   DROP CONSTRAINT IF EXISTS chk_ai_conversations_single_context;
                 ALTER TABLE ai_conversations
                   DROP CONSTRAINT IF EXISTS fk_ai_conversations_owner;
                 ALTER TABLE ai_pending_actions
                   ALTER COLUMN project_id SET NOT NULL,
                   ALTER COLUMN created_by DROP NOT NULL;
                 ALTER TABLE ai_conversations
                   ALTER COLUMN project_id SET NOT NULL,
                   ALTER COLUMN created_by DROP NOT NULL;
                 CREATE INDEX idx_ai_conversations_context
                   ON ai_conversations (project_id, context_type, context_id);",
            )
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};

    #[tokio::test]
    async fn migration_fails_closed_before_making_owners_required() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([Default::default()])
            .into_connection();
        let manager = SchemaManager::new(&db);

        Migration.up(&manager).await.expect("migration succeeds");

        let log = db.into_transaction_log();
        let sql = log
            .iter()
            .flat_map(|transaction| transaction.statements())
            .map(|statement| statement.sql.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(sql.contains("UPDATE ai_pending_actions AS pending"));
        assert!(!sql.contains("project.created_by"));
        assert!(sql.contains("SELECT 1 FROM users"));
        assert!(sql.contains("RAISE EXCEPTION 'cannot make AI conversations user-owned"));
        assert!(!sql.contains("DELETE FROM ai_conversations"));
        assert!(!sql.contains("DELETE FROM ai_pending_actions"));
        assert!(sql.contains("ALTER COLUMN project_id DROP NOT NULL"));
        assert!(sql.contains("ALTER COLUMN created_by SET NOT NULL"));
        assert!(sql.contains("SET project_id = NULL"));
        assert!(sql.contains("chk_ai_conversations_single_context"));
        assert!(sql.contains("fk_ai_conversations_owner"));
        assert!(sql.contains("idx_ai_conversations_owner_activity"));

        let drop_not_null = sql
            .find("ALTER COLUMN project_id DROP NOT NULL")
            .expect("project context must become nullable");
        let null_application_context = sql
            .find("SET project_id = NULL")
            .expect("application conversations must lose the legacy project anchor");
        let add_owner_fk = sql
            .find("ADD CONSTRAINT fk_ai_conversations_owner")
            .expect("owner foreign key must be installed");
        assert!(
            drop_not_null < null_application_context,
            "the column must become nullable before application rows are updated"
        );
        assert!(
            null_application_context < add_owner_fk,
            "legacy cleanup and context conversion must finish before constraints are installed"
        );
    }

    #[tokio::test]
    async fn rollback_refuses_to_delete_unprojected_history() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([Default::default(), Default::default()])
            .into_connection();
        let manager = SchemaManager::new(&db);

        Migration.down(&manager).await.expect("rollback succeeds");

        let sql = db
            .into_transaction_log()
            .iter()
            .flat_map(|transaction| transaction.statements())
            .map(|statement| statement.sql.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(sql.contains("cannot roll back user-owned AI conversations"));
        assert!(sql.contains("RAISE EXCEPTION"));
        assert!(!sql.contains("DELETE FROM ai_conversations"));
        assert!(!sql.contains("DELETE FROM ai_pending_actions"));
        let refusal = sql
            .find("RAISE EXCEPTION")
            .expect("rollback must fail closed before schema changes");
        let schema_change = sql
            .find("ALTER COLUMN project_id SET NOT NULL")
            .expect("rollback should retain the historical schema reversal");
        assert!(refusal < schema_change);
    }
}
