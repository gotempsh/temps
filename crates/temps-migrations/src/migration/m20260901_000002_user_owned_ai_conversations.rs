// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Makes AI conversations private, user-owned resources.
//!
//! A project is optional execution context, not the authorization boundary.
//! Legacy rows without an owner fail closed and are removed before the owner
//! columns become non-null. Pending actions follow the same ownership model so
//! a global operation can be proposed without inventing a project scope.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "DELETE FROM ai_pending_actions AS pending
                   WHERE pending.created_by IS NULL
                      OR NOT EXISTS (
                          SELECT 1 FROM users
                           WHERE users.id = pending.created_by
                      )
                      OR NOT EXISTS (
                          SELECT 1 FROM ai_conversations AS conversation
                           WHERE conversation.id = pending.conversation_id
                      )
                      OR EXISTS (
                          SELECT 1 FROM ai_conversations AS conversation
                           WHERE conversation.id = pending.conversation_id
                             AND (
                                 conversation.created_by IS NULL
                                 OR conversation.created_by IS DISTINCT FROM pending.created_by
                                 OR NOT EXISTS (
                                     SELECT 1 FROM users
                                      WHERE users.id = conversation.created_by
                                 )
                             )
                      );
                 DELETE FROM ai_conversations AS conversation
                  WHERE conversation.created_by IS NULL
                     OR NOT EXISTS (
                         SELECT 1 FROM users
                          WHERE users.id = conversation.created_by
                     );

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

                 DELETE FROM ai_pending_actions WHERE project_id IS NULL;
                 DELETE FROM ai_conversations WHERE project_id IS NULL;
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
        assert!(sql.contains("DELETE FROM ai_pending_actions AS pending"));
        assert!(sql.contains("NOT EXISTS (\n                          SELECT 1 FROM users"));
        assert!(sql.contains("conversation.created_by IS DISTINCT FROM pending.created_by"));
        assert!(sql.contains("DELETE FROM ai_conversations AS conversation"));
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
}
