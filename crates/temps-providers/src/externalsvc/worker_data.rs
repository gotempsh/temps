// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Backups and in-place restores of the managed services a worker runs,
//! performed on that worker (the agent's `data-backup` and `data-restore`
//! routes).
//!
//! A control plane started with `--profile control-plane` has no Docker, and
//! even one that does cannot reach a worker's containers, so the engines that
//! drive a local container (`redis`, `mongodb`, `mariadb_dump`, `s3_mirror`)
//! cannot back up a worker's service. Here the worker runs the same native
//! tools against its own container:
//!
//! - **MongoDB, Redis, MariaDB**: a one-shot helper from the service's own
//!   image (already on the node) joins the service's network and runs the
//!   engine's dump to stdout (`mongodump --archive --gzip`,
//!   `redis-cli --rdb` then `gzip`, `mariadb-dump | gzip`). This process
//!   streams that output into an S3 multipart upload, one part in memory at a
//!   time. The object formats and suffixes are the local engines', so the
//!   providers' own restores read them.
//! - **Managed S3 storage** (RustFS, Garage): the provider's bucket mirror,
//!   which already runs its `rc` helper on this node's Docker.
//!
//! The service's container is never stopped or restarted by a backup; the
//! helper is removed on every path, including when the request is dropped.
//! Credentials reach the helper only through its environment, never argv.
//!
//! A restore is the provider's own in-place restore (`restore_from_s3` for a
//! dump, the bucket mirror's restore for storage), run here against this
//! node's Docker. It is user-initiated: a Redis restore stops and starts the
//! container exactly as it does on the control plane.

use std::net::IpAddr;
use std::sync::Arc;

use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::types::{CompletedMultipartUpload, CompletedPart};
use bollard::models::{ContainerCreateBody, HostConfig};
use bollard::query_parameters::{
    AttachContainerOptionsBuilder, CreateContainerOptionsBuilder, InspectContainerOptions,
    RemoveContainerOptionsBuilder, StartContainerOptions, WaitContainerOptionsBuilder,
};
use bollard::Docker;
use futures::StreamExt;
use temps_service_protocol::{
    RemoteServiceBackupRequest, RemoteServiceBackupResponse, RemoteServiceRestoreRequest,
    RemoteServiceRestoreResponse, WorkerS3Destination,
};
use tracing::{info, warn};

use super::managed_s3::S3Location;
use super::naming::shell_escape;
use super::{
    managed_instance_name, resolve_runtime_target, EngineError, ExternalService, GarageService,
    ManagedS3Backend, ManagedS3BackendKind, ManagedS3BackendSelection, ManagedS3Error,
    MariaDbService, MongodbService, RedisService, RuntimeProvisioningError, RustfsService,
    S3Credentials, SensitiveValues, ServiceConfig, ServiceType,
};

/// Size of one uploaded part, and so the memory one dump holds. S3 allows
/// 10,000 parts, so a dump may reach 160 GiB.
const PART_SIZE: usize = 16 * 1024 * 1024;
const MAX_PARTS: i32 = 10_000;
/// How much of a failed dump's stderr is kept for its error.
const STDERR_TAIL_BYTES: usize = 4 * 1024;
/// Memory of one dump helper, swap included: the dump tools stream, so this
/// bounds a misbehaving one rather than sizing a normal dump.
const HELPER_MEMORY_BYTES: i64 = 1024 * 1024 * 1024;
/// Processes in one dump helper: a shell, the client, the dump tool, gzip.
const HELPER_PIDS_LIMIT: i64 = 64;

/// The dump helper's host settings: on the service network, bounded in
/// memory and processes, without new privileges.
fn helper_host_config() -> HostConfig {
    HostConfig {
        network_mode: Some(temps_core::NETWORK_NAME.to_string()),
        memory: Some(HELPER_MEMORY_BYTES),
        memory_swap: Some(HELPER_MEMORY_BYTES),
        pids_limit: Some(HELPER_PIDS_LIMIT),
        security_opt: Some(vec!["no-new-privileges:true".to_string()]),
        ..Default::default()
    }
}

/// The engines whose services are backed up as a dump.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DumpEngine {
    Mongodb,
    Redis,
    Mariadb,
}

impl DumpEngine {
    /// The dump engine of `service_type`, `None` for storage services and
    /// PostgreSQL (backed up by a job on its node).
    pub fn for_service_type(service_type: ServiceType) -> Option<Self> {
        match service_type {
            ServiceType::Mongodb => Some(Self::Mongodb),
            ServiceType::Redis | ServiceType::Kv => Some(Self::Redis),
            ServiceType::Mariadb => Some(Self::Mariadb),
            _ => None,
        }
    }

    /// The file name a dump is stored under: the local engine's, so the
    /// provider's restore recognizes it.
    pub fn file_suffix(self) -> &'static str {
        match self {
            Self::Mongodb => "dump.archive",
            Self::Redis => "dump.rdb.gz",
            Self::Mariadb => "dump.sql.gz",
        }
    }

    fn tool(self) -> &'static str {
        match self {
            Self::Mongodb => "mongodump",
            Self::Redis => "redis-cli --rdb",
            Self::Mariadb => "mariadb-dump",
        }
    }

    fn internal_port(self) -> u16 {
        match self {
            Self::Mongodb => 27017,
            Self::Redis => 6379,
            Self::Mariadb => 3306,
        }
    }
}

/// Why a backup or restore of a worker's service failed.
#[derive(Debug, thiserror::Error)]
pub enum WorkerDataError {
    #[error(
        "service '{service}' ({service_type}) cannot be {operation} on its worker node: {reason}"
    )]
    Unsupported {
        service: String,
        service_type: ServiceType,
        operation: &'static str,
        reason: String,
    },

    #[error("service '{service}': {source}")]
    Target {
        service: String,
        #[source]
        source: Box<RuntimeProvisioningError>,
    },

    #[error("service '{service}': its container '{container}' does not exist on this node")]
    ContainerMissing { service: String, container: String },

    #[error(
        "service '{service}': its container '{container}' is not running; start the service \
         before backing it up"
    )]
    ContainerStopped { service: String, container: String },

    #[error("service '{service}': inspecting its container '{container}' failed: {source}")]
    Inspect {
        service: String,
        container: String,
        #[source]
        source: Box<bollard::errors::Error>,
    },

    #[error("service '{service}': the {tool} helper for '{container}' failed to {step}: {source}")]
    Helper {
        service: String,
        container: String,
        tool: &'static str,
        step: &'static str,
        #[source]
        source: Box<bollard::errors::Error>,
    },

    #[error("service '{service}': {tool} exited with code {exit_code}: {stderr}")]
    DumpFailed {
        service: String,
        tool: &'static str,
        exit_code: i64,
        stderr: String,
    },

    #[error("service '{service}': {tool} produced no output")]
    EmptyDump { service: String, tool: &'static str },

    #[error("service '{service}': uploading the dump to '{bucket}/{key}' failed: {reason}")]
    Upload {
        service: String,
        bucket: String,
        key: String,
        reason: String,
    },

    #[error("service '{service}': {operation} failed: {source}")]
    Provider {
        service: String,
        operation: &'static str,
        #[source]
        source: Box<EngineError>,
    },

    #[error("service '{service}': {operation} failed: {source}")]
    Storage {
        service: String,
        operation: &'static str,
        #[source]
        source: Box<ManagedS3Error>,
    },

    #[error("service '{service}': creating a key for its storage helper failed: {source}")]
    Encryption {
        service: String,
        #[source]
        source: temps_core::EncryptionError,
    },
}

