// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! How a plugin is separated from Temps: read it, and move a plugin into or
//! out of a container.
use super::*;
use crate::sandbox::{IsolationMode, PluginIsolation};
use crate::service::PluginIsolationStatus;

/// Where the console explains plugin isolation.
const SETUP_PATH: &str = "/settings/plugins";

/// How one installed plugin is separated from Temps.
#[derive(Debug, Serialize, ToSchema)]
pub struct PluginIsolationResponse {
    pub name: String,
    /// What an administrator chose. Absent for a plugin that has not started
    /// yet, which starts in a container.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requested: Option<PluginIsolation>,
    /// What separates the running plugin from Temps. Absent when it is not
    /// running.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective: Option<IsolationMode>,
    /// Whether the running plugin is kept away from Temps' own files,
    /// processes and database.
    pub sandboxed: bool,
    /// Whether an administrator can change the isolation of this plugin
    /// here. False when every plugin runs through the plugin launcher.
    pub changeable: bool,
    /// Why the plugin did not start, when its last start failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_failure: Option<String>,
}

/// Whether this server can run plugins in containers, and how each installed
/// plugin is isolated.
#[derive(Debug, Serialize, ToSchema)]
pub struct PluginSandboxResponse {
    /// Whether a newly installed plugin is confined on this server.
    pub configured: bool,
    /// Why plugins cannot run in containers here, when they cannot.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Where the console explains what is missing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub setup_path: Option<String>,
    /// What confines a newly installed plugin on this server.
    pub mode: IsolationMode,
    pub plugins: Vec<PluginIsolationResponse>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdatePluginIsolation {
    /// `container` confines the plugin. `process` runs it as the Temps
    /// account with nothing between it and the control plane.
    pub isolation: PluginIsolation,
}

#[derive(Serialize)]
struct PluginIsolationAudit {
    context: temps_core::audit::AuditContext,
    #[serde(skip)]
    operation: &'static str,
    plugin: String,
    isolation: PluginIsolation,
    #[serde(skip_serializing_if = "Option::is_none")]
    failure: Option<String>,
}

impl temps_core::audit::AuditOperation for PluginIsolationAudit {
    fn operation_type(&self) -> String {
        self.operation.into()
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
    fn serialize(&self) -> temps_core::AuditResult<String> {
        temps_core::serialize_audit_operation(self)
    }
}

/// Record that an installation named its own isolation, before it runs. An
/// installation that names none is confined and needs no separate entry.
pub(super) async fn audit_requested_isolation(
    state: &ExternalPluginsAppState,
    context: &temps_core::audit::AuditContext,
    plugin: &str,
    isolation: Option<PluginIsolation>,
) -> Result<(), Problem> {
    let Some(isolation) = isolation else {
        return Ok(());
    };
    record_required_audit(
        state,
        &PluginIsolationAudit {
            context: context.clone(),
            operation: "EXTERNAL_PLUGIN_ISOLATION_REQUESTED",
            plugin: plugin.to_string(),
            isolation,
            failure: None,
        },
    )
    .await
}

fn isolation_response(
    name: String,
    status: PluginIsolationStatus,
    start_failure: Option<String>,
) -> PluginIsolationResponse {
    PluginIsolationResponse {
        name,
        requested: status.requested,
        effective: status.effective,
        sandboxed: status.effective.is_some_and(IsolationMode::is_sandboxed),
        changeable: !status.launcher,
        start_failure,
    }
}

/// Whether this server confines plugins, and how each installed one runs.
#[utoipa::path(
    tag = "External Plugins",
    get,
    path = "/x/plugins/sandbox",
    operation_id = "get_plugin_sandbox",
    responses(
        (status = 200, description = "Plugin isolation on this server", body = PluginSandboxResponse),
        (status = 401, description = "Unauthorized", body = temps_core::ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = temps_core::ProblemDetails),
        (status = 503, description = "Stored plugin isolation could not be read", body = temps_core::ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
pub(super) async fn get_plugin_sandbox(
    RequireAuth(auth): RequireAuth,
    State(state): State<ExternalPluginsAppState>,
) -> Result<Json<PluginSandboxResponse>, Problem> {
    permission_guard!(auth, SystemAdmin);
    let config = state.service.manager().config();
    let launcher = config.launcher.is_some();
    let unavailable = config.sandbox.availability().err();
    let plugins = state
        .service
        .isolation_overview()
        .await
        .map_err(|error| service_problem(&error))?
        .into_iter()
        .map(|(name, status, failure)| isolation_response(name, status, failure))
        .collect();
    let configured = launcher || unavailable.is_none();
    Ok(Json(PluginSandboxResponse {
        configured,
        reason: if launcher { None } else { unavailable },
        setup_path: (!configured).then(|| SETUP_PATH.to_string()),
        mode: if launcher {
            IsolationMode::SeparateUser
        } else if configured {
            IsolationMode::Container
        } else {
            IsolationMode::Process
        },
        plugins,
    }))
}

/// How one installed plugin is separated from Temps.
#[utoipa::path(
    tag = "External Plugins",
    get,
    path = "/x/plugins/{name}/isolation",
    operation_id = "get_plugin_isolation",
    params(("name" = String, Path)),
    responses(
        (status = 200, description = "Isolation of the plugin", body = PluginIsolationResponse),
        (status = 400, description = "Invalid plugin name", body = temps_core::ProblemDetails),
        (status = 401, description = "Unauthorized", body = temps_core::ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = temps_core::ProblemDetails),
        (status = 503, description = "Stored plugin isolation could not be read", body = temps_core::ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
pub(super) async fn get_plugin_isolation(
    RequireAuth(auth): RequireAuth,
    State(state): State<ExternalPluginsAppState>,
    Path(name): Path<String>,
) -> Result<Json<PluginIsolationResponse>, Problem> {
    permission_guard!(auth, SystemAdmin);
    let status = state
        .service
        .plugin_isolation(&name)
        .await
        .map_err(|error| service_problem(&error))?;
    let start_failure = state.service.manager().start_failure(&name).await;
    Ok(Json(isolation_response(name, status, start_failure)))
}

/// Move an installed plugin into a container, or out of one, and restart it.
#[utoipa::path(
    tag = "External Plugins",
    put,
    path = "/x/plugins/{name}/isolation",
    operation_id = "put_plugin_isolation",
    params(("name" = String, Path)),
    request_body = UpdatePluginIsolation,
    responses(
        (status = 200, description = "Plugin restarted with the requested isolation", body = PluginIsolationResponse),
        (status = 400, description = "Invalid plugin name or request body", body = temps_core::ProblemDetails),
        (status = 401, description = "Unauthorized", body = temps_core::ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = temps_core::ProblemDetails),
        (status = 404, description = "No active installation", body = temps_core::ProblemDetails),
        (status = 409, description = "This server cannot run plugins in containers, the plugin asks for access a container is not given, or isolation is managed by the plugin launcher", body = temps_core::ProblemDetails),
        (status = 415, description = "Request body is not JSON", body = temps_core::ProblemDetails),
        (status = 422, description = "Unknown isolation", body = temps_core::ProblemDetails),
        (status = 428, description = "Recent sensitive-action verification required", body = temps_core::ProblemDetails),
        (status = 500, description = "The plugin container could not be prepared", body = temps_core::ProblemDetails),
        (status = 502, description = "The plugin did not start with the new isolation and was returned to the previous one", body = temps_core::ProblemDetails),
        (status = 503, description = "Audit, database or plugin service unavailable", body = temps_core::ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
pub(super) async fn put_plugin_isolation(
    RequireAuth(auth): RequireAuth,
    State(state): State<ExternalPluginsAppState>,
    Extension(metadata): Extension<temps_core::RequestMetadata>,
    Path(name): Path<String>,
    request: Result<Json<UpdatePluginIsolation>, JsonRejection>,
) -> Result<Json<PluginIsolationResponse>, Problem> {
    permission_guard!(auth, SystemAdmin);
    crate::install::validate_plugin_name(&name)
        .map_err(|error| service_problem(&ExternalPluginsError::Install(error)))?;
    let Json(request) = request.map_err(|rejection| {
        temps_core::problemdetails::new(rejection.status())
            .with_title("Invalid Plugin Isolation Request")
            .with_detail(format!(
                "Changing the isolation of plugin '{name}' requires an application/json body \
                 whose isolation field is 'container' or 'process'"
            ))
    })?;
    temps_auth::require_sensitive_action(
        state.sensitive_action_authorizer.as_ref(),
        &auth,
        temps_core::SensitiveAction::ChangeExternalPluginGrants { name: name.clone() },
    )
    .await?;
    let context = audit_context(&auth, &metadata);
    let audit = |operation: &'static str, failure: Option<String>| PluginIsolationAudit {
        context: context.clone(),
        operation,
        plugin: name.clone(),
        isolation: request.isolation,
        failure,
    };
    // Leaving a container hands the plugin everything the Temps account can
    // reach, so the attempt is on record before it happens.
    record_required_audit(
        &state,
        &audit("EXTERNAL_PLUGIN_ISOLATION_CHANGE_REQUESTED", None),
    )
    .await?;
    let status = match state
        .service
        .set_plugin_isolation(&name, request.isolation)
        .await
    {
        Ok(status) => status,
        Err(error) => {
            record_audit(
                &state,
                &audit(
                    "EXTERNAL_PLUGIN_ISOLATION_CHANGE_FAILED",
                    Some(public_error_detail(&error)),
                ),
            )
            .await;
            return Err(service_problem(&error));
        }
    };
    record_audit(&state, &audit("EXTERNAL_PLUGIN_ISOLATION_CHANGED", None)).await;
    Ok(Json(isolation_response(name, status, None)))
}

#[cfg(test)]
mod tests {
    use super::super::tests::{
        metadata, test_state, test_state_with_audit, user_auth, AllowSensitiveActions,
        RecordingAuditLogger, RequireSensitiveVerification,
    };
    use super::*;
    use axum::response::IntoResponse;
    use temps_auth::permissions::Role;
    use tower::ServiceExt;

    fn change(isolation: PluginIsolation) -> Result<Json<UpdatePluginIsolation>, JsonRejection> {
        Ok(Json(UpdatePluginIsolation { isolation }))
    }

    /// A state whose database answers the stored-isolation query with `rows`.
    fn state_with_stored(
        rows: Vec<(&str, &str)>,
        audit: Arc<RecordingAuditLogger>,
    ) -> (tempfile::TempDir, ExternalPluginsAppState) {
        let root = tempfile::tempdir().expect("tempdir");
        let rows: Vec<std::collections::BTreeMap<&str, sea_orm::Value>> = rows
            .into_iter()
            .map(|(name, isolation)| {
                std::collections::BTreeMap::from([
                    ("plugin_name", sea_orm::Value::from(name)),
                    ("isolation", sea_orm::Value::from(isolation)),
                ])
            })
            .collect();
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
                .append_query_results([rows])
                .into_connection(),
        );
        let config = crate::manager::ExternalPluginConfig::new(
            root.path().to_path_buf(),
            "postgres://localhost/test".to_string(),
        );
        let state = ExternalPluginsAppState {
            service: Arc::new(ExternalPluginsService::new_empty(config, None, db)),
            audit_service: audit,
            sensitive_action_authorizer: Arc::new(AllowSensitiveActions),
        };
        (root, state)
    }

    #[tokio::test]
    async fn isolation_routes_reject_a_request_without_a_caller() {
        for (method, uri) in [
            ("GET", "/x/plugins/sandbox"),
            ("GET", "/x/plugins/reports/isolation"),
            ("PUT", "/x/plugins/reports/isolation"),
        ] {
            let response = configure_routes()
                .with_state(test_state())
                .layer(Extension(metadata().0))
                .oneshot(
                    axum::http::Request::builder()
                        .method(method)
                        .uri(uri)
                        .header("content-type", "application/json")
                        .body(axum::body::Body::from(r#"{"isolation":"container"}"#))
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
    async fn isolation_is_read_and_changed_only_by_a_system_administrator() {
        let audit = Arc::new(RecordingAuditLogger::default());
        let sandbox = get_plugin_sandbox(
            user_auth(Role::User),
            State(test_state_with_audit(audit.clone())),
        )
        .await
        .expect_err("reading the sandbox must require system administration");
        assert_eq!(sandbox.status_code, StatusCode::FORBIDDEN);
        let read = get_plugin_isolation(
            user_auth(Role::User),
            State(test_state_with_audit(audit.clone())),
            Path("reports".to_string()),
        )
        .await
        .expect_err("reading isolation must require system administration");
        assert_eq!(read.status_code, StatusCode::FORBIDDEN);
        let write = put_plugin_isolation(
            user_auth(Role::User),
            State(test_state_with_audit(audit.clone())),
            metadata(),
            Path("reports".to_string()),
            change(PluginIsolation::Process),
        )
        .await
        .expect_err("changing isolation must require system administration");
        assert_eq!(write.status_code, StatusCode::FORBIDDEN);
        assert!(audit.operations.lock().expect("audit lock").is_empty());
    }

    #[tokio::test]
    async fn changing_isolation_requires_recent_verification_before_anything_is_recorded() {
        let audit = Arc::new(RecordingAuditLogger::default());
        let mut state = test_state_with_audit(audit.clone());
        state.sensitive_action_authorizer = Arc::new(RequireSensitiveVerification);

        let error = put_plugin_isolation(
            user_auth(Role::PlatformAdmin),
            State(state),
            metadata(),
            Path("reports".to_string()),
            change(PluginIsolation::Process),
        )
        .await
        .expect_err("leaving a container must require recent verification");

        assert_eq!(error.status_code, StatusCode::PRECONDITION_REQUIRED);
        assert!(audit.operations.lock().expect("audit lock").is_empty());
    }

    #[tokio::test]
    async fn changing_isolation_of_a_missing_plugin_is_not_found_and_audited_as_failed() {
        let audit = Arc::new(RecordingAuditLogger::default());
        let (_root, state) = state_with_stored(Vec::new(), audit.clone());

        let error = put_plugin_isolation(
            user_auth(Role::PlatformAdmin),
            State(state),
            metadata(),
            Path("reports".to_string()),
            change(PluginIsolation::Process),
        )
        .await
        .expect_err("a plugin that is not installed cannot be moved");

        assert_eq!(error.status_code, StatusCode::NOT_FOUND);
        let response = error.into_response();
        assert_eq!(
            response.headers()["content-type"],
            "application/problem+json"
        );
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("problem body");
        let problem: serde_json::Value = serde_json::from_slice(&body).expect("problem json");
        assert_eq!(problem["title"], "Plugin Not Installed");
        assert!(problem["detail"]
            .as_str()
            .expect("detail")
            .contains("'reports'"));
        assert_eq!(
            *audit.operations.lock().expect("audit lock"),
            vec![
                "EXTERNAL_PLUGIN_ISOLATION_CHANGE_REQUESTED".to_string(),
                "EXTERNAL_PLUGIN_ISOLATION_CHANGE_FAILED".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn an_unsafe_plugin_name_or_unknown_isolation_is_rejected_before_any_audit() {
        let audit = Arc::new(RecordingAuditLogger::default());
        let unsafe_name = put_plugin_isolation(
            user_auth(Role::PlatformAdmin),
            State(test_state_with_audit(audit.clone())),
            metadata(),
            Path("../reports".to_string()),
            change(PluginIsolation::Container),
        )
        .await
        .expect_err("an unsafe name must be rejected");
        assert_eq!(unsafe_name.status_code, StatusCode::BAD_REQUEST);

        let response = configure_routes()
            .with_state(test_state_with_audit(audit.clone()))
            .layer(Extension(metadata().0))
            .layer(Extension(user_auth(Role::PlatformAdmin).0))
            .oneshot(
                axum::http::Request::builder()
                    .method("PUT")
                    .uri("/x/plugins/reports/isolation")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(r#"{"isolation":"virtual-machine"}"#))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            response.headers()["content-type"],
            "application/problem+json"
        );
        assert!(audit.operations.lock().expect("audit lock").is_empty());
    }

    #[tokio::test]
    async fn the_sandbox_status_says_why_plugins_cannot_be_contained_and_where_to_look() {
        // This state was given no Docker client: the unconfigured case.
        let (_root, state) =
            state_with_stored(Vec::new(), Arc::new(RecordingAuditLogger::default()));

        let Json(status) = get_plugin_sandbox(user_auth(Role::PlatformAdmin), State(state))
            .await
            .expect("sandbox status");

        assert!(!status.configured);
        assert!(!status.reason.as_deref().unwrap_or_default().is_empty());
        assert_eq!(status.setup_path.as_deref(), Some(SETUP_PATH));
        assert_eq!(status.mode, IsolationMode::Process);
        assert!(status.plugins.is_empty());
        let json = serde_json::to_value(&status).expect("serialize");
        assert_eq!(json["mode"], "process");
    }

    #[tokio::test]
    async fn a_failed_read_of_stored_isolation_does_not_expose_the_database_error() {
        // The mock has no prepared answer, so the query fails.
        let error = get_plugin_sandbox(user_auth(Role::PlatformAdmin), State(test_state()))
            .await
            .expect_err("a database failure must be reported");

        assert_eq!(error.status_code, StatusCode::SERVICE_UNAVAILABLE);
        let body = axum::body::to_bytes(error.into_response().into_body(), 64 * 1024)
            .await
            .expect("problem body");
        let text = String::from_utf8_lossy(&body);
        assert!(!text.contains("external_plugin_actors"), "{text}");
    }

    #[test]
    fn a_sandbox_refusal_is_a_conflict_that_explains_itself() {
        let unavailable =
            ExternalPluginsError::Sandbox(crate::sandbox::SandboxError::Unavailable {
                plugin: "reports".into(),
                reason: "Temps runs inside a container".into(),
            });
        let problem = service_problem(&unavailable);
        assert_eq!(problem.status_code, StatusCode::CONFLICT);
        assert!(public_error_detail(&unavailable).contains("Temps runs inside a container"));

        let privileged =
            ExternalPluginsError::Sandbox(crate::sandbox::SandboxError::PrivilegedAccess {
                plugin: "reports".into(),
                access: "the control-plane database",
            });
        assert_eq!(
            service_problem(&privileged).status_code,
            StatusCode::CONFLICT
        );
        assert!(public_error_detail(&privileged).contains("the control-plane database"));

        // A Docker failure names local paths; they stay in the server log.
        let runtime = ExternalPluginsError::Sandbox(crate::sandbox::SandboxError::Runtime {
            plugin: "reports".into(),
            container: "temps-plugin-reports".into(),
            operation: "create",
            reason: "invalid mount /srv/private/path".into(),
        });
        assert_eq!(
            service_problem(&runtime).status_code,
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert!(!public_error_detail(&runtime).contains("/srv/private/path"));

        let failed = ExternalPluginsError::IsolationChangeFailed {
            name: "reports".into(),
            isolation: PluginIsolation::Container,
            reason: "Plugin failed startup verification".into(),
        };
        assert_eq!(
            service_problem(&failed).status_code,
            StatusCode::BAD_GATEWAY
        );
        assert!(public_error_detail(&failed).contains("returned to its previous isolation"));
    }

    #[test]
    fn isolation_routes_and_schemas_are_in_the_openapi_document() {
        let spec = serde_json::to_value(ExternalPluginsApiDoc::openapi()).expect("spec");
        assert!(spec["paths"]["/x/plugins/sandbox"]["get"].is_object());
        let isolation = &spec["paths"]["/x/plugins/{name}/isolation"];
        assert!(isolation["get"].is_object());
        for status in ["200", "401", "403", "404", "409", "428", "502"] {
            assert!(
                isolation["put"]["responses"][status].is_object(),
                "PUT isolation must document {status}"
            );
        }
        let schemas = &spec["components"]["schemas"];
        assert_eq!(
            schemas["PluginIsolation"]["enum"],
            serde_json::json!(["container", "process"])
        );
        assert_eq!(
            schemas["IsolationMode"]["enum"],
            serde_json::json!(["container", "separate_user", "process"])
        );
        for schema in [
            "PluginSandboxResponse",
            "PluginIsolationResponse",
            "UpdatePluginIsolation",
        ] {
            assert!(schemas[schema].is_object(), "{schema}");
        }
        assert!(
            schemas["InstallRepositoryRequest"]["properties"]["isolation"].is_object(),
            "an installation can name its isolation"
        );
    }
}
