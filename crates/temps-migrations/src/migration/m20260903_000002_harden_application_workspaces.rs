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
                "UPDATE ai_application_workspaces SET runtime = 'node', image = NULL \
                 WHERE runtime = 'custom' OR image IS NOT NULL; \
                 UPDATE ai_application_workspaces SET cpu_limit = LEAST(cpu_limit, 8), \
                   memory_limit_mb = LEAST(memory_limit_mb, 16384), \
                   pids_limit = LEAST(pids_limit, 2048), \
                   disk_limit_mb = LEAST(disk_limit_mb, 65536); \
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
