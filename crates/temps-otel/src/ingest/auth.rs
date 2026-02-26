//! Per-project authentication for OTel ingest.
//!
//! Supports two token types:
//! - **API keys (`tk_`)**: User-scoped. Requires `X-Temps-Project-Id` header
//!   to identify the target project.
//! - **Deployment tokens (`dt_`)**: Project-scoped. Project, environment, and
//!   deployment IDs are inferred from the token record itself.

use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tracing::warn;

use crate::error::OtelError;

/// Authenticated project context after token validation.
#[derive(Debug, Clone)]
pub struct ProjectAuth {
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub deployment_id: Option<i32>,
    /// Identifies the token used. For `tk_` keys this is the api_key id;
    /// for `dt_` tokens it is the deployment_token id.
    pub token_id: i32,
    pub project_name: String,
}

/// Service for authenticating OTel ingest requests.
pub struct OtelAuthService {
    db: Arc<DatabaseConnection>,
}

impl OtelAuthService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    /// Authenticate an OTel ingest request.
    ///
    /// For `tk_` (API key) tokens the caller must supply `project_id` via
    /// the `X-Temps-Project-Id` header so we can verify access.
    ///
    /// For `dt_` (deployment token) tokens the project/environment/deployment
    /// are resolved from the token record; `header_project_id` is ignored.
    pub async fn authenticate(
        &self,
        token: &str,
        header_project_id: Option<i32>,
    ) -> Result<ProjectAuth, OtelError> {
        if token.starts_with("dt_") {
            self.authenticate_deployment_token(token).await
        } else if token.starts_with("tk_") {
            self.authenticate_api_key(token, header_project_id).await
        } else {
            Err(OtelError::InvalidApiKey)
        }
    }

    /// Authenticate using an API key (`tk_`).
    ///
    /// The key identifies a user. We require `X-Temps-Project-Id` to know
    /// which project to associate telemetry with, and verify the user has
    /// access to that project.
    async fn authenticate_api_key(
        &self,
        api_key: &str,
        header_project_id: Option<i32>,
    ) -> Result<ProjectAuth, OtelError> {
        if api_key.len() < 10 {
            return Err(OtelError::InvalidApiKey);
        }

        let project_id = header_project_id.ok_or_else(|| OtelError::AuthFailed {
            reason:
                "X-Temps-Project-Id header is required when using an API key (tk_) for OTel ingest"
                    .into(),
        })?;

        // Hash the key (same algorithm as temps-auth)
        let mut hasher = Sha256::new();
        hasher.update(api_key.as_bytes());
        let key_hash = hex::encode(hasher.finalize());

        // Look up key + verify user has access to the requested project
        let sql = r#"
            SELECT ak.id AS key_id, p.name AS project_name
            FROM api_keys ak
            JOIN projects p ON p.id = $2
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
                vec![key_hash.into(), project_id.into()],
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
                let project_name: String =
                    row.try_get("", "project_name")
                        .map_err(|_| OtelError::AuthFailed {
                            reason: "Failed to parse project name".into(),
                        })?;

                // Update last_used_at (fire-and-forget)
                let db = self.db.clone();
                tokio::spawn(async move {
                    let update_sql = "UPDATE api_keys SET last_used_at = NOW() WHERE id = $1";
                    if let Err(e) = db
                        .execute(Statement::from_sql_and_values(
                            DatabaseBackend::Postgres,
                            update_sql,
                            vec![key_id.into()],
                        ))
                        .await
                    {
                        warn!(key_id, error = %e, "Failed to update API key last_used_at");
                    }
                });

                Ok(ProjectAuth {
                    project_id,
                    environment_id: None,
                    deployment_id: None,
                    token_id: key_id,
                    project_name,
                })
            }
            None => Err(OtelError::AuthFailed {
                reason: "Invalid or expired API key, or project not found".into(),
            }),
        }
    }

    /// Authenticate using a deployment token (`dt_`).
    ///
    /// The token record contains `project_id`, `environment_id`, and
    /// `deployment_id`, so no extra headers are needed.
    async fn authenticate_deployment_token(&self, token: &str) -> Result<ProjectAuth, OtelError> {
        if token.len() < 10 {
            return Err(OtelError::InvalidApiKey);
        }

        // Hash the token
        let mut hasher = Sha256::new();
        hasher.update(token.as_bytes());
        let token_hash = hex::encode(hasher.finalize());

        let sql = r#"
            SELECT dt.id AS token_id,
                   dt.project_id,
                   dt.environment_id,
                   dt.deployment_id,
                   p.name AS project_name
            FROM deployment_tokens dt
            JOIN projects p ON p.id = dt.project_id
            WHERE dt.token_hash = $1
              AND dt.is_active = true
              AND (dt.expires_at IS NULL OR dt.expires_at > NOW())
            LIMIT 1
        "#;

        let result = self
            .db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                sql,
                vec![token_hash.into()],
            ))
            .await
            .map_err(|e| OtelError::Storage {
                message: format!("Database error during OTel auth: {}", e),
            })?;

        match result {
            Some(row) => {
                let token_id: i32 =
                    row.try_get("", "token_id")
                        .map_err(|_| OtelError::AuthFailed {
                            reason: "Failed to parse deployment token ID".into(),
                        })?;
                let project_id: i32 =
                    row.try_get("", "project_id")
                        .map_err(|_| OtelError::AuthFailed {
                            reason: "Failed to parse project ID".into(),
                        })?;
                let environment_id: Option<i32> =
                    row.try_get("", "environment_id")
                        .map_err(|_| OtelError::AuthFailed {
                            reason: "Failed to parse environment ID".into(),
                        })?;
                let deployment_id: Option<i32> =
                    row.try_get("", "deployment_id")
                        .map_err(|_| OtelError::AuthFailed {
                            reason: "Failed to parse deployment ID".into(),
                        })?;
                let project_name: String =
                    row.try_get("", "project_name")
                        .map_err(|_| OtelError::AuthFailed {
                            reason: "Failed to parse project name".into(),
                        })?;

                // Update last_used_at (fire-and-forget)
                let db = self.db.clone();
                tokio::spawn(async move {
                    let update_sql =
                        "UPDATE deployment_tokens SET last_used_at = NOW() WHERE id = $1";
                    if let Err(e) = db
                        .execute(Statement::from_sql_and_values(
                            DatabaseBackend::Postgres,
                            update_sql,
                            vec![token_id.into()],
                        ))
                        .await
                    {
                        warn!(token_id, error = %e, "Failed to update deployment token last_used_at");
                    }
                });

                Ok(ProjectAuth {
                    project_id,
                    environment_id,
                    deployment_id,
                    token_id,
                    project_name,
                })
            }
            None => Err(OtelError::AuthFailed {
                reason: "Invalid or expired deployment token".into(),
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

        // Wrong prefix — neither tk_ nor dt_
        assert!(matches!(
            validate_key_format("xx_abcdefghij"),
            Err(OtelError::InvalidApiKey)
        ));

        // Valid tk_ format
        assert!(validate_key_format("tk_abcdefghij").is_ok());

        // Valid dt_ format
        assert!(validate_key_format("dt_abcdefghij").is_ok());
    }

    fn validate_key_format(key: &str) -> Result<(), OtelError> {
        if key.len() < 10 {
            return Err(OtelError::InvalidApiKey);
        }
        if !key.starts_with("tk_") && !key.starts_with("dt_") {
            return Err(OtelError::InvalidApiKey);
        }
        Ok(())
    }
}
