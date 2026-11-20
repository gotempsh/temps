// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Second-factor verification state.
//!
//! - `user_mfa_guards` holds, per user, the highest TOTP time step ever
//!   accepted (so a code cannot be accepted twice), the consecutive failed
//!   second-factor attempts across every verification path, and a lockout
//!   deadline that applies regardless of the client address.
//! - `sessions.mfa_failed_attempts` counts wrong codes against one pending
//!   sign-in challenge so the challenge can be discarded after a few misses.
//!
//! Both are additive. The column default of `0` keeps an older binary that
//! omits it during a rolling upgrade inserting valid rows; the guard table is
//! only read by the new binary and starts empty, which means "no step
//! accepted yet, no failures".

use sea_orm_migration::prelude::*;

const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS user_mfa_guards (
    user_id INTEGER PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    last_totp_step BIGINT,
    failed_attempts INTEGER NOT NULL DEFAULT 0,
    locked_until TIMESTAMPTZ,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

ALTER TABLE sessions
ADD COLUMN IF NOT EXISTS mfa_failed_attempts INTEGER NOT NULL DEFAULT 0;
"#;

const DOWN_SQL: &str = r#"
ALTER TABLE sessions DROP COLUMN IF EXISTS mfa_failed_attempts;
DROP TABLE IF EXISTS user_mfa_guards;
"#;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP_SQL).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(DOWN_SQL)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{DOWN_SQL, UP_SQL};

    #[test]
    fn guard_rows_follow_their_user() {
        assert!(
            UP_SQL.contains("REFERENCES users(id) ON DELETE CASCADE"),
            "deleting a user must not leave orphaned verification state"
        );
    }

    #[test]
    fn challenge_counter_has_a_default_for_older_binaries() {
        assert!(
            UP_SQL.contains("mfa_failed_attempts INTEGER NOT NULL DEFAULT 0"),
            "an older binary omitting the column must still insert sessions"
        );
    }

    #[test]
    fn up_is_idempotent_and_down_reverses_it() {
        assert!(UP_SQL.contains("CREATE TABLE IF NOT EXISTS user_mfa_guards"));
        assert!(UP_SQL.contains("ADD COLUMN IF NOT EXISTS mfa_failed_attempts"));
        assert!(DOWN_SQL.contains("DROP TABLE IF EXISTS user_mfa_guards"));
        assert!(DOWN_SQL.contains("DROP COLUMN IF EXISTS mfa_failed_attempts"));
    }
}
