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
//! # Admission is the count, in one statement, with no lock across Argon2
//!
//! [`crate::mfa_guard`] serializes its read-check-write on the `users` row
//! lock. A login cannot: verifying a password is tens of milliseconds of
//! Argon2, and holding a row lock and a pooled connection across it would
//! be a self-inflicted outage under load.
//!
//! So the attempt is *reserved* before the password is read, by the same
//! conditional `UPDATE` that counts it ([`reserve_attempt`]). Checking a
//! deadline and then counting separately cannot bound a burst — one short of
//! the threshold, every simultaneous request reads "not locked" and gets
//! evaluated before the first count lands. An `UPDATE` whose `WHERE` clause
//! excludes a throttled row is re-evaluated against the updated row after it
//! waits on the row lock, so concurrent attempts are admitted one at a time
//! and the one that reaches the threshold refuses the rest of the burst. The
//! statement completes before any Argon2 work begins.

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

/// Deliberately **not** convertible into `UserAuthError`: a guard failure
/// must never become a database error on the login path, because a database
/// that serves reads and rejects writes would then answer 500 for an address
/// that exists and 401 for one that does not. `login` turns every one of
/// these into the same refusal instead.
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

/// A user id no row can have, used to probe the guard for an email that does
/// not exist so the lookup costs the same either way.
///
/// `users.id` is a `serial`, which never issues a negative value.
const NO_SUCH_USER: i32 = -1;

/// Touch the guard table for an email that matched no account.
///
/// [`reserve_attempt`] cannot run here — the table's foreign key means there
/// is no row to reserve against — so this reads instead, keeping the work an
/// unknown address costs in the same shape as a real one. The result is
/// always `None`; only the lookup matters.
pub(crate) async fn probe_missing_account<C: ConnectionTrait>(
    conn: &C,
) -> Result<(), LoginGuardError> {
    let statement = Statement::from_sql_and_values(
        sea_orm::DatabaseBackend::Postgres,
        "SELECT locked_until FROM user_login_guards WHERE user_id = $1",
        [NO_SUCH_USER.into()],
    );
    ProbedGuard::find_by_statement(statement)
        .one(conn)
        .await
        .map(|_| ())
        .map_err(|source| LoginGuardError::Database {
            operation: "probe",
            user_id: NO_SUCH_USER,
            source,
        })
}

#[derive(Debug, FromQueryResult)]
struct ProbedGuard {
    #[allow(dead_code, reason = "selected only so the probe costs a real lookup")]
    locked_until: Option<DateTime<Utc>>,
}

/// What the guard decided about one password attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Admission {
    /// The attempt is counted and may be evaluated.
    Admitted { attempts: i32 },
    /// The account is in its backoff window, so the password is not
    /// evaluated at all. Nothing was counted, so whoever keeps sending
    /// attempts cannot push the deadline further out.
    Throttled { until: DateTime<Utc> },
}

#[derive(Debug, FromQueryResult)]
struct AdmittedAttempt {
    failed_attempts: i32,
}

#[derive(Debug, FromQueryResult)]
struct Deadline {
    locked_until: Option<DateTime<Utc>>,
}

/// Reserve one password attempt against `user_id`, **before** the password is
/// verified, and say whether it may proceed.
///
/// Counting after the fact cannot bound a burst: with the count one short of
/// the threshold, any number of simultaneous requests all read "not locked",
/// and every one of them gets its password evaluated before the first
/// increment lands. So the increment *is* the admission, and the condition
/// that admits it rides on the same `UPDATE`:
///
/// * the `WHERE` clause matches only a row that is not currently throttled;
/// * under `READ COMMITTED` a concurrent `UPDATE` waits on the row lock and
///   then re-evaluates its `WHERE` against the *updated* row, so once one
///   attempt has pushed the count to the threshold and set the deadline, the
///   rest of the burst matches nothing and is refused;
/// * a row that matched is returned, so "admitted" and "refused" are the
///   presence or absence of a row rather than a second read.
///
/// The statement finishes before any Argon2 work starts, so no row lock or
/// pooled connection is held across the verification.
///
/// A refused attempt is never counted and never rewrites the deadline, so a
/// flood of them cannot push somebody else's lockout further out.
pub(crate) async fn reserve_attempt<C: ConnectionTrait>(
    conn: &C,
    user_id: i32,
) -> Result<Admission, LoginGuardError> {
    // The gate below can only admit an existing row, so make sure there is
    // one. Idempotent, and it leaves an existing row untouched.
    let ensure = Statement::from_sql_and_values(
        sea_orm::DatabaseBackend::Postgres,
        "INSERT INTO user_login_guards (user_id) VALUES ($1) ON CONFLICT (user_id) DO NOTHING",
        [user_id.into()],
    );
    conn.execute(ensure)
        .await
        .map_err(|source| LoginGuardError::Database {
            operation: "open a throttle record for",
            user_id,
            source,
        })?;

    // `power(2, ...)` is capped so neither the exponent nor the interval can
    // run away after a long guessing run. An expired window matches too, so
    // the account comes back on its own; the count carries over, which is
    // what makes the next window longer until a success clears it.
    let gate = format!(
        "UPDATE user_login_guards SET \
             failed_attempts = LEAST(failed_attempts + 1, {max_attempts}), \
             locked_until = CASE \
                 WHEN failed_attempts + 1 >= {threshold} \
                 THEN now() + make_interval(secs => LEAST( \
                     {max_seconds}::double precision, \
                     {base_seconds}::double precision * power(2, LEAST( \
                         failed_attempts + 1 - {threshold}, \
                         {max_doublings} \
                     )) \
                 )) \
                 ELSE NULL \
             END, \
             updated_at = now() \
         WHERE user_id = $1 AND (locked_until IS NULL OR locked_until <= now()) \
         RETURNING failed_attempts",
        max_attempts = MAX_RECORDED_ATTEMPTS,
        threshold = LOCKOUT_THRESHOLD,
        max_seconds = LOCKOUT_MAX_SECONDS,
        base_seconds = LOCKOUT_BASE_SECONDS,
        max_doublings = LOCKOUT_MAX_DOUBLINGS,
    );
    let admitted = AdmittedAttempt::find_by_statement(Statement::from_sql_and_values(
        sea_orm::DatabaseBackend::Postgres,
        &gate,
        [user_id.into()],
    ))
    .one(conn)
    .await
    .map_err(|source| LoginGuardError::Database {
        operation: "reserve a password attempt against",
        user_id,
        source,
    })?;
    if let Some(admitted) = admitted {
        return Ok(Admission::Admitted {
            attempts: admitted.failed_attempts,
        });
    }

    // No row matched, so the account is inside its window. Read the deadline
    // only to report it; nothing is written on this path.
    let deadline = Deadline::find_by_statement(Statement::from_sql_and_values(
        sea_orm::DatabaseBackend::Postgres,
        "SELECT locked_until FROM user_login_guards WHERE user_id = $1",
        [user_id.into()],
    ))
    .one(conn)
    .await
    .map_err(|source| LoginGuardError::Database {
        operation: "read the throttle deadline of",
        user_id,
        source,
    })?
    .and_then(|row| row.locked_until);
    // A window that expired between the two statements refuses this one
    // attempt rather than admitting an uncounted one: failing closed costs a
    // single retry, the other way costs a free guess.
    Ok(Admission::Throttled {
        until: deadline.unwrap_or_else(Utc::now),
    })
}

/// Clear `user_id`'s throttle after a password was accepted.
///
/// This also releases the reservation [`reserve_attempt`] made for the
/// accepted attempt, which is why an ordinary login writes here rather than
/// leaving a count of one behind.
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
