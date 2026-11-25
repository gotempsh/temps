// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Coding-agent CLI endpoints (ADR 113): the latest release of Claude Code,
//! Codex and OpenCode, and the instance's automatic-update setting.
//!
//! The version installed in a workspace and the "Update" action are the
//! per-workspace `/settings/ai-providers/{provider_id}/harness-update`
//! endpoints. Turning automatic updates on lets Temps replace a binary in
//! every user's workspace, so it needs `settings:write` and `system:admin`.

use std::sync::Arc;

use axum::{
    extract::{Query, State},
    http::{header, StatusCode},
    response::IntoResponse,
    routing::{get, patch},
    Extension, Json, Router,
};
use serde::{Deserialize, Serialize};
use temps_authz::{permission_guard, AuthContext, RequireAuth};
use temps_core::problemdetails::{self, Problem, ProblemDetails};
use temps_core::{AuditOperation, RequestMetadata};
use tracing::error;
use utoipa::{IntoParams, OpenApi};

use crate::handlers::types::AiGatewayAppState;
use crate::services::coding_agents::{
    CodingAgentError, CodingAgentRelease, CodingAgentSettings, CodingAgentSettingsUpdate,
    CodingAgentsOverview,
};

pub(crate) const CODING_AGENT_SETTINGS_UPDATED: &str = "AI_CODING_AGENT_SETTINGS_UPDATED";

#[derive(OpenApi)]
#[openapi(
    paths(list_coding_agents, update_coding_agent_settings),
    components(schemas(
        CodingAgentsOverview,
        CodingAgentRelease,
        CodingAgentSettings,
        CodingAgentSettingsUpdate
    )),
    info(
        title = "AI Coding Agents API",
        description = "Coding-agent CLI releases and automatic updates",
        version = "1.0.0"
    ),
    tags((name = "AI Coding Agents", description = "Coding-agent CLI release endpoints"))
)]
pub struct AiCodingAgentsApiDoc;

pub fn configure_coding_agent_routes() -> Router<Arc<AiGatewayAppState>> {
    Router::new()
        .route("/ai/coding-agents", get(list_coding_agents))
        .route(
            "/ai/coding-agents/settings",
            patch(update_coding_agent_settings),
        )
}

impl From<CodingAgentError> for Problem {
    fn from(error: CodingAgentError) -> Self {
        let (status, title) = match &error {
            CodingAgentError::Registry { .. } => (StatusCode::BAD_GATEWAY, "Registry Unreachable"),
            CodingAgentError::RegistryTimeout { .. } => {
                (StatusCode::GATEWAY_TIMEOUT, "Registry Timed Out")
            }
            CodingAgentError::RegistryStatus { .. } => {
                (StatusCode::BAD_GATEWAY, "Registry Refused")
            }
            CodingAgentError::RegistryTooLarge { .. } => {
                (StatusCode::BAD_GATEWAY, "Registry Answer Too Large")
            }
            CodingAgentError::RegistryParse { .. } => {
                (StatusCode::BAD_GATEWAY, "Invalid Registry Answer")
            }
            CodingAgentError::Database(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Coding Agent Settings Error",
            ),
        };
        problemdetails::new(status)
            .with_title(title)
            .with_detail(error.to_string())
    }
}

#[derive(Debug, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct CodingAgentsQuery {
    /// Ask the npm registry again instead of using the six-hour cache
    /// (at most once a minute).
    #[serde(default)]
    pub refresh: bool,
}

/// Every coding-agent CLI Temps manages, its latest published release, and
/// whether newer releases are installed automatically.
#[utoipa::path(
    tag = "AI Coding Agents",
    get,
    path = "/ai/coding-agents",
    params(CodingAgentsQuery),
    responses(
        (status = 200, description = "Coding agents and their latest releases", body = CodingAgentsOverview),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Database error", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
async fn list_coding_agents(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AiGatewayAppState>>,
    Query(query): Query<CodingAgentsQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AiGatewayRead);
    let overview = app_state
        .coding_agent_service
        .overview(query.refresh)
        .await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(overview)))
}

