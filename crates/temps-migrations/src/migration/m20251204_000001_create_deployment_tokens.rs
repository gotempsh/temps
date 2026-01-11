//! Migration to create deployment_tokens table
//!
//! Deployment tokens provide API access credentials that are automatically
//! injected into deployments as TEMPS_API_TOKEN environment variable.
//! This allows deployed applications to access Temps APIs for:
//! - Enriching visitor data
//! - Sending emails
//! - Other platform features

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Create deployment_tokens table
        manager
            .create_table(
                Table::create()
                    .table(DeploymentTokens::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(DeploymentTokens::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(DeploymentTokens::ProjectId)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(DeploymentTokens::EnvironmentId)
                            .integer()
                            .null(),
                    )
                    .col(ColumnDef::new(DeploymentTokens::Name).string().not_null())
                    .col(
                        ColumnDef::new(DeploymentTokens::TokenHash)
                            .string()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(DeploymentTokens::TokenPrefix)
                            .string()
                            .not_null(),
                    )
                    .col(ColumnDef::new(DeploymentTokens::Permissions).json().null())
                    .col(
                        ColumnDef::new(DeploymentTokens::IsActive)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(
                        ColumnDef::new(DeploymentTokens::ExpiresAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(DeploymentTokens::LastUsedAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(DeploymentTokens::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(DeploymentTokens::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(ColumnDef::new(DeploymentTokens::CreatedBy).integer().null())
                    .to_owned(),
            )
            .await?;

        // Add foreign key to projects table
        manager
            .create_foreign_key(
                ForeignKey::create()
                    .name("fk_deployment_tokens_project")
                    .from(DeploymentTokens::Table, DeploymentTokens::ProjectId)
                    .to(Projects::Table, Projects::Id)
                    .on_delete(ForeignKeyAction::Cascade)
                    .to_owned(),
            )
            .await?;

        // Add foreign key to environments table (optional)
        manager
            .create_foreign_key(
                ForeignKey::create()
                    .name("fk_deployment_tokens_environment")
                    .from(DeploymentTokens::Table, DeploymentTokens::EnvironmentId)
                    .to(Environments::Table, Environments::Id)
                    .on_delete(ForeignKeyAction::Cascade)
                    .to_owned(),
            )
            .await?;

        // Add foreign key to users table for created_by (optional)
        manager
            .create_foreign_key(
                ForeignKey::create()
                    .name("fk_deployment_tokens_created_by")
                    .from(DeploymentTokens::Table, DeploymentTokens::CreatedBy)
                    .to(Users::Table, Users::Id)
                    .on_delete(ForeignKeyAction::SetNull)
                    .to_owned(),
            )
            .await?;

        // Create index on project_id for faster lookups
        manager
            .create_index(
                Index::create()
                    .name("idx_deployment_tokens_project_id")
                    .table(DeploymentTokens::Table)
                    .col(DeploymentTokens::ProjectId)
                    .to_owned(),
            )
            .await?;

        // Create index on environment_id for faster lookups
        manager
            .create_index(
                Index::create()
                    .name("idx_deployment_tokens_environment_id")
                    .table(DeploymentTokens::Table)
                    .col(DeploymentTokens::EnvironmentId)
                    .to_owned(),
            )
            .await?;

        // Create index on token_hash for validation lookups
        manager
            .create_index(
                Index::create()
                    .name("idx_deployment_tokens_token_hash")
                    .table(DeploymentTokens::Table)
                    .col(DeploymentTokens::TokenHash)
                    .to_owned(),
            )
            .await?;

        // Create unique constraint on name within project scope
        manager
            .create_index(
                Index::create()
                    .name("idx_deployment_tokens_project_name_unique")
                    .table(DeploymentTokens::Table)
                    .col(DeploymentTokens::ProjectId)
                    .col(DeploymentTokens::Name)
                    .unique()
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Drop indexes
        manager
            .drop_index(
                Index::drop()
                    .name("idx_deployment_tokens_project_name_unique")
                    .table(DeploymentTokens::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name("idx_deployment_tokens_token_hash")
                    .table(DeploymentTokens::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name("idx_deployment_tokens_environment_id")
                    .table(DeploymentTokens::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name("idx_deployment_tokens_project_id")
                    .table(DeploymentTokens::Table)
                    .to_owned(),
            )
            .await?;

        // Drop foreign keys
        manager
            .drop_foreign_key(
                ForeignKey::drop()
                    .name("fk_deployment_tokens_created_by")
                    .table(DeploymentTokens::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .drop_foreign_key(
                ForeignKey::drop()
                    .name("fk_deployment_tokens_environment")
                    .table(DeploymentTokens::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .drop_foreign_key(
                ForeignKey::drop()
                    .name("fk_deployment_tokens_project")
                    .table(DeploymentTokens::Table)
                    .to_owned(),
            )
            .await?;

        // Drop table
        manager
            .drop_table(Table::drop().table(DeploymentTokens::Table).to_owned())
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum DeploymentTokens {
    Table,
    Id,
    ProjectId,
    EnvironmentId,
    Name,
    TokenHash,
    TokenPrefix,
    Permissions,
    IsActive,
    ExpiresAt,
    LastUsedAt,
    CreatedAt,
    UpdatedAt,
    CreatedBy,
}

#[derive(DeriveIden)]
enum Projects {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum Environments {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum Users {
    Table,
    Id,
}
