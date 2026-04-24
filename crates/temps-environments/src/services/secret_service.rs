//! Service for managing secrets.
//!
//! Secrets are exposed to user containers as files under `/run/secrets/<KEY>`
//! (tmpfs, mode 0400) instead of as environment variables. Values are always
//! stored encrypted with AES-256-GCM via `EncryptionService` and are never
//! returned in plaintext from the API after creation — the UI shows a masked
//! placeholder. Plaintext is only decrypted at deploy time for the deployer.
//!
//! Shape mirrors `EnvVarService` so callers familiar with env vars can reason
//! about secrets the same way (project + optional environment scoping,
//! junction table for multi-environment membership, transactional writes).

use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set, TransactionTrait,
};
use std::collections::HashMap;
use std::sync::Arc;
use temps_core::EncryptionService;
use temps_entities::{environments, secret_environments, secrets};
use thiserror::Error;

use super::types::{SecretEnvironmentRef, SecretWithEnvironments};

/// Maximum plaintext size for a single secret, in bytes. Matches the
/// per-container tmpfs budget set in the deployer.
pub const SECRET_VALUE_MAX_BYTES: usize = 1_048_576; // 1 MiB

#[derive(Error, Debug)]
pub enum SecretError {
    #[error("Secret {secret_id} not found in project {project_id}")]
    NotFound { secret_id: i32, project_id: i32 },

    #[error("Secret with key '{key}' already exists in project {project_id}")]
    KeyAlreadyExists { project_id: i32, key: String },

    #[error("Secret value for key '{key}' is {size} bytes, exceeds limit of {limit} bytes")]
    ValueTooLarge {
        key: String,
        size: usize,
        limit: usize,
    },

    #[error("Invalid secret key '{key}': {reason}")]
    InvalidKey { key: String, reason: String },

    #[error("Environment {environment_id} not found")]
    EnvironmentNotFound { environment_id: i32 },

    #[error("Failed to encrypt secret '{key}': {reason}")]
    EncryptionFailed { key: String, reason: String },

    #[error("Failed to decrypt secret '{key}' (id={secret_id}): {reason}")]
    DecryptionFailed {
        secret_id: i32,
        key: String,
        reason: String,
    },

    #[error("Database connection error: {0}")]
    DatabaseConnection(String),

    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),
}

impl From<sea_orm::TransactionError<SecretError>> for SecretError {
    fn from(error: sea_orm::TransactionError<SecretError>) -> Self {
        match error {
            sea_orm::TransactionError::Transaction(e) => e,
            sea_orm::TransactionError::Connection(e) => {
                SecretError::DatabaseConnection(e.to_string())
            }
        }
    }
}

/// Validates a secret key. Keys become file names under `/run/secrets/` and
/// are commonly consumed as env-var-like identifiers, so we require the same
/// conservative shape: uppercase letters, digits, and underscores; must start
/// with a letter or underscore; max 255 chars.
fn validate_secret_key(key: &str) -> Result<(), SecretError> {
    if key.is_empty() {
        return Err(SecretError::InvalidKey {
            key: key.to_string(),
            reason: "key cannot be empty".to_string(),
        });
    }
    if key.len() > 255 {
        return Err(SecretError::InvalidKey {
            key: key.to_string(),
            reason: format!("key length {} exceeds 255", key.len()),
        });
    }
    let mut chars = key.chars();
    let first = chars.next().unwrap();
    if !(first.is_ascii_alphabetic() || first == '_') {
        return Err(SecretError::InvalidKey {
            key: key.to_string(),
            reason: "key must start with a letter or underscore".to_string(),
        });
    }
    for c in chars {
        if !(c.is_ascii_alphanumeric() || c == '_') {
            return Err(SecretError::InvalidKey {
                key: key.to_string(),
                reason: format!("invalid character '{}' (allowed: A-Z, a-z, 0-9, _)", c),
            });
        }
    }
    Ok(())
}

#[derive(Clone)]
pub struct SecretService {
    db: Arc<temps_database::DbConnection>,
    encryption_service: Arc<EncryptionService>,
}

