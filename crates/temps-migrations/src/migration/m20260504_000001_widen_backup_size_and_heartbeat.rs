//! Widen `size_bytes` from INTEGER to BIGINT on `backups` and
//! `external_service_backups`, and add a `last_heartbeat_at` column to
//! `backups` for stuck-backup detection.
//!
//! The previous i32 cap silently truncated any DB backup larger than ~2.1 GB
//! (and produced NULL when the cast overflowed). The heartbeat column lets
//! the UI badge a backup as "stalled" when the worker stops updating it.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // backups.size_bytes: INTEGER -> BIGINT
        manager
            .alter_table(
                Table::alter()
                    .table(Backups::Table)
                    .modify_column(ColumnDef::new(Backups::SizeBytes).big_integer().null())
                    .to_owned(),
            )
            .await?;

        // external_service_backups.size_bytes: INTEGER -> BIGINT
        manager
            .alter_table(
                Table::alter()
                    .table(ExternalServiceBackups::Table)
                    .modify_column(
                        ColumnDef::new(ExternalServiceBackups::SizeBytes)
                            .big_integer()
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;

        // backups.last_heartbeat_at: track liveness of running backups so the
        // UI can flag stalled ones.
        manager
            .alter_table(
                Table::alter()
                    .table(Backups::Table)
                    .add_column(
                        ColumnDef::new(Backups::LastHeartbeatAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Backups::Table)
                    .drop_column(Backups::LastHeartbeatAt)
                    .to_owned(),
            )
            .await?;

        // BIGINT -> INTEGER will fail if any row exceeds i32::MAX, which is
        // exactly the scenario this migration was created to fix. We attempt
        // the narrowing anyway so `down` is symmetric; production rollback
        // requires data cleanup first.
        manager
            .alter_table(
                Table::alter()
                    .table(ExternalServiceBackups::Table)
                    .modify_column(
                        ColumnDef::new(ExternalServiceBackups::SizeBytes)
                            .integer()
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Backups::Table)
                    .modify_column(ColumnDef::new(Backups::SizeBytes).integer().null())
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum Backups {
    Table,
    SizeBytes,
    LastHeartbeatAt,
}

#[derive(DeriveIden)]
enum ExternalServiceBackups {
    Table,
    SizeBytes,
}
