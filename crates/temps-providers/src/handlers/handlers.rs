use std::collections::HashMap;
use std::sync::Arc;

use super::types::AppState;
use axum::{
    extract::{Extension, Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, post, put},
    Json, Router,
};
use temps_auth::permission_guard;
use temps_auth::RequireAuth;
use temps_core::{
    error_builder::{bad_request, forbidden, internal_server_error, not_found},
    problemdetails::Problem,
};
use tracing::{error, info};
use utoipa::OpenApi;

use super::audit::{
    ExternalServiceCreatedAudit, ExternalServiceDeletedAudit, ExternalServiceStatusChangedAudit,
    ExternalServiceUpdatedAudit,
};
use crate::handlers::types::{
    AvailableContainerInfo, CreateExternalServiceRequest, EnvironmentVariableInfo,
    ExternalServiceDetails, ExternalServiceInfo, ImportExternalServiceRequest, LinkServiceRequest,
    ProjectServiceInfo, ProviderMetadata, ServiceParameter, ServiceTypeInfo, ServiceTypeRoute,
    UpdateExternalServiceRequest, UpgradeExternalServiceRequest,
};
use crate::services::EnvironmentVariableOptions;
use temps_core::AuditContext;
use temps_core::RequestMetadata;

/// Get available service types
#[utoipa::path(
    get,
    path = "/external-services/types",
    tag = "External Services",
    responses(
        (status = 200, description = "List of available service types", body = Vec<ServiceTypeRoute>),
        (status = 500, description = "Internal server error")
    )
)]
async fn get_service_types(RequireAuth(auth): RequireAuth) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesRead);

    let service_types: Vec<ServiceTypeRoute> = ServiceTypeRoute::get_all();
    Ok((StatusCode::OK, Json(service_types)))
}

/// Get provider metadata (display names, icons, descriptions)
#[utoipa::path(
    get,
    path = "/external-services/providers/metadata",
    tag = "External Services",
    responses(
        (status = 200, description = "List of provider metadata", body = Vec<ProviderMetadata>),
        (status = 500, description = "Internal server error")
    )
)]
async fn get_providers_metadata(
    RequireAuth(auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesRead);

    let metadata = ProviderMetadata::get_all();
    Ok((StatusCode::OK, Json(metadata)))
}

/// Get metadata for a specific provider
#[utoipa::path(
    get,
    path = "/external-services/providers/metadata/{service_type}",
    tag = "External Services",
    responses(
        (status = 200, description = "Provider metadata", body = ProviderMetadata),
        (status = 404, description = "Provider not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("service_type" = String, Path, description = "Service type (mongodb, postgres, redis, s3)")
    )
)]
async fn get_provider_metadata(
    RequireAuth(auth): RequireAuth,
    Path(service_type): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesRead);

    match ServiceTypeRoute::from_str(&service_type) {
        Ok(service_type) => match ProviderMetadata::get_by_type(&service_type) {
            Some(metadata) => Ok((StatusCode::OK, Json(metadata))),
            None => Err(not_found().detail("Provider metadata not found").build()),
        },
        Err(_) => Err(not_found().detail("Invalid service type").build()),
    }
}

/// List available Docker containers that can be imported as services
#[utoipa::path(
    get,
    path = "/external-services/available-containers",
    tag = "External Services",
    responses(
        (status = 200, description = "List of available containers", body = Vec<AvailableContainerInfo>),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
async fn list_available_containers(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesRead);

    let containers = state
        .external_service_manager
        .list_available_containers()
        .await
        .map_err(|e| {
            error!("Failed to list available containers: {}", e);
            internal_server_error()
                .detail("Failed to list available containers")
                .build()
        })?;

    let response: Vec<AvailableContainerInfo> = containers
        .into_iter()
        .map(|c| AvailableContainerInfo {
            container_id: c.container_id,
            container_name: c.container_name,
            image: c.image,
            version: c.version,
            service_type: ServiceTypeRoute::from(c.service_type),
            is_running: c.is_running,
            exposed_ports: c.exposed_ports,
        })
        .collect();

    Ok((StatusCode::OK, Json(response)))
}

