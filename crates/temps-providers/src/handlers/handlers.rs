use std::collections::HashMap;
use std::sync::Arc;

use super::types::AppState;
use axum::{
    extract::{Extension, Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, patch, post, put},
    Json, Router,
};
use temps_auth::RequireAuth;
use temps_auth::{deny_deployment_token, permission_guard, project_scope_guard};
use temps_core::{
    error_builder::{bad_request, forbidden, internal_server_error, not_found, ErrorBuilder},
    problemdetails::Problem,
};
use tracing::{error, info};
use utoipa::OpenApi;

use super::audit::{
    ExternalServiceClusterMemberAddedAudit, ExternalServiceClusterMemberPromotedAudit,
    ExternalServiceClusterMemberRemovedAudit, ExternalServiceCreatedAudit,
    ExternalServiceDeletedAudit, ExternalServiceStatusChangedAudit, ExternalServiceUpdatedAudit,
    ServiceHealthChecked,
};
use crate::handlers::types::{
    AddClusterMemberRequest, AvailableContainerInfo, ClusterHealthReportResponse,
    ClusterMemberHealthResponse, CreateExternalServiceRequest, EnvironmentVariableInfo,
    ExternalServiceDetails, ExternalServiceInfo, HealthCheckEntryResponse,
    ImportExternalServiceRequest, LinkServiceRequest, ProjectServiceInfo, ProviderMetadata,
    RetryClusterRequest, ServiceHealthResponse, ServiceHealthStatusBatchResponse,
    ServiceHealthStatusEntryResponse, ServiceMemberInfo, ServiceParameter, ServiceTypeInfo,
    ServiceTypeRoute, UpdateExternalServiceRequest, UpgradeExternalServiceRequest,
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
        .route(
            "/external-services/{id}/cluster-health",
            get(get_cluster_health),
        )
        .route(
            "/external-services/{id}/health-status",
            get(get_service_health_status),
        )
        .route(
            "/external-services/{id}/health-check",
            post(trigger_service_health_check),
        )
        .route(
            "/external-services/{id}/wal-health",
            get(get_postgres_wal_health),
        )
        .route(
            "/external-services/health-status-batch",
            get(list_service_health_statuses),
        )
        .route("/external-services/{id}/start", post(start_service))
        .route("/external-services/{id}/stop", post(stop_service))
        .route("/external-services/{id}/retry", post(retry_cluster))
        .route("/external-services/{id}/members", post(add_cluster_member))
        .route(
            "/external-services/{id}/members/{member_id}",
            get(get_cluster_member).delete(remove_cluster_member),
        )
        .route(
            "/external-services/{id}/members/{member_id}/promote",
            post(promote_cluster_member),
        )
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
        .route("/external-services/{id}/runtime", get(get_service_runtime))
        .route("/external-services/{id}/stats", get(get_service_stats))
        .route(
            "/external-services/{id}/resources",
            patch(update_service_resources),
        )
        .merge(super::query_handlers::configure_query_routes())
        .merge(super::metrics_handlers::configure_metrics_routes())
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
    params(
        temps_core::PaginationParams,
    ),
    responses(
        (status = 200, description = "List of external services", body = Vec<ExternalServiceInfo>),
        (status = 500, description = "Internal server error")
    )
)]
async fn list_services(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Query(pagination): Query<temps_core::PaginationParams>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesRead);
    // This returns every service in the fleet (unscoped). A project-bound
    // deployment token must not enumerate other tenants' services — it can use
    // GET /external-services/projects/{its_own_project_id} instead.
    deny_deployment_token!(auth);

    let (page, page_size) = pagination.normalize();

    match app_state
        .external_service_manager
        .list_services_paginated(page, page_size)
        .await
    {
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
    super::metrics_handlers::assert_service_owned_by_caller(id, &auth, &app_state).await?;

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
        node_id: request.node_id,
        topology: request.topology,
        members: request
            .members
            .into_iter()
            .map(|m| crate::services::ClusterMemberRequest {
                role: m.role,
                node_id: m.node_id,
            })
            .collect(),
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

            // Monitoring is on by default for new services (metrics_enabled is set
            // to true at creation). Wire up the supporting state here, in the handler
            // layer where api_key_service / config_service live. All side effects are
            // non-fatal — a monitoring-setup failure must not fail service creation.
            //
            // - Seed default alert rules for the engine (idempotent, all engines).
            // - For OTLP-push engines (rustfs/s3) provision the ingest key + URL and
            //   apply it. The container was just created, so this restart lands on a
            //   service that is effectively still first-booting.
            let engine = service.service_type.to_string();
            if let Err(e) =
                temps_monitoring::seed_default_rules(app_state.db.as_ref(), service.id, &engine)
                    .await
            {
                error!(
                    service_id = service.id,
                    engine = %engine,
                    error = %e,
                    "Failed to seed default alert rules at creation; continuing"
                );
            }

            if matches!(engine.as_str(), "rustfs" | "s3") {
                if let Ok(model) = app_state
                    .external_service_manager
                    .get_service(service.id)
                    .await
                {
                    crate::handlers::metrics_handlers::provision_otlp_ingest_key(
                        &app_state,
                        &model,
                        auth.user_id(),
                    )
                    .await;
                }
            }

            app_state.telemetry.report(
                temps_core::telemetry::TelemetryEvent::new(
                    temps_core::telemetry::TelemetryEventKind::ServiceCreated,
                )
                .with("engine", service.service_type.to_string()),
            );
            if service.topology == "cluster" {
                app_state.telemetry.report(
                    temps_core::telemetry::TelemetryEvent::new(
                        temps_core::telemetry::TelemetryEventKind::ServiceClusterCreated,
                    )
                    .with("engine", service.service_type.to_string()),
                );
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
    super::metrics_handlers::assert_service_owned_by_caller(id, &auth, &app_state).await?;

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
    super::metrics_handlers::assert_service_owned_by_caller(id, &auth, &app_state).await?;

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
    super::metrics_handlers::assert_service_owned_by_caller(id, &auth, &app_state).await?;
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
    super::metrics_handlers::assert_service_owned_by_caller(id, &auth, &app_state).await?;

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

/// Per-member health for a Postgres HA cluster.
///
/// Reads pg_auto_failover's `pgautofailover.node` table from the cluster's
/// monitor (TLS, autoctl_node) and joins each member with its
/// `pg_stat_replication` row from the current primary. Returns one row per
/// data member with role/state, sync state, and replay lag.
///
/// Returns `200` with `monitor_error` set when the monitor is briefly
/// unreachable (UI surfaces it as a banner above the table); the table
/// itself is empty in that case. Returns `400` for non-cluster services.
#[utoipa::path(
    get,
    path = "/external-services/{id}/cluster-health",
    tag = "External Services",
    responses(
        (status = 200, description = "Per-member cluster health report", body = ClusterHealthReportResponse),
        (status = 400, description = "Service is not a cluster"),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Service not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("id" = i32, Path, description = "External service ID")
    ),
    security(("bearer_auth" = []))
)]
async fn get_cluster_health(
    State(app_state): State<Arc<AppState>>,
    Path(id): Path<i32>,
    RequireAuth(auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesRead);
    super::metrics_handlers::assert_service_owned_by_caller(id, &auth, &app_state).await?;

    // Fetch the underlying row to (a) confirm existence, (b) check topology.
    let service = match app_state.external_service_manager.get_service(id).await {
        Ok(s) => s,
        Err(e) => match e.to_string().as_str() {
            "Service not found" => {
                return Err(not_found()
                    .detail(format!("External service {} not found", id))
                    .build())
            }
            _ => {
                return Err(internal_server_error()
                    .detail(format!("Failed to load service {}: {}", id, e))
                    .build())
            }
        },
    };

    if service.topology != "cluster" {
        return Err(bad_request()
            .detail(format!(
                "Service {} is not a cluster (topology = {:?}); cluster-health is only \
                 defined for HA clusters",
                id, service.topology
            ))
            .build());
    }

    let report = app_state
        .external_service_manager
        .cluster_health(&service)
        .await;
    let body: ClusterHealthReportResponse = report.into();
    Ok((StatusCode::OK, Json(body)))
}

