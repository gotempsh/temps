//! Migration to add `allow_alternate_sources` to the `projects` table.
//!
//! Per-project opt-in that lets a project accept deployments from a source
//! other than its configured `source_type` — most importantly, letting a
//! Git-backed project take an uploaded source archive (`drop`) without giving
//! up its repository, branch, webhooks or rollback-rebuild behaviour.
//!
//! The column is NULLABLE with no default: existing rows get NULL on upgrade,
//! which reads as "off". That keeps the upgrade additive and preserves today's
//! behaviour, where `POST /projects/{id}/environments/{id}/deploy/source`
//! rejects any project whose `source_type` is not `uploaded_source`.
//!
//! Deliberately a column rather than a wider `source_type` change: everything
//! that keys off `source_type == Git` (rollback rebuild-from-source in
//! `temps-deployments`, git-info validation in `temps-projects`) must keep
//! seeing a Git project.

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
                        ColumnDef::new(Projects::AllowAlternateSources)
                            .boolean()
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
                    .table(Projects::Table)
                    .drop_column(Projects::AllowAlternateSources)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum Projects {
    Table,
    AllowAlternateSources,
}
