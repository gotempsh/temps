// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Service initialization, start and stop.

use crate::externalsvc::{ExternalService, ServiceConfig, ServiceType};
use crate::services::*;
use chrono::Utc;
use sea_orm::{ActiveModelTrait, Set};
use std::collections::HashMap;
use temps_entities::external_services;
use tracing::{info, warn};

impl ExternalServiceManager {
    pub(super) async fn initialize_service(
        &self,
        service_id: i32,
    ) -> Result<(), ExternalServiceError> {
        info!("Initializing service: {}", service_id);
        self.ensure_no_active_upgrade(service_id).await?;
        let service = self.get_service(service_id).await?;
        self.ensure_unambiguous_service_name(&service).await?;
        let parameters = self.get_service_parameters(service_id).await?;
        let service_type_enum = ServiceType::from_str(&service.service_type).map_err(|_| {
            ExternalServiceError::InvalidServiceType {
                id: service_id,
                service_type: service.service_type.clone(),
            }
        })?;

        // Remote node — delegate to agent
        if let Some(node_id) = service.node_id {
            return self
                .initialize_service_remote(
                    service_id,
                    node_id,
                    &service,
                    &parameters,
                    &service_type_enum,
                )
                .await;
        }

        // Local node — guard the profile contract before starting any container.
        if !self.local_workloads_enabled {
            return Err(ExternalServiceError::LocalWorkloadsDisabled {
                name: service.name.clone(),
            });
        }

        // Local node — use existing Docker-based service logic
        let service_instance = self.create_service_instance_for_parameters(
            service.name.clone(),
            service_type_enum,
            &parameters,
        )?;

        let config = ServiceConfig {
            name: service.name.clone(),
            service_type: ServiceType::from_str(&service.service_type).map_err(|_| {
                ExternalServiceError::InvalidServiceType {
                    id: service_id,
                    service_type: service.service_type.clone(),
                }
            })?,
            version: service.version.clone(),
            parameters: serde_json::to_value(parameters).map_err(|e| {
                ExternalServiceError::InternalError {
                    reason: format!("Failed to serialize parameters: {}", e),
                }
            })?,
        };

        // Stop existing container if running (important for upgrades)
        info!("Stopping existing container for service {}", service_id);
        if let Err(e) = service_instance.stop().await {
            // Log but don't fail - container might not exist yet
            info!("Could not stop container (may not exist): {}", e);
        }

        // Initialize the service
        let inferred_params = service_instance.init(config).await.map_err(|e| {
            match e.downcast_ref::<crate::externalsvc::recreate_guard::RecreateGuardError>() {
                Some(guard) if guard.is_refusal() => ExternalServiceError::DataSafetyRefusal {
                    service_id,
                    name: service.name.clone(),
                    reason: guard.to_string(),
                },
                _ => ExternalServiceError::InitializationFailed {
                    id: service_id,
                    reason: e.to_string(),
                },
            }
        })?;

        // Store inferred parameters
        self.store_inferred_parameters(service_id, service_instance.as_ref(), inferred_params)
            .await?;

        // Start the service (create and start container)
        service_instance
            .start()
            .await
            .map_err(|e| ExternalServiceError::InitializationFailed {
                id: service_id,
                reason: format!("Failed to start service: {}", e),
            })?;

        // `running`, or `starting` until the engine's readiness gate
        // confirms the service can serve a real request. Built from the
        // stored parameters, which now include the ports and credentials
        // `init()` inferred.
        let gate = self.readiness_gate_for(&service).await?;
        let mut service_update: external_services::ActiveModel = service.clone().into();
        let readiness = Self::apply_started_status(&service, &mut service_update, gate.as_deref());
        service_update.updated_at = Set(Utc::now());
        service_update.update(self.db.as_ref()).await?;
        if let (Some(gate), Some(attempt)) = (gate, readiness) {
            self.spawn_readiness_watch(service_id, gate, attempt, true);
        }

        // Attach to the overlay and publish `<service>.temps.local` so apps
        // scheduled on other nodes have an address that can actually work.
        // Best-effort: a healthy service must not be failed because the
        // overlay isn't bootstrapped (single-node installs never need it).
        if let Err(e) = self.register_standalone_service_dns(service_id).await {
            warn!(
                service_id,
                error = %e,
                "Failed to publish internal DNS record for service; cross-node linking \
                 will be refused at deploy time until this succeeds"
            );
        }

        Ok(())
    }

