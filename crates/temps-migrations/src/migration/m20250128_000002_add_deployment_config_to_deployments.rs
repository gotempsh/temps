use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Add deployment_config column to deployments table
        manager
            .alter_table(
                Table::alter()
                    .table(Deployments::Table)
                    .add_column_if_not_exists(
                        ColumnDef::new(Deployments::DeploymentConfig).json().null(),
                    )
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Remove deployment_config column from deployments table
        manager
            .alter_table(
                Table::alter()
                    .table(Deployments::Table)
                    .drop_column(Deployments::DeploymentConfig)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum Deployments {
    Table,
    DeploymentConfig,
}
