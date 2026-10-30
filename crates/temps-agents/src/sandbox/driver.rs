// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The sandbox backends as runtime drivers (ADR-055 §6, phase 6).
//!
//! [`SandboxBackendDriver`] presents one backend (Docker, Firecracker,
//! microsandbox) as a [`RuntimeDriver`] + [`SandboxExt`], with a capability
//! table that says truthfully what that backend can do. The backend's own
//! code is unchanged: every operation is the backend's own provider call,
//! so the driver adds capability reporting without changing what a sandbox
//! does.
//!
//! Errors keep their type across the boundary: a backend failure travels as
//! [`DriverError::Backend`] with the [`AgentError`] inside, and
//! [`agent_error`] gets it back, so `SandboxNotFound` stays a 404 and a
//! validation failure stays a 400 through the facade.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use futures::{Future, StreamExt};
use temps_workload::{
    AgentRuntimeCompatibility, Artifact, ArtifactKinds, AttachmentKind, Attachments, ByteChannel,
    DriverCapabilities, DriverError, EgressProfile, EgressSupport, ExecLine, ExecLineSink,
    ExecOutput, ExecSpec, ExecStream as DriverExecStream, ExecUser, InstanceHandle, InstanceSpec,
    InstanceState, InstanceStats, InstanceStatus, Isolation, LimitKind, LimitSupport, LogLine,
    LogQuery, LogStream, PreparedArtifact, ProcessSignal, RuntimeDriver, SandboxCapabilities,
    SandboxCreate, SandboxExec, SandboxExt, SandboxInstance, SandboxSnapshot, SnapshotFile,
    VolumeKinds,
};

use super::{
    microsandbox, user::SANDBOX_WORK_DIR, ExecStream, KillSignal, OnStreamEventCallback,
    PtyAttachment, RuntimeCompatibility, SandboxBackend, SandboxCreateConfig, SandboxExecResult,
    SandboxHandle, SandboxProvider, SnapshotArtifact, SnapshotCompanionArtifact,
};
use crate::ai_cli::OnEventCallback;
use crate::error::AgentError;

/// Bounded queue between a running exec and the [`RuntimeDriver::exec`]
/// stream reading it; a slow reader slows the exec instead of buffering.
const EXEC_STREAM_QUEUE: usize = 256;

/// What each backend can do (§6). These tables are the filter the facade
/// applies; a `true` here is a promise the eval harness checks.
pub fn capabilities_for(backend: SandboxBackend) -> (DriverCapabilities, SandboxCapabilities) {
    match backend {
        SandboxBackend::Docker => (
            DriverCapabilities {
                name: "docker".to_string(),
                artifacts: ArtifactKinds {
                    oci: true,
                    ..ArtifactKinds::default()
                },
                isolation: Isolation::Process,
                network_attachment: AttachmentKind::DockerNetwork,
                volumes: VolumeKinds {
                    named: true,
                    ..VolumeKinds::default()
                },
                exec: true,
                snapshot: true,
                resize_disk: false,
                limits: LimitSupport {
                    cpu: true,
                    memory: true,
                    pids: true,
                    disk: false,
                    disk_io: false,
                    network: false,
                },
            },
            SandboxCapabilities {
                file_access: true,
                pty: true,
                agent_runtime: true,
                model_relay: true,
                git_relay: true,
                mcp_relay: true,
                snapshot: true,
                restore: true,
                resize_disk: false,
                application_network: true,
                workspace_volume: true,
                guest_docker: false,
                egress: EgressSupport {
                    open: true,
                    restricted: true,
                    none: true,
                },
                // The per-sandbox egress proxy enforces host and CIDR
                // allow-lists.
                egress_rules: true,
            },
        ),
        SandboxBackend::Firecracker => (
            DriverCapabilities {
                name: "firecracker".to_string(),
                artifacts: ArtifactKinds {
                    oci: true,
                    rootfs: true,
                    ..ArtifactKinds::default()
                },
                isolation: Isolation::MicroVm,
                network_attachment: AttachmentKind::Tap,
                volumes: VolumeKinds {
                    block_image: true,
                    ..VolumeKinds::default()
                },
                exec: true,
                snapshot: true,
                resize_disk: true,
                limits: LimitSupport {
                    cpu: true,
                    memory: true,
                    pids: false,
                    disk: true,
                    disk_io: false,
                    network: false,
                },
            },
            SandboxCapabilities {
                file_access: true,
                pty: true,
                agent_runtime: false,
                model_relay: true,
                git_relay: false,
                mcp_relay: true,
                snapshot: true,
                restore: true,
                resize_disk: true,
                application_network: false,
                workspace_volume: false,
                guest_docker: false,
                // `restricted` boots with no network until the egress proxy
                // exists for TAP devices: not what was asked for, so it is
                // not offered.
                egress: EgressSupport {
                    open: true,
                    restricted: false,
                    none: true,
                },
                // A TAP device routes straight out; there is no proxy to
                // hold an allow-list.
                egress_rules: false,
            },
        ),
        SandboxBackend::Microsandbox => (
            DriverCapabilities {
                name: "microsandbox".to_string(),
                artifacts: ArtifactKinds {
                    oci: true,
                    ..ArtifactKinds::default()
                },
                isolation: Isolation::MicroVm,
                network_attachment: AttachmentKind::SdkPolicy,
                volumes: VolumeKinds::default(),
                exec: true,
                snapshot: false,
                resize_disk: false,
                limits: LimitSupport {
                    cpu: true,
                    memory: true,
                    pids: true,
                    disk: true,
                    disk_io: false,
                    network: false,
                },
            },
            SandboxCapabilities {
                file_access: true,
                pty: true,
                agent_runtime: false,
                model_relay: true,
                git_relay: false,
                mcp_relay: true,
                snapshot: false,
                restore: false,
                resize_disk: false,
                application_network: false,
                workspace_volume: false,
                guest_docker: true,
                egress: EgressSupport {
                    open: true,
                    restricted: true,
                    none: true,
                },
                // The SDK's network policy is all-or-host-only; it has no
                // per-destination allow-list.
                egress_rules: false,
            },
        ),
        // The dev-only fork-exec fallback is not a driver: it has no
        // isolation to describe. Listed so the table is total.
        SandboxBackend::Local => (
            DriverCapabilities {
                name: "local".to_string(),
                artifacts: ArtifactKinds::default(),
                isolation: Isolation::InHost,
                network_attachment: AttachmentKind::None,
                volumes: VolumeKinds::default(),
                exec: true,
                snapshot: false,
                resize_disk: false,
                limits: LimitSupport::default(),
            },
            SandboxCapabilities {
                file_access: true,
                ..SandboxCapabilities::default()
            },
        ),
    }
}

