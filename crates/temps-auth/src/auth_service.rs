// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::email_templates::AuthEmailService;
use argon2::{PasswordHasher, PasswordVerifier};
use axum::http::header::SET_COOKIE;
use axum::http::HeaderMap;
use chrono::{Duration, Utc};
use cookie::Cookie;
use rand::RngExt;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter,
    QuerySelect, Set, TransactionTrait,
};
use serde::Serialize;
use std::sync::Arc;
use temps_core::notifications::DynNotificationService;
use thiserror::Error;
use tracing::{debug, error, info, warn};
use uuid::Uuid;
const DEFAULT_EXTERNAL_URL: &str = "http://localhost:8000";
#[derive(Serialize)]
pub struct AuthStatusResponse {
    pub status: String,
    pub cli_token: Option<String>,
}

#[derive(Debug, Clone)]
pub struct VerifiedSession {
    pub user: temps_entities::users::Model,
    pub session_id: i32,
}

#[derive(Error, Debug)]
pub enum AuthError {
    #[error("Database error: {reason}")]
    DatabaseError { reason: String },
    #[error("Database error: {0}")]
    DatabaseConnectionError(String),
    #[error("GitHub API error: {0}")]
    GithubApiError(String),
    #[error("Encryption error: {0}")]
    EncryptionError(String),
    #[error("Decryption error: {0}")]
    DecryptionError(String),
    #[error("Reqwest error: {0}")]
    ReqwestError(String),
    #[error("Authentication error: {0}")]
    AuthenticationError(String),
    #[error("Unauthorized: {0}")]
    Unauthorized(String),
    #[error("Invalid input: {0}")]
    InvalidInput(String),
    #[error("Not found: {0}")]
    NotFound(String),
    #[error("Conflict: {0}")]
    Conflict(String),
    #[error("Internal server error: {0}")]
    InternalServerError(String),
    #[error("Generic error: {0}")]
    GenericError(String),

    // Password-change-specific errors. Distinct variants so the handler can
    // map each one to a meaningful HTTP status / problem type instead of
    // collapsing everything to 500.
    #[error("Current password is incorrect")]
    InvalidCurrentPassword,
    #[error("MFA code is required for this account")]
    MfaCodeRequired,
    #[error("MFA code is invalid")]
    InvalidMfaCode,
    #[error(
        "Second-factor verification for user {user_id} is locked for {retry_after_seconds}s after repeated failures"
    )]
    MfaLockedOut {
        user_id: i32,
        retry_after_seconds: i64,
    },
    #[error("New password must differ from the current password")]
    SamePassword,
    #[error("Password complexity check failed: {0}")]
    WeakPassword(String),
    #[error("Account has no password set (likely an SSO-only user)")]
    NoPasswordSet,
    #[error("User {user_id} must change their password before a session can be created")]
    PasswordChangeRequired { user_id: i32 },
    #[error(
        "User {user_id} enabled MFA after the first factor was checked; a session needs the second factor"
    )]
    MfaRequired { user_id: i32 },
}

/// Whether the caller of [`AuthService::create_session_in_transaction`] has
/// already verified the user's second factor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SecondFactor {
    /// Only the first factor (password or IdP) was checked. Refused if the
    /// user has MFA enabled by the time the session row is written.
    NotPresented,
    /// An MFA challenge was completed for this sign-in.
    Verified,
}

#[derive(Error, Debug)]
pub enum MfaChallengeError {
    #[error("MFA challenge session is invalid or expired")]
    InvalidOrExpiredSession,
    #[error("MFA challenge references missing user {user_id}")]
    UserNotFound { user_id: i32 },
    #[error("MFA code is invalid for user {user_id}")]
    InvalidCode { user_id: i32 },
    #[error(
        "MFA challenge for user {user_id} was discarded after too many wrong codes; the user must sign in again"
    )]
    ChallengeExhausted { user_id: i32 },
    #[error(
        "Second-factor verification for user {user_id} is locked for {retry_after_seconds}s after repeated failures"
    )]
    LockedOut {
        user_id: i32,
        retry_after_seconds: i64,
    },
    #[error("MFA verifier configuration is invalid for user {user_id}: {reason}")]
    VerifierConfiguration { user_id: i32, reason: String },
    #[error("MFA verification clock failed for user {user_id}: {reason}")]
    VerificationClock { user_id: i32, reason: String },
    #[error("MFA verifier could not run for user {user_id}: {reason}")]
    VerifierUnavailable { user_id: i32, reason: String },
    #[error("Failed to {operation} for MFA challenge (user {user_id:?}): {source}")]
    Database {
        operation: &'static str,
        user_id: Option<i32>,
        #[source]
        source: sea_orm::DbErr,
    },
}

impl MfaChallengeError {
    fn from_guard(error: crate::mfa_guard::MfaGuardError) -> Self {
        match error {
            crate::mfa_guard::MfaGuardError::InvalidSecret { user_id, reason }
            | crate::mfa_guard::MfaGuardError::InvalidRecoveryCodes { user_id, reason } => {
                MfaChallengeError::VerifierConfiguration { user_id, reason }
            }
            crate::mfa_guard::MfaGuardError::VerifierUnavailable { user_id, reason } => {
                MfaChallengeError::VerifierUnavailable { user_id, reason }
            }
            crate::mfa_guard::MfaGuardError::Database {
                operation,
                user_id,
                source,
            } => MfaChallengeError::Database {
                operation,
                user_id: Some(user_id),
                source,
            },
        }
    }
}

/// Result of one sign-in challenge transaction. Rejections are `Ok` values so
/// the transaction commits the failure counters before the caller reports
/// the rejection as an error.
enum ChallengeOutcome {
    Verified(Box<temps_entities::users::Model>, Option<String>),
    Rejected {
        user_id: i32,
    },
    /// The wrong code used up the challenge's attempts and the challenge was
    /// discarded; only a fresh sign-in can continue.
    Exhausted {
        user_id: i32,
    },
    Locked {
        user_id: i32,
        retry_after_seconds: i64,
    },
}

impl From<sea_orm::DbErr> for AuthError {
    fn from(error: sea_orm::DbErr) -> Self {
        match error {
            sea_orm::DbErr::RecordNotFound(_) => {
                AuthError::NotFound("Record not found".to_string())
            }
            _ => AuthError::DatabaseError {
                reason: error.to_string(),
            },
        }
    }
}

pub struct AuthService {
    db: Arc<DatabaseConnection>,
    email_service: AuthEmailService,
}

impl AuthService {
    pub fn new(db: Arc<DatabaseConnection>, notification_service: DynNotificationService) -> Self {
        let email_service = AuthEmailService::new(notification_service);
        Self { db, email_service }
    }

    pub async fn create_session(&self, user_id: i32) -> Result<String, AuthError> {
        let transaction = self.db.begin().await?;
        let token = self
            .create_session_in_transaction(user_id, None, SecondFactor::NotPresented, &transaction)
            .await?;
        transaction.commit().await?;
        Ok(token)
    }

    async fn create_session_in_transaction(
        &self,
        user_id: i32,
        issuer: Option<&str>,
        second_factor: SecondFactor,
        transaction: &sea_orm::DatabaseTransaction,
    ) -> Result<String, AuthError> {
        // The shared lock waits for any in-flight writer holding the row
        // exclusively (password reset, MFA enrolment) and then reads its
        // committed result, so the checks below see the user as it is now,
        // not as the caller saw it before verifying the first factor.
        let user = temps_entities::users::Entity::find_by_id(user_id)
            .filter(temps_entities::users::Column::DeletedAt.is_null())
            .lock_shared()
            .one(transaction)
            .await?
            .ok_or_else(|| AuthError::NotFound(format!("User {user_id} not found or deleted")))?;
        if user.must_change_password {
            return Err(AuthError::PasswordChangeRequired { user_id });
        }
        // MFA enrolment revokes every other session under the exclusive lock.
        // A first-factor sign-in that decided "no MFA" before that enrolment
        // committed must not write a full session after it.
        if second_factor == SecondFactor::NotPresented && user.mfa_enabled {
            return Err(AuthError::MfaRequired { user_id });
        }

        let session_token = self.generate_session_token();
        let expires_at = Utc::now() + Duration::days(7);

        let new_session = temps_entities::sessions::ActiveModel {
            user_id: Set(user_id),
            session_token: Set(session_token.clone()),
            expires_at: Set(expires_at),
            // Fully authenticated session, not an MFA challenge.
            mfa_pending: Set(false),
            mfa_pending_origin: Set(issuer.map(str::to_owned)),
            ..Default::default()
        };

        new_session.insert(transaction).await?;
        Ok(session_token)
    }

    /// Count this user's currently active (non-expired) sessions.
    ///
    /// Used by the login handlers to detect and audit-log concurrent
    /// sessions (bherila/temps#24) -- e.g. a second device/browser logging
    /// in while an earlier session for the same account is still live. This
    /// is purely observational: callers MUST call it *before* creating the
    /// new session (otherwise the session being created would always count
    /// itself), and a non-zero result must never block the login, only be
    /// recorded in the audit trail.
    pub async fn count_active_sessions(&self, user_id: i32) -> Result<u64, AuthError> {
        let count = temps_entities::sessions::Entity::find()
            .filter(temps_entities::sessions::Column::UserId.eq(user_id))
            .filter(temps_entities::sessions::Column::ExpiresAt.gt(Utc::now()))
            // Pending MFA challenge rows are not real sessions -- excluding
            // them keeps the concurrent-login audit signal honest.
            .filter(temps_entities::sessions::Column::MfaPending.eq(false))
            .count(self.db.as_ref())
            .await?;
        Ok(count)
    }

    pub async fn verify_session(
        &self,
        session_token: &str,
    ) -> Result<temps_entities::users::Model, AuthError> {
        Ok(self.verify_session_details(session_token).await?.user)
    }

    /// Verify a fully authenticated session and retain its database identity
    /// for session-scoped authorization such as short-lived step-up access.
    pub async fn verify_session_details(
        &self,
        session_token: &str,
    ) -> Result<VerifiedSession, AuthError> {
        let session = temps_entities::sessions::Entity::find()
            .filter(temps_entities::sessions::Column::SessionToken.eq(session_token))
            .filter(temps_entities::sessions::Column::ExpiresAt.gt(Utc::now()))
            // A first-factor-only MFA challenge row must never authenticate a
            // real request. Only `verify_mfa_challenge` may consume those.
            .filter(temps_entities::sessions::Column::MfaPending.eq(false))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| AuthError::NotFound("Session not found or expired".to_string()))?;

