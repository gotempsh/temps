use base64::Engine;
use chrono::Utc;
use qrcode::QrCode;
use rand::Rng;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder, Set,
};
use serde::{Deserialize, Serialize};
use std::io::Cursor;
use std::sync::Arc;
use temps_core::UtcDateTime;
use temps_entities::types::RoleType;
use thiserror::Error;
use totp_rs::{Algorithm, Secret, TOTP};
use tracing::{error, info};

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

#[derive(Debug, Serialize, Deserialize)]
pub struct ServiceUser {
    pub id: i32,
    pub name: String,
    pub email: String,
    pub image: String,
    pub mfa_enabled: bool,
    pub deleted_at: Option<UtcDateTime>,
    // pub created_at: UtcDateTime,
    // pub updated_at: UtcDateTime,
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
            image: format!(
                "https://ui-avatars.com/api/?name={}&background=random",
                urlencoding::encode(&db_user.name)
            ),
            mfa_enabled: db_user.mfa_enabled,
            deleted_at: db_user.deleted_at,
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
}

impl UserService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
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

        info!("Initialized default roles");
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
    ) -> anyhow::Result<UserWithRoles, UserServiceError> {
        let now = Utc::now();

        // Hash password if provided
        let password_hash = if let Some(pwd) = password {
            Some(bcrypt::hash(pwd, bcrypt::DEFAULT_COST).map_err(|e| {
                UserServiceError::Internal(format!("Failed to hash password: {}", e))
            })?)
        } else {
            None
        };

        // Create the user
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
            ..Default::default()
        };

        let user = new_user.insert(self.db.as_ref()).await?;

        // Assign roles
        for role_type in roles {
            let role = temps_entities::roles::Entity::find()
                .filter(temps_entities::roles::Column::Name.eq(role_type.as_str()))
                .one(self.db.as_ref())
                .await?
                .ok_or_else(|| {
                    UserServiceError::RoleNotFound(format!("Role {} not found", role_type.as_str()))
                })?;

            let new_user_role = temps_entities::user_roles::ActiveModel {
                user_id: Set(user.id),
                role_id: Set(role.id),
                created_at: Set(now),
                updated_at: Set(now),
                ..Default::default()
            };

            new_user_role.insert(self.db.as_ref()).await?;
        }

        info!("Created new user with id: {}", user.id);

        // Fetch the user with roles to return
        self.get_user_with_roles(user.id).await
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

        // Soft delete the user by setting deleted_at
        let now = Utc::now();
        let mut user_update: temps_entities::users::ActiveModel = user.into();
        user_update.deleted_at = Set(Some(now));

        let deleted_user = user_update.update(self.db.as_ref()).await?;

        info!("Soft deleted user with id: {}", user_id);
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
        let user = temps_entities::users::Entity::find_by_id(user_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| UserServiceError::NotFound(format!("User {} not found", user_id)))?;

        // Generate random secret with explicit type
        let secret: Vec<u8> = (0..20).map(|_| rand::thread_rng().gen::<u8>()).collect();
        let secret_b32 = base32::encode(base32::Alphabet::Rfc4648 { padding: true }, &secret);

        // Generate recovery codes
        let recovery_codes: Vec<String> = (0..8)
            .map(|_| {
                let code: String = (0..6)
                    .map(|_| rand::thread_rng().gen_range(0..10).to_string())
                    .collect();
                code
            })
            .collect();

        // Hash recovery codes before storing
        let hashed_recovery_codes: Vec<String> = recovery_codes
            .iter()
            .map(|code| {
                bcrypt::hash(code, bcrypt::DEFAULT_COST).map_err(|e| {
                    UserServiceError::Mfa(format!("Failed to hash recovery code: {}", e))
                })
            })
            .collect::<Result<Vec<String>, UserServiceError>>()?;

        // Create TOTP with proper parameters & verify it
        TOTP::new(
            Algorithm::SHA1,
            6,
            1,
            30,
            Secret::Raw(secret.clone()).to_bytes().map_err(|e| {
                UserServiceError::Mfa(format!("Failed to create TOTP secret: {}", e))
            })?,
        )
        .map_err(|e| UserServiceError::Mfa(format!("Failed to create TOTP: {}", e)))?;

        // Generate the otpauth URL manually
        let otp_auth_url = format!(
            "otpauth://totp/Temps:{}?secret={}&issuer=Temps&algorithm=SHA1&digits=6&period=30",
            user.email, // Use email for MFA identifier
            secret_b32
        );

        // Generate QR code
        let qr = QrCode::new(otp_auth_url)
            .map_err(|e| UserServiceError::Mfa(format!("Failed to generate QR code: {}", e)))?;
        let qr_image = qr.render::<image::Luma<u8>>().quiet_zone(false).build();

        // Convert QR code to base64 PNG
        let mut bytes: Vec<u8> = Vec::new();
        qr_image
            .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)
            .map_err(|e| UserServiceError::Mfa(format!("Failed to encode QR code: {}", e)))?;
        let qr_base64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        let qr_data_url = format!("data:image/png;base64,{}", qr_base64);

        // Update user in database
        let mut user_update: temps_entities::users::ActiveModel = user.into();
        user_update.mfa_secret = Set(Some(secret_b32.clone()));
        user_update.mfa_enabled = Set(false);
        user_update.mfa_recovery_codes = Set(Some(
            serde_json::to_string(&hashed_recovery_codes)
                .map_err(UserServiceError::Serialization)?,
        ));

        user_update.update(self.db.as_ref()).await?;

        Ok(MfaSetupData {
            secret_key: secret_b32,
            qr_code: qr_data_url,
            recovery_codes,
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
        let user = temps_entities::users::Entity::find_by_id(user_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| UserServiceError::NotFound(format!("User {} not found", user_id)))?;

        if !user.mfa_enabled {
            return Ok(true); // MFA not enabled, always pass
        }

        let secret = user
            .mfa_secret
            .ok_or(UserServiceError::MfaNotSetup(user_id))?;

        // Check if it's a recovery code
        if let Some(recovery_codes) = user.mfa_recovery_codes {
            let hashed_codes: Vec<String> =
                serde_json::from_str(&recovery_codes).map_err(UserServiceError::Serialization)?;

            // Clone hashed_codes for later use
            let hashed_codes_clone = hashed_codes.clone();

            // First check if the code matches any recovery code
            for hashed_code in hashed_codes {
                if bcrypt::verify(code, &hashed_code).map_err(|e| {
                    UserServiceError::Encryption(format!("Failed to verify recovery code: {}", e))
                })? {
                    // Remove used recovery code using the cloned vector
                    let new_codes: Vec<String> = hashed_codes_clone
                        .into_iter()
                        .filter(|c| c != &hashed_code)
                        .collect();

                    let mut user_update: temps_entities::users::ActiveModel =
                        temps_entities::users::Entity::find_by_id(user_id)
                            .one(self.db.as_ref())
                            .await?
                            .ok_or_else(|| {
                                UserServiceError::NotFound(format!("User {} not found", user_id))
                            })?
                            .into();
                    user_update.mfa_recovery_codes = Set(Some(serde_json::to_string(&new_codes)?));
                    user_update.update(self.db.as_ref()).await?;

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

    pub async fn disable_mfa(&self, user_id: i32) -> Result<(), UserServiceError> {
        let user = temps_entities::users::Entity::find_by_id(user_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| UserServiceError::NotFound(format!("User {} not found", user_id)))?;

        let mut user_update: temps_entities::users::ActiveModel = user.into();
        user_update.mfa_secret = Set(None);
        user_update.mfa_enabled = Set(false);
        user_update.mfa_recovery_codes = Set(None);

        user_update.update(self.db.as_ref()).await?;

        Ok(())
    }

    pub async fn verify_and_disable_mfa(
        &self,
        user_id: i32,
        code: &str,
    ) -> anyhow::Result<(), UserServiceError> {
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