/// Import an existing Docker container as a managed external service
#[utoipa::path(
    post,
    path = "/external-services/import",
    tag = "External Services",
    request_body = ImportExternalServiceRequest,
    responses(
        (status = 201, description = "Service imported successfully", body = ExternalServiceInfo),
        (status = 400, description = "Invalid request"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
async fn import_external_service(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<ImportExternalServiceRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesCreate);

    // Convert handler-layer request to service-layer request
    let service_type =
        crate::ServiceType::from_str(&request.service_type.to_string()).map_err(|e| {
            error!("Invalid service type: {}", e);
            bad_request()
                .detail(format!("Invalid service type: {}", e))
                .build()
        })?;

    let service_request = crate::services::ImportExternalServiceRequest {
        name: request.name.clone(),
        service_type,
        version: request.version,
        parameters: request.parameters.clone(),
        container_id: request.container_id.clone(),
    };

    let service = state
        .external_service_manager
        .import_service(service_request)
        .await
        .map_err(|e| {
            error!("Failed to import service: {}", e);
            bad_request()
                .detail(format!("Failed to import service: {}", e))
                .build()
        })?;

    // Log audit event
    let audit = ExternalServiceCreatedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        service_id: service.id,
        name: service.name.clone(),
        service_type: service.service_type.to_string(),
        version: service.version.clone(),
    };

    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    Ok((StatusCode::CREATED, Json(service)))
}

pub fn configure_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/external-services", get(list_services))
        .route("/external-services", post(create_service))
        .route(
            "/external-services/available-containers",
            get(list_available_containers),
        )
        .route("/external-services/import", post(import_external_service))
        .route("/external-services/types", get(get_service_types))
        .route(
            "/external-services/providers/metadata",
            get(get_providers_metadata),
        )
        .route(
            "/external-services/providers/metadata/{service_type}",
            get(get_provider_metadata),
        )
        .route(
            "/external-services/types/{service_type}/parameters",
            get(get_service_type_parameters),
        )
        .route("/external-services/{id}", get(get_service))
        .route("/external-services/{id}", put(update_service))
        .route("/external-services/{id}", delete(delete_service))
        .route("/external-services/{id}/health", get(check_health))
        .route("/external-services/{id}/start", post(start_service))
        .route("/external-services/{id}/stop", post(stop_service))
        .route("/external-services/{id}/upgrade", post(upgrade_service))
        .route(
            "/external-services/{id}/projects",
            post(link_service_to_project),
        )
        .route(
            "/external-services/{id}/projects/{project_id}",
            delete(unlink_service_from_project),
        )
        .route(
            "/external-services/{id}/projects",
            get(list_service_projects),
        )
        .route(
            "/external-services/projects/{project_id}",
            get(list_project_services),
        )
        .route(
            "/external-services/{id}/projects/{project_id}/environment/{var_name}",
            get(get_service_environment_variable),
        )
        .route(
            "/external-services/{id}/projects/{project_id}/environment",
            get(get_service_environment_variables),
        )
        .route(
            "/external-services/projects/{project_id}/environment",
            get(get_project_service_environment_variables),
        )
        .route(
            "/external-services/{id}/preview-environment-names",
            get(get_service_preview_environment_variable_names),
        )
        .route(
            "/external-services/{id}/preview-environment-masked",
            get(get_service_preview_environment_variables_masked),
        )
        .route(
            "/external-services/by-slug/{slug}",
            get(get_service_by_slug),
        )
        .merge(super::query_handlers::configure_query_routes())
}

/// Get parameter schema for a specific service type
#[utoipa::path(
    get,
    path = "/external-services/types/{service_type}/parameters",
    tag = "External Services",
    responses(
        (status = 200, description = "Service type parameter schema"),
        (status = 404, description = "Service type not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("service_type" = String, Path, description = "Service type")
    )
)]
async fn get_service_type_parameters(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path(service_type): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesRead);

    match ServiceTypeRoute::from_str(&service_type) {
        Ok(service_type) => match app_state
            .external_service_manager
            .get_service_type_schema(service_type.into())
            .await
        {
            Ok(schema) => Ok((StatusCode::OK, Json(schema))),
            Err(e) => Err(internal_server_error()
                .detail(format!("Failed to get parameter schema: {}", e))
                .build()),
        },
        Err(_) => Err(not_found().detail("Service type not found").build()),
    }
}

