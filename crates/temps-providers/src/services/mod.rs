// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::externalsvc::{
    garage::GarageService, managed_instance_name, mariadb::MariaDbService, mongodb::MongodbService,
    postgres::PostgresService, redis::RedisService, rustfs::RustfsService, s3::S3Service,
    ExternalService, ManagedS3BackendKind, ManagedS3BackendSelection, ServiceType,
};
use anyhow::Result;
use bollard::Docker;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};
use std::collections::HashMap;
use std::sync::Arc;
use temps_core::DockerHandle;
use temps_entities::{external_services, nodes};
// use crate::routes::types::external_services::EnvironmentVariableInfo;
use temps_core::EncryptionService;

mod cluster_state;
mod cluster_topology;
mod errors;
mod manager;
mod schema;
#[cfg(test)]
mod test_support;
mod transactions;
mod types;

pub use cluster_state::*;
pub(crate) use cluster_topology::*;
pub use errors::*;
pub use schema::*;
pub use transactions::*;
pub use types::*;

// ADR-055 Phase 4: per-database workload mode, pending changes, recreate
// from the spec and the shadow pass.
mod workload_ops;
pub use workload_ops::{
    RecreateOutcome, WorkloadModeChange, WorkloadModeStatus, SHADOW_BATCH, SHADOW_INTERVAL,
};

/// Local cluster ports are published on Docker's IPv4 loopback interface.
/// Keep control-plane connections on the same address: `localhost` may resolve
/// to IPv6 first on Linux even though Docker is only listening on 127.0.0.1.
pub(crate) const LOCAL_CLUSTER_HOST: &str = "127.0.0.1";

/// The address the control plane dials to reach a cluster member's published
/// port: the worker's data address for a member on a worker, the IPv4
/// loopback for a member on the control plane.
///
/// Never the member's overlay `compute_ip`. The control plane is not on the
/// workload overlay, so that address routes only between workloads; a monitor
/// probe sent there times out, which left every worker-hosted cluster reading
/// as Down with no known primary. Workers publish member ports on their data
/// address, which is what this returns.
pub(crate) async fn control_plane_member_dial_host<C: sea_orm::ConnectionTrait>(
    db: &C,
    node_id: Option<i32>,
    container_name: &str,
) -> Result<String, String> {
    let Some(node_id) = node_id.filter(|id| *id != 0) else {
        return Ok(LOCAL_CLUSTER_HOST.to_string());
    };
    match nodes::Entity::find_by_id(node_id).one(db).await {
        Ok(Some(node)) => Ok(node.data_address().to_string()),
        Ok(None) => Err(format!(
            "node {node_id} of cluster member '{container_name}' was not found in the nodes table"
        )),
        Err(error) => Err(format!(
            "failed to load node {node_id} of cluster member '{container_name}': {error}"
        )),
    }
}

