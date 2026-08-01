use super::types::AppState;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use std::sync::Arc;
use temps_auth::permission_guard;
use temps_auth::RequireAuth;
use tracing::error;
use utoipa::OpenApi;

use super::types::{AuditLogIpInfo, AuditLogResponse, AuditLogUserInfo, ListAuditLogsQuery};

#[derive(OpenApi)]
#[openapi(
    paths(list_audit_logs, get_audit_log),
    components(schemas(AuditLogResponse, ListAuditLogsQuery, AuditLogUserInfo, AuditLogIpInfo)),
    info(
        title = "Audit API",
        description = "API endpoints for managing and retrieving audit logs. \
        Provides detailed tracking of system events, user actions, and security-relevant operations.",
        version = "1.0.0"
    )
)]
pub struct AuditApiDoc;

pub fn configure_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/audit/logs", get(list_audit_logs))
        .route("/audit/logs/{id}", get(get_audit_log))
}

/// List audit logs with optional filtering
#[utoipa::path(
    tag = "Audit Logs",
    get,
    path = "audit/logs",
    params(ListAuditLogsQuery),
    responses(
        (status = 200, description = "List of audit logs", body = Vec<AuditLogResponse>),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error")
    ),
    security(("api_key" = []))
)]
async fn list_audit_logs(
    State(app_state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Query(query): Query<ListAuditLogsQuery>,
) -> Result<impl IntoResponse, temps_core::problemdetails::Problem> {
    permission_guard!(auth, AuditRead);
    let from_date = query.from.map(Into::into);
    let to_date = query.to.map(Into::into);

    match app_state
        .audit_service
        .filter_audit_logs(
            query.operation_type.as_deref(),
            query.user_id,
            from_date,
            to_date,
            query.limit.unwrap_or(100),
            query.offset.unwrap_or(0),
        )
        .await
    {
        Ok(logs) => {
            let responses: Vec<AuditLogResponse> = logs.into_iter().map(Into::into).collect();
            Ok(Json(responses))
        }
        Err(e) => {
            error!("Failed to list audit logs: {}", e);
            Err(
                temps_core::error_builder::ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .type_("https://temps.sh/probs/audit-error")
                    .title("Audit Log Error")
                    .detail(format!("Failed to list audit logs: {}", e))
                    .build(),
            )
        }
    }
}

/// Get a specific audit log entry by ID
#[utoipa::path(
    tag = "Audit Logs",
    get,
    path = "audit/logs/{id}",
    responses(
        (status = 200, description = "Audit log details", body = AuditLogResponse),
        (status = 404, description = "Audit log not found"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error")
    ),
    security(("api_key" = []))
)]
async fn get_audit_log(
    State(app_state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, temps_core::problemdetails::Problem> {
    permission_guard!(auth, AuditRead);
    match app_state.audit_service.get_log_by_id(id).await {
        Ok(Some(log_details)) => Ok(Json(AuditLogResponse::from(log_details))),
        Ok(None) => Err(
            temps_core::error_builder::ErrorBuilder::new(StatusCode::NOT_FOUND)
                .type_("https://temps.sh/probs/not-found")
                .title("Audit Log Not Found")
                .detail("Audit log not found")
                .build(),
        ),
        Err(e) => {
            error!("Failed to get audit log: {}", e);
            Err(
                temps_core::error_builder::ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .type_("https://temps.sh/probs/audit-error")
                    .title("Audit Log Error")
                    .detail(format!("Failed to get audit log: {}", e))
                    .build(),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{get_audit_log, list_audit_logs};
    use crate::handlers::types::{AppState, ListAuditLogsQuery};
    use crate::services::AuditService;
    use axum::extract::{Path, Query, State};
    use axum::http::StatusCode;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use std::sync::Arc;
    use temps_auth::context::AuthContext;
    use temps_auth::permissions::{Permission, Role};
    use temps_auth::RequireAuth;
    use temps_entities::users;
    use temps_geo::{GeoIpService, IpAddressService, MockGeoIpService};

    fn test_user() -> users::Model {
        let now = chrono::Utc::now();
        users::Model {
            id: 1,
            name: "Test User".to_string(),
            email: "test@example.com".to_string(),
            password_hash: None,
            email_verified: true,
            email_verification_token: None,
            email_verification_expires: None,
            password_reset_token: None,
            password_reset_expires: None,
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

    fn app_state() -> Arc<AppState> {
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let geoip = Arc::new(GeoIpService::Mock(MockGeoIpService::new()));
        let ip_service = Arc::new(IpAddressService::new(db.clone(), geoip));
        let audit_service = Arc::new(AuditService::new(db, ip_service));
        Arc::new(AppState { audit_service })
    }

    fn empty_query() -> ListAuditLogsQuery {
        ListAuditLogsQuery {
            operation_type: None,
            user_id: None,
            from: None,
            to: None,
            limit: None,
            offset: None,
        }
    }

    /// Returns the Problem status if the handler rejected the request, or `None`
    /// if it passed the `AuditRead` guard (and proceeded to the service).
    async fn list_rejection_status(auth: AuthContext) -> Option<StatusCode> {
        list_audit_logs(State(app_state()), RequireAuth(auth), Query(empty_query()))
            .await
            .err()
            .map(|p| p.status_code)
    }

    async fn get_rejection_status(auth: AuthContext) -> Option<StatusCode> {
        get_audit_log(State(app_state()), RequireAuth(auth), Path(1))
            .await
            .err()
            .map(|p| p.status_code)
    }

    #[tokio::test]
    async fn audit_endpoints_forbidden_for_low_privilege_roles() {
        for role in [Role::User, Role::Reader] {
            assert_eq!(
                list_rejection_status(AuthContext::new_session(test_user(), role.clone())).await,
                Some(StatusCode::FORBIDDEN),
                "list_audit_logs must return 403 for {role:?}"
            );
            assert_eq!(
                get_rejection_status(AuthContext::new_session(test_user(), role.clone())).await,
                Some(StatusCode::FORBIDDEN),
                "get_audit_log must return 403 for {role:?}"
            );
        }
    }

    #[tokio::test]
    async fn audit_endpoints_pass_guard_for_administration_roles_and_scoped_key() {
        // Administration roles, plus a custom API key explicitly carrying
        // `audit:read`, must pass the permission guard. They then reach the
        // service (which fails on the empty mock DB) -- the point is only that
        // they are NOT rejected with 403.
        let principals: Vec<Box<dyn Fn() -> AuthContext>> = vec![
            Box::new(|| AuthContext::new_session(test_user(), Role::Admin)),
            Box::new(|| AuthContext::new_session(test_user(), Role::PlatformAdmin)),
            Box::new(|| {
                AuthContext::new_api_key(
                    test_user(),
                    None,
                    Some(vec![Permission::AuditRead]),
                    "audit-scoped-key".to_string(),
                    1,
                )
            }),
        ];

        for make_auth in principals {
            assert_ne!(
                list_rejection_status(make_auth()).await,
                Some(StatusCode::FORBIDDEN),
                "list_audit_logs must not 403 an audit:read principal"
            );
            assert_ne!(
                get_rejection_status(make_auth()).await,
                Some(StatusCode::FORBIDDEN),
                "get_audit_log must not 403 an audit:read principal"
            );
        }
    }
}
