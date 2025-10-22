use crate::externalsvc::{
    postgres::PostgresService, redis::RedisService, s3::S3Service, ExternalService, ServiceConfig,
    ServiceParameter, ServiceType,
};
use crate::types::EnvironmentVariableInfo;
use anyhow::Result;
use bollard::Docker;
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder, Set,
    TransactionTrait,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use temps_entities::{
    external_service_backups, external_service_params, external_services, project_services,
    projects,
};
use thiserror::Error;
use tracing::{error, info};
// use crate::routes::types::external_services::EnvironmentVariableInfo;
use temps_core::EncryptionService;
// Add these constants at the top of the file proper key management
#[allow(dead_code)]
const NONCE_LENGTH: usize = 12;

#[derive(Error, Debug)]
pub enum ExternalServiceError {
    #[error("Service {id} not found")]
    ServiceNotFound { id: i32 },

    #[error("Service with name '{name}' not found")]
    ServiceNotFoundByName { name: String },

    #[error("Service with slug '{slug}' not found")]
    ServiceNotFoundBySlug { slug: String },

    #[error("Failed to initialize service {id}: {reason}")]
    InitializationFailed { id: i32, reason: String },

    #[error("Failed to encrypt parameter '{param_name}' for service {service_id}: {reason}")]
    EncryptionFailed {
        service_id: i32,
        param_name: String,
        reason: String,
    },

    #[error("Failed to decrypt parameter '{param_name}' for service {service_id}: {reason}")]
    DecryptionFailed {
        service_id: i32,
        param_name: String,
        reason: String,
    },

    #[error("Invalid service type '{service_type}' for service {id}")]
    InvalidServiceType { id: i32, service_type: String },

    #[error("Service {service_id} is not linked to project {project_id}")]
    ServiceNotLinkedToProject { service_id: i32, project_id: i32 },

    #[error("Project {id} not found")]
    ProjectNotFound { id: i32 },

    #[error("Database error: {reason}")]
    DatabaseError { reason: String },

    #[error("Parameter validation failed for service {service_id}: {reason}")]
    ParameterValidationFailed { service_id: i32, reason: String },

    #[error("Failed to start service {id}: {reason}")]
    StartFailed { id: i32, reason: String },

    #[error("Failed to stop service {id}: {reason}")]
    StopFailed { id: i32, reason: String },

    #[error("Failed to delete service {id}: {reason}")]
    DeletionFailed { id: i32, reason: String },

    #[error("Environment variable '{var_name}' not found for service {service_id}")]
    EnvironmentVariableNotFound { service_id: i32, var_name: String },

    #[error("Access denied for encrypted variable '{var_name}' in service {service_id}")]
    EncryptedVariableAccessDenied { service_id: i32, var_name: String },

    #[error("Docker operation failed for service {id}: {reason}")]
    DockerError { id: i32, reason: String },

    #[error("Internal error: {reason}")]
    InternalError { reason: String },
}

impl From<sea_orm::DbErr> for ExternalServiceError {
    fn from(err: sea_orm::DbErr) -> Self {
        ExternalServiceError::DatabaseError {
            reason: err.to_string(),
        }
    }
}

impl From<anyhow::Error> for ExternalServiceError {
    fn from(err: anyhow::Error) -> Self {
        ExternalServiceError::InternalError {
            reason: err.to_string(),
        }
    }
}

