pub mod ai_providers;
pub mod autofixer;
pub mod config;
pub mod definitions;
pub mod preview_gateway;
pub mod runs;
pub mod secrets;
pub mod trigger;
pub mod workflows;

use axum::Router;
use std::sync::Arc;
use utoipa::OpenApi;

use crate::services::autofixer::AutofixerService;
use crate::services::config_service::AgentConfigService;
use crate::services::definition_service::DefinitionService;
use crate::services::executor::AgentExecutor;
use crate::services::run_service::AgentRunService;
use crate::services::secret_service::SecretService;

/// OpenAPI document for the temps-agents crate (`/projects/{project_id}/agents/*`,
/// `/projects/{project_id}/autofixer/*`, etc.).
///
/// This document is merged into the unified Temps OpenAPI at
/// `/api-docs/openapi.json` via the plugin system's `openapi_schema()` hook.
/// External SDK generators (`bun openapi-ts`) and compatibility tests should
/// fetch the unified doc and filter by the `Agents` tag.
///
/// When you add a decorated handler, register it here in `paths(...)` and
/// add any new DTOs to `components(schemas(...))` — otherwise the SDK
/// codegen will silently miss it. The guardrail test below enforces this.
#[derive(OpenApi)]
#[openapi(
    paths(
        // Agent CRUD
        config::list_agents,
        config::create_agent,
        config::get_agent,
        config::update_agent,
        config::delete_agent,

        // Agent triggers
        trigger::trigger_agent,
        trigger::webhook_trigger,
        trigger::get_cli_status,
        trigger::get_sandbox_status,
        trigger::get_global_sandbox_status,
        trigger::rebuild_sandbox_image,
        trigger::smoke_test_agent,
        trigger::save_agent_token,

        // Agent runs
        runs::list_all_runs,
        runs::latest_run_for_source,
        runs::list_agent_runs,
        runs::get_run_with_logs,
        runs::stream_run_events,
        runs::cancel_run,
        runs::retry_run,

        // Agent secrets
        secrets::list_secrets,
        secrets::upsert_secret,
        secrets::delete_secret,

        // Ephemeral workflow dry-run (CLI)
        workflows::workflow_dry_run,

        // Autofixer interactive flow
        autofixer::start_analysis,
        autofixer::get_run,
        autofixer::stream_events,
        autofixer::add_context,
        autofixer::start_fix,
        autofixer::create_pr,
        autofixer::re_analyze,
        autofixer::cancel,

        // Preview gateway (settings page)
        preview_gateway::get_preview_gateway_status,
        preview_gateway::get_preview_gateway_logs,
        preview_gateway::restart_preview_gateway,
        preview_gateway::upgrade_preview_gateway,
        preview_gateway::get_preview_gateway_settings,
        preview_gateway::patch_preview_gateway_settings,

        // Skills + MCP definitions (project-scoped)
        definitions::list_skills,
        definitions::create_skill,
        definitions::get_skill,
        definitions::update_skill,
        definitions::delete_skill,
        definitions::upload_skill,
        definitions::download_skill_archive,
        definitions::list_mcps,
        definitions::create_mcp,
        definitions::get_mcp,
        definitions::update_mcp,
        definitions::delete_mcp,

        // Skills + MCP definitions (global / settings)
        definitions::list_global_skills,
        definitions::create_global_skill,
        definitions::get_global_skill,
        definitions::update_global_skill,
        definitions::delete_global_skill,
        definitions::upload_global_skill,
        definitions::download_global_skill_archive,
        definitions::list_global_mcps,
        definitions::create_global_mcp,
        definitions::get_global_mcp,
        definitions::update_global_mcp,
        definitions::delete_global_mcp,

        // AI providers
        ai_providers::list_ai_providers,
        ai_providers::save_ai_provider_credential,
        ai_providers::activate_ai_provider,
        ai_providers::update_ai_provider,
    ),
    components(schemas(
        // Agent config
        config::AgentConfigResponse,
        config::ListAgentsResponse,
        crate::services::config_service::UpsertAgentRequest,

        // Trigger + sandbox status
        trigger::TriggerAgentRequest,
        trigger::WebhookTriggerRequest,
        trigger::WebhookTriggerResponse,
        trigger::SandboxStatusResponse,
        trigger::SmokeTestResponse,
        trigger::SaveAgentTokenRequest,
        trigger::SaveAgentTokenResponse,

        // Runs
        runs::AgentRunResponse,
        runs::AgentRunLogResponse,
        runs::AgentRunWithLogsResponse,
        runs::ListRunsResponse,

        // Secrets
        secrets::UpsertSecretRequest,
        secrets::SecretResponse,
        secrets::ListSecretsResponse,

        // Workflow dry-run
        workflows::WorkflowDryRunRequest,

        // Autofixer
        autofixer::StartAnalysisRequest,
        autofixer::AddContextRequest,
        autofixer::AutofixerRunResponse,
        autofixer::AutofixerRunWithLogsResponse,
        autofixer::CreatePrResponse,

        // Preview gateway
        preview_gateway::LogsQuery,
        preview_gateway::LogsResponse,
        preview_gateway::UpgradeRequest,
        preview_gateway::PreviewGatewaySettingsResponse,
        preview_gateway::PatchSettingsRequest,
        crate::preview_gateway::GatewayStatus,

        // Skill definitions
        definitions::SkillDefinitionResponse,
        definitions::ListSkillsResponse,
        definitions::CreateSkillRequest,
        definitions::UpdateSkillRequest,

        // MCP server definitions
        definitions::McpDefinitionResponse,
        definitions::ListMcpsResponse,
        definitions::CreateMcpRequest,
        definitions::UpdateMcpRequest,

        // AI providers
        ai_providers::AuthFlavorDto,
        ai_providers::ProviderCatalogDto,
        ai_providers::ProviderCatalogResponse,
        ai_providers::SaveCredentialRequest,
        ai_providers::SaveCredentialResponse,
        ai_providers::ActivateProviderResponse,
        ai_providers::UpdateProviderRequest,
        ai_providers::UpdateProviderResponse,
    )),
    tags(
        (name = "Agents", description = "Autonomous AI agents, autofixer (interactive AI debugging), skills/MCP definitions, and preview gateway management.")
    )
)]
pub struct AgentsApiDoc;

