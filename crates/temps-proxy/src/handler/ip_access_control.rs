// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use std::sync::Arc;
use temps_authz::permission_guard;
use temps_authz::RequireAuth;
use temps_core::problemdetails::{self, Problem, ProblemDetails};
use temps_core::{AuditLogger, RequestMetadata};
use utoipa::{IntoParams, ToSchema};

use super::audit::{impl_audit_operation, write_audit, AuditContext};
use crate::service::ip_access_control_service::{
    CreateIpAccessControlRequest, IpAccessControlError, IpAccessControlResponse,
    IpAccessControlService, UpdateIpAccessControlRequest,
};

/// Handler state: the service and where audit entries go.
pub struct IpAccessControlState {
    pub service: Arc<IpAccessControlService>,
    pub audit_service: Arc<dyn AuditLogger>,
}

/// The address the console saw for this request, when it parses.
fn caller_ip(metadata: &RequestMetadata) -> Option<IpAddr> {
    metadata.ip_address.trim().parse::<IpAddr>().ok()
}

/// What an audit entry records about a rule.
#[derive(Debug, Clone, Serialize)]
struct IpRuleAuditDetail {
    rule_id: i32,
    ip_address: String,
    action: String,
    reason: Option<String>,
}

impl From<&temps_entities::ip_access_control::Model> for IpRuleAuditDetail {
    fn from(rule: &temps_entities::ip_access_control::Model) -> Self {
        Self {
            rule_id: rule.id,
            ip_address: rule.ip_address.clone(),
            action: rule.action.clone(),
            reason: rule.reason.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct IpAccessRuleCreatedAudit {
    context: AuditContext,
    rule: IpRuleAuditDetail,
    confirmed_broad_range: bool,
    acknowledged_lockout: bool,
}

#[derive(Debug, Clone, Serialize)]
struct IpAccessRuleUpdatedAudit {
    context: AuditContext,
    rule: IpRuleAuditDetail,
    confirmed_broad_range: bool,
    acknowledged_lockout: bool,
}

#[derive(Debug, Clone, Serialize)]
struct IpAccessRuleDeletedAudit {
    context: AuditContext,
    rule: IpRuleAuditDetail,
}

impl_audit_operation!(IpAccessRuleCreatedAudit, "IP_ACCESS_RULE_CREATED");
impl_audit_operation!(IpAccessRuleUpdatedAudit, "IP_ACCESS_RULE_UPDATED");
impl_audit_operation!(IpAccessRuleDeletedAudit, "IP_ACCESS_RULE_DELETED");

impl From<IpAccessControlError> for Problem {
    fn from(error: IpAccessControlError) -> Self {
        match error {
            IpAccessControlError::NotFound(_) => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("IP Access Control Rule Not Found")
                .with_detail(error.to_string()),

            IpAccessControlError::InvalidIpAddress(_) => {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Invalid IP Address")
                    .with_detail(error.to_string())
            }

            IpAccessControlError::DuplicateIp(_) => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Duplicate IP Address")
                .with_detail(error.to_string()),

            IpAccessControlError::RangeTooBroad { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Block Range Needs Confirmation")
                .with_detail(error.to_string()),

            IpAccessControlError::WouldLockOutCaller { .. } => {
                problemdetails::new(StatusCode::CONFLICT)
                    .with_title("Rule Would Lock You Out")
                    .with_detail(error.to_string())
            }

            IpAccessControlError::Database(_) | IpAccessControlError::Internal(_) => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Server Error")
                    .with_detail(error.to_string())
            }
        }
    }
}

/// Query parameters for listing IP access control rules
#[derive(Debug, Deserialize, IntoParams, ToSchema)]
pub struct IpAccessControlQuery {
    /// Filter by action ("block" or "allow")
    pub action: Option<String>,
}

/// List all IP access control rules
#[utoipa::path(
    get,
    path = "/ip-access-control",
    params(IpAccessControlQuery),
    responses(
        (status = 200, description = "List of IP access control rules", body = Vec<IpAccessControlResponse>),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = [])),
    tag = "IP Access Control"
)]
pub async fn list_ip_access_control(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<IpAccessControlState>>,
    Query(query): Query<IpAccessControlQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, LoadBalancerRead);

    let rules = state
        .service
        .list(query.action)
        .await
        .map_err(Problem::from)?;

    let responses: Vec<IpAccessControlResponse> = rules
        .into_iter()
        .map(IpAccessControlResponse::from)
        .collect();

    Ok(Json(responses))
}

