// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A plugin is pinned to the build an administrator approved. A new build
//! of its service waits here until an administrator approves exactly it.
use super::*;
use crate::service::PendingUpdateView;
use crate::updates::AccessDifference;

/// A build of a plugin's service that waits for an administrator. Temps is
/// not attached to it.
#[derive(Debug, Serialize, ToSchema)]
pub struct PendingPluginUpdateResponse {
    /// The deployment that waits.
    pub deployment_id: i32,
    /// What an approval must quote: this build answering this manifest.
    pub review: String,
    /// Since when it waits.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<chrono::DateTime<chrono::Utc>>,
    /// The image that waits, when the deployment was made from an image.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// The Docker image ID the image that waits is, when Temps recorded it:
    /// what a registry image resolved to, or what an uploaded image was
    /// imported as (which has no `image`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_id: Option<String>,
    /// What it asks for compared with the approved build.
    pub difference: AccessDifference,
}

impl From<PendingUpdateView> for PendingPluginUpdateResponse {
    fn from(view: PendingUpdateView) -> Self {
        Self {
            deployment_id: view.deployment_id,
            review: view.review,
            since: view.since,
            image: view.image,
            image_id: view.image_id,
            difference: view.difference,
        }
    }
}

/// The update an administrator reviewed and approves.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ApprovePluginUpdate {
    /// The `review` of the update that was shown. An update that changed
    /// since is not approved by it.
    pub review: String,
}

/// The build a plugin now runs as.
#[derive(Debug, Serialize, ToSchema)]
pub struct ApprovedPluginUpdateResponse {
    pub name: String,
    pub version: String,
    /// The deployment the plugin is pinned to now.
    pub deployment_id: i32,
    /// What the update asked for beyond the build it replaced. Nothing in it
    /// was allowed by the approval: new permissions stay off until chosen.
    pub difference: AccessDifference,
}

#[derive(Serialize)]
struct PluginUpdateAudit {
    context: temps_core::audit::AuditContext,
    #[serde(skip)]
    operation: &'static str,
    plugin: String,
    review: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_deployment_id: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    deployment_id: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    difference: Option<AccessDifference>,
    #[serde(skip_serializing_if = "Option::is_none")]
    failure: Option<String>,
}

