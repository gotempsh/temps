// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Second-factor verification shared by every MFA entry point: the sign-in
//! challenge, self-service enrolment, step-up, password change and MFA
//! disable.
//!
//! Three properties are enforced here so no entry point can drift from the
//! others:
//!
//! 1. **A TOTP code is accepted at most once.** The matched time-step counter
//!    must be strictly greater than the last one accepted for the user
//!    (RFC 6238 §5.2), and the new step is stored on success.
//! 2. **Recovery codes are single use.** A matching hash is removed from the
//!    stored list in the same transaction that accepts it.
//! 3. **Wrong codes are throttled per user, not per address.** Consecutive
//!    failures are persisted in `user_mfa_guards`; past
//!    [`USER_LOCKOUT_THRESHOLD`] every attempt is refused without evaluation
//!    for an exponentially growing window, so rotating client addresses buys
//!    an attacker nothing.
//!
//! # Locking contract
//!
//! Callers must hold the owning `users` row lock (`SELECT ... FOR UPDATE`) in
//! the transaction passed to these functions. That lock is what makes the
//! read-check-write on `user_mfa_guards` atomic across concurrent requests.
//!
//! # Failures must be committed
//!
//! A rejected code is only throttled if [`record_failure`]'s write survives.
//! Callers therefore commit the transaction on a wrong code and report the
//! rejection afterwards, rather than returning an error from inside the
//! transaction (which would roll the counter back and make every guess free).

use argon2::{Argon2, PasswordHash, PasswordVerifier};
use chrono::{DateTime, Duration, Utc};
use sea_orm::sea_query::OnConflict;
use sea_orm::{ActiveModelTrait, ConnectionTrait, DbErr, EntityTrait, Set};
use temps_entities::{user_mfa_guards, users};
use thiserror::Error;
use totp_rs::{Algorithm, Builder, Totp};

/// Wrong codes allowed against one pending sign-in challenge before it is
/// discarded and the user must present their password again.
pub(crate) const MAX_FAILED_ATTEMPTS_PER_CHALLENGE: i32 = 5;

/// Consecutive wrong codes (across all challenges and verification paths)
/// after which the user's second factor is locked.
pub(crate) const USER_LOCKOUT_THRESHOLD: i32 = 10;

/// First lockout window; doubles with every further failure once the
/// threshold is reached.
const USER_LOCKOUT_BASE_SECONDS: i64 = 5 * 60;

/// Upper bound for a single lockout window.
const USER_LOCKOUT_MAX_SECONDS: i64 = 60 * 60;

#[derive(Debug, Error)]
pub(crate) enum MfaGuardError {
    #[error("Stored MFA secret for user {user_id} is unusable: {reason}")]
    InvalidSecret { user_id: i32, reason: String },
    #[error("Stored MFA recovery codes for user {user_id} are unusable: {reason}")]
    InvalidRecoveryCodes { user_id: i32, reason: String },
    #[error("Failed to {operation} for user {user_id}: {source}")]
    Database {
        operation: &'static str,
        user_id: i32,
        #[source]
        source: DbErr,
    },
}

/// The factor that satisfied a verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AcceptedFactor {
    /// A TOTP code matching this time-step counter.
    Totp { step: u64 },
    /// A recovery code; `remaining` is the stored hash list without it.
    RecoveryCode { remaining: Vec<String> },
}

/// Snapshot of a user's `user_mfa_guards` row (defaults when absent).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct GuardState {
    pub last_totp_step: Option<i64>,
    pub failed_attempts: i32,
    pub locked_until: Option<DateTime<Utc>>,
}

impl GuardState {
    /// Seconds until the lockout ends, or `None` when not locked.
    pub fn lockout_remaining_seconds(&self, now: DateTime<Utc>) -> Option<i64> {
        self.locked_until
            .filter(|until| *until > now)
            .map(|until| (until - now).num_seconds().max(1))
    }
}

/// Build the verifier with the exact parameters used at enrolment
/// (`generate_mfa_setup_candidate`): SHA1, 6 digits, skew 1, 30 s step.
pub(crate) fn build_totp(user_id: i32, secret: &str) -> Result<Totp, MfaGuardError> {
    let decoded = base32::decode(base32::Alphabet::Rfc4648 { padding: true }, secret).ok_or(
        MfaGuardError::InvalidSecret {
            user_id,
            reason: "stored secret is not valid base32".to_string(),
        },
    )?;
    Builder::new()
        .with_algorithm(Algorithm::SHA1)
        .with_digits(6)
        .with_skew(1)
        .with_step_duration(30)
        .with_secret(decoded)
        .build()
        .map_err(|error| MfaGuardError::InvalidSecret {
            user_id,
            reason: format!("TOTP verifier cannot be initialized: {error}"),
        })
}

