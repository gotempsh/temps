// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! HTTP handlers for external plugin management endpoints.

mod ai_tools;
mod services;
mod updates;
mod watches;
use ai_tools::{get_plugin_ai_tools, put_plugin_ai_tools};

use std::sync::Arc;

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde::{Deserialize, Serialize};
use temps_auth::{permission_guard, RequireAuth};
use temps_core::problemdetails::Problem;
use temps_plugin_protocol::{NavEntry, NavSection, PluginManifest, UiManifest, UiRoute};
use utoipa::{OpenApi as OpenApiTrait, ToSchema};

use crate::service::ExternalPluginsError;
use crate::service::ExternalPluginsService;
use temps_plugin_protocol::channel::{PluginActorInfo, PluginAiCapability, PluginHostPermission};

fn grant_problem(error: crate::grants::GrantError) -> Problem {
    match error {
        crate::grants::GrantError::AiToolsPublication { .. } => {
            tracing::error!(error = %error, "Saved plugin AI consent could not be published");
            temps_core::problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Plugin AI Tools Unavailable")
                .with_detail("Consent was saved, but AI tools could not be refreshed and remain unavailable. Retry saving this setting or reload plugins after the database recovers.")
        }
        crate::grants::GrantError::NotFound { .. } => {
            temps_core::problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Plugin Actor Not Found")
                .with_detail(error.to_string())
        }
        crate::grants::GrantError::Invalid { .. } => {
            temps_core::problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Invalid Plugin Grants")
                .with_detail(error.to_string())
        }
        crate::grants::GrantError::ActorChanged { .. } => {
            temps_core::problemdetails::new(StatusCode::CONFLICT)
                .with_title("Plugin Actor Changed")
                .with_detail(error.to_string())
        }
        crate::grants::GrantError::Database { .. } => {
            tracing::error!(error = %error, "Plugin grant database operation failed");
            temps_core::problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Plugin Grants Unavailable")
                .with_detail("Plugin grants could not be read or saved. Please try again later.")
        }
    }
}

/// Everything a plugin asks for and everything it was given, in one place:
/// what an administrator reads before granting anything.
#[derive(Debug, Serialize, ToSchema)]
pub struct PluginGrantsResponse {
    pub actor: PluginActorInfo,
    pub requested_permissions: Vec<PluginHostPermission>,
    pub permissions: Vec<PluginHostPermission>,
    pub ai: PluginAiCapability,
    /// What a plugin granted `api_write` may change. Empty: nothing.
    pub write_areas: Vec<crate::scope::PluginWriteArea>,
    /// The only projects those changes may touch. Absent: every project.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub write_project_ids: Option<Vec<i32>>,
    /// Routes of the plugin that anyone can open without signing in.
    pub public_paths: Vec<String>,
    /// The platform events the plugin asks to receive.
    pub events: Vec<String>,
    /// What the plugin's container may connect to.
    pub outbound_network: PluginOutboundNetwork,
}

/// What a plugin's container may connect to on the network.
#[derive(Debug, Serialize, ToSchema, PartialEq, Eq)]
pub struct PluginOutboundNetwork {
    /// `open`, `restricted` or `none`.
    pub profile: String,
    /// What that profile means, in words.
    pub detail: String,
    /// Where an administrator changes it.
    pub settings_path: String,
}

impl PluginOutboundNetwork {
    /// The profile of the environment the plugin runs in; an environment
    /// that sets none keeps the default for deployments, which is open.
    fn of(stored: Option<String>) -> Self {
        let profile = match stored.as_deref() {
            Some("restricted") => "restricted",
            Some("none") => "none",
            _ => "open",
        };
        let detail = match profile {
            "restricted" => "The plugin can only connect to the destinations its service's network policy allows.",
            "none" => "The plugin cannot connect to anything outside Temps.",
            _ => "The plugin can connect to anything its server can reach, including the internet.",
        };
        Self {
            profile: profile.to_string(),
            detail: detail.to_string(),
            settings_path: "/settings/network-policy".to_string(),
        }
    }
}

#[derive(Debug, Serialize)]
struct PluginGrantChangeAudit {
    context: temps_core::audit::AuditContext,
    target_plugin_actor: PluginActorInfo,
    old_config: crate::grants::PluginGrantConfig,
    new_config: crate::grants::PluginGrantConfig,
}