/// Persisted health status for an external service
///
/// Returns the latest health probe result recorded by
/// `ExternalServiceHealthMonitor`, plus recent check history for sparklines
/// and a 24-hour uptime percentage. Safe to poll from the UI every 30s.
#[utoipa::path(
    get,
    path = "/external-services/{id}/health-status",
    tag = "External Services",
    responses(
        (status = 200, description = "Current health + recent history", body = ServiceHealthResponse),
        (status = 404, description = "Service not found"),
        (status = 500, description = "Internal server error"),
    ),
    params(
        ("id" = i32, Path, description = "External service ID"),
        ("limit" = Option<u64>, Query, description = "Max number of recent checks (default 50, max 200)"),
    )
)]
async fn get_service_health_status(
    State(app_state): State<Arc<AppState>>,
    Path(id): Path<i32>,
    Query(params): Query<HashMap<String, String>>,
    RequireAuth(auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesRead);
    super::metrics_handlers::assert_service_owned_by_caller(id, &auth, &app_state).await?;

    let limit = params
        .get("limit")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(50);

    match app_state
        .external_service_manager
        .get_health_snapshot(id, limit)
        .await
    {
        Ok(snap) => Ok((StatusCode::OK, Json(ServiceHealthResponse::from(snap)))),
        Err(crate::services::ExternalServiceError::ServiceNotFound { .. }) => {
            Err(not_found().detail("Service not found").build())
        }
        Err(e) => Err(internal_server_error()
            .detail(format!("Failed to load service health: {}", e))
            .build()),
    }
}