/// The work dir every sandbox on `backend` has. A property of the backend,
/// so a handle rebuilt from an [`InstanceHandle`] is the one the backend
/// returned.
pub fn work_dir_for(backend: SandboxBackend) -> PathBuf {
    match backend {
        SandboxBackend::Docker | SandboxBackend::Local => PathBuf::from(SANDBOX_WORK_DIR),
        SandboxBackend::Firecracker => PathBuf::from(temps_vm_agent::WORK_DIR),
        SandboxBackend::Microsandbox => PathBuf::from(microsandbox::WORK_DIR),
    }
}

/// The backend a driver name names.
pub fn backend_named(driver: &str) -> Option<SandboxBackend> {
    driver.parse().ok()
}

/// The facade's `network_mode` strings as egress profiles. `None` keeps the
/// driver's configured default.
pub fn egress_from_network_mode(
    mode: Option<&str>,
    sandbox: &str,
) -> Result<Option<EgressProfile>, AgentError> {
    match mode {
        None => Ok(None),
        Some("full") | Some("open") => Ok(Some(EgressProfile::Open)),
        Some("restricted") => Ok(Some(EgressProfile::Restricted)),
        Some("none") => Ok(Some(EgressProfile::None)),
        Some(other) => Err(AgentError::Validation {
            message: format!(
                "sandbox '{}': network_mode '{}' is not one of \"full\", \"restricted\" or \"none\"",
                sandbox, other
            ),
        }),
    }
}

fn network_mode_for(egress: EgressProfile) -> &'static str {
    match egress {
        EgressProfile::Open => "full",
        EgressProfile::Restricted => "restricted",
        EgressProfile::None => "none",
    }
}

/// A sandbox request as the drivers take it. `sandbox` names it in errors.
pub fn sandbox_create_from(
    config: SandboxCreateConfig,
    owner: temps_workload::WorkloadRef,
) -> Result<SandboxCreate, AgentError> {
    let label = config
        .container_name_override
        .clone()
        .unwrap_or_else(|| config.run_id.to_string());
    let egress = egress_from_network_mode(config.network_mode.as_deref(), &label)?;
    let egress_policy = config
        .egress_policy
        .normalized()
        .map_err(|e| AgentError::Validation {
            message: format!("sandbox '{}': {}", label, e),
        })?;
    let mut env: Vec<(String, String)> = config.env_vars.into_iter().collect();
    env.sort();
    Ok(SandboxCreate {
        owner,
        run_id: config.run_id,
        label: config.container_name_override,
        image: config.image,
        env,
        limits: temps_workload::SandboxLimits {
            cpu_cores: config.cpu_limit,
            memory_mb: config.memory_limit_mb,
            pids: config.pids_limit,
            disk_mb: config.disk_size_mb,
        },
        egress,
        egress_policy,
        host_work_dir: config.host_work_dir,
        workspace_volume: config.workspace_volume,
        idle_timeout: config.idle_timeout,
        allow_docker: config.allow_docker,
    })
}

fn create_config_for(backend: SandboxBackend, create: SandboxCreate) -> SandboxCreateConfig {
    SandboxCreateConfig {
        allow_docker: create.allow_docker,
        run_id: create.run_id,
        container_name_override: create.label,
        host_work_dir: create.host_work_dir,
        workspace_volume: create.workspace_volume,
        image: create.image,
        cpu_limit: create.limits.cpu_cores,
        memory_limit_mb: create.limits.memory_mb,
        pids_limit: create.limits.pids,
        disk_size_mb: create.limits.disk_mb,
        egress_policy: create.egress_policy,
        network_mode: create.egress.map(|e| network_mode_for(e).to_string()),
        env_vars: create.env.into_iter().collect::<HashMap<_, _>>(),
        idle_timeout: create.idle_timeout,
        backend: Some(backend),
        owner_user_id: None,
        node_id: None,
    }
}

