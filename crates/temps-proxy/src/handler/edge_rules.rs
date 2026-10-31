// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! HTTP surface for project edge rules (ADR-089).
//!
//! Handlers authorise (`edge_rules:read` / `edge_rules:write`, narrowed to
//! the project), call the service and audit. Every change is enforced by the
//! proxies within seconds of the response: there are no drafts.

use crate::service::edge_rule_service::{
    CreateEdgeRuleRequest, EdgeRuleListResponse, EdgeRuleResponse, EdgeRuleService,
    EdgeRuleServiceError, EvaluateEdgeRulesRequest, EvaluateEdgeRulesResponse, EvaluateRuleInput,
    EvaluateTraceEntry, ReorderEdgeRulesRequest, UpdateEdgeRuleRequest,
};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Extension, Json,
};
use serde::Serialize;
use std::sync::Arc;
use temps_auth::{project_permission_guard, project_scope_guard, RequireAuth};
use temps_core::problemdetails::{self, Problem, ProblemDetails};
use temps_core::{AuditLogger, AuditOperation, RequestMetadata};
use temps_edge_rules::{
    Action, Combinator, Condition, Decision, Expression, Field, Group, Mode, Operator, RuleIssue,
    RuleOutcome, SampleHeader, SampleRequest,
};
use tracing::error;

pub struct EdgeRulesState {
    pub service: Arc<EdgeRuleService>,
    pub audit_service: Arc<dyn AuditLogger>,
    pub project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
}

