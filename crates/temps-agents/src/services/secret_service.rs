// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, DatabaseConnection, EntityTrait, QueryFilter,
    QueryOrder, Set,
};
use std::collections::HashMap;
use std::sync::Arc;

use temps_core::EncryptionService;
use temps_entities::{agent_secrets, projects};

use crate::error::AgentError;

/// The type of secret determines how it's injected into the sandbox.
#[derive(Debug, Clone, PartialEq)]
pub enum SecretType {
    /// Injected as an environment variable.
    Env,
    /// Written to a file at `mount_path`.
    File,
}

impl SecretType {
    fn as_str(&self) -> &str {
        match self {
            SecretType::Env => "env",
            SecretType::File => "file",
        }
    }

    fn from_str(s: &str) -> Self {
        match s {
            "file" => SecretType::File,
            _ => SecretType::Env,
        }
    }
}

/// A resolved secret ready for injection into a sandbox.
pub struct ResolvedSecret {
    pub name: String,
    pub secret_type: SecretType,
    pub value: String,
    pub mount_path: Option<String>,
}

// Custom Debug impl never prints `value` — defense against accidental leaks
// via `{:?}` formatting in logs or error paths.
impl std::fmt::Debug for ResolvedSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedSecret")
            .field("name", &self.name)
            .field("secret_type", &self.secret_type)
            .field("value", &"<redacted>")
            .field("mount_path", &self.mount_path)
            .finish()
    }
}

pub struct SecretService {
    db: Arc<DatabaseConnection>,
    encryption_service: Arc<EncryptionService>,
}

impl SecretService {
    pub fn new(db: Arc<DatabaseConnection>, encryption_service: Arc<EncryptionService>) -> Self {
        Self {
            db,
            encryption_service,
        }
    }

    /// Create or update a secret in `project_id`'s scope (`None` =
    /// instance-wide, injected into every project's runs).
    pub async fn upsert_secret(
        &self,
        name: &str,
        project_id: Option<i32>,
        secret_type: SecretType,
        value: &str,
        mount_path: Option<&str>,
        description: Option<&str>,
    ) -> Result<agent_secrets::Model, AgentError> {
        if name.is_empty() {
            return Err(AgentError::Validation {
                message: "Secret name cannot be empty".to_string(),
            });
        }
        if value.is_empty() {
            return Err(AgentError::Validation {
                message: format!("Secret '{}' value cannot be empty", name),
            });
        }
        if secret_type == SecretType::File && mount_path.is_none() {
            return Err(AgentError::Validation {
                message: format!(
                    "Secret '{}' is type 'file' but no mount_path provided",
                    name
                ),
            });
        }

        if let Some(project_id) = project_id {
            projects::Entity::find_by_id(project_id)
                .one(self.db.as_ref())
                .await
                .map_err(AgentError::Database)?
                .ok_or(AgentError::ProjectNotFound { project_id })?;
        }

        let encrypted = self.encryption_service.encrypt_string(value).map_err(|e| {
            AgentError::EncryptionError {
                message: format!("Failed to encrypt secret '{}': {}", name, e),
            }
        })?;

        // Check if the secret already exists in this scope
        let existing = agent_secrets::Entity::find()
            .filter(agent_secrets::Column::Name.eq(name))
            .filter(scope_condition(project_id))
            .one(self.db.as_ref())
            .await
            .map_err(AgentError::Database)?;

        if let Some(existing_model) = existing {
            let mut active: agent_secrets::ActiveModel = existing_model.into();
            active.secret_type = Set(secret_type.as_str().to_string());
            active.encrypted_value = Set(encrypted);
            active.mount_path = Set(mount_path.map(|s| s.to_string()));
            active.description = Set(description.map(|s| s.to_string()));
            active
                .update(self.db.as_ref())
                .await
                .map_err(AgentError::Database)
        } else {
            let active = agent_secrets::ActiveModel {
                project_id: Set(project_id),
                name: Set(name.to_string()),
                secret_type: Set(secret_type.as_str().to_string()),
                encrypted_value: Set(encrypted),
                mount_path: Set(mount_path.map(|s| s.to_string())),
                description: Set(description.map(|s| s.to_string())),
                ..Default::default()
            };
            active
                .insert(self.db.as_ref())
                .await
                .map_err(AgentError::Database)
        }
    }

