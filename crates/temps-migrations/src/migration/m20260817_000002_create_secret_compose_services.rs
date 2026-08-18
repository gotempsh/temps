//! Migration to create the `secret_compose_services` junction table.
//!
//! Restricts a secret to specific Docker Compose services. Without it, every
//! service in a stack receives every secret the project defines -- so a
//! database or proxy sidecar can read an application's API keys even though
//! it has no use for them.
//!
//! Mirrors `secret_environments`: one row per (secret_id, service_name) pair.
//!
//! **No rows means every service**, which is why this migration backfills
//! nothing. Existing secrets keep their current delivery exactly; scoping is
//! opt-in per secret. Inserting rows on upgrade would silently stop delivering
//! secrets to services a running stack depends on.
//!
//! `service_name` is a plain string rather than a foreign key because Compose
//! service names live in the user's repository, not in our schema. A name that
//! no longer matches the compose file is reported at deploy time rather than
//! rejected here -- the repository can change long after the secret is scoped.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(SecretComposeServices::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(SecretComposeServices::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(SecretComposeServices::SecretId)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SecretComposeServices::ServiceName)
                            .string()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SecretComposeServices::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_secret_compose_services_secret_id")
                            .from(
                                SecretComposeServices::Table,
                                SecretComposeServices::SecretId,
                            )
                            .to(Secrets::Table, Secrets::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_secret_compose_services_unique")
                    .table(SecretComposeServices::Table)
                    .col(SecretComposeServices::SecretId)
                    .col(SecretComposeServices::ServiceName)
                    .unique()
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(SecretComposeServices::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum SecretComposeServices {
    Table,
    Id,
    SecretId,
    ServiceName,
    CreatedAt,
}

#[derive(DeriveIden)]
enum Secrets {
    Table,
    Id,
}
