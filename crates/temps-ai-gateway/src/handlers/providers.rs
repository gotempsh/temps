use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, patch, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_auth::permission_guard;
use temps_auth::RequireAuth;
use temps_core::problemdetails;
use temps_core::problemdetails::{Problem, ProblemDetails};
use utoipa::{OpenApi, ToSchema};

use crate::error::AiGatewayError;
use crate::handlers::types::AiGatewayAppState;

// ============================================================================
// Error conversion
// ============================================================================

impl From<AiGatewayError> for Problem {
    fn from(error: AiGatewayError) -> Self {
        match error {
            AiGatewayError::ProviderKeyNotFound { .. } => {
                problemdetails::new(StatusCode::NOT_FOUND)
                    .with_title("Provider Key Not Found")
                    .with_detail(error.to_string())
            }
            AiGatewayError::Validation { .. } => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Validation Error")
                .with_detail(error.to_string()),
            AiGatewayError::Database(_)
            | AiGatewayError::Encryption(_)
            | AiGatewayError::HttpClient(_) => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Server Error")
                    .with_detail(error.to_string())
            }
            AiGatewayError::ProviderNotConfigured { .. } => {
                problemdetails::new(StatusCode::NOT_FOUND)
                    .with_title("Provider Not Configured")
                    .with_detail(error.to_string())
            }
            AiGatewayError::ModelNotFound { .. } => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Model Not Found")
                .with_detail(error.to_string()),
            AiGatewayError::ModelNotAllowed { .. } => problemdetails::new(StatusCode::FORBIDDEN)
                .with_title("Model Not Allowed")
                .with_detail(error.to_string()),
            AiGatewayError::UpstreamError { status, .. } => {
                let http_status = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
                problemdetails::new(http_status)
                    .with_title("Upstream Provider Error")
                    .with_detail(error.to_string())
            }
            AiGatewayError::TranslationError { .. } => problemdetails::new(StatusCode::BAD_GATEWAY)
                .with_title("Translation Error")
                .with_detail(error.to_string()),
            AiGatewayError::StreamError { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Stream Error")
                    .with_detail(error.to_string())
            }
            AiGatewayError::Internal { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Error")
                    .with_detail(error.to_string())
            }
            AiGatewayError::InvalidProviderUrl { .. } => {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Invalid Provider URL")
                    .with_detail(error.to_string())
            }
        }
    }
}

// ============================================================================
// OpenAPI schema
// ============================================================================

#[derive(OpenApi)]
#[openapi(
    paths(
        list_provider_keys,
        create_provider_key,
        update_provider_key,
        delete_provider_key,
        test_provider_key_inline,
        test_provider_key_by_id,
    ),
    components(schemas(
        ProviderKeyResponse,
        CreateProviderKeyRequest,
        UpdateProviderKeyRequest,
        TestProviderKeyRequest,
        TestProviderKeyResponse,
    )),
    info(
        title = "AI Gateway Admin API",
        description = "Manage AI provider API keys and gateway configuration",
        version = "1.0.0"
    ),
    tags(
        (name = "AI Gateway Admin", description = "Provider key management endpoints")
    )
)]
pub struct AiGatewayAdminApiDoc;

pub fn configure_admin_routes() -> Router<Arc<AiGatewayAppState>> {
    Router::new()
        .route("/ai/providers", get(list_provider_keys))
        .route("/ai/providers", post(create_provider_key))
        .route("/ai/providers/{id}", patch(update_provider_key))
        .route("/ai/providers/{id}", delete(delete_provider_key))
        .route("/ai/providers/test", post(test_provider_key_inline))
        .route("/ai/providers/{id}/test", post(test_provider_key_by_id))
}

// ============================================================================
// DTOs
// ============================================================================

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ProviderKeyResponse {
    pub id: i32,
    pub provider: String,
    pub display_name: String,
    /// Masked API key (only last 4 chars visible)
    pub api_key_masked: String,
    pub base_url: Option<String>,
    /// Model id this provider serves (NULL → per-provider default).
    pub default_model: Option<String>,
    pub is_active: bool,
    pub created_at: String,
    pub updated_at: String,
}