impl SecretService {
    pub fn new(
        db: Arc<temps_database::DbConnection>,
        encryption_service: Arc<EncryptionService>,
    ) -> Self {
        Self {
            db,
            encryption_service,
        }
    }

    fn encrypt_value(&self, key: &str, value: &str) -> Result<String, SecretError> {
        self.encryption_service
            .encrypt_string(value)
            .map_err(|e| SecretError::EncryptionFailed {
                key: key.to_string(),
                reason: e.to_string(),
            })
    }

    fn decrypt_value(
        &self,
        secret_id: i32,
        key: &str,
        ciphertext: &str,
    ) -> Result<String, SecretError> {
        self.encryption_service
            .decrypt_string(ciphertext)
            .map_err(|e| SecretError::DecryptionFailed {
                secret_id,
                key: key.to_string(),
                reason: e.to_string(),
            })
    }

    /// Lists secrets visible to a project, optionally filtered to a specific
    /// environment via the junction table.
    ///
    /// Values are NOT decrypted — callers that render to the UI must mask the
    /// value. Use `get_for_deploy` when plaintext is required.
    pub async fn list(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
    ) -> Result<Vec<SecretWithEnvironments>, SecretError> {
        let rows = secrets::Entity::find()
            .filter(secrets::Column::ProjectId.eq(project_id))
            .order_by_desc(secrets::Column::UpdatedAt)
            .all(self.db.as_ref())
            .await?;

        let ids: Vec<i32> = rows.iter().map(|s| s.id).collect();
        if ids.is_empty() {
            return Ok(Vec::new());
        }

        let mut env_query = secret_environments::Entity::find()
            .filter(secret_environments::Column::SecretId.is_in(ids));
        if let Some(env_id) = environment_id {
            env_query = env_query.filter(secret_environments::Column::EnvironmentId.eq(env_id));
        }
        let env_rows: Vec<(secret_environments::Model, Option<environments::Model>)> = env_query
            .find_also_related(environments::Entity)
            .all(self.db.as_ref())
            .await?;

        let mut env_map: HashMap<i32, Vec<SecretEnvironmentRef>> = HashMap::new();
        for (junction, env_opt) in env_rows {
            if let Some(env) = env_opt {
                env_map
                    .entry(junction.secret_id)
                    .or_default()
                    .push(SecretEnvironmentRef {
                        id: env.id,
                        name: env.name,
                        main_url: env.subdomain,
                    });
            }
        }

        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let envs = env_map.get(&row.id).cloned().unwrap_or_default();
            if environment_id.is_some() && envs.is_empty() {
                continue;
            }
            out.push(SecretWithEnvironments {
                id: row.id,
                project_id: row.project_id,
                key: row.key,
                include_in_preview: row.include_in_preview,
                created_at: row.created_at,
                updated_at: row.updated_at,
                environments: envs,
            });
        }
        Ok(out)
    }

    /// Creates a secret. Value is encrypted before insert. Returns the metadata
    /// only (no plaintext value) — callers that need the plaintext back must
    /// call `get_for_deploy` explicitly.
    pub async fn create(
        &self,
        project_id: i32,
        environment_ids: Vec<i32>,
        key: String,
        value: String,
        include_in_preview: bool,
    ) -> Result<SecretWithEnvironments, SecretError> {
        validate_secret_key(&key)?;

        if value.len() > SECRET_VALUE_MAX_BYTES {
            return Err(SecretError::ValueTooLarge {
                key: key.clone(),
                size: value.len(),
                limit: SECRET_VALUE_MAX_BYTES,
            });
        }

        let duplicate = secrets::Entity::find()
            .filter(secrets::Column::ProjectId.eq(project_id))
            .filter(secrets::Column::Key.eq(&key))
            .one(self.db.as_ref())
            .await?;
        if duplicate.is_some() {
            return Err(SecretError::KeyAlreadyExists {
                project_id,
                key: key.clone(),
            });
        }

        let encrypted = self.encrypt_value(&key, &value)?;

        let result = self
            .db
            .transaction::<_, SecretWithEnvironments, SecretError>(|txn| {
                let key = key.clone();
                let encrypted = encrypted.clone();
                let environment_ids = environment_ids.clone();
                Box::pin(async move {
                    let new_row = secrets::ActiveModel {
                        project_id: Set(project_id),
                        environment_id: Set(None),
                        key: Set(key.clone()),
                        value: Set(encrypted),
                        include_in_preview: Set(include_in_preview),
                        created_at: Set(chrono::Utc::now()),
                        updated_at: Set(chrono::Utc::now()),
                        ..Default::default()
                    };
                    let row = new_row.insert(txn).await?;

                    let mut envs = Vec::new();
                    for env_id in &environment_ids {
                        let env = environments::Entity::find_by_id(*env_id)
                            .one(txn)
                            .await?
                            .ok_or(SecretError::EnvironmentNotFound {
                                environment_id: *env_id,
                            })?;

                        let junction = secret_environments::ActiveModel {
                            secret_id: Set(row.id),
                            environment_id: Set(*env_id),
                            created_at: Set(chrono::Utc::now()),
                            ..Default::default()
                        };
                        junction.insert(txn).await?;

                        envs.push(SecretEnvironmentRef {
                            id: env.id,
                            name: env.name,
                            main_url: env.subdomain,
                        });
                    }

                    Ok(SecretWithEnvironments {
                        id: row.id,
                        project_id: row.project_id,
                        key: row.key,
                        include_in_preview: row.include_in_preview,
                        created_at: row.created_at,
                        updated_at: row.updated_at,
                        environments: envs,
                    })
                })
            })
            .await?;

        Ok(result)
    }

    /// Updates a secret's value and/or environment membership. Key is
    /// immutable here — rotating a secret keeps the same key so consumers
    /// don't need config changes.
    pub async fn update(
        &self,
        project_id: i32,
        secret_id: i32,
        new_value: Option<String>,
        environment_ids: Vec<i32>,
        include_in_preview: bool,
    ) -> Result<SecretWithEnvironments, SecretError> {
        if let Some(v) = &new_value {
            if v.len() > SECRET_VALUE_MAX_BYTES {
                // Key unknown here without a DB read; use a placeholder that the
                // handler can enrich. Cheap read first so the error is accurate.
                let row = secrets::Entity::find_by_id(secret_id)
                    .filter(secrets::Column::ProjectId.eq(project_id))
                    .one(self.db.as_ref())
                    .await?
                    .ok_or(SecretError::NotFound {
                        secret_id,
                        project_id,
                    })?;
                return Err(SecretError::ValueTooLarge {
                    key: row.key,
                    size: v.len(),
                    limit: SECRET_VALUE_MAX_BYTES,
                });
            }
        }

        let encrypted_new = match &new_value {
            Some(v) => {
                // Encrypt eagerly so the transaction body is pure DB work.
                // We use a placeholder key for the error context; the real
                // key is fetched inside the txn before use.
                Some(self.encryption_service.encrypt_string(v).map_err(|e| {
                    SecretError::EncryptionFailed {
                        key: format!("secret_id={}", secret_id),
                        reason: e.to_string(),
                    }
                })?)
            }
            None => None,
        };

        let result = self
            .db
            .transaction::<_, SecretWithEnvironments, SecretError>(|txn| {
                let environment_ids = environment_ids.clone();
                let encrypted_new = encrypted_new.clone();
                Box::pin(async move {
                    let row = secrets::Entity::find_by_id(secret_id)
                        .filter(secrets::Column::ProjectId.eq(project_id))
                        .one(txn)
                        .await?
                        .ok_or(SecretError::NotFound {
                            secret_id,
                            project_id,
                        })?;

                    let mut active: secrets::ActiveModel = row.into();
                    if let Some(v) = encrypted_new {
                        active.value = Set(v);
                    }
                    active.include_in_preview = Set(include_in_preview);
                    active.updated_at = Set(chrono::Utc::now());
                    let row = active.update(txn).await?;

                    secret_environments::Entity::delete_many()
                        .filter(secret_environments::Column::SecretId.eq(secret_id))
                        .exec(txn)
                        .await?;

                    let mut envs = Vec::new();
                    for env_id in &environment_ids {
                        let env = environments::Entity::find_by_id(*env_id)
                            .one(txn)
                            .await?
                            .ok_or(SecretError::EnvironmentNotFound {
                                environment_id: *env_id,
                            })?;

                        let junction = secret_environments::ActiveModel {
                            secret_id: Set(row.id),
                            environment_id: Set(*env_id),
                            created_at: Set(chrono::Utc::now()),
                            ..Default::default()
                        };
                        junction.insert(txn).await?;

                        envs.push(SecretEnvironmentRef {
                            id: env.id,
                            name: env.name,
                            main_url: env.subdomain,
                        });
                    }

                    Ok(SecretWithEnvironments {
                        id: row.id,
                        project_id: row.project_id,
                        key: row.key,
                        include_in_preview: row.include_in_preview,
                        created_at: row.created_at,
                        updated_at: row.updated_at,
                        environments: envs,
                    })
                })
            })
            .await?;

        Ok(result)
    }

    pub async fn delete(&self, project_id: i32, secret_id: i32) -> Result<(), SecretError> {
        let affected = self
            .db
            .transaction::<_, u64, SecretError>(|txn| {
                Box::pin(async move {
                    secret_environments::Entity::delete_many()
                        .filter(secret_environments::Column::SecretId.eq(secret_id))
                        .exec(txn)
                        .await?;

                    let res = secrets::Entity::delete_many()
                        .filter(secrets::Column::Id.eq(secret_id))
                        .filter(secrets::Column::ProjectId.eq(project_id))
                        .exec(txn)
                        .await?;

                    Ok(res.rows_affected)
                })
            })
            .await?;

        if affected == 0 {
            return Err(SecretError::NotFound {
                secret_id,
                project_id,
            });
        }
        Ok(())
    }

    /// Returns decrypted secrets for a project+environment, ready to be
    /// materialized as files under `/run/secrets/<KEY>` by the deployer.
    ///
    /// Selection semantics match env vars:
    ///   - A secret with no junction rows applies project-wide (all envs)
    ///   - A secret with junction rows applies only to its listed envs
    ///   - `include_in_preview` filters preview environments at the caller
    ///     layer; this method returns the raw project + environment set.
    pub async fn get_for_deploy(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
    ) -> Result<HashMap<String, String>, SecretError> {
        let rows = secrets::Entity::find()
            .filter(secrets::Column::ProjectId.eq(project_id))
            .all(self.db.as_ref())
            .await?;

        if rows.is_empty() {
            return Ok(HashMap::new());
        }

        let ids: Vec<i32> = rows.iter().map(|s| s.id).collect();
        let junctions = secret_environments::Entity::find()
            .filter(secret_environments::Column::SecretId.is_in(ids))
            .all(self.db.as_ref())
            .await?;

        // Per-secret set of environment_ids the secret is bound to. An empty
        // set means "applies to all environments in the project".
        let mut bindings: HashMap<i32, Vec<i32>> = HashMap::new();
        for j in junctions {
            bindings
                .entry(j.secret_id)
                .or_default()
                .push(j.environment_id);
        }

        let mut out = HashMap::new();
        for row in rows {
            let applies = match (environment_id, bindings.get(&row.id)) {
                // No environment requested: include project-scoped (no bindings) only
                (None, None) => true,
                (None, Some(_)) => false,
                // Environment requested: include if project-scoped or explicitly bound
                (Some(_), None) => true,
                (Some(env_id), Some(env_list)) => env_list.contains(&env_id),
            };
            if !applies {
                continue;
            }
            let plaintext = self.decrypt_value(row.id, &row.key, &row.value)?;
            out.insert(row.key, plaintext);
        }
        Ok(out)
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

    fn make_secret_model(id: i32, project_id: i32, key: &str, value: &str) -> secrets::Model {
        secrets::Model {
            id,
            project_id,
            environment_id: None,
            key: key.to_string(),
            value: value.to_string(),
            include_in_preview: false,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn test_validate_secret_key_accepts_valid() {
        assert!(validate_secret_key("DB_PASSWORD").is_ok());
        assert!(validate_secret_key("_underscore").is_ok());
        assert!(validate_secret_key("api_key_2").is_ok());
        assert!(validate_secret_key("A").is_ok());
    }

    #[test]
    fn test_validate_secret_key_rejects_empty() {
        let err = validate_secret_key("").unwrap_err();
        assert!(matches!(err, SecretError::InvalidKey { .. }));
    }

    #[test]
    fn test_validate_secret_key_rejects_leading_digit() {
        let err = validate_secret_key("1FOO").unwrap_err();
        assert!(matches!(err, SecretError::InvalidKey { .. }));
    }

    #[test]
    fn test_validate_secret_key_rejects_special_chars() {
        let err = validate_secret_key("FOO-BAR").unwrap_err();
        assert!(matches!(err, SecretError::InvalidKey { .. }));
        let err = validate_secret_key("FOO.BAR").unwrap_err();
        assert!(matches!(err, SecretError::InvalidKey { .. }));
        let err = validate_secret_key("FOO/BAR").unwrap_err();
        assert!(matches!(err, SecretError::InvalidKey { .. }));
    }

    #[test]
    fn test_validate_secret_key_rejects_over_255_chars() {
        let long = "A".repeat(256);
        let err = validate_secret_key(&long).unwrap_err();
        assert!(matches!(err, SecretError::InvalidKey { .. }));
    }

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        let svc = make_encryption_service();
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let service = SecretService::new(db, svc);

        let plaintext = "my_super_secret_password_123";
        let encrypted = service.encrypt_value("DB_PASSWORD", plaintext).unwrap();
        assert_ne!(encrypted, plaintext);

        let decrypted = service.decrypt_value(1, "DB_PASSWORD", &encrypted).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn test_decrypt_invalid_returns_typed_error() {
        let svc = make_encryption_service();
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let service = SecretService::new(db, svc);

        let err = service
            .decrypt_value(7, "KEY", "not-valid-base64!!!")
            .unwrap_err();
        assert!(matches!(
            err,
            SecretError::DecryptionFailed { secret_id: 7, .. }
        ));
    }

    #[tokio::test]
    async fn test_create_rejects_value_over_limit() {
        let svc = make_encryption_service();
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let service = SecretService::new(db, svc);

        let big = "x".repeat(SECRET_VALUE_MAX_BYTES + 1);
        let err = service
            .create(10, vec![], "BIG_SECRET".to_string(), big, false)
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            SecretError::ValueTooLarge {
                size, limit, ..
            } if size == SECRET_VALUE_MAX_BYTES + 1 && limit == SECRET_VALUE_MAX_BYTES
        ));
    }

    #[tokio::test]
    async fn test_create_rejects_invalid_key() {
        let svc = make_encryption_service();
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let service = SecretService::new(db, svc);

        let err = service
            .create(10, vec![], "bad-key!".to_string(), "v".to_string(), false)
            .await
            .unwrap_err();
        assert!(matches!(err, SecretError::InvalidKey { .. }));
    }

    #[tokio::test]
    async fn test_create_rejects_duplicate_key() {
        let svc = make_encryption_service();
        let existing = make_secret_model(1, 10, "API_KEY", "cipher");
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![existing]])
                .into_connection(),
        );
        let service = SecretService::new(db, svc);

        let err = service
            .create(
                10,
                vec![],
                "API_KEY".to_string(),
                "new_value".to_string(),
                false,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, SecretError::KeyAlreadyExists { .. }));
    }

    #[tokio::test]
    async fn test_list_empty_returns_empty() {
        let svc = make_encryption_service();
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![Vec::<secrets::Model>::new()])
                .into_connection(),
        );
        let service = SecretService::new(db, svc);
        let out = service.list(10, None).await.unwrap();
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn test_list_does_not_expose_ciphertext() {
        // list() returns SecretWithEnvironments, which has NO value field.
        // This test confirms the shape by pattern-matching.
        let svc = make_encryption_service();
        let row = make_secret_model(1, 10, "TOKEN", "ciphertext_blob");
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![row]])
                .append_query_results(vec![Vec::<(
                    secret_environments::Model,
                    Option<environments::Model>,
                )>::new()])
                .into_connection(),
        );
        let service = SecretService::new(db, svc);
        let out = service.list(10, None).await.unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].key, "TOKEN");
        // There is no `value` field on SecretWithEnvironments — ciphertext
        // never leaves the service boundary via list().
    }

    #[tokio::test]
    async fn test_get_for_deploy_returns_decrypted_project_scoped() {
        let svc = make_encryption_service();
        let plaintext = "redis://user:pass@host:6379/0";
        let encrypted = svc.encrypt_string(plaintext).unwrap();
        let row = make_secret_model(1, 10, "REDIS_URL", &encrypted);

        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![row]])
                .append_query_results(vec![Vec::<secret_environments::Model>::new()])
                .into_connection(),
        );
        let service = SecretService::new(db, svc);

        let out = service.get_for_deploy(10, None).await.unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out.get("REDIS_URL").map(|s| s.as_str()), Some(plaintext));
    }

    #[tokio::test]
    async fn test_get_for_deploy_skips_env_scoped_when_no_env_requested() {
        let svc = make_encryption_service();
        let row = make_secret_model(1, 10, "BOUND", &svc.encrypt_string("v").unwrap());

        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![row]])
                .append_query_results(vec![vec![secret_environments::Model {
                    id: 1,
                    secret_id: 1,
                    environment_id: 99,
                    created_at: chrono::Utc::now(),
                }]])
                .into_connection(),
        );
        let service = SecretService::new(db, svc);

        let out = service.get_for_deploy(10, None).await.unwrap();
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn test_get_for_deploy_includes_matching_env_binding() {
        let svc = make_encryption_service();
        let plaintext = "bound-value";
        let row = make_secret_model(1, 10, "BOUND", &svc.encrypt_string(plaintext).unwrap());

        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![row]])
                .append_query_results(vec![vec![secret_environments::Model {
                    id: 1,
                    secret_id: 1,
                    environment_id: 42,
                    created_at: chrono::Utc::now(),
                }]])
                .into_connection(),
        );
        let service = SecretService::new(db, svc);

        let out = service.get_for_deploy(10, Some(42)).await.unwrap();
        assert_eq!(out.get("BOUND").map(|s| s.as_str()), Some(plaintext));
    }

    #[tokio::test]
    async fn test_get_for_deploy_excludes_non_matching_env_binding() {
        let svc = make_encryption_service();
        let row = make_secret_model(1, 10, "BOUND", &svc.encrypt_string("v").unwrap());

        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![row]])
                .append_query_results(vec![vec![secret_environments::Model {
                    id: 1,
                    secret_id: 1,
                    environment_id: 42,
                    created_at: chrono::Utc::now(),
                }]])
                .into_connection(),
        );
        let service = SecretService::new(db, svc);

        let out = service.get_for_deploy(10, Some(7)).await.unwrap();
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn test_delete_not_found_returns_typed_error() {
        let svc = make_encryption_service();
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                // First delete (junction): 0 rows is fine
                .append_exec_results(vec![MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 0,
                }])
                // Second delete (secrets): 0 rows -> triggers NotFound
                .append_exec_results(vec![MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 0,
                }])
                .into_connection(),
        );
        let service = SecretService::new(db, svc);
        let err = service.delete(10, 999).await.unwrap_err();
        assert!(matches!(
            err,
            SecretError::NotFound {
                secret_id: 999,
                project_id: 10
            }
        ));
    }
}
