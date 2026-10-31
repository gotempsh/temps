// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Create, read, list, update and delete of managed services.

use crate::externalsvc::{
    legacy_managed_instance_names, ExternalService, ManagedS3BackendKind,
    ManagedS3BackendSelection, ServiceConfig, ServiceType,
};
use crate::parameter_strategies;
use crate::services::*;
use chrono::Utc;
use sea_orm::{
    sea_query::Expr, ActiveModelTrait, ColumnTrait, Condition, EntityTrait, PaginatorTrait,
    QueryFilter, QueryOrder, QuerySelect, Set, TransactionTrait,
};
use temps_entities::{
    backup_schedule_services, backup_schedules, external_service_backups, external_services,
    postgres_major_upgrades, project_services, service_members,
};
use tracing::{debug, error, info, warn};

fn generated_schedule_loses_last_target(
    generated_kind: Option<&str>,
    remaining_targets: u64,
) -> bool {
    generated_kind.is_some() && remaining_targets == 0
}

impl ExternalServiceManager {
    pub async fn get_service_by_name(
        &self,
        name_param: &str,
    ) -> Result<external_services::Model, ExternalServiceError> {
        let service = external_services::Entity::find()
            .filter(external_services::Column::Name.eq(name_param))
            .one(self.db.as_ref())
            .await?;

        service.ok_or(ExternalServiceError::ServiceNotFoundByName {
            name: name_param.to_string(),
        })
    }

    pub async fn get_service_by_slug(
        &self,
        slug_param: &str,
    ) -> Result<external_services::Model, ExternalServiceError> {
        let service = external_services::Entity::find()
            .filter(external_services::Column::Slug.eq(slug_param))
            .one(self.db.as_ref())
            .await?;

        service.ok_or(ExternalServiceError::ServiceNotFoundBySlug {
            slug: slug_param.to_string(),
        })
    }

    pub async fn create_service(
        &self,
        request: CreateExternalServiceRequest,
    ) -> Result<ExternalServiceInfo, ExternalServiceError> {
        self.create_service_with_creator(request, None).await
    }

    pub async fn create_service_for_user(
        &self,
        request: CreateExternalServiceRequest,
        user_id: i32,
    ) -> Result<ExternalServiceInfo, ExternalServiceError> {
        self.create_service_with_creator(request, Some(user_id))
            .await
    }

