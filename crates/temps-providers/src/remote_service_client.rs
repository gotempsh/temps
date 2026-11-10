// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! HTTP client for calling the agent's service management API on remote nodes.
//!
//! Used by `ExternalServiceManager` to route service operations (create, start,
//! stop, remove) through a worker node's agent when `node_id` is set.

use futures::StreamExt;
use serde::Deserialize;
use std::collections::HashMap;
use std::time::Duration;
use tracing::{error, info};

pub use temps_providers_core::remote_params::{RemotePortMapping, RemoteServiceCreateParams};

use crate::services::ExternalServiceError;
use crate::workload::ServiceWorkloadError;
pub use temps_service_protocol::{
    MariadbBinlogArchiveRequest, MariadbBinlogArchiveResult, MariadbBinlogCoordinate,
    MariadbDumpBackupRequest, MariadbDumpBackupResult, MariadbPhysicalBackupRequest,
    MariadbPhysicalBackupResult, MariadbRestoreRequest, MariadbRestoreResult, MongodbBackupMode,
    MongodbBackupRequest, MongodbBackupResult, MongodbRestoreRequest, MongodbRestoreResult,
    PostgresWalgArchiveRequest, PostgresWalgArchiveResult, PostgresWalgBackupRequest,
    PostgresWalgBackupResult, PostgresWalgCredentialsRequest, PostgresWalgCredentialsResult,
    PostgresWalgRestoreOutcomeRequest, PostgresWalgRestoreProgress, PostgresWalgRestoreRequest,
    PostgresWalgRestoreResult, RecoveryTarget, RedisRdbBackupRequest, RedisRdbBackupResult,
    RedisRestoreRequest, RedisRestoreResult, RedisWalgBackupRequest, RedisWalgBackupResult,
    RemoteHealthProbeRequest, RemoteHealthProbeResponse, RemoteRuntimeEnvRequest,
    RemoteRuntimeEnvResponse, RemoteServiceBackupRequest, RemoteServiceBackupResponse,
    RemoteServiceRestoreRequest, RemoteServiceRestoreResponse, RemoteWalHealthRequest,
    RemoteWalHealthResponse, ServiceContainerRuntime, ServiceContainerStats, WorkerS3Destination,
};

/// Whether a node's agent has a route this release added. An agent that
/// predates it is not failing: the caller says which agent to update rather
/// than reporting an error.
#[derive(Debug)]
pub enum AgentSupport<T> {
    Supported(T),
    Unsupported,
}

/// Response envelope from the agent API (mirrors `AgentResponse` in temps-node-agent).
#[derive(Deserialize)]
struct AgentResponse<T> {
    success: bool,
    data: Option<T>,
    error: Option<String>,
}

const MAX_AGENT_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const MAX_HEALTH_ERROR_BYTES: usize = 2 * 1024;
const HEALTH_PROBE_TIMEOUT: Duration = Duration::from_secs(10);
/// Runtime and stats are read on every poll of the service page; two stats
/// samples take a second on the agent.
const OBSERVE_TIMEOUT: Duration = Duration::from_secs(10);
/// The WAL probe's connect and its queries are bounded at 5s each on the
/// agent, after a container inspection.
const WAL_PROBE_TIMEOUT: Duration = Duration::from_secs(20);
/// A worker backup or restore streams the whole dataset; the agent bounds
/// its own work by the same deadline.
pub const DATA_OPERATION_TIMEOUT: Duration = Duration::from_secs(6 * 3600);

/// How long the agent has to say where a restore is: it reads an in-memory
/// record, so a slow answer means the node is unreachable, and the caller
/// asks again later.
const RESTORE_OUTCOME_TIMEOUT: Duration = Duration::from_secs(20);

/// Lightweight client for the agent's service endpoints.
pub struct RemoteServiceClient {
    agent_url: String,
    token: String,
    node_name: String,
    client: reqwest::Client,
    /// How environment, secrets and service parameters may reach this node.
    /// Refuses them until [`Self::with_sealing`] says otherwise.
    sealing: temps_core::sealed_request::NodeSealing,
}

/// Response after creating a service on the agent.
#[derive(Debug, Deserialize)]
pub struct RemoteServiceCreateResponse {
    pub container_id: String,
    pub container_name: String,
    pub host_port: u16,
    /// Container's `temps-overlay` IP, when the container is attached to
    /// it. `None` from single-host clusters and from agents that haven't
    /// been upgraded yet (the field is `serde(default)`-able so older
    /// servers' responses still parse). See ADR-011.
    #[serde(default)]
    pub compute_ip: Option<String>,
}

/// Status of a service on a remote node.
#[derive(Debug, Deserialize)]
pub struct RemoteServiceStatus {
    pub container_name: String,
    pub container_id: Option<String>,
    pub running: bool,
    pub health: Option<String>,
}

/// Request body for `exec_in_service`. Mirrors `temps_node_agent::ServiceExecRequest`
/// but kept local to avoid a cross-crate dep.
#[derive(Debug, serde::Serialize)]
pub struct RemoteExecParams {
    pub container_name: String,
    pub command: Vec<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub environment: HashMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default)]
    pub detach: bool,
}

/// Response from a remote container exec.
#[derive(Debug, Deserialize)]
pub struct RemoteExecResult {
    pub exit_code: i64,
    pub stdout: String,
    pub stderr: String,
}

/// Request for `POST /agent/services/workload` (ADR-055 Phase 4): create a
/// database container from its admitted workload spec through the agent's
/// Docker driver, instead of the agent building its own request.
///
/// The plan never holds a secret value; the values travel next to it in
/// `secrets`, only inside a body sealed to the node, and are never logged
/// (`Debug` prints their names only).
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct ServiceWorkloadCreateRequest {
    /// The `external_services.name`, for the recreate guard's messages.
    pub service_name: String,
    pub plan: crate::workload::DatabaseWorkloadPlan,
    /// Secret values by the names the plan's spec references.
    #[serde(default)]
    pub secrets: std::collections::BTreeMap<String, String>,
    /// The Docker network the container joins (the app network).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<String>,
    /// Replace the running container through the recreate guard (the
    /// confirmed recreate) instead of creating a new one.
    #[serde(default)]
    pub replace_existing: bool,
}

impl std::fmt::Debug for ServiceWorkloadCreateRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServiceWorkloadCreateRequest")
            .field("service_name", &self.service_name)
            .field("plan", &self.plan)
            .field("secrets", &self.secrets.keys().collect::<Vec<_>>())
            .field("network", &self.network)
            .field("replace_existing", &self.replace_existing)
            .finish()
    }
}

/// How long the agent may take to create (or recreate, through the guard)
/// a database from its spec: pull, create, the health wait and the data
/// fingerprint comparison.
const WORKLOAD_CREATE_TIMEOUT: Duration = Duration::from_secs(600);
const WORKLOAD_SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(30);