/// Every field is `Arc`-wrapped, so `Clone` is a cheap refcount bump that
/// shares the SAME `reconciler_shutdowns` map with the original -- unlike
/// `ExternalServiceManager::new(...)`, which always allocates a fresh, empty
/// one. Background tasks spawned off a manager method (e.g. cluster
/// initialization) must clone `self` for exactly this reason: constructing a
/// new instance instead orphans any role reconciler that task spawns in a
/// map nobody else can ever reach, so `stop_role_reconciler` (called on the
/// real, shared instance) silently no-ops and the reconciler leaks forever.
#[derive(Clone)]
pub struct ExternalServiceManager {
    db: Arc<DatabaseConnection>,
    encryption_service: Arc<EncryptionService>,
    /// The process-wide Docker handle. May be `Disabled` on a control-plane
    /// profile that deliberately runs no local workloads. Call
    /// [`Self::require_docker`] anywhere an actual client is needed; call
    /// [`Self::local_workloads_enabled`] to gate local-provisioning paths
    /// before touching the handle.
    docker: Arc<DockerHandle>,
    /// Whether this process is allowed to start containers locally.
    /// `false` on `--profile control-plane`, `true` on `--profile full`
    /// (the historical default). This is a *policy* flag — it gates local
    /// provisioning even when a Docker socket happens to be mounted, so the
    /// serve profile is a hard contract and not just a fallback.
    local_workloads_enabled: bool,
    /// Internal DNS registry (ADR-011). Required, not optional — making it
    /// optional led to silent no-ops where one constructor wired it and
    /// another didn't, so cluster members that *should* have DNS records
    /// never got them. The registry is a stateless wrapper over the
    /// shared `DatabaseConnection`, so every constructor can produce one
    /// trivially.
    dns_registry: Arc<temps_dns::DnsRegistry>,
    /// Per-cluster role reconciler shutdown handles, keyed by service_id.
    /// `delete_service` calls `ReconcilerShutdown::signal` and the task
    /// observes it — either on its next `select!` wakeup, or (if the
    /// signal lands mid-tick) on its very next loop-top check; see
    /// `ReconcilerShutdown`'s doc comment. Held inside a tokio mutex
    /// because the reconciler-spawn path is async and we want a Send
    /// MutexGuard across awaits.
    reconciler_shutdowns: Arc<
        tokio::sync::Mutex<
            HashMap<i32, Arc<crate::externalsvc::postgres_role_reconciler::ReconcilerShutdown>>,
        >,
    >,
    /// The control plane's cluster resolver address (ADR-024), published by
    /// the deployer's probe only while cluster DNS is enabled and the
    /// resolver answers. Empty otherwise, and on processes that run no
    /// resolver: local cluster members then keep Docker's embedded DNS.
    cluster_dns_slot: temps_dns::OverlayDnsSlot,
}

impl ExternalServiceManager {
    /// Construct with all required dependencies. The `DnsRegistry` is
    /// required (not optional) so cluster lifecycle hooks always have a
    /// place to write A records — the historical `Option<DnsRegistry>` +
    /// `with_dns_registry` setter caused silent no-ops when one
    /// constructor wired the registry and another didn't.
    ///
    /// Callers that don't have a `DnsRegistry` in scope can build one
    /// trivially: `Arc::new(temps_dns::DnsRegistry::new(db.clone()))`.
    /// The registry is a stateless wrapper over the same `db` handle.
    ///
    /// This constructor wraps `docker` into a [`DockerHandle::Available`]
    /// and sets `local_workloads_enabled = true` (the historical behaviour
    /// of a full-profile process that owns a Docker daemon). Call
    /// [`Self::new_with_handle`] when the handle and policy come from the
    /// serve bootstrap rather than a direct socket.
    pub fn new(
        db: Arc<DatabaseConnection>,
        encryption_service: Arc<EncryptionService>,
        docker: Arc<Docker>,
        dns_registry: Arc<temps_dns::DnsRegistry>,
    ) -> Self {
        Self {
            db,
            encryption_service,
            docker: Arc::new(DockerHandle::available(docker)),
            local_workloads_enabled: true,
            dns_registry,
            reconciler_shutdowns: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            cluster_dns_slot: Arc::new(std::sync::RwLock::new(None)),
        }
    }

    /// Construct with a pre-built [`DockerHandle`] and an explicit
    /// `local_workloads_enabled` policy flag. Used by the serve bootstrap
    /// when the profile is known at startup time.
    pub fn new_with_handle(
        db: Arc<DatabaseConnection>,
        encryption_service: Arc<EncryptionService>,
        docker: Arc<DockerHandle>,
        local_workloads_enabled: bool,
        dns_registry: Arc<temps_dns::DnsRegistry>,
    ) -> Self {
        Self {
            db,
            encryption_service,
            docker,
            local_workloads_enabled,
            dns_registry,
            reconciler_shutdowns: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            cluster_dns_slot: Arc::new(std::sync::RwLock::new(None)),
        }
    }

    /// Follow the control plane's cluster resolver slot, so local cluster
    /// members resolve `*.temps.local` while the resolver is up.
    pub fn with_cluster_dns_slot(mut self, slot: temps_dns::OverlayDnsSlot) -> Self {
        self.cluster_dns_slot = slot;
        self
    }

