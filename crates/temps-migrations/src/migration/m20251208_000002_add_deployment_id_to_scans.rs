use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Add deployment_id column to vulnerability_scans table
        manager
            .alter_table(
                Table::alter()
                    .table(VulnerabilityScans::Table)
                    .add_column(
                        ColumnDef::new(VulnerabilityScans::DeploymentId)
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
                    .table(VulnerabilityScans::Table)
                    .add_foreign_key(
                        TableForeignKey::new()
                            .name("fk_vulnerability_scans_deployment")
                            .from_tbl(VulnerabilityScans::Table)
                            .from_col(VulnerabilityScans::DeploymentId)
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
                    .name("idx_vulnerability_scans_deployment_id")
                    .table(VulnerabilityScans::Table)
                    .col(VulnerabilityScans::DeploymentId)
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
                    .name("idx_vulnerability_scans_deployment_id")
                    .table(VulnerabilityScans::Table)
                    .to_owned(),
            )
            .await?;

        // Drop foreign key (need to drop it before dropping the column)
        manager
            .alter_table(
                Table::alter()
                    .table(VulnerabilityScans::Table)
                    .drop_foreign_key(Alias::new("fk_vulnerability_scans_deployment"))
                    .to_owned(),
            )
            .await?;

        // Drop deployment_id column
        manager
            .alter_table(
                Table::alter()
                    .table(VulnerabilityScans::Table)
                    .drop_column(VulnerabilityScans::DeploymentId)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum VulnerabilityScans {
    Table,
    DeploymentId,
}

#[derive(DeriveIden)]
enum Deployments {
    Table,
    Id,
}
