// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `GET`, `PUT` and `DELETE /settings/private-ingress`: the operator's
//! consent for the control plane to serve private addresses on this machine
//! (ADR-114 D2). See [`temps_config::private_ingress`].

use axum::{
    extract::{Extension, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use std::sync::Arc;
use temps_auth::{permission_guard, RequireAuth};
use temps_config::private_ingress::{
    PrivateIngressConsent, PrivateIngressError, PrivateIngressRequest, PrivateIngressService,
    PrivateIngressStatus,
};
use temps_core::error_builder::ErrorBuilder;
use temps_core::{
    problemdetails::Problem, AuditContext, AuditLogger, AuditOperation, RequestMetadata,
};
use tracing::error;
use utoipa::OpenApi;

pub struct PrivateIngressHandlerState {
    pub private_ingress_service: Arc<PrivateIngressService>,
    pub audit_service: Arc<dyn AuditLogger>,
}

/// Router state for the private-ingress endpoints.
pub type PrivateIngressRouterState = Arc<PrivateIngressHandlerState>;

/// The Problem for a private-ingress error; every variant explicitly.
fn private_ingress_problem(error: PrivateIngressError) -> Problem {
    match error {
        PrivateIngressError::Validation { .. } => ErrorBuilder::new(StatusCode::BAD_REQUEST)
            .type_("https://temps.sh/probs/private-ingress-invalid")
            .title("Invalid Private Ingress Address")
            .detail(error.to_string())
            .build(),
        PrivateIngressError::Database(_) => {
            error!(%error, "Failed to read or write the private ingress consent");
            ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .type_("https://temps.sh/probs/private-ingress-storage")
                .title("Private Ingress Consent Unavailable")
                .detail(error.to_string())
                .build()
        }
        PrivateIngressError::Serialization(_) => {
            error!(%error, "Failed to serialize the private ingress consent");
            ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .type_("https://temps.sh/probs/private-ingress-storage")
                .title("Private Ingress Consent Unavailable")
                .detail(error.to_string())
                .build()
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
struct PrivateIngressUpdatedAudit {
    context: AuditContext,
    /// The consent before and after; `null` is none. Holds no secrets.
    before: Option<PrivateIngressConsent>,
    after: Option<PrivateIngressConsent>,
}

impl AuditOperation for PrivateIngressUpdatedAudit {
    fn operation_type(&self) -> String {
        "PRIVATE_INGRESS_UPDATED".to_string()
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
    paths(get_private_ingress, put_private_ingress, delete_private_ingress),
    components(schemas(PrivateIngressStatus, PrivateIngressConsent, PrivateIngressRequest))
)]
pub struct PrivateIngressApiDoc;

pub fn configure_private_ingress_routes() -> Router<PrivateIngressRouterState> {
    Router::new().route(
        "/settings/private-ingress",
        get(get_private_ingress)
            .put(put_private_ingress)
            .delete(delete_private_ingress),
    )
}

async fn record_audit(
    state: &PrivateIngressHandlerState,
    auth: &temps_auth::AuthContext,
    metadata: &RequestMetadata,
    before: Option<PrivateIngressConsent>,
    after: Option<PrivateIngressConsent>,
) {
    let audit = PrivateIngressUpdatedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        before,
        after,
    };
    if let Err(error) = state.audit_service.create_audit_log(&audit).await {
        error!(%error, "Failed to record the private ingress audit log");
    }
}

/// Whether this machine serves private addresses: the consent, and what the
/// control plane last made of it (the address it serves on, or why not).
#[utoipa::path(
    tag = "Settings",
    get,
    path = "/settings/private-ingress",
    responses(
        (status = 200, description = "The consent and its status", body = PrivateIngressStatus),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "The consent could not be read")
    ),
    security(("bearer_auth" = []))
)]
async fn get_private_ingress(
    RequireAuth(auth): RequireAuth,
    State(state): State<PrivateIngressRouterState>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsRead);
    let status = state
        .private_ingress_service
        .status()
        .await
        .map_err(private_ingress_problem)?;
    Ok(Json(status))
}

/// Serve private addresses from this machine.
///
/// `listen` is `tailscale` (the Tailscale interface's address) or one of the
/// machine's private addresses. The proxy then serves an address made
/// private on this machine only to connections that arrive on that address
/// from inside its range. `tailscale_serve` also lets Temps write its own
/// entries in tailscaled's Serve configuration.
#[utoipa::path(
    tag = "Settings",
    put,
    path = "/settings/private-ingress",
    request_body = PrivateIngressRequest,
    responses(
        (status = 200, description = "The stored consent", body = PrivateIngressConsent),
        (status = 400, description = "The address is not one a private listener may use; the detail says why"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "The consent could not be written")
    ),
    security(("bearer_auth" = []))
)]
async fn put_private_ingress(
    RequireAuth(auth): RequireAuth,
    State(state): State<PrivateIngressRouterState>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<PrivateIngressRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);
    let (before, after) = state
        .private_ingress_service
        .put(request)
        .await
        .map_err(private_ingress_problem)?;
    record_audit(&state, &auth, &metadata, before, Some(after.clone())).await;
    Ok(Json(after))
}

