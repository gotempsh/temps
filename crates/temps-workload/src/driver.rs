// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The runtime driver trait (ADR-055 §6).
//!
//! A driver runs instances on one node: Docker, Firecracker, microsandbox,
//! later WASM. It receives an [`InstanceSpec`], the spec of one instance
//! with everything resolved (secret values, host port bindings, the
//! security profile admission chose), and starts processes. Networking and
//! storage are separate plug points ([`Attachments`]); a driver only
//! reports what it can attach to ([`AttachmentKind`]).
//!
//! Capabilities are filters, not hints: the scheduler never places an
//! instance on a driver that lacks what its spec needs.

use std::collections::BTreeMap;
use std::fmt;

use async_trait::async_trait;
use bytes::Bytes;
use chrono::{DateTime, Utc};
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};

use crate::{Artifact, Isolation, PortProtocol, RestartPolicy, SecurityProfile, WorkloadRef};

/// Which artifact formats a driver runs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactKinds {
    pub oci: bool,
    pub wasm: bool,
    pub rootfs: bool,
}

/// What a driver gives the network attacher to apply policy to (§6, §9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentKind {
    /// A Docker network, with nftables rules installed around it.
    DockerNetwork,
    /// A TAP device (Firecracker).
    Tap,
    /// The microsandbox SDK's network policy.
    SdkPolicy,
    /// An in-host socket allow-list (WASM).
    InHost,
    /// No attachment point: network policy cannot be enforced.
    None,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeKinds {
    pub named: bool,
    pub block_image: bool,
    pub preopened_dir: bool,
}

/// Which limits a driver can enforce. A limit it cannot enforce is
/// reported as `LimitEnforced = False`, never ignored.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LimitSupport {
    pub cpu: bool,
    pub memory: bool,
    pub pids: bool,
    pub disk: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriverCapabilities {
    /// `docker`, `firecracker`, `microsandbox`, `wasm`, `local`.
    pub name: String,
    pub artifacts: ArtifactKinds,
    pub isolation: Isolation,
    pub network_attachment: AttachmentKind,
    pub volumes: VolumeKinds,
    pub exec: bool,
    pub snapshot: bool,
    pub resize_disk: bool,
    pub limits: LimitSupport,
}

/// A prepared artifact, ready to create instances from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreparedArtifact {
    pub artifact: Artifact,
    /// The driver's local name for it (an image id, a rootfs path).
    pub local_ref: String,
}

/// A secret file delivered at `/run/secrets/<name>`. The value never
/// appears in `Debug` output.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SecretFile {
    pub name: String,
    pub value: String,
}

impl fmt::Debug for SecretFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecretFile")
            .field("name", &self.name)
            .field("value", &"***")
            .finish()
    }
}

/// A published port, with the host binding the plane allocated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortBinding {
    pub container_port: u16,
    pub protocol: PortProtocol,
    /// Host interface. Never `0.0.0.0`.
    pub host_ip: String,
    /// `None`: the runtime picks a free port.
    pub host_port: Option<u16>,
}

/// Rotation for the instance's own log files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogRotation {
    pub max_size: String,
    pub max_file: u32,
}

/// Limits as the driver applies them. Derived from `Resources` (§5) and
/// the profile's caps by the plane; carried in the units runtimes take so
/// a driver never re-derives (and re-rounds) them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceLimits {
    pub nano_cpus: Option<i64>,
    pub memory_bytes: Option<i64>,
    /// Memory plus swap. Equal to `memory_bytes` disables swap.
    pub memory_swap_bytes: Option<i64>,
    pub pids: Option<i64>,
    pub disk_mb: Option<u32>,
}

/// The spec of one instance, fully resolved. Transient: built for a
/// create call and never stored, because it holds secret values.
#[derive(Debug, Clone, PartialEq)]
pub struct InstanceSpec {
    pub owner: WorkloadRef,
    /// The runtime's name for the instance (a container name).
    pub name: String,
    pub image: String,
    /// `os/arch`, when the image must match a platform.
    pub platform: Option<String>,
    pub env: Vec<(String, String)>,
    pub secret_files: Vec<SecretFile>,
    pub command: Option<Vec<String>>,
    pub entrypoint: Option<Vec<String>>,
    pub workdir: Option<String>,
    pub user: Option<String>,
    pub ports: Vec<PortBinding>,
    pub restart: RestartPolicy,
    pub limits: InstanceLimits,
    pub log_rotation: Option<LogRotation>,
    pub labels: BTreeMap<String, String>,
    pub security: SecurityProfile,
}

/// Where the attacher put the instance: networks to join and the resolver.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkAttachment {
    /// The network the instance boots on.
    pub primary: Option<String>,
    /// Networks it must also join before it starts. A failure fails the
    /// create.
    pub required: Vec<String>,
    /// Networks it joins if they exist (the multi-host overlay before it is
    /// bootstrapped on this node).
    pub optional: Vec<String>,
    /// Resolvers. `None` keeps the runtime's default.
    pub dns: Option<Vec<String>>,
}