        let user = temps_entities::users::Entity::find_by_id(session.user_id)
            .filter(temps_entities::users::Column::DeletedAt.is_null())
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| AuthError::NotFound("User not found or deleted".to_string()))?;

        Ok(VerifiedSession {
            user,
            session_id: session.id,
        })
    }

    fn generate_session_token(&self) -> String {
        let mut rng = rand::rng();
        (0..64)
            .map(|_| rng.sample(rand::distr::Alphanumeric) as char)
            .collect()
    }

    pub fn create_session_cookie(&self, session_token: &str, is_https: bool) -> HeaderMap {
        let session_cookie = Cookie::build(("session", session_token))
            .http_only(true)
            .path("/")
            .max_age(cookie::time::Duration::days(7))
            .same_site(cookie::SameSite::Strict)
            .secure(is_https)
            .build();

        let mfa_clear_cookie = Cookie::build(("mfa_session", ""))
            .http_only(true)
            .path("/")
            .max_age(cookie::time::Duration::seconds(0))
            .same_site(cookie::SameSite::Strict)
            .secure(is_https)
            .build();

        debug!("Setting session cookie (token redacted)");
        let mut headers = HeaderMap::new();
        if let Ok(value) = session_cookie.to_string().parse() {
            headers.append(SET_COOKIE, value);
        } else {
            error!("Failed to parse session cookie header value");
        }
        if let Ok(value) = mfa_clear_cookie.to_string().parse() {
            headers.append(SET_COOKIE, value);
        } else {
            error!("Failed to parse MFA clear cookie header value");
        }
        headers
    }

    pub async fn get_user_by_id(
        &self,
        user_id: i32,
    ) -> Result<temps_entities::users::Model, AuthError> {
        let user = temps_entities::users::Entity::find_by_id(user_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| AuthError::NotFound("User not found".to_string()))?;
        Ok(user)
    }

    pub async fn logout(&self, user_id: i32, _headers: &HeaderMap) -> Result<(), AuthError> {
        let txn = self
            .db
            .begin()
            .await
            .map_err(|e| AuthError::GenericError(e.to_string()))?;

        // Delete user sessions
        temps_entities::sessions::Entity::delete_many()
            .filter(temps_entities::sessions::Column::UserId.eq(user_id))
            .exec(&txn)
            .await?;

        txn.commit()
            .await
            .map_err(|e| AuthError::GenericError(e.to_string()))?;
        Ok(())
    }

    // Creates temporary session for MFA verification
    //
    // `origin` records which login method created this pending challenge
    // (e.g. `"password"`, `"oidc"`, `"saml"`) so a later caller can tell an
    // SSO-originated challenge apart from a password-originated one -- see
    // `mfa_session_origin`'s doc comment for why that distinction matters.
    // Callers should use short, self-consistent values; they don't need to
    // match any other string convention (e.g. audit-log `login_method`
    // values) verbatim.
    pub async fn create_mfa_session(
        &self,
        user_id: i32,
        origin: &str,
    ) -> Result<String, AuthError> {
        let session_token = self.generate_session_token();
        let expires_at = Utc::now() + Duration::minutes(5); // Short expiration for MFA sessions

        let new_session = temps_entities::sessions::ActiveModel {
            user_id: Set(user_id),
            session_token: Set(session_token.clone()),
            expires_at: Set(expires_at),
            // Mark this as a pending MFA challenge so it can never be replayed
            // as a real session cookie -- `verify_session` rejects such rows.
            mfa_pending: Set(true),
            mfa_pending_origin: Set(Some(origin.to_string())),
            ..Default::default()
        };

        new_session.insert(self.db.as_ref()).await?;

        Ok(session_token)
    }

    /// Look up the login method that created a still-pending MFA challenge,
    /// without consuming it.
    ///
    /// Used by SSO enforcement (temps-ee-sso) so `POST /auth/verify-mfa` can
    /// let an SSO-originated pending challenge (`"oidc"`/`"saml"`) through
    /// even while enforcement is blocking password/magic-link/reset logins,
    /// instead of permanently stranding an MFA+SSO account on the "Two-factor
    /// authentication" screen.
    ///
    /// Applies the exact same filters `verify_mfa_challenge` uses (token
    /// match, not expired, `mfa_pending = true`), so this never reports an
    /// origin for a row `verify_mfa_challenge` would itself reject. Every
    /// "not currently a valid pending MFA session" case -- unknown token,
    /// expired, already consumed, or a fully authenticated session token --
    /// collapses to `None`; callers don't need to distinguish them.
    pub async fn mfa_session_origin(
        &self,
        session_token: &str,
    ) -> Result<Option<String>, AuthError> {
        let session = temps_entities::sessions::Entity::find()
            .filter(temps_entities::sessions::Column::SessionToken.eq(session_token))
            .filter(temps_entities::sessions::Column::ExpiresAt.gt(Utc::now()))
            .filter(temps_entities::sessions::Column::MfaPending.eq(true))
            .one(self.db.as_ref())
            .await?;

        Ok(session.and_then(|s| s.mfa_pending_origin).map(|origin| {
            if origin.starts_with("oidc:") {
                "oidc".to_string()
            } else {
                origin
            }
        }))
    }

    // Verifies the MFA code
    pub async fn verify_mfa_challenge(
        &self,
        session_token: &str,
        code: &str,
    ) -> Result<temps_entities::users::Model, MfaChallengeError> {
        self.verify_mfa_challenge_with_origin(session_token, code)
            .await
            .map(|(user, _)| user)
    }

    /// Keep the verified challenge's origin until the final session is issued.
    /// OIDC MFA checks its issuing provider in the session-creation transaction.
    pub(crate) async fn create_session_after_mfa(
        &self,
        user: &temps_entities::users::Model,
        origin: Option<&str>,
    ) -> Result<String, AuthError> {
        self.create_authenticated_session(user.id, origin, SecondFactor::Verified)
            .await
    }

    /// Session for an SSO sign-in that did not need an MFA challenge.
    pub(crate) async fn create_oidc_session(
        &self,
        user_id: i32,
        provider_id: i32,
    ) -> Result<String, AuthError> {
        self.create_authenticated_session(
            user_id,
            Some(&format!("oidc:{provider_id}")),
            SecondFactor::NotPresented,
        )
        .await
    }

    async fn create_authenticated_session(
        &self,
        user_id: i32,
        origin: Option<&str>,
        second_factor: SecondFactor,
    ) -> Result<String, AuthError> {
        let transaction = self.db.begin().await?;
        if let Some(origin) = origin.filter(|origin| origin.starts_with("oidc")) {
            // Bind to the immutable issuer recorded on the challenge, never
            // the user's mutable provider ID. Old unbound challenges expire
            // normally but must restart login to gain this protection.
            let provider_id = origin
                .strip_prefix("oidc:")
                .and_then(|id| id.parse::<i32>().ok())
                .filter(|id| *id > 0)
                .ok_or_else(|| {
                    AuthError::Unauthorized(
                        "Restart OIDC login to verify the issuing provider.".into(),
                    )
                })?;
            let provider = temps_entities::oidc_providers::Entity::find_by_id(provider_id)
                .lock_shared()
                .one(&transaction)
                .await?;
            if !provider.is_some_and(|provider| provider.enabled) {
                return Err(AuthError::Unauthorized(
                    "The OIDC provider was revoked. Please log in again.".into(),
                ));
            }
        }
        // Check and insert commit together under the provider lock. Query or
        // insert failure rolls back; teardown either wins first or deletes
        // this committed session after acquiring its exclusive provider lock.
        let token = self
            .create_session_in_transaction(
                user_id,
                origin.filter(|origin| origin.starts_with("oidc:")),
                second_factor,
                &transaction,
            )
            .await?;
        transaction.commit().await?;
        Ok(token)
    }

    /// Spend a pending sign-in challenge with a TOTP code or, once MFA is
    /// enrolled, a recovery code.
    ///
    /// Everything happens under the user's exclusive row lock:
    /// - a TOTP code is accepted only if its step is newer than the last
    ///   accepted one, so a code cannot sign in twice;
    /// - a recovery code is burned in the same transaction that accepts it;
    /// - a wrong code is counted against the challenge (discarded after
    ///   [`crate::mfa_guard::MAX_FAILED_ATTEMPTS_PER_CHALLENGE`] misses) and
    ///   against the user (locked out past
    ///   [`crate::mfa_guard::USER_LOCKOUT_THRESHOLD`]), and those counters are
    ///   committed before the rejection is returned;
    /// - completing a first-time enrolment here revokes every other session,
    ///   matching self-service enrolment.
    pub(crate) async fn verify_mfa_challenge_with_origin(
        &self,
        session_token: &str,
        code: &str,
    ) -> Result<(temps_entities::users::Model, Option<String>), MfaChallengeError> {
        let session_token = session_token.to_owned();
        let code = code.to_owned();
        let outcome = self
            .db
            .transaction::<_, ChallengeOutcome, MfaChallengeError>(|transaction| {
                Box::pin(async move {
                    // Get the user from the temporary session. Require mfa_pending so a
                    // real (fully authenticated) session token can never be spent as an
                    // MFA challenge -- the discriminator cuts both ways.
                    let pending = temps_entities::sessions::Entity::find()
                        .filter(temps_entities::sessions::Column::SessionToken.eq(&session_token))
                        .filter(temps_entities::sessions::Column::ExpiresAt.gt(Utc::now()))
                        .filter(temps_entities::sessions::Column::MfaPending.eq(true))
                        .one(transaction)
                        .await
                        .map_err(|source| MfaChallengeError::Database {
                            operation: "load the pending session",
                            user_id: None,
                            source,
                        })?
                        .ok_or(MfaChallengeError::InvalidOrExpiredSession)?;

                    let user = temps_entities::users::Entity::find_by_id(pending.user_id)
                        .lock_exclusive()
                        .one(transaction)
                        .await
                        .map_err(|source| MfaChallengeError::Database {
                            operation: "load the challenge user",
                            user_id: Some(pending.user_id),
                            source,
                        })?
                        .ok_or(MfaChallengeError::UserNotFound {
                            user_id: pending.user_id,
                        })?;
                    let user_id = user.id;

                    // Re-read the challenge now that the user lock serializes every
                    // writer: its failure count is current, and a challenge a
                    // concurrent request already spent or discarded is gone.
                    let session = temps_entities::sessions::Entity::find_by_id(pending.id)
                        .filter(temps_entities::sessions::Column::MfaPending.eq(true))
                        .filter(temps_entities::sessions::Column::ExpiresAt.gt(Utc::now()))
                        .lock_exclusive()
                        .one(transaction)
                        .await
                        .map_err(|source| MfaChallengeError::Database {
                            operation: "lock the pending session",
                            user_id: Some(user_id),
                            source,
                        })?
                        .ok_or(MfaChallengeError::InvalidOrExpiredSession)?;
                    if session.mfa_failed_attempts
                        >= crate::mfa_guard::MAX_FAILED_ATTEMPTS_PER_CHALLENGE
                    {
                        return Err(MfaChallengeError::InvalidOrExpiredSession);
                    }

                    let now = Utc::now();
                    let guard = crate::mfa_guard::load_guard(transaction, user_id)
                        .await
                        .map_err(MfaChallengeError::from_guard)?;
                    if let Some(retry_after_seconds) = guard.lockout_remaining_seconds(now) {
                        return Ok(ChallengeOutcome::Locked {
                            user_id,
                            retry_after_seconds,
                        });
                    }

                    // Recovery codes stand in for an enrolled authenticator; they can
                    // never confirm a first-time enrolment.
                    let allow_recovery_codes = user.mfa_enabled;
                    let factor = crate::mfa_guard::check_factor(
                        &user,
                        guard.last_totp_step,
                        &code,
                        allow_recovery_codes,
                        now,
                    )
                    .await
                    .map_err(MfaChallengeError::from_guard)?;

                    let Some(factor) = factor else {
                        crate::mfa_guard::record_failure(transaction, user_id, &guard, now)
                            .await
                            .map_err(MfaChallengeError::from_guard)?;
                        let attempts = session.mfa_failed_attempts.saturating_add(1);
                        if attempts >= crate::mfa_guard::MAX_FAILED_ATTEMPTS_PER_CHALLENGE {
                            temps_entities::sessions::Entity::delete_by_id(session.id)
                                .exec(transaction)
                                .await
                                .map_err(|source| MfaChallengeError::Database {
                                    operation: "discard the exhausted pending session",
                                    user_id: Some(user_id),
                                    source,
                                })?;
                            warn!(
                                user_id,
                                attempts, "MFA challenge discarded after repeated wrong codes"
                            );
                            return Ok(ChallengeOutcome::Exhausted { user_id });
                        } else {
                            let mut challenge: temps_entities::sessions::ActiveModel =
                                session.into();
                            challenge.mfa_failed_attempts = Set(attempts);
                            challenge.update(transaction).await.map_err(|source| {
                                MfaChallengeError::Database {
                                    operation: "count a wrong code on the pending session",
                                    user_id: Some(user_id),
                                    source,
                                }
                            })?;
                        }
                        return Ok(ChallengeOutcome::Rejected { user_id });
                    };

                    let mut verified_user =
                        crate::mfa_guard::record_success(transaction, user, &guard, &factor, now)
                            .await
                            .map_err(MfaChallengeError::from_guard)?;
                    if matches!(
                        factor,
                        crate::mfa_guard::AcceptedFactor::RecoveryCode { .. }
                    ) {
                        info!(user_id, "MFA challenge satisfied with a recovery code");
                    }

                    if !verified_user.mfa_enabled {
                        let mut user_update: temps_entities::users::ActiveModel =
                            verified_user.into();
                        user_update.mfa_enabled = Set(true);
                        verified_user =
                            user_update.update(transaction).await.map_err(|source| {
                                MfaChallengeError::Database {
                                    operation: "enable MFA for the challenge user",
                                    user_id: Some(user_id),
                                    source,
                                }
                            })?;
                        // Sessions created before the second factor existed must not
                        // outlive its enrolment. The challenge itself is consumed below.
                        let revoked = crate::user_service::revoke_sessions_except(
                            transaction,
                            user_id,
                            Some(session.id),
                        )
                        .await
                        .map_err(|source| MfaChallengeError::Database {
                            operation: "revoke sessions that predate MFA enrolment",
                            user_id: Some(user_id),
                            source,
                        })?;
                        info!(
                            user_id,
                            revoked_sessions = revoked,
                            "MFA enabled at sign-in; other sessions revoked"
                        );
                    }

                    // Consume the challenge in the same transaction as enrollment so a
                    // failed update never strands the user without a retryable challenge.
                    let deletion = temps_entities::sessions::Entity::delete_many()
                        .filter(temps_entities::sessions::Column::Id.eq(session.id))
                        .filter(temps_entities::sessions::Column::MfaPending.eq(true))
                        .exec(transaction)
                        .await
                        .map_err(|source| MfaChallengeError::Database {
                            operation: "consume the verified pending session",
                            user_id: Some(user_id),
                            source,
                        })?;
                    if deletion.rows_affected != 1 {
                        return Err(MfaChallengeError::InvalidOrExpiredSession);
                    }

                    Ok(ChallengeOutcome::Verified(
                        Box::new(verified_user),
                        session.mfa_pending_origin,
                    ))
                })
            })
            .await
            .map_err(|error| match error {
                sea_orm::TransactionError::Transaction(error) => error,
                sea_orm::TransactionError::Connection(source) => MfaChallengeError::Database {
                    operation: "run the MFA challenge transaction",
                    user_id: None,
                    source,
                },
            })?;

        match outcome {
            ChallengeOutcome::Verified(user, origin) => Ok((*user, origin)),
            ChallengeOutcome::Rejected { user_id } => {
                Err(MfaChallengeError::InvalidCode { user_id })
            }
            ChallengeOutcome::Exhausted { user_id } => {
                Err(MfaChallengeError::ChallengeExhausted { user_id })
            }
            ChallengeOutcome::Locked {
                user_id,
                retry_after_seconds,
            } => Err(MfaChallengeError::LockedOut {
                user_id,
                retry_after_seconds,
            }),
        }
    }

    // Register a new user with email/password
    pub async fn register_user(
        &self,
        request: RegisterRequest,
    ) -> Result<temps_entities::users::Model, UserAuthError> {
        // Validate password complexity
        validate_password_complexity(&request.password)?;

        // Check if email already exists
        let existing_user = temps_entities::users::Entity::find()
            .filter(temps_entities::users::Column::Email.eq(request.email.to_lowercase()))
            .one(self.db.as_ref())
            .await?;

        if existing_user.is_some() {
            return Err(UserAuthError::EmailAlreadyRegistered);
        }

        // Hash the password
        let argon2 = argon2::Argon2::default();

        let password_hash = argon2
            .hash_password(request.password.as_bytes())
            .map_err(|_| UserAuthError::PasswordHashError)?
            .to_string();

        // Create the user
        let new_user = temps_entities::users::ActiveModel {
            email: Set(request.email.to_lowercase()),
            name: Set(request.name.clone()),
            password_hash: Set(Some(password_hash)),
            must_change_password: Set(false),
            email_verified: Set(false),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            deleted_at: Set(None),
            mfa_enabled: Set(false),
            mfa_secret: Set(None),
            mfa_recovery_codes: Set(None),
            ..Default::default()
        };

        let user = new_user.insert(self.db.as_ref()).await?;

        // Send verification email if email service is configured
        let verification_token = self.generate_token();

        // Update user with verification token
        let mut user_update: temps_entities::users::ActiveModel = user.into();
        user_update.email_verification_token = Set(Some(verification_token.clone()));
        user_update.email_verification_expires = Set(Some(Utc::now() + Duration::hours(24)));
        let updated_user = user_update.update(self.db.as_ref()).await?;
        let settings = self.get_settings().await?;

        // Send verification email
        let base_url = settings
            .external_url
            .unwrap_or_else(|| DEFAULT_EXTERNAL_URL.to_string());

        let _ = self
            .email_service
            .send_verification_email(&request.email, &verification_token, &base_url)
            .await;

        Ok(updated_user)
    }

    // Login with email/password
    pub async fn login(
        &self,
        request: LoginRequest,
    ) -> Result<temps_entities::users::Model, UserAuthError> {
        // Every rejection below costs one Argon2 verification, the same as a
        // wrong password for a real account, so response time does not
        // reveal whether the email exists or has a password.
        let user = match temps_entities::users::Entity::find()
            .filter(temps_entities::users::Column::Email.eq(request.email.to_lowercase()))
            .filter(temps_entities::users::Column::DeletedAt.is_null())
            .one(self.db.as_ref())
            .await?
        {
            Some(user) => user,
            None => {
                verify_against_dummy_hash(&request.password);
                warn!(
                    "Login attempt for non-existent or deleted email: {}",
                    request.email
                );
                return Err(UserAuthError::InvalidCredentials);
            }
        };

        // Check if user has a password (might be GitHub-only user)
        let Some(password_hash) = user.password_hash.as_ref() else {
            verify_against_dummy_hash(&request.password);
            warn!("Login attempt for user {} with no password hash", user.id);
            return Err(UserAuthError::InvalidCredentials);
        };

        // Verify password - only Argon2 is supported
        let password_valid = match password_hash
            .starts_with("$argon2")
            .then(|| argon2::PasswordHash::new(password_hash))
        {
            Some(Ok(parsed_hash)) => {
                debug!("Verifying Argon2 password for user {}", user.id);
                argon2::Argon2::default()
                    .verify_password(request.password.as_bytes(), &parsed_hash)
                    .is_ok()
            }
            Some(Err(e)) => {
                error!("Failed to parse Argon2 hash for user {}: {}", user.id, e);
                verify_against_dummy_hash(&request.password);
                false
            }
            None => {
                // Only Argon2 is supported - all other hash formats are rejected
                error!(
                    "User {} has unsupported password hash format (only Argon2 is supported): {}",
                    user.id,
                    &password_hash[..std::cmp::min(20, password_hash.len())]
                );
                verify_against_dummy_hash(&request.password);
                false
            }
        };

        if !password_valid {
            warn!("Invalid password attempt for user {}", user.id);
            return Err(UserAuthError::InvalidCredentials);
        }

        // A required password change is completed before role-based MFA
        // enrollment. The password-change handler starts a pending MFA
        // challenge when the policy applies, without issuing a full session.
        if user.must_change_password {
            debug!("Temporary password verified for user {}", user.id);
            return Ok(user);
        }

        // SOC2 hardening (bherila/temps#32): operators can require MFA
        // enrollment for Admin-role accounts via the `require_mfa_for_admins`
        // setting. This check only runs in the password-login path -- SSO/OIDC
        // logins go through `OidcService::resolve_user` +
        // `oidc_handler::complete_oidc_login`, which never call this method,
        // so federated logins are unaffected by design (see doc comment on
        // `AppSettings::require_mfa_for_admins`).
        //
        // A settings-lookup failure must never block login for the whole
        // instance (this runs on every successful password login, not just
        // admins) -- degrade to the default (`require_mfa_for_admins: false`)
        // and let the login proceed, same graceful-degradation contract as
        // `count_active_sessions` below.
        let settings = self.get_settings().await.unwrap_or_default();
        if settings.require_mfa_for_admins && !user.mfa_enabled {
            let user_service = crate::user_service::UserService::new(self.db.clone());
            let is_admin = match user_service.is_admin(user.id).await {
                Ok(is_admin) => is_admin,
                Err(e) => {
                    error!(
                        "Failed to determine admin status for user {} while enforcing require_mfa_for_admins: {}",
                        user.id, e
                    );
                    false
                }
            };
            if is_admin {
                warn!(
                    "Blocking login for admin user {}: require_mfa_for_admins is enabled and MFA is not enrolled",
                    user.id
                );
                return Err(UserAuthError::MfaRequiredForRole {
                    user_id: user.id,
                    role: "Admin".to_string(),
                });
            }
        }

        debug!("Successful login for user {}", user.id);
        Ok(user)
    }

    /// Create a short-lived reset credential after the user's temporary
    /// password has been verified. No regular session is created while the
    /// account is marked as requiring a password change.
    ///
    /// `verified` is the user row the caller authenticated against. The token
    /// is only issued if the stored password hash is still the one that was
    /// verified: an admin reset committing between verification and this call
    /// replaces the hash, and the superseded temporary password must not be
    /// able to mint a change token. The check runs under the same row lock
    /// the reset takes, so the two cannot interleave.
    pub async fn create_required_password_change_token(
        &self,
        verified: &temps_entities::users::Model,
    ) -> Result<String, UserAuthError> {
        let user_id = verified.id;
        let transaction = self.db.begin().await?;
        let user = temps_entities::users::Entity::find_by_id(user_id)
            .lock_exclusive()
            .one(&transaction)
            .await?
            .ok_or(UserAuthError::UserNotFound)?;

        if user.password_hash != verified.password_hash {
            return Err(UserAuthError::CredentialsChanged { user_id });
        }
        if !user.must_change_password {
            return Err(UserAuthError::PasswordChangeNotRequired { user_id });
        }

        let token = self.generate_token();
        let mut user_update: temps_entities::users::ActiveModel = user.into();
        user_update.password_reset_token = Set(Some(token.clone()));
        user_update.password_reset_expires = Set(Some(Utc::now() + Duration::minutes(15)));
        user_update.update(&transaction).await?;
        transaction.commit().await?;

        Ok(token)
    }

    /// Resolve a live first-login password-change credential without
    /// consuming it. The handler uses this to evaluate post-change policy
    /// before committing the password replacement.
    pub async fn required_password_change_user(
        &self,
        token: &str,
    ) -> Result<temps_entities::users::Model, UserAuthError> {
        let user = temps_entities::users::Entity::find()
            .filter(temps_entities::users::Column::PasswordResetToken.eq(token))
            .one(self.db.as_ref())
            .await?
            .ok_or(UserAuthError::InvalidToken)?;

        if !user.must_change_password
            || user
                .password_reset_expires
                .is_none_or(|expires_at| expires_at < Utc::now())
        {
            return Err(UserAuthError::InvalidToken);
        }

        Ok(user)
    }

    // Request password reset
    pub async fn request_password_reset(&self, email: &str) -> Result<(), UserAuthError> {
        // Check if email service is configured
        // Find user by email
        let user = temps_entities::users::Entity::find()
            .filter(temps_entities::users::Column::Email.eq(email.to_lowercase()))
            .one(self.db.as_ref())
            .await?;

        // Always return success to avoid email enumeration
        if let Some(user) = user {
            let reset_token = self.generate_token();
            let expires_at = Utc::now() + Duration::hours(1);

            // Update user with reset token
            let mut user_update: temps_entities::users::ActiveModel = user.clone().into();
            user_update.password_reset_token = Set(Some(reset_token.clone()));
            user_update.password_reset_expires = Set(Some(expires_at));
            user_update.update(self.db.as_ref()).await?;
            let settings = self.get_settings().await?;
            // Send password reset email
            let base_url = settings
                .external_url
                .unwrap_or_else(|| DEFAULT_EXTERNAL_URL.to_string());

            let _ = self
                .email_service
                .send_password_reset_email(email, &reset_token, &base_url)
                .await;
        }
        Ok(())
    }

    // Reset password with token
    pub async fn reset_password(
        &self,
        request: ResetPasswordRequest,
    ) -> Result<temps_entities::users::Model, UserAuthError> {
        // Validate password complexity
        validate_password_complexity(&request.new_password)?;

        self.db
            .transaction::<_, temps_entities::users::Model, UserAuthError>(|transaction| {
                Box::pin(async move {
                    // Find user by reset token. The transaction callback guarantees that
                    // every rejection path is rolled back before this method returns.
                    let user = temps_entities::users::Entity::find()
                        .filter(
                            temps_entities::users::Column::PasswordResetToken.eq(&request.token),
                        )
                        .lock_exclusive()
                        .one(transaction)
                        .await?
                        .ok_or(UserAuthError::InvalidToken)?;

                    // Check if token is expired
                    if user
                        .password_reset_expires
                        .is_none_or(|expires_at| expires_at < Utc::now())
                    {
                        return Err(UserAuthError::InvalidToken);
                    }

                    if user.must_change_password {
                        if let Some(current_hash) = user.password_hash.as_deref() {
                            let parsed_hash = argon2::PasswordHash::new(current_hash)
                                .map_err(|_| UserAuthError::PasswordHashError)?;
                            if argon2::Argon2::default()
                                .verify_password(request.new_password.as_bytes(), &parsed_hash)
                                .is_ok()
                            {
                                return Err(UserAuthError::SamePassword);
                            }
                        }
                    }

                    // Hash new password
                    let argon2 = argon2::Argon2::default();

                    let password_hash = argon2
                        .hash_password(request.new_password.as_bytes())
                        .map_err(|_| UserAuthError::PasswordHashError)?
                        .to_string();

                    // Update user password and clear reset token
                    let mut user_update: temps_entities::users::ActiveModel = user.into();
                    user_update.password_hash = Set(Some(password_hash));
                    user_update.password_reset_token = Set(None);
                    user_update.password_reset_expires = Set(None);
                    user_update.must_change_password = Set(false);
                    user_update.updated_at = Set(Utc::now());
                    user_update.update(transaction).await.map_err(Into::into)
                })
            })
            .await
            .map_err(Into::into)
    }

    /// Complete the first-login password change. Unlike an email reset, this
    /// requires the account to still be flagged and rejects reuse of the
    /// administrator-provided temporary password.
    pub async fn reset_required_password(
        &self,
        request: ResetPasswordRequest,
    ) -> Result<temps_entities::users::Model, UserAuthError> {
        validate_password_complexity(&request.new_password)?;

        self.db
            .transaction::<_, temps_entities::users::Model, UserAuthError>(|transaction| {
                Box::pin(async move {
                    let user = temps_entities::users::Entity::find()
                        .filter(
                            temps_entities::users::Column::PasswordResetToken.eq(&request.token),
                        )
                        .lock_exclusive()
                        .one(transaction)
                        .await?
                        .ok_or(UserAuthError::InvalidToken)?;

                    if !user.must_change_password
                        || user
                            .password_reset_expires
                            .is_none_or(|expires_at| expires_at < Utc::now())
                    {
                        return Err(UserAuthError::InvalidToken);
                    }

                    if let Some(current_hash) = user.password_hash.as_deref() {
                        let parsed_hash = argon2::PasswordHash::new(current_hash)
                            .map_err(|_| UserAuthError::PasswordHashError)?;
                        if argon2::Argon2::default()
                            .verify_password(request.new_password.as_bytes(), &parsed_hash)
                            .is_ok()
                        {
                            return Err(UserAuthError::SamePassword);
                        }
                    }

                    let password_hash = argon2::Argon2::default()
                        .hash_password(request.new_password.as_bytes())
                        .map_err(|_| UserAuthError::PasswordHashError)?
                        .to_string();

                    let mut user_update: temps_entities::users::ActiveModel = user.into();
                    user_update.password_hash = Set(Some(password_hash));
                    user_update.password_reset_token = Set(None);
                    user_update.password_reset_expires = Set(None);
                    user_update.must_change_password = Set(false);
                    user_update.updated_at = Set(Utc::now());
                    user_update.update(transaction).await.map_err(Into::into)
                })
            })
            .await
            .map_err(Into::into)
    }

    pub async fn requires_mfa_enrollment(&self, user_id: i32) -> Result<bool, UserAuthError> {
        let settings = self.get_settings().await?;
        if !settings.require_mfa_for_admins {
            return Ok(false);
        }

        let user = temps_entities::users::Entity::find_by_id(user_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(UserAuthError::UserNotFound)?;
        if user.mfa_enabled {
            return Ok(false);
        }

        crate::user_service::UserService::new(self.db.clone())
            .is_admin(user_id)
            .await
            .map_err(|error| UserAuthError::RoleLookup {
                user_id,
                reason: error.to_string(),
            })
    }

    /// In-app password change for an authenticated user.
    ///
    /// Distinct from [`reset_password`] — that flow runs out-of-band via an
    /// email link and trusts the token as proof of identity. This flow runs
    /// while the user is already logged in and uses the current password +
    /// (when applicable) an MFA code as the re-auth gate.
    ///
    /// Caller must pass the encrypted session token from the request cookie
    /// in `current_session_token` so we can preserve that one session when
    /// `revoke_other_sessions` is true. The decrypted form is stored in the
    /// `sessions` table; the handler decrypts before passing it in.
    pub async fn change_password_self(
        &self,
        user_id: i32,
        current_password: &str,
        new_password: &str,
        mfa_code: Option<&str>,
        revoke_other_sessions: bool,
        current_session_token: Option<&str>,
    ) -> Result<temps_entities::users::Model, AuthError> {
        let user = temps_entities::users::Entity::find_by_id(user_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| AuthError::NotFound(format!("User {} not found", user_id)))?;

        // 1. Account must have a password set. SSO-only users
        // get a friendly error instead of a generic 401.
        let stored_hash = user
            .password_hash
            .as_ref()
            .ok_or(AuthError::NoPasswordSet)?;

        // 2. Verify current password (Argon2 only; legacy bcrypt rows would
        // already have been migrated by login).
        if !stored_hash.starts_with("$argon2") {
            error!(
                "User {} has unsupported password hash format during change",
                user_id
            );
            return Err(AuthError::InvalidCurrentPassword);
        }
        use argon2::PasswordVerifier;
        let parsed = argon2::PasswordHash::new(stored_hash)
            .map_err(|_| AuthError::InvalidCurrentPassword)?;
        if argon2::Argon2::default()
            .verify_password(current_password.as_bytes(), &parsed)
            .is_err()
        {
            warn!("Password-change re-auth failed for user {}", user_id);
            return Err(AuthError::InvalidCurrentPassword);
        }

        // 3. MFA gate. When the account has MFA, a code must be supplied
        // and accepted by the same verifier used for login (TOTP or
        // recovery code). Recovery codes are single-use; verify_mfa_code
        // burns them on success — that's the desired audit trail.
        if user.mfa_enabled {
            let code = mfa_code
                .filter(|c| !c.is_empty())
                .ok_or(AuthError::MfaCodeRequired)?;
            // Reuse the user_service verifier so behavior matches login
            // exactly (TOTP skew window, recovery code burn-on-use).
            let user_service = crate::user_service::UserService::new(self.db.clone());
            let mfa_ok =
                user_service
                    .verify_mfa_code(user_id, code)
                    .await
                    .map_err(|e| match e {
                        crate::user_service::UserServiceError::MfaLocked {
                            user_id,
                            retry_after_seconds,
                        } => AuthError::MfaLockedOut {
                            user_id,
                            retry_after_seconds,
                        },
                        other => {
                            error!("MFA verification error during password change: {}", other);
                            AuthError::InvalidMfaCode
                        }
                    })?;
            if !mfa_ok {
                return Err(AuthError::InvalidMfaCode);
            }
        }

        // 4. Refuse no-op changes — silently succeeding here is confusing
        // ("did it work?") and weakens the audit log.
        if current_password == new_password {
            return Err(AuthError::SamePassword);
        }

        // 5. New-password complexity check. The crate-level helper is the
        // same one register / reset use, so the rules stay consistent.
        crate::auth_service::validate_password_complexity(new_password)
            .map_err(|e| AuthError::WeakPassword(e.to_string()))?;

        // 6. Hash + persist. We also bump updated_at; downstream watchers
        // (audit consumers, cache invalidators) key off this.
        let argon2 = argon2::Argon2::default();
        use argon2::PasswordHasher;
        let new_hash = argon2
            .hash_password(new_password.as_bytes())
            .map_err(|e| AuthError::EncryptionError(format!("password hash failed: {}", e)))?
            .to_string();

        let mut user_update: temps_entities::users::ActiveModel = user.clone().into();
        user_update.password_hash = Set(Some(new_hash));
        user_update.updated_at = Set(Utc::now());
        let updated = user_update.update(self.db.as_ref()).await?;

        // 7. Optional session sweep. Drops every session for this user
        // EXCEPT the current one (kept so the user's tab keeps working).
        // When current_session_token is None — e.g. called from a non-
        // browser context — we drop everything to be safe.
        if revoke_other_sessions {
            use sea_orm::QueryFilter;
            let mut q = temps_entities::sessions::Entity::delete_many()
                .filter(temps_entities::sessions::Column::UserId.eq(user_id));
            if let Some(tok) = current_session_token {
                q = q.filter(temps_entities::sessions::Column::SessionToken.ne(tok));
            }
            if let Err(e) = q.exec(self.db.as_ref()).await {
                // Don't fail the whole operation — the password is already
                // rotated. Log loudly so an operator notices.
                error!(
                    "Failed to revoke other sessions for user {} after password change: {}",
                    user_id, e
                );
            }
        }

        info!(
            "Password changed for user {} (revoke_other_sessions={})",
            user_id, revoke_other_sessions
        );
        Ok(updated)
    }

    // Verify email with token
    pub async fn verify_email(
        &self,
        token: &str,
    ) -> Result<temps_entities::users::Model, UserAuthError> {
        // Find user by verification token
        let user = temps_entities::users::Entity::find()
            .filter(temps_entities::users::Column::EmailVerificationToken.eq(token))
            .one(self.db.as_ref())
            .await?
            .ok_or(UserAuthError::InvalidToken)?;

        // Check if token is expired
        if let Some(expires_at) = user.email_verification_expires {
            if expires_at < Utc::now() {
                return Err(UserAuthError::InvalidToken);
            }
        } else {
            return Err(UserAuthError::InvalidToken);
        }

        // Mark email as verified
        let mut user_update: temps_entities::users::ActiveModel = user.into();
        user_update.email_verified = Set(true);
        user_update.email_verification_token = Set(None);
        user_update.email_verification_expires = Set(None);
        user_update.updated_at = Set(Utc::now());
        let updated_user = user_update.update(self.db.as_ref()).await?;

        Ok(updated_user)
    }

    /// Whether an email provider is configured for transactional mail.
    ///
    /// Checks specifically for an enabled *email* notification provider —
    /// not just any provider — because password reset and email
    /// verification require real inbox delivery, not a Slack/webhook channel.
    pub async fn is_email_configured(&self) -> bool {
        self.email_service.is_email_provider_configured().await
    }

    // Helper to generate secure random tokens
    fn generate_token(&self) -> String {
        Uuid::new_v4().to_string()
    }

    /// Get the application settings
    async fn get_settings(&self) -> Result<temps_core::AppSettings, sea_orm::DbErr> {
        let record = temps_entities::settings::Entity::find_by_id(1)
            .one(self.db.as_ref())
            .await?;

        Ok(record
            .map(|r| temps_core::AppSettings::from_json(r.data))
            .unwrap_or_default())
    }
}

