//! HTTP handlers for KV operations

use std::sync::Arc;

use axum::{
    extract::{Extension, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, patch, post},
    Json, Router,
};
use std::collections::HashMap;
use temps_auth::{permission_guard, RequireAuth};
use temps_core::problemdetails::Problem;
use temps_core::RequestMetadata;
use temps_providers::externalsvc::{ExternalService, ServiceType};
use temps_providers::{CreateExternalServiceRequest, UpdateExternalServiceRequest};
use tracing::{error, info};
use utoipa::OpenApi;

use super::audit::{
    AuditContext, KvServiceDisabledAudit, KvServiceEnabledAudit, KvServiceUpdatedAudit,
};

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
        kv_update,
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
            UpdateKvRequest,
            UpdateKvResponse,
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
        .route("/kv/update", patch(kv_update))
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
    let project_id = extract_project_id(&auth, request.project_id)?;

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
    let project_id = extract_project_id(&auth, request.project_id)?;

    info!(
        "KV SET request: key={}, project_id={}",
        request.key, project_id
    );

    let options = SetOptions {
        ex: request.ex,
        px: request.px,
        nx: request.nx,
        xx: request.xx,
    };

    match state
        .kv_service
        .set(project_id, &request.key, request.value, options)
        .await
    {
        Ok(_) => {
            info!("KV SET success: key={}", request.key);
            Ok(Json(SetResponse {
                result: "OK".to_string(),
            }))
        }
        Err(e) => {
            error!("KV SET failed: key={}, error={}", request.key, e);
            Err(e.into())
        }
    }
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
    let project_id = extract_project_id(&auth, request.project_id)?;

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
    let project_id = extract_project_id(&auth, request.project_id)?;

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
    let project_id = extract_project_id(&auth, request.project_id)?;

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
    let project_id = extract_project_id(&auth, request.project_id)?;

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
    let project_id = extract_project_id(&auth, request.project_id)?;

    let keys = state.kv_service.keys(project_id, &request.pattern).await?;

    Ok(Json(KeysResponse { keys }))
}