/// Run a health check for one service right now
///
/// Triggers the same engine-specific probe as the background monitor, writes
/// a history row, updates the denormalized fields on `external_services`, and
/// fires alerts on the Nth consecutive failure (so consecutive-failure state
/// stays honest). Returns the fresh snapshot the UI can display immediately.
#[utoipa::path(
    post,
    path = "/external-services/{id}/health-check",
    tag = "External Services",
    responses(
        (status = 200, description = "Fresh health snapshot after probing", body = ServiceHealthResponse),
        (status = 404, description = "Service not found"),
        (status = 503, description = "Health monitor not running on this node"),
        (status = 500, description = "Internal server error"),
    ),
    params(
        ("id" = i32, Path, description = "External service ID"),
    )
)]
async fn trigger_service_health_check(
    State(app_state): State<Arc<AppState>>,
    Path(id): Path<i32>,
    RequireAuth(auth): RequireAuth,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesWrite);
    super::metrics_handlers::assert_service_owned_by_caller(id, &auth, &app_state).await?;

    let Some(monitor) = app_state.health_monitor.as_ref() else {
        return Err(ErrorBuilder::new(StatusCode::SERVICE_UNAVAILABLE)
            .title("Health Monitor Unavailable")
            .detail(
                "Health monitor is not running on this node. Manual checks are only \
                 available on the control plane.",
            )
            .build());
    };

    match monitor.run_check_for(id).await {
        Ok(()) => {}
        Err(crate::health_monitor::HealthMonitorError::ServiceNotFound { .. }) => {
            return Err(not_found().detail("Service not found").build());
        }
        Err(e) => {
            return Err(internal_server_error()
                .detail(format!("Manual health check failed: {}", e))
                .build());
        }
    }

    // Audit — manual probes are a write action. Log failure, don't propagate.
    let audit = ServiceHealthChecked {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        service_id: id,
    };
    if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log for manual health check: {}", e);
    }

    // Return the fresh snapshot (same shape as GET /health-status).
    match app_state
        .external_service_manager
        .get_health_snapshot(id, 50)
        .await
    {
        Ok(snap) => Ok((StatusCode::OK, Json(ServiceHealthResponse::from(snap)))),
        Err(crate::services::ExternalServiceError::ServiceNotFound { .. }) => {
            Err(not_found().detail("Service not found").build())
        }
        Err(e) => Err(internal_server_error()
            .detail(format!("Failed to load service health: {}", e))
            .build()),
    }
}

/// Postgres WAL & archive health snapshot
///
/// Returns the latest WAL/archive health snapshot recorded by the background
/// health monitor for a Postgres external service. Powers the warning banner
/// on the service detail page when the disk is filling up due to stale
/// replication slots, archive backlog, or misconfigured `archive_command`.
///
/// Returns 404 when no snapshot exists yet (probe hasn't run, or the service
/// isn't Postgres).
#[utoipa::path(
    get,
    path = "/external-services/{id}/wal-health",
    operation_id = "getPostgresWalHealth",
    tag = "External Services",
    responses(
        (status = 200, description = "Latest WAL health snapshot", body = crate::externalsvc::postgres_wal_health::PostgresWalHealth),
        (status = 404, description = "Service not found, or no WAL snapshot available"),
        (status = 500, description = "Internal server error"),
    ),
    params(
        ("id" = i32, Path, description = "External service ID"),
    )
)]
async fn get_postgres_wal_health(
    State(app_state): State<Arc<AppState>>,
    Path(id): Path<i32>,
    RequireAuth(auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesRead);
    super::metrics_handlers::assert_service_owned_by_caller(id, &auth, &app_state).await?;

    match app_state
        .external_service_manager
        .get_postgres_wal_health(id)
        .await
    {
        Ok(Some(snapshot)) => Ok((StatusCode::OK, Json(snapshot))),
        Ok(None) => Err(not_found()
            .detail(format!(
                "No WAL health snapshot available for service {}",
                id
            ))
            .build()),
        Err(crate::services::ExternalServiceError::ServiceNotFound { .. }) => {
            Err(not_found().detail("Service not found").build())
        }
        Err(e) => Err(internal_server_error()
            .detail(format!("Failed to load WAL health: {}", e))
            .build()),
    }
}

