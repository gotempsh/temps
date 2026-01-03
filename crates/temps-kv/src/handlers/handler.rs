//! HTTP handlers for KV operations

use std::sync::Arc;

use axum::{
    extract::{Extension, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, post},
    Json, Router,
};
use temps_auth::{permission_guard, RequireAuth};
use temps_core::problemdetails::Problem;
use temps_core::RequestMetadata;
use temps_providers::externalsvc::ExternalService;
use tracing::{error, info};
use utoipa::OpenApi;

use super::audit::{AuditContext, KvServiceDisabledAudit, KvServiceEnabledAudit};

use super::types::*;
use crate::services::SetOptions;

/// OpenAPI documentation for KV endpoints
#[derive(OpenApi)]
#[openapi(
    paths(
        kv_get,
        kv_set,
        kv_del,
        kv_incr,
        kv_expire,
        kv_ttl,
        kv_keys,
        kv_status,
        kv_enable,
        kv_disable,
    ),
    components(
        schemas(
            GetRequest,
            GetResponse,
            SetRequest,
            SetResponse,
            DelRequest,
            DelResponse,
            IncrRequest,
            IncrResponse,
            ExpireRequest,
            ExpireResponse,
            TtlRequest,
            TtlResponse,
            KeysRequest,
            KeysResponse,
            KvStatusResponse,
            EnableKvRequest,
            EnableKvResponse,
            DisableKvResponse,
        )
    ),
    tags(
        (name = "KV Store", description = "Key-Value storage operations"),
        (name = "KV Management", description = "KV service management operations")
    )
)]
pub struct KvApiDoc;

/// Configure KV routes
pub fn configure_routes() -> Router<Arc<KvAppState>> {
    Router::new()
        // Data operations
        .route("/kv/get", post(kv_get))
        .route("/kv/set", post(kv_set))
        .route("/kv/del", post(kv_del))
        .route("/kv/incr", post(kv_incr))
        .route("/kv/expire", post(kv_expire))
        .route("/kv/ttl", post(kv_ttl))
        .route("/kv/keys", post(kv_keys))
        // Management operations
        .route("/kv/status", get(kv_status))
        .route("/kv/enable", post(kv_enable))
        .route("/kv/disable", delete(kv_disable))
}