    /// Nameservers for a container this process creates: the cluster
    /// resolver followed by the host's own servers, or `None` (Docker's
    /// embedded DNS) while the resolver is not published.
    fn cluster_dns_servers(&self) -> Option<Vec<String>> {
        let resolver = match self.cluster_dns_slot.read() {
            Ok(slot) => *slot,
            Err(poisoned) => *poisoned.into_inner(),
        }?;
        Some(temps_node_dns::host_dns::dns_with_fallback(vec![
            resolver.to_string()
        ]))
    }

    /// Whether this process may run containers locally. Gates local
    /// provisioning paths independently of whether a Docker socket is
    /// mounted — the profile is a contract, not a capability check.
    pub fn local_workloads_enabled(&self) -> bool {
        self.local_workloads_enabled
    }

    /// Extract the Docker client, returning a typed error when this process
    /// was started without a daemon (e.g. a containerised control plane).
    ///
    /// Every daemon-dependent code path should call this once — as late as
    /// possible — and propagate [`ExternalServiceError::DockerUnavailable`]
    /// upward. Local-provisioning paths should also check
    /// [`Self::local_workloads_enabled`] **first** so the user gets a
    /// policy error (409) rather than a capability error.
    fn require_docker(&self) -> Result<Arc<Docker>, ExternalServiceError> {
        Ok(self.docker.require()?)
    }

    /// Build a service engine instance for the given name and type.
    ///
    /// Returns [`ExternalServiceError::DockerUnavailable`] when this process
    /// has no Docker daemon — that is the typed signal callers map to a
    /// 409/503 rather than a connection-refused from inside the engine.
    /// Local-provisioning callers must additionally guard on
    /// [`Self::local_workloads_enabled`] before reaching this function so
    /// the policy error (not the capability error) is what the operator sees.
    pub fn get_service_instance(
        &self,
        name: String,
        service_type: ServiceType,
    ) -> Result<Box<dyn ExternalService>, ExternalServiceError> {
        self.create_service_instance(name, service_type)
    }

    /// Engine instance for the service registered under `name`.
    ///
    /// [`Self::get_service_instance`] only knows the service type, so for an
    /// S3 or Blob service it always answers with the default engine. Anything
    /// that acts on a registered service (backup, restore, import) needs the
    /// engine that service actually runs, which is recorded in its
    /// parameters. A name that is not registered gets the default engine.
    pub async fn resolve_service_instance(
        &self,
        name: &str,
        service_type: ServiceType,
    ) -> Result<Box<dyn ExternalService>, ExternalServiceError> {
        if !matches!(service_type, ServiceType::S3 | ServiceType::Blob) {
            return self.create_service_instance(name.to_string(), service_type);
        }
        let registered = external_services::Entity::find()
            .filter(external_services::Column::Name.eq(name))
            .one(self.db.as_ref())
            .await?;
        match registered {
            Some(row) => {
                let parameters = self.get_service_parameters(row.id).await?;
                self.create_service_instance_for_parameters(
                    name.to_string(),
                    service_type,
                    &parameters,
                )
            }
            None => self.create_service_instance(name.to_string(), service_type),
        }
    }

    /// Every engine that could own a service called `name` of `service_type`.
    ///
    /// A service that is not registered yet has no recorded engine, and the
    /// callers that ask (name-collision checks, cleanup after a cancelled
    /// restore) must cover whichever engine the restore picked.
    fn candidate_instances(
        &self,
        name: &str,
        service_type: ServiceType,
    ) -> Result<Vec<Box<dyn ExternalService>>, ExternalServiceError> {
        let mut candidates = vec![self.create_service_instance(name.to_string(), service_type)?];
        if matches!(service_type, ServiceType::S3 | ServiceType::Blob) {
            let docker = self.require_docker()?;
            candidates.push(Box::new(GarageService::new(
                name.to_string(),
                docker,
                self.encryption_service.clone(),
                service_type,
            )));
        }
        Ok(candidates)
    }

