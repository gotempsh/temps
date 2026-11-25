// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `GET` and `PATCH /settings/onboarding`: the install's first-run record
//! (whether the welcome flow was finished, the steps left for later and why,
//! and the default way in). See [`temps_config::onboarding`].

use axum::{
    extract::{Extension, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use std::sync::Arc;
use temps_auth::{permission_guard, RequireAuth};
use temps_config::onboarding::{
    DefaultReach, OnboardingError, OnboardingLaterInput, OnboardingLaterStep, OnboardingPatch,
    OnboardingService, OnboardingState, OnboardingStep, ReachAudience, ReachNaming, ReachWay,
};
use temps_core::error_builder::ErrorBuilder;
use temps_core::{
    problemdetails::Problem, AuditContext, AuditLogger, AuditOperation, RequestMetadata,
};
use tracing::error;
use utoipa::OpenApi;

pub struct OnboardingHandlerState {
    pub onboarding_service: Arc<OnboardingService>,
    pub audit_service: Arc<dyn AuditLogger>,
}

/// Router state for the onboarding endpoints.
pub type OnboardingRouterState = Arc<OnboardingHandlerState>;

/// The Problem for an onboarding error; every variant explicitly.
fn onboarding_problem(error: OnboardingError) -> Problem {
    {
        match error {
            OnboardingError::Validation { .. } => ErrorBuilder::new(StatusCode::BAD_REQUEST)
                .type_("https://temps.sh/probs/onboarding-invalid")
                .title("Invalid Onboarding Update")
                .detail(error.to_string())
                .build(),
            OnboardingError::Database(_) => {
                error!(%error, "Failed to read or write the onboarding record");
                ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .type_("https://temps.sh/probs/onboarding-storage")
                    .title("Onboarding Record Unavailable")
                    .detail(error.to_string())
                    .build()
            }
            OnboardingError::Serialization(_) => {
                error!(%error, "Failed to serialize the onboarding record");
                ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .type_("https://temps.sh/probs/onboarding-storage")
                    .title("Onboarding Record Unavailable")
                    .detail(error.to_string())
                    .build()
            }
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
struct OnboardingUpdatedAudit {
    context: AuditContext,
    /// What the request asked for. Holds no secrets: steps, reasons, the way
    /// in and a domain.
    patch: OnboardingPatch,
    completed_before: bool,
    completed_after: bool,
}

impl AuditOperation for OnboardingUpdatedAudit {
    fn operation_type(&self) -> String {
        "ONBOARDING_UPDATED".to_string()
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
    fn serialize(&self) -> temps_core::AuditResult<String> {
        temps_core::serialize_audit_operation(self)
    }
}

#[derive(OpenApi)]
#[openapi(
    paths(get_onboarding, update_onboarding),
    components(schemas(
        OnboardingState,
        OnboardingPatch,
        OnboardingLaterStep,
        OnboardingLaterInput,
        OnboardingStep,
        DefaultReach,
        ReachAudience,
        ReachWay,
        ReachNaming,
    ))
)]
pub struct OnboardingApiDoc;

pub fn configure_onboarding_routes() -> Router<OnboardingRouterState> {
    Router::new().route(
        "/settings/onboarding",
        get(get_onboarding).patch(update_onboarding),
    )
}

/// The install's first-run record.
///
/// `completed_at` is `null` until the welcome flow is finished or skipped; the
/// console sends an instance admin with no projects to `/welcome` until then.
#[utoipa::path(
    tag = "Settings",
    get,
    path = "/settings/onboarding",
    responses(
        (status = 200, description = "The onboarding record", body = OnboardingState),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "The record could not be read")
    ),
    security(("bearer_auth" = []))
)]
async fn get_onboarding(
    RequireAuth(auth): RequireAuth,
    State(state): State<OnboardingRouterState>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsRead);
    let record = state
        .onboarding_service
        .get()
        .await
        .map_err(onboarding_problem)?;
    Ok(Json(record))
}