/// Back up the service `request` names on this node. See the module docs.
pub async fn backup_service_on_node(
    request: RemoteServiceBackupRequest,
    docker: Arc<Docker>,
    published_address: Option<IpAddr>,
) -> Result<RemoteServiceBackupResponse, WorkerDataError> {
    let RemoteServiceBackupRequest {
        service_config: mut config,
        destination,
        key,
    } = request;
    let service = config.name.clone();
    resolve_runtime_target(&mut config, published_address).map_err(|source| {
        WorkerDataError::Target {
            service: service.clone(),
            source: Box::new(source),
        }
    })?;
    match DumpEngine::for_service_type(config.service_type) {
        Some(engine) => {
            if !key.ends_with(engine.file_suffix()) {
                return Err(WorkerDataError::Unsupported {
                    service,
                    service_type: config.service_type,
                    operation: "backed up",
                    reason: format!(
                        "the object key '{key}' does not end in '{}'",
                        engine.file_suffix()
                    ),
                });
            }
            let container = service_container(&config, docker.as_ref()).await?;
            let size_bytes =
                dump_to_s3(docker, engine, &config, &container, &destination, &key).await?;
            Ok(RemoteServiceBackupResponse {
                size_bytes,
                buckets: vec![],
            })
        }
        None => {
            let backend = storage_backend(&config, docker, "backed up")?;
            let report = backend
                .backup(&config, &location(&destination, &key))
                .await
                .map_err(|source| WorkerDataError::Storage {
                    service,
                    operation: "mirroring its buckets",
                    source: Box::new(source),
                })?;
            Ok(RemoteServiceBackupResponse {
                size_bytes: 0,
                buckets: report.buckets,
            })
        }
    }
}

/// Restore the service `request` names on this node, in place, from a backup
/// [`backup_service_on_node`] wrote.
pub async fn restore_service_on_node(
    request: RemoteServiceRestoreRequest,
    docker: Arc<Docker>,
    published_address: Option<IpAddr>,
) -> Result<RemoteServiceRestoreResponse, WorkerDataError> {
    let RemoteServiceRestoreRequest {
        service_config: mut config,
        source,
        location: backup_location,
    } = request;
    let service = config.name.clone();
    resolve_runtime_target(&mut config, published_address).map_err(|error| {
        WorkerDataError::Target {
            service: service.clone(),
            source: Box::new(error),
        }
    })?;
    match DumpEngine::for_service_type(config.service_type) {
        Some(engine) => {
            if !backup_location.ends_with(engine.file_suffix()) {
                return Err(WorkerDataError::Unsupported {
                    service,
                    service_type: config.service_type,
                    operation: "restored",
                    reason: format!(
                        "backup '{backup_location}' is not a '{}' dump, the format backups on a \
                         worker node write",
                        engine.file_suffix()
                    ),
                });
            }
            let container = service_container(&config, docker.as_ref()).await?;
            let instance = dump_instance(engine, &config, Arc::clone(&docker));
            // The providers' restores write to the container they derive from
            // the service's name; an imported container under another name
            // would not be the one restored.
            let derived = instance.get_docker_container_name();
            if container != derived {
                return Err(WorkerDataError::Unsupported {
                    service,
                    service_type: config.service_type,
                    operation: "restored",
                    reason: format!(
                        "it runs in imported container '{container}', and restores on a worker \
                         node apply only to the container Temps created ('{derived}')"
                    ),
                });
            }
            set_container_name(&mut config, &container);
            let credentials = s3_credentials(&source);
            let client = credentials.build_s3_client().await;
            let s3_source = s3_source_row(&source);
            info!(
                service = %service,
                container = %container,
                backup_location = %backup_location,
                "Restoring a worker service in place from its dump"
            );
            instance
                .restore_from_s3(&client, &credentials, &backup_location, &s3_source, config)
                .await
                .map_err(|error| WorkerDataError::Provider {
                    service,
                    operation: "restoring the dump",
                    source: Box::new(error),
                })?;
            Ok(RemoteServiceRestoreResponse { buckets: vec![] })
        }
        None => {
            let backend = storage_backend(&config, docker, "restored")?;
            let report = backend
                .restore(&config, &location(&source, &backup_location), false)
                .await
                .map_err(|error| WorkerDataError::Storage {
                    service,
                    operation: "restoring its buckets",
                    source: Box::new(error),
                })?;
            Ok(RemoteServiceRestoreResponse {
                buckets: report.buckets,
            })
        }
    }
}

/// The provider handle of a dump engine's service.
fn dump_instance(
    engine: DumpEngine,
    config: &ServiceConfig,
    docker: Arc<Docker>,
) -> Box<dyn ExternalService> {
    let name = managed_instance_name(&config.name, config.service_type);
    match engine {
        DumpEngine::Mongodb => Box::new(MongodbService::new(name, docker)),
        DumpEngine::Redis => Box::new(RedisService::new(name, docker)),
        DumpEngine::Mariadb => Box::new(MariaDbService::new(name, docker)),
    }
}

/// The managed S3 backend of a storage service, or why it has none here.
fn storage_backend(
    config: &ServiceConfig,
    docker: Arc<Docker>,
    operation: &'static str,
) -> Result<Box<dyn ManagedS3Backend>, WorkerDataError> {
    let unsupported = |reason: String| WorkerDataError::Unsupported {
        service: config.name.clone(),
        service_type: config.service_type,
        operation,
        reason,
    };
    #[allow(deprecated, reason = "MinIO is matched to refuse it by name")]
    let backend = match config.service_type {
        ServiceType::S3 | ServiceType::Blob => {
            ManagedS3BackendSelection::from_parameters(&config.parameters)
                .map_err(|error| unsupported(error.to_string()))?
                .backend
        }
        ServiceType::Rustfs => ManagedS3BackendKind::Rustfs,
        ServiceType::Minio => ManagedS3BackendKind::Minio,
        ServiceType::Postgres => {
            return Err(unsupported(
                "PostgreSQL on a worker is backed up and restored by a job on its node".to_string(),
            ))
        }
        ServiceType::Mongodb | ServiceType::Redis | ServiceType::Kv | ServiceType::Mariadb => {
            return Err(unsupported("the service is not storage".to_string()))
        }
    };
    // The providers need an encryption service at construction; the mirror
    // never uses it, so a fresh request-local key is enough.
    let encryption = temps_core::EncryptionService::generate_raw_key()
        .and_then(|key| temps_core::EncryptionService::new(&key))
        .map(Arc::new)
        .map_err(|source| WorkerDataError::Encryption {
            service: config.name.clone(),
            source,
        })?;
    match backend {
        ManagedS3BackendKind::Rustfs => {
            let name = if config.service_type == ServiceType::Blob {
                managed_instance_name(&config.name, config.service_type)
            } else {
                config.name.clone()
            };
            Ok(Box::new(RustfsService::new(name, docker, encryption)))
        }
        ManagedS3BackendKind::Garage => Ok(Box::new(GarageService::new(
            config.name.clone(),
            docker,
            encryption,
            config.service_type,
        ))),
        ManagedS3BackendKind::Minio => Err(unsupported(
            "the legacy MinIO backend has no bucket mirror on worker nodes; migrate the \
             service to the RustFS backend"
                .to_string(),
        )),
    }
}

fn location(destination: &WorkerS3Destination, prefix: &str) -> S3Location {
    S3Location {
        endpoint: destination.endpoint.clone(),
        access_key: destination.access_key_id.clone(),
        secret_key: destination.secret_access_key.clone(),
        session_token: destination.session_token.clone(),
        bucket: destination.bucket.clone(),
        prefix: prefix.trim_matches('/').to_string(),
    }
}