/// Get all external services
#[utoipa::path(
    get,
    path = "/external-services",
    tag = "External Services",
    responses(
        (status = 200, description = "List of external services", body = Vec<ExternalServiceInfo>),
        (status = 500, description = "Internal server error")
    )
)]
async fn list_services(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesRead);

    match app_state.external_service_manager.list_services().await {
        Ok(services) => Ok((StatusCode::OK, Json(services))),
        Err(e) => {
            error!("Failed to list services: {}", e);
            Err(internal_server_error()
                .detail(format!("Failed to list services: {}", e))
                .build())
        }
    }
}

/// Get external service details
#[utoipa::path(
    get,
    path = "/external-services/{id}",
    tag = "External Services",
    responses(
        (status = 200, description = "External service details", body = ExternalServiceDetails),
        (status = 404, description = "Service not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("id" = i32, Path, description = "External service ID")
    )
)]
async fn get_service(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesRead);

    match app_state
        .external_service_manager
        .get_service_details(id)
        .await
    {
        Ok(service) => Ok((StatusCode::OK, Json(service))),
        Err(e) => match e.to_string().as_str() {
            "Service not found" => Err(not_found().detail("Service not found").build()),
            _ => Err(internal_server_error()
                .detail(format!("Failed to get service: {}", e))
                .build()),
        },
    }
}

/// Create new external service
#[utoipa::path(
    post,
    path = "/external-services",
    tag = "External Services",
    request_body = CreateExternalServiceRequest,
    responses(
        (status = 201, description = "Service created successfully", body = ExternalServiceInfo),
        (status = 400, description = "Invalid request"),
        (status = 500, description = "Internal server error")
    )
)]
async fn create_service(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<CreateExternalServiceRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesCreate);

    let service_config = crate::services::CreateExternalServiceRequest {
        name: request.name.clone(),
        service_type: request.service_type.into(),
        version: request.version.clone(),
        parameters: request.parameters,
    };

    match app_state
        .external_service_manager
        .create_service(service_config)
        .await
    {
        Ok(service) => {
            // Create audit log with metadata
            let audit = ExternalServiceCreatedAudit {
                context: AuditContext {
                    user_id: auth.user_id(),
                    ip_address: Some(metadata.ip_address.clone()),
                    user_agent: metadata.user_agent.clone(),
                },
                service_id: service.id,
                name: service.name.clone(),
                service_type: service.service_type.to_string(),
                version: service.version.clone(),
            };

            if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
                error!("Failed to create audit log: {}", e);
            }

            Ok((StatusCode::CREATED, Json(service)))
        }
        Err(e) => {
            let error_msg = e.to_string();
            info!("Failed to create service: {}", error_msg);
            if error_msg.contains("validation failed") {
                Err(bad_request().detail(&error_msg).build())
            } else {
                Err(internal_server_error()
                    .detail(format!("Failed to create service: {}", e))
                    .build())
            }
        }
    }
}

/// Update external service
#[utoipa::path(
    put,
    path = "/external-services/{id}",
    tag = "External Services",
    request_body = UpdateExternalServiceRequest,
    responses(
        (status = 200, description = "Service updated successfully", body = ExternalServiceInfo),
        (status = 400, description = "Invalid request"),
        (status = 404, description = "Service not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("id" = i32, Path, description = "External service ID")
    )
)]
async fn update_service(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path(id): Path<i32>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<UpdateExternalServiceRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesWrite);

    let service_config = crate::services::UpdateExternalServiceRequest {
        parameters: request.parameters.clone(),
        name: None,
        docker_image: request.docker_image.clone(),
    };

    match app_state
        .external_service_manager
        .update_service(id, service_config)
        .await
    {
        Ok(service) => {
            // Convert parameters to strings for audit log
            let params_as_strings: HashMap<String, String> = request
                .parameters
                .iter()
                .map(|(k, v)| {
                    let v_str = match v {
                        serde_json::Value::String(s) => s.clone(),
                        serde_json::Value::Number(n) => n.to_string(),
                        serde_json::Value::Bool(b) => b.to_string(),
                        serde_json::Value::Null => String::new(),
                        _ => v.to_string(),
                    };
                    (k.clone(), v_str)
                })
                .collect();

            // Create audit log with metadata
            let audit = ExternalServiceUpdatedAudit {
                context: AuditContext {
                    user_id: auth.user_id(),
                    ip_address: Some(metadata.ip_address.clone()),
                    user_agent: metadata.user_agent.clone(),
                },
                service_id: service.id,
                name: service.name.clone(),
                service_type: service.service_type.to_string(),
                updated_parameters: params_as_strings,
            };

            if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
                error!("Failed to create audit log: {}", e);
            }

            Ok((StatusCode::OK, Json(service)))
        }
        Err(e) => match e.to_string().as_str() {
            "Service not found" => Err(not_found().detail("Service not found").build()),
            _ if e.to_string().contains("validation failed") => {
                Err(bad_request().detail(e.to_string()).build())
            }
            _ => Err(internal_server_error()
                .detail(format!("Failed to update service: {}", e))
                .build()),
        },
    }
}

