//! Add a `backend` column to `sandboxes` (ADR-029).
//!
//! Records the isolation backend each standalone sandbox runs on
//! ("docker" | "firecracker") in a typed column, rather than inferring it
//! from the container-name prefix. Nullable: existing rows predate the
//! column and are treated as the historical default (docker) by readers.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("ALTER TABLE sandboxes ADD COLUMN IF NOT EXISTS backend VARCHAR")
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("ALTER TABLE sandboxes DROP COLUMN IF EXISTS backend")
            .await?;
        Ok(())
    }
}
