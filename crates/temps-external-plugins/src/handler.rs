// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! HTTP handlers for external plugin management endpoints.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde::{Deserialize, Serialize};
use temps_auth::{permission_guard, RequireAuth};
use temps_core::external_plugin::{NavEntry, NavSection, PluginManifest, UiManifest, UiRoute};
use temps_core::problemdetails::Problem;
use utoipa::{OpenApi as OpenApiTrait, ToSchema};

use crate::service::ExternalPluginsError;
use crate::service::ExternalPluginsService;

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

    fn serialize(&self) -> anyhow::Result<String> {
        serde_json::to_string(self).map_err(|error| {
            anyhow::anyhow!("failed to serialize external-plugin audit event: {error}")
        })
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
    if let Err(error) = state.audit_service.create_audit_log(operation).await {
        tracing::error!(operation = %operation.operation_type(), error = %error, "Failed to record external-plugin write audit");
    }
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

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct InstallPluginRequest {
    /// Validated registry name only. URLs, paths, versions, and hashes are not
    /// accepted from HTTP callers.
    pub name: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct InstallPluginResponse {
    pub name: String,
    pub version: String,
    pub platform: String,
    pub sha256: String,
    pub message: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PluginCatalogResponse {
    pub available: bool,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub plugins: Vec<crate::catalog::RegistryPlugin>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PluginStatusResponse {
    pub configured: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub setup_path: String,
}

fn service_problem(error: &ExternalPluginsError) -> Problem {
    use crate::catalog::CatalogError;
    use crate::install::InstallError;
    let (status, title) = match error {
        ExternalPluginsError::Install(InstallError::UnsafePluginName { .. })
        | ExternalPluginsError::Install(InstallError::UnsafeVersion { .. })
        | ExternalPluginsError::Install(InstallError::UnsupportedPlatform { .. })
        | ExternalPluginsError::Install(InstallError::NoRelease { .. })
        | ExternalPluginsError::NotInRegistry { .. }
        | ExternalPluginsError::DuplicateRegistryEntry { .. } => {
            (StatusCode::BAD_REQUEST, "Plugin Cannot Be Installed")
        }
        ExternalPluginsError::Catalog(CatalogError::TrustNotConfigured { .. }) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "Plugin Registry Trust Is Not Configured",
        ),
        ExternalPluginsError::Catalog(CatalogError::UntrustedKey { .. }) => (
            StatusCode::BAD_GATEWAY,
            "Plugin Registry Authentication Failed",
        ),
        ExternalPluginsError::Catalog(_) => (
            StatusCode::BAD_GATEWAY,
            "Plugin Registry Verification Failed",
        ),
        ExternalPluginsError::Install(InstallError::InvalidDigest { .. })
        | ExternalPluginsError::Install(InstallError::UnsafeArtifactUrl { .. })
        | ExternalPluginsError::Install(InstallError::Client { .. })
        | ExternalPluginsError::Install(InstallError::Download { .. })
        | ExternalPluginsError::Install(InstallError::DownloadStatus { .. })
        | ExternalPluginsError::Install(InstallError::TooLarge { .. })
        | ExternalPluginsError::Install(InstallError::DigestMismatch { .. }) => (
            StatusCode::BAD_GATEWAY,
            "Plugin Artifact Verification Failed",
        ),
        ExternalPluginsError::Install(InstallError::RegistryRollback { .. }) => {
            (StatusCode::CONFLICT, "Plugin Registry Rollback Refused")
        }
        ExternalPluginsError::Install(InstallError::Io { .. })
        | ExternalPluginsError::Install(InstallError::MissingActiveRecord { .. })
        | ExternalPluginsError::Install(InstallError::InvalidReceipt { .. }) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Plugin Installation Failed",
        ),
        ExternalPluginsError::CandidateRejected { .. } => (
            StatusCode::BAD_GATEWAY,
            "Plugin Startup Verification Failed",
        ),
        ExternalPluginsError::ShuttingDown => (
            StatusCode::SERVICE_UNAVAILABLE,
            "Plugin Service Is Shutting Down",
        ),
    };
    temps_core::problemdetails::new(status)
        .with_title(title)
        .with_detail(public_error_detail(error))
}

/// Render an operator-facing error without exposing local paths, transport
/// internals, or plugin-controlled diagnostics. Full typed errors remain in
/// server logs at their origin.
fn public_error_detail(error: &ExternalPluginsError) -> String {
    use crate::catalog::CatalogError;
    use crate::install::InstallError;

    match error {
        ExternalPluginsError::Install(
            error @ (InstallError::UnsafePluginName { .. }
            | InstallError::UnsafeVersion { .. }
            | InstallError::UnsupportedPlatform { .. }
            | InstallError::NoRelease { .. }
            | InstallError::InvalidDigest { .. }
            | InstallError::RegistryRollback { .. }),
        ) => error.to_string(),
        ExternalPluginsError::NotInRegistry { .. }
        | ExternalPluginsError::DuplicateRegistryEntry { .. }
        | ExternalPluginsError::ShuttingDown => error.to_string(),
        ExternalPluginsError::Catalog(CatalogError::TrustNotConfigured { .. }) => {
            "Configure a trusted plugin-registry key ID and Ed25519 public key before using the registry"
                .to_string()
        }
        ExternalPluginsError::Catalog(CatalogError::UntrustedKey { key_id, .. }) => {
            format!("The plugin registry used untrusted signing key ID '{key_id}'")
        }
        ExternalPluginsError::Catalog(_) => {
            "The signed plugin registry response could not be authenticated".to_string()
        }
        ExternalPluginsError::Install(InstallError::DigestMismatch {
            plugin, version, ..
        }) => format!(
            "Downloaded artifact for plugin '{plugin}' v{version} did not match its signed digest"
        ),
        ExternalPluginsError::Install(
            InstallError::UnsafeArtifactUrl { plugin, .. }
            | InstallError::Download { plugin, .. },
        ) => format!("Plugin '{plugin}' could not be downloaded securely"),
        ExternalPluginsError::Install(
            InstallError::Client { .. }
            | InstallError::DownloadStatus { .. }
            | InstallError::TooLarge { .. },
        ) => "The plugin artifact could not be downloaded securely".to_string(),
        ExternalPluginsError::Install(
            InstallError::Io { plugin, .. }
            | InstallError::MissingActiveRecord { plugin, .. }
            | InstallError::InvalidReceipt { plugin, .. },
        ) => format!("Plugin '{plugin}' could not be installed or verified locally"),
        ExternalPluginsError::CandidateRejected { name, version, .. } => {
            format!("Plugin '{name}' v{version} did not pass startup verification")
        }
    }
}

#[utoipa::path(
    tag = "External Plugins",
    get,
    path = "/x/plugins/catalog",
    responses((status = 200, body = PluginCatalogResponse)),
    security(("bearer_auth" = []))
)]
async fn list_plugin_catalog(
    RequireAuth(auth): RequireAuth,
    State(state): State<ExternalPluginsAppState>,
) -> Result<Json<PluginCatalogResponse>, Problem> {
    permission_guard!(auth, SystemAdmin);
    let source = state.service.manager().config().registry.url.clone();
    match state.service.catalog().await {
        Ok(registry) => Ok(Json(PluginCatalogResponse {
            available: true,
            source,
            reason: None,
            plugins: registry.document.plugins,
        })),
        Err(error) => Ok(Json(PluginCatalogResponse {
            available: false,
            source,
            reason: Some(error.to_string()),
            plugins: Vec::new(),
        })),
    }
}

