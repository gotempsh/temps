// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Three credentials move to a digest at rest, joining the API keys,
//! deployment tokens and node tokens that already store one.
//!
//! What the threat is: not reading a token off the wire, but reading the
//! *column* — a database dump, a backup, a read-only replica, or a query
//! that returns more than it meant to. A column holding a digest cannot be
//! replayed as a credential.
//!
//! Each of the three gets the treatment its lifetime allows:
//!
//! * **`sessions.session_token`** is hashed **in place**, so every signed-in
//!   user stays signed in. The digest is computed in the database with the
//!   built-in `sha256()` over the UTF-8 bytes, which is byte-identical to
//!   `temps_core::crypto::sha256_hex` — `encode(sha256(convert_to(t,
//!   'UTF8')), 'hex')`. No extension is required.
//! * **`project_agents.webhook_token`** is hashed in place too: an existing
//!   webhook integration keeps working with the token its operator already
//!   configured, which matters because the console never showed that token
//!   in full and so could not help anyone reconfigure.
//! * **`users.password_reset_token`** is *cleared*. These are single-use and
//!   live an hour (fifteen minutes for a forced first-login change), so
//!   invalidating the few outstanding ones costs one more "forgot password"
//!   click, and clearing is strictly safer than carrying them over.
//!
//! On a populated database this is three single-pass updates over small
//! tables — `sessions` holds one row per signed-in browser, `project_agents`
//! one row per configured agent, and only the handful of `users` rows with a
//! live reset token are touched. No table is rewritten, no index is built,
//! no column is altered, so no long lock is taken.
//!
//! `down` cannot undo a digest. It revokes every session and clears every
//! webhook token instead, which is the honest reverse: everyone signs in
//! again and webhooks are reconfigured, and nothing is left that an older
//! binary would compare against the wrong form.

use sea_orm_migration::prelude::*;

const UP_SQL: &str = r#"
-- Users stay signed in: the cookie still carries the token, and the row now
-- carries its digest.
UPDATE sessions
SET session_token = encode(sha256(convert_to(session_token, 'UTF8')), 'hex');

-- Configured webhooks keep working with the token their caller already has.
UPDATE project_agents
SET webhook_token = encode(sha256(convert_to(webhook_token, 'UTF8')), 'hex')
WHERE webhook_token IS NOT NULL;

-- Outstanding password-reset and first-login credentials are single-use and
-- short-lived; invalidate them rather than carry them across the change.
UPDATE users
SET password_reset_token = NULL,
    password_reset_expires = NULL
WHERE password_reset_token IS NOT NULL
   OR password_reset_expires IS NOT NULL;
"#;

const DOWN_SQL: &str = r#"
-- A digest cannot be turned back into the token it came from. Revoking is
-- the only honest reverse: sign everyone out rather than leave rows an older
-- binary would compare against a plaintext token and never match.
DELETE FROM sessions;

UPDATE project_agents
SET webhook_token = NULL
WHERE webhook_token IS NOT NULL;
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
    fn the_digest_matches_what_the_application_computes() {
        // `temps_core::crypto::sha256_hex` is lowercase hex of SHA-256 over
        // the value's UTF-8 bytes. Both halves have to agree exactly or
        // every stored credential stops matching, so pin the expression.
        assert!(
            UP_SQL.contains("encode(sha256(convert_to(session_token, 'UTF8')), 'hex')"),
            "{UP_SQL}"
        );
        assert!(
            UP_SQL.contains("encode(sha256(convert_to(webhook_token, 'UTF8')), 'hex')"),
            "{UP_SQL}"
        );
        // `sha256()` is built in; requiring pgcrypto's `digest()` would make
        // the upgrade fail wherever the extension is absent.
        assert!(!UP_SQL.contains("digest("), "{UP_SQL}");
    }

    #[test]
    fn sessions_and_webhooks_survive_but_reset_tokens_do_not() {
        // Hashing in place keeps a signed-in browser signed in and a
        // configured webhook working.
        assert!(!UP_SQL.contains("DELETE FROM sessions"), "{UP_SQL}");
        assert!(UP_SQL.contains("UPDATE sessions"), "{UP_SQL}");
        assert!(UP_SQL.contains("UPDATE project_agents"), "{UP_SQL}");
        // Reset credentials are single-use and short-lived, so they are
        // invalidated rather than carried over.
        assert!(
            UP_SQL.contains("SET password_reset_token = NULL"),
            "{UP_SQL}"
        );
        assert!(
            UP_SQL.contains("password_reset_expires = NULL"),
            "{UP_SQL}"
        );
    }

    #[test]
    fn nothing_takes_a_schema_lock_or_builds_an_index() {
        for forbidden in ["ALTER TABLE", "CREATE INDEX", "VACUUM", "CLUSTER"] {
            assert!(!UP_SQL.contains(forbidden), "{forbidden}");
            assert!(!DOWN_SQL.contains(forbidden), "{forbidden}");
        }
    }

    #[test]
    fn down_leaves_nothing_an_older_binary_would_mismatch() {
        assert!(DOWN_SQL.contains("DELETE FROM sessions"), "{DOWN_SQL}");
        assert!(
            DOWN_SQL.contains("SET webhook_token = NULL"),
            "{DOWN_SQL}"
        );
    }
}
