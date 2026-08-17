use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, put},
    Extension, Json, Router,
};
use serde::{Deserialize, Serialize};
use temps_auth::{permission_guard, permissions::Role, AuthContext, RequireAuth};
use temps_core::audit::{AuditContext, AuditOperation};
use temps_core::error_builder::ErrorBuilder;
use temps_core::problemdetails::{PermissionDenialKind, Problem, ProblemDetails};
use temps_core::RequestMetadata;
use utoipa::{OpenApi, ToSchema};

use crate::handlers::types::AiGatewayAppState;

/// AI gateway governance policies (model allowlists, RPM limits, budgets) are
/// operator-only, by design: they apply to every request in a scope
/// regardless of who owns the project, so `AiGatewayWrite` alone is too broad
/// a gate — `Role::User` holds it for their own projects' day-to-day AI usage,
/// not for setting platform-wide or another tenant's spend limits. Require
/// instance-administrator privileges on top of the permission check, the same
/// bar `project_access_guard!` already uses for instance-scoped bypasses.
fn require_operator(auth: &AuthContext) -> Result<(), Problem> {
    if auth.is_admin() || auth.has_role(&Role::PlatformAdmin) {
        return Ok(());
    }
    Err(ErrorBuilder::new(StatusCode::FORBIDDEN)
        .type_("https://temps.sh/probs/insufficient-permissions")
        .title("Operator Privileges Required")
        .detail(
            "AI gateway governance policies are operator-only: configuring or viewing them \
             requires an instance administrator, regardless of AI gateway write access on your \
             own projects.",
        )
        .value("user_role", auth.effective_role.to_string())
        .permission_denial(PermissionDenialKind::InsufficientPermission, None)
        .build())
}

#[derive(OpenApi)]
#[openapi(
    paths(list_governance_configs, upsert_governance_config, delete_governance_config),
    components(schemas(GovernanceConfigResponse, UpsertGovernanceConfigRequest)),
    info(
        title = "AI Gateway Governance API",
        description = "Configure instance, project, environment, and deployment-token AI gateway limits",
        version = "1.0.0"
    ),
    tags((name = "AI Gateway Governance", description = "AI model, rate, and budget policy"))
)]
pub struct AiGatewayGovernanceApiDoc;

pub fn configure_governance_routes() -> Router<Arc<AiGatewayAppState>> {
    Router::new()
        .route("/ai/governance", get(list_governance_configs))
        .route(
            "/ai/governance/{scope}",
            put(upsert_governance_config).delete(delete_governance_config),
        )
}

#[derive(Debug, Serialize, ToSchema)]
pub struct GovernanceConfigResponse {
    pub id: i32,
    pub scope: String,
    pub allowed_models: Option<Vec<String>>,
    pub max_requests_per_minute: Option<i64>,
    pub max_cost_per_month_microcents: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
}