#[utoipa::path(
    tag = "External Plugins",
    post,
    path = "/x/plugins/install",
    request_body = InstallPluginRequest,
    responses((status = 200, body = InstallPluginResponse)),
    security(("bearer_auth" = []))
)]
async fn install_plugin(
    RequireAuth(auth): RequireAuth,
    State(state): State<ExternalPluginsAppState>,
    Extension(metadata): Extension<temps_core::RequestMetadata>,
    Json(request): Json<InstallPluginRequest>,
) -> Result<Json<InstallPluginResponse>, Problem> {
    permission_guard!(auth, SystemAdmin);
    temps_auth::require_sensitive_action(
        state.sensitive_action_authorizer.as_ref(),
        &auth,
        temps_core::SensitiveAction::InstallExternalPlugin {
            name: request.name.clone(),
        },
    )
    .await?;
    let context = audit_context(&auth, &metadata);
    record_audit(
        &state,
        &ExternalPluginWriteAudit {
            context: context.clone(),
            operation: "EXTERNAL_PLUGIN_INSTALL_REQUESTED".to_string(),
            plugin_name: Some(request.name.clone()),
            version: None,
            platform: None,
            sha256: None,
            signer_key_id: None,
            registry_source: Some(state.service.manager().config().registry.url.clone()),
            failure: None,
        },
    )
    .await;
    let selected = match state.service.select_plugin(&request.name).await {
        Ok(selected) => selected,
        Err(error) => {
            record_audit(
                &state,
                &ExternalPluginWriteAudit {
                    context,
                    operation: "EXTERNAL_PLUGIN_INSTALL_FAILED".to_string(),
                    plugin_name: Some(request.name),
                    version: None,
                    platform: None,
                    sha256: None,
                    signer_key_id: None,
                    registry_source: Some(state.service.manager().config().registry.url.clone()),
                    failure: Some(public_error_detail(&error)),
                },
            )
            .await;
            return Err(service_problem(&error));
        }
    };
    let identity = selected.identity.clone();
    record_audit(
        &state,
        &ExternalPluginWriteAudit {
            context: context.clone(),
            operation: "EXTERNAL_PLUGIN_RELEASE_SELECTED".to_string(),
            plugin_name: Some(identity.name.clone()),
            version: Some(identity.version.clone()),
            platform: Some(identity.platform.clone()),
            sha256: Some(identity.sha256.clone()),
            signer_key_id: Some(identity.signer_key_id.clone()),
            registry_source: Some(identity.registry_source.clone()),
            failure: None,
        },
    )
    .await;
    let outcome = match state.service.install_selected(selected).await {
        Ok(outcome) => outcome,
        Err(error) => {
            record_audit(
                &state,
                &ExternalPluginWriteAudit {
                    context,
                    operation: "EXTERNAL_PLUGIN_INSTALL_FAILED".to_string(),
                    plugin_name: Some(identity.name),
                    version: Some(identity.version),
                    platform: Some(identity.platform),
                    sha256: Some(identity.sha256),
                    signer_key_id: Some(identity.signer_key_id),
                    registry_source: Some(identity.registry_source),
                    failure: Some(public_error_detail(&error)),
                },
            )
            .await;
            return Err(service_problem(&error));
        }
    };
    record_audit(
        &state,
        &ExternalPluginWriteAudit {
            context,
            operation: "EXTERNAL_PLUGIN_INSTALLED".to_string(),
            plugin_name: Some(outcome.name.clone()),
            version: Some(outcome.version.clone()),
            platform: Some(outcome.platform.clone()),
            sha256: Some(outcome.sha256.clone()),
            signer_key_id: Some(outcome.signer_key_id.clone()),
            registry_source: Some(outcome.registry_source.clone()),
            failure: None,
        },
    )
    .await;
    Ok(Json(InstallPluginResponse {
        name: outcome.name.clone(),
        version: outcome.version,
        platform: outcome.platform,
        sha256: outcome.sha256,
        message: format!(
            "Plugin '{}' was verified, installed, and started",
            outcome.name
        ),
    }))
}