/// Upgrade external service to new Docker image with data migration
/// This endpoint uses service-specific upgrade procedures (e.g., pg_upgrade for PostgreSQL)
#[utoipa::path(
    post,
    path = "/external-services/{id}/upgrade",
    tag = "External Services",
    request_body = UpgradeExternalServiceRequest,
    responses(
        (status = 200, description = "Service upgraded successfully", body = ExternalServiceInfo),
        (status = 400, description = "Invalid request or upgrade not supported"),
        (status = 404, description = "Service not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("id" = i32, Path, description = "External service ID")
    )
)]
async fn upgrade_service(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path(id): Path<i32>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<UpgradeExternalServiceRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesWrite);

    match app_state
        .external_service_manager
        .upgrade_service(id, request.docker_image.clone())
        .await
    {
        Ok(service) => {
            // Create audit log
            let audit = ExternalServiceUpdatedAudit {
                context: AuditContext {
                    user_id: auth.user_id(),
                    ip_address: Some(metadata.ip_address.clone()),
                    user_agent: metadata.user_agent.clone(),
                },
                service_id: service.id,
                name: service.name.clone(),
                service_type: service.service_type.to_string(),
                updated_parameters: HashMap::from([(
                    "docker_image".to_string(),
                    request.docker_image,
                )]),
            };

            if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
                error!("Failed to create audit log: {}", e);
            }

            Ok((StatusCode::OK, Json(service)))
        }
        Err(e) => match e.to_string().as_str() {
            "Service not found" => Err(not_found().detail("Service not found").build()),
            msg if msg.contains("Upgrade not implemented") => {
                Err(bad_request().detail(msg).build())
            }
            _ => Err(internal_server_error()
                .detail(format!("Failed to upgrade service: {}", e))
                .build()),
        },
    }
}

/// Delete external service
#[utoipa::path(
    delete,
    path = "/external-services/{id}",
    tag = "External Services",
    responses(
        (status = 204, description = "Service deleted successfully"),
        (status = 400, description = "Cannot delete: service is still linked to projects"),
        (status = 404, description = "Service not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("id" = i32, Path, description = "External service ID")
    )
)]
async fn delete_service(
    State(app_state): State<Arc<AppState>>,
    Path(id): Path<i32>,
    RequireAuth(auth): RequireAuth,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesDelete);
    match app_state
        .external_service_manager
        .get_service_details(id)
        .await
    {
        Ok(service_details) => {
            match app_state.external_service_manager.delete_service(id).await {
                Ok(_) => {
                    // Create audit log with metadata
                    let audit = ExternalServiceDeletedAudit {
                        context: AuditContext {
                            user_id: auth.user_id(),
                            ip_address: Some(metadata.ip_address.clone()),
                            user_agent: metadata.user_agent.clone(),
                        },
                        service_id: id,
                        name: service_details.service.name,
                        service_type: service_details.service.service_type.to_string(),
                    };

                    if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
                        error!("Failed to create audit log: {}", e);
                    }

                    Ok(StatusCode::NO_CONTENT)
                }
                Err(e) => {
                    // Check for specific error types
                    let error_str = e.to_string();
                    if error_str.contains("Service not found") {
                        Err(not_found().detail("Service not found").build())
                    } else if error_str.contains("still linked to") {
                        // Return 400 Bad Request with detailed message about linked projects
                        Err(bad_request().detail(error_str).build())
                    } else {
                        Err(internal_server_error()
                            .detail(format!("Failed to delete service: {}", e))
                            .build())
                    }
                }
            }
        }
        Err(e) => Err(internal_server_error()
            .detail(format!("Failed to get service details: {}", e))
            .build()),
    }
}