    #[allow(deprecated)]
    fn create_service_instance(
        &self,
        name: String,
        service_type: ServiceType,
    ) -> Result<Box<dyn ExternalService>, ExternalServiceError> {
        let docker = self.require_docker()?;
        Ok(match service_type {
            ServiceType::Mariadb => Box::new(MariaDbService::new(name, docker)),
            ServiceType::Mongodb => Box::new(MongodbService::new(name, docker)),
            ServiceType::Postgres => Box::new(PostgresService::new(name, docker)),
            // Note: PostgresCluster is handled via create_cluster_service_instance, not here
            ServiceType::Redis => Box::new(RedisService::new(name, docker)),
            // S3 now uses RustFS by default (high-performance S3-compatible storage)
            ServiceType::S3 => Box::new(RustfsService::new(
                name,
                docker,
                self.encryption_service.clone(),
            )),
            // Temps KV uses Redis backend. The instance name must come from
            // `managed_instance_name` so this agrees with the kv plugin —
            // see the module docs on `externalsvc::naming` and issue #495.
            ServiceType::Kv => Box::new(RedisService::new(
                managed_instance_name(&name, service_type),
                docker,
            )),
            // Temps Blob uses RustfsService (high-performance S3-compatible
            // storage). Same naming contract as `Kv` above.
            ServiceType::Blob => Box::new(RustfsService::new(
                managed_instance_name(&name, service_type),
                docker,
                self.encryption_service.clone(),
            )),
            // RustFS standalone S3-compatible storage
            ServiceType::Rustfs => Box::new(RustfsService::new(
                name,
                docker,
                self.encryption_service.clone(),
            )),
            // MinIO (deprecated) - kept for backward compatibility with existing services
            ServiceType::Minio => Box::new(S3Service::new(
                name,
                docker,
                self.encryption_service.clone(),
            )),
        })
    }

    /// Parameter-strategy key for updating a stored service. An S3/Blob
    /// service on Garage is configured like Garage; every other service keeps
    /// the strategy of its stored type (legacy MinIO included).
    fn update_strategy_key(
        service_type: &str,
        parameters: &HashMap<String, serde_json::Value>,
    ) -> String {
        let is_s3_family = matches!(service_type, "s3" | "blob");
        let garage = is_s3_family
            && parameters
                .get("backend")
                .and_then(serde_json::Value::as_str)
                .and_then(|backend| ManagedS3BackendKind::parse(backend).ok())
                == Some(ManagedS3BackendKind::Garage);
        if garage {
            ManagedS3BackendKind::Garage.as_str().to_string()
        } else {
            service_type.to_string()
        }
    }

    #[allow(deprecated)]
    fn create_service_instance_for_parameters(
        &self,
        name: String,
        service_type: ServiceType,
        parameters: &HashMap<String, serde_json::Value>,
    ) -> Result<Box<dyn ExternalService>, ExternalServiceError> {
        let parameter_value =
            serde_json::to_value(parameters).map_err(|e| ExternalServiceError::InternalError {
                reason: format!("Failed to inspect managed S3 backend parameters: {}", e),
            })?;
        self.create_service_instance_for_parameter_value(name, service_type, &parameter_value)
    }

    #[allow(deprecated)]
    fn create_service_instance_for_parameter_value(
        &self,
        name: String,
        service_type: ServiceType,
        parameters: &serde_json::Value,
    ) -> Result<Box<dyn ExternalService>, ExternalServiceError> {
        if !matches!(service_type, ServiceType::S3 | ServiceType::Blob) {
            return self.create_service_instance(name, service_type);
        }

        let backend_selection =
            ManagedS3BackendSelection::from_parameters(parameters).map_err(|e| {
                ExternalServiceError::ParameterValidationFailed {
                    service_id: 0,
                    reason: e.to_string(),
                }
            })?;
        match backend_selection.backend {
            ManagedS3BackendKind::Rustfs => self.create_service_instance(name, service_type),
            ManagedS3BackendKind::Minio if service_type == ServiceType::S3 => {
                let docker = self.require_docker()?;
                Ok(Box::new(S3Service::new(
                    name,
                    docker,
                    self.encryption_service.clone(),
                )))
            }
            ManagedS3BackendKind::Minio => Err(ExternalServiceError::ParameterValidationFailed {
                service_id: 0,
                reason: "managed S3 backend 'minio' is only supported for S3 services; use the default 'rustfs' backend for Blob services"
                    .to_string(),
            }),
            ManagedS3BackendKind::Garage => {
                let docker = self.require_docker()?;
                Ok(Box::new(GarageService::new(
                    name,
                    docker,
                    self.encryption_service.clone(),
                    service_type,
                )))
            }
        }
    }