/// Answers the resolvers a new instance should use, read at each create:
/// the node's cluster DNS resolver (ADR-024) and its public fallbacks, or
/// `None` while that resolver is not up (the runtime's default then).
#[derive(Clone)]
pub struct ResolverSource(std::sync::Arc<dyn Fn() -> Option<Vec<String>> + Send + Sync>);

impl ResolverSource {
    pub fn new(resolvers: impl Fn() -> Option<Vec<String>> + Send + Sync + 'static) -> Self {
        Self(std::sync::Arc::new(resolvers))
    }

    pub fn resolvers(&self) -> Option<Vec<String>> {
        (self.0)()
    }

    /// Fill `network.dns` unless the template already chose resolvers.
    pub fn apply(&self, network: &mut NetworkAttachment) {
        if network.dns.is_none() {
            network.dns = self.resolvers();
        }
    }
}

impl fmt::Debug for ResolverSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("ResolverSource")
            .field(&self.resolvers())
            .finish()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachments {
    pub network: NetworkAttachment,
    /// Volume binds (`<source>:<target>[:ro]`), from the volume provider.
    pub volumes: Vec<String>,
}

/// A driver's handle to an instance it created.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct InstanceHandle {
    pub driver: String,
    /// The runtime's id (a container id).
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstanceState {
    Created,
    Running,
    Exited,
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceStatus {
    pub state: InstanceState,
    pub exit_code: Option<i64>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    /// The instance's address on its primary network.
    pub address: Option<String>,
    /// Ports the runtime actually published (host port picked at create).
    pub published_ports: Vec<PortBinding>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LogQuery {
    pub since: Option<DateTime<Utc>>,
    pub tail: Option<u32>,
    pub follow: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLine {
    pub stderr: bool,
    pub line: Bytes,
}

pub type LogStream = BoxStream<'static, Result<LogLine, DriverError>>;
pub type ExecStream = BoxStream<'static, Result<LogLine, DriverError>>;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceStats {
    pub cpu_nanos: Option<u64>,
    pub memory_bytes: Option<u64>,
    pub pids: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecSpec {
    pub command: Vec<String>,
    pub env: Vec<(String, String)>,
    pub workdir: Option<String>,
}

/// An instance a driver found on its node, with the labels it carries.
/// Used by the reconciler to find instances it no longer tracks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedInstance {
    pub handle: InstanceHandle,
    pub labels: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotRef {
    pub driver: String,
    pub id: String,
}

/// Errors every driver returns. Each names the instance and the driver.
#[derive(Debug, thiserror::Error)]
pub enum DriverError {
    #[error("driver '{driver}' does not support {operation} (instance '{instance}')")]
    Unsupported {
        driver: String,
        operation: &'static str,
        instance: String,
    },
    #[error("driver '{driver}' has no instance '{instance}'")]
    NotFound { driver: String, instance: String },
    #[error("driver '{driver}' is unavailable: {reason}")]
    Unavailable { driver: String, reason: String },
    #[error("driver '{driver}' could not prepare artifact '{artifact}': {reason}")]
    Artifact {
        driver: String,
        artifact: String,
        reason: String,
    },
    #[error("driver '{driver}' could not deliver secrets to instance '{instance}': {reason}")]
    Secrets {
        driver: String,
        instance: String,
        reason: String,
    },
    #[error(
        "driver '{driver}' could not attach instance '{instance}' to network '{network}': {reason}"
    )]
    Attach {
        driver: String,
        instance: String,
        network: String,
        reason: String,
    },
    #[error("driver '{driver}' failed to {operation} instance '{instance}': {reason}")]
    Runtime {
        driver: String,
        operation: &'static str,
        instance: String,
        reason: String,
    },
    /// A backend failure kept whole, so a facade over the driver can recover
    /// the backend's own error (and its HTTP mapping) by downcasting.
    #[error("driver '{driver}' failed to {operation} instance '{instance}': {source}")]
    Backend {
        driver: String,
        operation: &'static str,
        instance: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

#[async_trait]
pub trait RuntimeDriver: Send + Sync {
    fn capabilities(&self) -> &DriverCapabilities;

    async fn prepare(&self, artifact: &Artifact) -> Result<PreparedArtifact, DriverError>;
    async fn create(
        &self,
        spec: &InstanceSpec,
        attach: &Attachments,
    ) -> Result<InstanceHandle, DriverError>;
    async fn start(&self, handle: &InstanceHandle) -> Result<(), DriverError>;
    async fn stop(&self, handle: &InstanceHandle) -> Result<(), DriverError>;
    /// `purge` also removes what the instance owns on the node (its secret
    /// files, anonymous volumes).
    async fn remove(&self, handle: &InstanceHandle, purge: bool) -> Result<(), DriverError>;
    async fn inspect(&self, handle: &InstanceHandle) -> Result<InstanceStatus, DriverError>;
    async fn logs(
        &self,
        handle: &InstanceHandle,
        query: LogQuery,
    ) -> Result<LogStream, DriverError>;
    async fn stats(&self, handle: &InstanceHandle) -> Result<InstanceStats, DriverError>;

    /// Gated on `capabilities().exec`.
    async fn exec(
        &self,
        handle: &InstanceHandle,
        _cmd: ExecSpec,
    ) -> Result<ExecStream, DriverError> {
        Err(DriverError::Unsupported {
            driver: self.capabilities().name.clone(),
            operation: "exec",
            instance: handle.name.clone(),
        })
    }

    /// Gated on `capabilities().snapshot`.
    async fn snapshot(&self, handle: &InstanceHandle) -> Result<SnapshotRef, DriverError> {
        Err(DriverError::Unsupported {
            driver: self.capabilities().name.clone(),
            operation: "snapshot",
            instance: handle.name.clone(),
        })
    }

    /// Every instance on this node carrying all of `labels` (running or
    /// not). The reconciler uses it to find instances it no longer tracks;
    /// a driver that cannot list returns `Unsupported`, and orphan cleanup
    /// is skipped with a logged reason.
    async fn list(
        &self,
        _labels: &BTreeMap<String, String>,
    ) -> Result<Vec<ListedInstance>, DriverError> {
        Err(DriverError::Unsupported {
            driver: self.capabilities().name.clone(),
            operation: "list",
            instance: String::new(),
        })
    }

    /// Resolves when the instance is no longer running (it exited, or it is
    /// gone). Lets the reconciler react to an exit instead of polling; a
    /// driver that cannot wait returns `Unsupported` and the reconciler
    /// polls `inspect` instead.
    async fn wait(&self, handle: &InstanceHandle) -> Result<(), DriverError> {
        Err(DriverError::Unsupported {
            driver: self.capabilities().name.clone(),
            operation: "wait",
            instance: handle.name.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_values_never_appear_in_debug_output() {
        let file = SecretFile {
            name: "DB_PASSWORD".into(),
            value: "hunter2-seeded".into(),
        };
        let debug = format!("{file:?}");
        assert!(debug.contains("DB_PASSWORD"));
        assert!(!debug.contains("hunter2-seeded"), "{debug}");
    }

    struct Minimal(DriverCapabilities);

    #[async_trait]
    impl RuntimeDriver for Minimal {
        fn capabilities(&self) -> &DriverCapabilities {
            &self.0
        }
        async fn prepare(&self, artifact: &Artifact) -> Result<PreparedArtifact, DriverError> {
            Ok(PreparedArtifact {
                artifact: artifact.clone(),
                local_ref: "x".into(),
            })
        }
        async fn create(
            &self,
            s: &InstanceSpec,
            _: &Attachments,
        ) -> Result<InstanceHandle, DriverError> {
            Ok(InstanceHandle {
                driver: "minimal".into(),
                id: "1".into(),
                name: s.name.clone(),
            })
        }
        async fn start(&self, _: &InstanceHandle) -> Result<(), DriverError> {
            Ok(())
        }
        async fn stop(&self, _: &InstanceHandle) -> Result<(), DriverError> {
            Ok(())
        }
        async fn remove(&self, _: &InstanceHandle, _: bool) -> Result<(), DriverError> {
            Ok(())
        }
        async fn inspect(&self, h: &InstanceHandle) -> Result<InstanceStatus, DriverError> {
            Err(DriverError::NotFound {
                driver: "minimal".into(),
                instance: h.name.clone(),
            })
        }
        async fn logs(&self, _: &InstanceHandle, _: LogQuery) -> Result<LogStream, DriverError> {
            Ok(Box::pin(futures::stream::empty()))
        }
        async fn stats(&self, _: &InstanceHandle) -> Result<InstanceStats, DriverError> {
            Ok(InstanceStats::default())
        }
    }

    #[tokio::test]
    async fn optional_operations_default_to_a_typed_unsupported_error() {
        let driver = Minimal(DriverCapabilities {
            name: "minimal".into(),
            artifacts: ArtifactKinds::default(),
            isolation: Isolation::Process,
            network_attachment: AttachmentKind::None,
            volumes: VolumeKinds::default(),
            exec: false,
            snapshot: false,
            resize_disk: false,
            limits: LimitSupport::default(),
        });
        let handle = InstanceHandle {
            driver: "minimal".into(),
            id: "1".into(),
            name: "job-3-run-1".into(),
        };
        let error = driver.snapshot(&handle).await.unwrap_err();
        assert_eq!(
            error.to_string(),
            "driver 'minimal' does not support snapshot (instance 'job-3-run-1')"
        );
        let error = driver
            .exec(
                &handle,
                ExecSpec {
                    command: vec!["true".into()],
                    env: vec![],
                    workdir: None,
                },
            )
            .await
            .err()
            .unwrap();
        assert!(matches!(
            error,
            DriverError::Unsupported {
                operation: "exec",
                ..
            }
        ));
        let error = driver.wait(&handle).await.unwrap_err();
        assert!(matches!(
            error,
            DriverError::Unsupported {
                operation: "wait",
                ..
            }
        ));
        let error = driver.list(&BTreeMap::new()).await.unwrap_err();
        assert!(matches!(
            error,
            DriverError::Unsupported {
                operation: "list",
                ..
            }
        ));
    }
}
