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
//!    stored list in the same transaction that accepts it. A value that is
//!    both a fresh TOTP code and an unused recovery code is accepted as the
//!    recovery code *and* records the TOTP step, so it cannot pass again
//!    through either route.
//! 3. **Wrong codes are throttled per user, not per address.** Consecutive
//!    failures are persisted in `user_mfa_guards`; past
//!    [`USER_LOCKOUT_THRESHOLD`] every attempt is refused without evaluation
//!    for an exponentially growing window, so rotating client addresses buys
//!    an attacker nothing.
//!
//! # Secret storage
//!
//! The TOTP seed in `users.mfa_secret` is encrypted at rest with the instance
//! [`temps_core::EncryptionService`] and stored as
//! [`MFA_SECRET_ENCRYPTED_PREFIX`]`<ciphertext>`. Rows written before that
//! change hold the bare base32 seed; they are still accepted (base32 never
//! contains `:`, so the two forms cannot be confused) and are re-encrypted in
//! place the next time the factor is accepted ([`record_success`]).
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

use std::sync::LazyLock;

use argon2::{Argon2, PasswordHash, PasswordVerifier};
use chrono::{DateTime, Duration, Utc};
use sea_orm::sea_query::OnConflict;
use sea_orm::{ActiveModelTrait, ConnectionTrait, DbErr, EntityTrait, Set};
use temps_core::EncryptionService;
use temps_entities::{user_mfa_guards, users};
use thiserror::Error;
use tokio::sync::Semaphore;
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

/// Recovery-code checks allowed to hash at once across the process. Each one
/// runs up to one Argon2 verification per stored code, so they run on the
/// blocking pool and queue (without holding a worker) beyond this limit.
const MAX_CONCURRENT_RECOVERY_CHECKS: usize = 2;

static RECOVERY_CHECK_SLOTS: LazyLock<Semaphore> =
    LazyLock::new(|| Semaphore::new(MAX_CONCURRENT_RECOVERY_CHECKS));

/// Marks an encrypted `users.mfa_secret` value. A legacy plaintext seed is
/// RFC 4648 base32 (`A-Z2-7=`) and can never start with this.
pub(crate) const MFA_SECRET_ENCRYPTED_PREFIX: &str = "enc:v1:";

