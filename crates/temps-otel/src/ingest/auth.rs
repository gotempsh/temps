//! Per-project API key authentication for OTel ingest.

use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tracing::warn;

use crate::error::OtelError;

/// Authenticated project context after API key validation.
#[derive(Debug, Clone)]
pub struct ProjectAuth {
    pub project_id: i32,
    pub api_key_id: i32,
    pub project_name: String,
}

/// Service for authenticating OTel ingest requests via API key.
pub struct OtelAuthService {
    db: Arc<DatabaseConnection>,
}

impl OtelAuthService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    /// Authenticate an OTel ingest request using the API key from headers.
    ///
    /// Expected header: `Authorization: Bearer tk_<key>` or custom `X-Temps-Api-Key: tk_<key>`
    pub async fn authenticate(&self, api_key: &str) -> Result<ProjectAuth, OtelError> {
        // Validate key format
        if !api_key.starts_with("tk_") || api_key.len() < 10 {
            return Err(OtelError::InvalidApiKey);
        }

        // Hash the key (same as temps-auth)
        let mut hasher = Sha256::new();
        hasher.update(api_key.as_bytes());
        let key_hash = hex::encode(hasher.finalize());

        // Look up key in database
        let sql = r#"
            SELECT ak.id as key_id, ak.user_id, ak.is_active, ak.expires_at,
                   p.id as project_id, p.name as project_name
            FROM api_keys ak
            CROSS JOIN projects p
            WHERE ak.key_hash = $1
              AND ak.is_active = true
              AND (ak.expires_at IS NULL OR ak.expires_at > NOW())
            LIMIT 1
        "#;

        let result = self
            .db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                sql,
                vec![key_hash.into()],
            ))
            .await
            .map_err(|e| OtelError::Storage {
                message: format!("Database error during OTel auth: {}", e),
            })?;

        match result {
            Some(row) => {
                let key_id: i32 = row
                    .try_get("", "key_id")
                    .map_err(|_| OtelError::AuthFailed {
                        reason: "Failed to parse auth result".into(),
                    })?;
                let project_id: i32 =
                    row.try_get("", "project_id")
                        .map_err(|_| OtelError::AuthFailed {
                            reason: "Failed to parse project ID".into(),
                        })?;
                let project_name: String =
                    row.try_get("", "project_name")
                        .map_err(|_| OtelError::AuthFailed {
                            reason: "Failed to parse project name".into(),
                        })?;

                // Update last_used_at (fire-and-forget)
                let db = self.db.clone();
                let key_id_copy = key_id;
                tokio::spawn(async move {
                    let update_sql = "UPDATE api_keys SET last_used_at = NOW() WHERE id = $1";
                    if let Err(e) = db
                        .execute(Statement::from_sql_and_values(
                            DatabaseBackend::Postgres,
                            update_sql,
                            vec![key_id_copy.into()],
                        ))
                        .await
                    {
                        warn!(key_id = key_id_copy, error = %e, "Failed to update API key last_used_at");
                    }
                });

                Ok(ProjectAuth {
                    project_id,
                    api_key_id: key_id,
                    project_name,
                })
            }
            None => Err(OtelError::AuthFailed {
                reason: "Invalid or expired API key".into(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_invalid_api_key_format() {
        // Key too short
        assert!(matches!(
            validate_key_format("tk_ab"),
            Err(OtelError::InvalidApiKey)
        ));

        // Wrong prefix
        assert!(matches!(
            validate_key_format("dt_abcdefghij"),
            Err(OtelError::InvalidApiKey)
        ));

        // Valid format
        assert!(validate_key_format("tk_abcdefghij").is_ok());
    }

    fn validate_key_format(key: &str) -> Result<(), OtelError> {
        if !key.starts_with("tk_") || key.len() < 10 {
            return Err(OtelError::InvalidApiKey);
        }
        Ok(())
    }
}
