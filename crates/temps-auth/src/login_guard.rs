// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Per-account throttling for password logins.
//!
//! `/auth/login` is rate-limited per client address
//! ([`crate::rate_limit`]), which slows one address guessing many accounts
//! but does nothing about many addresses guessing one account — the shape
//! every credential-stuffing run has. This module adds the other axis:
//! consecutive rejected password attempts are counted against the *account*
//! in `user_login_guards`, and past [`LOCKOUT_THRESHOLD`] every attempt is
//! refused without evaluating the password for a growing window.
//!
//! Modelled on [`crate::mfa_guard`], with three deliberate differences.
//!
//! # No account-existence oracle
//!
//! A throttled account answers exactly like a wrong password and like an
//! unknown email: the same `InvalidCredentials`, and the same single Argon2
//! verification, because the caller still runs
//! `verify_against_dummy_hash`. Nothing in the status, the body or the time
//! taken says whether the account exists or is being throttled. That is also
//! why a failed throttle *write* must never surface as an error: a 500 for a
//! real account next to a 401 for an unknown one would be the oracle this
//! exists to avoid.
//!
//! # Short, bounded backoff — never a lockout to be cleared by hand
//!
//! Anyone can send a wrong password for a known address, so a hard lockout
//! would be a cheap way to keep an administrator out. The window starts at
//! [`LOCKOUT_BASE_SECONDS`], doubles, and stops at
//! [`LOCKOUT_MAX_SECONDS`]; a successful password resets the count to zero.
//! At the cap an attacker costs the account holder a few minutes' wait,
//! while cutting a distributed guessing run to a handful of attempts an
//! hour. SSO and password reset are unaffected either way.
//!
//! # Counted in one statement, with no lock held across Argon2
//!
//! [`crate::mfa_guard`] serializes its read-check-write on the `users` row
//! lock. A login cannot: verifying a password is tens of milliseconds of
//! Argon2, and holding a row lock and a pooled connection across it would
//! be a self-inflicted outage under load. Instead the whole increment —
//! including the new deadline — is derived from the stored row inside one
//! `INSERT ... ON CONFLICT DO UPDATE`, so concurrent rejected attempts
//! cannot lose a count between them.

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DbErr, FromQueryResult, Statement};
use thiserror::Error;

/// Consecutive rejected password attempts after which an account is
/// throttled. Generous enough that a person fixing a typo never meets it.
pub(crate) const LOCKOUT_THRESHOLD: i32 = 10;

/// First backoff window, in seconds; doubles with every further rejection
/// once the threshold is reached.
pub(crate) const LOCKOUT_BASE_SECONDS: i64 = 60;

/// Upper bound for a single backoff window, in seconds.
pub(crate) const LOCKOUT_MAX_SECONDS: i64 = 15 * 60;

/// How many doublings the window may grow by. Also what keeps the exponent
/// bounded in SQL.
const LOCKOUT_MAX_DOUBLINGS: i32 = 16;

/// Ceiling for the stored count, so a long-running guessing run cannot
/// overflow the column. Far above the threshold, so it changes no decision.
const MAX_RECORDED_ATTEMPTS: i32 = 1_000_000;

#[derive(Debug, Error)]
pub(crate) enum LoginGuardError {
    #[error("Failed to {operation} the login throttle for user {user_id}: {source}")]
    Database {
        operation: &'static str,
        user_id: i32,
        #[source]
        source: DbErr,
    },
}

/// The guard's only failure is the database not answering, which `login`
/// already reports as a database error (a 500, never an authentication
/// outcome).
impl From<LoginGuardError> for crate::auth_service::UserAuthError {
    fn from(error: LoginGuardError) -> Self {
        match error {
            LoginGuardError::Database { source, .. } => Self::DatabaseError(source),
        }
    }
}

/// A user id no row can have, used to read the guard for an email that does
/// not exist so the lookup costs the same either way.
///
/// `users.id` is a `serial`, which never issues a negative value.
pub(crate) const NO_SUCH_USER: i32 = -1;

#[derive(Debug, FromQueryResult)]
struct LockedUntilRow {
    locked_until: Option<DateTime<Utc>>,
}

/// When `user_id`'s password login is throttled until, or `None`.
///
/// Call this for every attempt, including one whose email matched no row
/// ([`NO_SUCH_USER`]): doing the lookup only for accounts that exist would
/// make the response time say which is which.
pub(crate) async fn locked_until<C: ConnectionTrait>(
    conn: &C,
    user_id: i32,
) -> Result<Option<DateTime<Utc>>, LoginGuardError> {
    let statement = Statement::from_sql_and_values(
        sea_orm::DatabaseBackend::Postgres,
        "SELECT locked_until FROM user_login_guards \
         WHERE user_id = $1 AND locked_until > now()",
        [user_id.into()],
    );
    let row = LockedUntilRow::find_by_statement(statement)
        .one(conn)
        .await
        .map_err(|source| LoginGuardError::Database {
            operation: "read",
            user_id,
            source,
        })?;
    Ok(row.and_then(|row| row.locked_until))
}

