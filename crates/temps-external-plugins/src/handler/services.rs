// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Plugins that run as Temps services: list them with how each is doing,
//! and make a deployed service a plugin.
use super::*;
use crate::manager::ServicePluginState;
use crate::service::ServicePluginView;
use crate::service_runtime::ServiceBinding;

/// Where the console lists plugins and adds one from a service.
const SETUP_PATH: &str = "/settings/plugins";

/// One plugin that runs as a Temps service.
#[derive(Debug, Serialize, ToSchema)]
pub struct ServicePluginResponse {
    /// The plugin's name, as its manifest declares it.
    pub name: String,
    /// Whether Temps is attached to the service's running container.
    pub state: ServicePluginState,
    /// Why the plugin is not running, when it is not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// The service the plugin runs as. Absent once the service was deleted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment_id: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_slug: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment_name: Option<String>,
    /// Whether the service stops when idle and starts on the next request,
    /// so an unused plugin holds no memory.
    pub stops_when_idle: bool,
    /// The deployment Temps is attached to, while the plugin is running.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deployment_id: Option<i32>,
    /// Who stands behind the plugin. A community plugin runs in its own
    /// container and is never given Temps's database or data directory.
    pub tier: crate::tier::PluginTier,
    /// The managed databases and stores linked to the plugin's service,
    /// where it keeps its own data. Empty when it has none.
    pub own_storage: Vec<String>,
    /// The deployment whose build an administrator approved. The plugin is
    /// pinned to it: Temps attaches to no other build of the service.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pinned_deployment_id: Option<i32>,
    /// A build the service runs that is not the approved one. It waits for
    /// an administrator and is never applied on its own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_update: Option<super::updates::PendingPluginUpdateResponse>,
    /// Since when an administrator has the plugin switched off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disabled_at: Option<chrono::DateTime<chrono::Utc>>,
    /// The user who switched it off, while that user exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disabled_by: Option<i32>,
    /// The memory and CPU the plugin's container is limited to: the limits
    /// of the deployment its service runs. Absent before it has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_limits: Option<PluginResourceLimitsResponse>,
    /// The image the plugin runs, when its service was deployed from an
    /// image: the reference that was approved. Absent for a service built
    /// from a repository.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(example = "registry.example.com/acme/reports:1.2.0")]
    pub image: Option<String>,
    /// The Docker image ID the plugin's image is: what a registry image
    /// resolved to when it was deployed, or what an image uploaded from a
    /// client was imported as (such a plugin has no `image`). This is the
    /// content that was approved; a tag moved to other content later does
    /// not change it. Absent when Temps did not record it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_id: Option<String>,
}

/// The limits a plugin's container runs with. They are the limits of its
/// service, changed in the service's settings and applied by a deployment.
#[derive(Debug, Serialize, ToSchema, PartialEq, Eq)]
pub struct PluginResourceLimitsResponse {
    /// The deployment these limits were applied to.
    pub deployment_id: i32,
    /// Megabytes of memory. Absent when memory is not capped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_limit_mb: Option<i32>,
    /// Microcores of CPU: 1,000,000 is one core. Absent when not capped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_limit_microcores: Option<i32>,
}

impl From<crate::service::PluginResourceLimits> for PluginResourceLimitsResponse {
    fn from(limits: crate::service::PluginResourceLimits) -> Self {
        Self {
            deployment_id: limits.deployment_id,
            memory_limit_mb: limits.memory_limit_mb,
            cpu_limit_microcores: limits.cpu_limit_microcores,
        }
    }
}

impl From<ServicePluginView> for ServicePluginResponse {
    fn from(view: ServicePluginView) -> Self {
        Self {
            name: view.name,
            state: view.state,
            detail: view.detail,
            project_id: view.binding.map(|binding| binding.project_id),
            environment_id: view.binding.map(|binding| binding.environment_id),
            service_name: view.service_name,
            service_slug: view.service_slug,
            environment_name: view.environment_name,
            stops_when_idle: view.stops_when_idle,
            deployment_id: view.deployment_id,
            tier: view.tier,
            own_storage: view.own_storage,
            pinned_deployment_id: view.pinned_deployment_id,
            pending_update: view.pending_update.map(Into::into),
            disabled_at: view.disabled_at,
            disabled_by: view.disabled_by,
            resource_limits: view.resource_limits.map(Into::into),
            image: view.image,
            image_id: view.image_id,
        }
    }
}

