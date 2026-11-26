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
    sea_query::LockType, ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseBackend,
    DatabaseTransaction, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, Set,
    Statement, TransactionTrait,
};
use std::collections::HashMap;
use std::sync::Arc;
use temps_core::{EncryptionService, PageQuery};
use temps_entities::{
    environments, secret_compose_services, secret_environments, secret_history, secrets,
};
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

/// Who can read a secret matters as much as its value. Scope lives in junction
/// tables that updates rewrite wholesale, so a trigger would see a delete and
/// re-insert on every save; the change is detected here instead. Returns the
/// `secret_history` details when the effective scope changed.
fn scope_change(
    mut previous_environments: Vec<i32>,
    mut previous_services: Vec<String>,
    environments: &[i32],
    services: &[String],
) -> Option<serde_json::Value> {
    let mut environments = environments.to_vec();
    let mut services = services.to_vec();
    for list in [&mut previous_environments, &mut environments] {
        list.sort_unstable();
        list.dedup();
    }
    for list in [&mut previous_services, &mut services] {
        list.sort_unstable();
        list.dedup();
    }
    if previous_environments == environments && previous_services == services {
        return None;
    }
    Some(serde_json::json!({
        "environment_ids": environments,
        "compose_services": services,
    }))
}

/// Longest part of a refused key or service name repeated in an error. The
/// input itself can be up to the request body limit, and an error must not
/// echo all of it back.
const MAX_ECHOED_CHARS: usize = 64;

/// `value` as an error repeats it: whole when short, otherwise its first
/// [`MAX_ECHOED_CHARS`] characters followed by `…`.
fn echoed(value: &str) -> String {
    if value.chars().count() <= MAX_ECHOED_CHARS {
        return value.to_string();
    }
    let mut shown: String = value.chars().take(MAX_ECHOED_CHARS).collect();
    shown.push('…');
    shown
}