/// Get a single IP access control rule by ID
#[utoipa::path(
    get,
    path = "/ip-access-control/{id}",
    params(
        ("id" = i32, Path, description = "IP access control rule ID")
    ),
    responses(
        (status = 200, description = "IP access control rule details", body = IpAccessControlResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "IP access control rule not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = [])),
    tag = "IP Access Control"
)]
pub async fn get_ip_access_control(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<IpAccessControlState>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, LoadBalancerRead);

    let rule = state.service.get_by_id(id).await.map_err(Problem::from)?;

    Ok(Json(IpAccessControlResponse::from(rule)))
}

/// Create a new IP access control rule
#[utoipa::path(
    post,
    path = "/ip-access-control",
    request_body = CreateIpAccessControlRequest,
    responses(
        (status = 201, description = "IP access control rule created", body = IpAccessControlResponse),
        (status = 400, description = "Invalid request", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 409, description = "Duplicate IP address, a block range broader than /8 (IPv4) or /16 (IPv6) without confirm_broad_range, or a block rule that matches the caller's own address without acknowledge_lockout", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = [])),
    tag = "IP Access Control"
)]
pub async fn create_ip_access_control(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<IpAccessControlState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<CreateIpAccessControlRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, LoadBalancerWrite);

    let created_by = Some(auth.user_id());
    let confirmed_broad_range = request.confirm_broad_range;
    let acknowledged_lockout = request.acknowledge_lockout;

    let rule = state
        .service
        .create(request, created_by, caller_ip(&metadata))
        .await
        .map_err(Problem::from)?;

    write_audit(
        state.audit_service.as_ref(),
        &IpAccessRuleCreatedAudit {
            context: AuditContext::new(auth.user_id_opt(), &metadata),
            rule: IpRuleAuditDetail::from(&rule),
            confirmed_broad_range,
            acknowledged_lockout,
        },
    )
    .await;

    Ok((
        StatusCode::CREATED,
        Json(IpAccessControlResponse::from(rule)),
    ))
}

/// Update an IP access control rule
#[utoipa::path(
    patch,
    path = "/ip-access-control/{id}",
    params(
        ("id" = i32, Path, description = "IP access control rule ID")
    ),
    request_body = UpdateIpAccessControlRequest,
    responses(
        (status = 200, description = "IP access control rule updated", body = IpAccessControlResponse),
        (status = 400, description = "Invalid request", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "IP access control rule not found", body = ProblemDetails),
        (status = 409, description = "A block range broader than /8 (IPv4) or /16 (IPv6) without confirm_broad_range, or a block rule that matches the caller's own address without acknowledge_lockout", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = [])),
    tag = "IP Access Control"
)]
pub async fn update_ip_access_control(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<IpAccessControlState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(id): Path<i32>,
    Json(request): Json<UpdateIpAccessControlRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, LoadBalancerWrite);

    let confirmed_broad_range = request.confirm_broad_range;
    let acknowledged_lockout = request.acknowledge_lockout;
    let rule = state
        .service
        .update(id, request, caller_ip(&metadata))
        .await
        .map_err(Problem::from)?;

    write_audit(
        state.audit_service.as_ref(),
        &IpAccessRuleUpdatedAudit {
            context: AuditContext::new(auth.user_id_opt(), &metadata),
            rule: IpRuleAuditDetail::from(&rule),
            confirmed_broad_range,
            acknowledged_lockout,
        },
    )
    .await;

    Ok(Json(IpAccessControlResponse::from(rule)))
}

/// Delete an IP access control rule
#[utoipa::path(
    delete,
    path = "/ip-access-control/{id}",
    params(
        ("id" = i32, Path, description = "IP access control rule ID")
    ),
    responses(
        (status = 204, description = "IP access control rule deleted"),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "IP access control rule not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = [])),
    tag = "IP Access Control"
)]
pub async fn delete_ip_access_control(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<IpAccessControlState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, LoadBalancerWrite);

    let removed = state.service.delete(id).await.map_err(Problem::from)?;

    write_audit(
        state.audit_service.as_ref(),
        &IpAccessRuleDeletedAudit {
            context: AuditContext::new(auth.user_id_opt(), &metadata),
            rule: IpRuleAuditDetail::from(&removed),
        },
    )
    .await;

    Ok(StatusCode::NO_CONTENT)
}

