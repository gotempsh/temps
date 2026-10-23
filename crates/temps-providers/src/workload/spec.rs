// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The one database `WorkloadSpec` builder (ADR-055 Phase 4).
//!
//! Both paths that create a managed database container -- the control-plane
//! engines and the worker agent -- turn the database into a [`WorkloadSpec`]
//! here, admit it (§4), and build the container-create request from the
//! admitted spec with [`database_create_body`] through the one Docker driver.
//! Before Phase 4 the two built their own requests and drifted (defect 1):
//! the worker's containers had no healthcheck, no PID cap, no
//! `no-new-privileges`, `unless-stopped` instead of `always`, no log
//! rotation and different labels.
//!
//! A spec has two halves with different sources:
//!
//! - **The process** ([`DatabaseProcess`]): image, environment, command,
//!   storage, port and limits. It is the database's own: what the engine (on
//!   the control plane) or the agent request (on a worker) would run today,
//!   or what an existing container already runs. Volume names and data
//!   paths differ between the two paths and between releases; replacing them
//!   would point a database at an empty directory, so a spec never does.
//! - **The policy**: the `Data` security profile and its host config, the
//!   `always` restart policy, the engine's healthcheck, log rotation and
//!   both label sets (`temps.*`, which the control plane has always set,
//!   and `sh.temps.*`, which the worker has). It comes from the
//!   `external_services` row and the engine, and is the same on every node.
//!
//! Secrets are references in the spec ([`SecretRef`]) and placeholders in
//! the command (`$(REDIS_PASSWORD)`, Kubernetes' expansion syntax). Their
//! values travel next to the spec and are only put into the request by
//! [`database_instance`], so a spec can be logged, compared and sent.

use std::collections::BTreeMap;
use std::fmt;

use bollard::models::{ContainerCreateBody, ContainerInspectResponse, MountPointTypeEnum};
use serde::{Deserialize, Serialize};
use temps_workload::{
    labels as workload_labels, Artifact, Attachments, EnvSpec, InstanceLimits, InstanceSpec,
    Isolation, Lifetime, LogRotation, NetworkAttachment, PlacementSpec, PortBinding, PortProtocol,
    PortSpec, Probe, ProbeKind, Probes, Process, ResourceAmounts, Resources, RestartPolicy,
    SecretRef, SecurityProfile, VolumeMount, WorkloadKind, WorkloadRef, WorkloadSpec,
};
use temps_workload_docker::{data_container_create_body, ContainerShape, DataExtras};
use temps_workload_plane::{admit_data_service, AdmissionInput, AdmissionPolicy, Owner};

use crate::externalsvc::recreate_guard::ManagedEngine;
use crate::remote_service_client::RemoteServiceCreateParams;
use crate::workload::ServiceWorkloadError;

/// The owner kind of a database spec at admission.
pub const OWNER_KIND: &str = "external_service";

/// Log rotation every managed database container gets (the control plane's
/// `default_service_log_config`).
pub const DATABASE_LOG_MAX_SIZE: &str = "20m";
pub const DATABASE_LOG_MAX_FILE: u32 = 3;

/// The managed database engines on the workload plane. Object stores, KV
/// and clusters keep their own create paths for now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DatabaseEngine {
    Postgres,
    Redis,
    Mongodb,
    Mariadb,
}

impl DatabaseEngine {
    /// The engine of an `external_services.service_type`, if it is one.
    pub fn from_service_type(service_type: &str) -> Option<Self> {
        match service_type {
            "postgres" => Some(Self::Postgres),
            "redis" => Some(Self::Redis),
            "mongodb" => Some(Self::Mongodb),
            "mariadb" => Some(Self::Mariadb),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Postgres => "postgres",
            Self::Redis => "redis",
            Self::Mongodb => "mongodb",
            Self::Mariadb => "mariadb",
        }
    }

    /// The container name the engine (and the agent) give a service.
    pub fn container_name(self, service_name: &str) -> String {
        match self {
            Self::Postgres => crate::externalsvc::postgres::postgres_container_name(service_name),
            Self::Redis => crate::externalsvc::redis::redis_container_name(service_name),
            Self::Mongodb => crate::externalsvc::mongodb::mongodb_container_name(service_name),
            Self::Mariadb => crate::externalsvc::mariadb::mariadb_container_name(service_name),
        }
    }

    pub fn managed_engine(self) -> ManagedEngine {
        match self {
            Self::Postgres => ManagedEngine::Postgres,
            Self::Redis => ManagedEngine::Redis,
            Self::Mongodb => ManagedEngine::MongoDb,
            Self::Mariadb => ManagedEngine::MariaDb,
        }
    }

