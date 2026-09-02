// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(CloudBackupMirrorStates::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(CloudBackupMirrorStates::BackupId)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(CloudBackupMirrorStates::TenantId)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(CloudBackupMirrorStates::SchemaVersion)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(CloudBackupMirrorStates::Outcome)
                            .string_len(32)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(CloudBackupMirrorStates::Classification)
                            .string_len(32)
                            .not_null(),
                    )
                    .col(ColumnDef::new(CloudBackupMirrorStates::Reason).text())
                    .col(
                        ColumnDef::new(CloudBackupMirrorStates::AttemptCount)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(CloudBackupMirrorStates::RetryAfter)
                            .timestamp_with_time_zone(),
                    )
                    .col(
                        ColumnDef::new(CloudBackupMirrorStates::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .primary_key(
                        Index::create()
                            .col(CloudBackupMirrorStates::BackupId)
                            .col(CloudBackupMirrorStates::TenantId),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_cloud_backup_mirror_states_backup_id")
                            .from(
                                CloudBackupMirrorStates::Table,
                                CloudBackupMirrorStates::BackupId,
                            )
                            .to(Backups::Table, Backups::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .get_connection()
            .execute_unprepared(
                "CREATE INDEX IF NOT EXISTS idx_cloud_backup_mirror_states_due ON cloud_backup_mirror_states (tenant_id, retry_after, backup_id) WHERE outcome <> 'complete'",
            )
            .await?;
        manager
            .create_table(
                Table::create()
                    .table(CloudBackupMirrorCursors::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(CloudBackupMirrorCursors::TenantId)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(CloudBackupMirrorCursors::LastFinishedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(CloudBackupMirrorCursors::LastBackupId)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(CloudBackupMirrorCursors::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE INDEX IF NOT EXISTS idx_backups_cloud_mirror_discovery ON backups ((COALESCE(finished_at, started_at)), id) WHERE state = 'completed'",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP INDEX IF EXISTS idx_backups_cloud_mirror_discovery")
            .await?;
        manager
            .drop_table(
                Table::drop()
                    .table(CloudBackupMirrorCursors::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(
                Table::drop()
                    .table(CloudBackupMirrorStates::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum CloudBackupMirrorStates {
    Table,
    BackupId,
    TenantId,
    SchemaVersion,
    Outcome,
    Classification,
    Reason,
    AttemptCount,
    RetryAfter,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum CloudBackupMirrorCursors {
    Table,
    TenantId,
    LastFinishedAt,
    LastBackupId,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum Backups {
    Table,
    Id,
}

#[cfg(test)]
mod tests {
    use super::Migration;
    use sea_orm_migration::{
        sea_orm::{ConnectionTrait, Database},
        MigrationTrait, SchemaManager,
    };

    #[tokio::test]
    async fn cloud_backup_mirror_state_schema_runs_on_sqlite() {
        let db = Database::connect("sqlite::memory:")
            .await
            .expect("in-memory SQLite connects");
        db.execute_unprepared(
            "CREATE TABLE backups (id INTEGER PRIMARY KEY, state TEXT NOT NULL, started_at TEXT NOT NULL, finished_at TEXT)",
        )
        .await
        .expect("minimal backups table creates");
        let manager = SchemaManager::new(&db);
        Migration
            .up(&manager)
            .await
            .expect("Cloud mirror migration runs on SQLite");

        assert!(manager
            .has_table("cloud_backup_mirror_states")
            .await
            .expect("state table check succeeds"));
        assert!(manager
            .has_table("cloud_backup_mirror_cursors")
            .await
            .expect("cursor table check succeeds"));
        assert!(manager
            .has_index(
                "cloud_backup_mirror_states",
                "idx_cloud_backup_mirror_states_due"
            )
            .await
            .expect("due index check succeeds"));
        assert!(manager
            .has_index("backups", "idx_backups_cloud_mirror_discovery")
            .await
            .expect("discovery index check succeeds"));
    }
}