/// A plugin an earlier version installed as an executable. It does not run:
/// a plugin runs only as a Temps service.
#[derive(Debug, Serialize, ToSchema)]
pub struct RetiredPluginResponse {
    pub name: String,
    /// The version that was installed, when its record still says.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Why it does not run and what to do about it.
    pub reason: String,
}

impl From<crate::retired::RetiredPlugin> for RetiredPluginResponse {
    fn from(plugin: crate::retired::RetiredPlugin) -> Self {
        Self {
            name: plugin.name,
            version: plugin.version,
            reason: crate::retired::RETIRED_REASON.to_string(),
        }
    }
}

/// Whether this server can run plugins as services, and the ones it runs.
#[derive(Debug, Serialize, ToSchema)]
pub struct PluginServicesResponse {
    /// Whether a service can be added as a plugin on this server.
    pub configured: bool,
    /// Why not, when it cannot.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Where the console explains what is missing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub setup_path: Option<String>,
    pub plugins: Vec<ServicePluginResponse>,
    /// Plugins installed as executables, which no longer run. Each stays
    /// listed, with the reason, until an administrator removes it.
    pub retired: Vec<RetiredPluginResponse>,
}

/// The deployed service to add as a plugin.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AddServicePlugin {
    /// The service (project) the plugin runs as.
    pub project_id: i32,
    /// The environment of that service whose deployment is the plugin.
    pub environment_id: i32,
    /// The deployment that was looked at. When given, the service is added
    /// only if this is the deployment it runs; another one is refused with
    /// 409 and nothing is added.
    #[serde(default)]
    #[schema(nullable = false)]
    pub deployment_id: Option<i32>,
}

/// The plugin a service was added as.
#[derive(Debug, Serialize, ToSchema)]
pub struct AddedServicePluginResponse {
    pub name: String,
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub project_id: i32,
    pub environment_id: i32,
}

#[derive(Serialize)]
struct ServicePluginAudit {
    context: temps_core::audit::AuditContext,
    #[serde(skip)]
    operation: &'static str,
    project_id: i32,
    environment_id: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    plugin: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    failure: Option<String>,
}