    /// Variables whose value is a secret.
    pub fn env_secrets(self) -> &'static [&'static str] {
        match self {
            Self::Postgres => &["POSTGRES_PASSWORD"],
            Self::Redis => &["REDIS_PASSWORD"],
            Self::Mongodb => &["MONGO_INITDB_ROOT_PASSWORD", "TEMPS_MONGO_KEYFILE_B64"],
            Self::Mariadb => &["MARIADB_ROOT_PASSWORD", "MARIADB_PASSWORD"],
        }
    }

    /// Command flags whose next argument is a secret, and its name.
    fn command_secret_flags(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::Redis => &[("--requirepass", "REDIS_PASSWORD")],
            Self::Postgres | Self::Mongodb | Self::Mariadb => &[],
        }
    }

    /// The `temps.*` labels the control-plane engine has always set.
    pub fn engine_labels(self, service_name: &str) -> BTreeMap<String, String> {
        let prefix = temps_core::DOCKER_LABEL_PREFIX;
        match self {
            Self::Mongodb => BTreeMap::from([
                (format!("{prefix}service"), "mongodb".to_string()),
                (format!("{prefix}name"), service_name.to_string()),
            ]),
            Self::Postgres | Self::Redis | Self::Mariadb => BTreeMap::from([
                (format!("{prefix}service_type"), self.as_str().to_string()),
                (format!("{prefix}service_name"), service_name.to_string()),
            ]),
        }
    }

    /// The engine's healthcheck, as §13 probes: readiness is the check,
    /// startup the grace period. The values are the control-plane engines'.
    pub fn probes(self, env: &BTreeMap<String, String>) -> Probes {
        let (script, period, timeout, failures, grace, grace_period) = match self {
            Self::Postgres => {
                let user = env
                    .get("POSTGRES_USER")
                    .map(String::as_str)
                    .unwrap_or("postgres");
                // The image creates POSTGRES_DB, defaulting to the user.
                let database = env.get("POSTGRES_DB").map(String::as_str).unwrap_or(user);
                (
                    crate::externalsvc::postgres::postgres_healthcheck_cmd(user, database),
                    1,
                    3,
                    3,
                    30,
                    1,
                )
            }
            Self::Redis => ("redis-cli ping".to_string(), 1, 3, 3, 5, 1),
            Self::Mongodb => (
                crate::externalsvc::mongodb::HEALTHCHECK_COMMAND.to_string(),
                2,
                10,
                5,
                45,
                2,
            ),
            Self::Mariadb => (
                "mariadb-admin ping -h 127.0.0.1 -uroot -p\"$MARIADB_ROOT_PASSWORD\" --silent"
                    .to_string(),
                1,
                3,
                5,
                30,
                1,
            ),
        };
        let exec = ProbeKind::Exec {
            command: vec!["/bin/sh".to_string(), "-c".to_string(), script],
        };
        Probes {
            startup: Some(Probe {
                initial_delay_secs: grace,
                period_secs: grace_period,
                ..Probe::new(exec.clone())
            }),
            readiness: Some(Probe {
                period_secs: period,
                timeout_secs: timeout,
                failure_threshold: failures,
                ..Probe::new(exec)
            }),
            liveness: None,
        }
    }
}

impl fmt::Display for DatabaseEngine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Host settings of a database that `Resources` (§5) does not carry
/// exactly: byte-precise memory and swap, `/dev/shm`, the CPU weight.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatabaseHostSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_bytes: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_swap_bytes: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nano_cpus: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shm_size_bytes: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_shares: Option<i64>,
}

/// Docker's `/dev/shm` when none is set; inspect reports it either way.
const DOCKER_DEFAULT_SHM_BYTES: i64 = 64 * 1024 * 1024;
const MIB: i64 = 1024 * 1024;

fn positive(value: Option<i64>) -> Option<i64> {
    value.filter(|v| *v > 0)
}

impl DatabaseHostSettings {
    fn from_host_config(host: Option<&bollard::models::HostConfig>) -> Self {
        let Some(host) = host else {
            return Self::default();
        };
        Self {
            memory_bytes: positive(host.memory),
            memory_swap_bytes: positive(host.memory_swap),
            nano_cpus: positive(host.nano_cpus),
            shm_size_bytes: positive(host.shm_size).filter(|v| *v != DOCKER_DEFAULT_SHM_BYTES),
            cpu_shares: positive(host.cpu_shares),
        }
    }

    fn resources(&self) -> Resources {
        let limits = ResourceAmounts {
            cpu_millis: self
                .nano_cpus
                .and_then(|n| u32::try_from(n / 1_000_000).ok()),
            memory_mb: self.memory_bytes.and_then(|b| u32::try_from(b / MIB).ok()),
            disk_mb: None,
            pids: None,
        };
        Resources {
            requests: None,
            limits: (limits != ResourceAmounts::default()).then_some(limits),
        }
    }
}

/// One secret of a database process.
#[derive(Clone, PartialEq, Eq)]
pub struct ProcessSecret {
    pub value: String,
    /// Whether the process receives it as a variable (`POSTGRES_PASSWORD`)
    /// or only as a command argument (Redis' `--requirepass` on a worker).
    pub in_env: bool,
}

