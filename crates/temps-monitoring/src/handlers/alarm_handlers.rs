//! HTTP handlers for the unified alarms API (Phase 1 of ADR-025).
//!
//! Exposes the EXISTING `AlarmService` query methods over HTTP.
//! No migration, no new table, no changes to how alarms are written.
//!
//! # Route layout (all nested under `/api`)
//!
//! ```text
//! GET  /projects/{project_id}/alarms              — list, paginated, filterable
//! GET  /projects/{project_id}/alarms/summary      — counts by status/severity/type
//! POST /projects/{project_id}/alarms/{alarm_id}/acknowledge
//! POST /projects/{project_id}/alarms/{alarm_id}/resolve
//! ```

use std::sync::Arc;

use axum::{
    extract::{Extension, Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use temps_auth::{permission_guard, project_scope_guard, RequireAuth};
use temps_core::{
    problemdetails::{self, Problem},
    AuditContext, AuditOperation, RequestMetadata,
};
use tracing::error;
use utoipa::{IntoParams, OpenApi, ToSchema};

use crate::alarm_service::{AlarmError, AlarmFilters, AlarmService, AlarmSummary, AlarmType};

// ---------------------------------------------------------------------------
// AppState
// ---------------------------------------------------------------------------

pub struct AlarmAppState {
    pub alarm_service: Arc<AlarmService>,
    pub audit_service: Arc<dyn temps_core::AuditLogger>,
}

// ---------------------------------------------------------------------------
// Error → Problem conversion
// ---------------------------------------------------------------------------

impl From<AlarmError> for Problem {
    fn from(error: AlarmError) -> Self {
        match error {
            AlarmError::NotFound { .. } => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Alarm Not Found")
                .with_detail(error.to_string()),

            AlarmError::Database { .. } => problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Database Error")
                .with_detail(error.to_string()),

            AlarmError::Notification { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Notification Error")
                    .with_detail(error.to_string())
            }

            AlarmError::Queue { .. } => problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Queue Error")
                .with_detail(error.to_string()),
        }
    }
}

// ---------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------

/// Full alarm representation returned by list/summary endpoints.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AlarmResponse {
    pub id: i32,
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub deployment_id: Option<i32>,
    pub container_id: Option<i32>,
    pub service_id: Option<i32>,
    pub alarm_type: String,
    pub severity: String,
    pub status: String,
    pub title: String,
    pub message: Option<String>,
    /// Arbitrary JSON metadata attached by the alarm source.
    pub metadata: Option<serde_json::Value>,
    /// ISO-8601 UTC timestamp when the alarm fired.
    pub fired_at: String,
    /// ISO-8601 UTC timestamp when the alarm was acknowledged, if any.
    pub acknowledged_at: Option<String>,
    /// User ID who acknowledged the alarm, if any.
    pub acknowledged_by: Option<i32>,
    /// ISO-8601 UTC timestamp when the alarm was resolved, if any.
    pub resolved_at: Option<String>,
    /// ISO-8601 UTC timestamp when the row was created.
    pub created_at: String,
    /// ISO-8601 UTC timestamp when the row was last updated.
    pub updated_at: String,
}

impl From<temps_entities::alarms::Model> for AlarmResponse {
    fn from(m: temps_entities::alarms::Model) -> Self {
        Self {
            id: m.id,
            project_id: m.project_id,
            environment_id: m.environment_id,
            deployment_id: m.deployment_id,
            container_id: m.container_id,
            service_id: m.service_id,
            alarm_type: m.alarm_type,
            severity: m.severity,
            status: m.status,
            title: m.title,
            message: m.message,
            metadata: m.metadata,
            fired_at: m.fired_at.to_rfc3339(),
            acknowledged_at: m.acknowledged_at.map(|t| t.to_rfc3339()),
            acknowledged_by: m.acknowledged_by,
            resolved_at: m.resolved_at.map(|t| t.to_rfc3339()),
            created_at: m.created_at.to_rfc3339(),
            updated_at: m.updated_at.to_rfc3339(),
        }
    }
}

/// Paginated list of alarms.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct AlarmListResponse {
    pub items: Vec<AlarmResponse>,
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
}