    /// Initialize a service on a remote node via the agent API.
    async fn initialize_service_remote(
        &self,
        service_id: i32,
        node_id: i32,
        service: &external_services::Model,
        parameters: &HashMap<String, serde_json::Value>,
        service_type: &ServiceType,
    ) -> Result<(), ExternalServiceError> {
        info!(
            "Initializing service {} on remote node {}",
            service_id, node_id
        );
        let client = self.get_remote_client(node_id).await?;

        // Flatten serde_json::Value parameters to strings for the builder
        let string_params: HashMap<String, String> = parameters
            .iter()
            .map(|(k, v)| {
                let s = match v {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                (k.clone(), s)
            })
            .collect();

        let create_params =
            self.build_remote_create_params(&service.name, service_type, &string_params)?;

        // Try to stop existing container first (ignore errors — may not exist)
        let container_name = create_params.name.clone();
        if let Err(e) = client.stop_service(&container_name).await {
            info!(
                "Could not stop remote container {} (may not exist): {}",
                container_name, e
            );
        }

        // Create the container on the remote node. A database in `enforce`
        // mode (ADR-055 Phase 4) is created from its admitted workload spec
        // through the agent's Docker driver; every other service sends the
        // agent's own request exactly as before.
        let response = if service.workload_mode.creates_from_spec() {
            self.create_remote_from_workload_spec(service, &client, &create_params)
                .await
                .map_err(|e| ExternalServiceError::InitializationFailed {
                    id: service_id,
                    reason: format!("Remote agent create from workload spec failed: {}", e),
                })?
        } else {
            client.create_service(create_params).await.map_err(|e| {
                ExternalServiceError::InitializationFailed {
                    id: service_id,
                    reason: format!("Remote agent create_service failed: {}", e),
                }
            })?
        };

        info!(
            "Service {} created on node {} — container {} (port {})",
            service_id, node_id, response.container_name, response.host_port
        );

        // Store the host_port as an inferred parameter so env-var generation works
        let mut inferred = HashMap::new();
        inferred.insert("port".to_string(), response.host_port.to_string());
        inferred.insert("container_id".to_string(), response.container_id.clone());
        // The agent reports the container's `temps-overlay` IP when it
        // attached one. That IP is the ONLY address another node can use to
        // reach this service — the published host port binds to 127.0.0.1
        // on the worker — so persist it and publish it as DNS below.
        if let Some(compute_ip) = response
            .compute_ip
            .as_deref()
            .map(str::trim)
            .filter(|ip| !ip.is_empty())
        {
            inferred.insert("compute_ip".to_string(), compute_ip.to_string());
        }
        inferred.insert(
            "container_name".to_string(),
            response.container_name.clone(),
        );

        // Persist inferred parameters
        let mut current_params = self.get_service_parameters(service_id).await?;
        for (key, value) in inferred {
            if Self::is_inferred_parameter(&key) || !current_params.contains_key(&key) {
                current_params.insert(key, serde_json::Value::String(value));
            }
        }
        let config_json = serde_json::to_string(&current_params).map_err(|e| {
            ExternalServiceError::InternalError {
                reason: format!("Failed to serialize updated params: {}", e),
            }
        })?;
        let encrypted_config = self
            .encryption_service
            .encrypt_string(&config_json)
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!("Failed to encrypt updated params: {}", e),
            })?;

        let mut service_update: external_services::ActiveModel = service.clone().into();
        service_update.status = Set("running".to_string());
        service_update.config = Set(Some(encrypted_config));
        service_update.updated_at = Set(Utc::now());
        self.persist_service_config(service_id, service_update)
            .await?;

        // Publish `<service>.temps.local` -> the overlay IP the agent
        // reported. Best-effort for the same reason as the local path.
        if let Err(e) = self.register_standalone_service_dns(service_id).await {
            warn!(
                service_id,
                node_id,
                error = %e,
                "Failed to publish internal DNS record for remote service; cross-node \
                 linking will be refused at deploy time until this succeeds"
            );
        }

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Cluster initialization
    // -----------------------------------------------------------------------

