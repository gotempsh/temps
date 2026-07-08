use super::types::AppState;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
    routing::get,
    Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_auth::{permission_guard, project_access_guard, project_scope_guard, RequireAuth};
use temps_core::problemdetails::Problem;
use temps_entities::error_alert_rules;
use utoipa::{OpenApi, ToSchema};

#[derive(OpenApi)]
#[openapi(
    paths(
        list_alert_rules,
        get_alert_rule,
        create_alert_rule,
        update_alert_rule,
        delete_alert_rule,
    ),
    components(schemas(
        AlertRuleResponse,
        CreateAlertRuleRequest,
        UpdateAlertRuleRequest,
    )),
    tags(
        (name = "error-alert-rules", description = "Error tracking alert rule management")
    )
)]
pub struct AlertRulesApiDoc;

pub fn configure_alert_rules_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/projects/{project_id}/error-alert-rules",
            get(list_alert_rules).post(create_alert_rule),
        )
        .route(
            "/projects/{project_id}/error-alert-rules/{rule_id}",
            get(get_alert_rule)
                .put(update_alert_rule)
                .delete(delete_alert_rule),
        )
}

// ===== Request/Response Types =====

#[derive(Debug, Serialize, ToSchema)]
pub struct AlertRuleResponse {
    pub id: i32,
    pub project_id: i32,
    pub name: String,
    pub trigger_type: String,
    pub trigger_config: serde_json::Value,
    pub environment_filter: Option<i32>,
    pub error_level_filter: Option<String>,
    pub notification_priority: String,
    pub cooldown_minutes: i32,
    pub enabled: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateAlertRuleRequest {
    pub name: String,
    /// Trigger type: new_issue, regression, frequency, new_user, user_count, status_change
    pub trigger_type: String,
    /// Trigger-specific configuration (e.g., {"count": 100, "window_minutes": 60} for frequency)
    #[serde(default = "default_trigger_config")]
    pub trigger_config: serde_json::Value,
    /// Optional environment ID to filter alerts
    pub environment_filter: Option<i32>,
    /// Optional error type/level filter
    pub error_level_filter: Option<String>,
    /// Notification priority: Low, Normal, High, Critical
    #[serde(default = "default_priority")]
    pub notification_priority: String,
    /// Minimum minutes between notifications for same rule+group
    #[serde(default = "default_cooldown")]
    pub cooldown_minutes: i32,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateAlertRuleRequest {
    pub name: Option<String>,
    pub trigger_type: Option<String>,
    pub trigger_config: Option<serde_json::Value>,
    pub environment_filter: Option<Option<i32>>,
    pub error_level_filter: Option<Option<String>>,
    pub notification_priority: Option<String>,
    pub cooldown_minutes: Option<i32>,
    pub enabled: Option<bool>,
}

fn default_trigger_config() -> serde_json::Value {
    serde_json::json!({})
}

fn default_priority() -> String {
    "High".to_string()
}

fn default_cooldown() -> i32 {
    30
}

fn default_enabled() -> bool {
    true
}

impl From<error_alert_rules::Model> for AlertRuleResponse {
    fn from(rule: error_alert_rules::Model) -> Self {
        Self {
            id: rule.id,
            project_id: rule.project_id,
            name: rule.name,
            trigger_type: rule.trigger_type,
            trigger_config: rule.trigger_config,
            environment_filter: rule.environment_filter,
            error_level_filter: rule.error_level_filter,
            notification_priority: rule.notification_priority,
            cooldown_minutes: rule.cooldown_minutes,
            enabled: rule.enabled,
            created_at: rule.created_at.to_rfc3339(),
            updated_at: rule.updated_at.to_rfc3339(),
        }
    }
}

// ===== Handlers =====

/// List all alert rules for a project
#[utoipa::path(
    get,
    path = "/projects/{project_id}/error-alert-rules",
    responses(
        (status = 200, description = "List of alert rules", body = Vec<AlertRuleResponse>),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID")
    ),
    tag = "error-alert-rules"
)]
pub async fn list_alert_rules(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path(project_id): Path<i32>,
) -> Result<Json<Vec<AlertRuleResponse>>, Problem> {
    permission_guard!(auth, ErrorTrackingRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let rules = state.alert_service.list_rules(project_id).await?;
    Ok(Json(
        rules.into_iter().map(AlertRuleResponse::from).collect(),
    ))
}

/// Get a specific alert rule
#[utoipa::path(
    get,
    path = "/projects/{project_id}/error-alert-rules/{rule_id}",
    responses(
        (status = 200, description = "Alert rule details", body = AlertRuleResponse),
        (status = 404, description = "Alert rule not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("rule_id" = i32, Path, description = "Alert rule ID")
    ),
    tag = "error-alert-rules"
)]
pub async fn get_alert_rule(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path((project_id, rule_id)): Path<(i32, i32)>,
) -> Result<Json<AlertRuleResponse>, Problem> {
    permission_guard!(auth, ErrorTrackingRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let rule = state.alert_service.get_rule(rule_id, project_id).await?;
    Ok(Json(AlertRuleResponse::from(rule)))
}

/// Create a new alert rule
#[utoipa::path(
    post,
    path = "/projects/{project_id}/error-alert-rules",
    request_body = CreateAlertRuleRequest,
    responses(
        (status = 201, description = "Alert rule created", body = AlertRuleResponse),
        (status = 400, description = "Validation error"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID")
    ),
    tag = "error-alert-rules"
)]
pub async fn create_alert_rule(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path(project_id): Path<i32>,
    Json(request): Json<CreateAlertRuleRequest>,
) -> Result<(StatusCode, Json<AlertRuleResponse>), Problem> {
    permission_guard!(auth, ErrorTrackingCreate);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let rule = state
        .alert_service
        .create_rule(
            project_id,
            request.name,
            request.trigger_type,
            request.trigger_config,
            request.environment_filter,
            request.error_level_filter,
            request.notification_priority,
            request.cooldown_minutes,
            request.enabled,
        )
        .await?;

    Ok((StatusCode::CREATED, Json(AlertRuleResponse::from(rule))))
}

