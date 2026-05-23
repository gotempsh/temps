use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(OidcProviders::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(OidcProviders::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(OidcProviders::Name).text().not_null())
                    .col(ColumnDef::new(OidcProviders::IssuerUrl).text().not_null())
                    .col(ColumnDef::new(OidcProviders::ClientId).text().not_null())
                    .col(
                        ColumnDef::new(OidcProviders::ClientSecretEncrypted)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(OidcProviders::Scopes)
                            .text()
                            .not_null()
                            .default("openid email profile"),
                    )
                    .col(
                        ColumnDef::new(OidcProviders::JitProvisioning)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(
                        ColumnDef::new(OidcProviders::Enabled)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(
                        ColumnDef::new(OidcProviders::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(OidcProviders::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Users::Table)
                    .add_column(ColumnDef::new(Users::OidcSubject).text().null())
                    .add_column(ColumnDef::new(Users::OidcProviderId).integer().null())
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("users_oidc_unique")
                    .table(Users::Table)
                    .col(Users::OidcProviderId)
                    .col(Users::OidcSubject)
                    .unique()
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(OidcLoginStates::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(OidcLoginStates::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(OidcLoginStates::State)
                            .string_len(128)
                            .not_null()
                            .unique_key(),
                    )
                    .col(ColumnDef::new(OidcLoginStates::Nonce).text().not_null())
                    .col(
                        ColumnDef::new(OidcLoginStates::PkceVerifier)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(OidcLoginStates::ProviderId)
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(OidcLoginStates::ReturnTo).text().null())
                    .col(
                        ColumnDef::new(OidcLoginStates::ExpiresAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(OidcLoginStates::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_oidc_login_states_provider_id")
                            .from(OidcLoginStates::Table, OidcLoginStates::ProviderId)
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
                    .name("idx_oidc_login_states_expires_at")
                    .table(OidcLoginStates::Table)
                    .col(OidcLoginStates::ExpiresAt)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(OidcLoginStates::Table).to_owned())
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name("users_oidc_unique")
                    .table(Users::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Users::Table)
                    .drop_column(Users::OidcSubject)
                    .drop_column(Users::OidcProviderId)
                    .to_owned(),
            )
            .await?;

        manager
            .drop_table(Table::drop().table(OidcProviders::Table).to_owned())
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum OidcProviders {
    Table,
    Id,
    Name,
    IssuerUrl,
    ClientId,
    ClientSecretEncrypted,
    Scopes,
    JitProvisioning,
    Enabled,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum OidcLoginStates {
    Table,
    Id,
    State,
    Nonce,
    PkceVerifier,
    ProviderId,
    ReturnTo,
    ExpiresAt,
    CreatedAt,
}

#[derive(DeriveIden)]
enum Users {
    Table,
    OidcSubject,
    OidcProviderId,
}
