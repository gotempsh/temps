use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Json},
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_core::problemdetails::Problem;
use utoipa::{OpenApi, ToSchema};

use super::permission_guard;
use super::RequireAuth;
use crate::apikey_handler_types::{
    ApiKeyListResponse, ApiKeyResponse, CreateApiKeyRequest, CreateApiKeyResponse,
    UpdateApiKeyRequest,
};
use crate::{
    apikey_service::ApiKeyService,
    apikey_types::{get_available_permissions, AvailablePermissions},
};

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ListApiKeysQuery {
    pub page: Option<u64>,
    pub page_size: Option<u64>,
}

pub struct ApiKeyState {
    pub api_key_service: Arc<ApiKeyService>,
}

#[utoipa::path(
    post,
    path = "/api-keys",
    request_body = CreateApiKeyRequest,
    responses(
        (status = 201, description = "API key created successfully", body = CreateApiKeyResponse),
        (status = 400, description = "Bad request"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden"),
        (status = 409, description = "Conflict - API key name already exists"),
        (status = 500, description = "Internal server error")
    ),
    tag = "API Keys",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn create_api_key(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ApiKeyState>>,
    Json(request): Json<CreateApiKeyRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ApiKeysCreate);

    match state
        .api_key_service
        .create_api_key(auth.user_id(), request.into())
        .await
    {
        Ok(api_key) => Ok((StatusCode::CREATED, Json(api_key))),
        Err(e) => Err(e.to_problem()),
    }
}

#[utoipa::path(
    get,
    path = "/api-keys",
    params(
        ("page" = Option<u64>, Query, description = "Page number (default: 1)"),
        ("page_size" = Option<u64>, Query, description = "Items per page (default: 20)")
    ),
    responses(
        (status = 200, description = "API keys retrieved successfully", body = ApiKeyListResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden"),
        (status = 500, description = "Internal server error")
    ),
    tag = "API Keys",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn list_api_keys(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ApiKeyState>>,
    Query(query): Query<ListApiKeysQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ApiKeysRead);

    let page = query.page.unwrap_or(1).max(1);
    let page_size = query.page_size.unwrap_or(20).min(100).max(1);

    match state
        .api_key_service
        .list_api_keys(auth.user_id(), page, page_size)
        .await
    {
        Ok(response) => Ok(Json(response)),
        Err(e) => Err(e.to_problem()),
    }
}

#[utoipa::path(
    get,
    path = "/api-keys/{id}",
    params(
        ("id" = i32, Path, description = "API key ID")
    ),
    responses(
        (status = 200, description = "API key retrieved successfully", body = ApiKeyResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden"),
        (status = 404, description = "Not found"),
        (status = 500, description = "Internal server error")
    ),
    tag = "API Keys",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_api_key(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ApiKeyState>>,
    Path(api_key_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ApiKeysRead);

    match state
        .api_key_service
        .get_api_key(auth.user_id(), api_key_id)
        .await
    {
        Ok(api_key) => Ok(Json(api_key)),
        Err(e) => Err(e.to_problem()),
    }
}

#[utoipa::path(
    put,
    path = "/api-keys/{id}",
    params(
        ("id" = i32, Path, description = "API key ID")
    ),
    request_body = UpdateApiKeyRequest,
    responses(
        (status = 200, description = "API key updated successfully", body = ApiKeyResponse),
        (status = 400, description = "Bad request"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden"),
        (status = 404, description = "Not found"),
        (status = 409, description = "Conflict - API key name already exists"),
        (status = 500, description = "Internal server error")
    ),
    tag = "API Keys",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn update_api_key(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ApiKeyState>>,
    Path(api_key_id): Path<i32>,
    Json(request): Json<UpdateApiKeyRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ApiKeysWrite);

    match state
        .api_key_service
        .update_api_key(auth.user_id(), api_key_id, request.into())
        .await
    {
        Ok(api_key) => Ok(Json(api_key)),
        Err(e) => Err(e.to_problem()),
    }
}

#[utoipa::path(
    delete,
    path = "/api-keys/{id}",
    params(
        ("id" = i32, Path, description = "API key ID")
    ),
    responses(
        (status = 204, description = "API key deleted successfully"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden"),
        (status = 404, description = "Not found"),
        (status = 500, description = "Internal server error")
    ),
    tag = "API Keys",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn delete_api_key(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ApiKeyState>>,
    Path(api_key_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ApiKeysDelete);

    match state
        .api_key_service
        .delete_api_key(auth.user_id(), api_key_id)
        .await
    {
        Ok(()) => Ok(StatusCode::NO_CONTENT),
        Err(e) => Err(e.to_problem()),
    }
}

#[utoipa::path(
    post,
    path = "/api-keys/{id}/deactivate",
    params(
        ("id" = i32, Path, description = "API key ID")
    ),
    responses(
        (status = 200, description = "API key deactivated successfully", body = ApiKeyResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden"),
        (status = 404, description = "Not found"),
        (status = 500, description = "Internal server error")
    ),
    tag = "API Keys",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn deactivate_api_key(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ApiKeyState>>,
    Path(api_key_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ApiKeysWrite);

    match state
        .api_key_service
        .deactivate_api_key(auth.user_id(), api_key_id)
        .await
    {
        Ok(api_key) => Ok(Json(api_key)),
        Err(e) => Err(e.to_problem()),
    }
}

#[utoipa::path(
    post,
    path = "/api-keys/{id}/activate",
    params(
        ("id" = i32, Path, description = "API key ID")
    ),
    responses(
        (status = 200, description = "API key activated successfully", body = ApiKeyResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden"),
        (status = 404, description = "Not found"),
        (status = 500, description = "Internal server error")
    ),
    tag = "API Keys",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn activate_api_key(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ApiKeyState>>,
    Path(api_key_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ApiKeysWrite);

    match state
        .api_key_service
        .activate_api_key(auth.user_id(), api_key_id)
        .await
    {
        Ok(api_key) => Ok(Json(api_key)),
        Err(e) => Err(e.to_problem()),
    }
}

#[utoipa::path(
    get,
    path = "/api-keys/permissions",
    responses(
        (status = 200, description = "Available permissions and roles retrieved successfully", body = AvailablePermissions),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    tag = "API Keys",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_api_key_permissions(RequireAuth(_auth): RequireAuth) -> impl IntoResponse {
    // No specific permission check needed - authenticated users can see available permissions
    Json(get_available_permissions())
}

#[derive(OpenApi)]
#[openapi(
    paths(
        create_api_key,
        list_api_keys,
        get_api_key,
        update_api_key,
        delete_api_key,
        activate_api_key,
        deactivate_api_key,
        get_api_key_permissions,
    ),
    components(
        schemas(
            CreateApiKeyRequest,
            UpdateApiKeyRequest,
            ApiKeyResponse,
            CreateApiKeyResponse,
            ApiKeyListResponse,
            ListApiKeysQuery,
            AvailablePermissions,
        )
    ),
    tags(
        (name = "API Keys", description = "API key management endpoints")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub struct ApiKeyApiDoc;
