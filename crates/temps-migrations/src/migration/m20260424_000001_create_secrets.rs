//! Migration to create the `secrets` and `secret_environments` tables.
//!
//! Secrets are exposed to containers as files mounted under `/run/secrets/<KEY>`
//! (mode 0400, tmpfs) instead of as environment variables. Values are always
//! stored encrypted via EncryptionService (AES-256-GCM) and are never returned
//! in plaintext from the API after creation.
//!
//! Schema mirrors `env_vars` / `env_var_environments` so secrets share the same
//! project + environment scoping model, but lives in a dedicated table because:
//!   - Secrets have a different delivery mechanism (tmpfs file, not env var)
//!   - Values are immutable post-create and never plaintext-readable
//!   - Separating the tables prevents accidental plaintext exposure via any
//!     existing env-var read endpoint

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Secrets::Table)
                    .col(
                        ColumnDef::new(Secrets::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Secrets::ProjectId).integer().not_null())
                    .col(ColumnDef::new(Secrets::EnvironmentId).integer().null())
                    .col(ColumnDef::new(Secrets::Key).string().not_null())
                    .col(ColumnDef::new(Secrets::Value).text().not_null())
                    .col(
                        ColumnDef::new(Secrets::IncludeInPreview)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(Secrets::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(Secrets::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_secrets_project_id")
                            .from(Secrets::Table, Secrets::ProjectId)
                            .to(Projects::Table, Projects::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_secrets_environment_id")
                            .from(Secrets::Table, Secrets::EnvironmentId)
                            .to(Environments::Table, Environments::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_secrets_project_environment")
                    .table(Secrets::Table)
                    .col(Secrets::ProjectId)
                    .col(Secrets::EnvironmentId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_secrets_project_key_environment")
                    .table(Secrets::Table)
                    .col(Secrets::ProjectId)
                    .col(Secrets::Key)
                    .col(Secrets::EnvironmentId)
                    .unique()
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(SecretEnvironments::Table)
                    .col(
                        ColumnDef::new(SecretEnvironments::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(SecretEnvironments::SecretId)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SecretEnvironments::EnvironmentId)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SecretEnvironments::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_secret_environments_secret_id")
                            .from(SecretEnvironments::Table, SecretEnvironments::SecretId)
                            .to(Secrets::Table, Secrets::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_secret_environments_environment_id")
                            .from(SecretEnvironments::Table, SecretEnvironments::EnvironmentId)
                            .to(Environments::Table, Environments::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_secret_environments_secret_env_unique")
                    .table(SecretEnvironments::Table)
                    .col(SecretEnvironments::SecretId)
                    .col(SecretEnvironments::EnvironmentId)
                    .unique()
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(SecretEnvironments::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Secrets::Table).to_owned())
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum Secrets {
    Table,
    Id,
    ProjectId,
    EnvironmentId,
    Key,
    Value,
    IncludeInPreview,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum SecretEnvironments {
    Table,
    Id,
    SecretId,
    EnvironmentId,
    CreatedAt,
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