/// Update an existing alert rule
#[utoipa::path(
    put,
    path = "/projects/{project_id}/error-alert-rules/{rule_id}",
    request_body = UpdateAlertRuleRequest,
    responses(
        (status = 200, description = "Alert rule updated", body = AlertRuleResponse),
        (status = 400, description = "Validation error"),
        (status = 404, description = "Alert rule not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("rule_id" = i32, Path, description = "Alert rule ID")
    ),
    tag = "error-alert-rules"
)]
pub async fn update_alert_rule(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path((project_id, rule_id)): Path<(i32, i32)>,
    Json(request): Json<UpdateAlertRuleRequest>,
) -> Result<Json<AlertRuleResponse>, Problem> {
    permission_guard!(auth, ErrorTrackingWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let rule = state
        .alert_service
        .update_rule(
            rule_id,
            project_id,
            request.name,
            request.trigger_type,
            request.trigger_config,
            request.environment_filter,
            request.error_level_filter,
            request.notification_priority,
            request.cooldown_minutes,
            request.enabled,
        )
        .await?;

    Ok(Json(AlertRuleResponse::from(rule)))
}

/// Delete an alert rule
#[utoipa::path(
    delete,
    path = "/projects/{project_id}/error-alert-rules/{rule_id}",
    responses(
        (status = 204, description = "Alert rule deleted"),
        (status = 404, description = "Alert rule not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("rule_id" = i32, Path, description = "Alert rule ID")
    ),
    tag = "error-alert-rules"
)]
pub async fn delete_alert_rule(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path((project_id, rule_id)): Path<(i32, i32)>,
) -> Result<StatusCode, Problem> {
    permission_guard!(auth, ErrorTrackingWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    state.alert_service.delete_rule(rule_id, project_id).await?;

    Ok(StatusCode::NO_CONTENT)
}
