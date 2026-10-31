// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Container runtime inspection, stats, resource limits and log ingestion.

use crate::externalsvc::{ServiceConfig, ServiceType};
use crate::services::*;
use chrono::Utc;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QuerySelect, Set};
use std::collections::HashMap;
use temps_entities::{external_services, service_members};
use tracing::info;

impl ExternalServiceManager {
    /// Resolve `(role, container_name)` pairs for every container that
    /// makes up this service. Standalone services return a single entry;
    /// cluster services return one entry per `service_members` row.
    async fn resolve_member_containers(
        &self,
        service: &external_services::Model,
    ) -> Result<Vec<(String, String)>, ExternalServiceError> {
        if service.topology == "cluster" {
            let members = service_members::Entity::find()
                .filter(service_members::Column::ServiceId.eq(service.id))
                .all(self.db.as_ref())
                .await?;
            Ok(members
                .into_iter()
                .map(|m| (m.role, m.container_name))
                .collect())
        } else {
            // Build a fresh service instance just to ask for its container
            // name — every engine knows its own naming convention.
            let service_type = ServiceType::from_str(&service.service_type).map_err(|_| {
                ExternalServiceError::InvalidServiceType {
                    id: service.id,
                    service_type: service.service_type.clone(),
                }
            })?;
            let parameters = self.get_service_parameters(service.id).await?;
            let instance = self.create_service_instance_for_parameters(
                service.name.clone(),
                service_type,
                &parameters,
            )?;
            // An imported service's real container name wins over the derived
            // `{type}-{name}` one — see the identical pattern in `get_runtime_info`.
            let container_name = parameters
                .get("container_name")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| instance.get_docker_container_name());
            Ok(vec![("standalone".to_string(), container_name)])
        }
    }

    /// Inspect a single container and project the result onto
    /// `ContainerRuntimeInfo`. Treats container-not-found as a soft
    /// signal (returns `container_id: None`) rather than an error so
    /// the UI can still render "container missing" instead of a 500.
    async fn inspect_one_container(
        &self,
        role: String,
        container_name: String,
    ) -> ContainerRuntimeInfo {
        // No local Docker daemon in this process (control-plane profile) is
        // the same soft signal as "container not found" here — there is no
        // separate error path for this best-effort inspection.
        let Some(docker) = self.docker.get() else {
            return ContainerRuntimeInfo {
                role,
                container_name,
                container_id: None,
                status: None,
                restart_count: None,
                oom_killed: None,
                exit_code: None,
                started_at: None,
                finished_at: None,
                image: None,
                resource_limits: crate::externalsvc::ServiceResourceLimits::default(),
            };
        };

        let inspected = docker
            .inspect_container(
                &container_name,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await;

        match inspected {
            Ok(info) => {
                let state = info.state.as_ref();
                let host_config_limits = info
                    .host_config
                    .as_ref()
                    .map(|hc| crate::externalsvc::ServiceResourceLimits {
                        memory_mb: hc.memory.filter(|&m| m > 0).map(|m| m / (1024 * 1024)),
                        memory_swap_mb: hc
                            .memory_swap
                            .filter(|&m| m > 0)
                            .map(|m| m / (1024 * 1024)),
                        nano_cpus: hc.nano_cpus.filter(|&n| n > 0),
                        cpu_shares: hc.cpu_shares.filter(|&n| n > 0),
                        shm_size_mb: hc.shm_size.filter(|&m| m > 0).map(|m| m / (1024 * 1024)),
                    })
                    .unwrap_or_default();

                ContainerRuntimeInfo {
                    role,
                    container_name,
                    container_id: info.id,
                    status: state.and_then(|s| {
                        s.status
                            .as_ref()
                            .map(|st| format!("{:?}", st).to_lowercase())
                    }),
                    restart_count: info.restart_count,
                    oom_killed: state.and_then(|s| s.oom_killed),
                    exit_code: state.and_then(|s| s.exit_code),
                    started_at: state.and_then(|s| s.started_at.clone()),
                    finished_at: state.and_then(|s| s.finished_at.clone()),
                    image: info.image,
                    resource_limits: host_config_limits,
                }
            }
            Err(_) => ContainerRuntimeInfo {
                role,
                container_name,
                container_id: None,
                status: None,
                restart_count: None,
                oom_killed: None,
                exit_code: None,
                started_at: None,
                finished_at: None,
                image: None,
                resource_limits: crate::externalsvc::ServiceResourceLimits::default(),
            },
        }
    }

    /// Get a snapshot of every container that makes up this service:
    /// status, restart count, OOM-killed flag, exit code, and current
    /// applied resource limits.
    pub async fn get_service_runtime(
        &self,
        service_id: i32,
    ) -> Result<ServiceRuntimeReport, ExternalServiceError> {
        let service = self.get_service(service_id).await?;
        let containers = self.resolve_member_containers(&service).await?;

        let mut members = Vec::with_capacity(containers.len());
        for (role, name) in containers {
            members.push(self.inspect_one_container(role, name).await);
        }

        Ok(ServiceRuntimeReport {
            service_id: service.id,
            topology: service.topology,
            members,
        })
    }

    /// Sample one stats snapshot from every container in this service.
    /// Uses `one_shot=true, stream=false` so the call returns immediately
    /// instead of holding the connection open for streaming updates.
    pub async fn get_service_stats(
        &self,
        service_id: i32,
    ) -> Result<ServiceStatsReport, ExternalServiceError> {
        // Stream consumption + sampling now lives in
        // `sample_container_stats_twice`. This caller just iterates
        // containers and projects the result.
        let service = self.get_service(service_id).await?;
        let containers = self.resolve_member_containers(&service).await?;

        let mut members = Vec::with_capacity(containers.len());
        for (role, name) in containers {
            // Docker's `one_shot` stats response carries `precpu_stats` as
            // zeros — the CPU formula needs deltas, so a single one_shot
            // sample produces either 0% or the "cumulative since container
            // start" ratio (which was the pre-fix bug: a container at
            // 108% real load read back as 0.6% because total/system over
            // the container's full lifetime is dominated by idle history).
            //
            // Take two one_shot samples 1s apart and compute the delta
            // ourselves. Matches `docker stats` exactly. The 1s window is
            // the same default the Docker CLI uses for its "default"
            // streaming interval.
            let stats = match sample_container_stats_twice(
                match self.docker.get() {
                    Some(d) => d,
                    None => {
                        members.push(ContainerStatsSample {
                            role,
                            container_name: name,
                            cpu_percent: None,
                            memory_usage_bytes: None,
                            memory_limit_bytes: None,
                            memory_percent: None,
                            online_cpus: None,
                        });
                        continue;
                    }
                },
                &name,
            )
            .await
            {
                Some((first, second)) => {
                    // `first` is the earlier sample, `second` is the later one.
                    // `compute_stats_sample` wants (current=later, previous=earlier)
                    // so the delta is positive — passing them reversed makes
                    // `cpu_delta` negative and CPU reads back as `None` (the UI
                    // shows "—" while memory still works from `current`).
                    compute_stats_sample(role.clone(), name.clone(), &second, Some(&first))
                }
                None => ContainerStatsSample {
                    role,
                    container_name: name,
                    cpu_percent: None,
                    memory_usage_bytes: None,
                    memory_limit_bytes: None,
                    memory_percent: None,
                    online_cpus: None,
                },
            };
            members.push(stats);
        }

        Ok(ServiceStatsReport {
            service_id: service.id,
            topology: service.topology,
            members,
        })
    }

    /// Sample stats for every container in this service against a
    /// caller-held baseline map (`container_name` → previous raw sample).
    ///
    /// Designed for periodic pollers (e.g. the health monitor's 30s loop):
    /// the poll interval itself provides the CPU delta window, so unlike
    /// `get_service_stats` no artificial 1s sleep per container is needed.
    /// The first tick for a container has no baseline, so `cpu_percent` is
    /// `None` (memory is still reported) and the baseline is seeded for the
    /// next tick.
    ///
    /// The baseline map is rewritten on every call: entries for containers
    /// that no longer back the service are dropped, and entries whose
    /// sample failed this tick (container stopped / remote node) are
    /// carried over unchanged — cumulative counters stay valid across a
    /// longer window, and a restart in between reads back as a counter
    /// reset which `cpu_percent_from_delta` already rejects.
    pub async fn sample_service_stats(
        &self,
        service: &external_services::Model,
        baselines: &mut HashMap<String, bollard::models::ContainerStatsResponse>,
    ) -> Result<ServiceStatsReport, ExternalServiceError> {
        let containers = self.resolve_member_containers(service).await?;

        let mut members = Vec::with_capacity(containers.len());
        let mut next_baselines = HashMap::with_capacity(containers.len());

        for (role, name) in containers {
            match sample_container_stats_once(
                match self.docker.get() {
                    Some(d) => d,
                    None => {
                        if let Some(prev) = baselines.remove(&name) {
                            next_baselines.insert(name.clone(), prev);
                        }
                        members.push(ContainerStatsSample {
                            role,
                            container_name: name,
                            cpu_percent: None,
                            memory_usage_bytes: None,
                            memory_limit_bytes: None,
                            memory_percent: None,
                            online_cpus: None,
                        });
                        continue;
                    }
                },
                &name,
            )
            .await
            {
                Some(current) => {
                    let previous = baselines.get(&name);
                    members.push(compute_stats_sample(role, name.clone(), &current, previous));
                    next_baselines.insert(name, current);
                }
                None => {
                    // Container missing/stopped or on a remote node — keep
                    // the old baseline (if any) so a later success still has
                    // a valid delta window.
                    if let Some(prev) = baselines.remove(&name) {
                        next_baselines.insert(name.clone(), prev);
                    }
                    members.push(ContainerStatsSample {
                        role,
                        container_name: name,
                        cpu_percent: None,
                        memory_usage_bytes: None,
                        memory_limit_bytes: None,
                        memory_percent: None,
                        online_cpus: None,
                    });
                }
            }
        }

        *baselines = next_baselines;

        Ok(ServiceStatsReport {
            service_id: service.id,
            topology: service.topology.clone(),
            members,
        })
    }

    /// Apply a resource-limits block to every container that backs this
    /// service via Docker's live `update_container` API. Works on running
    /// AND stopped containers (Docker accepts updates for both states —
    /// stopped containers pick up the new caps on next start).
    ///
    /// **Limitation: removing a previously-set memory cap requires a
    /// container recreate on most Docker setups.** The Docker daemon
    /// silently treats `Memory: 0` as "no change" and rejects `Memory:
    /// -1` with "Minimum memory limit allowed is 6MB" on Docker Desktop /
    /// recent versions. We detect this case and mark the outcome as
    /// "requires_recreate" so the UI can prompt the operator to restart.
    ///
    /// CPU caps don't have this problem: `NanoCpus: 0` correctly removes
    /// the CPU cap on a running container.
    ///
    /// Returns a per-member outcome so the caller can tell which
    /// containers got the update and which were skipped (missing or
    /// errored). Never fails the request as a whole — limits are already
    /// persisted in the DB by the time this runs.
    async fn apply_limits_to_running_containers(
        &self,
        service: &external_services::Model,
        limits: &crate::externalsvc::ServiceResourceLimits,
    ) -> Vec<ResourceLimitApplyResult> {
        // Resolve every container name that backs this service. Soft-fails
        // back to an empty list when resolution itself errors so the
        // outer call doesn't blow up — limits are already persisted.
        let containers = match self.resolve_member_containers(service).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    service_id = service.id,
                    error = %e,
                    "Could not resolve member containers for live limit update"
                );
                return Vec::new();
            }
        };

        let mut results = Vec::with_capacity(containers.len());

        let docker = match self.docker.get() {
            Some(d) => d,
            None => return Vec::new(),
        };
        for (role, container_name) in containers {
            // First check whether the container actually exists. Calling
            // update_container on a missing name returns a confusing 404;
            // distinguishing "missing" from "failed" up front gives the
            // operator a clearer signal in the response.
            let inspected = docker
                .inspect_container(
                    &container_name,
                    None::<bollard::query_parameters::InspectContainerOptions>,
                )
                .await;

            let outcome = match inspected {
                Err(_) => ResourceLimitApplyResult {
                    role,
                    container_name,
                    outcome: "missing".to_string(),
                    error: None,
                },
                Ok(info) => {
                    let is_running = info.state.as_ref().and_then(|s| s.running).unwrap_or(false);

                    // Detect "removing a previously-set memory cap" up
                    // front: if the container currently has a non-zero
                    // memory limit and the new request is unlimited, the
                    // live update can't honor it. Tell the caller they
                    // need to restart the container to pick up the
                    // change. The persisted limits are correct, and the
                    // recreate path in the engines' `start()` will apply
                    // them on next boot.
                    let current_memory = info
                        .host_config
                        .as_ref()
                        .and_then(|hc| hc.memory)
                        .unwrap_or(0);
                    let removing_memory_cap = current_memory > 0 && limits.memory_mb.is_none();

                    // Detect a shared-memory (`/dev/shm`) size change. Docker
                    // cannot hot-apply `shm_size` — it's a create-time-only
                    // HostConfig field with no slot in the update API — so a
                    // changed shm requires removing + recreating the
                    // container. Only flag when the operator explicitly set a
                    // size that differs from what the live container has; an
                    // unset (None) shm leaves whatever Docker already chose and
                    // must not trigger a spurious recreate against the 64 MiB
                    // default.
                    let current_shm = info
                        .host_config
                        .as_ref()
                        .and_then(|hc| hc.shm_size)
                        .unwrap_or(0);
                    let shm_changed = match limits.shm_size_mb {
                        Some(mb) => {
                            current_shm > 0 && mb.saturating_mul(1024 * 1024) != current_shm
                        }
                        None => false,
                    };

                    if removing_memory_cap {
                        ResourceLimitApplyResult {
                            role,
                            container_name,
                            outcome: "requires_recreate".to_string(),
                            error: Some(
                                "Docker cannot remove a memory limit on a live \
                                 container. Restart the service to apply unlimited \
                                 memory."
                                    .to_string(),
                            ),
                        }
                    } else if shm_changed {
                        ResourceLimitApplyResult {
                            role,
                            container_name,
                            outcome: "requires_recreate".to_string(),
                            error: Some(
                                "Changing shared-memory (/dev/shm) size requires \
                                 deleting and recreating the container (a stop/start \
                                 reuses the old container and keeps the old shm size). \
                                 Recreate the service to apply the new shm size."
                                    .to_string(),
                            ),
                        }
                    } else {
                        // Build the body fresh per-container so a future
                        // per-member override (different caps per cluster
                        // role, etc.) slots in cleanly.
                        let body = build_container_update_body(limits);
                        match docker.update_container(&container_name, body).await {
                            Ok(()) => ResourceLimitApplyResult {
                                role,
                                container_name,
                                outcome: if is_running { "applied" } else { "stopped" }.to_string(),
                                error: None,
                            },
                            Err(e) => {
                                // Most common failure: setting memory
                                // below current usage (Docker rejects).
                                // Surface the raw message so the operator
                                // sees the actionable detail.
                                let msg = e.to_string();
                                tracing::warn!(
                                    service_id = service.id,
                                    container = %container_name,
                                    error = %msg,
                                    "docker update_container rejected"
                                );
                                ResourceLimitApplyResult {
                                    role,
                                    container_name,
                                    outcome: "failed".to_string(),
                                    error: Some(msg),
                                }
                            }
                        }
                    }
                }
            };
            results.push(outcome);
        }

        results
    }

    /// Persist a new resource-limits block onto an existing service's
    /// `external_service_params`, then live-apply via Docker's update API.
    ///
    /// Memory and CPU caps can be hot-changed on running containers — no
    /// restart required. Stopped containers also accept the update; they
    /// pick up the new caps on next start. When the container is
    /// completely absent (e.g., never created on this node), only the
    /// stored config is updated and the apply step records "missing".
    /// Delete and recreate a service's container so a new `/dev/shm` size
    /// takes effect.
    ///
    /// `shm_size` is fixed when a Docker container is created — there is no
    /// live update for it, and a plain stop+start reuses the existing
    /// container (keeping the old shm). The only way to apply a new value is
    /// to REMOVE the container and let the engine recreate it. We remove the
    /// container with `v: false` so the **data volume is preserved** (the
    /// engine's `remove()` trait method would also drop the volume — we must
    /// not use it here), then call `initialize_service`, whose
    /// `create_container` now takes the create branch and bakes in the new
    /// `HostConfig.shm_size`.
    ///
    /// Scope: standalone, local-node services. Remote-node and cluster
    /// services are rejected with a clear message so the caller surfaces
    /// "recreate manually" rather than silently doing nothing.
    async fn recreate_service_container_for_shm(
        &self,
        service: &external_services::Model,
    ) -> Result<(), ExternalServiceError> {
        // Remote-node containers live on a worker's Docker daemon, not on
        // `self.docker`. Removing here would target the wrong daemon. The
        // worker's create path already honors shm; the operator must
        // redeploy/recreate the service on that node.
        if service.node_id.is_some() {
            return Err(ExternalServiceError::InternalError {
                reason: format!(
                    "Service {} runs on a remote node; recreate it on that node to apply the new shm size",
                    service.id
                ),
            });
        }

        // Cluster recreate spans multiple members with ordering constraints
        // (monitor → primary → replicas) and isn't covered by
        // `initialize_service`. Don't pretend; tell the operator to recreate.
        if service.topology == "cluster" {
            return Err(ExternalServiceError::InternalError {
                reason: format!(
                    "Service {} is a cluster; recreate its members to apply the new shm size",
                    service.id
                ),
            });
        }

        let service_type = ServiceType::from_str(&service.service_type).map_err(|_| {
            ExternalServiceError::InvalidServiceType {
                id: service.id,
                service_type: service.service_type.clone(),
            }
        })?;
        let parameters = self.get_service_parameters(service.id).await?;
        let instance = self.create_service_instance_for_parameters(
            service.name.clone(),
            service_type,
            &parameters,
        )?;
        let container_name = instance.get_docker_container_name();

        // Recreating a local container needs a real Docker daemon; this is
        // a hard requirement (the caller already ruled out remote/cluster).
        let docker = self.docker.require()?;

        // Through the shared guard (#1415): refuse unless the new container
        // mounts the data the current one uses, keep the current one until
        // the new one is verified to hold the same data, restore it otherwise.
        let service_config = ServiceConfig {
            name: service.name.clone(),
            service_type: ServiceType::from_str(&service.service_type).map_err(|_| {
                ExternalServiceError::InvalidServiceType {
                    id: service.id,
                    service_type: service.service_type.clone(),
                }
            })?,
            version: service.version.clone(),
            parameters: serde_json::to_value(&parameters).map_err(|e| {
                ExternalServiceError::InternalError {
                    reason: format!(
                        "Failed to serialize parameters of service {}: {}",
                        service.id, e
                    ),
                }
            })?,
        };
        let (engine, replacement) = instance.recreate_plan(&service_config).ok_or_else(|| {
            ExternalServiceError::DataSafetyRefusal {
                service_id: service.id,
                name: service.name.clone(),
                reason: format!(
                    "its {} container '{}' cannot be recreated safely by Temps (imported, or an engine without a known storage layout), so the new /dev/shm size was saved but not applied. Recreate the container by hand with the same mounts",
                    service.service_type, container_name
                ),
            }
        })?;
        let report = crate::externalsvc::recreate_guard::recreate_preserving_data(
            crate::externalsvc::recreate_guard::RecreateRequest {
                docker: docker.as_ref(),
                engine,
                service_name: &service.name,
                container_name: &container_name,
                replacement,
                operation: "applying a new /dev/shm size",
            },
            || self.initialize_service(service.id),
        )
        .await
        .map_err(|e| {
            if e.is_refusal() {
                ExternalServiceError::DataSafetyRefusal {
                    service_id: service.id,
                    name: service.name.clone(),
                    reason: e.to_string(),
                }
            } else {
                ExternalServiceError::InternalError {
                    reason: e.to_string(),
                }
            }
        })?;
        info!(
            service_id = service.id,
            container = %container_name,
            replaced = report.replaced,
            "Recreated container to apply new /dev/shm size; data verified"
        );
        Ok(())
    }

    /// This service's log ingestion override (both `None` = inherit).
    pub async fn get_log_ingestion(
        &self,
        service_id: i32,
    ) -> Result<ServiceLogIngestion, ExternalServiceError> {
        Ok(ServiceLogIngestion::of(
            &self.get_service(service_id).await?,
        ))
    }

    /// Replace this service's log ingestion override. Takes effect on its
    /// running containers within a minute (the log aggregator's budget sync)
    /// without recreating anything.
    pub async fn set_log_ingestion(
        &self,
        service_id: i32,
        settings: ServiceLogIngestion,
    ) -> Result<ServiceLogIngestionUpdate, ExternalServiceError> {
        temps_core::ContainerLogSettings::validate_ingest_override(
            settings.log_ingest_lines_per_sec,
            settings.log_ingest_kib_per_sec,
        )
        .map_err(|reason| ExternalServiceError::ParameterValidationFailed { service_id, reason })?;

        const OPERATION: &str = "saving the log ingestion budget of";
        let txn = begin_bounded_transaction(&self.db, service_id, OPERATION, SERVICE_LOCK_TIMEOUT)
            .await?;
        let (previous, service_name) =
            match Self::write_log_ingestion(&txn, service_id, &settings).await {
                Ok(saved) => saved,
                Err(error) => {
                    // Roll back now so the row lock is not held until the
                    // pooled connection is recycled.
                    let error = lock_timeout_or_database_error(
                        service_id,
                        OPERATION,
                        SERVICE_LOCK_TIMEOUT,
                        error,
                    );
                    return Err(rollback_before_error(txn, error).await);
                }
            };
        txn.commit().await?;

        info!(
            service_id,
            lines_per_sec = ?settings.log_ingest_lines_per_sec,
            kib_per_sec = ?settings.log_ingest_kib_per_sec,
            "log ingestion override saved for external service {} ({})",
            service_id,
            service_name
        );
        Ok(ServiceLogIngestionUpdate {
            service_id,
            service_name,
            previous,
            current: settings,
        })
    }

    /// The locked read-compare-write behind [`Self::set_log_ingestion`].
    /// Returns the previous override and the service name.
    async fn write_log_ingestion(
        txn: &sea_orm::DatabaseTransaction,
        service_id: i32,
        settings: &ServiceLogIngestion,
    ) -> Result<(ServiceLogIngestion, String), ExternalServiceError> {
        let service = external_services::Entity::find_by_id(service_id)
            .lock_exclusive()
            .one(txn)
            .await?
            .ok_or(ExternalServiceError::ServiceNotFound { id: service_id })?;
        let previous = ServiceLogIngestion::of(&service);
        let service_name = service.name.clone();
        if previous != *settings {
            let mut active: external_services::ActiveModel = service.into();
            active.log_ingest_lines_per_sec = Set(settings.log_ingest_lines_per_sec);
            active.log_ingest_kib_per_sec = Set(settings.log_ingest_kib_per_sec);
            active.updated_at = Set(Utc::now());
            active.update(txn).await?;
        }
        Ok((previous, service_name))
    }

    pub async fn update_service_resource_limits(
        &self,
        service_id: i32,
        new_limits: crate::externalsvc::ServiceResourceLimits,
    ) -> Result<ResourceLimitsUpdateResponse, ExternalServiceError> {
        if let Err(e) = new_limits.validate() {
            return Err(ExternalServiceError::ParameterValidationFailed {
                service_id,
                reason: e,
            });
        }

        // Load the service plus its current parameters JSON.
        let service = self.get_service(service_id).await?;
        let mut config_json: serde_json::Value = match service.config.as_deref() {
            Some(s) => match self.encryption_service.decrypt_string(s) {
                Ok(decrypted) => serde_json::from_str(&decrypted).map_err(|e| {
                    ExternalServiceError::InternalError {
                        reason: format!(
                            "Failed to parse stored config for service {}: {}",
                            service_id, e
                        ),
                    }
                })?,
                Err(e) => {
                    return Err(ExternalServiceError::InternalError {
                        reason: format!(
                            "Failed to decrypt config for service {}: {}",
                            service_id, e
                        ),
                    })
                }
            },
            None => serde_json::json!({}),
        };

        // Capture the prior shm size before we splice in the new block.
        // shm_size is create-time-only in Docker, so a change here means the
        // container must be removed + recreated — a plain live update can't
        // honor it. We compare prior vs new and drive a recreate below.
        let prior_limits = crate::externalsvc::ServiceResourceLimits::from_parameters(&config_json);
        let shm_changed = prior_limits.shm_size_mb != new_limits.shm_size_mb;

        // Splice the resources block into the parameters JSON. Setting
        // `unlimited` removes the block so the service goes back to
        // running without caps.
        if new_limits.is_unlimited() {
            if let Some(obj) = config_json.as_object_mut() {
                obj.remove("resources");
            }
        } else {
            let limits_value = serde_json::to_value(&new_limits).map_err(|e| {
                ExternalServiceError::InternalError {
                    reason: format!("Failed to serialize resource limits: {}", e),
                }
            })?;
            match config_json.as_object_mut() {
                Some(obj) => {
                    obj.insert("resources".to_string(), limits_value);
                }
                None => {
                    config_json = serde_json::json!({ "resources": limits_value });
                }
            }
        }

        // Re-encrypt and persist.
        let serialized = serde_json::to_string(&config_json).map_err(|e| {
            ExternalServiceError::InternalError {
                reason: format!("Failed to serialize updated config: {}", e),
            }
        })?;
        let encrypted = self
            .encryption_service
            .encrypt_string(&serialized)
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!("Failed to encrypt updated config: {}", e),
            })?;

        let mut active: external_services::ActiveModel = service.clone().into();
        active.config = Set(Some(encrypted));
        self.persist_service_config(service_id, active).await?;

        // When the shared-memory size changed, a live `update_container` can't
        // honor it (shm_size is fixed at container-CREATE time in Docker). The
        // container must be DELETED and recreated — a stop+start reuses the old
        // container and keeps the old shm. `recreate_service_container_for_shm`
        // removes the container (data volume preserved) so the engine's
        // `create_container` rebuilds it with the new `HostConfig.shm_size`.
        //
        // Best-effort: the limits are already persisted, so a recreate failure
        // must not roll them back. We log and fall through to report the
        // per-member outcome — the operator can recreate manually.
        let mut shm_not_applied = None;
        if shm_changed {
            if let Err(e) = self.recreate_service_container_for_shm(&service).await {
                shm_not_applied = Some(e.to_string());
                tracing::warn!(
                    service_id,
                    error = %e,
                    "Failed to recreate service container after shm size change; \
                     limits are persisted but the running container still has the \
                     old shm size — recreate manually to apply"
                );
            }
        }

        // Best-effort live apply. Errors here don't undo the persisted
        // limits — the operator can still recreate the container manually
        // to pick them up. After a recreate above, the fresh container
        // already has the new caps, so this reports "applied"/"missing"
        // harmlessly.
        let applied = self
            .apply_limits_to_running_containers(&service, &new_limits)
            .await;

        Ok(ResourceLimitsUpdateResponse {
            limits: new_limits,
            applied,
            shm_not_applied,
        })
    }
}

