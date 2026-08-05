use crate::avatar::generate_avatar_data_url;
use base64::Engine;
use chrono::Utc;
use qrcode::QrCode;
use rand::RngExt;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, DatabaseTransaction, EntityTrait,
    QueryFilter, QueryOrder, QuerySelect, Set, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Cursor;
use std::sync::Arc;
use temps_core::UtcDateTime;
use temps_entities::types::RoleType;
use thiserror::Error;
use tokio::sync::{Mutex, OwnedMutexGuard, OwnedSemaphorePermit, Semaphore};
use totp_rs::{Algorithm, Secret, TOTP};
use tracing::{debug, error, info, warn};

const MAX_USER_EMAIL_BYTES: usize = 254;
const MAX_USER_NAME_CHARS: usize = 100;
const MAX_CONCURRENT_MFA_SETUPS: usize = 2;

pub(crate) fn normalize_user_email(email: &str) -> Option<String> {
    let normalized = email.trim().to_lowercase();
    if normalized.is_empty()
        || normalized.len() > MAX_USER_EMAIL_BYTES
        || normalized.chars().any(char::is_whitespace)
        || normalized.chars().any(char::is_control)
    {
        return None;
    }

    let (local, domain) = normalized.split_once('@')?;
    if local.is_empty()
        || local.len() > 64
        || local.starts_with('.')
        || local.ends_with('.')
        || local.contains("..")
        || domain.is_empty()
        || domain.contains('@')
        || domain.starts_with('.')
        || domain.ends_with('.')
    {
        return None;
    }

    let labels: Vec<_> = domain.split('.').collect();
    if labels.len() < 2
        || labels.iter().any(|label| {
            label.is_empty()
                || label.starts_with('-')
                || label.ends_with('-')
                || !label
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '-')
        })
    {
        return None;
    }

    Some(normalized)
}

// First add the custom error type at the top of the file
#[derive(Error, Debug)]
pub enum UserServiceError {
    #[error("Database connection error: {0}")]
    DatabaseConnection(String),

    #[error("Database error: {reason}")]
    Database { reason: String },

    #[error("User not found: {0}")]
    NotFound(String),

    #[error("Role not found: {0}")]
    RoleNotFound(String),

    #[error("MFA error: {0}")]
    Mfa(String),

    #[error("Invalid MFA code")]
    InvalidMfaCode,

    #[error("MFA not set up for user {0}")]
    MfaNotSetup(i32),

    #[error("MFA is already enabled for user {0}")]
    MfaAlreadyEnabled(i32),

    #[error("User {0} is already deleted")]
    AlreadyDeleted(i32),

    #[error("User {0} is not deleted")]
    NotDeleted(i32),

    #[error("Role {0} already assigned to user {1}")]
    RoleAlreadyAssigned(String, i32),

    #[error("Role {0} not assigned to user {1}")]
    RoleNotAssigned(String, i32),

    #[error("Validation error: {0}")]
    Validation(String),