impl temps_core::audit::AuditOperation for PluginGrantChangeAudit {
    fn operation_type(&self) -> String {
        "EXTERNAL_PLUGIN_GRANTS_CHANGE_REQUESTED".to_string()
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

/// What a plugin asks for and what an administrator gave it.
#[utoipa::path(
    tag = "External Plugins",
    get,
    path = "/x/plugins/{name}/grants",
    params(("name" = String, Path, description = "The plugin's name")),
    responses(
        (status = 200, description = "What the plugin asks for and what it was given", body = PluginGrantsResponse),
        (status = 401, description = "Unauthorized", body = temps_core::ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = temps_core::ProblemDetails),
        (status = 404, description = "No plugin with that name was added", body = temps_core::ProblemDetails),
        (status = 500, description = "The plugin's permissions could not be read", body = temps_core::ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
async fn get_plugin_grants(
    RequireAuth(auth): RequireAuth,
    State(state): State<ExternalPluginsAppState>,
    Path(name): Path<String>,
) -> Result<Json<PluginGrantsResponse>, Problem> {
    permission_guard!(auth, SystemAdmin);
    plugin_grants_response(&state, &name).await.map(Json)
}

/// Choose what a plugin may do, and how far a permission to change reaches.
#[utoipa::path(
    tag = "External Plugins",
    put,
    path = "/x/plugins/{name}/grants",
    params(("name" = String, Path, description = "The plugin's name")),
    request_body = crate::grants::PluginGrantConfig,
    responses(
        (status = 200, description = "The permissions now in effect", body = PluginGrantsResponse),
        (status = 400, description = "A permission the plugin did not ask for, or a scope that is not valid", body = temps_core::ProblemDetails),
        (status = 401, description = "Unauthorized", body = temps_core::ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = temps_core::ProblemDetails),
        (status = 404, description = "The plugin is not running or was never added", body = temps_core::ProblemDetails),
        (status = 409, description = "The plugin was replaced while its permissions were being changed", body = temps_core::ProblemDetails),
        (status = 428, description = "Recent verification is required", body = temps_core::ProblemDetails),
        (status = 500, description = "The plugin's permissions could not be saved", body = temps_core::ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
async fn put_plugin_grants(
    RequireAuth(auth): RequireAuth,
    State(state): State<ExternalPluginsAppState>,
    Extension(metadata): Extension<temps_core::RequestMetadata>,
    Path(name): Path<String>,
    Json(config): Json<crate::grants::PluginGrantConfig>,
) -> Result<Json<PluginGrantsResponse>, Problem> {
    permission_guard!(auth, SystemAdmin);
    temps_auth::require_sensitive_action(
        state.sensitive_action_authorizer.as_ref(),
        &auth,
        temps_core::SensitiveAction::ChangeExternalPluginGrants { name: name.clone() },
    )
    .await?;
    let manifests = state.service.manifests().await;
    let requested = manifests
        .iter()
        .find(|manifest| manifest.name == name)
        .map(|manifest| &manifest.host_permissions)
        .ok_or_else(|| {
            temps_core::problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Plugin Not Found")
                .with_detail(format!("Plugin '{name}' is not running"))
        })?;
    if config
        .permissions
        .iter()
        .any(|permission| !requested.contains(permission))
    {
        return Err(temps_core::problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Undeclared Plugin Permission")
            .with_detail(
                "A plugin can only receive permissions declared by its authenticated manifest",
            ));
    }
    let old = state
        .service
        .plugin_grants(&name)
        .await
        .map_err(grant_problem)?;
    let actor_id = old.actor.id.clone();
    record_required_audit(
        &state,
        &PluginGrantChangeAudit {
            context: audit_context(&auth, &metadata),
            target_plugin_actor: old.actor,
            old_config: old.config,
            new_config: config.clone(),
        },
    )
    .await?;
    state
        .service
        .update_plugin_grants_for_actor(&name, &actor_id, config)
        .await
        .map_err(grant_problem)?;
    record_audit(
        &state,
        &ExternalPluginWriteAudit {
            context: audit_context(&auth, &metadata),
            operation: "EXTERNAL_PLUGIN_GRANTS_CHANGED".into(),
            plugin_name: Some(name.clone()),
            version: None,
            platform: None,
            sha256: None,
            signer_key_id: None,
            registry_source: None,
            failure: None,
        },
    )
    .await;
    plugin_grants_response(&state, &name).await.map(Json)
}

async fn plugin_grants_response(
    state: &ExternalPluginsAppState,
    name: &str,
) -> Result<PluginGrantsResponse, Problem> {
    let grants = state
        .service
        .plugin_grants(name)
        .await
        .map_err(grant_problem)?;
    let manifest = state
        .service
        .manifests()
        .await
        .into_iter()
        .find(|manifest| manifest.name == name);
    let (requested_permissions, public_paths, events) = manifest
        .map(|manifest| {
            (
                manifest.host_permissions,
                manifest.public_paths,
                manifest.events,
            )
        })
        .unwrap_or_default();
    let outbound = state
        .service
        .plugin_outbound_profile(name)
        .await
        .map_err(|error| service_problem(&error))?;
    let configured = state.service.manager().ai_available().await;
    Ok(PluginGrantsResponse {
        actor: grants.actor,
        requested_permissions,
        write_areas: grants.config.write_areas,
        write_project_ids: grants.config.write_project_ids,
        public_paths,
        events,
        outbound_network: PluginOutboundNetwork::of(outbound),
        permissions: grants.config.permissions,
        ai: PluginAiCapability {
            configured,
            reason: (!configured).then(|| "No host AI provider is configured".to_string()),
            setup_path: "/settings/ai-providers".into(),
            daily_call_limit: grants.config.ai_daily_call_limit,
            max_output_tokens: grants.config.ai_max_output_tokens,
            max_prompt_bytes: crate::grants::MAX_PROMPT_BYTES,
        },
    })
}

#[derive(Debug, Clone, Serialize)]
struct ExternalPluginWriteAudit {
    context: temps_core::audit::AuditContext,
    operation: String,
    plugin_name: Option<String>,
    version: Option<String>,
    platform: Option<String>,
    sha256: Option<String>,
    signer_key_id: Option<String>,
    registry_source: Option<String>,
    failure: Option<String>,
}

impl temps_core::audit::AuditOperation for ExternalPluginWriteAudit {
    fn operation_type(&self) -> String {
        self.operation.clone()
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

/// Handler state for the external plugins API.
#[derive(Clone)]
pub struct ExternalPluginsAppState {
    pub service: Arc<ExternalPluginsService>,
    pub audit_service: Arc<dyn temps_core::AuditLogger>,
    pub sensitive_action_authorizer: Arc<dyn temps_core::SensitiveActionAuthorizer>,
}

fn audit_context(
    auth: &temps_auth::AuthContext,
    metadata: &temps_core::RequestMetadata,
) -> temps_core::audit::AuditContext {
    temps_core::audit::AuditContext {
        user_id: auth.user_id(),
        ip_address: Some(metadata.ip_address.clone()),
        user_agent: metadata.user_agent.clone(),
    }
}

async fn record_audit(
    state: &ExternalPluginsAppState,
    operation: &dyn temps_core::audit::AuditOperation,
) {
    if state
        .audit_service
        .create_audit_log(operation)
        .await
        .is_err()
    {
        tracing::error!(
            operation = %operation.operation_type(),
            "Failed to record external-plugin write audit"
        );
    }
}

async fn record_required_audit(
    state: &ExternalPluginsAppState,
    operation: &dyn temps_core::audit::AuditOperation,
) -> Result<(), Problem> {
    state
        .audit_service
        .create_audit_log(operation)
        .await
        .map_err(|_error| {
            tracing::error!(
                operation = %operation.operation_type(),
                "Required external-plugin security audit could not be recorded"
            );
            temps_core::problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
                .with_title("Plugin Installation Audit Unavailable")
                .with_detail(
                    "Plugin installation cannot continue until the security audit log is available",
                )
        })
}

/// List all running external plugins and their manifests.
///
/// Requires only a valid session/token (no specific permission) since the
/// manifest drives sidebar navigation rendering for every authenticated
/// user, not just admins.
#[utoipa::path(
    tag = "External Plugins",
    get,
    path = "/x/plugins",
    responses(
        (status = 200, description = "List of all running external plugins", body = Vec<PluginManifest>),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = []))
)]
async fn list_external_plugins(
    RequireAuth(_auth): RequireAuth,
    State(state): State<ExternalPluginsAppState>,
) -> Json<Vec<PluginManifest>> {
    Json(state.service.manifests().await)
}

/// Response from the reload endpoint.
#[derive(Debug, Serialize, ToSchema)]
pub struct ReloadResponse {
    /// Number of plugins successfully loaded after reload
    pub loaded: usize,
    /// Names of loaded plugins
    pub plugins: Vec<String>,
    /// Activated installs that could not be verified or started.
    pub failures: Vec<ReloadFailureResponse>,
    /// Human-readable status message
    pub message: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ReloadFailureResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plugin: Option<String>,
    pub reason: String,
}

/// Reload all external plugins.
///
/// Stops all running plugin processes, re-scans the plugins directory,
/// starts any discovered binaries, and hot-swaps the proxy router so new
/// and removed plugins take effect immediately without a server restart.
///
/// Requires `SystemAdmin` permission.
#[utoipa::path(
    tag = "External Plugins",
    post,
    path = "/x/plugins/reload",
    responses(
        (status = 200, description = "All plugins reloaded successfully", body = ReloadResponse),
        (status = 207, description = "Some plugins reloaded and some failed", body = ReloadResponse),
        (status = 502, description = "No activated plugin could be reloaded", body = ReloadResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
    ),
    security(("bearer_auth" = []))
)]
async fn reload_plugins(
    RequireAuth(auth): RequireAuth,
    State(state): State<ExternalPluginsAppState>,
    Extension(metadata): Extension<temps_core::RequestMetadata>,
) -> Result<(StatusCode, Json<ReloadResponse>), Problem> {
    permission_guard!(auth, SystemAdmin);

    tracing::info!("Admin triggered plugin reload");

    let result = state
        .service
        .reload_plugins()
        .await
        .map_err(|error| service_problem(&error))?;
    let names: Vec<String> = result.manifests.iter().map(|m| m.name.clone()).collect();
    let count = names.len();
    let failures: Vec<ReloadFailureResponse> = result
        .failures
        .iter()
        .map(|failure| ReloadFailureResponse {
            plugin: failure.plugin.clone(),
            reason: failure.reason.clone(),
        })
        .collect();
    let status = if failures.is_empty() {
        StatusCode::OK
    } else if count == 0 {
        StatusCode::BAD_GATEWAY
    } else {
        StatusCode::MULTI_STATUS
    };
    let operation = if failures.is_empty() {
        "EXTERNAL_PLUGINS_RELOADED"
    } else if count == 0 {
        "EXTERNAL_PLUGINS_RELOAD_FAILED"
    } else {
        "EXTERNAL_PLUGINS_RELOAD_PARTIAL"
    };
    let failure_detail = (!failures.is_empty()).then(|| {
        failures
            .iter()
            .map(|failure| failure.reason.as_str())
            .collect::<Vec<_>>()
            .join("; ")
    });

    record_audit(
        &state,
        &ExternalPluginWriteAudit {
            context: audit_context(&auth, &metadata),
            operation: operation.to_string(),
            plugin_name: None,
            version: None,
            platform: None,
            sha256: None,
            signer_key_id: None,
            registry_source: None,
            failure: failure_detail,
        },
    )
    .await;

    Ok((
        status,
        Json(ReloadResponse {
            loaded: count,
            plugins: names,
            message: if failures.is_empty() {
                format!("Reload complete. {count} plugin(s) loaded.")
            } else {
                format!(
                    "Reload complete with failures. {count} plugin(s) loaded; {} failed.",
                    failures.len()
                )
            },
            failures,
        }),
    ))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct RepositoryCatalogResponse {
    pub available: bool,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub plugins: Vec<crate::source_catalog::RepositoryCatalogPlugin>,
}

#[utoipa::path(
    tag = "External Plugins",
    get,
    path = "/x/plugins/catalog/repositories",
    operation_id = "list_repository_plugin_catalog",
    responses((status = 200, body = RepositoryCatalogResponse)),
    security(("bearer_auth" = []))
)]
async fn list_repository_plugin_catalog(
    RequireAuth(_auth): RequireAuth,
    State(state): State<ExternalPluginsAppState>,
) -> Json<RepositoryCatalogResponse> {
    let source = crate::source_catalog::SOURCE.to_string();
    match state.service.repository_catalog().await {
        Ok(plugins) => Json(RepositoryCatalogResponse {
            available: true,
            source,
            reason: None,
            plugins,
        }),
        Err(error) => {
            tracing::warn!(error = %error, "GitHub source plugin catalog unavailable");
            Json(RepositoryCatalogResponse { available: false, source, reason: Some("GitHub plugin catalog is unavailable. Check outbound access to raw.githubusercontent.com and try again.".into()), plugins: Vec::new() })
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PluginStatusResponse {
    pub configured: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub setup_path: String,
}

fn service_problem(error: &ExternalPluginsError) -> Problem {
    let (status, title) = match error {
        ExternalPluginsError::InvalidPluginName { .. } => {
            (StatusCode::BAD_REQUEST, "Invalid Plugin Name")
        }
        ExternalPluginsError::ShuttingDown => (
            StatusCode::SERVICE_UNAVAILABLE,
            "Plugin Service Is Shutting Down",
        ),
        ExternalPluginsError::NotInstalled { .. } => {
            (StatusCode::NOT_FOUND, "Plugin Not Installed")
        }
        ExternalPluginsError::RetiredRecord { .. } => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Plugin Could Not Be Removed",
        ),
        ExternalPluginsError::Grant(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "Plugin Permissions Could Not Be Revoked",
        ),
        ExternalPluginsError::ServiceLookup { .. } => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Plugin Services Could Not Be Read",
        ),
        ExternalPluginsError::Service(attach) => service_attach_status(attach),
        ExternalPluginsError::NoUpdateWaiting { .. } => {
            (StatusCode::CONFLICT, "No Plugin Update Waits")
        }
        ExternalPluginsError::UpdateChanged { .. } => {
            (StatusCode::CONFLICT, "Plugin Update Changed")
        }
        ExternalPluginsError::Update(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Approved Plugin Build Unavailable",
        ),
    };
    temps_core::problemdetails::new(status)
        .with_title(title)
        .with_detail(public_error_detail(error))
}

/// The status and title for a failure to attach to the service a plugin runs
/// as. Every variant is named: a new one has to be placed deliberately.
fn service_attach_status(
    error: &crate::service_runtime::ServiceAttachError,
) -> (StatusCode, &'static str) {
    use crate::service_runtime::ServiceAttachError as Attach;
    match error {
        Attach::ServiceNotFound { .. } => (StatusCode::NOT_FOUND, "Service Not Found"),
        Attach::EnvironmentNotFound { .. } => (StatusCode::NOT_FOUND, "Environment Not Found"),
        Attach::Asleep { .. } => (StatusCode::CONFLICT, "Plugin Service Is Stopped"),
        Attach::NotDeployed { .. } => (StatusCode::CONFLICT, "Service Is Not Deployed"),
        Attach::NoReachableContainer { .. } => {
            (StatusCode::CONFLICT, "Service Has No Running Container")
        }
        Attach::NameChanged { .. } => (StatusCode::CONFLICT, "Plugin Name Changed"),
        Attach::UpdateWaiting { .. } => (StatusCode::CONFLICT, "Plugin Update Waits For Approval"),
        Attach::NameTaken { .. } => (StatusCode::CONFLICT, "Plugin Name Already Installed"),
        Attach::NotAPlugin { .. } => (StatusCode::BAD_REQUEST, "Service Is Not A Plugin"),
        Attach::InvalidHello { .. } => (StatusCode::BAD_REQUEST, "Plugin Hello Is Invalid"),
        Attach::ProtocolMismatch { .. } => {
            (StatusCode::BAD_REQUEST, "Plugin Protocol Not Supported")
        }
        Attach::InvalidName { .. } => (StatusCode::BAD_REQUEST, "Plugin Name Is Invalid"),
        Attach::PrivilegedAccess { .. } => (
            StatusCode::BAD_REQUEST,
            "Plugin Asks For Access A Service Is Not Given",
        ),
        Attach::Unreachable { .. } => (StatusCode::BAD_GATEWAY, "Plugin Service Unreachable"),
        Attach::Refused { .. } => (StatusCode::BAD_GATEWAY, "Plugin Service Refused Temps"),
        Attach::UnexpectedStatus { .. } => (
            StatusCode::BAD_GATEWAY,
            "Plugin Service Answered Unexpectedly",
        ),
        Attach::ChannelRefused { .. } => (
            StatusCode::BAD_GATEWAY,
            "Plugin Refused The Platform Channel",
        ),
        Attach::Unavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            "Plugins Cannot Run As Services Here",
        ),
        Attach::Token { .. } => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Deployment Token Unavailable",
        ),
        Attach::Record { .. } => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Plugin Could Not Be Recorded",
        ),
        Attach::Database(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Plugin Service Could Not Be Read",
        ),
    }
}

/// Render an operator-facing error without exposing local paths, transport
/// internals, or plugin-controlled diagnostics. Full typed errors remain in
/// server logs at their origin.
fn public_error_detail(error: &ExternalPluginsError) -> String {
    match error {
        // These carry only the plugin name the caller sent, and a deployment
        // of its service.
        ExternalPluginsError::InvalidPluginName { .. }
        | ExternalPluginsError::ShuttingDown
        | ExternalPluginsError::NotInstalled { .. }
        | ExternalPluginsError::NoUpdateWaiting { .. }
        | ExternalPluginsError::UpdateChanged { .. } => error.to_string(),
        ExternalPluginsError::Update(_) => {
            "The plugin's approved build could not be read from or recorded in the database. See the server log."
                .to_string()
        }
        ExternalPluginsError::RetiredRecord { name, .. } => format!(
            "The installation record of plugin '{name}' could not be removed from this server's plugin directory, so it is still listed. See the server log."
        ),
        ExternalPluginsError::Grant(_) => {
            "The plugin's permissions could not be revoked in the database, so it was not removed. Try again."
                .to_string()
        }
        ExternalPluginsError::Service(error) => error.public_reason(),
        ExternalPluginsError::ServiceLookup { .. } => {
            "Temps could not read the services plugins run as from its database. See the server log."
                .to_string()
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct UninstallPluginResponse {
    pub name: String,
    pub message: String,
    pub data_preserved: bool,
}

/// Remove a plugin. Its service, or the files an executable stored, are kept.
#[utoipa::path(
    tag = "External Plugins",
    post,
    path = "/x/plugins/{name}/uninstall",
    operation_id = "uninstall_plugin",
    params(("name" = String, Path)),
    responses(
        (status = 200, description = "Plugin removed; its service or stored files are kept", body = UninstallPluginResponse),
        (status = 400, description = "Unsafe plugin name", body = temps_core::ProblemDetails),
        (status = 401, description = "Unauthorized", body = temps_core::ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = temps_core::ProblemDetails),
        (status = 404, description = "No plugin with this name", body = temps_core::ProblemDetails),
        (status = 428, description = "Recent sensitive-action verification required", body = temps_core::ProblemDetails),
        (status = 500, description = "The plugin could not be removed", body = temps_core::ProblemDetails),
        (status = 503, description = "Audit or plugin service unavailable", body = temps_core::ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
async fn uninstall_plugin(
    RequireAuth(auth): RequireAuth,
    State(state): State<ExternalPluginsAppState>,
    Extension(metadata): Extension<temps_core::RequestMetadata>,
    Path(name): Path<String>,
) -> Result<Json<UninstallPluginResponse>, Problem> {
    permission_guard!(auth, SystemAdmin);
    if !crate::naming::valid_plugin_name(&name) {
        return Err(service_problem(&ExternalPluginsError::InvalidPluginName {
            name,
        }));
    }
    temps_auth::require_sensitive_action(
        state.sensitive_action_authorizer.as_ref(),
        &auth,
        temps_core::SensitiveAction::UninstallExternalPlugin { name: name.clone() },
    )
    .await?;
    let context = audit_context(&auth, &metadata);
    let audit = |operation: &str, failure: Option<String>| ExternalPluginWriteAudit {
        context: context.clone(),
        operation: operation.to_string(),
        plugin_name: Some(name.clone()),
        version: None,
        platform: None,
        sha256: None,
        signer_key_id: None,
        registry_source: None,
        failure,
    };
    record_required_audit(&state, &audit("EXTERNAL_PLUGIN_UNINSTALL_REQUESTED", None)).await?;
    if let Err(error) = state.service.uninstall_plugin(&name).await {
        record_audit(
            &state,
            &audit(
                "EXTERNAL_PLUGIN_UNINSTALL_FAILED",
                Some(public_error_detail(&error)),
            ),
        )
        .await;
        return Err(service_problem(&error));
    }
    record_audit(&state, &audit("EXTERNAL_PLUGIN_UNINSTALLED", None)).await;
    Ok(Json(UninstallPluginResponse {
        message: format!(
            "Plugin '{name}' was removed; its service and the files it stored were kept"
        ),
        name,
        data_preserved: true,
    }))
}

#[utoipa::path(
    tag = "External Plugins",
    get,
    path = "/x/plugins/{name}/status",
    params(("name" = String, Path)),
    responses(
        (status = 200, description = "Whether Temps is connected to the plugin", body = PluginStatusResponse),
        (status = 400, description = "Invalid plugin name", body = temps_core::ProblemDetails),
        (status = 401, description = "Unauthorized", body = temps_core::ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = temps_core::ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
async fn get_plugin_status(
    RequireAuth(auth): RequireAuth,
    State(state): State<ExternalPluginsAppState>,
    Path(name): Path<String>,
) -> Result<Json<PluginStatusResponse>, Problem> {
    permission_guard!(auth, SystemAdmin);
    if !crate::naming::valid_plugin_name(&name) {
        return Err(service_problem(&ExternalPluginsError::InvalidPluginName {
            name,
        }));
    }
    let configured = state.service.manager().is_running(&name).await;
    Ok(Json(PluginStatusResponse {
        configured,
        reason: (!configured)
            .then(|| format!("Plugin '{name}' is not connected. A plugin runs as a Temps service; see Settings > Plugins for the state of its service")),
        setup_path: "/settings/plugins".to_string(),
    }))
}

/// Build the router for external plugin management endpoints.
pub fn configure_routes() -> Router<ExternalPluginsAppState> {
    Router::new()
        .route("/x/plugins", get(list_external_plugins))
        .route("/x/plugins/reload", post(reload_plugins))
        .route(
            "/x/plugins/catalog/repositories",
            get(list_repository_plugin_catalog),
        )
        .route(
            "/ai/conversations/{chat}/plugin-watches",
            get(watches::list_chat_plugin_watches).post(watches::change_chat_plugin_watch),
        )
        .route("/x/plugins/{name}/uninstall", post(uninstall_plugin))
        .route("/x/plugins/{name}/status", get(get_plugin_status))
        .route(
            "/x/plugins/{name}/ai-tools",
            get(get_plugin_ai_tools).put(put_plugin_ai_tools),
        )
        .route(
            "/x/plugins/{name}/grants",
            get(get_plugin_grants).put(put_plugin_grants),
        )
        .route(
            "/x/plugins/services",
            get(services::get_plugin_services).post(services::add_service_plugin),
        )
        .route(
            "/x/plugins/{name}/update/approve",
            post(updates::approve_plugin_update),
        )
}

#[derive(OpenApiTrait)]
#[openapi(
    paths(
        list_external_plugins,
        reload_plugins,
        list_repository_plugin_catalog,
        uninstall_plugin,
        get_plugin_status,
        watches::list_chat_plugin_watches,
        watches::change_chat_plugin_watch,
        ai_tools::get_plugin_ai_tools,
        ai_tools::put_plugin_ai_tools,
        get_plugin_grants,
        put_plugin_grants,
        services::get_plugin_services,
        services::add_service_plugin,
        updates::approve_plugin_update,
    ),
    components(
        schemas(
            PluginManifest,
            crate::watches::WatchSource, crate::watches::WatchPanel, crate::watches::WatchChange, crate::watches::WatchMode, crate::watches::ChatWatch,
            NavEntry,
            NavSection,
            UiManifest,
            UiRoute,
            ReloadResponse,
            ReloadFailureResponse,
            UninstallPluginResponse,
            RepositoryCatalogResponse,
            crate::source_catalog::RepositoryCatalogPlugin,
            crate::source_catalog::RepositoryCatalogPermission,
            crate::source_catalog::RepositoryScreenshot,
            crate::source_catalog::RepositoryValidation,
            PluginStatusResponse,
            crate::ai_tools::PluginAiToolsResponse,
            crate::ai_tools::UpdatePluginAiTools,
            crate::ai_tools::AiToolInfo,
            PluginGrantsResponse,
            PluginOutboundNetwork,
            crate::scope::PluginWriteArea,
            services::PluginServicesResponse,
            services::ServicePluginResponse,
            services::RetiredPluginResponse,
            services::AddServicePlugin,
            services::AddedServicePluginResponse,
            updates::PendingPluginUpdateResponse,
            updates::ApprovePluginUpdate,
            updates::ApprovedPluginUpdateResponse,
            crate::updates::AccessDifference,
            crate::manager::ServicePluginState,
            crate::grants::PluginGrantConfig,
            PluginActorInfo,
            PluginAiCapability,
            PluginHostPermission,
            temps_core::ProblemDetails,
        )
    ),
    tags(
        (name = "External Plugins", description = "External plugin management and discovery")
    )
)]
pub struct ExternalPluginsApiDoc;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn uninstall_rejects_non_admin_before_audit() {
        let temp = tempfile::tempdir().expect("tempdir");
        let config = crate::manager::ExternalPluginConfig::new(temp.path().to_path_buf());
        let audit = Arc::new(RecordingAuditLogger::default());
        let state = ExternalPluginsAppState {
            service: Arc::new(ExternalPluginsService::new_empty(config, None, mock_db())),
            audit_service: audit.clone(),
            sensitive_action_authorizer: Arc::new(AllowSensitiveActions),
        };
        let error = uninstall_plugin(
            user_auth(Role::User),
            State(state),
            metadata(),
            Path("example".to_string()),
        )
        .await
        .expect_err("non-admin must be rejected");
        assert_eq!(error.status_code, StatusCode::FORBIDDEN);
        assert!(audit.operations.lock().expect("audit lock").is_empty());
    }

    #[tokio::test]
    async fn uninstall_missing_installation_returns_not_found_with_failure_audit() {
        let temp = tempfile::tempdir().expect("tempdir");
        let config = crate::manager::ExternalPluginConfig::new(temp.path().to_path_buf());
        let audit = Arc::new(RecordingAuditLogger::default());
        let state = ExternalPluginsAppState {
            service: Arc::new(ExternalPluginsService::new_empty(config, None, mock_db())),
            audit_service: audit.clone(),
            sensitive_action_authorizer: Arc::new(AllowSensitiveActions),
        };
        let error = uninstall_plugin(
            user_auth(Role::PlatformAdmin),
            State(state),
            metadata(),
            Path("example".to_string()),
        )
        .await
        .expect_err("missing install must be rejected");
        assert_eq!(error.status_code, StatusCode::NOT_FOUND);
        assert_eq!(
            *audit.operations.lock().expect("audit lock"),
            vec![
                "EXTERNAL_PLUGIN_UNINSTALL_REQUESTED".to_string(),
                "EXTERNAL_PLUGIN_UNINSTALL_FAILED".to_string(),
            ]
        );
    }

    use std::sync::Mutex;

    use chrono::Utc;
    use temps_auth::context::AuthContext;
    use temps_auth::permissions::Role;
    use temps_entities::users;
    use tracing::instrument::WithSubscriber;
    use tracing_subscriber::{layer::SubscriberExt, Layer};

    use crate::manager::ExternalPluginConfig;

    struct NoopAuditLogger;

    struct RejectingAuditLogger;

    #[derive(Clone, Default)]
    struct CapturedAuditEvents(Arc<Mutex<Vec<String>>>);

    struct EventFieldVisitor<'a>(&'a mut String);

    impl tracing::field::Visit for EventFieldVisitor<'_> {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            use std::fmt::Write as _;

            let _ = write!(self.0, " {}={value:?}", field.name());
        }

        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            use std::fmt::Write as _;

            let _ = write!(self.0, " {}={value}", field.name());
        }
    }

    impl<S> Layer<S> for CapturedAuditEvents
    where
        S: tracing::Subscriber,
    {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _context: tracing_subscriber::layer::Context<'_, S>,
        ) {
            let mut fields = String::new();
            event.record(&mut EventFieldVisitor(&mut fields));
            self.0
                .lock()
                .expect("captured audit-event lock")
                .push(fields);
        }
    }