fn unix_seconds(now: DateTime<Utc>) -> u64 {
    u64::try_from(now.timestamp()).unwrap_or(0)
}

/// Decide whether `code` is an acceptable second factor for `user` right now.
///
/// TOTP is checked first and only counts when its step is newer than
/// `last_totp_step`; a replayed code falls through to the recovery-code
/// check and is otherwise rejected exactly like a wrong code, so the response
/// never distinguishes "reused" from "wrong". Recovery codes are only
/// considered when `allow_recovery_codes` is set (they are never valid for
/// confirming a fresh enrolment).
///
/// Pure: performs no writes. Persist the outcome with [`record_success`] or
/// [`record_failure`].
pub(crate) fn check_factor(
    user: &users::Model,
    last_totp_step: Option<i64>,
    code: &str,
    allow_recovery_codes: bool,
    now: DateTime<Utc>,
) -> Result<Option<AcceptedFactor>, MfaGuardError> {
    let secret = user
        .mfa_secret
        .as_deref()
        .ok_or(MfaGuardError::InvalidSecret {
            user_id: user.id,
            reason: "stored MFA secret is missing".to_string(),
        })?;
    let totp = build_totp(user.id, secret)?;

    if let Some(step) = totp.check(code, unix_seconds(now)) {
        let fresh = match (last_totp_step, i64::try_from(step)) {
            (_, Err(_)) => false,
            (None, Ok(_)) => true,
            (Some(last), Ok(step)) => step > last,
        };
        if fresh {
            return Ok(Some(AcceptedFactor::Totp { step }));
        }
    }

    if !allow_recovery_codes {
        return Ok(None);
    }
    let Some(stored) = user.mfa_recovery_codes.as_deref() else {
        return Ok(None);
    };
    let hashes: Vec<String> =
        serde_json::from_str(stored).map_err(|error| MfaGuardError::InvalidRecoveryCodes {
            user_id: user.id,
            reason: error.to_string(),
        })?;
    let argon2 = Argon2::default();
    for (index, hash) in hashes.iter().enumerate() {
        let parsed =
            PasswordHash::new(hash).map_err(|error| MfaGuardError::InvalidRecoveryCodes {
                user_id: user.id,
                reason: format!("recovery code hash {index} cannot be parsed: {error}"),
            })?;
        if argon2.verify_password(code.as_bytes(), &parsed).is_ok() {
            let mut remaining = hashes.clone();
            remaining.remove(index);
            return Ok(Some(AcceptedFactor::RecoveryCode { remaining }));
        }
    }
    Ok(None)
}

/// Load the guard row for `user_id`. Caller holds the `users` row lock.
pub(crate) async fn load_guard<C: ConnectionTrait>(
    conn: &C,
    user_id: i32,
) -> Result<GuardState, MfaGuardError> {
    let row = user_mfa_guards::Entity::find_by_id(user_id)
        .one(conn)
        .await
        .map_err(|source| MfaGuardError::Database {
            operation: "load second-factor guard state",
            user_id,
            source,
        })?;
    Ok(row
        .map(|row| GuardState {
            last_totp_step: row.last_totp_step,
            failed_attempts: row.failed_attempts,
            locked_until: row.locked_until,
        })
        .unwrap_or_default())
}

async fn store_guard<C: ConnectionTrait>(
    conn: &C,
    user_id: i32,
    state: &GuardState,
    now: DateTime<Utc>,
    operation: &'static str,
) -> Result<(), MfaGuardError> {
    let row = user_mfa_guards::ActiveModel {
        user_id: Set(user_id),
        last_totp_step: Set(state.last_totp_step),
        failed_attempts: Set(state.failed_attempts),
        locked_until: Set(state.locked_until),
        updated_at: Set(now),
    };
    user_mfa_guards::Entity::insert(row)
        .on_conflict(
            OnConflict::column(user_mfa_guards::Column::UserId)
                .update_columns([
                    user_mfa_guards::Column::LastTotpStep,
                    user_mfa_guards::Column::FailedAttempts,
                    user_mfa_guards::Column::LockedUntil,
                    user_mfa_guards::Column::UpdatedAt,
                ])
                .to_owned(),
        )
        .exec_without_returning(conn)
        .await
        .map_err(|source| MfaGuardError::Database {
            operation,
            user_id,
            source,
        })?;
    Ok(())
}

