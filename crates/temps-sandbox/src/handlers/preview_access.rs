// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Who may open a sandbox's preview URLs, and from which networks.
//!
//! `PUT /v1/sandboxes/{id}/preview-access` sets both halves at once:
//!
//! - `access`: `temps_login` (a signed-in Temps user who may read the
//!   sandbox; the default), `password` (only while a preview password is
//!   set) or `public`;
//! - `allowed_cidrs`: client networks allowed in, checked before `access`.
//!   Empty = any address.
//!
//! Also holds the audit records for the preview password endpoints, which
//! change the access mode as a side effect.

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    response::IntoResponse,
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use temps_auth::{permissions::Permission, RequireAuth};
use temps_core::problemdetails::Problem;
use temps_core::{AuditContext, AuditOperation, RequestMetadata};
use temps_entities::ingress_mode::SandboxPreviewAccess;

use crate::handlers::sandboxes::sandbox_permission_guard;
use crate::handlers::SandboxAppState;

/// Request body for `PUT /v1/sandboxes/{id}/preview-access`. Both members
/// are required: the call replaces the whole setting, so an omitted
/// allow-list can never be read as "clear it".
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdatePreviewAccessBody {
    /// `temps_login` | `password` | `public`. `password` is refused while
    /// the sandbox has no preview password (set one with
    /// `PUT /v1/sandboxes/{id}/preview-password`, which also selects it).
    pub access: SandboxPreviewAccess,
    /// IPv4/IPv6 addresses or CIDRs allowed to open the preview URLs, at
    /// most 50. A single address is stored as a `/32` or `/128`; a CIDR as
    /// its network address. Empty = any client address.
    pub allowed_cidrs: Vec<String>,
}

/// A sandbox's preview access settings.
#[derive(Debug, Serialize, ToSchema)]
pub struct PreviewAccessResponse {
    /// Sandbox public id (`sbx_...`).
    pub id: String,
    pub preview_access: SandboxPreviewAccess,
    /// Normalised CIDRs. Empty = any client address.
    pub preview_allowed_cidrs: Vec<String>,
    /// Last 4 characters of the preview password, when one is set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview_password_hint: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct SandboxPreviewAccessUpdatedAudit {
    context: AuditContext,
    sandbox_id: String,
    previous_access: String,
    access: String,
    previous_allowed_cidrs: Vec<String>,
    allowed_cidrs: Vec<String>,
}

impl AuditOperation for SandboxPreviewAccessUpdatedAudit {
    fn operation_type(&self) -> String {
        "SANDBOX_PREVIEW_ACCESS_UPDATED".to_string()
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
    fn serialize(&self) -> anyhow::Result<String> {
        serde_json::to_string(self).map_err(|e| {
            anyhow::anyhow!(
                "Failed to serialize SandboxPreviewAccessUpdatedAudit for sandbox {}: {}",
                self.sandbox_id,
                e
            )
        })
    }
}

/// Audit record for setting or clearing a sandbox's preview password. Never
/// carries the password or its hash; `access` is the mode afterwards.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct SandboxPreviewPasswordAudit {
    #[serde(skip)]
    operation: &'static str,
    context: AuditContext,
    sandbox_id: String,
    access: String,
}

impl SandboxPreviewPasswordAudit {
    pub(crate) fn set(context: AuditContext, sandbox_id: &str) -> Self {
        Self {
            operation: "SANDBOX_PREVIEW_PASSWORD_SET",
            context,
            sandbox_id: sandbox_id.to_string(),
            access: SandboxPreviewAccess::Password.as_str().to_string(),
        }
    }

    pub(crate) fn cleared(
        context: AuditContext,
        sandbox_id: &str,
        access: SandboxPreviewAccess,
    ) -> Self {
        Self {
            operation: "SANDBOX_PREVIEW_PASSWORD_CLEARED",
            context,
            sandbox_id: sandbox_id.to_string(),
            access: access.as_str().to_string(),
        }
    }
}