    /// Initialize a plugin-owned service instance from its stored config and
    /// persist whatever the engine inferred back onto the row.
    ///
    /// The blob and kv plugins construct their own `RustfsService` /
    /// `RedisService` and used to call `init()` directly, dropping the
    /// inferred parameters. That let `external_services.config` drift from
    /// the container it describes — most consequentially the port, which
    /// `ExternalService::health_probe` reads straight out of the stored
    /// config rather than from the live instance.
    ///
    /// On an install upgraded across the #495 naming fix, the stored port is
    /// the one the pre-fix manager container took. Uploads are fine (they go
    /// through the instance, which adopts the running container's real
    /// port), but the health monitor probes the stale port — so the console
    /// reports Blob as down the moment the leftover container is removed,
    /// while the service is actually healthy. Writing back on this path
    /// keeps the row describing the container that exists.
    ///
    /// Only genuinely inferred keys are merged (see
    /// `is_inferred_parameter`), so operator-set configuration such as
    /// `docker_image` or `access_key` is never overwritten.
    pub async fn initialize_plugin_service(
        &self,
        service_id: i32,
        service_instance: &dyn ExternalService,
    ) -> Result<(), ExternalServiceError> {
        let config = self.get_service_config(service_id).await?;

        let inferred_params = service_instance.init(config).await.map_err(|e| {
            ExternalServiceError::InitializationFailed {
                id: service_id,
                reason: e.to_string(),
            }
        })?;

        // Persisting is best-effort. By this point the instance is
        // initialized and the service is usable, so failing the caller would
        // turn a working enable into a 500 over bookkeeping. A stale row
        // only degrades health reporting, and the next successful start or
        // enable rewrites it.
        if let Err(e) = self
            .store_inferred_parameters(service_id, service_instance, inferred_params)
            .await
        {
            warn!(
                service_id,
                error = %e,
                "Service initialized, but its inferred parameters could not be persisted — \
                 health checks may report a stale port until the next start"
            );
        }

        Ok(())
    }