#[derive(Debug, FromQueryResult)]
struct RecordedFailure {
    failed_attempts: i32,
    locked_until: Option<DateTime<Utc>>,
}

/// Count one rejected password attempt for `user_id` and return the state it
/// leaves behind.
///
/// The new count and the new deadline are both derived from the stored row
/// in a single statement, so two attempts arriving together cannot overwrite
/// each other's count.
pub(crate) async fn record_failure<C: ConnectionTrait>(
    conn: &C,
    user_id: i32,
) -> Result<(i32, Option<DateTime<Utc>>), LoginGuardError> {
    // `failed_attempts + 1` is the count this attempt produces, and it
    // decides the window. `power(2, ...)` is capped so neither the exponent
    // nor the interval can run away after a long guessing run.
    let sql = format!(
        "INSERT INTO user_login_guards (user_id, failed_attempts, locked_until, updated_at) \
         VALUES ($1, 1, NULL, now()) \
         ON CONFLICT (user_id) DO UPDATE SET \
             failed_attempts = LEAST(user_login_guards.failed_attempts + 1, {max_attempts}), \
             locked_until = CASE \
                 WHEN user_login_guards.failed_attempts + 1 >= {threshold} \
                 THEN now() + make_interval(secs => LEAST( \
                     {max_seconds}::double precision, \
                     {base_seconds}::double precision * power(2, LEAST( \
                         user_login_guards.failed_attempts + 1 - {threshold}, \
                         {max_doublings} \
                     )) \
                 )) \
                 ELSE NULL \
             END, \
             updated_at = now() \
         RETURNING failed_attempts, locked_until",
        max_attempts = MAX_RECORDED_ATTEMPTS,
        threshold = LOCKOUT_THRESHOLD,
        max_seconds = LOCKOUT_MAX_SECONDS,
        base_seconds = LOCKOUT_BASE_SECONDS,
        max_doublings = LOCKOUT_MAX_DOUBLINGS,
    );
    let statement =
        Statement::from_sql_and_values(sea_orm::DatabaseBackend::Postgres, &sql, [user_id.into()]);
    let recorded = RecordedFailure::find_by_statement(statement)
        .one(conn)
        .await
        .map_err(|source| LoginGuardError::Database {
            operation: "record a rejected password attempt against",
            user_id,
            source,
        })?
        .ok_or_else(|| LoginGuardError::Database {
            operation: "record a rejected password attempt against",
            user_id,
            source: DbErr::RecordNotInserted,
        })?;
    Ok((recorded.failed_attempts, recorded.locked_until))
}

/// Clear `user_id`'s throttle after a password was accepted.
///
/// Writes nothing when there is nothing to clear, which is every ordinary
/// login, so the common path stays read-only.
pub(crate) async fn record_success<C: ConnectionTrait>(
    conn: &C,
    user_id: i32,
) -> Result<(), LoginGuardError> {
    let statement = Statement::from_sql_and_values(
        sea_orm::DatabaseBackend::Postgres,
        "UPDATE user_login_guards \
         SET failed_attempts = 0, locked_until = NULL, updated_at = now() \
         WHERE user_id = $1 AND (failed_attempts <> 0 OR locked_until IS NOT NULL)",
        [user_id.into()],
    );
    conn.execute(statement)
        .await
        .map(|_| ())
        .map_err(|source| LoginGuardError::Database {
            operation: "clear",
            user_id,
            source,
        })
}

/// The policy these constants express, checked at compile time so it cannot
/// be tuned into something it should not be:
///
/// * the threshold leaves room for a person fixing a typo, or the throttle
///   becomes a support queue;
/// * the backoff is positive and capped, because anyone can send a wrong
///   password for a known address — the worst they may cost the account
///   holder is one bounded wait, never a lockout an administrator has to
///   clear by hand.
const _: () = {
    assert!(LOCKOUT_THRESHOLD >= 5);
    assert!(LOCKOUT_BASE_SECONDS > 0);
    assert!(LOCKOUT_MAX_SECONDS >= LOCKOUT_BASE_SECONDS);
    assert!(LOCKOUT_MAX_SECONDS <= 60 * 60);
    assert!(LOCKOUT_MAX_DOUBLINGS > 0);
    assert!(MAX_RECORDED_ATTEMPTS > LOCKOUT_THRESHOLD);
};