    /// List every secret across all scopes (metadata only, no decrypted
    /// values). Operator view: instance-wide secrets first, then by project.
    pub async fn list_secrets(&self) -> Result<Vec<agent_secrets::Model>, AgentError> {
        agent_secrets::Entity::find()
            .order_by_asc(agent_secrets::Column::ProjectId)
            .order_by_asc(agent_secrets::Column::Name)
            .all(self.db.as_ref())
            .await
            .map_err(AgentError::Database)
    }

    /// Secrets visible to `project_id`'s agent runs: its own plus the
    /// instance-wide ones. Never another project's.
    pub async fn list_secrets_for_project(
        &self,
        project_id: i32,
    ) -> Result<Vec<agent_secrets::Model>, AgentError> {
        agent_secrets::Entity::find()
            .filter(
                Condition::any()
                    .add(agent_secrets::Column::ProjectId.eq(project_id))
                    .add(agent_secrets::Column::ProjectId.is_null()),
            )
            .all(self.db.as_ref())
            .await
            .map_err(AgentError::Database)
    }

    /// Delete the secret called `name` in `project_id`'s scope (`None` =
    /// instance-wide).
    pub async fn delete_secret(
        &self,
        name: &str,
        project_id: Option<i32>,
    ) -> Result<(), AgentError> {
        let secret = agent_secrets::Entity::find()
            .filter(agent_secrets::Column::Name.eq(name))
            .filter(scope_condition(project_id))
            .one(self.db.as_ref())
            .await
            .map_err(AgentError::Database)?
            .ok_or_else(|| AgentError::SecretNotFound {
                name: name.to_string(),
                scope: scope_label(project_id),
            })?;

        let active: agent_secrets::ActiveModel = secret.into();
        active
            .delete(self.db.as_ref())
            .await
            .map_err(AgentError::Database)?;
        Ok(())
    }

    /// Decrypt the secrets an agent run of `project_id` may receive. A
    /// project-scoped secret shadows an instance-wide one of the same name.
    pub async fn resolve_secrets(
        &self,
        project_id: i32,
    ) -> Result<Vec<ResolvedSecret>, AgentError> {
        let secrets = select_secrets_for_project(self.list_secrets_for_project(project_id).await?);
        let mut resolved = Vec::with_capacity(secrets.len());

        for secret in secrets {
            let value = self
                .encryption_service
                .decrypt_string(&secret.encrypted_value)
                .map_err(|e| AgentError::EncryptionError {
                    message: format!(
                        "Failed to decrypt secret '{}' ({}): {}",
                        secret.name,
                        scope_label(secret.project_id),
                        e
                    ),
                })?;
            resolved.push(ResolvedSecret {
                name: secret.name,
                secret_type: SecretType::from_str(&secret.secret_type),
                value,
                mount_path: secret.mount_path,
            });
        }

        Ok(resolved)
    }

    /// Resolve `${TEMPS_SECRET:name}` placeholders in a string.
    /// Returns the string with all placeholders replaced by decrypted values.
    pub fn resolve_placeholders(content: &str, secrets: &HashMap<String, String>) -> String {
        let mut result = content.to_string();
        for (name, value) in secrets {
            let placeholder = format!("${{TEMPS_SECRET:{}}}", name);
            result = result.replace(&placeholder, value);
        }
        result
    }
}

/// Filter matching exactly one scope: a project, or the instance-wide rows.
fn scope_condition(project_id: Option<i32>) -> Condition {
    match project_id {
        Some(project_id) => Condition::all().add(agent_secrets::Column::ProjectId.eq(project_id)),
        None => Condition::all().add(agent_secrets::Column::ProjectId.is_null()),
    }
}

fn scope_label(project_id: Option<i32>) -> String {
    match project_id {
        Some(project_id) => format!("project {}", project_id),
        None => "instance-wide scope".to_string(),
    }
}