#[utoipa::path(
    tag = "External Plugins",
    get,
    path = "/x/plugins/{name}/status",
    params(("name" = String, Path)),
    responses((status = 200, body = PluginStatusResponse)),
    security(("bearer_auth" = []))
)]
async fn get_plugin_status(
    RequireAuth(auth): RequireAuth,
    State(state): State<ExternalPluginsAppState>,
    Path(name): Path<String>,
) -> Result<Json<PluginStatusResponse>, Problem> {
    permission_guard!(auth, SystemAdmin);
    crate::install::validate_plugin_name(&name)
        .map_err(|error| service_problem(&ExternalPluginsError::Install(error)))?;
    let configured = state.service.manager().is_running(&name).await;
    Ok(Json(PluginStatusResponse {
        configured,
        reason: (!configured)
            .then(|| format!("Plugin '{name}' is not running from a verified active installation")),
        setup_path: "/settings/plugins".to_string(),
    }))
}

/// Build the router for external plugin management endpoints.
pub fn configure_routes() -> Router<ExternalPluginsAppState> {
    Router::new()
        .route("/x/plugins", get(list_external_plugins))
        .route("/x/plugins/reload", post(reload_plugins))
        .route("/x/plugins/catalog", get(list_plugin_catalog))
        .route("/x/plugins/install", post(install_plugin))
        .route("/x/plugins/{name}/status", get(get_plugin_status))
}