pub struct AppState {
    pub db: Arc<sea_orm::DatabaseConnection>,
    pub encryption_service: Arc<temps_core::EncryptionService>,
    pub config_service: Arc<AgentConfigService>,
    pub run_service: Arc<AgentRunService>,
    pub executor: Arc<AgentExecutor>,
    pub audit_service: Arc<dyn temps_core::AuditLogger>,
    pub autofixer_service: Arc<AutofixerService>,
    pub secret_service: Arc<SecretService>,
    pub definition_service: Arc<DefinitionService>,
    /// Docker client used by the preview gateway supervisor handlers.
    pub docker: Arc<bollard::Docker>,
    /// Platform settings service used by the preview gateway handlers to
    /// persist image / auto-upgrade changes.
    pub platform_config_service: Arc<temps_config::ConfigService>,
    /// Anonymous product-telemetry reporter. Fire-and-forget; never fails the
    /// surrounding request. Defaults to a no-op when telemetry is not registered.
    pub telemetry: Arc<dyn temps_core::TelemetryReporter>,
}

pub fn configure_routes() -> Router<Arc<AppState>> {
    Router::new()
        .merge(ai_providers::routes())
        .merge(autofixer::routes())
        .merge(config::routes())
        .merge(definitions::routes())
        .merge(preview_gateway::routes())
        .merge(runs::routes())
        .merge(secrets::routes())
        .merge(trigger::routes())
        .merge(workflows::routes())
}