    // -----------------------------------------------------------------------
    // Remote-node helpers
    // -----------------------------------------------------------------------

    /// Read a single `external_services` row by id. Public so handlers can
    /// branch on per-service fields (e.g. `topology`) without redoing the
    /// existence check.
    pub async fn get_service(
        &self,
        service_id: i32,
    ) -> Result<external_services::Model, ExternalServiceError> {
        external_services::Entity::find_by_id(service_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(ExternalServiceError::ServiceNotFound { id: service_id })
    }

    pub(crate) async fn get_service_parameters(
        &self,
        service_id_val: i32,
    ) -> Result<HashMap<String, serde_json::Value>, ExternalServiceError> {
        let service = self.get_service(service_id_val).await?;

        // Get encrypted config from service record
        let encrypted_config =
            service
                .config
                .ok_or_else(|| ExternalServiceError::InternalError {
                    reason: format!("Service {} has no config", service_id_val),
                })?;

        // Decrypt config
        let config_json = self
            .encryption_service
            .decrypt_string(&encrypted_config)
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!(
                    "Failed to decrypt config for service {}: {}",
                    service_id_val, e
                ),
            })?;

        // Deserialize JSON to HashMap
        let parameters: HashMap<String, serde_json::Value> = serde_json::from_str(&config_json)
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!(
                    "Failed to deserialize config for service {}: {}",
                    service_id_val, e
                ),
            })?;

        Ok(parameters)
    }
}

/// Remove a single key from an `external_services.health_metadata` JSONB
/// blob. Returns `None` when the result would be an empty object (so the
/// column goes back to NULL instead of `{}`).
fn clear_health_metadata_key(
    existing: Option<&sea_orm::JsonValue>,
    key: &str,
) -> Option<sea_orm::JsonValue> {
    let map = match existing {
        Some(serde_json::Value::Object(m)) => m,
        _ => return None,
    };
    let mut next = map.clone();
    next.remove(key);
    if next.is_empty() {
        None
    } else {
        Some(serde_json::Value::Object(next))
    }
}