/// What a database runs: the process half of its spec (see the module
/// documentation). `Debug` never prints a secret.
#[derive(Clone, PartialEq, Eq)]
pub struct DatabaseProcess {
    pub image: String,
    /// Non-secret variables.
    pub env: BTreeMap<String, String>,
    pub secrets: BTreeMap<String, ProcessSecret>,
    /// `None`: the image's command. Secret arguments are `$(NAME)`.
    pub command: Option<Vec<String>>,
    pub container_port: u16,
    /// `None`: the runtime picks one.
    pub host_port: Option<u16>,
    /// `(named volume, container path)`.
    pub volumes: Vec<(String, String)>,
    pub host: DatabaseHostSettings,
}

impl fmt::Debug for DatabaseProcess {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DatabaseProcess")
            .field("image", &self.image)
            .field("env", &self.env)
            .field("secrets", &self.secrets.keys().collect::<Vec<_>>())
            .field("command", &self.command)
            .field("container_port", &self.container_port)
            .field("host_port", &self.host_port)
            .field("volumes", &self.volumes)
            .field("host", &self.host)
            .finish()
    }
}

/// `$(NAME)`, the placeholder a secret argument is written as.
pub fn placeholder(name: &str) -> String {
    format!("$({name})")
}

/// Values shorter than this are not replaced inside a longer argument: a
/// short value can occur in an unrelated one by chance.
const MIN_EMBEDDED_SECRET_LEN: usize = 8;

fn split_env(entry: &str) -> (String, String) {
    match entry.split_once('=') {
        Some((name, value)) => (name.to_string(), value.to_string()),
        None => (entry.to_string(), String::new()),
    }
}

fn port_number(key: &str) -> Option<u16> {
    key.split('/').next().and_then(|p| p.parse().ok())
}

type PortBindings = std::collections::HashMap<String, Option<Vec<bollard::models::PortBinding>>>;

/// The first published port and the host port it is bound to.
fn published_port(
    bindings: Option<&PortBindings>,
    exposed: Option<&Vec<String>>,
) -> Option<(u16, Option<u16>)> {
    if let Some(bindings) = bindings {
        let mut keys: Vec<&String> = bindings.keys().collect();
        keys.sort();
        if let Some(key) = keys.first() {
            let container_port = port_number(key)?;
            let host_port = bindings
                .get(*key)
                .and_then(|b| b.as_ref())
                .and_then(|b| b.first())
                .and_then(|b| b.host_port.as_deref())
                .and_then(|p| p.parse().ok())
                .filter(|p: &u16| *p != 0);
            return Some((container_port, host_port));
        }
    }
    let mut exposed: Vec<&String> = exposed?.iter().collect();
    exposed.sort();
    Some((port_number(exposed.first()?)?, None))
}

fn bind_volume(bind: &str) -> Option<(String, String)> {
    let mut parts = bind.splitn(3, ':');
    Some((parts.next()?.to_string(), parts.next()?.to_string()))
}

impl DatabaseProcess {
    /// Sort the environment into variables and secrets, and replace the
    /// secrets in the command with placeholders.
    #[allow(clippy::too_many_arguments)]
    fn assemble(
        engine: DatabaseEngine,
        image: String,
        env: impl IntoIterator<Item = (String, String)>,
        command: Option<Vec<String>>,
        container_port: u16,
        host_port: Option<u16>,
        volumes: Vec<(String, String)>,
        host: DatabaseHostSettings,
    ) -> Self {
        let mut vars = BTreeMap::new();
        let mut secrets = BTreeMap::new();
        for (name, value) in env {
            if engine.env_secrets().contains(&name.as_str()) {
                secrets.insert(
                    name,
                    ProcessSecret {
                        value,
                        in_env: true,
                    },
                );
            } else {
                vars.insert(name, value);
            }
        }
        let command = command.map(|mut args| {
            for (flag, name) in engine.command_secret_flags() {
                let mut i = 0;
                while i + 1 < args.len() {
                    if args[i] == *flag {
                        let value = std::mem::replace(&mut args[i + 1], placeholder(name));
                        secrets.entry((*name).to_string()).or_insert(ProcessSecret {
                            value,
                            in_env: false,
                        });
                        i += 1;
                    }
                    i += 1;
                }
            }
            for (name, secret) in &secrets {
                if secret.value.is_empty() {
                    continue;
                }
                let marker = placeholder(name);
                for arg in args.iter_mut() {
                    if *arg == secret.value {
                        *arg = marker.clone();
                    } else if secret.value.len() >= MIN_EMBEDDED_SECRET_LEN
                        && arg.contains(&secret.value)
                    {
                        *arg = arg.replace(&secret.value, &marker);
                    }
                }
            }
            args
        });
        let mut volumes = volumes;
        volumes.sort();
        volumes.dedup();
        Self {
            image,
            env: vars,
            secrets,
            command,
            container_port,
            host_port,
            volumes,
            host,
        }
    }

