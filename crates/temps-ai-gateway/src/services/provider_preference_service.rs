// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Storage for gateway-only defaults used by server-authored AI summaries.
//!
//! Development harnesses never read or write this configuration. They are
//! selected explicitly by an application thread and run in the agent runtime.

use sea_orm::{ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, Set};
use std::sync::Arc;
use temps_entities::ai_gateway_config::{self, Column, Entity};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ProviderPreferenceError {
    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),
}

pub struct ProviderPreferenceService {
    db: Arc<DatabaseConnection>,
}

impl ProviderPreferenceService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    pub async fn get(
        &self,
        scope: &str,
    ) -> Result<Option<ai_gateway_config::Model>, ProviderPreferenceError> {
        Entity::find()
            .filter(Column::Scope.eq(scope))
            .one(self.db.as_ref())
            .await
            .map_err(ProviderPreferenceError::Database)
    }

    /// Persist gateway defaults inherited by server-authored `*.summary`
    /// requests. Capability validation happens in the HTTP handler before this
    /// write; runtime adapters validate again before dispatch.
    pub async fn set_summary_preference(
        &self,
        provider_id: Option<String>,
        model: Option<String>,
        thinking_level: Option<String>,
    ) -> Result<ai_gateway_config::Model, ProviderPreferenceError> {
        let existing = self.get("instance").await?;
        let mut active = if let Some(existing) = existing {
            ai_gateway_config::ActiveModel::from(existing)
        } else {
            ai_gateway_config::ActiveModel {
                scope: Set("instance".to_string()),
                provider_type: Set("gateway".to_string()),
                agent_cli_provider_id: Set(None),
                interactive_bridge_enabled: Set(false),
                ..Default::default()
            }
        };
        active.summary_provider_id = Set(provider_id);
        active.summary_model = Set(model);
        active.summary_thinking_level = Set(thinking_level);
        if active.id.is_not_set() {
            active
                .insert(self.db.as_ref())
                .await
                .map_err(ProviderPreferenceError::Database)
        } else {
            active
                .update(self.db.as_ref())
                .await
                .map_err(ProviderPreferenceError::Database)
        }
    }
}

/// This preserves the summary-default read seam without allowing an ambient
/// AI Gateway preference to select a host harness.
#[async_trait::async_trait]
impl temps_ai_agent_cli::ActiveProviderReader for ProviderPreferenceService {
    async fn active_agent_cli_provider(&self) -> Option<String> {
        None
    }

    async fn summary_preference(
        &self,
    ) -> Result<temps_ai_agent_cli::AiSummaryPreference, temps_ai::AiError> {
        match self.get("instance").await {
            Ok(Some(row)) => Ok(temps_ai_agent_cli::AiSummaryPreference {
                provider: row.summary_provider_id,
                model: row.summary_model,
                thinking_level: row.summary_thinking_level,
            }),
            Ok(None) => Ok(temps_ai_agent_cli::AiSummaryPreference::default()),
            Err(error) => Err(temps_ai::AiError::Provider {
                purpose: "summary.preference".to_string(),
                reason: format!("failed to read configured AI summary routing: {error}"),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};

    #[tokio::test]
    async fn get_on_empty_table_returns_none() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![Vec::<ai_gateway_config::Model>::new()])
                .into_connection(),
        );
        let service = ProviderPreferenceService::new(db);

        assert_eq!(service.get("instance").await.unwrap(), None);
    }

    #[tokio::test]
    async fn active_harness_preference_is_never_exposed() {
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let service = ProviderPreferenceService::new(db);

        assert_eq!(
            temps_ai_agent_cli::ActiveProviderReader::active_agent_cli_provider(&service).await,
            None
        );
    }
}
