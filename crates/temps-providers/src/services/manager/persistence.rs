// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Service row persistence, name and container reservation, and data-
//! preservation guards.

use crate::externalsvc::{ExternalService, ServiceConfig, ServiceType};
use crate::services::*;
use chrono::Utc;
use sea_orm::{
    sea_query::Expr, ActiveModelTrait, ColumnTrait, Condition, EntityTrait, QueryFilter,
    QuerySelect, Set,
};
use std::collections::HashMap;
use temps_entities::{external_services, service_members};

impl ExternalServiceManager {
    /// Refuse a change that recreates `service`'s local container with
    /// `parameters` when the new container would not see the data the
    /// current one uses. Read-only; a no-op when there is no container yet,
    /// or the service is remote or a cluster (whose paths guard themselves).
    pub(super) async fn ensure_recreate_keeps_data(
        &self,
        service: &external_services::Model,
        parameters: &HashMap<String, serde_json::Value>,
        operation: &str,
    ) -> Result<(), ExternalServiceError> {
        if service.node_id.is_some() || service.topology == "cluster" {
            return Ok(());
        }
        let service_type = ServiceType::from_str(&service.service_type).map_err(|_| {
            ExternalServiceError::InvalidServiceType {
                id: service.id,
                service_type: service.service_type.clone(),
            }
        })?;
        let instance = self.create_service_instance_for_parameters(
            service.name.clone(),
            service_type,
            parameters,
        )?;
        let config = ServiceConfig {
            name: service.name.clone(),
            service_type,
            version: service.version.clone(),
            parameters: serde_json::to_value(parameters).map_err(|e| {
                ExternalServiceError::InternalError {
                    reason: format!(
                        "Failed to serialize parameters of service {}: {}",
                        service.id, e
                    ),
                }
            })?,
        };
        // Imported services are never recreated, so there is nothing to check.
        let Some((engine, replacement)) = instance.recreate_plan(&config) else {
            return Ok(());
        };
        let docker = self.require_docker()?;
        crate::externalsvc::recreate_guard::preflight(
            docker.as_ref(),
            engine,
            &service.name,
            &instance.get_docker_container_name(),
            &replacement,
            operation,
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
                ExternalServiceError::DockerError {
                    id: service.id,
                    reason: e.to_string(),
                }
            }
        })?;
        Ok(())
    }

    /// Refuse to rename a local service that already has Docker resources
    /// under its current name: its data volume cannot follow the rename, so
    /// the renamed service would come up empty (#1415 class).
    pub(super) async fn ensure_rename_keeps_data(
        &self,
        service: &external_services::Model,
        new_name: &str,
    ) -> Result<(), ExternalServiceError> {
        if service.node_id.is_some() {
            // Its resources are on a worker this process cannot inspect, and
            // they are named after the service just the same.
            return Err(ExternalServiceError::DataSafetyRefusal {
                service_id: service.id,
                name: service.name.clone(),
                reason: format!(
                    "renaming it to '{new_name}' would start it on new, empty storage on its \
                     worker node: its container and volume are named after the current name. \
                     Nothing was changed. Keep the current name, or back the service up and \
                     restore it into a new service with the name you want"
                ),
            });
        }
        let parameters = self.get_service_parameters(service.id).await?;
        let service_type = ServiceType::from_str(&service.service_type).map_err(|_| {
            ExternalServiceError::InvalidServiceType {
                id: service.id,
                service_type: service.service_type.clone(),
            }
        })?;
        let instance = self.create_service_instance_for_parameters(
            service.name.clone(),
            service_type,
            &parameters,
        )?;
        let refuse = |resource: String| ExternalServiceError::DataSafetyRefusal {
            service_id: service.id,
            name: service.name.clone(),
            reason: format!(
                "renaming it to '{new_name}' would start it on new, empty storage: its data is in \
                 Docker resource '{resource}', which is named after the current service name and \
                 cannot be renamed. Nothing was changed. Keep the current name, or back the \
                 service up and restore it into a new service with the name you want"
            ),
        };
        let Some(names) = instance.docker_resource_names() else {
            return Err(refuse(instance.get_docker_container_name()));
        };
        let docker = self.require_docker()?;
        for volume in names.volumes {
            match docker.inspect_volume(&volume).await {
                Ok(_) => return Err(refuse(volume)),
                Err(bollard::errors::Error::DockerResponseServerError {
                    status_code: 404, ..
                }) => {}
                Err(e) => {
                    return Err(ExternalServiceError::DockerError {
                        id: service.id,
                        reason: format!(
                            "Failed to check volume '{}' before renaming service {}: {}",
                            volume, service.id, e
                        ),
                    })
                }
            }
        }
        for container in names.containers {
            match docker
                .inspect_container(
                    &container,
                    None::<bollard::query_parameters::InspectContainerOptions>,
                )
                .await
            {
                Ok(_) => return Err(refuse(container)),
                Err(bollard::errors::Error::DockerResponseServerError {
                    status_code: 404, ..
                }) => {}
                Err(e) => {
                    return Err(ExternalServiceError::DockerError {
                        id: service.id,
                        reason: format!(
                            "Failed to check container '{}' before renaming service {}: {}",
                            container, service.id, e
                        ),
                    })
                }
            }
        }
        Ok(())
    }

    pub(super) async fn ensure_new_local_service_resources_unused(
        &self,
        service: &external_services::Model,
    ) -> Result<(), ExternalServiceError> {
        if service.node_id.is_some() {
            return Ok(());
        }
        let parameters = self.get_service_parameters(service.id).await?;
        let service_type = ServiceType::from_str(&service.service_type).map_err(|_| {
            ExternalServiceError::InvalidServiceType {
                id: service.id,
                service_type: service.service_type.clone(),
            }
        })?;
        let instance = self.create_service_instance_for_parameters(
            service.name.clone(),
            service_type,
            &parameters,
        )?;
        let names = instance.docker_resource_names().ok_or_else(|| {
            ExternalServiceError::InternalError {
                reason: format!("Cannot safely create service '{}': its engine does not report owned Docker resources", service.name),
            }
        })?;
        let docker = self.require_docker()?;
        for container in names.containers {
            match docker
                .inspect_container(
                    &container,
                    None::<bollard::query_parameters::InspectContainerOptions>,
                )
                .await
            {
                Ok(_) => {
                    return Err(ExternalServiceError::ServiceResourceConflict {
                        name: service.name.clone(),
                        resource: container,
                    })
                }
                Err(bollard::errors::Error::DockerResponseServerError {
                    status_code: 404, ..
                }) => {}
                Err(error) => {
                    return Err(ExternalServiceError::DockerError {
                        id: service.id,
                        reason: format!(
                            "Failed to inspect new service '{}' container '{container}': {error}",
                            service.name
                        ),
                    })
                }
            }
        }
        // A stale volume must not become a new service's data by name alone.
        for volume in names.volumes {
            match docker.inspect_volume(&volume).await {
                Ok(_) => {
                    return Err(ExternalServiceError::ServiceResourceConflict {
                        name: service.name.clone(),
                        resource: volume,
                    })
                }
                Err(bollard::errors::Error::DockerResponseServerError {
                    status_code: 404, ..
                }) => {}
                Err(error) => {
                    return Err(ExternalServiceError::DockerError {
                        id: service.id,
                        reason: format!(
                            "Failed to inspect new service '{}' volume '{volume}': {error}",
                            service.name
                        ),
                    })
                }
            }
        }
        Ok(())
    }

    pub(super) async fn reserve_imported_container(
        &self,
        txn: &sea_orm::DatabaseTransaction,
        container_id: &str,
        container_name: &str,
    ) -> Result<(), ExternalServiceError> {
        use sea_orm::ConnectionTrait;
        txn.execute(sea_orm::Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT pg_advisory_xact_lock(hashtextextended('external-service-container:' || $1, 0))",
            [container_id.into()],
        ))
        .await?;
        let canonical_owner = Expr::cust_with_values(
            "CASE service_type WHEN 'postgres' THEN 'postgres-' WHEN 'redis' THEN 'redis-' WHEN 'kv' THEN 'redis-' WHEN 'mariadb' THEN 'mariadb-' WHEN 'mongodb' THEN 'temps-mongodb-' WHEN 'rustfs' THEN 'rustfs-' WHEN 'blob' THEN 'rustfs-' WHEN 's3' THEN 'rustfs-' WHEN 'minio' THEN 'minio-' ELSE NULL END || name = $1",
            [container_name],
        );
        let minio_owner = Expr::cust_with_values(
            "service_type = 's3' AND 'minio-' || name = $1",
            [container_name],
        );
        // Only the two built-in services have historical aliases. Arbitrary
        // user names must not claim another service's prefixed workload.
        let legacy_builtin_owner = Expr::cust_with_values(
            "(service_type = 'blob' AND name = 'temps-blob' AND $1 = 'rustfs-blob-temps-blob') OR (service_type IN ('kv', 'redis') AND name = 'temps-kv' AND $1 = 'redis-kv-temps-kv')",
            [container_name],
        );
        let registered_owners = external_services::Entity::find()
            .filter(external_services::Column::NodeId.is_null())
            .filter(
                Condition::any()
                    .add(external_services::Column::ContainerName.eq(container_name))
                    .add(canonical_owner)
                    .add(minio_owner)
                    .add(legacy_builtin_owner),
            );
        if let Some(existing) = registered_owners.clone().one(txn).await? {
            return Err(ExternalServiceError::ServiceContainerConflict {
                container: container_name.to_string(),
                existing_service_id: existing.id,
            });
        }
        // A clone's active run owns its canonical container before its
        // service row is registered. An alias import must not adopt it in
        // that interval. Join its template in one bounded ownership query.
        let restore = txn.query_one(sea_orm::Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT r.id FROM restore_runs r JOIN external_services s ON s.id = r.source_service_id WHERE r.status IN ('pending', 'running') AND r.target_service_name IS NOT NULL AND ((CASE s.service_type WHEN 'postgres' THEN 'postgres-' WHEN 'redis' THEN 'redis-' WHEN 'kv' THEN 'redis-' WHEN 'mariadb' THEN 'mariadb-' WHEN 'mongodb' THEN 'temps-mongodb-' WHEN 'rustfs' THEN 'rustfs-' WHEN 'blob' THEN 'rustfs-' WHEN 's3' THEN 'rustfs-' WHEN 'minio' THEN 'minio-' ELSE NULL END || r.target_service_name) = $1 OR (s.service_type = 's3' AND 'minio-' || r.target_service_name = $1)) LIMIT 1",
            [container_name.into()],
        )).await?;
        if let Some(restore) = restore {
            return Err(ExternalServiceError::ServiceContainerRestoreConflict {
                container: container_name.to_string(),
                restore_run_id: restore.try_get("", "id")?,
            });
        }
        // Registration commits its service row before the run becomes
        // terminal. Recheck after the active-run snapshot to cover that
        // handoff when a differently named import uses a different name lock.
        if let Some(existing) = registered_owners.one(txn).await? {
            return Err(ExternalServiceError::ServiceContainerConflict {
                container: container_name.to_string(),
                existing_service_id: existing.id,
            });
        }
        // Imports made before the plaintext ownership column existed may
        // retain their actual container name only in encrypted parameters.
        // Stream this compatibility check; do not load all service configs.
        {
            use futures::TryStreamExt;
            let mut legacy = external_services::Entity::find()
                .filter(external_services::Column::NodeId.is_null())
                .filter(external_services::Column::ContainerName.is_null())
                .filter(external_services::Column::Config.is_not_null())
                .stream(txn)
                .await?;
            while let Some(service) = legacy.try_next().await? {
                let config = service.config.as_deref().unwrap_or_default();
                let plaintext =
                    self.encryption_service
                        .decrypt_string(config)
                        .map_err(|error| ExternalServiceError::DecryptionFailed {
                            service_id: service.id,
                            param_name: "config".to_string(),
                            reason: format!("Cannot verify existing container ownership: {error}"),
                        })?;
                let parameters: serde_json::Value =
                    serde_json::from_str(&plaintext).map_err(|error| {
                        ExternalServiceError::InternalError {
                            reason: format!(
                                "Cannot verify container ownership for service {}: {error}",
                                service.id
                            ),
                        }
                    })?;
                if parameters
                    .get("container_name")
                    .and_then(|value| value.as_str())
                    .is_some_and(|name| {
                        name.trim_start_matches('/') == container_name || name == container_id
                    })
                {
                    return Err(ExternalServiceError::ServiceContainerConflict {
                        container: container_name.to_string(),
                        existing_service_id: service.id,
                    });
                }
            }
        }
        if let Some(member) = service_members::Entity::find()
            .filter(service_members::Column::NodeId.is_null())
            .filter(service_members::Column::ContainerName.eq(container_name))
            .one(txn)
            .await?
        {
            return Err(ExternalServiceError::ServiceContainerConflict {
                container: container_name.to_string(),
                existing_service_id: member.service_id,
            });
        }
        Ok(())
    }

    pub(super) async fn ensure_unambiguous_service_name(
        &self,
        service: &external_services::Model,
    ) -> Result<(), ExternalServiceError> {
        if let Some(existing) = external_services::Entity::find()
            .filter(external_services::Column::Name.eq(&service.name))
            .filter(external_services::Column::Id.ne(service.id))
            .one(self.db.as_ref())
            .await?
        {
            return Err(ExternalServiceError::AmbiguousServiceName {
                service_id: service.id,
                name: service.name.clone(),
                existing_service_id: existing.id,
            });
        }
        Ok(())
    }

    /// Load a cluster's parameters, generating and persisting its SCRAM
    /// secrets (see `ClusterAuthSecrets`) if it has none yet.
    ///
    /// Returns the (decrypted) parameters including the secrets, the secrets
    /// themselves, and whether they were generated by this call — which means
    /// the cluster predates SCRAM auth and its running members still have to
    /// be upgraded with [`Self::upgrade_cluster_auth`].
    /// Preserve infrastructure credentials when saving a stale configuration.
    /// Every existing-service config writer uses the same row lock as generation.
    pub(super) async fn persist_service_config(
        &self,
        service_id: i32,
        active: external_services::ActiveModel,
    ) -> Result<external_services::Model, ExternalServiceError> {
        let txn = begin_bounded_transaction(
            &self.db,
            service_id,
            "saving the configuration of",
            SERVICE_LOCK_TIMEOUT,
        )
        .await?;
        let updated = match self
            .persist_service_config_in_transaction(service_id, active, &txn)
            .await
        {
            Ok(updated) => updated,
            Err(error) => {
                // Roll back now: leaving it to Drop keeps the row lock and
                // the name's advisory lock until the connection is recycled.
                let error = lock_timeout_or_database_error(
                    service_id,
                    "saving the configuration of",
                    SERVICE_LOCK_TIMEOUT,
                    error,
                );
                return Err(rollback_before_error(txn, error).await);
            }
        };
        txn.commit().await?;
        Ok(updated)
    }

    /// Refuse a service name that is already taken or reserved, without
    /// keeping any lock: the check runs in a short transaction that is rolled
    /// back before this returns, so callers can go on to Docker work.
    pub(super) async fn ensure_service_name_available(
        &self,
        service_id: i32,
        name: &str,
    ) -> Result<(), ExternalServiceError> {
        let txn = begin_bounded_transaction(
            &self.db,
            service_id,
            "checking the new name of",
            SERVICE_LOCK_TIMEOUT,
        )
        .await?;
        if let Err(error) = reserve_service_name(&txn, name, Some(service_id)).await {
            let error = lock_timeout_or_database_error(
                service_id,
                "checking the new name of",
                SERVICE_LOCK_TIMEOUT,
                error,
            );
            return Err(rollback_before_error(txn, error).await);
        }
        txn.rollback().await?;
        Ok(())
    }

    async fn persist_service_config_in_transaction(
        &self,
        service_id: i32,
        mut active: external_services::ActiveModel,
        txn: &sea_orm::DatabaseTransaction,
    ) -> Result<external_services::Model, ExternalServiceError> {
        if let sea_orm::ActiveValue::Set(name) = &active.name {
            reserve_service_name(txn, name, Some(service_id)).await?;
        }
        let current = external_services::Entity::find_by_id(service_id)
            .lock_exclusive()
            .one(txn)
            .await?
            .ok_or(ExternalServiceError::ServiceNotFound { id: service_id })?;
        let parse =
            |encrypted: &str| -> Result<HashMap<String, serde_json::Value>, ExternalServiceError> {
                let json = self
                    .encryption_service
                    .decrypt_string(encrypted)
                    .map_err(|e| ExternalServiceError::InternalError {
                        reason: format!("Failed to decrypt service config: {e}"),
                    })?;
                let value: serde_json::Value = serde_json::from_str(&json).map_err(|e| {
                    ExternalServiceError::InternalError {
                        reason: format!(
                            "Failed to parse service config for service {service_id}: {e}"
                        ),
                    }
                })?;
                // Older services can store valid non-object JSON. It has no
                // cluster credentials to preserve and may be replaced safely.
                Ok(value
                    .as_object()
                    .map(|map| map.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                    .unwrap_or_default())
            };
        if let Some(encrypted) = current.config.as_deref() {
            let stored = parse(encrypted)?;
            if let Some(auth) =
                crate::externalsvc::postgres_cluster::ClusterAuthSecrets::from_parameters(&stored)
                    .map_err(|e| ExternalServiceError::ParameterValidationFailed {
                    service_id,
                    reason: e.to_string(),
                })?
            {
                let proposed = match &active.config {
                    sea_orm::ActiveValue::Set(Some(value))
                    | sea_orm::ActiveValue::Unchanged(Some(value)) => value,
                    _ => {
                        return Err(ExternalServiceError::ParameterValidationFailed {
                            service_id,
                            reason: "Cluster credentials cannot be removed".to_string(),
                        })
                    }
                };
                let mut parameters = parse(proposed)?;
                auth.insert_into(&mut parameters);
                if cluster_auth_version(&stored) >= CLUSTER_INFRA_AUTH_VERSION {
                    parameters.insert(
                        CLUSTER_AUTH_VERSION_KEY.to_string(),
                        stored[CLUSTER_AUTH_VERSION_KEY].clone(),
                    );
                }
                let json = serde_json::to_string(&parameters).map_err(|e| {
                    ExternalServiceError::InternalError {
                        reason: e.to_string(),
                    }
                })?;
                active.config = Set(Some(
                    self.encryption_service.encrypt_string(&json).map_err(|e| {
                        ExternalServiceError::InternalError {
                            reason: e.to_string(),
                        }
                    })?,
                ));
            }
        }
        Ok(active.update(txn).await?)
    }

    pub(super) async fn store_inferred_parameters(
        &self,
        service_id: i32,
        _service_instance: &dyn ExternalService,
        inferred_params: HashMap<String, String>,
    ) -> Result<(), ExternalServiceError> {
        // Get current parameters
        let mut current_params = self.get_service_parameters(service_id).await?;

        // Only merge parameters that are truly auto-generated/inferred
        // Skip user-facing parameters like docker_image, host, database, etc.
        for (key, value) in inferred_params {
            if Self::is_inferred_parameter(&key) {
                current_params.insert(key, serde_json::Value::String(value));
            }
        }

        // Serialize updated config to JSON and encrypt
        let config_json = serde_json::to_string(&current_params).map_err(|e| {
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

        // Update service config
        let service = self.get_service(service_id).await?;
        let mut service_update: external_services::ActiveModel = service.into();
        service_update.config = Set(Some(encrypted_config));
        service_update.updated_at = Set(Utc::now());
        self.persist_service_config(service_id, service_update)
            .await?;

        Ok(())
    }

    pub(super) fn is_inferred_parameter(key: &str) -> bool {
        // Only truly inferred/auto-generated parameters should be merged here.
        // User-provided parameters (docker_image, etc.) should NOT be overwritten by inferred values.
        // Inferred parameters are those auto-generated by the init() method:
        // - Actual port mappings/addresses after container creation
        // - Connection strings derived from the deployed service
        // - Auto-generated passwords (when not provided or invalid)
        // - Other runtime-determined values
        matches!(
            key,
            // Only include truly inferred values
            "port"
                | "connection_string"
                | "local_address"
                | "inferred_port"
                | "password"
                | "root_password"
                // Overlay ("temps-overlay") IP reported by the agent for a
                // remote service container. Changes every time the container
                // is recreated, so it must be refreshed like `port` rather
                // than treated as user-provided config.
                | "compute_ip"
                // S3/MinIO server kind (`rustfs`/`minio`) that `init()`
                // resolved from the image metadata for a service created
                // without one; an explicit value comes back unchanged.
                | "server"
                // Garage's admin API port: when the requested one is taken,
                // `provision` starts the container on a replacement, and
                // health checks and bucket operations must follow it.
                | "admin_port"
        )
    }

    // Add this new helper method
    pub(super) fn generate_slug(name: &str) -> String {
        name.to_lowercase()
            .chars()
            .filter_map(|c| {
                if c.is_alphanumeric() {
                    Some(c)
                } else if c.is_whitespace() {
                    Some('-')
                } else {
                    None
                }
            })
            .collect()
    }

    /// Convert HashMap<String, serde_json::Value> to HashMap<String, String>
    pub(super) fn params_to_strings(
        params: &HashMap<String, serde_json::Value>,
    ) -> HashMap<String, String> {
        params
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
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::test_support::*;
    use sea_orm::TransactionTrait;
    use std::sync::Arc;
    use temps_entities::external_services;

    #[tokio::test]
    async fn test_slug_generation() {
        // Test the slug generation logic
        assert_eq!(
            ExternalServiceManager::generate_slug("My Service Name"),
            "my-service-name"
        );
        assert_eq!(
            ExternalServiceManager::generate_slug("Service@#$123"),
            "service123"
        );
        assert_eq!(
            ExternalServiceManager::generate_slug("   Spaces   Everywhere   "),
            "---spaces---everywhere---"
        );
    }

    /// `initialize_plugin_service` runs on every boot, so the write-back it
    /// performs must be able to correct the port without touching anything
    /// the operator configured. `store_inferred_parameters` merges only keys
    /// this predicate accepts — if `docker_image` or the credentials ever
    /// leaked into it, a restart would silently overwrite operator config.
    #[test]
    fn write_back_corrects_the_port_and_leaves_operator_config_alone() {
        assert!(
            ExternalServiceManager::is_inferred_parameter("port"),
            "the port must be written back, or health_probe keeps reading a stale one"
        );
        assert!(
            ExternalServiceManager::is_inferred_parameter("admin_port"),
            "a Garage admin port moved by provisioning must be written back"
        );

        for operator_set in [
            "docker_image",
            "access_key",
            "secret_key",
            "host",
            "region",
            "console_port",
        ] {
            assert!(
                !ExternalServiceManager::is_inferred_parameter(operator_set),
                "'{operator_set}' is operator-facing and must survive a plugin re-init"
            );
        }
    }

    #[tokio::test]
    async fn imported_container_reservation_rejects_existing_owner_before_validation() {
        let existing = encrypted_service_model(17, serde_json::json!({}));
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
                .append_exec_results([sea_orm::MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 1,
                }])
                .append_query_results([vec![existing]])
                .into_connection(),
        );
        let manager = mock_service_manager_with_db(db.clone());
        let txn = db.begin().await.unwrap();
        let error = manager
            .reserve_imported_container(&txn, "immutable-container-id", "postgres-test")
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            ExternalServiceError::ServiceContainerConflict {
                existing_service_id: 17,
                ..
            }
        ));
        txn.rollback().await.unwrap();
        drop(manager);
        let db = Arc::try_unwrap(db).unwrap_or_else(|_| panic!("test database still has owners"));
        let log = db.into_transaction_log();
        let statements = log[0].statements();
        let lock = statements
            .iter()
            .position(|s| s.sql.contains("pg_advisory_xact_lock"))
            .unwrap();
        let owner = statements
            .iter()
            .position(|s| s.sql.contains("SELECT") && s.sql.contains("external_services"))
            .unwrap();
        assert!(lock < owner);
        assert!(statements.iter().all(|s| !s.sql.contains("INSERT")));
    }

    #[tokio::test]
    async fn imported_container_reservation_refuses_an_unregistered_clone() {
        let owner =
            std::collections::BTreeMap::from([("id".to_string(), sea_orm::Value::Int(Some(41)))]);
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
                .append_exec_results([sea_orm::MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 1,
                }])
                .append_query_results([Vec::<external_services::Model>::new()])
                .append_query_results([vec![owner]])
                .into_connection(),
        );
        let manager = mock_service_manager_with_db(db.clone());
        let txn = db.begin().await.unwrap();
        let error = manager
            .reserve_imported_container(&txn, "owned-clone-id", "postgres-copy")
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            ExternalServiceError::ServiceContainerRestoreConflict {
                restore_run_id: 41,
                ..
            }
        ));
        txn.rollback().await.unwrap();
    }

    #[tokio::test]
    async fn imported_container_reservation_rechecks_registration_after_clone_handoff() {
        let owner = encrypted_service_model(23, serde_json::json!({}));
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
                .append_exec_results([sea_orm::MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 1,
                }])
                .append_query_results([Vec::<external_services::Model>::new()])
                .append_query_results([
                    Vec::<std::collections::BTreeMap<String, sea_orm::Value>>::new(),
                ])
                .append_query_results([vec![owner]])
                .into_connection(),
        );
        let manager = mock_service_manager_with_db(db.clone());
        let txn = db.begin().await.unwrap();
        let error = manager
            .reserve_imported_container(&txn, "owned-clone-id", "postgres-copy")
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            ExternalServiceError::ServiceContainerConflict {
                existing_service_id: 23,
                ..
            }
        ));
        txn.rollback().await.unwrap();
    }
}
