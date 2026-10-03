// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! HTTP handlers for managing the preview gateway from the settings UI.
//!
//! Mounted under `/api/preview-gateway`. All endpoints require
//! `Permission::SettingsWrite`.
//!
//! These handlers are deliberately thin — every meaningful operation lives
//! in `crate::preview_gateway`. The handlers just adapt between HTTP DTOs,
//! the Docker handle (held on `AppState`), and the database-backed settings.

use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::{Extension, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use temps_auth::{permission_guard, RequireAuth};
use temps_core::{
    audit::{AuditContext, AuditOperation},
    problemdetails::{Problem, ProblemDetails},
    PreviewGatewaySettings, RequestMetadata,
};
use tracing::{error, info};
use utoipa::ToSchema;

use crate::handlers::AppState;
use crate::preview_gateway::{
    self, sanitize_gateway_diagnostic, GatewayName, GatewayStatus, OperationsLock,
    PreviewGatewayError, PreviewGatewaySpec, DEFAULT_PREVIEW_GATEWAY_HOST_PORT,
    PREVIEW_GATEWAY_CONTAINER, PREVIEW_GATEWAY_IMAGE,
};

/// How long a request waits for a gateway operation already running in this
/// process (the startup reconciliation pulling an image can take a while)
/// before it is refused as busy instead of left hanging.
const OPERATION_WAIT: Duration = Duration::from_secs(30);

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/preview-gateway/status", get(get_preview_gateway_status))
        .route("/preview-gateway/logs", get(get_preview_gateway_logs))
        .route("/preview-gateway/restart", post(restart_preview_gateway))
        .route("/preview-gateway/upgrade", post(upgrade_preview_gateway))
        .route(
            "/preview-gateway/settings",
            get(get_preview_gateway_settings).patch(patch_preview_gateway_settings),
        )
}