fn s3_credentials(destination: &WorkerS3Destination) -> S3Credentials {
    S3Credentials {
        access_key_id: destination.access_key_id.clone(),
        secret_key: destination.secret_access_key.clone(),
        session_token: destination
            .session_token
            .clone()
            .filter(|token| !token.is_empty()),
        region: destination.region.clone(),
        endpoint: destination
            .endpoint
            .clone()
            .filter(|endpoint| !endpoint.trim().is_empty()),
        bucket_name: destination.bucket.clone(),
        bucket_path: String::new(),
        force_path_style: destination.force_path_style,
    }
}

/// The S3 source row the providers' restores read, holding the decrypted
/// credentials as their contract expects. Never stored.
fn s3_source_row(destination: &WorkerS3Destination) -> temps_entities::s3_sources::Model {
    let now = chrono::Utc::now();
    temps_entities::s3_sources::Model {
        id: 0,
        name: "worker-restore".to_string(),
        bucket_name: destination.bucket.clone(),
        region: destination.region.clone(),
        endpoint: destination.endpoint.clone(),
        bucket_path: String::new(),
        access_key_id: destination.access_key_id.clone(),
        secret_key: destination.secret_access_key.clone(),
        session_token: destination.session_token.clone(),
        credentials_expire_at: None,
        force_path_style: Some(destination.force_path_style),
        is_default: false,
        managed_by_cloud: false,
        lifecycle_reconcile_failed_at: None,
        lifecycle_reconcile_generation: 0,
        created_at: now,
        updated_at: now,
        backing_service_id: None,
    }
}

fn set_container_name(config: &mut ServiceConfig, container: &str) {
    if let Some(parameters) = config.parameters.as_object_mut() {
        parameters.insert(
            "container_name".to_string(),
            serde_json::Value::String(container.to_string()),
        );
    }
}

/// The container the service runs in on this node: an imported service's
/// recorded name, or the provider's.
async fn service_container(
    config: &ServiceConfig,
    docker: &Docker,
) -> Result<String, WorkerDataError> {
    let recorded = config
        .parameters
        .get("container_name")
        .and_then(serde_json::Value::as_str)
        .filter(|name| !name.is_empty())
        .map(str::to_string);
    let container = match recorded {
        Some(name) => name,
        None => {
            let Some(engine) = DumpEngine::for_service_type(config.service_type) else {
                return Err(WorkerDataError::Unsupported {
                    service: config.name.clone(),
                    service_type: config.service_type,
                    operation: "backed up",
                    reason: "the service has no dump engine".to_string(),
                });
            };
            dump_instance(engine, config, Arc::new(docker.clone())).get_docker_container_name()
        }
    };
    match docker
        .inspect_container(&container, None::<InspectContainerOptions>)
        .await
    {
        Ok(_) => Ok(container),
        Err(bollard::errors::Error::DockerResponseServerError {
            status_code: 404, ..
        }) => Err(WorkerDataError::ContainerMissing {
            service: config.name.clone(),
            container,
        }),
        Err(source) => Err(WorkerDataError::Inspect {
            service: config.name.clone(),
            container,
            source: Box::new(source),
        }),
    }
}

/// What the dump helper runs: a fixed script per engine and the variables
/// carrying its credentials. Nothing secret is in the script.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct DumpPlan {
    pub(crate) script: String,
    pub(crate) env: Vec<String>,
}

/// Credentials a dump connects with.
#[derive(Default)]
pub(crate) struct DumpCredentials {
    pub(crate) username: String,
    pub(crate) password: String,
}

