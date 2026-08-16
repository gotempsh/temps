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
//! Existing rows cannot be classified safely because the old schema did not
//! distinguish real sessions from MFA challenges. The migration therefore
//! revokes every existing session. The database default is `TRUE` so an older
//! binary participating in a rolling upgrade fails closed when it omits the
//! discriminator. The new binary writes `FALSE` explicitly only after full
//! authentication. Users must sign in again after the upgrade, but no old or
//! mixed-version challenge can be promoted into a real session.

use sea_orm_migration::prelude::*;

const UP_SQL: &str = r#"
ALTER TABLE sessions
ADD COLUMN IF NOT EXISTS mfa_pending BOOLEAN;

-- Old rows are ambiguous: they may be authenticated sessions or MFA
-- challenges. Revoking all of them is the only fail-closed backfill.
DELETE FROM sessions;

ALTER TABLE sessions
ALTER COLUMN mfa_pending SET DEFAULT TRUE;

ALTER TABLE sessions
ALTER COLUMN mfa_pending SET NOT NULL;
"#;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(UP_SQL).await?;

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

#[cfg(test)]
mod tests {
    use super::UP_SQL;

    #[test]
    fn revokes_ambiguous_sessions_before_setting_authenticated_default() {
        let delete_position = UP_SQL
            .find("DELETE FROM sessions")
            .expect("migration must revoke pre-upgrade sessions");
        let default_position = UP_SQL
            .find("ALTER COLUMN mfa_pending SET DEFAULT TRUE")
            .expect("migration must default omitted session purpose to MFA-pending");
        let not_null_position = UP_SQL
            .find("ALTER COLUMN mfa_pending SET NOT NULL")
            .expect("migration must make the discriminator mandatory");

        assert!(
            delete_position < default_position,
            "ambiguous rows must be revoked before enforcing the fail-closed default"
        );
        assert!(
            default_position < not_null_position,
            "the default must be established before enforcing NOT NULL"
        );
    }
}
