use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Add deployment_id column to deployment_tokens table
        manager
            .alter_table(
                Table::alter()
                    .table(DeploymentTokens::Table)
                    .add_column(
                        ColumnDef::new(DeploymentTokens::DeploymentId)
                            .integer()
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;

        // Add foreign key constraint to deployments table
        manager
            .alter_table(
                Table::alter()
                    .table(DeploymentTokens::Table)
                    .add_foreign_key(
                        TableForeignKey::new()
                            .name("fk_deployment_tokens_deployment")
                            .from_tbl(DeploymentTokens::Table)
                            .from_col(DeploymentTokens::DeploymentId)
                            .to_tbl(Deployments::Table)
                            .to_col(Deployments::Id)
                            .on_delete(ForeignKeyAction::SetNull),
                    )
                    .to_owned(),
            )
            .await?;

        // Add index for efficient queries by deployment_id
        manager
            .create_index(
                Index::create()
                    .name("idx_deployment_tokens_deployment_id")
                    .table(DeploymentTokens::Table)
                    .col(DeploymentTokens::DeploymentId)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Drop index
        manager
            .drop_index(
                Index::drop()
                    .name("idx_deployment_tokens_deployment_id")
                    .table(DeploymentTokens::Table)
                    .to_owned(),
            )
            .await?;

        // Drop foreign key
        manager
            .alter_table(
                Table::alter()
                    .table(DeploymentTokens::Table)
                    .drop_foreign_key(Alias::new("fk_deployment_tokens_deployment"))
                    .to_owned(),
            )
            .await?;

        // Drop deployment_id column
        manager
            .alter_table(
                Table::alter()
                    .table(DeploymentTokens::Table)
                    .drop_column(DeploymentTokens::DeploymentId)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum DeploymentTokens {
    Table,
    DeploymentId,
}

#[derive(DeriveIden)]
enum Deployments {
    Table,
    Id,
}