/// The driver's handle for a sandbox handle.
pub fn instance_handle(handle: &SandboxHandle) -> InstanceHandle {
    InstanceHandle {
        driver: handle.backend.to_string(),
        id: handle.sandbox_id.clone(),
        name: handle.sandbox_name.clone(),
    }
}

/// A backend's snapshot artifact in the driver's terms.
pub fn sandbox_snapshot(artifact: &SnapshotArtifact) -> SandboxSnapshot {
    let workspace = artifact.workspace.as_ref().map(|w| SnapshotFile {
        path: w.content_path.clone(),
        digest: w.content_digest.clone(),
        size_bytes: w.size_bytes,
    });
    let companion_bytes = workspace.as_ref().map(|w| w.size_bytes).unwrap_or(0);
    SandboxSnapshot {
        driver: artifact.backend.to_string(),
        primary: SnapshotFile {
            path: artifact.content_path.clone(),
            digest: artifact.primary_digest.clone(),
            size_bytes: artifact.size_bytes.saturating_sub(companion_bytes),
        },
        content_digest: artifact.content_digest.clone(),
        size_bytes: artifact.size_bytes,
        image_ref: artifact.image_ref.clone(),
        image_id: artifact.image_id.clone(),
        workspace,
    }
}

/// The backend artifact a driver snapshot stands for.
pub fn snapshot_artifact(snapshot: &SandboxSnapshot) -> Result<SnapshotArtifact, AgentError> {
    let backend = backend_named(&snapshot.driver).ok_or_else(|| AgentError::Validation {
        message: format!(
            "snapshot {} was taken by unknown driver '{}'",
            snapshot.content_digest, snapshot.driver
        ),
    })?;
    Ok(SnapshotArtifact {
        content_path: snapshot.primary.path.clone(),
        content_digest: snapshot.content_digest.clone(),
        primary_digest: snapshot.primary.digest.clone(),
        size_bytes: snapshot.size_bytes,
        backend,
        image_ref: snapshot.image_ref.clone(),
        image_id: snapshot.image_id.clone(),
        workspace: snapshot
            .workspace
            .as_ref()
            .map(|w| SnapshotCompanionArtifact {
                content_path: w.path.clone(),
                content_digest: w.digest.clone(),
                size_bytes: w.size_bytes,
            }),
    })
}

/// Recover the backend error a driver error carries, or describe a driver
/// refusal in the provider's error vocabulary.
pub fn agent_error(error: DriverError) -> AgentError {
    match error {
        DriverError::Backend { source, .. } => match source.downcast::<AgentError>() {
            Ok(agent) => *agent,
            Err(other) => AgentError::SandboxExecFailed {
                run_id: 0,
                sandbox_id: String::new(),
                reason: other.to_string(),
            },
        },
        DriverError::NotFound { driver, instance } => AgentError::SandboxNotFound {
            run_id: 0,
            sandbox: format!("{} ({} driver)", instance, driver),
        },
        DriverError::Unavailable { driver, reason } => AgentError::SandboxProviderUnavailable {
            provider: driver,
            reason,
        },
        ref unsupported @ DriverError::Unsupported { ref instance, .. } => {
            AgentError::SandboxExecFailed {
                run_id: 0,
                sandbox_id: instance.clone(),
                reason: unsupported.to_string(),
            }
        }
        other @ (DriverError::Artifact { .. }
        | DriverError::Secrets { .. }
        | DriverError::Attach { .. }
        | DriverError::Runtime { .. }) => AgentError::SandboxExecFailed {
            run_id: 0,
            sandbox_id: String::new(),
            reason: other.to_string(),
        },
    }
}

fn line_callback(sink: ExecLineSink) -> OnEventCallback {
    Arc::new(move |line: String| {
        let sink = sink.clone();
        let fut: Pin<Box<dyn Future<Output = ()> + Send>> = Box::pin(async move {
            sink(ExecLine {
                stderr: false,
                line,
            })
            .await
        });
        fut
    })
}

fn stream_callback(sink: ExecLineSink) -> OnStreamEventCallback {
    Arc::new(move |stream: ExecStream, line: String| {
        let sink = sink.clone();
        let fut: Pin<Box<dyn Future<Output = ()> + Send>> = Box::pin(async move {
            sink(ExecLine {
                stderr: stream == ExecStream::Stderr,
                line,
            })
            .await
        });
        fut
    })
}

fn exec_output(result: SandboxExecResult) -> ExecOutput {
    ExecOutput {
        exit_code: result.exit_code,
        stdout: result.stdout,
        stderr: result.stderr,
    }
}

/// One sandbox backend as a runtime driver.
pub struct SandboxBackendDriver {
    backend: SandboxBackend,
    provider: Arc<dyn SandboxProvider>,
    capabilities: DriverCapabilities,
    sandbox: SandboxCapabilities,
}

impl SandboxBackendDriver {
    /// `provider` must be the single-backend provider for `backend` (not
    /// the routing provider): every call goes straight to it.
    pub fn new(backend: SandboxBackend, provider: Arc<dyn SandboxProvider>) -> Self {
        let (capabilities, sandbox) = capabilities_for(backend);
        Self {
            backend,
            provider,
            capabilities,
            sandbox,
        }
    }

