//! HTTP handlers for managing the preview gateway from the settings UI.
//!
//! Mounted under `/api/preview-gateway`. All endpoints require
//! `Permission::SettingsWrite`.
//!
//! These handlers are deliberately thin — every meaningful operation lives
//! in `crate::preview_gateway`. The handlers just adapt between HTTP DTOs,
//! the Docker handle (held on `AppState`), and the database-backed settings.

use std::sync::Arc;

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use temps_auth::{permission_guard, RequireAuth};
use temps_core::{problemdetails::Problem, PreviewGatewaySettings};
use tracing::{error, info};
use utoipa::ToSchema;

use crate::handlers::AppState;
use crate::preview_gateway::{
    self, GatewayStatus, PreviewGatewaySpec, DEFAULT_PREVIEW_GATEWAY_HOST_PORT,
    PREVIEW_GATEWAY_IMAGE,
};

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
pub struct LogsResponse {
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
    pub image: String,
    pub host_port: u16,
    pub auto_upgrade: bool,
    /// The compile-time default image — exposed so the UI can offer a
    /// "Reset to default" link without round-tripping.
    pub default_image: String,
    /// The compile-time default host port.
    pub default_host_port: u16,
}

impl From<PreviewGatewaySettings> for PreviewGatewaySettingsResponse {
    fn from(s: PreviewGatewaySettings) -> Self {
        Self {
            image: s.image,
            host_port: s.host_port,
            auto_upgrade: s.auto_upgrade,
            default_image: PREVIEW_GATEWAY_IMAGE.to_string(),
            default_host_port: DEFAULT_PREVIEW_GATEWAY_HOST_PORT,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct PatchSettingsRequest {
    pub image: Option<String>,
    pub host_port: Option<u16>,
    pub auto_upgrade: Option<bool>,
}

// ─── Handlers ───────────────────────────────────────────────────────────────

#[utoipa::path(
    tag = "Preview Gateway",
    get,
    path = "/preview-gateway/status",
    responses((status = 200, body = GatewayStatus)),
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
            error!("preview gateway status failed: {}", e);
            internal(format!("failed to inspect preview gateway: {}", e))
        })?;
    Ok(Json(status))
}

#[utoipa::path(
    tag = "Preview Gateway",
    get,
    path = "/preview-gateway/logs",
    params(("tail" = Option<usize>, Query, description = "Lines to tail (default 200, max 2000)")),
    responses((status = 200, body = LogsResponse)),
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
    .map_err(|e| internal(format!("failed to tail logs: {}", e)))?;
    Ok(Json(LogsResponse { lines }))
}

#[utoipa::path(
    tag = "Preview Gateway",
    post,
    path = "/preview-gateway/restart",
    responses((status = 204, description = "Gateway restarted")),
    security(("bearer_auth" = []))
)]
pub async fn restart_preview_gateway(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);
    let settings = preview_gateway::load_settings(&state.db).await;
    let spec = PreviewGatewaySpec::from_settings(&settings);
    info!(
        user_id = auth.user_id(),
        image = %spec.image,
        "preview gateway restart requested"
    );
    preview_gateway::force_restart(state.docker.clone(), spec)
        .await
        .map_err(|e| internal(format!("restart failed: {}", e)))?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    tag = "Preview Gateway",
    post,
    path = "/preview-gateway/upgrade",
    request_body = UpgradeRequest,
    responses((status = 204, description = "Gateway upgraded")),
    security(("bearer_auth" = []))
)]
pub async fn upgrade_preview_gateway(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Json(body): Json<UpgradeRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);

    let new_image = if body.image.trim().is_empty() {
        PREVIEW_GATEWAY_IMAGE.to_string()
    } else {
        body.image.trim().to_string()
    };
    info!(
        user_id = auth.user_id(),
        image = %new_image,
        "preview gateway upgrade requested"
    );

    state
        .platform_config_service
        .update_setting_field(|s| s.preview_gateway.image = new_image.clone())
        .await
        .map_err(|e| internal(format!("failed to persist new image: {}", e)))?;

    let settings = preview_gateway::load_settings(&state.db).await;
    let spec = PreviewGatewaySpec::from_settings(&settings);
    preview_gateway::reconcile(state.docker.clone(), spec)
        .await
        .map_err(|e| internal(format!("reconcile failed: {}", e)))?;

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
    responses((status = 200, body = PreviewGatewaySettingsResponse)),
    security(("bearer_auth" = []))
)]
pub async fn patch_preview_gateway_settings(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Json(patch): Json<PatchSettingsRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);

    state
        .platform_config_service
        .update_setting_field(|s| {
            if let Some(image) = patch.image.clone() {
                s.preview_gateway.image = image;
            }
            if let Some(host_port) = patch.host_port {
                s.preview_gateway.host_port = host_port;
            }
            if let Some(auto_upgrade) = patch.auto_upgrade {
                s.preview_gateway.auto_upgrade = auto_upgrade;
            }
        })
        .await
        .map_err(|e| internal(format!("failed to persist settings: {}", e)))?;

    let settings = preview_gateway::load_settings(&state.db).await;
    Ok(Json(PreviewGatewaySettingsResponse::from(settings)))
}

fn internal(detail: String) -> Problem {
    use temps_core::error_builder;
    error_builder::internal_server_error()
        .title("Preview gateway error")
        .detail(detail)
        .build()
}
