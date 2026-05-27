//! Adds `trust_idp_email` to `oidc_providers`.
//!
//! When `trust_idp_email = true`, the resolver skips the `email_verified`
//! claim gate in `oidc_service::resolve_user`. This is only safe for IdPs
//! where an administrator controls every user account (corporate Okta,
//! Azure AD, internal SSO) and a self-signed `victim@example.com`
//! registration is not possible. For public/social IdPs that allow
//! self-signup (Auth0 social, Google consumer), leaving this `false`
//! prevents an account-takeover vector where an attacker registers the
//! victim's email at the IdP without verifying it.
//!
//! Default `false` preserves the existing security behaviour on upgrade
//! — admins must opt in per provider.

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
                        ColumnDef::new(OidcProviders::TrustIdpEmail)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(OidcProviders::Table)
                    .drop_column(OidcProviders::TrustIdpEmail)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum OidcProviders {
    Table,
    TrustIdpEmail,
}