    pub fn backend(&self) -> SandboxBackend {
        self.backend
    }

    /// The provider's handle for a driver handle.
    fn handle(&self, instance: &InstanceHandle) -> SandboxHandle {
        SandboxHandle {
            node_id: None,
            sandbox_id: instance.id.clone(),
            sandbox_name: instance.name.clone(),
            work_dir: work_dir_for(self.backend),
            backend: self.backend,
            image: String::new(),
            conditions: Vec::new(),
        }
    }

    fn fail(&self, operation: &'static str, instance: &str, error: AgentError) -> DriverError {
        DriverError::Backend {
            driver: self.capabilities.name.clone(),
            operation,
            instance: instance.to_string(),
            source: Box::new(error),
        }
    }

    fn instance(&self, handle: SandboxHandle) -> SandboxInstance {
        SandboxInstance {
            handle: instance_handle(&handle),
            image: handle.image,
            work_dir: handle.work_dir,
        }
    }

    fn unsupported(&self, operation: &'static str, instance: &str) -> DriverError {
        DriverError::Unsupported {
            driver: self.capabilities.name.clone(),
            operation,
            instance: instance.to_string(),
        }
    }
}

#[async_trait]
impl RuntimeDriver for SandboxBackendDriver {
    fn capabilities(&self) -> &DriverCapabilities {
        &self.capabilities
    }

    /// Sandbox backends resolve (pull, build, convert) their image at
    /// create; preparing records the reference.
    async fn prepare(&self, artifact: &Artifact) -> Result<PreparedArtifact, DriverError> {
        match artifact {
            Artifact::OciImage { reference, .. } => Ok(PreparedArtifact {
                artifact: artifact.clone(),
                local_ref: reference.clone(),
            }),
            other => Err(DriverError::Artifact {
                driver: self.capabilities.name.clone(),
                artifact: format!("{other:?}"),
                reason: "sandbox drivers run OCI images".to_string(),
            }),
        }
    }

    /// Sandboxes are created through [`SandboxExt::create_sandbox`], which
    /// carries what a sandbox needs (work dir seeding, egress, relays); a
    /// generic instance spec has none of it.
    async fn create(
        &self,
        spec: &InstanceSpec,
        _attach: &Attachments,
    ) -> Result<InstanceHandle, DriverError> {
        Err(self.unsupported("create (use create_sandbox)", &spec.name))
    }

    async fn start(&self, handle: &InstanceHandle) -> Result<(), DriverError> {
        self.provider
            .start(&self.handle(handle))
            .await
            .map_err(|e| self.fail("start", &handle.name, e))
    }

    async fn stop(&self, handle: &InstanceHandle) -> Result<(), DriverError> {
        self.provider
            .stop(&self.handle(handle))
            .await
            .map_err(|e| self.fail("stop", &handle.name, e))
    }

    async fn remove(&self, handle: &InstanceHandle, purge: bool) -> Result<(), DriverError> {
        self.provider
            .destroy(&self.handle(handle), purge)
            .await
            .map_err(|e| self.fail("remove", &handle.name, e))
    }

    async fn inspect(&self, handle: &InstanceHandle) -> Result<InstanceStatus, DriverError> {
        let alive = self.is_alive(handle).await?;
        Ok(InstanceStatus {
            state: if alive {
                InstanceState::Running
            } else {
                InstanceState::Exited
            },
            exit_code: None,
            started_at: None,
            finished_at: None,
            address: None,
            published_ports: Vec::new(),
        })
    }

    async fn logs(
        &self,
        handle: &InstanceHandle,
        _query: LogQuery,
    ) -> Result<LogStream, DriverError> {
        Err(self.unsupported("logs", &handle.name))
    }

    async fn stats(&self, handle: &InstanceHandle) -> Result<InstanceStats, DriverError> {
        Err(self.unsupported("stats", &handle.name))
    }

    async fn exec(
        &self,
        handle: &InstanceHandle,
        cmd: ExecSpec,
    ) -> Result<DriverExecStream, DriverError> {
        let (tx, rx) =
            tokio::sync::mpsc::channel::<Result<LogLine, DriverError>>(EXEC_STREAM_QUEUE);
        let sink_tx = tx.clone();
        let sink: ExecLineSink = Arc::new(move |line: ExecLine| {
            let tx = sink_tx.clone();
            Box::pin(async move {
                let mut bytes = line.line.into_bytes();
                bytes.push(b'\n');
                // The reader went away: the exec keeps running to its end.
                let _ = tx
                    .send(Ok(LogLine {
                        stderr: line.stderr,
                        line: Bytes::from(bytes),
                        timestamp: None,
                    }))
                    .await;
            })
        });
        let provider = self.provider.clone();
        let sandbox = self.handle(handle);
        let driver = self.capabilities.name.clone();
        let instance = handle.name.clone();
        let mut command = cmd.command;
        if let Some(workdir) = cmd.workdir {
            command = vec![
                "sh".to_string(),
                "-c".to_string(),
                "cd \"$0\" && exec \"$@\"".to_string(),
                workdir,
            ]
            .into_iter()
            .chain(command)
            .collect();
        }
        let env: HashMap<String, String> = cmd.env.into_iter().collect();
        tokio::spawn(async move {
            let result = provider
                .exec_streamed(&sandbox, command, env, Some(stream_callback(sink)))
                .await;
            if let Err(error) = result {
                let _ = tx
                    .send(Err(DriverError::Backend {
                        driver,
                        operation: "exec",
                        instance,
                        source: Box::new(error),
                    }))
                    .await;
            }
        });
        Ok(futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|item| (item, rx))
        })
        .boxed())
    }
}