/// An Argon2 hash of a random secret nobody knows, produced with the same
/// parameters as real password hashes (`Argon2::default()`). Computed once.
fn dummy_password_hash() -> &'static str {
    static DUMMY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    DUMMY.get_or_init(|| {
        let secret: [u8; 32] = rand::rng().random();
        argon2::Argon2::default()
            .hash_password(&secret)
            .map(|hash| hash.to_string())
            // Hashing fixed-size input with default params cannot fail in
            // practice; an empty string makes the verification below a no-op
            // rather than a panic, and is logged.
            .unwrap_or_else(|error| {
                error!("Failed to compute the login timing-equalization hash: {error}");
                String::new()
            })
    })
}

#[cfg(test)]
thread_local! {
    static DUMMY_VERIFICATIONS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Spend the same Argon2 work a real wrong-password check costs, so the
/// "no such account" / "no password" paths are not measurably faster.
/// Always rejects.
fn verify_against_dummy_hash(password: &str) -> bool {
    #[cfg(test)]
    DUMMY_VERIFICATIONS.with(|count| count.set(count.get() + 1));
    match argon2::PasswordHash::new(dummy_password_hash()) {
        Ok(parsed) => {
            let _ = argon2::Argon2::default().verify_password(password.as_bytes(), &parsed);
        }
        Err(error) => {
            error!("Login timing-equalization hash is unusable: {error}");
        }
    }
    false
}

/// Validate password meets minimum complexity requirements.
/// Requirements:
/// - At least 8 characters long
/// - Contains at least one uppercase letter
/// - Contains at least one lowercase letter
/// - Contains at least one digit
/// - Contains at least one special character
pub fn validate_password_complexity(password: &str) -> Result<(), UserAuthError> {
    let char_count = password.chars().count();
    if char_count < 8 {
        return Err(UserAuthError::WeakPassword(
            "Password must be at least 8 characters long".to_string(),
        ));
    }
    if char_count > 128 {
        return Err(UserAuthError::WeakPassword(
            "Password must not exceed 128 characters".to_string(),
        ));
    }
    if !password.chars().any(|c| c.is_uppercase()) {
        return Err(UserAuthError::WeakPassword(
            "Password must contain at least one uppercase letter".to_string(),
        ));
    }
    if !password.chars().any(|c| c.is_lowercase()) {
        return Err(UserAuthError::WeakPassword(
            "Password must contain at least one lowercase letter".to_string(),
        ));
    }
    if !password.chars().any(|c| c.is_ascii_digit()) {
        return Err(UserAuthError::WeakPassword(
            "Password must contain at least one digit".to_string(),
        ));
    }
    if !password.chars().any(|c| !c.is_alphanumeric()) {
        return Err(UserAuthError::WeakPassword(
            "Password must contain at least one special character".to_string(),
        ));
    }
    Ok(())
}

#[derive(Error, Debug)]
pub enum UserAuthError {
    // Deliberately NOT `#[from]` -- see the manual `From<sea_orm::DbErr>` impl
    // below, which detects a unique-constraint violation on `users.email`
    // (raised by the DB-level `idx_users_email_unique` index) and maps it to
    // `EmailAlreadyRegistered` instead of a generic database error. `#[source]`
    // is kept (independent of `#[from]`) so this variant still participates
    // in `Error::source()` error-chain tooling.
    #[error("Database error: {0}")]
    DatabaseError(#[source] sea_orm::DbErr),
    #[error("Invalid credentials")]
    InvalidCredentials,
    #[error("User not found")]
    UserNotFound,
    #[error("Email already registered")]
    EmailAlreadyRegistered,
    #[error("Invalid or expired token")]
    InvalidToken,
    #[error("Password hashing error")]
    PasswordHashError,
    #[error("Password does not meet complexity requirements: {0}")]
    WeakPassword(String),
    #[error("Email service not configured")]
    EmailServiceNotConfigured,
    #[error("Email service error: {0}")]
    EmailServiceError(String),
    #[error("Encryption error: {0}")]
    EncryptionError(String),
    /// Raised at login when `require_mfa_for_admins` is enabled, the user
    /// holds the `role` (elevated) role, and they have not enrolled MFA.
    /// (bherila/temps#32.)
    #[error(
        "MFA is required for the '{role}' role but user {user_id} has not enrolled multi-factor authentication"
    )]
    MfaRequiredForRole { user_id: i32, role: String },
    #[error("User {user_id} is not required to change their password")]
    PasswordChangeNotRequired { user_id: i32 },
    #[error("Password of user {user_id} changed after it was verified")]
    CredentialsChanged { user_id: i32 },
    #[error("New password must differ from the temporary password")]
    SamePassword,
    #[error("Failed to determine roles for user {user_id}: {reason}")]
    RoleLookup { user_id: i32, reason: String },
}