/// The engine's root credentials: the service config's, unless the
/// container's environment names the root account (MongoDB's configured
/// user may hold a narrower role; MariaDB keeps only the root password).
pub(crate) fn dump_credentials(
    engine: DumpEngine,
    parameters: &serde_json::Value,
    container_env: &[String],
) -> DumpCredentials {
    let text = |key: &str| {
        parameters
            .get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let env = |name: &str| {
        container_env
            .iter()
            .find_map(|entry| entry.strip_prefix(name)?.strip_prefix('='))
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    match engine {
        DumpEngine::Mongodb => {
            let mut username = text("username");
            let mut password = text("password");
            if let (Some(user), Some(pass)) = (
                env("MONGO_INITDB_ROOT_USERNAME"),
                env("MONGO_INITDB_ROOT_PASSWORD"),
            ) {
                username = user;
                password = pass;
            }
            if username.is_empty() {
                username = "admin".to_string();
            }
            DumpCredentials { username, password }
        }
        DumpEngine::Redis => DumpCredentials {
            username: String::new(),
            password: text("password"),
        },
        DumpEngine::Mariadb => {
            let configured = text("root_password");
            let password = if configured.is_empty() {
                env("MARIADB_ROOT_PASSWORD")
                    .or_else(|| env("MYSQL_ROOT_PASSWORD"))
                    .unwrap_or_default()
            } else {
                configured
            };
            DumpCredentials {
                username: "root".to_string(),
                password,
            }
        }
    }
}

/// The dump of `engine` against `host` (the service's container on the
/// shared network), written to stdout.
pub(crate) fn dump_plan(engine: DumpEngine, host: &str, credentials: &DumpCredentials) -> DumpPlan {
    let quoted_host = shell_escape(host);
    let port = engine.internal_port();
    match engine {
        // The URI is one token that cannot be read as a flag, and keeps the
        // password out of argv (see the local MongoDB engine).
        DumpEngine::Mongodb => DumpPlan {
            script: "exec mongodump --quiet --uri=\"$MONGODB_URI\" --archive --gzip".to_string(),
            env: vec![format!(
                "MONGODB_URI=mongodb://{}:{}@{}:{port}/?authSource=admin",
                urlencoding::encode(&credentials.username),
                urlencoding::encode(&credentials.password),
                host,
            )],
        },
        // `--rdb -` appends a 40-byte marker Redis rejects on restore, so the
        // snapshot goes to a file in the helper first.
        DumpEngine::Redis => DumpPlan {
            script: format!(
                "set -e\n\
                 f=\"${{TMPDIR:-/tmp}}/temps-backup.rdb\"\n\
                 redis-cli -h {quoted_host} -p {port} --rdb \"$f\" >&2\n\
                 gzip -c \"$f\"\n\
                 rm -f \"$f\""
            ),
            env: if credentials.password.is_empty() {
                vec![]
            } else {
                vec![format!("REDISCLI_AUTH={}", credentials.password)]
            },
        },
        // The local engine's dump (every user database, one transaction),
        // over the network. POSIX `sh` has no `pipefail`, so the dump's own
        // status is carried out of the pipeline in a file. Schema names are
        // data a database user controls: each is one argument (split on
        // newlines only, `set -f`: never globbed) after `--`, so one named
        // like an option cannot redirect the dump and the password with it.
        DumpEngine::Mariadb => DumpPlan {
            script: format!(
                "set -f\n\
                 if command -v mariadb-dump >/dev/null 2>&1; then dump=mariadb-dump; else dump=mysqldump; fi\n\
                 if command -v mariadb >/dev/null 2>&1; then client=mariadb; else client=mysql; fi\n\
                 dbs=$($client -h {quoted_host} -P {port} -N -B -u{user} -e \"SELECT SCHEMA_NAME FROM information_schema.SCHEMATA WHERE SCHEMA_NAME NOT IN ('information_schema','mysql','performance_schema','sys') ORDER BY SCHEMA_NAME\") || exit 3\n\
                 if [ -z \"$dbs\" ]; then echo '-- No user databases to dump' | gzip; exit 0; fi\n\
                 IFS='\n'; set -- $dbs; unset IFS\n\
                 rc=\"${{TMPDIR:-/tmp}}/temps-backup.rc\"\n\
                 {{ $dump -h {quoted_host} -P {port} --single-transaction --quick -u{user} --databases -- \"$@\"; echo $? > \"$rc\"; }} | gzip\n\
                 [ \"$(cat \"$rc\")\" = 0 ]",
                user = shell_escape(&credentials.username),
            ),
            env: vec![
                format!("MYSQL_PWD={}", credentials.password),
                format!("MARIADB_PWD={}", credentials.password),
            ],
        },
    }
}

/// Removes the dump helper on every path, including a dropped request.
struct HelperGuard {
    docker: Arc<Docker>,
    id: Option<String>,
}

impl HelperGuard {
    async fn remove(mut self) {
        if let Some(id) = self.id.take() {
            remove_helper(&self.docker, &id).await;
        }
    }
}

impl Drop for HelperGuard {
    fn drop(&mut self) {
        let Some(id) = self.id.take() else {
            return;
        };
        let docker = Arc::clone(&self.docker);
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => {
                runtime.spawn(async move { remove_helper(&docker, &id).await });
            }
            Err(_) => warn!(helper = %id, "dump helper left behind: no runtime to remove it"),
        }
    }
}

async fn remove_helper(docker: &Docker, id: &str) {
    if let Err(error) = docker
        .remove_container(
            id,
            Some(RemoveContainerOptionsBuilder::new().force(true).build()),
        )
        .await
    {
        warn!(helper = %id, %error, "removing the dump helper failed");
    }
}

/// Run the dump helper for `container` and stream its stdout into `key`.
async fn dump_to_s3(
    docker: Arc<Docker>,
    engine: DumpEngine,
    config: &ServiceConfig,
    container: &str,
    destination: &WorkerS3Destination,
    key: &str,
) -> Result<u64, WorkerDataError> {
    let service = config.name.clone();
    let helper_error = |step: &'static str| {
        let service = service.clone();
        move |source| WorkerDataError::Helper {
            service,
            container: container.to_string(),
            tool: engine.tool(),
            step,
            source: Box::new(source),
        }
    };
    let inspect = docker
        .inspect_container(container, None::<InspectContainerOptions>)
        .await
        .map_err(|source| WorkerDataError::Inspect {
            service: service.clone(),
            container: container.to_string(),
            source: Box::new(source),
        })?;
    if !inspect
        .state
        .as_ref()
        .and_then(|state| state.running)
        .unwrap_or(false)
    {
        return Err(WorkerDataError::ContainerStopped {
            service,
            container: container.to_string(),
        });
    }
    let container_config = inspect.config.unwrap_or_default();
    let Some(image) = container_config.image.filter(|image| !image.is_empty()) else {
        return Err(WorkerDataError::Unsupported {
            service,
            service_type: config.service_type,
            operation: "backed up",
            reason: format!("container '{container}' reports no image"),
        });
    };
    let credentials = dump_credentials(
        engine,
        &config.parameters,
        &container_config.env.unwrap_or_default(),
    );
    let plan = dump_plan(engine, container, &credentials);
    let sensitive = SensitiveValues::new()
        .credential("", &credentials.password, None)
        .credential(
            &destination.access_key_id,
            &destination.secret_access_key,
            destination.session_token.as_deref(),
        );

    let helper_name = format!(
        "temps-worker-backup-{}-{:08x}",
        container.trim_start_matches('/'),
        rand::random::<u32>()
    );
    let created = docker
        .create_container(
            Some(
                CreateContainerOptionsBuilder::new()
                    .name(&helper_name)
                    .build(),
            ),
            ContainerCreateBody {
                image: Some(image),
                entrypoint: Some(vec!["sh".to_string(), "-c".to_string()]),
                cmd: Some(vec![plan.script]),
                env: Some(plan.env),
                labels: Some(
                    [
                        ("sh.temps.kind".to_string(), "backup".to_string()),
                        ("sh.temps.backup_of".to_string(), container.to_string()),
                    ]
                    .into_iter()
                    .collect(),
                ),
                attach_stdout: Some(true),
                attach_stderr: Some(true),
                tty: Some(false),
                host_config: Some(helper_host_config()),
                ..Default::default()
            },
        )
        .await
        .map_err(helper_error("be created"))?;
    let guard = HelperGuard {
        docker: Arc::clone(&docker),
        id: Some(created.id.clone()),
    };

    let attached = docker
        .attach_container(
            &created.id,
            Some(
                AttachContainerOptionsBuilder::new()
                    .stream(true)
                    .stdout(true)
                    .stderr(true)
                    .build(),
            ),
        )
        .await
        .map_err(helper_error("attach"))?;
    docker
        .start_container(&created.id, None::<StartContainerOptions>)
        .await
        .map_err(helper_error("start"))?;
    info!(
        service = %service,
        container = %container,
        helper = %helper_name,
        bucket = %destination.bucket,
        key = %key,
        "Dumping a worker service to S3"
    );

    let client = s3_credentials(destination).build_s3_client().await;
    let mut upload = MultipartUpload::start(client, destination, key, &service).await?;
    let mut stderr_tail: Vec<u8> = Vec::new();
    let mut output = attached.output;
    let streamed = async {
        while let Some(frame) = output.next().await {
            match frame {
                Ok(bollard::container::LogOutput::StdOut { message }) => {
                    upload.write(&message).await?;
                }
                Ok(bollard::container::LogOutput::StdErr { message }) => {
                    stderr_tail.extend_from_slice(&message);
                    let excess = stderr_tail.len().saturating_sub(STDERR_TAIL_BYTES);
                    stderr_tail.drain(..excess);
                }
                Ok(_) => {}
                Err(error) => {
                    return Err(WorkerDataError::Helper {
                        service: service.clone(),
                        container: container.to_string(),
                        tool: engine.tool(),
                        step: "stream its output",
                        source: Box::new(error),
                    })
                }
            }
        }
        Ok(())
    }
    .await;
    if let Err(error) = streamed {
        upload.abort().await;
        guard.remove().await;
        return Err(error);
    }

    let exit_code = match docker
        .wait_container(
            &created.id,
            Some(WaitContainerOptionsBuilder::new().build()),
        )
        .next()
        .await
    {
        Some(Ok(response)) => response.status_code,
        Some(Err(bollard::errors::Error::DockerContainerWaitError { code, .. })) => code,
        Some(Err(source)) => {
            upload.abort().await;
            guard.remove().await;
            return Err(helper_error("report its exit status")(source));
        }
        None => -1,
    };
    guard.remove().await;
    if exit_code != 0 {
        upload.abort().await;
        let stderr = String::from_utf8_lossy(&stderr_tail);
        return Err(WorkerDataError::DumpFailed {
            service,
            tool: engine.tool(),
            exit_code,
            stderr: sensitive.redact(stderr.trim()),
        });
    }
    if upload.total == 0 {
        upload.abort().await;
        return Err(WorkerDataError::EmptyDump {
            service,
            tool: engine.tool(),
        });
    }
    let size = upload.finish().await?;
    info!(
        service = %service,
        bucket = %destination.bucket,
        key = %key,
        size_bytes = size,
        "Worker service dump uploaded"
    );
    Ok(size)
}

/// An S3 multipart upload fed one buffered part at a time. Aborted when
/// dropped before it completed, so a cancelled dump leaves no parts behind.
struct MultipartUpload {
    client: aws_sdk_s3::Client,
    bucket: String,
    key: String,
    service: String,
    upload_id: Option<String>,
    parts: Vec<CompletedPart>,
    buffer: Vec<u8>,
    total: u64,
}