fn pty_channel(attachment: PtyAttachment, driver: String, instance: String) -> ByteChannel {
    let output = attachment
        .output
        .map(move |item| {
            item.map_err(|error| DriverError::Backend {
                driver: driver.clone(),
                operation: "attach_pty",
                instance: instance.clone(),
                source: Box::new(error),
            })
        })
        .boxed();
    ByteChannel {
        output,
        input: attachment.input,
    }
}

/// A driver byte channel as the provider's attachment.
pub fn pty_attachment(channel: ByteChannel) -> PtyAttachment {
    PtyAttachment {
        output: Box::pin(channel.output.map(|item| item.map_err(agent_error))),
        input: channel.input,
    }
}

fn compatibility(runtime: RuntimeCompatibility) -> AgentRuntimeCompatibility {
    match runtime {
        RuntimeCompatibility::Compatible => AgentRuntimeCompatibility::Compatible,
        RuntimeCompatibility::Unavailable { reason } => {
            AgentRuntimeCompatibility::Unavailable { reason }
        }
        RuntimeCompatibility::Incompatible { reason } => {
            AgentRuntimeCompatibility::Incompatible { reason }
        }
    }
}

/// The driver's agent-runtime answer as the provider's.
pub fn runtime_compatibility(runtime: AgentRuntimeCompatibility) -> RuntimeCompatibility {
    match runtime {
        AgentRuntimeCompatibility::Compatible => RuntimeCompatibility::Compatible,
        AgentRuntimeCompatibility::Unavailable { reason } => {
            RuntimeCompatibility::Unavailable { reason }
        }
        AgentRuntimeCompatibility::Incompatible { reason } => {
            RuntimeCompatibility::Incompatible { reason }
        }
    }
}

#[async_trait]
impl SandboxExt for SandboxBackendDriver {
    fn sandbox_capabilities(&self) -> &SandboxCapabilities {
        &self.sandbox
    }

    fn limit_gap(&self, limit: LimitKind) -> String {
        match (self.backend, limit) {
            (SandboxBackend::Docker, LimitKind::Disk) => "docker: disk_size_mb is not applied; \
                 the container writes to the daemon's shared storage, and a per-container \
                 size needs an overlay2-on-XFS project quota this driver does not configure"
                .to_string(),
            (SandboxBackend::Firecracker, LimitKind::Pids) => "firecracker: a pids limit is \
                 not applied inside the guest; the microVM's memory bounds its processes"
                .to_string(),
            (backend, limit) => format!(
                "{}: {} limits are not supported by this driver",
                backend,
                limit.as_str()
            ),
        }
    }

    async fn create_sandbox(&self, create: SandboxCreate) -> Result<SandboxInstance, DriverError> {
        let name = create
            .label
            .clone()
            .unwrap_or_else(|| create.run_id.to_string());
        let handle = self
            .provider
            .create(create_config_for(self.backend, create))
            .await
            .map_err(|e| self.fail("create", &name, e))?;
        Ok(self.instance(handle))
    }

    async fn recover_sandbox(&self, name: &str) -> Result<Option<SandboxInstance>, DriverError> {
        let handle = self
            .provider
            .recover_by_name(name)
            .await
            .map_err(|e| self.fail("recover", name, e))?;
        Ok(handle.map(|h| self.instance(h)))
    }

    async fn recover_run(&self, run_id: i32) -> Result<Option<SandboxInstance>, DriverError> {
        let handle = self
            .provider
            .recover(run_id)
            .await
            .map_err(|e| self.fail("recover", &run_id.to_string(), e))?;
        Ok(handle.map(|h| self.instance(h)))
    }

    async fn release_orphaned(&self, name: &str) -> Result<(), DriverError> {
        self.provider
            .release_orphaned_resources(name)
            .await
            .map_err(|e| self.fail("release orphaned resources of", name, e))
    }

    async fn is_alive(&self, handle: &InstanceHandle) -> Result<bool, DriverError> {
        self.provider
            .is_alive(&self.handle(handle))
            .await
            .map_err(|e| self.fail("probe", &handle.name, e))
    }

    async fn exec_in(
        &self,
        handle: &InstanceHandle,
        exec: SandboxExec,
        sink: Option<ExecLineSink>,
    ) -> Result<ExecOutput, DriverError> {
        let sandbox = self.handle(handle);
        let env: HashMap<String, String> = exec.env.into_iter().collect();
        let result = match exec.user {
            ExecUser::Root => {
                self.provider
                    .exec_as_root(&sandbox, exec.command, env, sink.map(line_callback))
                    .await
            }
            ExecUser::Named(user) => {
                self.provider
                    .exec_as_user(&sandbox, &user, exec.command, env, sink.map(line_callback))
                    .await
            }
            ExecUser::Default if exec.split_streams => {
                self.provider
                    .exec_streamed(&sandbox, exec.command, env, sink.map(stream_callback))
                    .await
            }
            ExecUser::Default => {
                self.provider
                    .exec(&sandbox, exec.command, env, sink.map(line_callback))
                    .await
            }
        };
        result
            .map(exec_output)
            .map_err(|e| self.fail("exec in", &handle.name, e))
    }