/// Map our `ServiceResourceLimits` onto a bollard `ContainerUpdateBody`.
///
/// CRITICAL: Docker uses `0` (not `null`) as the special value for
/// "remove this constraint". Sending `null` leaves the existing limit in
/// place — exactly the bug an operator hits when they switch a service
/// from limited back to unlimited. So we always emit explicit zeros for
/// the four fields we manage.
///
/// Conversions:
/// - memory_mb       → memory       (bytes; 0 = unlimited)
/// - memory_swap_mb  → memory_swap  (bytes; -1 = unlimited swap, 0 = no swap; we map None→0)
/// - nano_cpus       → nano_cpus    (1e9 = 1 core; 0 = unlimited)
/// - cpu_shares      → cpu_shares   (default 1024; 0 = default)
///
/// NOTE: `shm_size` is intentionally absent. Docker treats `/dev/shm` size
/// as create-time-only; `ContainerUpdateBody` has no `shm_size` field.
/// Changing shm requires REMOVING + RECREATING the container — handled by
/// the auto-recreate path in `update_service_resource_limits`, NOT here.
fn build_container_update_body(
    limits: &crate::externalsvc::ServiceResourceLimits,
) -> bollard::models::ContainerUpdateBody {
    let memory_bytes = limits
        .memory_mb
        .map(|mb| mb.saturating_mul(1024 * 1024))
        .unwrap_or(0);
    let memory_swap_bytes = limits
        .memory_swap_mb
        .map(|mb| mb.saturating_mul(1024 * 1024))
        .unwrap_or(0);
    bollard::models::ContainerUpdateBody {
        memory: Some(memory_bytes),
        memory_swap: Some(memory_swap_bytes),
        nano_cpus: Some(limits.nano_cpus.unwrap_or(0)),
        cpu_shares: Some(limits.cpu_shares.unwrap_or(0)),
        ..Default::default()
    }
}