    pub async fn start_service(
        &self,
        service_id: i32,
    ) -> Result<ExternalServiceInfo, ExternalServiceError> {
        self.ensure_no_active_upgrade(service_id).await?;
        let service = self.get_service(service_id).await?;
        self.ensure_unambiguous_service_name(&service).await?;
        // Never fall back to `initialize_service` here: that builds a
        // standalone container under the service's own name.
        if service.topology == "cluster" {
            self.run_cluster_lifecycle(service_id, ClusterLifecycleAction::Start)
                .await?;
            return self.get_service_info(service_id).await;
        }
        let service_type_enum = ServiceType::from_str(&service.service_type).map_err(|_| {
            ExternalServiceError::InvalidServiceType {
                id: service_id,
                service_type: service.service_type.clone(),
            }
        })?;
        let parameters = self.get_service_parameters(service_id).await?;

        // Remote node — delegate to agent
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

            match client.start_service(&container_name).await {
                Ok(()) => {}
                Err(e) => {
                    info!(
                        "Remote start failed for service {} ({}), falling back to initialize: {}",
                        service_id, service.name, e
                    );
                    self.initialize_service(service_id)
                        .await
                        .map_err(|init_err| ExternalServiceError::StartFailed {
                            id: service_id,
                            reason: format!(
                                "Start failed: {}. Re-initialize also failed: {}",
                                e, init_err
                            ),
                        })?;
                    return self.get_service_info(service_id).await;
                }
            }
        } else {
            // Local node — guard the profile contract before starting any container.
            if !self.local_workloads_enabled {
                return Err(ExternalServiceError::LocalWorkloadsDisabled {
                    name: service.name.clone(),
                });
            }

            let service_instance = self.create_service_instance_for_parameters(
                service.name.clone(),
                service_type_enum,
                &parameters,
            )?;

            // A freshly-constructed instance has no in-memory config, so
            // `start()` can't resolve an imported service's real container
            // name (it only lives in `config.container_name`). Hydrate it via
            // `init()` first — same fix as `stop_service` — so `start()`
            // targets the real container instead of failing with
            // "configuration not found" and falling back to a full
            // re-initialize.
            #[allow(deprecated)]
            let needs_config_hydration = matches!(
                service_type_enum,
                ServiceType::Mariadb
                    | ServiceType::Postgres
                    | ServiceType::Redis
                    | ServiceType::Mongodb
                    | ServiceType::Minio
            );
            if needs_config_hydration {
                let parameters = self.get_service_parameters(service_id).await?;
                if parameters.contains_key("container_name") {
                    let service_config = ServiceConfig {
                        name: service.name.clone(),
                        service_type: service_type_enum,
                        version: service.version.clone(),
                        parameters: serde_json::to_value(parameters).map_err(|e| {
                            ExternalServiceError::InternalError {
                                reason: format!("Failed to serialize parameters: {}", e),
                            }
                        })?,
                    };
                    service_instance.init(service_config).await.map_err(|e| {
                        ExternalServiceError::StartFailed {
                            id: service_id,
                            reason: format!(
                                "Failed to initialize imported {} service before start: {}",
                                service_type_enum, e
                            ),
                        }
                    })?;
                }
            }

            // A managed PostgreSQL service is started with its stored
            // configuration loaded (`force_recreate` = load it, then
            // `start()`), so start-time drift repair (archive_mode,
            // shared_preload_libraries) has a specification to rebuild from.
            // Without it, start() can only start the existing container as
            // it is. The repair itself goes through the recreate guard.
            let managed_postgres = matches!(service_type_enum, ServiceType::Postgres)
                && !parameters.contains_key("container_name");
            // A managed container that no longer exists is created afresh
            // by re-initialisation, which stores the host port it actually
            // bound (a port retry happens when the old one is now taken).
            // Starting it through the provider would keep that port only in
            // memory and leave the service's connection details stale.
            if managed_postgres {
                let docker = self.require_docker()?;
                let container = service_instance.get_docker_container_name();
                match docker
                    .inspect_container(
                        &container,
                        None::<bollard::query_parameters::InspectContainerOptions>,
                    )
                    .await
                {
                    Ok(_) => {}
                    Err(bollard::errors::Error::DockerResponseServerError {
                        status_code: 404,
                        ..
                    }) => {
                        info!(
                            "Container {} of service {} ({}) is missing; re-initializing it",
                            container, service_id, service.name
                        );
                        self.initialize_service(service_id).await?;
                        return self.get_service_info(service_id).await;
                    }
                    Err(e) => {
                        return Err(ExternalServiceError::DockerError {
                            id: service_id,
                            reason: format!(
                                "Failed to inspect container '{}' before starting service {}: {}",
                                container, service_id, e
                            ),
                        })
                    }
                }
            }
            let start_result = if managed_postgres {
                let service_config = ServiceConfig {
                    name: service.name.clone(),
                    service_type: service_type_enum,
                    version: service.version.clone(),
                    parameters: serde_json::to_value(&parameters).map_err(|e| {
                        ExternalServiceError::InternalError {
                            reason: format!(
                                "Failed to serialize parameters of service {}: {}",
                                service_id, e
                            ),
                        }
                    })?,
                };
                service_instance.force_recreate(service_config).await
            } else {
                service_instance.start().await
            };
            match start_result {
                Ok(()) => {}
                Err(e) => {
                    // The guard's verdict is final. Re-initialising would
                    // stop whatever holds the container's name and recreate
                    // it again -- over an original that is still set aside,
                    // or a replacement that failed verification.
                    if let Some(guard_failure) =
                        Self::guard_failure_for_start(service_id, &service.name, &e)
                    {
                        return Err(guard_failure);
                    }
                    info!(
                        "Direct start failed for service {} ({}), falling back to initialize: {}",
                        service_id, service.name, e
                    );
                    self.initialize_service(service_id)
                        .await
                        .map_err(|init_err| ExternalServiceError::StartFailed {
                            id: service_id,
                            reason: format!(
                                "Start failed: {}. Re-initialize also failed: {}",
                                e, init_err
                            ),
                        })?;
                    return self.get_service_info(service_id).await;
                }
            }
        }

        // `running`, or `starting` until the engine's readiness gate
        // confirms the restarted service can serve a real request.
        let gate = self.readiness_gate_for(&service).await?;
        let mut service_update: external_services::ActiveModel = service.clone().into();
        let readiness = Self::apply_started_status(&service, &mut service_update, gate.as_deref());
        service_update.updated_at = Set(Utc::now());
        service_update.update(self.db.as_ref()).await?;
        if let (Some(gate), Some(attempt)) = (gate, readiness) {
            self.spawn_readiness_watch(service_id, gate, attempt, true);
        }