    async fn kill_processes(
        &self,
        handle: &InstanceHandle,
        pattern: &str,
        signal: ProcessSignal,
    ) -> Result<(), DriverError> {
        let signal = match signal {
            ProcessSignal::Term => KillSignal::Term,
            ProcessSignal::Kill => KillSignal::Kill,
        };
        self.provider
            .kill_processes(&self.handle(handle), pattern, signal)
            .await
            .map_err(|e| self.fail("kill processes in", &handle.name, e))
    }

    async fn read_file(
        &self,
        handle: &InstanceHandle,
        path: &str,
        max_bytes: Option<u64>,
    ) -> Result<Vec<u8>, DriverError> {
        let sandbox = self.handle(handle);
        let result = match max_bytes {
            Some(max) => self.provider.read_file_bounded(&sandbox, path, max).await,
            None => self.provider.read_file(&sandbox, path).await,
        };
        result.map_err(|e| self.fail("read a file from", &handle.name, e))
    }

    async fn write_file(
        &self,
        handle: &InstanceHandle,
        path: &str,
        contents: &[u8],
        mode: u32,
    ) -> Result<(), DriverError> {
        self.provider
            .write_file(&self.handle(handle), path, contents, mode)
            .await
            .map_err(|e| self.fail("write a file to", &handle.name, e))
    }

    async fn write_directory(
        &self,
        handle: &InstanceHandle,
        local_dir: &Path,
        target_path: &str,
    ) -> Result<(), DriverError> {
        self.provider
            .write_directory(&self.handle(handle), local_dir, target_path)
            .await
            .map_err(|e| self.fail("write a directory to", &handle.name, e))
    }

    async fn sync_workspace_to_host(
        &self,
        handle: &InstanceHandle,
        host_dir: &Path,
    ) -> Result<(), DriverError> {
        self.provider
            .sync_workspace_to_host(&self.handle(handle), host_dir)
            .await
            .map_err(|e| self.fail("sync the workspace of", &handle.name, e))
    }

    async fn attach_pty(&self, handle: &InstanceHandle) -> Result<ByteChannel, DriverError> {
        let attachment = self
            .provider
            .attach_pty(&self.handle(handle))
            .await
            .map_err(|e| self.fail("attach_pty", &handle.name, e))?;
        Ok(pty_channel(
            attachment,
            self.capabilities.name.clone(),
            handle.name.clone(),
        ))
    }

    async fn connect_agent_runtime(
        &self,
        handle: &InstanceHandle,
    ) -> Result<ByteChannel, DriverError> {
        let attachment = self
            .provider
            .connect_agent_runtime(&self.handle(handle))
            .await
            .map_err(|e| self.fail("connect_agent_runtime", &handle.name, e))?;
        Ok(pty_channel(
            attachment,
            self.capabilities.name.clone(),
            handle.name.clone(),
        ))
    }

    async fn connect_port(
        &self,
        handle: &InstanceHandle,
        port: u16,
    ) -> Result<ByteChannel, DriverError> {
        let attachment = self
            .provider
            .connect_port(&self.handle(handle), port)
            .await
            .map_err(|e| self.fail("connect_port", &handle.name, e))?;
        Ok(pty_channel(
            attachment,
            self.capabilities.name.clone(),
            handle.name.clone(),
        ))
    }

    async fn check_agent_runtime(
        &self,
        handle: &InstanceHandle,
    ) -> Result<AgentRuntimeCompatibility, DriverError> {
        self.provider
            .check_agent_runtime(&self.handle(handle))
            .await
            .map(compatibility)
            .map_err(|e| self.fail("check_agent_runtime", &handle.name, e))
    }

    async fn recover_agent_harness(
        &self,
        handle: &InstanceHandle,
        epoch: u64,
    ) -> Result<(), DriverError> {
        self.provider
            .recover_agent_harness(&self.handle(handle), epoch)
            .await
            .map_err(|e| self.fail("recover_agent_harness", &handle.name, e))
    }

    async fn image_identity(&self, handle: &InstanceHandle) -> Result<String, DriverError> {
        self.provider
            .image_identity(&self.handle(handle))
            .await
            .map_err(|e| self.fail("image_identity", &handle.name, e))
    }

    async fn model_relay_base_url(
        &self,
        handle: &InstanceHandle,
        control_plane_url: &str,
    ) -> Result<String, DriverError> {
        self.provider
            .model_relay_base_url(&self.handle(handle), control_plane_url)
            .await
            .map_err(|e| self.fail("model_relay_base_url", &handle.name, e))
    }

    async fn git_relay_base_url(
        &self,
        handle: &InstanceHandle,
        control_plane_url: &str,
    ) -> Result<String, DriverError> {
        self.provider
            .git_relay_base_url(&self.handle(handle), control_plane_url)
            .await
            .map_err(|e| self.fail("git_relay_base_url", &handle.name, e))
    }