/// Sample the same container twice ~1s apart so we have a delta window for
/// the CPU formula. Returns `None` on any error or if Docker returns no
/// frames (container missing / stopped). The 1-second pause matches the
/// Docker CLI's default sampling interval.
async fn sample_container_stats_twice(
    docker: &bollard::Docker,
    name: &str,
) -> Option<(
    bollard::models::ContainerStatsResponse,
    bollard::models::ContainerStatsResponse,
)> {
    let first = sample_container_stats_once(docker, name).await?;

    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

    let second = sample_container_stats_once(docker, name).await?;

    Some((first, second))
}

/// Take a single `one_shot` stats sample from a container. Returns `None`
/// on any error or if Docker returns no frames (container missing /
/// stopped). Note Docker zeroes `precpu_stats` on one_shot responses, so a
/// lone sample cannot yield a CPU percent — callers must diff two samples
/// (`sample_container_stats_twice`, or a poller holding its own baseline).
async fn sample_container_stats_once(
    docker: &bollard::Docker,
    name: &str,
) -> Option<bollard::models::ContainerStatsResponse> {
    use futures::StreamExt;

    let opts = bollard::query_parameters::StatsOptionsBuilder::default()
        .stream(false)
        .one_shot(true)
        .build();

    let mut stream = docker.stats(name, Some(opts));
    stream.next().await?.ok()
}