/// Check service health
#[utoipa::path(
    get,
    path = "/external-services/{id}/health",
    tag = "External Services",
    responses(
        (status = 200, description = "Health check result", body = bool),
        (status = 404, description = "Service not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("id" = i32, Path, description = "External service ID")
    )
)]
async fn check_health(
    State(app_state): State<Arc<AppState>>,
    Path(id): Path<i32>,
    RequireAuth(auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesRead);

    match app_state
        .external_service_manager
        .check_service_health(id)
        .await
    {
        Ok(health) => Ok((StatusCode::OK, Json(health))),
        Err(e) => match e.to_string().as_str() {
            "Service not found" => Err(not_found().detail("Service not found").build()),
            _ => Err(internal_server_error()
                .detail(format!("Health check failed: {}", e))
                .build()),
        },
    }
}

/// Start an external service
#[utoipa::path(
    post,
    path = "/external-services/{id}/start",
    tag = "External Services",
    responses(
        (status = 200, description = "Service started successfully", body = ExternalServiceInfo),
        (status = 404, description = "Service not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("id" = i32, Path, description = "External service ID")
    )
)]
async fn start_service(
    State(app_state): State<Arc<AppState>>,
    Path(id): Path<i32>,
    RequireAuth(auth): RequireAuth,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesWrite);
    match app_state
        .external_service_manager
        .get_service_details(id)
        .await
    {
        Ok(service_details) => {
            match app_state
                .external_service_manager
                .start_service(service_details.service.id)
                .await
            {
                Ok(service) => {
                    // Create audit log with metadata
                    let audit = ExternalServiceStatusChangedAudit {
                        context: AuditContext {
                            user_id: auth.user_id(),
                            ip_address: Some(metadata.ip_address.clone()),
                            user_agent: metadata.user_agent.clone(),
                        },
                        service_id: service.id,
                        name: service.name.clone(),
                        service_type: service.service_type.to_string(),
                        new_status: "started".to_string(),
                    };

                    if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
                        error!("Failed to create audit log: {}", e);
                    }

                    Ok((StatusCode::OK, Json(service)))
                }
                Err(e) => {
                    error!("Failed to start service: {}", e);
                    match e.to_string().as_str() {
                        "Service not found" => Err(not_found().detail("Service not found").build()),
                        _ => Err(internal_server_error()
                            .detail(format!("Failed to start service: {}", e))
                            .build()),
                    }
                }
            }
        }
        Err(e) => Err(internal_server_error()
            .detail(format!("Failed to get service details: {}", e))
            .build()),
    }
}

/// Stop an external service
#[utoipa::path(
    post,
    path = "/external-services/{id}/stop",
    tag = "External Services",
    responses(
        (status = 200, description = "Service stopped successfully", body = ExternalServiceInfo),
        (status = 404, description = "Service not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("id" = i32, Path, description = "External service ID")
    )
)]
async fn stop_service(
    State(app_state): State<Arc<AppState>>,
    Path(id): Path<i32>,
    RequireAuth(auth): RequireAuth,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesWrite);
    match app_state
        .external_service_manager
        .get_service_details(id)
        .await
    {
        Ok(service_details) => {
            match app_state
                .external_service_manager
                .stop_service(service_details.service.id)
                .await
            {
                Ok(service) => {
                    // Create audit log with metadata
                    let audit = ExternalServiceStatusChangedAudit {
                        context: AuditContext {
                            user_id: auth.user_id(),
                            ip_address: Some(metadata.ip_address.clone()),
                            user_agent: metadata.user_agent.clone(),
                        },
                        service_id: service.id,
                        name: service.name.clone(),
                        service_type: service.service_type.to_string(),
                        new_status: "stopped".to_string(),
                    };

                    if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
                        error!("Failed to create audit log: {}", e);
                    }

                    Ok((StatusCode::OK, Json(service)))
                }
                Err(e) => {
                    error!("Failed to stop service: {}", e);
                    match e.to_string().as_str() {
                        "Service not found" => Err(not_found().detail("Service not found").build()),
                        _ => Err(internal_server_error()
                            .detail(format!("Failed to stop service: {}", e))
                            .build()),
                    }
                }
            }
        }
        Err(e) => Err(internal_server_error()
            .detail(format!("Failed to get service details: {}", e))
            .build()),
    }
}

