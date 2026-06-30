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

    /// `is_secret` is one-way: a row already marked secret cannot be flipped
    /// back to a normal env var. Toggling it off would let a caller leak the
    /// value by reading the next `list` response.
    #[error("Cannot demote secret env var '{key}' (id={var_id}) back to non-secret")]
    CannotDemoteSecret { var_id: i32, key: String },

    /// Secret env vars require a value on create. On update the value is
    /// optional (omit to keep the existing ciphertext), but explicitly passing
    /// an empty string is a logic error in the caller.
    #[error("Secret env var '{key}' requires a non-empty value on create")]
    SecretValueRequired { key: String },

    #[error("Environment variable '{key}' already exists in one of the selected environments")]
    AlreadyExists { key: String },

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

    /// Encrypt a value before storing it in the database.
    fn encrypt_value(&self, key: &str, value: &str) -> Result<String, EnvVarError> {
        self.encryption_service
            .encrypt_string(value)
            .map_err(|e| EnvVarError::EncryptionFailed {
                key: key.to_string(),
                reason: e.to_string(),
            })
    }

    /// Decrypt a stored value. If `is_encrypted` is false, returns the value as-is
    /// (backward-compatibility for rows written before encryption was enabled).
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
        environment_id: Option<i32>,
    ) -> Result<Vec<EnvVarWithEnvironments>, EnvVarError> {
        let vars = env_vars::Entity::find()
            .filter(env_vars::Column::ProjectId.eq(project_id))
            .order_by_desc(env_vars::Column::UpdatedAt)
            .all(self.db.as_ref())
            .await?;

        let var_ids: Vec<i32> = vars.iter().map(|v| v.id).collect();

        let mut env_relationships_query = env_var_environments::Entity::find()
            .filter(env_var_environments::Column::EnvVarId.is_in(var_ids));

        if let Some(env_id) = environment_id {
            env_relationships_query = env_relationships_query
                .filter(env_var_environments::Column::EnvironmentId.eq(env_id));
        }

        let env_relationships: Vec<(env_var_environments::Model, Option<environments::Model>)> =
            env_relationships_query
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
                        main_url: env.subdomain,
                        current_deployment_id: env.current_deployment_id,
                    });
            }
        }

        let mut result = Vec::new();
        for var in vars {
            let environments = env_map.get(&var.id).cloned().unwrap_or_default();

            if environment_id.is_some() && environments.is_empty() {
                continue;
            }

            // Secret values are write-only — never returned in plaintext from
            // the API surface. The deployer path goes through
            // `get_for_deploy` instead.
            let value = if var.is_secret {
                None
            } else {
                Some(self.decrypt_value(var.id, &var.key, &var.value, var.is_encrypted)?)
            };

            result.push(EnvVarWithEnvironments {
                id: var.id,
                project_id: var.project_id,
                key: var.key,
                value,
                created_at: var.created_at,
                updated_at: var.updated_at,
                environments,
                include_in_preview: var.include_in_preview,
                is_secret: var.is_secret,
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
        include_in_preview: bool,
        is_secret: bool,
    ) -> Result<EnvVarWithEnvironments, EnvVarError> {
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
                return Err(EnvVarError::AlreadyExists { key: key.clone() });
            }
        }

        let encrypted_value = self.encrypt_value(&key, &value)?;

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
                        include_in_preview: Set(include_in_preview),
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
                            main_url: env.subdomain,
                            current_deployment_id: env.current_deployment_id,
                        });
                    }

                    // Secrets return no plaintext even on create — caller
                    // knows the value they just submitted; the API contract
                    // is that the value is never echoed back. Non-secrets
                    // return the plaintext for editor convenience.
                    let value = if var.is_secret {
                        None
                    } else {
                        Some(value.clone())
                    };

                    Ok(EnvVarWithEnvironments {
                        id: var.id,
                        project_id: var.project_id,
                        key: var.key,
                        value,
                        created_at: var.created_at,
                        updated_at: var.updated_at,
                        environments,
                        include_in_preview: var.include_in_preview,
                        is_secret: var.is_secret,
                    })
                })
            })
            .await?;

        Ok(result)
    }

    /// Updates an env var.
    ///
    /// - `value: None` keeps the existing ciphertext (useful for secret env
    ///   vars whose plaintext the client doesn't have).
    /// - `value: Some(plaintext)` re-encrypts and replaces.
    /// - `is_secret: Some(true)` promotes a regular env var to a secret.
    ///   `Some(false)` is rejected if the row is already a secret — the flag
    ///   is one-way. `None` leaves the flag unchanged.
    // 8 args after adding `is_secret`. Refactoring to an UpdateEnvVarRequest
    // struct would ripple through every caller (handlers + tests) for no
    // semantic gain; the args are the genuine inputs to the operation.
    #[allow(clippy::too_many_arguments)]
    pub async fn update_environment_variable(
        &self,
        project_id: i32,
        var_id: i32,
        key: String,
        value: Option<String>,
        environment_ids: Vec<i32>,
        include_in_preview: bool,
        is_secret: Option<bool>,
    ) -> Result<EnvVarWithEnvironments, EnvVarError> {
        let encrypted_value_opt = match &value {
            Some(v) => Some(self.encrypt_value(&key, v)?),
            None => None,
        };

        let result = self
            .db
            .transaction::<_, EnvVarWithEnvironments, EnvVarError>(|txn| {
                let encrypted_value_opt = encrypted_value_opt.clone();
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

                    // One-way secret flag: reject demotion.
                    let final_is_secret = match (env_var.is_secret, is_secret) {
                        (true, Some(false)) => {
                            return Err(EnvVarError::CannotDemoteSecret {
                                var_id: env_var.id,
                                key: env_var.key.clone(),
                            });
                        }
                        (current, Some(new)) => current || new,
                        (current, None) => current,
                    };

                    let mut active_var: env_vars::ActiveModel = env_var.into();
                    active_var.key = Set(key.clone());
                    if let Some(encrypted_value) = encrypted_value_opt {
                        active_var.value = Set(encrypted_value);
                        active_var.is_encrypted = Set(true);
                    }
                    active_var.is_secret = Set(final_is_secret);
                    active_var.include_in_preview = Set(include_in_preview);
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
                            main_url: env.subdomain,
                            current_deployment_id: env.current_deployment_id,
                        });
                    }

                    // Secret rows never return plaintext, even from update.
                    // Non-secret rows return the supplied plaintext or
                    // None when value wasn't changed (caller already has
                    // the current value via list).
                    let value = if var.is_secret { None } else { value };

                    Ok(EnvVarWithEnvironments {
                        id: var.id,
                        project_id: var.project_id,
                        key: var.key,
                        value,
                        created_at: var.created_at,
                        updated_at: var.updated_at,
                        environments,
                        include_in_preview: var.include_in_preview,
                        is_secret: var.is_secret,
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
            .ok_or_else(|| {
                EnvVarError::NotFound(format!(
                    "Environment variable '{}' not found in project {}",
                    key, project_id
                ))
            })?;

        self.decrypt_value(var.id, &var.key, &var.value, var.is_encrypted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};

    fn make_encryption_service() -> Arc<EncryptionService> {
        Arc::new(
            EncryptionService::new(
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            )
            .unwrap(),
        )
    }

    fn make_env_var_model(
        id: i32,
        project_id: i32,
        key: &str,
        value: &str,
        is_encrypted: bool,
    ) -> env_vars::Model {
        make_env_var_model_full(id, project_id, key, value, is_encrypted, false)
    }

    fn make_env_var_model_full(
        id: i32,
        project_id: i32,
        key: &str,
        value: &str,
        is_encrypted: bool,
        is_secret: bool,
    ) -> env_vars::Model {
        env_vars::Model {
            id,
            project_id,
            environment_id: None,
            key: key.to_string(),
            value: value.to_string(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            include_in_preview: false,
            is_encrypted,
            is_secret,
        }
    }

    #[test]
    fn test_encrypt_then_decrypt_roundtrip() {
        let svc = make_encryption_service();
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let service = EnvVarService::new(db, svc.clone());

        let plaintext = "super_secret_value";
        let encrypted = service.encrypt_value("MY_KEY", plaintext).unwrap();
        // Encrypted value must differ from plaintext
        assert_ne!(encrypted, plaintext);

        let decrypted = service
            .decrypt_value(1, "MY_KEY", &encrypted, true)
            .unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn test_decrypt_unencrypted_passthrough() {
        let svc = make_encryption_service();
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let service = EnvVarService::new(db, svc);

        // When is_encrypted=false the value is returned as-is (backward compat)
        let value = "plaintext_legacy_value";
        let result = service
            .decrypt_value(42, "LEGACY_KEY", value, false)
            .unwrap();
        assert_eq!(result, value);
    }

    #[test]
    fn test_decrypt_with_wrong_key_returns_error() {
        let svc1 = make_encryption_service();
        let svc2 = Arc::new(
            EncryptionService::new(
                "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
            )
            .unwrap(),
        );
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let service1 = EnvVarService::new(db.clone(), svc1);
        let service2 = EnvVarService::new(db, svc2);

        let encrypted = service1.encrypt_value("KEY", "secret").unwrap();
        let result = service2.decrypt_value(1, "KEY", &encrypted, true);

        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            EnvVarError::DecryptionFailed { .. }
        ));
    }

    #[test]
    fn test_decrypt_invalid_base64_returns_error() {
        let svc = make_encryption_service();
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let service = EnvVarService::new(db, svc);

        let result = service.decrypt_value(5, "KEY", "not-valid-base64!!!", true);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            EnvVarError::DecryptionFailed { var_id: 5, .. }
        ));
    }

    #[test]
    fn test_encryption_different_each_call() {
        let svc = make_encryption_service();
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let service = EnvVarService::new(db, svc);

        let e1 = service.encrypt_value("K", "value").unwrap();
        let e2 = service.encrypt_value("K", "value").unwrap();
        // Random nonce means each encryption produces a different ciphertext
        assert_ne!(e1, e2);

        // But both decrypt to the same value
        let d1 = service.decrypt_value(1, "K", &e1, true).unwrap();
        let d2 = service.decrypt_value(1, "K", &e2, true).unwrap();
        assert_eq!(d1, "value");
        assert_eq!(d2, "value");
    }

    #[tokio::test]
    async fn test_create_env_var_duplicate_returns_already_exists() {
        // Re-creating a key that already exists in one of the selected
        // environments must surface a typed AlreadyExists error (mapped to HTTP
        // 409), not the catch-all Other (which mapped to 500). Regression guard.
        let svc = make_encryption_service();
        let existing_var = make_env_var_model(1, 10, "DB_URL", "v", false);
        let existing_link = env_var_environments::Model {
            id: 1,
            env_var_id: 1,
            environment_id: 5,
            created_at: chrono::Utc::now(),
        };
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![(existing_var, Some(existing_link))]])
                .into_connection(),
        );
        let service = EnvVarService::new(db, svc);

        // Requesting environment 5, which already has the key -> duplicate.
        let result = service
            .create_environment_variable(
                10,
                vec![5],
                "DB_URL".to_string(),
                "newval".to_string(),
                false,
                false,
            )
            .await;

        match result {
            Err(EnvVarError::AlreadyExists { key }) => assert_eq!(key, "DB_URL"),
            Err(other) => panic!("expected EnvVarError::AlreadyExists, got {other:?}"),
            Ok(_) => panic!("expected EnvVarError::AlreadyExists, got Ok"),
        }
    }

    #[tokio::test]
    async fn test_get_environment_variables_decrypts_values() {
        let svc = make_encryption_service();
        let plaintext = "my_db_password";
        let encrypted = svc.encrypt_string(plaintext).unwrap();

        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![make_env_var_model(
                    1,
                    10,
                    "DB_PASSWORD",
                    &encrypted,
                    true,
                )]])
                .append_query_results(vec![Vec::<(
                    env_var_environments::Model,
                    Option<environments::Model>,
                )>::new()])
                .into_connection(),
        );

        let service = EnvVarService::new(db, svc);
        let result = service.get_environment_variables(10, None).await.unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].key, "DB_PASSWORD");
        assert_eq!(result[0].value.as_deref(), Some(plaintext));
        assert!(!result[0].is_secret);
    }

    #[tokio::test]
    async fn test_get_environment_variables_masks_secret_values() {
        let svc = make_encryption_service();
        let encrypted = svc.encrypt_string("never_returned").unwrap();

        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![make_env_var_model_full(
                    7,
                    10,
                    "DEEP_SECRET",
                    &encrypted,
                    true,
                    true, // is_secret
                )]])
                .append_query_results(vec![Vec::<(
                    env_var_environments::Model,
                    Option<environments::Model>,
                )>::new()])
                .into_connection(),
        );

        let service = EnvVarService::new(db, svc);
        let result = service.get_environment_variables(10, None).await.unwrap();

        assert_eq!(result.len(), 1);
        assert!(result[0].is_secret);
        assert!(
            result[0].value.is_none(),
            "secret value must not be returned"
        );
    }

    #[tokio::test]
    async fn test_get_environment_variables_handles_unencrypted_legacy() {
        let svc = make_encryption_service();

        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![make_env_var_model(
                    2,
                    10,
                    "LEGACY_VAR",
                    "plaintext_legacy",
                    false, // not encrypted — legacy row
                )]])
                .append_query_results(vec![Vec::<(
                    env_var_environments::Model,
                    Option<environments::Model>,
                )>::new()])
                .into_connection(),
        );

        let service = EnvVarService::new(db, svc);
        let result = service.get_environment_variables(10, None).await.unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].value.as_deref(), Some("plaintext_legacy"));
    }

    #[tokio::test]
    async fn test_get_environment_variable_value_decrypts() {
        let svc = make_encryption_service();
        let plaintext = "secret_api_key";
        let encrypted = svc.encrypt_string(plaintext).unwrap();

        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![make_env_var_model(
                    3, 10, "API_KEY", &encrypted, true,
                )]])
                .into_connection(),
        );

        let service = EnvVarService::new(db, svc);
        let value = service
            .get_environment_variable_value(10, "API_KEY", None)
            .await
            .unwrap();

        assert_eq!(value, plaintext);
    }
}