/// Compute the docker-CLI-equivalent CPU percent from two consecutive
/// stats samples. Returns `None` when either sample is missing the
/// counters we need, the deltas are zero/negative (container just
/// started / stopped), or the result isn't finite.
///
/// Formula (matches `docker stats`):
/// ```text
/// cpu_delta    = current.total_usage     - previous.total_usage
/// system_delta = current.system_cpu_usage - previous.system_cpu_usage
/// percent      = (cpu_delta / system_delta) * online_cpus * 100
/// ```
fn cpu_percent_from_delta(
    current: &bollard::models::ContainerCpuStats,
    previous: &bollard::models::ContainerCpuStats,
) -> Option<f64> {
    let cur_total = current.cpu_usage.as_ref()?.total_usage? as i128;
    let prev_total = previous.cpu_usage.as_ref()?.total_usage? as i128;
    let cur_system = current.system_cpu_usage? as i128;
    let prev_system = previous.system_cpu_usage? as i128;

    let cpu_delta = cur_total - prev_total;
    let system_delta = cur_system - prev_system;

    // The system delta is the increment of CPU time available across ALL
    // cores; multiplying by online_cpus rescales the ratio so a fully
    // pinned 4-core container reads as 400%, not 100%.
    let cpus = current.online_cpus.unwrap_or(1).max(1) as f64;

    // `system_delta <= 0` means we have no elapsed wall-clock CPU time to
    // divide against — either Docker returned identical samples (just
    // started, missing counters) or the counter wrapped. Either way we
    // can't compute a meaningful percent.
    if system_delta <= 0 {
        return None;
    }

    // `cpu_delta < 0` indicates a counter reset (container restart between
    // samples). `cpu_delta == 0` is the legitimate idle case — the
    // container did zero CPU work during the sample window. Report 0.0%
    // explicitly rather than `None`, otherwise idle services render as
    // "—" in the UI and look like a sampling bug.
    if cpu_delta < 0 {
        return None;
    }

    let percent = (cpu_delta as f64 / system_delta as f64) * cpus * 100.0;
    if percent.is_finite() && percent >= 0.0 {
        Some(percent)
    } else {
        None
    }
}

