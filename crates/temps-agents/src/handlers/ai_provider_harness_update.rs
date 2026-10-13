// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Keep a provider's harness CLI current inside the operator's persistent
//! workspace.
//!
//! The managed workspace image bundles one CLI release, so it falls behind and
//! stops offering models published after it. `GET` reports installed versus
//! latest (cached, re-read from the workspace at most every few hours);
//! `POST` installs the latest release in the workspace on the operator's
//! request. Nothing is updated silently: the console shows an action bar.

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::{header, StatusCode},
    response::IntoResponse,
    routing::get,
    Extension, Json, Router,
};
use serde::Deserialize;
use utoipa::IntoParams;

use temps_auth::{permission_guard, RequireAuth};
use temps_core::problemdetails::{self, Problem};
use temps_core::RequestMetadata;

use crate::ai_cli::catalog::find_provider;
use crate::handlers::ai_providers::{
    ensure_workspace_model_discovery_permission, write_provider_settings_audit,
};
use crate::handlers::AppState;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new().route(
        "/settings/ai-providers/{provider_id}/harness-update",
        get(get_ai_provider_harness_update).post(update_ai_provider_harness),
    )
}

#[derive(Debug, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct HarnessUpdateQuery {
    /// Look inside the workspace again instead of returning the cached check.
    #[serde(default)]
    pub refresh: bool,
}

fn authorize(auth: &temps_auth::AuthContext, provider_id: &str) -> Result<(), Problem> {
    permission_guard!(auth, SettingsWrite);
    ensure_workspace_model_discovery_permission(auth)?;
    find_provider(provider_id).ok_or_else(|| {
        problemdetails::new(StatusCode::NOT_FOUND)
            .with_title("Unknown AI Provider")
            .with_detail(format!("Unknown AI provider '{provider_id}'"))
    })?;
    Ok(())
}

fn update_problem(provider_id: &str, error: &temps_ai::AiError) -> Problem {
    let (status, title) = match error {
        temps_ai::AiError::Provider { purpose, .. }
            if purpose == "provider.harness.update.busy" =>
        {
            (StatusCode::CONFLICT, "Harness Busy")
        }
        _ => (
            StatusCode::SERVICE_UNAVAILABLE,
            "Harness Update Unavailable",
        ),
    };
    tracing::warn!(
        provider_id,
        error_kind = ?std::mem::discriminant(error),
        "AI harness update request failed"
    );
    let detail = match error {
        temps_ai::AiError::Provider { reason, .. } => reason.clone(),
        _ => format!("Temps could not check or update the {provider_id} harness."),
    };
    problemdetails::new(status)
        .with_title(title)
        .with_detail(detail)
}

fn service(app_state: &AppState) -> Result<&Arc<dyn temps_ai::AiService>, Problem> {
    app_state.ai_service.as_ref().ok_or_else(|| {
        problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
            .with_title("Harness Update Unavailable")
            .with_detail("The AI harness service is not configured on this Temps instance.")
    })
}

/// Installed and latest version of the provider's harness CLI in your
/// persistent workspace, and whether an update is available.
#[utoipa::path(
    tag = "Agents",
    get,
    path = "/settings/ai-providers/{provider_id}/harness-update",
    params(
        ("provider_id" = String, Path, description = "AI provider ID"),
        HarnessUpdateQuery
    ),
    responses(
        (status = 200, description = "Harness version status", body = temps_ai::HarnessUpdateStatus),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Unknown provider"),
        (status = 503, description = "The workspace could not be inspected"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_ai_provider_harness_update(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path(provider_id): Path<String>,
    Query(query): Query<HarnessUpdateQuery>,
) -> Result<impl IntoResponse, Problem> {
    authorize(&auth, &provider_id)?;
    let status = service(&app_state)?
        .workspace_harness_update_status(&provider_id, auth.user_id(), query.refresh)
        .await
        .map_err(|error| update_problem(&provider_id, &error))?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(status)))
}

/// Install the latest release of the provider's harness CLI in your
/// persistent workspace so newly released models become available.
#[utoipa::path(
    tag = "Agents",
    post,
    path = "/settings/ai-providers/{provider_id}/harness-update",
    params(("provider_id" = String, Path, description = "AI provider ID")),
    responses(
        (status = 200, description = "Harness updated", body = temps_ai::HarnessUpdateStatus),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Unknown provider"),
        (status = 409, description = "The workspace is busy or already updating"),
        (status = 503, description = "The update could not run"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn update_ai_provider_harness(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(provider_id): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    authorize(&auth, &provider_id)?;
    let ai_service = service(&app_state)?;
    let before = ai_service
        .workspace_harness_update_status(&provider_id, auth.user_id(), false)
        .await
        .ok();
    let status = ai_service
        .update_workspace_harness(&provider_id, auth.user_id())
        .await
        .map_err(|error| update_problem(&provider_id, &error))?;
    // New models appear only after the next discovery.
    ai_service
        .invalidate_capabilities_for(Some(&provider_id))
        .await;
    write_provider_settings_audit(
        &app_state,
        &auth,
        &metadata,
        &provider_id,
        &format!(
            "harness-updated:{}->{}",
            before
                .and_then(|status| status.installed_version)
                .unwrap_or_else(|| "unknown".to_string()),
            status
                .installed_version
                .clone()
                .unwrap_or_else(|| "unknown".to_string())
        ),
    )
    .await;
    tracing::info!(
        provider_id,
        user_id = auth.user_id(),
        installed = status.installed_version.as_deref().unwrap_or("unknown"),
        "AI harness updated in the persistent workspace"
    );
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(status)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn busy_workspace_is_a_conflict_and_other_failures_are_unavailable() {
        let busy = temps_ai::AiError::Provider {
            purpose: "provider.harness.update.busy".to_string(),
            reason: "the persistent workspace is busy with a turn".to_string(),
        };
        let failed = temps_ai::AiError::Provider {
            purpose: "provider.harness.update".to_string(),
            reason: "the update command exited with status 1".to_string(),
        };
        assert_eq!(
            update_problem("claude_cli", &busy).status_code,
            StatusCode::CONFLICT
        );
        assert_eq!(
            update_problem("claude_cli", &failed).status_code,
            StatusCode::SERVICE_UNAVAILABLE
        );
    }
}