/// Link service to project
#[utoipa::path(
    post,
    path = "/external-services/{id}/projects",
    tag = "External Services",
    request_body = LinkServiceRequest,
    responses(
        (status = 201, description = "Service linked to project successfully", body = ProjectServiceInfo),
        (status = 404, description = "Service or project not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("id" = i32, Path, description = "External service ID")
    )
)]
async fn link_service_to_project(
    State(app_state): State<Arc<AppState>>,
    Path(id): Path<i32>,
    RequireAuth(auth): RequireAuth,
    Json(request): Json<LinkServiceRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesWrite);

    match app_state
        .external_service_manager
        .link_service_to_project(id, request.project_id)
        .await
    {
        Ok(info) => Ok((StatusCode::CREATED, Json(info))),
        Err(e) => match e.to_string().as_str() {
            "Service not found" | "Project not found" => {
                Err(not_found().detail(e.to_string()).build())
            }
            _ => Err(internal_server_error()
                .detail(format!("Failed to link service: {}", e))
                .build()),
        },
    }
}

/// Unlink service from project
#[utoipa::path(
    delete,
    path = "/external-services/{id}/projects/{project_id}",
    tag = "External Services",
    responses(
        (status = 204, description = "Service unlinked from project successfully"),
        (status = 404, description = "Service link not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("id" = i32, Path, description = "External service ID"),
        ("project_id" = i32, Path, description = "Project ID")
    )
)]
async fn unlink_service_from_project(
    State(app_state): State<Arc<AppState>>,
    Path((id, project_id)): Path<(i32, i32)>,
    RequireAuth(auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesWrite);

    match app_state
        .external_service_manager
        .unlink_service_from_project(id, project_id)
        .await
    {
        Ok(_) => Ok(StatusCode::NO_CONTENT),
        Err(e) => match e.to_string().as_str() {
            "Service link not found" => Err(not_found().detail(e.to_string()).build()),
            _ => Err(internal_server_error()
                .detail(format!("Failed to unlink service: {}", e))
                .build()),
        },
    }
}

/// List projects linked to service
#[utoipa::path(
    get,
    path = "/external-services/{id}/projects",
    tag = "External Services",
    responses(
        (status = 200, description = "List of linked projects", body = Vec<ProjectServiceInfo>),
        (status = 404, description = "Service not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("id" = i32, Path, description = "External service ID")
    )
)]
async fn list_service_projects(
    State(app_state): State<Arc<AppState>>,
    Path(id): Path<i32>,
    RequireAuth(auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesRead);

    match app_state
        .external_service_manager
        .list_service_projects(id)
        .await
    {
        Ok(projects) => Ok((StatusCode::OK, Json(projects))),
        Err(e) => match e.to_string().as_str() {
            "Service not found" => Err(not_found().detail("Service not found").build()),
            _ => Err(internal_server_error()
                .detail(format!("Failed to list projects: {}", e))
                .build()),
        },
    }
}

/// List services linked to a project
#[utoipa::path(
    get,
    path = "/external-services/projects/{project_id}",
    tag = "External Services",
    responses(
        (status = 200, description = "List of services linked to project", body = Vec<ProjectServiceInfo>),
        (status = 404, description = "Project not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID")
    )
)]
async fn list_project_services(
    State(app_state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
    RequireAuth(auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesRead);

    match app_state
        .external_service_manager
        .list_project_services(project_id)
        .await
    {
        Ok(services) => Ok((StatusCode::OK, Json(services))),
        Err(e) => match e.to_string().as_str() {
            "Project not found" => Err(not_found().detail("Project not found").build()),
            _ => Err(internal_server_error()
                .detail(format!("Failed to list services: {}", e))
                .build()),
        },
    }
}

/// Get specific environment variable for a service-project pair
#[utoipa::path(
    get,
    path = "/external-services/{id}/projects/{project_id}/environment/{var_name}",
    tag = "External Services",
    responses(
        (status = 200, description = "Environment variable value", body = EnvironmentVariableInfo),
        (status = 404, description = "Service, project, or variable not found"),
        (status = 403, description = "Access denied for encrypted variable"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("id" = i32, Path, description = "External service ID"),
        ("project_id" = i32, Path, description = "Project ID"),
        ("var_name" = String, Path, description = "Environment variable name")
    )
)]
async fn get_service_environment_variable(
    State(app_state): State<Arc<AppState>>,
    Path((id, project_id, var_name)): Path<(i32, i32, String)>,
    RequireAuth(auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesRead);

    match app_state
        .external_service_manager
        .get_service_environment_variable(id, project_id, &var_name)
        .await
    {
        Ok(var_info) => Ok((StatusCode::OK, Json(var_info))),
        Err(e) => match e.to_string().as_str() {
            "Service not found" | "Project not found" | "Variable not found" => {
                Err(not_found().detail(e.to_string()).build())
            }
            "Access denied for encrypted variable" => {
                Err(forbidden().detail(e.to_string()).build())
            }
            _ => Err(internal_server_error()
                .detail(format!("Failed to get environment variable: {}", e))
                .build()),
        },
    }
}

