// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm::{
    ConnectionTrait, DatabaseBackend, DatabaseConnection, DbErr, Statement, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_core::external_plugin::channel::{PluginActorInfo, PluginHostPermission};
use thiserror::Error;

pub const MAX_PROMPT_BYTES: u32 = 65_536;
pub const MAX_SYSTEM_BYTES: usize = 16_384;
pub const MAX_OUTPUT_TOKENS: u32 = 4_096;

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct PluginGrantConfig {
    pub permissions: Vec<PluginHostPermission>,
    pub ai_daily_call_limit: u32,
    pub ai_max_output_tokens: u32,
}

impl Default for PluginGrantConfig {
    fn default() -> Self {
        Self {
            permissions: Vec::new(),
            ai_daily_call_limit: 100,
            ai_max_output_tokens: 1_024,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PluginGrants {
    pub actor: PluginActorInfo,
    pub config: PluginGrantConfig,
}

#[derive(Debug, Error)]
pub enum GrantError {
    #[error("Plugin actor for '{plugin_name}' was not found or is inactive")]
    NotFound { plugin_name: String },
    #[error("Invalid grant configuration for plugin '{plugin_name}': {reason}")]
    Invalid { plugin_name: String, reason: String },
    #[error("Plugin actor for '{plugin_name}' changed while grants were being approved")]
    ActorChanged { plugin_name: String },
    #[error("Database operation '{operation}' failed for plugin '{plugin_name}': {source}")]
    Database {
        plugin_name: String,
        operation: &'static str,
        source: DbErr,
    },
}

#[derive(Clone)]
pub struct PluginGrantService {
    db: Arc<DatabaseConnection>,
}

impl PluginGrantService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    pub async fn ensure_actor(
        &self,
        plugin_name: &str,
        sha256: &str,
        source_identity: &str,
    ) -> Result<PluginGrants, GrantError> {
        let id = uuid::Uuid::new_v4();
        // A reinstallation never inherits the previous actor's authority.
        // Active, authenticated upgrades retain the actor but cannot expand
        // its grants; source-conflict checks happen before activation.
        let transaction = self
            .db
            .begin()
            .await
            .map_err(|source| GrantError::Database {
                plugin_name: plugin_name.into(),
                operation: "begin actor binding",
                source,
            })?;
        transaction
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
                [plugin_name.into()],
            ))
            .await
            .map_err(|source| GrantError::Database {
                plugin_name: plugin_name.into(),
                operation: "lock actor identity",
                source,
            })?;
        transaction
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "DELETE FROM external_plugin_actors WHERE plugin_name=$1 AND (active=FALSE OR source_identity<>$2)",
                [plugin_name.into(), source_identity.into()],
            ))
            .await
            .map_err(|source| GrantError::Database {
                plugin_name: plugin_name.into(),
                operation: "rotate actor",
                source,
            })?;
        transaction.execute(Statement::from_sql_and_values(DatabaseBackend::Postgres,
            "INSERT INTO external_plugin_actors (id, plugin_name, binary_sha256, source_identity, active) VALUES ($1,$2,$3,$4,TRUE) ON CONFLICT (plugin_name) DO NOTHING",
            [id.into(), plugin_name.into(), sha256.into(), source_identity.into()])).await
            .map_err(|source| GrantError::Database { plugin_name: plugin_name.into(), operation: "ensure actor", source })?;
        let bound_id: String = transaction
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT id::text AS id FROM external_plugin_actors WHERE plugin_name=$1 AND active=TRUE",
                [plugin_name.into()],
            ))
            .await
            .map_err(|source| GrantError::Database { plugin_name: plugin_name.into(), operation: "read bound actor", source })?
            .ok_or_else(|| GrantError::NotFound { plugin_name: plugin_name.into() })?
            .try_get("", "id")
            .map_err(|source| GrantError::Database { plugin_name: plugin_name.into(), operation: "decode bound actor", source })?;
        transaction
            .commit()
            .await
            .map_err(|source| GrantError::Database {
                plugin_name: plugin_name.into(),
                operation: "commit actor binding",
                source,
            })?;
        let grants = self.get(plugin_name).await?;
        if grants.actor.id != bound_id {
            return Err(GrantError::NotFound {
                plugin_name: plugin_name.into(),
            });
        }
        Ok(grants)
    }

    /// Commit the binary identity only after the candidate has passed startup
    /// verification and its installation has been activated.
    pub async fn commit_binary_hash(
        &self,
        plugin_name: &str,
        sha256: &str,
    ) -> Result<(), GrantError> {
        let result = self
            .db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE external_plugin_actors SET binary_sha256=$2,updated_at=NOW() WHERE plugin_name=$1 AND active=TRUE",
                [plugin_name.into(), sha256.into()],
            ))
            .await
            .map_err(|source| GrantError::Database {
                plugin_name: plugin_name.into(),
                operation: "commit activated binary identity",
                source,
            })?;
        if result.rows_affected() != 1 {
            return Err(GrantError::NotFound {
                plugin_name: plugin_name.into(),
            });
        }
        Ok(())
    }

    pub async fn get(&self, plugin_name: &str) -> Result<PluginGrants, GrantError> {
        let row = self.db.query_one(Statement::from_sql_and_values(DatabaseBackend::Postgres,
            "SELECT a.id::text AS id,a.plugin_name,a.active,COALESCE(g.permissions,ARRAY[]::text[]) AS permissions,COALESCE(g.ai_daily_call_limit,100) AS ai_daily_call_limit,COALESCE(g.ai_max_output_tokens,1024) AS ai_max_output_tokens FROM external_plugin_actors a LEFT JOIN external_plugin_grants g ON g.actor_id=a.id WHERE a.plugin_name=$1 AND a.active=TRUE",
            [plugin_name.into()])).await
            .map_err(|source| GrantError::Database { plugin_name: plugin_name.into(), operation: "read grants", source })?
            .ok_or_else(|| GrantError::NotFound { plugin_name: plugin_name.into() })?;
        let raw: Vec<String> =
            row.try_get("", "permissions")
                .map_err(|source| GrantError::Database {
                    plugin_name: plugin_name.into(),
                    operation: "decode grants",
                    source,
                })?;
        let permissions = raw
            .iter()
            .map(|value| {
                parse_permission(value).ok_or_else(|| GrantError::Invalid {
                    plugin_name: plugin_name.into(),
                    reason: format!("unknown stored permission '{value}'"),
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let daily: i32 =
            row.try_get("", "ai_daily_call_limit")
                .map_err(|source| GrantError::Database {
                    plugin_name: plugin_name.into(),
                    operation: "decode daily AI limit",
                    source,
                })?;
        let output: i32 =
            row.try_get("", "ai_max_output_tokens")
                .map_err(|source| GrantError::Database {
                    plugin_name: plugin_name.into(),
                    operation: "decode AI token limit",
                    source,
                })?;
        Ok(PluginGrants {
            actor: PluginActorInfo {
                id: row
                    .try_get("", "id")
                    .map_err(|source| GrantError::Database {
                        plugin_name: plugin_name.into(),
                        operation: "decode actor id",
                        source,
                    })?,
                name: plugin_name.into(),
                active: true,
            },
            config: PluginGrantConfig {
                permissions,
                ai_daily_call_limit: daily.max(0) as u32,
                ai_max_output_tokens: output.max(0) as u32,
            },
        })
    }

    pub async fn update(
        &self,
        plugin_name: &str,
        config: PluginGrantConfig,
    ) -> Result<PluginGrants, GrantError> {
        validate(plugin_name, &config)?;
        let current = self.get(plugin_name).await?;
        self.update_for_actor(plugin_name, &current.actor.id, config)
            .await
    }

    pub async fn update_for_actor(
        &self,
        plugin_name: &str,
        actor_id: &str,
        config: PluginGrantConfig,
    ) -> Result<PluginGrants, GrantError> {
        validate(plugin_name, &config)?;
        let values = config
            .permissions
            .iter()
            .map(|p| p.as_str().to_string())
            .collect::<Vec<_>>();
        let result = self.db.execute(Statement::from_sql_and_values(DatabaseBackend::Postgres,
            "INSERT INTO external_plugin_grants(actor_id,permissions,ai_daily_call_limit,ai_max_output_tokens) SELECT id,$3,$4,$5 FROM external_plugin_actors WHERE plugin_name=$1 AND id=$2::uuid AND active=TRUE ON CONFLICT(actor_id) DO UPDATE SET permissions=EXCLUDED.permissions,ai_daily_call_limit=EXCLUDED.ai_daily_call_limit,ai_max_output_tokens=EXCLUDED.ai_max_output_tokens,updated_at=NOW()",
            [plugin_name.into(), actor_id.into(), values.into(), (config.ai_daily_call_limit as i32).into(), (config.ai_max_output_tokens as i32).into()])).await
            .map_err(|source| GrantError::Database { plugin_name: plugin_name.into(), operation: "update grants", source })?;
        if result.rows_affected() != 1 {
            return Err(GrantError::ActorChanged {
                plugin_name: plugin_name.into(),
            });
        }
        self.get(plugin_name).await
    }

    pub async fn revoke_actor(&self, plugin_name: &str) -> Result<(), GrantError> {
        self.db.execute(Statement::from_sql_and_values(DatabaseBackend::Postgres,
            "UPDATE external_plugin_actors SET active=FALSE,updated_at=NOW() WHERE plugin_name=$1", [plugin_name.into()])).await
            .map_err(|source| GrantError::Database { plugin_name: plugin_name.into(), operation: "revoke actor", source })?;
        Ok(())
    }

    pub async fn consume_ai_call(
        &self,
        grants: &PluginGrants,
        requested_tokens: u32,
    ) -> Result<bool, GrantError> {
        let row = self.db.query_one(Statement::from_sql_and_values(DatabaseBackend::Postgres,
            "WITH eligible AS (SELECT a.id,g.ai_daily_call_limit AS call_limit FROM external_plugin_actors a JOIN external_plugin_grants g ON g.actor_id=a.id WHERE a.plugin_name=$1 AND a.id=$2::uuid AND a.active=TRUE AND g.permissions @> ARRAY['ai_generate']::text[] AND g.ai_max_output_tokens >= $3) INSERT INTO external_plugin_ai_usage(actor_id,usage_date,calls) SELECT id,CURRENT_DATE,1 FROM eligible WHERE call_limit>0 ON CONFLICT(actor_id,usage_date) DO UPDATE SET calls=external_plugin_ai_usage.calls+1 WHERE external_plugin_ai_usage.calls < (SELECT call_limit FROM eligible) RETURNING calls",
            [grants.actor.name.clone().into(), grants.actor.id.clone().into(), (requested_tokens as i32).into()])).await
            .map_err(|source| GrantError::Database { plugin_name: grants.actor.name.clone(), operation: "consume AI quota", source })?;
        Ok(row.is_some())
    }
}

fn validate(plugin_name: &str, config: &PluginGrantConfig) -> Result<(), GrantError> {
    if config.ai_daily_call_limit > 10_000
        || config.ai_max_output_tokens == 0
        || config.ai_max_output_tokens > MAX_OUTPUT_TOKENS
    {
        return Err(GrantError::Invalid {
            plugin_name: plugin_name.into(),
            reason: format!(
                "daily calls must be <= 10000 and output tokens must be 1..={MAX_OUTPUT_TOKENS}"
            ),
        });
    }
    Ok(())
}

fn parse_permission(value: &str) -> Option<PluginHostPermission> {
    match value {
        "ai_generate" => Some(PluginHostPermission::AiGenerate),
        "projects_read" => Some(PluginHostPermission::ProjectsRead),
        "environments_read" => Some(PluginHostPermission::EnvironmentsRead),
        "deployments_read" => Some(PluginHostPermission::DeploymentsRead),
        "api_read" => Some(PluginHostPermission::ApiRead),
        "api_write" => Some(PluginHostPermission::ApiWrite),
        "events_read" => Some(PluginHostPermission::EventsRead),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};

    #[tokio::test]
    async fn commit_binary_hash_requires_one_active_actor() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_exec_results([MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 0,
                }])
                .into_connection(),
        );
        let service = PluginGrantService::new(db);

        let result = service
            .commit_binary_hash("missing-plugin", "sha256:verified")
            .await;

        assert!(matches!(
            result,
            Err(GrantError::NotFound { plugin_name }) if plugin_name == "missing-plugin"
        ));
    }

    #[tokio::test]
    async fn commit_binary_hash_accepts_exactly_one_active_actor() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_exec_results([MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 1,
                }])
                .into_connection(),
        );
        let service = PluginGrantService::new(db);

        service
            .commit_binary_hash("active-plugin", "sha256:verified")
            .await
            .expect("the activated actor hash should commit");
    }

    #[tokio::test]
    async fn update_for_actor_rejects_a_rotated_actor_without_granting_replacement() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_exec_results([MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 0,
                }])
                .into_connection(),
        );
        let service = PluginGrantService::new(db);

        let result = service
            .update_for_actor(
                "rotated-plugin",
                "00000000-0000-0000-0000-000000000001",
                PluginGrantConfig::default(),
            )
            .await;

        assert!(matches!(
            result,
            Err(GrantError::ActorChanged { plugin_name }) if plugin_name == "rotated-plugin"
        ));
    }
}