/// Detect a Postgres unique-constraint violation regardless of the specific
/// `DbErr` variant Sea-ORM wraps it in. The `users` table has exactly one
/// unique constraint (`idx_users_email_unique` on `email`, see
/// `m20250127_000001_add_unique_email_constraint.rs`), so within this crate
/// any unique-violation surfacing through a `UserAuthError` conversion can
/// only be a duplicate email -- this backstops the application-level
/// pre-check in `register_user` against a race between the SELECT and the
/// INSERT (bherila/temps#24).
fn is_unique_violation(error: &sea_orm::DbErr) -> bool {
    if matches!(error, sea_orm::DbErr::RecordNotInserted) {
        return true;
    }
    // Match the specific constraint name rather than a generic "duplicate
    // key"/"23505" substring: this crate has other unique-constrained
    // columns reachable through `UserAuthError` (e.g.
    // `api_keys.key_hash`), and a generic substring match would
    // misreport an unrelated collision on one of those as
    // `EmailAlreadyRegistered`.
    error.to_string().contains("idx_users_email_unique")
}

impl From<sea_orm::DbErr> for UserAuthError {
    fn from(error: sea_orm::DbErr) -> Self {
        if is_unique_violation(&error) {
            return UserAuthError::EmailAlreadyRegistered;
        }
        UserAuthError::DatabaseError(error)
    }
}

impl From<sea_orm::TransactionError<UserAuthError>> for UserAuthError {
    fn from(error: sea_orm::TransactionError<UserAuthError>) -> Self {
        match error {
            sea_orm::TransactionError::Transaction(error) => error,
            sea_orm::TransactionError::Connection(error) => error.into(),
        }
    }
}