/// Get a value by key
#[utoipa::path(
    tag = "KV Store",
    post,
    path = "/kv/get",
    request_body = GetRequest,
    responses(
        (status = 200, description = "Value retrieved", body = GetResponse),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn kv_get(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<KvAppState>>,
    Json(request): Json<GetRequest>,
) -> Result<impl IntoResponse, Problem> {
    let project_id = extract_project_id(&auth)?;

    let value = state.kv_service.get(project_id, &request.key).await?;

    Ok(Json(GetResponse { value }))
}

/// Set a value with optional expiration
#[utoipa::path(
    tag = "KV Store",
    post,
    path = "/kv/set",
    request_body = SetRequest,
    responses(
        (status = 200, description = "Value set", body = SetResponse),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn kv_set(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<KvAppState>>,
    Json(request): Json<SetRequest>,
) -> Result<impl IntoResponse, Problem> {
    let project_id = extract_project_id(&auth)?;

    let options = SetOptions {
        ex: request.ex,
        px: request.px,
        nx: request.nx,
        xx: request.xx,
    };

    state
        .kv_service
        .set(project_id, &request.key, request.value, options)
        .await?;

    Ok(Json(SetResponse {
        result: "OK".to_string(),
    }))
}

/// Delete one or more keys
#[utoipa::path(
    tag = "KV Store",
    post,
    path = "/kv/del",
    request_body = DelRequest,
    responses(
        (status = 200, description = "Keys deleted", body = DelResponse),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn kv_del(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<KvAppState>>,
    Json(request): Json<DelRequest>,
) -> Result<impl IntoResponse, Problem> {
    let project_id = extract_project_id(&auth)?;

    let deleted = state.kv_service.del(project_id, request.keys).await?;

    Ok(Json(DelResponse { deleted }))
}

/// Increment a numeric value
#[utoipa::path(
    tag = "KV Store",
    post,
    path = "/kv/incr",
    request_body = IncrRequest,
    responses(
        (status = 200, description = "Value incremented", body = IncrResponse),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn kv_incr(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<KvAppState>>,
    Json(request): Json<IncrRequest>,
) -> Result<impl IntoResponse, Problem> {
    let project_id = extract_project_id(&auth)?;

    let value = match request.amount {
        Some(amount) if amount != 1 => {
            state
                .kv_service
                .incrby(project_id, &request.key, amount)
                .await?
        }
        _ => state.kv_service.incr(project_id, &request.key).await?,
    };

    Ok(Json(IncrResponse { value }))
}

/// Set expiration on a key
#[utoipa::path(
    tag = "KV Store",
    post,
    path = "/kv/expire",
    request_body = ExpireRequest,
    responses(
        (status = 200, description = "Expiration set", body = ExpireResponse),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn kv_expire(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<KvAppState>>,
    Json(request): Json<ExpireRequest>,
) -> Result<impl IntoResponse, Problem> {
    let project_id = extract_project_id(&auth)?;

    let success = state
        .kv_service
        .expire(project_id, &request.key, request.seconds)
        .await?;

    Ok(Json(ExpireResponse { success }))
}

/// Get time-to-live for a key
#[utoipa::path(
    tag = "KV Store",
    post,
    path = "/kv/ttl",
    request_body = TtlRequest,
    responses(
        (status = 200, description = "TTL retrieved", body = TtlResponse),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn kv_ttl(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<KvAppState>>,
    Json(request): Json<TtlRequest>,
) -> Result<impl IntoResponse, Problem> {
    let project_id = extract_project_id(&auth)?;

    let ttl = state.kv_service.ttl(project_id, &request.key).await?;

    Ok(Json(TtlResponse { ttl }))
}

/// Get keys matching a pattern
#[utoipa::path(
    tag = "KV Store",
    post,
    path = "/kv/keys",
    request_body = KeysRequest,
    responses(
        (status = 200, description = "Keys retrieved", body = KeysResponse),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn kv_keys(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<KvAppState>>,
    Json(request): Json<KeysRequest>,
) -> Result<impl IntoResponse, Problem> {
    let project_id = extract_project_id(&auth)?;

    let keys = state.kv_service.keys(project_id, &request.pattern).await?;

    Ok(Json(KeysResponse { keys }))
}

/// Extract project_id from authentication context
fn extract_project_id(auth: &temps_auth::AuthContext) -> Result<i32, Problem> {
    auth.project_id().ok_or_else(|| {
        temps_core::problemdetails::new(axum::http::StatusCode::FORBIDDEN)
            .with_title("Project Required")
            .with_detail("This operation requires a project-scoped token")
    })
}

// =============================================================================
// Management Handlers
// =============================================================================

/// Get KV service status
#[utoipa::path(
    tag = "KV Management",
    get,
    path = "/kv/status",
    responses(
        (status = 200, description = "KV service status", body = KvStatusResponse),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn kv_status(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<KvAppState>>,
) -> Result<impl IntoResponse, Problem> {
    // Status check requires SystemRead permission
    permission_guard!(auth, SystemRead);

    let healthy = state.redis_service.health_check().await.unwrap_or(false);

    let (version, docker_image) = if healthy {
        let version = state.redis_service.get_current_version().await.ok();
        let image = state
            .redis_service
            .get_current_docker_image()
            .await
            .ok()
            .map(|(name, tag)| format!("{}:{}", name, tag));
        (version, image)
    } else {
        (None, None)
    };

    Ok(Json(KvStatusResponse {
        enabled: healthy,
        healthy,
        version,
        docker_image,
    }))
}

/// Enable KV service
#[utoipa::path(
    tag = "KV Management",
    post,
    path = "/kv/enable",
    request_body = EnableKvRequest,
    responses(
        (status = 200, description = "KV service enabled", body = EnableKvResponse),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn kv_enable(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<KvAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<EnableKvRequest>,
) -> Result<impl IntoResponse, Problem> {
    // Enable requires SystemAdmin permission
    permission_guard!(auth, SystemAdmin);

    info!("Enabling KV service with config: {:?}", request);

    // Start the Redis service
    if let Err(e) = state.redis_service.start().await {
        error!("Failed to start KV service: {}", e);
        return Err(
            temps_core::problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Failed to Enable KV Service")
                .with_detail(format!("Could not start Redis container: {}", e)),
        );
    }

    // Get status after starting
    let healthy = state.redis_service.health_check().await.unwrap_or(false);
    let version = state.redis_service.get_current_version().await.ok();
    let docker_image = state
        .redis_service
        .get_current_docker_image()
        .await
        .ok()
        .map(|(name, tag)| format!("{}:{}", name, tag));

    // Create audit log
    let audit = KvServiceEnabledAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        service_name: "temps-kv".to_string(),
        docker_image: docker_image.clone(),
        version: version.clone(),
    };

    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    Ok(Json(EnableKvResponse {
        success: true,
        message: "KV service enabled successfully".to_string(),
        status: KvStatusResponse {
            enabled: true,
            healthy,
            version,
            docker_image,
        },
    }))
}

/// Disable KV service
#[utoipa::path(
    tag = "KV Management",
    delete,
    path = "/kv/disable",
    responses(
        (status = 200, description = "KV service disabled", body = DisableKvResponse),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn kv_disable(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<KvAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<impl IntoResponse, Problem> {
    // Disable requires SystemAdmin permission
    permission_guard!(auth, SystemAdmin);

    info!("Disabling KV service");

    // Stop the Redis service
    if let Err(e) = state.redis_service.stop().await {
        error!("Failed to stop KV service: {}", e);
        return Err(
            temps_core::problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Failed to Disable KV Service")
                .with_detail(format!("Could not stop Redis container: {}", e)),
        );
    }

    // Create audit log
    let audit = KvServiceDisabledAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        service_name: "temps-kv".to_string(),
    };

    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    Ok(Json(DisableKvResponse {
        success: true,
        message: "KV service disabled successfully".to_string(),
    }))
}
