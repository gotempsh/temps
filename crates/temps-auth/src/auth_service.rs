use crate::email_templates::AuthEmailService;
use argon2::{PasswordHasher, PasswordVerifier};
use axum::http::header::SET_COOKIE;
use axum::http::HeaderMap;
use chrono::{Duration, Utc};
use cookie::Cookie;
use rand::Rng;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter,
    Set, TransactionTrait,
};
use serde::Serialize;
use std::sync::Arc;
use temps_core::notifications::DynNotificationService;
use thiserror::Error;
use totp_rs::Secret;
use tracing::{debug, error, info, warn};
use uuid::Uuid;
const DEFAULT_EXTERNAL_URL: &str = "http://localhost:8000";
#[derive(Serialize)]
pub struct AuthStatusResponse {
    pub status: String,
    pub cli_token: Option<String>,
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
    #[error("New password must differ from the current password")]
    SamePassword,
    #[error("Password complexity check failed: {0}")]
    WeakPassword(String),
    #[error("Account has no password set (likely a SSO/magic-link-only user)")]
    NoPasswordSet,
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
        let session_token = self.generate_session_token();
        let expires_at = Utc::now() + Duration::days(7);

        let new_session = temps_entities::sessions::ActiveModel {
            user_id: Set(user_id),
            session_token: Set(session_token.clone()),
            expires_at: Set(expires_at),
            ..Default::default()
        };