    async fn create_service_with_creator(
        &self,
        request: CreateExternalServiceRequest,
        created_by_user_id: Option<i32>,
    ) -> Result<ExternalServiceInfo, ExternalServiceError> {
        info!("Creating new external service");

        let mut request = request;
        if request.topology == "cluster" {
            request.members = resolve_cluster_members(&request.name, &request.members)?;
        }

        #[allow(deprecated)]
        if request.service_type == ServiceType::Minio {
            return Err(ExternalServiceError::ParameterValidationFailed {
                service_id: 0,
                reason:
                    "MinIO service creation is deprecated; create an S3 or RustFS service instead"
                        .to_string(),
            });
        }

        // For standalone services that would run locally, guard the profile
        // contract BEFORE writing any database state. A cluster with only
        // remote members is fine — that path spawns containers on worker nodes
        // only; `initialize_cluster` catches any local member in a cluster
        // created with mixed placement.
        if request.node_id.is_none()
            && request.topology != "cluster"
            && !self.local_workloads_enabled
        {
            return Err(ExternalServiceError::LocalWorkloadsDisabled {
                name: request.name.clone(),
            });
        }

        // A cluster spanning the control plane and workers needs a
        // control-plane address the workers can reach. Cluster creation runs
        // in the background, so check it here, before any row exists, and
        // answer the request with the remedy instead of a half-built cluster.
        if request.topology == "cluster" {
            validate_cluster_member_count(&request.name, request.members.len())?;
        }
        if request.topology == "cluster"
            && Self::cluster_spans_control_plane_and_workers(&request.members)
        {
            self.control_plane_member_address(&request.name).await?;
        }

        let service_slug = Self::generate_slug(&request.name);

        let backend_selection =
            if matches!(request.service_type, ServiceType::S3 | ServiceType::Blob) {
                let parameter_value = serde_json::to_value(&request.parameters).map_err(|e| {
                    ExternalServiceError::InternalError {
                        reason: format!("Failed to inspect managed S3 backend parameters: {}", e),
                    }
                })?;
                let backend_selection =
                    ManagedS3BackendSelection::from_parameters(&parameter_value).map_err(|e| {
                        ExternalServiceError::ParameterValidationFailed {
                            service_id: 0,
                            reason: e.to_string(),
                        }
                    })?;
                backend_selection
                    .validate_for_service_create()
                    .map_err(|e| ExternalServiceError::ParameterValidationFailed {
                        service_id: 0,
                        reason: e.to_string(),
                    })?;
                Some(backend_selection)
            } else {
                None
            };

        // The strategy key follows the engine that will run, not the stored
        // service type: an S3 service on Garage is configured like Garage.
        #[allow(deprecated)]
        let strategy_key = match backend_selection
            .as_ref()
            .map(|selection| selection.backend)
        {
            Some(ManagedS3BackendKind::Minio) if request.service_type == ServiceType::S3 => {
                ServiceType::Minio.to_string()
            }
            Some(ManagedS3BackendKind::Minio) => {
                return Err(ExternalServiceError::ParameterValidationFailed {
                    service_id: 0,
                    reason: "managed S3 backend 'minio' is only supported for S3 services; use the default 'rustfs' backend for Blob services".to_string(),
                });
            }
            Some(ManagedS3BackendKind::Garage) if request.service_type == ServiceType::Blob => {
                // The built-in Blob service is wired to RustFS by its plugin,
                // which would reinitialize a Garage-backed service as RustFS
                // on every boot.
                return Err(ExternalServiceError::ParameterValidationFailed {
                    service_id: 0,
                    reason: "managed S3 backend 'garage' is only supported for S3 services; the built-in Blob service runs on the default 'rustfs' backend".to_string(),
                });
            }
            Some(ManagedS3BackendKind::Garage) => ManagedS3BackendKind::Garage.as_str().to_string(),
            Some(ManagedS3BackendKind::Rustfs) | None => request.service_type.to_string(),
        };

        // Get the parameter strategy for the selected backend defaults. The
        // stored service type remains the requested S3-compatible type.
        let strategy = parameter_strategies::get_strategy(&strategy_key).ok_or(
            ExternalServiceError::InvalidServiceType {
                id: 0,
                service_type: strategy_key.clone(),
            },
        )?;

        // Validate required parameters
        strategy
            .validate_for_creation(&request.parameters)
            .map_err(|reason| ExternalServiceError::ParameterValidationFailed {
                service_id: 0,
                reason,
            })?;

        // Auto-generate missing optional parameters
        let mut parameters = request.parameters.clone();
        strategy
            .auto_generate_missing(&mut parameters)
            .map_err(|reason| ExternalServiceError::InternalError { reason })?;

        // Store the validated, normalised backend selection rather than the
        // raw request, so every later read sees one canonical shape.
        if let Some(selection) = backend_selection.as_ref() {
            selection.write_into_parameters(&mut parameters);
        }

        // Serialize parameters to JSON and encrypt
        let config_json = serde_json::to_string(&parameters).map_err(|e| {
            ExternalServiceError::InternalError {
                reason: format!("Failed to serialize config to JSON: {}", e),
            }
        })?;

        let encrypted_config = self
            .encryption_service
            .encrypt_string(&config_json)
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!("Failed to encrypt config: {}", e),
            })?;

        let topology = request.topology.clone();
        let topology_for_txn = topology.clone();

        // Start transaction
        let service = self
            .db
            .transaction::<_, external_services::Model, ExternalServiceError>(|txn| {
                Box::pin(async move {
                    reserve_service_name(txn, &request.name, None).await?;
                    // Create service record with encrypted config
                    let new_service = external_services::ActiveModel {
                        name: Set(request.name.clone()),
                        slug: Set(Some(service_slug.clone())),
                        service_type: Set(request.service_type.to_string()),
                        version: Set(request.version.clone()),
                        status: Set("pending".to_string()),
                        config: Set(Some(encrypted_config)),
                        node_id: Set(request.node_id),
                        topology: Set(topology_for_txn),
                        // Monitoring is on by default for every new service. For DB
                        // engines (postgres/redis/mongodb) the metrics poller picks up
                        // any service with this flag set — zero extra footprint, no
                        // restart. For OTLP-push engines (rustfs/s3) the create handler
                        // provisions the ingest key right after creation. The entity
                        // default stays `false` so existing rows and out-of-band inserts
                        // are unaffected.
                        metrics_enabled: Set(true),
                        created_by_user_id: Set(created_by_user_id),
                        created_at: Set(Utc::now()),
                        updated_at: Set(Utc::now()),
                        ..Default::default()
                    };

                    let service = new_service.insert(txn).await?;

                    Ok(service)
                })
            })
            .await
            .map_err(ExternalServiceError::from)?;

        // Initialize the service
        if topology == "cluster" {
            // Cluster creation is async — update status to "creating" and spawn background task.
            // The frontend polls GET /external-services/{id} to track progress.
            let mut service_update: external_services::ActiveModel = service.clone().into();
            service_update.status = Set("creating".to_string());
            service_update.update(self.db.as_ref()).await?;

            // `self.clone()`, not `ExternalServiceManager::new(...)`: the clone
            // shares this instance's `reconciler_shutdowns` map, so a role
            // reconciler spawned inside `initialize_cluster` stays reachable by
            // `stop_role_reconciler` on the real, shared manager later. See the
            // struct's doc comment.
            let manager = self.clone();
            let db = self.db.clone();
            let service_id = service.id;
            let members = request.members.clone();

            tokio::spawn(async move {
                let result = manager.initialize_cluster(service_id, &members).await;

                match result {
                    Ok(()) => {
                        info!(
                            "Cluster service {} initialized successfully (background)",
                            service_id
                        );
                        // Status already set to "running" inside initialize_cluster
                    }
                    Err(e) => {
                        error!(
                            "Background cluster creation failed for service {}: {}",
                            service_id, e
                        );

                        // Update service status to "failed" with error message
                        let update_result: Result<_, sea_orm::DbErr> = async {
                            let mut svc: external_services::ActiveModel =
                                external_services::Entity::find_by_id(service_id)
                                    .one(db.as_ref())
                                    .await?
                                    .ok_or(sea_orm::DbErr::RecordNotFound(
                                        "Service not found during rollback".to_string(),
                                    ))?
                                    .into();
                            svc.status = Set("failed".to_string());
                            svc.error_message = Set(Some(e.to_string()));
                            svc.updated_at = Set(Utc::now());
                            svc.update(db.as_ref()).await?;
                            Ok(())
                        }
                        .await;

                        if let Err(db_err) = update_result {
                            error!(
                                "Failed to update service {} status to 'failed': {}",
                                service_id, db_err
                            );
                        }
                    }
                }
            });

            // Return immediately with "creating" status
            self.get_service_info(service.id).await
        } else {
            // Standalone: initialize synchronously
            let init_result = async {
                self.ensure_new_local_service_resources_unused(&service)
                    .await?;
                self.initialize_service(service.id).await
            }
            .await;

            if let Err(e) = init_result {
                error!(
                    "Service initialization failed for service {}: {}. Rolling back database record.",
                    service.id, e
                );

                if let Err(delete_err) = external_services::Entity::delete_by_id(service.id)
                    .exec(self.db.as_ref())
                    .await
                {
                    error!(
                        "Failed to clean up service {} after initialization failure: {}",
                        service.id, delete_err
                    );
                }

                if matches!(e, ExternalServiceError::ServiceResourceConflict { .. }) {
                    return Err(e);
                }
                return Err(ExternalServiceError::InitializationFailed {
                    id: service.id,
                    reason: e.to_string(),
                });
            }

            self.get_service_info(service.id).await
        }
    }

    pub async fn get_service_config(
        &self,
        service_id: i32,
    ) -> Result<ServiceConfig, ExternalServiceError> {
        let service = self.get_service(service_id).await?;
        let service_type = ServiceType::from_str(&service.service_type).map_err(|_| {
            ExternalServiceError::InvalidServiceType {
                id: service_id,
                service_type: service.service_type.clone(),
            }
        })?;

        let parameters = self.get_service_parameters(service_id).await?;
        let config = ServiceConfig {
            name: service.name.clone(),
            service_type,
            version: service.version,
            parameters: serde_json::to_value(parameters).map_err(|e| {
                ExternalServiceError::InternalError {
                    reason: format!("Failed to serialize parameters: {}", e),
                }
            })?,
        };

        Ok(config)
    }

    /// Store a metrics ingest key + internal URL into the service's encrypted
    /// config blob and restart the container so the OTLP env vars take effect.
    ///
    /// Both are persisted in the config (`metrics_ingest_key` and
    /// `metrics_ingest_url`) so any future container recreate (upgrade,
    /// restart) automatically picks them up without re-provisioning.
    pub async fn store_and_apply_ingest_key(
        &self,
        service_id: i32,
        ingest_key: String,
        ingest_url: String,
    ) -> Result<(), ExternalServiceError> {
        let service = self.get_service(service_id).await?;
        self.ensure_unambiguous_service_name(&service).await?;
        // Merge the key + URL into the existing encrypted params.
        let mut params = self.get_service_parameters(service_id).await?;
        params.insert(
            "metrics_ingest_key".to_string(),
            serde_json::Value::String(ingest_key),
        );
        params.insert(
            "metrics_ingest_url".to_string(),
            serde_json::Value::String(ingest_url),
        );

        let config_json =
            serde_json::to_string(&params).map_err(|e| ExternalServiceError::InternalError {
                reason: format!("Failed to serialize config: {}", e),
            })?;
        let encrypted = self
            .encryption_service
            .encrypt_string(&config_json)
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!("Failed to encrypt config: {}", e),
            })?;

        external_services::Entity::update(external_services::ActiveModel {
            id: sea_orm::Set(service_id),
            config: sea_orm::Set(Some(encrypted)),
            ..Default::default()
        })
        .exec(self.db.as_ref())
        .await
        .map_err(|e| ExternalServiceError::InternalError {
            reason: format!("Failed to save ingest key: {}", e),
        })?;

        // Now recreate the container — it will read metrics_ingest_key from config.
        let service = self.get_service(service_id).await?;
        let service_type = ServiceType::from_str(&service.service_type).map_err(|_| {
            ExternalServiceError::InvalidServiceType {
                id: service_id,
                service_type: service.service_type.clone(),
            }
        })?;
        let config = self.get_service_config(service_id).await?;
        let instance = self.create_service_instance_for_parameter_value(
            service.name.clone(),
            service_type,
            &config.parameters,
        )?;
        instance
            .apply_ingest_key(config)
            .await
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!("Failed to restart container with ingest key: {}", e),
            })
    }

    /// Force-recreate a service's container so a CMD-baked config change
    /// (currently: `shared_preload_libraries`) takes effect immediately,
    /// rather than waiting for the next unrelated restart to happen to also
    /// pick it up via drift-reconciliation.
    ///
    /// Unlike `store_and_apply_ingest_key`, this doesn't persist anything new
    /// into the service's config — the desired state is already derivable
    /// from the container's own image — it just drives the engine's
    /// `force_recreate` (see `ExternalService::force_recreate`) with a
    /// properly hydrated config so the recreate step has what it needs.
    pub async fn force_recreate_service_container(
        &self,
        service_id: i32,
    ) -> Result<(), ExternalServiceError> {
        let service = self.get_service(service_id).await?;
        self.ensure_unambiguous_service_name(&service).await?;
        let service_type = ServiceType::from_str(&service.service_type).map_err(|_| {
            ExternalServiceError::InvalidServiceType {
                id: service_id,
                service_type: service.service_type.clone(),
            }
        })?;
        let config = self.get_service_config(service_id).await?;
        let instance = self.create_service_instance_for_parameter_value(
            service.name.clone(),
            service_type,
            &config.parameters,
        )?;
        instance
            .force_recreate(config)
            .await
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!("Failed to recreate container: {}", e),
            })?;

        // A recreated container gets a new Docker IP, so the previously
        // published A record now points at nothing. Re-publish it.
        if let Err(e) = self.register_standalone_service_dns(service_id).await {
            warn!(
                service_id,
                error = %e,
                "Failed to refresh internal DNS record after recreating service container"
            );
        }

        Ok(())
    }

    pub async fn list_services(&self) -> Result<Vec<ExternalServiceInfo>, ExternalServiceError> {
        let services = external_services::Entity::find()
            .order_by_desc(external_services::Column::CreatedAt)
            .all(self.db.as_ref())
            .await?;

        let mut result = Vec::new();
        for service in services {
            result.push(self.get_service_info(service.id).await?);
        }

        Ok(result)
    }

    pub async fn list_services_paginated(
        &self,
        page: u64,
        page_size: u64,
    ) -> Result<Vec<ExternalServiceInfo>, ExternalServiceError> {
        let services = external_services::Entity::find()
            .order_by_desc(external_services::Column::CreatedAt)
            .paginate(self.db.as_ref(), page_size)
            .fetch_page(page - 1)
            .await?;

        let mut result = Vec::new();
        for service in services {
            result.push(self.get_service_info(service.id).await?);
        }

        Ok(result)
    }

    /// List services linked to at least one project visible to the caller, plus
    /// services the caller has just created but has not linked yet.
    ///
    /// `hidden_project_ids` comes from the registered `ProjectAccessChecker`.
    /// The left join and creator condition apply access filtering before
    /// pagination, so restricted callers cannot enumerate another user's
    /// unlinked service or receive sparse/misleading pages. `DISTINCT` prevents
    /// a service linked to multiple visible projects from appearing twice.
    pub async fn list_project_accessible_services_paginated(
        &self,
        page: u64,
        page_size: u64,
        hidden_project_ids: &[i32],
        creator_user_id: i32,
    ) -> Result<Vec<ExternalServiceInfo>, ExternalServiceError> {
        let linked_to_visible_project = if hidden_project_ids.is_empty() {
            Condition::all().add(project_services::Column::ServiceId.is_not_null())
        } else {
            Condition::all().add(
                project_services::Column::ProjectId.is_not_in(hidden_project_ids.iter().copied()),
            )
        };
        let creator_owned_and_unlinked = Condition::all()
            .add(external_services::Column::CreatedByUserId.eq(creator_user_id))
            .add(project_services::Column::ServiceId.is_null());
        let query = external_services::Entity::find()
            .left_join(project_services::Entity)
            .filter(
                Condition::any()
                    .add(linked_to_visible_project)
                    .add(creator_owned_and_unlinked),
            )
            .distinct()
            .order_by_desc(external_services::Column::CreatedAt);

        let services = query
            .paginate(self.db.as_ref(), page_size)
            .fetch_page(page - 1)
            .await
            .map_err(|error| ExternalServiceError::DatabaseError {
                reason: format!("failed to list project-accessible external services: {error}"),
            })?;

        let mut result = Vec::with_capacity(services.len());
        for service in services {
            result.push(self.get_service_info(service.id).await?);
        }

        Ok(result)
    }

    pub async fn get_service_details(
        &self,
        service_id: i32,
    ) -> Result<ExternalServiceDetails, ExternalServiceError> {
        let service_info = self.get_service_info(service_id).await?;
        let mut parameters = self.get_service_parameters(service_id).await?;
        // Hide structured sub-blocks from the flat Configuration card. The
        // `resources` block ({memory_mb, nano_cpus, ...}) is surfaced via
        // its own panel + endpoint; if it stays in this map the React
        // `<dl>` renders the object directly and crashes the page.
        parameters.remove("resources");
        let service_type =
            ServiceType::from_str(&service_info.service_type.to_string()).map_err(|_| {
                ExternalServiceError::InvalidServiceType {
                    id: service_id,
                    service_type: service_info.service_type.to_string(),
                }
            })?;

        // Schema only — resolved statically so a service's detail page still
        // renders on a process with no local Docker daemon.
        let parameter_schema = Self::parameter_schema_for(service_type, &parameters)?;
        let sensitive_parameters = Self::mask_sensitive_parameter_values(&mut parameters);

        Ok(ExternalServiceDetails {
            service: service_info,
            parameter_schema,
            current_parameters: Some(parameters),
            sensitive_parameters,
        })
    }

    /// Decrypt a single sensitive service parameter for an explicit reveal request.
    ///
    /// Normal detail responses are always masked. Keeping plaintext access in this
    /// narrowly-scoped service method makes it possible for the HTTP layer to apply
    /// authorization and write an audit event for every reveal.
    pub async fn get_sensitive_parameter_value(
        &self,
        service_id: i32,
        param_name: &str,
    ) -> Result<String, ExternalServiceError> {
        if !Self::is_sensitive_parameter(param_name) {
            return Err(ExternalServiceError::ParameterNotSensitive {
                service_id,
                param_name: param_name.to_string(),
            });
        }

        let parameters = self.get_service_parameters(service_id).await?;
        let value =
            parameters
                .get(param_name)
                .ok_or_else(|| ExternalServiceError::ParameterNotFound {
                    service_id,
                    param_name: param_name.to_string(),
                })?;

        Ok(match value {
            serde_json::Value::String(value) => value.clone(),
            other => other.to_string(),
        })
    }

    pub async fn upgrade_service(
        &self,
        service_id: i32,
        new_docker_image: String,
    ) -> Result<ExternalServiceInfo, ExternalServiceError> {
        info!(
            "Upgrading service {} to Docker image {}",
            service_id, new_docker_image
        );
        self.ensure_no_active_upgrade(service_id).await?;

        let service = self.get_service(service_id).await?;
        self.ensure_unambiguous_service_name(&service).await?;
        let old_parameters = self.get_service_parameters(service_id).await?;

        // Get old configuration
        let old_config = ServiceConfig {
            name: service.name.clone(),
            service_type: ServiceType::from_str(&service.service_type).map_err(|_| {
                ExternalServiceError::InvalidServiceType {
                    id: service_id,
                    service_type: service.service_type.clone(),
                }
            })?,
            version: service.version.clone(),
            parameters: serde_json::to_value(&old_parameters).map_err(|e| {
                ExternalServiceError::InternalError {
                    reason: format!("Failed to serialize old parameters: {}", e),
                }
            })?,
        };

        // Create new configuration with updated Docker image
        let mut new_parameters = old_parameters.clone();
        new_parameters.insert(
            "docker_image".to_string(),
            serde_json::Value::String(new_docker_image.clone()),
        );

        let new_config = ServiceConfig {
            name: service.name.clone(),
            service_type: ServiceType::from_str(&service.service_type).map_err(|_| {
                ExternalServiceError::InvalidServiceType {
                    id: service_id,
                    service_type: service.service_type.clone(),
                }
            })?,
            version: service.version.clone(),
            parameters: serde_json::to_value(&new_parameters).map_err(|e| {
                ExternalServiceError::InternalError {
                    reason: format!("Failed to serialize new parameters: {}", e),
                }
            })?,
        };

        // Create service instance
        let service_type_enum = ServiceType::from_str(&service.service_type).map_err(|_| {
            ExternalServiceError::InvalidServiceType {
                id: service_id,
                service_type: service.service_type.clone(),
            }
        })?;
        let service_instance = self.create_service_instance_for_parameter_value(
            service.name.clone(),
            service_type_enum,
            &new_config.parameters,
        )?;

        // Call the upgrade method on the service instance. `upgrade()`
        // returns `anyhow::Result` (shared across every provider), so a
        // same-version rejection is downcast here -- before the error is
        // stringified below -- into a typed `UpgradeRejected` variant the
        // handler can match on directly instead of substring-matching the
        // message text.
        service_instance
            .upgrade(old_config, new_config.clone())
            .await
            .map_err(|e| {
                if let Some(rejected) =
                    e.downcast_ref::<crate::externalsvc::postgres::PostgresUpgradeRejected>()
                {
                    ExternalServiceError::UpgradeRejected {
                        id: service_id,
                        reason: rejected.to_string(),
                    }
                } else {
                    ExternalServiceError::InitializationFailed {
                        id: service_id,
                        reason: format!("Upgrade failed: {}", e),
                    }
                }
            })?;

        // Update the service configuration in the database with the new Docker image
        let config_json = serde_json::to_string(&new_parameters).map_err(|e| {
            ExternalServiceError::InternalError {
                reason: format!("Failed to serialize config to JSON: {}", e),
            }
        })?;

        let encrypted_config = self
            .encryption_service
            .encrypt_string(&config_json)
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!("Failed to encrypt config: {}", e),
            })?;

        // Update service config in database: `running`, or `starting` until
        // the readiness gate confirms the new image can serve a real request.
        // The gate reads the stored ports and credentials, which an image
        // upgrade does not change.
        let gate = self.readiness_gate_for(&service).await?;
        let mut service_update: external_services::ActiveModel = service.clone().into();
        service_update.config = Set(Some(encrypted_config));
        let readiness = Self::apply_started_status(&service, &mut service_update, gate.as_deref());
        service_update.updated_at = Set(Utc::now());
        self.persist_service_config(service_id, service_update)
            .await?;
        if let (Some(gate), Some(attempt)) = (gate, readiness) {
            self.spawn_readiness_watch(service_id, gate, attempt, true);
        }

        self.get_service_info(service_id).await
    }

    pub async fn update_service(
        &self,
        service_id: i32,
        request: UpdateExternalServiceRequest,
    ) -> Result<ExternalServiceInfo, ExternalServiceError> {
        let service = self.get_service(service_id).await?;
        self.ensure_unambiguous_service_name(&service).await?;

        // The stored parameters name the engine an S3/Blob service runs, so
        // they are read first: the update rules follow the engine, exactly
        // as they do at creation.
        let mut existing_params = self.get_service_parameters(service_id).await?;
        // The engine never changes on update (`backend` is read-only), so the
        // pre-merge snapshot identifies the pre-rename container.
        let stored_params = existing_params.clone();
        let strategy_key = Self::update_strategy_key(&service.service_type, &existing_params);
        let strategy = parameter_strategies::get_strategy(&strategy_key).ok_or(
            ExternalServiceError::InvalidServiceType {
                id: service_id,
                service_type: strategy_key.clone(),
            },
        )?;

        // Prepare update parameters (merge docker_image if provided)
        let mut update_params = request.parameters.clone();
        // Detail responses use "***" for sensitive parameters. Treat that
        // sentinel as "leave unchanged" so opening and saving an edit form
        // cannot replace a real credential with the mask.
        update_params.retain(|name, value| {
            !(Self::is_sensitive_parameter(name)
                && value.as_str().is_some_and(|value| value == "***"))
        });
        if let Some(docker_image) = &request.docker_image {
            info!(
                "Updating service {} with new Docker image: {}",
                service_id, docker_image
            );
            update_params.insert(
                "docker_image".to_string(),
                serde_json::Value::String(docker_image.clone()),
            );
        }

        // Validate that only updateable parameters are being changed
        strategy
            .validate_for_update(&update_params)
            .map_err(|reason| ExternalServiceError::ParameterValidationFailed {
                service_id,
                reason,
            })?;

        // Merge the updates into the parameters read above
        let previous_image = existing_params.get("docker_image").cloned();
        let previous_replica_set = existing_params.get("replica_set").cloned();
        strategy
            .merge_updates(&mut existing_params, update_params)
            .map_err(|reason| ExternalServiceError::ParameterValidationFailed {
                service_id,
                reason,
            })?;
        // A new image means a new container. Refuse before saving anything
        // or stopping the service when that container would not see the
        // current one's data (#1415).
        if existing_params.get("docker_image") != previous_image.as_ref() {
            self.ensure_recreate_keeps_data(&service, &existing_params, "changing its image")
                .await?;
        }
        // Enabling a MongoDB replica set also means a new container (the
        // flag is on its command line): refuse before saving when the data
        // would not come along, rather than report a setting that can't apply.
        let replica_set_enabled = existing_params
            .get("replica_set")
            .and_then(|v| v.as_str())
            .is_some_and(|v| !v.is_empty());
        if replica_set_enabled
            && existing_params.get("replica_set") != previous_replica_set.as_ref()
        {
            self.ensure_recreate_keeps_data(&service, &existing_params, "enabling its replica set")
                .await?;
        }

        // Serialize and encrypt the merged parameters
        let config_json = serde_json::to_string(&existing_params).map_err(|e| {
            ExternalServiceError::InternalError {
                reason: format!("Failed to serialize config to JSON: {}", e),
            }
        })?;

        let encrypted_config = self
            .encryption_service
            .encrypt_string(&config_json)
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!("Failed to encrypt config: {}", e),
            })?;

        // Fail fast on a taken name, before anything is stopped. This is a
        // short transaction that is rolled back before any Docker work: the
        // name lock is never held across a container call. The save below
        // reserves the name again, atomically with the write.
        if let Some(new_name) = request.name.as_deref() {
            self.ensure_service_name_available(service_id, new_name)
                .await?;
        }

        // Update service config (and optionally name/slug) in database.
        // `name` was previously accepted by the request but silently dropped;
        // applying it here keeps the API contract honest.
        let mut service_update: external_services::ActiveModel = service.clone().into();
        service_update.config = Set(Some(encrypted_config));
        // Set when the rename moves the service off a running container that
        // is identified by its pre-rename name.
        let mut pre_rename_instance: Option<Box<dyn ExternalService>> = None;
        if let Some(new_name) = request.name {
            if new_name != service.name {
                // Reserving a database name does not grant ownership of an
                // existing Docker resource. Refuse a rename destination before
                // stopping the original service or persisting its new identity.
                // Every resource is named after the service, and Docker cannot
                // rename a volume: a rename would start the service on a new,
                // empty volume and leave its data behind under the old name.
                self.ensure_rename_keeps_data(&service, &new_name).await?;
                let mut renamed_service = service.clone();
                renamed_service.name = new_name.clone();
                self.ensure_new_local_service_resources_unused(&renamed_service)
                    .await?;
                // The running container is identified by the service's
                // current (pre-rename) name (see create_service_instance).
                // initialize_service() below rebuilds its stop-then-recreate
                // instance from whatever name is in the DB at that point --
                // once the rename is saved, it looks for a container under
                // the *new* name, finds nothing, and the still-running old
                // container is left holding the host port, so the new
                // container's start fails with "port is already allocated".
                // Build the instance for the pre-rename identity now (so a
                // failure to build it stops nothing), and stop it after the
                // rename is saved and before initialize_service().
                let service_type_enum =
                    ServiceType::from_str(&service.service_type).map_err(|_| {
                        ExternalServiceError::InvalidServiceType {
                            id: service_id,
                            service_type: service.service_type.clone(),
                        }
                    })?;
                // The stored parameters pick the engine, so a Garage service
                // stops `garage-<old>` rather than a `rustfs-<old>` that
                // does not exist.
                pre_rename_instance = Some(self.create_service_instance_for_parameters(
                    service.name.clone(),
                    service_type_enum,
                    &stored_params,
                )?);
            }
            let new_slug = Self::generate_slug(&new_name);
            service_update.name = Set(new_name);
            service_update.slug = Set(Some(new_slug));
        }
        service_update.updated_at = Set(Utc::now());
        // The save reserves the name and writes the row in one short
        // transaction, so a name taken since the check above is refused
        // before the original container is stopped.
        self.persist_service_config(service_id, service_update)
            .await?;

        if let Some(old_instance) = pre_rename_instance {
            if let Err(e) = old_instance.stop().await {
                info!(
                    "Could not stop pre-rename container for service {} (may not exist): {}",
                    service_id, e
                );
            }
        }

        // Reinitialize the service (this will stop, remove, and recreate the container with new image)
        self.initialize_service(service_id).await?;

        self.get_service_info(service_id).await
    }

    pub async fn delete_service(&self, service_id: i32) -> Result<(), ExternalServiceError> {
        // Get service to check if it exists
        let service = self.get_service(service_id).await?;
        self.ensure_unambiguous_service_name(&service).await?;
        let service_type_enum = ServiceType::from_str(&service.service_type).map_err(|_| {
            ExternalServiceError::InvalidServiceType {
                id: service_id,
                service_type: service.service_type.clone(),
            }
        })?;

        // Safety check: Verify no projects are linked to this service
        let linked_projects = project_services::Entity::find()
            .filter(project_services::Column::ServiceId.eq(service_id))
            .all(self.db.as_ref())
            .await?;

        if !linked_projects.is_empty() {
            return Err(ExternalServiceError::ServiceHasLinkedProjects {
                service_id,
                project_count: linked_projects.len(),
            });
        }

        // Load cluster members BEFORE deleting DB records (needed for container cleanup)
        let members = service_members::Entity::find()
            .filter(service_members::Column::ServiceId.eq(service_id))
            .all(self.db.as_ref())
            .await?;
        let is_cluster = !members.is_empty();

        // Fetch parameters BEFORE deleting the DB row -- they're needed below to
        // reconstruct the service instance for container removal, and
        // get_service_parameters looks the service up by ID, which would fail
        // once the row is gone.
        let parameters = self.get_service_parameters(service_id).await?;

        // Delete from database first
        self.db
            .transaction::<_, (), ExternalServiceError>(|txn| {
                Box::pin(async move {
                    // Restore admission holds this source row while it
                    // records its active run. Never cascade that reservation
                    // away while the clone's engine can still create resources.
                    let source = external_services::Entity::find_by_id(service_id)
                        .lock_exclusive()
                        .one(txn)
                        .await?
                        .ok_or(ExternalServiceError::ServiceNotFound { id: service_id })?;
                    if let Some(run) = temps_entities::restore_runs::Entity::find()
                        .filter(
                            temps_entities::restore_runs::Column::SourceServiceId.eq(service_id),
                        )
                        .filter(
                            temps_entities::restore_runs::Column::Status
                                .is_in(["pending", "running"]),
                        )
                        .one(txn)
                        .await?
                    {
                        return Err(ExternalServiceError::ServiceNameRestoreConflict {
                            name: source.name,
                            restore_run_id: run.id,
                        });
                    }
                    // Auto-generated per-service schedules are lifecycle-owned
                    // by Temps. Disable one in the same transaction when its
                    // final target is removed; user-created schedules are left
                    // untouched for the operator to repair deliberately.
                    let generated_schedules = backup_schedules::Entity::find()
                        .inner_join(backup_schedule_services::Entity)
                        .filter(backup_schedule_services::Column::ServiceId.eq(service_id))
                        .filter(backup_schedules::Column::GeneratedKind.is_not_null())
                        .all(txn)
                        .await?;
                    for schedule in generated_schedules {
                        let remaining_targets = backup_schedule_services::Entity::find()
                            .filter(backup_schedule_services::Column::ScheduleId.eq(schedule.id))
                            .filter(backup_schedule_services::Column::ServiceId.ne(service_id))
                            .count(txn)
                            .await?;
                        if generated_schedule_loses_last_target(
                            schedule.generated_kind.as_deref(),
                            remaining_targets,
                        ) {
                            let mut update: backup_schedules::ActiveModel = schedule.into();
                            update.enabled = Set(false);
                            update.updated_at = Set(Utc::now());
                            update.update(txn).await?;
                        }
                    }

                    project_services::Entity::delete_many()
                        .filter(project_services::Column::ServiceId.eq(service_id))
                        .exec(txn)
                        .await?;

                    // Admission captured the repository's immutable identity.
                    // Preserve it across renames; backfill only missing legacy
                    // fields using the service row locked above.
                    external_service_backups::Entity::update_many()
                        .col_expr(
                            external_service_backups::Column::ServiceNameSnapshot,
                            Expr::value(source.name.clone()),
                        )
                        .filter(external_service_backups::Column::ServiceId.eq(service_id))
                        .filter(external_service_backups::Column::ServiceNameSnapshot.is_null())
                        .exec(txn)
                        .await?;
                    external_service_backups::Entity::update_many()
                        .col_expr(
                            external_service_backups::Column::ServiceTypeSnapshot,
                            Expr::value(source.service_type.clone()),
                        )
                        .filter(external_service_backups::Column::ServiceId.eq(service_id))
                        .filter(external_service_backups::Column::ServiceTypeSnapshot.is_null())
                        .exec(txn)
                        .await?;

                    service_members::Entity::delete_many()
                        .filter(service_members::Column::ServiceId.eq(service_id))
                        .exec(txn)
                        .await?;

                    external_services::Entity::delete_by_id(service_id)
                        .exec(txn)
                        .await?;

                    Ok(())
                })
            })
            .await
            .map_err(ExternalServiceError::from)?;

        // Stop the per-cluster role reconciler before dropping its records,
        // otherwise a tick mid-deletion could re-write what we just removed.
        self.stop_role_reconciler(service_id).await;

        // Drop DNS records that pointed at this service's members (ADR-011).
        // Best-effort, post-DB-commit: the rows that owned the records are
        // already gone, so the worst case is a stale record served until
        // the next janitor pass. We ignore registry errors so a stuck DNS
        // plane doesn't fail an otherwise successful service deletion.
        // Per-member records (Tier 2).
        for member in &members {
            let owner_id = member.id as i64;
            if let Err(e) = self
                .dns_registry
                .delete_by_owner(temps_dns::InternalOwnerKind::ServiceMember, owner_id)
                .await
            {
                warn!(
                    service_id,
                    member_id = member.id,
                    error = %e,
                    "Failed to drop DNS records for deleted cluster member"
                );
            }
        }
        // Role/VIP records (Tier 3) — owner_id == service_id.
        if let Err(e) = self
            .dns_registry
            .delete_by_owner(temps_dns::InternalOwnerKind::ServiceRole, service_id as i64)
            .await
        {
            warn!(
                service_id,
                error = %e,
                "Failed to drop role/VIP DNS records for deleted cluster"
            );
        }

        // Remove containers
        if is_cluster {
            // Cluster: remove each member container (best-effort, log failures)
            info!(
                "Removing {} cluster member container(s) for service {}",
                members.len(),
                service_id
            );
            let mut errors = Vec::new();

            for member in &members {
                if let Some(node_id) = member.node_id {
                    match self.get_remote_client(node_id).await {
                        Ok(client) => {
                            if let Err(e) = client.remove_service(&member.container_name).await {
                                let msg = format!(
                                    "Failed to remove remote container '{}' on node {}: {}",
                                    member.container_name, node_id, e
                                );
                                error!("{}", msg);
                                errors.push(msg);
                            }
                        }
                        Err(e) => {
                            let msg = format!(
                                "Failed to connect to node {} to remove '{}': {}",
                                node_id, member.container_name, e
                            );
                            error!("{}", msg);
                            errors.push(msg);
                        }
                    }
                } else {
                    // Local container. If this process has no local Docker
                    // daemon (control-plane profile), there is nothing local
                    // to clean up here — that's expected, not a failure.
                    match self.docker.get() {
                        Some(docker) => {
                            if let Err(e) = docker
                                .remove_container(
                                    &member.container_name,
                                    Some(bollard::query_parameters::RemoveContainerOptions {
                                        force: true,
                                        ..Default::default()
                                    }),
                                )
                                .await
                            {
                                let msg = format!(
                                    "Failed to remove local container '{}': {}",
                                    member.container_name, e
                                );
                                error!("{}", msg);
                                errors.push(msg);
                            }

                            // Also remove the volume
                            let volume_name = format!("{}_data", member.container_name);
                            if let Err(e) = docker
                                .remove_volume(
                                    &volume_name,
                                    None::<bollard::query_parameters::RemoveVolumeOptions>,
                                )
                                .await
                            {
                                warn!("Failed to remove volume '{}': {}", volume_name, e);
                            }
                        }
                        None => {
                            debug!(
                                container_name = %member.container_name,
                                "No local Docker daemon in this process; skipping local cleanup for member"
                            );
                        }
                    }
                }
            }

            if !errors.is_empty() {
                return Err(ExternalServiceError::DeletionFailed {
                    id: service_id,
                    reason: format!(
                        "Service deleted from database but {} container(s) failed to remove: {}",
                        errors.len(),
                        errors.join("; ")
                    ),
                });
            }
        } else {
            // Standalone: remove single container
            info!("Removing service {} container", service_id);
            if let Some(node_id) = service.node_id {
                let client = self.get_remote_client(node_id).await?;
                let service_instance = self.create_service_instance_for_parameters(
                    service.name.clone(),
                    service_type_enum,
                    &parameters,
                )?;
                let container_name = self
                    .resolve_remote_container_name(&client, service_instance.as_ref(), &parameters)
                    .await?;
                client.remove_service(&container_name).await.map_err(|e| {
                    ExternalServiceError::DeletionFailed {
                        id: service_id,
                        reason: e.to_string(),
                    }
                })?;
            } else {
                let service_instance = self.create_service_instance_for_parameters(
                    service.name.clone(),
                    service_type_enum,
                    &parameters,
                )?;
                // An original kept aside by an interrupted recreate still
                // mounts the data volume and would make removing it fail.
                // Deleting the service deletes that data anyway.
                if let Ok(docker) = self.require_docker() {
                    crate::externalsvc::recreate_guard::remove_set_aside_originals(
                        docker.as_ref(),
                        &service_instance.get_docker_container_name(),
                        &service_instance.get_name(),
                    )
                    .await;
                }
                service_instance.remove().await.map_err(|e| {
                    ExternalServiceError::DeletionFailed {
                        id: service_id,
                        reason: e.to_string(),
                    }
                })?;
            }
        }

        // Sweep containers stranded by the pre-#495 naming split.
        //
        // Installs that enabled Blob before the fix have a second container
        // under the old prefixed name (`rustfs-blob-temps-blob`). It has no
        // `external_services` row of its own, and the row that could still
        // reach it is the one the transaction above just deleted — so this is
        // the last moment anything can find it. Local containers only: the
        // blob and kv plugins run on the control plane, never on a worker.
        //
        // Best-effort. A missing legacy container is the normal case on any
        // install created after the fix, and a Docker hiccup here must not
        // turn an otherwise successful delete into a 500.
        if service.node_id.is_none() {
            for legacy_name in legacy_managed_instance_names(&service.name, service_type_enum) {
                match self.create_service_instance(legacy_name.clone(), service_type_enum) {
                    Ok(instance) => match instance.remove().await {
                        Ok(()) => info!(
                            service_id,
                            legacy_name,
                            "Removed duplicate container left behind by the earlier managed-service naming split"
                        ),
                        Err(e) => debug!(
                            service_id,
                            legacy_name,
                            error = %e,
                            "No legacy duplicate container to remove (expected on installs created after the naming fix)"
                        ),
                    },
                    Err(e) => debug!(
                        service_id,
                        legacy_name,
                        error = %e,
                        "Skipping legacy container cleanup (Docker unavailable or disabled)"
                    ),
                }
            }
        }

        Ok(())
    }

    pub(super) async fn get_service_info(
        &self,
        service_id: i32,
    ) -> Result<ExternalServiceInfo, ExternalServiceError> {
        let service = self.get_service(service_id).await?;

        // Load cluster members if this is a cluster topology, and enrich
        // each one with the monitor's view of its FSM state. The UI uses
        // `live_state` for the role badge so failovers and promotions
        // reflect immediately, instead of being gated on the
        // `service_members.role` reconciler.
        let members = if service.topology == "cluster" {
            self.get_service_members_with_live_state(service_id).await?
        } else {
            Vec::new()
        };

        let readiness = reported_readiness(&service.status, service.health_metadata.as_ref());

        Ok(ExternalServiceInfo {
            id: service.id,
            name: service.name,
            service_type: ServiceType::from_str(&service.service_type).map_err(|_| {
                ExternalServiceError::InvalidServiceType {
                    id: service_id,
                    service_type: service.service_type,
                }
            })?,
            version: service.version,
            status: service.status,
            connection_info: None,
            created_at: service.created_at.to_rfc3339(),
            updated_at: service.updated_at.to_rfc3339(),
            node_id: service.node_id,
            topology: service.topology,
            members,
            error_message: service.error_message,
            metrics_enabled: service.metrics_enabled,
            continuous_archive_s3_source_id: service.continuous_archive_s3_source_id,
            continuous_archive_pinned_at: service
                .continuous_archive_pinned_at
                .map(|pinned_at| pinned_at.to_rfc3339()),
            readiness,
        })
    }

    /// Guard against reconciling/(re)starting or recreating a container
    /// while a Postgres major upgrade (or its rollback) is actively working
    /// on it. The upgrade orchestrator stops the container and
    /// deletes/recreates its volumes across several phases
    /// (`postgres_upgrade.rs`), and `rollback()` does the same in reverse;
    /// a concurrent container mutation (start, reconcile, resource-limit
    /// recreate, a same-version image swap) would recreate the container
    /// against an implicitly re-created empty volume, silently diverging
    /// from the volume the orchestrator/rollback is dumping/restoring —
    /// accepting writes that never make it into the upgraded database.
    ///
    /// Called from every entry point that stops/recreates a service's
    /// container: `start_service`, `initialize_service` (covers
    /// `update_service`, service creation, and the resource-limit recreate
    /// path), and the legacy same-version `upgrade_service`.
    ///
    /// PENDING/RUNNING/ROLLING_BACK rows are active; FAILED/COMPLETED/
    /// CANCELLED/ROLLED_BACK are terminal and don't block a start.
    pub(super) async fn ensure_no_active_upgrade(
        &self,
        service_id: i32,
    ) -> Result<(), ExternalServiceError> {
        use crate::externalsvc::postgres_upgrade::status;

        let active = postgres_major_upgrades::Entity::find()
            .filter(postgres_major_upgrades::Column::ServiceId.eq(service_id))
            .filter(
                postgres_major_upgrades::Column::Status
                    .eq(status::PENDING)
                    .or(postgres_major_upgrades::Column::Status.eq(status::RUNNING))
                    .or(postgres_major_upgrades::Column::Status.eq(status::ROLLING_BACK)),
            )
            .one(self.db.as_ref())
            .await
            .map_err(|e| ExternalServiceError::DatabaseError {
                reason: format!("failed to check for active major upgrade: {}", e),
            })?;

        if let Some(upgrade) = active {
            return Err(ExternalServiceError::UpgradeInProgress {
                id: service_id,
                upgrade_id: upgrade.id,
                phase: upgrade.phase,
            });
        }

        Ok(())
    }

    pub async fn get_service_details_by_slug(
        &self,
        service: external_services::Model,
    ) -> Result<ExternalServiceDetails, ExternalServiceError> {
        // Get service info
        let service_info = self.get_service_info(service.id).await?;
        let mut parameters = self.get_service_parameters(service.id).await?;
        let service_type = ServiceType::from_str(&service_info.service_type.to_string())?;

        // Schema only — resolved statically so a service's detail page still
        // renders on a process with no local Docker daemon.
        let parameter_schema = Self::parameter_schema_for(service_type, &parameters)?;
        let sensitive_parameters = Self::mask_sensitive_parameter_values(&mut parameters);

        Ok(ExternalServiceDetails {
            service: service_info,
            parameter_schema,
            current_parameters: Some(parameters),
            sensitive_parameters,
        })
    }
}

#[cfg(test)]
mod tests;