/// Current health status for many services at once
///
/// Powers the status dot on the Storage list page. Pass a comma-separated
/// list of service IDs via `?ids=1,2,3`. Omit to get every service.
#[utoipa::path(
    get,
    path = "/external-services/health-status-batch",
    tag = "External Services",
    responses(
        (status = 200, description = "Batch of current health statuses", body = ServiceHealthStatusBatchResponse),
        (status = 500, description = "Internal server error"),
    ),
    params(
        ("ids" = Option<String>, Query, description = "Comma-separated service IDs. Omit for all services."),
    )
)]
async fn list_service_health_statuses(
    State(app_state): State<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
    RequireAuth(auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesRead);
    // When `ids` is omitted this reports health for the whole fleet. A
    // project-bound deployment token must not enumerate foreign services.
    deny_deployment_token!(auth);

    let ids: Vec<i32> = params
        .get("ids")
        .map(|raw| {
            raw.split(',')
                .filter_map(|s| s.trim().parse::<i32>().ok())
                .collect()
        })
        .unwrap_or_default();

    // When `ids` is omitted, fall back to every service the user can see.
    let ids = if ids.is_empty() {
        match app_state.external_service_manager.list_services().await {
            Ok(svcs) => svcs.into_iter().map(|s| s.id).collect::<Vec<_>>(),
            Err(e) => {
                return Err(internal_server_error()
                    .detail(format!("Failed to list services: {}", e))
                    .build())
            }
        }
    } else {
        ids
    };

    match app_state
        .external_service_manager
        .list_health_statuses(&ids)
        .await
    {
        Ok(entries) => Ok((
            StatusCode::OK,
            Json(ServiceHealthStatusBatchResponse {
                statuses: entries
                    .into_iter()
                    .map(ServiceHealthStatusEntryResponse::from)
                    .collect(),
            }),
        )),
        Err(e) => Err(internal_server_error()
            .detail(format!("Failed to load health statuses: {}", e))
            .build()),
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
    super::metrics_handlers::assert_service_owned_by_caller(id, &auth, &app_state).await?;
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

/// Retry a failed cluster service initialization.
///
/// Cleans up any leftover containers from the previous attempt and
/// re-runs cluster initialization with the provided member specifications.
#[utoipa::path(
    post,
    path = "/external-services/{id}/retry",
    tag = "External Services",
    request_body = RetryClusterRequest,
    responses(
        (status = 200, description = "Cluster retry initiated", body = ExternalServiceInfo),
        (status = 400, description = "Service is not a failed cluster"),
        (status = 404, description = "Service not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
async fn retry_cluster(
    State(app_state): State<Arc<AppState>>,
    Path(id): Path<i32>,
    RequireAuth(auth): RequireAuth,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<RetryClusterRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesWrite);
    super::metrics_handlers::assert_service_owned_by_caller(id, &auth, &app_state).await?;

    let members: Vec<crate::services::ClusterMemberRequest> = request
        .members
        .into_iter()
        .map(|m| crate::services::ClusterMemberRequest {
            role: m.role,
            node_id: m.node_id,
        })
        .collect();

    match app_state
        .external_service_manager
        .retry_cluster(id, &members)
        .await
    {
        Ok(service) => {
            let audit = ExternalServiceStatusChangedAudit {
                context: AuditContext {
                    user_id: auth.user_id(),
                    ip_address: Some(metadata.ip_address.clone()),
                    user_agent: metadata.user_agent.clone(),
                },
                service_id: service.id,
                name: service.name.clone(),
                service_type: service.service_type.to_string(),
                new_status: "retry".to_string(),
            };

            if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
                error!("Failed to create audit log: {}", e);
            }

            Ok((StatusCode::OK, Json(service)))
        }
        Err(e) => {
            error!("Failed to retry cluster service {}: {}", id, e);
            let msg = e.to_string();
            if msg.contains("not found") {
                Err(not_found()
                    .detail(format!("Service {} not found", id))
                    .build())
            } else if msg.contains("only valid for") || msg.contains("must be in") {
                Err(bad_request().detail(msg).build())
            } else {
                Err(internal_server_error()
                    .detail(format!("Failed to retry cluster: {}", e))
                    .build())
            }
        }
    }
}

/// Begin adding a single new member to a running cluster.
///
/// Currently only `replica` members can be added at runtime. The
/// response is **202 Accepted** as soon as the validation passes and
/// the placeholder `service_members` row is inserted. The actual
/// container provisioning + DNS registration runs in the background;
/// poll `GET /external-services/{id}/members/{member_id}` to watch
/// `provisioning_step` advance through the phases.
#[utoipa::path(
    post,
    path = "/external-services/{id}/members",
    tag = "External Services",
    request_body = AddClusterMemberRequest,
    responses(
        (status = 202, description = "Cluster member provisioning started", body = ServiceMemberInfo),
        (status = 400, description = "Validation failed (wrong topology, status, or role)"),
        (status = 404, description = "Service not found"),
        (status = 500, description = "Internal server error")
    ),
    params(("id" = i32, Path, description = "External service ID")),
    security(("bearer_auth" = []))
)]
async fn add_cluster_member(
    State(app_state): State<Arc<AppState>>,
    Path(id): Path<i32>,
    RequireAuth(auth): RequireAuth,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<AddClusterMemberRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesWrite);
    super::metrics_handlers::assert_service_owned_by_caller(id, &auth, &app_state).await?;

    match app_state
        .external_service_manager
        .add_cluster_member(id, &request.role, request.node_id)
        .await
    {
        Ok(member) => {
            let service = match app_state.external_service_manager.get_service(id).await {
                Ok(s) => Some(s),
                Err(e) => {
                    error!("Failed to load service {} for audit: {}", id, e);
                    None
                }
            };

            let audit = ExternalServiceClusterMemberAddedAudit {
                context: AuditContext {
                    user_id: auth.user_id(),
                    ip_address: Some(metadata.ip_address.clone()),
                    user_agent: metadata.user_agent.clone(),
                },
                service_id: id,
                service_name: service.as_ref().map(|s| s.name.clone()).unwrap_or_default(),
                member_id: member.id,
                role: member.role.clone(),
                ordinal: member.ordinal,
                node_id: member.node_id,
            };
            if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
                error!("Failed to create audit log: {}", e);
            }

            let wire: ServiceMemberInfo = member.into();
            Ok((StatusCode::ACCEPTED, Json(wire)))
        }
        Err(e) => {
            error!("Failed to add cluster member to service {}: {}", id, e);
            let msg = e.to_string();
            if msg.contains("not found") {
                Err(not_found()
                    .detail(format!("Service {} not found", id))
                    .build())
            } else if msg.contains("only valid for")
                || msg.contains("must be in")
                || msg.contains("only 'replica'")
                || msg.contains("only supported for Postgres")
                || msg.contains("Only 'replica'")
            {
                Err(bad_request().detail(msg).build())
            } else {
                Err(internal_server_error()
                    .detail(format!("Failed to add cluster member: {}", e))
                    .build())
            }
        }
    }
}

