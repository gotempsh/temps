use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(OidcProviders::Table)
                    .add_column(
                        ColumnDef::new(OidcProviders::Template)
                            .text()
                            .not_null()
                            .default("generic"),
                    )
                    .add_column(
                        ColumnDef::new(OidcProviders::GroupClaim)
                            .text()
                            .not_null()
                            .default("groups"),
                    )
                    .add_column(
                        ColumnDef::new(OidcProviders::RoleClaim)
                            .text()
                            .not_null()
                            .default("roles"),
                    )
                    .add_column(
                        ColumnDef::new(OidcProviders::DefaultRole)
                            .text()
                            .not_null()
                            .default("user"),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(OidcRoleMappings::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(OidcRoleMappings::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(OidcRoleMappings::ProviderId)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(OidcRoleMappings::Priority)
                            .integer()
                            .not_null()
                            .default(100),
                    )
                    .col(ColumnDef::new(OidcRoleMappings::IdpGroup).text().not_null())
                    .col(ColumnDef::new(OidcRoleMappings::Role).text().not_null())
                    .col(
                        ColumnDef::new(OidcRoleMappings::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_oidc_role_mappings_provider_id")
                            .from(OidcRoleMappings::Table, OidcRoleMappings::ProviderId)
                            .to(OidcProviders::Table, OidcProviders::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_oidc_role_mappings_provider_priority")
                    .table(OidcRoleMappings::Table)
                    .col(OidcRoleMappings::ProviderId)
                    .col(OidcRoleMappings::Priority)
                    .col(OidcRoleMappings::Id)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(OidcRoleMappings::Table).to_owned())
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(OidcProviders::Table)
                    .drop_column(OidcProviders::Template)
                    .drop_column(OidcProviders::GroupClaim)
                    .drop_column(OidcProviders::RoleClaim)
                    .drop_column(OidcProviders::DefaultRole)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum OidcProviders {
    Table,
    Id,
    Template,
    GroupClaim,
    RoleClaim,
    DefaultRole,
}

#[derive(DeriveIden)]
enum OidcRoleMappings {
    Table,
    Id,
    ProviderId,
    Priority,
    IdpGroup,
    Role,
    CreatedAt,
}
