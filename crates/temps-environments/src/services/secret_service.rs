// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Service for managing secrets.
//!
//! Secrets are exposed to user containers as files under `/run/secrets/<KEY>`
//! via a read-only mount instead of as environment variables. Values are always
//! stored encrypted with AES-256-GCM via `EncryptionService` and are never
//! returned in plaintext from the API after creation — the UI shows a masked
//! placeholder. Plaintext is only decrypted at deploy time for the deployer.
//!
//! Shape mirrors `EnvVarService` so callers familiar with env vars can reason
//! about secrets the same way (project + optional environment scoping,
//! junction table for multi-environment membership, transactional writes).

use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseBackend, DatabaseTransaction,
    EntityTrait, QueryFilter, QueryOrder, QuerySelect, Set, Statement, TransactionTrait,
};
use std::collections::HashMap;
use std::sync::Arc;
use temps_core::EncryptionService;
use temps_entities::{environments, secret_compose_services, secret_environments, secrets};
use thiserror::Error;

use super::types::{SecretEnvironmentRef, SecretWithEnvironments};

/// Maximum plaintext size for a single secret, in bytes. Bounds how much
/// plaintext a single deployment can materialize onto the host.
pub const SECRET_VALUE_MAX_BYTES: usize = 1_048_576; // 1 MiB

#[derive(Error, Debug)]
pub enum SecretError {
    #[error("Invalid compose service name '{service}': {reason}")]
    InvalidComposeService { service: String, reason: String },

    #[error("Secret {secret_id} not found in project {project_id}")]
    NotFound { secret_id: i32, project_id: i32 },

    #[error(
        "Secret with key '{key}' already applies to one or more requested environments in project {project_id}"
    )]
    KeyAlreadyExists { project_id: i32, key: String },

    #[error("Secret value for key '{key}' is {size} bytes, exceeds limit of {limit} bytes")]
    ValueTooLarge {
        key: String,
        size: usize,
        limit: usize,
    },

    #[error("Invalid secret key '{key}': {reason}")]
    InvalidKey { key: String, reason: String },

    #[error("Environment {environment_id} was not found in project {project_id}")]
    EnvironmentNotFound {
        environment_id: i32,
        project_id: i32,
    },

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
    let Some(first) = chars.next() else {
        return Err(SecretError::InvalidKey {
            key: key.to_string(),
            reason: "key cannot be empty".to_string(),
        });
    };
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

/// Validates a Docker Compose service name.
///
/// Compose itself allows `[a-zA-Z0-9._-]`. We enforce that same set and
/// additionally reject `.` and `..`, because the deployer turns this value
/// into a directory name under the secrets root -- a name containing a path
/// separator or a parent-directory reference would materialize plaintext
/// outside the stack's own directory.
fn validate_compose_service_name(name: &str) -> Result<(), SecretError> {
    if name.is_empty() {
        return Err(SecretError::InvalidComposeService {
            service: name.to_string(),
            reason: "service name cannot be empty".to_string(),
        });
    }
    if name.len() > 255 {
        return Err(SecretError::InvalidComposeService {
            service: name.to_string(),
            reason: format!("service name length {} exceeds 255", name.len()),
        });
    }
    if name == "." || name == ".." {
        return Err(SecretError::InvalidComposeService {
            service: name.to_string(),
            reason: "service name cannot be '.' or '..'".to_string(),
        });
    }
    if let Some(c) = name
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || *c == '.' || *c == '_' || *c == '-'))
    {
        return Err(SecretError::InvalidComposeService {
            service: name.to_string(),
            reason: format!("invalid character '{c}' (allowed: A-Z, a-z, 0-9, '.', '_', '-')"),
        });
    }
    Ok(())
}