/// Collapse a project's visible secrets to one per name, preferring the
/// project-scoped row over the instance-wide one.
fn select_secrets_for_project(rows: Vec<agent_secrets::Model>) -> Vec<agent_secrets::Model> {
    let mut by_name: HashMap<String, agent_secrets::Model> = HashMap::with_capacity(rows.len());
    for row in rows {
        match by_name.get(&row.name) {
            Some(current) if current.project_id.is_some() && row.project_id.is_none() => {}
            _ => {
                by_name.insert(row.name.clone(), row);
            }
        }
    }
    let mut selected: Vec<_> = by_name.into_values().collect();
    selected.sort_by(|a, b| a.name.cmp(&b.name));
    selected
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};

    fn make_encryption_service() -> Arc<EncryptionService> {
        Arc::new(
            EncryptionService::new(
                "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20",
            )
            .expect("valid test key"),
        )
    }

    #[tokio::test]
    async fn test_upsert_secret_validates_empty_name() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let svc = SecretService::new(Arc::new(db), make_encryption_service());

        let result = svc
            .upsert_secret("", None, SecretType::Env, "value", None, None)
            .await;
        assert!(matches!(result.unwrap_err(), AgentError::Validation { .. }));
    }

    #[tokio::test]
    async fn test_upsert_secret_validates_empty_value() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let svc = SecretService::new(Arc::new(db), make_encryption_service());

        let result = svc
            .upsert_secret("my-secret", None, SecretType::Env, "", None, None)
            .await;
        assert!(matches!(result.unwrap_err(), AgentError::Validation { .. }));
    }

    #[tokio::test]
    async fn test_upsert_secret_file_requires_mount_path() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let svc = SecretService::new(Arc::new(db), make_encryption_service());

        let result = svc
            .upsert_secret("my-cert", None, SecretType::File, "cert-data", None, None)
            .await;
        assert!(matches!(result.unwrap_err(), AgentError::Validation { .. }));
    }

    #[tokio::test]
    async fn test_delete_secret_not_found() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<agent_secrets::Model>::new()])
            .into_connection();
        let svc = SecretService::new(Arc::new(db), make_encryption_service());

        let result = svc.delete_secret("nonexistent", Some(4)).await;
        assert!(matches!(
            result.unwrap_err(),
            AgentError::SecretNotFound {
                name,
                scope,
            } if name == "nonexistent" && scope == "project 4"
        ));
    }

    #[test]
    fn test_resolve_placeholders() {
        let mut secrets = HashMap::new();
        secrets.insert("api_key".to_string(), "sk-12345".to_string());
        secrets.insert("db_url".to_string(), "postgres://localhost".to_string());

        let content = r#"{
            "env": {
                "API_KEY": "${TEMPS_SECRET:api_key}",
                "DB_URL": "${TEMPS_SECRET:db_url}",
                "PLAIN": "no-secret-here"
            }
        }"#;

        let result = SecretService::resolve_placeholders(content, &secrets);
        assert!(result.contains("sk-12345"));
        assert!(result.contains("postgres://localhost"));
        assert!(result.contains("no-secret-here"));
        assert!(!result.contains("TEMPS_SECRET"));
    }

    #[test]
    fn test_resolve_placeholders_missing_secret_left_as_is() {
        let secrets = HashMap::new();
        let content = "${TEMPS_SECRET:missing}";
        let result = SecretService::resolve_placeholders(content, &secrets);
        assert_eq!(result, "${TEMPS_SECRET:missing}");
    }

    #[test]
    fn test_secret_type_roundtrip() {
        assert_eq!(
            SecretType::from_str(SecretType::Env.as_str()),
            SecretType::Env
        );
        assert_eq!(
            SecretType::from_str(SecretType::File.as_str()),
            SecretType::File
        );
        assert_eq!(SecretType::from_str("unknown"), SecretType::Env);
    }

    fn secret_row(id: i32, project_id: Option<i32>, name: &str) -> agent_secrets::Model {
        let now = chrono::Utc::now();
        agent_secrets::Model {
            id,
            project_id,
            name: name.to_string(),
            secret_type: "env".to_string(),
            encrypted_value: format!("encrypted-{id}"),
            mount_path: None,
            description: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn project_scoped_secret_shadows_instance_wide_secret_of_same_name() {
        // Order must not matter: the project row wins either way.
        for rows in [
            vec![
                secret_row(1, None, "TOKEN"),
                secret_row(2, Some(7), "TOKEN"),
            ],
            vec![
                secret_row(2, Some(7), "TOKEN"),
                secret_row(1, None, "TOKEN"),
            ],
        ] {
            let selected = select_secrets_for_project(rows);
            assert_eq!(selected.len(), 1);
            assert_eq!(selected[0].id, 2);
        }
    }
}

/// F1 regression: agent secrets are resolved per project, so a member who can
/// run an agent in one project never receives another project's secrets.
#[cfg(test)]
mod integration_tests {
    use super::*;
    use sea_orm::{ActiveModelTrait, Set};
    use temps_database::test_utils::{is_container_runtime_unavailable, TestDatabase};

    async fn test_database() -> Option<TestDatabase> {
        match TestDatabase::with_migrations().await {
            Ok(database) => Some(database),
            Err(error) if is_container_runtime_unavailable(&error.to_string()) => {
                eprintln!("Docker not available, skipping agent secret scope test: {error}");
                None
            }
            Err(error) => panic!("failed to start the agent secret test database: {error}"),
        }
    }

    fn service(test_db: &TestDatabase) -> SecretService {
        SecretService::new(
            test_db.connection_arc(),
            Arc::new(
                EncryptionService::new(
                    "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20",
                )
                .expect("valid test key"),
            ),
        )
    }

    async fn create_project(test_db: &TestDatabase, slug: &str) -> i32 {
        projects::ActiveModel {
            name: Set(slug.to_string()),
            slug: Set(slug.to_string()),
            repo_name: Set(slug.to_string()),
            repo_owner: Set("owner".to_string()),
            directory: Set(".".to_string()),
            main_branch: Set("main".to_string()),
            preset: Set(temps_entities::preset::Preset::NextJs),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            ..Default::default()
        }
        .insert(test_db.connection())
        .await
        .expect("insert project")
        .id
    }

    fn resolved_names(secrets: &[ResolvedSecret]) -> Vec<(String, String)> {
        let mut names: Vec<_> = secrets
            .iter()
            .map(|s| (s.name.clone(), s.value.clone()))
            .collect();
        names.sort();
        names
    }

    #[tokio::test]
    async fn runs_only_receive_their_own_project_and_instance_wide_secrets() {
        let Some(test_db) = test_database().await else {
            return;
        };
        let svc = service(&test_db);
        let project_a = create_project(&test_db, "secrets-scope-a").await;
        let project_b = create_project(&test_db, "secrets-scope-b").await;

        svc.upsert_secret("SHARED", None, SecretType::Env, "shared-value", None, None)
            .await
            .unwrap();
        svc.upsert_secret(
            "A_ONLY",
            Some(project_a),
            SecretType::Env,
            "a-value",
            None,
            None,
        )
        .await
        .unwrap();
        svc.upsert_secret(
            "SHARED",
            Some(project_b),
            SecretType::Env,
            "b-override",
            None,
            None,
        )
        .await
        .unwrap();

        let for_a = svc.resolve_secrets(project_a).await.unwrap();
        assert_eq!(
            resolved_names(&for_a),
            vec![
                ("A_ONLY".to_string(), "a-value".to_string()),
                ("SHARED".to_string(), "shared-value".to_string()),
            ]
        );

        // Project B never sees A's secret, and its own SHARED shadows the
        // instance-wide one.
        let for_b = svc.resolve_secrets(project_b).await.unwrap();
        assert_eq!(
            resolved_names(&for_b),
            vec![("SHARED".to_string(), "b-override".to_string())]
        );

        // Deleting B's override is scoped: the instance-wide row survives.
        svc.delete_secret("SHARED", Some(project_b)).await.unwrap();
        let for_b = svc.resolve_secrets(project_b).await.unwrap();
        assert_eq!(
            resolved_names(&for_b),
            vec![("SHARED".to_string(), "shared-value".to_string())]
        );
    }

    #[tokio::test]
    async fn upsert_into_missing_project_is_rejected() {
        let Some(test_db) = test_database().await else {
            return;
        };
        let err = service(&test_db)
            .upsert_secret("TOKEN", Some(987_654), SecretType::Env, "v", None, None)
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            AgentError::ProjectNotFound {
                project_id: 987_654
            }
        ));
    }
}