impl From<temps_entities::ai_provider_keys::Model> for ProviderKeyResponse {
    fn from(model: temps_entities::ai_provider_keys::Model) -> Self {
        Self {
            id: model.id,
            provider: model.provider,
            display_name: model.display_name,
            api_key_masked: "***".to_string(), // Never expose encrypted key
            base_url: model.base_url,
            default_model: model.default_model,
            is_active: model.is_active,
            created_at: model.created_at.to_rfc3339(),
            updated_at: model.updated_at.to_rfc3339(),
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateProviderKeyRequest {
    pub provider: String,
    pub display_name: String,
    pub api_key: String,
    pub base_url: Option<String>,
    /// Optional model id to pin for this provider (e.g. "gpt-4o-mini").
    pub default_model: Option<String>,
}

/// Deserialize a PATCH field that distinguishes three states:
/// absent (`None`), present-null (`Some(None)` → clear), and present-value
/// (`Some(Some(v))` → set). Plain `Option<Option<T>>` cannot do this because
/// serde collapses an explicit JSON `null` to the outer `None`, making "clear
/// this field" indistinguishable from "field omitted". Pair with
/// `#[serde(default, deserialize_with = "deserialize_optional_field")]`.
fn deserialize_optional_field<'de, T, D>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    // serde only invokes this when the key is present, so wrap whatever we parse
    // — including an explicit `null` — in an outer `Some`.
    Ok(Some(Option::<T>::deserialize(deserializer)?))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateProviderKeyRequest {
    pub display_name: Option<String>,
    pub api_key: Option<String>,
    /// Double-Option: absent = leave unchanged, present-null = clear (revert to
    /// the provider's default endpoint), present-value = set.
    #[serde(default, deserialize_with = "deserialize_optional_field")]
    pub base_url: Option<Option<String>>,
    /// Double-Option: absent = leave unchanged, present-null = clear the pinned
    /// model (revert to the per-provider default), present-value = set.
    #[serde(default, deserialize_with = "deserialize_optional_field")]
    pub default_model: Option<Option<String>>,
    pub is_active: Option<bool>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct TestProviderKeyRequest {
    /// Provider ID: "openai", "anthropic", "xai", "gemini"
    pub provider: String,
    /// The raw API key to test
    pub api_key: String,
    /// Optional custom base URL
    pub base_url: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct TestProviderKeyResponse {
    pub success: bool,
    pub provider: String,
    /// Error message if the test failed
    pub error: Option<String>,
    /// Response time in milliseconds
    pub latency_ms: u64,
}

// ============================================================================
// Handlers
// ============================================================================

#[utoipa::path(
    tag = "AI Gateway Admin",
    get,
    path = "/ai/providers",
    responses(
        (status = 200, description = "List of provider keys", body = Vec<ProviderKeyResponse>),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn list_provider_keys(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AiGatewayAppState>>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AiGatewayRead);

    let keys = app_state.provider_key_service.list().await?;
    let responses: Vec<ProviderKeyResponse> = keys.into_iter().map(Into::into).collect();

    Ok(Json(responses))
}

#[utoipa::path(
    tag = "AI Gateway Admin",
    post,
    path = "/ai/providers",
    request_body = CreateProviderKeyRequest,
    responses(
        (status = 201, description = "Provider key created", body = ProviderKeyResponse),
        (status = 400, description = "Validation error", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn create_provider_key(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AiGatewayAppState>>,
    Json(request): Json<CreateProviderKeyRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AiGatewayWrite);

    // Verify the key works before persisting it. Fails fast with a 400 so
    // the UI doesn't end up with a stored-but-broken credential.
    app_state
        .gateway_service
        .test_provider(
            &request.provider,
            &request.api_key,
            request.base_url.as_deref(),
        )
        .await
        .map_err(|e| AiGatewayError::Validation {
            message: format!("API key verification failed: {}", e),
        })?;

    let key = app_state
        .provider_key_service
        .create(
            &request.provider,
            &request.display_name,
            &request.api_key,
            request.base_url.as_deref(),
            request.default_model.as_deref(),
        )
        .await?;

    Ok((StatusCode::CREATED, Json(ProviderKeyResponse::from(key))))
}

#[utoipa::path(
    tag = "AI Gateway Admin",
    patch,
    path = "/ai/providers/{id}",
    request_body = UpdateProviderKeyRequest,
    responses(
        (status = 200, description = "Provider key updated", body = ProviderKeyResponse),
        (status = 400, description = "Validation error", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Not found", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn update_provider_key(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AiGatewayAppState>>,
    Path(id): Path<i32>,
    Json(request): Json<UpdateProviderKeyRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AiGatewayWrite);

    let key = app_state
        .provider_key_service
        .update(
            id,
            request.display_name.as_deref(),
            request.api_key.as_deref(),
            request.base_url.as_ref().map(|o| o.as_deref()),
            request.default_model.as_ref().map(|o| o.as_deref()),
            request.is_active,
        )
        .await?;

    Ok(Json(ProviderKeyResponse::from(key)))
}

#[utoipa::path(
    tag = "AI Gateway Admin",
    delete,
    path = "/ai/providers/{id}",
    responses(
        (status = 204, description = "Provider key deleted"),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Not found", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn delete_provider_key(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AiGatewayAppState>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AiGatewayWrite);

    app_state.provider_key_service.delete(id).await?;

    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    tag = "AI Gateway Admin",
    post,
    path = "/ai/providers/test",
    request_body = TestProviderKeyRequest,
    responses(
        (status = 200, description = "Test result", body = TestProviderKeyResponse),
        (status = 400, description = "Validation error", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn test_provider_key_inline(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AiGatewayAppState>>,
    Json(request): Json<TestProviderKeyRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AiGatewayWrite);

    let start = std::time::Instant::now();
    let result = app_state
        .gateway_service
        .test_provider(
            &request.provider,
            &request.api_key,
            request.base_url.as_deref(),
        )
        .await;
    let latency_ms = start.elapsed().as_millis() as u64;

    let response = match result {
        Ok(()) => TestProviderKeyResponse {
            success: true,
            provider: request.provider,
            error: None,
            latency_ms,
        },
        Err(e) => TestProviderKeyResponse {
            success: false,
            provider: request.provider,
            error: Some(friendly_error_message(&e)),
            latency_ms,
        },
    };

    Ok(Json(response))
}

#[utoipa::path(
    tag = "AI Gateway Admin",
    post,
    path = "/ai/providers/{id}/test",
    responses(
        (status = 200, description = "Test result", body = TestProviderKeyResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Provider key not found", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn test_provider_key_by_id(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AiGatewayAppState>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AiGatewayRead);

    // get_by_id already returns Err(ProviderKeyNotFound) if not found
    let key_record = app_state.provider_key_service.get_by_id(id).await?;

    let decrypted_key = app_state
        .provider_key_service
        .decrypt_api_key(&key_record.api_key_encrypted)?;

    let provider_name = key_record.provider.clone();
    let start = std::time::Instant::now();
    let result = app_state
        .gateway_service
        .test_provider(
            &key_record.provider,
            &decrypted_key,
            key_record.base_url.as_deref(),
        )
        .await;
    let latency_ms = start.elapsed().as_millis() as u64;

    let response = match result {
        Ok(()) => TestProviderKeyResponse {
            success: true,
            provider: provider_name,
            error: None,
            latency_ms,
        },
        Err(e) => TestProviderKeyResponse {
            success: false,
            provider: provider_name,
            error: Some(friendly_error_message(&e)),
            latency_ms,
        },
    };

    Ok(Json(response))
}

/// Extract a human-friendly error message from an AiGatewayError.
/// For upstream errors the raw body is often a JSON blob; we try to
/// pull out just the `error.message` field for a cleaner UX.
fn friendly_error_message(err: &AiGatewayError) -> String {
    if let AiGatewayError::UpstreamError {
        status, message, ..
    } = err
    {
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(message) {
            if let Some(msg) = parsed
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
            {
                return format!("{} — {}", status, msg);
            }
        }
    }
    err.to_string()
}

#[cfg(test)]
mod patch_deser_tests {
    use super::UpdateProviderKeyRequest;

    #[test]
    fn base_url_present_value_sets() {
        let r: UpdateProviderKeyRequest =
            serde_json::from_str(r#"{"base_url":"http://localhost:11434/v1"}"#).unwrap();
        assert_eq!(
            r.base_url,
            Some(Some("http://localhost:11434/v1".to_string()))
        );
    }

    #[test]
    fn base_url_explicit_null_clears() {
        let r: UpdateProviderKeyRequest = serde_json::from_str(r#"{"base_url":null}"#).unwrap();
        // The fix: explicit null is now distinguishable from absent.
        assert_eq!(r.base_url, Some(None));
    }

    #[test]
    fn base_url_absent_is_unchanged() {
        let r: UpdateProviderKeyRequest = serde_json::from_str(r#"{}"#).unwrap();
        assert_eq!(r.base_url, None);
    }

    #[test]
    fn default_model_three_states() {
        let set: UpdateProviderKeyRequest =
            serde_json::from_str(r#"{"default_model":"gpt-5-mini"}"#).unwrap();
        assert_eq!(set.default_model, Some(Some("gpt-5-mini".to_string())));

        let clear: UpdateProviderKeyRequest =
            serde_json::from_str(r#"{"default_model":null}"#).unwrap();
        assert_eq!(clear.default_model, Some(None));

        let absent: UpdateProviderKeyRequest = serde_json::from_str(r#"{}"#).unwrap();
        assert_eq!(absent.default_model, None);
    }
}