/// Get all environment variables for a service-project pair
#[utoipa::path(
    get,
    path = "/external-services/{id}/projects/{project_id}/environment",
    tag = "External Services",
    responses(
        (status = 200, description = "List of environment variables", body = Vec<EnvironmentVariableInfo>),
        (status = 404, description = "Service or project not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("id" = i32, Path, description = "External service ID"),
        ("project_id" = i32, Path, description = "Project ID")
    )
)]
async fn get_service_environment_variables(
    State(app_state): State<Arc<AppState>>,
    Path((id, project_id)): Path<(i32, i32)>,
    RequireAuth(auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesRead);

    let options = EnvironmentVariableOptions {
        include_docker: false,
        include_runtime: false,
        mask_sensitive: false,
        names_only: false,
    };

    match app_state
        .external_service_manager
        .get_environment_variables(id, Some(project_id), None, options)
        .await
    {
        Ok(response) => Ok((StatusCode::OK, Json(response.variables))),
        Err(e) => match e.to_string().as_str() {
            "Service not found" | "Project not found" => {
                Err(not_found().detail(e.to_string()).build())
            }
            _ => Err(internal_server_error()
                .detail(format!("Failed to get environment variables: {}", e))
                .build()),
        },
    }
}

/// Get all environment variables for all services linked to a project
#[utoipa::path(
    get,
    path = "/external-services/projects/{project_id}/environment",
    tag = "External Services",
    responses(
        (status = 200, description = "Map of service IDs to their environment variables", body = HashMap<i32, HashMap<String, String>>),
        (status = 404, description = "Project not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID")
    )
)]
async fn get_project_service_environment_variables(
    State(app_state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
    RequireAuth(auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesRead);

    match app_state
        .external_service_manager
        .get_project_service_environment_variables(project_id)
        .await
    {
        Ok(variables) => Ok((StatusCode::OK, Json(variables))),
        Err(e) => match e.to_string().as_str() {
            "Project not found" => Err(not_found().detail(e.to_string()).build()),
            _ => Err(internal_server_error()
                .detail(format!("Failed to get environment variables: {}", e))
                .build()),
        },
    }
}

/// Get external service details by slug
#[utoipa::path(
    get,
    path = "/external-services/by-slug/{slug}",
    tag = "External Services",
    responses(
        (status = 200, description = "External service details", body = ExternalServiceDetails),
        (status = 404, description = "Service not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("slug" = String, Path, description = "External service slug")
    )
)]
async fn get_service_by_slug(
    State(app_state): State<Arc<AppState>>,
    Path(slug): Path<String>,
    RequireAuth(auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesRead);
    let service = match app_state
        .external_service_manager
        .get_service_by_slug(&slug)
        .await
    {
        Ok(service) => service,
        Err(e) => {
            return Err(not_found()
                .detail(format!("Service not found: {}", e))
                .build());
        }
    };
    // .ok_or_else(|| (StatusCode::NOT_FOUND, Json("Service not found")).into_response());
    match app_state
        .external_service_manager
        .get_service_details_by_slug(service)
        .await
    {
        Ok(service) => Ok((StatusCode::OK, Json(service))),
        Err(e) => match e.to_string().as_str() {
            "Service not found" => Err(not_found().detail("Service not found").build()),
            _ => Err(internal_server_error()
                .detail(format!("Failed to get service: {}", e))
                .build()),
        },
    }
}

