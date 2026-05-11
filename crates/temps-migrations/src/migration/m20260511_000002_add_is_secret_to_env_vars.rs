//! Adds `is_secret` to `env_vars`.
//!
//! When `is_secret = true` the value is treated as write-only:
//!   - API responses never return plaintext (the `value` field is masked / null).
//!   - Updates with a missing `value` keep the existing ciphertext.
//!   - The flag is one-way: once a row is marked secret it cannot be flipped
//!     back to a regular env var (prevents leaking by toggle).
//!
//! Storage is unchanged — the value is still AES-256-GCM encrypted at rest
//! via `EncryptionService` (same column, same `is_encrypted` flag); the new
//! flag only controls API/UI visibility.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(EnvVars::Table)
                    .add_column(
                        ColumnDef::new(EnvVars::IsSecret)
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
                    .table(EnvVars::Table)
                    .drop_column(EnvVars::IsSecret)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum EnvVars {
    Table,
    IsSecret,
}