/// Subtract page cache from raw memory usage so the number matches
/// `docker stats`'s "MEM USAGE" column.
///
/// Docker reports `usage` straight from cgroups, which includes page
/// cache. A Postgres container with an 8 GB working set + 8 GB of file
/// cache reads back as `usage == limit` on a 16 GB cap, even though only
/// half is real RSS. The Docker CLI compensates by subtracting:
/// - cgroup v1: `stats.cache`
/// - cgroup v2: `stats.inactive_file`
///
/// We try cgroup v2 first (modern hosts), fall back to v1. If neither
/// key is present, return the raw usage unchanged — better to slightly
/// over-report than to crash on a missing field.
fn memory_usage_excluding_cache(mem: &bollard::models::ContainerMemoryStats) -> Option<u64> {
    let raw_usage = mem.usage?;
    let cache = mem.stats.as_ref().and_then(|s| {
        // cgroup v2 uses `inactive_file`; older v1 hosts use `cache`.
        // Prefer v2; fall back to v1. Some hosts report both, in which
        // case `inactive_file` is the better signal (matches docker
        // CLI exactly).
        s.get("inactive_file").or_else(|| s.get("cache")).copied()
    });
    match cache {
        Some(c) if c <= raw_usage => Some(raw_usage - c),
        _ => Some(raw_usage),
    }
}