        // Refresh the WAL health snapshot immediately for Postgres services.
        // The reconcile-on-start path may have recreated the container with a
        // different archive_mode; without this refresh, the UI would keep
        // showing the pre-recreate snapshot for up to one health-monitor
        // cycle (~30s). Failure is non-fatal — the background monitor will
        // converge on its own.
        if service_type_enum == ServiceType::Postgres {
            if let Err(e) = self.refresh_postgres_wal_health(service_id).await {
                tracing::debug!(
                    "Failed to refresh WAL health snapshot for service {} after start: {} (non-fatal)",
                    service_id,
                    e
                );
            }
        }

        // Docker hands out a fresh IP whenever a container is recreated, and
        // `start()` reconciles (and may recreate) the container. Re-publish
        // the A record so `<service>.temps.local` never points at a dead
        // address. Best-effort — a running service must not be reported as
        // failed because DNS is unavailable.
        if let Err(e) = self.register_standalone_service_dns(service_id).await {
            warn!(
                service_id,
                error = %e,
                "Failed to refresh internal DNS record after starting service"
            );
        }

        self.get_service_info(service_id).await
    }

    /// The error to return from a failed start when the failure came from
    /// the recreate guard, or `None` for any other failure (which may fall
    /// back to re-initialising the service). A guard error is never retried
    /// through `initialize_service`: it stops the container that holds the
    /// name and recreates it again, which is exactly what the guard just
    /// refused or failed to complete safely.
    fn guard_failure_for_start(
        service_id: i32,
        name: &str,
        error: &anyhow::Error,
    ) -> Option<ExternalServiceError> {
        let guard =
            error.downcast_ref::<crate::externalsvc::recreate_guard::RecreateGuardError>()?;
        Some(if guard.is_refusal() {
            ExternalServiceError::DataSafetyRefusal {
                service_id,
                name: name.to_string(),
                reason: guard.to_string(),
            }
        } else {
            ExternalServiceError::StartFailed {
                id: service_id,
                reason: guard.to_string(),
            }
        })
    }

    pub async fn stop_service(
        &self,
        service_id: i32,
    ) -> Result<ExternalServiceInfo, ExternalServiceError> {
        let service = self.get_service(service_id).await?;
        self.ensure_unambiguous_service_name(&service).await?;
        // A cluster has no container under the service's own name: its
        // monitor and data nodes are separate, suffixed members.
        if service.topology == "cluster" {
            self.run_cluster_lifecycle(service_id, ClusterLifecycleAction::Stop)
                .await?;
            return self.get_service_info(service_id).await;
        }
        let service_type_enum = ServiceType::from_str(&service.service_type).map_err(|_| {
            ExternalServiceError::InvalidServiceType {
                id: service_id,
                service_type: service.service_type.clone(),
            }
        })?;
        let parameters = self.get_service_parameters(service_id).await?;

        // Remote node — delegate to agent
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

            client.stop_service(&container_name).await.map_err(|e| {
                ExternalServiceError::StopFailed {
                    id: service_id,
                    reason: e.to_string(),
                }
            })?;
        } else {
            // Local node
            let service_instance = self.create_service_instance_for_parameters(
                service.name.clone(),
                service_type_enum,
                &parameters,
            )?;

            #[allow(deprecated)]
            let needs_config_hydration = matches!(
                service_type_enum,
                ServiceType::Mariadb
                    | ServiceType::Postgres
                    | ServiceType::Redis
                    | ServiceType::Mongodb
                    | ServiceType::Minio
            );
            if needs_config_hydration {
                let parameters = self.get_service_parameters(service_id).await?;
                if parameters.contains_key("container_name") {
                    let service_config = ServiceConfig {
                        name: service.name.clone(),
                        service_type: service_type_enum,
                        version: service.version.clone(),
                        parameters: serde_json::to_value(parameters).map_err(|e| {
                            ExternalServiceError::InternalError {
                                reason: format!("Failed to serialize parameters: {}", e),
                            }
                        })?,
                    };
                    service_instance.init(service_config).await.map_err(|e| {
                        ExternalServiceError::StopFailed {
                            id: service_id,
                            reason: format!(
                                "Failed to initialize imported {} service before stop: {}",
                                service_type_enum, e
                            ),
                        }
                    })?;
                }
            }

            service_instance
                .stop()
                .await
                .map_err(|e| ExternalServiceError::StopFailed {
                    id: service_id,
                    reason: e.to_string(),
                })?;
        }

        // Update status to stopped
        let mut service_update: external_services::ActiveModel = service.into();
        service_update.status = Set("stopped".to_string());
        service_update.updated_at = Set(Utc::now());
        service_update.update(self.db.as_ref()).await?;

        self.get_service_info(service_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "docker-tests")]
    use crate::externalsvc::ServiceType;
    use crate::services::test_support::*;
    #[cfg(feature = "docker-tests")]
    use serde_json::Value as JsonValue;
    #[cfg(feature = "docker-tests")]
    use std::collections::HashMap;

    fn guard_error(restored: bool) -> anyhow::Error {
        crate::externalsvc::recreate_guard::RecreateGuardError::DataNotPreserved {
            engine: crate::externalsvc::recreate_guard::ManagedEngine::Postgres,
            service: "app".into(),
            container: "postgres-app".into(),
            operation: "a test".into(),
            reason: "identity changed".into(),
            rollback: "rollback note".into(),
            original_in_place: restored,
        }
        .into()
    }

    #[test]
    fn a_failed_start_after_a_guard_error_is_never_retried_by_reinitialising() {
        // Whether or not the rollback completed, the guard's verdict is
        // returned as the start failure instead of recreating again.
        for restored in [true, false] {
            let failure =
                ExternalServiceManager::guard_failure_for_start(7, "app", &guard_error(restored));
            assert!(
                matches!(
                    failure,
                    Some(ExternalServiceError::StartFailed { id: 7, ref reason })
                        if reason.contains("postgres-app")
                ),
                "restored={restored}: {failure:?}"
            );
        }
    }

    #[test]
    fn a_failed_start_without_a_guard_error_may_fall_back_to_reinitialising() {
        let other = anyhow::anyhow!("container is missing");
        assert!(ExternalServiceManager::guard_failure_for_start(7, "app", &other).is_none());
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_stop_and_start_service() {
        let (manager, _test_db) = setup_test_manager_or_skip!();
        let random_unused_port = get_unused_port();
        // Create a service first. Postgres requires database/username/password
        // at the parameter-validation layer (parameter_strategies), so they
        // must be present even when the test only cares about lifecycle.
        let mut params = HashMap::new();
        params.insert(
            "database".to_string(),
            JsonValue::String("testdb".to_string()),
        );
        params.insert(
            "username".to_string(),
            JsonValue::String("testuser".to_string()),
        );
        params.insert(
            "password".to_string(),
            JsonValue::String("testpass".to_string()),
        );
        params.insert(
            "port".to_string(),
            JsonValue::String(random_unused_port.to_string()),
        );
        params.insert(
            "host".to_string(),
            JsonValue::String("localhost".to_string()),
        );

        let request = CreateExternalServiceRequest {
            name: "test-stop-start".to_string(),
            service_type: ServiceType::Postgres,
            version: None,
            parameters: params,
            node_id: None,
            topology: "standalone".to_string(),
            members: Vec::new(),
        };

        let service = manager.create_service(request).await.unwrap();
        let service_id = service.id;

        // Stop the service
        let stopped_service = manager.stop_service(service_id).await;
        assert!(stopped_service.is_ok());
        assert_eq!(stopped_service.unwrap().status, "stopped");

        // Start the service
        let started_service = manager.start_service(service_id).await;
        assert!(started_service.is_ok());
        assert_eq!(started_service.unwrap().status, "running");

        // Cleanup
        let _ = manager.delete_service(service_id).await;
    }

    #[tokio::test]
    async fn legacy_duplicate_service_cannot_stop_shared_container() {
        let existing = encrypted_service_model(17, serde_json::json!({}));
        let duplicate = encrypted_service_model(23, serde_json::json!({}));
        let manager = mock_service_manager(vec![vec![duplicate], vec![existing]]);
        let error = manager.stop_service(23).await.unwrap_err();
        assert!(matches!(
            error,
            ExternalServiceError::AmbiguousServiceName {
                service_id: 23,
                existing_service_id: 17,
                ..
            }
        ));
    }
}