impl MultipartUpload {
    async fn start(
        client: aws_sdk_s3::Client,
        destination: &WorkerS3Destination,
        key: &str,
        service: &str,
    ) -> Result<Self, WorkerDataError> {
        let created = client
            .create_multipart_upload()
            .bucket(&destination.bucket)
            .key(key)
            .content_type("application/octet-stream")
            .send()
            .await
            .map_err(|error| WorkerDataError::Upload {
                service: service.to_string(),
                bucket: destination.bucket.clone(),
                key: key.to_string(),
                reason: format!(
                    "starting the upload: {}",
                    aws_sdk_s3::error::DisplayErrorContext(&error)
                ),
            })?;
        let upload_id = created.upload_id.ok_or_else(|| WorkerDataError::Upload {
            service: service.to_string(),
            bucket: destination.bucket.clone(),
            key: key.to_string(),
            reason: "the storage returned no upload id".to_string(),
        })?;
        Ok(Self {
            client,
            bucket: destination.bucket.clone(),
            key: key.to_string(),
            service: service.to_string(),
            upload_id: Some(upload_id),
            parts: Vec::new(),
            buffer: Vec::with_capacity(PART_SIZE),
            total: 0,
        })
    }

    fn error(&self, reason: String) -> WorkerDataError {
        WorkerDataError::Upload {
            service: self.service.clone(),
            bucket: self.bucket.clone(),
            key: self.key.clone(),
            reason,
        }
    }

    async fn write(&mut self, bytes: &[u8]) -> Result<(), WorkerDataError> {
        self.buffer.extend_from_slice(bytes);
        self.total += bytes.len() as u64;
        if self.buffer.len() >= PART_SIZE {
            self.send_part().await?;
        }
        Ok(())
    }

    async fn send_part(&mut self) -> Result<(), WorkerDataError> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        let Some(upload_id) = self.upload_id.clone() else {
            return Err(self.error("the upload was already closed".to_string()));
        };
        let part_number = i32::try_from(self.parts.len() + 1).unwrap_or(i32::MAX);
        if part_number > MAX_PARTS {
            return Err(self.error(format!(
                "the dump is larger than {MAX_PARTS} parts of {PART_SIZE} bytes"
            )));
        }
        let body = std::mem::replace(&mut self.buffer, Vec::with_capacity(PART_SIZE));
        let sent = self
            .client
            .upload_part()
            .bucket(&self.bucket)
            .key(&self.key)
            .upload_id(upload_id)
            .part_number(part_number)
            .body(ByteStream::from(body))
            .send()
            .await
            .map_err(|error| {
                self.error(format!(
                    "uploading part {part_number}: {}",
                    aws_sdk_s3::error::DisplayErrorContext(&error)
                ))
            })?;
        self.parts.push(
            CompletedPart::builder()
                .part_number(part_number)
                .set_e_tag(sent.e_tag)
                .build(),
        );
        Ok(())
    }

    async fn finish(mut self) -> Result<u64, WorkerDataError> {
        let result = self.complete().await;
        if result.is_err() {
            self.abort().await;
        }
        result
    }

    async fn complete(&mut self) -> Result<u64, WorkerDataError> {
        self.send_part().await?;
        let Some(upload_id) = self.upload_id.clone() else {
            return Err(self.error("the upload was already closed".to_string()));
        };
        self.client
            .complete_multipart_upload()
            .bucket(&self.bucket)
            .key(&self.key)
            .upload_id(upload_id)
            .multipart_upload(
                CompletedMultipartUpload::builder()
                    .set_parts(Some(self.parts.clone()))
                    .build(),
            )
            .send()
            .await
            .map_err(|error| {
                self.error(format!(
                    "completing the upload: {}",
                    aws_sdk_s3::error::DisplayErrorContext(&error)
                ))
            })?;
        self.upload_id = None;
        Ok(self.total)
    }

    async fn abort(&mut self) {
        let Some(upload_id) = self.upload_id.take() else {
            return;
        };
        abort_upload(&self.client, &self.bucket, &self.key, &upload_id).await;
    }
}

impl Drop for MultipartUpload {
    fn drop(&mut self) {
        let Some(upload_id) = self.upload_id.take() else {
            return;
        };
        let client = self.client.clone();
        let bucket = self.bucket.clone();
        let key = self.key.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move { abort_upload(&client, &bucket, &key, &upload_id).await });
        }
    }
}