#[derive(OpenApiTrait)]
#[openapi(
    paths(
        list_external_plugins,
        reload_plugins,
        list_plugin_catalog,
        install_plugin,
        get_plugin_status,
    ),
    components(
        schemas(
            PluginManifest,
            NavEntry,
            NavSection,
            UiManifest,
            UiRoute,
            ReloadResponse,
            ReloadFailureResponse,
            crate::catalog::RegistryEnvelope,
            crate::catalog::RegistryPlugin,
            crate::catalog::PlatformRelease,
            InstallPluginRequest,
            InstallPluginResponse,
            PluginCatalogResponse,
            PluginStatusResponse,
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

    use chrono::Utc;
    use temps_auth::context::AuthContext;
    use temps_auth::permissions::Role;
    use temps_entities::users;

    use crate::manager::ExternalPluginConfig;

    struct NoopAuditLogger;

    struct AllowSensitiveActions;
    struct RequireSensitiveVerification;

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
        ) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct RecordingAuditLogger {
        operations: std::sync::Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl temps_core::AuditLogger for RecordingAuditLogger {
        async fn create_audit_log(
            &self,
            operation: &dyn temps_core::audit::AuditOperation,
        ) -> anyhow::Result<()> {
            self.operations
                .lock()
                .expect("audit operation lock")
                .push(operation.operation_type());
            Ok(())
        }
    }

    fn mock_db() -> Arc<sea_orm::DatabaseConnection> {
        Arc::new(sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection())
    }

    fn test_state() -> ExternalPluginsAppState {
        let config = ExternalPluginConfig::new(
            std::env::temp_dir().join("temps-external-plugins-handler-test"),
            "postgres://localhost/test".to_string(),
        );
        ExternalPluginsAppState {
            service: Arc::new(ExternalPluginsService::new_empty(config, None, mock_db())),
            audit_service: Arc::new(NoopAuditLogger),
            sensitive_action_authorizer: Arc::new(AllowSensitiveActions),
        }
    }

    fn test_state_with_audit(
        audit_service: Arc<dyn temps_core::AuditLogger>,
    ) -> ExternalPluginsAppState {
        let mut state = test_state();
        state.audit_service = audit_service;
        state
    }

    fn metadata() -> Extension<temps_core::RequestMetadata> {
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

    fn user_auth(role: Role) -> RequireAuth {
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
        let config = ExternalPluginConfig::new(
            temp.path().to_path_buf(),
            "postgres://localhost/test".to_string(),
        );
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
    async fn test_reload_plugins_all_failed_returns_bad_gateway_and_failure_audit() {
        // Arrange
        let temp = tempfile::tempdir().expect("tempdir");
        let config = ExternalPluginConfig::new(
            temp.path().to_path_buf(),
            "postgres://localhost/test".to_string(),
        );
        std::fs::create_dir_all(config.plugins_dir.join("broken-plugin"))
            .expect("broken active plugin directory");
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
                .expect("reload reports individual failures in a typed response");

        // Assert
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(response.loaded, 0);
        assert_eq!(response.failures.len(), 1);
        assert_eq!(
            response.failures[0].reason,
            "Activated plugin installation failed verification"
        );
        assert_eq!(
            *audit.operations.lock().expect("audit operation lock"),
            vec!["EXTERNAL_PLUGINS_RELOAD_FAILED".to_string()]
        );
    }

    #[tokio::test]
    async fn test_list_plugin_catalog_non_admin_returns_forbidden() {
        // Arrange
        let state = test_state();

        // Act
        let error = list_plugin_catalog(user_auth(Role::User), State(state))
            .await
            .expect_err("catalogue access must require system administration");

        // Assert
        assert_eq!(error.status_code, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn test_install_plugin_non_admin_returns_forbidden_without_audit() {
        // Arrange
        let audit = Arc::new(RecordingAuditLogger::default());
        let state = test_state_with_audit(audit.clone());

        // Act
        let error = install_plugin(
            user_auth(Role::User),
            State(state),
            metadata(),
            Json(InstallPluginRequest {
                name: "safe-plugin".to_string(),
            }),
        )
        .await
        .expect_err("install must require system administration");

        // Assert
        assert_eq!(error.status_code, StatusCode::FORBIDDEN);
        assert!(
            audit
                .operations
                .lock()
                .expect("audit operation lock")
                .is_empty(),
            "a rejected caller must not create an install audit entry"
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
    async fn failed_install_records_attempt_and_failure() {
        let audit = Arc::new(RecordingAuditLogger::default());
        let state = test_state_with_audit(audit.clone());
        let error = install_plugin(
            user_auth(Role::PlatformAdmin),
            State(state),
            metadata(),
            Json(InstallPluginRequest {
                name: "../../escape".to_string(),
            }),
        )
        .await
        .expect_err("unsafe plugin name must fail");
        assert_eq!(error.status_code, StatusCode::BAD_REQUEST);
        assert_eq!(
            *audit.operations.lock().expect("audit operation lock"),
            vec![
                "EXTERNAL_PLUGIN_INSTALL_REQUESTED".to_string(),
                "EXTERNAL_PLUGIN_INSTALL_FAILED".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn install_requires_sensitive_action_verification_before_audit_or_execution() {
        let audit = Arc::new(RecordingAuditLogger::default());
        let mut state = test_state_with_audit(audit.clone());
        state.sensitive_action_authorizer = Arc::new(RequireSensitiveVerification);
        let error = install_plugin(
            user_auth(Role::PlatformAdmin),
            State(state),
            metadata(),
            Json(InstallPluginRequest {
                name: "safe".to_string(),
            }),
        )
        .await
        .expect_err("step-up verification must be required");
        assert_eq!(error.status_code, StatusCode::PRECONDITION_REQUIRED);
        assert!(
            audit
                .operations
                .lock()
                .expect("audit operation lock")
                .is_empty(),
            "install audit must not claim an attempt before authorization succeeds"
        );
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
    fn openapi_spec_has_catalog_install_and_status_paths() {
        let spec = ExternalPluginsApiDoc::openapi();
        for path in [
            "/x/plugins/catalog",
            "/x/plugins/install",
            "/x/plugins/{name}/status",
        ] {
            assert!(spec.paths.paths.contains_key(path), "missing {path}");
        }
    }

    #[test]
    fn install_request_rejects_remote_control_fields() {
        for body in [
            r#"{"name":"safe","url":"https://example.com/plugin"}"#,
            r#"{"name":"safe","path":"../../plugin"}"#,
            r#"{"name":"safe","sha256":"00"}"#,
            r#"{"name":"safe","version":"1.0.0"}"#,
        ] {
            assert!(serde_json::from_str::<InstallPluginRequest>(body).is_err());
        }
        assert!(serde_json::from_str::<InstallPluginRequest>(r#"{"name":"safe"}"#).is_ok());
    }

    #[test]
    fn test_service_problem_untrusted_registry_key_returns_authentication_bad_gateway() {
        // Arrange
        let error = ExternalPluginsError::Catalog(crate::catalog::CatalogError::UntrustedKey {
            url: "https://registry.temps.sh/api/plugins".to_string(),
            key_id: "rotated-without-anchor".to_string(),
        });

        // Act
        let problem = service_problem(&error);

        // Assert
        assert_eq!(problem.status_code, StatusCode::BAD_GATEWAY);
        assert_eq!(
            problem
                .body
                .get("title")
                .and_then(serde_json::Value::as_str),
            Some("Plugin Registry Authentication Failed")
        );
        assert!(problem
            .body
            .get("detail")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|detail| detail.contains("rotated-without-anchor")));
    }

    #[test]
    fn test_service_problem_and_audit_detail_hide_local_install_paths() {
        let secret_path = "/srv/temps/private/plugins/example/receipt.json";
        let sentinel_secret = "postgres://admin:must-not-leak@example.test/temps";
        let error = ExternalPluginsError::Install(crate::install::InstallError::InvalidReceipt {
            plugin: "example".to_string(),
            path: secret_path.to_string(),
            reason: sentinel_secret.to_string(),
        });

        let public_detail = public_error_detail(&error);
        let problem = service_problem(&error);
        let audit = ExternalPluginWriteAudit {
            context: temps_core::audit::AuditContext {
                user_id: 1,
                ip_address: Some("192.0.2.1".to_string()),
                user_agent: "test".to_string(),
            },
            operation: "EXTERNAL_PLUGIN_INSTALL_FAILED".to_string(),
            plugin_name: Some("example".to_string()),
            version: None,
            platform: None,
            sha256: None,
            signer_key_id: None,
            registry_source: None,
            failure: Some(public_detail.clone()),
        };
        let serialized_audit = temps_core::audit::AuditOperation::serialize(&audit)
            .expect("safe audit event must serialize");

        assert!(!public_detail.contains(secret_path));
        assert!(!public_detail.contains(sentinel_secret));
        assert!(!serialized_audit.contains(secret_path));
        assert!(!serialized_audit.contains(sentinel_secret));
        assert_eq!(
            problem
                .body
                .get("detail")
                .and_then(serde_json::Value::as_str),
            Some(public_detail.as_str())
        );
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
}