impl temps_core::audit::AuditOperation for ServicePluginAudit {
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

/// The plugins that run as services, and whether one can be added here.
#[utoipa::path(
    tag = "External Plugins",
    get,
    path = "/x/plugins/services",
    operation_id = "get_plugin_services",
    responses(
        (status = 200, description = "Plugins that run as Temps services", body = PluginServicesResponse),
        (status = 401, description = "Unauthorized", body = temps_core::ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = temps_core::ProblemDetails),
        (status = 500, description = "The services plugins run as could not be read", body = temps_core::ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
pub(super) async fn get_plugin_services(
    RequireAuth(auth): RequireAuth,
    State(state): State<ExternalPluginsAppState>,
) -> Result<Json<PluginServicesResponse>, Problem> {
    permission_guard!(auth, SystemAdmin);
    let configured = state.service.manager().can_run_services().await;
    let plugins = state
        .service
        .service_plugins()
        .await
        .map_err(|error| service_problem(&error))?
        .into_iter()
        .map(ServicePluginResponse::from)
        .collect();
    Ok(Json(PluginServicesResponse {
        configured,
        reason: (!configured)
            .then(|| crate::service_runtime::ServiceAttachError::Unavailable.public_reason()),
        setup_path: (!configured).then(|| SETUP_PATH.to_string()),
        plugins,
        retired: state
            .service
            .manager()
            .retired_plugins()
            .await
            .into_iter()
            .map(RetiredPluginResponse::from)
            .collect(),
    }))
}

/// Make a deployed service a plugin: Temps attaches to its running
/// container, reads its manifest and routes requests to it. The plugin
/// starts with no grants, and the service itself is not changed.
#[utoipa::path(
    tag = "External Plugins",
    post,
    path = "/x/plugins/services",
    operation_id = "add_service_plugin",
    request_body = AddServicePlugin,
    responses(
        (status = 201, description = "The service is now a plugin", body = AddedServicePluginResponse),
        (status = 400, description = "Invalid request body, or the service is not a plugin Temps accepts", body = temps_core::ProblemDetails),
        (status = 401, description = "Unauthorized", body = temps_core::ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = temps_core::ProblemDetails),
        (status = 404, description = "The service or environment does not exist", body = temps_core::ProblemDetails),
        (status = 409, description = "The service is stopped or not deployed, its plugin name is already installed, or it runs another deployment than the one named", body = temps_core::ProblemDetails),
        (status = 415, description = "Request body is not JSON", body = temps_core::ProblemDetails),
        (status = 422, description = "Request body does not name a service and an environment", body = temps_core::ProblemDetails),
        (status = 428, description = "Recent sensitive-action verification required", body = temps_core::ProblemDetails),
        (status = 500, description = "The plugin could not be recorded", body = temps_core::ProblemDetails),
        (status = 502, description = "The service did not answer as a plugin", body = temps_core::ProblemDetails),
        (status = 503, description = "Audit unavailable, or this server runs no services", body = temps_core::ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
pub(super) async fn add_service_plugin(
    RequireAuth(auth): RequireAuth,
    State(state): State<ExternalPluginsAppState>,
    Extension(metadata): Extension<temps_core::RequestMetadata>,
    request: Result<Json<AddServicePlugin>, JsonRejection>,
) -> Result<(StatusCode, Json<AddedServicePluginResponse>), Problem> {
    permission_guard!(auth, SystemAdmin);
    let Json(request) = request.map_err(|rejection| {
        temps_core::problemdetails::new(rejection.status())
            .with_title("Invalid Service Plugin Request")
            .with_detail(
                "Adding a service as a plugin requires an application/json body with the \
                 project_id of the service and the environment_id of the environment it runs in",
            )
    })?;
    let binding = ServiceBinding {
        project_id: request.project_id,
        environment_id: request.environment_id,
    };
    temps_auth::require_sensitive_action(
        state.sensitive_action_authorizer.as_ref(),
        &auth,
        temps_core::SensitiveAction::InstallExternalPlugin {
            name: binding.source_identity(),
        },
    )
    .await?;
    let context = audit_context(&auth, &metadata);
    let audit = |operation: &'static str,
                 plugin: Option<&PluginManifest>,
                 failure: Option<String>| ServicePluginAudit {
        context: context.clone(),
        operation,
        project_id: binding.project_id,
        environment_id: binding.environment_id,
        plugin: plugin.map(|manifest| manifest.name.clone()),
        version: plugin.map(|manifest| manifest.version.clone()),
        failure,
    };
    // Attaching lets the service's code act as a plugin, so the attempt is on
    // record before Temps connects to it.
    record_required_audit(
        &state,
        &audit("EXTERNAL_PLUGIN_SERVICE_ADD_REQUESTED", None, None),
    )
    .await?;
    let manifest = match state
        .service
        .add_service_plugin(binding, request.deployment_id)
        .await
    {
        Ok(manifest) => manifest,
        Err(error) => {
            record_audit(
                &state,
                &audit(
                    "EXTERNAL_PLUGIN_SERVICE_ADD_FAILED",
                    None,
                    Some(public_error_detail(&error)),
                ),
            )
            .await;
            return Err(service_problem(&error));
        }
    };
    record_audit(
        &state,
        &audit("EXTERNAL_PLUGIN_SERVICE_ADDED", Some(&manifest), None),
    )
    .await;
    Ok((
        StatusCode::CREATED,
        Json(AddedServicePluginResponse {
            name: manifest.name,
            version: manifest.version,
            display_name: manifest.display_name,
            project_id: binding.project_id,
            environment_id: binding.environment_id,
        }),
    ))
}

#[cfg(test)]
pub(super) mod tests {
    use super::super::tests::{
        metadata, test_state, test_state_with_audit, user_auth, AllowSensitiveActions,
        RecordingAuditLogger, RequireSensitiveVerification,
    };
    use super::*;
    use crate::manager::services::tests::{platform, serve_plugin, Platform, TOKEN};
    use axum::response::IntoResponse;
    use temps_auth::permissions::Role;
    use tower::ServiceExt;

    pub(in crate::handler) fn add(
        binding: ServiceBinding,
    ) -> Result<Json<AddServicePlugin>, JsonRejection> {
        Ok(Json(AddServicePlugin {
            project_id: binding.project_id,
            environment_id: binding.environment_id,
            deployment_id: None,
        }))
    }

    /// A state on a real database whose plugin host can attach to services.
    pub(in crate::handler) async fn state_on(
        platform: &Platform,
        audit: Arc<RecordingAuditLogger>,
    ) -> ExternalPluginsAppState {
        let service = Arc::new(ExternalPluginsService::new_empty(
            platform.config(),
            None,
            platform.db(),
        ));
        service
            .manager()
            .set_service_runtime(platform.runtime())
            .await;
        ExternalPluginsAppState {
            service,
            audit_service: audit,
            sensitive_action_authorizer: Arc::new(AllowSensitiveActions),
        }
    }

    pub(in crate::handler) fn operations(audit: &RecordingAuditLogger) -> Vec<String> {
        audit.operations.lock().expect("audit lock").clone()
    }

    #[test]
    fn service_plugin_routes_and_schemas_are_in_the_openapi_document() {
        let spec = serde_json::to_value(ExternalPluginsApiDoc::openapi()).expect("spec");
        let route = &spec["paths"]["/x/plugins/services"];
        for status in ["200", "401", "403", "500"] {
            assert!(
                route["get"]["responses"][status].is_object(),
                "GET service plugins must document {status}"
            );
        }
        for status in [
            "201", "400", "401", "403", "404", "409", "428", "500", "502", "503",
        ] {
            assert!(
                route["post"]["responses"][status].is_object(),
                "POST service plugins must document {status}"
            );
        }
        let schemas = &spec["components"]["schemas"];
        assert_eq!(
            schemas["ServicePluginState"]["enum"],
            serde_json::json!([
                "running",
                "asleep",
                "update_waiting",
                "disabled",
                "unavailable"
            ])
        );
        for schema in [
            "PluginServicesResponse",
            "ServicePluginResponse",
            "AddServicePlugin",
            "AddedServicePluginResponse",
        ] {
            assert!(schemas[schema].is_object(), "{schema}");
        }
    }

    #[tokio::test]
    async fn service_plugin_routes_require_a_caller() {
        let state = test_state();
        for (method, body) in [
            ("GET", ""),
            ("POST", r#"{"project_id":1,"environment_id":1}"#),
        ] {
            let response = configure_routes()
                .with_state(state.clone())
                .oneshot(
                    axum::http::Request::builder()
                        .method(method)
                        .uri("/x/plugins/services")
                        .header("content-type", "application/json")
                        .body(axum::body::Body::from(body))
                        .expect("request"),
                )
                .await
                .expect("response");
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{method}");
        }
    }

    #[tokio::test]
    async fn service_plugins_are_for_system_administrators_only() {
        let audit = Arc::new(RecordingAuditLogger::default());
        let state = test_state_with_audit(audit.clone());
        let binding = ServiceBinding {
            project_id: 1,
            environment_id: 1,
        };

        let listed = get_plugin_services(user_auth(Role::User), State(state.clone()))
            .await
            .expect_err("a member cannot list service plugins")
            .into_response();
        assert_eq!(listed.status(), StatusCode::FORBIDDEN);

        let added = add_service_plugin(
            user_auth(Role::User),
            State(state),
            metadata(),
            add(binding),
        )
        .await
        .expect_err("a member cannot add a service plugin")
        .into_response();
        assert_eq!(added.status(), StatusCode::FORBIDDEN);
        assert!(operations(&audit).is_empty(), "nothing was attempted");
    }

    #[tokio::test]
    async fn adding_a_service_needs_recent_verification_before_anything_is_attempted() {
        let audit = Arc::new(RecordingAuditLogger::default());
        let mut state = test_state_with_audit(audit.clone());
        state.sensitive_action_authorizer = Arc::new(RequireSensitiveVerification);

        let error = add_service_plugin(
            user_auth(Role::PlatformAdmin),
            State(state),
            metadata(),
            add(ServiceBinding {
                project_id: 1,
                environment_id: 1,
            }),
        )
        .await
        .expect_err("verification is required");

        assert_eq!(error.status_code, StatusCode::PRECONDITION_REQUIRED);
        assert!(operations(&audit).is_empty());
    }

    #[tokio::test]
    async fn a_server_that_runs_no_services_says_so_instead_of_listing_nothing() {
        let audit = Arc::new(RecordingAuditLogger::default());
        let state = test_state_with_audit(audit.clone());

        let Json(listed) =
            get_plugin_services(user_auth(Role::PlatformAdmin), State(state.clone()))
                .await
                .expect("the capability is readable");
        assert!(!listed.configured);
        assert!(
            listed
                .reason
                .as_deref()
                .is_some_and(|reason| reason.contains("cannot run plugins as services")),
            "{listed:?}"
        );
        assert_eq!(listed.setup_path.as_deref(), Some("/settings/plugins"));
        assert!(listed.plugins.is_empty());
        assert!(listed.retired.is_empty());

        let error = add_service_plugin(
            user_auth(Role::PlatformAdmin),
            State(state),
            metadata(),
            add(ServiceBinding {
                project_id: 1,
                environment_id: 1,
            }),
        )
        .await
        .expect_err("nothing to attach to");
        assert_eq!(error.status_code, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            operations(&audit),
            [
                "EXTERNAL_PLUGIN_SERVICE_ADD_REQUESTED",
                "EXTERNAL_PLUGIN_SERVICE_ADD_FAILED"
            ]
        );
    }

    /// What an upgrade finds on a server where an earlier version installed
    /// a plugin as an executable.
    #[tokio::test]
    async fn a_plugin_installed_as_an_executable_is_listed_with_the_reason_until_it_is_removed() {
        let root = tempfile::tempdir().expect("tempdir");
        let config = crate::manager::ExternalPluginConfig::new(root.path().to_path_buf());
        let installation = config.plugins_dir.join("reports");
        std::fs::create_dir_all(installation.join("1.4.0-abc")).expect("release");
        std::fs::write(installation.join("1.4.0-abc/plugin"), b"binary").expect("binary");
        std::fs::write(
            installation.join("active.json"),
            br#"{"version":"1.4.0","directory":"1.4.0-abc"}"#,
        )
        .expect("active record");
        let audit = Arc::new(RecordingAuditLogger::default());
        let database = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();
        let service = Arc::new(ExternalPluginsService::new_empty(
            config,
            None,
            Arc::new(database),
        ));
        service.manager().discover_and_start().await;
        let state = ExternalPluginsAppState {
            service,
            audit_service: audit.clone(),
            sensitive_action_authorizer: Arc::new(AllowSensitiveActions),
        };

        // It is not a plugin anyone can use: no manifest, no route.
        let Json(manifests) =
            list_external_plugins(user_auth(Role::User), State(state.clone())).await;
        assert!(manifests.is_empty());
        // An administrator sees it, with what to do.
        let Json(listed) =
            get_plugin_services(user_auth(Role::PlatformAdmin), State(state.clone()))
                .await
                .expect("the listing is readable");
        assert!(listed.plugins.is_empty());
        assert_eq!(listed.retired.len(), 1);
        assert_eq!(listed.retired[0].name, "reports");
        assert_eq!(listed.retired[0].version.as_deref(), Some("1.4.0"));
        assert!(listed.retired[0]
            .reason
            .contains("Deploy the plugin as a service"));
        let body = serde_json::to_value(&listed).expect("serializable");
        assert_eq!(body["retired"][0]["name"], "reports");

        // Removing it is audited and keeps what it stored.
        let Json(removed) = uninstall_plugin(
            user_auth(Role::PlatformAdmin),
            State(state.clone()),
            metadata(),
            Path("reports".to_string()),
        )
        .await
        .expect("an installed executable can be removed");
        assert_eq!(removed.name, "reports");
        assert!(removed.data_preserved);
        assert_eq!(
            operations(&audit),
            [
                "EXTERNAL_PLUGIN_UNINSTALL_REQUESTED",
                "EXTERNAL_PLUGIN_UNINSTALLED"
            ]
        );
        assert!(installation.join("1.4.0-abc/plugin").is_file());
        let Json(after) = get_plugin_services(user_auth(Role::PlatformAdmin), State(state))
            .await
            .expect("the listing is readable");
        assert!(after.retired.is_empty());
    }

    #[tokio::test]
    async fn an_unreadable_request_is_explained_as_problem_details() {
        let state = test_state();
        let mut request = axum::http::Request::builder()
            .method("POST")
            .uri("/x/plugins/services")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(r#"{"project_id":"reports"}"#))
            .expect("request");
        request
            .extensions_mut()
            .insert(user_auth(Role::PlatformAdmin).0);
        request.extensions_mut().insert(metadata().0);

        let response = configure_routes()
            .with_state(state)
            .oneshot(request)
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            response
                .headers()
                .get("content-type")
                .and_then(|value| value.to_str().ok()),
            Some("application/problem+json")
        );
        let body = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .expect("body");
        let problem: serde_json::Value = serde_json::from_slice(&body).expect("problem json");
        assert_eq!(problem["title"], "Invalid Service Plugin Request");
        assert!(
            problem["detail"]
                .as_str()
                .is_some_and(|detail| detail.contains("project_id")),
            "{problem}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_administrator_adds_a_service_sees_it_running_and_removes_it() {
        let Some(platform) = platform("adding a service as a plugin over the API").await else {
            return;
        };
        let plugin = serve_plugin("reports", TOKEN);
        let binding = platform.service("reports-service").await;
        let deployment_id = platform.deploy(binding, plugin.port).await;
        let audit = Arc::new(RecordingAuditLogger::default());
        let state = state_on(&platform, audit.clone()).await;

        let (status, Json(added)) = add_service_plugin(
            user_auth(Role::PlatformAdmin),
            State(state.clone()),
            metadata(),
            add(binding),
        )
        .await
        .expect("a deployed plugin service is added");

        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(added.name, "reports");
        assert_eq!(added.version, "1.0.0");
        assert_eq!(added.display_name.as_deref(), Some("Reports"));
        assert_eq!(
            operations(&audit),
            [
                "EXTERNAL_PLUGIN_SERVICE_ADD_REQUESTED",
                "EXTERNAL_PLUGIN_SERVICE_ADDED"
            ]
        );

        let Json(listed) =
            get_plugin_services(user_auth(Role::PlatformAdmin), State(state.clone()))
                .await
                .expect("list");
        assert!(listed.configured);
        assert_eq!(listed.reason, None);
        assert_eq!(listed.plugins.len(), 1);
        let listed = &listed.plugins[0];
        assert_eq!(listed.name, "reports");
        assert_eq!(listed.state, ServicePluginState::Running);
        assert_eq!(listed.project_id, Some(binding.project_id));
        assert_eq!(listed.environment_id, Some(binding.environment_id));
        assert_eq!(listed.service_name.as_deref(), Some("reports-service"));
        assert_eq!(listed.environment_name.as_deref(), Some("production"));
        assert_eq!(listed.deployment_id, Some(deployment_id));
        assert!(!listed.stops_when_idle);
        // A service an administrator adds is a community plugin, and one
        // with nothing linked to its service has no storage of its own.
        assert_eq!(listed.tier, crate::tier::PluginTier::Community);
        assert!(listed.own_storage.is_empty());
        assert_eq!(
            serde_json::to_value(listed).expect("json")["tier"],
            serde_json::json!("community")
        );

        // A database linked to the plugin's service is the plugin's own.
        let db = platform.db();
        use sea_orm::ConnectionTrait;
        db.execute_unprepared(&format!(
            "WITH store AS (INSERT INTO external_services (name, service_type, status, created_at, updated_at) \
                 VALUES ('reports-db', 'postgres', 'running', NOW(), NOW()) RETURNING id) \
             INSERT INTO project_services (project_id, service_id, database_provisioning_mode, env_aliases, created_at, updated_at) \
                 SELECT {}, id, 'project_environment', '{{}}'::jsonb, NOW(), NOW() FROM store",
            binding.project_id
        ))
        .await
        .expect("link a database to the plugin's service");
        let Json(with_storage) =
            get_plugin_services(user_auth(Role::PlatformAdmin), State(state.clone()))
                .await
                .expect("list");
        assert_eq!(with_storage.plugins[0].own_storage, ["reports-db"]);

        // Removing the plugin revokes it and leaves the service alone.
        state
            .service
            .uninstall_plugin("reports")
            .await
            .expect("remove the plugin");
        assert_eq!(
            platform.stored("reports").await.map(|row| row.4),
            Some(false),
            "the plugin's actor is revoked"
        );
        let Json(after) = get_plugin_services(user_auth(Role::PlatformAdmin), State(state.clone()))
            .await
            .expect("list");
        assert!(after.plugins.is_empty());
        let response = reqwest::get(format!(
            "http://127.0.0.1:{}{}",
            plugin.port,
            temps_plugin_protocol::PLUGIN_HELLO_PATH
        ))
        .await
        .expect("the service still answers");
        assert_eq!(
            response.status().as_u16(),
            401,
            "the service keeps running and still asks for the instance's secret"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_permission_screen_shows_what_a_plugin_asks_for_and_narrows_what_it_may_change() {
        use super::super::{get_plugin_grants, put_plugin_grants};
        use crate::grants::PluginGrantConfig;
        use crate::scope::PluginWriteArea;
        use temps_plugin_protocol::channel::PluginHostPermission;

        let Some(platform) = platform("choosing what a plugin may change over the API").await
        else {
            return;
        };
        let plugin = serve_plugin("reports", TOKEN);
        let binding = platform.service("reports-service").await;
        platform.deploy(binding, plugin.port).await;
        let audit = Arc::new(RecordingAuditLogger::default());
        let state = state_on(&platform, audit.clone()).await;
        let (status, _) = add_service_plugin(
            user_auth(Role::PlatformAdmin),
            State(state.clone()),
            metadata(),
            add(binding),
        )
        .await
        .expect("a deployed plugin service is added");
        assert_eq!(status, StatusCode::CREATED);

        // A plugin that was just added asks for things and has none of them.
        let Json(asked) = get_plugin_grants(
            user_auth(Role::PlatformAdmin),
            State(state.clone()),
            Path("reports".to_string()),
        )
        .await
        .expect("the permission screen");
        assert!(asked
            .requested_permissions
            .contains(&PluginHostPermission::ApiWrite));
        assert!(asked.permissions.is_empty());
        assert!(asked.write_areas.is_empty());
        assert_eq!(asked.write_project_ids, None);
        assert_eq!(asked.public_paths, ["/hooks/*"]);
        assert_eq!(asked.events, ["deployment.succeeded"]);
        assert_eq!(asked.outbound_network.profile, "open");
        assert_eq!(
            asked.outbound_network.settings_path,
            "/settings/network-policy"
        );

        let save = |config: PluginGrantConfig| {
            put_plugin_grants(
                user_auth(Role::PlatformAdmin),
                State(state.clone()),
                metadata(),
                Path("reports".to_string()),
                Json(config),
            )
        };
        let writing = |areas: &[PluginWriteArea], projects: Option<Vec<i32>>| PluginGrantConfig {
            permissions: vec![PluginHostPermission::ApiWrite],
            write_areas: areas.to_vec(),
            write_project_ids: projects,
            ..Default::default()
        };

        // What may change cannot be chosen without the permission to change.
        let problem = save(PluginGrantConfig {
            write_areas: vec![PluginWriteArea::Deployments],
            ..Default::default()
        })
        .await
        .expect_err("areas without api_write");
        assert_eq!(problem.status_code, StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(problem.into_response().into_body(), usize::MAX)
            .await
            .expect("problem body");
        let body: serde_json::Value = serde_json::from_slice(&body).expect("problem json");
        assert_eq!(body["title"], "Invalid Plugin Grants");
        assert!(
            body["detail"]
                .as_str()
                .is_some_and(|detail| detail.contains("api_write")),
            "{body}"
        );

        for (config, why) in [
            (
                writing(
                    &[PluginWriteArea::Deployments, PluginWriteArea::Deployments],
                    None,
                ),
                "an area listed twice",
            ),
            (
                writing(&[PluginWriteArea::Deployments], Some(Vec::new())),
                "an empty project limit",
            ),
            (
                writing(&[PluginWriteArea::Deployments], Some(vec![i32::MAX])),
                "a project that does not exist",
            ),
        ] {
            let problem = save(config).await.expect_err(why);
            assert_eq!(problem.status_code, StatusCode::BAD_REQUEST, "{why}");
        }
        let unknown = save(writing(
            &[PluginWriteArea::Deployments],
            Some(vec![i32::MAX]),
        ))
        .await
        .expect_err("a project that does not exist");
        let body = axum::body::to_bytes(unknown.into_response().into_body(), usize::MAX)
            .await
            .expect("problem body");
        let body: serde_json::Value = serde_json::from_slice(&body).expect("problem json");
        assert!(
            body["detail"]
                .as_str()
                .is_some_and(|detail| detail.contains(&i32::MAX.to_string())),
            "the refusal names the missing project: {body}"
        );

        // Deployments only, and only in the plugin's own project.
        let Json(saved) = save(writing(
            &[PluginWriteArea::Deployments],
            Some(vec![binding.project_id]),
        ))
        .await
        .expect("a narrowed write permission is saved");
        assert_eq!(saved.permissions, [PluginHostPermission::ApiWrite]);
        assert_eq!(saved.write_areas, [PluginWriteArea::Deployments]);
        assert_eq!(saved.write_project_ids, Some(vec![binding.project_id]));
        let json = serde_json::to_value(&saved).expect("json");
        assert_eq!(json["write_areas"], serde_json::json!(["deployments"]));
        assert!(operations(&audit).contains(&"EXTERNAL_PLUGIN_GRANTS_CHANGED".to_string()));

        // The environment's network policy is what the screen reports.
        use sea_orm::ConnectionTrait;
        platform
            .db()
            .execute_unprepared(&format!(
                "INSERT INTO network_policy_profiles (subject_kind, subject_id, egress) \
                 VALUES ('deployment', {}, 'none')",
                binding.environment_id
            ))
            .await
            .expect("a network policy for the plugin's environment");
        let Json(closed) = get_plugin_grants(
            user_auth(Role::PlatformAdmin),
            State(state.clone()),
            Path("reports".to_string()),
        )
        .await
        .expect("the permission screen");
        assert_eq!(closed.outbound_network.profile, "none");
        assert!(closed.outbound_network.detail.contains("cannot connect"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_service_is_added_only_at_the_deployment_that_was_named() {
        let Some(platform) = platform("adding a service at a named deployment").await else {
            return;
        };
        let first = serve_plugin("reports", TOKEN);
        let binding = platform.service("reports-service").await;
        let watched = platform.deploy(binding, first.port).await;
        // Another deployment becomes current before the service is added.
        let second = serve_plugin("reports", TOKEN);
        let current = platform.deploy(binding, second.port).await;
        let audit = Arc::new(RecordingAuditLogger::default());
        let state = state_on(&platform, audit.clone()).await;
        let naming = |deployment_id| {
            Ok(Json(AddServicePlugin {
                project_id: binding.project_id,
                environment_id: binding.environment_id,
                deployment_id: Some(deployment_id),
            }))
        };

        let error = add_service_plugin(
            user_auth(Role::PlatformAdmin),
            State(state.clone()),
            metadata(),
            naming(watched),
        )
        .await
        .expect_err("the service runs another deployment");
        assert_eq!(error.status_code, StatusCode::CONFLICT);
        let body = axum::body::to_bytes(error.into_response().into_body(), 1 << 20)
            .await
            .expect("body");
        let problem: serde_json::Value = serde_json::from_slice(&body).expect("problem json");
        assert_eq!(problem["title"], "Service Runs Another Deployment");
        assert!(
            problem["detail"].as_str().is_some_and(|detail| detail
                .contains(&format!("deployment {current}, not deployment {watched}"))),
            "{problem}"
        );
        let Json(listed) =
            get_plugin_services(user_auth(Role::PlatformAdmin), State(state.clone()))
                .await
                .expect("list");
        assert!(listed.plugins.is_empty(), "nothing was added");

        let (status, Json(added)) = add_service_plugin(
            user_auth(Role::PlatformAdmin),
            State(state.clone()),
            metadata(),
            naming(current),
        )
        .await
        .expect("the deployment that runs is added");
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(added.name, "reports");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_service_that_is_not_a_plugin_is_refused_with_a_reason_and_an_audit_entry() {
        let Some(platform) = platform("refusing a service over the API").await else {
            return;
        };
        let binding = platform.service("reports-service").await;
        let audit = Arc::new(RecordingAuditLogger::default());
        let state = state_on(&platform, audit.clone()).await;

        // Not deployed yet.
        let error = add_service_plugin(
            user_auth(Role::PlatformAdmin),
            State(state.clone()),
            metadata(),
            add(binding),
        )
        .await
        .expect_err("nothing is deployed");
        assert_eq!(error.status_code, StatusCode::CONFLICT);

        // A service that does not exist: the detail names the id.
        let error = add_service_plugin(
            user_auth(Role::PlatformAdmin),
            State(state.clone()),
            metadata(),
            add(ServiceBinding {
                project_id: 987_654,
                environment_id: 1,
            }),
        )
        .await
        .expect_err("no such service");
        assert_eq!(error.status_code, StatusCode::NOT_FOUND);
        let body = axum::body::to_bytes(error.into_response().into_body(), 1 << 20)
            .await
            .expect("body");
        let problem: serde_json::Value = serde_json::from_slice(&body).expect("problem json");
        assert_eq!(problem["title"], "Service Not Found");
        assert!(
            problem["detail"]
                .as_str()
                .is_some_and(|detail| detail.contains("987654")),
            "{problem}"
        );

        assert_eq!(
            operations(&audit),
            [
                "EXTERNAL_PLUGIN_SERVICE_ADD_REQUESTED",
                "EXTERNAL_PLUGIN_SERVICE_ADD_FAILED",
                "EXTERNAL_PLUGIN_SERVICE_ADD_REQUESTED",
                "EXTERNAL_PLUGIN_SERVICE_ADD_FAILED"
            ]
        );
        assert_eq!(platform.stored("reports").await, None);
    }
}
