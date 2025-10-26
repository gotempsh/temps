use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Add config column to external_services table
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("external_services"))
                    .add_column(
                        ColumnDef::new(Alias::new("config")).text().null(), // Nullable for existing rows
                    )
                    .to_owned(),
            )
            .await?;

        // Drop external_service_params table (no longer needed)
        manager
            .drop_table(
                Table::drop()
                    .table(Alias::new("external_service_params"))
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Recreate external_service_params table
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("external_service_params"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("service_id"))
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("key")).string().not_null())
                    .col(ColumnDef::new(Alias::new("value")).text().not_null())
                    .col(
                        ColumnDef::new(Alias::new("created_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("updated_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        // Drop config column from external_services
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("external_services"))
                    .drop_column(Alias::new("config"))
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}
