// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm::{ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, Set};
use std::collections::HashMap;
use std::sync::Arc;

use temps_core::EncryptionService;
use temps_entities::agent_secrets;

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

    /// Create or update a global secret.
    pub async fn upsert_secret(
        &self,
        name: &str,
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

        let encrypted = self.encryption_service.encrypt_string(value).map_err(|e| {
            AgentError::EncryptionError {
                message: format!("Failed to encrypt secret '{}': {}", name, e),
            }
        })?;

        // Check if secret already exists
        let existing = agent_secrets::Entity::find()
            .filter(agent_secrets::Column::Name.eq(name))
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

    /// List all global secrets (metadata only, no decrypted values).
    pub async fn list_secrets(&self) -> Result<Vec<agent_secrets::Model>, AgentError> {
        agent_secrets::Entity::find()
            .all(self.db.as_ref())
            .await
            .map_err(AgentError::Database)
    }

    /// Delete a secret by name.
    pub async fn delete_secret(&self, name: &str) -> Result<(), AgentError> {
        let secret = agent_secrets::Entity::find()
            .filter(agent_secrets::Column::Name.eq(name))
            .one(self.db.as_ref())
            .await
            .map_err(AgentError::Database)?
            .ok_or_else(|| AgentError::SecretNotFound {
                name: name.to_string(),
            })?;

        let active: agent_secrets::ActiveModel = secret.into();
        active
            .delete(self.db.as_ref())
            .await
            .map_err(AgentError::Database)?;
        Ok(())
    }

    /// Resolve all secrets: decrypt and return for injection into sandboxes.
    pub async fn resolve_secrets(&self) -> Result<Vec<ResolvedSecret>, AgentError> {
        let secrets = self.list_secrets().await?;
        let mut resolved = Vec::with_capacity(secrets.len());

        for secret in secrets {
            let value = self
                .encryption_service
                .decrypt_string(&secret.encrypted_value)
                .map_err(|e| AgentError::EncryptionError {
                    message: format!("Failed to decrypt secret '{}': {}", secret.name, e),
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
            .upsert_secret("", SecretType::Env, "value", None, None)
            .await;
        assert!(matches!(result.unwrap_err(), AgentError::Validation { .. }));
    }

    #[tokio::test]
    async fn test_upsert_secret_validates_empty_value() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let svc = SecretService::new(Arc::new(db), make_encryption_service());

        let result = svc
            .upsert_secret("my-secret", SecretType::Env, "", None, None)
            .await;
        assert!(matches!(result.unwrap_err(), AgentError::Validation { .. }));
    }

    #[tokio::test]
    async fn test_upsert_secret_file_requires_mount_path() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let svc = SecretService::new(Arc::new(db), make_encryption_service());

        let result = svc
            .upsert_secret("my-cert", SecretType::File, "cert-data", None, None)
            .await;
        assert!(matches!(result.unwrap_err(), AgentError::Validation { .. }));
    }

    #[tokio::test]
    async fn test_delete_secret_not_found() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<agent_secrets::Model>::new()])
            .into_connection();
        let svc = SecretService::new(Arc::new(db), make_encryption_service());

        let result = svc.delete_secret("nonexistent").await;
        assert!(matches!(
            result.unwrap_err(),
            AgentError::SecretNotFound {
                name,
            } if name == "nonexistent"
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
}