    #[error("Encryption error: {0}")]
    Encryption(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Internal error: {0}")]
    Internal(String),
}

impl From<sea_orm::DbErr> for UserServiceError {
    fn from(error: sea_orm::DbErr) -> Self {
        match error {
            sea_orm::DbErr::RecordNotFound(_) => {
                UserServiceError::NotFound("Record not found".to_string())
            }
            _ => UserServiceError::Database {
                reason: error.to_string(),
            },
        }
    }
}

// Add a new struct for MFA setup data
#[derive(Debug, Serialize)]
pub struct MfaSetupData {
    pub secret_key: String,
    pub qr_code: String,
    pub recovery_codes: Vec<String>,
}

struct MfaSetupCandidate {
    secret_key: String,
    qr_code: String,
    recovery_codes: Vec<String>,
    hashed_recovery_codes: Vec<String>,
}

fn generate_mfa_setup_candidate(email: &str) -> Result<MfaSetupCandidate, UserServiceError> {
    use argon2::password_hash::{rand_core::OsRng, PasswordHasher, SaltString};
    use argon2::Argon2;

    let secret: Vec<u8> = (0..20).map(|_| rand::rng().random::<u8>()).collect();
    let secret_key = base32::encode(base32::Alphabet::Rfc4648 { padding: true }, &secret);
    let recovery_codes: Vec<String> = (0..8)
        .map(|_| {
            (0..6)
                .map(|_| rand::rng().random_range(0..10).to_string())
                .collect()
        })
        .collect();

    let argon2 = Argon2::default();
    let hashed_recovery_codes = recovery_codes
        .iter()
        .map(|code| {
            let salt = SaltString::generate(&mut OsRng);
            argon2
                .hash_password(code.as_bytes(), &salt)
                .map(|hash| hash.to_string())
                .map_err(|error| {
                    UserServiceError::Mfa(format!("Failed to hash MFA recovery code: {}", error))
                })
        })
        .collect::<Result<Vec<_>, _>>()?;

    TOTP::new(
        Algorithm::SHA1,
        6,
        1,
        30,
        Secret::Raw(secret).to_bytes().map_err(|error| {
            UserServiceError::Mfa(format!("Failed to create TOTP secret: {}", error))
        })?,
    )
    .map_err(|error| UserServiceError::Mfa(format!("Failed to create TOTP: {}", error)))?;

    let otp_auth_url = format!(
        "otpauth://totp/Temps:{}?secret={}&issuer=Temps&algorithm=SHA1&digits=6&period=30",
        email, secret_key
    );
    let qr = QrCode::new(otp_auth_url)
        .map_err(|error| UserServiceError::Mfa(format!("Failed to generate QR code: {}", error)))?;
    let qr_image = qr.render::<image::Luma<u8>>().quiet_zone(false).build();
    let mut bytes = Vec::new();
    qr_image
        .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)
        .map_err(|error| {
            UserServiceError::Mfa(format!("Failed to encode MFA QR code: {}", error))
        })?;
    let qr_code = format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    );

    Ok(MfaSetupCandidate {
        secret_key,
        qr_code,
        recovery_codes,
        hashed_recovery_codes,
    })
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ServiceUser {
    pub id: i32,
    pub name: String,
    pub email: String,
    pub image: String,
    pub mfa_enabled: bool,
    pub email_verified: bool,
    pub must_change_password: bool,
    pub deleted_at: Option<UtcDateTime>,
    pub created_at: UtcDateTime,
    pub updated_at: UtcDateTime,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ServiceRole {
    pub id: i32,
    pub name: String,
    pub created_at: UtcDateTime,
    pub updated_at: UtcDateTime,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UserWithRoles {
    pub user: ServiceUser,
    pub roles: Vec<ServiceRole>,
}

impl From<temps_entities::users::Model> for ServiceUser {
    fn from(db_user: temps_entities::users::Model) -> Self {
        Self {
            id: db_user.id,
            name: db_user.name.clone(),
            email: db_user.email,
            image: generate_avatar_data_url(&db_user.name),
            mfa_enabled: db_user.mfa_enabled,
            email_verified: db_user.email_verified,
            must_change_password: db_user.must_change_password,
            deleted_at: db_user.deleted_at,
            created_at: db_user.created_at,
            updated_at: db_user.updated_at,
        }
    }
}

impl From<temps_entities::roles::Model> for ServiceRole {
    fn from(db_role: temps_entities::roles::Model) -> Self {
        Self {
            id: db_role.id,
            name: db_role.name,
            created_at: db_role.created_at,
            updated_at: db_role.updated_at,
        }
    }
}

pub struct UserService {
    db: Arc<DatabaseConnection>,
    mfa_setup_global: Arc<Semaphore>,
    mfa_setup_users: Arc<Mutex<HashMap<i32, Arc<Mutex<()>>>>>,
}

struct MfaSetupPermit {
    _user: OwnedMutexGuard<()>,
    _global: OwnedSemaphorePermit,
}

async fn run_mfa_setup_blocking<T, F>(
    permit: MfaSetupPermit,
    work: F,
) -> Result<(MfaSetupPermit, T), UserServiceError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, UserServiceError> + Send + 'static,
{
    let (permit, result) = tokio::task::spawn_blocking(move || (permit, work()))
        .await
        .map_err(|error| {
            UserServiceError::Internal(format!("MFA setup worker failed: {}", error))
        })?;
    Ok((permit, result?))
}

impl UserService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self {
            db,
            mfa_setup_global: Arc::new(Semaphore::new(MAX_CONCURRENT_MFA_SETUPS)),
            mfa_setup_users: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    async fn acquire_mfa_setup_permit(
        &self,
        user_id: i32,
    ) -> Result<MfaSetupPermit, UserServiceError> {
        let user_lock = {
            let mut users = self.mfa_setup_users.lock().await;
            users
                .entry(user_id)
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let user = user_lock.lock_owned().await;
        let global = self
            .mfa_setup_global
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| {
                UserServiceError::Internal("MFA setup concurrency limiter closed".to_string())
            })?;
        Ok(MfaSetupPermit {
            _user: user,
            _global: global,
        })
    }

    pub async fn initialize_roles(&self) -> Result<(), UserServiceError> {
        let now = Utc::now();

        // Create admin role if it doesn't exist
        let admin_role = temps_entities::roles::ActiveModel {
            name: Set(RoleType::Admin.as_str().to_string()),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        };

        // Try to insert, ignore if it already exists
        let _ = admin_role.insert(self.db.as_ref()).await;

        // Create user role if it doesn't exist
        let user_role = temps_entities::roles::ActiveModel {
            name: Set(RoleType::User.as_str().to_string()),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        };

        // Try to insert, ignore if it already exists
        let _ = user_role.insert(self.db.as_ref()).await;

        // Create demo role if it doesn't exist
        // This is used for demo mode access via demo.<preview_domain> subdomain
        let demo_role = temps_entities::roles::ActiveModel {
            name: Set("demo".to_string()),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        };

        // Try to insert, ignore if it already exists
        let _ = demo_role.insert(self.db.as_ref()).await;

        info!("Initialized default roles (admin, user, demo)");
        Ok(())
    }

    pub async fn has_role(
        &self,
        user_id_val: i32,
        role_type_val: RoleType,
    ) -> Result<bool, UserServiceError> {
        // First get the role by name
        let role = temps_entities::roles::Entity::find()
            .filter(temps_entities::roles::Column::Name.eq(role_type_val.as_str().to_lowercase()))
            .one(self.db.as_ref())
            .await?;

        if let Some(role) = role {
            // Then check if user has this role
            let user_role = temps_entities::user_roles::Entity::find()
                .filter(temps_entities::user_roles::Column::UserId.eq(user_id_val))
                .filter(temps_entities::user_roles::Column::RoleId.eq(role.id))
                .one(self.db.as_ref())
                .await?;

            Ok(user_role.is_some())
        } else {
            Ok(false)
        }
    }

    pub async fn is_admin(&self, user_id: i32) -> Result<bool, UserServiceError> {
        self.has_role(user_id, RoleType::Admin).await
    }

    pub async fn get_user_with_roles(
        &self,
        user_id: i32,
    ) -> Result<UserWithRoles, UserServiceError> {
        let user = temps_entities::users::Entity::find_by_id(user_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| UserServiceError::NotFound(format!("User {} not found", user_id)))?;

        // Get all roles for this user
        let user_role_entries = temps_entities::user_roles::Entity::find()
            .filter(temps_entities::user_roles::Column::UserId.eq(user_id))
            .all(self.db.as_ref())
            .await?;

        let mut user_roles = Vec::new();
        for user_role_entry in user_role_entries {
            if let Some(role) = temps_entities::roles::Entity::find_by_id(user_role_entry.role_id)
                .one(self.db.as_ref())
                .await?
            {
                user_roles.push(role);
            }
        }

        Ok(UserWithRoles {
            user: user.into(),
            roles: user_roles.into_iter().map(|r| r.into()).collect(),
        })
    }

    pub async fn assign_default_role(&self, user_id: i32) -> Result<(), UserServiceError> {
        // Get the user role
        let user_role = temps_entities::roles::Entity::find()
            .filter(temps_entities::roles::Column::Name.eq(RoleType::User.as_str()))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| UserServiceError::RoleNotFound("User role not found".to_string()))?;

        // Assign the user role
        self.assign_role_to_user(user_id, user_role.id).await?;

        Ok(())
    }

    pub async fn assign_role_to_user(
        &self,
        user_id: i32,
        role_id: i32,
    ) -> Result<(), UserServiceError> {
        // Verify user exists
        temps_entities::users::Entity::find_by_id(user_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| UserServiceError::NotFound(format!("User {} not found", user_id)))?;

        // Verify role exists
        temps_entities::roles::Entity::find_by_id(role_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| UserServiceError::RoleNotFound(format!("Role {} not found", role_id)))?;

        let new_user_role = temps_entities::user_roles::ActiveModel {
            user_id: Set(user_id),
            role_id: Set(role_id),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };

        new_user_role.insert(self.db.as_ref()).await?;

        info!("Assigned role {} to user {}", role_id, user_id);
        Ok(())
    }

    pub async fn get_all_users(
        &self,
        include_deleted: bool,
    ) -> Result<Vec<UserWithRoles>, UserServiceError> {
        let mut query = temps_entities::users::Entity::find()
            .filter(temps_entities::users::Column::Id.ne(0))
            .order_by_desc(temps_entities::users::Column::CreatedAt);

        if !include_deleted {
            query = query.filter(temps_entities::users::Column::DeletedAt.is_null());
        }

        let users = query.all(self.db.as_ref()).await?;
        let mut users_with_roles = Vec::new();

        for user in users {
            // Get roles for each user
            let user_role_entries = temps_entities::user_roles::Entity::find()
                .filter(temps_entities::user_roles::Column::UserId.eq(user.id))
                .all(self.db.as_ref())
                .await?;

            let mut roles = Vec::new();
            for user_role_entry in user_role_entries {
                if let Some(role) =
                    temps_entities::roles::Entity::find_by_id(user_role_entry.role_id)
                        .one(self.db.as_ref())
                        .await?
                {
                    roles.push(role.into());
                }
            }

            users_with_roles.push(UserWithRoles {
                user: user.into(),
                roles,
            });
        }

        Ok(users_with_roles)
    }

    pub async fn get_role_by_type(
        &self,
        role_type: RoleType,
    ) -> Result<temps_entities::roles::Model, UserServiceError> {
        let role = temps_entities::roles::Entity::find()
            .filter(temps_entities::roles::Column::Name.eq(role_type.as_str()))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                UserServiceError::RoleNotFound(format!("Role {} not found", role_type.as_str()))
            })?;

        Ok(role)
    }
    pub async fn get_user_by_id(
        &self,
        user_id: i32,
    ) -> Result<temps_entities::users::Model, UserServiceError> {
        let user = temps_entities::users::Entity::find_by_id(user_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| UserServiceError::NotFound(format!("User {} not found", user_id)))?;

        Ok(user)
    }

    pub async fn assign_role_by_type(
        &self,
        user_id: i32,
        role_type: RoleType,
    ) -> Result<(), UserServiceError> {
        // Get role ID for the role type
        let role = self
            .get_role_by_type(role_type.clone())
            .await
            .map_err(|_| {
                UserServiceError::RoleNotFound(format!("Role {} not found", role_type.as_str()))
            })?;

        // Assign the role
        self.assign_role_to_user(user_id, role.id).await?;

        Ok(())
    }

    pub async fn create_user(
        &self,
        username: String,
        email: String,
        password: Option<String>,
        roles: Vec<RoleType>,
        must_change_password: bool,
    ) -> Result<UserWithRoles, UserServiceError> {
        let now = Utc::now();

        let username = username.trim().to_string();
        let username_chars = username.chars().count();
        if !(1..=MAX_USER_NAME_CHARS).contains(&username_chars)
            || username.chars().any(char::is_control)
        {
            return Err(UserServiceError::Validation(format!(
                "Name must contain between 1 and {MAX_USER_NAME_CHARS} characters"
            )));
        }

        let email = normalize_user_email(&email).ok_or_else(|| {
            UserServiceError::Validation("A valid email address is required".to_string())
        })?;

        if roles.is_empty() {
            return Err(UserServiceError::Validation(
                "At least one valid role is required".to_string(),
            ));
        }

        let mut role_names = std::collections::HashSet::new();
        if roles.iter().any(|role| !role_names.insert(role.as_str())) {
            return Err(UserServiceError::Validation(
                "Duplicate roles are not allowed".to_string(),
            ));
        }

        if must_change_password && password.is_none() {
            return Err(UserServiceError::Validation(
                "A temporary password is required when first-login password change is enabled"
                    .to_string(),
            ));
        }

        // Hash password if provided using Argon2 (same as auth_service for consistency)
        let password_hash = if let Some(pwd) = password {
            // Validate password complexity
            crate::auth_service::validate_password_complexity(&pwd)
                .map_err(|e| UserServiceError::Validation(e.to_string()))?;
            debug!("Hashing password with Argon2 for user: {}", username);
            use argon2::password_hash::{rand_core::OsRng, PasswordHasher, SaltString};

            let salt = SaltString::generate(&mut OsRng);
            let argon2 = argon2::Argon2::default();
            let hash = argon2
                .hash_password(pwd.as_bytes(), &salt)
                .map_err(|e| {
                    error!(
                        "Failed to hash password with Argon2 for user {}: {}",
                        username, e
                    );
                    UserServiceError::Internal(format!("Failed to hash password: {}", e))
                })?
                .to_string();
            Some(hash)
        } else {
            warn!(
                "No password provided for user: {} - password_hash will be NULL",
                username
            );
            None
        };

        let transaction = self.db.begin().await?;

        // Resolve every role before writing the user. This both produces a
        // precise error and prevents a missing role from creating an account
        // that cannot be administered as intended.
        let mut resolved_roles = Vec::with_capacity(roles.len());
        for role_type in roles {
            let role = temps_entities::roles::Entity::find()
                .filter(temps_entities::roles::Column::Name.eq(role_type.as_str()))
                .one(&transaction)
                .await?
                .ok_or_else(|| {
                    UserServiceError::RoleNotFound(format!("Role {} not found", role_type.as_str()))
                });

            match role {
                Ok(role) => resolved_roles.push(role),
                Err(error) => {
                    transaction.rollback().await.map_err(|rollback_error| {
                        UserServiceError::Database {
                            reason: format!(
                                "failed to roll back user creation after {error}: {rollback_error}"
                            ),
                        }
                    })?;
                    return Err(error);
                }
            }
        }

        // Create the user and all role assignments atomically. If any write
        // fails, dropping the uncommitted transaction rolls back every write.
        let new_user = temps_entities::users::ActiveModel {
            name: Set(username.clone()),
            email: Set(email.clone()),
            deleted_at: Set(None),
            mfa_enabled: Set(false),
            mfa_secret: Set(None),
            mfa_recovery_codes: Set(None),
            password_hash: Set(password_hash),
            email_verified: Set(false),
            email_verification_token: Set(None),
            email_verification_expires: Set(None),
            password_reset_token: Set(None),
            password_reset_expires: Set(None),
            must_change_password: Set(must_change_password),
            ..Default::default()
        };

        let user = new_user.insert(&transaction).await?;

        // Build the response from the records already resolved inside this
        // transaction. Once commit succeeds there must be no fallible read
        // that can turn a successfully-created account into an HTTP 500.
        let response_roles = resolved_roles
            .iter()
            .cloned()
            .map(ServiceRole::from)
            .collect();

        // Assign roles
        for role in resolved_roles {
            let new_user_role = temps_entities::user_roles::ActiveModel {
                user_id: Set(user.id),
                role_id: Set(role.id),
                created_at: Set(now),
                updated_at: Set(now),
                ..Default::default()
            };

            new_user_role.insert(&transaction).await?;
        }

        transaction.commit().await?;

        info!("Created new user with id: {}", user.id);
        Ok(UserWithRoles {
            user: ServiceUser::from(user),
            roles: response_roles,
        })
    }

    pub async fn delete_user(
        &self,
        user_id: i32,
    ) -> Result<temps_entities::users::Model, UserServiceError> {
        // Check if user exists and isn't already deleted
        let user = temps_entities::users::Entity::find_by_id(user_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| UserServiceError::NotFound(format!("User {} not found", user_id)))?;

        if user.deleted_at.is_some() {
            return Err(UserServiceError::AlreadyDeleted(user_id));
        }

        // Soft delete the user by setting deleted_at and changing email to prevent duplicates
        let now = Utc::now();
        let unix_epoch = now.timestamp();
        let deleted_email = format!("deleted_{}_{}", unix_epoch, user.email);

        let mut user_update: temps_entities::users::ActiveModel = user.into();
        user_update.deleted_at = Set(Some(now));
        user_update.email = Set(deleted_email);

        let deleted_user = user_update.update(self.db.as_ref()).await?;

        info!(
            "Soft deleted user with id: {}, email changed to prevent duplicates",
            user_id
        );
        Ok(deleted_user)
    }

    pub async fn remove_role_from_user(
        &self,
        user_id_val: i32,
        role_type: RoleType,
    ) -> Result<(), UserServiceError> {
        // Get role ID for the role type
        let role = temps_entities::roles::Entity::find()
            .filter(temps_entities::roles::Column::Name.eq(role_type.as_str()))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                UserServiceError::RoleNotFound(format!("Role {} not found", role_type.as_str()))
            })?;

        // Delete the user_role entry
        let delete_result = temps_entities::user_roles::Entity::delete_many()
            .filter(temps_entities::user_roles::Column::UserId.eq(user_id_val))
            .filter(temps_entities::user_roles::Column::RoleId.eq(role.id))
            .exec(self.db.as_ref())
            .await?;

        if delete_result.rows_affected == 0 {
            return Err(UserServiceError::RoleNotFound(format!(
                "Role {} not assigned to user {}",
                role_type.as_str(),
                user_id_val
            )));
        }

        info!(
            "Removed role {} from user {}",
            role_type.as_str(),
            user_id_val
        );
        Ok(())
    }

    pub async fn update_user(
        &self,
        user_id: i32,
        email_p: Option<String>,
        name_p: Option<String>,
    ) -> Result<UserWithRoles, UserServiceError> {
        // First check if user exists
        let existing_user = temps_entities::users::Entity::find_by_id(user_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| UserServiceError::NotFound(format!("User {} not found", user_id)))?;

        // Update the user with new values
        let mut user_update: temps_entities::users::ActiveModel = existing_user.into();

        if let Some(email) = email_p {
            user_update.email = Set(email);
        }
        if let Some(name) = name_p {
            user_update.name = Set(name);
        }

        let updated_user = user_update.update(self.db.as_ref()).await?;

        // Get user with roles
        let user_with_roles = self.get_user_with_roles(updated_user.id).await?;

        info!("Updated user {}", user_id);
        Ok(user_with_roles)
    }

    pub async fn restore_user(&self, user_id: i32) -> Result<UserWithRoles, UserServiceError> {
        // Check if user exists and is deleted
        let user = temps_entities::users::Entity::find_by_id(user_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| UserServiceError::NotFound(format!("User {} not found", user_id)))?;

        if user.deleted_at.is_none() {
            return Err(UserServiceError::NotDeleted(user_id));
        }

        // Restore the user by setting deleted_at to null
        let mut user_update: temps_entities::users::ActiveModel = user.into();
        user_update.deleted_at = Set(None);

        let updated_user = user_update.update(self.db.as_ref()).await?;

        // Get user with roles
        let user_with_roles = self.get_user_with_roles(updated_user.id).await?;

        info!("Restored user {}", user_id);
        Ok(user_with_roles)
    }

    pub async fn setup_mfa(&self, user_id: i32) -> Result<MfaSetupData, UserServiceError> {
        // Serialize setup per user and cap total Argon2 work across accounts.
        // This guard is acquired before any database or CPU work so repeated
        // requests cannot amplify password hashing for one account.
        let permit = self.acquire_mfa_setup_permit(user_id).await?;

        // Fetch the display identity before generating the candidate. The
        // exclusive database lock below is only held for the final
        // compare-and-write, never for Argon2 or QR rendering.
        let initial_user = temps_entities::users::Entity::find_by_id(user_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| UserServiceError::NotFound(format!("User {} not found", user_id)))?;

        if initial_user.mfa_enabled {
            return Err(UserServiceError::MfaAlreadyEnabled(user_id));
        }

        // Argon2 and PNG encoding are intentionally kept off Tokio's async
        // workers. An unverified enrollment is replaceable as one complete
        // bundle, so every successful retry rotates all credentials together.
        let email = initial_user.email;
        // The permit moves into the non-cancellable blocking task. If the HTTP
        // request is cancelled, Argon2 retains both guards until it actually
        // exits instead of silently escaping the concurrency cap.
        let (_permit, candidate) =
            run_mfa_setup_blocking(permit, move || generate_mfa_setup_candidate(&email)).await?;

        // Lock only for the final state check and atomic bundle replacement.
        // A concurrent request may have enabled MFA while the candidate was
        // generated, so the condition must be checked again under the lock.
        let transaction = self.db.begin().await?;
        let user = temps_entities::users::Entity::find_by_id(user_id)
            .lock_exclusive()
            .one(&transaction)
            .await?
            .ok_or_else(|| UserServiceError::NotFound(format!("User {} not found", user_id)))?;
        if user.mfa_enabled {
            return Err(UserServiceError::MfaAlreadyEnabled(user_id));
        }

        let mut user_update: temps_entities::users::ActiveModel = user.into();
        user_update.mfa_secret = Set(Some(candidate.secret_key.clone()));
        user_update.mfa_enabled = Set(false);
        user_update.mfa_recovery_codes = Set(Some(
            serde_json::to_string(&candidate.hashed_recovery_codes)
                .map_err(UserServiceError::Serialization)?,
        ));
        user_update.update(&transaction).await?;
        transaction.commit().await?;

        Ok(MfaSetupData {
            secret_key: candidate.secret_key,
            qr_code: candidate.qr_code,
            recovery_codes: candidate.recovery_codes,
        })
    }

    pub async fn verify_and_enable_mfa(
        &self,
        user_id: i32,
        code: &str,
    ) -> Result<(), UserServiceError> {
        let user = temps_entities::users::Entity::find_by_id(user_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| UserServiceError::NotFound(format!("User {} not found", user_id)))?;

        let secret = user
            .mfa_secret
            .clone()
            .ok_or(UserServiceError::MfaNotSetup(user_id))?;

        // Verify TOTP code
        let totp = TOTP::new(
            Algorithm::SHA1,
            6,
            1,
            30,
            Secret::Raw(
                base32::decode(base32::Alphabet::Rfc4648 { padding: true }, &secret).ok_or_else(
                    || UserServiceError::Mfa("Invalid MFA secret encoding".to_string()),
                )?,
            )
            .to_bytes()
            .map_err(|e| UserServiceError::Mfa(format!("Invalid MFA secret: {}", e)))?,
        )
        .map_err(|e| UserServiceError::Mfa(format!("Failed to create TOTP: {}", e)))?;

        if totp
            .check_current(code)
            .map_err(|e| UserServiceError::Mfa(format!("Failed to verify code: {}", e)))?
        {
            // Enable MFA
            let mut user_update: temps_entities::users::ActiveModel = user.into();
            user_update.mfa_enabled = Set(true);
            user_update.update(self.db.as_ref()).await?;

            Ok(())
        } else {
            Err(UserServiceError::InvalidMfaCode)
        }
    }

    pub async fn verify_mfa_code(
        &self,
        user_id: i32,
        code: &str,
    ) -> Result<bool, UserServiceError> {
        let transaction = self.db.begin().await?;
        let valid = self
            .verify_mfa_code_in_transaction(&transaction, user_id, code)
            .await?;
        transaction.commit().await?;
        Ok(valid)
    }

    /// Verify MFA while keeping the user row locked in the caller's
    /// transaction. Step-up authorization uses this to make recovery-code
    /// consumption and session elevation one atomic state transition.
    pub(crate) async fn verify_mfa_code_in_transaction(
        &self,
        transaction: &DatabaseTransaction,
        user_id: i32,
        code: &str,
    ) -> Result<bool, UserServiceError> {
        // Recovery codes are single-use credentials, so checking and
        // consuming one must remain under this exclusive row lock.
        let user = temps_entities::users::Entity::find_by_id(user_id)
            .filter(temps_entities::users::Column::DeletedAt.is_null())
            .lock_exclusive()
            .one(transaction)
            .await?
            .ok_or_else(|| UserServiceError::NotFound(format!("User {} not found", user_id)))?;

        if !user.mfa_enabled {
            return Err(UserServiceError::MfaNotSetup(user_id));
        }

        let secret = user
            .mfa_secret
            .clone()
            .ok_or(UserServiceError::MfaNotSetup(user_id))?;

        // Check if it's a recovery code
        if let Some(recovery_codes) = user.mfa_recovery_codes.clone() {
            let hashed_codes: Vec<String> =
                serde_json::from_str(&recovery_codes).map_err(UserServiceError::Serialization)?;

            // Clone hashed_codes for later use
            let hashed_codes_clone = hashed_codes.clone();

            // First check if the code matches any recovery code using Argon2id
            use argon2::password_hash::{PasswordHash, PasswordVerifier};
            use argon2::Argon2;

            let argon2 = Argon2::default();
            for hashed_code in hashed_codes {
                let parsed_hash = PasswordHash::new(&hashed_code).map_err(|e| {
                    UserServiceError::Encryption(format!(
                        "Failed to parse recovery code hash: {}",
                        e
                    ))
                })?;

                if argon2
                    .verify_password(code.as_bytes(), &parsed_hash)
                    .is_ok()
                {
                    // Remove used recovery code using the cloned vector
                    let new_codes: Vec<String> = hashed_codes_clone
                        .into_iter()
                        .filter(|c| c != &hashed_code)
                        .collect();

                    let mut user_update: temps_entities::users::ActiveModel = user.clone().into();
                    user_update.mfa_recovery_codes = Set(Some(serde_json::to_string(&new_codes)?));
                    user_update.update(transaction).await?;

                    return Ok(true);
                }
            }
        }

        // Verify TOTP code
        let totp = TOTP::new(
            Algorithm::SHA1,
            6,
            1,
            30,
            Secret::Raw(
                base32::decode(base32::Alphabet::Rfc4648 { padding: true }, &secret).ok_or_else(
                    || UserServiceError::Mfa("Invalid MFA secret encoding".to_string()),
                )?,
            )
            .to_bytes()
            .map_err(|e| UserServiceError::Mfa(format!("Invalid MFA secret: {}", e)))?,
        )
        .map_err(|e| UserServiceError::Mfa(format!("Failed to create TOTP: {}", e)))?;

        totp.check_current(code)
            .map_err(|e| UserServiceError::Mfa(format!("Failed to verify TOTP code: {}", e)))
    }

    async fn disable_mfa(&self, user_id: i32) -> Result<(), UserServiceError> {
        let transaction = self.db.begin().await?;
        let user = temps_entities::users::Entity::find_by_id(user_id)
            .lock_exclusive()
            .one(&transaction)
            .await?
            .ok_or_else(|| UserServiceError::NotFound(format!("User {} not found", user_id)))?;

        let mut user_update: temps_entities::users::ActiveModel = user.into();
        user_update.mfa_secret = Set(None);
        user_update.mfa_enabled = Set(false);
        user_update.mfa_recovery_codes = Set(None);

        user_update.update(&transaction).await?;

        temps_entities::sessions::Entity::update_many()
            .col_expr(
                temps_entities::sessions::Column::StepUpExpiresAt,
                sea_orm::sea_query::Expr::value(Option::<chrono::DateTime<Utc>>::None),
            )
            .filter(temps_entities::sessions::Column::UserId.eq(user_id))
            .exec(&transaction)
            .await?;
        transaction.commit().await?;

        Ok(())
    }

    pub async fn verify_and_disable_mfa(
        &self,
        user_id: i32,
        code: &str,
    ) -> Result<(), UserServiceError> {
        // First verify the code
        if !self.verify_mfa_code(user_id, code).await? {
            return Err(UserServiceError::Validation(
                "Invalid verification code".to_string(),
            ));
        }

        // If verification succeeds, disable MFA
        self.disable_mfa(user_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use argon2::password_hash::{rand_core::OsRng, PasswordHasher, SaltString};
    use sea_orm::{DatabaseBackend, DbErr, MockDatabase, MockExecResult};

    fn user(mfa_enabled: bool, recovery_codes: Option<String>) -> temps_entities::users::Model {
        let now = Utc::now();
        temps_entities::users::Model {
            id: 7,
            name: "MFA User".to_string(),
            email: "mfa@example.com".to_string(),
            password_hash: None,
            email_verified: true,
            email_verification_token: None,
            email_verification_expires: None,
            password_reset_token: None,
            password_reset_expires: None,
            must_change_password: false,
            deleted_at: None,
            mfa_secret: mfa_enabled.then(|| "JBSWY3DPEHPK3PXPJBSWY3DPEHPK3PXP".to_string()),
            mfa_enabled,
            mfa_recovery_codes: recovery_codes,
            oidc_subject: None,
            oidc_provider_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn role(role_type: RoleType) -> temps_entities::roles::Model {
        let now = Utc::now();
        temps_entities::roles::Model {
            id: match role_type {
                RoleType::Admin => 1,
                RoleType::User => 2,
            },
            name: role_type.as_str().to_string(),
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn user_email_is_normalized_and_strictly_validated() {
        assert_eq!(
            normalize_user_email("  Admin@Example.COM  "),
            Some("admin@example.com".to_string())
        );
        for invalid in ["", "missing-at.example.com", "@example.com", "user@"] {
            assert_eq!(normalize_user_email(invalid), None, "accepted {invalid:?}");
        }
    }

    #[tokio::test]
    async fn create_user_rejects_invalid_identity_before_database_writes() {
        let service = UserService::new(Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres).into_connection(),
        ));

        let blank_name = service
            .create_user(
                "  ".to_string(),
                "valid@example.com".to_string(),
                None,
                vec![RoleType::User],
                false,
            )
            .await;
        assert!(matches!(blank_name, Err(UserServiceError::Validation(_))));

        let malformed_email = service
            .create_user(
                "Valid User".to_string(),
                "not-an-email".to_string(),
                None,
                vec![RoleType::User],
                false,
            )
            .await;
        assert!(matches!(
            malformed_email,
            Err(UserServiceError::Validation(_))
        ));
    }

    #[tokio::test]
    async fn create_user_requires_at_least_one_role() {
        let service = UserService::new(Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres).into_connection(),
        ));

        let result = service
            .create_user(
                "roleless".to_string(),
                "roleless@example.com".to_string(),
                None,
                vec![],
                false,
            )
            .await;

        assert!(matches!(result, Err(UserServiceError::Validation(_))));
    }

    #[tokio::test]
    async fn create_user_rejects_duplicate_roles() {
        let service = UserService::new(Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres).into_connection(),
        ));

        let result = service
            .create_user(
                "duplicate-role".to_string(),
                "duplicate@example.com".to_string(),
                None,
                vec![RoleType::User, RoleType::User],
                false,
            )
            .await;

        assert!(matches!(result, Err(UserServiceError::Validation(_))));
    }

    #[tokio::test]
    async fn create_user_does_not_insert_account_when_role_is_missing() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([Vec::<temps_entities::roles::Model>::new()])
                .into_connection(),
        );
        let service = UserService::new(db.clone());

        let result = service
            .create_user(
                "missing-role".to_string(),
                "missing-role@example.com".to_string(),
                None,
                vec![RoleType::User],
                false,
            )
            .await;

        assert!(matches!(result, Err(UserServiceError::RoleNotFound(_))));
        drop(service);
        let log = Arc::try_unwrap(db)
            .expect("service released the database")
            .into_transaction_log();
        let statements: Vec<_> = log
            .iter()
            .flat_map(|transaction| transaction.statements())
            .collect();
        assert!(statements
            .iter()
            .any(|statement| statement.sql.starts_with("SELECT")));
        assert!(!statements
            .iter()
            .any(|statement| statement.sql.starts_with("INSERT INTO \"users\"")));
    }

    #[tokio::test]
    async fn create_user_rolls_back_when_role_assignment_fails() {
        let new_user = user(false, None);
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![role(RoleType::User)]])
                .append_query_results([vec![new_user]])
                .append_query_errors([DbErr::Custom(
                    "simulated role assignment failure".to_string(),
                )])
                .into_connection(),
        );
        let service = UserService::new(db.clone());

        let result = service
            .create_user(
                "atomic-user".to_string(),
                "atomic@example.com".to_string(),
                None,
                vec![RoleType::User],
                false,
            )
            .await;

        assert!(matches!(result, Err(UserServiceError::Database { .. })));
        drop(service);
        let log = Arc::try_unwrap(db)
            .expect("service released the database")
            .into_transaction_log();
        assert_eq!(
            log.len(),
            1,
            "all provisioning writes share one transaction"
        );
        let statements = log[0].statements();
        assert!(statements
            .iter()
            .any(|statement| statement.sql.starts_with("INSERT INTO \"users\"")));
        assert!(statements
            .iter()
            .any(|statement| { statement.sql.starts_with("INSERT INTO \"user_roles\"") }));
    }

    #[tokio::test]
    async fn create_user_returns_committed_records_without_a_post_commit_read() {
        let new_user = user(false, None);
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![role(RoleType::User)]])
                .append_query_results([vec![new_user.clone()]])
                .append_query_results([vec![temps_entities::user_roles::Model {
                    id: 1,
                    user_id: new_user.id,
                    role_id: 2,
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                }]])
                .into_connection(),
        );
        let service = UserService::new(db.clone());

        let created = service
            .create_user(
                "Created User".to_string(),
                "created@example.com".to_string(),
                None,
                vec![RoleType::User],
                false,
            )
            .await
            .expect("committed user should be returned without another query");

        assert_eq!(created.user.id, new_user.id);
        assert_eq!(created.roles.len(), 1);
        assert_eq!(created.roles[0].name, RoleType::User.as_str());
        drop(service);
        let log = Arc::try_unwrap(db)
            .expect("service released the database")
            .into_transaction_log();
        let statements: Vec<_> = log
            .iter()
            .flat_map(|transaction| transaction.statements())
            .collect();
        assert_eq!(
            statements
                .iter()
                .filter(|statement| statement.sql.starts_with("SELECT"))
                .count(),
            1,
            "only role resolution may read during account creation"
        );
    }

    #[tokio::test]
    async fn verification_fails_closed_when_mfa_is_disabled() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![user(false, None)]])
                .into_connection(),
        );
        let service = UserService::new(db);

        let result = service.verify_mfa_code(7, "anything").await;
        assert!(matches!(result, Err(UserServiceError::MfaNotSetup(7))));
    }

    #[tokio::test]
    async fn mfa_setup_limits_parallel_work_globally_and_per_user() {
        use std::time::Duration;

        let service = UserService::new(Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres).into_connection(),
        ));
        let first_user = service
            .acquire_mfa_setup_permit(7)
            .await
            .expect("first setup should acquire a permit");

        let duplicate = tokio::time::timeout(
            Duration::from_millis(25),
            service.acquire_mfa_setup_permit(7),
        )
        .await;
        assert!(
            duplicate.is_err(),
            "a second setup for the same account must be serialized"
        );

        let second_user = service
            .acquire_mfa_setup_permit(8)
            .await
            .expect("a second account may use the remaining global permit");
        let global_overflow = tokio::time::timeout(
            Duration::from_millis(25),
            service.acquire_mfa_setup_permit(9),
        )
        .await;
        assert!(
            global_overflow.is_err(),
            "MFA setup must cap total concurrent password hashing"
        );

        drop(first_user);
        service
            .acquire_mfa_setup_permit(9)
            .await
            .expect("a queued account should proceed after a permit is released");
        drop(second_user);
    }

    #[tokio::test]
    async fn cancelled_mfa_setup_holds_admission_until_blocking_work_exits() {
        use std::sync::mpsc;
        use std::time::Duration;

        let service = Arc::new(UserService::new(Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres).into_connection(),
        )));
        let first = service
            .acquire_mfa_setup_permit(7)
            .await
            .expect("first setup should acquire a permit");
        let second = service
            .acquire_mfa_setup_permit(8)
            .await
            .expect("second setup should fill the global limit");
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = mpsc::channel();

        let task = tokio::spawn(run_mfa_setup_blocking(first, move || {
            let _ = started_tx.send(());
            release_rx.recv().map_err(|error| {
                UserServiceError::Internal(format!(
                    "MFA cancellation test release failed: {}",
                    error
                ))
            })?;
            Ok(())
        }));
        started_rx
            .await
            .expect("blocking setup should report that it started");
        task.abort();
        let cancelled = task.await;
        assert!(matches!(cancelled, Err(error) if error.is_cancelled()));

        assert!(
            tokio::time::timeout(
                Duration::from_millis(25),
                service.acquire_mfa_setup_permit(7)
            )
            .await
            .is_err(),
            "cancellation must not release the per-user guard while work continues"
        );
        assert!(
            tokio::time::timeout(
                Duration::from_millis(25),
                service.acquire_mfa_setup_permit(9)
            )
            .await
            .is_err(),
            "cancellation must not release the global permit while work continues"
        );

        release_tx
            .send(())
            .expect("blocking work should still own its release receiver");
        tokio::time::timeout(Duration::from_secs(1), service.acquire_mfa_setup_permit(9))
            .await
            .expect("permit should be released after blocking work exits")
            .expect("limiter should remain open");
        drop(second);
    }

    #[tokio::test]
    async fn setup_mfa_retry_rotates_complete_unverified_credential_bundle() {
        let mut pending = user(false, Some("[\"existing-hash\"]".to_string()));
        pending.mfa_secret = Some("JBSWY3DPEHPK3PXPJBSWY3DPEHPK3PXP".to_string());
        let updated = pending.clone();
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![pending.clone()], vec![pending], vec![updated]])
                .into_connection(),
        );
        let service = UserService::new(db.clone());

        let setup = service
            .setup_mfa(7)
            .await
            .expect("pending MFA setup should be resumable");

        assert_ne!(setup.secret_key, "JBSWY3DPEHPK3PXPJBSWY3DPEHPK3PXP");
        assert_eq!(setup.recovery_codes.len(), 8);
        assert!(setup.recovery_codes.iter().all(
            |code| code.len() == 6 && code.chars().all(|character| character.is_ascii_digit())
        ));
        assert!(setup.qr_code.starts_with("data:image/png;base64,"));

        drop(service);
        let log = Arc::try_unwrap(db)
            .expect("service released the database")
            .into_transaction_log();
        let statements: Vec<_> = log
            .iter()
            .flat_map(|transaction| transaction.statements())
            .collect();
        assert!(statements
            .iter()
            .any(|statement| statement.sql.contains("FOR UPDATE")));
        assert!(statements
            .iter()
            .any(|statement| statement.sql.starts_with("UPDATE \"users\"")));
    }

    #[tokio::test]
    async fn setup_mfa_rechecks_enabled_state_under_the_write_lock() {
        let pending = user(false, Some("[\"pending-hash\"]".to_string()));
        let enabled = user(true, Some("[\"existing-hash\"]".to_string()));
        let expected_secret = enabled.mfa_secret.clone();
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![pending], vec![enabled]])
                .into_connection(),
        );
        let service = UserService::new(db.clone());

        let result = service.setup_mfa(7).await;

        assert!(matches!(
            result,
            Err(UserServiceError::MfaAlreadyEnabled(7))
        ));
        drop(service);
        let log = Arc::try_unwrap(db)
            .expect("service released the database")
            .into_transaction_log();
        let statements: Vec<_> = log
            .iter()
            .flat_map(|transaction| transaction.statements())
            .collect();
        assert!(statements
            .iter()
            .any(|statement| statement.sql.contains("FOR UPDATE")));
        assert!(!statements
            .iter()
            .any(|statement| statement.sql.starts_with("UPDATE \"users\"")));
        assert!(expected_secret.is_some());
    }

    #[tokio::test]
    async fn recovery_code_is_consumed_under_an_exclusive_row_lock() {
        let recovery_code = "RECOVERY-CODE";
        let hash = argon2::Argon2::default()
            .hash_password(recovery_code.as_bytes(), &SaltString::generate(&mut OsRng))
            .expect("test recovery code hashes")
            .to_string();
        let original = user(
            true,
            Some(serde_json::to_string(&vec![hash]).expect("recovery codes serialize")),
        );
        let consumed = user(true, Some("[]".to_string()));
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![original], vec![consumed]])
                .into_connection(),
        );
        let service = UserService::new(db.clone());

        assert!(service
            .verify_mfa_code(7, recovery_code)
            .await
            .expect("recovery code verifies"));

        drop(service);
        let log = Arc::try_unwrap(db)
            .expect("service released the database")
            .into_transaction_log();
        let statements: Vec<_> = log
            .iter()
            .flat_map(|transaction| transaction.statements())
            .collect();
        assert!(
            statements
                .iter()
                .any(|statement| statement.sql.contains("FOR UPDATE")),
            "verification must lock the user row"
        );
        assert!(
            statements
                .iter()
                .any(|statement| statement.sql.starts_with("UPDATE \"users\"")),
            "verification must consume the recovery code before commit"
        );
    }

    #[tokio::test]
    async fn disabling_mfa_clears_every_session_elevation() {
        let enabled = user(true, None);
        let disabled = user(false, None);
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![enabled], vec![disabled]])
                .append_exec_results([MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 2,
                }])
                .into_connection(),
        );
        let service = UserService::new(db.clone());

        service
            .disable_mfa(7)
            .await
            .expect("MFA disable should clear elevations");

        drop(service);
        let log = Arc::try_unwrap(db)
            .expect("service released the database")
            .into_transaction_log();
        let statements: Vec<_> = log
            .iter()
            .flat_map(|transaction| transaction.statements())
            .collect();
        assert!(statements.iter().any(|statement| {
            statement.sql.starts_with("UPDATE \"sessions\"")
                && statement.sql.contains("step_up_expires_at")
        }));
    }
}