impl From<temps_entities::ai_gateway_config::Model> for GovernanceConfigResponse {
    fn from(model: temps_entities::ai_gateway_config::Model) -> Self {
        Self {
            id: model.id,
            scope: model.scope,
            allowed_models: model.allowed_models.and_then(|value| {
                value.as_array().map(|models| {
                    models
                        .iter()
                        .filter_map(|model| model.as_str().map(String::from))
                        .collect()
                })
            }),
            max_requests_per_minute: model.max_requests_per_minute,
            max_cost_per_month_microcents: model.max_cost_per_month_microcents,
            created_at: model.created_at.to_rfc3339(),
            updated_at: model.updated_at.to_rfc3339(),
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpsertGovernanceConfigRequest {
    /// NULL allows every model; an empty array denies every model.
    pub allowed_models: Option<Vec<String>>,
    /// NULL disables the request-rate limit for this scope.
    pub max_requests_per_minute: Option<i64>,
    /// NULL disables the operator-funded monthly budget for this scope.
    pub max_cost_per_month_microcents: Option<i64>,
}

#[derive(Debug, Serialize)]
struct GovernanceConfigAudit {
    context: AuditContext,
    action: String,
    scope: String,
}

impl AuditOperation for GovernanceConfigAudit {
    fn operation_type(&self) -> String {
        format!("AI_GATEWAY_GOVERNANCE_{}", self.action)
    }

    fn user_id(&self) -> Option<i32> {
        Some(self.context.user_id)
    }

    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }

    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }

    fn serialize(&self) -> temps_core::anyhow::Result<String> {
        serde_json::to_string(self).map_err(|error| {
            temps_core::anyhow::anyhow!(
                "Failed to serialize AI gateway governance audit for scope '{}': {}",
                self.scope,
                error
            )
        })
    }
}

#[utoipa::path(
    tag = "AI Gateway Governance",
    get,
    path = "/ai/governance",
    responses(
        (status = 200, body = Vec<GovernanceConfigResponse>),
        (status = 401, body = ProblemDetails),
        (status = 403, body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
async fn list_governance_configs(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AiGatewayAppState>>,
) -> Result<impl IntoResponse, Problem> {
    // Governance policies expose budget and allowlist details and therefore
    // use the same operator-only permission as mutations.
    permission_guard!(auth, AiGatewayWrite);
    require_operator(&auth)?;
    let configs = app_state.governance_service.list_configs().await?;
    Ok(Json(
        configs
            .into_iter()
            .map(GovernanceConfigResponse::from)
            .collect::<Vec<_>>(),
    ))
}

#[utoipa::path(
    tag = "AI Gateway Governance",
    put,
    path = "/ai/governance/{scope}",
    params(("scope" = String, Path, description = "instance, project:<id>, environment:<id>, or token:<id>")),
    request_body = UpsertGovernanceConfigRequest,
    responses(
        (status = 200, body = GovernanceConfigResponse),
        (status = 400, body = ProblemDetails),
        (status = 401, body = ProblemDetails),
        (status = 403, body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
async fn upsert_governance_config(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AiGatewayAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(scope): Path<String>,
    Json(request): Json<UpsertGovernanceConfigRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AiGatewayWrite);
    require_operator(&auth)?;

    let allowed_models = request
        .allowed_models
        .map(|models| serde_json::json!(models));
    let config = app_state
        .governance_service
        .upsert_config(
            &scope,
            allowed_models,
            request.max_requests_per_minute,
            request.max_cost_per_month_microcents,
        )
        .await?;

    audit_change(&app_state, &auth, &metadata, "UPSERTED", &scope).await;
    Ok(Json(GovernanceConfigResponse::from(config)))
}

#[utoipa::path(
    tag = "AI Gateway Governance",
    delete,
    path = "/ai/governance/{scope}",
    params(("scope" = String, Path, description = "instance, project:<id>, environment:<id>, or token:<id>")),
    responses(
        (status = 204),
        (status = 401, body = ProblemDetails),
        (status = 403, body = ProblemDetails),
        (status = 404, body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
async fn delete_governance_config(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AiGatewayAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(scope): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AiGatewayWrite);
    require_operator(&auth)?;
    app_state.governance_service.delete_config(&scope).await?;
    audit_change(&app_state, &auth, &metadata, "DELETED", &scope).await;
    Ok(StatusCode::NO_CONTENT)
}

async fn audit_change(
    app_state: &AiGatewayAppState,
    auth: &temps_auth::AuthContext,
    metadata: &RequestMetadata,
    action: &str,
    scope: &str,
) {
    let audit = GovernanceConfigAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        action: action.to_string(),
        scope: scope.to_string(),
    };
    if let Err(error) = app_state.audit_service.create_audit_log(&audit).await {
        tracing::error!(
            error = %error,
            scope,
            action,
            "Failed to create AI gateway governance audit log"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::State;
    use temps_auth::permissions::{Permission, Role};
    use temps_auth::RequireAuth;
    use temps_core::telemetry::NoopTelemetryReporter;
    use temps_core::EncryptionService;

    // ── helpers ──────────────────────────────────────────────────────────────

    fn test_user(id: i32) -> temps_entities::users::Model {
        let now = chrono::Utc::now();
        temps_entities::users::Model {
            id,
            name: "Test User".to_string(),
            email: format!("user{id}@example.com"),
            password_hash: None,
            email_verified: true,
            email_verification_token: None,
            email_verification_expires: None,
            password_reset_token: None,
            password_reset_expires: None,
            must_change_password: false,
            deleted_at: None,
            mfa_secret: None,
            mfa_enabled: false,
            mfa_recovery_codes: None,
            oidc_subject: None,
            oidc_provider_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn user_auth_with_role(role: Role) -> temps_auth::AuthContext {
        temps_auth::AuthContext::new_session(test_user(1), role)
    }

    /// Returns an AuthContext that has `AiGatewayWrite` explicitly granted but is
    /// not an admin or platform-admin.  We use an API-key context with an
    /// explicit permission list because `Role::User` does NOT include
    /// `AiGatewayWrite` in its built-in permission set; only `Admin` and
    /// `PlatformAdmin` carry it by default.  The API-key path lets us grant the
    /// single permission without elevating the caller to an operator role, which
    /// is exactly the case `require_operator` must reject.
    fn non_admin_ai_write_auth() -> temps_auth::AuthContext {
        temps_auth::AuthContext::new_api_key(
            test_user(1),
            None, // no role → effective_role = Role::Custom, not admin
            Some(vec![Permission::AiGatewayWrite]),
            "test-ai-write-key".to_string(),
            1,
        )
    }

    struct NoopAuditLogger;

    #[async_trait::async_trait]
    impl temps_core::AuditLogger for NoopAuditLogger {
        async fn create_audit_log(&self, _: &dyn temps_core::AuditOperation) -> anyhow::Result<()> {
            Ok(())
        }
    }

    /// Minimal stub `AiService` that reports itself as unavailable.
    struct StubAiService;

    #[async_trait::async_trait]
    impl temps_ai::service::AiService for StubAiService {
        async fn is_available(&self) -> bool {
            false
        }

        async fn complete(
            &self,
            _: temps_ai::service::AiRequest,
        ) -> Result<temps_ai::service::AiResponse, temps_ai::service::AiError> {
            Err(temps_ai::service::AiError::NotAvailable)
        }

        async fn chat_stream(
            &self,
            _: temps_ai::streaming::ChatTurnRequest,
        ) -> Result<temps_ai::streaming::TokenStream, temps_ai::service::AiError> {
            Err(temps_ai::service::AiError::NotAvailable)
        }
    }

    /// Build a minimal `AiGatewayAppState` backed by a disconnected database.
    /// Auth guard checks happen before any service method is called, so the
    /// disconnected DB never triggers an actual query in these tests.
    fn minimal_state() -> Arc<crate::handlers::types::AiGatewayAppState> {
        use crate::services::{
            GatewayService, GovernanceService, ProviderKeyService, ProviderModelService,
            ProviderPreferenceService, StructuredOutputService, UsageService,
        };

        let db = Arc::new(sea_orm::DatabaseConnection::Disconnected);
        let encryption = Arc::new(EncryptionService::new_from_password(
            "test-key-for-governance-tests",
        ));
        let provider_key_service = Arc::new(ProviderKeyService::new(db.clone(), encryption));
        let gateway_service = Arc::new(GatewayService::new(provider_key_service.clone()));
        let provider_model_service = Arc::new(ProviderModelService::new(
            db.clone(),
            provider_key_service.clone(),
            gateway_service.clone(),
        ));
        let provider_preference_service = Arc::new(ProviderPreferenceService::new(
            db.clone(),
            provider_key_service.clone(),
        ));
        let usage_service = Arc::new(UsageService::new(db.clone()));
        let governance_service = Arc::new(GovernanceService::new(db.clone()));
        let audit_service = Arc::new(NoopAuditLogger) as Arc<dyn temps_core::AuditLogger>;
        let telemetry =
            Arc::new(NoopTelemetryReporter) as Arc<dyn temps_core::telemetry::TelemetryReporter>;
        let structured_output_service =
            Arc::new(StructuredOutputService::new(Arc::new(StubAiService)));

        Arc::new(crate::handlers::types::AiGatewayAppState {
            db,
            gateway_service,
            provider_key_service,
            provider_model_service,
            provider_preference_service,
            usage_service,
            governance_service,
            audit_service,
            telemetry,
            provider_status_cache: Arc::new(
                crate::handlers::provider_status::AiProviderStatusCache::default(),
            ),
            structured_output_service,
            project_access_checker: None,
        })
    }

    fn assert_problem_status(problem: &Problem, expected_status: axum::http::StatusCode) {
        use axum::response::IntoResponse;
        let resp = problem.clone().into_response();
        assert_eq!(
            resp.status(),
            expected_status,
            "expected HTTP {expected_status}"
        );
    }

    // ── require_operator unit tests ───────────────────────────────────────────

    #[test]
    fn require_operator_allows_admin() {
        let auth = user_auth_with_role(Role::Admin);
        assert!(require_operator(&auth).is_ok());
    }

    #[test]
    fn require_operator_allows_platform_admin() {
        let auth = user_auth_with_role(Role::PlatformAdmin);
        assert!(require_operator(&auth).is_ok());
    }

    #[test]
    fn require_operator_rejects_regular_user() {
        let auth = user_auth_with_role(Role::User);
        match require_operator(&auth) {
            Err(ref problem) => assert_problem_status(problem, axum::http::StatusCode::FORBIDDEN),
            Ok(_) => panic!("expected Err(403) but got Ok"),
        }
    }

    // ── handler-level auth tests ──────────────────────────────────────────────

    /// A regular user (Role::User) has `AiGatewayWrite` permission on their own
    /// projects but must be denied governance endpoints because they are not an
    /// instance administrator.
    #[tokio::test]
    async fn list_governance_configs_rejects_non_admin_with_ai_gateway_write() {
        let auth = non_admin_ai_write_auth();
        // Confirm the auth context DOES have AiGatewayWrite (so permission_guard
        // passes) but is NOT an admin (so require_operator rejects it).
        assert!(auth.has_permission(&Permission::AiGatewayWrite));
        assert!(!auth.is_admin());

        match list_governance_configs(RequireAuth(auth), State(minimal_state())).await {
            Err(ref problem) => assert_problem_status(problem, axum::http::StatusCode::FORBIDDEN),
            Ok(_) => panic!("expected Err(403) but got Ok"),
        }
    }

    #[tokio::test]
    async fn upsert_governance_config_rejects_non_admin_with_ai_gateway_write() {
        let auth = non_admin_ai_write_auth();
        let metadata = temps_core::RequestMetadata {
            ip_address: "127.0.0.1".to_string(),
            user_agent: String::new(),
            headers: axum::http::HeaderMap::new(),
            visitor_id_cookie: None,
            session_id_cookie: None,
            base_url: "https://localhost".to_string(),
            scheme: "https".to_string(),
            host: "localhost".to_string(),
            is_secure: true,
        };

        match upsert_governance_config(
            RequireAuth(auth),
            State(minimal_state()),
            Extension(metadata),
            Path("instance".to_string()),
            Json(UpsertGovernanceConfigRequest {
                allowed_models: None,
                max_requests_per_minute: None,
                max_cost_per_month_microcents: None,
            }),
        )
        .await
        {
            Err(ref problem) => assert_problem_status(problem, axum::http::StatusCode::FORBIDDEN),
            Ok(_) => panic!("expected Err(403) but got Ok"),
        }
    }

    #[tokio::test]
    async fn delete_governance_config_rejects_non_admin_with_ai_gateway_write() {
        let auth = non_admin_ai_write_auth();
        let metadata = temps_core::RequestMetadata {
            ip_address: "127.0.0.1".to_string(),
            user_agent: String::new(),
            headers: axum::http::HeaderMap::new(),
            visitor_id_cookie: None,
            session_id_cookie: None,
            base_url: "https://localhost".to_string(),
            scheme: "https".to_string(),
            host: "localhost".to_string(),
            is_secure: true,
        };

        match delete_governance_config(
            RequireAuth(auth),
            State(minimal_state()),
            Extension(metadata),
            Path("instance".to_string()),
        )
        .await
        {
            Err(ref problem) => assert_problem_status(problem, axum::http::StatusCode::FORBIDDEN),
            Ok(_) => panic!("expected Err(403) but got Ok"),
        }
    }

    /// A user with no permissions at all gets 403 from `permission_guard!` (which
    /// enforces `AiGatewayWrite`) before `require_operator` even runs.
    #[tokio::test]
    async fn list_governance_configs_rejects_user_without_ai_gateway_write() {
        // Role::Custom with no permissions has no AiGatewayWrite.
        let auth = temps_auth::AuthContext::new_api_key(
            test_user(2),
            None,
            Some(vec![]), // explicit empty permission list
            "empty-key".to_string(),
            99,
        );
        assert!(!auth.has_permission(&Permission::AiGatewayWrite));

        match list_governance_configs(RequireAuth(auth), State(minimal_state())).await {
            Err(ref problem) => assert_problem_status(problem, axum::http::StatusCode::FORBIDDEN),
            Ok(_) => panic!("expected Err(403) but got Ok"),
        }
    }

    // ── existing struct test ──────────────────────────────────────────────────

    #[test]
    fn config_response_preserves_scope_and_limits() {
        let response = GovernanceConfigResponse::from(temps_entities::ai_gateway_config::Model {
            id: 1,
            scope: "project:7".to_string(),
            allowed_models: Some(serde_json::json!(["gpt-5-mini"])),
            max_requests_per_minute: Some(60),
            max_cost_per_month_microcents: Some(1_000_000),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            provider_type: "gateway".to_string(),
            agent_cli_provider_id: None,
            interactive_bridge_enabled: false,
            summary_provider_id: None,
            summary_model: None,
            summary_thinking_level: None,
        });
        assert_eq!(response.scope, "project:7");
        assert_eq!(
            response.allowed_models,
            Some(vec!["gpt-5-mini".to_string()])
        );
        assert_eq!(response.max_requests_per_minute, Some(60));
    }
}
