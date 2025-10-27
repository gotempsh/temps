use crate::email_templates::AuthEmailService;
use argon2::{PasswordHasher, PasswordVerifier};
use axum::http::header::SET_COOKIE;
use axum::http::HeaderMap;
use chrono::{Duration, Utc};
use cookie::Cookie;
use rand::Rng;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, Set,
    TransactionTrait,
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

    pub async fn verify_session(
        &self,
        session_token: &str,
    ) -> Result<temps_entities::users::Model, AuthError> {
        let session = temps_entities::sessions::Entity::find()
            .filter(temps_entities::sessions::Column::SessionToken.eq(session_token))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| AuthError::NotFound("Session not found".to_string()))?;

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

        info!("Adding session cookie: {}", session_cookie);
        let mut headers = HeaderMap::new();
        headers.append(SET_COOKIE, session_cookie.to_string().parse().unwrap());
        headers.append(SET_COOKIE, mfa_clear_cookie.to_string().parse().unwrap());
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
        // Implement TOTP verification logic here
        // You can use the 'totp-rs' crate or similar
        match &user.mfa_secret {
            Some(secret) => {
                use totp_rs::{Algorithm, TOTP};

                let totp = TOTP::new(
                    Algorithm::SHA1,
                    6,
                    1,
                    30,
                    Secret::Raw(
                        base32::decode(base32::Alphabet::Rfc4648 { padding: true }, secret)
                            .unwrap(),
                    )
                    .to_bytes()
                    .unwrap(),
                )
                .expect("Failed to create TOTP instance");

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

    // Check if email provider is configured
    pub fn is_email_configured(&self) -> bool {
        false
        // self.email_service.is_some()
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

#[derive(Error, Debug)]
pub enum UserAuthError {
    #[error("Database error: {0}")]
    DatabaseError(#[from] sea_orm::DbErr),
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
    #[error("Email service not configured")]
    EmailServiceNotConfigured,
    #[error("Email service error: {0}")]
    EmailServiceError(String),
    #[error("Encryption error: {0}")]
    EncryptionError(String),
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
            password: "password123".to_string(),
            name: "Test User".to_string(),
        };

        let user = auth_service.register_user(request1).await.unwrap();
        assert_eq!(user.email, "test@example.com"); // Should be lowercase

        let request2 = RegisterRequest {
            email: "TEST@EXAMPLE.COM".to_string(),
            password: "password456".to_string(),
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

        // Currently hardcoded to false in the implementation
        assert!(!auth_service.is_email_configured());
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
}
