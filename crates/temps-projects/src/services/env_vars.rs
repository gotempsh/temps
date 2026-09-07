// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set, TransactionTrait,
};
use std::sync::Arc;
use temps_core::EncryptionService;
use temps_entities::{env_var_environments, env_vars, environments};
use thiserror::Error;

use super::types::{EnvVarEnvironment, EnvVarWithEnvironments};

/// Placeholder returned in place of a secret's plaintext in list responses.
const SECRET_VALUE_MASK: &str = "***";

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

    #[error(
        "Secret env var '{key}' (id={var_id}) must be read through the audited reveal endpoint"
    )]
    SecretValueRequiresAuditedReveal { var_id: i32, key: String },

    #[error("Secret env var '{key}' requires a non-empty value")]
    SecretValueRequired { key: String },

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

        // Older rows can still carry their scope in env_vars.environment_id
        // instead of the junction table. Surface that scope to callers so
        // upgrade previews use the same precedence as deployment resolution.
        let direct_environment_ids = vars
            .iter()
            .filter_map(|variable| variable.environment_id)
            .collect::<Vec<_>>();
        let direct_environments = if direct_environment_ids.is_empty() {
            std::collections::HashMap::new()
        } else {
            environments::Entity::find()
                .filter(environments::Column::Id.is_in(direct_environment_ids))
                .all(self.db.as_ref())
                .await?
                .into_iter()
                .map(|environment| (environment.id, environment))
                .collect::<std::collections::HashMap<_, _>>()
        };
        for variable in &vars {
            let Some(environment_id) = variable.environment_id else {
                continue;
            };
            let Some(environment) = direct_environments.get(&environment_id) else {
                continue;
            };
            let scopes = env_map.entry(variable.id).or_default();
            if !scopes.iter().any(|scope| scope.id == environment.id) {
                scopes.push(EnvVarEnvironment {
                    id: environment.id,
                    name: environment.name.clone(),
                });
            }
        }

        let mut result = Vec::new();
        for var in vars {
            let environments = env_map.get(&var.id).cloned().unwrap_or_default();
            // Secrets never yield plaintext through a listing path.
            // Masking (rather than skipping the row) keeps the variable visible
            // so callers can still see that the key exists.
            let cleartext = self.decrypt_value(var.id, &var.key, &var.value, var.is_encrypted)?;
            let has_value = !cleartext.is_empty();
            let decrypted_value = if var.is_secret {
                SECRET_VALUE_MASK.to_string()
            } else {
                cleartext
            };
            result.push(EnvVarWithEnvironments {
                id: var.id,
                project_id: var.project_id,
                key: var.key,
                value: decrypted_value,
                has_value,
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
        is_secret: bool,
    ) -> Result<EnvVarWithEnvironments, EnvVarError> {
        // Empty secrets are almost always accidental and cannot authenticate
        // anything, so reject them at creation.
        if is_secret && value.is_empty() {
            return Err(EnvVarError::SecretValueRequired { key });
        }

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

        let result = self
            .db
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
                        is_secret: Set(is_secret),
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

                    let cleartext = encryption_service.decrypt_string(&var.value).map_err(|e| {
                        EnvVarError::DecryptionFailed {
                            var_id: var.id,
                            key: var.key.clone(),
                            reason: e.to_string(),
                        }
                    })?;
                    let has_value = !cleartext.is_empty();
                    let decrypted_value = if var.is_secret {
                        SECRET_VALUE_MASK.to_string()
                    } else {
                        cleartext
                    };

                    Ok(EnvVarWithEnvironments {
                        id: var.id,
                        project_id: var.project_id,
                        key: var.key,
                        value: decrypted_value,
                        has_value,
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
        let has_value = !value.is_empty();
        let encrypted_value = self.encrypt_value(&key, &value)?;
        let encryption_service = self.encryption_service.clone();

        let result = self
            .db
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
                    if env_var.is_secret && !has_value {
                        return Err(EnvVarError::SecretValueRequired { key: key.clone() });
                    }

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

                    let cleartext = encryption_service.decrypt_string(&var.value).map_err(|e| {
                        EnvVarError::DecryptionFailed {
                            var_id: var.id,
                            key: var.key.clone(),
                            reason: e.to_string(),
                        }
                    })?;
                    let has_value = !cleartext.is_empty();
                    let decrypted_value = if var.is_secret {
                        SECRET_VALUE_MASK.to_string()
                    } else {
                        cleartext
                    };

                    Ok(EnvVarWithEnvironments {
                        id: var.id,
                        project_id: var.project_id,
                        key: var.key,
                        value: decrypted_value,
                        has_value,
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

        // This legacy service has no authorization or audit context. Secret
        // plaintext is available only through temps-environments' dedicated
        // permission-checked, fail-closed audited reveal flow.
        if var.is_secret {
            return Err(EnvVarError::SecretValueRequiresAuditedReveal {
                var_id: var.id,
                key: var.key,
            });
        }

        self.decrypt_value(var.id, &var.key, &var.value, var.is_encrypted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};

    fn make_service(db: MockDatabase) -> EnvVarService {
        EnvVarService::new(
            Arc::new(db.into_connection()),
            Arc::new(
                EncryptionService::new(
                    "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                )
                .expect("test key is valid"),
            ),
        )
    }

    #[tokio::test]
    async fn create_rejects_a_secret_with_an_empty_value() {
        // Empty secrets are unusable credentials and should be refused up front.
        let service = make_service(MockDatabase::new(DatabaseBackend::Postgres));

        let error = service
            .create_environment_variable(1, vec![1], "API_KEY".to_string(), String::new(), true)
            .await
            .expect_err("an empty secret must be refused");

        assert!(matches!(
            error,
            EnvVarError::SecretValueRequired { ref key } if key == "API_KEY"
        ));
    }

    #[tokio::test]
    async fn create_allows_a_non_secret_with_an_empty_value() {
        // Empty is a legitimate value for a normal variable, and it stays
        // readable, so the secret guard must not reject it. The mock returns no
        // rows for the duplicate-key lookup, then fails the insert — reaching
        // the DB at all proves validation passed.
        let service = make_service(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![Vec::<env_vars::Model>::new()]),
        );

        let error = service
            .create_environment_variable(1, vec![1], "OPTIONAL".to_string(), String::new(), false)
            .await
            .expect_err("the mock has no insert result to return");

        assert!(
            !matches!(error, EnvVarError::SecretValueRequired { .. }),
            "a non-secret empty value must not trip the secret guard, got: {error}"
        );
    }

    #[tokio::test]
    async fn update_rejects_an_empty_value_for_an_existing_secret() {
        let now = chrono::Utc::now();
        let service = make_service(
            MockDatabase::new(DatabaseBackend::Postgres).append_query_results([vec![
                env_vars::Model {
                    id: 9,
                    project_id: 3,
                    environment_id: None,
                    key: "API_KEY".to_string(),
                    value: "encrypted".to_string(),
                    created_at: now,
                    updated_at: now,
                    include_in_preview: false,
                    is_encrypted: true,
                    is_secret: true,
                },
            ]]),
        );

        let error = service
            .update_environment_variable(3, 9, "API_KEY".to_string(), String::new(), vec![])
            .await
            .expect_err("an existing secret cannot be cleared to an empty value");

        assert!(matches!(
            error,
            EnvVarError::SecretValueRequired { ref key } if key == "API_KEY"
        ));
    }

    #[tokio::test]
    async fn legacy_read_rejects_secret_without_an_audit_context() {
        let encryption_service = Arc::new(
            EncryptionService::new(
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            )
            .expect("test key is valid"),
        );
        let encrypted = encryption_service
            .encrypt_string("generated-admin-password")
            .expect("test value encrypts");
        let now = chrono::Utc::now();
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![env_vars::Model {
                    id: 9,
                    project_id: 3,
                    environment_id: None,
                    key: "KC_BOOTSTRAP_ADMIN_PASSWORD".to_string(),
                    value: encrypted,
                    created_at: now,
                    updated_at: now,
                    include_in_preview: false,
                    is_encrypted: true,
                    is_secret: true,
                }]])
                .into_connection(),
        );
        let service = EnvVarService::new(db, encryption_service);

        let error = service
            .get_environment_variable_value(3, "KC_BOOTSTRAP_ADMIN_PASSWORD", None)
            .await
            .expect_err("legacy reads must not bypass the audited reveal endpoint");

        assert!(matches!(
            error,
            EnvVarError::SecretValueRequiresAuditedReveal {
                var_id: 9,
                ref key,
            } if key == "KC_BOOTSTRAP_ADMIN_PASSWORD"
        ));
    }
}