/// Merge a single typed snapshot under `key` into an
/// `external_services.health_metadata` JSONB blob, preserving sibling keys.
fn merge_health_metadata_key<T: serde::Serialize>(
    existing: Option<&sea_orm::JsonValue>,
    key: &str,
    snapshot: &T,
) -> sea_orm::JsonValue {
    let value = serde_json::to_value(snapshot).unwrap_or(serde_json::Value::Null);
    let mut map = match existing {
        Some(serde_json::Value::Object(m)) => m.clone(),
        _ => serde_json::Map::new(),
    };
    map.insert(key.to_string(), value);
    serde_json::Value::Object(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::externalsvc::{legacy_managed_instance_names, ServiceType};
    use crate::services::test_support::*;
    use std::collections::HashMap;
    use std::sync::Arc;
    use temps_core::DockerHandle;
    use temps_core::EncryptionService;
    use temps_entities::{external_services, nodes};

    fn s3_row(name: &str, parameters: serde_json::Value) -> external_services::Model {
        let mut row = encrypted_service_model(41, parameters);
        row.name = name.to_string();
        row.service_type = "s3".to_string();
        row
    }

    async fn resolved_container(
        manager: &ExternalServiceManager,
        name: &str,
    ) -> Result<String, ExternalServiceError> {
        let instance = manager
            .resolve_service_instance(name, ServiceType::S3)
            .await?;
        Ok(instance
            .docker_resource_names()
            .and_then(|names| names.containers.into_iter().next())
            .unwrap_or_default())
    }

    #[test]
    fn a_garage_service_is_updated_with_the_garage_strategy() {
        let garage = HashMap::from([("backend".to_string(), serde_json::json!("garage"))]);
        assert_eq!(
            ExternalServiceManager::update_strategy_key("s3", &garage),
            "garage"
        );
        assert_eq!(
            ExternalServiceManager::update_strategy_key("blob", &garage),
            "garage"
        );
    }

    #[test]
    fn other_services_keep_the_strategy_of_their_stored_type() {
        let garage = HashMap::from([("backend".to_string(), serde_json::json!("garage"))]);
        let rustfs = HashMap::from([("backend".to_string(), serde_json::json!("rustfs"))]);
        let legacy = HashMap::new();
        assert_eq!(
            ExternalServiceManager::update_strategy_key("s3", &rustfs),
            "s3"
        );
        assert_eq!(
            ExternalServiceManager::update_strategy_key("s3", &legacy),
            "s3"
        );
        assert_eq!(
            ExternalServiceManager::update_strategy_key("blob", &legacy),
            "blob"
        );
        assert_eq!(
            ExternalServiceManager::update_strategy_key("postgres", &garage),
            "postgres"
        );
    }

    #[test]
    fn a_renamed_garage_service_is_stopped_under_its_garage_container_name() {
        let manager = mock_service_manager(vec![]);
        let parameters = HashMap::from([("backend".to_string(), serde_json::json!("garage"))]);

        let instance = manager
            .create_service_instance_for_parameters(
                "media".to_string(),
                ServiceType::S3,
                &parameters,
            )
            .expect("garage instance");

        assert_eq!(instance.get_docker_container_name(), "garage-media");
    }

    #[tokio::test]
    async fn a_registered_garage_service_resolves_to_the_garage_engine() {
        let row = s3_row("media", serde_json::json!({ "backend": "garage" }));
        let manager = mock_service_manager(vec![vec![row.clone()], vec![row]]);

        let container = resolved_container(&manager, "media")
            .await
            .expect("resolve");

        assert!(container.starts_with("garage-"), "{container}");
    }

    #[tokio::test]
    async fn a_registered_s3_service_without_a_backend_resolves_to_rustfs() {
        let row = s3_row("legacy-media", serde_json::json!({}));
        let manager = mock_service_manager(vec![vec![row.clone()], vec![row]]);

        let container = resolved_container(&manager, "legacy-media")
            .await
            .expect("resolve");

        assert!(container.starts_with("rustfs-"), "{container}");
    }

    #[tokio::test]
    async fn an_unregistered_s3_name_resolves_to_the_default_engine() {
        let manager = mock_service_manager(vec![Vec::<external_services::Model>::new()]);

        let container = resolved_container(&manager, "fresh")
            .await
            .expect("resolve");

        assert!(container.starts_with("rustfs-"), "{container}");
    }

    #[tokio::test]
    async fn an_unregistered_s3_name_is_checked_against_every_engine() {
        let manager = mock_service_manager(vec![]);

        let candidates = manager
            .candidate_instances("restored", ServiceType::S3)
            .expect("candidates");
        let containers: Vec<String> = candidates
            .iter()
            .filter_map(|instance| instance.docker_resource_names())
            .flat_map(|names| names.containers)
            .collect();

        assert_eq!(containers.len(), 2, "{containers:?}");
        assert!(containers.iter().any(|c| c.starts_with("rustfs-")));
        assert!(containers.iter().any(|c| c.starts_with("garage-")));
    }

    #[tokio::test]
    async fn control_plane_dials_a_worker_member_on_its_data_address_not_its_overlay_ip() {
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([vec![nodes_test_model(2)]])
            .into_connection();
        let mut monitor = service_member_model(1, 0, "monitor");
        monitor.node_id = Some(2);
        monitor.compute_ip = Some("172.20.1.3".to_string());

        let host = control_plane_member_dial_host(&db, monitor.node_id, &monitor.container_name)
            .await
            .unwrap();

        assert_eq!(host, "10.0.0.2");
    }

    #[tokio::test]
    async fn control_plane_dials_its_own_members_on_loopback() {
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection();
        let mut monitor = service_member_model(1, 0, "monitor");
        monitor.compute_ip = Some("172.18.0.5".to_string());
        assert_eq!(
            control_plane_member_dial_host(&db, monitor.node_id, &monitor.container_name)
                .await
                .unwrap(),
            LOCAL_CLUSTER_HOST
        );
        monitor.node_id = Some(0);
        assert_eq!(
            control_plane_member_dial_host(&db, monitor.node_id, &monitor.container_name)
                .await
                .unwrap(),
            LOCAL_CLUSTER_HOST
        );
    }

    #[tokio::test]
    async fn control_plane_member_dial_host_names_a_missing_node() {
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([Vec::<nodes::Model>::new()])
            .into_connection();
        let mut monitor = service_member_model(1, 0, "monitor");
        monitor.node_id = Some(9);

        let reason = control_plane_member_dial_host(&db, monitor.node_id, &monitor.container_name)
            .await
            .unwrap_err();

        assert!(reason.contains("node 9"), "{reason}");
        assert!(reason.contains("cluster-member-0"), "{reason}");
    }

    /// Regression for #495.
    ///
    /// The blob plugin creates and serves `rustfs-temps-blob`. When
    /// `create_service_instance` prefixed the name, the manager built
    /// `rustfs-blob-temps-blob` instead — so enabling Blob and restarting left
    /// two containers, and `delete_service` removed the empty one while the
    /// container holding every uploaded blob stayed up with its
    /// `external_services` row gone, unreachable from the platform.
    #[test]
    fn blob_service_instance_targets_the_container_the_plugin_created() {
        let manager = mock_service_manager(vec![]);
        let instance = manager
            .create_service_instance("temps-blob".to_string(), ServiceType::Blob)
            .expect("mock manager always has an available Docker handle");

        assert_eq!(
            instance.get_name(),
            "temps-blob",
            "delete_service targets container `rustfs-{}`, but the blob plugin created \
             `rustfs-temps-blob`",
            instance.get_name()
        );
    }

    /// Guard rail, not a live bug: `temps-kv` persists `ServiceType::Redis`,
    /// so the `Kv` branch is off the hot path today. Correcting that type is
    /// the obvious cleanup, and before #495 was fixed it would have
    /// reproduced the same orphan for KV.
    #[test]
    fn kv_service_instance_targets_the_container_the_plugin_created() {
        let manager = mock_service_manager(vec![]);
        let instance = manager
            .create_service_instance("temps-kv".to_string(), ServiceType::Kv)
            .expect("mock manager always has an available Docker handle");

        assert_eq!(
            instance.get_name(),
            "temps-kv",
            "delete_service targets container `redis-{}`, but the kv plugin created \
             `redis-temps-kv`",
            instance.get_name()
        );
    }

    /// The sweep at the end of `delete_service` reaches pre-fix containers by
    /// building an instance from each legacy name. That only works if the
    /// instance comes out named exactly what the old code produced — if this
    /// round-trip drifts, upgraded installs keep their orphan forever and the
    /// delete still reports success.
    #[test]
    fn legacy_instance_names_round_trip_to_the_pre_fix_containers() {
        let manager = mock_service_manager(vec![]);

        for (service_name, service_type, expected) in [
            ("temps-blob", ServiceType::Blob, "blob-temps-blob"),
            ("temps-kv", ServiceType::Kv, "kv-temps-kv"),
        ] {
            let legacy = legacy_managed_instance_names(service_name, service_type);
            assert_eq!(legacy, vec![expected.to_string()]);

            let instance = manager
                .create_service_instance(legacy[0].clone(), service_type)
                .expect("mock manager always has an available Docker handle");
            assert_eq!(
                instance.get_name(),
                expected,
                "the sweep must address the old container, not re-derive the canonical one"
            );
            assert_ne!(
                instance.get_name(),
                manager
                    .create_service_instance(service_name.to_string(), service_type)
                    .expect("mock manager always has an available Docker handle")
                    .get_name(),
                "sweeping the canonical container would delete the live service's data"
            );
        }
    }

    /// A disabled `DockerHandle` causes `require_docker()` to return a typed
    /// `DockerUnavailable` error. The policy flag is independent: even when
    /// `local_workloads_enabled = true`, no daemon → clear typed error.
    #[test]
    fn disabled_handle_yields_docker_unavailable() {
        use sea_orm::DatabaseBackend;
        use sea_orm::MockDatabase;

        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let enc = Arc::new(EncryptionService::new(&"0".repeat(64)).unwrap());
        let dns = Arc::new(temps_dns::DnsRegistry::new(db.clone()));
        let handle = Arc::new(DockerHandle::disabled(
            temps_core::PROFILE_CONTROL_PLANE,
            "no socket mounted in this process",
        ));

        let manager = ExternalServiceManager::new_with_handle(
            db, enc, handle,
            true, // local_workloads_enabled — policy allows it, but handle is disabled
            dns,
        );

        let err = manager
            .require_docker()
            .expect_err("disabled handle must yield an error");
        assert!(
            matches!(err, ExternalServiceError::DockerUnavailable(_)),
            "expected DockerUnavailable, got: {err:?}"
        );
        // The error message must carry the profile so operators know why
        assert!(
            err.to_string().contains("control-plane"),
            "error must name the profile: {err}"
        );
    }

    /// When `local_workloads_enabled = false` (control-plane profile), the
    /// policy flag is `false` regardless of whether a Docker socket exists.
    #[test]
    fn control_plane_policy_flag_is_false() {
        use sea_orm::DatabaseBackend;
        use sea_orm::MockDatabase;

        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let enc = Arc::new(EncryptionService::new(&"0".repeat(64)).unwrap());
        let dns = Arc::new(temps_dns::DnsRegistry::new(db.clone()));
        // Use a disabled handle — that is the normal control-plane state, but
        // the policy flag is what gates local provisioning, not the handle.
        let handle = Arc::new(DockerHandle::disabled(
            temps_core::PROFILE_CONTROL_PLANE,
            "control-plane profile",
        ));

        let manager = ExternalServiceManager::new_with_handle(
            db, enc, handle, false, // <-- the policy says no local workloads
            dns,
        );

        assert!(
            !manager.local_workloads_enabled(),
            "control-plane policy must report local_workloads_enabled = false"
        );
    }

    /// `LocalWorkloadsDisabled` carries the service name in its error text so
    /// an operator reading the 409 response knows which service was rejected.
    #[test]
    fn local_workloads_disabled_error_includes_service_name() {
        let err = ExternalServiceError::LocalWorkloadsDisabled {
            name: "my-postgres".to_string(),
        };
        assert!(
            err.to_string().contains("my-postgres"),
            "error must include the service name: {err}"
        );
        assert!(
            err.to_string().contains("control-plane"),
            "error must mention the profile: {err}"
        );
        assert!(
            err.to_string().contains("temps join"),
            "error must mention the remedy: {err}"
        );
    }

    /// Full-profile managers report `local_workloads_enabled = true`.
    #[test]
    fn full_profile_policy_flag_is_true() {
        use sea_orm::DatabaseBackend;
        use sea_orm::MockDatabase;

        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let enc = Arc::new(EncryptionService::new(&"0".repeat(64)).unwrap());
        let dns = Arc::new(temps_dns::DnsRegistry::new(db.clone()));
        let handle = Arc::new(DockerHandle::disabled(
            temps_core::PROFILE_CONTROL_PLANE,
            "no socket (even full-profile constructors get tested here)",
        ));

        let manager = ExternalServiceManager::new_with_handle(
            db, enc, handle, true, // full-profile
            dns,
        );

        assert!(
            manager.local_workloads_enabled(),
            "full profile must report local_workloads_enabled = true"
        );
    }
}
