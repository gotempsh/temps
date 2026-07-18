//! Migration to drop the `magic_link_tokens` table.
//!
//! Magic-link login has been removed entirely: it had no first-party consumer
//! (the login screen never offered it, no component read its availability flag,
//! and no crate outside `temps-auth` called it) and was a live unauthenticated
//! login endpoint — pure attack surface. SSO/IdP-down account recovery is now
//! served by the password-reset flow, which can set an initial password for a
//! passwordless SSO account. This drops the now-unused table.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS magic_link_tokens")
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Recreate the table so the migration is reversible. Mirrors the
        // original definition from the initial schema migration.
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                CREATE TABLE IF NOT EXISTS magic_link_tokens (
                    id SERIAL PRIMARY KEY,
                    email VARCHAR NOT NULL,
                    token VARCHAR NOT NULL UNIQUE,
                    expires_at TIMESTAMPTZ NOT NULL,
                    used BOOLEAN NOT NULL,
                    created_at TIMESTAMPTZ NOT NULL
                )
                "#,
            )
            .await?;
        Ok(())
    }
}