/// Stop serving private addresses from this machine. Addresses made private
/// here stop answering until consent is given again.
#[utoipa::path(
    tag = "Settings",
    delete,
    path = "/settings/private-ingress",
    responses(
        (status = 204, description = "The consent is withdrawn"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "The consent could not be written")
    ),
    security(("bearer_auth" = []))
)]
async fn delete_private_ingress(
    RequireAuth(auth): RequireAuth,
    State(state): State<PrivateIngressRouterState>,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);
    let before = state
        .private_ingress_service
        .delete()
        .await
        .map_err(private_ingress_problem)?;
    if before.is_some() {
        record_audit(&state, &auth, &metadata, before, None).await;
    }
    Ok(StatusCode::NO_CONTENT)
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

    fn metadata() -> RequestMetadata {
        RequestMetadata {
            ip_address: "203.0.113.9".to_string(),
            user_agent: "private-ingress-test".to_string(),
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
        configure_private_ingress_routes().with_state(Arc::new(PrivateIngressHandlerState {
            private_ingress_service: Arc::new(PrivateIngressService::new(Arc::new(db), None)),
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
            .uri("/settings/private-ingress");
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

    fn tailscale() -> serde_json::Value {
        serde_json::json!({"listen": "tailscale", "tailscale_serve": false})
    }

    #[tokio::test]
    async fn every_endpoint_requires_authentication() {
        for (method, body) in [("GET", None), ("PUT", Some(tailscale())), ("DELETE", None)] {
            let audit = Arc::new(RecordingAudit::default());
            let (status, _) = send(app(empty_db(), audit.clone()), method, body, None).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{method}");
            assert!(audit.records.lock().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn changing_the_consent_needs_settings_write() {
        for (method, body) in [("PUT", Some(tailscale())), ("DELETE", None)] {
            let audit = Arc::new(RecordingAudit::default());
            let (status, body) = send(
                app(empty_db(), audit.clone()),
                method,
                body,
                Some(settings_reader()),
            )
            .await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{method}");
            assert!(
                body.get("title").is_some(),
                "403 is Problem Details: {body}"
            );
            assert!(audit.records.lock().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn get_without_consent_says_nothing_is_served() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![settings_row(serde_json::json!({}))]])
            .into_connection();
        let (status, body) = send(
            app(db, Arc::default()),
            "GET",
            None,
            Some(settings_reader()),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["consent"], serde_json::Value::Null);
        assert_eq!(body["serving"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn put_stores_the_consent_and_audits_before_and_after() {
        let stored = settings_row(serde_json::json!({"external_url": "https://temps.example.com"}));
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![stored.clone()]])
            .append_exec_results([MockExecResult {
                last_insert_id: 1,
                rows_affected: 1,
            }])
            .append_query_results([vec![stored]])
            .into_connection();
        let audit = Arc::new(RecordingAudit::default());
        let (status, body) = send(
            app(db, audit.clone()),
            "PUT",
            Some(serde_json::json!({"listen": "100.101.102.103", "tailscale_serve": true})),
            Some(admin()),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["listen"], "100.101.102.103");
        assert_eq!(body["tailscale_serve"], true);
        let records = audit.records.lock().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].0, "PRIVATE_INGRESS_UPDATED");
        assert!(records[0].1.contains("\"before\":null"), "{}", records[0].1);
        assert!(records[0].1.contains("100.101.102.103"), "{}", records[0].1);
    }

    #[tokio::test]
    async fn a_public_address_is_a_400_with_why_and_writes_nothing() {
        let audit = Arc::new(RecordingAudit::default());
        let (status, body) = send(
            app(empty_db(), audit.clone()),
            "PUT",
            Some(serde_json::json!({"listen": "203.0.113.7"})),
            Some(admin()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["title"], "Invalid Private Ingress Address");
        assert!(body["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("not a private address"));
        assert!(audit.records.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn delete_withdraws_the_consent_and_audits_it() {
        let stored = settings_row(serde_json::json!({
            "private_ingress": {"listen": "tailscale", "updated_at": "2026-11-24T10:00:00Z"}
        }));
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![stored.clone()]])
            .append_exec_results([MockExecResult {
                last_insert_id: 1,
                rows_affected: 1,
            }])
            .append_query_results([vec![settings_row(serde_json::json!({}))]])
            .into_connection();
        let audit = Arc::new(RecordingAudit::default());
        let (status, _) = send(app(db, audit.clone()), "DELETE", None, Some(admin())).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let records = audit.records.lock().unwrap();
        assert_eq!(records.len(), 1);
        assert!(records[0].1.contains("\"after\":null"), "{}", records[0].1);
    }

    #[tokio::test]
    async fn a_database_failure_is_a_500_problem() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_errors([sea_orm::DbErr::Custom("connection reset".into())])
            .into_connection();
        let (status, body) = send(app(db, Arc::default()), "GET", None, Some(admin())).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body["title"], "Private Ingress Consent Unavailable");
    }
}
