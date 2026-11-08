// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Durable default-off consent and lifecycle publication of plugin tools.
use crate::{grants::GrantError, service::ExternalPluginsService};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use serde::{Deserialize, Serialize};
use temps_ai_api_tools::{PluginToolAccess, PluginToolsIndex};
use temps_entities::external_plugin_actors::{self as actors, Entity as Actors};

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct AiToolInfo {
    pub name: String,
    pub description: String,
    pub access: String,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct PluginAiToolsResponse {
    pub actor_id: String,
    pub enabled: bool,
    pub tools: Vec<AiToolInfo>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct UpdatePluginAiTools {
    pub actor_id: String,
    pub enabled: bool,
}

impl ExternalPluginsService {
    pub fn with_ai_tools_registry(
        mut self,
        registry: std::sync::Arc<temps_ai_api_tools::PluginToolsRegistry>,
    ) -> Self {
        self.ai_tools = registry;
        self
    }

    pub async fn plugin_ai_tools(&self, name: &str) -> Result<PluginAiToolsResponse, GrantError> {
        let actor = Actors::find()
            .filter(actors::Column::PluginName.eq(name))
            .filter(actors::Column::Active.eq(true))
            .one(self.db.as_ref())
            .await
            .map_err(|source| GrantError::Database {
                plugin_name: name.into(),
                operation: "read AI tool consent",
                source,
            })?
            .ok_or_else(|| GrantError::NotFound {
                plugin_name: name.into(),
            })?;
        Ok(self.ai_tools_response(actor))
    }

    fn ai_tools_response(&self, actor: actors::Model) -> PluginAiToolsResponse {
        let name = &actor.plugin_name;
        let snapshot = self.ai_tools.snapshot();
        let tools = snapshot
            .tools()
            .iter()
            .filter(|tool| &tool.plugin == name)
            .map(|tool| AiToolInfo {
                name: tool.operation.operation_id.clone(),
                description: tool.operation.description.clone().unwrap_or_default(),
                access: match tool.access {
                    PluginToolAccess::Read => "read",
                    PluginToolAccess::Write => "write",
                }
                .into(),
            })
            .collect();
        PluginAiToolsResponse {
            actor_id: actor.id.to_string(),
            enabled: actor.ai_tools_enabled,
            tools,
        }
    }

    pub async fn update_plugin_ai_tools(
        &self,
        name: &str,
        request: UpdatePluginAiTools,
    ) -> Result<PluginAiToolsResponse, GrantError> {
        let _lifecycle = self.lifecycle.lock().await;
        let actor = Actors::find()
            .filter(actors::Column::PluginName.eq(name))
            .filter(actors::Column::Active.eq(true))
            .one(self.db.as_ref())
            .await
            .map_err(|source| GrantError::Database {
                plugin_name: name.into(),
                operation: "read AI tool consent for update",
                source,
            })?
            .ok_or_else(|| GrantError::NotFound {
                plugin_name: name.into(),
            })?;
        if actor.id.to_string() != request.actor_id {
            return Err(GrantError::ActorChanged {
                plugin_name: name.into(),
            });
        }
        let mut active: actors::ActiveModel = actor.into();
        active.ai_tools_enabled = Set(request.enabled);
        active.updated_at = Set(chrono::Utc::now());
        let saved =
            active
                .update(self.db.as_ref())
                .await
                .map_err(|source| GrantError::Database {
                    plugin_name: name.into(),
                    operation: "update AI tool consent",
                    source,
                })?;
        self.rebuild_ai_tools().await;
        Ok(self.ai_tools_response(saved))
    }

    /// Called under the lifecycle lock so old discovery cannot overwrite a toggle.
    pub(crate) async fn rebuild_ai_tools(&self) {
        let actors = match Actors::find()
            .filter(actors::Column::Active.eq(true))
            .all(self.db.as_ref())
            .await
        {
            Ok(actors) => actors,
            Err(error) => {
                self.ai_tools.publish(PluginToolsIndex::default());
                tracing::error!(%error, "Failed to load plugin AI consent; tools disabled");
                return;
            }
        };
        let enabled: std::collections::HashMap<_, _> = actors
            .into_iter()
            .map(|a| (a.plugin_name, a.ai_tools_enabled))
            .collect();
        let mut index = PluginToolsIndex::default();
        for (name, spec) in self.manager.openapi_schemas().await {
            match PluginToolsIndex::from_openapi(
                &name,
                &spec,
                enabled.get(&name).copied().unwrap_or(false),
            ) {
                Ok(tools) => index.extend(tools),
                Err(error) => {
                    tracing::warn!(plugin=%name,%error,"Invalid plugin AI declarations excluded")
                }
            }
        }
        self.ai_tools.publish(index);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, DbErr, MockDatabase};
    use std::sync::Arc;

    fn service(db: sea_orm::DatabaseConnection) -> ExternalPluginsService {
        ExternalPluginsService::new_empty(
            crate::manager::ExternalPluginConfig::new(
                std::env::temp_dir().join("plugin-ai-service-test"),
                "postgres://localhost/test".into(),
            ),
            None,
            Arc::new(db),
        )
    }

    #[tokio::test]
    async fn consent_errors_identify_plugin_and_reject_replaced_actor() {
        let missing = service(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([Vec::<actors::Model>::new()])
                .into_connection(),
        );
        assert!(
            matches!(missing.plugin_ai_tools("reports").await,Err(GrantError::NotFound {plugin_name}) if plugin_name=="reports")
        );
        let failed = service(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_errors([DbErr::Custom("offline".into())])
                .into_connection(),
        );
        assert!(
            matches!(failed.plugin_ai_tools("reports").await,Err(GrantError::Database {plugin_name,..}) if plugin_name=="reports")
        );
        let actor = actors::Model {
            id: uuid::Uuid::from_u128(2),
            plugin_name: "reports".into(),
            binary_sha256: "fixture".into(),
            source_identity: "fixture".into(),
            active: true,
            ai_tools_enabled: false,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        let changed = service(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![actor]])
                .into_connection(),
        );
        assert!(
            matches!(changed.update_plugin_ai_tools("reports",UpdatePluginAiTools {actor_id:uuid::Uuid::from_u128(1).to_string(),enabled:true}).await,Err(GrantError::ActorChanged {plugin_name}) if plugin_name=="reports")
        );
    }
    #[tokio::test]
    async fn saved_consent_survives_a_failed_index_refresh_for_auditing() {
        let actor = actors::Model {
            id: uuid::Uuid::from_u128(3),
            plugin_name: "reports".into(),
            binary_sha256: "fixture".into(),
            source_identity: "fixture".into(),
            active: true,
            ai_tools_enabled: false,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        let mut saved = actor.clone();
        saved.ai_tools_enabled = true;
        let service = service(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![actor], vec![saved]])
                .append_query_errors([DbErr::Custom("refresh unavailable".into())])
                .into_connection(),
        );
        let response = service
            .update_plugin_ai_tools(
                "reports",
                UpdatePluginAiTools {
                    actor_id: uuid::Uuid::from_u128(3).to_string(),
                    enabled: true,
                },
            )
            .await
            .unwrap();
        assert!(
            response.enabled,
            "saved changes must return success so handlers audit them"
        );
        assert!(
            service.ai_tools.snapshot().tools().is_empty(),
            "refresh failure disables execution"
        );
    }

    #[tokio::test]
    async fn shutdown_revokes_a_snapshot_published_by_an_in_progress_lifecycle() {
        let service = Arc::new(service(
            MockDatabase::new(DatabaseBackend::Postgres).into_connection(),
        ));
        let lifecycle = service.lifecycle.lock().await;
        let closing = service.clone();
        let shutdown = tokio::spawn(async move { closing.shutdown_all().await });
        tokio::task::yield_now().await;
        let spec = serde_json::from_value(serde_json::json!({"openapi":"3.1.0","info":{"title":"Reports","version":"1"},"paths":{"/reports":{"get":{"operationId":"list","x-temps-ai-tool":{},"responses":{"200":{"description":"ok"}}}}}})).unwrap();
        service
            .ai_tools
            .publish(PluginToolsIndex::from_openapi("reports", &spec, true).unwrap());
        drop(lifecycle);
        shutdown.await.unwrap();
        assert!(service.ai_tools.snapshot().tools().is_empty());
    }
    #[tokio::test]
    async fn failed_uninstall_revocation_retains_other_plugins_tools() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(directory.path().join("reports")).unwrap();
        std::fs::write(directory.path().join("reports/active.json"), "{}").unwrap();
        let service = ExternalPluginsService::new_empty(
            crate::manager::ExternalPluginConfig::new(
                directory.path().to_path_buf(),
                "postgres://localhost/test".into(),
            ),
            None,
            Arc::new(
                MockDatabase::new(DatabaseBackend::Postgres)
                    .append_exec_errors([DbErr::Custom("revocation unavailable".into())])
                    .into_connection(),
            ),
        );
        let spec = serde_json::from_value(serde_json::json!({"openapi":"3.1.0","info":{"title":"Reports","version":"1"},"paths":{"/reports":{"get":{"operationId":"list","x-temps-ai-tool":{},"responses":{"200":{"description":"ok"}}}}}})).unwrap();
        let mut index = PluginToolsIndex::from_openapi("reports", &spec, true).unwrap();
        index.extend(PluginToolsIndex::from_openapi("metrics", &spec, true).unwrap());
        service.ai_tools.publish(index);
        let error = service.uninstall_plugin("reports").await.unwrap_err();
        assert!(error.to_string().contains("revocation unavailable"));
        let remaining = service.ai_tools.snapshot();
        assert_eq!(remaining.tools().len(), 1);
        assert!(remaining
            .callable("plugin.metrics.list", PluginToolAccess::Read)
            .is_some());
        assert!(remaining
            .callable("plugin.reports.list", PluginToolAccess::Read)
            .is_none());
    }
}