    pub(super) struct AllowSensitiveActions;
    pub(super) struct RequireSensitiveVerification;

    #[async_trait::async_trait]
    impl temps_core::SensitiveActionAuthorizer for AllowSensitiveActions {
        async fn authorize(
            &self,
            _action: &temps_core::SensitiveAction,
            _principal: &temps_core::SensitiveActionPrincipal,
        ) -> Result<
            temps_core::SensitiveActionDecision,
            temps_core::SensitiveActionAuthorizationError,
        > {
            Ok(temps_core::SensitiveActionDecision::Allow)
        }
    }

    #[async_trait::async_trait]
    impl temps_core::SensitiveActionAuthorizer for RequireSensitiveVerification {
        async fn authorize(
            &self,
            _action: &temps_core::SensitiveAction,
            _principal: &temps_core::SensitiveActionPrincipal,
        ) -> Result<
            temps_core::SensitiveActionDecision,
            temps_core::SensitiveActionAuthorizationError,
        > {
            Ok(temps_core::SensitiveActionDecision::RequireVerification {
                mfa_setup_required: false,
            })
        }
    }

    #[async_trait::async_trait]
    impl temps_core::AuditLogger for NoopAuditLogger {
        async fn create_audit_log(
            &self,
            _operation: &dyn temps_core::audit::AuditOperation,
        ) -> temps_core::AuditResult<()> {
            Ok(())
        }
    }