    async fn harness_mcp_url(
        &self,
        handle: &InstanceHandle,
        control_plane_url: &str,
        registered_url: &str,
    ) -> Result<String, DriverError> {
        self.provider
            .harness_mcp_url(&self.handle(handle), control_plane_url, registered_url)
            .await
            .map_err(|e| self.fail("harness_mcp_url", &handle.name, e))
    }

    async fn configure_application_network(
        &self,
        handle: &InstanceHandle,
        network_name: &str,
        service_containers: &[String],
    ) -> Result<(), DriverError> {
        self.provider
            .configure_application_network(&self.handle(handle), network_name, service_containers)
            .await
            .map_err(|e| self.fail("configure_application_network", &handle.name, e))
    }

    async fn take_snapshot(
        &self,
        handle: &InstanceHandle,
        label: Option<String>,
        max_size_bytes: u64,
    ) -> Result<SandboxSnapshot, DriverError> {
        let artifact = self
            .provider
            .take_snapshot(&self.handle(handle), label, max_size_bytes)
            .await
            .map_err(|e| self.fail("take_snapshot", &handle.name, e))?;
        Ok(sandbox_snapshot(&artifact))
    }

    async fn restore_snapshot(
        &self,
        snapshot: &SandboxSnapshot,
        create: SandboxCreate,
    ) -> Result<SandboxInstance, DriverError> {
        let name = create
            .label
            .clone()
            .unwrap_or_else(|| create.run_id.to_string());
        let artifact = snapshot_artifact(snapshot).map_err(|e| self.fail("restore", &name, e))?;
        let handle = self
            .provider
            .create_from_snapshot(&artifact, create_config_for(self.backend, create))
            .await
            .map_err(|e| self.fail("restore", &name, e))?;
        Ok(self.instance(handle))
    }

    async fn delete_snapshot_image(&self, image_ref: &str) -> Result<(), DriverError> {
        self.provider
            .delete_image(image_ref)
            .await
            .map_err(|e| self.fail("delete snapshot image", image_ref, e))
    }

    async fn resize_disk(
        &self,
        handle: &InstanceHandle,
        new_size_mb: u64,
    ) -> Result<(), DriverError> {
        self.provider
            .resize_disk(&self.handle(handle), new_size_mb)
            .await
            .map_err(|e| self.fail("resize_disk", &handle.name, e))
    }