    /// The process of a container-create request (the control-plane
    /// engine's, before it is sent).
    pub fn from_create_body(
        engine: DatabaseEngine,
        body: &ContainerCreateBody,
    ) -> Result<Self, String> {
        let image = body
            .image
            .clone()
            .filter(|i| !i.is_empty())
            .ok_or("the request has no image")?;
        let host = body.host_config.as_ref();
        // Fail closed on settings a database spec cannot carry: dropping
        // them silently would change what the container runs.
        let uncarried = [
            ("an entrypoint", body.entrypoint.is_some()),
            ("a user", body.user.is_some()),
            ("a working directory", body.working_dir.is_some()),
            (
                "added capabilities",
                host.is_some_and(|h| h.cap_add.is_some()),
            ),
            (
                "privileged mode",
                host.is_some_and(|h| h.privileged == Some(true)),
            ),
            ("devices", host.is_some_and(|h| h.devices.is_some())),
            ("tmpfs mounts", host.is_some_and(|h| h.tmpfs.is_some())),
        ];
        if let Some((what, _)) = uncarried.iter().find(|(_, set)| *set) {
            return Err(format!(
                "the request sets {what}, which a database workload spec does not carry"
            ));
        }
        let (container_port, host_port) = published_port(
            host.and_then(|h| h.port_bindings.as_ref()),
            body.exposed_ports.as_ref(),
        )
        .ok_or("the request publishes no port")?;
        let mut volumes: Vec<(String, String)> = host
            .and_then(|h| h.binds.as_ref())
            .into_iter()
            .flatten()
            .filter_map(|b| bind_volume(b))
            .collect();
        for mount in host.and_then(|h| h.mounts.as_ref()).into_iter().flatten() {
            if let (Some(source), Some(target)) = (&mount.source, &mount.target) {
                volumes.push((source.clone(), target.clone()));
            }
        }
        Ok(Self::assemble(
            engine,
            image,
            body.env.iter().flatten().map(|e| split_env(e)),
            body.cmd.clone(),
            container_port,
            host_port,
            volumes,
            DatabaseHostSettings::from_host_config(host),
        ))
    }

    /// The process an existing container runs. `image_env` and `image_cmd`
    /// are its image's, which Docker merged into the container's and which
    /// are not the database's to set.
    pub fn from_inspect(
        engine: DatabaseEngine,
        inspect: &ContainerInspectResponse,
        image_env: &[String],
        image_cmd: Option<&[String]>,
    ) -> Result<Self, String> {
        let config = inspect
            .config
            .as_ref()
            .ok_or("the container has no configuration")?;
        let image = config
            .image
            .clone()
            .filter(|i| !i.is_empty())
            .ok_or("the container has no image")?;
        let host = inspect.host_config.as_ref();
        let (container_port, host_port) = published_port(
            host.and_then(|h| h.port_bindings.as_ref()),
            config.exposed_ports.as_ref(),
        )
        .ok_or("the container publishes no port")?;
        let volumes = inspect
            .mounts
            .iter()
            .flatten()
            .filter_map(|mount| {
                let source = match mount.typ {
                    Some(MountPointTypeEnum::VOLUME) => mount.name.clone()?,
                    Some(MountPointTypeEnum::BIND) => mount.source.clone()?,
                    _ => return None,
                };
                Some((source, mount.destination.clone()?))
            })
            .collect();
        let env = config
            .env
            .iter()
            .flatten()
            .filter(|entry| !image_env.contains(entry))
            .map(|e| split_env(e));
        let command = config
            .cmd
            .clone()
            .filter(|cmd| image_cmd != Some(cmd.as_slice()));
        Ok(Self::assemble(
            engine,
            image,
            env,
            command,
            container_port,
            host_port,
            volumes,
            DatabaseHostSettings::from_host_config(host),
        ))
    }

    /// The process of the agent request a worker database is created with.
    pub fn from_remote_params(engine: DatabaseEngine, params: &RemoteServiceCreateParams) -> Self {
        let (container_port, host_port) = params
            .port_mappings
            .first()
            .map(|m| (m.container_port, Some(m.host_port).filter(|p| *p != 0)))
            .unwrap_or((0, None));
        let limits = params.resource_limits.clone().unwrap_or_default();
        Self::assemble(
            engine,
            params.image.clone(),
            params
                .environment
                .iter()
                .map(|(k, v)| (k.clone(), v.clone())),
            params.command.clone(),
            container_port,
            host_port,
            params
                .volumes
                .iter()
                .map(|(volume, path)| (volume.clone(), path.clone()))
                .collect(),
            DatabaseHostSettings {
                memory_bytes: limits.memory_mb.map(|mb| mb.saturating_mul(MIB)),
                memory_swap_bytes: limits.memory_swap_mb.map(|mb| mb.saturating_mul(MIB)),
                nano_cpus: limits.nano_cpus,
                shm_size_bytes: limits.shm_size_mb.map(|mb| mb.saturating_mul(MIB)),
                cpu_shares: limits.cpu_shares,
            },
        )
    }

    /// Secret values by name, for [`SecretValues::Resolved`].
    pub fn secret_values(&self) -> BTreeMap<String, String> {
        self.secrets
            .iter()
            .map(|(name, secret)| (name.clone(), secret.value.clone()))
            .collect()
    }
}

/// Which database a spec is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabaseIdentity {
    pub service_id: i32,
    pub service_name: String,
    pub engine: DatabaseEngine,
    /// `None`: the control plane.
    pub node_id: Option<i32>,
    pub container_name: String,
}