/// Get environment variable names preview (safe - no sensitive values)
#[utoipa::path(
    get,
    path = "/external-services/{id}/preview-environment-names",
    tag = "External Services",
    responses(
        (status = 200, description = "List of environment variable names that would be provided", body = Vec<String>),
        (status = 404, description = "Service not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("id" = i32, Path, description = "External service ID")
    )
)]
async fn get_service_preview_environment_variable_names(
    State(app_state): State<Arc<AppState>>,
    Path(id): Path<i32>,
    RequireAuth(auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesRead);

    let options = EnvironmentVariableOptions {
        include_docker: false,
        include_runtime: false,
        mask_sensitive: false,
        names_only: true,
    };

    match app_state
        .external_service_manager
        .get_environment_variables(id, None, None, options)
        .await
    {
        Ok(response) => {
            let variable_names: Vec<String> = response.variables.keys().cloned().collect();
            Ok((StatusCode::OK, Json(variable_names)))
        }
        Err(e) => match e.to_string().as_str() {
            "Service not found" => Err(not_found().detail("Service not found").build()),
            _ => Err(internal_server_error()
                .detail(format!(
                    "Failed to get preview environment variable names: {}",
                    e
                ))
                .build()),
        },
    }
}

/// Get environment variables preview with masked sensitive values
#[utoipa::path(
    get,
    path = "/external-services/{id}/preview-environment-masked",
    tag = "External Services",
    responses(
        (status = 200, description = "Preview of environment variables with sensitive values masked as ***", body = HashMap<String, String>),
        (status = 404, description = "Service not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("id" = i32, Path, description = "External service ID")
    )
)]
async fn get_service_preview_environment_variables_masked(
    State(app_state): State<Arc<AppState>>,
    Path(id): Path<i32>,
    RequireAuth(auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesRead);

    let options = EnvironmentVariableOptions {
        include_docker: false,
        include_runtime: false,
        mask_sensitive: true,
        names_only: false,
    };

    match app_state
        .external_service_manager
        .get_environment_variables(id, None, None, options)
        .await
    {
        Ok(response) => Ok((StatusCode::OK, Json(response.variables))),
        Err(e) => match e.to_string().as_str() {
            "Service not found" => Err(not_found().detail("Service not found").build()),
            _ => Err(internal_server_error()
                .detail(format!(
                    "Failed to get preview environment variables: {}",
                    e
                ))
                .build()),
        },
    }
}

#[derive(OpenApi)]
#[openapi(
    paths(
        get_service_types,
        get_providers_metadata,
        get_provider_metadata,
        get_service_type_parameters,
        list_services,
        get_service,
        create_service,
        list_available_containers,
        import_external_service,
        update_service,
        upgrade_service,
        delete_service,
        start_service,
        stop_service,
        link_service_to_project,
        unlink_service_from_project,
        list_service_projects,
        list_project_services,
        get_service_environment_variable,
        get_service_environment_variables,
        get_project_service_environment_variables,
        get_service_preview_environment_variable_names,
        get_service_preview_environment_variables_masked,
        get_service_by_slug,
        super::query_handlers::check_explorer_support,
        super::query_handlers::list_root_containers,
        super::query_handlers::list_containers_at_path,
        super::query_handlers::get_container_info,
        super::query_handlers::list_entities,
        super::query_handlers::get_entity_info,
        super::query_handlers::query_data,
        super::query_handlers::download_object,
    ),
    components(schemas(
        ServiceTypeInfo,
        ServiceTypeRoute,
        ServiceParameter,
        ProviderMetadata,
        ExternalServiceDetails,
        ExternalServiceInfo,
        CreateExternalServiceRequest,
        UpdateExternalServiceRequest,
        UpgradeExternalServiceRequest,
        ImportExternalServiceRequest,
        AvailableContainerInfo,
        LinkServiceRequest,
        ProjectServiceInfo,
        EnvironmentVariableInfo,
        super::query_handlers::ExplorerSupportResponse,
        super::query_handlers::ContainerResponse,
        super::query_handlers::EntityResponse,
        super::query_handlers::EntityInfoResponse,
        super::query_handlers::FieldResponse,
        super::query_handlers::QueryDataRequest,
        super::query_handlers::QueryDataResponse,
    )),
    info(
        title = "External Services API",
        description = "API endpoints for managing external service integrations. \
        Handles configuration, authentication, and interaction with third-party services. \
        Includes query capabilities for browsing and querying data from external services.",
        version = "1.0.0"
    ),
    tags(
        (name = "External Services", description = "External service integration endpoints"),
        (name = "External Services - Query", description = "Data querying and exploration endpoints")
    )
)]
pub struct ExternalServiceApiDoc;
