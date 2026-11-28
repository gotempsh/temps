// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The switch that cuts a plugin off from Temps, and puts it back.
use super::*;

/// Whether a plugin is switched off, after the call.
#[derive(Debug, Serialize, ToSchema)]
pub struct PluginSwitchResponse {
    pub name: String,
    /// Whether Temps is cut off from the plugin.
    pub disabled: bool,
    /// What happened, and what was left as it is.
    pub message: String,
}

fn switch_audit(
    context: &temps_core::audit::AuditContext,
    name: &str,
    operation: &str,
    failure: Option<String>,
) -> ExternalPluginWriteAudit {
    ExternalPluginWriteAudit {
        context: context.clone(),
        operation: operation.to_string(),
        plugin_name: Some(name.to_string()),
        version: None,
        platform: None,
        sha256: None,
        signer_key_id: None,
        registry_source: None,
        failure,
    }
}

/// Switch a plugin off: Temps closes its channel at once and sends it no
/// request and no event until it is enabled. Its permissions, its approved
/// build and its service are kept.
///
/// Switching off takes away access, so it asks for no further verification.
#[utoipa::path(
    tag = "External Plugins",
    post,
    path = "/x/plugins/{name}/disable",
    operation_id = "disable_plugin",
    params(("name" = String, Path, description = "The plugin's name")),
    responses(
        (status = 200, description = "Temps is cut off from the plugin", body = PluginSwitchResponse),
        (status = 400, description = "Invalid plugin name", body = temps_core::ProblemDetails),
        (status = 401, description = "Unauthorized", body = temps_core::ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = temps_core::ProblemDetails),
        (status = 404, description = "No plugin with that name was added", body = temps_core::ProblemDetails),
        (status = 500, description = "The plugin is cut off, but that could not be saved", body = temps_core::ProblemDetails),
        (status = 503, description = "The plugin host is shutting down", body = temps_core::ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
pub(super) async fn disable_plugin(
    RequireAuth(auth): RequireAuth,
    State(state): State<ExternalPluginsAppState>,
    Extension(metadata): Extension<temps_core::RequestMetadata>,
    Path(name): Path<String>,
) -> Result<Json<PluginSwitchResponse>, Problem> {
    permission_guard!(auth, SystemAdmin);
    let context = audit_context(&auth, &metadata);
    if let Err(error) = state.service.disable_plugin(&name, auth.user_id()).await {
        record_audit(
            &state,
            &switch_audit(
                &context,
                &name,
                "EXTERNAL_PLUGIN_DISABLE_FAILED",
                Some(public_error_detail(&error)),
            ),
        )
        .await;
        return Err(service_problem(&error));
    }
    // The plugin is already cut off: a failed audit write is logged and the
    // switch still succeeds.
    record_audit(
        &state,
        &switch_audit(&context, &name, "EXTERNAL_PLUGIN_DISABLED", None),
    )
    .await;
    Ok(Json(PluginSwitchResponse {
        message: format!(
            "Plugin '{name}' is disabled: Temps sends it nothing and takes no calls from it. Its service keeps running, and its permissions are kept for when it is enabled"
        ),
        name,
        disabled: true,
    }))
}

/// Switch a plugin on again and connect to it, if its approved build runs.
#[utoipa::path(
    tag = "External Plugins",
    post,
    path = "/x/plugins/{name}/enable",
    operation_id = "enable_plugin",
    params(("name" = String, Path, description = "The plugin's name")),
    responses(
        (status = 200, description = "The plugin may connect again", body = PluginSwitchResponse),
        (status = 400, description = "Invalid plugin name", body = temps_core::ProblemDetails),
        (status = 401, description = "Unauthorized", body = temps_core::ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = temps_core::ProblemDetails),
        (status = 404, description = "No plugin with that name was added", body = temps_core::ProblemDetails),
        (status = 428, description = "Recent sensitive-action verification required", body = temps_core::ProblemDetails),
        (status = 500, description = "The plugin could not be enabled", body = temps_core::ProblemDetails),
        (status = 503, description = "Audit unavailable, or the plugin host is shutting down", body = temps_core::ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
pub(super) async fn enable_plugin(
    RequireAuth(auth): RequireAuth,
    State(state): State<ExternalPluginsAppState>,
    Extension(metadata): Extension<temps_core::RequestMetadata>,
    Path(name): Path<String>,
) -> Result<Json<PluginSwitchResponse>, Problem> {
    permission_guard!(auth, SystemAdmin);
    if !crate::naming::valid_plugin_name(&name) {
        return Err(service_problem(&ExternalPluginsError::InvalidPluginName {
            name,
        }));
    }
    // Enabling gives the plugin's code its permissions back.
    temps_auth::require_sensitive_action(
        state.sensitive_action_authorizer.as_ref(),
        &auth,
        temps_core::SensitiveAction::InstallExternalPlugin { name: name.clone() },
    )
    .await?;
    let context = audit_context(&auth, &metadata);
    record_required_audit(
        &state,
        &switch_audit(&context, &name, "EXTERNAL_PLUGIN_ENABLE_REQUESTED", None),
    )
    .await?;
    if let Err(error) = state.service.enable_plugin(&name).await {
        record_audit(
            &state,
            &switch_audit(
                &context,
                &name,
                "EXTERNAL_PLUGIN_ENABLE_FAILED",
                Some(public_error_detail(&error)),
            ),
        )
        .await;
        return Err(service_problem(&error));
    }
    record_audit(
        &state,
        &switch_audit(&context, &name, "EXTERNAL_PLUGIN_ENABLED", None),
    )
    .await;
    Ok(Json(PluginSwitchResponse {
        message: format!(
            "Plugin '{name}' is enabled, with the permissions it had. Temps connects to it when its approved build runs"
        ),
        name,
        disabled: false,
    }))
}

#[cfg(test)]
mod tests {
    use super::super::services::tests::{add, operations, state_on};
    use super::super::services::{add_service_plugin, get_plugin_services};
    use super::super::tests::{
        metadata, test_state_with_audit, user_auth, RecordingAuditLogger,
        RequireSensitiveVerification,
    };
    use super::*;
    use crate::manager::services::tests::{platform, serve_plugin, through_proxy, TOKEN};
    use crate::manager::ServicePluginState;
    use axum::response::IntoResponse;
    use temps_auth::permissions::Role;
    use tower::ServiceExt;

    async fn problem_of(problem: Problem) -> (StatusCode, serde_json::Value) {
        let response = problem.into_response();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .expect("problem body");
        (status, serde_json::from_slice(&body).expect("problem json"))
    }

    #[test]
    fn the_switch_routes_and_their_schemas_are_in_the_openapi_document() {
        let spec = serde_json::to_value(ExternalPluginsApiDoc::openapi()).expect("spec");
        let disable = &spec["paths"]["/x/plugins/{name}/disable"]["post"];
        for status in ["200", "400", "401", "403", "404", "500", "503"] {
            assert!(
                disable["responses"][status].is_object(),
                "disabling a plugin must document {status}"
            );
        }
        assert!(
            disable["responses"]["428"].is_null(),
            "switching a plugin off asks for no further verification"
        );
        let enable = &spec["paths"]["/x/plugins/{name}/enable"]["post"];
        for status in ["200", "400", "401", "403", "404", "428", "500", "503"] {
            assert!(
                enable["responses"][status].is_object(),
                "enabling a plugin must document {status}"
            );
        }
        let schemas = &spec["components"]["schemas"];
        for schema in ["PluginSwitchResponse", "PluginResourceLimitsResponse"] {
            assert!(schemas[schema].is_object(), "{schema}");
        }
        assert!(
            schemas["ServicePluginState"]["enum"]
                .as_array()
                .is_some_and(|states| states.contains(&serde_json::json!("disabled"))),
            "the disabled state is part of the contract"
        );
        let plugin = &schemas["ServicePluginResponse"]["properties"];
        for field in ["disabled_at", "disabled_by", "resource_limits"] {
            assert!(plugin[field].is_object(), "{field}");
        }
    }

    #[tokio::test]
    async fn switching_a_plugin_requires_a_caller() {
        for route in ["disable", "enable"] {
            let audit = Arc::new(RecordingAuditLogger::default());
            let response = configure_routes()
                .with_state(test_state_with_audit(audit.clone()))
                .oneshot(
                    axum::http::Request::builder()
                        .method("POST")
                        .uri(format!("/x/plugins/reports/{route}"))
                        .body(axum::body::Body::empty())
                        .expect("request"),
                )
                .await
                .expect("response");
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{route}");
            assert!(operations(&audit).is_empty(), "{route}");
        }
    }

    #[tokio::test]
    async fn only_a_system_administrator_switches_a_plugin_and_only_enabling_needs_verification() {
        let audit = Arc::new(RecordingAuditLogger::default());
        let state = test_state_with_audit(audit.clone());
        let member = disable_plugin(
            user_auth(Role::User),
            State(state.clone()),
            metadata(),
            Path("reports".to_string()),
        )
        .await
        .expect_err("a member cannot disable a plugin");
        assert_eq!(member.status_code, StatusCode::FORBIDDEN);
        let member = enable_plugin(
            user_auth(Role::User),
            State(state.clone()),
            metadata(),
            Path("reports".to_string()),
        )
        .await
        .expect_err("a member cannot enable a plugin");
        assert_eq!(member.status_code, StatusCode::FORBIDDEN);
        assert!(operations(&audit).is_empty(), "nothing was attempted");

        let mut unverified = state;
        unverified.sensitive_action_authorizer = Arc::new(RequireSensitiveVerification);
        let error = enable_plugin(
            user_auth(Role::PlatformAdmin),
            State(unverified.clone()),
            metadata(),
            Path("reports".to_string()),
        )
        .await
        .expect_err("enabling needs recent verification");
        assert_eq!(error.status_code, StatusCode::PRECONDITION_REQUIRED);
        assert!(operations(&audit).is_empty(), "nothing was attempted");

        // Switching off is never held up by verification: it goes straight
        // to looking for the plugin.
        let error = disable_plugin(
            user_auth(Role::PlatformAdmin),
            State(unverified.clone()),
            metadata(),
            Path("reports".to_string()),
        )
        .await
        .expect_err("no such plugin");
        let (status, problem) = problem_of(error).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(
            problem["detail"]
                .as_str()
                .is_some_and(|detail| detail.contains("'reports'")),
            "{problem}"
        );
        assert_eq!(operations(&audit), ["EXTERNAL_PLUGIN_DISABLE_FAILED"]);

        // A name no plugin can have is refused as such, on both.
        let error = disable_plugin(
            user_auth(Role::PlatformAdmin),
            State(unverified.clone()),
            metadata(),
            Path("Not A Name".to_string()),
        )
        .await
        .expect_err("an invalid name");
        assert_eq!(error.status_code, StatusCode::BAD_REQUEST);
        let error = enable_plugin(
            user_auth(Role::PlatformAdmin),
            State(unverified),
            metadata(),
            Path("Not A Name".to_string()),
        )
        .await
        .expect_err("an invalid name");
        assert_eq!(error.status_code, StatusCode::BAD_REQUEST);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_plugin_is_switched_off_in_one_call_and_listed_with_who_did_it_and_its_limits() {
        let Some(platform) = platform("switching a plugin off over the API").await else {
            return;
        };
        let plugin = serve_plugin("reports", TOKEN);
        let binding = platform.service("reports-service").await;
        let deployment_id = platform.deploy(binding, plugin.port).await;
        platform.limit(deployment_id, 512, 0).await;
        // The administrator the test signs in as.
        let administrator = platform.user(1).await;
        let audit = Arc::new(RecordingAuditLogger::default());
        let state = state_on(&platform, audit.clone()).await;
        let admin = || user_auth(Role::PlatformAdmin);
        let (status, _) =
            add_service_plugin(admin(), State(state.clone()), metadata(), add(binding))
                .await
                .expect("a deployed plugin service is added");
        assert_eq!(status, StatusCode::CREATED);

        // A running plugin is listed with the limits of its container.
        let Json(listed) = get_plugin_services(admin(), State(state.clone()))
            .await
            .expect("list");
        let running = &listed.plugins[0];
        assert_eq!(running.state, ServicePluginState::Running);
        assert_eq!((running.disabled_at, running.disabled_by), (None, None));
        let json = serde_json::to_value(running).expect("json");
        assert_eq!(
            json["resource_limits"],
            serde_json::json!({ "deployment_id": deployment_id, "memory_limit_mb": 512 }),
            "memory is capped and CPU, recorded as zero, is not"
        );

        let before = operations(&audit).len();
        let Json(switched) = disable_plugin(
            admin(),
            State(state.clone()),
            metadata(),
            Path("reports".to_string()),
        )
        .await
        .expect("the plugin is disabled");
        assert_eq!(switched.name, "reports");
        assert!(switched.disabled);
        assert!(
            switched.message.contains("Its service keeps running"),
            "{}",
            switched.message
        );
        assert_eq!(operations(&audit)[before..], ["EXTERNAL_PLUGIN_DISABLED"]);

        let Json(listed) = get_plugin_services(admin(), State(state.clone()))
            .await
            .expect("list");
        let disabled = &listed.plugins[0];
        assert_eq!(disabled.state, ServicePluginState::Disabled);
        assert!(disabled.disabled_at.is_some());
        assert_eq!(disabled.disabled_by, Some(administrator));
        assert!(
            disabled
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains("was disabled by an administrator")),
            "{:?}",
            disabled.detail
        );
        assert_eq!(
            disabled
                .resource_limits
                .as_ref()
                .map(|limits| limits.memory_limit_mb),
            Some(Some(512)),
            "the limits of the approved build stay visible while it is off"
        );
        let json = serde_json::to_value(disabled).expect("json");
        assert_eq!(json["state"], "disabled");
        let (status, body) =
            through_proxy(state.service.manager().as_ref(), "reports", "/items", true).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(body.contains("was disabled by an administrator"), "{body}");

        // Switching off twice changes nothing and is not an error.
        let Json(again) = disable_plugin(
            admin(),
            State(state.clone()),
            metadata(),
            Path("reports".to_string()),
        )
        .await
        .expect("already disabled");
        assert!(again.disabled);

        let before = operations(&audit).len();
        let Json(switched) = enable_plugin(
            admin(),
            State(state.clone()),
            metadata(),
            Path("reports".to_string()),
        )
        .await
        .expect("the plugin is enabled");
        assert!(!switched.disabled);
        assert_eq!(
            operations(&audit)[before..],
            [
                "EXTERNAL_PLUGIN_ENABLE_REQUESTED",
                "EXTERNAL_PLUGIN_ENABLED"
            ]
        );
        let Json(listed) = get_plugin_services(admin(), State(state.clone()))
            .await
            .expect("list");
        let enabled = &listed.plugins[0];
        assert_eq!(enabled.state, ServicePluginState::Running);
        assert_eq!((enabled.disabled_at, enabled.disabled_by), (None, None));
        let (status, body) =
            through_proxy(state.service.manager().as_ref(), "reports", "/items", true).await;
        assert_eq!((status, body.as_str()), (StatusCode::OK, "three items"));

        // A plugin that was never added cannot be enabled.
        let before = operations(&audit).len();
        let missing = enable_plugin(
            admin(),
            State(state.clone()),
            metadata(),
            Path("absent".to_string()),
        )
        .await
        .expect_err("no such plugin");
        let (status, problem) = problem_of(missing).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(
            problem["detail"]
                .as_str()
                .is_some_and(|detail| detail.contains("'absent'")),
            "{problem}"
        );
        assert_eq!(
            operations(&audit)[before..],
            [
                "EXTERNAL_PLUGIN_ENABLE_REQUESTED",
                "EXTERNAL_PLUGIN_ENABLE_FAILED"
            ]
        );
    }
}