impl RemoteServiceClient {
    /// Create a new client for the given agent.
    ///
    /// * `agent_url` — base URL, e.g. `https://10.100.0.2:3100`
    /// * `token` — plaintext bearer token for auth
    /// * `node_name` — human-readable name (for error messages)
    pub fn new(
        agent_url: String,
        token: String,
        node_name: String,
    ) -> Result<Self, ExternalServiceError> {
        // Strict TLS by default; operators with self-signed agent certs
        // on a trusted internal network can opt in via the
        // `insecure_tls` toggle in the application settings UI.
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(300))
            .danger_accept_invalid_certs(temps_core::tls::insecure_tls_enabled())
            .build()
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!("Failed to create HTTP client for node {}: {}", node_name, e),
            })?;

        Ok(Self {
            agent_url,
            token,
            node_name,
            client,
            sealing: Default::default(),
        })
    }

    /// Create a client that presents the control-plane identity and trusts
    /// only the cluster CA. This is required for agents enrolled with mTLS;
    /// using the process-wide public root store cannot authenticate either
    /// side of that private trust domain.
    pub fn new_mtls(
        agent_url: String,
        token: String,
        node_name: String,
        identity_pem: &str,
        cluster_ca_pem: &str,
    ) -> Result<Self, ExternalServiceError> {
        let identity = reqwest::Identity::from_pem(identity_pem.as_bytes()).map_err(|e| {
            ExternalServiceError::InternalError {
                reason: format!(
                    "Failed to parse control-plane mTLS identity for node {}: {}",
                    node_name, e
                ),
            }
        })?;
        let cluster_ca =
            reqwest::Certificate::from_pem(cluster_ca_pem.as_bytes()).map_err(|e| {
                ExternalServiceError::InternalError {
                    reason: format!(
                        "Failed to parse cluster CA certificate for node {}: {}",
                        node_name, e
                    ),
                }
            })?;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(300))
            .use_rustls_tls()
            .identity(identity)
            .add_root_certificate(cluster_ca)
            .tls_built_in_root_certs(false)
            .build()
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!("Failed to create mTLS client for node {}: {}", node_name, e),
            })?;

        Ok(Self {
            agent_url,
            token,
            node_name,
            client,
            sealing: Default::default(),
        })
    }

    /// [`Self::new_mtls`] for an agent reached as `endpoint` says: when it
    /// dials the node's WireGuard mesh address, the agent's certificate is
    /// verified for `endpoint.server_name` (the node's registered host, which
    /// is what the leaf names) instead of the dialed address. Roots, client
    /// identity, chain and expiry checks are unchanged.
    pub fn new_mtls_at(
        endpoint: &temps_core::node_address::NodeAgentEndpoint,
        token: String,
        node_name: String,
        identity_pem: &str,
        cluster_ca_pem: &str,
    ) -> Result<Self, ExternalServiceError> {
        let Some(server_name) = endpoint.server_name.as_deref() else {
            return Self::new_mtls(
                endpoint.dial_url.clone(),
                token,
                node_name,
                identity_pem,
                cluster_ca_pem,
            );
        };
        let tls = temps_core::node_pki::node_agent_client_config(
            identity_pem,
            cluster_ca_pem,
            server_name,
        )
        .map_err(|e| ExternalServiceError::InternalError {
            reason: format!(
                "Failed to build the mesh mTLS configuration for node {} at {}: {}",
                node_name, endpoint.dial_url, e
            ),
        })?;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(300))
            .use_preconfigured_tls(tls)
            .build()
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!(
                    "Failed to create mesh mTLS client for node {} at {}: {}",
                    node_name, endpoint.dial_url, e
                ),
            })?;

        Ok(Self {
            agent_url: endpoint.dial_url.clone(),
            token,
            node_name,
            client,
            sealing: Default::default(),
        })
    }

    /// How requests carrying values reach this node (see
    /// [`temps_core::sealed_request`]); from the node row's
    /// `request_sealing()`.
    pub fn with_sealing(mut self, sealing: temps_core::sealed_request::NodeSealing) -> Self {
        self.sealing = sealing;
        self
    }

    /// Encrypt `body` to the node, so nothing is sent if it cannot be.
    fn with_sealed_body<B: serde::Serialize + ?Sized>(
        &self,
        request: reqwest::RequestBuilder,
        path: &str,
        body: &B,
    ) -> Result<reqwest::RequestBuilder, temps_core::sealed_request::SealRequestError> {
        let sealed = self.sealing.seal_json(&self.node_name, path, body)?;
        Ok(request
            .header(
                reqwest::header::CONTENT_TYPE,
                temps_core::sealed_request::SEALED_REQUEST_CONTENT_TYPE,
            )
            .body(sealed))
    }

    fn older_agent_error(&self) -> ExternalServiceError {
        ExternalServiceError::InternalError {
            reason: temps_core::sealed_request::older_agent_message(&self.node_name),
        }
    }

    /// Create and start a service container on the remote node.
    pub async fn create_service(
        &self,
        params: RemoteServiceCreateParams,
    ) -> Result<RemoteServiceCreateResponse, ExternalServiceError> {
        info!(
            "Creating service '{}' on remote node '{}'",
            params.name, self.node_name
        );
        // Always sealed: a database's password is not only in its environment
        // (Redis takes it as `--requirepass` in the command).
        self.agent_post_sealed("/agent/services", &params).await
    }

    /// Start an existing service container on the remote node.
    pub async fn start_service(&self, container_name: &str) -> Result<(), ExternalServiceError> {
        info!(
            "Starting service '{}' on remote node '{}'",
            container_name, self.node_name
        );
        let _: serde_json::Value = self
            .agent_post_no_body(&format!("/agent/services/{}/start", container_name))
            .await?;
        Ok(())
    }

    /// Stop a service container on the remote node.
    pub async fn stop_service(&self, container_name: &str) -> Result<(), ExternalServiceError> {
        info!(
            "Stopping service '{}' on remote node '{}'",
            container_name, self.node_name
        );
        let _: serde_json::Value = self
            .agent_post_no_body(&format!("/agent/services/{}/stop", container_name))
            .await?;
        Ok(())
    }

    /// Remove a service container (and its volumes) on the remote node.
    pub async fn remove_service(&self, container_name: &str) -> Result<(), ExternalServiceError> {
        info!(
            "Removing service '{}' on remote node '{}'",
            container_name, self.node_name
        );
        self.agent_delete(&format!("/agent/services/{}", container_name))
            .await
    }

    /// Get the status of a service on the remote node.
    pub async fn service_status(
        &self,
        container_name: &str,
    ) -> Result<RemoteServiceStatus, ExternalServiceError> {
        self.agent_get(&format!("/agent/services/{}/status", container_name))
            .await
    }

    /// Run a command inside a service container on the remote node.
    /// Used for cluster admin operations (e.g. `pg_autoctl perform
    /// promotion`) that have to run from the container's perspective.
    /// Bounded admin commands only — never user input — see the
    /// CLAUDE.md note on docker exec safety.
    pub async fn exec_in_service(
        &self,
        params: RemoteExecParams,
    ) -> Result<RemoteExecResult, ExternalServiceError> {
        // Never the command itself: its arguments can hold a credential.
        info!(
            container = %params.container_name,
            node = %self.node_name,
            command_argc = params.command.len(),
            "Executing command in service container on remote node"
        );
        // Always sealed: the command can hold a credential as well.
        self.agent_post_sealed("/agent/services/exec", &params)
            .await
    }

    /// Initialize the provider and provision its per-project resource on the
    /// remote node. This keeps Docker and node-local TCP access inside the
    /// worker agent instead of accidentally using the control plane host.
    pub async fn get_runtime_env_vars(
        &self,
        request: RemoteRuntimeEnvRequest,
    ) -> Result<RemoteRuntimeEnvResponse, ExternalServiceError> {
        info!(
            service = %request.service_config.name,
            node = %self.node_name,
            "Provisioning external-service runtime environment on remote node"
        );
        // The service config holds the service's credentials.
        self.agent_post_sealed("/agent/services/runtime-env", &request)
            .await
    }

    /// Preview the runtime environment a project environment would receive
    /// from a service on the remote node, with nothing provisioned there.
    pub async fn preview_runtime_env_vars(
        &self,
        request: RemoteRuntimeEnvRequest,
    ) -> Result<RemoteRuntimeEnvResponse, ExternalServiceError> {
        // The service config holds the service's credentials.
        self.agent_post_sealed("/agent/services/runtime-env-preview", &request)
            .await
    }

    /// Revoke the per-environment storage credentials a project link
    /// provisioned on the worker that owns the service. Unlinking must not
    /// leave working keys behind, and the control plane has no route to the
    /// worker's storage engine, so the agent performs the revocation.
    pub async fn revoke_s3_access(
        &self,
        request: RemoteRuntimeEnvRequest,
    ) -> Result<(), ExternalServiceError> {
        info!(
            service = %request.service_config.name,
            node = %self.node_name,
            "Revoking managed S3 credentials on remote node"
        );
        self.agent_post_sealed::<_, String>("/agent/services/revoke-s3-access", &request)
            .await
            .map(|_| ())
    }

    /// Back up a service on the worker that runs it, with the engine's own
    /// tool, straight into the destination bucket. The destination's
    /// credentials and the service's travel only in the sealed body.
    pub async fn backup_service(
        &self,
        request: &RemoteServiceBackupRequest,
    ) -> Result<RemoteServiceBackupResponse, ExternalServiceError> {
        info!(
            service = %request.service_config.name,
            node = %self.node_name,
            bucket = %request.destination.bucket,
            key = %request.key,
            "Backing up an external service on its remote node"
        );
        self.agent_post_with_timeout(
            "/agent/services/data-backup",
            request,
            DATA_OPERATION_TIMEOUT,
        )
        .await
    }

    /// Restore a service in place on the worker that runs it, from a backup
    /// [`Self::backup_service`] wrote.
    pub async fn restore_service(
        &self,
        request: &RemoteServiceRestoreRequest,
    ) -> Result<RemoteServiceRestoreResponse, ExternalServiceError> {
        info!(
            service = %request.service_config.name,
            node = %self.node_name,
            location = %request.location,
            "Restoring an external service on its remote node"
        );
        self.agent_post_with_timeout(
            "/agent/services/data-restore",
            request,
            DATA_OPERATION_TIMEOUT,
        )
        .await
    }

    /// Take a WAL-G base backup of a PostgreSQL service on the worker that
    /// runs it, with the control plane's own engine (ADR-099). The
    /// destination's credentials and the service's travel only in the
    /// sealed body.
    pub async fn postgres_walg_backup(
        &self,
        request: &PostgresWalgBackupRequest,
    ) -> Result<PostgresWalgBackupResult, ExternalServiceError> {
        info!(
            service = %request.service_config.name,
            service_id = request.service_id,
            node = %self.node_name,
            bucket = %request.destination.bucket,
            allow_restart = request.allow_restart,
            "Taking a WAL-G base backup of a PostgreSQL service on its remote node"
        );
        self.agent_post_with_timeout(
            "/agent/services/postgres/walg-backup",
            request,
            DATA_OPERATION_TIMEOUT,
        )
        .await
    }

    /// Point a worker PostgreSQL's continuous archiving at another S3
    /// source, without a base backup. The source's credentials travel
    /// sealed; an agent without the route is told to update.
    pub async fn postgres_walg_archive(
        &self,
        request: &PostgresWalgArchiveRequest,
    ) -> Result<PostgresWalgArchiveResult, ExternalServiceError> {
        info!(
            service = %request.service_config.name,
            service_id = request.service_id,
            node = %self.node_name,
            bucket = %request.destination.bucket,
            "Pointing a remote PostgreSQL service's WAL-G archiving at an S3 source"
        );
        self.agent_post_with_timeout(
            "/agent/services/postgres/walg-archive",
            request,
            DATA_OPERATION_TIMEOUT,
        )
        .await
    }

    /// Refresh the credential a worker PostgreSQL's continuous archiving
    /// reads (ADR-093). Travels sealed; an agent without the route is told
    /// to update.
    pub async fn postgres_walg_credentials(
        &self,
        request: &PostgresWalgCredentialsRequest,
    ) -> Result<PostgresWalgCredentialsResult, ExternalServiceError> {
        info!(
            service = %request.service_config.name,
            service_id = request.service_id,
            node = %self.node_name,
            bucket = %request.destination.bucket,
            "Refreshing a remote PostgreSQL service's WAL-G archiving credential"
        );
        self.agent_post_with_timeout(
            "/agent/services/postgres/walg-credentials",
            request,
            DATA_OPERATION_TIMEOUT,
        )
        .await
    }

    /// Restore a worker's PostgreSQL from WAL-G, in place or to a point in
    /// time, with the control plane's own restore run by the node's agent.
    /// The S3 source's credentials and the logins travel sealed; an agent
    /// without the route is told to update.
    pub async fn postgres_walg_restore(
        &self,
        request: &PostgresWalgRestoreRequest,
    ) -> Result<PostgresWalgRestoreResult, ExternalServiceError> {
        info!(
            service = %request.live_service_config.name,
            service_id = request.service_id,
            node = %self.node_name,
            bucket = %request.source.bucket,
            pitr = request.recovery_target.is_some(),
            "Restoring a PostgreSQL service on its remote node from WAL-G"
        );
        self.agent_post_with_timeout(
            "/agent/services/postgres/walg-restore",
            request,
            DATA_OPERATION_TIMEOUT,
        )
        .await
    }

    /// Where a WAL-G restore started with an `operation_id` is on its node:
    /// still running, finished with an outcome, or ended without one. Asked
    /// when the restore's own request went unanswered, so the run it belongs
    /// to is completed from what the node actually did.
    pub async fn postgres_walg_restore_outcome(
        &self,
        request: &PostgresWalgRestoreOutcomeRequest,
    ) -> Result<PostgresWalgRestoreProgress, ExternalServiceError> {
        self.agent_post_with_timeout(
            "/agent/services/postgres/walg-restore/outcome",
            request,
            RESTORE_OUTCOME_TIMEOUT,
        )
        .await
    }

    /// Back up a worker's MongoDB with the control plane's own engine, run
    /// by the node's agent (ADR-099): a WAL-G stream when the image has
    /// WAL-G, else a mongodump archive. The destination's credentials and
    /// the service's travel only in the sealed body; an agent without the
    /// route is told to update.
    pub async fn mongodb_backup(
        &self,
        request: &MongodbBackupRequest,
    ) -> Result<MongodbBackupResult, ExternalServiceError> {
        info!(
            service = %request.service_config.name,
            service_id = request.service_id,
            node = %self.node_name,
            bucket = %request.destination.bucket,
            "Backing up a MongoDB service on its remote node"
        );
        self.agent_post_with_timeout(
            "/agent/services/mongodb/backup",
            request,
            DATA_OPERATION_TIMEOUT,
        )
        .await
    }

    /// Restore a worker's MongoDB in place with the control plane's own
    /// restore, run by the node's agent. The source's credentials and the
    /// logins travel sealed; an agent without the route is told to update.
    pub async fn mongodb_restore(
        &self,
        request: &MongodbRestoreRequest,
    ) -> Result<MongodbRestoreResult, ExternalServiceError> {
        info!(
            service = %request.service_config.name,
            service_id = request.service_id,
            node = %self.node_name,
            bucket = %request.source.bucket,
            location = %request.location,
            "Restoring a MongoDB service on its remote node"
        );
        self.agent_post_with_timeout(
            "/agent/services/mongodb/restore",
            request,
            DATA_OPERATION_TIMEOUT,
        )
        .await
    }

    /// Push a WAL-G stream backup of a Redis service on the worker that runs
    /// it, with the control plane's own Redis engine (ADR-099). The source's
    /// credentials and the service's password travel only sealed; an agent
    /// without the route is told to update.
    pub async fn redis_walg_backup(
        &self,
        request: &RedisWalgBackupRequest,
    ) -> Result<RedisWalgBackupResult, ExternalServiceError> {
        info!(
            service = %request.service_config.name,
            service_id = request.service_id,
            node = %self.node_name,
            bucket = %request.destination.bucket,
            "Pushing a WAL-G stream backup of a Redis service on its remote node"
        );
        self.agent_post_with_timeout(
            "/agent/services/redis/walg-backup",
            request,
            DATA_OPERATION_TIMEOUT,
        )
        .await
    }

    /// Take an RDB backup of a worker's Redis (an image without WAL-G) with
    /// the control plane's own pinned helper, run by the node's agent.
    pub async fn redis_rdb_backup(
        &self,
        request: &RedisRdbBackupRequest,
    ) -> Result<RedisRdbBackupResult, ExternalServiceError> {
        info!(
            service = %request.service_config.name,
            service_id = request.service_id,
            node = %self.node_name,
            bucket = %request.destination.bucket,
            key = %request.key,
            "Taking an RDB backup of a Redis service on its remote node"
        );
        self.agent_post_with_timeout(
            "/agent/services/redis/rdb-backup",
            request,
            DATA_OPERATION_TIMEOUT,
        )
        .await
    }

    /// Restore a worker's Redis in place with the provider's own restore,
    /// run by the node's agent.
    pub async fn redis_restore(
        &self,
        request: &RedisRestoreRequest,
    ) -> Result<RedisRestoreResult, ExternalServiceError> {
        info!(
            service = %request.service_config.name,
            service_id = request.service_id,
            node = %self.node_name,
            bucket = %request.source.bucket,
            keep_target_bookkeeping = request.keep_target_bookkeeping,
            "Restoring a Redis service on its remote node"
        );
        self.agent_post_with_timeout(
            "/agent/services/redis/restore",
            request,
            DATA_OPERATION_TIMEOUT,
        )
        .await
    }

    /// Push a WAL-G physical base backup of a worker's MariaDB with the
    /// control plane's own engine, run by the node's agent. The S3 source's
    /// credentials travel sealed; an agent without the route is told to
    /// update.
    pub async fn mariadb_physical_backup(
        &self,
        request: &MariadbPhysicalBackupRequest,
    ) -> Result<MariadbPhysicalBackupResult, ExternalServiceError> {
        info!(
            service = %request.service_config.name,
            service_id = request.service_id,
            node = %self.node_name,
            bucket = %request.destination.bucket,
            "Backing up a MariaDB service on its remote node with WAL-G"
        );
        self.agent_post_with_timeout(
            "/agent/services/mariadb/physical-backup",
            request,
            DATA_OPERATION_TIMEOUT,
        )
        .await
    }

    /// Dump a worker's MariaDB to S3 with the control plane's own dump,
    /// run by the node's agent.
    pub async fn mariadb_dump_backup(
        &self,
        request: &MariadbDumpBackupRequest,
    ) -> Result<MariadbDumpBackupResult, ExternalServiceError> {
        info!(
            service = %request.service_config.name,
            service_id = request.service_id,
            node = %self.node_name,
            bucket = %request.destination.bucket,
            key = %request.key,
            "Dumping a MariaDB service on its remote node"
        );
        self.agent_post_with_timeout(
            "/agent/services/mariadb/dump-backup",
            request,
            DATA_OPERATION_TIMEOUT,
        )
        .await
    }

    /// Restore a worker's MariaDB in place, or to a point in time, with the
    /// control plane's own restore run by the node's agent.
    pub async fn mariadb_restore(
        &self,
        request: &MariadbRestoreRequest,
    ) -> Result<MariadbRestoreResult, ExternalServiceError> {
        info!(
            service = %request.service_config.name,
            service_id = request.service_id,
            node = %self.node_name,
            bucket = %request.source.bucket,
            pitr = request.recovery_target.is_some(),
            "Restoring a MariaDB service on its remote node"
        );
        self.agent_post_with_timeout(
            "/agent/services/mariadb/restore",
            request,
            DATA_OPERATION_TIMEOUT,
        )
        .await
    }

    /// Ship a worker's MariaDB binary logs to its S3 source with the
    /// control plane's own archiver, run by the node's agent.
    pub async fn mariadb_binlog_archive(
        &self,
        request: &MariadbBinlogArchiveRequest,
    ) -> Result<MariadbBinlogArchiveResult, ExternalServiceError> {
        self.agent_post_with_timeout(
            "/agent/services/mariadb/binlog-archive",
            request,
            DATA_OPERATION_TIMEOUT,
        )
        .await
    }

    /// Run the provider's authenticated health probe inside the worker that
    /// owns the service. Secrets are carried only in the sealed JSON
    /// request body and are never included in URL or tracing fields.
    pub async fn probe_health(
        &self,
        request: RemoteHealthProbeRequest,
    ) -> Result<RemoteHealthProbeResponse, ExternalServiceError> {
        info!(
            service = %request.service_config.name,
            node = %self.node_name,
            "Probing external service health on remote node"
        );
        let mut response: RemoteHealthProbeResponse = self
            .agent_post_with_timeout(
                "/agent/services/health-probe",
                &request,
                HEALTH_PROBE_TIMEOUT,
            )
            .await?;
        if let Some(error) = response.result.error_message.as_mut() {
            truncate_utf8(error, MAX_HEALTH_ERROR_BYTES);
        }
        Ok(response)
    }

    /// Inspect a database container and its image on the remote node, for
    /// the shadow pass and the pending changes of a recreate (ADR-055
    /// Phase 4). An agent from before Phase 4 answers
    /// [`ServiceWorkloadError::AgentUnsupported`].
    pub async fn service_snapshot(
        &self,
        container_name: &str,
    ) -> Result<crate::workload::runtime::ContainerSnapshot, ServiceWorkloadError> {
        let path = format!("/agent/services/{}/snapshot", container_name);
        self.workload_call(
            self.client.get(format!("{}{}", self.agent_url, path)),
            &path,
            container_name,
            "read a database container snapshot",
            "inspect",
            WORKLOAD_SNAPSHOT_TIMEOUT,
        )
        .await
    }

    /// The lifecycle state of a service container on the remote node (status,
    /// restarts, OOM kill, applied limits). An agent from before this route
    /// answers [`AgentSupport::Unsupported`].
    pub async fn service_runtime(
        &self,
        container_name: &str,
    ) -> Result<AgentSupport<ServiceContainerRuntime>, ExternalServiceError> {
        let path = format!("/agent/services/{}/runtime", container_name);
        let request = self
            .client
            .get(format!("{}{}", self.agent_url, path))
            .timeout(OBSERVE_TIMEOUT);
        self.agent_call_if_supported(request, &path).await
    }

    /// Raw one-shot stats samples of a service container on the remote node,
    /// oldest first, a second apart (`samples` is 1 or 2). An agent from
    /// before this route answers [`AgentSupport::Unsupported`].
    pub async fn service_stats(
        &self,
        container_name: &str,
        samples: usize,
    ) -> Result<AgentSupport<ServiceContainerStats>, ExternalServiceError> {
        let path = format!("/agent/services/{}/stats", container_name);
        let samples = samples.clamp(1, temps_service_protocol::MAX_SERVICE_STATS_SAMPLES);
        let request = self
            .client
            .get(format!("{}{}?samples={}", self.agent_url, path, samples))
            .timeout(OBSERVE_TIMEOUT);
        self.agent_call_if_supported(request, &path).await
    }

    /// Run the Postgres WAL and archive probe on the node that owns the
    /// service. The service config holds its password, so the body is
    /// sealed to the node. An agent from before this route answers
    /// [`AgentSupport::Unsupported`].
    pub async fn probe_wal_health(
        &self,
        request: RemoteWalHealthRequest,
    ) -> Result<AgentSupport<RemoteWalHealthResponse>, ExternalServiceError> {
        info!(
            service = %request.service_config.name,
            node = %self.node_name,
            "Probing Postgres WAL health on remote node"
        );
        let path = "/agent/services/wal-health";
        let builder = self
            .client
            .post(format!("{}{}", self.agent_url, path))
            .timeout(WAL_PROBE_TIMEOUT);
        let builder = self
            .with_sealed_body(builder, path, &request)
            .map_err(|error| ExternalServiceError::InternalError {
                reason: error.to_string(),
            })?;
        let mut response: AgentSupport<RemoteWalHealthResponse> =
            self.agent_call_if_supported(builder, path).await?;
        if let AgentSupport::Supported(RemoteWalHealthResponse {
            error: Some(error), ..
        }) = &mut response
        {
            truncate_utf8(error, MAX_HEALTH_ERROR_BYTES);
        }
        Ok(response)
    }

    /// Send `builder` with this client's token. A route the agent does not
    /// have (`404`, or `405` where the path matches another route's pattern)
    /// is [`AgentSupport::Unsupported`]; any other failure is an error.
    async fn agent_call_if_supported<T: for<'de> Deserialize<'de>>(
        &self,
        builder: reqwest::RequestBuilder,
        path: &str,
    ) -> Result<AgentSupport<T>, ExternalServiceError> {
        let url = format!("{}{}", self.agent_url, path);
        let response = builder.bearer_auth(&self.token).send().await.map_err(|e| {
            ExternalServiceError::InternalError {
                reason: format!(
                    "Failed to reach agent on node {} at {}: {}",
                    self.node_name, url, e
                ),
            }
        })?;
        if matches!(
            response.status(),
            reqwest::StatusCode::NOT_FOUND | reqwest::StatusCode::METHOD_NOT_ALLOWED
        ) {
            return Ok(AgentSupport::Unsupported);
        }
        // Only a sealed request gets this: the agent predates sealing.
        if response.status() == reqwest::StatusCode::UNSUPPORTED_MEDIA_TYPE {
            return Err(self.older_agent_error());
        }
        let (status, body): (_, AgentResponse<T>) =
            self.decode_agent_response(response, &url).await?;
        if !body.success {
            let mut error = body.error.unwrap_or_default();
            truncate_utf8(&mut error, MAX_HEALTH_ERROR_BYTES);
            return Err(ExternalServiceError::InternalError {
                reason: format!(
                    "Agent on node {} returned error ({}): {}",
                    self.node_name, status, error
                ),
            });
        }
        body.data
            .map(AgentSupport::Supported)
            .ok_or_else(|| ExternalServiceError::InternalError {
                reason: format!(
                    "Agent on node {} returned success but no data at {}",
                    self.node_name, url
                ),
            })
    }

    /// The node this client talks to, for messages.
    pub fn node_name(&self) -> &str {
        &self.node_name
    }

    /// Create (or, with `replace_existing`, recreate) a database container
    /// from its admitted spec on the remote node.
    pub async fn create_service_workload(
        &self,
        request: &ServiceWorkloadCreateRequest,
    ) -> Result<RemoteServiceCreateResponse, ServiceWorkloadError> {
        info!(
            service = %request.service_name,
            container = %request.plan.container_name,
            node = %self.node_name,
            replace_existing = request.replace_existing,
            "Creating database container from its workload spec on remote node"
        );
        let path = "/agent/services/workload";
        let builder = self.client.post(format!("{}{}", self.agent_url, path));
        let builder = self
            .with_sealed_body(builder, path, request)
            .map_err(|error| ServiceWorkloadError::Agent {
                node: self.node_name.clone(),
                operation: "create",
                container: request.plan.container_name.clone(),
                reason: error.to_string(),
            })?;
        self.workload_call(
            builder,
            path,
            &request.plan.container_name,
            "create a database container from its workload spec",
            "create",
            WORKLOAD_CREATE_TIMEOUT,
        )
        .await
    }

    async fn workload_call<T: for<'de> Deserialize<'de>>(
        &self,
        builder: reqwest::RequestBuilder,
        path: &str,
        container: &str,
        unsupported: &'static str,
        operation: &'static str,
        timeout: Duration,
    ) -> Result<T, ServiceWorkloadError> {
        let url = format!("{}{}", self.agent_url, path);
        let agent_error = |reason: String| ServiceWorkloadError::Agent {
            node: self.node_name.clone(),
            operation,
            container: container.to_string(),
            reason,
        };
        let response = builder
            .timeout(timeout)
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| agent_error(format!("failed to reach the agent at {url}: {e}")))?;
        // Only a sealed request gets this: every plain one here is JSON or GET.
        if response.status() == reqwest::StatusCode::UNSUPPORTED_MEDIA_TYPE {
            return Err(agent_error(
                temps_core::sealed_request::older_agent_message(&self.node_name),
            ));
        }
        // An agent from before Phase 4 has no such route: `404`, or `405`
        // where the path matches `/agent/services/{name}`.
        if matches!(
            response.status(),
            reqwest::StatusCode::NOT_FOUND | reqwest::StatusCode::METHOD_NOT_ALLOWED
        ) {
            return Err(ServiceWorkloadError::AgentUnsupported {
                node: self.node_name.clone(),
                operation: unsupported,
            });
        }
        let (status, body): (_, AgentResponse<T>) = self
            .decode_agent_response(response, &url)
            .await
            .map_err(|e| agent_error(e.to_string()))?;
        if !body.success {
            return Err(agent_error(format!(
                "the agent answered {status}: {}",
                body.error.unwrap_or_default()
            )));
        }
        body.data
            .ok_or_else(|| agent_error(format!("the agent answered {status} with no data")))
    }

    // -----------------------------------------------------------------------
    // HTTP helpers
    // -----------------------------------------------------------------------

    async fn agent_get<T: for<'de> Deserialize<'de>>(
        &self,
        path: &str,
    ) -> Result<T, ExternalServiceError> {
        let url = format!("{}{}", self.agent_url, path);
        let response = self
            .client
            .get(&url)
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!(
                    "Failed to reach agent on node {} at {}: {}",
                    self.node_name, url, e
                ),
            })?;

        let (status, body) = self.decode_agent_response(response, &url).await?;

        if !body.success {
            let err_msg = body.error.unwrap_or_default();
            error!(
                "Agent on node {} returned error ({}) at {}: {}",
                self.node_name, status, url, err_msg
            );
            return Err(ExternalServiceError::InternalError {
                reason: format!(
                    "Agent on node {} returned error ({}): {}",
                    self.node_name, status, err_msg
                ),
            });
        }

        body.data
            .ok_or_else(|| ExternalServiceError::InternalError {
                reason: format!(
                    "Agent on node {} returned success but no data at {}",
                    self.node_name, url
                ),
            })
    }

    /// POST `body` encrypted to the node. Every route this client calls with a
    /// body can carry a credential somewhere in it, so none of them decide by
    /// looking at one field.
    async fn agent_post_sealed<B: serde::Serialize, T: for<'de> Deserialize<'de>>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, ExternalServiceError> {
        let url = format!("{}{}", self.agent_url, path);
        let request = self.client.post(&url).bearer_auth(&self.token);
        let request = self
            .with_sealed_body(request, path, body)
            .map_err(|error| ExternalServiceError::InternalError {
                reason: error.to_string(),
            })?;
        let response = request
            .send()
            .await
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!(
                    "Failed to reach agent on node {} at {}: {}",
                    self.node_name, url, e
                ),
            })?;
        if response.status() == reqwest::StatusCode::UNSUPPORTED_MEDIA_TYPE {
            return Err(self.older_agent_error());
        }

        let (status, body) = self.decode_agent_response(response, &url).await?;

        if !body.success {
            let err_msg = body.error.unwrap_or_default();
            error!(
                "Agent on node {} returned error ({}) at {}: {}",
                self.node_name, status, url, err_msg
            );
            return Err(ExternalServiceError::InternalError {
                reason: format!(
                    "Agent on node {} returned error ({}): {}",
                    self.node_name, status, err_msg
                ),
            });
        }

        body.data
            .ok_or_else(|| ExternalServiceError::InternalError {
                reason: format!(
                    "Agent on node {} returned success but no data at {}",
                    self.node_name, url
                ),
            })
    }

    async fn agent_post_with_timeout<B: serde::Serialize, T: for<'de> Deserialize<'de>>(
        &self,
        path: &str,
        body: &B,
        timeout: Duration,
    ) -> Result<T, ExternalServiceError> {
        let url = format!("{}{}", self.agent_url, path);
        // The health probe's service config holds the service's credentials.
        let response = self
            .with_sealed_body(
                self.client
                    .post(&url)
                    .timeout(timeout)
                    .bearer_auth(&self.token),
                path,
                body,
            )
            .map_err(|error| ExternalServiceError::InternalError {
                reason: error.to_string(),
            })?
            .send()
            .await
            .map_err(|error| {
                // Not connected: the request never reached the agent.
                if error.is_connect() {
                    return ExternalServiceError::InternalError {
                        reason: format!(
                            "Failed to reach agent on node {} at {}: {}",
                            self.node_name, url, error
                        ),
                    };
                }
                // Sent, then a timeout or a dropped connection: the agent may
                // have started the operation, and may still be running it.
                ExternalServiceError::AgentUnanswered {
                    node: self.node_name.clone(),
                    route: path.to_string(),
                    detail: format!("{url}: {error}"),
                }
            })?;
        if response.status() == reqwest::StatusCode::UNSUPPORTED_MEDIA_TYPE {
            return Err(self.older_agent_error());
        }
        if matches!(
            response.status(),
            reqwest::StatusCode::NOT_FOUND | reqwest::StatusCode::METHOD_NOT_ALLOWED
        ) {
            return Err(ExternalServiceError::InternalError {
                reason: format!(
                    "The agent on node {} has no {path} route: it predates this server. \
                     Update the node's agent to this release",
                    self.node_name
                ),
            });
        }
        let (status, body): (_, AgentResponse<T>) =
            self.decode_agent_response(response, &url).await?;
        if !body.success {
            let mut error = body.error.unwrap_or_default();
            truncate_utf8(&mut error, MAX_HEALTH_ERROR_BYTES);
            // The node runs its maximum of backups and restores: nothing ran,
            // and the caller decides whether to wait for a slot.
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                return Err(ExternalServiceError::NodeBusy {
                    node: self.node_name.clone(),
                    route: path.to_string(),
                    detail: error,
                });
            }
            return Err(ExternalServiceError::InternalError {
                reason: format!(
                    "Agent on node {} returned error ({}): {}",
                    self.node_name, status, error
                ),
            });
        }
        body.data
            .ok_or_else(|| ExternalServiceError::InternalError {
                reason: format!(
                    "Agent on node {} returned success but no data at {}",
                    self.node_name, url
                ),
            })
    }

    async fn agent_post_no_body<T: for<'de> Deserialize<'de>>(
        &self,
        path: &str,
    ) -> Result<T, ExternalServiceError> {
        let url = format!("{}{}", self.agent_url, path);
        let response = self
            .client
            .post(&url)
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!(
                    "Failed to reach agent on node {} at {}: {}",
                    self.node_name, url, e
                ),
            })?;

        let (status, body) = self.decode_agent_response(response, &url).await?;

        if !body.success {
            let err_msg = body.error.unwrap_or_default();
            return Err(ExternalServiceError::InternalError {
                reason: format!(
                    "Agent on node {} returned error ({}): {}",
                    self.node_name, status, err_msg
                ),
            });
        }

        body.data
            .ok_or_else(|| ExternalServiceError::InternalError {
                reason: format!(
                    "Agent on node {} returned success but no data at {}",
                    self.node_name, url
                ),
            })
    }

    async fn agent_delete(&self, path: &str) -> Result<(), ExternalServiceError> {
        let url = format!("{}{}", self.agent_url, path);
        let response = self
            .client
            .delete(&url)
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!(
                    "Failed to reach agent on node {} at {}: {}",
                    self.node_name, url, e
                ),
            })?;

        let (_, body): (_, AgentResponse<serde_json::Value>) =
            self.decode_agent_response(response, &url).await?;

        if !body.success {
            let err_msg = body.error.unwrap_or_default();
            return Err(ExternalServiceError::InternalError {
                reason: format!(
                    "Agent on node {} returned error: {}",
                    self.node_name, err_msg
                ),
            });
        }

        Ok(())
    }

    async fn decode_agent_response<T: for<'de> Deserialize<'de>>(
        &self,
        response: reqwest::Response,
        url: &str,
    ) -> Result<(reqwest::StatusCode, AgentResponse<T>), ExternalServiceError> {
        let status = response.status();
        if response
            .content_length()
            .is_some_and(|length| length > MAX_AGENT_RESPONSE_BYTES as u64)
        {
            return Err(ExternalServiceError::InternalError {
                reason: format!(
                    "Response from node {} at {} exceeded {} bytes",
                    self.node_name, url, MAX_AGENT_RESPONSE_BYTES
                ),
            });
        }

        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| ExternalServiceError::InternalError {
                reason: format!(
                    "Failed reading response from node {} at {}: {}",
                    self.node_name, url, error
                ),
            })?;
            if bytes.len().saturating_add(chunk.len()) > MAX_AGENT_RESPONSE_BYTES {
                return Err(ExternalServiceError::InternalError {
                    reason: format!(
                        "Response from node {} at {} exceeded {} bytes",
                        self.node_name, url, MAX_AGENT_RESPONSE_BYTES
                    ),
                });
            }
            bytes.extend_from_slice(&chunk);
        }

        let body = serde_json::from_slice(&bytes).map_err(|error| {
            ExternalServiceError::InternalError {
                reason: format!(
                    "Invalid response from node {} at {}: {}",
                    self.node_name, url, error
                ),
            }
        })?;
        Ok((status, body))
    }
}

