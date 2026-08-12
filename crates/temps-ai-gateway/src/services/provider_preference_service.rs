//! Read/write the per-scope AI provider preference stored in
//! `ai_gateway_config`.
//!
//! Only the two columns added in ADR-037 Phase 2 are managed here:
//! `provider_type` and `agent_cli_provider_id`.  The governance limit columns
//! (`allowed_models`, `max_requests_per_minute`, `max_cost_per_month_microcents`)
//! are left untouched by this service.

use sea_orm::{ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, Set};
use std::sync::Arc;
use temps_entities::ai_gateway_config::{self, Column, Entity};
use thiserror::Error;

use crate::services::ProviderKeyService;

#[derive(Error, Debug)]
pub enum ProviderPreferenceError {
    /// The caller supplied a value that fails invariant checks.  The `message`
    /// field includes the offending value and the expected alternatives.
    #[error("Validation error: {message}")]
    Validation { message: String },

    /// A Sea-ORM error propagated from the underlying database query.
    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),
}

pub struct ProviderPreferenceService {
    db: Arc<DatabaseConnection>,
    provider_key_service: Arc<ProviderKeyService>,
}

impl ProviderPreferenceService {
    pub fn new(db: Arc<DatabaseConnection>, provider_key_service: Arc<ProviderKeyService>) -> Self {
        Self {
            db,
            provider_key_service,
        }
    }

    /// Return the `ai_gateway_config` row for `scope`, or `None` if no row
    /// exists yet.
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

    /// Upsert the provider preference for `scope`.
    ///
    /// Validation:
    /// - `provider_type` must be exactly `"gateway"` or `"agent_cli"`.
    /// - When `provider_type == "agent_cli"`, `agent_cli_provider_id` is
    ///   required and must match a catalog entry.
    /// - When `provider_type == "gateway"`, `agent_cli_provider_id` is forced
    ///   to `None` regardless of what the caller passes.
    /// - The legacy `interactive_bridge_enabled` input is accepted for API
    ///   compatibility but ignored. All adapters now use the shared turn
    ///   services contract; the persisted legacy flag is reset to `false`.
    ///
    /// If a row already exists for `scope`, only `provider_type`,
    /// `agent_cli_provider_id`, and the retired bridge flag are mutated;
    /// governance limit columns are left untouched.
    /// If no row exists, a new one is inserted with the governance columns left `NULL`.
    pub async fn set(
        &self,
        scope: &str,
        provider_type: String,
        agent_cli_provider_id: Option<String>,
        _interactive_bridge_enabled: Option<bool>,
    ) -> Result<ai_gateway_config::Model, ProviderPreferenceError> {
        // Validate provider_type.
        if provider_type != "gateway" && provider_type != "agent_cli" {
            return Err(ProviderPreferenceError::Validation {
                message: format!(
                    "Invalid provider_type '{}': must be 'gateway' or 'agent_cli'",
                    provider_type
                ),
            });
        }

        // Normalise agent_cli_provider_id based on provider_type.
        let agent_cli_provider_id: Option<String> = if provider_type == "agent_cli" {
            let id = agent_cli_provider_id.ok_or_else(|| ProviderPreferenceError::Validation {
                message: "provider_type 'agent_cli' requires agent_cli_provider_id to be present"
                    .to_string(),
            })?;
            // Validate against the static provider catalog.
            if temps_agents::ai_cli::find_provider(&id).is_none() {
                let valid_ids: Vec<&str> = temps_agents::ai_cli::PROVIDER_CATALOG
                    .iter()
                    .map(|p| p.id)
                    .collect();
                return Err(ProviderPreferenceError::Validation {
                    message: format!(
                        "Unknown agent_cli_provider_id '{}': valid ids are [{}]",
                        id,
                        valid_ids.join(", ")
                    ),
                });
            }
            Some(id)
        } else {
            // Gateway provider: no CLI provider id is stored.
            None
        };

        // Upsert: load existing row (if any) so governance columns are
        // preserved on update.
        if let Some(existing) = self.get(scope).await? {
            let mut active: ai_gateway_config::ActiveModel = existing.into();
            active.provider_type = Set(provider_type);
            active.agent_cli_provider_id = Set(agent_cli_provider_id);
            active.interactive_bridge_enabled = Set(false);
            active
                .update(self.db.as_ref())
                .await
                .map_err(ProviderPreferenceError::Database)
        } else {
            let model = ai_gateway_config::ActiveModel {
                scope: Set(scope.to_string()),
                provider_type: Set(provider_type),
                agent_cli_provider_id: Set(agent_cli_provider_id),
                interactive_bridge_enabled: Set(false),
                ..Default::default()
            };
            model
                .insert(self.db.as_ref())
                .await
                .map_err(ProviderPreferenceError::Database)
        }
    }