async fn abort_upload(client: &aws_sdk_s3::Client, bucket: &str, key: &str, upload_id: &str) {
    if let Err(error) = client
        .abort_multipart_upload()
        .bucket(bucket)
        .key(key)
        .upload_id(upload_id)
        .send()
        .await
    {
        warn!(
            bucket,
            key,
            error = %aws_sdk_s3::error::DisplayErrorContext(&error),
            "aborting an unfinished dump upload failed; the bucket's lifecycle removes its parts"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASSWORD: &str = "-p@ss w'rd/$x";

    fn credentials() -> DumpCredentials {
        DumpCredentials {
            username: "root".to_string(),
            password: PASSWORD.to_string(),
        }
    }

    #[test]
    fn every_dump_engine_writes_the_local_engines_format() {
        assert_eq!(
            DumpEngine::for_service_type(ServiceType::Mongodb).map(DumpEngine::file_suffix),
            Some("dump.archive")
        );
        assert_eq!(
            DumpEngine::for_service_type(ServiceType::Redis).map(DumpEngine::file_suffix),
            Some("dump.rdb.gz")
        );
        assert_eq!(
            DumpEngine::for_service_type(ServiceType::Kv),
            Some(DumpEngine::Redis)
        );
        assert_eq!(
            DumpEngine::for_service_type(ServiceType::Mariadb).map(DumpEngine::file_suffix),
            Some("dump.sql.gz")
        );
        for storage in [
            ServiceType::S3,
            ServiceType::Rustfs,
            ServiceType::Blob,
            ServiceType::Postgres,
        ] {
            assert_eq!(DumpEngine::for_service_type(storage), None);
        }
    }

    /// A runaway dump tool is stopped by its helper's bounds, not by the
    /// node running out of memory or processes.
    #[test]
    fn the_dump_helper_is_bounded_in_memory_and_processes() {
        let host = helper_host_config();
        assert_eq!(host.memory, Some(HELPER_MEMORY_BYTES));
        assert_eq!(
            host.memory_swap, host.memory,
            "no swap beyond the memory bound"
        );
        assert_eq!(host.pids_limit, Some(HELPER_PIDS_LIMIT));
        assert_eq!(
            host.network_mode.as_deref(),
            Some(temps_core::NETWORK_NAME.as_str())
        );
        assert_eq!(
            host.security_opt,
            Some(vec!["no-new-privileges:true".to_string()])
        );
    }

    /// No password reaches argv: each engine's credential travels in the
    /// helper's environment, and the dump connects to the service's
    /// container by name on the shared network.
    #[test]
    fn dump_plans_keep_credentials_out_of_the_script() {
        for engine in [DumpEngine::Mongodb, DumpEngine::Redis, DumpEngine::Mariadb] {
            let plan = dump_plan(engine, "temps-svc-orders", &credentials());
            assert!(
                !plan.script.contains(PASSWORD),
                "{engine:?}: {}",
                plan.script
            );
            assert!(!plan.script.contains("--password"), "{engine:?}");
            assert!(
                plan.env
                    .iter()
                    .any(|entry| entry.contains("temps-svc-orders")
                        || plan.script.contains("temps-svc-orders")),
                "{engine:?} must target the container: {plan:?}"
            );
        }
        let mongo = dump_plan(DumpEngine::Mongodb, "temps-mongodb-orders", &credentials());
        assert!(mongo.script.contains("--archive --gzip"));
        assert_eq!(mongo.env.len(), 1);
        assert!(mongo.env[0].starts_with("MONGODB_URI=mongodb://root:"));
        assert!(mongo.env[0].ends_with("@temps-mongodb-orders:27017/?authSource=admin"));
        assert!(
            !mongo.env[0].contains(PASSWORD),
            "the URI percent-encodes it"
        );

        let redis = dump_plan(DumpEngine::Redis, "temps-redis-cache", &credentials());
        assert_eq!(redis.env, vec![format!("REDISCLI_AUTH={PASSWORD}")]);
        assert!(redis.script.contains("--rdb \"$f\""));
        assert!(redis.script.contains("gzip -c"));

        let maria = dump_plan(DumpEngine::Mariadb, "temps-mariadb-shop", &credentials());
        assert!(
            maria.script.starts_with("set -f\n"),
            "no globbing of database names"
        );
        assert!(
            maria.script.contains("--databases -- \"$@\""),
            "schema names are arguments after `--`, never options"
        );
        assert!(!maria.script.contains("$dbs --"), "{}", maria.script);
        assert_eq!(
            maria.env,
            vec![
                format!("MYSQL_PWD={PASSWORD}"),
                format!("MARIADB_PWD={PASSWORD}")
            ]
        );
        assert!(maria.script.contains("--single-transaction"));
        assert!(maria.script.contains("echo $? > \"$rc\""));
    }

    #[test]
    fn a_redis_without_a_password_gets_no_auth_variable() {
        let plan = dump_plan(
            DumpEngine::Redis,
            "temps-redis-cache",
            &DumpCredentials::default(),
        );
        assert!(plan.env.is_empty());
    }

    #[test]
    fn dump_credentials_prefer_the_root_account_of_the_container() {
        let parameters = serde_json::json!({ "username": "app", "password": "app-pass" });
        let env = vec![
            "MONGO_INITDB_ROOT_USERNAME=root-user".to_string(),
            "MONGO_INITDB_ROOT_PASSWORD=root-pass".to_string(),
        ];
        let mongo = dump_credentials(DumpEngine::Mongodb, &parameters, &env);
        assert_eq!(
            (mongo.username.as_str(), mongo.password.as_str()),
            ("root-user", "root-pass")
        );
        let configured = dump_credentials(DumpEngine::Mongodb, &parameters, &[]);
        assert_eq!(configured.username, "app");

        let maria = dump_credentials(
            DumpEngine::Mariadb,
            &serde_json::json!({}),
            &["MARIADB_ROOT_PASSWORD=from-env".to_string()],
        );
        assert_eq!(
            (maria.username.as_str(), maria.password.as_str()),
            ("root", "from-env")
        );
        let maria = dump_credentials(
            DumpEngine::Mariadb,
            &serde_json::json!({ "root_password": "configured" }),
            &["MARIADB_ROOT_PASSWORD=from-env".to_string()],
        );
        assert_eq!(maria.password, "configured");

        let redis = dump_credentials(
            DumpEngine::Redis,
            &serde_json::json!({ "password": "cache-pass" }),
            &[],
        );
        assert_eq!(redis.password, "cache-pass");
    }

    fn config(service_type: ServiceType, parameters: serde_json::Value) -> ServiceConfig {
        ServiceConfig {
            name: "store".to_string(),
            service_type,
            version: None,
            parameters,
        }
    }

    fn docker() -> Arc<Docker> {
        // Never contacted: the selection below fails or succeeds before any
        // call. `connect_with_http` only parses the address.
        #[allow(clippy::unwrap_used, reason = "test helper with a constant address")]
        Arc::new(
            Docker::connect_with_http("http://127.0.0.1:1", 1, bollard::API_DEFAULT_VERSION)
                .unwrap(),
        )
    }

    #[test]
    fn storage_backends_are_chosen_from_the_service_and_minio_is_refused_by_name() {
        let rustfs = storage_backend(
            &config(ServiceType::S3, serde_json::json!({ "backend": "rustfs" })),
            docker(),
            "backed up",
        )
        .map(|backend| backend.kind());
        assert!(matches!(rustfs, Ok(ManagedS3BackendKind::Rustfs)));
        let garage = storage_backend(
            &config(ServiceType::S3, serde_json::json!({ "backend": "garage" })),
            docker(),
            "backed up",
        )
        .map(|backend| backend.kind());
        assert!(matches!(garage, Ok(ManagedS3BackendKind::Garage)));

        #[allow(deprecated, reason = "the legacy type is what is refused")]
        let minio = storage_backend(
            &config(ServiceType::Minio, serde_json::json!({})),
            docker(),
            "backed up",
        );
        let Err(error) = minio else {
            panic!("MinIO must be refused");
        };
        assert!(error
            .to_string()
            .contains("migrate the service to the RustFS backend"));

        let Err(error) = storage_backend(
            &config(ServiceType::Postgres, serde_json::json!({})),
            docker(),
            "backed up",
        ) else {
            panic!("PostgreSQL takes the job path");
        };
        assert!(error.to_string().contains("job on its node"));
    }

    /// The restore's S3 source carries the destination's decrypted
    /// credentials, session token included, as the providers expect.
    #[test]
    fn restore_sources_carry_the_session_token() {
        let destination = WorkerS3Destination {
            bucket: "backups".to_string(),
            region: "eu-west-1".to_string(),
            endpoint: Some("https://s3.example.test".to_string()),
            force_path_style: true,
            access_key_id: "AKIA".to_string(),
            secret_access_key: "secret".to_string(),
            session_token: Some("token".to_string()),
        };
        let row = s3_source_row(&destination);
        assert_eq!(row.session_token.as_deref(), Some("token"));
        assert_eq!(row.bucket_name, "backups");
        let credentials = s3_credentials(&destination);
        assert_eq!(credentials.session_token.as_deref(), Some("token"));
        let mirror = location(&destination, "/backups/s3/store/abc/");
        assert_eq!(mirror.prefix, "backups/s3/store/abc");
        assert_eq!(mirror.session_token.as_deref(), Some("token"));
    }

    /// A backup or restore of a dump engine refuses a key in another format
    /// before touching Docker.
    #[tokio::test]
    async fn a_key_in_another_format_is_refused_before_any_container_call() {
        let destination = WorkerS3Destination {
            bucket: "b".to_string(),
            region: "us-east-1".to_string(),
            endpoint: None,
            force_path_style: true,
            access_key_id: "a".to_string(),
            secret_access_key: "s".to_string(),
            session_token: None,
        };
        let backup = backup_service_on_node(
            RemoteServiceBackupRequest {
                service_config: config(ServiceType::Redis, serde_json::json!({})),
                destination: destination.clone(),
                key: "redis/store/1/dump.sql.gz".to_string(),
            },
            docker(),
            None,
        )
        .await;
        assert!(matches!(backup, Err(WorkerDataError::Unsupported { .. })));
        let restore = restore_service_on_node(
            RemoteServiceRestoreRequest {
                service_config: config(ServiceType::Mongodb, serde_json::json!({})),
                source: destination,
                location: "mongodb/store/1/dump.rdb.gz".to_string(),
            },
            docker(),
            None,
        )
        .await;
        let Err(error) = restore else {
            panic!("a Redis dump must not be applied to MongoDB");
        };
        assert!(error.to_string().contains("dump.archive"), "{error}");
    }

    #[tokio::test]
    async fn a_non_loopback_service_host_is_refused() {
        let destination = WorkerS3Destination {
            bucket: "b".to_string(),
            region: "us-east-1".to_string(),
            endpoint: None,
            force_path_style: true,
            access_key_id: "a".to_string(),
            secret_access_key: "s".to_string(),
            session_token: None,
        };
        let result = backup_service_on_node(
            RemoteServiceBackupRequest {
                service_config: config(
                    ServiceType::Mariadb,
                    serde_json::json!({ "host": "db.example.test" }),
                ),
                destination,
                key: "mariadb/store/1/dump.sql.gz".to_string(),
            },
            docker(),
            None,
        )
        .await;
        assert!(matches!(result, Err(WorkerDataError::Target { .. })));
    }
}

/// The worker path end to end against real containers: a service on the
/// shared network, a RustFS bucket as the destination. Skips without Docker.
#[cfg(test)]
#[cfg(feature = "docker-tests")]
mod docker_tests {
    use super::*;
    use bollard::models::{NetworkCreateRequest, PortBinding};
    use bollard::query_parameters::{CreateImageOptionsBuilder, InspectNetworkOptions};
    use std::collections::HashMap;
    use std::io::Read;

    const RUSTFS_IMAGE: &str = "rustfs/rustfs:1.0.0";
    const RUSTFS_KEY: &str = "rustfsadmin";
    const BUCKET: &str = "worker-backups";

    /// Everything a test created, removed at the end; nothing else is touched.
    struct Created {
        docker: Arc<Docker>,
        containers: Vec<String>,
        network: Option<String>,
    }

    impl Created {
        async fn cleanup(self) {
            for container in &self.containers {
                let _ = self
                    .docker
                    .remove_container(
                        container,
                        Some(
                            RemoveContainerOptionsBuilder::new()
                                .force(true)
                                .v(true)
                                .build(),
                        ),
                    )
                    .await;
            }
            if let Some(network) = &self.network {
                let _ = self.docker.remove_network(network).await;
            }
        }
    }

    async fn docker_or_skip() -> Option<Arc<Docker>> {
        let docker = Docker::connect_with_local_defaults().ok()?;
        if docker.ping().await.is_err() {
            eprintln!("Docker unavailable, skipping");
            return None;
        }
        Some(Arc::new(docker))
    }

    async fn pull(docker: &Docker, image: &str) -> bool {
        let mut stream = docker.create_image(
            Some(CreateImageOptionsBuilder::new().from_image(image).build()),
            None,
            None,
        );
        while let Some(item) = stream.next().await {
            if let Err(error) = item {
                eprintln!("could not pull {image}, skipping: {error}");
                return false;
            }
        }
        true
    }

    async fn ensure_network(docker: &Docker, created: &mut Created) {
        let name = temps_core::NETWORK_NAME.as_str();
        if docker
            .inspect_network(name, None::<InspectNetworkOptions>)
            .await
            .is_ok()
        {
            return;
        }
        docker
            .create_network(NetworkCreateRequest {
                name: name.to_string(),
                ..Default::default()
            })
            .await
            .unwrap();
        created.network = Some(name.to_string());
    }

    async fn run(
        docker: &Docker,
        created: &mut Created,
        name: &str,
        body: ContainerCreateBody,
    ) -> String {
        docker
            .create_container(
                Some(CreateContainerOptionsBuilder::new().name(name).build()),
                body,
            )
            .await
            .unwrap();
        created.containers.push(name.to_string());
        docker
            .start_container(name, None::<StartContainerOptions>)
            .await
            .unwrap();
        name.to_string()
    }

    async fn exec(docker: &Docker, container: &str, env: Vec<String>, command: &[&str]) -> String {
        use bollard::exec::{CreateExecOptions, StartExecResults};
        let exec = docker
            .create_exec(
                container,
                CreateExecOptions {
                    cmd: Some(command.iter().map(|s| s.to_string()).collect()),
                    env: Some(env),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let mut out = String::new();
        if let StartExecResults::Attached { mut output, .. } =
            docker.start_exec(&exec.id, None).await.unwrap()
        {
            while let Some(Ok(chunk)) = output.next().await {
                out.push_str(&chunk.to_string());
            }
        }
        out
    }

    /// A RustFS server on a random loopback port, with `BUCKET` created.
    async fn destination(docker: &Docker, created: &mut Created, id: &str) -> WorkerS3Destination {
        let name = format!("temps-test-worker-data-s3-{id}");
        run(
            docker,
            created,
            &name,
            ContainerCreateBody {
                image: Some(RUSTFS_IMAGE.to_string()),
                env: Some(vec![
                    format!("RUSTFS_ACCESS_KEY={RUSTFS_KEY}"),
                    format!("RUSTFS_SECRET_KEY={RUSTFS_KEY}"),
                ]),
                host_config: Some(HostConfig {
                    port_bindings: Some(HashMap::from([(
                        "9000/tcp".to_string(),
                        Some(vec![PortBinding {
                            host_ip: Some("127.0.0.1".to_string()),
                            host_port: Some(String::new()),
                        }]),
                    )])),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await;
        let inspect = docker
            .inspect_container(&name, None::<InspectContainerOptions>)
            .await
            .unwrap();
        let port = inspect
            .network_settings
            .and_then(|settings| settings.ports)
            .and_then(|ports| ports.get("9000/tcp").cloned().flatten())
            .and_then(|bindings| bindings.first().and_then(|b| b.host_port.clone()))
            .unwrap();
        let destination = WorkerS3Destination {
            bucket: BUCKET.to_string(),
            region: "us-east-1".to_string(),
            endpoint: Some(format!("http://127.0.0.1:{port}")),
            force_path_style: true,
            access_key_id: RUSTFS_KEY.to_string(),
            secret_access_key: RUSTFS_KEY.to_string(),
            session_token: None,
        };
        let client = s3_credentials(&destination).build_s3_client().await;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            match client.create_bucket().bucket(BUCKET).send().await {
                Ok(_) => break,
                Err(error) if tokio::time::Instant::now() > deadline => {
                    panic!("RustFS never accepted the bucket: {error:?}")
                }
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(500)).await,
            }
        }
        destination
    }

    async fn object(destination: &WorkerS3Destination, key: &str) -> Vec<u8> {
        let client = s3_credentials(destination).build_s3_client().await;
        let body = client
            .get_object()
            .bucket(&destination.bucket)
            .key(key)
            .send()
            .await
            .unwrap()
            .body
            .collect()
            .await
            .unwrap()
            .into_bytes();
        body.to_vec()
    }

    fn gunzip(bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(bytes)
            .read_to_end(&mut out)
            .unwrap();
        out
    }

    /// Redis on a worker: the backup dumps an RDB through the helper into the
    /// bucket without stopping Redis, and the restore brings a flushed key
    /// back from it.
    #[tokio::test]
    async fn a_redis_on_a_worker_backs_up_and_restores_in_place() {
        let Some(docker) = docker_or_skip().await else {
            return;
        };
        if !pull(&docker, "redis:7.4.10-alpine").await || !pull(&docker, RUSTFS_IMAGE).await {
            return;
        }
        let id = format!("{:08x}", rand::random::<u32>());
        let mut created = Created {
            docker: Arc::clone(&docker),
            containers: vec![],
            network: None,
        };
        ensure_network(&docker, &mut created).await;
        let destination = destination(&docker, &mut created, &id).await;

        let service = format!("wdtest{id}");
        let container =
            RedisService::new(service.clone(), Arc::clone(&docker)).get_docker_container_name();
        let password = "-redis p@ss";
        run(
            &docker,
            &mut created,
            &container,
            ContainerCreateBody {
                image: Some("redis:7.4.10-alpine".to_string()),
                cmd: Some(vec![
                    "redis-server".to_string(),
                    "--requirepass".to_string(),
                    password.to_string(),
                    "--appendonly".to_string(),
                    "yes".to_string(),
                ]),
                host_config: Some(HostConfig {
                    network_mode: Some(temps_core::NETWORK_NAME.to_string()),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await;
        let auth = vec![format!("REDISCLI_AUTH={password}")];
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
        while !exec(&docker, &container, auth.clone(), &["redis-cli", "ping"])
            .await
            .contains("PONG")
        {
            assert!(
                tokio::time::Instant::now() < deadline,
                "Redis never started"
            );
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        }
        exec(
            &docker,
            &container,
            auth.clone(),
            &["redis-cli", "SET", "worker-key", "worker-value"],
        )
        .await;

        let config = ServiceConfig {
            name: service.clone(),
            service_type: ServiceType::Redis,
            version: None,
            parameters: serde_json::json!({
                "host": "localhost",
                "port": "6379",
                "password": password,
            }),
        };
        let key = format!("redis/{service}/{id}/dump.rdb.gz");
        let outcome = backup_service_on_node(
            RemoteServiceBackupRequest {
                service_config: config.clone(),
                destination: destination.clone(),
                key: key.clone(),
            },
            Arc::clone(&docker),
            None,
        )
        .await;
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(error) => {
                created.cleanup().await;
                panic!("backup failed: {error}");
            }
        };
        let stored = object(&destination, &key).await;
        assert_eq!(outcome.size_bytes, stored.len() as u64);
        assert!(gunzip(&stored).starts_with(b"REDIS"), "an RDB snapshot");
        // The backup never stopped the database.
        let inspect = docker
            .inspect_container(&container, None::<InspectContainerOptions>)
            .await
            .unwrap();
        assert_eq!(inspect.state.and_then(|state| state.running), Some(true));
        assert_eq!(inspect.restart_count.unwrap_or(0), 0);
        let leftovers = docker
            .list_containers(Some(
                bollard::query_parameters::ListContainersOptionsBuilder::new()
                    .all(true)
                    .filters(&HashMap::from([(
                        "label",
                        vec![format!("sh.temps.backup_of={container}")],
                    )]))
                    .build(),
            ))
            .await
            .unwrap();
        assert!(leftovers.is_empty(), "the dump helper is removed");

        exec(
            &docker,
            &container,
            auth.clone(),
            &["redis-cli", "FLUSHALL"],
        )
        .await;
        let restored = restore_service_on_node(
            RemoteServiceRestoreRequest {
                service_config: config,
                source: destination,
                location: key,
            },
            Arc::clone(&docker),
            None,
        )
        .await;
        let value = exec(
            &docker,
            &container,
            auth,
            &["redis-cli", "GET", "worker-key"],
        )
        .await;
        created.cleanup().await;
        restored.unwrap();
        assert!(value.contains("worker-value"), "restored value: {value}");
    }

    /// MariaDB on a worker: the dump is the local engine's gzipped SQL of
    /// every user database.
    #[tokio::test]
    async fn a_mariadb_on_a_worker_is_dumped_as_gzipped_sql() {
        let Some(docker) = docker_or_skip().await else {
            return;
        };
        if !pull(&docker, "mariadb:11.4").await || !pull(&docker, RUSTFS_IMAGE).await {
            return;
        }
        let id = format!("{:08x}", rand::random::<u32>());
        let mut created = Created {
            docker: Arc::clone(&docker),
            containers: vec![],
            network: None,
        };
        ensure_network(&docker, &mut created).await;
        let destination = destination(&docker, &mut created, &id).await;
        let service = format!("wdtest{id}");
        let container =
            MariaDbService::new(service.clone(), Arc::clone(&docker)).get_docker_container_name();
        let password = "-maria p@ss";
        run(
            &docker,
            &mut created,
            &container,
            ContainerCreateBody {
                image: Some("mariadb:11.4".to_string()),
                env: Some(vec![
                    format!("MARIADB_ROOT_PASSWORD={password}"),
                    "MARIADB_DATABASE=shop".to_string(),
                ]),
                host_config: Some(HostConfig {
                    network_mode: Some(temps_core::NETWORK_NAME.to_string()),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await;
        let auth = vec![format!("MYSQL_PWD={password}")];
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(90);
        while !exec(
            &docker,
            &container,
            auth.clone(),
            &["mariadb", "-uroot", "-e", "CREATE TABLE IF NOT EXISTS shop.orders (id INT); INSERT INTO shop.orders VALUES (42); SELECT 'seeded'"],
        )
        .await
        .contains("seeded")
        {
            assert!(tokio::time::Instant::now() < deadline, "MariaDB never started");
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }

        let key = format!("mariadb/{service}/{id}/dump.sql.gz");
        let outcome = backup_service_on_node(
            RemoteServiceBackupRequest {
                service_config: ServiceConfig {
                    name: service,
                    service_type: ServiceType::Mariadb,
                    version: None,
                    parameters: serde_json::json!({ "host": "localhost", "port": "3306" }),
                },
                destination: destination.clone(),
                key: key.clone(),
            },
            Arc::clone(&docker),
            None,
        )
        .await;
        let stored = match outcome {
            Ok(_) => object(&destination, &key).await,
            Err(error) => {
                created.cleanup().await;
                panic!("backup failed: {error}");
            }
        };
        created.cleanup().await;
        let sql = String::from_utf8_lossy(&gunzip(&stored)).to_string();
        assert!(
            sql.contains("CREATE DATABASE"),
            "{}",
            &sql[..sql.len().min(400)]
        );
        // The seeding loop may run its insert more than once before MariaDB
        // answers, so the row can appear repeatedly.
        assert!(sql.contains("INSERT INTO `orders` VALUES"), "{sql}");
        assert!(sql.contains("(42)"), "{sql}");
    }

    /// A stopped service is refused before any helper or upload starts.
    #[tokio::test]
    async fn a_stopped_service_is_not_backed_up() {
        let Some(docker) = docker_or_skip().await else {
            return;
        };
        if !pull(&docker, "redis:7.4.10-alpine").await {
            return;
        }
        let id = format!("{:08x}", rand::random::<u32>());
        let mut created = Created {
            docker: Arc::clone(&docker),
            containers: vec![],
            network: None,
        };
        let service = format!("wdtest{id}");
        let container =
            RedisService::new(service.clone(), Arc::clone(&docker)).get_docker_container_name();
        docker
            .create_container(
                Some(
                    CreateContainerOptionsBuilder::new()
                        .name(&container)
                        .build(),
                ),
                ContainerCreateBody {
                    image: Some("redis:7.4.10-alpine".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        created.containers.push(container.clone());
        let result = backup_service_on_node(
            RemoteServiceBackupRequest {
                service_config: ServiceConfig {
                    name: service,
                    service_type: ServiceType::Redis,
                    version: None,
                    parameters: serde_json::json!({}),
                },
                destination: WorkerS3Destination {
                    bucket: BUCKET.to_string(),
                    region: "us-east-1".to_string(),
                    endpoint: Some("http://127.0.0.1:1".to_string()),
                    force_path_style: true,
                    access_key_id: "a".to_string(),
                    secret_access_key: "s".to_string(),
                    session_token: None,
                },
                key: "redis/x/1/dump.rdb.gz".to_string(),
            },
            Arc::clone(&docker),
            None,
        )
        .await;
        created.cleanup().await;
        assert!(
            matches!(result, Err(WorkerDataError::ContainerStopped { .. })),
            "{result:?}"
        );
    }
}
