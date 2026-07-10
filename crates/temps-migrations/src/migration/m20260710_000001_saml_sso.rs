use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(SamlProviders::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(SamlProviders::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(SamlProviders::Name).text().not_null())
                    .col(
                        ColumnDef::new(SamlProviders::Template)
                            .text()
                            .not_null()
                            .default("generic"),
                    )
                    .col(ColumnDef::new(SamlProviders::SpEntityId).text().not_null())
                    .col(ColumnDef::new(SamlProviders::IdpEntityId).text().not_null())
                    .col(ColumnDef::new(SamlProviders::IdpSsoUrl).text().not_null())
                    .col(ColumnDef::new(SamlProviders::IdpX509Cert).text().not_null())
                    .col(ColumnDef::new(SamlProviders::IdpMetadataUrl).text().null())
                    .col(
                        ColumnDef::new(SamlProviders::GroupAttribute)
                            .text()
                            .not_null()
                            .default("groups"),
                    )
                    .col(
                        ColumnDef::new(SamlProviders::RoleAttribute)
                            .text()
                            .not_null()
                            .default("roles"),
                    )
                    .col(
                        ColumnDef::new(SamlProviders::DefaultRole)
                            .text()
                            .not_null()
                            .default("user"),
                    )
                    .col(ColumnDef::new(SamlProviders::EmailAttribute).text().null())
                    .col(
                        ColumnDef::new(SamlProviders::JitProvisioning)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(
                        ColumnDef::new(SamlProviders::Enabled)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(
                        // Defaults true (unlike oidc_providers.trust_idp_email, which
                        // defaults false) -- SAML has no email_verified equivalent; the
                        // signed assertion itself is the trust anchor. See ADR 0013 §3.
                        ColumnDef::new(SamlProviders::TrustIdpEmail)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(
                        ColumnDef::new(SamlProviders::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(SamlProviders::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_saml_providers_name")
                    .table(SamlProviders::Table)
                    .col(SamlProviders::Name)
                    .unique()
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(SamlLoginStates::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(SamlLoginStates::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(SamlLoginStates::RelayState)
                            .string_len(128)
                            .not_null()
                            .unique_key(),
                    )
                    .col(
                        ColumnDef::new(SamlLoginStates::AuthnRequestId)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SamlLoginStates::ProviderId)
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(SamlLoginStates::ReturnTo).text().null())
                    .col(
                        ColumnDef::new(SamlLoginStates::ExpiresAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SamlLoginStates::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_saml_login_states_provider_id")
                            .from(SamlLoginStates::Table, SamlLoginStates::ProviderId)
                            .to(SamlProviders::Table, SamlProviders::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_saml_login_states_expires_at")
                    .table(SamlLoginStates::Table)
                    .col(SamlLoginStates::ExpiresAt)
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(SamlRoleMappings::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(SamlRoleMappings::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(SamlRoleMappings::ProviderId)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SamlRoleMappings::Priority)
                            .integer()
                            .not_null()
                            .default(100),
                    )
                    .col(ColumnDef::new(SamlRoleMappings::IdpGroup).text().not_null())
                    .col(ColumnDef::new(SamlRoleMappings::Role).text().not_null())
                    .col(
                        ColumnDef::new(SamlRoleMappings::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_saml_role_mappings_provider_id")
                            .from(SamlRoleMappings::Table, SamlRoleMappings::ProviderId)
                            .to(SamlProviders::Table, SamlProviders::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_saml_role_mappings_provider_priority")
                    .table(SamlRoleMappings::Table)
                    .col(SamlRoleMappings::ProviderId)
                    .col(SamlRoleMappings::Priority)
                    .col(SamlRoleMappings::Id)
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Users::Table)
                    .add_column(ColumnDef::new(Users::SamlSubject).text().null())
                    .add_column(ColumnDef::new(Users::SamlProviderId).integer().null())
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("users_saml_unique")
                    .table(Users::Table)
                    .col(Users::SamlProviderId)
                    .col(Users::SamlSubject)
                    .unique()
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                Index::drop()
                    .name("users_saml_unique")
                    .table(Users::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Users::Table)
                    .drop_column(Users::SamlSubject)
                    .drop_column(Users::SamlProviderId)
                    .to_owned(),
            )
            .await?;

        manager
            .drop_table(Table::drop().table(SamlRoleMappings::Table).to_owned())
            .await?;

        manager
            .drop_table(Table::drop().table(SamlLoginStates::Table).to_owned())
            .await?;

        manager
            .drop_table(Table::drop().table(SamlProviders::Table).to_owned())
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum SamlProviders {
    Table,
    Id,
    Name,
    Template,
    SpEntityId,
    IdpEntityId,
    IdpSsoUrl,
    IdpX509Cert,
    IdpMetadataUrl,
    GroupAttribute,
    RoleAttribute,
    DefaultRole,
    EmailAttribute,
    JitProvisioning,
    Enabled,
    TrustIdpEmail,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum SamlLoginStates {
    Table,
    Id,
    RelayState,
    AuthnRequestId,
    ProviderId,
    ReturnTo,
    ExpiresAt,
    CreatedAt,
}

#[derive(DeriveIden)]
enum SamlRoleMappings {
    Table,
    Id,
    ProviderId,
    Priority,
    IdpGroup,
    Role,
    CreatedAt,
}

#[derive(DeriveIden)]
enum Users {
    Table,
    SamlSubject,
    SamlProviderId,
}