/// Get a single cluster member's current state.
///
/// Used by the add-member page to poll the row every second while the
/// background provisioning task walks through its phases. The
/// `provisioning_step` field advances through `inserting_row` →
/// `provisioning_container` → `registering_dns` → `done` (or `failed`
/// with `provisioning_error` set).
#[utoipa::path(
    get,
    path = "/external-services/{id}/members/{member_id}",
    tag = "External Services",
    responses(
        (status = 200, description = "Cluster member details", body = ServiceMemberInfo),
        (status = 404, description = "Service or member not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("id" = i32, Path, description = "External service ID"),
        ("member_id" = i32, Path, description = "Cluster member ID")
    ),
    security(("bearer_auth" = []))
)]
async fn get_cluster_member(
    State(app_state): State<Arc<AppState>>,
    Path((id, member_id)): Path<(i32, i32)>,
    RequireAuth(auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesRead);
    super::metrics_handlers::assert_service_owned_by_caller(id, &auth, &app_state).await?;

    match app_state
        .external_service_manager
        .get_cluster_member(id, member_id)
        .await
    {
        Ok(member) => {
            let wire: ServiceMemberInfo = member.into();
            Ok((StatusCode::OK, Json(wire)))
        }
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("not found") {
                Err(not_found().detail(msg).build())
            } else {
                Err(internal_server_error()
                    .detail(format!("Failed to load cluster member: {}", e))
                    .build())
            }
        }
    }
}

/// Remove a single member from a running cluster.
///
/// Refuses to remove the monitor (singleton), the current primary
/// (failover first), or any member if the cluster would drop below the
/// 2-data-member quorum required for HA. Stops + removes the container,
/// deletes the row, and drops the Tier-2 DNS record.
#[utoipa::path(
    delete,
    path = "/external-services/{id}/members/{member_id}",
    tag = "External Services",
    responses(
        (status = 204, description = "Cluster member removed"),
        (status = 400, description = "Validation failed (monitor, primary, or quorum violation)"),
        (status = 404, description = "Service or member not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("id" = i32, Path, description = "External service ID"),
        ("member_id" = i32, Path, description = "Cluster member ID")
    ),
    security(("bearer_auth" = []))
)]
async fn remove_cluster_member(
    State(app_state): State<Arc<AppState>>,
    Path((id, member_id)): Path<(i32, i32)>,
    RequireAuth(auth): RequireAuth,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesWrite);
    super::metrics_handlers::assert_service_owned_by_caller(id, &auth, &app_state).await?;

    match app_state
        .external_service_manager
        .remove_cluster_member(id, member_id)
        .await
    {
        Ok(()) => {
            let service_name = match app_state.external_service_manager.get_service(id).await {
                Ok(s) => s.name,
                Err(_) => String::new(),
            };

            let audit = ExternalServiceClusterMemberRemovedAudit {
                context: AuditContext {
                    user_id: auth.user_id(),
                    ip_address: Some(metadata.ip_address.clone()),
                    user_agent: metadata.user_agent.clone(),
                },
                service_id: id,
                service_name,
                member_id,
            };
            if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
                error!("Failed to create audit log: {}", e);
            }

            Ok(StatusCode::NO_CONTENT)
        }
        Err(e) => {
            error!(
                "Failed to remove cluster member {} from service {}: {}",
                member_id, id, e
            );
            let msg = e.to_string();
            if msg.contains("not found") {
                Err(not_found().detail(msg).build())
            } else if msg.contains("only valid for")
                || msg.contains("does not belong")
                || msg.contains("Cannot remove")
                || msg.contains("Refusing to remove")
            {
                Err(bad_request().detail(msg).build())
            } else {
                Err(internal_server_error()
                    .detail(format!("Failed to remove cluster member: {}", e))
                    .build())
            }
        }
    }
}

