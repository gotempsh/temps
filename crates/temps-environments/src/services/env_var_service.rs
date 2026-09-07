// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseTransaction, EntityTrait, QueryFilter, QueryOrder,
    QuerySelect, Set, TransactionTrait,
};
use std::sync::Arc;
use temps_core::EncryptionService;
use temps_entities::{env_var_environments, env_vars, environments};
use thiserror::Error;

use super::types::{EnvVarEnvironment, EnvVarWithEnvironments, UpdateEnvVarOutcome};

#[derive(Error, Debug)]
pub enum EnvVarError {
    #[error("Database connection error: {0}")]
    DatabaseConnectionError(String),

    #[error("Environment variable not found")]
    NotFound(String),

    #[error("Environment {environment_id} was not found in project {project_id}")]
    EnvironmentNotFound {
        environment_id: i32,
        project_id: i32,
    },

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
    /// value by reading the next `list` response. The only way back is to
    /// delete the variable and create it again as a regular one, which forces
    /// the operator to supply the value rather than recover it from storage.
    #[error(
        "Environment variable '{key}' (id={var_id}) is a secret and cannot be converted back to a regular variable. Delete it and create it again as a non-secret variable, supplying the value yourself."
    )]
    CannotDemoteSecret { var_id: i32, key: String },

    /// Secret env vars require a non-empty value. On update the value is
    /// optional — omitting it keeps the existing ciphertext — but explicitly
    /// passing an empty string is a logic error in the caller, and a
    /// destructive one: the write cannot be read back or undone.
    #[error(
        "Secret env var '{key}' requires a non-empty value. Omit the value entirely to keep the one already stored."
    )]
    SecretValueRequired { key: String },

    #[error(
        "Environment variable '{key}' is ambiguous in project {project_id}; specify an environment"
    )]
    AmbiguousValue { project_id: i32, key: String },

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

    /// Resolve every requested environment inside the authorized project.
    ///
    /// Environment IDs are globally allocated, so accepting an ID without the
    /// project and soft-delete filters would allow one project to create links
    /// to another project's environment and expose its metadata.
    async fn environments_in_project(
        txn: &DatabaseTransaction,
        project_id: i32,
        environment_ids: &[i32],
    ) -> Result<Vec<environments::Model>, EnvVarError> {
        if environment_ids.is_empty() {
            return Ok(Vec::new());
        }

        let unique_ids = environment_ids
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        if unique_ids.len() != environment_ids.len() {
            return Err(EnvVarError::InvalidInput(
                "Environment IDs must not contain duplicates".to_string(),
            ));
        }

        let models = environments::Entity::find()
            .filter(environments::Column::Id.is_in(unique_ids.iter().copied()))
            .filter(environments::Column::ProjectId.eq(project_id))
            .filter(environments::Column::DeletedAt.is_null())
            .all(txn)
            .await?;
        let mut by_id = models
            .into_iter()
            .map(|environment| (environment.id, environment))
            .collect::<std::collections::HashMap<_, _>>();

        environment_ids
            .iter()
            .map(|environment_id| {
                by_id
                    .remove(environment_id)
                    .ok_or(EnvVarError::EnvironmentNotFound {
                        environment_id: *environment_id,
                        project_id,
                    })
            })
            .collect()
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

            // Secret values are never returned in plaintext from this bulk
            // API surface. Deployment and explicit reveal use scoped methods.
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
                    let scoped_environments =
                        Self::environments_in_project(txn, project_id, &environment_ids).await?;

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
                    for (env_id, env) in environment_ids.iter().zip(scoped_environments) {
                        let new_env_rel = env_var_environments::ActiveModel {
                            env_var_id: Set(var.id),
                            environment_id: Set(*env_id),
                            created_at: Set(chrono::Utc::now()),
                            ..Default::default()
                        };

                        new_env_rel.insert(txn).await?;

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
    ///
    /// Promotion also guarantees the value is encrypted at rest: a legacy row
    /// still stored as plaintext (`is_encrypted = false`) is re-encrypted as
    /// part of the same transaction, even when the caller supplies no new
    /// value. Without that, "secret" would only hide the value from the API
    /// while leaving it readable in the database.
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
    ) -> Result<UpdateEnvVarOutcome, EnvVarError> {
        let encrypted_value_opt = match &value {
            Some(v) => Some(self.encrypt_value(&key, v)?),
            None => None,
        };
        // An explicitly-supplied empty value is only ever a caller bug when the
        // row ends up secret: the write is unreadable afterwards and the flag
        // cannot be undone, so there is no way to notice the mistake or recover
        // the old value. Omitting `value` entirely is the supported way to keep
        // the existing ciphertext.
        let value_is_explicitly_empty = value.as_ref().is_some_and(|v| v.is_empty());
        let encryption_service = self.encryption_service.clone();

        let result = self
            .db
            .transaction::<_, UpdateEnvVarOutcome, EnvVarError>(|txn| {
                let encrypted_value_opt = encrypted_value_opt.clone();
                let key = key.clone();
                let environment_ids = environment_ids.clone();
                let encryption_service = encryption_service.clone();

                Box::pin(async move {
                    // SELECT ... FOR UPDATE. Every decision below is derived
                    // from this row — whether the flag may change, and
                    // whether an empty value is about to be sealed — so the
                    // read has to be serialized with concurrent updates.
                    // Without the lock, a promotion committing between this
                    // read and our own write lets a deliberate blank land on
                    // a row that has since become secret, which is the
                    // unrecoverable state both guards exist to prevent.
                    let env_var = env_vars::Entity::find_by_id(var_id)
                        .filter(env_vars::Column::ProjectId.eq(project_id))
                        .lock_exclusive()
                        .one(txn)
                        .await?
                        .ok_or(EnvVarError::Other(
                            "Environment variable not found".to_string(),
                        ))?;
                    let scoped_environments =
                        Self::environments_in_project(txn, project_id, &environment_ids).await?;

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

                    // Refuse to seal an empty value over a real one. Without
                    // this a client that failed to load the current value
                    // (a denied or transient reveal) silently overwrites the
                    // credential with "" and marks it secret. The empty
                    // credential is unusable and the classification cannot
                    // be demoted. Mirrors the create-time guard.
                    if final_is_secret && value_is_explicitly_empty {
                        return Err(EnvVarError::SecretValueRequired { key: key.clone() });
                    }

                    // A promotion is the transition non-secret -> secret. It is
                    // reported back to the handler so the write can be audited
                    // as the one-way, security-relevant change that it is.
                    let was_secret = env_var.is_secret;
                    let promoted_to_secret = !was_secret && final_is_secret;
                    let was_encrypted = env_var.is_encrypted;
                    let stored_value = env_var.value.clone();

                    let mut active_var: env_vars::ActiveModel = env_var.into();
                    active_var.key = Set(key.clone());
                    if let Some(encrypted_value) = encrypted_value_opt {
                        active_var.value = Set(encrypted_value);
                        active_var.is_encrypted = Set(true);
                    } else if promoted_to_secret && !was_encrypted {
                        // Legacy plaintext row being promoted without a new
                        // value: encrypt what is already there so the secret is
                        // unreadable at rest, not merely hidden from the API.
                        let ciphertext =
                            encryption_service
                                .encrypt_string(&stored_value)
                                .map_err(|e| EnvVarError::EncryptionFailed {
                                    key: key.clone(),
                                    reason: e.to_string(),
                                })?;
                        active_var.value = Set(ciphertext);
                        active_var.is_encrypted = Set(true);
                    }
                    // Only touch the column on an actual promotion. Writing
                    // it unconditionally re-asserts a value derived from an
                    // unlocked read, so two concurrent updates that both saw
                    // `is_secret = false` would let the second (an ordinary
                    // edit that never asked to change the flag) clear the
                    // promotion the first one just committed — silently
                    // unmasking the secret, since the ciphertext survives.
                    // Leaving the column out of the UPDATE makes an
                    // unrequested demote impossible regardless of ordering.
                    if promoted_to_secret {
                        active_var.is_secret = Set(true);
                    }
                    active_var.include_in_preview = Set(include_in_preview);
                    active_var.updated_at = Set(chrono::Utc::now());
                    let var = active_var.update(txn).await?;

                    env_var_environments::Entity::delete_many()
                        .filter(env_var_environments::Column::EnvVarId.eq(var_id))
                        .exec(txn)
                        .await?;

                    let mut environments = Vec::new();
                    for (env_id, env) in environment_ids.iter().zip(scoped_environments) {
                        let new_env_rel = env_var_environments::ActiveModel {
                            env_var_id: Set(var.id),
                            environment_id: Set(*env_id),
                            created_at: Set(chrono::Utc::now()),
                            ..Default::default()
                        };

                        new_env_rel.insert(txn).await?;

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

                    Ok(UpdateEnvVarOutcome {
                        var: EnvVarWithEnvironments {
                            id: var.id,
                            project_id: var.project_id,
                            key: var.key,
                            value,
                            created_at: var.created_at,
                            updated_at: var.updated_at,
                            environments,
                            include_in_preview: var.include_in_preview,
                            is_secret: var.is_secret,
                        },
                        promoted_to_secret,
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
                    let env_var = env_vars::Entity::find_by_id(var_id)
                        .filter(env_vars::Column::ProjectId.eq(project_id))
                        .lock_exclusive()
                        .one(txn)
                        .await?
                        .ok_or_else(|| {
                            EnvVarError::NotFound(format!(
                                "Environment variable {} not found in project {}",
                                var_id, project_id
                            ))
                        })?;

                    env_var_environments::Entity::delete_many()
                        .filter(env_var_environments::Column::EnvVarId.eq(var_id))
                        .exec(txn)
                        .await?;

                    let active_var: env_vars::ActiveModel = env_var.into();
                    active_var.delete(txn).await?;

                    Ok(())
                })
            })
            .await?;

        Ok(())
    }

    /// Decrypt one value for an HTTP reveal flow.
    ///
    /// This stays crate-private and is deliberately named after its security
    /// invariant: callers must authorize and durably audit the reveal before
    /// returning the plaintext outside the process.
    pub(crate) async fn get_environment_variable_value_for_audited_reveal(
        &self,
        project_id: i32,
        key: &str,
        environment_id: Option<i32>,
        var_id: Option<i32>,
    ) -> Result<String, EnvVarError> {
        let mut query = env_vars::Entity::find()
            .filter(env_vars::Column::ProjectId.eq(project_id))
            .filter(env_vars::Column::Key.eq(key));
        if let Some(var_id) = var_id {
            query = query.filter(env_vars::Column::Id.eq(var_id));
        }
        let mut vars = query.all(self.db.as_ref()).await?;

        if let Some(environment_id) = environment_id {
            let var_ids = vars.iter().map(|var| var.id).collect::<Vec<_>>();
            let links = env_var_environments::Entity::find()
                .filter(env_var_environments::Column::EnvVarId.is_in(var_ids))
                .filter(env_var_environments::Column::EnvironmentId.eq(environment_id))
                .all(self.db.as_ref())
                .await?;
            let linked_ids = links
                .into_iter()
                .map(|link| link.env_var_id)
                .collect::<std::collections::HashSet<_>>();
            vars.retain(|var| linked_ids.contains(&var.id));
        }

        if vars.len() > 1 {
            return Err(EnvVarError::AmbiguousValue {
                project_id,
                key: key.to_string(),
            });
        }

        let var = vars.into_iter().next().ok_or_else(|| {
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
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};

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

    async fn assert_create_rejects_unavailable_environment(environment_id: i32) {
        let svc = make_encryption_service();
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([Vec::<env_vars::Model>::new()])
                .append_query_results([Vec::<environments::Model>::new()])
                .into_connection(),
        );
        let service = EnvVarService::new(db, svc);

        let error = service
            .create_environment_variable(
                10,
                vec![environment_id],
                "SCOPED_KEY".to_string(),
                "value".to_string(),
                false,
                false,
            )
            .await
            .expect_err("foreign or deleted environment must be rejected before insert");

        assert!(matches!(
            error,
            EnvVarError::EnvironmentNotFound {
                environment_id: actual_environment_id,
                project_id: 10,
            } if actual_environment_id == environment_id
        ));
    }

    #[tokio::test]
    async fn test_create_rejects_cross_project_environment() {
        assert_create_rejects_unavailable_environment(20).await;
    }

    #[tokio::test]
    async fn test_create_rejects_soft_deleted_environment() {
        assert_create_rejects_unavailable_environment(21).await;
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
            .get_environment_variable_value_for_audited_reveal(10, "API_KEY", None, None)
            .await
            .unwrap();

        assert_eq!(value, plaintext);
    }

    #[tokio::test]
    async fn test_get_environment_variable_value_selects_matching_environment() {
        let encryption_service = make_encryption_service();
        let first = encryption_service.encrypt_string("first-value").unwrap();
        let second = encryption_service.encrypt_string("second-value").unwrap();
        let first_model = make_env_var_model(3, 10, "SHARED_KEY", &first, true);
        let second_model = make_env_var_model(4, 10, "SHARED_KEY", &second, true);
        let matching_link = env_var_environments::Model {
            id: 9,
            env_var_id: 4,
            environment_id: 22,
            created_at: chrono::Utc::now(),
        };
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![first_model, second_model]])
                .append_query_results([vec![matching_link]])
                .into_connection(),
        );
        let service = EnvVarService::new(db, encryption_service);

        let value = service
            .get_environment_variable_value_for_audited_reveal(10, "SHARED_KEY", Some(22), None)
            .await
            .expect("environment-scoped reveal should select the linked row");

        assert_eq!(value, "second-value");
    }

    #[tokio::test]
    async fn test_get_environment_variable_value_rejects_ambiguous_key_without_environment() {
        let encryption_service = make_encryption_service();
        let first = encryption_service.encrypt_string("first-value").unwrap();
        let second = encryption_service.encrypt_string("second-value").unwrap();
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![
                    make_env_var_model(3, 10, "SHARED_KEY", &first, true),
                    make_env_var_model(4, 10, "SHARED_KEY", &second, true),
                ]])
                .into_connection(),
        );
        let service = EnvVarService::new(db, encryption_service);

        let error = service
            .get_environment_variable_value_for_audited_reveal(10, "SHARED_KEY", None, None)
            .await
            .expect_err("unscoped duplicate-key reveal must fail closed");

        assert!(matches!(
            error,
            EnvVarError::AmbiguousValue {
                project_id: 10,
                ref key,
            } if key == "SHARED_KEY"
        ));
    }

    #[tokio::test]
    async fn test_get_environment_variable_value_uses_authoritative_row_id() {
        let encryption_service = make_encryption_service();
        let selected = encryption_service
            .encrypt_string("selected-row-value")
            .unwrap();
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![make_env_var_model(
                    4,
                    10,
                    "SHARED_KEY",
                    &selected,
                    true,
                )]])
                .into_connection(),
        );
        let service = EnvVarService::new(db, encryption_service);

        let value = service
            .get_environment_variable_value_for_audited_reveal(10, "SHARED_KEY", None, Some(4))
            .await
            .expect("row-scoped reveal should return the requested env-var row");

        assert_eq!(value, "selected-row-value");
    }

    #[tokio::test]
    async fn test_get_environment_variable_value_reveals_secret_through_scoped_endpoint() {
        let encryption_service = make_encryption_service();
        let encrypted = encryption_service
            .encrypt_string("reveal-on-demand")
            .unwrap();
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![make_env_var_model_full(
                    4,
                    10,
                    "WRITE_ONLY_TOKEN",
                    &encrypted,
                    true,
                    true,
                )]])
                .into_connection(),
        );
        let service = EnvVarService::new(db, encryption_service);

        let value = service
            .get_environment_variable_value_for_audited_reveal(10, "WRITE_ONLY_TOKEN", None, None)
            .await
            .expect("an authorized audited endpoint must be able to reveal a secret");

        assert_eq!(value, "reveal-on-demand");
    }

    /// Building a mock that walks the update transaction: SELECT the row,
    /// UPDATE ... RETURNING the new row, then DELETE the environment links.
    /// `environment_ids` is empty in these tests so no link inserts follow.
    fn mock_update_db(
        before: env_vars::Model,
        after: env_vars::Model,
    ) -> Arc<sea_orm::DatabaseConnection> {
        Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![before]])
                .append_query_results(vec![vec![after]])
                .append_exec_results(vec![MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 0,
                }])
                .into_connection(),
        )
    }

    async fn assert_update_rejects_unavailable_environment(environment_id: i32) {
        let encryption_service = make_encryption_service();
        let before = make_env_var_model(3, 10, "SCOPED_KEY", "encrypted", true);
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![before]])
                .append_query_results([Vec::<environments::Model>::new()])
                .into_connection(),
        );
        let service = EnvVarService::new(db, encryption_service);

        let error = service
            .update_environment_variable(
                10,
                3,
                "SCOPED_KEY".to_string(),
                None,
                vec![environment_id],
                false,
                None,
            )
            .await
            .expect_err("foreign or deleted environment must be rejected before update");

        assert!(matches!(
            error,
            EnvVarError::EnvironmentNotFound {
                environment_id: actual_environment_id,
                project_id: 10,
            } if actual_environment_id == environment_id
        ));
    }

    #[tokio::test]
    async fn test_update_rejects_cross_project_environment() {
        assert_update_rejects_unavailable_environment(20).await;
    }

    #[tokio::test]
    async fn test_update_rejects_soft_deleted_environment() {
        assert_update_rejects_unavailable_environment(21).await;
    }

    #[tokio::test]
    async fn test_delete_rejects_cross_project_variable_before_deleting_links() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([Vec::<env_vars::Model>::new()])
                .into_connection(),
        );
        let service = EnvVarService::new(db.clone(), make_encryption_service());

        let error = service
            .delete_environment_variable(10, 404)
            .await
            .expect_err("foreign variable must be rejected");
        assert!(matches!(error, EnvVarError::NotFound(_)));

        drop(service);
        let db = Arc::try_unwrap(db).expect("service dropped, so this is the only handle");
        let statements = format!("{:?}", db.into_transaction_log()).to_uppercase();
        assert!(statements.contains("SELECT"));
        assert!(
            !statements.contains("DELETE"),
            "foreign variable links must remain untouched: {statements}"
        );
    }

    #[tokio::test]
    async fn test_update_promoting_to_secret_reports_promotion_and_withholds_value() {
        // Converting an existing variable to a secret is the whole point of the
        // is_secret transition: the caller must learn it happened (so it can be
        // audited) and the response must stop carrying the plaintext.
        let encryption_service = make_encryption_service();
        let encrypted = encryption_service.encrypt_string("old_value").unwrap();
        let before = make_env_var_model_full(3, 10, "API_KEY", &encrypted, true, false);
        let after = make_env_var_model_full(3, 10, "API_KEY", &encrypted, true, true);

        let service = EnvVarService::new(mock_update_db(before, after), encryption_service);

        let outcome = service
            .update_environment_variable(
                10,
                3,
                "API_KEY".to_string(),
                None,
                vec![],
                false,
                Some(true),
            )
            .await
            .expect("promotion should succeed");

        assert!(outcome.promoted_to_secret);
        assert!(outcome.var.is_secret);
        assert_eq!(outcome.var.value, None);
    }

    #[tokio::test]
    async fn test_update_of_already_secret_var_is_not_reported_as_promotion() {
        // Editing a variable that is already secret (e.g. changing which
        // environments it applies to) must not emit a second promotion audit.
        let encryption_service = make_encryption_service();
        let encrypted = encryption_service.encrypt_string("still_secret").unwrap();
        let before = make_env_var_model_full(4, 10, "TOKEN", &encrypted, true, true);
        let after = make_env_var_model_full(4, 10, "TOKEN", &encrypted, true, true);

        let service = EnvVarService::new(mock_update_db(before, after), encryption_service);

        let outcome = service
            .update_environment_variable(10, 4, "TOKEN".to_string(), None, vec![], false, None)
            .await
            .expect("no-op update should succeed");

        assert!(!outcome.promoted_to_secret);
        assert!(outcome.var.is_secret);
        assert_eq!(outcome.var.value, None);
    }

    #[tokio::test]
    async fn test_update_cannot_demote_a_secret_back_to_plain_var() {
        // The flag is one-way. Allowing is_secret: false would let a caller
        // unmask the value simply by toggling it off and re-reading the list.
        let encryption_service = make_encryption_service();
        let encrypted = encryption_service.encrypt_string("stays_hidden").unwrap();
        let before = make_env_var_model_full(5, 10, "PRIVATE_KEY", &encrypted, true, true);

        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![before]])
                .into_connection(),
        );
        let service = EnvVarService::new(db, encryption_service);

        let error = service
            .update_environment_variable(
                10,
                5,
                "PRIVATE_KEY".to_string(),
                None,
                vec![],
                false,
                Some(false),
            )
            .await
            .expect_err("demotion must be rejected");

        assert!(matches!(
            error,
            EnvVarError::CannotDemoteSecret { var_id: 5, ref key } if key == "PRIVATE_KEY"
        ));
    }

    #[tokio::test]
    async fn test_update_rejects_empty_value_when_row_ends_up_secret() {
        // The destructive case: a client that could not load the current value
        // (denied or failed reveal) submits an empty string together with the
        // promotion. Sealing "" over a real credential is unrecoverable — it
        // can never be read back to notice, nor demoted to inspect — so the
        // write must be refused rather than reported as a success.
        let encryption_service = make_encryption_service();
        let encrypted = encryption_service
            .encrypt_string("real_credential")
            .unwrap();
        let before = make_env_var_model_full(6, 10, "API_KEY", &encrypted, true, false);

        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![before]])
                .into_connection(),
        );
        let service = EnvVarService::new(db, encryption_service);

        let error = service
            .update_environment_variable(
                10,
                6,
                "API_KEY".to_string(),
                Some(String::new()),
                vec![],
                false,
                Some(true),
            )
            .await
            .expect_err("promoting with an empty value must be refused");

        assert!(matches!(
            error,
            EnvVarError::SecretValueRequired { ref key } if key == "API_KEY"
        ));
    }

    #[tokio::test]
    async fn test_update_rejects_empty_value_for_an_existing_secret() {
        // Same hazard without a promotion: blanking an existing secret leaves
        // an unusable credential.
        let encryption_service = make_encryption_service();
        let encrypted = encryption_service.encrypt_string("still_needed").unwrap();
        let before = make_env_var_model_full(7, 10, "TOKEN", &encrypted, true, true);

        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![before]])
                .into_connection(),
        );
        let service = EnvVarService::new(db, encryption_service);

        let error = service
            .update_environment_variable(
                10,
                7,
                "TOKEN".to_string(),
                Some(String::new()),
                vec![],
                false,
                None,
            )
            .await
            .expect_err("blanking an existing secret must be refused");

        assert!(matches!(
            error,
            EnvVarError::SecretValueRequired { ref key } if key == "TOKEN"
        ));
    }

    #[tokio::test]
    async fn test_promoting_legacy_plaintext_row_encrypts_the_stored_value() {
        // Rows written before encryption was enabled hold plaintext. Promoting
        // one without supplying a new value must encrypt what is already there:
        // a secret that is merely hidden from the API but still readable in the
        // database is not a secret. This is the only coverage of that branch —
        // the other tests all start from is_encrypted = true.
        //
        // Asserted against the statement the transaction actually emitted, not
        // against the mock's canned return row: the returned model is whatever
        // the fixture says, so checking it would pass even if the encryption
        // branch were deleted.
        let encryption_service = make_encryption_service();
        let before =
            make_env_var_model_full(8, 10, "LEGACY_KEY", "plain_secret_value", false, false);
        let after = make_env_var_model_full(8, 10, "LEGACY_KEY", "ciphertext", true, true);

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![before]])
            .append_query_results(vec![vec![after]])
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .into_connection();
        let db = Arc::new(db);
        let service = EnvVarService::new(db.clone(), encryption_service.clone());

        let outcome = service
            .update_environment_variable(
                10,
                8,
                "LEGACY_KEY".to_string(),
                None,
                vec![],
                false,
                Some(true),
            )
            .await
            .expect("promoting a legacy plaintext row should succeed");
        assert!(outcome.promoted_to_secret);

        // DatabaseConnection is not Clone under the `mock` feature, and
        // into_transaction_log consumes it — drop the service so this Arc is
        // the sole owner.
        drop(service);
        let db = Arc::try_unwrap(db).expect("service dropped, so this is the only handle");

        // Inspect the statements the transaction actually emitted. `Transaction`
        // keeps its statements private, so match on the Debug rendering: every
        // bound String shows up quoted, and exactly one of them is the
        // ciphertext (it is the only candidate our key can decrypt).
        let log = db.into_transaction_log();
        let dump = format!("{:?}", log);
        assert!(
            dump.to_uppercase().contains("UPDATE"),
            "the transaction must emit an UPDATE"
        );
        let written = dump
            .split('"')
            .skip(1)
            .step_by(2)
            .find(|candidate| encryption_service.decrypt_string(candidate).is_ok())
            .map(|candidate| candidate.to_string())
            .expect("the UPDATE must write a ciphertext this key can decrypt");

        // The plaintext was encrypted exactly once, and survived intact.
        assert_ne!(written, "plain_secret_value");
        assert_eq!(
            encryption_service.decrypt_string(&written).unwrap(),
            "plain_secret_value"
        );
    }
}