/// Validates a secret key. Keys become file names under `/run/secrets/` and
/// are commonly consumed as env-var-like identifiers, so we require the same
/// conservative shape: uppercase letters, digits, and underscores; must start
/// with a letter or underscore; max 255 chars.
fn validate_secret_key(key: &str) -> Result<(), SecretError> {
    if key.is_empty() {
        return Err(SecretError::InvalidKey {
            key: echoed(key),
            reason: "key cannot be empty".to_string(),
        });
    }
    if key.len() > 255 {
        return Err(SecretError::InvalidKey {
            key: echoed(key),
            reason: format!("key length {} exceeds 255", key.len()),
        });
    }
    let mut chars = key.chars();
    let Some(first) = chars.next() else {
        return Err(SecretError::InvalidKey {
            key: echoed(key),
            reason: "key cannot be empty".to_string(),
        });
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return Err(SecretError::InvalidKey {
            key: echoed(key),
            reason: "key must start with a letter or underscore".to_string(),
        });
    }
    for c in chars {
        if !(c.is_ascii_alphanumeric() || c == '_') {
            return Err(SecretError::InvalidKey {
                key: echoed(key),
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
            service: echoed(name),
            reason: "service name cannot be empty".to_string(),
        });
    }
    if name.len() > 255 {
        return Err(SecretError::InvalidComposeService {
            service: echoed(name),
            reason: format!("service name length {} exceeds 255", name.len()),
        });
    }
    if name == "." || name == ".." {
        return Err(SecretError::InvalidComposeService {
            service: echoed(name),
            reason: "service name cannot be '.' or '..'".to_string(),
        });
    }
    if let Some(c) = name
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || *c == '.' || *c == '_' || *c == '-'))
    {
        return Err(SecretError::InvalidComposeService {
            service: echoed(name),
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

    /// Soft-deleted environments of `project_id` that `secret_id` is still
    /// bound to, ordered by id.
    ///
    /// A deleted environment is restored when one is created again for the
    /// same branch, and keeps its secret bindings so it gets its secrets
    /// back. Updates leave these bindings in place rather than replacing
    /// them: a secret with no bindings is mounted in every environment, so
    /// dropping them would widen a secret scoped only to deleted environments
    /// to every environment, production included.
    async fn bound_deleted_environments(
        txn: &DatabaseTransaction,
        project_id: i32,
        secret_id: i32,
    ) -> Result<Vec<environments::Model>, SecretError> {
        let bindings = secret_environments::Entity::find()
            .filter(secret_environments::Column::SecretId.eq(secret_id))
            .find_also_related(environments::Entity)
            .filter(environments::Column::ProjectId.eq(project_id))
            .filter(environments::Column::DeletedAt.is_not_null())
            .order_by_asc(environments::Column::Id)
            .all(txn)
            .await?;
        Ok(bindings
            .into_iter()
            .filter_map(|(_, environment)| environment)
            .collect())
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

    /// One page of the secrets visible to a project, newest first, optionally
    /// only those attached to `environment_id` (and then reporting only that
    /// environment), with the number of matching secrets.
    ///
    /// Values are NOT decrypted — callers that render to the UI must mask the
    /// value. Use `get_for_deploy` when plaintext is required.
    pub async fn list_page(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
        page: PageQuery,
    ) -> Result<(Vec<SecretWithEnvironments>, u64), SecretError> {
        let mut query = secrets::Entity::find().filter(secrets::Column::ProjectId.eq(project_id));
        if let Some(env_id) = environment_id {
            query = query.filter(
                secrets::Column::Id.in_subquery(
                    sea_orm::sea_query::Query::select()
                        .column(secret_environments::Column::SecretId)
                        .from(secret_environments::Entity)
                        .and_where(secret_environments::Column::EnvironmentId.eq(env_id))
                        .to_owned(),
                ),
            );
        }
        let (_, page_size) = page.normalize();
        let paginator = query
            .order_by_desc(secrets::Column::CreatedAt)
            .order_by_desc(secrets::Column::Id)
            .paginate(self.db.as_ref(), page_size);
        let total = paginator.num_items().await?;
        let rows = paginator.fetch_page(page.page_index()).await?;
        Ok((self.with_environments(rows, environment_id).await?, total))
    }

    /// One secret of a project (metadata only, never the value).
    pub async fn get(
        &self,
        project_id: i32,
        secret_id: i32,
    ) -> Result<SecretWithEnvironments, SecretError> {
        let not_found = || SecretError::NotFound {
            secret_id,
            project_id,
        };
        let row = secrets::Entity::find_by_id(secret_id)
            .filter(secrets::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(not_found)?;
        self.with_environments(vec![row], None)
            .await?
            .into_iter()
            .next()
            .ok_or_else(not_found)
    }

    /// Attaches environments and compose-service scopes to `rows`, in two
    /// queries whatever the number of rows. With `environment_id`, only that
    /// environment is reported.
    async fn with_environments(
        &self,
        rows: Vec<secrets::Model>,
        environment_id: Option<i32>,
    ) -> Result<Vec<SecretWithEnvironments>, SecretError> {
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

        Ok(rows
            .into_iter()
            .map(|row| SecretWithEnvironments {
                environments: env_map.remove(&row.id).unwrap_or_default(),
                compose_services: service_map.remove(&row.id).unwrap_or_default(),
                id: row.id,
                project_id: row.project_id,
                key: row.key,
                include_in_preview: row.include_in_preview,
                created_at: row.created_at,
                updated_at: row.updated_at,
            })
            .collect())
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

    /// Updates a secret's value and/or scope. Key is immutable here —
    /// rotating a secret keeps the same key so consumers don't need config
    /// changes.
    ///
    /// `None` keeps the stored value of that field: the ciphertext, the
    /// environments, the preview flag or the compose services. Only a field
    /// that is given replaces what is stored, so rotating a value never
    /// changes who can read it. `Some(vec![])` for `environment_ids` scopes
    /// the secret to every environment of the project, and for
    /// `compose_services` to every service, as on create.
    pub async fn update(
        &self,
        project_id: i32,
        secret_id: i32,
        new_value: Option<String>,
        environment_ids: Option<Vec<i32>>,
        include_in_preview: Option<bool>,
        compose_services: Option<Vec<String>>,
    ) -> Result<SecretWithEnvironments, SecretError> {
        let compose_services = compose_services
            .map(normalize_compose_services)
            .transpose()?;
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
                    let row = Self::lock_for_write(txn, project_id, secret_id).await?;

                    let previous_bindings = secret_environments::Entity::find()
                        .filter(secret_environments::Column::SecretId.eq(secret_id))
                        .find_also_related(environments::Entity)
                        .order_by_asc(secret_environments::Column::EnvironmentId)
                        .all(txn)
                        .await?;
                    let previous_environments = previous_bindings
                        .iter()
                        .map(|(junction, _)| junction.environment_id)
                        .collect::<Vec<_>>();
                    let previous_services = secret_compose_services::Entity::find()
                        .filter(secret_compose_services::Column::SecretId.eq(secret_id))
                        .order_by_asc(secret_compose_services::Column::ServiceName)
                        .all(txn)
                        .await?
                        .into_iter()
                        .map(|junction| junction.service_name)
                        .collect::<Vec<_>>();

                    let (envs, final_environment_ids) = match environment_ids {
                        Some(environment_ids) => {
                            Self::replace_environments(
                                txn,
                                project_id,
                                secret_id,
                                &row.key,
                                environment_ids,
                            )
                            .await?
                        }
                        // The scope is not part of the update: keep every
                        // binding as it is.
                        None => (
                            previous_bindings
                                .into_iter()
                                .filter_map(|(_, environment)| environment)
                                .map(|env| SecretEnvironmentRef {
                                    id: env.id,
                                    name: env.name,
                                    main_url: env.subdomain,
                                })
                                .collect::<Vec<_>>(),
                            previous_environments.clone(),
                        ),
                    };

                    let mut active: secrets::ActiveModel = row.into();
                    if let Some(v) = encrypted_new {
                        active.value = Set(v);
                    }
                    if let Some(include_in_preview) = include_in_preview {
                        active.include_in_preview = Set(include_in_preview);
                    }
                    active.updated_at = Set(chrono::Utc::now());
                    let row = active.update(txn).await?;

                    let compose_services = match compose_services {
                        Some(compose_services) => {
                            // Replace the scope wholesale, same as
                            // environments: a list that leaves a service out
                            // removes that service's access.
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
                            compose_services
                        }
                        None => previous_services.clone(),
                    };

                    if let Some(details) = scope_change(
                        previous_environments,
                        previous_services,
                        &final_environment_ids,
                        &compose_services,
                    ) {
                        secret_history::ActiveModel {
                            project_id: Set(project_id),
                            secret_id: Set(secret_id),
                            kind: Set("scope_changed".into()),
                            details: Set(details),
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

    /// Replaces a secret's environment bindings with `environment_ids` and
    /// returns the environments it is bound to afterwards, with their ids.
    async fn replace_environments(
        txn: &DatabaseTransaction,
        project_id: i32,
        secret_id: i32,
        key: &str,
        environment_ids: Vec<i32>,
    ) -> Result<(Vec<SecretEnvironmentRef>, Vec<i32>), SecretError> {
        // Bindings to deleted environments survive the update (see
        // `bound_deleted_environments`). Their ids may come back from a
        // client that echoes the secret's current environments; treat them
        // as "keep as-is". Any other deleted or foreign id is still rejected
        // below.
        let preserved_environments =
            Self::bound_deleted_environments(txn, project_id, secret_id).await?;
        let preserved_ids = preserved_environments
            .iter()
            .map(|environment| environment.id)
            .collect::<Vec<_>>();
        let requested_ids = environment_ids
            .into_iter()
            .filter(|environment_id| !preserved_ids.contains(environment_id))
            .collect::<Vec<_>>();
        let scoped_environments =
            Self::environments_in_project(txn, project_id, &requested_ids).await?;
        let environment_ids = scoped_environments
            .iter()
            .map(|environment| environment.id)
            .collect::<Vec<_>>();
        // The scope the secret ends up with, preserved bindings included:
        // what collisions and history are judged on.
        let final_environment_ids = environment_ids
            .iter()
            .chain(preserved_ids.iter())
            .copied()
            .collect::<Vec<_>>();
        Self::claim_key_scope(
            txn,
            project_id,
            key,
            &final_environment_ids,
            Some(secret_id),
        )
        .await?;

        let mut replaced_bindings = secret_environments::Entity::delete_many()
            .filter(secret_environments::Column::SecretId.eq(secret_id));
        if !preserved_ids.is_empty() {
            replaced_bindings = replaced_bindings.filter(
                secret_environments::Column::EnvironmentId.is_not_in(preserved_ids.iter().copied()),
            );
        }
        replaced_bindings.exec(txn).await?;

        let mut envs = preserved_environments
            .into_iter()
            .map(|env| SecretEnvironmentRef {
                id: env.id,
                name: env.name,
                main_url: env.subdomain,
            })
            .collect::<Vec<_>>();
        for (env_id, env) in environment_ids.iter().zip(scoped_environments) {
            secret_environments::ActiveModel {
                secret_id: Set(secret_id),
                environment_id: Set(*env_id),
                created_at: Set(chrono::Utc::now()),
                ..Default::default()
            }
            .insert(txn)
            .await?;

            envs.push(SecretEnvironmentRef {
                id: env.id,
                name: env.name,
                main_url: env.subdomain,
            });
        }
        Ok((envs, final_environment_ids))
    }

    /// Locks a secret for update or delete, together with the credential checks
    /// that read it. A check writing its result inserts secret history, which
    /// key-share locks the secret while holding the check row. `FOR NO KEY
    /// UPDATE` does not conflict with that key-share lock, and taking the check
    /// rows before modifying the secret (a key rename or delete needs a full row
    /// lock, and cascades or the history trigger touch the checks) keeps the
    /// lock order the same as a running check's, so the two cannot deadlock.
    async fn lock_for_write(
        txn: &DatabaseTransaction,
        project_id: i32,
        secret_id: i32,
    ) -> Result<secrets::Model, SecretError> {
        let secret = secrets::Entity::find_by_id(secret_id)
            .filter(secrets::Column::ProjectId.eq(project_id))
            .lock(LockType::NoKeyUpdate)
            .one(txn)
            .await?
            .ok_or(SecretError::NotFound {
                secret_id,
                project_id,
            })?;
        txn.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT id FROM http_checks WHERE secret_id = $1 ORDER BY id FOR UPDATE",
            [secret_id.into()],
        ))
        .await?;
        Ok(secret)
    }

    /// Deletes a secret and returns its key for audit context.
    pub async fn delete(&self, project_id: i32, secret_id: i32) -> Result<String, SecretError> {
        let key = self
            .db
            .transaction::<_, String, SecretError>(|txn| {
                Box::pin(async move {
                    let secret = Self::lock_for_write(txn, project_id, secret_id).await?;

                    secret_environments::Entity::delete_many()
                        .filter(secret_environments::Column::SecretId.eq(secret_id))
                        .exec(txn)
                        .await?;

                    let key = secret.key.clone();
                    let active: secrets::ActiveModel = secret.into();
                    active.delete(txn).await?;
                    Ok(key)
                })
            })
            .await?;
        Ok(key)
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
mod tests;

#[cfg(test)]
mod integration_tests;