impl AuditOperation for SandboxPreviewPasswordAudit {
    fn operation_type(&self) -> String {
        self.operation.to_string()
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
    fn serialize(&self) -> anyhow::Result<String> {
        serde_json::to_string(self).map_err(|e| {
            anyhow::anyhow!(
                "Failed to serialize {} audit for sandbox {}: {}",
                self.operation,
                self.sandbox_id,
                e
            )
        })
    }
}

/// Write an audit record; a failure is logged and never fails the request.
pub(crate) async fn write_audit(
    state: &SandboxAppState,
    event: &dyn AuditOperation,
    sandbox_id: &str,
) {
    if let Some(ref audit) = state.audit_service {
        if let Err(e) = audit.create_audit_log(event).await {
            tracing::error!(
                sandbox_id = %sandbox_id,
                operation = %event.operation_type(),
                "sandbox preview access: failed to write audit log: {}",
                e
            );
        }
    }
}

pub(crate) fn audit_context(user_id: i32, metadata: &RequestMetadata) -> AuditContext {
    AuditContext {
        user_id,
        ip_address: Some(metadata.ip_address.clone()),
        user_agent: metadata.user_agent.clone(),
    }
}

/// Set who may open the sandbox's preview URLs and from which networks.
///
/// The proxy applies the change within 30 seconds (its sandbox lookup
/// cache). A visitor refused by the allow-list gets a 403 naming their
/// address and this endpoint.
#[utoipa::path(
    tag = "Sandboxes",
    put,
    path = "/v1/sandboxes/{id}/preview-access",
    params(("id" = String, Path, description = "Sandbox public id (`sbx_...`)")),
    request_body = UpdatePreviewAccessBody,
    responses(
        (status = 200, description = "Preview access updated", body = PreviewAccessResponse),
        (status = 400, description = "Invalid CIDR, more than 50 networks, or `password` without a preview password"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Sandbox not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn update_preview_access(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SandboxAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(id): Path<String>,
    Json(body): Json<UpdatePreviewAccessBody>,
) -> Result<impl IntoResponse, Problem> {
    sandbox_permission_guard(&auth, Permission::SandboxesWrite, Permission::ProjectsWrite)?;
    let user_id = auth.user_id();
    let change = state
        .sandbox_service
        .set_preview_access(&id, user_id, body.access, &body.allowed_cidrs)
        .await?;
    let sandbox = change.sandbox;

    tracing::info!(
        user_id = %user_id,
        sandbox_id = %sandbox.public_id,
        previous_access = %change.previous_access.as_str(),
        access = %sandbox.preview_access.as_str(),
        allowed_cidrs = sandbox.preview_allowed_cidrs.len(),
        "sandbox preview access updated"
    );
    let event = SandboxPreviewAccessUpdatedAudit {
        context: audit_context(user_id, &metadata),
        sandbox_id: sandbox.public_id.clone(),
        previous_access: change.previous_access.as_str().to_string(),
        access: sandbox.preview_access.as_str().to_string(),
        previous_allowed_cidrs: change.previous_allowed_cidrs,
        allowed_cidrs: sandbox.preview_allowed_cidrs.clone(),
    };
    write_audit(&state, &event, &sandbox.public_id).await;

    Ok(Json(PreviewAccessResponse {
        id: sandbox.public_id,
        preview_access: sandbox.preview_access,
        preview_allowed_cidrs: sandbox.preview_allowed_cidrs,
        preview_password_hint: sandbox.preview_password_hint,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;
    use chrono::Utc;
    use std::sync::Mutex;
    use temps_auth::context::AuthContext;
    use temps_auth::permissions::Role;
    use temps_core::{
        AuditLogger, SensitiveAction, SensitiveActionAuthorizationError, SensitiveActionAuthorizer,
        SensitiveActionDecision, SensitiveActionPrincipal,
    };
    use temps_entities::{sandboxes, users};
    use tower::ServiceExt;

    const SANDBOX: &str = "sbx_deadbeef01234567";

    fn user() -> users::Model {
        let now = Utc::now();
        users::Model {
            id: 1,
            name: "Sandbox Owner".to_string(),
            email: "owner@example.com".to_string(),
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

    fn row(user_id: i32) -> sandboxes::Model {
        let now = Utc::now();
        sandboxes::Model {
            id: 7,
            public_id: SANDBOX.into(),
            user_id: Some(user_id),
            agent_run_id: None,
            name: "sbx-7".into(),
            status: "running".into(),
            lifecycle: "ephemeral".into(),
            project_id: None,
            source_repo_url: None,
            image: None,
            work_dir: "/workspace".into(),
            timeout_secs: 600,
            metadata: None,
            backend: None,
            node_id: None,
            created_at: now,
            last_activity_at: now,
            expires_at: now,
            preview_password_hash: None,
            preview_password_hint: None,
            preview_access: SandboxPreviewAccess::TempsLogin,
            preview_allowed_cidrs: Vec::new(),
        }
    }

    struct Allow;

    #[async_trait::async_trait]
    impl SensitiveActionAuthorizer for Allow {
        async fn authorize(
            &self,
            _action: &SensitiveAction,
            _principal: &SensitiveActionPrincipal,
        ) -> Result<SensitiveActionDecision, SensitiveActionAuthorizationError> {
            Ok(SensitiveActionDecision::Allow)
        }
    }

    #[derive(Default)]
    struct RecordingAudit {
        records: Mutex<Vec<(String, String)>>,
    }

    #[async_trait::async_trait]
    impl AuditLogger for RecordingAudit {
        async fn create_audit_log(&self, operation: &dyn AuditOperation) -> anyhow::Result<()> {
            self.records
                .lock()
                .map_err(|_| anyhow::anyhow!("audit mutex poisoned"))?
                .push((operation.operation_type(), operation.serialize()?));
            Ok(())
        }
    }

    fn metadata() -> RequestMetadata {
        RequestMetadata {
            ip_address: "198.51.100.4".into(),
            user_agent: "test".into(),
            headers: axum::http::HeaderMap::new(),
            visitor_id_cookie: None,
            session_id_cookie: None,
            base_url: "http://localhost".into(),
            scheme: "http".into(),
            host: "localhost".into(),
            is_secure: false,
        }
    }

    /// The sandbox routes over a mock database answering `results` in
    /// order. `role: None` sends the request without an auth context.
    fn app(
        role: Option<Role>,
        results: Vec<Vec<sandboxes::Model>>,
        audit: Arc<RecordingAudit>,
    ) -> axum::Router {
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
                .append_query_results(results)
                .into_connection(),
        );
        let service =
            Arc::new(crate::services::sandbox_service::tests::preview_test_service_with_db(db));
        let state = Arc::new(SandboxAppState {
            sandbox_service: service,
            snapshot_service: None,
            project_access_checker: None,
            audit_service: Some(audit),
            sensitive_action_authorizer: Arc::new(Allow),
        });
        let router = crate::handlers::configure_routes()
            .with_state(state)
            .layer(Extension(metadata()));
        match role {
            Some(role) => router.layer(Extension(AuthContext::new_persisted_session(
                user(),
                role,
                9,
            ))),
            None => router,
        }
    }

    async fn put(app: axum::Router, body: &str) -> (StatusCode, String) {
        let request = axum::http::Request::builder()
            .method("PUT")
            .uri(format!("/v1/sandboxes/{SANDBOX}/preview-access"))
            .header("content-type", "application/json")
            .body(axum::body::Body::from(body.to_string()))
            .expect("request");
        let response = app.oneshot(request).await.expect("response");
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .expect("body");
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    const PUBLIC_BODY: &str = r#"{"access":"public","allowed_cidrs":["192.0.2.9/24"]}"#;

    #[tokio::test]
    async fn an_unauthenticated_request_is_refused() {
        let audit = Arc::new(RecordingAudit::default());
        let (status, _) = put(app(None, Vec::new(), audit), PUBLIC_BODY).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn a_reader_cannot_change_preview_access() {
        let audit = Arc::new(RecordingAudit::default());
        let (status, body) = put(app(Some(Role::Reader), Vec::new(), audit), PUBLIC_BODY).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert!(body.contains("sandboxes:write"), "{body}");
    }

    #[tokio::test]
    async fn another_users_sandbox_is_not_found() {
        let audit = Arc::new(RecordingAudit::default());
        let (status, body) = put(
            app(Some(Role::User), vec![vec![row(2)]], audit.clone()),
            PUBLIC_BODY,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        assert!(body.contains(SANDBOX), "{body}");
        assert!(audit.records.lock().expect("audit").is_empty());
    }

    #[tokio::test]
    async fn invalid_networks_and_password_without_a_hash_are_bad_requests() {
        let audit = Arc::new(RecordingAudit::default());
        let (status, body) = put(
            app(Some(Role::User), Vec::new(), audit.clone()),
            r#"{"access":"public","allowed_cidrs":["10.0.0.0/33"]}"#,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body.contains("allowed_cidrs[0]"), "{body}");

        let (status, body) = put(
            app(Some(Role::User), vec![vec![row(1)]], audit.clone()),
            r#"{"access":"password","allowed_cidrs":[]}"#,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body.contains("preview-password"), "{body}");

        // A missing member is rejected, never read as "clear the list".
        let (status, _) = put(
            app(Some(Role::User), Vec::new(), audit.clone()),
            r#"{"access":"public"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(audit.records.lock().expect("audit").is_empty());
    }

    #[tokio::test]
    async fn the_owner_updates_access_and_it_is_audited() {
        let audit = Arc::new(RecordingAudit::default());
        let mut updated = row(1);
        updated.preview_access = SandboxPreviewAccess::Public;
        updated.preview_allowed_cidrs = vec!["192.0.2.0/24".into()];
        let (status, body) = put(
            app(
                Some(Role::User),
                vec![vec![row(1)], vec![updated]],
                audit.clone(),
            ),
            PUBLIC_BODY,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let json: serde_json::Value = serde_json::from_str(&body).expect("json body");
        assert_eq!(json["id"], SANDBOX);
        assert_eq!(json["preview_access"], "public");
        assert_eq!(
            json["preview_allowed_cidrs"],
            serde_json::json!(["192.0.2.0/24"])
        );

        let records = audit.records.lock().expect("audit");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].0, "SANDBOX_PREVIEW_ACCESS_UPDATED");
        assert!(records[0].1.contains("\"previous_access\":\"temps_login\""));
        assert!(records[0].1.contains("192.0.2.0/24"));
        assert!(records[0].1.contains(SANDBOX));
    }

    #[test]
    fn the_route_and_schemas_are_in_the_openapi_document() {
        use utoipa::OpenApi;
        let api = crate::handlers::SandboxApiDoc::openapi();
        assert!(api
            .paths
            .paths
            .contains_key("/v1/sandboxes/{id}/preview-access"));
        let schemas = &api.components.as_ref().expect("components").schemas;
        for name in [
            "UpdatePreviewAccessBody",
            "PreviewAccessResponse",
            "SandboxPreviewAccess",
        ] {
            assert!(schemas.contains_key(name), "{name} missing");
        }
        let sandbox = serde_json::to_string(schemas.get("SandboxInner").expect("SandboxInner"))
            .expect("schema json");
        assert!(sandbox.contains("preview_access"), "{sandbox}");
        assert!(sandbox.contains("preview_allowed_cidrs"), "{sandbox}");
    }

    #[test]
    fn password_audits_never_carry_the_password() {
        let context = audit_context(1, &metadata());
        let set = SandboxPreviewPasswordAudit::set(context.clone(), SANDBOX);
        assert_eq!(set.operation_type(), "SANDBOX_PREVIEW_PASSWORD_SET");
        let json = AuditOperation::serialize(&set).expect("serialize");
        assert!(json.contains("\"access\":\"password\""), "{json}");
        assert!(!json.contains("hash"), "{json}");
        let cleared = SandboxPreviewPasswordAudit::cleared(
            context,
            SANDBOX,
            SandboxPreviewAccess::TempsLogin,
        );
        assert_eq!(cleared.operation_type(), "SANDBOX_PREVIEW_PASSWORD_CLEARED");
        assert!(AuditOperation::serialize(&cleared)
            .expect("serialize")
            .contains("\"access\":\"temps_login\""));
    }
}
