// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::audit::{
    AuditContext, ErrorAlertRuleCreatedAudit, ErrorAlertRuleDeletedAudit,
    ErrorAlertRuleUpdatedAudit,
};
use super::types::AppState;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
    routing::get,
    Extension, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_authz::{permission_guard, project_access_guard, project_scope_guard, RequireAuth};
use temps_core::{problemdetails::Problem, RequestMetadata};
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
    /// Unique within the project (case-insensitive), trimmed, 1-255 characters.
    #[schema(min_length = 1, max_length = 255)]
    pub name: String,
    /// Trigger type: new_issue, regression, frequency, new_user, user_count, status_change
    pub trigger_type: String,
    /// Trigger-specific configuration. `frequency`: `{"count": 100,
    /// "window_minutes": 60}`; `user_count`: `{"count": 10}` (the user
    /// threshold); `status_change`: `{"statuses": ["resolved"]}`; the other
    /// types take `{}`. Unknown keys are refused.
    #[serde(default = "default_trigger_config")]
    pub trigger_config: serde_json::Value,
    /// Only alert on errors in this environment of the project. It must be one
    /// of the project's environments; omit or `null` for every environment.
    pub environment_filter: Option<i32>,
    /// Only alert on errors of this exception type (for example `TypeError`),
    /// compared case-insensitively with the error group's type. Despite the
    /// name it does not match a severity level. Omit, `null` or `""` for
    /// every type.
    #[schema(max_length = 255)]
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

/// A partial update: an omitted field keeps its value.
#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateAlertRuleRequest {
    /// Unique within the project (case-insensitive), trimmed, 1-255 characters.
    #[schema(min_length = 1, max_length = 255)]
    pub name: Option<String>,
    pub trigger_type: Option<String>,
    /// See `CreateAlertRuleRequest.trigger_config`; unknown keys are refused.
    pub trigger_config: Option<serde_json::Value>,
    /// One of the project's environments, or `null` to alert on every
    /// environment again. Omit to keep the current filter.
    #[serde(default, deserialize_with = "temps_core::nullable::deserialize")]
    #[schema(value_type = Option<i32>, nullable)]
    pub environment_filter: Option<Option<i32>>,
    /// Exception type to match (for example `TypeError`), or `null` / `""` to
    /// alert on every type again. Omit to keep the current filter.
    #[serde(default, deserialize_with = "temps_core::nullable::deserialize")]
    #[schema(value_type = Option<String>, nullable, max_length = 255)]
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
///
/// Every new project starts with two rules, "New issue detected"
/// (`new_issue`) and "Regression detected" (`regression`), both High
/// priority with a 30-minute cooldown; they can be changed or deleted like
/// any other rule.
#[utoipa::path(
    get,
    path = "/projects/{project_id}/error-alert-rules",
    responses(
        (status = 200, description = "List of alert rules, including the two default rules every project starts with", body = Vec<AlertRuleResponse>),
        (status = 404, description = "Project not found"),
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
        (status = 400, description = "Invalid field (the problem names it), including an environment_filter that is not one of the project's environments"),
        (status = 404, description = "Project not found"),
        (status = 409, description = "Another rule of the project has this name, or the project already holds the maximum number of error alert rules"),
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
    Extension(metadata): Extension<RequestMetadata>,
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

    let audit = ErrorAlertRuleCreatedAudit {
        context: audit_context(&auth, &metadata),
        project_id,
        rule_id: rule.id,
        name: rule.name.clone(),
        trigger_type: rule.trigger_type.clone(),
        enabled: rule.enabled,
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        tracing::error!(
            project_id,
            rule_id = rule.id,
            "Failed to create error alert rule creation audit log: {}",
            e
        );
    }

    Ok((StatusCode::CREATED, Json(AlertRuleResponse::from(rule))))
}