    #[async_trait::async_trait]
    impl temps_core::AuditLogger for RejectingAuditLogger {
        async fn create_audit_log(
            &self,
            operation: &dyn temps_core::audit::AuditOperation,
        ) -> temps_core::AuditResult<()> {
            Err(temps_core::AuditError::write(
                operation.operation_type(),
                "audit backend unavailable: private-audit-detail-must-not-leak",
            ))
        }
    }

    #[derive(Default)]
    pub(super) struct RecordingAuditLogger {
        pub(super) operations: std::sync::Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl temps_core::AuditLogger for RecordingAuditLogger {
        async fn create_audit_log(
            &self,
            operation: &dyn temps_core::audit::AuditOperation,
        ) -> temps_core::AuditResult<()> {
            self.operations
                .lock()
                .expect("audit operation lock")
                .push(operation.operation_type());
            Ok(())
        }
    }

    pub(super) fn mock_db() -> Arc<sea_orm::DatabaseConnection> {
        Arc::new(sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection())
    }

    pub(super) fn test_state() -> ExternalPluginsAppState {
        let config = ExternalPluginConfig::new(
            std::env::temp_dir().join("temps-external-plugins-handler-test"),
        );
        ExternalPluginsAppState {
            service: Arc::new(ExternalPluginsService::new_empty(config, None, mock_db())),
            audit_service: Arc::new(NoopAuditLogger),
            sensitive_action_authorizer: Arc::new(AllowSensitiveActions),
        }
    }