/// Persist an accepted factor: advance the TOTP step (never backwards),
/// clear the failure counter and any lockout, and burn a used recovery code.
/// Returns the user row as it now stands.
pub(crate) async fn record_success<C: ConnectionTrait>(
    conn: &C,
    user: users::Model,
    guard: &GuardState,
    factor: &AcceptedFactor,
    now: DateTime<Utc>,
) -> Result<users::Model, MfaGuardError> {
    let user_id = user.id;
    let last_totp_step = match factor {
        AcceptedFactor::Totp { step } => {
            let step = i64::try_from(*step).unwrap_or(i64::MAX);
            Some(guard.last_totp_step.map_or(step, |last| last.max(step)))
        }
        AcceptedFactor::RecoveryCode { .. } => guard.last_totp_step,
    };
    store_guard(
        conn,
        user_id,
        &GuardState {
            last_totp_step,
            failed_attempts: 0,
            locked_until: None,
        },
        now,
        "record an accepted second factor",
    )
    .await?;

    match factor {
        AcceptedFactor::Totp { .. } => Ok(user),
        AcceptedFactor::RecoveryCode { remaining } => {
            let encoded = serde_json::to_string(remaining).map_err(|error| {
                MfaGuardError::InvalidRecoveryCodes {
                    user_id,
                    reason: format!("remaining recovery codes cannot be encoded: {error}"),
                }
            })?;
            let mut update: users::ActiveModel = user.into();
            update.mfa_recovery_codes = Set(Some(encoded));
            update
                .update(conn)
                .await
                .map_err(|source| MfaGuardError::Database {
                    operation: "consume the used recovery code",
                    user_id,
                    source,
                })
        }
    }
}

/// Length of the lockout imposed after `failed_attempts` consecutive
/// failures, or `None` while still under the threshold.
pub(crate) fn lockout_duration(failed_attempts: i32) -> Option<Duration> {
    if failed_attempts < USER_LOCKOUT_THRESHOLD {
        return None;
    }
    let doublings = (failed_attempts - USER_LOCKOUT_THRESHOLD).clamp(0, 16) as u32;
    let seconds = USER_LOCKOUT_BASE_SECONDS
        .saturating_mul(1_i64 << doublings)
        .min(USER_LOCKOUT_MAX_SECONDS);
    Some(Duration::seconds(seconds))
}

