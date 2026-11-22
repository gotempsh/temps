// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `GET`/`PUT /projects/{id}/container-security`: which Linux capabilities
//! a project's containers get back after `cap_drop: ALL`.
//!
//! Changing it is held to the bar of the other host-hardening exceptions
//! (Compose security policies): `projects:write` on the project, an instance
//! administrator, recent MFA when the administrator enrolled it, and, for
//! the assistant, the owner's own confirmation of that exact card. Every
//! change is audited with the old and new value.

use axum::{
    extract::{Extension, Path, State},
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_auth::{
    project_permission_guard, project_scope_guard, require_sensitive_action, RequireAuth,
};
use temps_core::{
    problemdetails::Problem, AuditLogger, AuditOperation, RequestMetadata, SensitiveAction,
    SensitiveActionAuthorizer,
};
use temps_workload::ContainerCapability;
use utoipa::ToSchema;

use crate::services::container_security::{adds_capabilities, ContainerSecurityService};

/// State for the container security routes. Kept separate from the
/// projects `AppState` so they depend only on what they use.
pub struct ContainerSecurityAppState {
    pub container_security_service: Arc<ContainerSecurityService>,
    pub audit_service: Arc<dyn AuditLogger>,
    /// Team-based project access; `None` in the plain OSS binary.
    pub project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
    /// MFA step-up policy for widening the setting.
    pub sensitive_action_authorizer: Arc<dyn SensitiveActionAuthorizer>,
}

pub fn configure_container_security_routes() -> Router<Arc<ContainerSecurityAppState>> {
    Router::new().route(
        "/projects/{id}/container-security",
        get(get_container_security).put(update_container_security),
    )
}

/// The operation id of the update, as the assistant's allowlists name it.
pub const UPDATE_CONTAINER_SECURITY_OPERATION: &str = "update_container_security";

/// One capability a project may allow, explained for the console.
#[derive(Debug, Serialize, ToSchema)]
pub struct ContainerCapabilityOption {
    pub id: ContainerCapability,
    /// Short label, e.g. "Bind ports below 1024".
    pub label: &'static str,
    /// What allowing it does, in plain words.
    pub description: &'static str,
    /// What it costs in isolation.
    pub trade_off: &'static str,
}

impl ContainerCapabilityOption {
    pub fn catalog() -> Vec<Self> {
        ContainerCapability::ALL
            .iter()
            .map(|capability| match capability {
                ContainerCapability::NetBindService => Self {
                    id: *capability,
                    label: "Allow binding ports below 1024 (NET_BIND_SERVICE)",
                    description: "Lets an image whose entrypoint was marked with `setcap cap_net_bind_service=+ep` start, so it can listen on port 80 or 443 as a non-root user. Every other capability stays dropped and no-new-privileges stays on.",
                    trade_off: "A process in the container may listen on privileged ports inside its own network namespace. It cannot gain other capabilities or reach the host.",
                },
            })
            .collect()
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ContainerSecurityResponse {
    /// Capabilities this project's containers get back after `cap_drop:
    /// ALL`. Empty: fully hardened (the default). Applies from the next
    /// deployment.
    pub allowed_capabilities: Vec<ContainerCapability>,
    /// Every capability a project may allow. Nothing outside this list can
    /// be granted.
    pub options: Vec<ContainerCapabilityOption>,
    /// Hardening every project container keeps whatever is allowed.
    pub always_enforced: Vec<&'static str>,
    /// Whether the caller may change the setting.
    pub can_edit: bool,
    /// Who may change it, for a caller who cannot.
    pub edit_requirement: &'static str,
}

const ALWAYS_ENFORCED: &[&str] = &[
    "Every Linux capability not allowed here is dropped",
    "no-new-privileges: setuid binaries and file capabilities never raise privileges",
    "A 512-process limit",
    "An init process for signals and zombie reaping",
];

const EDIT_REQUIREMENT: &str =
    "An instance administrator with projects:write on this project can change container security.";

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateContainerSecurityRequest {
    /// The capabilities to allow, as Docker names (`NET_BIND_SERVICE`).
    /// Anything outside `options` is rejected with 400.
    pub allowed_capabilities: Vec<String>,
    /// The value the editor loaded; the save fails with 409 if it changed.
    pub expected_allowed_capabilities: Vec<String>,
    /// Required (true) when the change allows a capability that was not
    /// allowed before.
    #[serde(default)]
    pub acknowledge_risks: bool,
}

fn response(allowed: Vec<ContainerCapability>, can_edit: bool) -> ContainerSecurityResponse {
    ContainerSecurityResponse {
        allowed_capabilities: allowed,
        options: ContainerCapabilityOption::catalog(),
        always_enforced: ALWAYS_ENFORCED.to_vec(),
        can_edit,
        edit_requirement: EDIT_REQUIREMENT,
    }
}

fn can_edit(auth: &temps_auth::AuthContext) -> bool {
    auth.is_admin() && auth.has_permission(&temps_auth::Permission::ProjectsWrite)
}

fn require_admin(auth: &temps_auth::AuthContext) -> Result<(), Problem> {
    if !auth.is_admin() {
        return Err(temps_core::problemdetails::new(http::StatusCode::FORBIDDEN)
            .with_title("Instance administrator required")
            .with_detail(EDIT_REQUIREMENT));
    }
    Ok(())
}

#[utoipa::path(get, path = "/projects/{id}/container-security", tag = "Projects",
    params(("id" = i32, Path, description = "Project ID")),
    responses(
        (status = 200, body = ContainerSecurityResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden"),
        (status = 404, description = "Project not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = [])))]
pub async fn get_container_security(
    State(state): State<Arc<ContainerSecurityAppState>>,
    Path(id): Path<i32>,
    RequireAuth(auth): RequireAuth,
) -> Result<Json<ContainerSecurityResponse>, Problem> {
    project_permission_guard!(auth, ProjectsRead, id, state.project_access_checker);
    project_scope_guard!(auth, id);
    let allowed = state
        .container_security_service
        .allowed_capabilities(id)
        .await?;
    Ok(Json(response(allowed, can_edit(&auth))))
}

#[utoipa::path(put, path = "/projects/{id}/container-security", tag = "Projects",
    params(("id" = i32, Path, description = "Project ID")),
    request_body = UpdateContainerSecurityRequest,
    responses(
        (status = 200, body = ContainerSecurityResponse),
        (status = 400, description = "A capability outside the allow-list, or a missing acknowledgement"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Instance administrator required, or the assistant tried to allow a capability without the owner's confirmation"),
        (status = 404, description = "Project not found"),
        (status = 409, description = "The setting changed since it was loaded; refresh before retrying"),
        (status = 428, description = "Recent MFA verification required"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = [])))]
pub async fn update_container_security(
    State(state): State<Arc<ContainerSecurityAppState>>,
    Path(id): Path<i32>,
    RequireAuth(auth): RequireAuth,
    Extension(metadata): Extension<RequestMetadata>,
    ai_call: Option<Extension<temps_core::ai_tool_call::AiToolCall>>,
    confirmation: Option<Extension<temps_core::ai_tool_call::ConfirmedAiWrite>>,
    Json(request): Json<UpdateContainerSecurityRequest>,
) -> Result<Json<ContainerSecurityResponse>, Problem> {
    project_permission_guard!(auth, ProjectsWrite, id, state.project_access_checker);
    project_scope_guard!(auth, id);
    require_admin(&auth)?;
    // Widening is the sensitive direction; narrowing back to fully hardened
    // is never challenged, so a person can always undo it quickly.
    let widens = adds_capabilities(
        id,
        &request.allowed_capabilities,
        &request.expected_allowed_capabilities,
    )?;
    if widens {
        temps_core::ai_tool_call::require_human_confirmation(
            ai_call.as_ref().map(|m| &m.0),
            confirmation.as_ref().map(|m| &m.0),
            UPDATE_CONTAINER_SECURITY_OPERATION,
        )?;
        require_sensitive_action(
            state.sensitive_action_authorizer.as_ref(),
            &auth,
            SensitiveAction::AllowContainerCapabilities { project_id: id },
        )
        .await?;
    }
    let change = state
        .container_security_service
        .update_allowed_capabilities(
            id,
            &request.allowed_capabilities,
            &request.expected_allowed_capabilities,
            request.acknowledge_risks,
        )
        .await?;
    if change.previous != change.current {
        let event = ContainerSecurityAudit {
            project_id: id,
            actor: auth.user_id(),
            ip_address: metadata.ip_address.to_string(),
            user_agent: metadata.user_agent,
            previous: change.previous.clone(),
            allowed_capabilities: change.current.clone(),
            acknowledged_risks: request.acknowledge_risks,
        };
        if let Err(error) = state.audit_service.create_audit_log(&event).await {
            tracing::error!(project_id = id, actor = auth.user_id(), %error, "Failed to record container security audit event");
        }
        tracing::info!(
            project_id = id,
            actor = auth.user_id(),
            previous = ?change.previous,
            allowed_capabilities = ?change.current,
            "Project container security changed; applies from the next deployment"
        );
    }
    Ok(Json(response(change.current, true)))
}

#[derive(Serialize)]
pub(crate) struct ContainerSecurityAudit {
    pub(crate) project_id: i32,
    pub(crate) actor: i32,
    pub(crate) ip_address: String,
    pub(crate) user_agent: String,
    pub(crate) previous: Vec<ContainerCapability>,
    pub(crate) allowed_capabilities: Vec<ContainerCapability>,
    pub(crate) acknowledged_risks: bool,
}

impl AuditOperation for ContainerSecurityAudit {
    fn operation_type(&self) -> String {
        "project.container_security.updated".to_string()
    }
    fn user_id(&self) -> Option<i32> {
        Some(self.actor)
    }
    fn ip_address(&self) -> Option<String> {
        Some(self.ip_address.clone())
    }
    fn user_agent(&self) -> &str {
        &self.user_agent
    }
    fn serialize(&self) -> temps_core::AuditResult<String> {
        temps_core::serialize_audit_operation(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult, Value};
    use std::collections::BTreeMap;
    use std::sync::Mutex;
    use temps_auth::{AuthContext, Permission, Role};
    use temps_core::{
        SensitiveActionAuthorizationError, SensitiveActionDecision, SensitiveActionPrincipal,
    };
    use tower::ServiceExt;

    const URI: &str = "/projects/7/container-security";

    #[derive(Default)]
    struct RecordingAudit(Mutex<Vec<(String, serde_json::Value)>>);

    #[temps_core::async_trait::async_trait]
    impl AuditLogger for RecordingAudit {
        async fn create_audit_log(
            &self,
            operation: &dyn AuditOperation,
        ) -> temps_core::AuditResult<()> {
            let body = serde_json::from_str(&operation.serialize()?).expect("audit body is JSON");
            self.0
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push((operation.operation_type(), body));
            Ok(())
        }
    }

    impl RecordingAudit {
        fn recorded(&self) -> Vec<(String, serde_json::Value)> {
            self.0.lock().unwrap_or_else(|p| p.into_inner()).clone()
        }
    }

    struct Policy(SensitiveActionDecision);

    #[temps_core::async_trait::async_trait]
    impl SensitiveActionAuthorizer for Policy {
        async fn authorize(
            &self,
            _action: &SensitiveAction,
            _principal: &SensitiveActionPrincipal,
        ) -> Result<SensitiveActionDecision, SensitiveActionAuthorizationError> {
            Ok(self.0.clone())
        }
    }

    fn allow() -> Policy {
        Policy(SensitiveActionDecision::Allow)
    }

    fn step_up() -> Policy {
        Policy(SensitiveActionDecision::RequireVerification {
            mfa_setup_required: false,
        })
    }

    fn stored(values: &[&str]) -> BTreeMap<&'static str, Value> {
        BTreeMap::from([(
            "allowed_capabilities",
            Value::from(values.iter().map(|v| v.to_string()).collect::<Vec<_>>()),
        )])
    }

    fn user() -> temps_entities::users::Model {
        temps_entities::users::Model {
            id: 3,
            name: "Test admin".into(),
            email: "admin@example.test".into(),
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

    fn admin() -> AuthContext {
        AuthContext::new_persisted_session(user(), Role::Admin, 42)
    }

    fn metadata() -> RequestMetadata {
        RequestMetadata {
            ip_address: "198.51.100.7".to_string(),
            user_agent: "container-security-test".to_string(),
            headers: Default::default(),
            visitor_id_cookie: None,
            session_id_cookie: None,
            base_url: "https://temps.example.test".to_string(),
            scheme: "https".to_string(),
            host: "temps.example.test".to_string(),
            is_secure: true,
        }
    }

    struct Harness {
        router: Router,
        audit: Arc<RecordingAudit>,
    }

    fn harness(db: MockDatabase, policy: Policy) -> Harness {
        let audit = Arc::new(RecordingAudit::default());
        let state = Arc::new(ContainerSecurityAppState {
            container_security_service: Arc::new(ContainerSecurityService::new(Arc::new(
                db.into_connection(),
            ))),
            audit_service: audit.clone(),
            project_access_checker: None,
            sensitive_action_authorizer: Arc::new(policy),
        });
        Harness {
            router: configure_container_security_routes().with_state(state),
            audit,
        }
    }

    fn empty_db() -> MockDatabase {
        MockDatabase::new(DatabaseBackend::Postgres)
    }

    /// A database holding `stored` that accepts one save.
    fn saving_db(stored_now: &[&str]) -> MockDatabase {
        empty_db()
            .append_query_results([vec![stored(stored_now)]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
    }

    struct Call {
        method: &'static str,
        body: Option<serde_json::Value>,
        auth: Option<AuthContext>,
        ai: bool,
    }

    async fn send(router: Router, call: Call) -> (StatusCode, serde_json::Value) {
        let mut request = Request::builder().method(call.method).uri(URI);
        if call.body.is_some() {
            request = request.header("content-type", "application/json");
        }
        let mut request = request
            .body(
                call.body
                    .map_or_else(Body::empty, |b| Body::from(b.to_string())),
            )
            .expect("request");
        request.extensions_mut().insert(metadata());
        if let Some(auth) = call.auth {
            request.extensions_mut().insert(auth);
        }
        if call.ai {
            request
                .extensions_mut()
                .insert(temps_core::ai_tool_call::AiToolCall);
        }
        let response = router.oneshot(request).await.expect("response");
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, json)
    }

    fn put(body: serde_json::Value, auth: AuthContext) -> Call {
        Call {
            method: "PUT",
            body: Some(body),
            auth: Some(auth),
            ai: false,
        }
    }

    fn allow_nbs() -> serde_json::Value {
        serde_json::json!({
            "allowed_capabilities": ["NET_BIND_SERVICE"],
            "expected_allowed_capabilities": [],
            "acknowledge_risks": true
        })
    }

    #[tokio::test]
    async fn both_routes_require_authentication() {
        for (method, body) in [("GET", None), ("PUT", Some(allow_nbs()))] {
            let h = harness(empty_db(), allow());
            let call = Call {
                method,
                body,
                auth: None,
                ai: false,
            };
            assert_eq!(
                send(h.router, call).await.0,
                StatusCode::UNAUTHORIZED,
                "{method}"
            );
        }
    }

    #[tokio::test]
    async fn a_reader_sees_the_setting_and_who_may_change_it() {
        let h = harness(
            empty_db().append_query_results([vec![stored(&["NET_BIND_SERVICE"])]]),
            allow(),
        );
        let call = Call {
            method: "GET",
            body: None,
            auth: Some(AuthContext::new_session(user(), Role::Reader)),
            ai: false,
        };
        let (status, body) = send(h.router, call).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body["allowed_capabilities"],
            serde_json::json!(["NET_BIND_SERVICE"])
        );
        assert_eq!(body["can_edit"], false);
        assert_eq!(body["edit_requirement"], EDIT_REQUIREMENT);
        assert_eq!(body["options"][0]["id"], "NET_BIND_SERVICE");
        assert_eq!(
            body["always_enforced"].as_array().map(Vec::len),
            Some(ALWAYS_ENFORCED.len())
        );
    }

    #[tokio::test]
    async fn an_admin_reading_a_missing_project_gets_404() {
        let h = harness(
            empty_db().append_query_results([Vec::<BTreeMap<&str, Value>>::new()]),
            allow(),
        );
        let call = Call {
            method: "GET",
            body: None,
            auth: Some(admin()),
            ai: false,
        };
        let (status, body) = send(h.router, call).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("Project 7"));
    }

    #[tokio::test]
    async fn only_an_instance_administrator_with_projects_write_may_change_it() {
        let read_only_key = AuthContext::new_api_key(
            user(),
            None,
            Some(vec![Permission::ProjectsRead]),
            "read only".into(),
            1,
        );
        let restricted_writer = AuthContext::new_api_key(
            user(),
            None,
            Some(vec![Permission::ProjectsWrite]),
            "project writer".into(),
            1,
        );
        let token = AuthContext::new_deployment_token(7, None, None, 1, "ci".into(), vec![]);
        for (label, auth) in [
            ("user session", AuthContext::new_session(user(), Role::User)),
            (
                "reader session",
                AuthContext::new_session(user(), Role::Reader),
            ),
            ("read-only key", read_only_key),
            ("restricted writer key", restricted_writer),
            ("deployment token", token),
        ] {
            let h = harness(empty_db(), allow());
            let (status, _) = send(h.router, put(allow_nbs(), auth)).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{label}");
            assert!(h.audit.recorded().is_empty(), "{label}");
        }
    }

    #[tokio::test]
    async fn an_admin_allowing_the_capability_saves_it_and_audits_old_and_new() {
        let h = harness(saving_db(&[]), allow());
        let (status, body) = send(h.router, put(allow_nbs(), admin())).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            body["allowed_capabilities"],
            serde_json::json!(["NET_BIND_SERVICE"])
        );
        assert_eq!(body["can_edit"], true);

        let recorded = h.audit.recorded();
        assert_eq!(recorded.len(), 1);
        let (operation, event) = &recorded[0];
        assert_eq!(operation, "project.container_security.updated");
        assert_eq!(event["project_id"], 7);
        assert_eq!(event["actor"], 3);
        assert_eq!(event["previous"], serde_json::json!([]));
        assert_eq!(
            event["allowed_capabilities"],
            serde_json::json!(["NET_BIND_SERVICE"])
        );
        assert_eq!(event["acknowledged_risks"], true);
        assert_eq!(event["ip_address"], "198.51.100.7");
    }

    #[tokio::test]
    async fn an_unrestricted_admin_api_key_can_allow_it_too() {
        // The CLI authenticates with an API key; the policy decides whether
        // it needs step-up, the handler does not refuse it outright.
        let key = AuthContext::new_api_key(user(), Some(Role::Admin), None, "cli".into(), 5);
        let h = harness(saving_db(&[]), allow());
        let (status, body) = send(h.router, put(allow_nbs(), key)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(h.audit.recorded().len(), 1);
    }

    #[tokio::test]
    async fn arbitrary_or_dangerous_capabilities_are_rejected_with_400() {
        for bad in [
            "SYS_ADMIN",
            "ALL",
            "privileged",
            "NET_RAW",
            "TOTALLY_MADE_UP",
        ] {
            let h = harness(empty_db(), allow());
            let body = serde_json::json!({
                "allowed_capabilities": [bad],
                "expected_allowed_capabilities": [],
                "acknowledge_risks": true
            });
            let (status, problem) = send(h.router, put(body, admin())).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
            assert!(
                problem["detail"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("only NET_BIND_SERVICE"),
                "{bad}: {problem}"
            );
            assert!(h.audit.recorded().is_empty());
        }
    }

    #[tokio::test]
    async fn extra_fields_are_rejected() {
        let h = harness(empty_db(), allow());
        let body = serde_json::json!({
            "allowed_capabilities": [],
            "expected_allowed_capabilities": [],
            "privileged": true
        });
        let (status, _) = send(h.router, put(body, admin())).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn adding_without_acknowledgement_is_a_400_and_writes_nothing() {
        let h = harness(
            empty_db().append_query_results([vec![stored(&[])]]),
            allow(),
        );
        let body = serde_json::json!({
            "allowed_capabilities": ["NET_BIND_SERVICE"],
            "expected_allowed_capabilities": []
        });
        let (status, problem) = send(h.router, put(body, admin())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(problem["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("acknowledge_risks"));
        assert!(h.audit.recorded().is_empty());
    }

    #[tokio::test]
    async fn a_stale_editor_gets_409_and_nothing_is_audited() {
        let h = harness(
            empty_db().append_query_results([vec![stored(&["NET_BIND_SERVICE"])]]),
            allow(),
        );
        let body = serde_json::json!({
            "allowed_capabilities": [],
            "expected_allowed_capabilities": [],
        });
        let (status, _) = send(h.router, put(body, admin())).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(h.audit.recorded().is_empty());
    }

    #[tokio::test]
    async fn widening_needs_recent_mfa_but_narrowing_never_does() {
        // Widening is challenged before the database is touched.
        let h = harness(empty_db(), step_up());
        let (status, _) = send(h.router, put(allow_nbs(), admin())).await;
        assert_eq!(status, StatusCode::PRECONDITION_REQUIRED);
        assert!(h.audit.recorded().is_empty());

        // Going back to fully hardened is never blocked by step-up.
        let h = harness(saving_db(&["NET_BIND_SERVICE"]), step_up());
        let body = serde_json::json!({
            "allowed_capabilities": [],
            "expected_allowed_capabilities": ["NET_BIND_SERVICE"],
        });
        let (status, body) = send(h.router, put(body, admin())).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["allowed_capabilities"], serde_json::json!([]));
        assert_eq!(
            h.audit.recorded()[0].1["previous"],
            serde_json::json!(["NET_BIND_SERVICE"])
        );
    }

    #[tokio::test]
    async fn the_assistant_cannot_widen_it_without_the_owners_confirmation() {
        let h = harness(empty_db(), allow());
        let mut call = put(allow_nbs(), admin());
        call.ai = true;
        let (status, _) = send(h.router, call).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(h.audit.recorded().is_empty());
    }

    #[tokio::test]
    async fn an_unchanged_save_is_not_audited() {
        let h = harness(
            empty_db().append_query_results([vec![stored(&["NET_BIND_SERVICE"])]]),
            allow(),
        );
        let body = serde_json::json!({
            "allowed_capabilities": ["NET_BIND_SERVICE"],
            "expected_allowed_capabilities": ["NET_BIND_SERVICE"],
        });
        let (status, _) = send(h.router, put(body, admin())).await;
        assert_eq!(status, StatusCode::OK);
        assert!(h.audit.recorded().is_empty());
    }

    #[test]
    fn the_schemas_are_published() {
        use utoipa::OpenApi;
        let doc = serde_json::to_value(super::super::ApiDoc::openapi()).expect("openapi");
        let paths = &doc["paths"]["/projects/{id}/container-security"];
        assert!(paths["get"].is_object() && paths["put"].is_object());
        let schemas = &doc["components"]["schemas"];
        for name in [
            "ContainerSecurityResponse",
            "UpdateContainerSecurityRequest",
            "ContainerCapabilityOption",
            "ContainerCapability",
        ] {
            assert!(schemas[name].is_object(), "{name} missing");
        }
    }
}