const SECRET_IN_ENV: &str = "env";
const SECRET_IN_COMMAND: &str = "command";

fn secret_source(service_id: i32, in_env: bool) -> String {
    format!(
        "{OWNER_KIND}:{service_id}:{}",
        if in_env {
            SECRET_IN_ENV
        } else {
            SECRET_IN_COMMAND
        }
    )
}

fn secret_in_env(secret: &SecretRef) -> bool {
    !secret.source.ends_with(&format!(":{SECRET_IN_COMMAND}"))
}

/// The labels both paths give a database container: the control plane's
/// `temps.*` and the worker's `sh.temps.*`.
pub fn database_labels(identity: &DatabaseIdentity) -> BTreeMap<String, String> {
    let mut labels = identity.engine.engine_labels(&identity.service_name);
    labels.extend([
        ("sh.temps.managed".to_string(), "true".to_string()),
        ("sh.temps.service".to_string(), "true".to_string()),
        (
            "sh.temps.service.type".to_string(),
            identity.engine.as_str().to_string(),
        ),
        (
            "sh.temps.service.name".to_string(),
            identity.container_name.clone(),
        ),
    ]);
    labels
}

/// The database's spec, before admission.
pub fn database_workload_spec(
    identity: &DatabaseIdentity,
    process: &DatabaseProcess,
) -> WorkloadSpec {
    let mut probe_env = process.env.clone();
    for (name, secret) in &process.secrets {
        probe_env.insert(name.clone(), secret.value.clone());
    }
    WorkloadSpec {
        owner: WorkloadRef::new(WorkloadKind::Service, i64::from(identity.service_id)),
        artifact: Artifact::oci(process.image.clone()),
        isolation: Isolation::Process,
        process: Process {
            command: process.command.clone(),
            ..Default::default()
        },
        env: EnvSpec {
            vars: process.env.clone(),
            secrets: process
                .secrets
                .iter()
                .map(|(name, secret)| SecretRef {
                    name: name.clone(),
                    source: secret_source(identity.service_id, secret.in_env),
                })
                .collect(),
            ..Default::default()
        },
        resources: process.host.resources(),
        volumes: process
            .volumes
            .iter()
            .map(|(volume, path)| VolumeMount {
                volume: volume.clone(),
                mount_path: path.clone(),
                read_only: false,
                source: None,
            })
            .collect(),
        ports: vec![PortSpec {
            name: identity.engine.as_str().to_string(),
            container_port: process.container_port,
            protocol: PortProtocol::Tcp,
            app_protocol: Default::default(),
        }],
        network: Default::default(),
        probes: identity.engine.probes(&probe_env),
        placement: PlacementSpec {
            // Node 0 is the control plane, as in the node list.
            nodes: Some(vec![identity.node_id.unwrap_or(0)]),
            ..Default::default()
        },
        lifetime: Lifetime::LongRunning {
            restart: RestartPolicy::Always,
        },
        security: None,
        labels: database_labels(identity),
    }
}

/// A database's admitted spec and the node-independent settings it is
/// created with. Holds no secret value: it is logged, compared, and sent to
/// a worker's agent next to the values.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DatabaseWorkloadPlan {
    pub service_id: i32,
    pub engine: DatabaseEngine,
    pub container_name: String,
    pub spec: WorkloadSpec,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_port: Option<u16>,
    #[serde(default)]
    pub host: DatabaseHostSettings,
}

impl DatabaseWorkloadPlan {
    /// Build and admit the spec of `process` for `identity`.
    pub fn admit(
        identity: &DatabaseIdentity,
        process: &DatabaseProcess,
    ) -> Result<Self, ServiceWorkloadError> {
        let spec = database_workload_spec(identity, process);
        let owner = Owner::new(OWNER_KIND, i64::from(identity.service_id));
        let admitted = admit_data_service(
            &AdmissionPolicy::default(),
            AdmissionInput {
                spec,
                name: &identity.container_name,
                owner: &owner,
                project_slug: None,
                active_for_owner: 0,
            },
        )
        .map_err(|refusal| ServiceWorkloadError::Admission {
            service_id: identity.service_id,
            container: identity.container_name.clone(),
            rule: refusal.rule.to_string(),
            message: refusal.message,
        })?;
        Ok(Self {
            service_id: identity.service_id,
            engine: identity.engine,
            container_name: identity.container_name.clone(),
            spec: admitted,
            host_port: process.host_port,
            host: process.host,
        })
    }