/// Promote a replica to primary by triggering a pg_auto_failover
/// failover. The monitor demotes the current primary and the chosen
/// replica transitions to primary; the role reconciler then refreshes
/// the role-aliased VIPs (≤30s).
#[utoipa::path(
    post,
    path = "/external-services/{id}/members/{member_id}/promote",
    tag = "External Services",
    responses(
        (status = 202, description = "Promotion initiated"),
        (status = 400, description = "Validation failed (monitor, already primary, not running, etc.)"),
        (status = 404, description = "Service or member not found"),
        (status = 500, description = "pg_autoctl perform promotion failed")
    ),
    params(
        ("id" = i32, Path, description = "External service ID"),
        ("member_id" = i32, Path, description = "Cluster member ID")
    ),
    security(("bearer_auth" = []))
)]
async fn promote_cluster_member(
    State(app_state): State<Arc<AppState>>,
    Path((id, member_id)): Path<(i32, i32)>,
    RequireAuth(auth): RequireAuth,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesWrite);
    super::metrics_handlers::assert_service_owned_by_caller(id, &auth, &app_state).await?;

    match app_state
        .external_service_manager
        .promote_cluster_member(id, member_id)
        .await
    {
        Ok(()) => {
            // Pull the member name for the audit log; failure here is
            // non-fatal (we already promoted, the audit row just won't
            // include the container name).
            let (service_name, container_name) = match (
                app_state.external_service_manager.get_service(id).await,
                app_state
                    .external_service_manager
                    .get_cluster_member(id, member_id)
                    .await,
            ) {
                (Ok(svc), Ok(m)) => (svc.name, m.container_name),
                _ => (String::new(), String::new()),
            };

            let audit = ExternalServiceClusterMemberPromotedAudit {
                context: AuditContext {
                    user_id: auth.user_id(),
                    ip_address: Some(metadata.ip_address.clone()),
                    user_agent: metadata.user_agent.clone(),
                },
                service_id: id,
                service_name,
                member_id,
                container_name,
            };
            if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
                error!("Failed to create audit log: {}", e);
            }

            Ok(StatusCode::ACCEPTED)
        }
        Err(e) => {
            error!(
                "Failed to promote cluster member {} on service {}: {}",
                member_id, id, e
            );
            let msg = e.to_string();
            if msg.contains("not found") {
                Err(not_found().detail(msg).build())
            } else if msg.contains("only valid for")
                || msg.contains("only supported for")
                || msg.contains("does not belong")
                || msg.contains("Cannot promote")
                || msg.contains("already the primary")
                || msg.contains("not running")
            {
                Err(bad_request().detail(msg).build())
            } else {
                Err(internal_server_error()
                    .detail(format!("Failed to promote cluster member: {}", e))
                    .build())
            }
        }
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
    super::metrics_handlers::assert_service_owned_by_caller(id, &auth, &app_state).await?;
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
        ("id" = i32, Path, description = "External service ID"),
        temps_core::PaginationParams,
    )
)]
async fn list_service_projects(
    State(app_state): State<Arc<AppState>>,
    Path(id): Path<i32>,
    RequireAuth(auth): RequireAuth,
    Query(pagination): Query<temps_core::PaginationParams>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesRead);

    let (page, page_size) = pagination.normalize();

    match app_state
        .external_service_manager
        .list_service_projects_paginated(id, page, page_size)
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
        ("project_id" = i32, Path, description = "Project ID"),
        temps_core::PaginationParams,
    )
)]
async fn list_project_services(
    State(app_state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
    RequireAuth(auth): RequireAuth,
    Query(pagination): Query<temps_core::PaginationParams>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesRead);
    project_scope_guard!(auth, project_id);

    let (page, page_size) = pagination.normalize();

    match app_state
        .external_service_manager
        .list_project_services_paginated(project_id, page, page_size)
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
    project_scope_guard!(auth, project_id);
    super::metrics_handlers::assert_service_owned_by_caller(id, &auth, &app_state).await?;

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
    project_scope_guard!(auth, project_id);
    super::metrics_handlers::assert_service_owned_by_caller(id, &auth, &app_state).await?;

    let options = EnvironmentVariableOptions {
        include_docker: false,
        include_runtime: false,
        // Only an admin may read plaintext connection strings / passwords.
        // Non-admin owners get masked values to prevent credential exfiltration.
        mask_sensitive: !auth.is_admin(),
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
    project_scope_guard!(auth, project_id);

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
    // No project_id/service-id to scope against here; a deployment token has no
    // business resolving services by global slug. Require user/API-key auth.
    deny_deployment_token!(auth);
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
    super::metrics_handlers::assert_service_owned_by_caller(id, &auth, &app_state).await?;

    let options = EnvironmentVariableOptions {
        include_docker: false,
        include_runtime: false,
        // names_only, so values aren't returned regardless.
        mask_sensitive: true,
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

// ---------------------------------------------------------------------------
// Container runtime + stats + resource limits.
//
// These three endpoints exist so operators can:
//   - see why a database is restarting (RestartCount, OOMKilled),
//   - watch live CPU/memory pressure,
//   - opt in to hard cgroup limits (with the OOM warning surfaced in the UI).
// They are intentionally read-mostly: PATCH .../resources only updates the
// stored config; new caps take effect on the next container recreate.
// ---------------------------------------------------------------------------

/// Inspect a service's container(s): status, restart count, OOM-killed flag,
/// exit code, and the cgroup limits actually applied.
#[utoipa::path(
    get,
    path = "/external-services/{id}/runtime",
    tag = "External Services",
    responses(
        (status = 200, description = "Container runtime snapshot", body = crate::services::ServiceRuntimeReport),
        (status = 404, description = "Service not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("id" = i32, Path, description = "External service ID")
    )
)]
async fn get_service_runtime(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesRead);
    super::metrics_handlers::assert_service_owned_by_caller(id, &auth, &app_state).await?;

    match app_state
        .external_service_manager
        .get_service_runtime(id)
        .await
    {
        Ok(report) => Ok((StatusCode::OK, Json(report))),
        Err(crate::services::ExternalServiceError::ServiceNotFound { .. }) => {
            Err(not_found().detail("Service not found").build())
        }
        Err(e) => Err(internal_server_error()
            .detail(format!("Failed to load service runtime: {}", e))
            .build()),
    }
}

/// Sample current CPU/memory usage from each of a service's containers.
/// One-shot sample, no streaming. Cheap to call (single Docker round-trip
/// per member) so the UI can poll on a 5–10s interval.
#[utoipa::path(
    get,
    path = "/external-services/{id}/stats",
    tag = "External Services",
    responses(
        (status = 200, description = "Container stats snapshot", body = crate::services::ServiceStatsReport),
        (status = 404, description = "Service not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("id" = i32, Path, description = "External service ID")
    )
)]
async fn get_service_stats(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesRead);
    super::metrics_handlers::assert_service_owned_by_caller(id, &auth, &app_state).await?;

    match app_state
        .external_service_manager
        .get_service_stats(id)
        .await
    {
        Ok(report) => Ok((StatusCode::OK, Json(report))),
        Err(crate::services::ExternalServiceError::ServiceNotFound { .. }) => {
            Err(not_found().detail("Service not found").build())
        }
        Err(e) => Err(internal_server_error()
            .detail(format!("Failed to load service stats: {}", e))
            .build()),
    }
}

