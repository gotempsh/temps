// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Records which login method (password, OIDC, SAML) created a pending
//! MFA-challenge `sessions` row.
//!
//! Every login flow that gates on `mfa_enabled` (password, OIDC, SAML)
//! calls the same `AuthService::create_mfa_session`, so the resulting row
//! was previously indistinguishable between a password-originated
//! challenge and an SSO-originated one. That ambiguity is what let SSO
//! enforcement's `POST /auth/verify-mfa` block correctly block a
//! password-originated challenge while incorrectly also blocking a user
//! who had already completed a legitimate OIDC/SAML login at their IdP,
//! permanently stranding MFA+SSO accounts. This column lets enforcement
//! tell the two cases apart.
//!
//! Nullable: fully authenticated sessions (`mfa_pending = false`) never
//! set this column.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("sessions"))
                    .add_column_if_not_exists(
                        ColumnDef::new(Alias::new("mfa_pending_origin"))
                            .string_len(32)
                            .null(),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("sessions"))
                    .drop_column(Alias::new("mfa_pending_origin"))
                    .to_owned(),
            )
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_name_is_stable() {
        assert_eq!(
            Migration.name(),
            "m20260907_000001_add_mfa_pending_origin_to_sessions"
        );
    }
}
