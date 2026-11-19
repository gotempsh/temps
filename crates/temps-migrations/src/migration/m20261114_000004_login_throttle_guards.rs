// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Per-account password-login throttling state.
//!
//! `/auth/login` was limited per client address only, so guessing one
//! account's password from many addresses was not slowed at all.
//! `user_login_guards` holds, per user, the consecutive failed password
//! attempts and a backoff deadline that applies regardless of where the
//! attempt came from. It belongs in the database rather than in process
//! memory so the count survives a restart and is shared by every
//! control-plane process.
//!
//! The same shape as `user_mfa_guards` (m20261019_000001), for the same
//! reason: keeping the bookkeeping out of `users` means it does not ride
//! along on every `users::Model` read and write.
//!
//! Additive and lock-free against a populated database: a new table with no
//! backfill, no index built on an existing one, and nothing altered. An empty
//! table means "no failures recorded", so an older binary during a rolling
//! upgrade simply does not throttle, and a newer one starts counting.

use sea_orm_migration::prelude::*;

const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS user_login_guards (
    user_id INTEGER PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    failed_attempts INTEGER NOT NULL DEFAULT 0,
    locked_until TIMESTAMPTZ,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
"#;

const DOWN_SQL: &str = r#"
DROP TABLE IF EXISTS user_login_guards;
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
            "deleting a user must not leave orphaned throttling state"
        );
    }

    #[test]
    fn an_account_with_no_recorded_failures_is_not_throttled() {
        assert!(
            UP_SQL.contains("failed_attempts INTEGER NOT NULL DEFAULT 0"),
            "a fresh guard row must mean no failures, not an immediate lockout"
        );
        assert!(
            UP_SQL.contains("locked_until TIMESTAMPTZ"),
            "the backoff deadline must be nullable so absent means not throttled"
        );
    }

    #[test]
    fn nothing_existing_is_altered_or_backfilled() {
        // A populated database must not take a long lock for this upgrade.
        for forbidden in ["ALTER TABLE", "UPDATE ", "CREATE INDEX"] {
            assert!(!UP_SQL.contains(forbidden), "{forbidden}");
        }
    }

    #[test]
    fn up_is_idempotent_and_down_reverses_it() {
        assert!(UP_SQL.contains("CREATE TABLE IF NOT EXISTS user_login_guards"));
        assert!(DOWN_SQL.contains("DROP TABLE IF EXISTS user_login_guards"));
    }
}