/// Update a service's resource limits (memory, CPU caps).
///
/// Persists the new caps to the encrypted config AND live-applies them
/// via Docker's update API. Memory and CPU can be hot-changed without a
/// restart on running containers; stopped containers also accept the
/// update and pick up the new caps on next start.
///
/// Pass `null` (or omit) any field to leave it unlimited. A request where
/// every field is `null` removes any existing limits.
///
/// The response includes a per-container `applied[]` list so the caller
/// can tell which members got the update and which were skipped (e.g.,
/// container not yet created, or `docker update` rejected because the
/// new memory cap is below current usage).
#[utoipa::path(
    patch,
    path = "/external-services/{id}/resources",
    tag = "External Services",
    request_body = crate::externalsvc::ServiceResourceLimits,
    responses(
        (status = 200, description = "Updated resource limits", body = crate::services::ResourceLimitsUpdateResponse),
        (status = 400, description = "Invalid resource limits"),
        (status = 404, description = "Service not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("id" = i32, Path, description = "External service ID")
    )
)]
async fn update_service_resources(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path(id): Path<i32>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<crate::externalsvc::ServiceResourceLimits>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesWrite);
    super::metrics_handlers::assert_service_owned_by_caller(id, &auth, &app_state).await?;

    match app_state
        .external_service_manager
        .update_service_resource_limits(id, request)
        .await
    {
        Ok(response) => {
            let applied_limits = &response.limits;
            // Audit: capture the new caps as flat strings so the existing
            // ExternalServiceUpdatedAudit shape works.
            let mut params = HashMap::new();
            params.insert(
                "memory_mb".to_string(),
                applied_limits
                    .memory_mb
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "unlimited".to_string()),
            );
            params.insert(
                "memory_swap_mb".to_string(),
                applied_limits
                    .memory_swap_mb
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "unlimited".to_string()),
            );
            params.insert(
                "nano_cpus".to_string(),
                applied_limits
                    .nano_cpus
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "unlimited".to_string()),
            );
            params.insert(
                "cpu_shares".to_string(),
                applied_limits
                    .cpu_shares
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "unlimited".to_string()),
            );

            // Look up the service for context fields the audit log needs.
            // If this fails, log but don't fail the response — the limits
            // are already saved.
            if let Ok(service) = app_state.external_service_manager.get_service(id).await {
                let audit = ExternalServiceUpdatedAudit {
                    context: AuditContext {
                        user_id: auth.user_id(),
                        ip_address: Some(metadata.ip_address.clone()),
                        user_agent: metadata.user_agent.clone(),
                    },
                    service_id: service.id,
                    name: service.name.clone(),
                    service_type: service.service_type.clone(),
                    updated_parameters: params,
                };
                if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
                    error!("Failed to create audit log for resource update: {}", e);
                }
            }

            Ok((StatusCode::OK, Json(response)))
        }
        Err(crate::services::ExternalServiceError::ServiceNotFound { .. }) => {
            Err(not_found().detail("Service not found").build())
        }
        Err(crate::services::ExternalServiceError::ParameterValidationFailed {
            reason, ..
        }) => Err(bad_request().detail(reason).build()),
        Err(e) => Err(internal_server_error()
            .detail(format!("Failed to update resource limits: {}", e))
            .build()),
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
        retry_cluster,
        add_cluster_member,
        get_cluster_member,
        remove_cluster_member,
        promote_cluster_member,
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
        get_service_health_status,
        trigger_service_health_check,
        get_postgres_wal_health,
        list_service_health_statuses,
        get_cluster_health,
        get_service_runtime,
        get_service_stats,
        update_service_resources,
        super::query_handlers::check_explorer_support,
        super::query_handlers::list_root_containers,
        super::query_handlers::list_containers_at_path,
        super::query_handlers::get_container_info,
        super::query_handlers::list_entities,
        super::query_handlers::get_entity_info,
        super::query_handlers::query_data,
        super::query_handlers::download_object,
        super::metrics_handlers::get_service_metrics_range,
        super::metrics_handlers::get_service_metrics_latest,
        super::metrics_handlers::get_service_metrics_status,
        super::metrics_handlers::get_service_metrics_by_database,
        super::metrics_handlers::list_service_alert_rules,
        super::metrics_handlers::create_service_alert_rule,
        super::metrics_handlers::update_service_alert_rule,
        super::metrics_handlers::delete_service_alert_rule,
        super::metrics_handlers::toggle_service_metrics,
        super::metrics_handlers::get_deployment_metrics_range,
        super::metrics_handlers::get_deployment_metrics_latest,
        super::metrics_handlers::toggle_deployment_metrics,
        super::metrics_handlers::get_node_metrics_range,
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
        RetryClusterRequest,
        AddClusterMemberRequest,
        ServiceMemberInfo,
        ImportExternalServiceRequest,
        AvailableContainerInfo,
        LinkServiceRequest,
        ProjectServiceInfo,
        EnvironmentVariableInfo,
        ServiceHealthResponse,
        HealthCheckEntryResponse,
        ServiceHealthStatusBatchResponse,
        ServiceHealthStatusEntryResponse,
        ClusterHealthReportResponse,
        ClusterMemberHealthResponse,
        crate::externalsvc::ServiceResourceLimits,
        crate::externalsvc::postgres_wal_health::PostgresWalHealth,
        crate::externalsvc::postgres_wal_health::ArchiveMode,
        crate::externalsvc::postgres_wal_health::StaleSlot,
        crate::externalsvc::postgres_wal_health::WalWarning,
        crate::externalsvc::postgres_wal_health::WalWarningSeverity,
        crate::services::ContainerRuntimeInfo,
        crate::services::ServiceRuntimeReport,
        crate::services::ContainerStatsSample,
        crate::services::ServiceStatsReport,
        crate::services::ResourceLimitApplyResult,
        crate::services::ResourceLimitsUpdateResponse,
        super::query_handlers::ExplorerSupportResponse,
        super::query_handlers::ContainerResponse,
        super::query_handlers::EntityResponse,
        super::query_handlers::PaginatedEntitiesResponse,
        super::query_handlers::ListEntitiesQuery,
        super::query_handlers::EntityInfoResponse,
        super::query_handlers::FieldResponse,
        super::query_handlers::QueryDataRequest,
        super::query_handlers::QueryDataResponse,
        super::metrics_handlers::MetricDataPoint,
        super::metrics_handlers::MetricsRangeQuery,
        super::metrics_handlers::MetricsStatusResponse,
        super::metrics_handlers::DatabaseMetricsRow,
        super::metrics_handlers::DatabaseMetricsResponse,
        super::metrics_handlers::AlertRuleResponse,
        super::metrics_handlers::CreateAlertRuleRequest,
        super::metrics_handlers::UpdateAlertRuleRequest,
        super::metrics_handlers::ToggleServiceMetricsRequest,
        super::metrics_handlers::ToggleDeploymentMetricsRequest,
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
        (name = "External Services - Query", description = "Data querying and exploration endpoints"),
        (name = "Metrics", description = "Time-series metrics and alert rule endpoints")
    )
)]
pub struct ExternalServiceApiDoc;
