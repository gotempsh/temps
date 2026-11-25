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

use temps_authz::{permission_guard, RequireAuth};
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
    /// Return only what was last read, never looking inside the workspace
    /// (which could create or wake it). Not checked yet: every version field
    /// and `checked_at` are null. Ignored when `refresh` is set.
    #[serde(default)]
    pub cached_only: bool,
}

/// What the cached-only read returns before the workspace was ever read.
fn not_checked_yet(provider_id: &str) -> temps_ai::HarnessUpdateStatus {
    temps_ai::HarnessUpdateStatus {
        provider_id: provider_id.to_string(),
        installed_version: None,
        latest_version: None,
        update_available: false,
        updating: false,
        checked_at: None,
        detail: None,
    }
}

fn authorize(auth: &temps_authz::AuthContext, provider_id: &str) -> Result<(), Problem> {
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
    let ai_service = service(&app_state)?;
    let status = harness_status(ai_service.as_ref(), &provider_id, auth.user_id(), &query)
        .await
        .map_err(|error| update_problem(&provider_id, &error))?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(status)))
}

/// The status the `GET` returns. `cached_only` never looks inside the
/// workspace (which could create or wake it) and reports "not checked yet"
/// when nothing was read; `refresh` always looks, and wins over `cached_only`.
async fn harness_status(
    ai_service: &dyn temps_ai::AiService,
    provider_id: &str,
    user_id: i32,
    query: &HarnessUpdateQuery,
) -> Result<temps_ai::HarnessUpdateStatus, temps_ai::AiError> {
    if query.cached_only && !query.refresh {
        return Ok(ai_service
            .cached_workspace_harness_update_status(provider_id, user_id)
            .await?
            .unwrap_or_else(|| not_checked_yet(provider_id)));
    }
    ai_service
        .workspace_harness_update_status(provider_id, user_id, query.refresh)
        .await
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
    use std::sync::Mutex;

    /// Records which read the handler chose; the live read stands in for
    /// "looked inside (and possibly woke) the workspace".
    #[derive(Default)]
    struct RecordingAiService {
        cached: Option<temps_ai::HarnessUpdateStatus>,
        calls: Mutex<Vec<String>>,
    }

    fn checked(installed: &str) -> temps_ai::HarnessUpdateStatus {
        temps_ai::HarnessUpdateStatus {
            provider_id: "codex_cli".to_string(),
            installed_version: Some(installed.to_string()),
            latest_version: Some("1.1.0".to_string()),
            update_available: installed != "1.1.0",
            updating: false,
            checked_at: Some("2026-11-25T09:00:00.000Z".to_string()),
            detail: None,
        }
    }

    #[async_trait::async_trait]
    impl temps_ai::AiService for RecordingAiService {
        async fn is_available(&self) -> bool {
            true
        }

        async fn complete(
            &self,
            _request: temps_ai::AiRequest,
        ) -> Result<temps_ai::AiResponse, temps_ai::AiError> {
            Err(temps_ai::AiError::NotAvailable)
        }

        async fn chat_stream(
            &self,
            _request: temps_ai::ChatTurnRequest,
        ) -> Result<temps_ai::TokenStream, temps_ai::AiError> {
            Err(temps_ai::AiError::NotAvailable)
        }

        async fn workspace_harness_update_status(
            &self,
            _provider: &str,
            principal_id: i32,
            refresh: bool,
        ) -> Result<temps_ai::HarnessUpdateStatus, temps_ai::AiError> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("inspect user={principal_id} refresh={refresh}"));
            Ok(checked("1.0.0"))
        }

        async fn cached_workspace_harness_update_status(
            &self,
            _provider: &str,
            principal_id: i32,
        ) -> Result<Option<temps_ai::HarnessUpdateStatus>, temps_ai::AiError> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("cached user={principal_id}"));
            Ok(self.cached.clone())
        }
    }

    fn query(refresh: bool, cached_only: bool) -> HarnessUpdateQuery {
        HarnessUpdateQuery {
            refresh,
            cached_only,
        }
    }

    #[tokio::test]
    async fn a_cached_only_read_never_inspects_the_workspace() {
        let never_checked = RecordingAiService::default();
        let status = harness_status(&never_checked, "codex_cli", 7, &query(false, true))
            .await
            .unwrap();
        assert_eq!(*never_checked.calls.lock().unwrap(), vec!["cached user=7"]);
        // Not checked yet: every version field and `checked_at` are null.
        assert_eq!(
            serde_json::to_value(&status).unwrap(),
            serde_json::to_value(not_checked_yet("codex_cli")).unwrap()
        );
        assert!(status.checked_at.is_none() && status.installed_version.is_none());

        let checked_before = RecordingAiService {
            cached: Some(checked("1.1.0")),
            ..Default::default()
        };
        let status = harness_status(&checked_before, "codex_cli", 7, &query(false, true))
            .await
            .unwrap();
        assert_eq!(status.installed_version.as_deref(), Some("1.1.0"));
        assert_eq!(*checked_before.calls.lock().unwrap(), vec!["cached user=7"]);
    }

    #[tokio::test]
    async fn refresh_inspects_the_workspace_even_with_cached_only() {
        let service = RecordingAiService {
            cached: Some(checked("1.1.0")),
            ..Default::default()
        };
        let status = harness_status(&service, "codex_cli", 7, &query(true, true))
            .await
            .unwrap();
        assert_eq!(status.installed_version.as_deref(), Some("1.0.0"));
        let plain = harness_status(&service, "codex_cli", 7, &query(false, false))
            .await
            .unwrap();
        assert_eq!(plain.installed_version.as_deref(), Some("1.0.0"));
        assert_eq!(
            *service.calls.lock().unwrap(),
            vec![
                "inspect user=7 refresh=true",
                "inspect user=7 refresh=false"
            ]
        );
    }

    #[test]
    fn the_query_flags_default_to_a_plain_cached_check() {
        let parsed: HarnessUpdateQuery =
            serde_json::from_value(serde_json::json!({ "cached_only": true })).unwrap();
        assert!(parsed.cached_only && !parsed.refresh);
        let parsed: HarnessUpdateQuery = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(!parsed.cached_only && !parsed.refresh);
    }

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
