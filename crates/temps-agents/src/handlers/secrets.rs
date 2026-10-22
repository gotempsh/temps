// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Extension, Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use utoipa::ToSchema;

use temps_auth::{permission_guard, RequireAuth};
use temps_core::audit::{AuditContext, AuditOperation};
use temps_core::problemdetails::Problem;
use temps_core::RequestMetadata;

use crate::handlers::AppState;
use crate::services::secret_service::SecretType;

// ── Request / Response DTOs ──────────────────────────────────────────────────

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpsertSecretRequest {
    pub name: String,
    /// Project whose agent runs receive the secret. Omit for an instance-wide
    /// secret, which is injected into every project's agent runs and so is
    /// readable by anyone who can run an agent in any project.
    #[serde(default)]
    pub project_id: Option<i32>,
    /// "env" (environment variable) or "file" (written to mount_path)
    #[serde(default = "default_env")]
    pub secret_type: String,
    pub value: String,
    /// Required for "file" type secrets — absolute path inside the sandbox
    pub mount_path: Option<String>,
    pub description: Option<String>,
}

fn default_env() -> String {
    "env".to_string()
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SecretResponse {
    pub id: i32,
    /// Project the secret is scoped to; `null` means instance-wide.
    pub project_id: Option<i32>,
    pub name: String,
    pub secret_type: String,
    /// Always masked in responses
    pub value: String,
    pub mount_path: Option<String>,
    pub description: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl From<temps_entities::agent_secrets::Model> for SecretResponse {
    fn from(model: temps_entities::agent_secrets::Model) -> Self {
        Self {
            id: model.id,
            project_id: model.project_id,
            name: model.name,
            secret_type: model.secret_type,
            value: "***".to_string(),
            mount_path: model.mount_path,
            description: model.description,
            created_at: model.created_at.to_rfc3339(),
            updated_at: model.updated_at.to_rfc3339(),
        }
    }
}

/// Selects which scope a secret name refers to.
#[derive(Debug, Default, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct SecretScopeQuery {
    /// Project scope; omit for the instance-wide secret.
    pub project_id: Option<i32>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ListSecretsResponse {
    pub items: Vec<SecretResponse>,
    pub total: usize,
}

// ── Audit structs ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
struct SecretUpsertedAudit {
    context: AuditContext,
    secret_name: String,
    project_id: Option<i32>,
}

#[derive(Debug, Clone, Serialize)]
struct SecretDeletedAudit {
    context: AuditContext,
    secret_name: String,
    project_id: Option<i32>,
}

impl AuditOperation for SecretUpsertedAudit {
    fn operation_type(&self) -> String {
        "SECRET_UPSERTED".to_string()
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
        serde_json::to_string(self)
            .map_err(|e| temps_core::anyhow::anyhow!("Failed to serialize audit: {}", e))
    }
}

impl AuditOperation for SecretDeletedAudit {
    fn operation_type(&self) -> String {
        "SECRET_DELETED".to_string()
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
        serde_json::to_string(self)
            .map_err(|e| temps_core::anyhow::anyhow!("Failed to serialize audit: {}", e))
    }
}

// ── Routes ───────────────────────────────────────────────────────────────────

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/settings/secrets", get(list_secrets).post(upsert_secret))
        .route(
            "/settings/secrets/{name}",
            axum::routing::delete(delete_secret),
        )
}

// ── Handlers ─────────────────────────────────────────────────────────────────

#[utoipa::path(
    tag = "Secrets",
    get,
    path = "/settings/secrets",
    responses(
        (status = 200, description = "Agent secrets in every scope (instance-wide and per project)", body = ListSecretsResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_secrets(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsRead);

    let secrets = app_state
        .secret_service
        .list_secrets()
        .await
        .map_err(Problem::from)?;

    let total = secrets.len();
    Ok(Json(ListSecretsResponse {
        items: secrets.into_iter().map(SecretResponse::from).collect(),
        total,
    }))
}

#[utoipa::path(
    tag = "Secrets",
    post,
    path = "/settings/secrets",
    request_body = UpsertSecretRequest,
    responses(
        (status = 201, description = "Secret created/updated", body = SecretResponse),
        (status = 400, description = "Validation error"),
        (status = 404, description = "Project not found"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn upsert_secret(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<UpsertSecretRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);

    let secret_type = match request.secret_type.as_str() {
        "file" => SecretType::File,
        _ => SecretType::Env,
    };

    let secret = app_state
        .secret_service
        .upsert_secret(
            &request.name,
            request.project_id,
            secret_type,
            &request.value,
            request.mount_path.as_deref(),
            request.description.as_deref(),
        )
        .await
        .map_err(Problem::from)?;

    let audit = SecretUpsertedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        secret_name: secret.name.clone(),
        project_id: secret.project_id,
    };
    if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
        tracing::error!(
            "Failed to create audit log for secret upsert (name {}): {}",
            secret.name,
            e
        );
    }

    Ok((StatusCode::CREATED, Json(SecretResponse::from(secret))))
}

#[utoipa::path(
    tag = "Secrets",
    delete,
    path = "/settings/secrets/{name}",
    params(
        ("name" = String, Path, description = "Secret name"),
        SecretScopeQuery,
    ),
    responses(
        (status = 204, description = "Secret deleted"),
        (status = 404, description = "Secret not found"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn delete_secret(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(name): Path<String>,
    Query(scope): Query<SecretScopeQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);

    app_state
        .secret_service
        .delete_secret(&name, scope.project_id)
        .await
        .map_err(Problem::from)?;

    let audit = SecretDeletedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        secret_name: name.clone(),
        project_id: scope.project_id,
    };
    if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
        tracing::error!(
            "Failed to create audit log for secret delete (name {}): {}",
            &name,
            e
        );
    }

    Ok(StatusCode::NO_CONTENT)
}