/// Re-export AlarmSummary for the OpenAPI schema.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct AlarmSummaryResponse {
    pub total_active: u32,
    pub firing: u32,
    pub acknowledged: u32,
    pub critical: u32,
    pub warning: u32,
    pub by_type: std::collections::HashMap<String, u32>,
}

impl From<AlarmSummary> for AlarmSummaryResponse {
    fn from(s: AlarmSummary) -> Self {
        Self {
            total_active: s.total_active,
            firing: s.firing,
            acknowledged: s.acknowledged,
            critical: s.critical,
            warning: s.warning,
            by_type: s.by_type,
        }
    }
}

// ---------------------------------------------------------------------------
// Query params
// ---------------------------------------------------------------------------

/// Query parameters for `GET /projects/{project_id}/alarms`.
#[derive(Debug, Default, Deserialize, IntoParams)]
pub struct ListAlarmsQuery {
    /// Filter by alarm type (e.g. `container_restart`, `outage`).
    pub alarm_type: Option<String>,
    /// Filter by status: `firing`, `acknowledged`, or `resolved`.
    pub status: Option<String>,
    /// Filter by severity: `info`, `warning`, or `critical`.
    pub severity: Option<String>,
    /// Filter by environment ID.
    pub environment_id: Option<i32>,
    /// Filter by deployment ID.
    pub deployment_id: Option<i32>,
    /// Filter by external service ID.
    pub service_id: Option<i32>,
    /// Page number (1-based, default 1).
    pub page: Option<u64>,
    /// Items per page (default 20, max 100).
    pub page_size: Option<u64>,
}

// ---------------------------------------------------------------------------
// Audit structs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
struct AlarmAcknowledgedAudit {
    context: AuditContext,
    alarm_id: i32,
    project_id: i32,
}

impl AuditOperation for AlarmAcknowledgedAudit {
    fn operation_type(&self) -> String {
        "ALARM_ACKNOWLEDGED".to_string()
    }

    fn user_id(&self) -> i32 {
        self.context.user_id
    }

    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }

    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }

    fn serialize(&self) -> anyhow::Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize AlarmAcknowledgedAudit: {}", e))
    }
}

#[derive(Debug, Clone, Serialize)]
struct AlarmResolvedAudit {
    context: AuditContext,
    alarm_id: i32,
    project_id: i32,
}

impl AuditOperation for AlarmResolvedAudit {
    fn operation_type(&self) -> String {
        "ALARM_RESOLVED".to_string()
    }