/// Project two consecutive stats responses onto `ContainerStatsSample`.
/// `previous` is `None` when only a single sample is available — in that
/// case CPU is reported as `None` since the delta formula needs two
/// samples; memory is still computed from the latest sample.
fn compute_stats_sample(
    role: String,
    container_name: String,
    current: &bollard::models::ContainerStatsResponse,
    previous: Option<&bollard::models::ContainerStatsResponse>,
) -> ContainerStatsSample {
    let cur_cpu = current.cpu_stats.as_ref();
    let online_cpus = cur_cpu.and_then(|c| c.online_cpus);

    let cpu_percent = match (cur_cpu, previous.and_then(|p| p.cpu_stats.as_ref())) {
        (Some(c), Some(p)) => cpu_percent_from_delta(c, p),
        _ => None,
    };

    let mem_stats = current.memory_stats.as_ref();
    let memory_usage_bytes = mem_stats.and_then(memory_usage_excluding_cache);
    let memory_limit_bytes = mem_stats.and_then(|m| m.limit);
    let memory_percent = match (memory_usage_bytes, memory_limit_bytes) {
        (Some(usage), Some(limit)) if limit > 0 => Some((usage as f64 / limit as f64) * 100.0),
        _ => None,
    };

    ContainerStatsSample {
        role,
        container_name,
        cpu_percent,
        memory_usage_bytes,
        memory_limit_bytes,
        memory_percent,
        online_cpus,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::test_support::*;
    use std::sync::Arc;
    use temps_entities::external_services;

    #[tokio::test]
    async fn set_log_ingestion_rejects_out_of_range_before_touching_the_db() {
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
        );
        let Some(manager) = mock_manager(db.clone()) else {
            println!("Docker client configuration unavailable, skipping");
            return;
        };
        let err = manager
            .set_log_ingestion(
                7,
                ServiceLogIngestion {
                    log_ingest_lines_per_sec: Some(-5),
                    log_ingest_kib_per_sec: None,
                },
            )
            .await
            .expect_err("negative budget must be rejected");
        assert!(
            matches!(&err, ExternalServiceError::ParameterValidationFailed { service_id: 7, reason } if reason.contains("log_ingest_lines_per_sec -5")),
            "{err:?}"
        );
        drop(manager);
        let db = Arc::try_unwrap(db).expect("manager released the database");
        assert!(
            db.into_transaction_log().is_empty(),
            "nothing may be queried"
        );
    }

    #[tokio::test]
    async fn set_log_ingestion_reports_missing_service_by_id() {
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
                .append_exec_results([lock_timeout_set()])
                .append_query_results([Vec::<external_services::Model>::new()])
                .into_connection(),
        );
        let Some(manager) = mock_manager(db) else {
            println!("Docker client configuration unavailable, skipping");
            return;
        };
        let err = manager
            .set_log_ingestion(42, ServiceLogIngestion::default())
            .await
            .expect_err("missing row");
        assert!(
            matches!(err, ExternalServiceError::ServiceNotFound { id: 42 }),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn set_log_ingestion_returns_previous_and_new_values() {
        let mut before = encrypted_service_model(7, serde_json::json!({}));
        before.log_ingest_lines_per_sec = Some(100);
        let mut after = before.clone();
        after.log_ingest_lines_per_sec = None;
        after.log_ingest_kib_per_sec = Some(0);
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
                .append_exec_results([lock_timeout_set()])
                .append_query_results([vec![before]])
                .append_query_results([vec![after]])
                .into_connection(),
        );
        let Some(manager) = mock_manager(db) else {
            println!("Docker client configuration unavailable, skipping");
            return;
        };
        let wanted = ServiceLogIngestion {
            log_ingest_lines_per_sec: None,
            log_ingest_kib_per_sec: Some(0),
        };
        let update = manager.set_log_ingestion(7, wanted).await.expect("saved");
        assert_eq!(update.previous.log_ingest_lines_per_sec, Some(100));
        assert_eq!(update.current, wanted);
        assert_eq!(update.service_name, "postgres-test");
    }

    /// The mock result for `begin_bounded_transaction`'s `SET LOCAL`.
    fn lock_timeout_set() -> sea_orm::MockExecResult {
        sea_orm::MockExecResult {
            last_insert_id: 0,
            rows_affected: 0,
        }
    }

    /// A save that cannot get the service row's lock gives up after
    /// `SERVICE_LOCK_TIMEOUT` with a typed "service busy" error naming the
    /// service, instead of waiting on the row forever.
    #[tokio::test]
    async fn set_log_ingestion_gives_up_on_a_held_row_lock() {
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
                .append_exec_results([lock_timeout_set()])
                .append_query_errors([sea_orm::DbErr::Custom(
                    "canceling statement due to lock timeout".to_owned(),
                )])
                .into_connection(),
        );
        let Some(manager) = mock_manager(db.clone()) else {
            println!("Docker client configuration unavailable, skipping");
            return;
        };
        let err = manager
            .set_log_ingestion(7, ServiceLogIngestion::default())
            .await
            .expect_err("a held lock must not be waited on forever");
        assert!(
            matches!(
                err,
                ExternalServiceError::LockTimeout {
                    service_id: 7,
                    operation: "saving the log ingestion budget of",
                    ..
                }
            ),
            "{err:?}"
        );

        drop(manager);
        let db = Arc::try_unwrap(db).expect("manager released the database");
        let log = format!("{:?}", db.into_transaction_log());
        let bounded = log
            .find("SET LOCAL lock_timeout")
            .expect("the lock wait must be bounded");
        let locked = log.find("FOR UPDATE").expect("the row is locked");
        assert!(bounded < locked, "the bound must precede the lock: {log}");
    }

    fn cpu_stats_at(
        total: u64,
        system: u64,
        online_cpus: u32,
    ) -> bollard::models::ContainerCpuStats {
        bollard::models::ContainerCpuStats {
            cpu_usage: Some(bollard::models::ContainerCpuUsage {
                total_usage: Some(total),
                ..Default::default()
            }),
            system_cpu_usage: Some(system),
            online_cpus: Some(online_cpus),
            ..Default::default()
        }
    }

    /// 50% on a 2-CPU host: cpu_delta = 1e9 (1 second of CPU time at the
    /// nanosecond resolution Docker reports), system_delta = 4e9 (the
    /// host's "system CPU" counter advances at `wall_ticks * cpus`).
    /// (cpu_delta / system_delta) * online_cpus = 0.25 * 2 = 0.5 = 50%.
    /// Matches docker stats output for a container at half utilization
    /// on 2 cores.
    #[test]
    fn cpu_percent_delta_50pct_two_cpus() {
        let prev = cpu_stats_at(0, 0, 2);
        let curr = cpu_stats_at(1_000_000_000, 4_000_000_000, 2);
        let pct = cpu_percent_from_delta(&curr, &prev).unwrap();
        assert!((pct - 50.0).abs() < 0.01, "expected ~50%, got {pct}");
    }

    /// A container fully saturating both of its 2 CPUs reads as 200%
    /// (matches docker stats display for multi-core saturation).
    #[test]
    fn cpu_percent_delta_fully_pinned_two_cpus_reads_200pct() {
        let prev = cpu_stats_at(0, 0, 2);
        // cpu_delta == system_delta means the container used every
        // available CPU-second the system gave it across all cores.
        let curr = cpu_stats_at(2_000_000_000, 2_000_000_000, 2);
        let pct = cpu_percent_from_delta(&curr, &prev).unwrap();
        assert!((pct - 200.0).abs() < 0.01, "expected ~200%, got {pct}");
    }

    /// Zero/negative deltas (container just started, stopped, or clock
    /// went backwards) must report `None` instead of NaN/Inf/negative.
    /// The pre-fix code returned a misleading "cumulative since boot"
    /// ratio here — usually ~0% for any long-running container.
    #[test]
    fn cpu_percent_delta_zero_returns_none() {
        let prev = cpu_stats_at(5_000_000_000, 10_000_000_000, 4);
        let same = cpu_stats_at(5_000_000_000, 10_000_000_000, 4);
        assert!(cpu_percent_from_delta(&same, &prev).is_none());
    }

    /// Idle running container: zero cpu_delta but positive system_delta
    /// (the host's wall clock kept ticking, the container did no work).
    /// Must report 0.0% — returning None here is what caused idle
    /// services to render as "—" in the UI instead of "0.0%".
    #[test]
    fn cpu_percent_delta_idle_container_reads_zero() {
        let prev = cpu_stats_at(5_000_000_000, 10_000_000_000, 4);
        // Same cpu_total, advanced system_total by 4s on 4 cores.
        let curr = cpu_stats_at(5_000_000_000, 14_000_000_000, 4);
        let pct = cpu_percent_from_delta(&curr, &prev).unwrap();
        assert_eq!(pct, 0.0, "idle container must read 0.0%, got {pct}");
    }

    /// A backwards cpu_delta (counter reset / container restart between
    /// samples) is still `None` — we can't compute a meaningful percent
    /// across a restart.
    #[test]
    fn cpu_percent_delta_negative_cpu_returns_none() {
        let prev = cpu_stats_at(10_000_000_000, 50_000_000_000, 4);
        let curr = cpu_stats_at(1_000_000_000, 54_000_000_000, 4);
        assert!(cpu_percent_from_delta(&curr, &prev).is_none());
    }

    #[test]
    fn cpu_percent_delta_missing_counters_returns_none() {
        let prev = bollard::models::ContainerCpuStats {
            cpu_usage: None,
            system_cpu_usage: Some(0),
            online_cpus: Some(1),
            ..Default::default()
        };
        let curr = cpu_stats_at(1_000_000_000, 1_000_000_000, 1);
        assert!(cpu_percent_from_delta(&curr, &prev).is_none());
    }

    fn mem_stats(
        usage: u64,
        limit: u64,
        cache: Option<(&'static str, u64)>,
    ) -> bollard::models::ContainerMemoryStats {
        let mut stats_map = std::collections::HashMap::new();
        if let Some((key, val)) = cache {
            stats_map.insert(key.to_string(), val);
        }
        bollard::models::ContainerMemoryStats {
            usage: Some(usage),
            limit: Some(limit),
            stats: if cache.is_some() {
                Some(stats_map)
            } else {
                None
            },
            ..Default::default()
        }
    }

    /// cgroup v2 `inactive_file` is preferred over the v1 `cache` key.
    /// A Postgres container with 8 GB working set + 8 GB page cache on a
    /// 16 GB limit must read as 8 GB usage (matching docker stats), not
    /// 16 GB / 16 GB which was the pre-fix bug.
    #[test]
    fn memory_usage_subtracts_inactive_file_cgroup_v2() {
        let mem = mem_stats(
            16 * 1024 * 1024 * 1024, // 16 GB raw usage
            16 * 1024 * 1024 * 1024, // 16 GB limit
            Some(("inactive_file", 8 * 1024 * 1024 * 1024)),
        );
        let usage = memory_usage_excluding_cache(&mem).unwrap();
        assert_eq!(usage, 8 * 1024 * 1024 * 1024);
    }

    /// cgroup v1 hosts surface the cache as `cache`. Subtract it.
    #[test]
    fn memory_usage_subtracts_cache_cgroup_v1() {
        let mem = mem_stats(
            10 * 1024 * 1024 * 1024,
            16 * 1024 * 1024 * 1024,
            Some(("cache", 3 * 1024 * 1024 * 1024)),
        );
        let usage = memory_usage_excluding_cache(&mem).unwrap();
        assert_eq!(usage, 7 * 1024 * 1024 * 1024);
    }

    /// When both keys are present (some hosts report both), prefer
    /// `inactive_file` — that's what the Docker CLI does and it's the
    /// more accurate signal on cgroup v2.
    #[test]
    fn memory_usage_prefers_inactive_file_over_cache_when_both_present() {
        let mut stats_map = std::collections::HashMap::new();
        stats_map.insert("inactive_file".to_string(), 4 * 1024 * 1024 * 1024);
        stats_map.insert("cache".to_string(), 6 * 1024 * 1024 * 1024);
        let mem = bollard::models::ContainerMemoryStats {
            usage: Some(10 * 1024 * 1024 * 1024),
            limit: Some(16 * 1024 * 1024 * 1024),
            stats: Some(stats_map),
            ..Default::default()
        };
        // 10 GB - 4 GB inactive_file = 6 GB. If the helper preferred
        // `cache` we'd see 4 GB.
        let expected: u64 = 6 * 1024 * 1024 * 1024;
        assert_eq!(memory_usage_excluding_cache(&mem).unwrap(), expected);
    }

    /// Without cache info, return raw usage rather than crashing.
    #[test]
    fn memory_usage_returns_raw_when_no_cache_info() {
        let mem = mem_stats(5 * 1024 * 1024 * 1024, 16 * 1024 * 1024 * 1024, None);
        assert_eq!(
            memory_usage_excluding_cache(&mem).unwrap(),
            5u64 * 1024 * 1024 * 1024
        );
    }

    /// Defensive: if `cache` is somehow larger than `usage` (sentinel
    /// values, stat skew), don't underflow — return raw usage.
    #[test]
    fn memory_usage_handles_cache_larger_than_usage() {
        let mem = mem_stats(
            1024 * 1024,
            16 * 1024 * 1024 * 1024,
            Some(("cache", 10 * 1024 * 1024 * 1024)),
        );
        // cache > usage → fall through to raw usage rather than wrap.
        assert_eq!(memory_usage_excluding_cache(&mem).unwrap(), 1024 * 1024);
    }

    fn stats_response_at(
        total: u64,
        system: u64,
        online_cpus: u32,
    ) -> bollard::models::ContainerStatsResponse {
        bollard::models::ContainerStatsResponse {
            cpu_stats: Some(bollard::models::ContainerCpuStats {
                cpu_usage: Some(bollard::models::ContainerCpuUsage {
                    total_usage: Some(total),
                    ..Default::default()
                }),
                system_cpu_usage: Some(system),
                online_cpus: Some(online_cpus),
                ..Default::default()
            }),
            memory_stats: Some(mem_stats(
                100 * 1024 * 1024,
                1024 * 1024 * 1024,
                Some(("inactive_file", 10 * 1024 * 1024)),
            )),
            ..Default::default()
        }
    }

    /// Regression: `compute_stats_sample(current=later, previous=earlier)` is
    /// the correct argument order. The earlier production bug had the call
    /// site swapped, which made `cpu_delta` negative and read back as `None`
    /// — UI showed "—" for CPU while memory still rendered (it doesn't need
    /// the delta).
    #[test]
    fn compute_stats_sample_correct_argument_order_reports_positive_cpu() {
        let earlier = stats_response_at(0, 0, 2);
        let later = stats_response_at(1_000_000_000, 4_000_000_000, 2);

        let sample = compute_stats_sample("primary".into(), "test".into(), &later, Some(&earlier));
        assert_eq!(sample.cpu_percent, Some(50.0));
        assert_eq!(sample.online_cpus, Some(2));
    }

    /// Reversed args (the pre-fix bug shape) produce `None`, not garbage.
    /// Documenting this so the kill-switch is obvious if someone reintroduces
    /// the swap.
    #[test]
    fn compute_stats_sample_swapped_arguments_reads_none() {
        let earlier = stats_response_at(0, 0, 2);
        let later = stats_response_at(1_000_000_000, 4_000_000_000, 2);

        let sample = compute_stats_sample("primary".into(), "test".into(), &earlier, Some(&later));
        assert_eq!(sample.cpu_percent, None);
    }

    // ── End container stats helpers ──────────────────────────────────────────
}
