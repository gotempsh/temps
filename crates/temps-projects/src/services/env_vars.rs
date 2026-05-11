use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set, TransactionTrait,
};
use std::sync::Arc;
use temps_core::EncryptionService;
use temps_entities::{env_var_environments, env_vars, environments};
use thiserror::Error;

use super::types::{EnvVarEnvironment, EnvVarWithEnvironments};

#[derive(Error, Debug)]
pub enum EnvVarError {
    #[error("Database connection error: {0}")]
    DatabaseConnectionError(String),

    #[error("Environment variable not found")]
    NotFound(String),

    #[error("Database error: {reason}")]
    DatabaseError { reason: String },

    #[error("Invalid input: {0}")]
    InvalidInput(String),

    #[error("Failed to encrypt environment variable '{key}': {reason}")]
    EncryptionFailed { key: String, reason: String },

    #[error("Failed to decrypt environment variable '{key}' (id={var_id}): {reason}")]
    DecryptionFailed {
        var_id: i32,
        key: String,
        reason: String,
    },

    #[error("Other error: {0}")]
    Other(String),
}

impl From<sea_orm::DbErr> for EnvVarError {
    fn from(error: sea_orm::DbErr) -> Self {
        match error {
            sea_orm::DbErr::RecordNotFound(_) => EnvVarError::NotFound(error.to_string()),
            _ => EnvVarError::DatabaseError {
                reason: error.to_string(),
            },
        }
    }
}

impl From<sea_orm::TransactionError<EnvVarError>> for EnvVarError {
    fn from(error: sea_orm::TransactionError<EnvVarError>) -> Self {
        match error {
            sea_orm::TransactionError::Transaction(e) => e,
            sea_orm::TransactionError::Connection(e) => {
                EnvVarError::DatabaseConnectionError(e.to_string())
            }
        }
    }
}

#[derive(Clone)]
pub struct EnvVarService {
    db: Arc<temps_database::DbConnection>,
    encryption_service: Arc<EncryptionService>,
}

impl EnvVarService {
    pub fn new(
        db: Arc<temps_database::DbConnection>,
        encryption_service: Arc<EncryptionService>,
    ) -> Self {
        EnvVarService {
            db,
            encryption_service,
        }
    }

    fn encrypt_value(&self, key: &str, value: &str) -> Result<String, EnvVarError> {
        self.encryption_service
            .encrypt_string(value)
            .map_err(|e| EnvVarError::EncryptionFailed {
                key: key.to_string(),
                reason: e.to_string(),
            })
    }

    fn decrypt_value(
        &self,
        var_id: i32,
        key: &str,
        value: &str,
        is_encrypted: bool,
    ) -> Result<String, EnvVarError> {
        if !is_encrypted {
            return Ok(value.to_string());
        }
        self.encryption_service
            .decrypt_string(value)
            .map_err(|e| EnvVarError::DecryptionFailed {
                var_id,
                key: key.to_string(),
                reason: e.to_string(),
            })
    }

    pub async fn get_environment_variables(
        &self,
        project_id: i32,
    ) -> Result<Vec<EnvVarWithEnvironments>, EnvVarError> {
        let vars = env_vars::Entity::find()
            .filter(env_vars::Column::ProjectId.eq(project_id))
            .order_by_asc(env_vars::Column::Key)
            .all(self.db.as_ref())
            .await?;

        let var_ids: Vec<i32> = vars.iter().map(|v| v.id).collect();

        let env_relationships: Vec<(env_var_environments::Model, Option<environments::Model>)> =
            env_var_environments::Entity::find()
                .filter(env_var_environments::Column::EnvVarId.is_in(var_ids))
                .find_also_related(environments::Entity)
                .all(self.db.as_ref())
                .await?;

        let mut env_map: std::collections::HashMap<i32, Vec<EnvVarEnvironment>> =
            std::collections::HashMap::new();

        for (env_var_env, env_option) in env_relationships {
            if let Some(env) = env_option {
                env_map
                    .entry(env_var_env.env_var_id)
                    .or_default()
                    .push(EnvVarEnvironment {
                        id: env.id,
                        name: env.name,
                    });
            }
        }

        let mut result = Vec::new();
        for var in vars {
            let environments = env_map.get(&var.id).cloned().unwrap_or_default();
            let decrypted_value =
                self.decrypt_value(var.id, &var.key, &var.value, var.is_encrypted)?;
            result.push(EnvVarWithEnvironments {
                id: var.id,
                project_id: var.project_id,
                key: var.key,
                value: decrypted_value,
                created_at: var.created_at,
                updated_at: var.updated_at,
                environments,
            });
        }

        Ok(result)
    }