/// Extract project_id from request body or authentication context
///
/// Priority:
/// 1. Deployment tokens: Use project_id from token (request body ignored for security)
/// 2. API keys/sessions: Use project_id from request body (required)
fn extract_project_id(
    auth: &temps_auth::AuthContext,
    request_project_id: Option<i32>,
) -> Result<i32, Problem> {
    // For deployment tokens, always use the token's project_id (security: prevent access to other projects)
    if let Some(token_project_id) = auth.project_id() {
        return Ok(token_project_id);
    }

    // For API keys and sessions, require project_id in the request body
    request_project_id.ok_or_else(|| {
        temps_core::problemdetails::new(axum::http::StatusCode::BAD_REQUEST)
            .with_title("Project ID Required")
            .with_detail(
                "The 'project_id' field is required in the request body for API key or session authentication",
            )
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

    // Check if the service exists in the database via ExternalServiceManager
    let service_result = state
        .external_service_manager
        .get_service_by_name("temps-kv")
        .await;

    match service_result {
        Ok(service) => {
            // Service exists in database, get full details
            let details = state
                .external_service_manager
                .get_service_details(service.id)
                .await
                .ok();

            // Check service status
            let is_running = service.status == "running";
            let is_stopped = service.status == "stopped";

            // Service is enabled if it exists and is not stopped
            let enabled = !is_stopped;
            let healthy = is_running;

            // Get docker_image from parameters
            let docker_image = details
                .as_ref()
                .and_then(|d| d.current_parameters.as_ref())
                .and_then(|p| p.get("docker_image").cloned())
                .and_then(|v| v.as_str().map(String::from));

            // Extract version from docker_image tag (e.g., "redis:8-alpine" -> "8-alpine")
            // This ensures the version always matches the actual docker image being used
            let version = docker_image
                .as_ref()
                .and_then(|img| img.split(':').nth(1))
                .map(String::from)
                .or_else(|| Some("8-alpine".to_string()));

            Ok(Json(KvStatusResponse {
                enabled,
                healthy,
                version,
                docker_image,
            }))
        }
        Err(_) => {
            // Service not found in database - not enabled
            Ok(Json(KvStatusResponse {
                enabled: false,
                healthy: false,
                version: None,
                docker_image: None,
            }))
        }
    }
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

    // Check if the service already exists (might be stopped)
    let existing_service = state
        .external_service_manager
        .get_service_by_name("temps-kv")
        .await
        .ok();

    let service_info = if let Some(existing) = existing_service {
        // Service exists - start it (will be a no-op if already running)
        info!(
            "KV service exists with status '{}', ensuring it's running...",
            existing.status
        );

        // Get the service config from the database and initialize the plugin's RedisService
        // This is necessary because the plugin may have skipped initialization if the service was stopped
        let service_config = state
            .external_service_manager
            .get_service_config(existing.id)
            .await
            .map_err(|e| {
                error!("Failed to get KV service config: {}", e);
                temps_core::problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Failed to Enable KV Service")
                    .with_detail(format!("Could not get KV service config: {}", e))
            })?;

        info!(
            "Retrieved KV service config (service_id: {}), initializing RedisService...",
            existing.id
        );

        // Initialize the plugin's RedisService with the config from database
        if let Err(e) = state.redis_service.init(service_config).await {
            error!("Failed to initialize RedisService: {}", e);
            return Err(
                temps_core::problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Failed to Enable KV Service")
                    .with_detail(format!("Could not initialize Redis service: {}", e)),
            );
        }

        info!("RedisService initialized, starting container...");

        // Start the container through the plugin's RedisService
        if let Err(e) = state.redis_service.start().await {
            // Log but continue - container might already be running
            info!(
                "Redis container start returned: {} (may already be running)",
                e
            );
        }

        // Start via ExternalServiceManager to update DB status
        state
            .external_service_manager
            .start_service(existing.id)
            .await
            .map_err(|e| {
                error!("Failed to update KV service status in database: {}", e);
                temps_core::problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Failed to Enable KV Service")
                    .with_detail(format!("Could not start KV service: {}", e))
            })?
    } else {
        // Service doesn't exist, create it
        info!("KV service doesn't exist, creating new service...");

        // Build parameters for Redis service creation
        let mut parameters: HashMap<String, serde_json::Value> = HashMap::new();
        if let Some(docker_image) = &request.docker_image {
            parameters.insert("docker_image".to_string(), serde_json::json!(docker_image));
        }
        if let Some(max_memory) = &request.max_memory {
            parameters.insert("max_memory".to_string(), serde_json::json!(max_memory));
        }
        parameters.insert(
            "persistence".to_string(),
            serde_json::json!(request.persistence),
        );

        // Extract version from docker_image (e.g., "redis:8-alpine" -> "7-alpine")
        let version = parameters
            .get("docker_image")
            .and_then(|v| v.as_str())
            .and_then(|img| img.split(':').nth(1))
            .map(String::from)
            .or_else(|| Some("8-alpine".to_string())); // Default Redis version

        // Create service request for ExternalServiceManager
        let create_request = CreateExternalServiceRequest {
            name: "temps-kv".to_string(),
            service_type: ServiceType::Redis,
            version,
            parameters,
        };

        // Create the service through ExternalServiceManager
        // This creates the database record AND initializes/starts the container
        state
            .external_service_manager
            .create_service(create_request)
            .await
            .map_err(|e| {
                error!("Failed to create KV service: {}", e);
                temps_core::problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Failed to Enable KV Service")
                    .with_detail(format!("Could not create Redis service: {}", e))
            })?
    };

    // Get status from the service
    let healthy = service_info.status == "running";
    let version = service_info.version.clone();

    // Get docker image from service details
    let docker_image = state
        .external_service_manager
        .get_service_details(service_info.id)
        .await
        .ok()
        .and_then(|details| details.current_parameters)
        .and_then(|p| p.get("docker_image").cloned())
        .and_then(|v| v.as_str().map(String::from));

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

/// Update KV service configuration
#[utoipa::path(
    tag = "KV Management",
    patch,
    path = "/kv/update",
    request_body = UpdateKvRequest,
    responses(
        (status = 200, description = "KV service updated", body = UpdateKvResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "KV service not enabled"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn kv_update(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<KvAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<UpdateKvRequest>,
) -> Result<impl IntoResponse, Problem> {
    // Update requires SystemAdmin permission
    permission_guard!(auth, SystemAdmin);

    info!("Updating KV service with config: {:?}", request);

    // Get existing service
    let service = state
        .external_service_manager
        .get_service_by_name("temps-kv")
        .await
        .map_err(|_| {
            temps_core::problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("KV Service Not Found")
                .with_detail("KV service is not enabled. Enable it first before updating.")
        })?;

    // Get current details for audit log
    let current_details = state
        .external_service_manager
        .get_service_details(service.id)
        .await
        .ok();

    let old_docker_image = current_details
        .as_ref()
        .and_then(|d| d.current_parameters.as_ref())
        .and_then(|p| p.get("docker_image").cloned())
        .and_then(|v| v.as_str().map(String::from));

    let old_version = current_details
        .as_ref()
        .and_then(|d| d.service.version.clone());

    // Build parameters for update
    let mut parameters: HashMap<String, serde_json::Value> = HashMap::new();
    if let Some(docker_image) = &request.docker_image {
        parameters.insert("docker_image".to_string(), serde_json::json!(docker_image));
    }

    // Extract version from docker_image
    let new_version = request
        .docker_image
        .as_ref()
        .and_then(|img| img.split(':').nth(1))
        .map(String::from);

    // Build update request
    let update_request = UpdateExternalServiceRequest {
        name: None,
        parameters,
        docker_image: request.docker_image.clone(),
    };

    // Update the service
    let updated_service = state
        .external_service_manager
        .update_service(service.id, update_request)
        .await
        .map_err(|e| {
            error!("Failed to update KV service: {}", e);
            temps_core::problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Failed to Update KV Service")
                .with_detail(format!("Could not update Redis service: {}", e))
        })?;

    // Get updated docker image from service details
    let new_docker_image = state
        .external_service_manager
        .get_service_details(updated_service.id)
        .await
        .ok()
        .and_then(|details| details.current_parameters)
        .and_then(|p| p.get("docker_image").cloned())
        .and_then(|v| v.as_str().map(String::from));

    // Create audit log
    let audit = KvServiceUpdatedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        service_name: "temps-kv".to_string(),
        old_docker_image,
        new_docker_image: new_docker_image.clone(),
        old_version,
        new_version: new_version.clone(),
    };

    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    let healthy = updated_service.status == "running";

    Ok(Json(UpdateKvResponse {
        success: true,
        message:
            "KV service updated successfully. Restart may be required for changes to take effect."
                .to_string(),
        status: KvStatusResponse {
            enabled: true,
            healthy,
            version: new_version,
            docker_image: new_docker_image,
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
        (status = 404, description = "KV service not enabled"),
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

    // Get the service record
    let service = state
        .external_service_manager
        .get_service_by_name("temps-kv")
        .await
        .map_err(|_| {
            temps_core::problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("KV Service Not Found")
                .with_detail("KV service is not enabled")
        })?;

    // Stop the service through external_service_manager (stops container + updates DB status)
    state
        .external_service_manager
        .stop_service(service.id)
        .await
        .map_err(|e| {
            error!("Failed to stop KV service: {}", e);
            temps_core::problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Failed to Disable KV Service")
                .with_detail(format!("Could not stop KV service: {}", e))
        })?;

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