/// Check if an IP address is blocked
#[utoipa::path(
    get,
    path = "/ip-access-control/check/{ip}",
    params(
        ("ip" = String, Path, description = "IP address to check")
    ),
    responses(
        (status = 200, description = "IP block status"),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = [])),
    tag = "IP Access Control"
)]
pub async fn check_ip_blocked(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<IpAccessControlState>>,
    Path(ip): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, LoadBalancerRead);

    let is_blocked = state.service.is_blocked(&ip).await.map_err(Problem::from)?;

    Ok(Json(serde_json::json!({
        "ip": ip,
        "is_blocked": is_blocked
    })))
}

/// Create routes for IP access control handlers
pub fn create_routes() -> axum::Router<Arc<IpAccessControlState>> {
    use axum::routing::{delete, get, patch, post};

    axum::Router::new()
        .route("/ip-access-control", get(list_ip_access_control))
        .route("/ip-access-control", post(create_ip_access_control))
        .route("/ip-access-control/{id}", get(get_ip_access_control))
        .route("/ip-access-control/{id}", patch(update_ip_access_control))
        .route("/ip-access-control/{id}", delete(delete_ip_access_control))
        .route("/ip-access-control/check/{ip}", get(check_ip_blocked))
}

/// Get OpenAPI documentation for IP access control handlers
pub fn openapi() -> utoipa::openapi::OpenApi {
    use utoipa::OpenApi;

    #[derive(OpenApi)]
    #[openapi(
        paths(
            list_ip_access_control,
            get_ip_access_control,
            create_ip_access_control,
            update_ip_access_control,
            delete_ip_access_control,
            check_ip_blocked,
        ),
        components(schemas(
            CreateIpAccessControlRequest,
            UpdateIpAccessControlRequest,
            IpAccessControlResponse,
            IpAccessControlQuery,
        )),
        tags(
            (name = "IP Access Control", description = "IP access control management endpoints")
        )
    )]
    struct IpAccessControlApiDoc;

    IpAccessControlApiDoc::openapi()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handler::audit::{test_metadata, test_user, RecordingAuditLogger};
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
    use temps_authz::context::AuthContext;
    use temps_authz::permissions::Role;
    use temps_entities::ip_access_control;
    use tower::ServiceExt;

    fn state_with(
        db: sea_orm::DatabaseConnection,
        audit: Arc<RecordingAuditLogger>,
    ) -> Arc<IpAccessControlState> {
        Arc::new(IpAccessControlState {
            service: Arc::new(IpAccessControlService::new(Arc::new(db))),
            audit_service: audit,
        })
    }

    fn empty_state(audit: Arc<RecordingAuditLogger>) -> Arc<IpAccessControlState> {
        state_with(
            MockDatabase::new(DatabaseBackend::Postgres).into_connection(),
            audit,
        )
    }

    fn block_request(ip: &str) -> CreateIpAccessControlRequest {
        CreateIpAccessControlRequest {
            ip_address: ip.to_string(),
            action: "block".to_string(),
            reason: None,
            confirm_broad_range: false,
            acknowledge_lockout: false,
        }
    }

    fn rule(id: i32, ip: &str) -> ip_access_control::Model {
        let now = chrono::Utc::now();
        ip_access_control::Model {
            id,
            ip_address: ip.to_string(),
            action: "block".to_string(),
            reason: Some("abuse".to_string()),
            created_by: Some(1),
            created_at: now,
            updated_at: now,
        }
    }

    #[tokio::test]
    async fn a_plain_user_cannot_change_the_instance_block_list() {
        let audit = Arc::new(RecordingAuditLogger::default());
        let user = || RequireAuth(AuthContext::new_session(test_user(), Role::User));

        let created = create_ip_access_control(
            user(),
            State(empty_state(audit.clone())),
            Extension(test_metadata("203.0.113.9")),
            Json(block_request("198.51.100.0/24")),
        )
        .await
        .err()
        .map(|problem| problem.status_code);
        let updated = update_ip_access_control(
            user(),
            State(empty_state(audit.clone())),
            Extension(test_metadata("203.0.113.9")),
            Path(1),
            Json(UpdateIpAccessControlRequest {
                ip_address: Some("0.0.0.0/0".to_string()),
                action: None,
                reason: None,
                confirm_broad_range: true,
                acknowledge_lockout: true,
            }),
        )
        .await
        .err()
        .map(|problem| problem.status_code);
        let deleted = delete_ip_access_control(
            user(),
            State(empty_state(audit.clone())),
            Extension(test_metadata("203.0.113.9")),
            Path(1),
        )
        .await
        .err()
        .map(|problem| problem.status_code);

        assert_eq!(created, Some(StatusCode::FORBIDDEN));
        assert_eq!(updated, Some(StatusCode::FORBIDDEN));
        assert_eq!(deleted, Some(StatusCode::FORBIDDEN));
        assert!(audit.entries().is_empty());
    }

    #[tokio::test]
    async fn the_block_list_cannot_be_written_without_signing_in() {
        let router =
            create_routes().with_state(empty_state(Arc::new(RecordingAuditLogger::default())));
        for (method, uri, body) in [
            (
                "POST",
                "/ip-access-control",
                r#"{"ip_address":"198.51.100.1","action":"block"}"#,
            ),
            ("PATCH", "/ip-access-control/1", r#"{"reason":"x"}"#),
            ("DELETE", "/ip-access-control/1", ""),
        ] {
            let response = router
                .clone()
                .oneshot(
                    axum::http::Request::builder()
                        .method(method)
                        .uri(uri)
                        .header("content-type", "application/json")
                        .body(axum::body::Body::from(body))
                        .expect("request"),
                )
                .await
                .expect("response");
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "{method} {uri}"
            );
        }
    }

    #[tokio::test]
    async fn an_admin_blocking_everyone_gets_a_409_that_names_the_flag_to_send() {
        let audit = Arc::new(RecordingAuditLogger::default());
        let problem = create_ip_access_control(
            RequireAuth(AuthContext::new_session(test_user(), Role::Admin)),
            State(empty_state(audit.clone())),
            Extension(test_metadata("203.0.113.9")),
            Json(block_request("0.0.0.0/0")),
        )
        .await
        .err()
        .expect("a catch-all block must not be saved silently");
        assert_eq!(problem.status_code, StatusCode::CONFLICT);
        let body = serde_json::to_string(&problem.body).expect("problem body");
        assert!(body.contains("confirm_broad_range"), "{body}");
        assert!(audit.entries().is_empty());
    }

    #[tokio::test]
    async fn an_admin_blocking_their_own_address_gets_a_409_that_names_the_flag_to_send() {
        let problem = create_ip_access_control(
            RequireAuth(AuthContext::new_session(test_user(), Role::Admin)),
            State(empty_state(Arc::new(RecordingAuditLogger::default()))),
            Extension(test_metadata("203.0.113.9")),
            Json(block_request("203.0.113.0/24")),
        )
        .await
        .err()
        .expect("a rule that blocks the caller must be acknowledged");
        assert_eq!(problem.status_code, StatusCode::CONFLICT);
        let body = serde_json::to_string(&problem.body).expect("problem body");
        assert!(body.contains("acknowledge_lockout"), "{body}");
        assert!(body.contains("203.0.113.9"), "{body}");
    }

    #[tokio::test]
    async fn deleting_a_rule_writes_an_audit_entry_naming_it() {
        let audit = Arc::new(RecordingAuditLogger::default());
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![rule(5, "198.51.100.0/24")]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .append_query_results([Vec::<ip_access_control::Model>::new()])
            .into_connection();
        let response = delete_ip_access_control(
            RequireAuth(AuthContext::new_session(test_user(), Role::Admin)),
            State(state_with(db, audit.clone())),
            Extension(test_metadata("203.0.113.9")),
            Path(5),
        )
        .await
        .expect("an admin may delete a rule")
        .into_response();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let entries = audit.entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, "IP_ACCESS_RULE_DELETED");
        assert!(entries[0].1.contains("198.51.100.0/24"), "{}", entries[0].1);
        assert!(entries[0].1.contains("203.0.113.9"), "{}", entries[0].1);
    }
}