        new_session.insert(self.db.as_ref()).await?;

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
            .count(self.db.as_ref())
            .await?;
        Ok(count)
    }

    pub async fn verify_session(
        &self,
        session_token: &str,
    ) -> Result<temps_entities::users::Model, AuthError> {
        let session = temps_entities::sessions::Entity::find()
            .filter(temps_entities::sessions::Column::SessionToken.eq(session_token))
            .filter(temps_entities::sessions::Column::ExpiresAt.gt(Utc::now()))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| AuthError::NotFound("Session not found or expired".to_string()))?;

        let user = temps_entities::users::Entity::find_by_id(session.user_id)
            .filter(temps_entities::users::Column::DeletedAt.is_null())
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| AuthError::NotFound("User not found or deleted".to_string()))?;

        Ok(user)
    }

    fn generate_session_token(&self) -> String {
        let mut rng = rand::thread_rng();
        (0..64)
            .map(|_| rng.sample(rand::distributions::Alphanumeric) as char)
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
    pub async fn create_mfa_session(&self, user_id: i32) -> Result<String, AuthError> {
        let session_token = self.generate_session_token();
        let expires_at = Utc::now() + Duration::minutes(5); // Short expiration for MFA sessions

        let new_session = temps_entities::sessions::ActiveModel {
            user_id: Set(user_id),
            session_token: Set(session_token.clone()),
            expires_at: Set(expires_at),
            ..Default::default()
        };

        new_session.insert(self.db.as_ref()).await?;

        Ok(session_token)
    }

    // Verifies the MFA code
    pub async fn verify_mfa_challenge(
        &self,
        session_token: &str,
        code: &str,
    ) -> Result<temps_entities::users::Model, AuthError> {
        // Get the user from the temporary session
        let session = temps_entities::sessions::Entity::find()
            .filter(temps_entities::sessions::Column::SessionToken.eq(session_token))
            .filter(temps_entities::sessions::Column::ExpiresAt.gt(Utc::now()))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| AuthError::GenericError("Invalid or expired session".to_string()))?;

        let user = temps_entities::users::Entity::find_by_id(session.user_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| AuthError::NotFound("User not found".to_string()))?;

        // Verify the MFA code
        if !self.verify_totp_code(&user, code) {
            return Err(AuthError::GenericError("Invalid MFA code".to_string()));
        }

        // Delete the temporary session
        temps_entities::sessions::Entity::delete_many()
            .filter(temps_entities::sessions::Column::SessionToken.eq(session_token))
            .exec(self.db.as_ref())
            .await?;

        Ok(user)
    }

    fn verify_totp_code(&self, user: &temps_entities::users::Model, code: &str) -> bool {
        match &user.mfa_secret {
            Some(secret) => {
                use totp_rs::{Algorithm, TOTP};

                let decoded =
                    match base32::decode(base32::Alphabet::Rfc4648 { padding: true }, secret) {
                        Some(bytes) => bytes,
                        None => {
                            tracing::error!(
                                "Failed to base32-decode MFA secret for user {}",
                                user.id
                            );
                            return false;
                        }
                    };

                let secret_bytes = match Secret::Raw(decoded).to_bytes() {
                    Ok(bytes) => bytes,
                    Err(e) => {
                        tracing::error!(
                            "Failed to convert MFA secret to bytes for user {}: {:?}",
                            user.id,
                            e
                        );
                        return false;
                    }
                };

                let totp = match TOTP::new(Algorithm::SHA1, 6, 1, 30, secret_bytes) {
                    Ok(t) => t,
                    Err(e) => {
                        tracing::error!(
                            "Failed to create TOTP instance for user {}: {:?}",
                            user.id,
                            e
                        );
                        return false;
                    }
                };

                totp.check_current(code).unwrap_or(false)
            }
            None => false,
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
        use argon2::password_hash::{rand_core::OsRng, SaltString};
        let argon2 = argon2::Argon2::default();
        let salt = SaltString::generate(&mut OsRng);

        let password_hash = argon2
            .hash_password(request.password.as_bytes(), &salt)
            .map_err(|_| UserAuthError::PasswordHashError)?
            .to_string();

        // Create the user
        let new_user = temps_entities::users::ActiveModel {
            email: Set(request.email.to_lowercase()),
            name: Set(request.name.clone()),
            password_hash: Set(Some(password_hash)),
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
        // Find user by email, excluding soft-deleted users
        let user = temps_entities::users::Entity::find()
            .filter(temps_entities::users::Column::Email.eq(request.email.to_lowercase()))
            .filter(temps_entities::users::Column::DeletedAt.is_null())
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                warn!(
                    "Login attempt for non-existent or deleted email: {}",
                    request.email
                );
                UserAuthError::InvalidCredentials
            })?;

        // Check if user has a password (might be GitHub-only user)
        let password_hash = user.password_hash.as_ref().ok_or_else(|| {
            warn!("Login attempt for user {} with no password hash", user.id);
            UserAuthError::InvalidCredentials
        })?;

        // Verify password - only Argon2 is supported
        let password_valid = if password_hash.starts_with("$argon2") {
            // Argon2 hash (only supported format)
            debug!("Verifying Argon2 password for user {}", user.id);
            let parsed_hash =
                argon2::password_hash::PasswordHash::new(password_hash).map_err(|e| {
                    error!("Failed to parse Argon2 hash for user {}: {}", user.id, e);
                    UserAuthError::InvalidCredentials
                })?;

            let argon2 = argon2::Argon2::default();
            argon2
                .verify_password(request.password.as_bytes(), &parsed_hash)
                .is_ok()
        } else {
            // Only Argon2 is supported - all other hash formats are rejected
            error!(
                "User {} has unsupported password hash format (only Argon2 is supported): {}",
                user.id,
                &password_hash[..std::cmp::min(20, password_hash.len())]
            );
            false
        };

        if !password_valid {
            warn!("Invalid password attempt for user {}", user.id);
            return Err(UserAuthError::InvalidCredentials);
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

    // Send magic link for passwordless login
    pub async fn send_magic_link(&self, request: MagicLinkRequest) -> Result<(), UserAuthError> {
        // Check if email service is configured

        // Check if user exists
        let user = temps_entities::users::Entity::find()
            .filter(temps_entities::users::Column::Email.eq(request.email.to_lowercase()))
            .one(self.db.as_ref())
            .await?;

        // Always return success to avoid email enumeration
        if user.is_none() {
            return Ok(());
        }

        // Generate magic link token
        let token = self.generate_token();
        let expires_at = Utc::now() + Duration::minutes(15);

        // Save token to database
        let magic_link_token = temps_entities::magic_link_tokens::ActiveModel {
            email: Set(request.email.to_lowercase()),
            token: Set(token.clone()),
            expires_at: Set(expires_at),
            used: Set(false),
            created_at: Set(Utc::now()),
            ..Default::default()
        };

        magic_link_token.insert(self.db.as_ref()).await?;
        let settings = self.get_settings().await?;
        // Send magic link email
        let base_url = settings
            .external_url
            .unwrap_or_else(|| DEFAULT_EXTERNAL_URL.to_string());
        let magic_link_url = format!("{}/auth/magic-link?token={}", base_url, token);

        self.email_service
            .send_magic_link_email(&request.email, &magic_link_url)
            .await
            .map_err(|e| UserAuthError::EmailServiceError(e.to_string()))?;

        Ok(())
    }

    // Verify magic link token
    pub async fn verify_magic_link(
        &self,
        token: &str,
    ) -> Result<temps_entities::users::Model, UserAuthError> {
        // Find the token
        let magic_link = temps_entities::magic_link_tokens::Entity::find()
            .filter(temps_entities::magic_link_tokens::Column::Token.eq(token))
            .filter(temps_entities::magic_link_tokens::Column::Used.eq(false))
            .filter(temps_entities::magic_link_tokens::Column::ExpiresAt.gt(Utc::now()))
            .one(self.db.as_ref())
            .await?
            .ok_or(UserAuthError::InvalidToken)?;

        // Mark token as used
        let mut magic_link_update: temps_entities::magic_link_tokens::ActiveModel =
            magic_link.clone().into();
        magic_link_update.used = Set(true);
        magic_link_update.update(self.db.as_ref()).await?;

        // Find user by email
        let user = temps_entities::users::Entity::find()
            .filter(temps_entities::users::Column::Email.eq(&magic_link.email))
            .one(self.db.as_ref())
            .await?
            .ok_or(UserAuthError::UserNotFound)?;

        // Mark email as verified if not already
        if !user.email_verified {
            let mut user_update: temps_entities::users::ActiveModel = user.clone().into();
            user_update.email_verified = Set(true);
            user_update.update(self.db.as_ref()).await?;
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

        // Find user by reset token
        let user = temps_entities::users::Entity::find()
            .filter(temps_entities::users::Column::PasswordResetToken.eq(&request.token))
            .one(self.db.as_ref())
            .await?
            .ok_or(UserAuthError::InvalidToken)?;

        // Check if token is expired
        if let Some(expires_at) = user.password_reset_expires {
            if expires_at < Utc::now() {
                return Err(UserAuthError::InvalidToken);
            }
        } else {
            return Err(UserAuthError::InvalidToken);
        }

        // Hash new password
        use argon2::password_hash::{rand_core::OsRng, SaltString};
        let argon2 = argon2::Argon2::default();
        let salt = SaltString::generate(&mut OsRng);

        let password_hash = argon2
            .hash_password(request.new_password.as_bytes(), &salt)
            .map_err(|_| UserAuthError::PasswordHashError)?
            .to_string();

        // Update user password and clear reset token
        let mut user_update: temps_entities::users::ActiveModel = user.into();
        user_update.password_hash = Set(Some(password_hash));
        user_update.password_reset_token = Set(None);
        user_update.password_reset_expires = Set(None);
        user_update.updated_at = Set(Utc::now());
        let updated_user = user_update.update(self.db.as_ref()).await?;

        Ok(updated_user)
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

        // 1. Account must have a password set. SSO/magic-link-only users
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
        let parsed = argon2::password_hash::PasswordHash::new(stored_hash)
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
            let mfa_ok = user_service
                .verify_mfa_code(user_id, code)
                .await
                .map_err(|e| {
                    error!("MFA verification error during password change: {}", e);
                    AuthError::InvalidMfaCode
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
        use argon2::password_hash::{rand_core::OsRng, SaltString};
        let argon2 = argon2::Argon2::default();
        let salt = SaltString::generate(&mut OsRng);
        use argon2::PasswordHasher;
        let new_hash = argon2
            .hash_password(new_password.as_bytes(), &salt)
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
    /// not just any provider — because password reset, email verification
    /// and magic links require real inbox delivery, not a Slack/webhook
    /// channel.
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

/// Validate password meets minimum complexity requirements.
/// Requirements:
/// - At least 8 characters long
/// - Contains at least one uppercase letter
/// - Contains at least one lowercase letter
/// - Contains at least one digit
/// - Contains at least one special character
pub fn validate_password_complexity(password: &str) -> Result<(), UserAuthError> {
    if password.len() < 8 {
        return Err(UserAuthError::WeakPassword(
            "Password must be at least 8 characters long".to_string(),
        ));
    }
    if password.len() > 128 {
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
    // `magic_link_tokens.token`), and a generic substring match would
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
pub struct MagicLinkRequest {
    pub email: String,
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
    use temps_database::test_utils::TestDatabase;
    use temps_entities::types::RoleType;
    use temps_entities::{magic_link_tokens, sessions, settings, users};

    struct MockEmailService {
        verification_emails_sent: std::sync::Mutex<Vec<(String, String, String)>>,
        password_reset_emails_sent: std::sync::Mutex<Vec<(String, String, String)>>,
        magic_link_emails_sent: std::sync::Mutex<Vec<(String, String)>>,
    }

    impl MockEmailService {
        fn new() -> Self {
            Self {
                verification_emails_sent: std::sync::Mutex::new(Vec::new()),
                password_reset_emails_sent: std::sync::Mutex::new(Vec::new()),
                magic_link_emails_sent: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn get_verification_emails(&self) -> Vec<(String, String, String)> {
            self.verification_emails_sent.lock().unwrap().clone()
        }

        fn get_password_reset_emails(&self) -> Vec<(String, String, String)> {
            self.password_reset_emails_sent.lock().unwrap().clone()
        }

        fn get_magic_link_emails(&self) -> Vec<(String, String)> {
            self.magic_link_emails_sent.lock().unwrap().clone()
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
                } else if message.subject.contains("Magic") {
                    // Extract magic link URL from body
                    if let Some(start) = message.body.find("Click here to login: ") {
                        let url = &message.body[start + 21..];
                        self.magic_link_emails_sent
                            .lock()
                            .unwrap()
                            .push((to.clone(), url.to_string()));
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
        use argon2::password_hash::{rand_core::OsRng, SaltString};
        let argon2 = argon2::Argon2::default();
        let salt = SaltString::generate(&mut OsRng);
        let password_hash = argon2
            .hash_password(password.as_bytes(), &salt)
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
        use argon2::password_hash::{rand_core::OsRng, SaltString};
        let argon2 = argon2::Argon2::default();
        let salt = SaltString::generate(&mut OsRng);
        let password_hash = argon2
            .hash_password(password.as_bytes(), &salt)
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
                Set(Some("JBSWY3DPEHPK3PXP".to_string()))
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
            Err(e) => {
                println!("Docker not available, skipping test: {}", e);
                return None;
            }
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

    // Magic Link Tests

    #[tokio::test]
    async fn test_send_magic_link_existing_user() {
        let (db, auth_service, email_service) = setup_test_env().await;
        create_test_user(&db.db, "user@example.com", "password").await;

        let request = MagicLinkRequest {
            email: "user@example.com".to_string(),
        };

        auth_service.send_magic_link(request).await.unwrap();

        // Verify token was saved
        let token = magic_link_tokens::Entity::find()
            .filter(magic_link_tokens::Column::Email.eq("user@example.com"))
            .one(db.db.as_ref())
            .await
            .unwrap()
            .unwrap();

        assert!(!token.used);
        assert!(token.expires_at > Utc::now());

        // Verify email was sent
        let emails = email_service.get_magic_link_emails();
        assert_eq!(emails.len(), 1);
        assert_eq!(emails[0].0, "user@example.com");
    }

    #[tokio::test]
    async fn test_send_magic_link_nonexistent_user() {
        let (_db, auth_service, email_service) = setup_test_env().await;

        let request = MagicLinkRequest {
            email: "nonexistent@example.com".to_string(),
        };

        // Should not error to prevent email enumeration
        auth_service.send_magic_link(request).await.unwrap();

        // No email should be sent
        let emails = email_service.get_magic_link_emails();
        assert_eq!(emails.len(), 0);
    }

    #[tokio::test]
    async fn test_verify_magic_link_valid() {
        let (db, auth_service, _) = setup_test_env().await;
        let user = create_test_user(&db.db, "user@example.com", "password").await;

        // Create magic link token manually
        let token = Uuid::new_v4().to_string();
        let magic_link = magic_link_tokens::ActiveModel {
            email: Set("user@example.com".to_string()),
            token: Set(token.clone()),
            expires_at: Set(Utc::now() + Duration::minutes(15)),
            used: Set(false),
            created_at: Set(Utc::now()),
            ..Default::default()
        };
        magic_link.insert(db.db.as_ref()).await.unwrap();

        let verified_user = auth_service.verify_magic_link(&token).await.unwrap();

        assert_eq!(verified_user.id, user.id);

        // Verify token was marked as used
        let updated_token = magic_link_tokens::Entity::find()
            .filter(magic_link_tokens::Column::Token.eq(&token))
            .one(db.db.as_ref())
            .await
            .unwrap()
            .unwrap();

        assert!(updated_token.used);
    }

    #[tokio::test]
    async fn test_verify_magic_link_expired() {
        let (db, auth_service, _) = setup_test_env().await;
        create_test_user(&db.db, "user@example.com", "password").await;

        // Create expired token
        let token = Uuid::new_v4().to_string();
        let magic_link = magic_link_tokens::ActiveModel {
            email: Set("user@example.com".to_string()),
            token: Set(token.clone()),
            expires_at: Set(Utc::now() - Duration::minutes(1)), // Expired
            used: Set(false),
            created_at: Set(Utc::now()),
            ..Default::default()
        };
        magic_link.insert(db.db.as_ref()).await.unwrap();

        let result = auth_service.verify_magic_link(&token).await;

        assert!(result.is_err());
        matches!(result.unwrap_err(), UserAuthError::InvalidToken);
    }

    #[tokio::test]
    async fn test_verify_magic_link_already_used() {
        let (db, auth_service, _) = setup_test_env().await;
        create_test_user(&db.db, "user@example.com", "password").await;

        // Create used token
        let token = Uuid::new_v4().to_string();
        let magic_link = magic_link_tokens::ActiveModel {
            email: Set("user@example.com".to_string()),
            token: Set(token.clone()),
            expires_at: Set(Utc::now() + Duration::minutes(15)),
            used: Set(true), // Already used
            created_at: Set(Utc::now()),
            ..Default::default()
        };
        magic_link.insert(db.db.as_ref()).await.unwrap();

        let result = auth_service.verify_magic_link(&token).await;

        assert!(result.is_err());
        matches!(result.unwrap_err(), UserAuthError::InvalidToken);
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

        // Verify new password works
        let login = LoginRequest {
            email: "user@example.com".to_string(),
            password: "newSecurePassword123!".to_string(),
        };
        auth_service.login(login).await.unwrap();
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

        let mfa_session_token = auth_service.create_mfa_session(user.id).await.unwrap();

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

    #[tokio::test]
    async fn test_verify_mfa_challenge_without_secret() {
        let (db, auth_service, _) = setup_test_env().await;
        let user = create_test_user(&db.db, "mfa@example.com", "password").await;

        // Create MFA session
        let mfa_session_token = auth_service.create_mfa_session(user.id).await.unwrap();

        // Try to verify without MFA secret set
        let result = auth_service
            .verify_mfa_challenge(&mfa_session_token, "123456")
            .await;

        assert!(result.is_err());
        matches!(result.unwrap_err(), AuthError::GenericError(_));
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

        assert!(result.is_err());
        matches!(result.unwrap_err(), AuthError::GenericError(_));
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
        // magic_link_tokens.token) must not be misreported as a duplicate
        // email -- only `idx_users_email_unique` should match.
        let err = sea_orm::DbErr::Custom(
            "error returned from database: duplicate key value violates unique constraint \"magic_link_tokens_token_key\" (SQLSTATE 23505)".to_string(),
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
        let salt = argon2::password_hash::SaltString::generate(
            &mut argon2::password_hash::rand_core::OsRng,
        );
        let password_hash = argon2
            .hash_password(password.as_bytes(), &salt)
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
}