// ─── DTOs ───────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct LogsQuery {
    /// Number of lines to return from the tail. Defaults to 200, capped at 2000.
    #[serde(default)]
    pub tail: Option<usize>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct PreviewGatewayLogsResponse {
    pub lines: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct UpgradeRequest {
    /// Image reference to pull and run (e.g.
    /// an immutable `ghcr.io/gotempsh/temps-preview-gateway@sha256:…` reference).
    /// Empty resets to default.
    pub image: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct PreviewGatewaySettingsResponse {
    /// Whether Temps runs the gateway. While false its containers are
    /// removed and workspace preview URLs are not served.
    pub enabled: bool,
    pub image: String,
    pub host_port: u16,
    pub auto_upgrade: bool,
    /// Docker container name of this instance's gateway.
    pub container_name: String,
    /// The compile-time default image — exposed so the UI can offer a
    /// "Reset to default" link without round-tripping.
    pub default_image: String,
    /// The compile-time default host port.
    pub default_host_port: u16,
    /// The default container name. Only installs that share one Docker
    /// daemon with another Temps instance need a different one.
    pub default_container_name: String,
}

impl From<PreviewGatewaySettings> for PreviewGatewaySettingsResponse {
    fn from(s: PreviewGatewaySettings) -> Self {
        Self {
            enabled: s.enabled,
            container_name: preview_gateway::container_name(&s),
            image: s.image,
            host_port: s.host_port,
            auto_upgrade: s.auto_upgrade,
            default_image: PREVIEW_GATEWAY_IMAGE.to_string(),
            default_host_port: DEFAULT_PREVIEW_GATEWAY_HOST_PORT,
            default_container_name: PREVIEW_GATEWAY_CONTAINER.to_string(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct PatchSettingsRequest {
    /// Turn the gateway off (its containers are removed at once, so preview
    /// URLs stop being served) or on (it is created again).
    pub enabled: Option<bool>,
    pub image: Option<String>,
    pub host_port: Option<u16>,
    pub auto_upgrade: Option<bool>,
    /// Docker container name for this instance's gateway; empty resets it
    /// to the default. Change it only when several Temps instances share one
    /// Docker daemon: each needs its own name and host port. A new name
    /// first removes this instance's gateway under the old one, and while
    /// the gateway is enabled it is then created under the new one. Refused
    /// while this host has sandboxes: their networks keep the current name.
    pub container_name: Option<String>,
}

// ─── Handlers ───────────────────────────────────────────────────────────────

#[utoipa::path(
    tag = "Preview Gateway",
    get,
    path = "/preview-gateway/status",
    responses(
        (status = 200, body = GatewayStatus),
        (status = 500, description = "Docker status request failed", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_preview_gateway_status(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);
    let settings = preview_gateway::load_settings(&state.db).await;
    let status = preview_gateway::inspect_status(&state.docker, &settings)
        .await
        .map_err(|e| {
            let detail = anyhow_detail(
                "failed to inspect preview gateway",
                &e,
                &[&settings.shared_secret],
            );
            error!(error = %detail, "preview gateway status failed");
            internal(detail)
        })?;
    Ok(Json(status))
}

#[utoipa::path(
    tag = "Preview Gateway",
    get,
    path = "/preview-gateway/logs",
    params(("tail" = Option<usize>, Query, description = "Lines to tail (default 200, max 2000)")),
    responses(
        (status = 200, body = PreviewGatewayLogsResponse),
        (status = 500, description = "Docker log request failed", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_preview_gateway_logs(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Query(q): Query<LogsQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);
    let tail = q.tail.unwrap_or(200).min(2000);
    // Resolve the container from settings, not the default name — otherwise
    // an instance with a custom container tails a container it doesn't own
    // (or, more likely, none at all).
    let settings = preview_gateway::load_settings(&state.db).await;
    let lines = preview_gateway::tail_logs(
        &state.docker,
        &preview_gateway::container_name(&settings),
        tail,
    )
    .await
    .map_err(|e| {
        internal(anyhow_detail(
            "failed to tail logs",
            &e,
            &[&settings.shared_secret],
        ))
    })?;
    Ok(Json(PreviewGatewayLogsResponse { lines }))
}

#[utoipa::path(
    tag = "Preview Gateway",
    post,
    path = "/preview-gateway/restart",
    responses(
        (status = 204, description = "Gateway restarted"),
        (status = 409, description = "The gateway is disabled in settings, or another gateway operation is still running", body = ProblemDetails),
        (status = 500, description = "Gateway restart failed", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn restart_preview_gateway(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);
    // Held from before the settings are read until the restart is done, so
    // a disable saved meanwhile waits for it and then removes the result.
    let held = wait_for_gateway_operations("restart the preview gateway", OPERATION_WAIT).await?;
    let settings = preview_gateway::load_settings(&state.db).await;
    require_enabled(&settings, "restart")?;
    let spec = PreviewGatewaySpec::from_settings(&settings);
    info!(
        user_id = auth.user_id(),
        image = %spec.image,
        "preview gateway restart requested"
    );
    preview_gateway::force_restart(&held, state.docker.clone(), &state.db, spec)
        .await
        .map_err(|e| {
            internal(anyhow_detail(
                "restart failed",
                &e,
                &[&settings.shared_secret],
            ))
        })?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    tag = "Preview Gateway",
    post,
    path = "/preview-gateway/upgrade",
    request_body = UpgradeRequest,
    responses(
        (status = 204, description = "Gateway upgraded"),
        (status = 409, description = "The gateway is disabled in settings, or another gateway operation is still running", body = ProblemDetails),
        (status = 500, description = "Gateway upgrade failed", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn upgrade_preview_gateway(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Json(body): Json<UpgradeRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);
    // Held through the image save and the reconcile: see the restart above.
    let held = wait_for_gateway_operations("upgrade the preview gateway", OPERATION_WAIT).await?;
    require_enabled(&preview_gateway::load_settings(&state.db).await, "upgrade")?;

    let new_image = if body.image.trim().is_empty() {
        String::new()
    } else {
        body.image.trim().to_string()
    };
    let safe_image = sanitize_gateway_diagnostic(&new_image, &[]);
    info!(
        user_id = auth.user_id(),
        image = %safe_image,
        "preview gateway upgrade requested"
    );

    let settings = state
        .platform_config_service
        .update_preview_gateway_settings(|gateway| gateway.image = new_image.clone())
        .await
        .map_err(|e| {
            internal(sanitize_gateway_diagnostic(
                &format!("failed to persist new image: {e}"),
                &[],
            ))
        })?;
    let spec = PreviewGatewaySpec::from_settings(&settings);
    preview_gateway::reconcile(&held, state.docker.clone(), &state.db, spec)
        .await
        .map_err(|e| {
            internal(anyhow_detail(
                "reconcile failed",
                &e,
                &[&settings.shared_secret],
            ))
        })?;

    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    tag = "Preview Gateway",
    get,
    path = "/preview-gateway/settings",
    responses((status = 200, body = PreviewGatewaySettingsResponse)),
    security(("bearer_auth" = []))
)]
pub async fn get_preview_gateway_settings(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);
    let settings = preview_gateway::load_settings(&state.db).await;
    Ok(Json(PreviewGatewaySettingsResponse::from(settings)))
}

#[utoipa::path(
    tag = "Preview Gateway",
    patch,
    path = "/preview-gateway/settings",
    request_body = PatchSettingsRequest,
    responses(
        (status = 200, body = PreviewGatewaySettingsResponse),
        (status = 400, description = "The container name is not one Docker accepts", body = ProblemDetails),
        (status = 409, description = "Another gateway operation is still running, or the container name cannot change while this host has sandboxes", body = ProblemDetails),
        (status = 500, description = "Saving the settings failed, or applying them to the gateway's containers failed", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn patch_preview_gateway_settings(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(patch): Json<PatchSettingsRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);
    let container_name = submitted_container_name(patch.container_name.as_deref())?;

    // Saved and applied under one lock: a restart, upgrade or startup
    // reconciliation in flight finishes first, and none can start between
    // this save and the gateway being switched on or off.
    let held =
        wait_for_gateway_operations("save the preview gateway settings", OPERATION_WAIT).await?;
    let previous = preview_gateway::load_settings(&state.db).await;
    let current_name = preview_gateway::container_name(&previous);
    let renaming = container_name
        .as_deref()
        .is_some_and(|name| name != current_name);

    // The sandbox provider's copy of the name, held from the sandbox check
    // until the new name is set: a sandbox created meanwhile waits, then
    // labels its network with the new name.
    let mut provider_name = None;
    if renaming {
        provider_name = Some(
            wait_for_sandbox_network_setup(&state.preview_gateway_name, OPERATION_WAIT).await?,
        );
        let sandboxes = preview_gateway::count_live_sandboxes(&state.db)
            .await
            .map_err(rename_check_failed)?;
        // Sandboxes without a row, such as credential checks, are known only
        // to the provider that set up their networks.
        let networks = if sandboxes == 0 {
            preview_gateway::sandbox_networks_still_present(
                &state.docker,
                state.preview_gateway_name.networks_set_up().await,
            )
            .await
            .map_err(rename_check_failed)?
        } else {
            Vec::new()
        };
        refuse_rename_while_in_use(&current_name, sandboxes, &networks)?;

        // The pair under the previous name still publishes the host port, so
        // it goes before the new name is saved. If removing it fails nothing
        // has changed yet, and saving again retries.
        preview_gateway::remove_renamed(&held, &state.docker, &previous)
            .await
            .map_err(|e| {
                internal(gateway_error_detail(
                    "removing the gateway under its previous container name failed, so the settings were not saved; save them again to retry",
                    &e,
                    &[&previous.shared_secret],
                ))
            })?;
    }

    let settings = state
        .platform_config_service
        .update_preview_gateway_settings(|gateway| {
            if let Some(enabled) = patch.enabled {
                gateway.enabled = enabled;
            }
            if let Some(image) = patch.image.clone() {
                gateway.image = image;
            }
            if let Some(host_port) = patch.host_port {
                gateway.host_port = host_port;
            }
            if let Some(auto_upgrade) = patch.auto_upgrade {
                gateway.auto_upgrade = auto_upgrade;
            }
            if let Some(name) = container_name {
                gateway.container_name = name;
            }
        })
        .await
        .map_err(|e| {
            internal(sanitize_gateway_diagnostic(
                &format!("failed to persist settings: {e}"),
                &[],
            ))
        })?;
    if let Some(mut name) = provider_name {
        *name = preview_gateway::container_name(&settings);
    }

    // `enabled` is an instance-wide kill switch for preview traffic, so who
    // flipped it (and every other gateway setting) must be on record. A failed
    // audit write is logged but does not undo the saved settings.
    let audit = PreviewGatewaySettingsUpdatedAudit::new(
        AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        &previous,
        &settings,
    );
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!(
            user_id = auth.user_id(),
            "Failed to create audit log for preview gateway settings update: {}", e
        );
    }

    // Apply the switch now rather than at the next server start: a disabled
    // gateway must stop serving previews at once.
    match switch_effect(&previous, &settings) {
        Some(SwitchEffect::Remove) => preview_gateway::disable(&held, &state.docker, &settings)
            .await
            .map_err(|e| {
                internal(gateway_error_detail(
                    "the settings were saved, but removing the disabled gateway failed, so workspace preview URLs may still be served; save the settings again to retry",
                    &e,
                    &[&settings.shared_secret],
                ))
            })?,
        Some(SwitchEffect::Reconcile) => preview_gateway::reconcile(
            &held,
            state.docker.clone(),
            &state.db,
            PreviewGatewaySpec::from_settings(&settings),
        )
        .await
        .map_err(|e| {
            internal(anyhow_detail(
                "the settings were saved, but starting the enabled gateway failed; restart it once the cause is fixed",
                &e,
                &[&settings.shared_secret],
            ))
        })?,
        None => {}
    }

    Ok(Json(PreviewGatewaySettingsResponse::from(settings)))
}

/// What a settings save does to the running gateway.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SwitchEffect {
    /// Remove its containers. Repeated on every save while it is disabled,
    /// so a removal that failed is retried.
    Remove,
    /// Create it again: it was disabled before this save, or it was renamed.
    Reconcile,
}

fn switch_effect(
    previous: &PreviewGatewaySettings,
    saved: &PreviewGatewaySettings,
) -> Option<SwitchEffect> {
    match (previous.enabled, saved.enabled) {
        (_, false) => Some(SwitchEffect::Remove),
        (false, true) => Some(SwitchEffect::Reconcile),
        (true, true)
            if preview_gateway::container_name(previous)
                != preview_gateway::container_name(saved) =>
        {
            Some(SwitchEffect::Reconcile)
        }
        (true, true) => None,
    }
}

/// The container name a settings save asks for, checked before anything is
/// changed: `None` keeps the stored name, an empty one restores the default.
fn submitted_container_name(submitted: Option<&str>) -> Result<Option<String>, Problem> {
    submitted
        .map(preview_gateway::validated_container_name)
        .transpose()
        .map_err(|error| {
            temps_core::error_builder::bad_request()
                .title("Invalid preview gateway settings")
                .detail(error.to_string())
                .build()
        })
}

/// Take the gateway operations lock for `action`, waiting up to `wait` for
/// an operation already running in this process to finish first.
async fn wait_for_gateway_operations(
    action: &str,
    wait: Duration,
) -> Result<OperationsLock, Problem> {
    tokio::time::timeout(wait, preview_gateway::lock_operations())
        .await
        .map_err(|_| {
            temps_core::error_builder::conflict()
                .title("Preview gateway busy")
                .detail(format!(
                    "Cannot {action}: another preview gateway operation (the startup reconciliation, a restart, an upgrade or a settings save) was still running after {} seconds. Try again once it finishes.",
                    wait.as_secs()
                ))
                .build()
        })
}

/// Take the sandbox provider's gateway name for a rename, waiting up to
/// `wait` for a sandbox network being set up to finish first.
async fn wait_for_sandbox_network_setup(
    name: &GatewayName,
    wait: Duration,
) -> Result<tokio::sync::RwLockWriteGuard<'_, String>, Problem> {
    tokio::time::timeout(wait, name.write())
        .await
        .map_err(|_| {
            temps_core::error_builder::conflict()
                .title("Preview gateway busy")
                .detail(format!(
                    "Cannot rename the preview gateway: a sandbox's network was still being set up after {} seconds. Try again once it finishes.",
                    wait.as_secs()
                ))
                .build()
        })
}

/// Refuse a rename while this instance has sandbox networks on this host:
/// `sandboxes` with a row, or `networks` its provider set up for sandboxes
/// without one. Each network is labelled with the gateway's current name,
/// Docker cannot relabel a network, and the gateway under a new name would
/// never join it again.
fn refuse_rename_while_in_use(
    current_name: &str,
    sandboxes: u64,
    networks: &[String],
) -> Result<(), Problem> {
    let detail = if sandboxes > 0 {
        let (existing, labelled, them) = if sandboxes == 1 {
            ("1 sandbox".to_string(), "its network is", "the sandbox")
        } else {
            (
                format!("{sandboxes} sandboxes"),
                "their networks are",
                "the sandboxes",
            )
        };
        format!(
            "Cannot rename the preview gateway container '{current_name}': this host has {existing}, and {labelled} labelled with the current name, which Docker cannot change. Delete {them} first, or keep the current name."
        )
    } else if let Some(first) = networks.first() {
        let (labelled, users) = match networks.len() - 1 {
            0 => (
                format!("sandbox network '{first}' is"),
                "It belongs to a sandbox that is not listed in Temps, such as a credential check in progress; try again once it finishes. If that sandbox is gone, remove the network with `docker network rm`.",
            ),
            more => (
                format!("sandbox networks '{first}' and {more} more are"),
                "They belong to sandboxes that are not listed in Temps, such as credential checks in progress; try again once they finish. If those sandboxes are gone, remove the networks with `docker network rm`.",
            ),
        };
        format!(
            "Cannot rename the preview gateway container '{current_name}': {labelled} still labelled with the current name, which Docker cannot change. {users}"
        )
    } else {
        return Ok(());
    };
    Err(temps_core::error_builder::conflict()
        .title("Preview gateway in use")
        .detail(detail)
        .build())
}

/// A failed check for this instance's sandboxes leaves the name unchanged.
fn rename_check_failed(error: preview_gateway::PreviewGatewayOwnershipError) -> Problem {
    internal(sanitize_gateway_diagnostic(
        &format!(
            "cannot rename the preview gateway: {}",
            preview_gateway::error_chain(&error)
        ),
        &[],
    ))
}

/// Refuse an operation that would run the gateway while it is disabled.
fn require_enabled(settings: &PreviewGatewaySettings, operation: &str) -> Result<(), Problem> {
    if settings.enabled {
        return Ok(());
    }
    Err(temps_core::error_builder::conflict()
        .title("Preview gateway disabled")
        .detail(format!(
            "Cannot {operation} the preview gateway: it is disabled in the preview gateway settings, so workspace preview URLs are not served. Enable it there first."
        ))
        .build())
}

/// One audited field change: the value before and after the save.
#[derive(Debug, Clone, Serialize)]
struct SettingChange<T> {
    previous: T,
    new: T,
}

#[derive(Debug, Clone, Serialize)]
struct PreviewGatewaySettingsUpdatedAudit {
    context: AuditContext,
    enabled: SettingChange<bool>,
    image: SettingChange<String>,
    host_port: SettingChange<u16>,
    auto_upgrade: SettingChange<bool>,
    container_name: SettingChange<String>,
}

impl PreviewGatewaySettingsUpdatedAudit {
    fn new(
        context: AuditContext,
        previous: &PreviewGatewaySettings,
        new: &PreviewGatewaySettings,
    ) -> Self {
        Self {
            context,
            enabled: SettingChange {
                previous: previous.enabled,
                new: new.enabled,
            },
            image: SettingChange {
                previous: previous.image.clone(),
                new: new.image.clone(),
            },
            host_port: SettingChange {
                previous: previous.host_port,
                new: new.host_port,
            },
            auto_upgrade: SettingChange {
                previous: previous.auto_upgrade,
                new: new.auto_upgrade,
            },
            container_name: SettingChange {
                previous: preview_gateway::container_name(previous),
                new: preview_gateway::container_name(new),
            },
        }
    }
}

impl AuditOperation for PreviewGatewaySettingsUpdatedAudit {
    fn operation_type(&self) -> String {
        "PREVIEW_GATEWAY_SETTINGS_UPDATED".to_string()
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
    fn serialize(&self) -> temps_core::anyhow::Result<String> {
        serde_json::to_string(self).map_err(|error| {
            temps_core::anyhow::anyhow!(
                "failed to serialize preview gateway settings audit for user {}: {error}",
                self.context.user_id
            )
        })
    }
}

fn internal(detail: String) -> Problem {
    use temps_core::error_builder;
    error_builder::internal_server_error()
        .title("Preview gateway error")
        .detail(detail)
        .build()
}

/// Render every `anyhow` context and source at the HTTP boundary. `Display`
/// (`{}`) only includes the outermost context, which hides Docker daemon
/// diagnostics such as a failed host-port bind added below
/// `create_and_start`'s operation context.
fn anyhow_detail(operation: &str, error: &anyhow::Error, sensitive_values: &[&str]) -> String {
    sanitize_gateway_diagnostic(&format!("{operation}: {error:#}"), sensitive_values)
}

/// [`anyhow_detail`] for a typed gateway error: its `Display` names the
/// container, and the Docker error that caused it follows.
fn gateway_error_detail(
    operation: &str,
    error: &PreviewGatewayError,
    sensitive_values: &[&str],
) -> String {
    sanitize_gateway_diagnostic(
        &format!("{operation}: {}", preview_gateway::error_chain(error)),
        sensitive_values,
    )
}

#[cfg(test)]
mod tests {
    use anyhow::anyhow;
    use axum::{body::to_bytes, http::header::CONTENT_TYPE, response::IntoResponse};

    use super::*;

    #[test]
    fn settings_response_exposes_empty_default_without_turning_it_into_a_pin() {
        let settings = PreviewGatewaySettings::default();
        assert!(settings.image.is_empty());
        let response = PreviewGatewaySettingsResponse::from(settings);
        assert!(response.image.is_empty());
        assert_eq!(response.default_image, PREVIEW_GATEWAY_IMAGE);
        assert_eq!(response.container_name, PREVIEW_GATEWAY_CONTAINER);
        assert_eq!(response.default_container_name, PREVIEW_GATEWAY_CONTAINER);

        // A stored empty name is reported as the name actually in use.
        let blank_name = PreviewGatewaySettings {
            container_name: String::new(),
            ..PreviewGatewaySettings::default()
        };
        assert_eq!(
            PreviewGatewaySettingsResponse::from(blank_name).container_name,
            PREVIEW_GATEWAY_CONTAINER
        );

        let explicit = PreviewGatewaySettings {
            image: "ghcr.io/operator/gateway@sha256:old-digest".into(),
            ..PreviewGatewaySettings::default()
        };
        let response = PreviewGatewaySettingsResponse::from(explicit);
        assert_eq!(response.image, "ghcr.io/operator/gateway@sha256:old-digest");
    }

    #[tokio::test]
    async fn internal_anyhow_problem_preserves_complete_error_chain() {
        let error = anyhow!(
            "Docker daemon rejected the port mapping: address already in use; registry https://user:password@example.test/image?token=query-value; Authorization=header-value; shared fixture-secret"
        )
            .context("failed to start container preview-gateway");

        let response =
            internal(anyhow_detail("restart failed", &error, &["fixture-secret"])).into_response();

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("application/problem+json")
        );

        let body = to_bytes(response.into_body(), 4096)
            .await
            .expect("problem response body should be readable");
        let json: serde_json::Value =
            serde_json::from_slice(&body).expect("problem response should be JSON");

        assert_eq!(json["title"], "Preview gateway error");
        let detail = json["detail"]
            .as_str()
            .expect("problem detail should be a string");
        assert!(detail.starts_with(
            "restart failed: failed to start container preview-gateway: Docker daemon rejected the port mapping: address already in use"
        ));
        assert!(
            detail.contains("https://[redacted]@example.test/image?[redacted]"),
            "unexpected sanitized detail: {detail}"
        );
        assert!(detail.contains("Authorization=[redacted]"));
        assert!(!detail.contains("password"));
        assert!(!detail.contains("query-value"));
        assert!(!detail.contains("header-value"));
        assert!(!detail.contains("fixture-secret"));
    }

    #[test]
    fn saving_disabled_settings_removes_the_gateway_and_enabling_recreates_it() {
        let enabled = PreviewGatewaySettings::default();
        let disabled = PreviewGatewaySettings {
            enabled: false,
            ..PreviewGatewaySettings::default()
        };
        assert_eq!(
            switch_effect(&enabled, &disabled),
            Some(SwitchEffect::Remove)
        );
        // Saving again while disabled retries the removal.
        assert_eq!(
            switch_effect(&disabled, &disabled),
            Some(SwitchEffect::Remove)
        );
        assert_eq!(
            switch_effect(&disabled, &enabled),
            Some(SwitchEffect::Reconcile)
        );
        // An enabled gateway is not recreated by every settings save.
        assert_eq!(switch_effect(&enabled, &enabled), None);
    }

    #[test]
    fn renaming_an_enabled_gateway_recreates_it_under_the_new_name() {
        let enabled = PreviewGatewaySettings::default();
        let renamed = PreviewGatewaySettings {
            container_name: "temps-preview-gateway-2".into(),
            ..PreviewGatewaySettings::default()
        };
        assert_eq!(
            switch_effect(&enabled, &renamed),
            Some(SwitchEffect::Reconcile)
        );
        // A blank name is the default one, so saving it renames nothing.
        let blank = PreviewGatewaySettings {
            container_name: String::new(),
            ..PreviewGatewaySettings::default()
        };
        assert_eq!(switch_effect(&enabled, &blank), None);
        // A gateway renamed while it is turned off is only removed.
        let renamed_and_disabled = PreviewGatewaySettings {
            enabled: false,
            ..renamed
        };
        assert_eq!(
            switch_effect(&enabled, &renamed_and_disabled),
            Some(SwitchEffect::Remove)
        );
    }

    #[tokio::test]
    async fn an_invalid_container_name_is_refused_before_anything_changes() {
        assert_eq!(submitted_container_name(None).ok(), Some(None));
        assert_eq!(
            submitted_container_name(Some(" temps-preview-gateway-2 "))
                .ok()
                .flatten()
                .as_deref(),
            Some("temps-preview-gateway-2")
        );
        assert_eq!(
            submitted_container_name(Some("")).ok().flatten().as_deref(),
            Some(PREVIEW_GATEWAY_CONTAINER)
        );

        let response = submitted_container_name(Some("bad name"))
            .expect_err("a container name with a space")
            .into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read problem body");
        let problem: serde_json::Value = serde_json::from_slice(&body).expect("problem JSON");
        assert_eq!(problem["title"], "Invalid preview gateway settings");
        let detail = problem["detail"].as_str().expect("problem detail");
        assert!(detail.contains("\"bad name\""), "{detail}");
    }

    #[tokio::test]
    async fn operations_that_run_the_gateway_are_refused_while_it_is_disabled() {
        assert!(require_enabled(&PreviewGatewaySettings::default(), "restart").is_ok());

        let disabled = PreviewGatewaySettings {
            enabled: false,
            ..PreviewGatewaySettings::default()
        };
        let response = require_enabled(&disabled, "restart")
            .expect_err("a disabled gateway is not restarted")
            .into_response();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read problem body");
        let problem: serde_json::Value = serde_json::from_slice(&body).expect("problem JSON");
        let detail = problem["detail"].as_str().expect("problem detail");
        assert!(
            detail.contains("Cannot restart the preview gateway"),
            "{detail}"
        );
        assert!(detail.contains("Enable it there first"), "{detail}");
    }

    #[tokio::test]
    async fn a_request_behind_a_running_gateway_operation_is_refused_as_busy() {
        let running = preview_gateway::lock_operations().await;
        let response = match wait_for_gateway_operations(
            "restart the preview gateway",
            Duration::from_millis(20),
        )
        .await
        {
            Ok(_) => panic!("a second gateway operation ran beside the first"),
            Err(problem) => problem.into_response(),
        };
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read problem body");
        let problem: serde_json::Value = serde_json::from_slice(&body).expect("problem JSON");
        assert_eq!(problem["title"], "Preview gateway busy");
        let detail = problem["detail"].as_str().expect("problem detail");
        assert!(
            detail.contains("Cannot restart the preview gateway"),
            "{detail}"
        );
        assert!(detail.contains("Try again once it finishes"), "{detail}");

        drop(running);
        assert!(
            wait_for_gateway_operations("restart the preview gateway", Duration::from_secs(60))
                .await
                .is_ok(),
            "the next operation runs once the one in flight finishes"
        );
    }

    async fn problem_json(problem: Problem, status: StatusCode) -> serde_json::Value {
        let response = problem.into_response();
        assert_eq!(response.status(), status);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read problem body");
        serde_json::from_slice(&body).expect("problem JSON")
    }

    #[tokio::test]
    async fn a_rename_is_refused_while_this_host_has_sandboxes() {
        assert!(refuse_rename_while_in_use(PREVIEW_GATEWAY_CONTAINER, 0, &[]).is_ok());

        for (sandboxes, existing, delete) in [
            (
                1,
                "has 1 sandbox, and its network is labelled",
                "Delete the sandbox first",
            ),
            (
                3,
                "has 3 sandboxes, and their networks are labelled",
                "Delete the sandboxes first",
            ),
        ] {
            let problem = problem_json(
                refuse_rename_while_in_use(PREVIEW_GATEWAY_CONTAINER, sandboxes, &[])
                    .expect_err("a rename that would strand sandbox networks"),
                StatusCode::CONFLICT,
            )
            .await;
            assert_eq!(problem["title"], "Preview gateway in use");
            let detail = problem["detail"].as_str().expect("problem detail");
            assert!(detail.contains("'temps-preview-gateway'"), "{detail}");
            assert!(detail.contains(existing), "{detail}");
            assert!(detail.contains(delete), "{detail}");
        }
    }

    #[tokio::test]
    async fn a_rename_is_refused_while_a_sandbox_without_a_row_has_a_network() {
        let one = vec!["temps-sandbox-net-v3-credential-verify-1".to_string()];
        let problem = problem_json(
            refuse_rename_while_in_use(PREVIEW_GATEWAY_CONTAINER, 0, &one)
                .expect_err("a rename that would strand a credential check's network"),
            StatusCode::CONFLICT,
        )
        .await;
        assert_eq!(problem["title"], "Preview gateway in use");
        let detail = problem["detail"].as_str().expect("problem detail");
        assert!(
            detail.contains(
                "sandbox network 'temps-sandbox-net-v3-credential-verify-1' is still labelled"
            ),
            "{detail}"
        );
        assert!(detail.contains("try again once it finishes"), "{detail}");
        assert!(detail.contains("docker network rm"), "{detail}");

        let three = vec![
            "temps-sandbox-net-v3-a".to_string(),
            "temps-sandbox-net-v3-b".to_string(),
            "temps-sandbox-net-v3-c".to_string(),
        ];
        let problem = problem_json(
            refuse_rename_while_in_use(PREVIEW_GATEWAY_CONTAINER, 0, &three)
                .expect_err("a rename that would strand three networks"),
            StatusCode::CONFLICT,
        )
        .await;
        let detail = problem["detail"].as_str().expect("problem detail");
        assert!(
            detail.contains("sandbox networks 'temps-sandbox-net-v3-a' and 2 more are still"),
            "{detail}"
        );
        assert!(detail.contains("try again once they finish"), "{detail}");
    }

    #[tokio::test]
    async fn a_rename_behind_a_sandbox_network_being_set_up_is_refused_as_busy() {
        let renamer = GatewayName::default();
        let provider = renamer.clone();

        let setting_up = provider.read().await;
        let refused =
            match wait_for_sandbox_network_setup(&renamer, Duration::from_millis(20)).await {
                Ok(_) => panic!("renamed while a sandbox network was being set up"),
                Err(problem) => problem,
            };
        let problem = problem_json(refused, StatusCode::CONFLICT).await;
        assert_eq!(problem["title"], "Preview gateway busy");
        let detail = problem["detail"].as_str().expect("problem detail");
        assert!(
            detail.contains("Cannot rename the preview gateway"),
            "{detail}"
        );
        assert!(detail.contains("Try again once it finishes"), "{detail}");

        drop(setting_up);
        assert!(
            wait_for_sandbox_network_setup(&renamer, Duration::from_secs(60))
                .await
                .is_ok(),
            "the rename proceeds once the sandbox network is set up"
        );
    }

    #[test]
    fn typed_gateway_errors_reach_the_problem_detail_with_their_docker_cause() {
        let error = PreviewGatewayError::RemoveContainer {
            container: "temps-preview-gateway".to_string(),
            source: bollard::errors::Error::DockerResponseServerError {
                status_code: 500,
                message: "driver failed; shared fixture-secret".to_string(),
            },
        };

        let detail = gateway_error_detail(
            "removing the disabled gateway failed",
            &error,
            &["fixture-secret"],
        );

        assert!(
            detail.starts_with(
                "removing the disabled gateway failed: failed to remove preview gateway container temps-preview-gateway: Docker responded with status code 500: driver failed"
            ),
            "{detail}"
        );
        assert!(!detail.contains("fixture-secret"), "{detail}");
    }

    #[test]
    fn settings_audit_records_previous_and_new_values() {
        let previous = PreviewGatewaySettings {
            enabled: true,
            ..PreviewGatewaySettings::default()
        };
        let new = PreviewGatewaySettings {
            enabled: false,
            host_port: previous.host_port.wrapping_add(1),
            container_name: "temps-preview-gateway-2".into(),
            ..previous.clone()
        };
        let audit = PreviewGatewaySettingsUpdatedAudit::new(
            AuditContext {
                user_id: 9,
                ip_address: Some("203.0.113.7".into()),
                user_agent: "test-agent".into(),
            },
            &previous,
            &new,
        );

        assert_eq!(audit.operation_type(), "PREVIEW_GATEWAY_SETTINGS_UPDATED");
        assert_eq!(audit.user_id(), Some(9));
        let json: serde_json::Value =
            serde_json::from_str(&AuditOperation::serialize(&audit).expect("serialize audit"))
                .expect("audit is JSON");
        assert_eq!(json["enabled"]["previous"], true);
        assert_eq!(json["enabled"]["new"], false);
        assert_eq!(json["host_port"]["previous"], previous.host_port);
        assert_eq!(json["host_port"]["new"], new.host_port);
        assert_eq!(
            json["container_name"]["previous"],
            PREVIEW_GATEWAY_CONTAINER
        );
        assert_eq!(json["container_name"]["new"], "temps-preview-gateway-2");
    }

    #[test]
    fn gateway_diagnostics_are_bounded_before_the_http_boundary() {
        let error = anyhow!("{}", "x".repeat(8_000)).context("docker request failed");
        let detail = anyhow_detail("restart failed", &error, &[]);

        assert!(detail.chars().count() < 4_100);
        assert!(detail.contains("diagnostic truncated"));
    }
}