impl From<sea_orm::TransactionError<ExternalServiceError>> for ExternalServiceError {
    fn from(err: sea_orm::TransactionError<ExternalServiceError>) -> Self {
        match err {
            sea_orm::TransactionError::Connection(e) => ExternalServiceError::DatabaseError {
                reason: e.to_string(),
            },
            sea_orm::TransactionError::Transaction(e) => e,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct CreateExternalServiceRequest {
    pub name: String,
    pub service_type: ServiceType,
    pub version: Option<String>,
    pub parameters: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateExternalServiceRequest {
    pub name: Option<String>,
    pub parameters: HashMap<String, String>,
}

#[derive(Debug, Serialize)]
pub struct ExternalServiceDetails {
    pub service: ExternalServiceInfo,
    pub parameters: Vec<ServiceParameter>,
    pub current_parameters: Option<HashMap<String, String>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExternalServiceInfo {
    pub id: i32,
    pub name: String,
    pub service_type: ServiceType,
    pub version: Option<String>,
    pub status: String,
    pub connection_info: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct ProjectServiceInfo {
    pub id: i32,
    pub project_id: i32,
    pub service: ExternalServiceInfo,
}

pub struct ExternalServiceManager {
    db: Arc<DatabaseConnection>,
    encryption_service: Arc<EncryptionService>,
    docker: Arc<Docker>,
}

impl ExternalServiceManager {
    pub fn new(
        db: Arc<DatabaseConnection>,
        encryption_service: Arc<EncryptionService>,
        docker: Arc<Docker>,
    ) -> Self {
        Self {
            db,
            encryption_service,
            docker,
        }
    }

    pub async fn get_local_address(
        &self,
        service: external_services::Model,
    ) -> Result<String, ExternalServiceError> {
        // Get service parameters
        let service_config = self.get_service_config(service.id).await?;

        // Create service instance
        let service_instance = self.create_service_instance(
            service.name.clone(),
            ServiceType::from_str(&service.service_type).map_err(|_| {
                ExternalServiceError::InvalidServiceType {
                    id: service.id,
                    service_type: service.service_type.clone(),
                }
            })?,
        );

        // Get local address from service instance
        let address = service_instance
            .get_local_address(service_config)
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!("Failed to get local address: {}", e),
            })?;

        info!(
            "Retrieved local address {} for service {}",
            address, service.id
        );
        Ok(address)
    }
    pub fn get_service_instance(
        &self,
        name: String,
        service_type: ServiceType,
    ) -> Box<dyn ExternalService> {
        self.create_service_instance(name, service_type)
    }
    fn create_service_instance(
        &self,
        name: String,
        service_type: ServiceType,
    ) -> Box<dyn ExternalService> {
        match service_type {
            ServiceType::Postgres => Box::new(PostgresService::new(name, self.docker.clone())),
            ServiceType::Redis => Box::new(RedisService::new(name, self.docker.clone())),
            ServiceType::S3 => Box::new(S3Service::new(name, self.docker.clone())),
        }
    }

    pub async fn get_service_by_name(
        &self,
        name_param: &str,
    ) -> Result<external_services::Model, ExternalServiceError> {
        let service = external_services::Entity::find()
            .filter(external_services::Column::Name.eq(name_param))
            .one(self.db.as_ref())
            .await?;

        service.ok_or(ExternalServiceError::ServiceNotFoundByName {
            name: name_param.to_string(),
        })
    }

    pub async fn get_service_by_slug(
        &self,
        slug_param: &str,
    ) -> Result<external_services::Model, ExternalServiceError> {
        let service = external_services::Entity::find()
            .filter(external_services::Column::Name.eq(slug_param))
            .one(self.db.as_ref())
            .await?;

        service.ok_or(ExternalServiceError::ServiceNotFoundBySlug {
            slug: slug_param.to_string(),
        })
    }

    pub async fn create_service(
        &self,
        request: CreateExternalServiceRequest,
    ) -> Result<ExternalServiceInfo, ExternalServiceError> {
        info!("Creating new external service");
        let service_slug = Self::generate_slug(&request.name);

        // Create service instance and validate parameters before transaction
        let service_instance =
            self.create_service_instance(request.name.clone(), request.service_type.clone());

        if let Err(e) = service_instance.validate_parameters(&request.parameters) {
            return Err(ExternalServiceError::ParameterValidationFailed {
                service_id: 0, // Not created yet
                reason: e.to_string(),
            });
        }

        let encryption_service = Arc::clone(&self.encryption_service);
        let param_defs = service_instance.get_parameter_definitions();

        // Start transaction
        let service = self
            .db
            .transaction::<_, external_services::Model, ExternalServiceError>(|txn| {
                Box::pin(async move {
                    // Create service record
                    let new_service = external_services::ActiveModel {
                        name: Set(request.name.clone()),
                        slug: Set(Some(service_slug.clone())),
                        service_type: Set(request.service_type.to_string()),
                        version: Set(request.version.clone()),
                        status: Set("pending".to_string()),
                        created_at: Set(Utc::now()),
                        updated_at: Set(Utc::now()),
                        ..Default::default()
                    };

                    let service = new_service.insert(txn).await?;

                    // Store parameters
                    for (key, value) in &request.parameters {
                        let param_def =
                            param_defs.iter().find(|p| p.name == *key).ok_or_else(|| {
                                ExternalServiceError::ParameterValidationFailed {
                                    service_id: service.id,
                                    reason: format!("Parameter '{}' not found in definitions", key),
                                }
                            })?;

                        let encrypted_value = if param_def.encrypted {
                            encryption_service.encrypt_string(value).map_err(|e| {
                                ExternalServiceError::EncryptionFailed {
                                    service_id: service.id,
                                    param_name: key.clone(),
                                    reason: e.to_string(),
                                }
                            })?
                        } else {
                            value.clone()
                        };

                        // Insert parameter (all params are now encrypted)
                        let new_param = external_service_params::ActiveModel {
                            service_id: Set(service.id),
                            key: Set(key.clone()),
                            value: Set(encrypted_value),
                            created_at: Set(Utc::now()),
                            updated_at: Set(Utc::now()),
                            ..Default::default()
                        };

                        new_param.insert(txn).await?;
                    }

                    Ok(service)
                })
            })
            .await
            .map_err(ExternalServiceError::from)?;

        // Initialize the service
        self.initialize_service(service.id).await.map_err(|e| {
            ExternalServiceError::InitializationFailed {
                id: service.id,
                reason: e.to_string(),
            }
        })?;

        self.get_service_info(service.id).await
    }

    pub async fn get_service_config(
        &self,
        service_id: i32,
    ) -> Result<ServiceConfig, ExternalServiceError> {
        let service = self.get_service(service_id).await?;
        let service_type = ServiceType::from_str(&service.service_type).map_err(|_| {
            ExternalServiceError::InvalidServiceType {
                id: service_id,
                service_type: service.service_type.clone(),
            }
        })?;

        let _service_instance =
            self.create_service_instance(service.name.clone(), service_type.clone());
        let parameters = self.get_service_parameters(service_id).await?;

        let config = ServiceConfig {
            name: service.name.clone(),
            service_type,
            version: service.version,
            parameters: serde_json::to_value(parameters).map_err(|e| {
                ExternalServiceError::InternalError {
                    reason: format!("Failed to serialize parameters: {}", e),
                }
            })?,
        };

        Ok(config)
    }

    pub async fn list_services(&self) -> Result<Vec<ExternalServiceInfo>, ExternalServiceError> {
        let services = external_services::Entity::find()
            .order_by_desc(external_services::Column::CreatedAt)
            .all(self.db.as_ref())
            .await?;

        let mut result = Vec::new();
        for service in services {
            result.push(self.get_service_info(service.id).await?);
        }

        Ok(result)
    }

    pub async fn get_service_details(
        &self,
        service_id: i32,
    ) -> Result<ExternalServiceDetails, ExternalServiceError> {
        let service_info = self.get_service_info(service_id).await?;
        let parameters = self.get_service_parameters(service_id).await?;
        let service_type =
            ServiceType::from_str(&service_info.service_type.to_string()).map_err(|_| {
                ExternalServiceError::InvalidServiceType {
                    id: service_id,
                    service_type: service_info.service_type.to_string(),
                }
            })?;

        let service_instance =
            self.create_service_instance(service_info.name.clone(), service_type);

        Ok(ExternalServiceDetails {
            service: service_info,
            parameters: service_instance.get_parameter_definitions(),
            current_parameters: Some(parameters),
        })
    }

    pub async fn update_service(
        &self,
        service_id: i32,
        request: UpdateExternalServiceRequest,
    ) -> Result<ExternalServiceInfo, ExternalServiceError> {
        let service = self.get_service(service_id).await?;
        let service_type = ServiceType::from_str(&service.service_type.clone()).map_err(|_| {
            ExternalServiceError::InvalidServiceType {
                id: service_id,
                service_type: service.service_type.clone(),
            }
        })?;

        // Generate new slug if name is being updated
        if let Some(new_name) = &request.name {
            let new_slug = Self::generate_slug(new_name);

            // Update the service name and slug
            let mut service_update: external_services::ActiveModel = service.clone().into();
            service_update.name = Set(new_name.clone());
            service_update.slug = Set(Some(new_slug));
            service_update.updated_at = Set(Utc::now());
            service_update.update(self.db.as_ref()).await?;
        }

        let service_instance = self.create_service_instance(service.name.clone(), service_type);

        // Validate parameters
        service_instance
            .validate_parameters(&request.parameters)
            .map_err(|e| ExternalServiceError::ParameterValidationFailed {
                service_id,
                reason: e.to_string(),
            })?;

        // Clone encryption service to avoid lifetime issues
        let encryption_service = Arc::clone(&self.encryption_service);

        // Update parameters in transaction
        self.db
            .transaction::<_, (), ExternalServiceError>(move |txn| {
                Box::pin(async move {
                    // Delete existing parameters
                    external_service_params::Entity::delete_many()
                        .filter(external_service_params::Column::ServiceId.eq(service_id))
                        .exec(txn)
                        .await?;

                    // Insert new parameters
                    for (key, value) in &request.parameters {
                        let param_defs = service_instance.get_parameter_definitions();
                        let param_def = param_defs.iter().find(|p| p.name == *key).ok_or(
                            ExternalServiceError::ParameterValidationFailed {
                                service_id,
                                reason: format!("Parameter '{}' not found in definitions", key),
                            },
                        )?;

                        let encrypted_value = if param_def.encrypted {
                            encryption_service.encrypt_string(value).map_err(|e| {
                                ExternalServiceError::EncryptionFailed {
                                    service_id,
                                    param_name: key.clone(),
                                    reason: e.to_string(),
                                }
                            })?
                        } else {
                            value.clone()
                        };

                        let new_param = external_service_params::ActiveModel {
                            service_id: Set(service_id),
                            key: Set(key.clone()),
                            value: Set(encrypted_value),
                            created_at: Set(Utc::now()),
                            updated_at: Set(Utc::now()),
                            ..Default::default()
                        };

                        new_param.insert(txn).await?;
                    }

                    Ok(())
                })
            })
            .await
            .map_err(ExternalServiceError::from)?;

        // Reinitialize the service
        self.initialize_service(service_id).await?;

        self.get_service_info(service_id).await
    }

    pub async fn delete_service(&self, service_id: i32) -> Result<(), ExternalServiceError> {
        // Get service to check if it exists
        let service = self.get_service(service_id).await?;
        let service_type_enum = ServiceType::from_str(&service.service_type).map_err(|_| {
            ExternalServiceError::InvalidServiceType {
                id: service_id,
                service_type: service.service_type,
            }
        })?;

        let service_instance =
            self.create_service_instance(service.name.clone(), service_type_enum);

        // Delete from database
        self.db
            .transaction::<_, (), ExternalServiceError>(|txn| {
                Box::pin(async move {
                    // Delete project links
                    project_services::Entity::delete_many()
                        .filter(project_services::Column::ServiceId.eq(service_id))
                        .exec(txn)
                        .await?;

                    // Delete parameters first due to foreign key constraint
                    external_service_params::Entity::delete_many()
                        .filter(external_service_params::Column::ServiceId.eq(service_id))
                        .exec(txn)
                        .await?;

                    // Delete service backups
                    external_service_backups::Entity::delete_many()
                        .filter(external_service_backups::Column::ServiceId.eq(service_id))
                        .exec(txn)
                        .await?;

                    // Delete service
                    external_services::Entity::delete_by_id(service_id)
                        .exec(txn)
                        .await?;

                    Ok(())
                })
            })
            .await
            .map_err(ExternalServiceError::from)?;

        // Stop the service
        info!("Stopping service {} before deletion", service_id);
        service_instance
            .remove()
            .await
            .map_err(|e| ExternalServiceError::DeletionFailed {
                id: service_id,
                reason: e.to_string(),
            })?;

        Ok(())
    }

    pub async fn check_service_health(&self, service_id: i32) -> Result<bool> {
        let _service = self.get_service(service_id).await?;

        Ok(false)
    }

    // Helper methods
    async fn get_service(
        &self,
        service_id: i32,
    ) -> Result<external_services::Model, ExternalServiceError> {
        external_services::Entity::find_by_id(service_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(ExternalServiceError::ServiceNotFound { id: service_id })
    }

    async fn get_service_info(
        &self,
        service_id: i32,
    ) -> Result<ExternalServiceInfo, ExternalServiceError> {
        let service = self.get_service(service_id).await?;

        Ok(ExternalServiceInfo {
            id: service.id,
            name: service.name,
            service_type: ServiceType::from_str(&service.service_type).map_err(|_| {
                ExternalServiceError::InvalidServiceType {
                    id: service_id,
                    service_type: service.service_type,
                }
            })?,
            version: service.version,
            status: service.status,
            connection_info: None,
            created_at: service.created_at.to_rfc3339(),
            updated_at: service.updated_at.to_rfc3339(),
        })
    }

    async fn get_service_parameters(
        &self,
        service_id_val: i32,
    ) -> Result<HashMap<String, String>, ExternalServiceError> {
        let params = external_service_params::Entity::find()
            .filter(external_service_params::Column::ServiceId.eq(service_id_val))
            .all(self.db.as_ref())
            .await?;

        // Get service to determine parameter definitions
        let service = self.get_service(service_id_val).await?;
        let service_type = ServiceType::from_str(&service.service_type).map_err(|_| {
            ExternalServiceError::InvalidServiceType {
                id: service_id_val,
                service_type: service.service_type.clone(),
            }
        })?;
        let service_instance = self.create_service_instance(service.name.clone(), service_type);
        let param_defs = service_instance.get_parameter_definitions();

        let mut result = HashMap::new();
        for param in params {
            // Only decrypt if parameter is marked as encrypted in definition
            let param_def = param_defs.iter().find(|p| p.name == param.key);
            let is_encrypted = param_def.map(|p| p.encrypted).unwrap_or(false);

            let value_val = if is_encrypted {
                // Decrypt encrypted parameters
                self.encryption_service
                    .decrypt_string(&param.value)
                    .map_err(|e| ExternalServiceError::DecryptionFailed {
                        service_id: service_id_val,
                        param_name: param.key.clone(),
                        reason: e.to_string(),
                    })?
            } else {
                // Return plaintext parameters as-is
                param.value
            };

            result.insert(param.key, value_val);
        }

        Ok(result)
    }

    async fn initialize_service(&self, service_id: i32) -> Result<(), ExternalServiceError> {
        info!("Initializing service: {}", service_id);
        let service = self.get_service(service_id).await?;
        let parameters = self.get_service_parameters(service_id).await?;
        let service_type_enum = ServiceType::from_str(&service.service_type).map_err(|_| {
            ExternalServiceError::InvalidServiceType {
                id: service_id,
                service_type: service.service_type.clone(),
            }
        })?;

        let service_instance =
            self.create_service_instance(service.name.clone(), service_type_enum);

        let config = ServiceConfig {
            name: service.name.clone(),
            service_type: ServiceType::from_str(&service.service_type).map_err(|_| {
                ExternalServiceError::InvalidServiceType {
                    id: service_id,
                    service_type: service.service_type.clone(),
                }
            })?,
            version: service.version.clone(),
            parameters: serde_json::to_value(parameters).map_err(|e| {
                ExternalServiceError::InternalError {
                    reason: format!("Failed to serialize parameters: {}", e),
                }
            })?,
        };

        // Initialize the service
        let inferred_params = service_instance.init(config).await.map_err(|e| {
            ExternalServiceError::InitializationFailed {
                id: service_id,
                reason: e.to_string(),
            }
        })?;

        // Store inferred parameters
        self.store_inferred_parameters(service_id, service_instance, inferred_params)
            .await?;

        // Update status to initialized
        let mut service_update: external_services::ActiveModel = service.clone().into();
        service_update.status = Set("running".to_string());
        service_update.updated_at = Set(Utc::now());
        service_update.update(self.db.as_ref()).await?;

        Ok(())
    }

    async fn store_inferred_parameters(
        &self,
        service_id: i32,
        service_instance: Box<dyn ExternalService>,
        inferred_params: HashMap<String, String>,
    ) -> Result<(), ExternalServiceError> {
        let param_defs = service_instance.get_parameter_definitions();

        for (key, value) in inferred_params {
            let param_def = param_defs.iter().find(|p| p.name == key);
            let default_service_param = &ServiceParameter {
                name: key.clone(),
                required: false,
                encrypted: false,
                description: "Inferred parameter".to_string(),
                default_value: None,
                validation_pattern: None,
            };
            let param_def = param_def.unwrap_or(default_service_param);

            let encrypted_value = if param_def.encrypted {
                self.encryption_service
                    .encrypt_string(&value)
                    .map_err(|e| ExternalServiceError::EncryptionFailed {
                        service_id,
                        param_name: key.clone(),
                        reason: e.to_string(),
                    })?
            } else {
                value
            };

            let new_param = external_service_params::ActiveModel {
                service_id: Set(service_id),
                key: Set(key.clone()),
                value: Set(encrypted_value.clone()),
                created_at: Set(Utc::now()),
                updated_at: Set(Utc::now()),
                ..Default::default()
            };

            // Try to insert, if it fails due to conflict, try to update
            match new_param.insert(self.db.as_ref()).await {
                Ok(_) => {
                    // Insert successful, continue
                }
                Err(_) => {
                    // If insert fails, try to update existing parameter
                    let existing = external_service_params::Entity::find()
                        .filter(external_service_params::Column::ServiceId.eq(service_id))
                        .filter(external_service_params::Column::Key.eq(&key))
                        .one(self.db.as_ref())
                        .await?;

                    if let Some(existing_param) = existing {
                        let mut update_param: external_service_params::ActiveModel =
                            existing_param.into();
                        update_param.value = Set(encrypted_value);
                        update_param.updated_at = Set(Utc::now());
                        update_param.update(self.db.as_ref()).await?;
                    }
                }
            }
        }

        Ok(())
    }

    // Add this new helper method
    fn generate_slug(name: &str) -> String {
        name.to_lowercase()
            .chars()
            .filter_map(|c| {
                if c.is_alphanumeric() {
                    Some(c)
                } else if c.is_whitespace() {
                    Some('-')
                } else {
                    None
                }
            })
            .collect()
    }

    /// Encrypts all parameters for external services that are marked as encrypted but not yet encrypted
    pub async fn encrypt_all_parameters(&self) -> Result<(), ExternalServiceError> {
        info!("Starting encryption of all external service parameters...");

        // Get all external services
        let services = external_services::Entity::find()
            .all(self.db.as_ref())
            .await?;

        for service in services {
            // Get service type to determine which parameters should be encrypted
            let _service_type = ServiceType::from_str(&service.service_type).map_err(|_| {
                ExternalServiceError::InvalidServiceType {
                    id: service.id,
                    service_type: service.service_type.clone(),
                }
            })?;

            // Get all parameters for this service
            let params = external_service_params::Entity::find()
                .filter(external_service_params::Column::ServiceId.eq(service.id))
                .all(self.db.as_ref())
                .await?;

            for param in params {
                // Since is_encrypted is removed, we assume all old params might be unencrypted
                // Try to decrypt first, if that fails, assume it's unencrypted and encrypt it
                let needs_encryption = self
                    .encryption_service
                    .decrypt_string(&param.value)
                    .is_err();

                if needs_encryption && !param.value.is_empty() {
                    info!(
                        "Encrypting parameter {} for service {}",
                        param.key, service.id
                    );

                    let encrypted_value = self
                        .encryption_service
                        .encrypt_string(&param.value)
                        .map_err(|e| ExternalServiceError::EncryptionFailed {
                            service_id: service.id,
                            param_name: param.key.clone(),
                            reason: e.to_string(),
                        })?;

                    let mut param_update: external_service_params::ActiveModel = param.into();
                    param_update.value = Set(encrypted_value);
                    param_update.updated_at = Set(Utc::now());
                    param_update.update(self.db.as_ref()).await?;
                }
            }
        }

        info!("Completed encryption of all external service parameters");
        Ok(())
    }

    pub async fn start_service(
        &self,
        service_id: i32,
    ) -> Result<ExternalServiceInfo, ExternalServiceError> {
        let service = self.get_service(service_id).await?;
        let service_type_enum = ServiceType::from_str(&service.service_type).map_err(|_| {
            ExternalServiceError::InvalidServiceType {
                id: service_id,
                service_type: service.service_type.clone(),
            }
        })?;

        let service_instance =
            self.create_service_instance(service.name.clone(), service_type_enum);

        // Start the service
        service_instance
            .start()
            .await
            .map_err(|e| ExternalServiceError::StartFailed {
                id: service_id,
                reason: e.to_string(),
            })?;

        // Update status to running
        let mut service_update: external_services::ActiveModel = service.into();
        service_update.status = Set("running".to_string());
        service_update.updated_at = Set(Utc::now());
        service_update.update(self.db.as_ref()).await?;

        self.get_service_info(service_id).await
    }

    pub async fn stop_service(
        &self,
        service_id: i32,
    ) -> Result<ExternalServiceInfo, ExternalServiceError> {
        let service = self.get_service(service_id).await?;
        let service_type_enum = ServiceType::from_str(&service.service_type).map_err(|_| {
            ExternalServiceError::InvalidServiceType {
                id: service_id,
                service_type: service.service_type.clone(),
            }
        })?;

        let service_instance =
            self.create_service_instance(service.name.clone(), service_type_enum);

        // Stop the service
        service_instance
            .stop()
            .await
            .map_err(|e| ExternalServiceError::StopFailed {
                id: service_id,
                reason: e.to_string(),
            })?;

        // Update status to stopped
        let mut service_update: external_services::ActiveModel = service.into();
        service_update.status = Set("stopped".to_string());
        service_update.updated_at = Set(Utc::now());
        service_update.update(self.db.as_ref()).await?;

        self.get_service_info(service_id).await
    }

    pub async fn link_service_to_project(
        &self,
        service_id_val: i32,
        project_id_val: i32,
    ) -> Result<ProjectServiceInfo, ExternalServiceError> {
        // Verify service exists
        let _service = self.get_service(service_id_val).await?;

        // Verify project exists
        let _project = projects::Entity::find_by_id(project_id_val)
            .one(self.db.as_ref())
            .await?
            .ok_or(ExternalServiceError::ProjectNotFound { id: project_id_val })?;

        // Create link
        let new_link = project_services::ActiveModel {
            project_id: Set(project_id_val),
            service_id: Set(service_id_val),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };

        let link = new_link.insert(self.db.as_ref()).await?;
        let service_info = self.get_service_info(service_id_val).await?;

        Ok(ProjectServiceInfo {
            id: link.id,
            project_id: link.project_id,
            service: service_info,
        })
    }

    pub async fn get_service_environment_variables(
        &self,
        service_id_val: i32,
        _project_id_val: i32,
    ) -> Result<HashMap<String, String>, ExternalServiceError> {
        let service = self.get_service(service_id_val).await?;
        let service_type = ServiceType::from_str(&service.service_type).map_err(|_| {
            ExternalServiceError::InvalidServiceType {
                id: service_id_val,
                service_type: service.service_type.clone(),
            }
        })?;
        let parameters = self.get_service_parameters(service_id_val).await?;

        let service_instance = self.create_service_instance(service.name.clone(), service_type);

        // Get connection info from the service instance
        service_instance
            .get_environment_variables(&parameters)
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!("Failed to get environment variables: {}", e),
            })
    }

    pub async fn get_runtime_env_vars(
        &self,
        service_id_val: i32,
        project_slug: String,
        environment_slug: String,
    ) -> Result<HashMap<String, String>, ExternalServiceError> {
        // Get service
        let service = self.get_service(service_id_val).await?;
        let service_type = ServiceType::from_str(&service.service_type).map_err(|_| {
            ExternalServiceError::InvalidServiceType {
                id: service_id_val,
                service_type: service.service_type.clone(),
            }
        })?;

        // Create service instance
        let service_instance =
            self.create_service_instance(service.name.clone(), service_type.clone());
        let parameters = self.get_service_parameters(service_id_val).await?;
        let service_config = ServiceConfig {
            name: service.name.clone(),
            service_type,
            version: service.version,
            parameters: serde_json::to_value(parameters).map_err(|e| {
                ExternalServiceError::InternalError {
                    reason: format!("Failed to serialize parameters: {}", e),
                }
            })?,
        };

        // Get runtime environment variables
        service_instance
            .get_runtime_env_vars(service_config, &project_slug, &environment_slug)
            .await
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!("Failed to get runtime environment variables: {}", e),
            })
    }

    pub async fn get_service_docker_environment_variables(
        &self,
        service_id_val: i32,
        project_id_val: i32,
    ) -> Result<HashMap<String, String>, ExternalServiceError> {
        // Verify service exists
        let service = self.get_service(service_id_val).await?;
        let service_type = ServiceType::from_str(&service.service_type).map_err(|_| {
            ExternalServiceError::InvalidServiceType {
                id: service_id_val,
                service_type: service.service_type.clone(),
            }
        })?;

        // Verify service is linked to project
        let link_exists = project_services::Entity::find()
            .filter(
                project_services::Column::ServiceId
                    .eq(service_id_val)
                    .and(project_services::Column::ProjectId.eq(project_id_val)),
            )
            .one(self.db.as_ref())
            .await?;

        if link_exists.is_none() {
            return Err(ExternalServiceError::ServiceNotLinkedToProject {
                service_id: service_id_val,
                project_id: project_id_val,
            });
        }

        let parameters = self.get_service_parameters(service_id_val).await?;
        let service_instance = self.create_service_instance(service.name.clone(), service_type);

        service_instance
            .get_docker_environment_variables(&parameters)
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!("Failed to get docker environment variables: {}", e),
            })
    }

    pub async fn unlink_service_from_project(
        &self,
        service_id_val: i32,
        project_id_val: i32,
    ) -> Result<(), ExternalServiceError> {
        // Verify service exists
        self.get_service(service_id_val).await?;

        // Delete the link
        let deleted = project_services::Entity::delete_many()
            .filter(
                project_services::Column::ServiceId
                    .eq(service_id_val)
                    .and(project_services::Column::ProjectId.eq(project_id_val)),
            )
            .exec(self.db.as_ref())
            .await?;

        if deleted.rows_affected == 0 {
            return Err(ExternalServiceError::ServiceNotLinkedToProject {
                service_id: service_id_val,
                project_id: project_id_val,
            });
        }

        Ok(())
    }

    pub async fn list_service_projects(
        &self,
        service_id_val: i32,
    ) -> Result<Vec<ProjectServiceInfo>, ExternalServiceError> {
        // Verify service exists and get service info
        let service_info = self.get_service_info(service_id_val).await?;

        // Get all project links for this service
        let links = project_services::Entity::find()
            .filter(project_services::Column::ServiceId.eq(service_id_val))
            .all(self.db.as_ref())
            .await?;

        // Convert to ProjectServiceInfo
        let project_services_list = links
            .into_iter()
            .map(|link| ProjectServiceInfo {
                id: link.id,
                project_id: link.project_id,
                service: service_info.clone(),
            })
            .collect();

        Ok(project_services_list)
    }

    pub async fn list_project_services(
        &self,
        project_id_val: i32,
    ) -> Result<Vec<ProjectServiceInfo>, ExternalServiceError> {
        // Verify project exists
        let _project = projects::Entity::find_by_id(project_id_val)
            .one(self.db.as_ref())
            .await?
            .ok_or(ExternalServiceError::ProjectNotFound { id: project_id_val })?;

        // Get all service links for this project
        let links = project_services::Entity::find()
            .filter(project_services::Column::ProjectId.eq(project_id_val))
            .all(self.db.as_ref())
            .await?;

        // Convert to ProjectServiceInfo with service details
        let mut project_services_list = Vec::new();
        for link in links {
            let service_info = self.get_service_info(link.service_id).await?;
            project_services_list.push(ProjectServiceInfo {
                id: link.id,
                project_id: link.project_id,
                service: service_info,
            });
        }

        Ok(project_services_list)
    }

    pub async fn get_service_environment_variable(
        &self,
        service_id_val: i32,
        project_id_val: i32,
        var_name: &str,
    ) -> Result<EnvironmentVariableInfo, ExternalServiceError> {
        let service = self.get_service(service_id_val).await?;
        let service_type = ServiceType::from_str(&service.service_type).map_err(|_| {
            ExternalServiceError::InvalidServiceType {
                id: service_id_val,
                service_type: service.service_type.clone(),
            }
        })?;
        let parameters = self.get_service_parameters(service_id_val).await?;

        // Verify project link exists
        let link_exists = project_services::Entity::find()
            .filter(
                project_services::Column::ServiceId
                    .eq(service_id_val)
                    .and(project_services::Column::ProjectId.eq(project_id_val)),
            )
            .one(self.db.as_ref())
            .await?;

        if link_exists.is_none() {
            return Err(ExternalServiceError::ServiceNotLinkedToProject {
                service_id: service_id_val,
                project_id: project_id_val,
            });
        }

        let service_instance = self.create_service_instance(service.name.clone(), service_type);
        let env_vars = service_instance
            .get_environment_variables(&parameters)
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!("Failed to get environment variables: {}", e),
            })?;

        // Check if the variable exists
        match env_vars.get(var_name) {
            Some(value) => {
                // All variables are now decrypted when retrieved, so we can return them
                // Check if the variable is marked as sensitive in definitions
                let param_defs = service_instance.get_parameter_definitions();
                let is_sensitive = param_defs
                    .iter()
                    .find(|p| p.name == var_name)
                    .map(|p| p.encrypted)
                    .unwrap_or(false);

                if is_sensitive {
                    // For sensitive variables, return masked value in responses
                    return Err(ExternalServiceError::EncryptedVariableAccessDenied {
                        service_id: service_id_val,
                        var_name: var_name.to_string(),
                    });
                }

                Ok(EnvironmentVariableInfo {
                    name: var_name.to_string(),
                    value: value.clone(),
                })
            }
            None => Err(ExternalServiceError::EnvironmentVariableNotFound {
                service_id: service_id_val,
                var_name: var_name.to_string(),
            }),
        }
    }

    pub async fn get_project_service_environment_variables(
        &self,
        project_id_val: i32,
    ) -> Result<HashMap<i32, HashMap<String, String>>, ExternalServiceError> {
        // Verify project exists
        let _project = projects::Entity::find_by_id(project_id_val)
            .one(self.db.as_ref())
            .await?
            .ok_or(ExternalServiceError::ProjectNotFound { id: project_id_val })?;

        // Get all services linked to this project
        let linked_services = project_services::Entity::find()
            .filter(project_services::Column::ProjectId.eq(project_id_val))
            .all(self.db.as_ref())
            .await?;

        let mut result = HashMap::new();

        // For each linked service, get its environment variables
        for linked_service in linked_services {
            match self
                .get_service_environment_variables(linked_service.service_id, project_id_val)
                .await
            {
                Ok(env_vars) => {
                    result.insert(linked_service.service_id, env_vars);
                }
                Err(e) => {
                    error!(
                        "Failed to get environment variables for service {}: {}",
                        linked_service.service_id, e
                    );
                    // Skip this service and continue with others
                    continue;
                }
            }
        }

        Ok(result)
    }

    pub async fn get_service_type_parameters(
        &self,
        service_type: ServiceType,
    ) -> Result<Vec<ServiceParameter>, ExternalServiceError> {
        let service_instance = self.create_service_instance("temp".to_string(), service_type);
        Ok(service_instance.get_parameter_definitions())
    }

    pub async fn get_service_details_by_slug(
        &self,
        service: external_services::Model,
    ) -> Result<ExternalServiceDetails, ExternalServiceError> {
        // Get service info
        let service_info = self.get_service_info(service.id).await?;
        let parameters = self.get_service_parameters(service.id).await?;
        let service_type = ServiceType::from_str(&service_info.service_type.to_string())?;

        let service_instance =
            self.create_service_instance(service_info.name.clone(), service_type);

        Ok(ExternalServiceDetails {
            service: service_info,
            parameters: service_instance.get_parameter_definitions(),
            current_parameters: Some(parameters),
        })
    }

    /// Get environment variable names (safe preview - no sensitive values)
    pub async fn get_service_preview_environment_variable_names(
        &self,
        service_id_val: i32,
    ) -> Result<Vec<String>, ExternalServiceError> {
        let service = self.get_service(service_id_val).await?;
        let service_type = ServiceType::from_str(&service.service_type).map_err(|_| {
            ExternalServiceError::InvalidServiceType {
                id: service_id_val,
                service_type: service.service_type.clone(),
            }
        })?;
        let parameters = self.get_service_parameters(service_id_val).await?;

        let service_instance = self.create_service_instance(service.name.clone(), service_type);

        let env_vars = service_instance
            .get_environment_variables(&parameters)
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!("Failed to get environment variables: {}", e),
            })?;

        Ok(env_vars.keys().cloned().collect())
    }

    /// Get environment variables with masked sensitive values
    pub async fn get_service_preview_environment_variables_masked(
        &self,
        service_id_val: i32,
    ) -> Result<HashMap<String, String>, ExternalServiceError> {
        let service = self.get_service(service_id_val).await?;
        let service_type = ServiceType::from_str(&service.service_type).map_err(|_| {
            ExternalServiceError::InvalidServiceType {
                id: service_id_val,
                service_type: service.service_type.clone(),
            }
        })?;
        let parameters = self.get_service_parameters(service_id_val).await?;

        let service_instance = self.create_service_instance(service.name.clone(), service_type);

        let env_vars = service_instance
            .get_environment_variables(&parameters)
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!("Failed to get environment variables: {}", e),
            })?;

        // Mask sensitive values based on variable names
        let masked_vars = env_vars
            .into_iter()
            .map(|(key, value)| {
                let masked_value = if Self::is_sensitive_variable(&key) {
                    "***".to_string()
                } else {
                    value
                };
                (key, masked_value)
            })
            .collect();

        Ok(masked_vars)
    }

    /// Determine if a variable name indicates sensitive data
    fn is_sensitive_variable(var_name: &str) -> bool {
        let sensitive_patterns = [
            "password",
            "pass",
            "secret",
            "key",
            "token",
            "credential",
            "auth",
            "api_key",
            "private",
            "cert",
            "ssl",
            "tls",
        ];

        let var_lower = var_name.to_lowercase();
        sensitive_patterns
            .iter()
            .any(|pattern| var_lower.contains(pattern))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bollard::Docker;
    use std::collections::HashMap;
    use std::net::TcpListener;
    use temps_core::EncryptionService;
    use temps_database::test_utils::TestDatabase;

    fn get_unused_port() -> u16 {
        TcpListener::bind("127.0.0.1:0")
            .expect("Failed to bind to address")
            .local_addr()
            .unwrap()
            .port()
    }
    async fn setup_test_manager() -> (ExternalServiceManager, TestDatabase) {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.db.clone();

        let encryption_key = "test_encryption_key_1234567890ab";
        let encryption_service = Arc::new(EncryptionService::new(encryption_key).unwrap());
        let docker = Arc::new(Docker::connect_with_local_defaults().ok().unwrap());

        let manager = ExternalServiceManager::new(db, encryption_service, docker.clone());
        (manager, test_db)
    }

    #[tokio::test]
    async fn test_create_postgres_service() {
        let (manager, _test_db) = setup_test_manager().await;
        let random_unused_port = get_unused_port();
        let mut params = HashMap::new();
        params.insert("database".to_string(), "testdb".to_string());
        params.insert("username".to_string(), "testuser".to_string());
        params.insert("password".to_string(), "testpass".to_string());
        params.insert("port".to_string(), random_unused_port.to_string());
        params.insert("host".to_string(), "localhost".to_string());

        let request = CreateExternalServiceRequest {
            name: "test-postgres".to_string(),
            service_type: ServiceType::Postgres,
            version: Some("16".to_string()),
            parameters: params,
        };

        let result = manager.create_service(request).await;
        assert!(
            result.is_ok(),
            "Failed to create service: {:?}",
            result.err()
        );

        let service = result.unwrap();
        assert_eq!(service.name, "test-postgres");
        assert_eq!(service.service_type, ServiceType::Postgres);
        assert_eq!(service.version, Some("16".to_string()));
        assert_eq!(service.status, "running");

        // Cleanup
        let _ = manager.delete_service(service.id).await;
    }

    #[tokio::test]
    async fn test_create_redis_service() {
        let (manager, _test_db) = setup_test_manager().await;
        let random_unused_port = get_unused_port();
        let mut params = HashMap::new();
        params.insert("port".to_string(), random_unused_port.to_string());
        let request = CreateExternalServiceRequest {
            name: "test-redis".to_string(),
            service_type: ServiceType::Redis,
            version: Some("7".to_string()),
            parameters: params,
        };

        let result = manager.create_service(request).await;

        let service = result.expect("Failed to create Redis service");
        assert_eq!(service.name, "test-redis");
        assert_eq!(service.service_type, ServiceType::Redis);
        assert_eq!(service.status, "running");

        // Cleanup
        let _ = manager.delete_service(service.id).await;
    }

    #[tokio::test]
    async fn test_create_s3_service() {
        let (manager, _test_db) = setup_test_manager().await;

        let random_unused_port = get_unused_port();
        let mut params = HashMap::new();
        params.insert("port".to_string(), random_unused_port.to_string());
        // Note: bucket_name is not a parameter - buckets are created dynamically during provisioning
        // access_key and secret_key have defaults, so they're optional

        let request = CreateExternalServiceRequest {
            name: "test-s3".to_string(),
            service_type: ServiceType::S3,
            version: None,
            parameters: params,
        };

        let result = manager.create_service(request).await;

        let service = result.expect("Failed to create S3 service");
        assert_eq!(service.name, "test-s3");
        assert_eq!(service.service_type, ServiceType::S3);
        assert_eq!(service.status, "running");

        // Cleanup
        let _ = manager.delete_service(service.id).await;
    }

    #[tokio::test]
    #[ignore] // TODO: Implement service stop/start functionality
    async fn test_stop_and_start_service() {
        let (manager, _test_db) = setup_test_manager().await;
        let random_unused_port = get_unused_port();
        // Create a service first
        let mut params = HashMap::new();
        params.insert("port".to_string(), random_unused_port.to_string());
        params.insert("host".to_string(), "localhost".to_string());

        let request = CreateExternalServiceRequest {
            name: "test-stop-start".to_string(),
            service_type: ServiceType::Postgres,
            version: None,
            parameters: params,
        };

        let service = manager.create_service(request).await.unwrap();
        let service_id = service.id;

        // Stop the service
        let stopped_service = manager.stop_service(service_id).await;
        assert!(stopped_service.is_ok());
        assert_eq!(stopped_service.unwrap().status, "stopped");

        // Start the service
        let started_service = manager.start_service(service_id).await;
        assert!(started_service.is_ok());
        assert_eq!(started_service.unwrap().status, "running");

        // Cleanup
        let _ = manager.delete_service(service_id).await;
    }

    #[tokio::test]
    #[ignore] // TODO: Implement service deletion functionality
    async fn test_delete_service() {
        let (manager, _test_db) = setup_test_manager().await;

        // Create a service first
        let mut params = HashMap::new();
        params.insert("password".to_string(), "redis_pass".to_string());

        let request = CreateExternalServiceRequest {
            name: "test-delete".to_string(),
            service_type: ServiceType::Redis,
            version: None,
            parameters: params,
        };

        let service = manager.create_service(request).await.unwrap();
        let service_id = service.id;

        // Delete the service
        let delete_result = manager.delete_service(service_id).await;
        assert!(delete_result.is_ok());

        // Verify service is deleted
        let get_result = manager.get_service_details(service_id).await;
        assert!(get_result.is_err());
        assert!(matches!(
            get_result.unwrap_err(),
            ExternalServiceError::ServiceNotFound { .. }
        ));
    }

    #[tokio::test]
    #[ignore] // TODO: Implement service parameter update functionality
    async fn test_update_service_parameters() {
        let (manager, _test_db) = setup_test_manager().await;

        // Create a service first
        let mut params = HashMap::new();
        params.insert("database".to_string(), "original_db".to_string());
        params.insert("username".to_string(), "original_user".to_string());
        params.insert("password".to_string(), "original_pass".to_string());

        let request = CreateExternalServiceRequest {
            name: "test-update".to_string(),
            service_type: ServiceType::Postgres,
            version: None,
            parameters: params,
        };

        let service = manager.create_service(request).await.unwrap();
        let service_id = service.id;

        // Update service parameters
        let mut new_params = HashMap::new();
        new_params.insert("database".to_string(), "updated_db".to_string());
        new_params.insert("username".to_string(), "updated_user".to_string());
        new_params.insert("password".to_string(), "updated_pass".to_string());

        let update_request = UpdateExternalServiceRequest {
            name: Some("test-update-renamed".to_string()),
            parameters: new_params,
        };

        let updated_service = manager.update_service(service_id, update_request).await;
        assert!(updated_service.is_ok());
        assert_eq!(updated_service.unwrap().name, "test-update-renamed");

        // Cleanup
        let _ = manager.delete_service(service_id).await;
    }

    #[tokio::test]
    #[ignore] // TODO: Implement get service by name functionality
    async fn test_get_service_by_name() {
        let (manager, _test_db) = setup_test_manager().await;

        // Create a service
        let mut params = HashMap::new();
        params.insert("password".to_string(), "test".to_string());

        let request = CreateExternalServiceRequest {
            name: "unique-service-name".to_string(),
            service_type: ServiceType::Redis,
            version: None,
            parameters: params,
        };

        let service = manager.create_service(request).await.unwrap();
        let service_id = service.id;

        // Get service by name
        let found_service = manager.get_service_by_name("unique-service-name").await;
        assert!(found_service.is_ok());
        assert_eq!(found_service.unwrap().id, service.id);

        // Cleanup
        let _ = manager.delete_service(service_id).await;
    }

    #[tokio::test]
    #[ignore] // TODO: Implement get service by slug functionality
    async fn test_get_service_by_slug() {
        let (manager, _test_db) = setup_test_manager().await;

        // Create a service with a name that will be slugified
        let mut params = HashMap::new();
        params.insert("password".to_string(), "test".to_string());

        let request = CreateExternalServiceRequest {
            name: "Service With Spaces".to_string(),
            service_type: ServiceType::Redis,
            version: None,
            parameters: params,
        };

        let service = manager.create_service(request).await.unwrap();
        let service_id = service.id;

        // Get service by slug
        let found_service = manager.get_service_by_slug("Service With Spaces").await;
        assert!(found_service.is_ok());
        assert_eq!(found_service.unwrap().id, service.id);

        // Cleanup
        let _ = manager.delete_service(service_id).await;
    }

    #[tokio::test]
    async fn test_list_services() {
        let (manager, _test_db) = setup_test_manager().await;

        // Create multiple services
        let mut services_created = vec![];

        for i in 0..3 {
            let random_unused_port = get_unused_port();
            let mut params = HashMap::new();
            params.insert("port".to_string(), random_unused_port.to_string());

            let request = CreateExternalServiceRequest {
                name: format!("service-{}", i),
                service_type: ServiceType::Redis,
                version: None,
                parameters: params,
            };

            let service = manager.create_service(request).await.unwrap();
            services_created.push(service);
        }

        // List all services
        let all_services = manager.list_services().await;
        assert!(all_services.is_ok());

        let services_list = all_services.unwrap();
        assert!(services_list.len() >= 3);

        // Verify our created services are in the list
        for created in &services_created {
            assert!(services_list.iter().any(|s| s.id == created.id));
        }

        // Cleanup
        for service in services_created {
            let _ = manager.delete_service(service.id).await;
        }
    }

    #[tokio::test]
    #[ignore] // TODO: Implement get_service_environment_variables functionality
    async fn test_service_environment_variables() {
        let (manager, _test_db) = setup_test_manager().await;
        let random_unused_port = get_unused_port();
        // Create a postgres service
        let mut params = HashMap::new();
        params.insert("database".to_string(), "envtest".to_string());
        params.insert("username".to_string(), "envuser".to_string());
        params.insert("password".to_string(), "envpass".to_string());
        params.insert("port".to_string(), random_unused_port.to_string());
        params.insert("host".to_string(), "localhost".to_string());

        let request = CreateExternalServiceRequest {
            name: "env-test-service".to_string(),
            service_type: ServiceType::Postgres,
            version: Some("16".to_string()),
            parameters: params,
        };

        let service = manager.create_service(request).await.unwrap();
        let service_id = service.id;

        // Create a dummy project for testing
        let project_id = 1; // Assuming project with ID 1 exists or will be created

        // Get environment variables
        let env_vars_result = manager
            .get_service_environment_variables(service_id, project_id)
            .await;
        assert!(env_vars_result.is_ok());

        let env_vars = env_vars_result.unwrap();
        assert!(env_vars.contains_key("POSTGRES_DB"));
        assert!(env_vars.contains_key("POSTGRES_USER"));
        assert!(env_vars.contains_key("POSTGRES_PASSWORD"));
        assert_eq!(env_vars.get("POSTGRES_DB"), Some(&"envtest".to_string()));
        assert_eq!(env_vars.get("POSTGRES_USER"), Some(&"envuser".to_string()));

        // Cleanup
        let _ = manager.delete_service(service_id).await;
    }

    #[tokio::test]
    async fn test_service_parameter_encryption() {
        let (manager, _test_db) = setup_test_manager().await;
        let random_unused_port = get_unused_port();
        // Create a service with sensitive parameters
        let mut params = HashMap::new();
        params.insert("database".to_string(), "cryptodb".to_string());
        params.insert("username".to_string(), "cryptouser".to_string());
        params.insert("password".to_string(), "super_secret_password".to_string());
        params.insert("port".to_string(), random_unused_port.to_string());
        params.insert("host".to_string(), "localhost".to_string());

        let request = CreateExternalServiceRequest {
            name: "crypto-service".to_string(),
            service_type: ServiceType::Postgres,
            version: None,
            parameters: params,
        };

        let service = manager.create_service(request).await.unwrap();
        let service_id = service.id;

        // Get service details and verify parameters are properly handled
        let details = manager.get_service_details(service_id).await;
        assert!(details.is_ok());

        let service_details = details.unwrap();
        assert!(service_details.current_parameters.is_some());

        let current_params = service_details.current_parameters.unwrap();
        // Password should be decrypted for authorized access
        assert_eq!(
            current_params.get("password"),
            Some(&"super_secret_password".to_string())
        );

        // Cleanup
        let _ = manager.delete_service(service_id).await;
    }

    #[tokio::test]
    async fn test_invalid_service_type() {
        let (manager, _test_db) = setup_test_manager().await;

        // Try to get a service with invalid ID
        let result = manager.get_service_details(99999).await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ExternalServiceError::ServiceNotFound { .. }
        ));
    }

    #[tokio::test]
    async fn test_validate_parameters_fails_with_missing_required() {
        let (manager, _test_db) = setup_test_manager().await;

        // Create a postgres service without required parameters
        let params = HashMap::new(); // Empty parameters

        let request = CreateExternalServiceRequest {
            name: "invalid-service".to_string(),
            service_type: ServiceType::Postgres,
            version: None,
            parameters: params,
        };

        let result = manager.create_service(request).await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ExternalServiceError::ParameterValidationFailed { .. }
        ));
    }

    #[tokio::test]
    async fn test_slug_generation() {
        // Test the slug generation logic
        assert_eq!(
            ExternalServiceManager::generate_slug("My Service Name"),
            "my-service-name"
        );
        assert_eq!(
            ExternalServiceManager::generate_slug("Service@#$123"),
            "service123"
        );
        assert_eq!(
            ExternalServiceManager::generate_slug("   Spaces   Everywhere   "),
            "---spaces---everywhere---"
        );
    }

    #[tokio::test]
    async fn test_is_sensitive_variable() {
        assert!(ExternalServiceManager::is_sensitive_variable("password"));
        assert!(ExternalServiceManager::is_sensitive_variable("SECRET_KEY"));
        assert!(ExternalServiceManager::is_sensitive_variable("api_token"));
        assert!(ExternalServiceManager::is_sensitive_variable(
            "PRIVATE_CERT"
        ));
        assert!(ExternalServiceManager::is_sensitive_variable(
            "auth_credential"
        ));

        assert!(!ExternalServiceManager::is_sensitive_variable("database"));
        assert!(!ExternalServiceManager::is_sensitive_variable("username"));
        assert!(!ExternalServiceManager::is_sensitive_variable("port"));
        assert!(!ExternalServiceManager::is_sensitive_variable("host"));
    }

    #[tokio::test]
    #[ignore] // TODO: Implement masked environment variables functionality
    async fn test_masked_environment_variables() {
        let (manager, _test_db) = setup_test_manager().await;
        // Find a random unused port on the system

        let random_unused_port = get_unused_port();

        // Create a service with sensitive parameters
        let mut params = HashMap::new();
        params.insert("database".to_string(), "testdb".to_string());
        params.insert("username".to_string(), "user".to_string());
        params.insert("password".to_string(), "secret123".to_string());
        params.insert("port".to_string(), random_unused_port.to_string());

        let request = CreateExternalServiceRequest {
            name: "masked-test".to_string(),
            service_type: ServiceType::Postgres,
            version: None,
            parameters: params,
        };

        let service = manager.create_service(request).await.unwrap();
        let service_id = service.id;

        // Get masked environment variables
        let masked_vars = manager
            .get_service_preview_environment_variables_masked(service_id)
            .await;

        assert!(masked_vars.is_ok());
        let vars = masked_vars.unwrap();

        // Password should be masked
        assert_eq!(vars.get("POSTGRES_PASSWORD"), Some(&"***".to_string()));
        // Non-sensitive values should not be masked
        assert_eq!(vars.get("POSTGRES_DB"), Some(&"testdb".to_string()));
        assert_eq!(vars.get("POSTGRES_USER"), Some(&"user".to_string()));

        // Cleanup
        let _ = manager.delete_service(service_id).await;
    }
}
