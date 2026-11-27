// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm::{
    ConnectionTrait, DatabaseBackend, DatabaseConnection, DbErr, Statement, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_plugin_protocol::channel::{PluginActorInfo, PluginHostPermission};

use crate::service_runtime::ServiceBinding;
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

/// A plugin that runs as a Temps service, as its actor row records it.
#[derive(Debug, Clone)]
pub struct StoredServicePlugin {
    pub name: String,
    /// The service and environment it runs as; `None` once either has been
    /// deleted.
    pub binding: Option<ServiceBinding>,
    /// The manifest it last answered with.
    pub manifest: Option<temps_plugin_protocol::PluginManifest>,
}

#[derive(Debug, Error)]
pub enum GrantError {
    #[error(
        "AI consent was saved for plugin '{plugin_name}', but publishing tools failed: {source}"
    )]
    AiToolsPublication { plugin_name: String, source: DbErr },
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

    /// Bind a candidate without mutating the durable actor. Same-source
    /// upgrades reuse their actor; a different source receives a provisional,
    /// default-deny identity until activation commits it.
    pub async fn prepare_actor(
        &self,
        plugin_name: &str,
        source_identity: &str,
    ) -> Result<PluginGrants, GrantError> {
        let row = self
            .db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT id::text AS id,source_identity FROM external_plugin_actors WHERE plugin_name=$1 AND active=TRUE",
                [plugin_name.into()],
            ))
            .await
            .map_err(|source| GrantError::Database {
                plugin_name: plugin_name.into(),
                operation: "prepare candidate actor",
                source,
            })?;
        if let Some(row) = row {
            let current_source: String =
                row.try_get("", "source_identity")
                    .map_err(|source| GrantError::Database {
                        plugin_name: plugin_name.into(),
                        operation: "decode candidate actor source",
                        source,
                    })?;
            if current_source == source_identity {
                return self.get(plugin_name).await;
            }
        }
        Ok(PluginGrants {
            actor: PluginActorInfo {
                id: uuid::Uuid::new_v4().to_string(),
                name: plugin_name.into(),
                active: false,
            },
            config: PluginGrantConfig::default(),
        })
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
        // its grants; source-conflict checks happen before activation. Startup
        // also reconciles a verified binary activated before a crash: AI consent
        // belongs to that binary, even if activation's database commit was lost.
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
            "INSERT INTO external_plugin_actors (id, plugin_name, binary_sha256, source_identity, active) VALUES ($1,$2,$3,$4,TRUE) ON CONFLICT (plugin_name) DO UPDATE SET ai_tools_enabled=CASE WHEN external_plugin_actors.binary_sha256 IS DISTINCT FROM EXCLUDED.binary_sha256 THEN FALSE ELSE external_plugin_actors.ai_tools_enabled END,binary_sha256=EXCLUDED.binary_sha256,updated_at=NOW()",
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

    /// Where the active actor of `plugin_name` was installed from, or `None`
    /// when the plugin has no active actor.
    pub async fn active_source(&self, plugin_name: &str) -> Result<Option<String>, GrantError> {
        let database = |operation| {
            move |source| GrantError::Database {
                plugin_name: plugin_name.into(),
                operation,
                source,
            }
        };
        let row = self
            .db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT source_identity FROM external_plugin_actors WHERE plugin_name=$1 AND active=TRUE",
                [plugin_name.into()],
            ))
            .await
            .map_err(database("read actor source"))?;
        match row {
            None => Ok(None),
            Some(row) => row
                .try_get("", "source_identity")
                .map(Some)
                .map_err(database("decode actor source")),
        }
    }

    /// Record that the active actor of `plugin_name` runs as a Temps service,
    /// and the manifest it answered with.
    pub async fn bind_service(
        &self,
        plugin_name: &str,
        binding: ServiceBinding,
        manifest: &temps_plugin_protocol::PluginManifest,
    ) -> Result<(), GrantError> {
        let manifest = serde_json::to_value(manifest).map_err(|error| GrantError::Invalid {
            plugin_name: plugin_name.into(),
            reason: format!("its manifest cannot be stored: {error}"),
        })?;
        let result = self
            .db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE external_plugin_actors SET isolation='service',project_id=$2,environment_id=$3,manifest=$4,updated_at=NOW() WHERE plugin_name=$1 AND active=TRUE",
                [
                    plugin_name.into(),
                    binding.project_id.into(),
                    binding.environment_id.into(),
                    manifest.into(),
                ],
            ))
            .await
            .map_err(|source| GrantError::Database {
                plugin_name: plugin_name.into(),
                operation: "bind service",
                source,
            })?;
        if result.rows_affected() != 1 {
            return Err(GrantError::NotFound {
                plugin_name: plugin_name.into(),
            });
        }
        Ok(())
    }

    /// Every active plugin that runs as a Temps service, in one query.
    pub async fn service_plugins(&self) -> Result<Vec<StoredServicePlugin>, GrantError> {
        let database = |source| GrantError::Database {
            plugin_name: "*".into(),
            operation: "list service plugins",
            source,
        };
        let rows = self
            .db
            .query_all(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT plugin_name, project_id, environment_id, manifest FROM external_plugin_actors WHERE active=TRUE AND isolation='service' ORDER BY plugin_name",
            ))
            .await
            .map_err(database)?;
        let mut plugins = Vec::with_capacity(rows.len());
        for row in rows {
            let name: String = row.try_get("", "plugin_name").map_err(database)?;
            let project_id: Option<i32> = row.try_get("", "project_id").map_err(database)?;
            let environment_id: Option<i32> =
                row.try_get("", "environment_id").map_err(database)?;
            let manifest: Option<serde_json::Value> =
                row.try_get("", "manifest").map_err(database)?;
            plugins.push(StoredServicePlugin {
                binding: project_id
                    .zip(environment_id)
                    .map(|(project_id, environment_id)| ServiceBinding {
                        project_id,
                        environment_id,
                    }),
                // A manifest this build cannot read is the same as none: the
                // plugin answers a current one when it is next attached to.
                manifest: manifest.and_then(|value| serde_json::from_value(value).ok()),
                name,
            });
        }
        Ok(plugins)
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