    /// Admit the plan's spec again where it is executed (a worker's agent):
    /// a plan is never trusted to have been admitted by its sender.
    pub fn readmit(&self) -> Result<Self, ServiceWorkloadError> {
        let owner = Owner::new(OWNER_KIND, i64::from(self.service_id));
        let admitted = admit_data_service(
            &AdmissionPolicy::default(),
            AdmissionInput {
                spec: self.spec.clone(),
                name: &self.container_name,
                owner: &owner,
                project_slug: None,
                active_for_owner: 0,
            },
        )
        .map_err(|refusal| ServiceWorkloadError::Admission {
            service_id: self.service_id,
            container: self.container_name.clone(),
            rule: refusal.rule.to_string(),
            message: refusal.message,
        })?;
        if admitted.owner.id != i64::from(self.service_id) {
            return Err(ServiceWorkloadError::Admission {
                service_id: self.service_id,
                container: self.container_name.clone(),
                rule: "validation".to_string(),
                message: format!(
                    "the spec belongs to owner {}, not service {}",
                    admitted.owner.id, self.service_id
                ),
            });
        }
        Ok(Self {
            spec: admitted,
            ..self.clone()
        })
    }

    /// The OCI image reference, which admission requires.
    pub fn image(&self) -> Option<&str> {
        match &self.spec.artifact {
            Artifact::OciImage { reference, .. } => Some(reference),
            _ => None,
        }
    }

    pub fn secret_names(&self) -> impl Iterator<Item = &str> {
        self.spec.env.secrets.iter().map(|s| s.name.as_str())
    }
}

/// How secrets are put into an instance.
#[derive(Debug, Clone, Copy)]
pub enum SecretValues<'a> {
    /// The values, for a container that is created.
    Resolved(&'a BTreeMap<String, String>),
    /// Variables shown as `***` and arguments left as `$(NAME)`: the
    /// request a spec stands for, for comparing and logging.
    Masked,
}

/// Where on its node an instance is attached. The only part of a database
/// request that differs by node, on purpose.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NodeAttachment {
    /// The interface the port is published on: loopback on the control
    /// plane, the private address on a worker.
    pub host_ip: String,
    pub network: Option<String>,
    /// Resolvers: the overlay's on a worker, Docker's default otherwise.
    pub dns: Option<Vec<String>>,
}

/// Everything the Docker driver needs to create a database instance.
#[derive(Debug, Clone)]
pub struct DatabaseInstance {
    pub spec: InstanceSpec,
    pub probes: Probes,
    pub extras: DataExtras,
    pub attach: Attachments,
}

fn expand(arg: &str, values: &BTreeMap<String, String>) -> String {
    let mut out = arg.to_string();
    for (name, value) in values {
        let marker = placeholder(name);
        if out.contains(&marker) {
            out = out.replace(&marker, value);
        }
    }
    out
}

/// The instance an admitted plan is created as on a node.
pub fn database_instance(
    plan: &DatabaseWorkloadPlan,
    secrets: SecretValues<'_>,
    node: &NodeAttachment,
) -> Result<DatabaseInstance, ServiceWorkloadError> {
    let spec = &plan.spec;
    let missing = |name: &str| ServiceWorkloadError::Process {
        service_id: plan.service_id,
        container: plan.container_name.clone(),
        reason: format!("no value was supplied for secret '{name}'"),
    };
    let mut env: BTreeMap<String, String> = spec.env.vars.clone();
    for secret in &spec.env.secrets {
        let value = match secrets {
            SecretValues::Resolved(values) => values
                .get(&secret.name)
                .cloned()
                .ok_or_else(|| missing(&secret.name))?,
            SecretValues::Masked => temps_workload_docker::shape::MASKED.to_string(),
        };
        if secret_in_env(secret) {
            env.insert(secret.name.clone(), value);
        }
    }
    // Admission only lets a long-running database through; anything else
    // never reaches a container.
    let restart = match spec.lifetime {
        Lifetime::LongRunning { restart } => restart,
        Lifetime::RunToCompletion { .. } | Lifetime::ScaleToZero { .. } => {
            return Err(ServiceWorkloadError::Process {
                service_id: plan.service_id,
                container: plan.container_name.clone(),
                reason: format!("a database must be long-running, not {:?}", spec.lifetime),
            });
        }
    };
    let command = match secrets {
        SecretValues::Resolved(values) => spec
            .process
            .command
            .as_ref()
            .map(|args| args.iter().map(|a| expand(a, values)).collect()),
        SecretValues::Masked => spec.process.command.clone(),
    };
    let image = match &spec.artifact {
        Artifact::OciImage { reference, .. } => reference.clone(),
        other => {
            return Err(ServiceWorkloadError::Process {
                service_id: plan.service_id,
                container: plan.container_name.clone(),
                reason: format!("artifact {other:?} is not an OCI image"),
            })
        }
    };
    let mut labels = spec.labels.clone();
    labels.insert(
        workload_labels::KIND.to_string(),
        WorkloadKind::Service.as_str().to_string(),
    );
    labels.insert(
        workload_labels::OWNER_ID.to_string(),
        spec.owner.id.to_string(),
    );
    let ports = spec
        .ports
        .iter()
        .map(|port| PortBinding {
            container_port: port.container_port,
            protocol: port.protocol,
            host_ip: node.host_ip.clone(),
            host_port: plan.host_port,
        })
        .collect();
    let instance = InstanceSpec {
        owner: spec.owner,
        name: plan.container_name.clone(),
        image,
        platform: None,
        env: env.into_iter().collect(),
        secret_files: Vec::new(),
        command,
        entrypoint: spec.process.entrypoint.clone(),
        workdir: spec.process.workdir.clone(),
        user: spec.process.user.clone(),
        ports,
        restart,
        limits: InstanceLimits {
            nano_cpus: plan.host.nano_cpus,
            memory_bytes: plan.host.memory_bytes,
            memory_swap_bytes: plan.host.memory_swap_bytes,
            pids: None,
            disk_mb: None,
        },
        log_rotation: Some(LogRotation {
            max_size: DATABASE_LOG_MAX_SIZE.to_string(),
            max_file: DATABASE_LOG_MAX_FILE,
        }),
        labels,
        security: spec.security.unwrap_or(SecurityProfile::Data),
    };
    Ok(DatabaseInstance {
        spec: instance,
        probes: spec.probes.clone(),
        extras: DataExtras {
            shm_size_bytes: plan.host.shm_size_bytes,
            cpu_shares: plan.host.cpu_shares,
        },
        attach: Attachments {
            network: NetworkAttachment {
                primary: node.network.clone(),
                required: Vec::new(),
                optional: Vec::new(),
                dns: node.dns.clone(),
            },
            volumes: spec
                .volumes
                .iter()
                .map(|v| {
                    if v.read_only {
                        format!("{}:{}:ro", v.volume, v.mount_path)
                    } else {
                        format!("{}:{}", v.volume, v.mount_path)
                    }
                })
                .collect(),
        },
    })
}

