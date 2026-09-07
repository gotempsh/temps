// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Constrain AI application workspaces to trusted runtimes and host-safe
//! per-workspace resource ceilings.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "DO $$ \
                 DECLARE incompatible_count BIGINT; \
                 BEGIN \
                   SELECT COUNT(*) INTO incompatible_count \
                   FROM ai_application_workspaces \
                   WHERE runtime = 'custom' OR image IS NOT NULL \
                      OR cpu_limit > 8 OR memory_limit_mb > 16384 \
                      OR pids_limit > 2048 OR disk_limit_mb > 65536; \
                   IF incompatible_count > 0 THEN \
                     RAISE EXCEPTION 'cannot harden application workspaces: % row(s) use a custom image or exceed the safe resource ceilings; update those workspace settings before retrying', incompatible_count; \
                   END IF; \
                 END $$; \
                 ALTER TABLE ai_application_workspaces \
                   DROP CONSTRAINT IF EXISTS ai_application_workspaces_runtime_check, \
                   DROP CONSTRAINT IF EXISTS ai_application_workspaces_cpu_check, \
                   DROP CONSTRAINT IF EXISTS ai_application_workspaces_memory_check, \
                   DROP CONSTRAINT IF EXISTS ai_application_workspaces_pids_check, \
                   DROP CONSTRAINT IF EXISTS ai_application_workspaces_disk_check, \
                   DROP CONSTRAINT IF EXISTS ai_application_workspaces_custom_image_check, \
                   ADD CONSTRAINT ai_application_workspaces_runtime_check \
                     CHECK (runtime IN ('node', 'bun', 'python', 'rust', 'go', 'full')), \
                   ADD CONSTRAINT ai_application_workspaces_image_check CHECK (image IS NULL), \
                   ADD CONSTRAINT ai_application_workspaces_cpu_check \
                     CHECK (cpu_limit BETWEEN 0.25 AND 8), \
                   ADD CONSTRAINT ai_application_workspaces_memory_check \
                     CHECK (memory_limit_mb BETWEEN 256 AND 16384), \
                   ADD CONSTRAINT ai_application_workspaces_pids_check \
                     CHECK (pids_limit BETWEEN 64 AND 2048), \
                   ADD CONSTRAINT ai_application_workspaces_disk_check \
                     CHECK (disk_limit_mb BETWEEN 512 AND 65536); \
                 /* SeaORM runs this migration transactionally, so PostgreSQL
                    cannot build this index CONCURRENTLY. A brief write lock on
                    sandboxes during upgrade is expected. */ \
                 CREATE UNIQUE INDEX uq_sandboxes_active_application_workspace \
                   ON sandboxes (user_id, name) \
                   WHERE name LIKE 'ai-application:%' AND status <> 'destroyed';",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE ai_application_workspaces \
                   DROP CONSTRAINT IF EXISTS ai_application_workspaces_image_check, \
                   DROP CONSTRAINT IF EXISTS ai_application_workspaces_runtime_check, \
                   DROP CONSTRAINT IF EXISTS ai_application_workspaces_cpu_check, \
                   DROP CONSTRAINT IF EXISTS ai_application_workspaces_memory_check, \
                   DROP CONSTRAINT IF EXISTS ai_application_workspaces_pids_check, \
                   DROP CONSTRAINT IF EXISTS ai_application_workspaces_disk_check, \
                   ADD CONSTRAINT ai_application_workspaces_runtime_check \
                     CHECK (runtime IN ('node', 'bun', 'python', 'rust', 'go', 'full', 'custom')), \
                   ADD CONSTRAINT ai_application_workspaces_cpu_check \
                     CHECK (cpu_limit BETWEEN 0.25 AND 32), \
                   ADD CONSTRAINT ai_application_workspaces_memory_check \
                     CHECK (memory_limit_mb BETWEEN 256 AND 131072), \
                   ADD CONSTRAINT ai_application_workspaces_pids_check \
                     CHECK (pids_limit BETWEEN 64 AND 32768), \
                   ADD CONSTRAINT ai_application_workspaces_disk_check \
                     CHECK (disk_limit_mb BETWEEN 512 AND 1048576), \
                   ADD CONSTRAINT ai_application_workspaces_custom_image_check \
                     CHECK (runtime <> 'custom' OR image IS NOT NULL); \
                 DROP INDEX IF EXISTS uq_sandboxes_active_application_workspace;",
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
    async fn incompatible_workspace_settings_fail_without_being_rewritten() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([Default::default()])
            .into_connection();
        Migration
            .up(&SchemaManager::new(&db))
            .await
            .expect("mock migration");
        let sql = db
            .into_transaction_log()
            .iter()
            .flat_map(|transaction| transaction.statements())
            .map(|statement| statement.sql.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(sql.contains("RAISE EXCEPTION 'cannot harden application workspaces"));
        assert!(!sql.contains("SET runtime = 'node'"));
        assert!(!sql.contains("LEAST("));
    }
}