impl temps_core::audit::AuditOperation for PluginUpdateAudit {
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

/// Approve the update of a plugin that waits, and attach to it.
///
/// An update is never applied without this call. Approving changes which
/// build runs as the plugin; it allows nothing the plugin newly asks for.
#[utoipa::path(
    tag = "External Plugins",
    post,
    path = "/x/plugins/{name}/update/approve",
    operation_id = "approve_plugin_update",
    params(("name" = String, Path, description = "The plugin's name")),
    request_body = ApprovePluginUpdate,
    responses(
        (status = 200, description = "The plugin now runs as the approved build", body = ApprovedPluginUpdateResponse),
        (status = 400, description = "Invalid request body or plugin name, or the build is not a plugin Temps accepts", body = temps_core::ProblemDetails),
        (status = 401, description = "Unauthorized", body = temps_core::ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = temps_core::ProblemDetails),
        (status = 404, description = "No plugin with that name was added", body = temps_core::ProblemDetails),
        (status = 409, description = "No update waits, or what waits is not what was reviewed", body = temps_core::ProblemDetails),
        (status = 415, description = "Request body is not JSON", body = temps_core::ProblemDetails),
        (status = 422, description = "Request body does not name the review", body = temps_core::ProblemDetails),
        (status = 428, description = "Recent sensitive-action verification required", body = temps_core::ProblemDetails),
        (status = 500, description = "The approved build could not be read or recorded", body = temps_core::ProblemDetails),
        (status = 502, description = "The service did not answer as a plugin", body = temps_core::ProblemDetails),
        (status = 503, description = "Audit unavailable, or the plugin host is shutting down", body = temps_core::ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
pub(super) async fn approve_plugin_update(
    RequireAuth(auth): RequireAuth,
    State(state): State<ExternalPluginsAppState>,
    Extension(metadata): Extension<temps_core::RequestMetadata>,
    Path(name): Path<String>,
    request: Result<Json<ApprovePluginUpdate>, JsonRejection>,
) -> Result<Json<ApprovedPluginUpdateResponse>, Problem> {
    permission_guard!(auth, SystemAdmin);
    let Json(request) = request.map_err(|rejection| {
        temps_core::problemdetails::new(rejection.status())
            .with_title("Invalid Plugin Update Approval")
            .with_detail(
                "Approving a plugin update requires an application/json body with the review \
                 of the update that was shown",
            )
    })?;
    // Approving lets new code run with the plugin's permissions.
    temps_auth::require_sensitive_action(
        state.sensitive_action_authorizer.as_ref(),
        &auth,
        temps_core::SensitiveAction::InstallExternalPlugin { name: name.clone() },
    )
    .await?;
    let context = audit_context(&auth, &metadata);
    let audit = |operation: &'static str| PluginUpdateAudit {
        context: context.clone(),
        operation,
        plugin: name.clone(),
        review: request.review.clone(),
        previous_deployment_id: None,
        deployment_id: None,
        difference: None,
        failure: None,
    };
    // On record before Temps connects to the new build.
    record_required_audit(&state, &audit("EXTERNAL_PLUGIN_UPDATE_APPROVE_REQUESTED")).await?;
    let approved = match state
        .service
        .approve_plugin_update(&name, &request.review)
        .await
    {
        Ok(approved) => approved,
        Err(error) => {
            record_audit(
                &state,
                &PluginUpdateAudit {
                    failure: Some(public_error_detail(&error)),
                    ..audit("EXTERNAL_PLUGIN_UPDATE_APPROVE_FAILED")
                },
            )
            .await;
            return Err(service_problem(&error));
        }
    };
    record_audit(
        &state,
        &PluginUpdateAudit {
            previous_deployment_id: approved.previous_deployment_id,
            deployment_id: Some(approved.deployment_id),
            difference: Some(approved.difference.clone()),
            ..audit("EXTERNAL_PLUGIN_UPDATE_APPROVED")
        },
    )
    .await;
    Ok(Json(ApprovedPluginUpdateResponse {
        name: approved.manifest.name,
        version: approved.manifest.version,
        deployment_id: approved.deployment_id,
        difference: approved.difference,
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
    use crate::manager::services::tests::{
        eventually, platform, serve_plugin, serve_plugin_asking, Platform, TOKEN,
    };
    use crate::manager::ServicePluginState;
    use axum::response::IntoResponse;
    use sea_orm::ConnectionTrait;
    use temps_auth::permissions::Role;
    use tower::ServiceExt;

    fn approve(review: &str) -> Result<Json<ApprovePluginUpdate>, JsonRejection> {
        Ok(Json(ApprovePluginUpdate {
            review: review.to_string(),
        }))
    }

    async fn problem_of(problem: Problem) -> (StatusCode, serde_json::Value) {
        let response = problem.into_response();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .expect("problem body");
        (status, serde_json::from_slice(&body).expect("problem json"))
    }

    #[test]
    fn the_approval_route_and_its_schemas_are_in_the_openapi_document() {
        let spec = serde_json::to_value(ExternalPluginsApiDoc::openapi()).expect("spec");
        let route = &spec["paths"]["/x/plugins/{name}/update/approve"]["post"];
        for status in [
            "200", "400", "401", "403", "404", "409", "415", "422", "428", "500", "502", "503",
        ] {
            assert!(
                route["responses"][status].is_object(),
                "approving an update must document {status}"
            );
        }
        let schemas = &spec["components"]["schemas"];
        for schema in [
            "PendingPluginUpdateResponse",
            "ApprovePluginUpdate",
            "ApprovedPluginUpdateResponse",
            "AccessDifference",
        ] {
            assert!(schemas[schema].is_object(), "{schema}");
        }
        assert!(
            schemas["ServicePluginState"]["enum"]
                .as_array()
                .is_some_and(|states| states.contains(&serde_json::json!("update_waiting"))),
            "the waiting state is part of the contract"
        );
    }

    #[tokio::test]
    async fn approving_an_update_requires_a_caller() {
        let audit = Arc::new(RecordingAuditLogger::default());
        let response = configure_routes()
            .with_state(test_state_with_audit(audit.clone()))
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/x/plugins/reports/update/approve")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(r#"{"review":"abc"}"#))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(operations(&audit).is_empty());
    }

    #[tokio::test]
    async fn only_a_system_administrator_with_recent_verification_approves_an_update() {
        let audit = Arc::new(RecordingAuditLogger::default());
        let state = test_state_with_audit(audit.clone());
        let member = approve_plugin_update(
            user_auth(Role::User),
            State(state.clone()),
            metadata(),
            Path("reports".to_string()),
            approve("abc"),
        )
        .await
        .expect_err("a member cannot approve a plugin update");
        assert_eq!(member.status_code, StatusCode::FORBIDDEN);

        let mut unverified = state;
        unverified.sensitive_action_authorizer = Arc::new(RequireSensitiveVerification);
        let error = approve_plugin_update(
            user_auth(Role::PlatformAdmin),
            State(unverified),
            metadata(),
            Path("reports".to_string()),
            approve("abc"),
        )
        .await
        .expect_err("verification is required");
        assert_eq!(error.status_code, StatusCode::PRECONDITION_REQUIRED);
        assert!(operations(&audit).is_empty(), "nothing was attempted");
    }

    #[tokio::test]
    async fn an_approval_that_names_no_review_is_explained_as_problem_details() {
        let audit = Arc::new(RecordingAuditLogger::default());
        let mut request = axum::http::Request::builder()
            .method("POST")
            .uri("/x/plugins/reports/update/approve")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(r#"{"deployment":7}"#))
            .expect("request");
        request
            .extensions_mut()
            .insert(user_auth(Role::PlatformAdmin).0);
        request.extensions_mut().insert(metadata().0);
        let response = configure_routes()
            .with_state(test_state_with_audit(audit.clone()))
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
        assert_eq!(problem["title"], "Invalid Plugin Update Approval");
        assert!(operations(&audit).is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_update_is_listed_with_what_it_asks_for_and_runs_only_once_approved() {
        let Some(platform) = platform("approving a plugin update over the API").await else {
            return;
        };
        let first = serve_plugin("reports", TOKEN);
        let binding = platform.service("reports-service").await;
        let first_deployment = platform.deploy(binding, first.port).await;
        let audit = Arc::new(RecordingAuditLogger::default());
        let state = state_on(&platform, audit.clone()).await;
        let admin = || user_auth(Role::PlatformAdmin);
        let (status, _) =
            add_service_plugin(admin(), State(state.clone()), metadata(), add(binding))
                .await
                .expect("a deployed plugin service is added");
        assert_eq!(status, StatusCode::CREATED);

        // Nothing waits for a plugin that runs its approved build.
        let Json(listed) = get_plugin_services(admin(), State(state.clone()))
            .await
            .expect("list");
        assert_eq!(
            listed.plugins[0].pinned_deployment_id,
            Some(first_deployment)
        );
        assert!(listed.plugins[0].pending_update.is_none());
        let none = approve_plugin_update(
            admin(),
            State(state.clone()),
            metadata(),
            Path("reports".to_string()),
            approve("abc"),
        )
        .await
        .expect_err("no update waits");
        let (status, problem) = problem_of(none).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(problem["title"], "No Plugin Update Waits");

        // A plugin that was never added.
        let missing = approve_plugin_update(
            admin(),
            State(state.clone()),
            metadata(),
            Path("absent".to_string()),
            approve("abc"),
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

        // The service is redeployed with a build that asks for more.
        let second = serve_plugin_asking("reports", TOKEN, vec![PluginHostPermission::AiGenerate]);
        let second_deployment = platform.deploy(binding, second.port).await;
        first.stop();
        eventually("the update is listed as waiting", || async {
            get_plugin_services(admin(), State(state.clone()))
                .await
                .is_ok_and(|Json(listed)| listed.plugins[0].pending_update.is_some())
        })
        .await;
        let Json(listed) = get_plugin_services(admin(), State(state.clone()))
            .await
            .expect("list");
        let plugin = &listed.plugins[0];
        assert_eq!(plugin.state, ServicePluginState::UpdateWaiting);
        assert_eq!(plugin.pinned_deployment_id, Some(first_deployment));
        let pending = plugin.pending_update.as_ref().expect("an update waits");
        assert_eq!(pending.deployment_id, second_deployment);
        assert!(pending.since.is_some());
        assert!(pending.difference.asks_for_more);
        assert_eq!(
            pending.difference.added_permissions,
            [PluginHostPermission::AiGenerate]
        );
        let json = serde_json::to_value(plugin).expect("json");
        assert_eq!(json["state"], "update_waiting");
        assert_eq!(
            json["pending_update"]["difference"]["added_permissions"],
            serde_json::json!(["ai_generate"])
        );
        let review = pending.review.clone();

        // A review that is not the one waiting approves nothing.
        let stale = approve_plugin_update(
            admin(),
            State(state.clone()),
            metadata(),
            Path("reports".to_string()),
            approve("a-review-of-something-else"),
        )
        .await
        .expect_err("another review");
        let (status, problem) = problem_of(stale).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(problem["title"], "Plugin Update Changed");
        assert!(
            problem["detail"]
                .as_str()
                .is_some_and(|detail| detail.contains(&format!("deployment {second_deployment}"))),
            "{problem}"
        );

        let before = operations(&audit).len();
        let Json(approved) = approve_plugin_update(
            admin(),
            State(state.clone()),
            metadata(),
            Path("reports".to_string()),
            approve(&review),
        )
        .await
        .expect("the reviewed update is approved");
        assert_eq!(approved.name, "reports");
        assert_eq!(approved.deployment_id, second_deployment);
        assert!(approved.difference.asks_for_more);
        assert_eq!(
            operations(&audit)[before..],
            [
                "EXTERNAL_PLUGIN_UPDATE_APPROVE_REQUESTED",
                "EXTERNAL_PLUGIN_UPDATE_APPROVED"
            ]
        );
        assert!(operations(&audit).contains(&"EXTERNAL_PLUGIN_UPDATE_APPROVE_FAILED".to_string()));

        let Json(listed) = get_plugin_services(admin(), State(state.clone()))
            .await
            .expect("list");
        let plugin = &listed.plugins[0];
        assert_eq!(plugin.state, ServicePluginState::Running);
        assert_eq!(plugin.deployment_id, Some(second_deployment));
        assert_eq!(plugin.pinned_deployment_id, Some(second_deployment));
        assert!(plugin.pending_update.is_none());
        // What the update newly asks for is requested, not allowed.
        let Json(grants) = super::super::get_plugin_grants(
            admin(),
            State(state.clone()),
            Path("reports".to_string()),
        )
        .await
        .expect("grants");
        assert!(grants
            .requested_permissions
            .contains(&PluginHostPermission::AiGenerate));
        assert!(grants.permissions.is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn approving_an_update_after_a_rollback_to_the_approved_build_approves_nothing() {
        let Some(platform) = platform("approving an update that was rolled back").await else {
            return;
        };
        let first = serve_plugin("reports", TOKEN);
        let binding = platform.service("reports-service").await;
        let first_deployment = platform.deploy(binding, first.port).await;
        let audit = Arc::new(RecordingAuditLogger::default());
        let state = state_on(&platform, audit.clone()).await;
        let admin = || user_auth(Role::PlatformAdmin);
        let _added = add_service_plugin(admin(), State(state.clone()), metadata(), add(binding))
            .await
            .expect("a deployed plugin service is added");

        let second = serve_plugin("reports", TOKEN);
        platform.deploy(binding, second.port).await;
        first.stop();
        eventually("the update is listed as waiting", || async {
            get_plugin_services(admin(), State(state.clone()))
                .await
                .is_ok_and(|Json(listed)| listed.plugins[0].pending_update.is_some())
        })
        .await;
        let Json(listed) = get_plugin_services(admin(), State(state.clone()))
            .await
            .expect("list");
        let review = listed.plugins[0]
            .pending_update
            .as_ref()
            .expect("an update waits")
            .review
            .clone();

        // The service goes back to the approved deployment before anyone
        // approves the update.
        let again = serve_plugin("reports", TOKEN);
        platform
            .db()
            .execute_unprepared(&format!(
                "UPDATE deployment_containers SET host_port={} WHERE deployment_id={first_deployment}; \
                 UPDATE environments SET current_deployment_id={first_deployment} WHERE id={}",
                again.port, binding.environment_id
            ))
            .await
            .expect("roll back");

        let refused = approve_plugin_update(
            admin(),
            State(state.clone()),
            metadata(),
            Path("reports".to_string()),
            approve(&review),
        )
        .await
        .expect_err("the update that was reviewed no longer runs");
        let (status, problem) = problem_of(refused).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(problem["title"], "No Plugin Update Waits");
        assert!(!operations(&audit).contains(&"EXTERNAL_PLUGIN_UPDATE_APPROVED".to_string()));

        let Json(listed) = get_plugin_services(admin(), State(state.clone()))
            .await
            .expect("list");
        assert_eq!(
            listed.plugins[0].pinned_deployment_id,
            Some(first_deployment)
        );
        assert!(listed.plugins[0].pending_update.is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_plugin_deployed_from_an_image_is_listed_with_the_image_that_runs_and_the_one_that_waits(
    ) {
        let Some(platform) = platform("listing the image of a plugin").await else {
            return;
        };
        let first = serve_plugin("reports", TOKEN);
        let binding = platform.service("reports-service").await;
        let first_deployment = platform
            .deploy_image(
                binding,
                first.port,
                "registry.example.com/acme/reports:1.0.0",
            )
            .await;
        let audit = Arc::new(RecordingAuditLogger::default());
        let state = state_on(&platform, audit.clone()).await;
        let admin = || user_auth(Role::PlatformAdmin);
        let (status, _) =
            add_service_plugin(admin(), State(state.clone()), metadata(), add(binding))
                .await
                .expect("a service deployed from an image is added");
        assert_eq!(status, StatusCode::CREATED);

        let Json(listed) = get_plugin_services(admin(), State(state.clone()))
            .await
            .expect("list");
        assert_eq!(
            listed.plugins[0].image.as_deref(),
            Some("registry.example.com/acme/reports:1.0.0")
        );
        assert_eq!(
            listed.plugins[0].image_id,
            Some(Platform::image_id_of(first_deployment)),
            "the content the image resolved to is what was approved"
        );

        let second = serve_plugin("reports", TOKEN);
        let second_deployment = platform
            .deploy_image(
                binding,
                second.port,
                "registry.example.com/acme/reports:1.1.0",
            )
            .await;
        first.stop();
        eventually("the new image is listed as waiting", || async {
            get_plugin_services(admin(), State(state.clone()))
                .await
                .is_ok_and(|Json(listed)| listed.plugins[0].pending_update.is_some())
        })
        .await;
        let Json(listed) = get_plugin_services(admin(), State(state.clone()))
            .await
            .expect("list");
        let plugin = &listed.plugins[0];
        assert_eq!(
            plugin.image.as_deref(),
            Some("registry.example.com/acme/reports:1.0.0"),
            "the approved image stays the plugin's image while another waits"
        );
        let pending = plugin.pending_update.as_ref().expect("an update waits");
        assert_eq!(pending.deployment_id, second_deployment);
        assert_eq!(
            pending.image.as_deref(),
            Some("registry.example.com/acme/reports:1.1.0")
        );
        assert_eq!(
            pending.image_id,
            Some(Platform::image_id_of(second_deployment))
        );
        assert_eq!(
            plugin.image_id,
            Some(Platform::image_id_of(first_deployment)),
            "the approved content stays the plugin's while other content waits"
        );
        let json = serde_json::to_value(plugin).expect("json");
        assert_eq!(json["image"], "registry.example.com/acme/reports:1.0.0");
        assert_eq!(
            json["pending_update"]["image"],
            "registry.example.com/acme/reports:1.1.0"
        );

        // A service built from a repository has no image to name.
        let built = serve_plugin("audit-export", TOKEN);
        let other = platform.service("audit-export-service").await;
        platform.deploy(other, built.port).await;
        let (status, _) = add_service_plugin(admin(), State(state.clone()), metadata(), add(other))
            .await
            .expect("a built service is added");
        assert_eq!(status, StatusCode::CREATED);
        let Json(listed) = get_plugin_services(admin(), State(state.clone()))
            .await
            .expect("list");
        let built = listed
            .plugins
            .iter()
            .find(|plugin| plugin.name == "audit-export")
            .expect("listed");
        assert!(built.image.is_none());
        assert!(built.image_id.is_none());

        // An image uploaded from a client has no reference anybody named:
        // it is listed by its content alone.
        let uploaded = serve_plugin("inventory", TOKEN);
        let third = platform.service("inventory-service").await;
        let upload = platform.deploy_upload(third, uploaded.port).await;
        let (status, _) = add_service_plugin(admin(), State(state.clone()), metadata(), add(third))
            .await
            .expect("a service deployed from an upload is added");
        assert_eq!(status, StatusCode::CREATED);
        let Json(listed) = get_plugin_services(admin(), State(state.clone()))
            .await
            .expect("list");
        let uploaded = listed
            .plugins
            .iter()
            .find(|plugin| plugin.name == "inventory")
            .expect("listed");
        assert!(
            uploaded.image.is_none(),
            "Temps's own name for it is not shown"
        );
        assert_eq!(uploaded.image_id, Some(Platform::image_id_of(upload)));
        assert!(serde_json::to_value(built)
            .expect("json")
            .get("image")
            .is_none());
    }
}
