use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Create ip_access_control table
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("ip_access_control"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("ip_address"))
                            .custom(Alias::new("inet"))
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("action"))
                            .string_len(10)
                            .not_null()
                            .default("block"),
                    )
                    .col(ColumnDef::new(Alias::new("reason")).text().null())
                    .col(ColumnDef::new(Alias::new("created_by")).integer().null())
                    .col(
                        ColumnDef::new(Alias::new("created_at"))
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(Alias::new("updated_at"))
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await?;

        // Create unique index on ip_address to prevent duplicates
        manager
            .create_index(
                Index::create()
                    .name("idx_ip_access_control_ip_unique")
                    .table(Alias::new("ip_access_control"))
                    .col(Alias::new("ip_address"))
                    .unique()
                    .to_owned(),
            )
            .await?;

        // Create index on action for filtering
        manager
            .create_index(
                Index::create()
                    .name("idx_ip_access_control_action")
                    .table(Alias::new("ip_access_control"))
                    .col(Alias::new("action"))
                    .to_owned(),
            )
            .await?;

        // Create foreign key to users table for created_by
        manager
            .create_foreign_key(
                ForeignKey::create()
                    .name("fk_ip_access_control_created_by")
                    .from(Alias::new("ip_access_control"), Alias::new("created_by"))
                    .to(Alias::new("users"), Alias::new("id"))
                    .on_delete(ForeignKeyAction::SetNull)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Drop foreign key first
        manager
            .drop_foreign_key(
                ForeignKey::drop()
                    .name("fk_ip_access_control_created_by")
                    .table(Alias::new("ip_access_control"))
                    .to_owned(),
            )
            .await?;

        // Drop indexes
        manager
            .drop_index(
                Index::drop()
                    .name("idx_ip_access_control_action")
                    .table(Alias::new("ip_access_control"))
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name("idx_ip_access_control_ip_unique")
                    .table(Alias::new("ip_access_control"))
                    .to_owned(),
            )
            .await?;

        // Drop table
        manager
            .drop_table(
                Table::drop()
                    .table(Alias::new("ip_access_control"))
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}