/// Update the first-run record: finish or reopen the flow, mark steps done,
/// leave steps for later with a reason, set the default way in.
#[utoipa::path(
    tag = "Settings",
    patch,
    path = "/settings/onboarding",
    request_body = OnboardingPatch,
    responses(
        (status = 200, description = "The updated record", body = OnboardingState),
        (status = 400, description = "The update is invalid; the detail says which field and why"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "The record could not be written")
    ),
    security(("bearer_auth" = []))
)]
async fn update_onboarding(
    RequireAuth(auth): RequireAuth,
    State(state): State<OnboardingRouterState>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(patch): Json<OnboardingPatch>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);
    let (before, after) = state
        .onboarding_service
        .update(patch.clone())
        .await
        .map_err(onboarding_problem)?;

    let audit = OnboardingUpdatedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        patch,
        completed_before: before.completed_at.is_some(),
        completed_after: after.completed_at.is_some(),
    };
    if let Err(error) = state.audit_service.create_audit_log(&audit).await {
        error!(%error, "Failed to record the onboarding update audit log");
    }
    Ok(Json(after))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{HeaderMap, Request};
    use sea_orm::{DatabaseBackend, DatabaseConnection, MockDatabase, MockExecResult};
    use std::sync::Mutex;
    use tower::ServiceExt;

    #[derive(Default)]
    struct RecordingAudit {
        records: Mutex<Vec<(String, String)>>,
    }

    #[async_trait::async_trait]
    impl AuditLogger for RecordingAudit {
        async fn create_audit_log(
            &self,
            operation: &dyn AuditOperation,
        ) -> temps_core::AuditResult<()> {
            self.records
                .lock()
                .expect("audit recorder")
                .push((operation.operation_type(), operation.serialize()?));
            Ok(())
        }
    }

    fn user() -> temps_entities::users::Model {
        temps_entities::users::Model {
            id: 1,
            name: "Admin".to_string(),
            email: "admin@example.com".to_string(),
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
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    fn admin() -> temps_auth::AuthContext {
        temps_auth::AuthContext::new_persisted_session(user(), temps_auth::Role::Admin, 42)
    }

    fn settings_reader() -> temps_auth::AuthContext {
        temps_auth::AuthContext::new_api_key(
            user(),
            None,
            Some(vec![temps_auth::Permission::SettingsRead]),
            "settings-reader".to_string(),
            7,
        )
    }

    fn outsider() -> temps_auth::AuthContext {
        temps_auth::AuthContext::new_api_key(
            user(),
            None,
            Some(vec![temps_auth::Permission::ProjectsRead]),
            "projects-only".to_string(),
            8,
        )
    }

    fn metadata() -> RequestMetadata {
        RequestMetadata {
            ip_address: "203.0.113.9".to_string(),
            user_agent: "onboarding-test".to_string(),
            headers: HeaderMap::new(),
            visitor_id_cookie: None,
            session_id_cookie: None,
            base_url: "https://cp.example.com".to_string(),
            scheme: "https".to_string(),
            host: "cp.example.com".to_string(),
            is_secure: true,
        }
    }

    fn settings_row(data: serde_json::Value) -> temps_entities::settings::Model {
        temps_entities::settings::Model {
            id: 1,
            data,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    fn app(db: DatabaseConnection, audit: Arc<RecordingAudit>) -> Router {
        configure_onboarding_routes().with_state(Arc::new(OnboardingHandlerState {
            onboarding_service: Arc::new(OnboardingService::new(Arc::new(db))),
            audit_service: audit,
        }))
    }

    fn empty_db() -> DatabaseConnection {
        MockDatabase::new(DatabaseBackend::Postgres).into_connection()
    }

    async fn send(
        app: Router,
        method: &str,
        body: Option<serde_json::Value>,
        auth: Option<temps_auth::AuthContext>,
    ) -> (StatusCode, serde_json::Value) {
        let mut request = Request::builder()
            .method(method)
            .uri("/settings/onboarding");
        if body.is_some() {
            request = request.header("content-type", "application/json");
        }
        let mut request = request
            .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
            .expect("request");
        request.extensions_mut().insert(metadata());
        if let Some(auth) = auth {
            request.extensions_mut().insert(auth);
        }
        let response = app.oneshot(request).await.expect("response");
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, json)
    }

    #[tokio::test]
    async fn both_endpoints_require_authentication() {
        for (method, body) in [
            ("GET", None),
            ("PATCH", Some(serde_json::json!({"completed": true}))),
        ] {
            let audit = Arc::new(RecordingAudit::default());
            let (status, _) = send(app(empty_db(), audit.clone()), method, body, None).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{method}");
            assert!(audit.records.lock().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn reading_needs_settings_read_and_writing_settings_write() {
        let (status, _) = send(
            app(empty_db(), Arc::default()),
            "GET",
            None,
            Some(outsider()),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        let audit = Arc::new(RecordingAudit::default());
        let (status, body) = send(
            app(empty_db(), audit.clone()),
            "PATCH",
            Some(serde_json::json!({"completed": true})),
            Some(settings_reader()),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(
            body.get("title").is_some(),
            "403 is Problem Details: {body}"
        );
        assert!(audit.records.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn get_returns_the_stored_record() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![settings_row(serde_json::json!({
                "onboarding": {
                    "completed_at": null,
                    "later": [{"step": "reach", "reason": "No domain yet", "at": "2026-11-24T10:00:00Z"}]
                }
            }))]])
            .into_connection();
        let (status, body) = send(
            app(db, Arc::default()),
            "GET",
            None,
            Some(settings_reader()),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["completed_at"], serde_json::Value::Null);
        assert_eq!(body["later"][0]["step"], "reach");
        assert_eq!(body["later"][0]["reason"], "No domain yet");
    }

    #[tokio::test]
    async fn patch_writes_only_its_key_and_is_audited() {
        let stored = settings_row(serde_json::json!({
            "external_url": "https://temps.example.com",
            "admin_gate": {"version": 1}
        }));
        let mut updated = stored.clone();
        updated.data["onboarding"] = serde_json::json!({});
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![stored]])
            .append_exec_results([MockExecResult {
                last_insert_id: 1,
                rows_affected: 1,
            }])
            .append_query_results([vec![updated]])
            .into_connection();
        let audit = Arc::new(RecordingAudit::default());
        let (status, body) = send(
            app(db, audit.clone()),
            "PATCH",
            Some(serde_json::json!({
                "completed": true,
                "done": ["host", "agents"],
                "later": [{"step": "reach", "reason": "No domain yet"}],
                "default_reach": null
            })),
            Some(admin()),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body["completed_at"].is_string());
        assert_eq!(body["done"], serde_json::json!(["host", "agents"]));
        assert_eq!(body["later"][0]["reason"], "No domain yet");

        let records = audit.records.lock().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].0, "ONBOARDING_UPDATED");
        assert!(records[0].1.contains("No domain yet"));
    }

    #[tokio::test]
    async fn an_invalid_patch_is_a_400_problem_and_writes_nothing() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![settings_row(serde_json::json!({}))]])
            .into_connection();
        let audit = Arc::new(RecordingAudit::default());
        let (status, body) = send(
            app(db, audit.clone()),
            "PATCH",
            Some(serde_json::json!({
                "default_reach": {
                    "audience": "public", "way": "direct", "naming": "wildcard",
                    "domain": "https://apps.example.com"
                }
            })),
            Some(admin()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["title"], "Invalid Onboarding Update");
        assert!(body["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("default_reach.domain"));
        assert!(audit.records.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_database_failure_is_a_500_problem() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_errors([sea_orm::DbErr::Custom("connection reset".into())])
            .into_connection();
        let (status, body) = send(app(db, Arc::default()), "GET", None, Some(admin())).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body["title"], "Onboarding Record Unavailable");
    }
}