fn require_agent_settings_writer(auth: &AuthContext) -> Result<(), Problem> {
    permission_guard!(auth, SettingsWrite);
    permission_guard!(auth, SystemAdmin);
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
struct CodingAgentSettingsAudit {
    user_id: Option<i32>,
    #[serde(skip)]
    ip_address: Option<String>,
    #[serde(skip)]
    user_agent: String,
    changed_fields: Vec<&'static str>,
    auto_update: bool,
}

impl AuditOperation for CodingAgentSettingsAudit {
    fn operation_type(&self) -> String {
        CODING_AGENT_SETTINGS_UPDATED.to_string()
    }

    fn user_id(&self) -> Option<i32> {
        self.user_id
    }

    fn ip_address(&self) -> Option<String> {
        self.ip_address.clone()
    }

    fn user_agent(&self) -> &str {
        &self.user_agent
    }

    fn serialize(&self) -> temps_core::AuditResult<String> {
        temps_core::serialize_audit_operation(self)
    }
}

/// Turn automatic coding-agent updates on or off.
#[utoipa::path(
    tag = "AI Coding Agents",
    patch,
    path = "/ai/coding-agents/settings",
    request_body = CodingAgentSettingsUpdate,
    responses(
        (status = 200, description = "Settings saved", body = CodingAgentSettings),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Database error", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
async fn update_coding_agent_settings(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AiGatewayAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(update): Json<CodingAgentSettingsUpdate>,
) -> Result<impl IntoResponse, Problem> {
    require_agent_settings_writer(&auth)?;
    let (settings, changed) = app_state
        .coding_agent_service
        .update_settings(update)
        .await?;
    if !changed.is_empty() {
        let audit = CodingAgentSettingsAudit {
            user_id: auth.user_id_opt(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
            changed_fields: changed,
            auto_update: settings.auto_update,
        };
        if let Err(audit_error) = app_state.audit_service.create_audit_log(&audit).await {
            error!(
                operation = CODING_AGENT_SETTINGS_UPDATED,
                error = %audit_error,
                "Failed to create audit log for coding-agent settings"
            );
        }
    }
    Ok(Json(settings))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use std::sync::Mutex;
    use temps_authz::permissions::Role;
    use temps_entities::users;
    use tower::ServiceExt;

    #[derive(Default)]
    struct RecordingAuditLogger {
        records: Mutex<Vec<(String, String)>>,
    }

    #[async_trait::async_trait]
    impl temps_core::AuditLogger for RecordingAuditLogger {
        async fn create_audit_log(
            &self,
            operation: &dyn AuditOperation,
        ) -> temps_core::AuditResult<()> {
            self.records
                .lock()
                .unwrap()
                .push((operation.operation_type(), operation.serialize()?));
            Ok(())
        }
    }

    fn auth(role: Role) -> AuthContext {
        let now = chrono::Utc::now();
        AuthContext::new_session(
            users::Model {
                id: 9,
                name: "Agent Tester".to_string(),
                email: "agent-tester@example.com".to_string(),
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
            },
            role,
        )
    }

    fn metadata() -> RequestMetadata {
        RequestMetadata {
            ip_address: "192.0.2.20".to_string(),
            user_agent: "agents-test".to_string(),
            headers: Default::default(),
            visitor_id_cookie: None,
            session_id_cookie: None,
            base_url: "https://temps.example.com".to_string(),
            scheme: "https".to_string(),
            host: "temps.example.com".to_string(),
            is_secure: true,
        }
    }

    fn app(
        db: Arc<sea_orm::DatabaseConnection>,
        audit: Arc<RecordingAuditLogger>,
        auth: Option<AuthContext>,
    ) -> Router {
        let state = crate::handlers::types::test_app_state(db, audit);
        let router = configure_coding_agent_routes()
            .with_state(state)
            .layer(Extension(metadata()));
        match auth {
            Some(auth) => router.layer(Extension(auth)),
            None => router,
        }
    }

    async fn call(
        app: Router,
        method: &str,
        uri: &str,
        body: Option<&str>,
    ) -> (StatusCode, String) {
        let mut request = Request::builder().method(method).uri(uri);
        if body.is_some() {
            request = request.header("content-type", "application/json");
        }
        let response = app
            .oneshot(
                request
                    .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    fn disconnected() -> Arc<sea_orm::DatabaseConnection> {
        Arc::new(sea_orm::DatabaseConnection::Disconnected)
    }

    #[tokio::test]
    async fn every_route_needs_a_signed_in_caller() {
        for (method, uri, body) in [
            ("GET", "/ai/coding-agents", None),
            ("PATCH", "/ai/coding-agents/settings", Some("{}")),
        ] {
            let audit = Arc::new(RecordingAuditLogger::default());
            let (status, _) = call(app(disconnected(), audit, None), method, uri, body).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri}");
        }
    }

    #[tokio::test]
    async fn only_operators_may_turn_on_automatic_updates() {
        for role in [Role::User, Role::Reader] {
            let audit = Arc::new(RecordingAuditLogger::default());
            let (status, body) = call(
                app(disconnected(), audit.clone(), Some(auth(role.clone()))),
                "PATCH",
                "/ai/coding-agents/settings",
                Some(r#"{"auto_update":true}"#),
            )
            .await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{role:?}");
            assert!(body.contains("Insufficient Permissions"), "{body}");
            assert!(audit.records.lock().unwrap().is_empty());
        }
        // `settings:write` without `system:admin`.
        let mut confined = auth(Role::Reader);
        confined.custom_permissions =
            Some(vec![temps_authz::permissions::Permission::SettingsWrite]);
        let audit = Arc::new(RecordingAuditLogger::default());
        let (status, _) = call(
            app(disconnected(), audit, Some(confined)),
            "PATCH",
            "/ai/coding-agents/settings",
            Some(r#"{"auto_update":true}"#),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[test]
    fn every_coding_agent_error_maps_to_its_status() {
        let package = || "opencode-ai".to_string();
        for (error, status) in [
            (
                CodingAgentError::Registry {
                    package: package(),
                    reason: "x".into(),
                },
                StatusCode::BAD_GATEWAY,
            ),
            (
                CodingAgentError::RegistryTimeout {
                    package: package(),
                    seconds: 10,
                },
                StatusCode::GATEWAY_TIMEOUT,
            ),
            (
                CodingAgentError::RegistryStatus {
                    package: package(),
                    status: 503,
                },
                StatusCode::BAD_GATEWAY,
            ),
            (
                CodingAgentError::RegistryTooLarge {
                    package: package(),
                    limit_bytes: 1,
                },
                StatusCode::BAD_GATEWAY,
            ),
            (
                CodingAgentError::RegistryParse {
                    package: package(),
                    reason: "x".into(),
                },
                StatusCode::BAD_GATEWAY,
            ),
            (
                CodingAgentError::Database(sea_orm::DbErr::Custom("down".into())),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
        ] {
            let detail = error.to_string();
            let problem = Problem::from(error);
            assert_eq!(problem.status_code, status, "{detail}");
            assert_eq!(
                problem.body.get("detail").and_then(|value| value.as_str()),
                Some(detail.as_str())
            );
        }
    }

    #[tokio::test]
    async fn an_operator_turns_on_automatic_updates_and_it_is_audited_once() {
        let test_db = match temps_test_utils::TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(error)
                if temps_test_utils::is_container_runtime_unavailable(&format!("{error:#}")) =>
            {
                println!(
                    "Skipping: no container runtime for the PostgreSQL test database: {error:#}"
                );
                return;
            }
            Err(error) => panic!("test database setup failed: {error:#}"),
        };
        let db = test_db.connection_arc();
        let audit = Arc::new(RecordingAuditLogger::default());
        for _ in 0..2 {
            let (status, body) = call(
                app(db.clone(), audit.clone(), Some(auth(Role::Admin))),
                "PATCH",
                "/ai/coding-agents/settings",
                Some(r#"{"auto_update":true}"#),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{body}");
            let saved: serde_json::Value = serde_json::from_str(&body).unwrap();
            assert_eq!(saved["auto_update"], true);
        }
        let records = audit.records.lock().unwrap().clone();
        assert_eq!(records.len(), 1, "an unchanged save is not audited");
        assert_eq!(records[0].0, CODING_AGENT_SETTINGS_UPDATED);
        assert!(
            records[0].1.contains(r#""changed_fields":["auto_update"]"#),
            "{}",
            records[0].1
        );
    }
}
