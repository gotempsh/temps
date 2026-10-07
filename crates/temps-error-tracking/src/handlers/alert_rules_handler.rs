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
use temps_auth::{permission_guard, project_access_guard, project_scope_guard, RequireAuth};
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
        (status = 404, description = "Project not found"),
        (status = 409, description = "Project already holds the maximum number of error alert rules"),
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

fn audit_context(auth: &temps_auth::AuthContext, metadata: &RequestMetadata) -> AuditContext {
    AuditContext {
        user_id: auth.user_id(),
        ip_address: Some(metadata.ip_address.clone()),
        user_agent: metadata.user_agent.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::error_alert_service::{
        insert_rule_within_project_limit, ErrorAlertService,
    };
    use crate::services::error_tracking_service::ErrorTrackingService;
    use crate::services::ErrorTrackingError;
    use axum::http::HeaderMap;
    use sea_orm::{ActiveModelTrait, EntityTrait, Set};
    use std::sync::Mutex;
    use temps_auth::{AuthContext, Role};
    use temps_database::test_utils::{is_container_runtime_unavailable, TestDatabase};
    use temps_entities::{projects, users};

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
        ) -> Result<(), anyhow::Error> {
            let payload: serde_json::Value = serde_json::from_str(&operation.serialize()?)?;
            self.entries
                .lock()
                .map_err(|_| anyhow::anyhow!("recording audit logger lock poisoned"))?
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

    /// The per-project cap against a real database: rules up to the limit are
    /// accepted, the next one is a typed `AlertRuleLimitReached` that the
    /// handler surfaces as 409. Uses a small limit so the test stays fast;
    /// `create_rule` passes `MAX_ERROR_ALERT_RULES_PER_PROJECT` to the same
    /// function.
    #[tokio::test]
    async fn alert_rule_creation_stops_at_the_per_project_limit() {
        let test_db = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(error) if is_container_runtime_unavailable(&error.to_string()) => {
                eprintln!("Skipping alert-rule limit test: {error}");
                return;
            }
            Err(error) => panic!("alert-rule limit test database setup failed: {error}"),
        };
        let db = test_db.connection_arc();
        let project_id = seed_project(db.as_ref()).await;
        let other_project_id = seed_project(db.as_ref()).await;

        let rule = |pid: i32| {
            let now = chrono::Utc::now();
            error_alert_rules::ActiveModel {
                project_id: Set(pid),
                name: Set("Rule".to_string()),
                trigger_type: Set("new_issue".to_string()),
                trigger_config: Set(serde_json::json!({})),
                environment_filter: Set(None),
                error_level_filter: Set(None),
                notification_priority: Set("High".to_string()),
                cooldown_minutes: Set(30),
                enabled: Set(true),
                created_at: Set(now),
                updated_at: Set(now),
                ..Default::default()
            }
        };

        const LIMIT: u64 = 3;
        for _ in 0..LIMIT {
            insert_rule_within_project_limit(db.as_ref(), project_id, rule(project_id), LIMIT)
                .await
                .expect("rules under the limit are accepted");
        }

        let err =
            insert_rule_within_project_limit(db.as_ref(), project_id, rule(project_id), LIMIT)
                .await
                .expect_err("the rule past the limit must be rejected");
        assert!(matches!(
            err,
            ErrorTrackingError::AlertRuleLimitReached {
                project_id: pid,
                existing: 3,
                limit: 3,
            } if pid == project_id
        ));
        assert_eq!(Problem::from(err).status_code, StatusCode::CONFLICT);

        // The cap is per project: another project is unaffected.
        insert_rule_within_project_limit(
            db.as_ref(),
            other_project_id,
            rule(other_project_id),
            LIMIT,
        )
        .await
        .expect("a different project has its own budget");

        // An unknown project is a typed not-found, not an FK violation.
        let missing = insert_rule_within_project_limit(
            db.as_ref(),
            other_project_id + 10_000,
            rule(other_project_id + 10_000),
            LIMIT,
        )
        .await
        .expect_err("unknown project must be rejected");
        assert!(matches!(missing, ErrorTrackingError::ProjectNotFound));
    }
}