    async fn set_egress_policy(
        &self,
        handle: &InstanceHandle,
        policy: &temps_workload::SandboxEgressPolicy,
    ) -> Result<(), DriverError> {
        self.provider
            .set_egress_policy(&self.handle(handle), policy)
            .await
            .map_err(|e| self.fail("set_egress_policy", &handle.name, e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_workload::{supports, SandboxFeature};

    #[test]
    fn capability_tables_match_what_each_backend_overrides() {
        let (docker, docker_sandbox) = capabilities_for(SandboxBackend::Docker);
        assert!(!docker.limits.disk, "Docker ignores disk_size_mb");
        assert!(!docker_sandbox.resize_disk);
        assert!(supports(&docker, &docker_sandbox, SandboxFeature::Snapshot));
        assert!(supports(
            &docker,
            &docker_sandbox,
            SandboxFeature::SafetyCaps
        ));

        let (fc, fc_sandbox) = capabilities_for(SandboxBackend::Firecracker);
        assert!(supports(&fc, &fc_sandbox, SandboxFeature::ResizeDisk));
        assert!(!supports(
            &fc,
            &fc_sandbox,
            SandboxFeature::Egress(EgressProfile::Restricted)
        ));
        assert!(!supports(&fc, &fc_sandbox, SandboxFeature::GitRelay));

        let (msb, msb_sandbox) = capabilities_for(SandboxBackend::Microsandbox);
        assert!(supports(&msb, &msb_sandbox, SandboxFeature::GuestDocker));
        assert!(!supports(&msb, &msb_sandbox, SandboxFeature::Snapshot));
        assert!(!supports(&msb, &msb_sandbox, SandboxFeature::Restore));
    }

    #[test]
    fn network_modes_map_to_egress_profiles_and_unknown_ones_are_refused() {
        assert_eq!(egress_from_network_mode(None, "s").unwrap(), None);
        assert_eq!(
            egress_from_network_mode(Some("full"), "s").unwrap(),
            Some(EgressProfile::Open)
        );
        assert_eq!(
            egress_from_network_mode(Some("none"), "s").unwrap(),
            Some(EgressProfile::None)
        );
        let error = egress_from_network_mode(Some("bridge"), "abc").unwrap_err();
        assert!(error.to_string().contains("sandbox 'abc'"), "{error}");
        assert!(error.to_string().contains("'bridge'"), "{error}");
    }

    #[test]
    fn a_create_request_round_trips_through_the_driver_form() {
        let config = SandboxCreateConfig {
            allow_docker: false,
            run_id: 7,
            container_name_override: Some("abc".into()),
            host_work_dir: PathBuf::from("/tmp/w"),
            workspace_volume: Some("vol".into()),
            image: Some("alpine:3.20".into()),
            cpu_limit: Some(1.5),
            memory_limit_mb: Some(256),
            pids_limit: Some(64),
            disk_size_mb: Some(2048),
            egress_policy: temps_workload::SandboxEgressPolicy::restricted(vec![
                temps_workload::EgressRule::host("API.example.com", &[]),
            ]),
            network_mode: Some("restricted".into()),
            env_vars: HashMap::from([("A".into(), "1".into())]),
            idle_timeout: std::time::Duration::from_secs(9),
            backend: None,
            owner_user_id: Some(3),
            node_id: None,
        };
        let owner = temps_workload::WorkloadRef::new(temps_workload::WorkloadKind::Sandbox, 7);
        let create = sandbox_create_from(config, owner).unwrap();
        let back = create_config_for(SandboxBackend::Docker, create);
        assert_eq!(back.container_name_override.as_deref(), Some("abc"));
        assert_eq!(back.workspace_volume.as_deref(), Some("vol"));
        assert_eq!(back.cpu_limit, Some(1.5));
        assert_eq!(back.memory_limit_mb, Some(256));
        assert_eq!(back.pids_limit, Some(64));
        assert_eq!(back.disk_size_mb, Some(2048));
        assert_eq!(back.network_mode.as_deref(), Some("restricted"));
        assert_eq!(back.env_vars.get("A").map(String::as_str), Some("1"));
        assert_eq!(back.backend, Some(SandboxBackend::Docker));
        // The policy travels normalized.
        assert_eq!(
            back.egress_policy.rules,
            vec![temps_workload::EgressRule::host(
                "api.example.com",
                &[80, 443]
            )]
        );
        assert!(back.egress_policy.is_restricted());
    }

    #[test]
    fn an_invalid_egress_policy_is_refused_naming_the_sandbox() {
        let mut config = test_config();
        config.egress_policy = temps_workload::SandboxEgressPolicy::restricted(vec![
            temps_workload::EgressRule::cidr("10.0.0.0/8", &[]),
        ]);
        let owner = temps_workload::WorkloadRef::new(temps_workload::WorkloadKind::Sandbox, 7);
        let error = sandbox_create_from(config, owner).unwrap_err();
        assert!(matches!(error, AgentError::Validation { .. }), "{error:?}");
        let message = error.to_string();
        assert!(message.contains("sandbox 'abc'"), "{message}");
        assert!(message.contains("10.0.0.0/8"), "{message}");
    }

    #[test]
    fn only_docker_enforces_egress_allow_lists() {
        for (backend, expected) in [
            (SandboxBackend::Docker, true),
            (SandboxBackend::Firecracker, false),
            (SandboxBackend::Microsandbox, false),
            (SandboxBackend::Local, false),
        ] {
            let (driver, sandbox) = capabilities_for(backend);
            assert_eq!(
                supports(&driver, &sandbox, SandboxFeature::EgressRules),
                expected,
                "{backend}"
            );
        }
    }

    fn test_config() -> SandboxCreateConfig {
        SandboxCreateConfig {
            allow_docker: false,
            run_id: 7,
            container_name_override: Some("abc".into()),
            host_work_dir: PathBuf::from("/tmp/w"),
            workspace_volume: None,
            image: None,
            cpu_limit: None,
            memory_limit_mb: None,
            pids_limit: None,
            disk_size_mb: None,
            egress_policy: Default::default(),
            network_mode: None,
            env_vars: HashMap::new(),
            idle_timeout: std::time::Duration::from_secs(9),
            backend: None,
            owner_user_id: None,
            node_id: None,
        }
    }

    #[test]
    fn snapshots_round_trip_and_keep_their_companion() {
        let artifact = SnapshotArtifact {
            content_path: PathBuf::from("/data/snapshots/abc.tar"),
            content_digest: "abc".into(),
            primary_digest: "p".into(),
            size_bytes: 30,
            backend: SandboxBackend::Docker,
            image_ref: Some("temps-snapshot/abc:latest".into()),
            image_id: Some("sha256:1".into()),
            workspace: Some(SnapshotCompanionArtifact {
                content_path: PathBuf::from("/data/snapshots/abc.workspace.tar"),
                content_digest: "w".into(),
                size_bytes: 10,
            }),
        };
        let snapshot = sandbox_snapshot(&artifact);
        assert_eq!(snapshot.primary.size_bytes, 20);
        let back = snapshot_artifact(&snapshot).unwrap();
        assert_eq!(back.content_path, artifact.content_path);
        assert_eq!(back.primary_digest, "p");
        assert_eq!(back.workspace, artifact.workspace);
        assert_eq!(back.size_bytes, 30);
        assert_eq!(back.image_id, artifact.image_id);
    }

    #[test]
    fn backend_errors_come_back_as_themselves() {
        let error = DriverError::Backend {
            driver: "docker".into(),
            operation: "exec",
            instance: "temps-sandbox-1".into(),
            source: Box::new(AgentError::SandboxNotFound {
                run_id: 4,
                sandbox: "temps-sandbox-1".into(),
            }),
        };
        assert!(matches!(
            agent_error(error),
            AgentError::SandboxNotFound { run_id: 4, .. }
        ));
        let refused = agent_error(DriverError::Unsupported {
            driver: "docker".into(),
            operation: "resize_disk",
            instance: "temps-sandbox-1".into(),
        });
        assert!(
            refused.to_string().contains("does not support resize_disk"),
            "{refused}"
        );
    }
}