/// Persist a rejected code and return the resulting state. Once the
/// consecutive failure count reaches [`USER_LOCKOUT_THRESHOLD`] the user's
/// second factor is locked for [`lockout_duration`].
pub(crate) async fn record_failure<C: ConnectionTrait>(
    conn: &C,
    user_id: i32,
    guard: &GuardState,
    now: DateTime<Utc>,
) -> Result<GuardState, MfaGuardError> {
    let failed_attempts = guard.failed_attempts.saturating_add(1);
    let next = GuardState {
        last_totp_step: guard.last_totp_step,
        failed_attempts,
        locked_until: lockout_duration(failed_attempts).map(|window| now + window),
    };
    store_guard(conn, user_id, &next, now, "record a rejected second factor").await?;
    if let Some(until) = next.locked_until {
        tracing::warn!(
            user_id,
            failed_attempts,
            locked_until = %until.to_rfc3339(),
            "Second-factor verification locked after repeated failures"
        );
    }
    Ok(next)
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    /// Code for the TOTP step containing `now`, using the enrolment params.
    pub(crate) fn code_at(secret: &str, now: DateTime<Utc>) -> String {
        build_totp(0, secret)
            .expect("test secret must be valid")
            .generate(unix_seconds(now))
            .to_string()
    }

    /// A fresh base32 secret of the size enrolment generates.
    pub(crate) fn new_secret() -> String {
        use rand::RngExt;
        let bytes: Vec<u8> = (0..20).map(|_| rand::rng().random::<u8>()).collect();
        base32::encode(base32::Alphabet::Rfc4648 { padding: true }, &bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{code_at, new_secret};
    use super::*;
    use argon2::PasswordHasher;

    fn user(secret: &str, recovery: Option<Vec<&str>>) -> users::Model {
        let now = Utc::now();
        let recovery = recovery.map(|codes| {
            let hashes: Vec<String> = codes
                .iter()
                .map(|code| {
                    Argon2::default()
                        .hash_password(code.as_bytes())
                        .unwrap()
                        .to_string()
                })
                .collect();
            serde_json::to_string(&hashes).unwrap()
        });
        users::Model {
            id: 42,
            name: "Guard".into(),
            email: "guard@example.com".into(),
            password_hash: None,
            email_verified: true,
            email_verification_token: None,
            email_verification_expires: None,
            password_reset_token: None,
            password_reset_expires: None,
            must_change_password: false,
            deleted_at: None,
            mfa_secret: Some(secret.to_string()),
            mfa_enabled: true,
            mfa_recovery_codes: recovery,
            oidc_subject: None,
            oidc_provider_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn totp_is_accepted_once_per_step() {
        let secret = new_secret();
        let user = user(&secret, None);
        let now = Utc::now();
        let code = code_at(&secret, now);

        let Some(AcceptedFactor::Totp { step }) =
            check_factor(&user, None, &code, true, now).unwrap()
        else {
            panic!("a fresh current code must be accepted");
        };
        let accepted = i64::try_from(step).unwrap();

        assert_eq!(
            check_factor(&user, Some(accepted), &code, true, now).unwrap(),
            None,
            "the same code must be rejected once its step was accepted"
        );
        assert_eq!(
            check_factor(&user, Some(accepted + 5), &code, true, now).unwrap(),
            None,
            "a code older than the last accepted step must be rejected"
        );
    }

    #[test]
    fn next_step_is_accepted_after_previous_one() {
        let secret = new_secret();
        let user = user(&secret, None);
        let now = Utc::now();
        let first = check_factor(&user, None, &code_at(&secret, now), false, now)
            .unwrap()
            .expect("current code accepted");
        let AcceptedFactor::Totp { step } = first else {
            panic!("expected TOTP");
        };
        let later = now + Duration::seconds(30);
        assert!(matches!(
            check_factor(
                &user,
                Some(step as i64),
                &code_at(&secret, later),
                false,
                later
            )
            .unwrap(),
            Some(AcceptedFactor::Totp { .. })
        ));
    }

    #[test]
    fn recovery_code_is_matched_and_removed_from_remaining() {
        let secret = new_secret();
        let user = user(&secret, Some(vec!["111111", "222222"]));
        let accepted = check_factor(&user, None, "222222", true, Utc::now()).unwrap();
        let Some(AcceptedFactor::RecoveryCode { remaining }) = accepted else {
            panic!("recovery code must be accepted");
        };
        assert_eq!(remaining.len(), 1);
        let survivor = PasswordHash::new(&remaining[0]).unwrap();
        assert!(Argon2::default()
            .verify_password(b"111111", &survivor)
            .is_ok());
    }

    #[test]
    fn recovery_codes_are_ignored_when_not_allowed() {
        let secret = new_secret();
        let user = user(&secret, Some(vec!["333333"]));
        assert_eq!(
            check_factor(&user, None, "333333", false, Utc::now()).unwrap(),
            None
        );
    }

    #[test]
    fn missing_secret_is_a_configuration_error() {
        let mut user = user(&new_secret(), None);
        user.mfa_secret = None;
        assert!(matches!(
            check_factor(&user, None, "123456", true, Utc::now()),
            Err(MfaGuardError::InvalidSecret { user_id: 42, .. })
        ));
    }

    #[test]
    fn lockout_starts_at_threshold_and_grows_to_a_cap() {
        assert_eq!(lockout_duration(USER_LOCKOUT_THRESHOLD - 1), None);
        assert_eq!(
            lockout_duration(USER_LOCKOUT_THRESHOLD),
            Some(Duration::seconds(USER_LOCKOUT_BASE_SECONDS))
        );
        assert_eq!(
            lockout_duration(USER_LOCKOUT_THRESHOLD + 1),
            Some(Duration::seconds(USER_LOCKOUT_BASE_SECONDS * 2))
        );
        assert_eq!(
            lockout_duration(USER_LOCKOUT_THRESHOLD + 50),
            Some(Duration::seconds(USER_LOCKOUT_MAX_SECONDS))
        );
    }

    #[test]
    fn lockout_remaining_only_reports_future_deadlines() {
        let now = Utc::now();
        let locked = GuardState {
            locked_until: Some(now + Duration::seconds(90)),
            ..Default::default()
        };
        assert_eq!(locked.lockout_remaining_seconds(now), Some(90));
        let expired = GuardState {
            locked_until: Some(now - Duration::seconds(1)),
            ..Default::default()
        };
        assert_eq!(expired.lockout_remaining_seconds(now), None);
    }
}