    /// Resolve the agent-CLI provider that should actually receive routed
    /// requests right now.
    ///
    /// An explicit preference (set via `PUT /ai/provider-preference`) always
    /// wins, whichever way it points. Only when NO preference has ever been
    /// saved for this scope do we auto-default — mirroring how BYOK provider
    /// keys already work (the first active key is used automatically, no
    /// separate "activate" step): if there's no BYOK key configured either,
    /// and exactly one agent CLI is authenticated on this host, that CLI
    /// becomes the de facto active provider without requiring the operator to
    /// click "Use this provider" first. Two or more authenticated CLIs is
    /// treated as ambiguous — the operator must pick.
    pub async fn resolve_active_agent_cli_provider(&self) -> Option<String> {
        match self.get("instance").await {
            Ok(Some(row)) => {
                return if row.provider_type == "agent_cli" {
                    row.agent_cli_provider_id
                } else {
                    None
                };
            }
            Ok(None) => {}
            Err(e) => {
                tracing::error!("Failed to read AI provider preference for routing: {e}");
                return None;
            }
        }

        match self.provider_key_service.list_active().await {
            Ok(keys) if !keys.is_empty() => return None,
            Err(e) => {
                tracing::error!(
                    "Failed to check active BYOK keys for AI provider auto-default: {e}"
                );
                return None;
            }
            _ => {}
        }

        let mut host_authenticated: Vec<String> = Vec::new();
        for registration in temps_agents::ai_cli::PROVIDER_CATALOG {
            if let Some(provider) = temps_agents::ai_cli::create_provider(registration.id) {
                let status = provider.get_status().await;
                if status.installed && status.authenticated {
                    host_authenticated.push(registration.id.to_string());
                }
            }
        }

        match host_authenticated.len() {
            1 => host_authenticated.into_iter().next(),
            _ => None,
        }
    }
}

/// Lets [`temps_ai_agent_cli::DispatchingAiService`] learn the active
/// agent-CLI preference without `temps-ai-agent-cli` depending on sea-orm or
/// this crate (which itself depends on `temps-ai-agent-cli` for
/// [`AgentCliAiService`](temps_ai_agent_cli::AgentCliAiService)).
#[async_trait::async_trait]
impl temps_ai_agent_cli::ActiveProviderReader for ProviderPreferenceService {
    async fn active_agent_cli_provider(&self) -> Option<String> {
        self.resolve_active_agent_cli_provider().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};

    fn test_provider_key_service(db: Arc<DatabaseConnection>) -> Arc<ProviderKeyService> {
        let encryption = Arc::new(
            temps_core::EncryptionService::new("01234567890123456789012345678901").unwrap(),
        );
        Arc::new(ProviderKeyService::new(db, encryption))
    }

    fn sample_config() -> ai_gateway_config::Model {
        ai_gateway_config::Model {
            id: 1,
            scope: "instance".to_string(),
            allowed_models: None,
            max_requests_per_minute: None,
            max_cost_per_month_microcents: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            provider_type: "gateway".to_string(),
            agent_cli_provider_id: None,
            interactive_bridge_enabled: false,
        }
    }