    pub(super) fn test_state_with_audit(
        audit_service: Arc<dyn temps_core::AuditLogger>,
    ) -> ExternalPluginsAppState {
        let mut state = test_state();
        state.audit_service = audit_service;
        state
    }

    pub(super) fn metadata() -> Extension<temps_core::RequestMetadata> {
        Extension(temps_core::RequestMetadata {
            ip_address: "192.0.2.1".to_string(),
            user_agent: "external-plugin-test".to_string(),
            headers: Default::default(),
            visitor_id_cookie: None,
            session_id_cookie: None,
            base_url: "http://localhost".to_string(),
            scheme: "http".to_string(),
            host: "localhost".to_string(),
            is_secure: false,
        })
    }

    fn test_user(id: i32) -> users::Model {
        let now = Utc::now();
        users::Model {
            id,
            name: "Test User".to_string(),
            email: format!("user{id}@example.com"),
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

    pub(super) fn user_auth(role: Role) -> RequireAuth {
        RequireAuth(AuthContext::new_persisted_session(test_user(1), role, 1))
    }

    // Regression tests for the unauthenticated-access finding: `reload_plugins`
    // stopped/restarted every plugin process and `list_external_plugins`
    // leaked the full plugin manifest to any caller because neither handler
    // had a `RequireAuth` extractor, despite the OpenAPI docs on this file
    // claiming `SystemAdmin` was required for reload.

    #[tokio::test]
    async fn reload_plugins_rejects_non_admin() {
        let state = test_state();
        let err = reload_plugins(user_auth(Role::User), State(state), metadata())
            .await
            .expect_err("a plain User role must not be able to reload plugins");
        assert_eq!(err.status_code, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn reload_plugins_allows_platform_admin() {
        // Arrange
        let temp = tempfile::tempdir().expect("tempdir");
        let config = ExternalPluginConfig::new(temp.path().to_path_buf());
        let audit = Arc::new(RecordingAuditLogger::default());
        let state = ExternalPluginsAppState {
            service: Arc::new(ExternalPluginsService::new_empty(config, None, mock_db())),
            audit_service: audit.clone(),
            sensitive_action_authorizer: Arc::new(AllowSensitiveActions),
        };

        // Act
        let (status, response) =
            reload_plugins(user_auth(Role::PlatformAdmin), State(state), metadata())
                .await
                .expect("a PlatformAdmin must be able to reload plugins");

        // Assert
        assert_eq!(status, StatusCode::OK);
        assert_eq!(response.loaded, 0);
        assert!(response.failures.is_empty());
        assert_eq!(
            *audit.operations.lock().expect("audit operation lock"),
            vec!["EXTERNAL_PLUGINS_RELOADED".to_string()]
        );
    }

    #[tokio::test]
    async fn reloading_a_server_that_only_has_an_installed_executable_succeeds_and_starts_nothing()
    {
        // Arrange: what an upgrade finds when a plugin was installed as an
        // executable.
        let temp = tempfile::tempdir().expect("tempdir");
        let config = ExternalPluginConfig::new(temp.path().to_path_buf());
        std::fs::create_dir_all(config.plugins_dir.join("reports")).expect("installation");
        std::fs::write(
            config.plugins_dir.join("reports/active.json"),
            br#"{"version":"1.4.0","directory":"1.4.0-abc"}"#,
        )
        .expect("active record");
        let audit = Arc::new(RecordingAuditLogger::default());
        let service = Arc::new(ExternalPluginsService::new_empty(config, None, mock_db()));
        let state = ExternalPluginsAppState {
            service: service.clone(),
            audit_service: audit.clone(),
            sensitive_action_authorizer: Arc::new(AllowSensitiveActions),
        };

        // Act
        let (status, response) =
            reload_plugins(user_auth(Role::PlatformAdmin), State(state), metadata())
                .await
                .expect("reload answers");

        // Assert: not running is what that plugin is now, not a failed reload.
        assert_eq!(status, StatusCode::OK);
        assert_eq!(response.loaded, 0);
        assert!(response.failures.is_empty());
        assert!(service.manifests().await.is_empty());
        assert_eq!(service.manager().retired_plugins().await.len(), 1);
        assert_eq!(
            *audit.operations.lock().expect("audit operation lock"),
            vec!["EXTERNAL_PLUGINS_RELOADED".to_string()]
        );
    }

    #[tokio::test]
    async fn test_get_plugin_status_non_admin_returns_forbidden() {
        // Arrange
        let state = test_state();

        // Act
        let error = get_plugin_status(
            user_auth(Role::User),
            State(state),
            Path("safe-plugin".to_string()),
        )
        .await
        .expect_err("status reveals host plugin state and must require an administrator");

        // Assert
        assert_eq!(error.status_code, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn list_external_plugins_allows_any_authenticated_role() {
        // Any signed-in user must be able to list plugins — the sidebar nav
        // for every authenticated user depends on this endpoint. Only
        // unauthenticated (no session at all) callers should be rejected,
        // which `RequireAuth`'s extractor enforces at the HTTP layer before
        // this handler body ever runs.
        let state = test_state();
        let Json(manifests) = list_external_plugins(user_auth(Role::User), State(state)).await;
        assert!(manifests.is_empty());
    }

    #[tokio::test]
    async fn failed_audit_logs_do_not_expose_backend_error_details() {
        let state = test_state_with_audit(Arc::new(RejectingAuditLogger));
        let captured = CapturedAuditEvents::default();
        let subscriber = tracing_subscriber::registry().with(captured.clone());
        let operation = ExternalPluginWriteAudit {
            context: audit_context(&user_auth(Role::PlatformAdmin).0, &metadata().0),
            operation: "EXTERNAL_PLUGIN_INSTALL_REQUESTED".to_string(),
            plugin_name: Some("safe-plugin".to_string()),
            version: None,
            platform: None,
            sha256: None,
            signer_key_id: None,
            registry_source: None,
            failure: None,
        };

        record_audit(&state, &operation)
            .with_subscriber(subscriber)
            .await;

        let logs = captured
            .0
            .lock()
            .expect("captured audit-event lock")
            .join("\n");
        assert!(logs.contains("EXTERNAL_PLUGIN_INSTALL_REQUESTED"));
        assert!(!logs.contains("private-audit-detail"));
    }

    #[test]
    fn test_openapi_spec_has_plugins_path() {
        let spec = ExternalPluginsApiDoc::openapi();
        assert!(
            spec.paths.paths.contains_key("/x/plugins"),
            "OpenAPI spec must contain /x/plugins path"
        );
    }

    #[test]
    fn test_openapi_spec_has_schemas() {
        let spec = ExternalPluginsApiDoc::openapi();
        let components = spec.components.expect("should have components");
        assert!(
            components.schemas.contains_key("PluginManifest"),
            "OpenAPI spec must contain PluginManifest schema"
        );
        assert!(
            components.schemas.contains_key("NavEntry"),
            "OpenAPI spec must contain NavEntry schema"
        );
        assert!(
            components.schemas.contains_key("NavSection"),
            "OpenAPI spec must contain NavSection schema"
        );
    }

    #[test]
    fn test_openapi_spec_has_reload_path() {
        let spec = ExternalPluginsApiDoc::openapi();
        assert!(
            spec.paths.paths.contains_key("/x/plugins/reload"),
            "OpenAPI spec must contain /x/plugins/reload path"
        );
    }

    #[test]
    fn openapi_spec_has_the_service_plugin_paths_and_no_way_to_install_an_executable() {
        let spec = ExternalPluginsApiDoc::openapi();
        for path in [
            "/x/plugins",
            "/x/plugins/services",
            "/x/plugins/catalog/repositories",
            "/x/plugins/{name}/status",
            "/x/plugins/{name}/uninstall",
            "/x/plugins/{name}/grants",
            "/x/plugins/{name}/update/approve",
        ] {
            assert!(spec.paths.paths.contains_key(path), "missing {path}");
        }
        // A plugin runs only as a Temps service: nothing installs, updates or
        // re-isolates an executable.
        for path in spec.paths.paths.keys() {
            assert!(
                !path.contains("/install") && !path.ends_with("/update"),
                "{path} installs or updates an executable"
            );
            assert!(
                !path.ends_with("/isolation") && !path.ends_with("/sandbox"),
                "{path} changes how an executable is isolated"
            );
        }
        // Approving the update of a plugin's service is not updating an
        // executable: it is the one route under `/update`.
        let routes = format!("{:?}", configure_routes()).replace("/update/approve", "");
        for removed in ["/install", "/update", "/isolation", "/sandbox"] {
            assert!(!routes.contains(removed), "{removed} is still routed");
        }
    }

    #[test]
    fn openapi_spec_documents_plugin_management_errors() {
        let spec = serde_json::to_value(ExternalPluginsApiDoc::openapi())
            .expect("serialize external plugin OpenAPI document");
        let paths = spec
            .get("paths")
            .and_then(serde_json::Value::as_object)
            .expect("OpenAPI paths");

        for (path, method, expected_statuses) in [
            (
                "/x/plugins/services",
                "post",
                &[
                    "201", "400", "401", "403", "404", "409", "428", "502", "503",
                ][..],
            ),
            (
                "/x/plugins/{name}/uninstall",
                "post",
                &["200", "400", "401", "403", "404", "428", "500", "503"][..],
            ),
            (
                "/x/plugins/{name}/status",
                "get",
                &["200", "400", "401", "403"][..],
            ),
        ] {
            let responses = paths
                .get(path)
                .and_then(|item| item.get(method))
                .and_then(|operation| operation.get("responses"))
                .and_then(serde_json::Value::as_object)
                .unwrap_or_else(|| panic!("missing responses for {method} {path}"));
            for status in expected_statuses {
                assert!(
                    responses.contains_key(*status),
                    "missing {status} response for {method} {path}"
                );
            }
        }
    }

    #[test]
    fn test_openapi_spec_has_reload_response_schema() {
        let spec = ExternalPluginsApiDoc::openapi();
        let components = spec.components.expect("should have components");
        assert!(
            components.schemas.contains_key("ReloadResponse"),
            "OpenAPI spec must contain ReloadResponse schema"
        );
    }

    #[test]
    fn test_reload_response_serialization() {
        let response = ReloadResponse {
            loaded: 2,
            plugins: vec!["seo-analyzer".into(), "monitoring".into()],
            failures: Vec::new(),
            message: "Reload complete. 2 plugin(s) loaded.".into(),
        };
        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(json["loaded"], 2);
        assert_eq!(json["plugins"][0], "seo-analyzer");
        assert_eq!(json["plugins"][1], "monitoring");
    }

    #[tokio::test]
    async fn plugin_grants_read_requires_system_admin() {
        let error = get_plugin_grants(
            user_auth(Role::User),
            State(test_state()),
            Path("safe-plugin".to_string()),
        )
        .await
        .expect_err("grant discovery must require system administration");
        assert_eq!(error.status_code, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn plugin_grants_write_requires_recent_sensitive_verification() {
        let audit = Arc::new(RecordingAuditLogger::default());
        let mut state = test_state_with_audit(audit.clone());
        state.sensitive_action_authorizer = Arc::new(RequireSensitiveVerification);
        let error = put_plugin_grants(
            user_auth(Role::PlatformAdmin),
            State(state),
            metadata(),
            Path("safe-plugin".to_string()),
            Json(crate::grants::PluginGrantConfig::default()),
        )
        .await
        .expect_err("grant mutation must require sensitive verification");
        assert_eq!(error.status_code, StatusCode::PRECONDITION_REQUIRED);
        assert!(audit.operations.lock().expect("audit lock").is_empty());
    }
    fn ai_actor(enabled: bool) -> temps_entities::external_plugin_actors::Model {
        temps_entities::external_plugin_actors::Model {
            id: uuid::Uuid::from_u128(1),
            plugin_name: "reports".into(),
            binary_sha256: "fixture".into(),
            source_identity: "fixture".into(),
            active: true,
            ai_tools_enabled: enabled,
            isolation: "container".into(),
            project_id: None,
            environment_id: None,
            manifest: None,
            tier: "community".into(),
            pinned_build: None,
            pinned_deployment_id: None,
            pending_build: None,
            pending_deployment_id: None,
            pending_manifest: None,
            pending_review: None,
            pending_since: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn chat_watch_routes_require_a_caller_and_reject_cross_chat_agent_requests() {
        use tower::ServiceExt;
        for method in ["GET", "POST"] {
            for authenticated in [false, true] {
                let mut router = configure_routes()
                    .with_state(test_state())
                    .layer(Extension(metadata().0));
                if authenticated {
                    router = router
                        .layer(Extension(user_auth(Role::Admin).0))
                        .layer(Extension(temps_core::ai_tool_call::AiToolCall))
                        .layer(Extension(temps_core::ai_tool_call::ChatWatchScope(
                            "another-chat".into(),
                        )));
                }
                let response = router
                    .oneshot(
                        axum::http::Request::builder()
                            .method(method)
                            .uri("/ai/conversations/target/plugin-watches")
                            .header("content-type", "application/json")
                            .body(axum::body::Body::from(
                                r#"{"action":"pause","id":"a1ab6f89-4446-43a0-93b5-eb48f8047a35"}"#,
                            ))
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(
                    response.status(),
                    if authenticated {
                        StatusCode::FORBIDDEN
                    } else {
                        StatusCode::UNAUTHORIZED
                    }
                );
                assert_eq!(
                    response.headers()["content-type"],
                    "application/problem+json"
                );
            }
        }
    }

    #[tokio::test]
    async fn plugin_ai_tools_routes_enforce_auth_and_permissions() {
        use tower::ServiceExt;
        for method in ["GET", "PUT"] {
            for role in [None, Some(Role::User)] {
                let mut app = configure_routes()
                    .with_state(test_state())
                    .layer(Extension(metadata().0));
                if let Some(role) = role.clone() {
                    app = app.layer(Extension(user_auth(role).0));
                }
                let response = app
                    .oneshot(
                        axum::http::Request::builder()
                            .method(method)
                            .uri("/x/plugins/reports/ai-tools")
                            .header("content-type", "application/json")
                            .body(axum::body::Body::from(
                                r#"{"actor_id":"fixture","enabled":true}"#,
                            ))
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(
                    response.status(),
                    if role.is_none() {
                        StatusCode::UNAUTHORIZED
                    } else {
                        StatusCode::FORBIDDEN
                    }
                );
                assert_eq!(
                    response.headers()["content-type"],
                    "application/problem+json"
                );
                let body: serde_json::Value = serde_json::from_slice(
                    &axum::body::to_bytes(response.into_body(), 8192)
                        .await
                        .unwrap(),
                )
                .unwrap();
                assert!(body["title"].is_string());
            }
        }
    }

    #[tokio::test]
    async fn plugin_ai_tools_toggle_returns_current_consent_and_audits() {
        use tower::ServiceExt;
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([
                vec![ai_actor(false)],
                vec![ai_actor(true)],
                vec![ai_actor(true)],
                vec![ai_actor(true)],
            ])
            .into_connection();
        let audit = Arc::new(RecordingAuditLogger::default());
        let config = ExternalPluginConfig::new(std::env::temp_dir().join("plugin-ai-test"));
        let state = ExternalPluginsAppState {
            service: Arc::new(ExternalPluginsService::new_empty(
                config,
                None,
                Arc::new(db),
            )),
            audit_service: audit.clone(),
            sensitive_action_authorizer: Arc::new(AllowSensitiveActions),
        };
        let app = configure_routes()
            .with_state(state)
            .layer(Extension(metadata().0))
            .layer(Extension(user_auth(Role::PlatformAdmin).0));
        for method in ["PUT", "GET"] {
            let response = app
                .clone()
                .oneshot(
                    axum::http::Request::builder()
                        .method(method)
                        .uri("/x/plugins/reports/ai-tools")
                        .header("content-type", "application/json")
                        .body(axum::body::Body::from(
                            serde_json::json!({"actor_id":uuid::Uuid::from_u128(1),"installation_revision":"fixture","enabled":true})
                                .to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body: serde_json::Value = serde_json::from_slice(
                &axum::body::to_bytes(response.into_body(), 8192)
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(body["enabled"], true);
            assert_eq!(body["actor_id"], uuid::Uuid::from_u128(1).to_string());
            assert_eq!(body["tools"], serde_json::json!([]));
        }
        assert_eq!(audit.operations.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn saved_ai_consent_publication_failure_is_audited_and_explains_recovery() {
        use tower::ServiceExt;
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([vec![ai_actor(false)], vec![ai_actor(true)]])
            .append_query_errors([sea_orm::DbErr::Custom("private diagnostic".into())])
            .into_connection();
        let audit = Arc::new(RecordingAuditLogger::default());
        let state = ExternalPluginsAppState {
            service: Arc::new(ExternalPluginsService::new_empty(
                ExternalPluginConfig::new(std::env::temp_dir().join("plugin-ai-publication-test")),
                None,
                Arc::new(db),
            )),
            audit_service: audit.clone(),
            sensitive_action_authorizer: Arc::new(AllowSensitiveActions),
        };
        let response = configure_routes()
            .with_state(state)
            .layer(Extension(metadata().0))
            .layer(Extension(user_auth(Role::PlatformAdmin).0))
            .oneshot(
                axum::http::Request::builder()
                    .method("PUT")
                    .uri("/x/plugins/reports/ai-tools")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        serde_json::json!({"actor_id":uuid::Uuid::from_u128(1),"installation_revision":"fixture","enabled":true})
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            response.headers()["content-type"],
            "application/problem+json"
        );
        let body: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), 8192)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(body["title"], "Plugin AI Tools Unavailable");
        assert!(body["detail"]
            .as_str()
            .unwrap()
            .contains("Consent was saved"));
        assert!(body["detail"].as_str().unwrap().contains("Retry saving"));
        assert!(!body.to_string().contains("private diagnostic"));
        assert_eq!(audit.operations.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn plugin_ai_tools_errors_are_problem_details_without_success_audits() {
        use sea_orm::{DatabaseBackend, DbErr, MockDatabase};
        use tower::ServiceExt;
        for (method, db, status, title) in [
            (
                "GET",
                MockDatabase::new(DatabaseBackend::Postgres)
                    .append_query_results([
                        Vec::<temps_entities::external_plugin_actors::Model>::new(),
                    ]),
                StatusCode::NOT_FOUND,
                "Plugin Actor Not Found",
            ),
            (
                "PUT",
                MockDatabase::new(DatabaseBackend::Postgres)
                    .append_query_results([vec![ai_actor(false)]]),
                StatusCode::CONFLICT,
                "Plugin Actor Changed",
            ),
            (
                "GET",
                MockDatabase::new(DatabaseBackend::Postgres)
                    .append_query_errors([DbErr::Custom("private diagnostic".into())]),
                StatusCode::INTERNAL_SERVER_ERROR,
                "Plugin Grants Unavailable",
            ),
            (
                "PUT",
                MockDatabase::new(DatabaseBackend::Postgres)
                    .append_query_errors([DbErr::Custom("private diagnostic".into())]),
                StatusCode::INTERNAL_SERVER_ERROR,
                "Plugin Grants Unavailable",
            ),
        ] {
            let audit = Arc::new(RecordingAuditLogger::default());
            let state = ExternalPluginsAppState {
                service: Arc::new(ExternalPluginsService::new_empty(
                    ExternalPluginConfig::new(std::env::temp_dir().join("plugin-ai-contract-test")),
                    None,
                    Arc::new(db.into_connection()),
                )),
                audit_service: audit.clone(),
                sensitive_action_authorizer: Arc::new(AllowSensitiveActions),
            };
            let response = configure_routes()
                .with_state(state)
                .layer(Extension(metadata().0))
                .layer(Extension(user_auth(Role::PlatformAdmin).0))
                .oneshot(
                    axum::http::Request::builder()
                        .method(method)
                        .uri("/x/plugins/reports/ai-tools")
                        .header("content-type", "application/json")
                        .body(axum::body::Body::from(
                            serde_json::json!({"actor_id":uuid::Uuid::from_u128(2),"installation_revision":"fixture","enabled":true})
                                .to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), status);
            assert_eq!(
                response.headers()["content-type"],
                "application/problem+json"
            );
            let body: serde_json::Value = serde_json::from_slice(
                &axum::body::to_bytes(response.into_body(), 8192)
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(body["title"], title);
            assert!(body["detail"].is_string());
            assert!(!body.to_string().contains("private diagnostic"));
            if status != StatusCode::INTERNAL_SERVER_ERROR {
                assert!(body["detail"].as_str().unwrap().contains("reports"));
            }
            assert!(audit.operations.lock().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn plugin_ai_tools_invalid_json_and_media_type_are_problem_details() {
        use tower::ServiceExt;
        for (body, content_type, status) in [
            (
                "untrusted-payload",
                "application/json",
                StatusCode::BAD_REQUEST,
            ),
            (
                r#"{"actor_id":"fixture","enabled":"untrusted-payload"}"#,
                "application/json",
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
            (
                "untrusted-payload",
                "text/plain",
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
            ),
        ] {
            let audit = Arc::new(RecordingAuditLogger::default());
            let mut state = test_state();
            state.audit_service = audit.clone();
            let response = configure_routes()
                .with_state(state)
                .layer(Extension(metadata().0))
                .layer(Extension(user_auth(Role::PlatformAdmin).0))
                .oneshot(
                    axum::http::Request::builder()
                        .method("PUT")
                        .uri("/x/plugins/reports/ai-tools")
                        .header("content-type", content_type)
                        .body(axum::body::Body::from(body))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), status);
            assert_eq!(
                response.headers()["content-type"],
                "application/problem+json"
            );
            let problem: serde_json::Value = serde_json::from_slice(
                &axum::body::to_bytes(response.into_body(), 8192)
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(problem["title"], "Invalid Plugin AI Access Request");
            assert!(problem["detail"].as_str().unwrap().contains("reports"));
            assert!(!problem.to_string().contains("untrusted-payload"));
            assert!(audit.operations.lock().unwrap().is_empty());
        }
    }
}
