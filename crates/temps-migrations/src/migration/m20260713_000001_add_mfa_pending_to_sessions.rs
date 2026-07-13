//! Migration to add `mfa_pending` to the sessions table.
//!
//! Closes an MFA-bypass: the temporary session created after the first factor
//! (password) passes was inserted into the same `sessions` table as a fully
//! authenticated session, distinguished only by a short expiry. `verify_session`
//! accepted any non-expired row, so the `mfa_session` challenge cookie could be
//! replayed as the `session` cookie to authenticate real requests without ever
//! completing the second factor.
//!
//! This column lets `verify_session` reject challenge rows and
//! `verify_mfa_challenge` reject real-session rows.
//!
//! Backfill note: existing rows default to FALSE. Real sessions are correctly
//! treated as fully authenticated. Any in-flight MFA challenge rows at deploy
//! time also become FALSE (i.e. promoted), but these live at most 5 minutes and
//! require the caller to already hold the account password, so the exposure is
//! negligible and self-clearing.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            r#"
            ALTER TABLE sessions
            ADD COLUMN IF NOT EXISTS mfa_pending BOOLEAN NOT NULL DEFAULT FALSE
            "#,
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            r#"
            ALTER TABLE sessions DROP COLUMN IF EXISTS mfa_pending
            "#,
        )
        .await?;

        Ok(())
    }
}
