use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // ========================================
        // PROJECTS TABLE - Add preview environment configuration
        // ========================================

        // Add enable_preview_environments column
        manager
            .alter_table(
                Table::alter()
                    .table(Projects::Table)
                    .add_column(
                        ColumnDef::new(Projects::EnablePreviewEnvironments)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .to_owned(),
            )
            .await?;

        // Create index on enable_preview_environments for query performance
        manager
            .create_index(
                Index::create()
                    .name("idx_projects_enable_preview_environments")
                    .table(Projects::Table)
                    .col(Projects::EnablePreviewEnvironments)
                    .to_owned(),
            )
            .await?;

        // ========================================
        // ENVIRONMENTS TABLE - Add preview environment tracking
        // ========================================

        // Add is_preview column to mark preview environments
        // Note: Use existing 'branch' field to track the preview branch, no need for source_branch
        manager
            .alter_table(
                Table::alter()
                    .table(Environments::Table)
                    .add_column(
                        ColumnDef::new(Environments::IsPreview)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .to_owned(),
            )
            .await?;

        // Create index on is_preview for query performance
        manager
            .create_index(
                Index::create()
                    .name("idx_environments_is_preview")
                    .table(Environments::Table)
                    .col(Environments::IsPreview)
                    .to_owned(),
            )
            .await?;

        // ========================================
        // ENV_VARS TABLE - Add preview inclusion flag
        // ========================================

        // Add include_in_preview column to control which vars are copied to preview envs
        manager
            .alter_table(
                Table::alter()
                    .table(EnvVars::Table)
                    .add_column(
                        ColumnDef::new(EnvVars::IncludeInPreview)
                            .boolean()
                            .not_null()
                            .default(true), // Default to true - include all existing vars in preview
                    )
                    .to_owned(),
            )
            .await?;

        // Create index on include_in_preview for query performance
        manager
            .create_index(
                Index::create()
                    .name("idx_env_vars_include_in_preview")
                    .table(EnvVars::Table)
                    .col(EnvVars::IncludeInPreview)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // ========================================
        // ENV_VARS TABLE - Remove preview inclusion flag
        // ========================================

        manager
            .drop_index(
                Index::drop()
                    .name("idx_env_vars_include_in_preview")
                    .table(EnvVars::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(EnvVars::Table)
                    .drop_column(EnvVars::IncludeInPreview)
                    .to_owned(),
            )
            .await?;

        // ========================================
        // ENVIRONMENTS TABLE - Remove preview environment tracking
        // ========================================

        manager
            .drop_index(
                Index::drop()
                    .name("idx_environments_is_preview")
                    .table(Environments::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Environments::Table)
                    .drop_column(Environments::IsPreview)
                    .to_owned(),
            )
            .await?;

        // ========================================
        // PROJECTS TABLE - Remove preview environment configuration
        // ========================================

        manager
            .drop_index(
                Index::drop()
                    .name("idx_projects_enable_preview_environments")
                    .table(Projects::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Projects::Table)
                    .drop_column(Projects::EnablePreviewEnvironments)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum Projects {
    Table,
    EnablePreviewEnvironments,
}

#[derive(DeriveIden)]
enum Environments {
    Table,
    IsPreview,
}

#[derive(DeriveIden)]
enum EnvVars {
    Table,
    IncludeInPreview,
}
