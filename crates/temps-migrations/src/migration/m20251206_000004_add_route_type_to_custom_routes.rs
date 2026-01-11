use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Add route_type column to custom_routes table
        // Values: 'http' (default) - matches on HTTP Host header
        //         'tls' - matches on TLS SNI hostname
        manager
            .alter_table(
                Table::alter()
                    .table(CustomRoutes::Table)
                    .add_column(
                        ColumnDef::new(CustomRoutes::RouteType)
                            .string_len(10)
                            .not_null()
                            .default("http"),
                    )
                    .to_owned(),
            )
            .await?;

        // Add index on route_type for efficient filtering
        manager
            .create_index(
                Index::create()
                    .name("idx_custom_routes_route_type")
                    .table(CustomRoutes::Table)
                    .col(CustomRoutes::RouteType)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Drop the index first
        manager
            .drop_index(
                Index::drop()
                    .name("idx_custom_routes_route_type")
                    .table(CustomRoutes::Table)
                    .to_owned(),
            )
            .await?;

        // Remove the route_type column
        manager
            .alter_table(
                Table::alter()
                    .table(CustomRoutes::Table)
                    .drop_column(CustomRoutes::RouteType)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum CustomRoutes {
    Table,
    RouteType,
}
