//! Migration to add encrypted_token column to deployment_tokens table
//!
//! This allows the platform to store the encrypted token value so it can be
//! retrieved during subsequent deployments. Previously, only the hash was stored,
//! which meant tokens couldn't be retrieved after initial creation.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Add encrypted_token column to deployment_tokens table
        // This stores the token encrypted with the platform's encryption key
        manager
            .alter_table(
                Table::alter()
                    .table(DeploymentTokens::Table)
                    .add_column(
                        ColumnDef::new(DeploymentTokens::EncryptedToken)
                            .text()
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
                    .table(DeploymentTokens::Table)
                    .drop_column(DeploymentTokens::EncryptedToken)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum DeploymentTokens {
    Table,
    EncryptedToken,
}