    fn user_id(&self) -> i32 {
        self.context.user_id
    }

    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }

    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }

    fn serialize(&self) -> anyhow::Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize AlarmResolvedAudit: {}", e))
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// List alarms for a project with optional filters.
#[utoipa::path(
    get,
    path = "/projects/{project_id}/alarms",
    tag = "Alarms",
    operation_id = "listProjectAlarms",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ListAlarmsQuery,
    ),
    responses(
        (status = 200, description = "Paginated list of alarms", body = AlarmListResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_project_alarms(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AlarmAppState>>,
    Path(project_id): Path<i32>,
    Query(params): Query<ListAlarmsQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentsRead);
    project_scope_guard!(auth, project_id);

    let page = params.page.unwrap_or(1);
    let page_size = params.page_size.unwrap_or(20);

    // Parse optional alarm_type filter
    let alarm_type_filter = match &params.alarm_type {
        Some(s) => match AlarmType::parse_alarm_type(s) {
            Some(t) => Some(t),
            None => {
                return Err(problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Invalid alarm_type")
                    .with_detail(format!("Unknown alarm type: {}", s)));
            }
        },
        None => None,
    };

    // Parse optional status filter
    let status_filter = match params.status.as_deref() {
        Some("firing") => Some(crate::alarm_service::AlarmStatus::Firing),
        Some("acknowledged") => Some(crate::alarm_service::AlarmStatus::Acknowledged),
        Some("resolved") => Some(crate::alarm_service::AlarmStatus::Resolved),
        Some(s) => {
            return Err(problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Invalid status")
                .with_detail(format!(
                    "Unknown alarm status '{}': must be firing, acknowledged, or resolved",
                    s
                )));
        }
        None => None,
    };

    // Parse optional severity filter
    let severity_filter = match params.severity.as_deref() {
        Some("info") => Some(crate::alarm_service::AlarmSeverity::Info),
        Some("warning") => Some(crate::alarm_service::AlarmSeverity::Warning),
        Some("critical") => Some(crate::alarm_service::AlarmSeverity::Critical),
        Some(s) => {
            return Err(problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Invalid severity")
                .with_detail(format!(
                    "Unknown severity '{}': must be info, warning, or critical",
                    s
                )));
        }
        None => None,
    };

    let filters = AlarmFilters {
        environment_id: params.environment_id,
        deployment_id: params.deployment_id,
        alarm_type: alarm_type_filter,
        status: status_filter,
        severity: severity_filter,
    };

    let (items, total) = state
        .alarm_service
        .list_alarms(project_id, filters, page, page_size)
        .await
        .map_err(Problem::from)?;

    let effective_page_size = std::cmp::min(page_size, 100);

    Ok(Json(AlarmListResponse {
        items: items.into_iter().map(AlarmResponse::from).collect(),
        total,
        page,
        page_size: effective_page_size,
    }))
}

/// Get alarm counts by status/severity/type for a project (dashboard summary widget).
#[utoipa::path(
    get,
    path = "/projects/{project_id}/alarms/summary",
    tag = "Alarms",
    operation_id = "getProjectAlarmsSummary",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
    ),
    responses(
        (status = 200, description = "Alarm summary counts", body = AlarmSummaryResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_project_alarms_summary(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AlarmAppState>>,
    Path(project_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentsRead);
    project_scope_guard!(auth, project_id);

    let summary = state
        .alarm_service
        .get_alarm_summary(project_id)
        .await
        .map_err(Problem::from)?;

    Ok(Json(AlarmSummaryResponse::from(summary)))
}

/// Acknowledge a firing alarm (marks it as seen but not resolved).
#[utoipa::path(
    post,
    path = "/projects/{project_id}/alarms/{alarm_id}/acknowledge",
    tag = "Alarms",
    operation_id = "acknowledgeAlarm",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("alarm_id" = i32, Path, description = "Alarm ID"),
    ),
    responses(
        (status = 200, description = "Alarm acknowledged"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Alarm not found"),
        (status = 500, description = "Internal server error"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn acknowledge_alarm(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AlarmAppState>>,
    Path((project_id, alarm_id)): Path<(i32, i32)>,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentsWrite);
    project_scope_guard!(auth, project_id);

    state
        .alarm_service
        .acknowledge_alarm(alarm_id, project_id, auth.user_id())
        .await
        .map_err(Problem::from)?;

    // Audit log — failure is non-fatal
    let audit = AlarmAcknowledgedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        alarm_id,
        project_id,
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!(
            "Failed to create audit log for alarm acknowledge {}: {}",
            alarm_id, e
        );
    }

    Ok(StatusCode::OK)
}

/// Resolve an alarm.
#[utoipa::path(
    post,
    path = "/projects/{project_id}/alarms/{alarm_id}/resolve",
    tag = "Alarms",
    operation_id = "resolveAlarm",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("alarm_id" = i32, Path, description = "Alarm ID"),
    ),
    responses(
        (status = 200, description = "Alarm resolved"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Alarm not found"),
        (status = 500, description = "Internal server error"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn resolve_alarm(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AlarmAppState>>,
    Path((project_id, alarm_id)): Path<(i32, i32)>,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentsWrite);
    project_scope_guard!(auth, project_id);

    state
        .alarm_service
        .resolve_alarm(alarm_id, project_id)
        .await
        .map_err(Problem::from)?;

    // Audit log — failure is non-fatal
    let audit = AlarmResolvedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        alarm_id,
        project_id,
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!(
            "Failed to create audit log for alarm resolve {}: {}",
            alarm_id, e
        );
    }

    Ok(StatusCode::OK)
}

// ---------------------------------------------------------------------------
// Route configuration
// ---------------------------------------------------------------------------

pub fn configure_routes() -> Router<Arc<AlarmAppState>> {
    Router::new()
        .route("/projects/{project_id}/alarms", get(list_project_alarms))
        .route(
            "/projects/{project_id}/alarms/summary",
            get(get_project_alarms_summary),
        )
        .route(
            "/projects/{project_id}/alarms/{alarm_id}/acknowledge",
            post(acknowledge_alarm),
        )
        .route(
            "/projects/{project_id}/alarms/{alarm_id}/resolve",
            post(resolve_alarm),
        )
}

// ---------------------------------------------------------------------------
// OpenAPI doc
// ---------------------------------------------------------------------------

#[derive(OpenApi)]
#[openapi(
    paths(
        list_project_alarms,
        get_project_alarms_summary,
        acknowledge_alarm,
        resolve_alarm,
    ),
    components(schemas(
        AlarmResponse,
        AlarmListResponse,
        AlarmSummaryResponse,
    )),
    info(
        title = "Alarms API",
        description = "Read/ack/resolve API over the unified alarms table (ADR-025 Phase 1).",
        version = "1.0.0"
    ),
    tags(
        (name = "Alarms", description = "Unified alarm history — list, summarise, acknowledge, resolve")
    )
)]
pub struct AlarmsApiDoc;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alarm_service::{AlarmError, AlarmSummary, AlarmType};
    use async_trait::async_trait;
    use chrono::Utc;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
    use std::sync::Arc;
    use temps_core::jobs::QueueError;
    use temps_core::notifications::{EmailMessage, NotificationData, NotificationError};
    use temps_entities::alarms;

    // ── Minimal stubs ──────────────────────────────────────────────────

    struct NoopNotificationService;

    #[async_trait]
    impl temps_core::notifications::NotificationService for NoopNotificationService {
        async fn send_notification(&self, _: NotificationData) -> Result<(), NotificationError> {
            Ok(())
        }
        async fn send_email(&self, _: EmailMessage) -> Result<(), NotificationError> {
            Ok(())
        }
        async fn is_configured(&self) -> Result<bool, NotificationError> {
            Ok(false)
        }
    }

    struct NoopJobQueue;

    #[async_trait]
    impl temps_core::JobQueue for NoopJobQueue {
        async fn send(&self, _: temps_core::Job) -> Result<(), QueueError> {
            Ok(())
        }
        fn subscribe(&self) -> Box<dyn temps_core::JobReceiver> {
            unimplemented!()
        }
    }

    fn sample_alarm(id: i32) -> alarms::Model {
        alarms::Model {
            id,
            project_id: 1,
            environment_id: Some(1),
            deployment_id: Some(10),
            container_id: None,
            service_id: None,
            alarm_type: "container_restart".to_string(),
            severity: "warning".to_string(),
            status: "firing".to_string(),
            title: format!("Test alarm {}", id),
            message: Some("test message".to_string()),
            metadata: None,
            fired_at: Utc::now(),
            acknowledged_at: None,
            acknowledged_by: None,
            resolved_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn make_alarm_service(db: sea_orm::DatabaseConnection) -> Arc<AlarmService> {
        Arc::new(AlarmService::new(
            Arc::new(db),
            Arc::new(NoopNotificationService),
            Arc::new(NoopJobQueue),
        ))
    }

    // ── AlarmResponse::from tests ──────────────────────────────────────

    #[test]
    fn test_alarm_response_from_model() {
        let model = sample_alarm(42);
        let resp = AlarmResponse::from(model.clone());

        assert_eq!(resp.id, 42);
        assert_eq!(resp.project_id, 1);
        assert_eq!(resp.alarm_type, "container_restart");
        assert_eq!(resp.severity, "warning");
        assert_eq!(resp.status, "firing");
        assert_eq!(resp.title, "Test alarm 42");
        assert!(resp.acknowledged_at.is_none());
        assert!(resp.resolved_at.is_none());
        // fired_at is an ISO-8601 string ending with timezone offset
        assert!(
            resp.fired_at.contains('T'),
            "fired_at should be ISO-8601: {}",
            resp.fired_at
        );
    }

    #[test]
    fn test_alarm_response_optional_fields() {
        let mut model = sample_alarm(1);
        model.acknowledged_at = Some(Utc::now());
        model.acknowledged_by = Some(99);
        model.resolved_at = Some(Utc::now());

        let resp = AlarmResponse::from(model);
        assert!(resp.acknowledged_at.is_some());
        assert_eq!(resp.acknowledged_by, Some(99));
        assert!(resp.resolved_at.is_some());
    }

    // ── AlarmSummaryResponse::from tests ──────────────────────────────

    #[test]
    fn test_alarm_summary_response_from() {
        let mut summary = AlarmSummary {
            firing: 3,
            acknowledged: 1,
            total_active: 4,
            critical: 2,
            warning: 2,
            ..Default::default()
        };
        summary.by_type.insert("outage".to_string(), 2);

        let resp = AlarmSummaryResponse::from(summary);
        assert_eq!(resp.firing, 3);
        assert_eq!(resp.acknowledged, 1);
        assert_eq!(resp.total_active, 4);
        assert_eq!(resp.by_type.get("outage"), Some(&2));
    }

    // ── From<AlarmError> for Problem tests ────────────────────────────

    #[test]
    fn test_alarm_not_found_maps_to_404() {
        let err = AlarmError::NotFound {
            alarm_id: 5,
            project_id: 1,
        };
        let problem = Problem::from(err);
        assert_eq!(problem.status_code, StatusCode::NOT_FOUND);
    }

    #[test]
    fn test_alarm_database_error_maps_to_500() {
        let err = AlarmError::Database {
            operation: "test".to_string(),
            reason: "connection refused".to_string(),
        };
        let problem = Problem::from(err);
        assert_eq!(problem.status_code, StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn test_alarm_notification_error_maps_to_500() {
        let err = AlarmError::Notification {
            alarm_id: 1,
            reason: "smtp error".to_string(),
        };
        let problem = Problem::from(err);
        assert_eq!(problem.status_code, StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn test_alarm_queue_error_maps_to_500() {
        let err = AlarmError::Queue {
            alarm_id: 1,
            reason: "channel closed".to_string(),
        };
        let problem = Problem::from(err);
        assert_eq!(problem.status_code, StatusCode::INTERNAL_SERVER_ERROR);
    }

    // ── list_alarms service-layer tests (via MockDatabase) ────────────

    #[tokio::test]
    async fn test_list_alarms_success() {
        let alarms = vec![sample_alarm(2), sample_alarm(1)];

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([[maplit::btreemap! {
                "num_items" => sea_orm::Value::BigInt(Some(2)),
            }]])
            .append_query_results(vec![alarms])
            .into_connection();

        let service = make_alarm_service(db);
        let (items, total) = service
            .list_alarms(1, AlarmFilters::default(), 1, 20)
            .await
            .unwrap();

        assert_eq!(total, 2);
        assert_eq!(items.len(), 2);
    }

    #[tokio::test]
    async fn test_list_alarms_empty() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([[maplit::btreemap! {
                "num_items" => sea_orm::Value::BigInt(Some(0)),
            }]])
            .append_query_results(vec![Vec::<alarms::Model>::new()])
            .into_connection();

        let service = make_alarm_service(db);
        let (items, total) = service
            .list_alarms(1, AlarmFilters::default(), 1, 20)
            .await
            .unwrap();

        assert_eq!(total, 0);
        assert!(items.is_empty());
    }

    #[tokio::test]
    async fn test_list_alarms_with_type_filter() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([[maplit::btreemap! {
                "num_items" => sea_orm::Value::BigInt(Some(1)),
            }]])
            .append_query_results(vec![vec![sample_alarm(1)]])
            .into_connection();

        let service = make_alarm_service(db);
        let filters = AlarmFilters {
            alarm_type: Some(AlarmType::ContainerRestart),
            ..Default::default()
        };
        let (items, total) = service.list_alarms(1, filters, 1, 20).await.unwrap();

        assert_eq!(total, 1);
        assert_eq!(items[0].alarm_type, "container_restart");
    }

    // ── acknowledge_alarm not-found test ──────────────────────────────

    #[tokio::test]
    async fn test_acknowledge_alarm_not_found() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<alarms::Model>::new()])
            .into_connection();

        let service = make_alarm_service(db);
        let result = service.acknowledge_alarm(999, 1, 42).await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(
            err,
            AlarmError::NotFound {
                alarm_id: 999,
                project_id: 1
            }
        ));

        // Error maps to 404
        let problem = Problem::from(err);
        assert_eq!(problem.status_code, StatusCode::NOT_FOUND);
    }

    // ── resolve_alarm not-found test ──────────────────────────────────

    #[tokio::test]
    async fn test_resolve_alarm_not_found() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<alarms::Model>::new()])
            .into_connection();

        let service = make_alarm_service(db);
        let result = service.resolve_alarm(888, 1).await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(
            err,
            AlarmError::NotFound {
                alarm_id: 888,
                project_id: 1
            }
        ));

        let problem = Problem::from(err);
        assert_eq!(problem.status_code, StatusCode::NOT_FOUND);
    }

    // ── resolve_alarm success test ────────────────────────────────────

    #[tokio::test]
    async fn test_resolve_alarm_success() {
        let alarm = sample_alarm(1);

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // find_by_id query
            .append_query_results(vec![vec![alarm.clone()]])
            // update query (returns updated model)
            .append_query_results(vec![vec![alarm]])
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 1,
                rows_affected: 1,
            }])
            .into_connection();

        let service = make_alarm_service(db);
        let result = service.resolve_alarm(1, 1).await;
        assert!(result.is_ok());
    }

    // ── acknowledge_alarm success test ────────────────────────────────

    #[tokio::test]
    async fn test_acknowledge_alarm_success() {
        let alarm = sample_alarm(1);

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![alarm.clone()]])
            .append_query_results(vec![vec![alarm]])
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 1,
                rows_affected: 1,
            }])
            .into_connection();

        let service = make_alarm_service(db);
        let result = service.acknowledge_alarm(1, 1, 42).await;
        assert!(result.is_ok());
    }

    // ── AlarmFilters default ──────────────────────────────────────────

    #[test]
    fn test_alarm_filters_default_all_none() {
        let f = AlarmFilters::default();
        assert!(f.environment_id.is_none());
        assert!(f.deployment_id.is_none());
        assert!(f.alarm_type.is_none());
        assert!(f.status.is_none());
        assert!(f.severity.is_none());
    }

    // ── OpenAPI schema sanity ─────────────────────────────────────────

    #[test]
    fn test_alarms_api_doc_generates() {
        use utoipa::OpenApi as _;
        let doc = AlarmsApiDoc::openapi();
        assert!(doc
            .paths
            .paths
            .contains_key("/projects/{project_id}/alarms"));
        assert!(doc
            .paths
            .paths
            .contains_key("/projects/{project_id}/alarms/summary"));
        assert!(doc
            .paths
            .paths
            .contains_key("/projects/{project_id}/alarms/{alarm_id}/acknowledge"));
        assert!(doc
            .paths
            .paths
            .contains_key("/projects/{project_id}/alarms/{alarm_id}/resolve"));
    }
}