// Request DTOs
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct RegisterRequest {
    pub email: String,
    pub password: String,
    pub name: String,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct ResetPasswordRequest {
    pub token: String,
    pub new_password: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderMap;
    use chrono::{Duration, Utc};
    use sea_orm::{DatabaseBackend, MockDatabase};
    use temps_database::test_utils::TestDatabase;
    use temps_entities::types::RoleType;
    use temps_entities::{sessions, settings, users};

    struct MockEmailService {
        verification_emails_sent: std::sync::Mutex<Vec<(String, String, String)>>,
        password_reset_emails_sent: std::sync::Mutex<Vec<(String, String, String)>>,
    }

    impl MockEmailService {
        fn new() -> Self {
            Self {
                verification_emails_sent: std::sync::Mutex::new(Vec::new()),
                password_reset_emails_sent: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn get_verification_emails(&self) -> Vec<(String, String, String)> {
            self.verification_emails_sent.lock().unwrap().clone()
        }

        fn get_password_reset_emails(&self) -> Vec<(String, String, String)> {
            self.password_reset_emails_sent.lock().unwrap().clone()
        }
    }

    use async_trait::async_trait;
    use temps_core::notifications::{NotificationError, NotificationService};

    #[async_trait]
    impl NotificationService for MockEmailService {
        async fn send_email(
            &self,
            message: temps_core::notifications::EmailMessage,
        ) -> Result<(), NotificationError> {
            // Extract email and URL from the message for testing purposes
            if let Some(to) = message.to.first() {
                if message.subject.contains("Verify") {
                    // Extract verification token from body
                    if let Some(start) = message.body.find("token is: ") {
                        let token = &message.body[start + 10..];
                        let url = format!("/auth/verify?token={}", token);
                        self.verification_emails_sent.lock().unwrap().push((
                            to.clone(),
                            "".to_string(),
                            url,
                        ));
                    }
                } else if message.subject.contains("Password") {
                    // Extract reset token from body
                    if let Some(start) = message.body.find("token is: ") {
                        let token = &message.body[start + 10..];
                        let url = format!("/auth/reset-password?token={}", token);
                        self.password_reset_emails_sent.lock().unwrap().push((
                            to.clone(),
                            "".to_string(),
                            url,
                        ));
                    }
                }
            }
            Ok(())
        }

        async fn send_transactional_email(
            &self,
            message: temps_core::notifications::EmailMessage,
        ) -> Result<(), NotificationError> {
            // The auth flows now send via the transactional path; record it
            // through the same logic the tests assert against.
            self.send_email(message).await
        }

        async fn send_notification(
            &self,
            _notification: temps_core::notifications::NotificationData,
        ) -> Result<(), NotificationError> {
            // No-op for tests
            Ok(())
        }

        async fn is_configured(&self) -> Result<bool, NotificationError> {
            // Always configured for tests
            Ok(true)
        }

        async fn is_email_provider_configured(&self) -> Result<bool, NotificationError> {
            // Always configured for tests
            Ok(true)
        }
    }

    async fn setup_test_env() -> (TestDatabase, AuthService, Arc<MockEmailService>) {
        let db = TestDatabase::with_migrations().await.unwrap();

        // Create default settings
        let settings = settings::ActiveModel {
            id: Set(1),
            data: Set(serde_json::json!({
                "external_url": "https://test.example.com"
            })),
            ..Default::default()
        };
        settings.insert(db.db.as_ref()).await.unwrap();

        let notification_service = Arc::new(MockEmailService::new());
        let auth_service = AuthService::new(db.db.clone(), notification_service.clone());
        (db, auth_service, notification_service)
    }

    async fn create_test_user(
        db: &Arc<DatabaseConnection>,
        email: &str,
        password: &str,
    ) -> users::Model {
        let argon2 = argon2::Argon2::default();
        let password_hash = argon2
            .hash_password(password.as_bytes())
            .unwrap()
            .to_string();

        let user = users::ActiveModel {
            email: Set(email.to_lowercase()),
            name: Set(format!("Test User {}", email)),
            password_hash: Set(Some(password_hash)),
            email_verified: Set(true),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            mfa_enabled: Set(false),
            ..Default::default()
        };
        user.insert(db.as_ref()).await.unwrap()
    }

    async fn create_test_user_with_mfa(
        db: &Arc<DatabaseConnection>,
        email: &str,
        password: &str,
        mfa_enabled: bool,
    ) -> users::Model {
        let argon2 = argon2::Argon2::default();
        let password_hash = argon2
            .hash_password(password.as_bytes())
            .unwrap()
            .to_string();

        let user = users::ActiveModel {
            email: Set(email.to_lowercase()),
            name: Set(format!("Test User {}", email)),
            password_hash: Set(Some(password_hash)),
            email_verified: Set(true),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            mfa_enabled: Set(mfa_enabled),
            mfa_secret: if mfa_enabled {
                Set(Some("JBSWY3DPEHPK3PXPJBSWY3DPEHPK3PXP".to_string()))
            } else {
                Set(None)
            },
            ..Default::default()
        };
        user.insert(db.as_ref()).await.unwrap()
    }

    /// Like `setup_test_env`, but (a) lets the caller control
    /// `require_mfa_for_admins`, and (b) skips gracefully instead of
    /// panicking when Docker is unavailable (CLAUDE.md: Docker-dependent
    /// tests must never use `#[ignore]` or otherwise fail the run when the
    /// daemon simply isn't present in this environment).
    async fn setup_test_env_with_mfa_setting(
        require_mfa_for_admins: bool,
    ) -> Option<(TestDatabase, AuthService)> {
        let db = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e)
                if temps_database::test_utils::is_container_runtime_unavailable(&e.to_string()) =>
            {
                println!("Docker not available, skipping test: {e}");
                return None;
            }
            Err(e) => panic!("test database setup failed: {e}"),
        };

        let settings_row = settings::ActiveModel {
            id: Set(1),
            data: Set(serde_json::json!({
                "external_url": "https://test.example.com",
                "require_mfa_for_admins": require_mfa_for_admins,
            })),
            ..Default::default()
        };
        settings_row.insert(db.db.as_ref()).await.unwrap();

        let notification_service = Arc::new(MockEmailService::new());
        let auth_service = AuthService::new(db.db.clone(), notification_service);
        Some((db, auth_service))
    }

    // Session Management Tests

    #[tokio::test]
    async fn failed_oidc_mfa_provider_check_never_inserts_a_session() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_errors([sea_orm::DbErr::Custom("provider lookup unavailable".into())])
                .into_connection(),
        );
        let auth = AuthService::new(db.clone(), Arc::new(MockEmailService::new()));
        assert!(auth
            .create_authenticated_session(7, Some("oidc:5"), SecondFactor::Verified)
            .await
            .is_err());
        assert!(matches!(
            auth.create_authenticated_session(7, Some("oidc"), SecondFactor::Verified)
                .await,
            Err(AuthError::Unauthorized(_))
        ));
        drop(auth);
        let transactions = Arc::try_unwrap(db).unwrap().into_transaction_log();
        assert!(
            !format!("{transactions:?}").contains("INSERT"),
            "failed provider checks must never create authenticated sessions"
        );
    }

    #[tokio::test]
    async fn oidc_mfa_completion_cannot_survive_provider_revocation() {
        let (db, auth, _) = setup_test_env().await;
        let oidc = crate::oidc_service::OidcService::new(
            db.db.clone(),
            Arc::new(temps_core::EncryptionService::new_from_password(
                "mfa-oidc-test",
            )),
            Arc::new(crate::UserService::new(db.db.clone())),
        );
        let provider = oidc
            .upsert_managed_cloud_provider(crate::oidc_service::ManagedCloudOidcConfig {
                issuer: "https://cloud.example.com".into(),
                client_id: "client".into(),
                client_secret: "test-secret".into(),
            })
            .await
            .expect("provider is provisioned");
        let user =
            create_test_user_with_mfa(&db.db, "mfa-cloud@example.com", "Password123!", true).await;
        let mut active: users::ActiveModel = user.into();
        active.oidc_provider_id = Set(Some(provider.id));
        let user = active.update(db.db.as_ref()).await.unwrap();
        let pending = auth
            .create_mfa_session(user.id, &format!("oidc:{}", provider.id))
            .await
            .unwrap();
        let code = current_totp_code(user.mfa_secret.as_deref().unwrap());
        let (verified, origin) = auth
            .verify_mfa_challenge_with_origin(&pending, &code)
            .await
            .unwrap();
        assert_eq!(origin, Some(format!("oidc:{}", provider.id)));
        let token = auth
            .create_session_after_mfa(&verified, origin.as_deref())
            .await
            .unwrap();
        assert!(auth.verify_session(&token).await.is_ok());

        // Consume another valid challenge before revocation, then finish the
        // delayed MFA request after teardown has deleted every visible session.
        // The first code's step is spent, so this one comes from the next step.
        let next_code = crate::mfa_guard::test_support::code_at(
            user.mfa_secret.as_deref().unwrap(),
            Utc::now() + Duration::seconds(30),
        );
        let pending = auth
            .create_mfa_session(user.id, &format!("oidc:{}", provider.id))
            .await
            .unwrap();
        let (verified, origin) = auth
            .verify_mfa_challenge_with_origin(&pending, &next_code)
            .await
            .unwrap();
        // Another live session from this provider, as a completed SSO + MFA
        // sign-in leaves it (an MFA user never gets a first-factor-only one).
        auth.create_authenticated_session(
            user.id,
            Some(&format!("oidc:{}", provider.id)),
            SecondFactor::Verified,
        )
        .await
        .unwrap();
        auth.create_mfa_session(user.id, &format!("oidc:{}", provider.id))
            .await
            .unwrap();
        let other_provider = temps_entities::oidc_providers::ActiveModel {
            name: Set("Other provider".into()),
            issuer_url: Set("https://other.example.com".into()),
            client_id: Set("other-client".into()),
            client_secret_encrypted: Set("unused-test-secret".into()),
            enabled: Set(true),
            ..Default::default()
        }
        .insert(db.db.as_ref())
        .await
        .unwrap();
        let mut relinked: users::ActiveModel = verified.into();
        relinked.oidc_provider_id = Set(Some(other_provider.id));
        let verified = relinked.update(db.db.as_ref()).await.unwrap();
        assert!(oidc.revoke_managed_cloud_provider().await.unwrap());
        assert!(matches!(
            auth.create_session_after_mfa(&verified, origin.as_deref())
                .await,
            Err(AuthError::Unauthorized(_))
        ));
        assert_eq!(auth.count_active_sessions(user.id).await.unwrap(), 0);
        assert_eq!(
            sessions::Entity::find()
                .filter(sessions::Column::UserId.eq(user.id))
                .count(db.db.as_ref())
                .await
                .unwrap(),
            0,
            "revocation removes bound pending and full sessions after user relinking"
        );

        // A password challenge keeps its own authentication semantics.
        let local = create_test_user(&db.db, "local-mfa@example.com", "Password123!").await;
        assert!(auth
            .create_session_after_mfa(&local, Some("password"))
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn test_create_session() {
        let (db, auth_service, _) = setup_test_env().await;
        let user = create_test_user(&db.db, "test@example.com", "password").await;

        let session_token = auth_service.create_session(user.id).await.unwrap();

        assert!(!session_token.is_empty());
        assert_eq!(session_token.len(), 64); // Session token should be 64 chars

        // Verify session was saved to database
        let session = sessions::Entity::find()
            .filter(sessions::Column::SessionToken.eq(&session_token))
            .one(db.db.as_ref())
            .await
            .unwrap()
            .unwrap();

        assert_eq!(session.user_id, user.id);
        assert!(session.expires_at > Utc::now());
    }

    #[tokio::test]
    async fn test_verify_session_valid() {
        let (_db, auth_service, _) = setup_test_env().await;
        let user = create_test_user(&auth_service.db, "test@example.com", "password").await;

        let session_token = auth_service.create_session(user.id).await.unwrap();
        let verified_user = auth_service.verify_session(&session_token).await.unwrap();

        assert_eq!(verified_user.id, user.id);
        assert_eq!(verified_user.email, user.email);
    }

    #[tokio::test]
    async fn test_verify_session_invalid() {
        let (_db, auth_service, _) = setup_test_env().await;

        let result = auth_service.verify_session("invalid_token").await;

        assert!(result.is_err());
        matches!(result.unwrap_err(), AuthError::NotFound(_));
    }

    #[tokio::test]
    async fn test_verify_session_expired_is_rejected() {
        let (db, auth_service, _) = setup_test_env().await;
        let user = create_test_user(&db.db, "expired@example.com", "password").await;

        // Manually insert an expired session (expired 1 hour ago)
        let expired_token = "expired_session_token_1234567890abcdef1234567890abcdef1234567890ab";
        let expired_session = sessions::ActiveModel {
            user_id: Set(user.id),
            session_token: Set(expired_token.to_string()),
            expires_at: Set(Utc::now() - Duration::hours(1)),
            ..Default::default()
        };
        expired_session.insert(db.db.as_ref()).await.unwrap();

        // Verify that the expired session is rejected
        let result = auth_service.verify_session(expired_token).await;
        assert!(result.is_err(), "Expired sessions must be rejected");
        assert!(
            matches!(result.unwrap_err(), AuthError::NotFound(_)),
            "Expired session should return NotFound error"
        );
    }

    #[tokio::test]
    async fn test_verify_session_just_expired_is_rejected() {
        let (db, auth_service, _) = setup_test_env().await;
        let user = create_test_user(&db.db, "justexpired@example.com", "password").await;

        // Insert a session that expired just 1 second ago
        let token = "justexpired_token_1234567890abcdef1234567890abcdef1234567890abcde";
        let session = sessions::ActiveModel {
            user_id: Set(user.id),
            session_token: Set(token.to_string()),
            expires_at: Set(Utc::now() - Duration::seconds(1)),
            ..Default::default()
        };
        session.insert(db.db.as_ref()).await.unwrap();

        let result = auth_service.verify_session(token).await;
        assert!(
            result.is_err(),
            "Even recently expired sessions must be rejected"
        );
    }

    #[tokio::test]
    async fn test_verify_session_not_yet_expired_is_accepted() {
        let (db, auth_service, _) = setup_test_env().await;
        let user = create_test_user(&db.db, "valid@example.com", "password").await;

        // Create a normal session (valid for 7 days)
        let session_token = auth_service.create_session(user.id).await.unwrap();

        // Session should be valid
        let verified_user = auth_service.verify_session(&session_token).await.unwrap();
        assert_eq!(verified_user.id, user.id);
    }

    /// Regression: an MFA challenge token (first factor only) must never
    /// authenticate a real request. Before the `mfa_pending` discriminator,
    /// `create_mfa_session` inserted an ordinary `sessions` row and
    /// `verify_session` accepted it, so replaying the `mfa_session` cookie as
    /// the `session` cookie yielded a fully authenticated context without the
    /// second factor.
    #[tokio::test]
    async fn test_mfa_pending_session_cannot_authenticate_via_verify_session() {
        let Some((db, auth_service)) = setup_test_env_with_mfa_setting(false).await else {
            return;
        };
        let user = create_test_user(&db.db, "mfabypass@example.com", "password").await;

        // Token handed to the client as the `mfa_session` cookie.
        let mfa_token = auth_service
            .create_mfa_session(user.id, "password")
            .await
            .unwrap();

        // The attack: replay it on the normal session path.
        let result = auth_service.verify_session(&mfa_token).await;
        assert!(
            result.is_err(),
            "MFA challenge token must NOT authenticate a real request"
        );
        assert!(
            matches!(result.unwrap_err(), AuthError::NotFound(_)),
            "MFA challenge token should be indistinguishable from a missing session"
        );

        // Sanity: a genuine session on the same user still authenticates.
        let real_token = auth_service.create_session(user.id).await.unwrap();
        assert_eq!(
            auth_service.verify_session(&real_token).await.unwrap().id,
            user.id
        );
    }

    /// Regression (symmetric): a real, fully authenticated session token must
    /// not be consumable as an MFA challenge. `verify_mfa_challenge` filters on
    /// `mfa_pending = true`, so a real token is rejected before the TOTP code
    /// is ever checked.
    #[tokio::test]
    async fn test_real_session_cannot_be_used_as_mfa_challenge() {
        let Some((db, auth_service)) = setup_test_env_with_mfa_setting(false).await else {
            return;
        };
        let user = create_test_user(&db.db, "notachallenge@example.com", "password").await;

        let real_token = auth_service.create_session(user.id).await.unwrap();

        let result = auth_service
            .verify_mfa_challenge(&real_token, "000000")
            .await;
        assert!(
            matches!(result, Err(MfaChallengeError::InvalidOrExpiredSession)),
            "A real session token must be rejected before TOTP verification, got: {result:?}"
        );
    }

    /// A pending MFA challenge row is not a real, active session, so it must
    /// not inflate the concurrent-session count used for login auditing.
    #[tokio::test]
    async fn test_mfa_pending_session_not_counted_as_active() {
        let Some((db, auth_service)) = setup_test_env_with_mfa_setting(false).await else {
            return;
        };
        let user = create_test_user(&db.db, "activecount@example.com", "password").await;

        auth_service
            .create_mfa_session(user.id, "password")
            .await
            .unwrap();
        assert_eq!(
            auth_service.count_active_sessions(user.id).await.unwrap(),
            0,
            "A pending MFA challenge must not count as an active session"
        );

        auth_service.create_session(user.id).await.unwrap();
        assert_eq!(
            auth_service.count_active_sessions(user.id).await.unwrap(),
            1,
            "Only the real session should be counted"
        );
    }

    #[tokio::test]
    async fn test_create_session_cookie() {
        let (_db, auth_service, _) = setup_test_env().await;

        let session_token = "test_session_token";
        let headers = auth_service.create_session_cookie(session_token, true);

        let cookies: Vec<_> = headers.get_all(SET_COOKIE).iter().collect();
        assert_eq!(cookies.len(), 2); // session and mfa_session cookies

        let session_cookie = cookies[0].to_str().unwrap();
        assert!(session_cookie.contains("session=test_session_token"));
        assert!(session_cookie.contains("HttpOnly"));
        assert!(session_cookie.contains("Secure"));
    }

    // Regression: verify_mfa_challenge handler must `append` (not `insert`) the
    // mfa_session clear cookie onto the headers returned by `create_session_cookie`.
    // Using `insert` overwrites all existing Set-Cookie headers, dropping the new
    // session cookie and leaving the user stuck on the login page after MFA.
    #[tokio::test]
    async fn test_verify_mfa_handler_cookie_merge_preserves_session_cookie() {
        let (_db, auth_service, _) = setup_test_env().await;

        let mut response_headers = auth_service.create_session_cookie("session_tok", true);
        let pre_count = response_headers.get_all(SET_COOKIE).iter().count();
        assert_eq!(pre_count, 2, "session_cookie + mfa_session clear");

        // Simulate the exact merge done in verify_mfa_challenge handler:
        // append a fresh mfa_session=; Max-Age=0 cookie.
        let clear_mfa_cookie = Cookie::build(("mfa_session", ""))
            .http_only(true)
            .path("/")
            .max_age(cookie::time::Duration::seconds(0))
            .same_site(cookie::SameSite::Strict)
            .secure(true)
            .build();
        response_headers.append(SET_COOKIE, clear_mfa_cookie.to_string().parse().unwrap());

        let cookies: Vec<String> = response_headers
            .get_all(SET_COOKIE)
            .iter()
            .map(|v| v.to_str().unwrap().to_string())
            .collect();

        // The session cookie MUST still be present alongside the mfa_session clear.
        assert!(
            cookies.iter().any(|c| c.contains("session=session_tok")),
            "session cookie was clobbered: {:?}",
            cookies,
        );
        assert!(
            cookies.iter().any(|c| c.contains("mfa_session=")),
            "mfa_session clear cookie missing: {:?}",
            cookies,
        );
    }

    #[tokio::test]
    async fn test_create_session_cookie_does_not_panic() {
        // Verify that cookie creation never panics, even with unusual tokens.
        // Previously used .unwrap() which could panic on malformed values.
        let (_db, auth_service, _) = setup_test_env().await;

        // Normal token
        let _ = auth_service.create_session_cookie("normal_token_value", true);

        // Empty token
        let _ = auth_service.create_session_cookie("", false);

        // Long token
        let long_token = "a".repeat(1000);
        let _ = auth_service.create_session_cookie(&long_token, true);

        // Token with special characters (should not panic)
        let _ = auth_service.create_session_cookie("token-with-dashes_and_underscores", false);
    }

    #[tokio::test]
    async fn test_logout() {
        let (db, auth_service, _) = setup_test_env().await;
        let user = create_test_user(&db.db, "test@example.com", "password").await;

        // Create session
        let session_token = auth_service.create_session(user.id).await.unwrap();

        // Logout
        let headers = HeaderMap::new();
        auth_service.logout(user.id, &headers).await.unwrap();

        // Verify session was deleted
        let session = sessions::Entity::find()
            .filter(sessions::Column::SessionToken.eq(&session_token))
            .one(db.db.as_ref())
            .await
            .unwrap();

        assert!(session.is_none());
    }

    // User Registration Tests

    #[tokio::test]
    async fn test_register_user_success() {
        let (_db, auth_service, email_service) = setup_test_env().await;

        let request = RegisterRequest {
            email: "newuser@example.com".to_string(),
            password: "SecurePassword123!".to_string(),
            name: "New User".to_string(),
        };

        let user = auth_service.register_user(request).await.unwrap();

        assert_eq!(user.email, "newuser@example.com");
        assert_eq!(user.name, "New User");
        assert!(!user.email_verified);
        assert!(user.password_hash.is_some());
        assert!(user.email_verification_token.is_some());

        // Verify email was sent
        let emails = email_service.get_verification_emails();
        assert_eq!(emails.len(), 1);
        assert_eq!(emails[0].0, "newuser@example.com");
    }

    #[tokio::test]
    async fn test_register_user_duplicate_email() {
        let (db, auth_service, _) = setup_test_env().await;
        create_test_user(&db.db, "existing@example.com", "password").await;

        let request = RegisterRequest {
            email: "existing@example.com".to_string(),
            password: "password123".to_string(),
            name: "Another User".to_string(),
        };

        let result = auth_service.register_user(request).await;

        assert!(result.is_err());
        matches!(result.unwrap_err(), UserAuthError::EmailAlreadyRegistered);
    }

    #[tokio::test]
    async fn test_register_user_case_insensitive_email() {
        let (_db, auth_service, _) = setup_test_env().await;

        let request1 = RegisterRequest {
            email: "Test@Example.Com".to_string(),
            password: "Password123!".to_string(),
            name: "Test User".to_string(),
        };

        let user = auth_service.register_user(request1).await.unwrap();
        assert_eq!(user.email, "test@example.com"); // Should be lowercase

        let request2 = RegisterRequest {
            email: "TEST@EXAMPLE.COM".to_string(),
            password: "Password456!".to_string(),
            name: "Another User".to_string(),
        };

        let result = auth_service.register_user(request2).await;
        assert!(result.is_err()); // Should fail due to duplicate
    }

    // Login Tests

    #[tokio::test]
    async fn test_login_success() {
        let (db, auth_service, _) = setup_test_env().await;
        create_test_user(&db.db, "user@example.com", "correctpassword").await;

        let request = LoginRequest {
            email: "user@example.com".to_string(),
            password: "correctpassword".to_string(),
        };

        let user = auth_service.login(request).await.unwrap();

        assert_eq!(user.email, "user@example.com");
    }

    #[tokio::test]
    async fn test_login_wrong_password() {
        let (db, auth_service, _) = setup_test_env().await;
        create_test_user(&db.db, "user@example.com", "correctpassword").await;

        let request = LoginRequest {
            email: "user@example.com".to_string(),
            password: "wrongpassword".to_string(),
        };

        let result = auth_service.login(request).await;

        assert!(result.is_err());
        matches!(result.unwrap_err(), UserAuthError::InvalidCredentials);
    }

    #[tokio::test]
    async fn test_login_nonexistent_user() {
        let (_db, auth_service, _) = setup_test_env().await;

        let request = LoginRequest {
            email: "nonexistent@example.com".to_string(),
            password: "password".to_string(),
        };

        let result = auth_service.login(request).await;

        assert!(result.is_err());
        matches!(result.unwrap_err(), UserAuthError::InvalidCredentials);
    }

    #[tokio::test]
    async fn test_login_case_insensitive() {
        let (db, auth_service, _) = setup_test_env().await;
        create_test_user(&db.db, "user@example.com", "password").await;

        let request = LoginRequest {
            email: "USER@EXAMPLE.COM".to_string(), // Uppercase
            password: "password".to_string(),
        };

        let user = auth_service.login(request).await.unwrap();
        assert_eq!(user.email, "user@example.com");
    }

    // Password Reset Tests

    #[tokio::test]
    async fn test_request_password_reset_existing_user() {
        let (db, auth_service, email_service) = setup_test_env().await;
        let user = create_test_user(&db.db, "user@example.com", "oldpassword").await;

        auth_service
            .request_password_reset("user@example.com")
            .await
            .unwrap();

        // Verify reset token was saved
        let updated_user = users::Entity::find_by_id(user.id)
            .one(db.db.as_ref())
            .await
            .unwrap()
            .unwrap();

        assert!(updated_user.password_reset_token.is_some());
        assert!(updated_user.password_reset_expires.is_some());

        // Verify email was sent
        let emails = email_service.get_password_reset_emails();
        assert_eq!(emails.len(), 1);
        assert_eq!(emails[0].0, "user@example.com");
    }

    #[tokio::test]
    async fn test_request_password_reset_nonexistent_user() {
        let (_db, auth_service, email_service) = setup_test_env().await;

        // Should not error to prevent email enumeration
        auth_service
            .request_password_reset("nonexistent@example.com")
            .await
            .unwrap();

        // No email should be sent
        let emails = email_service.get_password_reset_emails();
        assert_eq!(emails.len(), 0);
    }

    #[tokio::test]
    async fn test_reset_password_valid_token() {
        let (db, auth_service, _) = setup_test_env().await;
        let user = create_test_user(&db.db, "user@example.com", "oldpassword").await;

        // Set reset token
        let reset_token = Uuid::new_v4().to_string();
        let mut user_update: users::ActiveModel = user.clone().into();
        user_update.password_reset_token = Set(Some(reset_token.clone()));
        user_update.password_reset_expires = Set(Some(Utc::now() + Duration::hours(1)));
        user_update.must_change_password = Set(true);
        user_update.update(db.db.as_ref()).await.unwrap();

        // Reset password
        let request = ResetPasswordRequest {
            token: reset_token,
            new_password: "newSecurePassword123!".to_string(),
        };

        auth_service.reset_password(request).await.unwrap();

        // Verify password was changed and token cleared
        let updated_user = users::Entity::find_by_id(user.id)
            .one(db.db.as_ref())
            .await
            .unwrap()
            .unwrap();

        assert!(updated_user.password_reset_token.is_none());
        assert!(updated_user.password_reset_expires.is_none());
        assert!(!updated_user.must_change_password);

        // Verify new password works
        let login = LoginRequest {
            email: "user@example.com".to_string(),
            password: "newSecurePassword123!".to_string(),
        };
        auth_service.login(login).await.unwrap();
    }

    /// Issue #1078: a user who lost their password (no email provider
    /// configured) is recovered by an admin reset. The generated temporary
    /// password must sign them in only as far as the forced password change,
    /// revoke every existing session, invalidate any pending reset token, and
    /// be rejected as the "new" password.
    #[tokio::test]
    async fn admin_password_reset_forces_change_at_next_sign_in() {
        let (db, auth_service, _) = setup_test_env().await;
        let user = create_test_user(&db.db, "locked-out@example.com", "Forgotten123!").await;

        let session_token = auth_service.create_session(user.id).await.unwrap();
        let mut user_update: users::ActiveModel = user.clone().into();
        user_update.password_reset_token = Set(Some("stale-email-reset-token".to_string()));
        user_update.password_reset_expires = Set(Some(Utc::now() + Duration::hours(1)));
        user_update.update(db.db.as_ref()).await.unwrap();

        let user_service = crate::user_service::UserService::new(db.db.clone());
        let (updated, temporary_password) =
            user_service.admin_reset_password(user.id).await.unwrap();

        assert!(updated.must_change_password);
        assert!(updated.password_reset_token.is_none());
        assert!(updated.password_reset_expires.is_none());
        validate_password_complexity(&temporary_password).unwrap();

        let remaining_sessions = sessions::Entity::find()
            .filter(sessions::Column::UserId.eq(user.id))
            .count(db.db.as_ref())
            .await
            .unwrap();
        assert_eq!(remaining_sessions, 0, "existing sessions must be revoked");
        assert!(sessions::Entity::find()
            .filter(sessions::Column::SessionToken.eq(&session_token))
            .one(db.db.as_ref())
            .await
            .unwrap()
            .is_none());

        // The old password no longer works; the temporary one does, but only
        // reaches the forced-change state.
        let old_login = auth_service
            .login(LoginRequest {
                email: "locked-out@example.com".to_string(),
                password: "Forgotten123!".to_string(),
            })
            .await;
        assert!(matches!(old_login, Err(UserAuthError::InvalidCredentials)));

        let logged_in = auth_service
            .login(LoginRequest {
                email: "locked-out@example.com".to_string(),
                password: temporary_password.clone(),
            })
            .await
            .unwrap();
        assert!(logged_in.must_change_password);
        assert!(matches!(
            auth_service.create_session(user.id).await,
            Err(AuthError::PasswordChangeRequired { .. })
        ));

        // The stale email-reset token was cleared by the admin reset.
        let stale = auth_service
            .reset_password(ResetPasswordRequest {
                token: "stale-email-reset-token".to_string(),
                new_password: "Attacker123!".to_string(),
            })
            .await;
        assert!(matches!(stale, Err(UserAuthError::InvalidToken)));

        let change_token = auth_service
            .create_required_password_change_token(&logged_in)
            .await
            .unwrap();
        let reuse = auth_service
            .reset_required_password(ResetPasswordRequest {
                token: change_token.clone(),
                new_password: temporary_password.clone(),
            })
            .await;
        assert!(matches!(reuse, Err(UserAuthError::SamePassword)));

        let changed = auth_service
            .reset_required_password(ResetPasswordRequest {
                token: change_token,
                new_password: "MyOwnPassword456!".to_string(),
            })
            .await
            .unwrap();
        assert!(!changed.must_change_password);
        auth_service.create_session(user.id).await.unwrap();
    }

    /// A login that verified a temporary password just before an admin reset
    /// replaced it must not be able to mint a password-change token with the
    /// superseded credential.
    #[tokio::test]
    async fn superseded_temporary_password_cannot_mint_change_token() {
        let (db, auth_service, _) = setup_test_env().await;
        let user = create_test_user(&db.db, "raced-token@example.com", "Temporary123!").await;
        let mut user_update: users::ActiveModel = user.clone().into();
        user_update.must_change_password = Set(true);
        user_update.update(db.db.as_ref()).await.unwrap();

        // The login verified "Temporary123!" and holds this snapshot...
        let verified = auth_service
            .login(LoginRequest {
                email: "raced-token@example.com".to_string(),
                password: "Temporary123!".to_string(),
            })
            .await
            .unwrap();

        // ...when an admin reset replaces the password.
        crate::user_service::UserService::new(db.db.clone())
            .admin_reset_password(user.id)
            .await
            .unwrap();

        let result = auth_service
            .create_required_password_change_token(&verified)
            .await;
        assert!(matches!(
            result,
            Err(UserAuthError::CredentialsChanged { user_id }) if user_id == user.id
        ));
        let stored = users::Entity::find_by_id(user.id)
            .one(db.db.as_ref())
            .await
            .unwrap()
            .unwrap();
        assert!(stored.password_reset_token.is_none());
    }

    /// Session creation must serialize with an admin reset. With a reset in
    /// flight (exclusive lock held, account flagged, sessions deleted), a
    /// concurrent `create_session` has to wait for it and then see the flag,
    /// instead of inserting a session after the reset's delete.
    #[tokio::test]
    async fn create_session_waits_for_in_flight_password_reset() {
        let (db, auth_service, _) = setup_test_env().await;
        let user = create_test_user(&db.db, "raced-session@example.com", "Original123!").await;
        let auth_service = Arc::new(auth_service);

        let reset = db.db.begin().await.unwrap();
        let locked = users::Entity::find_by_id(user.id)
            .lock_exclusive()
            .one(&reset)
            .await
            .unwrap()
            .unwrap();

        let racing_login = {
            let auth_service = auth_service.clone();
            tokio::spawn(async move { auth_service.create_session(user.id).await })
        };
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert!(
            !racing_login.is_finished(),
            "create_session must block on the reset's row lock"
        );

        let mut flagged: users::ActiveModel = locked.into();
        flagged.must_change_password = Set(true);
        flagged.update(&reset).await.unwrap();
        sessions::Entity::delete_many()
            .filter(sessions::Column::UserId.eq(user.id))
            .exec(&reset)
            .await
            .unwrap();
        reset.commit().await.unwrap();

        let result = racing_login.await.unwrap();
        assert!(matches!(
            result,
            Err(AuthError::PasswordChangeRequired { user_id }) if user_id == user.id
        ));
        let surviving = sessions::Entity::find()
            .filter(sessions::Column::UserId.eq(user.id))
            .count(db.db.as_ref())
            .await
            .unwrap();
        assert_eq!(surviving, 0);
    }

    /// MFA enrolment holds the user row exclusively while it enables MFA and
    /// revokes other sessions. A password sign-in that checked the password
    /// before the enrolment committed waits on that lock, then must refuse to
    /// write a first-factor-only session.
    #[tokio::test]
    async fn create_session_refuses_a_first_factor_session_once_mfa_is_enabled() {
        let (db, auth_service, _) = setup_test_env().await;
        let user = create_test_user(&db.db, "raced-enrolment@example.com", "Original123!").await;
        let auth_service = Arc::new(auth_service);

        let enrolment = db.db.begin().await.unwrap();
        let locked = users::Entity::find_by_id(user.id)
            .lock_exclusive()
            .one(&enrolment)
            .await
            .unwrap()
            .unwrap();

        let racing_login = {
            let auth_service = auth_service.clone();
            tokio::spawn(async move { auth_service.create_session(user.id).await })
        };
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert!(
            !racing_login.is_finished(),
            "create_session must block on the enrolment's row lock"
        );

        let mut enrolled: users::ActiveModel = locked.into();
        enrolled.mfa_enabled = Set(true);
        enrolled.mfa_secret = Set(Some(HARDENING_SECRET.to_string()));
        enrolled.update(&enrolment).await.unwrap();
        sessions::Entity::delete_many()
            .filter(sessions::Column::UserId.eq(user.id))
            .exec(&enrolment)
            .await
            .unwrap();
        enrolment.commit().await.unwrap();

        let result = racing_login.await.unwrap();
        assert!(
            matches!(result, Err(AuthError::MfaRequired { user_id }) if user_id == user.id),
            "a password-only session must not outlive the enrolment: {result:?}"
        );
        let surviving = sessions::Entity::find()
            .filter(sessions::Column::UserId.eq(user.id))
            .count(db.db.as_ref())
            .await
            .unwrap();
        assert_eq!(surviving, 0);

        let enrolled = users::Entity::find_by_id(user.id)
            .one(db.db.as_ref())
            .await
            .unwrap()
            .unwrap();
        auth_service
            .create_session_after_mfa(&enrolled, Some("password"))
            .await
            .expect("a session after a completed MFA challenge is still issued");
    }

    #[tokio::test]
    async fn email_reset_rejects_temporary_password_reuse() {
        let (db, auth_service, _) = setup_test_env().await;
        let user =
            create_test_user(&db.db, "forced-email-reset@example.com", "Temporary123!").await;
        let reset_token = Uuid::new_v4().to_string();
        let mut user_update: users::ActiveModel = user.clone().into();
        user_update.password_reset_token = Set(Some(reset_token.clone()));
        user_update.password_reset_expires = Set(Some(Utc::now() + Duration::hours(1)));
        user_update.must_change_password = Set(true);
        user_update.update(db.db.as_ref()).await.unwrap();

        let result = auth_service
            .reset_password(ResetPasswordRequest {
                token: reset_token.clone(),
                new_password: "Temporary123!".to_string(),
            })
            .await;

        assert!(matches!(result, Err(UserAuthError::SamePassword)));
        let unchanged = users::Entity::find_by_id(user.id)
            .one(db.db.as_ref())
            .await
            .unwrap()
            .unwrap();
        assert!(unchanged.must_change_password);
        assert_eq!(
            unchanged.password_reset_token.as_deref(),
            Some(reset_token.as_str())
        );
    }

    #[tokio::test]
    async fn required_password_change_rejects_temporary_password_reuse() {
        let (db, auth_service, _) = setup_test_env().await;
        let user = create_test_user(&db.db, "forced-change@example.com", "Temporary123!").await;
        let reset_token = Uuid::new_v4().to_string();
        let mut user_update: users::ActiveModel = user.clone().into();
        user_update.password_reset_token = Set(Some(reset_token.clone()));
        user_update.password_reset_expires = Set(Some(Utc::now() + Duration::minutes(15)));
        user_update.must_change_password = Set(true);
        user_update.update(db.db.as_ref()).await.unwrap();

        let result = auth_service
            .reset_required_password(ResetPasswordRequest {
                token: reset_token.clone(),
                new_password: "Temporary123!".to_string(),
            })
            .await;

        assert!(matches!(result, Err(UserAuthError::SamePassword)));
        let unchanged = users::Entity::find_by_id(user.id)
            .one(db.db.as_ref())
            .await
            .unwrap()
            .unwrap();
        assert!(unchanged.must_change_password);
        assert_eq!(
            unchanged.password_reset_token.as_deref(),
            Some(reset_token.as_str())
        );
    }

    #[tokio::test]
    async fn required_password_change_consumes_token_and_allows_session_creation() {
        let (db, auth_service, _) = setup_test_env().await;
        let user =
            create_test_user(&db.db, "forced-change-success@example.com", "Temporary123!").await;
        let reset_token = Uuid::new_v4().to_string();
        let mut user_update: users::ActiveModel = user.clone().into();
        user_update.password_reset_token = Set(Some(reset_token.clone()));
        user_update.password_reset_expires = Set(Some(Utc::now() + Duration::minutes(15)));
        user_update.must_change_password = Set(true);
        user_update.update(db.db.as_ref()).await.unwrap();

        let updated = auth_service
            .reset_required_password(ResetPasswordRequest {
                token: reset_token.clone(),
                new_password: "DifferentPassword123!".to_string(),
            })
            .await
            .unwrap();

        assert!(!updated.must_change_password);
        assert!(updated.password_reset_token.is_none());
        assert!(auth_service.create_session(user.id).await.is_ok());
        assert!(matches!(
            auth_service
                .reset_required_password(ResetPasswordRequest {
                    token: reset_token,
                    new_password: "AnotherPassword123!".to_string(),
                })
                .await,
            Err(UserAuthError::InvalidToken)
        ));
    }

    #[tokio::test]
    async fn required_password_change_token_is_single_use_under_concurrency() {
        let (db, auth_service, _) = setup_test_env().await;
        let user =
            create_test_user(&db.db, "forced-change-race@example.com", "Temporary123!").await;
        let reset_token = Uuid::new_v4().to_string();
        let mut user_update: users::ActiveModel = user.into();
        user_update.password_reset_token = Set(Some(reset_token.clone()));
        user_update.password_reset_expires = Set(Some(Utc::now() + Duration::minutes(15)));
        user_update.must_change_password = Set(true);
        user_update.update(db.db.as_ref()).await.unwrap();

        let first = auth_service.reset_required_password(ResetPasswordRequest {
            token: reset_token.clone(),
            new_password: "FirstReplacement123!".to_string(),
        });
        let second = auth_service.reset_required_password(ResetPasswordRequest {
            token: reset_token,
            new_password: "SecondReplacement123!".to_string(),
        });
        let (first_result, second_result) = tokio::join!(first, second);

        assert_ne!(first_result.is_ok(), second_result.is_ok());
        let rejected = if first_result.is_err() {
            first_result
        } else {
            second_result
        };
        assert!(matches!(rejected, Err(UserAuthError::InvalidToken)));
    }

    #[tokio::test]
    async fn full_session_is_rejected_while_password_change_is_required() {
        let (db, auth_service, _) = setup_test_env().await;
        let user = create_test_user(&db.db, "no-session@example.com", "Temporary123!").await;
        let mut user_update: users::ActiveModel = user.clone().into();
        user_update.must_change_password = Set(true);
        user_update.update(db.db.as_ref()).await.unwrap();

        let result = auth_service.create_session(user.id).await;

        assert!(matches!(
            result,
            Err(AuthError::PasswordChangeRequired { user_id }) if user_id == user.id
        ));
        assert_eq!(
            sessions::Entity::find()
                .filter(sessions::Column::UserId.eq(user.id))
                .count(db.db.as_ref())
                .await
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn required_password_change_token_is_short_lived_and_only_for_flagged_users() {
        let (db, auth_service, _) = setup_test_env().await;
        let user = create_test_user(&db.db, "temporary@example.com", "Temporary1!").await;

        let result = auth_service
            .create_required_password_change_token(&user)
            .await;
        assert!(matches!(
            result,
            Err(UserAuthError::PasswordChangeNotRequired { user_id }) if user_id == user.id
        ));

        let mut user_update: users::ActiveModel = user.clone().into();
        user_update.must_change_password = Set(true);
        user_update.update(db.db.as_ref()).await.unwrap();

        let issued_at = Utc::now();
        let token = auth_service
            .create_required_password_change_token(&user)
            .await
            .unwrap();
        let updated_user = users::Entity::find_by_id(user.id)
            .one(db.db.as_ref())
            .await
            .unwrap()
            .unwrap();

        assert_eq!(
            updated_user.password_reset_token.as_deref(),
            Some(token.as_str())
        );
        let expires_at = updated_user
            .password_reset_expires
            .expect("required password-change token has an expiry");
        assert!(expires_at > issued_at + Duration::minutes(14));
        assert!(expires_at <= issued_at + Duration::minutes(16));
        assert!(updated_user.must_change_password);
    }

    #[tokio::test]
    async fn test_reset_password_expired_token() {
        let (db, auth_service, _) = setup_test_env().await;
        let user = create_test_user(&db.db, "user@example.com", "oldpassword").await;

        // Set expired reset token
        let reset_token = Uuid::new_v4().to_string();
        let mut user_update: users::ActiveModel = user.into();
        user_update.password_reset_token = Set(Some(reset_token.clone()));
        user_update.password_reset_expires = Set(Some(Utc::now() - Duration::hours(1)));
        user_update.update(db.db.as_ref()).await.unwrap();

        let request = ResetPasswordRequest {
            token: reset_token,
            new_password: "newpassword".to_string(),
        };

        let result = auth_service.reset_password(request).await;

        assert!(result.is_err());
        matches!(result.unwrap_err(), UserAuthError::InvalidToken);
    }

    // Email Verification Tests

    #[tokio::test]
    async fn test_verify_email_valid_token() {
        let (db, auth_service, _) = setup_test_env().await;

        // Create unverified user
        let verification_token = Uuid::new_v4().to_string();
        let user = users::ActiveModel {
            email: Set("unverified@example.com".to_string()),
            name: Set("Unverified User".to_string()),
            email_verified: Set(false),
            email_verification_token: Set(Some(verification_token.clone())),
            email_verification_expires: Set(Some(Utc::now() + Duration::hours(24))),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let user = user.insert(db.db.as_ref()).await.unwrap();

        auth_service
            .verify_email(&verification_token)
            .await
            .unwrap();

        // Verify email was marked as verified
        let updated_user = users::Entity::find_by_id(user.id)
            .one(db.db.as_ref())
            .await
            .unwrap()
            .unwrap();

        assert!(updated_user.email_verified);
        assert!(updated_user.email_verification_token.is_none());
        assert!(updated_user.email_verification_expires.is_none());
    }

    #[tokio::test]
    async fn test_verify_email_expired_token() {
        let (db, auth_service, _) = setup_test_env().await;

        // Create user with expired token
        let verification_token = Uuid::new_v4().to_string();
        let user = users::ActiveModel {
            email: Set("expired@example.com".to_string()),
            name: Set("Expired User".to_string()),
            email_verified: Set(false),
            email_verification_token: Set(Some(verification_token.clone())),
            email_verification_expires: Set(Some(Utc::now() - Duration::hours(1))),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        user.insert(db.db.as_ref()).await.unwrap();

        let result = auth_service.verify_email(&verification_token).await;

        assert!(result.is_err());
        matches!(result.unwrap_err(), UserAuthError::InvalidToken);
    }

    // MFA Tests

    #[tokio::test]
    async fn test_create_mfa_session() {
        let (db, auth_service, _) = setup_test_env().await;
        let user = create_test_user(&db.db, "mfa@example.com", "password").await;

        let mfa_session_token = auth_service
            .create_mfa_session(user.id, "password")
            .await
            .unwrap();

        assert!(!mfa_session_token.is_empty());

        // Verify MFA session was created with short expiration
        let session = sessions::Entity::find()
            .filter(sessions::Column::SessionToken.eq(&mfa_session_token))
            .one(db.db.as_ref())
            .await
            .unwrap()
            .unwrap();

        assert_eq!(session.user_id, user.id);

        // MFA sessions should expire in 5 minutes
        let expected_expiry = Utc::now() + Duration::minutes(5);
        let time_diff = (session.expires_at - expected_expiry).num_seconds().abs();
        assert!(time_diff < 2); // Allow 2 seconds of variance
    }

    /// Regression coverage for the SSO+MFA lockout fix: `create_mfa_session`
    /// must persist the given origin, and `mfa_session_origin` must read it
    /// back exactly -- this is the mechanism temps-ee-sso's enforcement
    /// middleware relies on to tell an SSO-originated pending challenge
    /// apart from a password-originated one. Runs it against a real
    /// (Dockerized) database so the new `mfa_pending_origin` column is
    /// proven to round-trip, not just compile against the entity model.
    #[tokio::test]
    async fn test_create_mfa_session_persists_origin_and_mfa_session_origin_reads_it_back() {
        let (db, auth_service, _) = setup_test_env().await;
        let user = create_test_user(&db.db, "mfa-origin@example.com", "password").await;

        let oidc_token = auth_service
            .create_mfa_session(user.id, "oidc")
            .await
            .unwrap();
        let saml_token = auth_service
            .create_mfa_session(user.id, "saml")
            .await
            .unwrap();
        let password_token = auth_service
            .create_mfa_session(user.id, "password")
            .await
            .unwrap();

        assert_eq!(
            auth_service.mfa_session_origin(&oidc_token).await.unwrap(),
            Some("oidc".to_string())
        );
        assert_eq!(
            auth_service.mfa_session_origin(&saml_token).await.unwrap(),
            Some("saml".to_string())
        );
        assert_eq!(
            auth_service
                .mfa_session_origin(&password_token)
                .await
                .unwrap(),
            Some("password".to_string())
        );
    }

    #[tokio::test]
    async fn test_mfa_session_origin_returns_none_for_unknown_token() {
        let (_db, auth_service, _) = setup_test_env().await;

        let origin = auth_service
            .mfa_session_origin("does-not-exist")
            .await
            .unwrap();

        assert_eq!(origin, None);
    }

    #[tokio::test]
    async fn test_mfa_session_origin_returns_none_for_expired_session() {
        let (db, auth_service, _) = setup_test_env().await;
        let user = create_test_user(&db.db, "mfa-origin-expired@example.com", "password").await;

        let token = "expired_mfa_origin_session";
        let session = sessions::ActiveModel {
            user_id: Set(user.id),
            session_token: Set(token.to_string()),
            expires_at: Set(Utc::now() - Duration::minutes(1)),
            mfa_pending: Set(true),
            mfa_pending_origin: Set(Some("oidc".to_string())),
            ..Default::default()
        };
        session.insert(db.db.as_ref()).await.unwrap();

        let origin = auth_service.mfa_session_origin(token).await.unwrap();

        assert_eq!(
            origin, None,
            "an expired pending session must not report an origin"
        );
    }

    /// A fully authenticated (non-pending) session token must never report
    /// an origin either -- `mfa_session_origin` filters on `mfa_pending =
    /// true` exactly like `verify_mfa_challenge`, so a real session token
    /// can never be mistaken for a pending SSO challenge by the enforcement
    /// middleware.
    #[tokio::test]
    async fn test_mfa_session_origin_returns_none_for_real_session() {
        let (db, auth_service, _) = setup_test_env().await;
        let user = create_test_user(&db.db, "mfa-origin-real@example.com", "password").await;

        let real_token = auth_service.create_session(user.id).await.unwrap();

        let origin = auth_service.mfa_session_origin(&real_token).await.unwrap();

        assert_eq!(
            origin, None,
            "a fully authenticated session must not report a pending-MFA origin"
        );
    }

    #[tokio::test]
    async fn test_verify_mfa_challenge_without_secret() {
        let (db, auth_service, _) = setup_test_env().await;
        let user = create_test_user(&db.db, "mfa@example.com", "password").await;

        // Create MFA session
        let mfa_session_token = auth_service
            .create_mfa_session(user.id, "password")
            .await
            .unwrap();

        // Try to verify without MFA secret set
        let result = auth_service
            .verify_mfa_challenge(&mfa_session_token, "123456")
            .await;

        assert!(matches!(
            result,
            Err(MfaChallengeError::VerifierConfiguration { user_id, .. })
                if user_id == user.id
        ));
    }

    fn current_totp_code(secret: &str) -> String {
        let secret_bytes = base32::decode(base32::Alphabet::Rfc4648 { padding: true }, secret)
            .expect("test secret should be valid base32");
        // In totp-rs 6.0.0 the constructor is Builder-based and generate_current()
        // returns Token (implements Display), not Result<String, _>.
        totp_rs::Builder::new()
            .with_algorithm(totp_rs::Algorithm::SHA1)
            .with_digits(6)
            .with_skew(1)
            .with_step_duration(30)
            .with_secret(secret_bytes)
            .build_noncompliant()
            .generate_current()
            .to_string()
    }

    #[tokio::test]
    async fn test_verify_mfa_challenge_distinguishes_invalid_code() {
        let Some((db, auth_service)) = setup_test_env_with_mfa_setting(false).await else {
            return;
        };
        let user =
            create_test_user_with_mfa(&db.db, "wrong-code@example.com", "password", true).await;
        let session_token = auth_service
            .create_mfa_session(user.id, "password")
            .await
            .unwrap();
        let current_code = current_totp_code("JBSWY3DPEHPK3PXPJBSWY3DPEHPK3PXP");
        let wrong_code = if current_code == "000000" {
            "000001"
        } else {
            "000000"
        };

        let result = auth_service
            .verify_mfa_challenge(&session_token, wrong_code)
            .await;

        assert!(matches!(
            result,
            Err(MfaChallengeError::InvalidCode { user_id }) if user_id == user.id
        ));
    }

    #[tokio::test]
    async fn test_verify_mfa_challenge_distinguishes_database_failure() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_errors([sea_orm::DbErr::Custom(
                "challenge database unavailable".to_string(),
            )])
            .into_connection();
        let auth_service = AuthService::new(Arc::new(db), Arc::new(MockEmailService::new()));

        let result = auth_service
            .verify_mfa_challenge("pending-session", "123456")
            .await;

        assert!(matches!(
            result,
            Err(MfaChallengeError::Database {
                operation: "load the pending session",
                user_id: None,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn test_correct_mfa_code_with_consume_failure_is_not_rejected_code() {
        let now = Utc::now();
        let user = users::Model {
            id: 7,
            name: "MFA User".to_string(),
            email: "mfa-user@example.com".to_string(),
            password_hash: None,
            email_verified: true,
            email_verification_token: None,
            email_verification_expires: None,
            password_reset_token: None,
            password_reset_expires: None,
            must_change_password: false,
            deleted_at: None,
            mfa_secret: Some("JBSWY3DPEHPK3PXPJBSWY3DPEHPK3PXP".to_string()),
            mfa_enabled: true,
            mfa_recovery_codes: None,
            oidc_subject: None,
            oidc_provider_id: None,
            created_at: now,
            updated_at: now,
        };
        let session = sessions::Model {
            id: 9,
            user_id: user.id,
            session_token: "pending-session".to_string(),
            expires_at: now + Duration::minutes(5),
            mfa_pending: true,
            step_up_expires_at: None,
            mfa_pending_origin: Some("password".to_string()),
            mfa_failed_attempts: 0,
        };
        // Query order: pending challenge, user (locked), challenge re-read
        // under the user lock, guard row (absent). Exec order: guard upsert
        // recording the accepted step, then the failing challenge delete.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![session.clone()]])
            .append_query_results([vec![user]])
            .append_query_results([vec![session]])
            .append_query_results([Vec::<temps_entities::user_mfa_guards::Model>::new()])
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 7,
                rows_affected: 1,
            }])
            .append_exec_errors([sea_orm::DbErr::Custom("session delete failed".to_string())])
            .into_connection();
        let auth_service = AuthService::new(Arc::new(db), Arc::new(MockEmailService::new()));
        let code = current_totp_code("JBSWY3DPEHPK3PXPJBSWY3DPEHPK3PXP");

        let result = auth_service
            .verify_mfa_challenge("pending-session", &code)
            .await;

        assert!(matches!(
            result,
            Err(MfaChallengeError::Database {
                operation: "consume the verified pending session",
                user_id: Some(7),
                ..
            })
        ));
    }

    #[tokio::test]
    async fn test_verify_mfa_challenge_with_expired_session() {
        let (db, auth_service, _) = setup_test_env().await;
        let user = create_test_user(&db.db, "mfa@example.com", "password").await;

        // Create expired MFA session manually
        let session_token = "expired_mfa_session";
        let session = sessions::ActiveModel {
            user_id: Set(user.id),
            session_token: Set(session_token.to_string()),
            expires_at: Set(Utc::now() - Duration::minutes(1)), // Expired
            ..Default::default()
        };
        session.insert(db.db.as_ref()).await.unwrap();

        let result = auth_service
            .verify_mfa_challenge(session_token, "123456")
            .await;

        assert!(matches!(
            result,
            Err(MfaChallengeError::InvalidOrExpiredSession)
        ));
    }

    // Helper Method Tests

    #[tokio::test]
    async fn test_get_user_by_id() {
        let (db, auth_service, _) = setup_test_env().await;
        let user = create_test_user(&db.db, "getuser@example.com", "password").await;

        let fetched_user = auth_service.get_user_by_id(user.id).await.unwrap();

        assert_eq!(fetched_user.id, user.id);
        assert_eq!(fetched_user.email, user.email);
    }

    #[tokio::test]
    async fn test_get_user_by_id_nonexistent() {
        let (_db, auth_service, _) = setup_test_env().await;

        let result = auth_service.get_user_by_id(999999).await;

        assert!(result.is_err());
        matches!(result.unwrap_err(), AuthError::NotFound(_));
    }

    #[tokio::test]
    async fn test_is_email_configured() {
        let (_db, auth_service, _) = setup_test_env().await;

        // Delegates to the notification service's email-provider check; the
        // test mock reports an email provider is configured.
        assert!(auth_service.is_email_configured().await);
    }

    #[tokio::test]
    async fn test_generate_session_token() {
        let (_db, auth_service, _) = setup_test_env().await;

        let token1 = auth_service.generate_session_token();
        let token2 = auth_service.generate_session_token();

        assert_eq!(token1.len(), 64);
        assert_eq!(token2.len(), 64);
        assert_ne!(token1, token2); // Should be unique
    }

    // === Password complexity validation tests ===

    #[test]
    fn test_password_complexity_valid() {
        // A strong password should pass
        assert!(validate_password_complexity("MyP@ssw0rd").is_ok());
        assert!(validate_password_complexity("Str0ng!Pass").is_ok());
        assert!(validate_password_complexity("C0mpl3x#").is_ok());
        assert!(validate_password_complexity("12345678Aa!").is_ok());
    }

    #[test]
    fn test_password_too_short() {
        let result = validate_password_complexity("Aa1!");
        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), UserAuthError::WeakPassword(msg) if msg.contains("8 characters"))
        );
    }

    #[test]
    fn test_password_too_long() {
        let long_password = format!("Aa1!{}", "x".repeat(128));
        let result = validate_password_complexity(&long_password);
        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), UserAuthError::WeakPassword(msg) if msg.contains("128 characters"))
        );
    }

    #[test]
    fn test_password_no_uppercase() {
        let result = validate_password_complexity("myp@ssw0rd");
        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), UserAuthError::WeakPassword(msg) if msg.contains("uppercase"))
        );
    }

    #[test]
    fn test_password_no_lowercase() {
        let result = validate_password_complexity("MYP@SSW0RD");
        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), UserAuthError::WeakPassword(msg) if msg.contains("lowercase"))
        );
    }

    #[test]
    fn test_password_no_digit() {
        let result = validate_password_complexity("MyP@ssword");
        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), UserAuthError::WeakPassword(msg) if msg.contains("digit"))
        );
    }

    #[test]
    fn test_password_no_special_char() {
        let result = validate_password_complexity("MyPassw0rd");
        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), UserAuthError::WeakPassword(msg) if msg.contains("special"))
        );
    }

    #[test]
    fn test_password_empty() {
        let result = validate_password_complexity("");
        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), UserAuthError::WeakPassword(msg) if msg.contains("8 characters"))
        );
    }

    #[test]
    fn test_password_multibyte_characters_minimum_length() {
        // "Aa1!🦀" is 5 characters (A, a, 1, !, 🦀) but 8 UTF-8 bytes.
        // It must be rejected because it is under the 8 character minimum.
        let result = validate_password_complexity("Aa1!🦀");
        assert!(
            result.is_err(),
            "Password with 5 characters must be rejected even if 8 bytes"
        );
        assert!(
            matches!(result.unwrap_err(), UserAuthError::WeakPassword(msg) if msg.contains("8 characters"))
        );
    }

    #[test]
    fn test_password_only_spaces() {
        let result = validate_password_complexity("        ");
        assert!(result.is_err());
        // Spaces pass the special char check but fail uppercase, lowercase, digit
        assert!(matches!(
            result.unwrap_err(),
            UserAuthError::WeakPassword(_)
        ));
    }

    #[test]
    fn test_password_attacker_common_passwords_rejected() {
        // Common weak passwords that meet some but not all criteria
        assert!(validate_password_complexity("password").is_err()); // no uppercase, digit, special
        assert!(validate_password_complexity("12345678").is_err()); // no uppercase, lowercase, special
        assert!(validate_password_complexity("Password").is_err()); // no digit, special
        assert!(validate_password_complexity("Password1").is_err()); // no special
    }

    // --- Unique email enforcement (bherila/temps#24) ---
    //
    // The DB-level constraint itself (`idx_users_email_unique`, added by
    // `m20250127_000001_add_unique_email_constraint.rs`) already exists and
    // is registered in the `Migrator`. These tests cover the part that was
    // actually missing: mapping the raw `DbErr` a real unique-violation
    // produces back into the friendly, typed `UserAuthError::EmailAlreadyRegistered`
    // instead of leaking a generic `DatabaseError` (which handlers currently
    // turn into an opaque 500).

    #[test]
    fn is_unique_violation_detects_postgres_sqlstate_23505() {
        let err = sea_orm::DbErr::Custom(
            "error returned from database: duplicate key value violates unique constraint \"idx_users_email_unique\" (SQLSTATE 23505)".to_string(),
        );
        assert!(is_unique_violation(&err));
    }

    #[test]
    fn is_unique_violation_detects_record_not_inserted() {
        assert!(is_unique_violation(&sea_orm::DbErr::RecordNotInserted));
    }

    #[test]
    fn is_unique_violation_false_for_unrelated_errors() {
        let err = sea_orm::DbErr::Custom("connection reset by peer".to_string());
        assert!(!is_unique_violation(&err));
    }

    #[test]
    fn is_unique_violation_false_for_unrelated_unique_constraint() {
        // A collision on a *different* unique-constrained column (e.g.
        // api_keys.key_hash) must not be misreported as a duplicate
        // email -- only `idx_users_email_unique` should match.
        let err = sea_orm::DbErr::Custom(
            "error returned from database: duplicate key value violates unique constraint \"api_keys_key_hash_key\" (SQLSTATE 23505)".to_string(),
        );
        assert!(!is_unique_violation(&err));
    }

    #[test]
    fn user_auth_error_from_dberr_maps_unique_violation_to_email_already_registered() {
        let err = sea_orm::DbErr::Custom(
            "duplicate key value violates unique constraint \"idx_users_email_unique\"".to_string(),
        );
        let mapped: UserAuthError = err.into();
        assert!(matches!(mapped, UserAuthError::EmailAlreadyRegistered));
    }

    #[test]
    fn user_auth_error_from_dberr_preserves_other_errors_as_database_error() {
        let err = sea_orm::DbErr::Custom("connection reset by peer".to_string());
        let mapped: UserAuthError = err.into();
        assert!(matches!(mapped, UserAuthError::DatabaseError(_)));
    }

    #[tokio::test]
    async fn register_user_maps_concurrent_duplicate_insert_to_email_already_registered() {
        // Simulates the race register_user's app-level pre-check can't fully
        // close: the SELECT sees no existing user, but a concurrent request
        // wins the INSERT race first and the DB-level unique index rejects
        // this one. `register_user` must surface a friendly
        // `EmailAlreadyRegistered`, not a raw `DatabaseError`.
        use sea_orm::{DatabaseBackend, MockDatabase};

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // 1) `find().filter(email...)` in register_user -> no existing user
            .append_query_results::<users::Model, Vec<_>, _>(vec![vec![]])
            // 2) `new_user.insert(...)` -> unique-violation from the DB
            .append_query_errors(vec![sea_orm::DbErr::Custom(
                "error returned from database: duplicate key value violates unique constraint \"idx_users_email_unique\" (SQLSTATE 23505)".to_string(),
            )])
            .into_connection();

        let notification_service = Arc::new(MockEmailService::new());
        let auth_service = AuthService::new(Arc::new(db), notification_service);

        let result = auth_service
            .register_user(RegisterRequest {
                email: "racer@example.com".to_string(),
                password: "ValidPass123!".to_string(),
                name: "Racer".to_string(),
            })
            .await;

        assert!(matches!(result, Err(UserAuthError::EmailAlreadyRegistered)));
    }

    // --- Concurrent-session detection (bherila/temps#24) ---

    #[tokio::test]
    async fn count_active_sessions_returns_current_count() {
        use sea_orm::{DatabaseBackend, MockDatabase};
        use std::collections::BTreeMap;

        let mut row: BTreeMap<&str, sea_orm::Value> = BTreeMap::new();
        row.insert("num_items", sea_orm::Value::BigInt(Some(2)));

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![row]])
            .into_connection();

        let notification_service = Arc::new(MockEmailService::new());
        let auth_service = AuthService::new(Arc::new(db), notification_service);

        let count = auth_service.count_active_sessions(42).await.unwrap();
        assert_eq!(count, 2);
    }

    #[tokio::test]
    async fn count_active_sessions_propagates_database_error() {
        use sea_orm::{DatabaseBackend, MockDatabase};

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_errors(vec![sea_orm::DbErr::Custom(
                "connection reset by peer".to_string(),
            )])
            .into_connection();

        let notification_service = Arc::new(MockEmailService::new());
        let auth_service = AuthService::new(Arc::new(db), notification_service);

        let result = auth_service.count_active_sessions(42).await;
        assert!(matches!(result, Err(AuthError::DatabaseError { .. })));
    }

    #[tokio::test]
    async fn count_active_sessions_ignores_expired_and_other_users_sessions() {
        let Some((db, auth_service)) = setup_test_env_with_mfa_setting(false).await else {
            return;
        };
        let user = create_test_user(&db.db, "concurrent@example.com", "Password123!").await;
        let other_user = create_test_user(&db.db, "other@example.com", "Password123!").await;

        // No sessions yet.
        assert_eq!(
            auth_service.count_active_sessions(user.id).await.unwrap(),
            0
        );

        // One active session for our user.
        auth_service.create_session(user.id).await.unwrap();
        assert_eq!(
            auth_service.count_active_sessions(user.id).await.unwrap(),
            1
        );

        // An expired session for our user must not be counted.
        let expired_session = sessions::ActiveModel {
            user_id: Set(user.id),
            session_token: Set("expired-token".to_string()),
            expires_at: Set(Utc::now() - Duration::hours(1)),
            ..Default::default()
        };
        expired_session.insert(db.db.as_ref()).await.unwrap();
        assert_eq!(
            auth_service.count_active_sessions(user.id).await.unwrap(),
            1
        );

        // A session belonging to a different user must not be counted.
        auth_service.create_session(other_user.id).await.unwrap();
        assert_eq!(
            auth_service.count_active_sessions(user.id).await.unwrap(),
            1
        );
        assert_eq!(
            auth_service
                .count_active_sessions(other_user.id)
                .await
                .unwrap(),
            1
        );
    }

    // --- MFA-required-for-admins (bherila/temps#32) ---

    #[tokio::test]
    async fn login_allows_mfa_enrolled_admin_when_required() {
        let Some((db, auth_service)) = setup_test_env_with_mfa_setting(true).await else {
            return;
        };
        let user =
            create_test_user_with_mfa(&db.db, "admin-mfa@example.com", "Password123!", true).await;

        let user_service = crate::user_service::UserService::new(db.db.clone());
        user_service.initialize_roles().await.unwrap();
        let admin_role = temps_entities::roles::Entity::find()
            .filter(temps_entities::roles::Column::Name.eq(RoleType::Admin.as_str()))
            .one(db.db.as_ref())
            .await
            .unwrap()
            .unwrap();
        user_service
            .assign_role_to_user(user.id, admin_role.id)
            .await
            .unwrap();

        let result = auth_service
            .login(LoginRequest {
                email: "admin-mfa@example.com".to_string(),
                password: "Password123!".to_string(),
            })
            .await;

        assert!(result.is_ok(), "MFA-enrolled admin must be able to log in");
    }

    #[tokio::test]
    async fn login_blocks_admin_without_mfa_when_required() {
        let Some((db, auth_service)) = setup_test_env_with_mfa_setting(true).await else {
            return;
        };
        let user =
            create_test_user_with_mfa(&db.db, "admin-nomfa@example.com", "Password123!", false)
                .await;

        let user_service = crate::user_service::UserService::new(db.db.clone());
        user_service.initialize_roles().await.unwrap();
        let admin_role = temps_entities::roles::Entity::find()
            .filter(temps_entities::roles::Column::Name.eq(RoleType::Admin.as_str()))
            .one(db.db.as_ref())
            .await
            .unwrap()
            .unwrap();
        user_service
            .assign_role_to_user(user.id, admin_role.id)
            .await
            .unwrap();

        let result = auth_service
            .login(LoginRequest {
                email: "admin-nomfa@example.com".to_string(),
                password: "Password123!".to_string(),
            })
            .await;

        assert!(matches!(
            result,
            Err(UserAuthError::MfaRequiredForRole { user_id, .. }) if user_id == user.id
        ));
    }

    #[tokio::test]
    async fn login_allows_admin_without_mfa_when_setting_disabled() {
        let Some((db, auth_service)) = setup_test_env_with_mfa_setting(false).await else {
            return;
        };
        let user =
            create_test_user_with_mfa(&db.db, "admin-nomfa2@example.com", "Password123!", false)
                .await;

        let user_service = crate::user_service::UserService::new(db.db.clone());
        user_service.initialize_roles().await.unwrap();
        let admin_role = temps_entities::roles::Entity::find()
            .filter(temps_entities::roles::Column::Name.eq(RoleType::Admin.as_str()))
            .one(db.db.as_ref())
            .await
            .unwrap()
            .unwrap();
        user_service
            .assign_role_to_user(user.id, admin_role.id)
            .await
            .unwrap();

        let result = auth_service
            .login(LoginRequest {
                email: "admin-nomfa2@example.com".to_string(),
                password: "Password123!".to_string(),
            })
            .await;

        assert!(
            result.is_ok(),
            "admin without MFA must be allowed to log in when require_mfa_for_admins is disabled"
        );
    }

    #[tokio::test]
    async fn login_never_blocks_non_admin_without_mfa_when_required() {
        let Some((db, auth_service)) = setup_test_env_with_mfa_setting(true).await else {
            return;
        };
        // Deliberately NOT assigned any role -- a plain user account.
        create_test_user_with_mfa(&db.db, "plain-user@example.com", "Password123!", false).await;

        let result = auth_service
            .login(LoginRequest {
                email: "plain-user@example.com".to_string(),
                password: "Password123!".to_string(),
            })
            .await;

        assert!(
            result.is_ok(),
            "non-admin users must never be blocked by require_mfa_for_admins"
        );
    }

    /// A transient failure reading the settings row must never block login
    /// for the whole instance -- it must degrade to the default
    /// (`require_mfa_for_admins: false`) and let a correct-password login
    /// through, exactly like a `count_active_sessions` lookup failure does.
    #[tokio::test]
    async fn login_succeeds_when_settings_lookup_fails() {
        use sea_orm::{DatabaseBackend, MockDatabase};

        let password = "Password123!";
        let argon2 = argon2::Argon2::default();
        let password_hash = argon2
            .hash_password(password.as_bytes())
            .unwrap()
            .to_string();

        let user = users::Model {
            id: 7,
            name: "Settings Outage User".to_string(),
            email: "settings-outage@example.com".to_string(),
            password_hash: Some(password_hash),
            email_verified: true,
            email_verification_token: None,
            email_verification_expires: None,
            password_reset_token: None,
            password_reset_expires: None,
            must_change_password: false,
            deleted_at: None,
            mfa_secret: None,
            mfa_enabled: false,
            mfa_recovery_codes: None,
            oidc_subject: None,
            oidc_provider_id: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // 1) `find().filter(email...)` in login() -> the user above
            .append_query_results(vec![vec![user]])
            // 2) `get_settings()`'s `settings::Entity::find_by_id(1)` -> DB error
            .append_query_errors(vec![sea_orm::DbErr::Custom(
                "connection reset by peer".to_string(),
            )])
            .into_connection();

        let notification_service = Arc::new(MockEmailService::new());
        let auth_service = AuthService::new(Arc::new(db), notification_service);

        let result = auth_service
            .login(LoginRequest {
                email: "settings-outage@example.com".to_string(),
                password: password.to_string(),
            })
            .await;

        assert!(
            result.is_ok(),
            "a settings-lookup failure must not block a correct-password login"
        );
    }

    // ---- Second-factor hardening: replay, throttling, recovery, enrolment ----

    const HARDENING_SECRET: &str = "JBSWY3DPEHPK3PXPJBSWY3DPEHPK3PXP";

    /// A six-digit code that is not valid anywhere in the current skew window.
    fn code_outside_window() -> String {
        let totp = crate::mfa_guard::build_totp(0, HARDENING_SECRET).unwrap();
        let now = Utc::now().timestamp() as u64;
        (0..1_000_000)
            .map(|n| format!("{:06}", n))
            .find(|candidate| totp.check(candidate, now).is_none())
            .unwrap()
    }

    async fn mfa_user_with_recovery(
        db: &Arc<DatabaseConnection>,
        email: &str,
        mfa_enabled: bool,
        recovery_codes: &[&str],
    ) -> users::Model {
        let hashes: Vec<String> = recovery_codes
            .iter()
            .map(|code| {
                argon2::Argon2::default()
                    .hash_password(code.as_bytes())
                    .unwrap()
                    .to_string()
            })
            .collect();
        users::ActiveModel {
            email: Set(email.to_string()),
            name: Set("Second Factor".to_string()),
            password_hash: Set(None),
            email_verified: Set(true),
            mfa_enabled: Set(mfa_enabled),
            mfa_secret: Set(Some(HARDENING_SECRET.to_string())),
            mfa_recovery_codes: Set(Some(serde_json::to_string(&hashes).unwrap())),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap()
    }

    async fn guard_row(
        db: &Arc<DatabaseConnection>,
        user_id: i32,
    ) -> Option<temps_entities::user_mfa_guards::Model> {
        temps_entities::user_mfa_guards::Entity::find_by_id(user_id)
            .one(db.as_ref())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn totp_code_is_rejected_for_a_second_challenge_in_the_same_step() {
        let Some((db, auth_service)) = setup_test_env_with_mfa_setting(false).await else {
            return;
        };
        let user = mfa_user_with_recovery(&db.db, "replay@example.com", true, &[]).await;
        let code = current_totp_code(HARDENING_SECRET);

        let first = auth_service
            .create_mfa_session(user.id, "password")
            .await
            .unwrap();
        auth_service
            .verify_mfa_challenge(&first, &code)
            .await
            .expect("a fresh code signs in");

        let second = auth_service
            .create_mfa_session(user.id, "password")
            .await
            .unwrap();
        let replay = auth_service.verify_mfa_challenge(&second, &code).await;
        assert!(
            matches!(replay, Err(MfaChallengeError::InvalidCode { user_id }) if user_id == user.id),
            "the same TOTP code must not complete a second sign-in: {replay:?}"
        );
        assert!(guard_row(&db.db, user.id)
            .await
            .unwrap()
            .last_totp_step
            .is_some());
    }

    #[tokio::test]
    async fn wrong_codes_discard_the_challenge_and_the_correct_code_is_then_rejected() {
        let Some((db, auth_service)) = setup_test_env_with_mfa_setting(false).await else {
            return;
        };
        let user = mfa_user_with_recovery(&db.db, "exhaust@example.com", true, &[]).await;
        let challenge = auth_service
            .create_mfa_session(user.id, "password")
            .await
            .unwrap();
        let wrong = code_outside_window();

        for attempt in 1..crate::mfa_guard::MAX_FAILED_ATTEMPTS_PER_CHALLENGE {
            let result = auth_service.verify_mfa_challenge(&challenge, &wrong).await;
            assert!(
                matches!(result, Err(MfaChallengeError::InvalidCode { .. })),
                "attempt {attempt} must be a plain rejection: {result:?}"
            );
        }
        let last = auth_service.verify_mfa_challenge(&challenge, &wrong).await;
        assert!(
            matches!(last, Err(MfaChallengeError::ChallengeExhausted { user_id }) if user_id == user.id),
            "the wrong code that uses up the challenge must say so: {last:?}"
        );

        let pending = sessions::Entity::find()
            .filter(sessions::Column::SessionToken.eq(&challenge))
            .one(db.db.as_ref())
            .await
            .unwrap();
        assert!(pending.is_none(), "an exhausted challenge must be deleted");

        let late = auth_service
            .verify_mfa_challenge(&challenge, &current_totp_code(HARDENING_SECRET))
            .await;
        assert!(
            matches!(late, Err(MfaChallengeError::InvalidOrExpiredSession)),
            "the correct code must not revive a discarded challenge: {late:?}"
        );
        assert_eq!(
            guard_row(&db.db, user.id).await.unwrap().failed_attempts,
            crate::mfa_guard::MAX_FAILED_ATTEMPTS_PER_CHALLENGE,
            "failures must survive the rejected transaction"
        );
    }

    #[tokio::test]
    async fn per_user_lockout_applies_across_fresh_challenges() {
        let Some((db, auth_service)) = setup_test_env_with_mfa_setting(false).await else {
            return;
        };
        let user = mfa_user_with_recovery(&db.db, "lockout@example.com", true, &[]).await;
        let wrong = code_outside_window();

        let mut failures = 0;
        while failures < crate::mfa_guard::USER_LOCKOUT_THRESHOLD {
            // Each new challenge is what an attacker gets by re-sending the
            // password, possibly from a different address.
            let challenge = auth_service
                .create_mfa_session(user.id, "password")
                .await
                .unwrap();
            for attempt in 1..=crate::mfa_guard::MAX_FAILED_ATTEMPTS_PER_CHALLENGE {
                if failures == crate::mfa_guard::USER_LOCKOUT_THRESHOLD {
                    break;
                }
                let result = auth_service.verify_mfa_challenge(&challenge, &wrong).await;
                if attempt == crate::mfa_guard::MAX_FAILED_ATTEMPTS_PER_CHALLENGE {
                    assert!(
                        matches!(result, Err(MfaChallengeError::ChallengeExhausted { .. })),
                        "the code that uses up a challenge ends it: {result:?}"
                    );
                } else {
                    assert!(
                        matches!(result, Err(MfaChallengeError::InvalidCode { .. })),
                        "attempt {attempt} is a plain rejection: {result:?}"
                    );
                }
                failures += 1;
            }
        }

        let fresh = auth_service
            .create_mfa_session(user.id, "password")
            .await
            .unwrap();
        let locked = auth_service
            .verify_mfa_challenge(&fresh, &current_totp_code(HARDENING_SECRET))
            .await;
        assert!(
            matches!(
                locked,
                Err(MfaChallengeError::LockedOut { user_id, retry_after_seconds })
                    if user_id == user.id && retry_after_seconds > 0
            ),
            "a correct code must be refused while locked: {locked:?}"
        );
        let guard = guard_row(&db.db, user.id).await.unwrap();
        assert!(guard.locked_until.is_some_and(|until| until > Utc::now()));
        assert_eq!(
            guard.last_totp_step, None,
            "a locked attempt accepts nothing"
        );
    }

    #[tokio::test]
    async fn successful_verification_resets_failure_counters() {
        let Some((db, auth_service)) = setup_test_env_with_mfa_setting(false).await else {
            return;
        };
        let user = mfa_user_with_recovery(&db.db, "reset@example.com", true, &[]).await;
        let challenge = auth_service
            .create_mfa_session(user.id, "password")
            .await
            .unwrap();
        let wrong = code_outside_window();
        for _ in 1..crate::mfa_guard::MAX_FAILED_ATTEMPTS_PER_CHALLENGE {
            assert!(auth_service
                .verify_mfa_challenge(&challenge, &wrong)
                .await
                .is_err());
        }
        assert_eq!(
            guard_row(&db.db, user.id).await.unwrap().failed_attempts,
            crate::mfa_guard::MAX_FAILED_ATTEMPTS_PER_CHALLENGE - 1
        );

        auth_service
            .verify_mfa_challenge(&challenge, &current_totp_code(HARDENING_SECRET))
            .await
            .expect("the correct code still works below the limit");

        let guard = guard_row(&db.db, user.id).await.unwrap();
        assert_eq!(guard.failed_attempts, 0);
        assert_eq!(guard.locked_until, None);
    }

    #[tokio::test]
    async fn recovery_code_signs_in_once() {
        let Some((db, auth_service)) = setup_test_env_with_mfa_setting(false).await else {
            return;
        };
        let user =
            mfa_user_with_recovery(&db.db, "recovery@example.com", true, &["482915", "730264"])
                .await;

        let first = auth_service
            .create_mfa_session(user.id, "password")
            .await
            .unwrap();
        let (signed_in, _) = auth_service
            .verify_mfa_challenge_with_origin(&first, "482915")
            .await
            .expect("an unused recovery code completes sign-in");
        let remaining: Vec<String> =
            serde_json::from_str(signed_in.mfa_recovery_codes.as_deref().unwrap()).unwrap();
        assert_eq!(remaining.len(), 1, "the used code is burned");

        let second = auth_service
            .create_mfa_session(user.id, "password")
            .await
            .unwrap();
        let reuse = auth_service.verify_mfa_challenge(&second, "482915").await;
        assert!(
            matches!(reuse, Err(MfaChallengeError::InvalidCode { .. })),
            "a recovery code is single use: {reuse:?}"
        );
    }

    /// Recovery codes are six digits like TOTP codes, so one value can be
    /// both. It must complete exactly one sign-in, not one per factor.
    #[tokio::test]
    async fn a_value_that_is_both_factors_signs_in_once() {
        let Some((db, auth_service)) = setup_test_env_with_mfa_setting(false).await else {
            return;
        };
        let both = current_totp_code(HARDENING_SECRET);
        let user = mfa_user_with_recovery(&db.db, "both-factors@example.com", true, &[&both]).await;

        let first = auth_service
            .create_mfa_session(user.id, "password")
            .await
            .unwrap();
        let (signed_in, _) = auth_service
            .verify_mfa_challenge_with_origin(&first, &both)
            .await
            .expect("the value signs in once");
        let remaining: Vec<String> =
            serde_json::from_str(signed_in.mfa_recovery_codes.as_deref().unwrap()).unwrap();
        assert!(remaining.is_empty(), "the matching recovery code is burned");
        assert!(guard_row(&db.db, user.id)
            .await
            .unwrap()
            .last_totp_step
            .is_some());

        let second = auth_service
            .create_mfa_session(user.id, "password")
            .await
            .unwrap();
        let again = auth_service.verify_mfa_challenge(&second, &both).await;
        assert!(
            matches!(again, Err(MfaChallengeError::InvalidCode { .. })),
            "the same value must not sign in a second time: {again:?}"
        );
    }

    #[tokio::test]
    async fn recovery_code_cannot_confirm_a_first_time_enrolment() {
        let Some((db, auth_service)) = setup_test_env_with_mfa_setting(false).await else {
            return;
        };
        let user = mfa_user_with_recovery(&db.db, "enrol-rc@example.com", false, &["193746"]).await;
        let challenge = auth_service
            .create_mfa_session(user.id, "password")
            .await
            .unwrap();
        let result = auth_service
            .verify_mfa_challenge(&challenge, "193746")
            .await;
        assert!(matches!(result, Err(MfaChallengeError::InvalidCode { .. })));
    }

    #[tokio::test]
    async fn enrolment_at_sign_in_revokes_sessions_that_predate_it() {
        let Some((db, auth_service)) = setup_test_env_with_mfa_setting(false).await else {
            return;
        };
        let user = mfa_user_with_recovery(&db.db, "enrol@example.com", false, &[]).await;
        let pre_mfa_session = auth_service.create_session(user.id).await.unwrap();
        assert!(auth_service.verify_session(&pre_mfa_session).await.is_ok());

        let challenge = auth_service
            .create_mfa_session(user.id, "password")
            .await
            .unwrap();
        let (enrolled, _) = auth_service
            .verify_mfa_challenge_with_origin(&challenge, &current_totp_code(HARDENING_SECRET))
            .await
            .expect("the enrolment code is accepted");
        assert!(enrolled.mfa_enabled);

        assert!(
            matches!(
                auth_service.verify_session(&pre_mfa_session).await,
                Err(AuthError::NotFound(_))
            ),
            "a session established before MFA existed must stop authenticating"
        );
    }

    fn dummy_verifications() -> u32 {
        DUMMY_VERIFICATIONS.with(|count| count.get())
    }

    #[test]
    fn dummy_password_hash_uses_the_real_hash_parameters() {
        let parsed = argon2::PasswordHash::new(dummy_password_hash()).unwrap();
        assert_eq!(parsed.algorithm, argon2::Algorithm::default().ident());
        // Compare the cost parameters only: a parsed hash records its output
        // length, the default leaves it unset, and neither changes the cost.
        let params = argon2::Params::try_from(&parsed).unwrap();
        let default = argon2::Params::default();
        assert_eq!(
            (params.m_cost(), params.t_cost(), params.p_cost()),
            (default.m_cost(), default.t_cost(), default.p_cost()),
            "the dummy must cost exactly what a real verification costs"
        );
        assert!(!verify_against_dummy_hash("anything"));
    }

    #[tokio::test]
    async fn login_runs_a_password_verification_on_every_rejection_path() {
        let Some((db, auth_service)) = setup_test_env_with_mfa_setting(false).await else {
            return;
        };
        create_test_user(&db.db, "has-password@example.com", "Correct-Horse-9!").await;
        users::ActiveModel {
            email: Set("sso-only@example.com".to_string()),
            name: Set("SSO".to_string()),
            password_hash: Set(None),
            email_verified: Set(true),
            mfa_enabled: Set(false),
            ..Default::default()
        }
        .insert(db.db.as_ref())
        .await
        .unwrap();

        let attempt = |email: &str| LoginRequest {
            email: email.to_string(),
            password: "Wrong-Password-1!".to_string(),
        };

        let before = dummy_verifications();
        let unknown = auth_service.login(attempt("nobody@example.com")).await;
        assert!(matches!(unknown, Err(UserAuthError::InvalidCredentials)));
        assert_eq!(dummy_verifications(), before + 1, "unknown email must hash");

        let no_password = auth_service.login(attempt("sso-only@example.com")).await;
        assert!(matches!(
            no_password,
            Err(UserAuthError::InvalidCredentials)
        ));
        assert_eq!(
            dummy_verifications(),
            before + 2,
            "no-password user must hash"
        );

        let wrong = auth_service
            .login(attempt("has-password@example.com"))
            .await;
        assert!(matches!(wrong, Err(UserAuthError::InvalidCredentials)));
        assert_eq!(
            dummy_verifications(),
            before + 2,
            "a real account verifies against its own hash, not the dummy"
        );
    }
}