/// Normalizes a requested service scope: validated, de-duplicated, order
/// preserved. An empty list means "every service in the stack".
fn normalize_compose_services(services: Vec<String>) -> Result<Vec<String>, SecretError> {
    let mut out: Vec<String> = Vec::with_capacity(services.len());
    for name in services {
        let name = name.trim().to_string();
        if name.is_empty() {
            continue;
        }
        validate_compose_service_name(&name)?;
        if !out.contains(&name) {
            out.push(name);
        }
    }
    Ok(out)
}

fn secret_scope_overlaps(
    requested_environment_ids: &[i32],
    existing_environment_ids: &[i32],
) -> bool {
    let requested_is_global = requested_environment_ids.is_empty();
    let existing_is_global = existing_environment_ids.is_empty();
    requested_is_global
        || existing_is_global
        || existing_environment_ids
            .iter()
            .any(|id| requested_environment_ids.contains(id))
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

    /// Resolve requested environments inside the project that owns the secret.
    /// IDs are de-duplicated so repeated input cannot trip the junction
    /// table's unique constraint.
    async fn environments_in_project(
        txn: &DatabaseTransaction,
        project_id: i32,
        environment_ids: &[i32],
    ) -> Result<Vec<environments::Model>, SecretError> {
        let unique_ids = environment_ids
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        if unique_ids.is_empty() {
            return Ok(Vec::new());
        }

        let models = environments::Entity::find()
            .filter(environments::Column::Id.is_in(unique_ids.iter().copied()))
            .filter(environments::Column::ProjectId.eq(project_id))
            .filter(environments::Column::DeletedAt.is_null())
            // Environment deletion updates this row, so FOR SHARE keeps every
            // validated environment active until the secret transaction has
            // inserted its bindings and committed. Lock in a stable order so
            // concurrent multi-environment writes cannot deadlock each other.
            .order_by_asc(environments::Column::Id)
            .lock_shared()
            .all(txn)
            .await?;
        let mut by_id = models
            .into_iter()
            .map(|environment| (environment.id, environment))
            .collect::<HashMap<_, _>>();

        unique_ids
            .into_iter()
            .map(|environment_id| {
                by_id
                    .remove(&environment_id)
                    .ok_or(SecretError::EnvironmentNotFound {
                        environment_id,
                        project_id,
                    })
            })
            .collect()
    }

    /// Serialize and validate one key's scope. Empty bindings mean project-wide,
    /// so they overlap every scoped secret with the same key.
    async fn claim_key_scope(
        txn: &DatabaseTransaction,
        project_id: i32,
        key: &str,
        environment_ids: &[i32],
        excluded_secret_id: Option<i32>,
    ) -> Result<(), SecretError> {
        txn.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT pg_advisory_xact_lock($1, hashtext($2))",
            [project_id.into(), key.to_string().into()],
        ))
        .await?;

        let mut query = secrets::Entity::find()
            .filter(secrets::Column::ProjectId.eq(project_id))
            .filter(secrets::Column::Key.eq(key));
        if let Some(secret_id) = excluded_secret_id {
            query = query.filter(secrets::Column::Id.ne(secret_id));
        }
        let existing = query
            .find_with_related(secret_environments::Entity)
            .all(txn)
            .await?;
        let overlaps = existing.iter().any(|(_, bindings)| {
            let existing_environment_ids = bindings
                .iter()
                .map(|binding| binding.environment_id)
                .collect::<Vec<_>>();
            secret_scope_overlaps(environment_ids, &existing_environment_ids)
        });
        if overlaps {
            return Err(SecretError::KeyAlreadyExists {
                project_id,
                key: key.to_string(),
            });
        }
        Ok(())
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
            .filter(secret_environments::Column::SecretId.is_in(ids.clone()));
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

        let mut service_map = self.load_compose_services(&ids).await?;

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
                compose_services: service_map.remove(&row.id).unwrap_or_default(),
            });
        }
        Ok(out)
    }

    /// Compose-service scopes for a set of secrets, in a single query.
    /// Secrets with no rows are simply absent from the map, which callers
    /// read as "every service".
    async fn load_compose_services(
        &self,
        secret_ids: &[i32],
    ) -> Result<HashMap<i32, Vec<String>>, SecretError> {
        if secret_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let rows = secret_compose_services::Entity::find()
            .filter(secret_compose_services::Column::SecretId.is_in(secret_ids.to_vec()))
            .order_by_asc(secret_compose_services::Column::ServiceName)
            .all(self.db.as_ref())
            .await?;
        let mut map: HashMap<i32, Vec<String>> = HashMap::new();
        for row in rows {
            map.entry(row.secret_id).or_default().push(row.service_name);
        }
        Ok(map)
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
        compose_services: Vec<String>,
    ) -> Result<SecretWithEnvironments, SecretError> {
        validate_secret_key(&key)?;
        let compose_services = normalize_compose_services(compose_services)?;

        if value.len() > SECRET_VALUE_MAX_BYTES {
            return Err(SecretError::ValueTooLarge {
                key: key.clone(),
                size: value.len(),
                limit: SECRET_VALUE_MAX_BYTES,
            });
        }

        let encrypted = self.encrypt_value(&key, &value)?;

        let result = self
            .db
            .transaction::<_, SecretWithEnvironments, SecretError>(|txn| {
                let key = key.clone();
                let encrypted = encrypted.clone();
                let environment_ids = environment_ids.clone();
                let compose_services = compose_services.clone();
                Box::pin(async move {
                    let scoped_environments =
                        Self::environments_in_project(txn, project_id, &environment_ids).await?;
                    let environment_ids = scoped_environments
                        .iter()
                        .map(|environment| environment.id)
                        .collect::<Vec<_>>();
                    Self::claim_key_scope(txn, project_id, &key, &environment_ids, None).await?;

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
                    for (env_id, env) in environment_ids.iter().zip(scoped_environments) {
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

                    for service in &compose_services {
                        secret_compose_services::ActiveModel {
                            secret_id: Set(row.id),
                            service_name: Set(service.clone()),
                            created_at: Set(chrono::Utc::now()),
                            ..Default::default()
                        }
                        .insert(txn)
                        .await?;
                    }

                    Ok(SecretWithEnvironments {
                        id: row.id,
                        project_id: row.project_id,
                        key: row.key,
                        include_in_preview: row.include_in_preview,
                        created_at: row.created_at,
                        updated_at: row.updated_at,
                        environments: envs,
                        compose_services,
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
        compose_services: Vec<String>,
    ) -> Result<SecretWithEnvironments, SecretError> {
        let compose_services = normalize_compose_services(compose_services)?;
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
                let compose_services = compose_services.clone();
                Box::pin(async move {
                    let row = secrets::Entity::find_by_id(secret_id)
                        .filter(secrets::Column::ProjectId.eq(project_id))
                        .lock_exclusive()
                        .one(txn)
                        .await?
                        .ok_or(SecretError::NotFound {
                            secret_id,
                            project_id,
                        })?;
                    let scoped_environments =
                        Self::environments_in_project(txn, project_id, &environment_ids).await?;
                    let environment_ids = scoped_environments
                        .iter()
                        .map(|environment| environment.id)
                        .collect::<Vec<_>>();
                    Self::claim_key_scope(
                        txn,
                        project_id,
                        &row.key,
                        &environment_ids,
                        Some(secret_id),
                    )
                    .await?;

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
                    for (env_id, env) in environment_ids.iter().zip(scoped_environments) {
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

                    // Replace the scope wholesale, same as environments above:
                    // a PATCH that omits a service must remove that service's
                    // access, not silently keep it.
                    secret_compose_services::Entity::delete_many()
                        .filter(secret_compose_services::Column::SecretId.eq(secret_id))
                        .exec(txn)
                        .await?;
                    for service in &compose_services {
                        secret_compose_services::ActiveModel {
                            secret_id: Set(row.id),
                            service_name: Set(service.clone()),
                            created_at: Set(chrono::Utc::now()),
                            ..Default::default()
                        }
                        .insert(txn)
                        .await?;
                    }

                    Ok(SecretWithEnvironments {
                        id: row.id,
                        project_id: row.project_id,
                        key: row.key,
                        include_in_preview: row.include_in_preview,
                        created_at: row.created_at,
                        updated_at: row.updated_at,
                        environments: envs,
                        compose_services,
                    })
                })
            })
            .await?;

        Ok(result)
    }

    pub async fn delete(&self, project_id: i32, secret_id: i32) -> Result<(), SecretError> {
        self.db
            .transaction::<_, (), SecretError>(|txn| {
                Box::pin(async move {
                    let secret = secrets::Entity::find_by_id(secret_id)
                        .filter(secrets::Column::ProjectId.eq(project_id))
                        .lock_exclusive()
                        .one(txn)
                        .await?
                        .ok_or(SecretError::NotFound {
                            secret_id,
                            project_id,
                        })?;

                    secret_environments::Entity::delete_many()
                        .filter(secret_environments::Column::SecretId.eq(secret_id))
                        .exec(txn)
                        .await?;

                    let active: secrets::ActiveModel = secret.into();
                    active.delete(txn).await?;
                    Ok(())
                })
            })
            .await?;
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
    fn test_secret_scope_allows_same_key_in_disjoint_environments() {
        assert!(!secret_scope_overlaps(&[2], &[1]));
    }

    #[test]
    fn test_secret_scope_rejects_shared_environment() {
        assert!(secret_scope_overlaps(&[1, 2], &[2, 3]));
    }

    #[test]
    fn test_secret_scope_rejects_global_overlap_in_both_directions() {
        assert!(secret_scope_overlaps(&[], &[1]));
        assert!(secret_scope_overlaps(&[1], &[]));
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
            .create(10, vec![], "BIG_SECRET".to_string(), big, false, vec![])
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
            .create(
                10,
                vec![],
                "bad-key!".to_string(),
                "v".to_string(),
                false,
                vec![],
            )
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
                .append_exec_results(vec![MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 1,
                }])
                .append_query_results(vec![vec![(
                    existing,
                    Option::<secret_environments::Model>::None,
                )]])
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
                vec![],
            )
            .await
            .unwrap_err();
        assert!(matches!(err, SecretError::KeyAlreadyExists { .. }));
    }

    #[tokio::test]
    async fn test_create_propagates_key_scope_lock_database_error() {
        let svc = make_encryption_service();
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_exec_errors([sea_orm::DbErr::Custom(
                    "advisory lock unavailable".to_string(),
                )])
                .into_connection(),
        );
        let service = SecretService::new(db, svc);

        let err = service
            .create(
                10,
                vec![],
                "API_KEY".to_string(),
                "value".to_string(),
                false,
                vec![],
            )
            .await
            .unwrap_err();

        assert!(matches!(err, SecretError::Database(_)));
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
                // Compose-service scopes: none, so the secret goes to every
                // service in the stack.
                .append_query_results(vec![Vec::<secret_compose_services::Model>::new()])
                .into_connection(),
        );
        let service = SecretService::new(db, svc);
        let out = service.list(10, None).await.unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].key, "TOKEN");
        assert!(
            out[0].compose_services.is_empty(),
            "no scope rows must read as 'every service', not 'no services'"
        );
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
                .append_query_results(vec![Vec::<secrets::Model>::new()])
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

#[cfg(test)]
mod integration_tests {
    use super::*;
    use chrono::Utc;
    use std::time::Duration;
    use temps_database::test_utils::{is_container_runtime_unavailable, TestDatabase};
    use temps_entities::{preset::Preset, projects, upstream_config::UpstreamList};

    const ENCRYPTION_KEY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    async fn test_database() -> Option<TestDatabase> {
        match TestDatabase::with_migrations().await {
            Ok(database) => Some(database),
            Err(error) if is_container_runtime_unavailable(&error.to_string()) => {
                eprintln!(
                    "Docker unavailable, skipping secret scoping integration test: {error:#}"
                );
                None
            }
            Err(error) => panic!("secret scoping test database setup failed: {error:#}"),
        }
    }

    fn secret_service(test_db: &TestDatabase) -> SecretService {
        let encryption = EncryptionService::new(ENCRYPTION_KEY)
            .expect("the test encryption key should be valid");
        SecretService::new(test_db.connection_arc(), Arc::new(encryption))
    }

    async fn create_project(test_db: &TestDatabase, suffix: &str) -> projects::Model {
        projects::ActiveModel {
            name: Set(format!("Secret scoping {suffix}")),
            repo_name: Set(format!("repo-{suffix}")),
            repo_owner: Set("temps-tests".to_string()),
            directory: Set("/".to_string()),
            main_branch: Set("main".to_string()),
            slug: Set(format!("secret-scoping-{suffix}")),
            preset: Set(Preset::NextJs),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(test_db.connection())
        .await
        .expect("project fixture should insert")
    }

    async fn create_environment(
        test_db: &TestDatabase,
        project_id: i32,
        suffix: &str,
    ) -> environments::Model {
        environments::ActiveModel {
            project_id: Set(project_id),
            name: Set(format!("Environment {suffix}")),
            slug: Set(suffix.to_string()),
            host: Set(format!("{suffix}.example.test")),
            upstreams: Set(UpstreamList::default()),
            subdomain: Set(format!("secret-scoping-{suffix}.example.test")),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(test_db.connection())
        .await
        .expect("environment fixture should insert")
    }

    async fn create_secret(
        service: &SecretService,
        project_id: i32,
        environment_ids: Vec<i32>,
        key: &str,
        value: &str,
    ) -> Result<SecretWithEnvironments, SecretError> {
        service
            .create(
                project_id,
                environment_ids,
                key.to_string(),
                value.to_string(),
                false,
                Vec::new(),
            )
            .await
    }

    async fn hold_key_scope_lock(
        test_db: &TestDatabase,
        project_id: i32,
        key: &str,
    ) -> DatabaseTransaction {
        let txn = test_db
            .connection()
            .begin()
            .await
            .expect("lock transaction should begin");
        txn.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT pg_advisory_xact_lock($1, hashtext($2))",
            [project_id.into(), key.to_string().into()],
        ))
        .await
        .expect("advisory lock should be acquired");
        txn
    }

    async fn wait_for_key_scope_waiter(test_db: &TestDatabase, project_id: i32, key: &str) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let row = test_db.connection().query_one(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "SELECT EXISTS (SELECT 1 FROM pg_locks WHERE locktype = 'advisory' AND classid = $1::oid AND objid = hashtext($2)::oid AND NOT granted) AS waiting",
                    [project_id.into(), key.to_string().into()],
                )).await.expect("waiter query should succeed").expect("waiter query should return a row");
                if row.try_get::<bool>("", "waiting").expect("waiting should be boolean") {
                    break;
                }
                tokio::task::yield_now().await;
            }
        }).await.expect("secret write should wait after locking environments");
    }

    async fn soft_delete_environment(db: Arc<temps_database::DbConnection>, environment_id: i32) {
        db.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE environments SET deleted_at = NOW() WHERE id = $1",
            [environment_id.into()],
        ))
        .await
        .expect("environment soft deletion should succeed");
    }

    #[tokio::test]
    async fn test_create_same_key_in_distinct_environments_returns_own_deploy_values() {
        let Some(test_db) = test_database().await else {
            return;
        };
        let project = create_project(&test_db, "distinct").await;
        let production = create_environment(&test_db, project.id, "distinct-production").await;
        let staging = create_environment(&test_db, project.id, "distinct-staging").await;
        let service = secret_service(&test_db);

        create_secret(
            &service,
            project.id,
            vec![production.id],
            "DATABASE_URL",
            "postgres://production",
        )
        .await
        .expect("production-scoped secret should be created");
        create_secret(
            &service,
            project.id,
            vec![staging.id],
            "DATABASE_URL",
            "postgres://staging",
        )
        .await
        .expect("disjoint staging-scoped secret should be created");

        let production_values = service
            .get_for_deploy(project.id, Some(production.id))
            .await
            .expect("production secrets should resolve");
        let staging_values = service
            .get_for_deploy(project.id, Some(staging.id))
            .await
            .expect("staging secrets should resolve");

        assert_eq!(
            production_values.get("DATABASE_URL").map(String::as_str),
            Some("postgres://production")
        );
        assert_eq!(production_values.len(), 1);
        assert_eq!(
            staging_values.get("DATABASE_URL").map(String::as_str),
            Some("postgres://staging")
        );
        assert_eq!(staging_values.len(), 1);
    }

    #[tokio::test]
    async fn test_create_overlapping_same_environment_and_global_scopes_rejects_collisions() {
        let Some(test_db) = test_database().await else {
            return;
        };
        let project = create_project(&test_db, "collisions").await;
        let production = create_environment(&test_db, project.id, "collisions-production").await;
        let staging = create_environment(&test_db, project.id, "collisions-staging").await;
        let service = secret_service(&test_db);

        create_secret(&service, project.id, vec![production.id], "TOKEN", "one")
            .await
            .expect("first environment-scoped secret should be created");
        let same_environment =
            create_secret(&service, project.id, vec![production.id], "TOKEN", "two")
                .await
                .expect_err("the same key and environment should overlap");
        let global_after_scoped =
            create_secret(&service, project.id, Vec::new(), "TOKEN", "global")
                .await
                .expect_err("a global secret should overlap an environment-scoped secret");

        create_secret(&service, project.id, Vec::new(), "GLOBAL_TOKEN", "global")
            .await
            .expect("first global secret should be created");
        let scoped_after_global = create_secret(
            &service,
            project.id,
            vec![staging.id],
            "GLOBAL_TOKEN",
            "staging",
        )
        .await
        .expect_err("an environment-scoped secret should overlap a global secret");

        for error in [same_environment, global_after_scoped, scoped_after_global] {
            assert!(matches!(
                error,
                SecretError::KeyAlreadyExists { project_id, .. } if project_id == project.id
            ));
        }
    }

    #[tokio::test]
    async fn test_update_to_overlapping_environment_rejects_and_rolls_back_original() {
        let Some(test_db) = test_database().await else {
            return;
        };
        let project = create_project(&test_db, "update-rollback").await;
        let production = create_environment(&test_db, project.id, "update-production").await;
        let staging = create_environment(&test_db, project.id, "update-staging").await;
        let service = secret_service(&test_db);
        let original = create_secret(
            &service,
            project.id,
            vec![production.id],
            "SHARED_KEY",
            "original-production",
        )
        .await
        .expect("production secret should be created");
        create_secret(
            &service,
            project.id,
            vec![staging.id],
            "SHARED_KEY",
            "original-staging",
        )
        .await
        .expect("staging secret should be created");

        let error = service
            .update(
                project.id,
                original.id,
                Some("replacement".to_string()),
                vec![staging.id],
                true,
                vec!["web".to_string()],
            )
            .await
            .expect_err("moving onto an occupied key scope should fail");

        assert!(matches!(error, SecretError::KeyAlreadyExists { .. }));
        let production_values = service
            .get_for_deploy(project.id, Some(production.id))
            .await
            .expect("the original production scope should remain readable");
        let staging_values = service
            .get_for_deploy(project.id, Some(staging.id))
            .await
            .expect("the original staging scope should remain readable");
        assert_eq!(
            production_values.get("SHARED_KEY").map(String::as_str),
            Some("original-production")
        );
        assert_eq!(
            staging_values.get("SHARED_KEY").map(String::as_str),
            Some("original-staging")
        );
        let unchanged = service
            .list(project.id, Some(production.id))
            .await
            .expect("the original metadata should remain readable");
        assert_eq!(unchanged.len(), 1);
        assert!(!unchanged[0].include_in_preview);
        assert!(unchanged[0].compose_services.is_empty());
    }

    #[tokio::test]
    async fn test_create_with_foreign_project_environment_rejects_without_inserting_secret() {
        let Some(test_db) = test_database().await else {
            return;
        };
        let owner = create_project(&test_db, "foreign-owner").await;
        let foreign = create_project(&test_db, "foreign-project").await;
        let foreign_environment =
            create_environment(&test_db, foreign.id, "foreign-project-environment").await;
        let service = secret_service(&test_db);

        let error = create_secret(
            &service,
            owner.id,
            vec![foreign_environment.id],
            "FOREIGN_SCOPE",
            "must-not-persist",
        )
        .await
        .expect_err("an environment owned by another project should be rejected");

        assert!(matches!(
            error,
            SecretError::EnvironmentNotFound {
                environment_id,
                project_id,
            } if environment_id == foreign_environment.id && project_id == owner.id
        ));
        assert!(service
            .list(owner.id, None)
            .await
            .expect("owner secrets should list")
            .is_empty());
    }

    #[tokio::test]
    async fn test_concurrent_create_with_overlapping_scope_allows_exactly_one() {
        let Some(test_db) = test_database().await else {
            return;
        };
        let project = create_project(&test_db, "concurrent").await;
        let environment = create_environment(&test_db, project.id, "concurrent-environment").await;
        let service = secret_service(&test_db);

        let first_service = service.clone();
        let second_service = service.clone();
        let first = tokio::spawn(async move {
            create_secret(
                &first_service,
                project.id,
                vec![environment.id],
                "RACING_KEY",
                "first",
            )
            .await
        });
        let second = tokio::spawn(async move {
            create_secret(
                &second_service,
                project.id,
                vec![environment.id],
                "RACING_KEY",
                "second",
            )
            .await
        });

        let first_result = first.await.expect("first create task should complete");
        let second_result = second.await.expect("second create task should complete");
        let results = [first_result, second_result];
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, Err(SecretError::KeyAlreadyExists { .. })))
                .count(),
            1
        );

        let visible = service
            .get_for_deploy(project.id, Some(environment.id))
            .await
            .expect("the winning secret should resolve");
        assert_eq!(visible.len(), 1);
        assert!(matches!(
            visible.get("RACING_KEY").map(String::as_str),
            Some("first" | "second")
        ));
    }

    #[tokio::test]
    async fn create_holds_environment_share_lock_until_binding_commits() {
        let Some(test_db) = test_database().await else {
            return;
        };
        let project = create_project(&test_db, "create-delete-race").await;
        let environment = create_environment(&test_db, project.id, "create-delete-race").await;
        let blocker = hold_key_scope_lock(&test_db, project.id, "LOCKED_CREATE").await;
        let service = secret_service(&test_db);
        let project_id = project.id;
        let environment_id = environment.id;
        let create = tokio::spawn(async move {
            create_secret(
                &service,
                project_id,
                vec![environment_id],
                "LOCKED_CREATE",
                "value",
            )
            .await
        });
        wait_for_key_scope_waiter(&test_db, project_id, "LOCKED_CREATE").await;
        let delete = tokio::spawn(soft_delete_environment(
            test_db.connection_arc(),
            environment_id,
        ));
        let mut delete = Box::pin(delete);
        assert!(
            tokio::time::timeout(Duration::from_millis(200), &mut delete)
                .await
                .is_err()
        );
        blocker
            .commit()
            .await
            .expect("advisory blocker should commit");
        create
            .await
            .expect("create task should complete")
            .expect("create should win");
        delete.await.expect("deletion task should complete");
    }

    #[tokio::test]
    async fn update_holds_environment_share_lock_until_binding_commits() {
        let Some(test_db) = test_database().await else {
            return;
        };
        let project = create_project(&test_db, "update-delete-race").await;
        let source = create_environment(&test_db, project.id, "update-delete-source").await;
        let target = create_environment(&test_db, project.id, "update-delete-target").await;
        let service = secret_service(&test_db);
        let secret = create_secret(
            &service,
            project.id,
            vec![source.id],
            "LOCKED_UPDATE",
            "old",
        )
        .await
        .expect("secret fixture should insert");
        let blocker = hold_key_scope_lock(&test_db, project.id, "LOCKED_UPDATE").await;
        let update_service = service.clone();
        let project_id = project.id;
        let target_id = target.id;
        let update = tokio::spawn(async move {
            update_service
                .update(
                    project_id,
                    secret.id,
                    None,
                    vec![target_id],
                    false,
                    Vec::new(),
                )
                .await
        });
        wait_for_key_scope_waiter(&test_db, project_id, "LOCKED_UPDATE").await;
        let delete = tokio::spawn(soft_delete_environment(test_db.connection_arc(), target_id));
        let mut delete = Box::pin(delete);
        assert!(
            tokio::time::timeout(Duration::from_millis(200), &mut delete)
                .await
                .is_err()
        );
        blocker
            .commit()
            .await
            .expect("advisory blocker should commit");
        update
            .await
            .expect("update task should complete")
            .expect("update should win");
        delete.await.expect("deletion task should complete");
    }

    #[tokio::test]
    async fn deletion_committing_first_makes_create_reject_environment() {
        let Some(test_db) = test_database().await else {
            return;
        };
        let project = create_project(&test_db, "delete-create-race").await;
        let environment = create_environment(&test_db, project.id, "delete-create-race").await;
        let deletion = test_db
            .connection()
            .begin()
            .await
            .expect("deletion transaction should begin");
        deletion
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE environments SET deleted_at = NOW() WHERE id = $1",
                [environment.id.into()],
            ))
            .await
            .expect("environment should be fenced");
        let service = secret_service(&test_db);
        let project_id = project.id;
        let environment_id = environment.id;
        let create = tokio::spawn(async move {
            create_secret(
                &service,
                project_id,
                vec![environment_id],
                "DELETE_FIRST",
                "value",
            )
            .await
        });
        tokio::task::yield_now().await;
        deletion.commit().await.expect("deletion should commit");
        assert!(matches!(create.await.expect("create task should complete"),
            Err(SecretError::EnvironmentNotFound { environment_id: id, .. }) if id == environment_id));
    }

    #[tokio::test]
    async fn deletion_committing_first_makes_update_reject_environment() {
        let Some(test_db) = test_database().await else {
            return;
        };
        let project = create_project(&test_db, "delete-update-race").await;
        let source = create_environment(&test_db, project.id, "delete-update-source").await;
        let target = create_environment(&test_db, project.id, "delete-update-target").await;
        let service = secret_service(&test_db);
        let secret = create_secret(
            &service,
            project.id,
            vec![source.id],
            "DELETE_UPDATE",
            "value",
        )
        .await
        .expect("secret fixture should insert");
        let deletion = test_db
            .connection()
            .begin()
            .await
            .expect("deletion transaction should begin");
        deletion
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE environments SET deleted_at = NOW() WHERE id = $1",
                [target.id.into()],
            ))
            .await
            .expect("environment should be fenced");
        let project_id = project.id;
        let target_id = target.id;
        let update = tokio::spawn(async move {
            service
                .update(
                    project_id,
                    secret.id,
                    None,
                    vec![target_id],
                    false,
                    Vec::new(),
                )
                .await
        });
        tokio::task::yield_now().await;
        deletion.commit().await.expect("deletion should commit");
        assert!(matches!(update.await.expect("update task should complete"),
            Err(SecretError::EnvironmentNotFound { environment_id: id, .. }) if id == target_id));
    }
}
