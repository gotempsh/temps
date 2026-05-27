//! Adds project-level defaults that force every newly-created preview
//! environment to use on-demand mode (scale-to-zero).
//!
//! Columns added to `projects`:
//!   - `preview_envs_on_demand` (bool, default false) — when true, every preview
//!     environment auto-created for a branch is configured with
//!     `deployment_config.on_demand = true`.
//!   - `preview_envs_idle_timeout_seconds` (int, default 300) — idle timeout
//!     applied to those previews.
//!   - `preview_envs_wake_timeout_seconds` (int, default 30) — wake timeout
//!     applied to those previews.
//!
//! These settings only affect preview environments created AFTER the flag is
//! enabled. Existing environments retain whatever `deployment_config` they had.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Projects::Table)
                    .add_column(
                        ColumnDef::new(Projects::PreviewEnvsOnDemand)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .add_column(
                        ColumnDef::new(Projects::PreviewEnvsIdleTimeoutSeconds)
                            .integer()
                            .not_null()
                            .default(300),
                    )
                    .add_column(
                        ColumnDef::new(Projects::PreviewEnvsWakeTimeoutSeconds)
                            .integer()
                            .not_null()
                            .default(30),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Projects::Table)
                    .drop_column(Projects::PreviewEnvsOnDemand)
                    .drop_column(Projects::PreviewEnvsIdleTimeoutSeconds)
                    .drop_column(Projects::PreviewEnvsWakeTimeoutSeconds)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum Projects {
    Table,
    PreviewEnvsOnDemand,
    PreviewEnvsIdleTimeoutSeconds,
    PreviewEnvsWakeTimeoutSeconds,
}