fn truncate_utf8(value: &mut String, max_bytes: usize) {
    if value.len() <= max_bytes {
        return;
    }
    let mut boundary = max_bytes;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value.truncate(boundary);
    value.push('…');
}

#[cfg(test)]
mod observe_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::externalsvc::{HealthProbeStatus, ServiceConfig, ServiceType};
    use axum::{
        extract::Json,
        http::{header::AUTHORIZATION, HeaderMap},
        routing::post,
        Router,
    };
    use temps_core::sealed_request::{open_request, NodeSealing, SEALED_REQUEST_CONTENT_TYPE};

    /// The fake worker's key. Handlers are plain `async fn`s, so it is shared.
    fn test_keys() -> &'static (String, String) {
        static KEYS: std::sync::OnceLock<(String, String)> = std::sync::OnceLock::new();
        KEYS.get_or_init(|| temps_core::worker_ingress::generate_ingress_key().unwrap())
    }

    fn test_sealing() -> NodeSealing {
        NodeSealing::Key(test_keys().1.clone())
    }

    /// Open a request as the worker's agent would, checking it was sealed.
    fn open_for_test<T: for<'de> Deserialize<'de>>(
        headers: &HeaderMap,
        path: &str,
        sealed: &[u8],
    ) -> T {
        assert_eq!(
            headers
                .get(axum::http::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some(SEALED_REQUEST_CONTENT_TYPE)
        );
        let opened = open_request(&test_keys().0, path, sealed).unwrap();
        serde_json::from_slice(&opened).unwrap()
    }

    /// The content type and raw body a fake worker last received.
    type RecordedRequest = std::sync::Arc<std::sync::Mutex<Option<(String, Vec<u8>)>>>;

    /// A fake worker on every route, answering `status` with `reply` and
    /// recording the raw body and content type it received.
    async fn recording_agent(
        status: axum::http::StatusCode,
        reply: serde_json::Value,
    ) -> (String, RecordedRequest) {
        let seen: RecordedRequest = Default::default();
        let recorded = seen.clone();
        let app = Router::new().fallback(move |headers: HeaderMap, body: axum::body::Bytes| {
            let recorded = recorded.clone();
            let reply = reply.clone();
            async move {
                let content_type = headers
                    .get(axum::http::header::CONTENT_TYPE)
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default()
                    .to_string();
                *recorded.lock().unwrap() = Some((content_type, body.to_vec()));
                (status, Json(reply))
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{address}"), seen)
    }

    fn client_at(url: String, sealing: NodeSealing) -> RemoteServiceClient {
        RemoteServiceClient::new(url, "node-token".to_string(), "worker-1".to_string())
            .unwrap()
            .with_sealing(sealing)
    }

    fn create_params(environment: HashMap<String, String>) -> RemoteServiceCreateParams {
        RemoteServiceCreateParams {
            name: "orders-db".to_string(),
            service_type: "postgres".to_string(),
            image: "postgres:18".to_string(),
            environment,
            port_mappings: vec![],
            volumes: HashMap::new(),
            network: None,
            command: None,
            resource_limits: None,
        }
    }

    fn password_env() -> HashMap<String, String> {
        HashMap::from([("POSTGRES_PASSWORD".to_string(), "db-password".to_string())])
    }

    #[tokio::test]
    async fn a_service_environment_crosses_the_wire_encrypted() {
        let (url, seen) = recording_agent(
            axum::http::StatusCode::OK,
            serde_json::json!({"success": false, "data": null, "error": "stop here"}),
        )
        .await;

        let _ = client_at(url, test_sealing())
            .create_service(create_params(password_env()))
            .await;

        let (content_type, body) = seen.lock().unwrap().take().unwrap();
        assert_eq!(content_type, SEALED_REQUEST_CONTENT_TYPE);
        let wire = String::from_utf8_lossy(&body);
        assert!(!wire.contains("db-password"), "{wire}");
        assert!(!wire.contains("POSTGRES_PASSWORD"), "{wire}");
        let opened = open_request(&test_keys().0, "/agent/services", &body).unwrap();
        assert!(String::from_utf8(opened).unwrap().contains("db-password"));
    }

    #[tokio::test]
    async fn a_service_environment_is_not_sent_to_a_node_without_a_key() {
        for (sealing, expected) in [
            (NodeSealing::NoKey, "no encryption key"),
            (NodeSealing::PlainHttp, "plain http://"),
        ] {
            let error = client_at("http://127.0.0.1:9".to_string(), sealing)
                .create_service(create_params(password_env()))
                .await
                .unwrap_err()
                .to_string();
            assert!(error.contains(expected), "{error}");
            assert!(error.contains("worker-1"), "{error}");
        }
    }

    /// A Redis password is only in the command, the environment is empty.
    fn redis_params() -> RemoteServiceCreateParams {
        let mut params = create_params(HashMap::new());
        params.command = Some(vec![
            "redis-server".to_string(),
            "--requirepass".to_string(),
            "db-password".to_string(),
        ]);
        params
    }

    #[tokio::test]
    async fn a_password_in_a_database_command_crosses_the_wire_encrypted() {
        let (url, seen) = recording_agent(
            axum::http::StatusCode::OK,
            serde_json::json!({"success": false, "data": null, "error": "stop here"}),
        )
        .await;

        let _ = client_at(url, test_sealing())
            .create_service(redis_params())
            .await;

        let (content_type, body) = seen.lock().unwrap().take().unwrap();
        assert_eq!(content_type, SEALED_REQUEST_CONTENT_TYPE);
        assert!(!String::from_utf8_lossy(&body).contains("db-password"));
        let opened = open_request(&test_keys().0, "/agent/services", &body).unwrap();
        assert!(String::from_utf8(opened).unwrap().contains("db-password"));
    }

    #[tokio::test]
    async fn a_password_in_a_database_command_is_not_sent_to_a_node_without_a_key() {
        for (sealing, expected) in [
            (NodeSealing::NoKey, "no encryption key"),
            (NodeSealing::PlainHttp, "plain http://"),
        ] {
            let error = client_at("http://127.0.0.1:9".to_string(), sealing)
                .create_service(redis_params())
                .await
                .unwrap_err()
                .to_string();
            assert!(error.contains(expected), "{error}");
        }
    }

    #[tokio::test]
    async fn a_database_workload_is_sealed_even_with_no_secret_named_in_its_request() {
        use crate::workload::{DatabaseEngine, DatabaseIdentity, DatabaseProcess};
        let (url, seen) = recording_agent(
            axum::http::StatusCode::OK,
            serde_json::json!({"success": false, "data": null, "error": "stop here"}),
        )
        .await;
        let mut params = create_params(HashMap::new());
        params.port_mappings = vec![RemotePortMapping {
            host_port: 0,
            container_port: 5432,
        }];
        params.volumes = HashMap::from([(
            "orders-db-data".to_string(),
            "/var/lib/postgresql/data".to_string(),
        )]);
        let process = DatabaseProcess::from_remote_params(DatabaseEngine::Postgres, &params);
        let plan = crate::workload::DatabaseWorkloadPlan::admit(
            &DatabaseIdentity {
                service_id: 1,
                service_name: "orders-db".to_string(),
                engine: DatabaseEngine::Postgres,
                node_id: Some(1),
                container_name: "orders-db".to_string(),
            },
            &process,
        )
        .unwrap();

        let _ = client_at(url, test_sealing())
            .create_service_workload(&ServiceWorkloadCreateRequest {
                service_name: "orders-db".to_string(),
                plan,
                secrets: Default::default(),
                network: None,
                replace_existing: false,
            })
            .await;

        let (content_type, body) = seen.lock().unwrap().take().unwrap();
        assert_eq!(content_type, SEALED_REQUEST_CONTENT_TYPE);
        assert!(open_request(&test_keys().0, "/agent/services/workload", &body).is_ok());
    }

    #[derive(Clone, Default)]
    struct CapturedLogs(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for CapturedLogs {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturedLogs {
        type Writer = Self;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    /// Keep a second tracing dispatcher registered for the life of the test
    /// process, so a capture's scoped subscriber is never the only one.
    ///
    /// While exactly one dispatcher is registered, `tracing-core` computes the
    /// interest of a callsite that registers for the first time from the
    /// *registering thread's* default subscriber, and caches it for every
    /// thread. Other tests here call `exec_in_service` on threads with no
    /// subscriber: when one of them registers its `info!` while the capture
    /// below is the lone dispatcher, the cached answer is "never" and the
    /// capture records nothing. With a second dispatcher registered,
    /// `tracing-core` combines every registered dispatcher, under its lock,
    /// and each event is checked against the emitting thread's subscriber.
    fn keep_dispatchers_from_being_alone() {
        static SECOND_DISPATCHER: std::sync::OnceLock<tracing::Dispatch> =
            std::sync::OnceLock::new();
        SECOND_DISPATCHER
            .get_or_init(|| tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default()));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_credential_in_a_service_exec_command_is_not_logged() {
        let (url, _seen) = recording_agent(
            axum::http::StatusCode::OK,
            serde_json::json!({"success": false, "data": null, "error": "stop here"}),
        )
        .await;
        let client = client_at(url, test_sealing());
        keep_dispatchers_from_being_alone();
        let logs = CapturedLogs::default();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_writer(logs.clone())
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);
        // Recompute what a registration already in flight on another thread
        // cached against a lone dispatcher before the capture was registered.
        tracing::callsite::rebuild_interest_cache();

        let _ = client
            .exec_in_service(RemoteExecParams {
                container_name: "redis-1".to_string(),
                command: vec!["redis-cli".into(), "-a".into(), "db-password".into()],
                environment: HashMap::new(),
                user: None,
                detach: false,
            })
            .await;

        let captured = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
        assert!(captured.contains("redis-1"), "logs: {captured}");
        assert!(captured.contains("command_argc"), "logs: {captured}");
        assert!(!captured.contains("db-password"), "logs: {captured}");
    }

    #[tokio::test]
    async fn a_credential_in_a_service_exec_command_crosses_the_wire_encrypted() {
        let (url, seen) = recording_agent(
            axum::http::StatusCode::OK,
            serde_json::json!({"success": false, "data": null, "error": "stop here"}),
        )
        .await;

        let _ = client_at(url, test_sealing())
            .exec_in_service(RemoteExecParams {
                container_name: "redis-1".to_string(),
                command: vec!["redis-cli".into(), "-a".into(), "db-password".into()],
                environment: HashMap::new(),
                user: None,
                detach: false,
            })
            .await;

        let (content_type, body) = seen.lock().unwrap().take().unwrap();
        assert_eq!(content_type, SEALED_REQUEST_CONTENT_TYPE);
        assert!(!String::from_utf8_lossy(&body).contains("db-password"));
        assert!(open_request(&test_keys().0, "/agent/services/exec", &body).is_ok());
    }

    #[tokio::test]
    async fn an_older_agent_gets_an_upgrade_message() {
        let (url, _) = recording_agent(
            axum::http::StatusCode::UNSUPPORTED_MEDIA_TYPE,
            serde_json::json!({}),
        )
        .await;
        let error = client_at(url, test_sealing())
            .create_service(create_params(password_env()))
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("too old"), "{error}");
        assert!(error.contains("worker-1"), "{error}");
    }

    #[tokio::test]
    async fn service_credentials_in_a_health_probe_are_not_sent_to_a_node_without_a_key() {
        let error = client_at("http://127.0.0.1:9".to_string(), NodeSealing::NoKey)
            .probe_health(RemoteHealthProbeRequest {
                service_config: ServiceConfig {
                    name: "orders-db".to_string(),
                    service_type: ServiceType::Postgres,
                    version: None,
                    parameters: serde_json::json!({"password": "db-password"}),
                },
            })
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("no encryption key"), "{error}");
    }

    #[test]
    fn test_remote_service_create_params_serialization() {
        let params = RemoteServiceCreateParams {
            name: "postgres-main".to_string(),
            service_type: "postgres".to_string(),
            image: "gotempsh/postgres-walg:18-bookworm".to_string(),
            environment: HashMap::from([
                ("POSTGRES_PASSWORD".to_string(), "secret".to_string()),
                ("POSTGRES_DB".to_string(), "mydb".to_string()),
            ]),
            port_mappings: vec![RemotePortMapping {
                host_port: 30001,
                container_port: 5432,
            }],
            volumes: HashMap::from([(
                "postgres-main_data".to_string(),
                "/var/lib/postgresql".to_string(),
            )]),
            network: Some("temps".to_string()),
            command: None,
            resource_limits: None,
        };

        let json = serde_json::to_string(&params).unwrap();
        assert!(json.contains("postgres-main"));
        assert!(json.contains("30001"));
        assert!(!json.contains("command")); // None fields skipped
        assert!(!json.contains("resource_limits")); // None field skipped
    }

    #[test]
    fn health_error_limit_preserves_utf8_boundary() {
        let mut error = "é".repeat(MAX_HEALTH_ERROR_BYTES);
        truncate_utf8(&mut error, MAX_HEALTH_ERROR_BYTES);
        assert!(error.is_char_boundary(error.len()));
        assert!(error.len() <= MAX_HEALTH_ERROR_BYTES + '…'.len_utf8());
    }

    #[test]
    fn mtls_client_rejects_invalid_control_plane_identity() {
        let error = RemoteServiceClient::new_mtls(
            "https://worker.internal:3100".to_string(),
            "node-token".to_string(),
            "worker-1".to_string(),
            "not-a-pem-identity",
            "not-reached",
        )
        .err()
        .expect("invalid identity must be rejected");

        assert!(error
            .to_string()
            .contains("Failed to parse control-plane mTLS identity for node worker-1"));
    }

    #[test]
    fn mtls_client_rejects_invalid_cluster_ca() {
        let ca = temps_core::node_pki::generate_cluster_ca().expect("generate test CA");
        let csr = temps_core::node_pki::generate_node_keypair_csr(
            "temps-control-plane",
            &["temps-control-plane".to_string()],
        )
        .expect("generate control-plane identity");
        let signed = temps_core::node_pki::sign_node_csr(
            &ca.cert_pem,
            &ca.key_pem,
            &csr.csr_pem,
            &["temps-control-plane".to_string()],
        )
        .expect("sign control-plane identity");
        let identity_pem = format!("{}{}", signed.cert_pem, csr.key_pem);

        let error = RemoteServiceClient::new_mtls(
            "https://worker.internal:3100".to_string(),
            "node-token".to_string(),
            "worker-1".to_string(),
            &identity_pem,
            "not-a-pem-certificate",
        )
        .err()
        .expect("invalid cluster CA must be rejected");

        assert!(error
            .to_string()
            .contains("Failed to parse cluster CA certificate for node worker-1"));
    }

    #[test]
    fn a_mesh_endpoint_builds_a_client_for_the_mesh_address() {
        let ca = temps_core::node_pki::generate_cluster_ca().expect("generate test CA");
        let csr = temps_core::node_pki::generate_node_keypair_csr("temps-control-plane", &[])
            .expect("generate control-plane identity");
        let signed =
            temps_core::node_pki::sign_node_csr(&ca.cert_pem, &ca.key_pem, &csr.csr_pem, &[])
                .expect("sign control-plane identity");
        let identity_pem = format!("{}\n{}", signed.cert_pem, csr.key_pem);
        let endpoint = temps_core::node_address::NodeAgentEndpoint {
            dial_url: "https://10.201.0.5:3100".to_string(),
            server_name: Some("203.0.113.10".to_string()),
        };

        let client = RemoteServiceClient::new_mtls_at(
            &endpoint,
            "node-token".to_string(),
            "worker-1".to_string(),
            &identity_pem,
            &ca.cert_pem,
        )
        .expect("the preconfigured TLS client builds");
        assert_eq!(client.agent_url, "https://10.201.0.5:3100");

        let error = RemoteServiceClient::new_mtls_at(
            &endpoint,
            "node-token".to_string(),
            "worker-1".to_string(),
            &identity_pem,
            "not-a-pem-certificate",
        )
        .err()
        .expect("invalid cluster CA must be rejected");
        assert!(error
            .to_string()
            .contains("worker-1 at https://10.201.0.5:3100"));
    }

    #[test]
    fn runtime_env_request_preserves_provider_config_and_tenant_identity() {
        let request = RemoteRuntimeEnvRequest {
            service_config: ServiceConfig {
                name: "orders-db".to_string(),
                service_type: ServiceType::Postgres,
                version: Some("18".to_string()),
                parameters: serde_json::json!({
                    "host": "localhost",
                    "port": "5432",
                    "password": "secret"
                }),
            },
            project_slug: "storefront".to_string(),
            environment_slug: "production".to_string(),
        };

        let encoded = serde_json::to_value(&request).unwrap();
        assert_eq!(encoded["service_config"]["name"], "orders-db");
        assert_eq!(encoded["service_config"]["service_type"], "postgres");
        assert_eq!(encoded["service_config"]["parameters"]["port"], "5432");
        assert_eq!(encoded["project_slug"], "storefront");
        assert_eq!(encoded["environment_slug"], "production");
    }

    #[tokio::test]
    async fn runtime_env_posts_to_agent_and_returns_typed_environment() {
        async fn handler(headers: HeaderMap, sealed: axum::body::Bytes) -> Json<serde_json::Value> {
            assert_eq!(
                headers
                    .get(AUTHORIZATION)
                    .and_then(|value| value.to_str().ok()),
                Some("Bearer node-token")
            );
            let request: RemoteRuntimeEnvRequest =
                open_for_test(&headers, "/agent/services/runtime-env", &sealed);
            assert_eq!(request.service_config.name, "orders-db");
            assert_eq!(request.project_slug, "storefront");
            assert_eq!(request.environment_slug, "production");
            Json(serde_json::json!({
                "success": true,
                "data": {
                    "environment": {
                        "DATABASE_URL": "postgres://app:secret@postgres-orders-db:5432/storefront_production"
                    }
                },
                "error": null
            }))
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route("/agent/services/runtime-env", post(handler)),
            )
            .await
            .unwrap();
        });

        let client = RemoteServiceClient::new(
            format!("http://{address}"),
            "node-token".to_string(),
            "worker-1".to_string(),
        )
        .unwrap()
        .with_sealing(test_sealing());
        let response = client
            .get_runtime_env_vars(RemoteRuntimeEnvRequest {
                service_config: ServiceConfig {
                    name: "orders-db".to_string(),
                    service_type: ServiceType::Postgres,
                    version: None,
                    parameters: serde_json::json!({"host": "localhost"}),
                },
                project_slug: "storefront".to_string(),
                environment_slug: "production".to_string(),
            })
            .await
            .unwrap();

        assert_eq!(
            response.environment.get("DATABASE_URL").map(String::as_str),
            Some("postgres://app:secret@postgres-orders-db:5432/storefront_production")
        );
        server.abort();
    }

    /// The console's per-environment preview of a worker service reaches the
    /// agent's preview route, sealed, never the provisioning one.
    #[tokio::test]
    async fn a_runtime_preview_is_sealed_to_the_agents_preview_route() {
        async fn handler(headers: HeaderMap, sealed: axum::body::Bytes) -> Json<serde_json::Value> {
            let request: RemoteRuntimeEnvRequest =
                open_for_test(&headers, "/agent/services/runtime-env-preview", &sealed);
            assert_eq!(request.service_config.name, "orders-db");
            assert_eq!(request.project_slug, "storefront");
            Json(serde_json::json!({
                "success": true,
                "data": {"environment": {"POSTGRES_DATABASE": "storefront_production"}},
                "error": null
            }))
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route("/agent/services/runtime-env-preview", post(handler)),
            )
            .await
            .unwrap();
        });

        let client = RemoteServiceClient::new(
            format!("http://{address}"),
            "node-token".to_string(),
            "worker-1".to_string(),
        )
        .unwrap()
        .with_sealing(test_sealing());
        let response = client
            .preview_runtime_env_vars(RemoteRuntimeEnvRequest {
                service_config: ServiceConfig {
                    name: "orders-db".to_string(),
                    service_type: ServiceType::Postgres,
                    version: None,
                    parameters: serde_json::json!({"host": "localhost", "password": "secret"}),
                },
                project_slug: "storefront".to_string(),
                environment_slug: "production".to_string(),
            })
            .await
            .unwrap();

        assert_eq!(
            response
                .environment
                .get("POSTGRES_DATABASE")
                .map(String::as_str),
            Some("storefront_production")
        );
        server.abort();
    }

    fn worker_destination() -> WorkerS3Destination {
        WorkerS3Destination {
            bucket: "backups".to_string(),
            region: "us-east-1".to_string(),
            endpoint: Some("https://s3.example.test".to_string()),
            force_path_style: true,
            access_key_id: "AKIAEXAMPLE".to_string(),
            secret_access_key: "destination-secret".to_string(),
            session_token: Some("destination-session".to_string()),
        }
    }

    /// A worker backup and restore reach the agent sealed, with the node
    /// token, and nothing secret is readable on the wire.
    #[tokio::test]
    async fn worker_backups_and_restores_are_sealed_to_the_node() {
        async fn backup(headers: HeaderMap, sealed: axum::body::Bytes) -> Json<serde_json::Value> {
            assert_eq!(
                headers
                    .get(AUTHORIZATION)
                    .and_then(|value| value.to_str().ok()),
                Some("Bearer node-token")
            );
            let wire = String::from_utf8_lossy(&sealed);
            assert!(!wire.contains("destination-secret"));
            assert!(!wire.contains("cache-password"));
            let request: RemoteServiceBackupRequest =
                open_for_test(&headers, "/agent/services/data-backup", &sealed);
            assert_eq!(request.service_config.name, "cache");
            assert_eq!(request.key, "redis/cache/u1/dump.rdb.gz");
            assert_eq!(
                request.destination.session_token.as_deref(),
                Some("destination-session")
            );
            Json(serde_json::json!({
                "success": true,
                "data": { "size_bytes": 4096, "buckets": [] },
                "error": null
            }))
        }
        async fn restore(headers: HeaderMap, sealed: axum::body::Bytes) -> Json<serde_json::Value> {
            let request: RemoteServiceRestoreRequest =
                open_for_test(&headers, "/agent/services/data-restore", &sealed);
            assert_eq!(request.location, "redis/cache/u1/dump.rdb.gz");
            Json(serde_json::json!({ "success": true, "data": {}, "error": null }))
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route("/agent/services/data-backup", post(backup))
                    .route("/agent/services/data-restore", post(restore)),
            )
            .await
            .unwrap();
        });
        let client = RemoteServiceClient::new(
            format!("http://{address}"),
            "node-token".to_string(),
            "worker-1".to_string(),
        )
        .unwrap()
        .with_sealing(test_sealing());
        let config = ServiceConfig {
            name: "cache".to_string(),
            service_type: ServiceType::Redis,
            version: None,
            parameters: serde_json::json!({"host": "localhost", "password": "cache-password"}),
        };
        let response = client
            .backup_service(&RemoteServiceBackupRequest {
                service_config: config.clone(),
                destination: worker_destination(),
                key: "redis/cache/u1/dump.rdb.gz".to_string(),
            })
            .await
            .unwrap();
        assert_eq!(response.size_bytes, 4096);
        client
            .restore_service(&RemoteServiceRestoreRequest {
                service_config: config,
                source: worker_destination(),
                location: "redis/cache/u1/dump.rdb.gz".to_string(),
            })
            .await
            .unwrap();
        server.abort();
    }

    /// An agent from before worker backups is named, with what to do.
    #[tokio::test]
    async fn an_agent_without_the_backup_route_is_told_to_update() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, Router::new()).await.unwrap();
        });
        let client = RemoteServiceClient::new(
            format!("http://{address}"),
            "node-token".to_string(),
            "worker-1".to_string(),
        )
        .unwrap()
        .with_sealing(test_sealing());
        let error = client
            .backup_service(&RemoteServiceBackupRequest {
                service_config: ServiceConfig {
                    name: "cache".to_string(),
                    service_type: ServiceType::Redis,
                    version: None,
                    parameters: serde_json::json!({}),
                },
                destination: worker_destination(),
                key: "k/dump.rdb.gz".to_string(),
            })
            .await
            .expect_err("an older agent cannot back up");
        server.abort();
        let message = error.to_string();
        assert!(message.contains("worker-1"), "{message}");
        assert!(message.contains("/agent/services/data-backup"), "{message}");
        assert!(message.contains("Update the node's agent"), "{message}");
    }

    /// A WAL-G base backup reaches the node sealed, with the destination's
    /// credentials and nothing the node could run, and the engine's outcome
    /// comes back as it is. An agent without the route is told to update.
    #[tokio::test]
    async fn a_walg_backup_is_sealed_to_the_node_and_its_outcome_returned() {
        async fn walg(headers: HeaderMap, sealed: axum::body::Bytes) -> Json<serde_json::Value> {
            let request: PostgresWalgBackupRequest =
                open_for_test(&headers, "/agent/services/postgres/walg-backup", &sealed);
            assert_eq!(request.service_id, 8);
            assert_eq!(request.node_id, 2);
            assert_eq!(request.bucket_path, "base");
            assert!(!request.allow_restart);
            assert_eq!(
                request.destination.session_token.as_deref(),
                Some("destination-session")
            );
            Json(serde_json::json!({
                "success": true,
                "data": {
                    "status": "completed",
                    "restarted": false,
                    "stdout": "",
                    "stderr": "INFO: Wrote backup with name base_000000010000000000000003",
                    "lsn": "0/3000060"
                },
                "error": null
            }))
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route("/agent/services/postgres/walg-backup", post(walg)),
            )
            .await
            .unwrap();
        });
        let client = |address: std::net::SocketAddr| {
            RemoteServiceClient::new(
                format!("http://{address}"),
                "node-token".to_string(),
                "worker-1".to_string(),
            )
            .unwrap()
            .with_sealing(test_sealing())
        };
        let request = PostgresWalgBackupRequest {
            service_id: 8,
            node_id: 2,
            service_config: ServiceConfig {
                name: "orders".to_string(),
                service_type: ServiceType::Postgres,
                version: None,
                parameters: serde_json::json!({"host": "localhost", "password": "db-password"}),
            },
            destination: worker_destination(),
            bucket_path: "base".to_string(),
            backup_uuid: "u-1".to_string(),
            allow_restart: false,
        };
        let outcome = client(address)
            .postgres_walg_backup(&request)
            .await
            .unwrap();
        server.abort();
        assert_eq!(
            outcome,
            PostgresWalgBackupResult::Completed {
                restarted: false,
                stdout: String::new(),
                stderr: "INFO: Wrote backup with name base_000000010000000000000003".to_string(),
                lsn: Some("0/3000060".to_string()),
            }
        );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let older = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, Router::new()).await.unwrap();
        });
        let error = client(older)
            .postgres_walg_backup(&request)
            .await
            .expect_err("an older agent cannot take a WAL-G backup");
        server.abort();
        let message = error.to_string();
        assert!(
            message.contains("/agent/services/postgres/walg-backup"),
            "{message}"
        );
        assert!(message.contains("Update the node's agent"), "{message}");
    }

    /// A MongoDB backup and restore reach the node sealed, with the
    /// destination's credentials and the origin's password unreadable on
    /// the wire, and their outcomes come back as they are. An agent without
    /// the routes is told to update, never served another path.
    #[tokio::test]
    async fn mongodb_backups_and_restores_are_sealed_to_the_node() {
        async fn backup(headers: HeaderMap, sealed: axum::body::Bytes) -> Json<serde_json::Value> {
            for secret in ["destination-secret", "db-password"] {
                assert!(
                    !String::from_utf8_lossy(&sealed).contains(secret),
                    "{secret} readable on the wire"
                );
            }
            let request: MongodbBackupRequest =
                open_for_test(&headers, "/agent/services/mongodb/backup", &sealed);
            assert_eq!(request.service_id, 8);
            assert_eq!(request.bucket_path, "base");
            assert_eq!(request.backup_uuid, "u-1");
            Json(serde_json::json!({
                "success": true,
                "data": {"status": "walg_stream", "stdout": "", "stderr": "done"},
                "error": null
            }))
        }
        async fn restore(headers: HeaderMap, sealed: axum::body::Bytes) -> Json<serde_json::Value> {
            assert!(!String::from_utf8_lossy(&sealed).contains("origin-password"));
            let request: MongodbRestoreRequest =
                open_for_test(&headers, "/agent/services/mongodb/restore", &sealed);
            assert_eq!(request.origin_password.as_deref(), Some("origin-password"));
            assert_eq!(request.location, "s3://backups/walg");
            Json(serde_json::json!({
                "success": true,
                "data": {"status": "failed", "reason": "no backups"},
                "error": null
            }))
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route("/agent/services/mongodb/backup", post(backup))
                    .route("/agent/services/mongodb/restore", post(restore)),
            )
            .await
            .unwrap();
        });
        let client = |address: std::net::SocketAddr| {
            RemoteServiceClient::new(
                format!("http://{address}"),
                "node-token".to_string(),
                "worker-1".to_string(),
            )
            .unwrap()
            .with_sealing(test_sealing())
        };
        let config = ServiceConfig {
            name: "orders".to_string(),
            service_type: ServiceType::Mongodb,
            version: None,
            parameters: serde_json::json!({"host": "localhost", "password": "db-password"}),
        };
        let backup_request = MongodbBackupRequest {
            service_id: 8,
            node_id: 2,
            service_config: config.clone(),
            destination: worker_destination(),
            bucket_path: "base".to_string(),
            backup_uuid: "u-1".to_string(),
            mode: MongodbBackupMode::WalgStream,
        };
        let restore_request = MongodbRestoreRequest {
            service_id: 8,
            node_id: 2,
            service_config: config,
            origin_password: Some("origin-password".to_string()),
            source: worker_destination(),
            location: "s3://backups/walg".to_string(),
        };
        assert_eq!(
            client(address)
                .mongodb_backup(&backup_request)
                .await
                .unwrap(),
            MongodbBackupResult::WalgStream {
                stdout: String::new(),
                stderr: "done".to_string()
            }
        );
        assert_eq!(
            client(address)
                .mongodb_restore(&restore_request)
                .await
                .unwrap(),
            MongodbRestoreResult::Failed {
                reason: "no backups".to_string()
            }
        );
        server.abort();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let older = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, Router::new()).await.unwrap();
        });
        let backup_error = client(older)
            .mongodb_backup(&backup_request)
            .await
            .expect_err("an older agent cannot back up MongoDB with the engine");
        let restore_error = client(older)
            .mongodb_restore(&restore_request)
            .await
            .expect_err("an older agent cannot restore MongoDB with the engine");
        server.abort();
        for (error, route) in [
            (backup_error, "/agent/services/mongodb/backup"),
            (restore_error, "/agent/services/mongodb/restore"),
        ] {
            let message = error.to_string();
            assert!(message.contains(route), "{message}");
            assert!(message.contains("Update the node's agent"), "{message}");
        }
    }

    /// Each Redis operation reaches the node sealed, with the credentials
    /// only in the body, the node's outcome comes back as it is, and an agent
    /// without the route is told to update rather than falling back.
    #[tokio::test]
    async fn redis_operations_are_sealed_to_the_node_and_an_older_agent_is_named() {
        async fn walg(headers: HeaderMap, sealed: axum::body::Bytes) -> Json<serde_json::Value> {
            let request: RedisWalgBackupRequest =
                open_for_test(&headers, "/agent/services/redis/walg-backup", &sealed);
            assert_eq!(request.service_id, 9);
            assert_eq!(request.bucket_path, "fleet");
            assert_eq!(request.backup_uuid, "u-9");
            assert_eq!(
                request.destination.session_token.as_deref(),
                Some("destination-session")
            );
            Json(serde_json::json!({
                "success": true, "data": { "status": "completed" }, "error": null
            }))
        }
        async fn rdb(headers: HeaderMap, sealed: axum::body::Bytes) -> Json<serde_json::Value> {
            let request: RedisRdbBackupRequest =
                open_for_test(&headers, "/agent/services/redis/rdb-backup", &sealed);
            assert!(request.key.ends_with("/dump.rdb.gz"));
            Json(serde_json::json!({
                "success": true,
                "data": { "status": "completed", "size_bytes": 512 },
                "error": null
            }))
        }
        async fn restore(headers: HeaderMap, sealed: axum::body::Bytes) -> Json<serde_json::Value> {
            let request: RedisRestoreRequest =
                open_for_test(&headers, "/agent/services/redis/restore", &sealed);
            assert!(request.keep_target_bookkeeping);
            Json(serde_json::json!({
                "success": true,
                "data": { "status": "failed", "reason": "staged snapshot is empty" },
                "error": null
            }))
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route("/agent/services/redis/walg-backup", post(walg))
                    .route("/agent/services/redis/rdb-backup", post(rdb))
                    .route("/agent/services/redis/restore", post(restore)),
            )
            .await
            .unwrap();
        });
        let client = |address: std::net::SocketAddr| {
            RemoteServiceClient::new(
                format!("http://{address}"),
                "node-token".to_string(),
                "worker-1".to_string(),
            )
            .unwrap()
            .with_sealing(test_sealing())
        };
        let config = ServiceConfig {
            name: "cache".to_string(),
            service_type: ServiceType::Redis,
            version: None,
            parameters: serde_json::json!({ "password": "redis-password" }),
        };
        let walg_request = RedisWalgBackupRequest {
            service_id: 9,
            service_config: config.clone(),
            destination: worker_destination(),
            bucket_path: "fleet".to_string(),
            backup_uuid: "u-9".to_string(),
        };
        let rdb_request = RedisRdbBackupRequest {
            service_id: 9,
            service_config: config.clone(),
            destination: worker_destination(),
            key: "external_services/redis/cache/2026/11/08/u-9/dump.rdb.gz".to_string(),
        };
        let restore_request = RedisRestoreRequest {
            service_id: 9,
            service_config: config,
            source: worker_destination(),
            location: "s3://bucket/external_services/redis/other/walg".to_string(),
            keep_target_bookkeeping: true,
        };
        assert_eq!(
            client(address)
                .redis_walg_backup(&walg_request)
                .await
                .unwrap(),
            RedisWalgBackupResult::Completed
        );
        assert_eq!(
            client(address)
                .redis_rdb_backup(&rdb_request)
                .await
                .unwrap(),
            RedisRdbBackupResult::Completed { size_bytes: 512 }
        );
        assert_eq!(
            client(address)
                .redis_restore(&restore_request)
                .await
                .unwrap(),
            RedisRestoreResult::Failed {
                reason: "staged snapshot is empty".to_string()
            }
        );
        server.abort();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let older = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, Router::new()).await.unwrap();
        });
        let messages = [
            client(older)
                .redis_walg_backup(&walg_request)
                .await
                .expect_err("an older agent has no Redis WAL-G route")
                .to_string(),
            client(older)
                .redis_rdb_backup(&rdb_request)
                .await
                .expect_err("an older agent has no Redis RDB route")
                .to_string(),
            client(older)
                .redis_restore(&restore_request)
                .await
                .expect_err("an older agent has no Redis restore route")
                .to_string(),
        ];
        server.abort();
        for (message, path) in messages.iter().zip([
            "/agent/services/redis/walg-backup",
            "/agent/services/redis/rdb-backup",
            "/agent/services/redis/restore",
        ]) {
            assert!(message.contains(path), "{message}");
            assert!(message.contains("Update the node's agent"), "{message}");
            assert!(!message.contains("redis-password"), "{message}");
        }
    }

    /// Every MariaDB operation reaches the node sealed, on its own route,
    /// and returns the node's outcome; an older agent is told to update.
    #[tokio::test]
    async fn mariadb_operations_are_sealed_to_the_node_and_their_outcomes_returned() {
        async fn physical(
            headers: HeaderMap,
            sealed: axum::body::Bytes,
        ) -> Json<serde_json::Value> {
            let request: MariadbPhysicalBackupRequest =
                open_for_test(&headers, "/agent/services/mariadb/physical-backup", &sealed);
            assert_eq!((request.service_id, request.node_id), (8, 2));
            assert_eq!(request.backup_uuid, "u-1");
            assert_eq!(
                request.destination.session_token.as_deref(),
                Some("destination-session")
            );
            Json(serde_json::json!({"success": true, "error": null, "data": {
                "status": "completed",
                "binlog": {"file": "mysql-bin.000002", "position": "385", "gtid": "0-1-4"}
            }}))
        }
        async fn dump(headers: HeaderMap, sealed: axum::body::Bytes) -> Json<serde_json::Value> {
            let request: MariadbDumpBackupRequest =
                open_for_test(&headers, "/agent/services/mariadb/dump-backup", &sealed);
            assert!(request.key.ends_with("dump.sql.gz"));
            Json(serde_json::json!({"success": true, "error": null, "data": {
                "status": "completed", "size_bytes": 512
            }}))
        }
        async fn restore(headers: HeaderMap, sealed: axum::body::Bytes) -> Json<serde_json::Value> {
            let request: MariadbRestoreRequest =
                open_for_test(&headers, "/agent/services/mariadb/restore", &sealed);
            assert!(request.recovery_target.is_some());
            Json(serde_json::json!({"success": true, "error": null, "data": {
                "status": "failed", "reason": "the original data was put back"
            }}))
        }
        async fn archive(headers: HeaderMap, sealed: axum::body::Bytes) -> Json<serde_json::Value> {
            let _: MariadbBinlogArchiveRequest =
                open_for_test(&headers, "/agent/services/mariadb/binlog-archive", &sealed);
            Json(serde_json::json!({"success": true, "error": null, "data": {
                "status": "completed", "shipped": 3
            }}))
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route("/agent/services/mariadb/physical-backup", post(physical))
                    .route("/agent/services/mariadb/dump-backup", post(dump))
                    .route("/agent/services/mariadb/restore", post(restore))
                    .route("/agent/services/mariadb/binlog-archive", post(archive)),
            )
            .await
            .unwrap();
        });
        let client = |address: std::net::SocketAddr| {
            RemoteServiceClient::new(
                format!("http://{address}"),
                "node-token".to_string(),
                "worker-1".to_string(),
            )
            .unwrap()
            .with_sealing(test_sealing())
        };
        let config = ServiceConfig {
            name: "shop".to_string(),
            service_type: ServiceType::Mariadb,
            version: None,
            parameters: serde_json::json!({"root_password": "root-password"}),
        };
        let physical_request = MariadbPhysicalBackupRequest {
            service_id: 8,
            node_id: 2,
            service_config: config.clone(),
            destination: worker_destination(),
            bucket_path: "base".to_string(),
            backup_uuid: "u-1".to_string(),
        };
        assert_eq!(
            client(address)
                .mariadb_physical_backup(&physical_request)
                .await
                .unwrap(),
            MariadbPhysicalBackupResult::Completed {
                binlog: Some(MariadbBinlogCoordinate {
                    file: "mysql-bin.000002".into(),
                    position: "385".into(),
                    gtid: "0-1-4".into(),
                })
            }
        );
        let dumped = client(address)
            .mariadb_dump_backup(&MariadbDumpBackupRequest {
                service_id: 8,
                node_id: 2,
                service_config: config.clone(),
                destination: worker_destination(),
                bucket_path: "base".to_string(),
                key: "base/external_services/mariadb/shop/u/dump.sql.gz".to_string(),
            })
            .await
            .unwrap();
        assert_eq!(
            dumped,
            MariadbDumpBackupResult::Completed { size_bytes: 512 }
        );
        let restored = client(address)
            .mariadb_restore(&MariadbRestoreRequest {
                service_id: 8,
                node_id: 2,
                service_config: config.clone(),
                source: worker_destination(),
                bucket_path: "base".to_string(),
                location: "s3://b/base/external_services/mariadb/shop/walg".to_string(),
                recovery_target: Some(RecoveryTarget::Time {
                    time: "2026-11-08T10:00:00Z".parse().unwrap(),
                }),
                target_user_data: None,
            })
            .await
            .unwrap();
        assert_eq!(
            restored,
            MariadbRestoreResult::Failed {
                reason: "the original data was put back".into()
            }
        );
        let archived = client(address)
            .mariadb_binlog_archive(&MariadbBinlogArchiveRequest {
                service_id: 8,
                node_id: 2,
                service_config: config,
                destination: worker_destination(),
                bucket_path: "base".to_string(),
            })
            .await
            .unwrap();
        assert_eq!(
            archived,
            MariadbBinlogArchiveResult::Completed { shipped: 3 }
        );
        server.abort();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let older = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, Router::new()).await.unwrap();
        });
        let message = client(older)
            .mariadb_physical_backup(&physical_request)
            .await
            .expect_err("an older agent has no MariaDB route")
            .to_string();
        server.abort();
        assert!(
            message.contains("/agent/services/mariadb/physical-backup"),
            "{message}"
        );
        assert!(message.contains("Update the node's agent"), "{message}");
    }

    /// An archive re-point reaches the node sealed, with the destination's
    /// credentials, and an agent without the route is told to update.
    #[tokio::test]
    async fn a_walg_archive_repoint_is_sealed_to_the_node() {
        async fn walg(headers: HeaderMap, sealed: axum::body::Bytes) -> Json<serde_json::Value> {
            let request: PostgresWalgArchiveRequest =
                open_for_test(&headers, "/agent/services/postgres/walg-archive", &sealed);
            assert_eq!(request.service_id, 8);
            assert_eq!(request.bucket_path, "fleet-b");
            assert_eq!(
                request.destination.secret_access_key,
                worker_destination().secret_access_key
            );
            Json(serde_json::json!({
                "success": true,
                "data": {"status": "completed", "restarted": true},
                "error": null
            }))
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route("/agent/services/postgres/walg-archive", post(walg)),
            )
            .await
            .unwrap();
        });
        let client = |address: std::net::SocketAddr| {
            RemoteServiceClient::new(
                format!("http://{address}"),
                "node-token".to_string(),
                "worker-1".to_string(),
            )
            .unwrap()
            .with_sealing(test_sealing())
        };
        let request = PostgresWalgArchiveRequest {
            service_id: 8,
            node_id: 2,
            service_config: ServiceConfig {
                name: "orders".to_string(),
                service_type: ServiceType::Postgres,
                version: None,
                parameters: serde_json::json!({"host": "localhost", "password": "db-password"}),
            },
            destination: worker_destination(),
            bucket_path: "fleet-b".to_string(),
        };
        let outcome = client(address)
            .postgres_walg_archive(&request)
            .await
            .unwrap();
        server.abort();
        assert_eq!(
            outcome,
            PostgresWalgArchiveResult::Completed { restarted: true }
        );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let older = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, Router::new()).await.unwrap();
        });
        let error = client(older)
            .postgres_walg_archive(&request)
            .await
            .expect_err("an older agent cannot re-point archiving");
        server.abort();
        let message = error.to_string();
        assert!(
            message.contains("/agent/services/postgres/walg-archive"),
            "{message}"
        );
        assert!(message.contains("Update the node's agent"), "{message}");
    }

    /// A node at its limit of backups and restores answers 429; the client
    /// returns the typed `NodeBusy` (nothing ran), so callers can wait for a
    /// slot instead of failing the run.
    #[tokio::test]
    async fn a_busy_node_is_reported_as_node_busy() {
        async fn busy() -> (reqwest::StatusCode, Json<serde_json::Value>) {
            (
                reqwest::StatusCode::TOO_MANY_REQUESTS,
                Json(serde_json::json!({
                    "success": false,
                    "data": null,
                    "error": "Cannot restore service 'orders': this node already runs 2 backups or restores; retry when one finishes"
                })),
            )
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route("/agent/services/data-restore", post(busy)),
            )
            .await
            .unwrap();
        });
        let client = RemoteServiceClient::new(
            format!("http://{address}"),
            "node-token".to_string(),
            "worker-1".to_string(),
        )
        .unwrap()
        .with_sealing(test_sealing());
        let error = client
            .restore_service(&RemoteServiceRestoreRequest {
                service_config: ServiceConfig {
                    name: "orders".to_string(),
                    service_type: ServiceType::Redis,
                    version: None,
                    parameters: serde_json::json!({}),
                },
                source: worker_destination(),
                location: "external_services/redis/orders/dump.rdb.gz".to_string(),
            })
            .await
            .expect_err("a busy node runs nothing");
        server.abort();
        match error {
            ExternalServiceError::NodeBusy {
                node,
                route,
                detail,
            } => {
                assert_eq!(node, "worker-1");
                assert_eq!(route, "/agent/services/data-restore");
                assert!(detail.contains("already runs 2"), "{detail}");
            }
            other => panic!("expected NodeBusy, got {other:?}"),
        }
    }

    /// A WAL-G restore reaches the node sealed, with the source's credentials,
    /// both logins and the typed recovery target, and the engine's outcome
    /// comes back as it is. An agent without the route is told to update.
    #[tokio::test]
    async fn a_walg_restore_is_sealed_to_the_node_and_its_outcome_returned() {
        async fn walg(headers: HeaderMap, sealed: axum::body::Bytes) -> Json<serde_json::Value> {
            let request: PostgresWalgRestoreRequest =
                open_for_test(&headers, "/agent/services/postgres/walg-restore", &sealed);
            assert_eq!(request.service_id, 8);
            assert_eq!(
                request.service_config.parameters["password"],
                "origin-password"
            );
            assert_eq!(
                request.live_service_config.parameters["password"],
                "db-password"
            );
            assert_eq!(
                request.source.session_token.as_deref(),
                Some("destination-session")
            );
            assert!(matches!(
                request.recovery_target,
                Some(RecoveryTarget::Lsn { ref lsn }) if lsn == "0/3000060"
            ));
            assert_eq!(request.operation_id.as_deref(), Some("restore-run-5"));
            Json(serde_json::json!({
                "success": true,
                "data": {"status": "failed", "reason": "the original database was put back unchanged"},
                "error": null
            }))
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route("/agent/services/postgres/walg-restore", post(walg)),
            )
            .await
            .unwrap();
        });
        let client = |address: std::net::SocketAddr| {
            RemoteServiceClient::new(
                format!("http://{address}"),
                "node-token".to_string(),
                "worker-1".to_string(),
            )
            .unwrap()
            .with_sealing(test_sealing())
        };
        let config = |password: &str| ServiceConfig {
            name: "orders".to_string(),
            service_type: ServiceType::Postgres,
            version: None,
            parameters: serde_json::json!({"host": "localhost", "password": password}),
        };
        let request = PostgresWalgRestoreRequest {
            service_id: 8,
            node_id: 2,
            service_config: config("origin-password"),
            live_service_config: config("db-password"),
            source: worker_destination(),
            walg_prefix: "s3://backups/external_services/postgres/billing/walg".to_string(),
            recovery_target: Some(RecoveryTarget::Lsn {
                lsn: "0/3000060".to_string(),
            }),
            target_user_data: None,
            operation_id: Some("restore-run-5".to_string()),
        };
        let outcome = client(address)
            .postgres_walg_restore(&request)
            .await
            .unwrap();
        server.abort();
        assert_eq!(
            outcome,
            PostgresWalgRestoreResult::Failed {
                reason: "the original database was put back unchanged".to_string()
            }
        );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let older = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, Router::new()).await.unwrap();
        });
        let error = client(older)
            .postgres_walg_restore(&request)
            .await
            .expect_err("an older agent cannot restore from WAL-G");
        server.abort();
        let message = error.to_string();
        assert!(
            message.contains("/agent/services/postgres/walg-restore"),
            "{message}"
        );
        assert!(message.contains("Update the node's agent"), "{message}");
    }

    /// A restore's request that the agent never answers (a timeout, or a
    /// connection dropped before the response) is `AgentUnanswered`, not an
    /// error the agent returned: the restore may still be running there.
    /// Its outcome is then asked for by its operation id, sealed like every
    /// worker service request.
    #[tokio::test]
    async fn an_unanswered_walg_restore_is_told_apart_and_its_outcome_asked_for() {
        async fn outcome(headers: HeaderMap, sealed: axum::body::Bytes) -> Json<serde_json::Value> {
            let request: PostgresWalgRestoreOutcomeRequest = open_for_test(
                &headers,
                "/agent/services/postgres/walg-restore/outcome",
                &sealed,
            );
            assert_eq!(request.service, "orders");
            assert_eq!(request.operation_id, "restore-run-5");
            Json(serde_json::json!({
                "success": true,
                "data": {"state": "running", "writes_began": true},
                "error": null
            }))
        }
        // Accepts the restore's connection and closes it without answering.
        let dropping = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let dropped = dropping.local_addr().unwrap();
        let closer = tokio::spawn(async move {
            while let Ok((mut socket, _)) = dropping.accept().await {
                let mut buffer = [0u8; 4096];
                let _ = tokio::io::AsyncReadExt::read(&mut socket, &mut buffer).await;
                drop(socket);
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let answering = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route(
                    "/agent/services/postgres/walg-restore/outcome",
                    post(outcome),
                ),
            )
            .await
            .unwrap();
        });
        let client = |address: std::net::SocketAddr| {
            RemoteServiceClient::new(
                format!("http://{address}"),
                "node-token".to_string(),
                "worker-1".to_string(),
            )
            .unwrap()
            .with_sealing(test_sealing())
        };
        let config = ServiceConfig {
            name: "orders".to_string(),
            service_type: ServiceType::Postgres,
            version: None,
            parameters: serde_json::json!({"host": "localhost"}),
        };
        let error = client(dropped)
            .postgres_walg_restore(&PostgresWalgRestoreRequest {
                service_id: 8,
                node_id: 2,
                service_config: config.clone(),
                live_service_config: config,
                source: worker_destination(),
                walg_prefix: "s3://backups/external_services/postgres/orders/walg".to_string(),
                recovery_target: None,
                target_user_data: None,
                operation_id: Some("restore-run-5".to_string()),
            })
            .await
            .expect_err("a dropped connection has no outcome");
        closer.abort();
        match error {
            ExternalServiceError::AgentUnanswered { node, route, .. } => {
                assert_eq!(node, "worker-1");
                assert_eq!(route, "/agent/services/postgres/walg-restore");
            }
            other => panic!("expected AgentUnanswered, got {other:?}"),
        }
        // Nothing listens: the request was never delivered, and nothing ran.
        let closed = {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            listener.local_addr().unwrap()
        };
        let error = client(closed)
            .postgres_walg_restore_outcome(&PostgresWalgRestoreOutcomeRequest {
                service: "orders".to_string(),
                operation_id: "restore-run-5".to_string(),
            })
            .await
            .expect_err("nothing listens");
        assert!(
            matches!(error, ExternalServiceError::InternalError { .. }),
            "{error:?}"
        );
        let progress = client(answering)
            .postgres_walg_restore_outcome(&PostgresWalgRestoreOutcomeRequest {
                service: "orders".to_string(),
                operation_id: "restore-run-5".to_string(),
            })
            .await
            .unwrap();
        server.abort();
        assert_eq!(
            progress,
            PostgresWalgRestoreProgress::Running { writes_began: true }
        );
    }

    /// A node without a sealing key never receives a backup's credentials.
    #[tokio::test]
    async fn a_worker_backup_is_refused_to_a_node_that_cannot_open_sealed_bodies() {
        let client = RemoteServiceClient::new(
            "http://127.0.0.1:9".to_string(),
            "node-token".to_string(),
            "worker-1".to_string(),
        )
        .unwrap();
        let error = client
            .backup_service(&RemoteServiceBackupRequest {
                service_config: ServiceConfig {
                    name: "cache".to_string(),
                    service_type: ServiceType::Redis,
                    version: None,
                    parameters: serde_json::json!({}),
                },
                destination: worker_destination(),
                key: "k/dump.rdb.gz".to_string(),
            })
            .await
            .expect_err("unsealed delivery must be refused");
        assert!(!error.to_string().contains("destination-secret"));
    }

    #[tokio::test]
    async fn health_probe_posts_authenticated_provider_config_and_returns_typed_result() {
        async fn handler(headers: HeaderMap, sealed: axum::body::Bytes) -> Json<serde_json::Value> {
            assert_eq!(
                headers
                    .get(AUTHORIZATION)
                    .and_then(|value| value.to_str().ok()),
                Some("Bearer node-token")
            );
            let request: RemoteHealthProbeRequest =
                open_for_test(&headers, "/agent/services/health-probe", &sealed);
            assert_eq!(request.service_config.name, "orders-mongo");
            assert_eq!(
                request.service_config.parameters["password"],
                "probe-secret"
            );
            Json(serde_json::json!({
                "success": true,
                "data": {
                    "result": {
                        "status": "operational",
                        "response_time_ms": 7,
                        "error_message": null
                    }
                },
                "error": null
            }))
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route("/agent/services/health-probe", post(handler)),
            )
            .await
            .unwrap();
        });

        let client = RemoteServiceClient::new(
            format!("http://{address}"),
            "node-token".to_string(),
            "worker-1".to_string(),
        )
        .unwrap()
        .with_sealing(test_sealing());
        let response = client
            .probe_health(RemoteHealthProbeRequest {
                service_config: ServiceConfig {
                    name: "orders-mongo".to_string(),
                    service_type: ServiceType::Mongodb,
                    version: None,
                    parameters: serde_json::json!({
                        "host": "localhost",
                        "port": "27017",
                        "password": "probe-secret"
                    }),
                },
            })
            .await
            .unwrap();

        assert_eq!(response.result.status, HealthProbeStatus::Operational);
        assert_eq!(response.result.response_time_ms, Some(7));
        assert_eq!(response.result.error_message, None);
        server.abort();
    }

    #[tokio::test]
    async fn health_probe_rejects_oversized_agent_response() {
        async fn handler() -> String {
            "x".repeat(MAX_AGENT_RESPONSE_BYTES + 1)
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route("/agent/services/health-probe", post(handler)),
            )
            .await
            .unwrap();
        });

        let client = RemoteServiceClient::new(
            format!("http://{address}"),
            "node-token".to_string(),
            "worker-1".to_string(),
        )
        .unwrap()
        .with_sealing(test_sealing());
        let error = client
            .probe_health(RemoteHealthProbeRequest {
                service_config: ServiceConfig {
                    name: "orders-db".to_string(),
                    service_type: ServiceType::Postgres,
                    version: None,
                    parameters: serde_json::json!({"host": "localhost"}),
                },
            })
            .await
            .unwrap_err();

        assert!(error.to_string().contains("exceeded"));
        server.abort();
    }
}