#[derive(Debug, Error)]
pub(crate) enum MfaGuardError {
    #[error("Stored MFA secret for user {user_id} is unusable: {reason}")]
    InvalidSecret { user_id: i32, reason: String },
    #[error("Failed to encrypt the MFA secret for user {user_id}: {reason}")]
    SecretEncryption { user_id: i32, reason: String },
    #[error("Stored MFA recovery codes for user {user_id} are unusable: {reason}")]
    InvalidRecoveryCodes { user_id: i32, reason: String },
    #[error("Recovery-code check for user {user_id} could not run: {reason}")]
    VerifierUnavailable { user_id: i32, reason: String },
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
    /// `totp_step` is set when the same value was also a fresh TOTP code, so
    /// that step is recorded too.
    RecoveryCode {
        remaining: Vec<String>,
        totp_step: Option<u64>,
    },
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

/// Encrypt a base32 TOTP seed for storage in `users.mfa_secret`.
pub(crate) fn seal_mfa_secret(
    encryption: &EncryptionService,
    user_id: i32,
    secret: &str,
) -> Result<String, MfaGuardError> {
    let ciphertext =
        encryption
            .encrypt_string(secret)
            .map_err(|error| MfaGuardError::SecretEncryption {
                user_id,
                reason: error.to_string(),
            })?;
    Ok(format!("{MFA_SECRET_ENCRYPTED_PREFIX}{ciphertext}"))
}

/// Whether a stored `users.mfa_secret` value predates at-rest encryption.
pub(crate) fn is_legacy_plaintext_secret(stored: &str) -> bool {
    !stored.starts_with(MFA_SECRET_ENCRYPTED_PREFIX)
}

/// Recover the base32 TOTP seed from a stored `users.mfa_secret` value,
/// accepting both the encrypted form and a legacy plaintext seed.
pub(crate) fn open_mfa_secret(
    encryption: &EncryptionService,
    user_id: i32,
    stored: &str,
) -> Result<String, MfaGuardError> {
    match stored.strip_prefix(MFA_SECRET_ENCRYPTED_PREFIX) {
        Some(ciphertext) => {
            encryption
                .decrypt_string(ciphertext)
                .map_err(|error| MfaGuardError::InvalidSecret {
                    user_id,
                    reason: format!(
                    "stored secret cannot be decrypted with the instance encryption key: {error}"
                ),
                })
        }
        None => Ok(stored.to_string()),
    }
}

fn unix_seconds(now: DateTime<Utc>) -> u64 {
    u64::try_from(now.timestamp()).unwrap_or(0)
}

/// Decide whether `code` is an acceptable second factor for `user` right now.
///
/// A TOTP code only counts when its step is newer than `last_totp_step`; a
/// replayed code is rejected exactly like a wrong one, so the response never
/// distinguishes "reused" from "wrong". Recovery codes are only considered
/// when `allow_recovery_codes` is set (they are never valid for confirming a
/// fresh enrolment), and are always checked, even when the TOTP check
/// passed: legacy recovery codes are six digits too, and a value matching
/// both must burn the recovery code rather than leave it usable for a second
/// sign-in. Recovery codes are compared in [`normalize_recovery_code`] form;
/// the TOTP check uses the code exactly as typed.
///
/// Performs no writes. Persist the outcome with [`record_success`] or
/// [`record_failure`]. The recovery-code hashing runs on the blocking pool;
/// the caller's transaction and row lock stay held across the await.
pub(crate) async fn check_factor(
    encryption: &EncryptionService,
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
    let secret = open_mfa_secret(encryption, user.id, secret)?;
    let totp = build_totp(user.id, &secret)?;

    let fresh_totp_step = totp.check(code, unix_seconds(now)).filter(|step| {
        match (last_totp_step, i64::try_from(*step)) {
            (_, Err(_)) => false,
            (None, Ok(_)) => true,
            (Some(last), Ok(step)) => step > last,
        }
    });

    let recovery = if allow_recovery_codes {
        match_recovery_code(user, code).await?
    } else {
        None
    };

    Ok(match (recovery, fresh_totp_step) {
        (Some(remaining), totp_step) => Some(AcceptedFactor::RecoveryCode {
            remaining,
            totp_step,
        }),
        (None, Some(step)) => Some(AcceptedFactor::Totp { step }),
        (None, None) => None,
    })
}

/// Canonical form of a typed recovery code: separators (`-`, spaces) removed
/// and letters upper-cased, matching what [`generate_recovery_code`] hashes.
/// Legacy six-digit codes are unchanged by this, so their stored hashes keep
/// verifying.
pub(crate) fn normalize_recovery_code(code: &str) -> String {
    code.chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

/// Characters a recovery code is drawn from: upper-case letters and digits
/// without the visually ambiguous `0/O` and `1/I`. 32 symbols = 5 bits each.
pub(crate) const RECOVERY_CODE_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

/// Symbols per recovery code (50 bits of entropy with the alphabet above).
pub(crate) const RECOVERY_CODE_LENGTH: usize = 10;

/// Longest normalized input that is compared against stored recovery codes.
const MAX_RECOVERY_CODE_INPUT_LENGTH: usize = 64;

/// A new recovery code as `(display, canonical)`: the display form is grouped
/// as `XXXXX-XXXXX` for reading off a screen; the canonical form (what gets
/// hashed and what [`normalize_recovery_code`] yields for the typed code) has
/// no separator.
pub(crate) fn generate_recovery_code() -> (String, String) {
    use rand::RngExt;
    let mut rng = rand::rng();
    let canonical: String = (0..RECOVERY_CODE_LENGTH)
        .map(|_| {
            char::from(RECOVERY_CODE_ALPHABET[rng.random_range(0..RECOVERY_CODE_ALPHABET.len())])
        })
        .collect();
    let (head, tail) = canonical.split_at(RECOVERY_CODE_LENGTH / 2);
    (format!("{head}-{tail}"), canonical)
}

/// The stored recovery-code hashes without the one `code` matches, or `None`
/// when it matches none of them.
async fn match_recovery_code(
    user: &users::Model,
    code: &str,
) -> Result<Option<Vec<String>>, MfaGuardError> {
    let user_id = user.id;
    let Some(stored) = user.mfa_recovery_codes.as_deref() else {
        return Ok(None);
    };
    let hashes: Vec<String> =
        serde_json::from_str(stored).map_err(|error| MfaGuardError::InvalidRecoveryCodes {
            user_id,
            reason: error.to_string(),
        })?;
    if hashes.is_empty() {
        return Ok(None);
    }

    let slot =
        RECOVERY_CHECK_SLOTS
            .acquire()
            .await
            .map_err(|_| MfaGuardError::VerifierUnavailable {
                user_id,
                reason: "the recovery-code check limiter is closed".to_string(),
            })?;
    let code = normalize_recovery_code(code);
    // No issued code (legacy six digits, current ten symbols) is anywhere
    // near this long; refusing early keeps oversized input away from Argon2.
    if code.is_empty() || code.len() > MAX_RECOVERY_CODE_INPUT_LENGTH {
        return Ok(None);
    }
    tokio::task::spawn_blocking(move || {
        let _slot = slot;
        let argon2 = Argon2::default();
        for (index, hash) in hashes.iter().enumerate() {
            let parsed =
                PasswordHash::new(hash).map_err(|error| MfaGuardError::InvalidRecoveryCodes {
                    user_id,
                    reason: format!("recovery code hash {index} cannot be parsed: {error}"),
                })?;
            if argon2.verify_password(code.as_bytes(), &parsed).is_ok() {
                let mut remaining = hashes.clone();
                remaining.remove(index);
                return Ok(Some(remaining));
            }
        }
        Ok(None)
    })
    .await
    .map_err(|error| MfaGuardError::VerifierUnavailable {
        user_id,
        reason: format!("the recovery-code check worker failed: {error}"),
    })?
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
    encryption: &EncryptionService,
    user: users::Model,
    guard: &GuardState,
    factor: &AcceptedFactor,
    now: DateTime<Utc>,
) -> Result<users::Model, MfaGuardError> {
    let user_id = user.id;
    let accepted_step = match factor {
        AcceptedFactor::Totp { step } => Some(*step),
        AcceptedFactor::RecoveryCode { totp_step, .. } => *totp_step,
    };
    let last_totp_step = match accepted_step {
        Some(step) => {
            let step = i64::try_from(step).unwrap_or(i64::MAX);
            Some(guard.last_totp_step.map_or(step, |last| last.max(step)))
        }
        None => guard.last_totp_step,
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

    let remaining_codes = match factor {
        AcceptedFactor::Totp { .. } => None,
        AcceptedFactor::RecoveryCode { remaining, .. } => {
            Some(serde_json::to_string(remaining).map_err(|error| {
                MfaGuardError::InvalidRecoveryCodes {
                    user_id,
                    reason: format!("remaining recovery codes cannot be encoded: {error}"),
                }
            })?)
        }
    };
    let upgraded_secret = upgrade_legacy_secret(encryption, &user);
    if remaining_codes.is_none() && upgraded_secret.is_none() {
        return Ok(user);
    }

    let operation = if remaining_codes.is_some() {
        "consume the used recovery code"
    } else {
        "encrypt the legacy plaintext MFA secret"
    };
    let mut update: users::ActiveModel = user.into();
    if let Some(encoded) = remaining_codes {
        update.mfa_recovery_codes = Set(Some(encoded));
    }
    if let Some(sealed) = upgraded_secret {
        update.mfa_secret = Set(Some(sealed));
    }
    update
        .update(conn)
        .await
        .map_err(|source| MfaGuardError::Database {
            operation,
            user_id,
            source,
        })
}

/// The encrypted replacement for a legacy plaintext seed, or `None` when the
/// stored value is already encrypted (or absent). Runs only after the factor
/// was accepted, so the seed is known-good. An encryption failure is logged
/// and leaves the row as it was: the verification itself succeeded and must
/// not be failed by an opportunistic upgrade.
fn upgrade_legacy_secret(encryption: &EncryptionService, user: &users::Model) -> Option<String> {
    let stored = user.mfa_secret.as_deref()?;
    if !is_legacy_plaintext_secret(stored) {
        return None;
    }
    match seal_mfa_secret(encryption, user.id, stored) {
        Ok(sealed) => {
            tracing::info!(
                user_id = user.id,
                "Encrypting legacy plaintext MFA secret after a successful verification"
            );
            Some(sealed)
        }
        Err(error) => {
            tracing::warn!(
                user_id = user.id,
                %error,
                "Leaving legacy plaintext MFA secret in place"
            );
            None
        }
    }
}

/// Forget the last accepted TOTP step because a new secret is being
/// installed. Steps counted against the old secret say nothing about the new
/// one, and keeping them would reject the new authenticator's codes until
/// the clock passes the old watermark. Failure counters and any lockout are
/// kept. Caller holds the `users` row lock.
pub(crate) async fn forget_totp_step<C: ConnectionTrait>(
    conn: &C,
    user_id: i32,
) -> Result<(), MfaGuardError> {
    use sea_orm::sea_query::Expr;
    use sea_orm::{ColumnTrait, QueryFilter};
    user_mfa_guards::Entity::update_many()
        .col_expr(
            user_mfa_guards::Column::LastTotpStep,
            Expr::value(Option::<i64>::None),
        )
        .col_expr(user_mfa_guards::Column::UpdatedAt, Expr::value(Utc::now()))
        .filter(user_mfa_guards::Column::UserId.eq(user_id))
        .exec(conn)
        .await
        .map_err(|source| MfaGuardError::Database {
            operation: "reset the accepted TOTP step for a replaced secret",
            user_id,
            source,
        })?;
    Ok(())
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

    /// An instance encryption service with a fixed test key.
    pub(crate) fn encryption() -> std::sync::Arc<EncryptionService> {
        std::sync::Arc::new(
            EncryptionService::new(&"7a".repeat(32)).expect("fixed test key must be valid"),
        )
    }

    /// `secret` as `users.mfa_secret` stores it for a current enrolment
    /// (encrypted under [`encryption`]'s key).
    pub(crate) fn sealed(secret: &str) -> String {
        seal_mfa_secret(&encryption(), 0, secret).expect("test secret must seal")
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
    use super::test_support::{code_at, encryption, new_secret};
    use super::*;
    use argon2::PasswordHasher;

    fn enc() -> std::sync::Arc<EncryptionService> {
        encryption()
    }

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

    #[tokio::test]
    async fn totp_is_accepted_once_per_step() {
        let secret = new_secret();
        let user = user(&secret, None);
        let now = Utc::now();
        let code = code_at(&secret, now);

        let Some(AcceptedFactor::Totp { step }) =
            check_factor(&enc(), &user, None, &code, true, now)
                .await
                .unwrap()
        else {
            panic!("a fresh current code must be accepted");
        };
        let accepted = i64::try_from(step).unwrap();

        assert_eq!(
            check_factor(&enc(), &user, Some(accepted), &code, true, now)
                .await
                .unwrap(),
            None,
            "the same code must be rejected once its step was accepted"
        );
        assert_eq!(
            check_factor(&enc(), &user, Some(accepted + 5), &code, true, now)
                .await
                .unwrap(),
            None,
            "a code older than the last accepted step must be rejected"
        );
    }

    #[tokio::test]
    async fn next_step_is_accepted_after_previous_one() {
        let secret = new_secret();
        let user = user(&secret, None);
        let now = Utc::now();
        let first = check_factor(&enc(), &user, None, &code_at(&secret, now), false, now)
            .await
            .unwrap()
            .expect("current code accepted");
        let AcceptedFactor::Totp { step } = first else {
            panic!("expected TOTP");
        };
        let later = now + Duration::seconds(30);
        assert!(matches!(
            check_factor(
                &enc(),
                &user,
                Some(step as i64),
                &code_at(&secret, later),
                false,
                later
            )
            .await
            .unwrap(),
            Some(AcceptedFactor::Totp { .. })
        ));
    }

    #[tokio::test]
    async fn recovery_code_is_matched_and_removed_from_remaining() {
        let secret = new_secret();
        let user = user(&secret, Some(vec!["111111", "222222"]));
        let accepted = check_factor(&enc(), &user, None, "222222", true, Utc::now())
            .await
            .unwrap();
        let Some(AcceptedFactor::RecoveryCode {
            remaining,
            totp_step: None,
        }) = accepted
        else {
            panic!("recovery code must be accepted");
        };
        assert_eq!(remaining.len(), 1);
        let survivor = PasswordHash::new(&remaining[0]).unwrap();
        assert!(Argon2::default()
            .verify_password(b"111111", &survivor)
            .is_ok());
    }

    #[tokio::test]
    async fn a_value_that_is_both_factors_burns_the_recovery_code_and_the_step() {
        let secret = new_secret();
        let now = Utc::now();
        let code = code_at(&secret, now);
        let user = user(&secret, Some(vec![code.as_str(), "555555"]));

        let Some(AcceptedFactor::RecoveryCode {
            remaining,
            totp_step: Some(step),
        }) = check_factor(&enc(), &user, None, &code, true, now)
            .await
            .unwrap()
        else {
            panic!("a value matching both factors must be taken as the recovery code");
        };
        assert_eq!(remaining.len(), 1, "the matching recovery code is burned");

        // As the stored state stands after `record_success`: the code is gone
        // from the list and its step is recorded.
        let mut after = user.clone();
        after.mfa_recovery_codes = Some(serde_json::to_string(&remaining).unwrap());
        assert_eq!(
            check_factor(&enc(), &after, Some(step as i64), &code, true, now)
                .await
                .unwrap(),
            None,
            "the same value must not pass again as either factor"
        );
    }

    #[tokio::test]
    async fn recovery_codes_are_ignored_when_not_allowed() {
        let secret = new_secret();
        let user = user(&secret, Some(vec!["333333"]));
        assert_eq!(
            check_factor(&enc(), &user, None, "333333", false, Utc::now())
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn missing_secret_is_a_configuration_error() {
        let mut user = user(&new_secret(), None);
        user.mfa_secret = None;
        assert!(matches!(
            check_factor(&enc(), &user, None, "123456", true, Utc::now()).await,
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

    #[tokio::test]
    async fn encrypted_secret_round_trips_and_verifies_totp() {
        let secret = new_secret();
        let sealed = seal_mfa_secret(&enc(), 42, &secret).unwrap();
        assert!(sealed.starts_with(MFA_SECRET_ENCRYPTED_PREFIX));
        assert!(
            !sealed.contains(&secret),
            "the stored value must not contain the plaintext seed"
        );
        assert!(!is_legacy_plaintext_secret(&sealed));
        assert_eq!(open_mfa_secret(&enc(), 42, &sealed).unwrap(), secret);

        let user = user(&sealed, None);
        let now = Utc::now();
        assert!(matches!(
            check_factor(&enc(), &user, None, &code_at(&secret, now), false, now)
                .await
                .unwrap(),
            Some(AcceptedFactor::Totp { .. })
        ));
    }

    #[tokio::test]
    async fn legacy_plaintext_secret_is_still_accepted() {
        let secret = new_secret();
        assert!(is_legacy_plaintext_secret(&secret));
        assert_eq!(open_mfa_secret(&enc(), 42, &secret).unwrap(), secret);
        let user = user(&secret, None);
        let now = Utc::now();
        assert!(
            check_factor(&enc(), &user, None, &code_at(&secret, now), false, now)
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn secret_sealed_under_another_key_is_a_configuration_error() {
        let other = EncryptionService::new(&"5c".repeat(32)).unwrap();
        let sealed = seal_mfa_secret(&other, 42, &new_secret()).unwrap();
        let user = user(&sealed, None);
        assert!(matches!(
            check_factor(&enc(), &user, None, "123456", true, Utc::now()).await,
            Err(MfaGuardError::InvalidSecret { user_id: 42, .. })
        ));
    }

    #[test]
    fn only_legacy_plaintext_secrets_are_upgraded() {
        let secret = new_secret();
        let legacy = user(&secret, None);
        let upgraded = upgrade_legacy_secret(&enc(), &legacy).expect("plaintext must be sealed");
        assert_eq!(open_mfa_secret(&enc(), 42, &upgraded).unwrap(), secret);

        let already = user(&upgraded, None);
        assert_eq!(upgrade_legacy_secret(&enc(), &already), None);

        let mut absent = legacy.clone();
        absent.mfa_secret = None;
        assert_eq!(upgrade_legacy_secret(&enc(), &absent), None);
    }

    #[test]
    fn generated_recovery_codes_are_long_and_unambiguous() {
        for _ in 0..64 {
            let (display, canonical) = generate_recovery_code();
            assert_eq!(canonical.len(), RECOVERY_CODE_LENGTH);
            assert!(canonical
                .bytes()
                .all(|b| RECOVERY_CODE_ALPHABET.contains(&b)));
            assert_eq!(display.len(), RECOVERY_CODE_LENGTH + 1);
            assert_eq!(display.as_bytes()[RECOVERY_CODE_LENGTH / 2], b'-');
            assert_eq!(normalize_recovery_code(&display), canonical);
        }
        assert_eq!(RECOVERY_CODE_ALPHABET.len(), 32);
        for ambiguous in *b"0O1I" {
            assert!(!RECOVERY_CODE_ALPHABET.contains(&ambiguous));
        }
    }

    #[test]
    fn recovery_code_normalization_keeps_legacy_codes_unchanged() {
        assert_eq!(normalize_recovery_code("123456"), "123456");
        assert_eq!(normalize_recovery_code(" abcde-fgh23 "), "ABCDEFGH23");
        assert_eq!(normalize_recovery_code(" - "), "");
    }

    #[tokio::test]
    async fn new_format_recovery_code_matches_typed_variants() {
        let (display, canonical) = generate_recovery_code();
        let user = user(&new_secret(), Some(vec![canonical.as_str(), "777777"]));
        for typed in [
            display.clone(),
            canonical.clone(),
            display.to_ascii_lowercase(),
            format!(" {} ", display.replace('-', " ")),
        ] {
            assert!(
                matches!(
                    check_factor(&enc(), &user, None, &typed, true, Utc::now())
                        .await
                        .unwrap(),
                    Some(AcceptedFactor::RecoveryCode { .. })
                ),
                "typed form {typed:?} must match the stored code"
            );
        }
        assert_eq!(
            check_factor(&enc(), &user, None, "ABCDE-FGHJK", true, Utc::now())
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn legacy_six_digit_recovery_code_still_verifies() {
        let user = user(&new_secret(), Some(vec!["482913"]));
        assert!(matches!(
            check_factor(&enc(), &user, None, "482913", true, Utc::now())
                .await
                .unwrap(),
            Some(AcceptedFactor::RecoveryCode { .. })
        ));
    }
}