/// Update an existing alert rule
#[utoipa::path(
    put,
    path = "/projects/{project_id}/error-alert-rules/{rule_id}",
    request_body = UpdateAlertRuleRequest,
    responses(
        (status = 200, description = "Alert rule updated", body = AlertRuleResponse),
        (status = 400, description = "Invalid field (the problem names it), including an environment_filter that is not one of the project's environments"),
        (status = 404, description = "Project not found, or the rule is not one of the project's"),
        (status = 409, description = "Another rule of the project has this name"),
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
    Extension(metadata): Extension<RequestMetadata>,
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

    let audit = ErrorAlertRuleUpdatedAudit {
        context: audit_context(&auth, &metadata),
        project_id,
        rule_id: rule.id,
        name: rule.name.clone(),
        trigger_type: rule.trigger_type.clone(),
        enabled: rule.enabled,
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        tracing::error!(
            project_id,
            rule_id,
            "Failed to create error alert rule update audit log: {}",
            e
        );
    }

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
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<StatusCode, Problem> {
    permission_guard!(auth, ErrorTrackingWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    state.alert_service.delete_rule(rule_id, project_id).await?;

    let audit = ErrorAlertRuleDeletedAudit {
        context: audit_context(&auth, &metadata),
        project_id,
        rule_id,
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        tracing::error!(
            project_id,
            rule_id,
            "Failed to create error alert rule deletion audit log: {}",
            e
        );
    }

    Ok(StatusCode::NO_CONTENT)
}

fn audit_context(auth: &temps_authz::AuthContext, metadata: &RequestMetadata) -> AuditContext {
    AuditContext {
        user_id: auth.user_id(),
        ip_address: Some(metadata.ip_address.clone()),
        user_agent: metadata.user_agent.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::error_alert_service::ErrorAlertService;
    use crate::services::error_tracking_service::ErrorTrackingService;
    use axum::http::HeaderMap;
    use sea_orm::{ActiveModelTrait, EntityTrait, Set};
    use std::sync::Mutex;
    use temps_authz::{AuthContext, Role};
    use temps_entities::{projects, users};
    use temps_test_utils::{is_container_runtime_unavailable, TestDatabase};

    /// Audit logger that records every operation it receives.
    #[derive(Default)]
    struct RecordingAuditLogger {
        entries: Mutex<Vec<(String, Option<i32>, serde_json::Value)>>,
    }

    #[async_trait::async_trait]
    impl temps_core::AuditLogger for RecordingAuditLogger {
        async fn create_audit_log(
            &self,
            operation: &dyn temps_core::AuditOperation,
        ) -> temps_core::AuditResult<()> {
            let payload: serde_json::Value = serde_json::from_str(&operation.serialize()?)
                .expect("audit operation serializes to valid JSON");
            self.entries
                .lock()
                .map_err(|_| {
                    temps_core::AuditError::write(
                        operation.operation_type(),
                        "recording audit logger lock poisoned",
                    )
                })?
                .push((operation.operation_type(), operation.user_id(), payload));
            Ok(())
        }
    }

    fn test_user(id: i32) -> users::Model {
        let now = chrono::Utc::now();
        users::Model {
            id,
            name: "Test User".to_string(),
            email: "test@example.com".to_string(),
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
            user_agent: "audit-test".to_string(),
            headers: HeaderMap::new(),
            visitor_id_cookie: None,
            session_id_cookie: None,
            base_url: "https://temps.test".to_string(),
            scheme: "https".to_string(),
            host: "temps.test".to_string(),
            is_secure: true,
        }
    }

    async fn seed_project(db: &sea_orm::DatabaseConnection) -> i32 {
        let now = chrono::Utc::now();
        projects::ActiveModel {
            name: Set("Alert Audit Project".to_string()),
            repo_name: Set("repo".to_string()),
            repo_owner: Set("owner".to_string()),
            directory: Set("/".to_string()),
            main_branch: Set("main".to_string()),
            slug: Set(format!("alert-audit-{}", uuid::Uuid::new_v4())),
            preset: Set(temps_entities::preset::Preset::NextJs),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("insert project")
        .id
    }

    fn create_request(name: &str) -> CreateAlertRuleRequest {
        CreateAlertRuleRequest {
            name: name.to_string(),
            trigger_type: "new_issue".to_string(),
            trigger_config: default_trigger_config(),
            environment_filter: None,
            error_level_filter: None,
            notification_priority: default_priority(),
            cooldown_minutes: default_cooldown(),
            enabled: default_enabled(),
        }
    }

    /// #2110: null clears a filter on update; absent keeps it.
    #[test]
    fn an_update_tells_null_from_absent_for_both_filters() {
        let cleared: UpdateAlertRuleRequest = serde_json::from_value(serde_json::json!({
            "environment_filter": null,
            "error_level_filter": null,
        }))
        .unwrap();
        assert_eq!(cleared.environment_filter, Some(None));
        assert_eq!(cleared.error_level_filter, Some(None));
        let kept: UpdateAlertRuleRequest =
            serde_json::from_value(serde_json::json!({"enabled": false})).unwrap();
        assert_eq!(kept.environment_filter, None);
        assert_eq!(kept.error_level_filter, None);
    }

    async fn seed_environment(db: &sea_orm::DatabaseConnection, project_id: i32) -> i32 {
        use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
        let host = format!("alert-env-{}", uuid::Uuid::new_v4());
        let row = db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                r#"INSERT INTO environments (name, slug, subdomain, host, upstreams, project_id,
                     is_preview, created_at, updated_at)
                   VALUES ('production', 'production', $1, $1, '[]'::json, $2, FALSE, NOW(), NOW())
                   RETURNING id"#,
                [host.into(), project_id.into()],
            ))
            .await
            .expect("insert environment")
            .expect("a returned row");
        row.try_get::<i32>("", "id").expect("environment id")
    }

    /// #2110: filters are validated and clearable, another project's rule is
    /// a 404, and names are required, bounded and unique.
    #[tokio::test]
    async fn alert_rule_filters_names_and_scope_are_checked() {
        let test_db = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(error) if is_container_runtime_unavailable(&error.to_string()) => {
                eprintln!("Skipping alert-rule validation test: {error}");
                return;
            }
            Err(error) => panic!("alert-rule validation test database setup failed: {error}"),
        };
        let db = test_db.connection_arc();
        let project_id = seed_project(db.as_ref()).await;
        let other_project = seed_project(db.as_ref()).await;
        let environment = seed_environment(db.as_ref(), project_id).await;
        let other_environment = seed_environment(db.as_ref(), other_project).await;
        let state = Arc::new(AppState {
            error_tracking_service: Arc::new(ErrorTrackingService::new(db.clone())),
            alert_service: Arc::new(ErrorAlertService::new(db.clone())),
            audit_service: Arc::new(RecordingAuditLogger::default()),
            project_access_checker: None,
        });
        let auth = AuthContext::new_session(test_user(5), Role::Admin);
        let create = |project: i32, request: CreateAlertRuleRequest| {
            create_alert_rule(
                State(state.clone()),
                RequireAuth(auth.clone()),
                Path(project),
                Extension(metadata()),
                Json(request),
            )
        };
        let update = |rule_id: i32, body: serde_json::Value| {
            update_alert_rule(
                State(state.clone()),
                RequireAuth(auth.clone()),
                Path((project_id, rule_id)),
                Extension(metadata()),
                Json(serde_json::from_value(body).expect("update body")),
            )
        };

        // An environment that does not exist, or is another project's.
        for missing in [987_654, other_environment] {
            let mut request = create_request("any-env");
            request.environment_filter = Some(missing);
            let problem = create(project_id, request).await.expect_err("refused");
            assert_eq!(problem.status_code, StatusCode::BAD_REQUEST);
            assert_eq!(
                problem.body["type"],
                "https://temps.sh/probs/invalid-alert-rule"
            );
            assert_eq!(problem.body["environment_id"], missing);
            assert!(
                problem.body["detail"]
                    .as_str()
                    .is_some_and(|d| d.contains(&format!("Environment {missing} not found"))),
                "{:?}",
                problem.body
            );
        }

        let mut request = create_request("  filtered  ");
        request.environment_filter = Some(environment);
        request.error_level_filter = Some("TypeError".to_string());
        let (_, Json(rule)) = create(project_id, request).await.expect("created");
        assert_eq!(rule.name, "filtered");
        assert_eq!(rule.environment_filter, Some(environment));
        assert_eq!(rule.error_level_filter.as_deref(), Some("TypeError"));

        // Absent keeps both filters; null clears them.
        let Json(kept) = update(rule.id, serde_json::json!({"enabled": false}))
            .await
            .expect("update");
        assert_eq!(kept.environment_filter, Some(environment));
        assert_eq!(kept.error_level_filter.as_deref(), Some("TypeError"));
        let Json(cleared) = update(
            rule.id,
            serde_json::json!({"environment_filter": null, "error_level_filter": null}),
        )
        .await
        .expect("update");
        assert_eq!(cleared.environment_filter, None);
        assert_eq!(cleared.error_level_filter, None);
        // An empty filter is no filter, not one that matches nothing.
        let Json(blank) = update(rule.id, serde_json::json!({"error_level_filter": "  "}))
            .await
            .expect("update");
        assert_eq!(blank.error_level_filter, None);
        let problem = update(
            rule.id,
            serde_json::json!({"environment_filter": other_environment}),
        )
        .await
        .expect_err("another project's environment");
        assert_eq!(problem.status_code, StatusCode::BAD_REQUEST);

        // Another project's rule is not found there.
        let problem = get_alert_rule(
            State(state.clone()),
            RequireAuth(auth.clone()),
            Path((other_project, rule.id)),
        )
        .await
        .expect_err("not this project's rule");
        assert_eq!(problem.status_code, StatusCode::NOT_FOUND);
        assert_eq!(
            problem.body["type"],
            "https://temps.sh/probs/alert-rule-not-found"
        );
        assert_eq!(problem.body["rule_id"], rule.id);

        // Names: required, bounded, unique per project (case-insensitive).
        for (name, field_reason) in [
            ("   ".to_string(), "must not be empty"),
            ("x".repeat(256), "at most 255 characters"),
        ] {
            let problem = create(project_id, create_request(&name))
                .await
                .expect_err("bad name");
            assert_eq!(problem.status_code, StatusCode::BAD_REQUEST);
            assert_eq!(problem.body["field"], "name");
            assert!(
                problem.body["detail"]
                    .as_str()
                    .is_some_and(|d| d.contains(field_reason)),
                "{:?}",
                problem.body
            );
        }
        let problem = create(project_id, create_request("FILTERED"))
            .await
            .expect_err("duplicate name");
        assert_eq!(problem.status_code, StatusCode::CONFLICT);
        assert_eq!(problem.body["rule_id"], rule.id);
        let (status, _) = create(other_project, create_request("filtered"))
            .await
            .expect("names are per project");
        assert_eq!(status, StatusCode::CREATED);

        // Unknown trigger config keys, and the user_count threshold message.
        let mut request = create_request("typo");
        request.trigger_type = "frequency".to_string();
        request.trigger_config = serde_json::json!({"count": 5, "window_minutes": 10, "windw": 1});
        let problem = create(project_id, request).await.expect_err("unknown key");
        assert!(
            problem.body["detail"]
                .as_str()
                .is_some_and(|d| d.contains("unknown key(s) windw")),
            "{:?}",
            problem.body
        );
        let mut request = create_request("users");
        request.trigger_type = "user_count".to_string();
        request.trigger_config = serde_json::json!({"count": 0});
        let problem = create(project_id, request)
            .await
            .expect_err("zero threshold");
        assert!(
            problem.body["detail"]
                .as_str()
                .is_some_and(|d| d.contains("threshold")),
            "{:?}",
            problem.body
        );
    }

    #[tokio::test]
    async fn alert_rule_create_update_delete_emit_audit_events_only_on_success() {
        let test_db = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(error) if is_container_runtime_unavailable(&error.to_string()) => {
                eprintln!("Skipping alert-rule audit test: {error}");
                return;
            }
            Err(error) => panic!("alert-rule audit test database setup failed: {error}"),
        };
        let db = test_db.connection_arc();
        let project_id = seed_project(db.as_ref()).await;

        let recorder = Arc::new(RecordingAuditLogger::default());
        let state = Arc::new(AppState {
            error_tracking_service: Arc::new(ErrorTrackingService::new(db.clone())),
            alert_service: Arc::new(ErrorAlertService::new(db.clone())),
            audit_service: recorder.clone(),
            project_access_checker: None,
        });
        let auth = AuthContext::new_session(test_user(5), Role::Admin);

        // Create (runs the real per-project lock + count against Postgres).
        let (status, Json(rule)) = create_alert_rule(
            State(state.clone()),
            RequireAuth(auth.clone()),
            Path(project_id),
            Extension(metadata()),
            Json(create_request("New issue")),
        )
        .await
        .expect("create succeeds");
        assert_eq!(status, StatusCode::CREATED);

        // A validation failure writes nothing and audits nothing.
        let mut invalid = create_request("Bad");
        invalid.trigger_type = "not-a-trigger".to_string();
        let rejected = create_alert_rule(
            State(state.clone()),
            RequireAuth(auth.clone()),
            Path(project_id),
            Extension(metadata()),
            Json(invalid),
        )
        .await;
        assert!(rejected.is_err(), "invalid trigger type must be rejected");

        // Update.
        let Json(updated) = update_alert_rule(
            State(state.clone()),
            RequireAuth(auth.clone()),
            Path((project_id, rule.id)),
            Extension(metadata()),
            Json(UpdateAlertRuleRequest {
                name: None,
                trigger_type: None,
                trigger_config: None,
                environment_filter: None,
                error_level_filter: None,
                notification_priority: None,
                cooldown_minutes: None,
                enabled: Some(false),
            }),
        )
        .await
        .expect("update succeeds");
        assert!(!updated.enabled);

        // Delete.
        let status = delete_alert_rule(
            State(state.clone()),
            RequireAuth(auth.clone()),
            Path((project_id, rule.id)),
            Extension(metadata()),
        )
        .await
        .expect("delete succeeds");
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert!(
            temps_entities::error_alert_rules::Entity::find_by_id(rule.id)
                .one(db.as_ref())
                .await
                .expect("query")
                .is_none()
        );

        // Deleting it again fails and is not audited.
        let missing = delete_alert_rule(
            State(state),
            RequireAuth(auth),
            Path((project_id, rule.id)),
            Extension(metadata()),
        )
        .await;
        assert!(missing.is_err(), "second delete must fail");

        let entries = recorder.entries.lock().expect("lock");
        let ops: Vec<&str> = entries.iter().map(|(op, _, _)| op.as_str()).collect();
        assert_eq!(
            ops,
            [
                "ERROR_ALERT_RULE_CREATED",
                "ERROR_ALERT_RULE_UPDATED",
                "ERROR_ALERT_RULE_DELETED"
            ]
        );
        for (op, user_id, payload) in entries.iter() {
            assert_eq!(*user_id, Some(5), "{op} records the acting user");
            assert_eq!(payload["project_id"], project_id);
            assert_eq!(payload["rule_id"], rule.id);
            assert_eq!(payload["context"]["ip_address"], "203.0.113.9");
        }
        assert_eq!(entries[0].2["name"], "New issue");
        assert_eq!(entries[0].2["trigger_type"], "new_issue");
        assert_eq!(entries[1].2["enabled"], false);
    }
}