    pub async fn create_environment_variable(
        &self,
        project_id: i32,
        environment_ids: Vec<i32>,
        key: String,
        value: String,
    ) -> Result<EnvVarWithEnvironments, EnvVarError> {
        let existing_env_vars = env_vars::Entity::find()
            .filter(env_vars::Column::ProjectId.eq(project_id))
            .filter(env_vars::Column::Key.eq(&key))
            .find_with_related(env_var_environments::Entity)
            .all(self.db.as_ref())
            .await?;

        let existing_env_ids: Vec<i32> = existing_env_vars
            .into_iter()
            .flat_map(|(_, env_var_envs)| {
                env_var_envs
                    .into_iter()
                    .map(|env_var_env| env_var_env.environment_id)
            })
            .collect();

        for env_id in &environment_ids {
            if existing_env_ids.contains(env_id) {
                return Err(EnvVarError::Other(format!(
                    "Environment variable '{}' already exists in one of the selected environments",
                    key
                )));
            }
        }

        let encrypted_value = self.encrypt_value(&key, &value)?;
        let encryption_service = self.encryption_service.clone();

        let result =
            self.db
                .transaction::<_, EnvVarWithEnvironments, EnvVarError>(|txn| {
                    let encrypted_value = encrypted_value.clone();
                    let key = key.clone();
                    let environment_ids = environment_ids.clone();

                    Box::pin(async move {
                        let new_var = env_vars::ActiveModel {
                            project_id: Set(project_id),
                            key: Set(key.clone()),
                            value: Set(encrypted_value),
                            is_encrypted: Set(true),
                            is_secret: Set(false),
                            created_at: Set(chrono::Utc::now()),
                            updated_at: Set(chrono::Utc::now()),
                            environment_id: Set(None),
                            ..Default::default()
                        };

                        let var = new_var.insert(txn).await?;

                        let mut environments = Vec::new();
                        for env_id in &environment_ids {
                            let new_env_rel = env_var_environments::ActiveModel {
                                env_var_id: Set(var.id),
                                environment_id: Set(*env_id),
                                created_at: Set(chrono::Utc::now()),
                                ..Default::default()
                            };

                            new_env_rel.insert(txn).await?;

                            let env = environments::Entity::find_by_id(*env_id)
                                .one(txn)
                                .await?
                                .ok_or(EnvVarError::Other("Environment not found".to_string()))?;

                            environments.push(EnvVarEnvironment {
                                id: env.id,
                                name: env.name,
                            });
                        }

                        let decrypted_value = encryption_service
                            .decrypt_string(&var.value)
                            .map_err(|e| EnvVarError::DecryptionFailed {
                                var_id: var.id,
                                key: var.key.clone(),
                                reason: e.to_string(),
                            })?;

                        Ok(EnvVarWithEnvironments {
                            id: var.id,
                            project_id: var.project_id,
                            key: var.key,
                            value: decrypted_value,
                            created_at: var.created_at,
                            updated_at: var.updated_at,
                            environments,
                        })
                    })
                })
                .await?;

        Ok(result)
    }

    pub async fn update_environment_variable(
        &self,
        project_id: i32,
        var_id: i32,
        key: String,
        value: String,
        environment_ids: Vec<i32>,
    ) -> Result<EnvVarWithEnvironments, EnvVarError> {
        let encrypted_value = self.encrypt_value(&key, &value)?;
        let encryption_service = self.encryption_service.clone();

        let result =
            self.db
                .transaction::<_, EnvVarWithEnvironments, EnvVarError>(|txn| {
                    let encrypted_value = encrypted_value.clone();
                    let key = key.clone();
                    let environment_ids = environment_ids.clone();

                    Box::pin(async move {
                        let env_var = env_vars::Entity::find_by_id(var_id)
                            .filter(env_vars::Column::ProjectId.eq(project_id))
                            .one(txn)
                            .await?
                            .ok_or(EnvVarError::Other(
                                "Environment variable not found".to_string(),
                            ))?;

                        let mut active_var: env_vars::ActiveModel = env_var.into();
                        active_var.key = Set(key.clone());
                        active_var.value = Set(encrypted_value);
                        active_var.is_encrypted = Set(true);
                        active_var.updated_at = Set(chrono::Utc::now());
                        let var = active_var.update(txn).await?;

                        env_var_environments::Entity::delete_many()
                            .filter(env_var_environments::Column::EnvVarId.eq(var_id))
                            .exec(txn)
                            .await?;

                        let mut environments = Vec::new();
                        for env_id in &environment_ids {
                            let new_env_rel = env_var_environments::ActiveModel {
                                env_var_id: Set(var.id),
                                environment_id: Set(*env_id),
                                created_at: Set(chrono::Utc::now()),
                                ..Default::default()
                            };

                            new_env_rel.insert(txn).await?;

                            let env = environments::Entity::find_by_id(*env_id)
                                .one(txn)
                                .await?
                                .ok_or(EnvVarError::Other("Environment not found".to_string()))?;

                            environments.push(EnvVarEnvironment {
                                id: env.id,
                                name: env.name,
                            });
                        }

                        let decrypted_value = encryption_service
                            .decrypt_string(&var.value)
                            .map_err(|e| EnvVarError::DecryptionFailed {
                                var_id: var.id,
                                key: var.key.clone(),
                                reason: e.to_string(),
                            })?;

                        Ok(EnvVarWithEnvironments {
                            id: var.id,
                            project_id: var.project_id,
                            key: var.key,
                            value: decrypted_value,
                            created_at: var.created_at,
                            updated_at: var.updated_at,
                            environments,
                        })
                    })
                })
                .await?;

        Ok(result)
    }

    pub async fn delete_environment_variable(
        &self,
        project_id: i32,
        var_id: i32,
    ) -> Result<(), EnvVarError> {
        self.db
            .transaction::<_, (), EnvVarError>(|txn| {
                Box::pin(async move {
                    env_var_environments::Entity::delete_many()
                        .filter(env_var_environments::Column::EnvVarId.eq(var_id))
                        .exec(txn)
                        .await?;

                    env_vars::Entity::delete_many()
                        .filter(env_vars::Column::Id.eq(var_id))
                        .filter(env_vars::Column::ProjectId.eq(project_id))
                        .exec(txn)
                        .await?;

                    Ok(())
                })
            })
            .await?;

        Ok(())
    }

    pub async fn get_environment_variable_value(
        &self,
        project_id: i32,
        key: &str,
        _environment_id: Option<i32>,
    ) -> Result<String, EnvVarError> {
        let var = env_vars::Entity::find()
            .filter(env_vars::Column::ProjectId.eq(project_id))
            .filter(env_vars::Column::Key.eq(key))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| EnvVarError::Other("Environment variable not found".to_string()))?;

        self.decrypt_value(var.id, &var.key, &var.value, var.is_encrypted)
    }
}