/// The container-create request of a plan on a node: what both paths send.
pub fn database_create_body(
    plan: &DatabaseWorkloadPlan,
    secrets: SecretValues<'_>,
    node: &NodeAttachment,
) -> Result<ContainerCreateBody, ServiceWorkloadError> {
    let instance = database_instance(plan, secrets, node)?;
    Ok(data_container_create_body(
        &instance.spec,
        &instance.probes,
        &instance.extras,
        &instance.attach,
        Vec::new(),
    ))
}

/// The shape the plan gives a container, with secrets masked.
pub fn expected_shape(plan: &DatabaseWorkloadPlan) -> Result<ContainerShape, ServiceWorkloadError> {
    let body = database_create_body(plan, SecretValues::Masked, &NodeAttachment::default())?;
    Ok(ContainerShape::from_create_body(&body))
}

/// The shape of a running database container, with the plan's secrets
/// masked.
pub fn observed_shape(
    plan: &DatabaseWorkloadPlan,
    inspect: &ContainerInspectResponse,
) -> ContainerShape {
    ContainerShape::from_inspect(inspect).mask(plan.secret_names())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn identity(engine: DatabaseEngine, container: &str, node_id: Option<i32>) -> DatabaseIdentity {
        DatabaseIdentity {
            service_id: 4,
            service_name: "orders".into(),
            engine,
            node_id,
            container_name: container.into(),
        }
    }

    fn redis_params() -> RemoteServiceCreateParams {
        RemoteServiceCreateParams {
            name: "redis-orders".into(),
            service_type: "redis".into(),
            image: "gotempsh/redis-walg:8-bookworm".into(),
            environment: HashMap::new(),
            port_mappings: vec![crate::remote_service_client::RemotePortMapping {
                host_port: 16379,
                container_port: 6379,
            }],
            volumes: HashMap::from([("redis-orders_data".into(), "/data".into())]),
            network: Some("temps-app-network".into()),
            command: Some(vec![
                "redis-server".into(),
                "--requirepass".into(),
                "seeded-secret-1".into(),
            ]),
            resource_limits: None,
        }
    }

    #[test]
    fn a_command_only_secret_becomes_a_placeholder_and_never_a_variable() {
        let process = DatabaseProcess::from_remote_params(DatabaseEngine::Redis, &redis_params());
        assert_eq!(
            process.command,
            Some(vec![
                "redis-server".into(),
                "--requirepass".into(),
                "$(REDIS_PASSWORD)".into()
            ])
        );
        assert!(!process.secrets["REDIS_PASSWORD"].in_env);
        assert!(!format!("{process:?}").contains("seeded-secret-1"));

        let plan = DatabaseWorkloadPlan::admit(
            &identity(DatabaseEngine::Redis, "redis-orders", Some(3)),
            &process,
        )
        .expect("admitted");
        assert!(!serde_json::to_string(&plan)
            .unwrap()
            .contains("seeded-secret-1"));

        let values = process.secret_values();
        let body = database_create_body(
            &plan,
            SecretValues::Resolved(&values),
            &NodeAttachment {
                host_ip: "10.0.0.2".into(),
                network: Some("temps-app-network".into()),
                dns: None,
            },
        )
        .unwrap();
        assert_eq!(
            body.cmd,
            Some(vec![
                "redis-server".into(),
                "--requirepass".into(),
                "seeded-secret-1".into()
            ])
        );
        assert!(
            !body
                .env
                .unwrap()
                .iter()
                .any(|e| e.starts_with("REDIS_PASSWORD=")),
            "the worker's Redis never had the variable"
        );
    }

    #[test]
    fn the_worker_request_gains_the_control_plane_policy_and_keeps_its_process() {
        let process = DatabaseProcess::from_remote_params(DatabaseEngine::Redis, &redis_params());
        let plan = DatabaseWorkloadPlan::admit(
            &identity(DatabaseEngine::Redis, "redis-orders", Some(3)),
            &process,
        )
        .unwrap();
        let values = process.secret_values();
        let body = database_create_body(
            &plan,
            SecretValues::Resolved(&values),
            &NodeAttachment {
                host_ip: "10.0.0.2".into(),
                network: Some("temps-app-network".into()),
                dns: Some(vec!["10.0.0.1".into()]),
            },
        )
        .unwrap();
        let host = body.host_config.as_ref().unwrap();
        assert_eq!(host.pids_limit, Some(512));
        assert_eq!(
            host.security_opt,
            Some(vec!["no-new-privileges:true".into()])
        );
        assert_eq!(
            host.restart_policy.as_ref().and_then(|p| p.name),
            Some(bollard::models::RestartPolicyNameEnum::ALWAYS)
        );
        assert_eq!(host.binds, Some(vec!["redis-orders_data:/data".into()]));
        assert_eq!(host.network_mode.as_deref(), Some("temps-app-network"));
        assert_eq!(host.dns, Some(vec!["10.0.0.1".into()]));
        let labels = body.labels.as_ref().unwrap();
        assert_eq!(labels["temps.service_type"], "redis");
        assert_eq!(labels["temps.service_name"], "orders");
        assert_eq!(labels["sh.temps.service.name"], "redis-orders");
        assert_eq!(labels["sh.temps.kind"], "service");
        assert_eq!(labels["sh.temps.owner_id"], "4");
        // Never the plane's own labels: its orphan sweep must not see it.
        assert!(!labels.contains_key(temps_workload_plane::labels::PLANE));
        assert!(!labels.contains_key(temps_workload_plane::labels::MANAGED_BY));
        let health = body.healthcheck.as_ref().unwrap();
        assert_eq!(
            health.test,
            Some(vec!["CMD-SHELL".into(), "redis-cli ping".into()])
        );
        assert_eq!(health.start_period, Some(5_000_000_000));
    }

    #[test]
    fn a_postgres_healthcheck_uses_the_configured_user_and_database() {
        let probes = DatabaseEngine::Postgres.probes(&BTreeMap::from([
            ("POSTGRES_USER".to_string(), "app".to_string()),
            ("POSTGRES_DB".to_string(), "shop".to_string()),
        ]));
        let health = temps_workload_docker::data::healthcheck(&probes).unwrap();
        assert_eq!(
            health.test,
            Some(vec![
                "CMD-SHELL".into(),
                crate::externalsvc::postgres::postgres_healthcheck_cmd("app", "shop")
            ])
        );
        assert_eq!(health.retries, Some(3));
        assert_eq!(health.start_period, Some(30_000_000_000));
        // Without POSTGRES_DB the image's database is the user's.
        let probes = DatabaseEngine::Postgres.probes(&BTreeMap::from([(
            "POSTGRES_USER".to_string(),
            "app".to_string(),
        )]));
        let health = temps_workload_docker::data::healthcheck(&probes).unwrap();
        assert_eq!(
            health.test.unwrap()[1],
            crate::externalsvc::postgres::postgres_healthcheck_cmd("app", "app")
        );
    }

    #[test]
    fn a_missing_secret_value_fails_the_instance_with_its_name() {
        let process = DatabaseProcess::from_remote_params(DatabaseEngine::Redis, &redis_params());
        let plan = DatabaseWorkloadPlan::admit(
            &identity(DatabaseEngine::Redis, "redis-orders", None),
            &process,
        )
        .unwrap();
        let error = database_instance(
            &plan,
            SecretValues::Resolved(&BTreeMap::new()),
            &NodeAttachment::default(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("REDIS_PASSWORD"), "{error}");
    }

    #[test]
    fn a_host_path_volume_is_refused_at_admission() {
        let mut params = redis_params();
        params.volumes = HashMap::from([("/srv/redis".into(), "/data".into())]);
        let process = DatabaseProcess::from_remote_params(DatabaseEngine::Redis, &params);
        let error = DatabaseWorkloadPlan::admit(
            &identity(DatabaseEngine::Redis, "redis-orders", None),
            &process,
        )
        .unwrap_err();
        assert!(
            matches!(error, ServiceWorkloadError::Admission { ref rule, .. } if rule == "validation"),
            "{error}"
        );
    }

    #[test]
    fn engines_map_from_service_types() {
        for engine in [
            DatabaseEngine::Postgres,
            DatabaseEngine::Redis,
            DatabaseEngine::Mongodb,
            DatabaseEngine::Mariadb,
        ] {
            assert_eq!(
                DatabaseEngine::from_service_type(engine.as_str()),
                Some(engine)
            );
        }
        assert_eq!(DatabaseEngine::from_service_type("kv"), None);
        assert_eq!(DatabaseEngine::from_service_type("s3"), None);
    }
}