    #[tokio::test]
    async fn test_get_on_empty_table_returns_none() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<ai_gateway_config::Model>::new()])
            .into_connection();

        let db = Arc::new(db);
        let service = ProviderPreferenceService::new(db.clone(), test_provider_key_service(db));
        let result = service.get("instance").await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), None);
    }

    #[tokio::test]
    async fn test_set_invalid_provider_type_returns_validation_error() {
        // Validation fires before any DB access; no query results needed.
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let db = Arc::new(db);
        let service = ProviderPreferenceService::new(db.clone(), test_provider_key_service(db));

        let result = service
            .set("instance", "unsupported".to_string(), None, None)
            .await;

        assert!(matches!(
            result.unwrap_err(),
            ProviderPreferenceError::Validation { message }
            if message.contains("unsupported")
        ));
    }

    #[tokio::test]
    async fn test_set_agent_cli_without_provider_id_returns_validation_error() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let db = Arc::new(db);
        let service = ProviderPreferenceService::new(db.clone(), test_provider_key_service(db));

        let result = service
            .set("instance", "agent_cli".to_string(), None, None)
            .await;

        assert!(matches!(
            result.unwrap_err(),
            ProviderPreferenceError::Validation { .. }
        ));
    }

    #[tokio::test]
    async fn test_set_unknown_agent_cli_provider_id_returns_validation_error() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let db = Arc::new(db);
        let service = ProviderPreferenceService::new(db.clone(), test_provider_key_service(db));

        let result = service
            .set(
                "instance",
                "agent_cli".to_string(),
                Some("does_not_exist".to_string()),
                None,
            )
            .await;

        let err = result.unwrap_err();
        assert!(matches!(err, ProviderPreferenceError::Validation { .. }));
        // Error message should name the bad id and list valid ones.
        let msg = err.to_string();
        assert!(msg.contains("does_not_exist"), "message: {msg}");
        assert!(msg.contains("claude_cli"), "message: {msg}");
    }

    #[tokio::test]
    async fn test_set_gateway_forces_agent_cli_provider_id_to_none() {
        // First query: SELECT for get() → no existing row.
        // Second query: INSERT → return the persisted model.
        let inserted = ai_gateway_config::Model {
            id: 1,
            scope: "instance".to_string(),
            allowed_models: None,
            max_requests_per_minute: None,
            max_cost_per_month_microcents: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            provider_type: "gateway".to_string(),
            agent_cli_provider_id: None,
            interactive_bridge_enabled: false,
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![
                Vec::<ai_gateway_config::Model>::new(), // SELECT → empty
                vec![inserted],                         // INSERT → returned model
            ])
            .into_connection();

        let db = Arc::new(db);
        let service = ProviderPreferenceService::new(db.clone(), test_provider_key_service(db));

        // Pass a non-None agent_cli_provider_id for a gateway preference — it
        // must be normalised to None before the insert.
        let result = service
            .set(
                "instance",
                "gateway".to_string(),
                Some("claude_cli".to_string()),
                None,
            )
            .await;

        assert!(result.is_ok(), "expected Ok, got {:?}", result.unwrap_err());
        let model = result.unwrap();
        assert_eq!(model.provider_type, "gateway");
        assert_eq!(model.agent_cli_provider_id, None);
    }

    /// Verify the happy path for a known catalog id passes validation.
    #[tokio::test]
    async fn test_set_valid_agent_cli_provider_passes_validation_and_inserts() {
        let inserted = ai_gateway_config::Model {
            id: 1,
            scope: "instance".to_string(),
            allowed_models: None,
            max_requests_per_minute: None,
            max_cost_per_month_microcents: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            provider_type: "agent_cli".to_string(),
            agent_cli_provider_id: Some("claude_cli".to_string()),
            interactive_bridge_enabled: false,
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![
                Vec::<ai_gateway_config::Model>::new(), // SELECT → empty
                vec![inserted],                         // INSERT → returned model
            ])
            .into_connection();

        let db = Arc::new(db);
        let service = ProviderPreferenceService::new(db.clone(), test_provider_key_service(db));
        let result = service
            .set(
                "instance",
                "agent_cli".to_string(),
                Some("claude_cli".to_string()),
                None,
            )
            .await;

        assert!(result.is_ok(), "expected Ok, got {:?}", result.unwrap_err());
        let model = result.unwrap();
        assert_eq!(model.provider_type, "agent_cli");
        assert_eq!(model.agent_cli_provider_id, Some("claude_cli".to_string()));
    }

    /// Upsert on an existing row updates only the two preference columns.
    #[tokio::test]
    async fn test_set_updates_existing_row_without_touching_governance_columns() {
        let existing = sample_config(); // provider_type="gateway", agent_cli_provider_id=None
        let updated = ai_gateway_config::Model {
            provider_type: "gateway".to_string(),
            agent_cli_provider_id: None,
            ..existing.clone()
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![
                vec![existing], // SELECT → existing row found
                vec![updated],  // UPDATE RETURNING → updated model
            ])
            .into_connection();

        let db = Arc::new(db);
        let service = ProviderPreferenceService::new(db.clone(), test_provider_key_service(db));
        let result = service
            .set("instance", "gateway".to_string(), None, None)
            .await;

        assert!(result.is_ok());
    }

    /// An explicit `agent_cli` preference always wins — auto-default logic
    /// must not even be consulted.
    #[tokio::test]
    async fn test_resolve_active_returns_explicit_agent_cli_preference() {
        let existing = ai_gateway_config::Model {
            provider_type: "agent_cli".to_string(),
            agent_cli_provider_id: Some("claude_cli".to_string()),
            ..sample_config()
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![existing]]) // SELECT → explicit row
            .into_connection();

        let db = Arc::new(db);
        let service = ProviderPreferenceService::new(db.clone(), test_provider_key_service(db));

        let resolved = service.resolve_active_agent_cli_provider().await;
        assert_eq!(resolved, Some("claude_cli".to_string()));
    }

    /// An explicit `gateway` preference also wins — no auto-default even if
    /// a CLI happens to be authenticated on the host.
    #[tokio::test]
    async fn test_resolve_active_returns_none_for_explicit_gateway_preference() {
        let existing = sample_config(); // provider_type="gateway"
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![existing]]) // SELECT → explicit row
            .into_connection();

        let db = Arc::new(db);
        let service = ProviderPreferenceService::new(db.clone(), test_provider_key_service(db));

        let resolved = service.resolve_active_agent_cli_provider().await;
        assert_eq!(resolved, None);
    }

    /// No preference has ever been saved, but a BYOK key is already active —
    /// auto-default must not override an existing gateway setup.
    #[tokio::test]
    async fn test_resolve_active_skips_auto_default_when_gateway_key_active() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<ai_gateway_config::Model>::new()]) // SELECT preference → none
            .append_query_results(vec![vec![temps_entities::ai_provider_keys::Model {
                id: 1,
                provider: "openai".to_string(),
                display_name: "OpenAI".to_string(),
                api_key_encrypted: "enc".to_string(),
                base_url: None,
                default_model: None,
                is_active: true,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            }]]) // list_active → one active key
            .into_connection();

        let db = Arc::new(db);
        let service = ProviderPreferenceService::new(db.clone(), test_provider_key_service(db));

        let resolved = service.resolve_active_agent_cli_provider().await;
        assert_eq!(resolved, None);
    }

    /// The retired provider-specific bridge flag is reset regardless of the
    /// compatibility value sent by an older client.
    #[tokio::test]
    async fn test_set_resets_the_retired_interactive_bridge_flag() {
        // The existing row has the bridge enabled.
        let existing = ai_gateway_config::Model {
            id: 1,
            scope: "instance".to_string(),
            allowed_models: None,
            max_requests_per_minute: None,
            max_cost_per_month_microcents: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            provider_type: "agent_cli".to_string(),
            agent_cli_provider_id: Some("claude_cli".to_string()),
            interactive_bridge_enabled: true,
        };
        // The DB returns a model reflecting the forced-disable update.
        let updated = ai_gateway_config::Model {
            provider_type: "gateway".to_string(),
            agent_cli_provider_id: None,
            interactive_bridge_enabled: false, // forced to false
            ..existing.clone()
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![
                vec![existing], // SELECT → existing claude_cli row
                vec![updated],  // UPDATE → returned model with bridge disabled
            ])
            .into_connection();

        let db = Arc::new(db);
        let service = ProviderPreferenceService::new(db.clone(), test_provider_key_service(db));

        // Older clients may still request the flag; the common runtime ignores
        // it and persists the retired field as false.
        let result = service
            .set("instance", "gateway".to_string(), None, Some(true))
            .await;

        assert!(result.is_ok(), "expected Ok, got {:?}", result.unwrap_err());
        let model = result.unwrap();
        assert!(
            !model.interactive_bridge_enabled,
            "interactive_bridge_enabled should be force-disabled when switching away from claude_cli"
        );
    }

    /// No preference and no BYOK key: falls through to the host-CLI check.
    /// The result genuinely depends on which CLIs are installed/authenticated
    /// on the machine running this test — 0 authenticated CLIs yields `None`,
    /// exactly 1 yields `Some(that id)`, 2+ yields `None` (ambiguous). None of
    /// those outcomes can be asserted hermetically without mocking the CLI
    /// subprocess layer, so this only asserts the documented invariant: any
    /// `Some` result must name a real catalog provider, never a stray value.
    #[tokio::test]
    async fn test_resolve_active_result_is_always_a_known_provider_or_none() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<ai_gateway_config::Model>::new()]) // SELECT preference → none
            .append_query_results(vec![Vec::<temps_entities::ai_provider_keys::Model>::new()]) // list_active → none
            .into_connection();

        let db = Arc::new(db);
        let service = ProviderPreferenceService::new(db.clone(), test_provider_key_service(db));

        let resolved = service.resolve_active_agent_cli_provider().await;
        if let Some(id) = resolved {
            assert!(
                temps_agents::ai_cli::find_provider(&id).is_some(),
                "auto-default resolved to unknown provider id '{id}'"
            );
        }
    }
}
