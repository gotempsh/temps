// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `GET`/`PUT /projects/{id}/identical-builds`: what a project's builds do
//! when an identical build (same repository, commit, build path and
//! build-time values) is already running (ADR 113).
//!
//! Reading needs `projects:read` on the project; changing it needs
//! `projects:write` on the project. Every change is audited with the old and
//! new value.

use axum::{
    extract::{Extension, Path, State},
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_auth::{project_permission_guard, project_scope_guard, RequireAuth};
use temps_core::{problemdetails::Problem, AuditLogger, AuditOperation, RequestMetadata};
use temps_entities::identical_build_mode::{
    IdenticalBuildMode, IdenticalBuildScope, MAX_WAIT_SECS, REUSE_WINDOW_SECS,
};
use utoipa::ToSchema;

use crate::services::identical_builds::{
    IdenticalBuildActivity, IdenticalBuildSetting, IdenticalBuildSettingsService,
};

/// State for the identical build routes, kept apart from the projects
/// `AppState` so they depend only on what they use.
pub struct IdenticalBuildsAppState {
    pub identical_build_settings_service: Arc<IdenticalBuildSettingsService>,
    pub audit_service: Arc<dyn AuditLogger>,
    /// Team-based project access; `None` in the plain OSS binary.
    pub project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
}

pub fn configure_identical_builds_routes() -> Router<Arc<IdenticalBuildsAppState>> {
    Router::new().route(
        "/projects/{id}/identical-builds",
        get(get_identical_builds).put(update_identical_builds),
    )
}

/// One mode, explained for the console and the CLI.
#[derive(Debug, Serialize, ToSchema)]
pub struct IdenticalBuildModeOption {
    pub id: IdenticalBuildMode,
    pub label: &'static str,
    /// What the mode does, in plain words.
    pub description: &'static str,
    /// A concrete case.
    pub example: &'static str,
}

/// One grouping scope, explained for the console and the CLI.
#[derive(Debug, Serialize, ToSchema)]
pub struct IdenticalBuildScopeOption {
    pub id: IdenticalBuildScope,
    pub label: &'static str,
    pub description: &'static str,
}

fn mode_options() -> Vec<IdenticalBuildModeOption> {
    IdenticalBuildMode::ALL
        .iter()
        .map(|mode| match mode {
            IdenticalBuildMode::Parallel => IdenticalBuildModeOption {
                id: *mode,
                label: "Build every deployment",
                description: "Every deployment builds its own image, at the same time as any other build. This is how Temps has always built.",
                example: "3 projects building the same repository and path at the same commit run 3 builds at once.",
            },
            IdenticalBuildMode::Queue => IdenticalBuildModeOption {
                id: *mode,
                label: "Queue identical builds",
                description: "Identical builds run one at a time, oldest first, so a small server is not asked to run them all at once. Each deployment still builds its own image.",
                example: "3 projects building the same repository and path at the same commit run 3 builds, one after the other.",
            },
            IdenticalBuildMode::Share => IdenticalBuildModeOption {
                id: *mode,
                label: "Build once and share the image",
                description: "The first identical build runs; the others wait for it and deploy its image instead of building. Only projects that also chose this mode, with the same team access, share images, and only when everything that decides the image matches: repository, commit, build path, Dockerfile, preset, platform and every build-time variable value.",
                example: "3 projects building the same repository and path at the same commit build once; the other 2 deploy that image.",
            },
        })
        .collect()
}

fn scope_options() -> Vec<IdenticalBuildScopeOption> {
    IdenticalBuildScope::ALL
        .iter()
        .map(|scope| match scope {
            IdenticalBuildScope::Fingerprint => IdenticalBuildScopeOption {
                id: *scope,
                label: "Same build",
                description: "Only builds with the same repository, commit, build path, Dockerfile, preset, platform and build-time values wait for each other.",
            },
            IdenticalBuildScope::Source => IdenticalBuildScopeOption {
                id: *scope,
                label: "Same repository and path",
                description: "Every build of the same repository and build path waits for the others, whatever the commit or variables, so at most one of them builds at a time. Images are still only shared between identical builds.",
            },
        })
        .collect()
}

/// What coordination did for this project in the last 30 days.
#[derive(Debug, Serialize, ToSchema)]
pub struct IdenticalBuildActivityResponse {
    /// Deployments that deployed an identical build's image instead of building.
    pub reused: u64,
    /// Coordinated builds that built their own image.
    pub built: u64,
    /// Builds of this project waiting for an identical build right now.
    pub waiting: u64,
    /// Builds of this project building for their group right now.
    pub building: u64,
}

impl From<IdenticalBuildActivity> for IdenticalBuildActivityResponse {
    fn from(activity: IdenticalBuildActivity) -> Self {
        Self {
            reused: activity.reused,
            built: activity.built,
            waiting: activity.waiting,
            building: activity.building,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct IdenticalBuildsResponse {
    /// What this project's builds do when an identical build is running.
    /// `parallel` (the default) builds every deployment. Applies from the
    /// next build.
    pub mode: IdenticalBuildMode,
    /// Which builds wait for each other.
    pub scope: IdenticalBuildScope,
    pub modes: Vec<IdenticalBuildModeOption>,
    pub scopes: Vec<IdenticalBuildScopeOption>,
    /// How long after it succeeded a shared image is offered to identical
    /// builds that start later.
    pub reuse_window_seconds: u64,
    /// The longest a build waits before it fails with a reason.
    pub max_wait_seconds: u64,
    /// The last 30 days.
    pub activity: IdenticalBuildActivityResponse,
    /// Whether the caller may change the setting.
    pub can_edit: bool,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateIdenticalBuildsRequest {
    pub mode: IdenticalBuildMode,
    /// Defaults to `fingerprint`.
    #[serde(default)]
    pub scope: IdenticalBuildScope,
}

fn response(
    setting: IdenticalBuildSetting,
    activity: IdenticalBuildActivity,
    can_edit: bool,
) -> IdenticalBuildsResponse {
    IdenticalBuildsResponse {
        mode: setting.mode,
        scope: setting.scope,
        modes: mode_options(),
        scopes: scope_options(),
        reuse_window_seconds: REUSE_WINDOW_SECS,
        max_wait_seconds: MAX_WAIT_SECS,
        activity: activity.into(),
        can_edit,
    }
}

#[utoipa::path(get, path = "/projects/{id}/identical-builds", tag = "Projects",
    params(("id" = i32, Path, description = "Project ID")),
    responses(
        (status = 200, body = IdenticalBuildsResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden"),
        (status = 404, description = "Project not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = [])))]
pub async fn get_identical_builds(
    State(state): State<Arc<IdenticalBuildsAppState>>,
    Path(id): Path<i32>,
    RequireAuth(auth): RequireAuth,
) -> Result<Json<IdenticalBuildsResponse>, Problem> {
    project_permission_guard!(auth, ProjectsRead, id, state.project_access_checker);
    project_scope_guard!(auth, id);
    let service = &state.identical_build_settings_service;
    let setting = service.setting(id).await?;
    let activity = service.activity(id).await?;
    let can_edit = auth.has_permission(&temps_auth::Permission::ProjectsWrite);
    Ok(Json(response(setting, activity, can_edit)))
}

#[utoipa::path(put, path = "/projects/{id}/identical-builds", tag = "Projects",
    params(("id" = i32, Path, description = "Project ID")),
    request_body = UpdateIdenticalBuildsRequest,
    responses(
        (status = 200, body = IdenticalBuildsResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden"),
        (status = 404, description = "Project not found"),
        (status = 422, description = "An unknown mode or scope"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = [])))]
pub async fn update_identical_builds(
    State(state): State<Arc<IdenticalBuildsAppState>>,
    Path(id): Path<i32>,
    RequireAuth(auth): RequireAuth,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<UpdateIdenticalBuildsRequest>,
) -> Result<Json<IdenticalBuildsResponse>, Problem> {
    project_permission_guard!(auth, ProjectsWrite, id, state.project_access_checker);
    project_scope_guard!(auth, id);
    let service = &state.identical_build_settings_service;
    let change = service
        .update(
            id,
            IdenticalBuildSetting {
                mode: request.mode,
                scope: request.scope,
            },
        )
        .await?;
    if change.previous != change.current {
        let event = IdenticalBuildsAudit {
            project_id: id,
            actor: auth.user_id(),
            ip_address: metadata.ip_address.to_string(),
            user_agent: metadata.user_agent,
            previous_mode: change.previous.mode,
            previous_scope: change.previous.scope,
            mode: change.current.mode,
            scope: change.current.scope,
        };
        if let Err(error) = state.audit_service.create_audit_log(&event).await {
            tracing::error!(project_id = id, actor = auth.user_id(), %error, "Failed to record identical builds audit event");
        }
        tracing::info!(
            project_id = id,
            actor = auth.user_id(),
            previous_mode = %change.previous.mode,
            previous_scope = %change.previous.scope,
            mode = %change.current.mode,
            scope = %change.current.scope,
            "Project identical build setting changed; applies from the next build"
        );
    }
    let activity = service.activity(id).await?;
    Ok(Json(response(change.current, activity, true)))
}

#[derive(Serialize)]
pub(crate) struct IdenticalBuildsAudit {
    pub(crate) project_id: i32,
    pub(crate) actor: i32,
    pub(crate) ip_address: String,
    pub(crate) user_agent: String,
    pub(crate) previous_mode: IdenticalBuildMode,
    pub(crate) previous_scope: IdenticalBuildScope,
    pub(crate) mode: IdenticalBuildMode,
    pub(crate) scope: IdenticalBuildScope,
}

impl AuditOperation for IdenticalBuildsAudit {
    fn operation_type(&self) -> String {
        "project.identical_builds.updated".to_string()
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
    use tower::ServiceExt;

    const URI: &str = "/projects/7/identical-builds";

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

    fn stored(mode: &str, scope: &str) -> BTreeMap<&'static str, Value> {
        BTreeMap::from([
            ("identical_build_mode", Value::from(mode.to_string())),
            ("identical_build_scope", Value::from(scope.to_string())),
        ])
    }

    fn activity_rows() -> Vec<BTreeMap<&'static str, Value>> {
        vec![BTreeMap::from([
            ("state", Value::from("reused".to_string())),
            ("builds", Value::from(2_i64)),
        ])]
    }

    fn user() -> temps_entities::users::Model {
        temps_entities::users::Model {
            id: 3,
            name: "Test user".into(),
            email: "user@example.test".into(),
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

    fn metadata() -> RequestMetadata {
        RequestMetadata {
            ip_address: "198.51.100.7".to_string(),
            user_agent: "identical-builds-test".to_string(),
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

    fn harness(db: MockDatabase) -> Harness {
        let audit = Arc::new(RecordingAudit::default());
        let state = Arc::new(IdenticalBuildsAppState {
            identical_build_settings_service: Arc::new(IdenticalBuildSettingsService::new(
                Arc::new(db.into_connection()),
            )),
            audit_service: audit.clone(),
            project_access_checker: None,
        });
        Harness {
            router: configure_identical_builds_routes().with_state(state),
            audit,
        }
    }

    fn empty_db() -> MockDatabase {
        MockDatabase::new(DatabaseBackend::Postgres)
    }

    async fn send(
        router: Router,
        method: &str,
        body: Option<serde_json::Value>,
        auth: Option<AuthContext>,
    ) -> (StatusCode, serde_json::Value) {
        let mut request = Request::builder().method(method).uri(URI);
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
        let response = router.oneshot(request).await.expect("response");
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, json)
    }

    fn share() -> serde_json::Value {
        serde_json::json!({ "mode": "share", "scope": "fingerprint" })
    }

    #[tokio::test]
    async fn both_routes_require_authentication() {
        for (method, body) in [("GET", None), ("PUT", Some(share()))] {
            let h = harness(empty_db());
            assert_eq!(
                send(h.router, method, body, None).await.0,
                StatusCode::UNAUTHORIZED,
                "{method}"
            );
        }
    }

    #[tokio::test]
    async fn a_reader_sees_the_setting_its_options_and_recent_activity() {
        let h = harness(
            empty_db()
                .append_query_results([vec![stored("queue", "source")]])
                .append_query_results([activity_rows()]),
        );
        let reader = AuthContext::new_session(user(), Role::Reader);
        let (status, body) = send(h.router, "GET", None, Some(reader)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["mode"], "queue");
        assert_eq!(body["scope"], "source");
        assert_eq!(body["can_edit"], false);
        assert_eq!(body["activity"]["reused"], 2);
        assert_eq!(body["modes"].as_array().map(Vec::len), Some(3));
        assert_eq!(body["modes"][2]["id"], "share");
        assert!(body["modes"][2]["example"]
            .as_str()
            .unwrap_or_default()
            .contains("build once"));
        assert_eq!(body["scopes"].as_array().map(Vec::len), Some(2));
        assert_eq!(body["reuse_window_seconds"], REUSE_WINDOW_SECS);
        assert_eq!(body["max_wait_seconds"], MAX_WAIT_SECS);
    }

    #[tokio::test]
    async fn a_missing_project_is_a_404_problem() {
        let h = harness(empty_db().append_query_results([Vec::<BTreeMap<&str, Value>>::new()]));
        let admin = AuthContext::new_session(user(), Role::Admin);
        let (status, body) = send(h.router, "GET", None, Some(admin)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("Project 7"));
    }

    #[tokio::test]
    async fn only_projects_write_may_change_it() {
        let read_only_key = AuthContext::new_api_key(
            user(),
            None,
            Some(vec![Permission::ProjectsRead]),
            "read only".into(),
            1,
        );
        let other_project_token =
            AuthContext::new_deployment_token(8, None, None, 1, "ci".into(), vec![]);
        for (label, auth) in [
            (
                "reader session",
                AuthContext::new_session(user(), Role::Reader),
            ),
            ("read-only key", read_only_key),
            ("deployment token", other_project_token),
        ] {
            let h = harness(empty_db());
            let (status, _) = send(h.router, "PUT", Some(share()), Some(auth)).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{label}");
            assert!(h.audit.recorded().is_empty(), "{label}");
        }
    }

    #[tokio::test]
    async fn a_change_is_saved_and_audited_with_the_old_and_new_value() {
        let h = harness(
            empty_db()
                .append_query_results([vec![stored("parallel", "fingerprint")]])
                .append_exec_results([MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 1,
                }])
                .append_query_results([activity_rows()]),
        );
        let user_session = AuthContext::new_session(user(), Role::User);
        let (status, body) = send(h.router, "PUT", Some(share()), Some(user_session)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["mode"], "share");
        assert_eq!(body["can_edit"], true);

        let recorded = h.audit.recorded();
        assert_eq!(recorded.len(), 1);
        let (operation, event) = &recorded[0];
        assert_eq!(operation, "project.identical_builds.updated");
        assert_eq!(event["project_id"], 7);
        assert_eq!(event["actor"], 3);
        assert_eq!(event["previous_mode"], "parallel");
        assert_eq!(event["previous_scope"], "fingerprint");
        assert_eq!(event["mode"], "share");
        assert_eq!(event["scope"], "fingerprint");
        assert_eq!(event["ip_address"], "198.51.100.7");
    }

    #[tokio::test]
    async fn an_unchanged_save_is_not_audited() {
        let h = harness(
            empty_db()
                .append_query_results([vec![stored("share", "fingerprint")]])
                .append_query_results([activity_rows()]),
        );
        let admin = AuthContext::new_session(user(), Role::Admin);
        let (status, _) = send(h.router, "PUT", Some(share()), Some(admin)).await;
        assert_eq!(status, StatusCode::OK);
        assert!(h.audit.recorded().is_empty());
    }

    #[tokio::test]
    async fn unknown_modes_scopes_and_fields_are_rejected() {
        for body in [
            serde_json::json!({ "mode": "always_share" }),
            serde_json::json!({ "mode": "share", "scope": "everything" }),
            serde_json::json!({ "mode": "share", "force": true }),
            serde_json::json!({}),
        ] {
            let h = harness(empty_db());
            let admin = AuthContext::new_session(user(), Role::Admin);
            let (status, _) = send(h.router, "PUT", Some(body.clone()), Some(admin)).await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
            assert!(h.audit.recorded().is_empty());
        }
    }

    #[test]
    fn the_schemas_are_published() {
        use utoipa::OpenApi;
        let doc = serde_json::to_value(super::super::ApiDoc::openapi()).expect("openapi");
        let paths = &doc["paths"]["/projects/{id}/identical-builds"];
        assert!(paths["get"].is_object() && paths["put"].is_object());
        let schemas = &doc["components"]["schemas"];
        for name in [
            "IdenticalBuildsResponse",
            "UpdateIdenticalBuildsRequest",
            "IdenticalBuildModeOption",
            "IdenticalBuildScopeOption",
            "IdenticalBuildMode",
            "IdenticalBuildScope",
            "IdenticalBuildActivityResponse",
        ] {
            assert!(schemas[name].is_object(), "{name} missing");
        }
    }
}