impl From<EdgeRuleServiceError> for Problem {
    fn from(error: EdgeRuleServiceError) -> Self {
        match error {
            EdgeRuleServiceError::ProjectNotFound { .. } => {
                problemdetails::new(StatusCode::NOT_FOUND)
                    .with_title("Project Not Found")
                    .with_detail(error.to_string())
            }
            EdgeRuleServiceError::EnvironmentNotFound { .. } => {
                problemdetails::new(StatusCode::NOT_FOUND)
                    .with_title("Environment Not Found")
                    .with_detail(error.to_string())
            }
            EdgeRuleServiceError::RuleNotFound { .. } => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Edge Rule Not Found")
                .with_detail(error.to_string()),
            EdgeRuleServiceError::Invalid { ref issues, .. } => {
                // `issues` is `Vec<RuleIssue>`; serialising it cannot fail.
                let issues = serde_json::to_value(issues).unwrap_or(serde_json::Value::Null);
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Invalid Edge Rule")
                    .with_detail(error.to_string())
                    .with_value("issues", issues)
            }
            EdgeRuleServiceError::TooManyRules { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Edge Rule Limit Reached")
                .with_detail(error.to_string()),
            EdgeRuleServiceError::RevisionConflict { current, .. } => {
                problemdetails::new(StatusCode::CONFLICT)
                    .with_title("Edge Rule Changed")
                    .with_detail(error.to_string())
                    .with_value("current_revision", current)
            }
            EdgeRuleServiceError::InvalidOrder { .. } => {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Invalid Edge Rule Order")
                    .with_detail(error.to_string())
            }
            EdgeRuleServiceError::Unreadable { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Unreadable Edge Rule")
                    .with_detail(error.to_string())
            }
            EdgeRuleServiceError::Database { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Server Error")
                    .with_detail(error.to_string())
            }
        }
    }
}

// ---- audit -------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
struct EdgeRuleAuditContext {
    user_id: Option<i32>,
    ip_address: Option<String>,
    user_agent: String,
}

#[derive(Debug, Clone, Serialize)]
struct EdgeRuleCreatedAudit {
    context: EdgeRuleAuditContext,
    project_id: i32,
    rule: EdgeRuleResponse,
}

#[derive(Debug, Clone, Serialize)]
struct EdgeRuleUpdatedAudit {
    context: EdgeRuleAuditContext,
    project_id: i32,
    rule_id: i32,
    before: EdgeRuleResponse,
    after: EdgeRuleResponse,
}

#[derive(Debug, Clone, Serialize)]
struct EdgeRuleDeletedAudit {
    context: EdgeRuleAuditContext,
    project_id: i32,
    rule: EdgeRuleResponse,
}

#[derive(Debug, Clone, Serialize)]
struct EdgeRulesReorderedAudit {
    context: EdgeRuleAuditContext,
    project_id: i32,
    rule_ids: Vec<i32>,
}

macro_rules! impl_audit_operation {
    ($ty:ty, $name:literal) => {
        impl AuditOperation for $ty {
            fn operation_type(&self) -> String {
                $name.to_string()
            }

            fn user_id(&self) -> Option<i32> {
                self.context.user_id
            }

            fn ip_address(&self) -> Option<String> {
                self.context.ip_address.clone()
            }

            fn user_agent(&self) -> &str {
                &self.context.user_agent
            }

            fn serialize(&self) -> anyhow::Result<String> {
                serde_json::to_string(self).map_err(|e| {
                    anyhow::anyhow!("Failed to serialize audit operation {}: {}", $name, e)
                })
            }
        }
    };
}

impl_audit_operation!(EdgeRuleCreatedAudit, "EDGE_RULE_CREATED");
impl_audit_operation!(EdgeRuleUpdatedAudit, "EDGE_RULE_UPDATED");
impl_audit_operation!(EdgeRuleDeletedAudit, "EDGE_RULE_DELETED");
impl_audit_operation!(EdgeRulesReorderedAudit, "EDGE_RULES_REORDERED");

fn audit_context(user_id: Option<i32>, metadata: &RequestMetadata) -> EdgeRuleAuditContext {
    EdgeRuleAuditContext {
        user_id,
        ip_address: Some(metadata.ip_address.clone()),
        user_agent: metadata.user_agent.clone(),
    }
}

async fn write_audit(state: &EdgeRulesState, audit: &dyn AuditOperation) {
    // The write already succeeded; a failed audit entry is reported, not fatal.
    if let Err(e) = state.audit_service.create_audit_log(audit).await {
        error!(
            "Failed to create audit log for {}: {}",
            audit.operation_type(),
            e
        );
    }
}

// ---- handlers ----------------------------------------------------------

/// List a project's edge rules
///
/// Returns every rule in evaluation order (at most 100), with match counts.
#[utoipa::path(
    tag = "Edge Rules",
    get,
    path = "/projects/{project_id}/edge-rules",
    params(("project_id" = i32, Path, description = "Project ID")),
    responses(
        (status = 200, description = "The project's rules in evaluation order", body = EdgeRuleListResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Project not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_edge_rules(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<EdgeRulesState>>,
    Path(project_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    project_permission_guard!(
        auth,
        EdgeRulesRead,
        project_id,
        state.project_access_checker
    );
    project_scope_guard!(auth, project_id);
    Ok(Json(state.service.list(project_id).await?))
}

/// Get one edge rule
#[utoipa::path(
    tag = "Edge Rules",
    get,
    path = "/projects/{project_id}/edge-rules/{rule_id}",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("rule_id" = i32, Path, description = "Edge rule ID")
    ),
    responses(
        (status = 200, description = "The rule", body = EdgeRuleResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Project or rule not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_edge_rule(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<EdgeRulesState>>,
    Path((project_id, rule_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    project_permission_guard!(
        auth,
        EdgeRulesRead,
        project_id,
        state.project_access_checker
    );
    project_scope_guard!(auth, project_id);
    Ok(Json(state.service.get(project_id, rule_id).await?))
}

/// Create an edge rule
///
/// The rule is enforced by every proxy within seconds. Create it with
/// `mode: log_only` to watch what it would match before it acts.
#[utoipa::path(
    tag = "Edge Rules",
    post,
    path = "/projects/{project_id}/edge-rules",
    params(("project_id" = i32, Path, description = "Project ID")),
    request_body = CreateEdgeRuleRequest,
    responses(
        (status = 201, description = "Rule created", body = EdgeRuleResponse),
        (status = 400, description = "Invalid rule; `issues` lists each problem", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Project or environment not found", body = ProblemDetails),
        (status = 409, description = "The project already has 100 rules", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn create_edge_rule(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<EdgeRulesState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(project_id): Path<i32>,
    Json(request): Json<CreateEdgeRuleRequest>,
) -> Result<impl IntoResponse, Problem> {
    project_permission_guard!(
        auth,
        EdgeRulesWrite,
        project_id,
        state.project_access_checker
    );
    project_scope_guard!(auth, project_id);
    let user_id = auth.user_id_opt();
    let rule = state.service.create(project_id, request, user_id).await?;
    write_audit(
        &state,
        &EdgeRuleCreatedAudit {
            context: audit_context(user_id, &metadata),
            project_id,
            rule: rule.clone(),
        },
    )
    .await;
    Ok((StatusCode::CREATED, Json(rule)))
}

/// Change an edge rule
///
/// Absent fields keep their value. Send the `revision` you edited so a
/// concurrent change is refused (409) instead of overwritten.
#[utoipa::path(
    tag = "Edge Rules",
    patch,
    path = "/projects/{project_id}/edge-rules/{rule_id}",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("rule_id" = i32, Path, description = "Edge rule ID")
    ),
    request_body = UpdateEdgeRuleRequest,
    responses(
        (status = 200, description = "Rule updated", body = EdgeRuleResponse),
        (status = 400, description = "Invalid rule; `issues` lists each problem", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Project, rule or environment not found", body = ProblemDetails),
        (status = 409, description = "The rule changed since `revision`", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn update_edge_rule(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<EdgeRulesState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, rule_id)): Path<(i32, i32)>,
    Json(request): Json<UpdateEdgeRuleRequest>,
) -> Result<impl IntoResponse, Problem> {
    project_permission_guard!(
        auth,
        EdgeRulesWrite,
        project_id,
        state.project_access_checker
    );
    project_scope_guard!(auth, project_id);
    let user_id = auth.user_id_opt();
    let (before, after) = state
        .service
        .update(project_id, rule_id, request, user_id)
        .await?;
    write_audit(
        &state,
        &EdgeRuleUpdatedAudit {
            context: audit_context(user_id, &metadata),
            project_id,
            rule_id,
            before,
            after: after.clone(),
        },
    )
    .await;
    Ok(Json(after))
}

/// Delete an edge rule
#[utoipa::path(
    tag = "Edge Rules",
    delete,
    path = "/projects/{project_id}/edge-rules/{rule_id}",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("rule_id" = i32, Path, description = "Edge rule ID")
    ),
    responses(
        (status = 204, description = "Rule deleted"),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Project or rule not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn delete_edge_rule(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<EdgeRulesState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, rule_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    project_permission_guard!(
        auth,
        EdgeRulesWrite,
        project_id,
        state.project_access_checker
    );
    project_scope_guard!(auth, project_id);
    let rule = state.service.delete(project_id, rule_id, None).await?;
    write_audit(
        &state,
        &EdgeRuleDeletedAudit {
            context: audit_context(auth.user_id_opt(), &metadata),
            project_id,
            rule,
        },
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

/// Reorder a project's edge rules
///
/// `rule_ids` must list every rule of the project once, in the new order.
#[utoipa::path(
    tag = "Edge Rules",
    put,
    path = "/projects/{project_id}/edge-rules/order",
    params(("project_id" = i32, Path, description = "Project ID")),
    request_body = ReorderEdgeRulesRequest,
    responses(
        (status = 200, description = "The rules in their new order", body = EdgeRuleListResponse),
        (status = 400, description = "The list does not name every rule exactly once", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Project not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn reorder_edge_rules(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<EdgeRulesState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(project_id): Path<i32>,
    Json(request): Json<ReorderEdgeRulesRequest>,
) -> Result<impl IntoResponse, Problem> {
    project_permission_guard!(
        auth,
        EdgeRulesWrite,
        project_id,
        state.project_access_checker
    );
    project_scope_guard!(auth, project_id);
    let rule_ids = request.rule_ids.clone();
    let list = state.service.reorder(project_id, request).await?;
    write_audit(
        &state,
        &EdgeRulesReorderedAudit {
            context: audit_context(auth.user_id_opt(), &metadata),
            project_id,
            rule_ids,
        },
    )
    .await;
    Ok(Json(list))
}

/// Test a request against the edge rules
///
/// A dry run: what the proxy would do with the described request, against
/// the saved rules or the `rules` supplied. Nothing is stored or counted.
#[utoipa::path(
    tag = "Edge Rules",
    post,
    path = "/projects/{project_id}/edge-rules/evaluate",
    params(("project_id" = i32, Path, description = "Project ID")),
    request_body = EvaluateEdgeRulesRequest,
    responses(
        (status = 200, description = "The decision and a per-rule trace", body = EvaluateEdgeRulesResponse),
        (status = 400, description = "A supplied rule is invalid", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Project not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn evaluate_edge_rules(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<EdgeRulesState>>,
    Path(project_id): Path<i32>,
    Json(request): Json<EvaluateEdgeRulesRequest>,
) -> Result<impl IntoResponse, Problem> {
    project_permission_guard!(
        auth,
        EdgeRulesRead,
        project_id,
        state.project_access_checker
    );
    project_scope_guard!(auth, project_id);
    Ok(Json(state.service.evaluate(project_id, request).await?))
}

pub fn create_routes() -> axum::Router<Arc<EdgeRulesState>> {
    use axum::routing::{get, post, put};

    axum::Router::new()
        .route(
            "/projects/{project_id}/edge-rules",
            get(list_edge_rules).post(create_edge_rule),
        )
        .route(
            "/projects/{project_id}/edge-rules/order",
            put(reorder_edge_rules),
        )
        .route(
            "/projects/{project_id}/edge-rules/evaluate",
            post(evaluate_edge_rules),
        )
        .route(
            "/projects/{project_id}/edge-rules/{rule_id}",
            get(get_edge_rule)
                .patch(update_edge_rule)
                .delete(delete_edge_rule),
        )
}

pub fn openapi() -> utoipa::openapi::OpenApi {
    use utoipa::OpenApi;

    #[derive(OpenApi)]
    #[openapi(
        paths(
            list_edge_rules,
            get_edge_rule,
            create_edge_rule,
            update_edge_rule,
            delete_edge_rule,
            reorder_edge_rules,
            evaluate_edge_rules,
        ),
        components(schemas(
            EdgeRuleListResponse,
            EdgeRuleResponse,
            CreateEdgeRuleRequest,
            UpdateEdgeRuleRequest,
            ReorderEdgeRulesRequest,
            EvaluateEdgeRulesRequest,
            EvaluateEdgeRulesResponse,
            EvaluateRuleInput,
            EvaluateTraceEntry,
            Group,
            Expression,
            Condition,
            Combinator,
            Field,
            Operator,
            Action,
            Mode,
            Decision,
            RuleOutcome,
            RuleIssue,
            SampleRequest,
            SampleHeader,
        )),
        tags(
            (name = "Edge Rules", description = "Ordered if/then rules the proxy applies to a project's requests before they reach the app")
        )
    )]
    struct EdgeRulesApiDoc;

    EdgeRulesApiDoc::openapi()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The spec is one global namespace: a generic name such as `Decision`
    /// silently replaces another crate's schema of that name.
    #[test]
    fn every_schema_is_namespaced_to_edge_rules() {
        let spec = openapi();
        let schemas = spec.components.expect("components").schemas;
        assert!(schemas.contains_key("EdgeRuleDecision"));
        // ProblemDetails is the shared error schema, the same everywhere.
        for name in schemas.keys().filter(|n| n.as_str() != "ProblemDetails") {
            assert!(
                name.contains("EdgeRule"),
                "schema {name} could collide with another crate's schema"
            );
        }
    }
    use crate::service::edge_rule_service::tests::{database, project};
    use axum::body::Body;
    use axum::http::{header, Method, Request};
    use temps_auth::{AuthContext, Role};
    use tower::ServiceExt;

    #[derive(Default)]
    struct RecordingAudit {
        entries: std::sync::Mutex<Vec<(String, String)>>,
    }

    impl RecordingAudit {
        fn recorded(&self) -> Vec<(String, String)> {
            self.entries
                .lock()
                .map(|entries| entries.clone())
                .unwrap_or_default()
        }
    }

    #[async_trait::async_trait]
    impl AuditLogger for RecordingAudit {
        async fn create_audit_log(&self, operation: &dyn AuditOperation) -> anyhow::Result<()> {
            let payload = operation.serialize()?;
            if let Ok(mut entries) = self.entries.lock() {
                entries.push((operation.operation_type(), payload));
            }
            Ok(())
        }
    }

    fn user() -> temps_entities::users::Model {
        let now = chrono::Utc::now();
        temps_entities::users::Model {
            id: 1,
            name: "Test User".to_string(),
            email: "user1@example.test".to_string(),
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

    fn metadata() -> RequestMetadata {
        RequestMetadata {
            ip_address: "203.0.113.9".to_string(),
            user_agent: "route-test".to_string(),
            headers: axum::http::HeaderMap::new(),
            visitor_id_cookie: None,
            session_id_cookie: None,
            base_url: "http://console.test".to_string(),
            scheme: "http".to_string(),
            host: "console.test".to_string(),
            is_secure: false,
        }
    }

    fn app(
        db: Arc<sea_orm::DatabaseConnection>,
        auth: Option<AuthContext>,
        audit: Arc<RecordingAudit>,
    ) -> axum::Router {
        let state = Arc::new(EdgeRulesState {
            service: Arc::new(EdgeRuleService::new(db)),
            audit_service: audit,
            project_access_checker: None,
        });
        create_routes()
            .layer(axum::middleware::from_fn(
                move |mut request: axum::extract::Request, next: axum::middleware::Next| {
                    let auth = auth.clone();
                    async move {
                        request.extensions_mut().insert(metadata());
                        if let Some(auth) = auth {
                            request.extensions_mut().insert(auth);
                        }
                        next.run(request).await
                    }
                },
            ))
            .with_state(state)
    }

    fn request(method: Method, uri: &str, body: Option<serde_json::Value>) -> Request<Body> {
        let builder = Request::builder().method(method).uri(uri);
        match body {
            Some(body) => builder
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string())),
            None => builder.body(Body::empty()),
        }
        .expect("request builds")
    }

    async fn send(app: axum::Router, request: Request<Body>) -> (StatusCode, serde_json::Value) {
        let response = app.oneshot(request).await.expect("router answers");
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body reads");
        let body = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, body)
    }

    fn block_admin() -> serde_json::Value {
        serde_json::json!({
            "expression": {"combinator": "all", "children": [
                {"kind": "condition", "field": "path", "op": "starts_with", "values": ["/admin"], "case_insensitive": true}
            ]},
            "action": {"type": "block", "status": 403}
        })
    }

    #[test]
    fn errors_map_to_statuses_and_carry_the_issues() {
        let problem = Problem::from(EdgeRuleServiceError::Invalid {
            project_id: 3,
            rule: "r".into(),
            issues: vec![RuleIssue {
                target: "c1".into(),
                message: "Enter a value.".into(),
            }],
        });
        assert_eq!(problem.status_code, StatusCode::BAD_REQUEST);
        let body = serde_json::to_value(&problem.body).unwrap_or_default();
        assert_eq!(body["issues"][0]["target"], "c1", "body: {body}");

        let problem = Problem::from(EdgeRuleServiceError::RevisionConflict {
            project_id: 3,
            rule_id: 4,
            expected: 1,
            current: 2,
        });
        assert_eq!(problem.status_code, StatusCode::CONFLICT);
        let body = serde_json::to_value(&problem.body).unwrap_or_default();
        assert_eq!(body["current_revision"], 2, "body: {body}");

        assert_eq!(
            Problem::from(EdgeRuleServiceError::RuleNotFound {
                project_id: 3,
                rule_id: 4
            })
            .status_code,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            Problem::from(EdgeRuleServiceError::TooManyRules {
                project_id: 3,
                max: 100
            })
            .status_code,
            StatusCode::CONFLICT
        );
    }

    #[tokio::test]
    async fn unauthenticated_and_read_only_callers_are_refused() {
        let Some(test_db) = database().await else {
            return;
        };
        let db = test_db.connection_arc();
        let (project_id, _) = project(&test_db, "edge-routes-auth").await;
        let audit = Arc::new(RecordingAudit::default());
        let uri = format!("/projects/{project_id}/edge-rules");

        let (status, _) = send(
            app(db.clone(), None, audit.clone()),
            request(Method::GET, &uri, None),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        let reader = AuthContext::new_session(user(), Role::Reader);
        let (status, body) = send(
            app(db.clone(), Some(reader.clone()), audit.clone()),
            request(Method::GET, &uri, None),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "readers can list: {body}");
        let (status, body) = send(
            app(db.clone(), Some(reader), audit.clone()),
            request(Method::POST, &uri, Some(block_admin())),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");
        assert!(audit.recorded().is_empty());
    }

    #[tokio::test]
    async fn rules_are_managed_over_http_and_every_write_is_audited() {
        let Some(test_db) = database().await else {
            return;
        };
        let db = test_db.connection_arc();
        let (project_id, _) = project(&test_db, "edge-routes-crud").await;
        sea_orm::ConnectionTrait::execute_unprepared(
            db.as_ref(),
            "INSERT INTO users (id, name, email, created_at, updated_at) VALUES \
             (1, 'Test User', 'user1@example.test', now(), now()) ON CONFLICT DO NOTHING",
        )
        .await
        .expect("seed the acting user");
        let audit = Arc::new(RecordingAudit::default());
        let admin = AuthContext::new_session(user(), Role::Admin);
        let call = |method: Method, uri: String, body: Option<serde_json::Value>| {
            send(
                app(db.clone(), Some(admin.clone()), audit.clone()),
                request(method, &uri, body),
            )
        };
        let base = format!("/projects/{project_id}/edge-rules");

        let (status, created) = call(Method::POST, base.clone(), Some(block_admin())).await;
        assert_eq!(status, StatusCode::CREATED, "body: {created}");
        let id = created["id"].as_i64().unwrap_or_default();
        assert_eq!(created["mode"], "log_only", "rules start log-only");
        assert_eq!(created["revision"], 1);

        let (status, second) = call(
            Method::POST,
            base.clone(),
            Some(serde_json::json!({
                "name": "Watch the API",
                "mode": "log_only",
                "expression": {"combinator": "all", "children": [
                    {"kind": "condition", "field": "path", "op": "starts_with", "values": ["/api"]}
                ]},
                "action": {"type": "log"}
            })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "body: {second}");
        let second_id = second["id"].as_i64().unwrap_or_default();

        let (status, invalid) = call(
            Method::POST,
            base.clone(),
            Some(serde_json::json!({
                "expression": {"combinator": "all", "children": [
                    {"kind": "condition", "field": "path", "op": "starts_with", "values": []}
                ]},
                "action": {"type": "block", "status": 403}
            })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "body: {invalid}");
        assert_eq!(invalid["title"], "Invalid Edge Rule");
        assert!(invalid["issues"].as_array().is_some_and(|i| !i.is_empty()));

        let (status, conflict) = call(
            Method::PATCH,
            format!("{base}/{id}"),
            Some(serde_json::json!({"revision": 7, "enabled": false})),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "body: {conflict}");
        assert_eq!(conflict["current_revision"], 1);

        let (status, updated) = call(
            Method::PATCH,
            format!("{base}/{id}"),
            Some(serde_json::json!({"revision": 1, "mode": "log_only"})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body: {updated}");
        assert_eq!(updated["mode"], "log_only");

        let (status, ordered) = call(
            Method::PUT,
            format!("{base}/order"),
            Some(serde_json::json!({"rule_ids": [second_id, id]})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body: {ordered}");
        assert_eq!(ordered["rules"][0]["id"], second_id);

        let (status, evaluated) = call(
            Method::POST,
            format!("{base}/evaluate"),
            Some(serde_json::json!({"request": {"url": "https://app.example.test/api/x"}})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body: {evaluated}");
        assert_eq!(evaluated["decision"]["kind"], "forward");
        assert_eq!(evaluated["matched_rule_id"], second_id);
        assert_eq!(evaluated["matched_mode"], "log_only");

        let (status, _) = call(Method::DELETE, format!("{base}/{id}"), None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, missing) = call(Method::GET, format!("{base}/{id}"), None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "body: {missing}");

        let names: Vec<String> = audit.recorded().into_iter().map(|(n, _)| n).collect();
        assert_eq!(
            names,
            [
                "EDGE_RULE_CREATED",
                "EDGE_RULE_CREATED",
                "EDGE_RULE_UPDATED",
                "EDGE_RULES_REORDERED",
                "EDGE_RULE_DELETED"
            ]
        );
        let updated_audit = &audit.recorded()[2].1;
        assert!(
            updated_audit.contains(r#""before""#) && updated_audit.contains(r#""after""#),
            "{updated_audit}"
        );
    }
}
