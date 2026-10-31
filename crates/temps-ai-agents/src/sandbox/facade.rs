// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `SandboxProvider` as the product facade over the runtime drivers
//! (ADR-055 §6, §14, phase 6).
//!
//! Every sandbox call on this host goes through
//! [`DriverSandboxProvider`]: it places a new sandbox by filtering the
//! drivers on what the request needs ([`SandboxRequirements`]), then ranks
//! the host's default driver first; every later call goes to the driver
//! that owns the handle, as a [`SandboxExt`] operation.
//!
//! - **Capabilities are filters.** A request for a driver by name that lacks
//!   a requirement is refused with what it lacks; without a named driver,
//!   the default is passed over for one that has it, and the handle says so
//!   in a `Scheduled` condition. Nothing eligible: a typed refusal naming
//!   every driver and what it lacks, never a silent downgrade.
//! - **Limits are reported, not dropped.** A requested limit the chosen
//!   driver cannot apply (Docker's `disk_size_mb`) is a
//!   `LimitEnforced = False` condition on the handle, with the driver's
//!   reason.
//! - Product-level operations that are not about one sandbox (the sandbox
//!   image, the Firecracker rootfs cache) stay with the product's provider.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use temps_workload::{
    choose_driver, limit_condition, Condition, ConditionKind, ConditionStatus, DriverCandidate,
    DriverRejection, ExecUser, NoEligibleDriver, ProcessSignal, SandboxExec, SandboxExt,
    SandboxFeature, SandboxInstance, SandboxRequirements, WorkloadKind, WorkloadRef,
};

use super::driver::{
    agent_error, backend_named, egress_from_network_mode, instance_handle, pty_attachment,
    runtime_compatibility, sandbox_create_from, sandbox_snapshot, snapshot_artifact,
};
use super::{
    KillSignal, OnStreamEventCallback, PtyAttachment, RootfsGcReport, RootfsReport,
    RuntimeCompatibility, SandboxBackend, SandboxCreateConfig, SandboxExecResult, SandboxHandle,
    SandboxProvider, SnapshotArtifact,
};
use crate::ai_cli::OnEventCallback;
use crate::error::AgentError;

/// Where the facade would put a sandbox, and what it would say about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatePlan {
    pub backend: SandboxBackend,
    /// Drivers tried before it and what each lacked.
    pub passed_over: Vec<DriverRejection>,
    /// The conditions the handle will carry (only the ones worth telling:
    /// an unapplied limit, a passed-over default).
    pub conditions: Vec<Condition>,
    index: usize,
}

/// The hard requirements and requested limits of a create request.
pub fn requirements_for(config: &SandboxCreateConfig) -> Result<SandboxRequirements, AgentError> {
    let label = config
        .container_name_override
        .clone()
        .unwrap_or_else(|| config.run_id.to_string());
    let mut requirements = SandboxRequirements::baseline();
    if let Some(egress) = egress_from_network_mode(config.network_mode.as_deref(), &label)? {
        requirements = requirements.require(SandboxFeature::Egress(egress));
    }
    if config.egress_policy.is_restricted() {
        requirements = requirements.require(SandboxFeature::EgressRules);
    }
    if config.workspace_volume.is_some() {
        requirements = requirements.require(SandboxFeature::WorkspaceVolume);
    }
    if config.allow_docker {
        requirements = requirements.require(SandboxFeature::GuestDocker);
    }
    requirements.limits = temps_workload::RequestedLimits {
        cpu: config.cpu_limit.is_some(),
        memory: config.memory_limit_mb.is_some(),
        pids: config.pids_limit.is_some(),
        disk: config.disk_size_mb.is_some(),
    };
    Ok(requirements)
}

/// A refusal from the filter, in the provider's error vocabulary: a request
/// no driver here can serve is the caller's to change (400), and says
/// exactly why.
fn refusal(error: NoEligibleDriver, run_id: i32) -> AgentError {
    match error {
        NoEligibleDriver::NotAvailable { .. } => AgentError::SandboxCreationFailed {
            run_id,
            provider: "driver facade".to_string(),
            reason: error.to_string(),
        },
        NoEligibleDriver::Ineligible { .. } => AgentError::Validation {
            message: error.to_string(),
        },
    }
}

/// The facade over this host's sandbox drivers.
pub struct DriverSandboxProvider {
    drivers: Vec<Arc<dyn SandboxExt>>,
    default: SandboxBackend,
    /// Product-level operations (the sandbox image, the rootfs cache, the
    /// provider's name and availability) are not driver operations.
    product: Arc<dyn SandboxProvider>,
}

impl DriverSandboxProvider {
    pub fn new(
        drivers: Vec<Arc<dyn SandboxExt>>,
        default: SandboxBackend,
        product: Arc<dyn SandboxProvider>,
    ) -> Self {
        Self {
            drivers,
            default,
            product,
        }
    }

    /// The driver names this facade places on, in candidate order.
    pub fn driver_names(&self) -> Vec<String> {
        self.drivers
            .iter()
            .map(|d| d.capabilities().name.clone())
            .collect()
    }

    fn driver_for(&self, handle: &SandboxHandle) -> Result<&Arc<dyn SandboxExt>, AgentError> {
        let name = handle.backend.to_string();
        self.drivers
            .iter()
            .find(|d| d.capabilities().name == name)
            .ok_or_else(|| AgentError::SandboxProviderUnavailable {
                provider: "driver facade".to_string(),
                reason: format!(
                    "sandbox '{}' runs on the '{}' driver, which is not available on this host \
                     (available: {})",
                    handle.sandbox_name,
                    name,
                    self.driver_names().join(", ")
                ),
            })
    }

    /// Filter, rank and describe a placement without creating anything.
    pub fn plan_create(&self, config: &SandboxCreateConfig) -> Result<CreatePlan, AgentError> {
        let sandbox = config
            .container_name_override
            .clone()
            .unwrap_or_else(|| config.run_id.to_string());
        let requirements = requirements_for(config)?;
        // A restricted egress policy on a backend that cannot enforce one is
        // its own typed refusal, so the caller learns which setting to
        // change rather than reading a list of what every driver lacks.
        if config.egress_policy.is_restricted() {
            let enforceable = |d: &Arc<dyn SandboxExt>| d.sandbox_capabilities().egress_rules;
            let refused_backend = match config.backend {
                Some(requested) => self
                    .drivers
                    .iter()
                    .find(|d| d.capabilities().name == requested.to_string())
                    .filter(|d| !enforceable(d))
                    .map(|_| requested.to_string()),
                None if !self.drivers.iter().any(enforceable) => Some(self.default.to_string()),
                None => None,
            };
            if let Some(backend) = refused_backend {
                return Err(AgentError::SandboxEgressPolicyUnsupported { sandbox, backend });
            }
        }
        let candidates: Vec<DriverCandidate<'_>> = self
            .drivers
            .iter()
            .map(|d| DriverCandidate {
                driver: d.capabilities(),
                sandbox: d.sandbox_capabilities(),
            })
            .collect();
        let requested = config.backend.map(|b| b.to_string());
        let preferred = self.default.to_string();
        let choice = choose_driver(
            &sandbox,
            &candidates,
            requested.as_deref(),
            Some(&preferred),
            &requirements,
        )
        .map_err(|e| refusal(e, config.run_id))?;
        let driver = &self.drivers[choice.index];
        let name = driver.capabilities().name.clone();
        let backend = backend_named(&name).ok_or_else(|| AgentError::SandboxCreationFailed {
            run_id: config.run_id,
            provider: name.clone(),
            reason: format!("driver '{}' is not a sandbox backend", name),
        })?;
        let now = Utc::now();
        let mut conditions = Vec::new();
        if !choice.passed_over.is_empty() {
            let reasons: Vec<String> = choice.passed_over.iter().map(|r| r.to_string()).collect();
            conditions.push(Condition {
                kind: ConditionKind::Scheduled,
                status: ConditionStatus::True,
                reason: "PreferredDriverIneligible".to_string(),
                message: format!("placed on {}: {}", name, reasons.join("; ")),
                last_transition_at: now,
            });
        }
        if let Some(limits) = limit_condition(
            driver.capabilities(),
            requirements.limits,
            |limit| driver.limit_gap(limit),
            now,
        ) {
            if limits.status == ConditionStatus::False {
                conditions.push(limits);
            }
        }
        Ok(CreatePlan {
            backend,
            passed_over: choice.passed_over,
            conditions,
            index: choice.index,
        })
    }

    fn handle(&self, backend: SandboxBackend, instance: SandboxInstance) -> SandboxHandle {
        SandboxHandle {
            node_id: None,
            sandbox_id: instance.handle.id,
            sandbox_name: instance.handle.name,
            work_dir: instance.work_dir,
            backend,
            image: instance.image,
            conditions: Vec::new(),
        }
    }

    fn owner(run_id: i32) -> WorkloadRef {
        WorkloadRef::new(WorkloadKind::Sandbox, i64::from(run_id))
    }

    async fn exec_as(
        &self,
        handle: &SandboxHandle,
        user: ExecUser,
        cmd: Vec<String>,
        env: HashMap<String, String>,
        sink: Option<temps_workload::ExecLineSink>,
        split_streams: bool,
    ) -> Result<SandboxExecResult, AgentError> {
        let driver = self.driver_for(handle)?;
        let mut env: Vec<(String, String)> = env.into_iter().collect();
        env.sort();
        let output = driver
            .exec_in(
                &instance_handle(handle),
                SandboxExec {
                    command: cmd,
                    env,
                    user,
                    split_streams,
                },
                sink,
            )
            .await
            .map_err(agent_error)?;
        Ok(SandboxExecResult {
            exit_code: output.exit_code,
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }
}

fn line_sink(cb: OnEventCallback) -> temps_workload::ExecLineSink {
    Arc::new(move |line: temps_workload::ExecLine| cb(line.line))
}

fn stream_sink(cb: OnStreamEventCallback) -> temps_workload::ExecLineSink {
    Arc::new(move |line: temps_workload::ExecLine| {
        let stream = if line.stderr {
            super::ExecStream::Stderr
        } else {
            super::ExecStream::Stdout
        };
        cb(stream, line.line)
    })
}

#[async_trait]
impl SandboxProvider for DriverSandboxProvider {
    async fn create(&self, config: SandboxCreateConfig) -> Result<SandboxHandle, AgentError> {
        let plan = self.plan_create(&config)?;
        if !plan.passed_over.is_empty() {
            tracing::info!(
                run_id = config.run_id,
                driver = %plan.backend,
                passed_over = ?plan.passed_over,
                "sandbox placed past the default driver, which lacks what it needs"
            );
        }
        let driver = &self.drivers[plan.index];
        let owner = Self::owner(config.run_id);
        let create = sandbox_create_from(config, owner)?;
        let instance = driver.create_sandbox(create).await.map_err(agent_error)?;
        let mut handle = self.handle(plan.backend, instance);
        handle.conditions = plan.conditions;
        Ok(handle)
    }

    async fn image_identity(&self, handle: &SandboxHandle) -> Result<String, AgentError> {
        self.driver_for(handle)?
            .image_identity(&instance_handle(handle))
            .await
            .map_err(agent_error)
    }

    async fn check_agent_runtime(
        &self,
        handle: &SandboxHandle,
    ) -> Result<RuntimeCompatibility, AgentError> {
        self.driver_for(handle)?
            .check_agent_runtime(&instance_handle(handle))
            .await
            .map(runtime_compatibility)
            .map_err(agent_error)
    }

    async fn recover_agent_harness(
        &self,
        handle: &SandboxHandle,
        epoch: u64,
    ) -> Result<(), AgentError> {
        self.driver_for(handle)?
            .recover_agent_harness(&instance_handle(handle), epoch)
            .await
            .map_err(agent_error)
    }

    async fn model_relay_base_url(
        &self,
        handle: &SandboxHandle,
        control_plane_url: &str,
    ) -> Result<String, AgentError> {
        self.driver_for(handle)?
            .model_relay_base_url(&instance_handle(handle), control_plane_url)
            .await
            .map_err(agent_error)
    }

    async fn git_relay_base_url(
        &self,
        handle: &SandboxHandle,
        control_plane_url: &str,
    ) -> Result<String, AgentError> {
        self.driver_for(handle)?
            .git_relay_base_url(&instance_handle(handle), control_plane_url)
            .await
            .map_err(agent_error)
    }

    async fn harness_mcp_url(
        &self,
        handle: &SandboxHandle,
        control_plane_url: &str,
        registered_url: &str,
    ) -> Result<String, AgentError> {
        self.driver_for(handle)?
            .harness_mcp_url(&instance_handle(handle), control_plane_url, registered_url)
            .await
            .map_err(agent_error)
    }

    async fn configure_application_network(
        &self,
        handle: &SandboxHandle,
        network_name: &str,
        service_containers: &[String],
    ) -> Result<(), AgentError> {
        self.driver_for(handle)?
            .configure_application_network(
                &instance_handle(handle),
                network_name,
                service_containers,
            )
            .await
            .map_err(agent_error)
    }

    async fn connect_agent_runtime(
        &self,
        handle: &SandboxHandle,
    ) -> Result<PtyAttachment, AgentError> {
        self.driver_for(handle)?
            .connect_agent_runtime(&instance_handle(handle))
            .await
            .map(pty_attachment)
            .map_err(agent_error)
    }

    async fn connect_port(
        &self,
        handle: &SandboxHandle,
        port: u16,
    ) -> Result<PtyAttachment, AgentError> {
        self.driver_for(handle)?
            .connect_port(&instance_handle(handle), port)
            .await
            .map(pty_attachment)
            .map_err(agent_error)
    }

    async fn set_egress_policy(
        &self,
        handle: &SandboxHandle,
        policy: &super::SandboxEgressPolicy,
    ) -> Result<(), AgentError> {
        let driver = self.driver_for(handle)?;
        if policy.is_restricted() && !driver.sandbox_capabilities().egress_rules {
            return Err(AgentError::SandboxEgressPolicyUnsupported {
                sandbox: handle.sandbox_name.clone(),
                backend: handle.backend.to_string(),
            });
        }
        driver
            .set_egress_policy(&instance_handle(handle), policy)
            .await
            .map_err(agent_error)
    }

    async fn exec(
        &self,
        handle: &SandboxHandle,
        cmd: Vec<String>,
        env: HashMap<String, String>,
        on_output: Option<OnEventCallback>,
    ) -> Result<SandboxExecResult, AgentError> {
        self.exec_as(
            handle,
            ExecUser::Default,
            cmd,
            env,
            on_output.map(line_sink),
            false,
        )
        .await
    }

    async fn exec_as_root(
        &self,
        handle: &SandboxHandle,
        cmd: Vec<String>,
        env: HashMap<String, String>,
        on_output: Option<OnEventCallback>,
    ) -> Result<SandboxExecResult, AgentError> {
        self.exec_as(
            handle,
            ExecUser::Root,
            cmd,
            env,
            on_output.map(line_sink),
            false,
        )
        .await
    }

    async fn exec_as_user(
        &self,
        handle: &SandboxHandle,
        user: &str,
        cmd: Vec<String>,
        env: HashMap<String, String>,
        on_output: Option<OnEventCallback>,
    ) -> Result<SandboxExecResult, AgentError> {
        self.exec_as(
            handle,
            ExecUser::Named(user.to_string()),
            cmd,
            env,
            on_output.map(line_sink),
            false,
        )
        .await
    }

    async fn exec_streamed(
        &self,
        handle: &SandboxHandle,
        cmd: Vec<String>,
        env: HashMap<String, String>,
        on_event: Option<OnStreamEventCallback>,
    ) -> Result<SandboxExecResult, AgentError> {
        self.exec_as(
            handle,
            ExecUser::Default,
            cmd,
            env,
            on_event.map(stream_sink),
            true,
        )
        .await
    }

    async fn is_alive(&self, handle: &SandboxHandle) -> Result<bool, AgentError> {
        self.driver_for(handle)?
            .is_alive(&instance_handle(handle))
            .await
            .map_err(agent_error)
    }

    async fn write_file(
        &self,
        handle: &SandboxHandle,
        path: &str,
        contents: &[u8],
        mode: u32,
    ) -> Result<(), AgentError> {
        self.driver_for(handle)?
            .write_file(&instance_handle(handle), path, contents, mode)
            .await
            .map_err(agent_error)
    }

    async fn read_file(&self, handle: &SandboxHandle, path: &str) -> Result<Vec<u8>, AgentError> {
        self.driver_for(handle)?
            .read_file(&instance_handle(handle), path, None)
            .await
            .map_err(agent_error)
    }

    async fn read_file_bounded(
        &self,
        handle: &SandboxHandle,
        path: &str,
        max_bytes: u64,
    ) -> Result<Vec<u8>, AgentError> {
        self.driver_for(handle)?
            .read_file(&instance_handle(handle), path, Some(max_bytes))
            .await
            .map_err(agent_error)
    }

    async fn write_directory(
        &self,
        handle: &SandboxHandle,
        local_dir: &Path,
        target_path: &str,
    ) -> Result<(), AgentError> {
        self.driver_for(handle)?
            .write_directory(&instance_handle(handle), local_dir, target_path)
            .await
            .map_err(agent_error)
    }

    async fn sync_workspace_to_host(
        &self,
        handle: &SandboxHandle,
        host_dir: &Path,
    ) -> Result<(), AgentError> {
        self.driver_for(handle)?
            .sync_workspace_to_host(&instance_handle(handle), host_dir)
            .await
            .map_err(agent_error)
    }

    async fn kill_processes(
        &self,
        handle: &SandboxHandle,
        pattern: &str,
        signal: KillSignal,
    ) -> Result<(), AgentError> {
        let signal = match signal {
            KillSignal::Term => ProcessSignal::Term,
            KillSignal::Kill => ProcessSignal::Kill,
        };
        self.driver_for(handle)?
            .kill_processes(&instance_handle(handle), pattern, signal)
            .await
            .map_err(agent_error)
    }

    async fn destroy(&self, handle: &SandboxHandle, purge_volumes: bool) -> Result<(), AgentError> {
        self.driver_for(handle)?
            .remove(&instance_handle(handle), purge_volumes)
            .await
            .map_err(agent_error)
    }

    async fn stop(&self, handle: &SandboxHandle) -> Result<(), AgentError> {
        self.driver_for(handle)?
            .stop(&instance_handle(handle))
            .await
            .map_err(agent_error)
    }

    async fn start(&self, handle: &SandboxHandle) -> Result<(), AgentError> {
        self.driver_for(handle)?
            .start(&instance_handle(handle))
            .await
            .map_err(agent_error)
    }

    async fn resize_disk(
        &self,
        handle: &SandboxHandle,
        new_size_mb: u64,
    ) -> Result<(), AgentError> {
        let driver = self.driver_for(handle)?;
        if !temps_workload::supports(
            driver.capabilities(),
            driver.sandbox_capabilities(),
            SandboxFeature::ResizeDisk,
        ) {
            return Err(AgentError::SandboxExecFailed {
                run_id: 0,
                sandbox_id: handle.sandbox_id.clone(),
                reason: format!(
                    "resize_disk is not supported by the '{}' driver (sandbox {})",
                    driver.capabilities().name,
                    handle.sandbox_name
                ),
            });
        }
        driver
            .resize_disk(&instance_handle(handle), new_size_mb)
            .await
            .map_err(agent_error)
    }

    async fn recover(&self, run_id: i32) -> Result<Option<SandboxHandle>, AgentError> {
        for backend in self.scan_order() {
            let driver = &self.drivers[backend.0];
            if let Some(instance) = driver.recover_run(run_id).await.map_err(agent_error)? {
                return Ok(Some(self.handle(backend.1, instance)));
            }
        }
        Ok(None)
    }

    async fn recover_by_name(
        &self,
        container_name: &str,
    ) -> Result<Option<SandboxHandle>, AgentError> {
        for backend in self.scan_order() {
            let driver = &self.drivers[backend.0];
            if let Some(instance) = driver
                .recover_sandbox(container_name)
                .await
                .map_err(agent_error)?
            {
                return Ok(Some(self.handle(backend.1, instance)));
            }
        }
        Ok(None)
    }

    async fn release_orphaned_resources(&self, container_name: &str) -> Result<(), AgentError> {
        for backend in self.scan_order() {
            self.drivers[backend.0]
                .release_orphaned(container_name)
                .await
                .map_err(agent_error)?;
        }
        Ok(())
    }

    fn supports_backend(&self, backend: SandboxBackend) -> bool {
        let name = backend.to_string();
        self.drivers.iter().any(|d| d.capabilities().name == name)
    }

    async fn attach_pty(&self, handle: &SandboxHandle) -> Result<PtyAttachment, AgentError> {
        self.driver_for(handle)?
            .attach_pty(&instance_handle(handle))
            .await
            .map(pty_attachment)
            .map_err(agent_error)
    }

    async fn take_snapshot(
        &self,
        handle: &SandboxHandle,
        label: Option<String>,
        max_size_bytes: u64,
    ) -> Result<SnapshotArtifact, AgentError> {
        let driver = self.driver_for(handle)?;
        if !temps_workload::supports(
            driver.capabilities(),
            driver.sandbox_capabilities(),
            SandboxFeature::Snapshot,
        ) {
            return Err(AgentError::SandboxExecFailed {
                run_id: 0,
                sandbox_id: handle.sandbox_id.clone(),
                reason: format!(
                    "take_snapshot is not supported by the '{}' driver (sandbox {})",
                    driver.capabilities().name,
                    handle.sandbox_name
                ),
            });
        }
        let snapshot = driver
            .take_snapshot(&instance_handle(handle), label, max_size_bytes)
            .await
            .map_err(agent_error)?;
        snapshot_artifact(&snapshot)
    }

    async fn create_from_snapshot(
        &self,
        artifact: &SnapshotArtifact,
        mut config: SandboxCreateConfig,
    ) -> Result<SandboxHandle, AgentError> {
        // A snapshot restores on the driver that took it: pin it, so the
        // filter checks that driver (and refuses it if it cannot restore)
        // instead of looking for another.
        config.backend = Some(artifact.backend);
        let plan = self.plan_create(&config)?;
        let driver = &self.drivers[plan.index];
        if !driver.sandbox_capabilities().restore {
            return Err(AgentError::Validation {
                message: format!(
                    "snapshot {} cannot be restored: the '{}' driver does not restore snapshots",
                    artifact.content_digest,
                    driver.capabilities().name
                ),
            });
        }
        let snapshot = sandbox_snapshot(artifact);
        let owner = Self::owner(config.run_id);
        let create = sandbox_create_from(config, owner)?;
        let instance = driver
            .restore_snapshot(&snapshot, create)
            .await
            .map_err(agent_error)?;
        let mut handle = self.handle(plan.backend, instance);
        handle.conditions = plan.conditions;
        Ok(handle)
    }

    async fn delete_image(&self, image_ref: &str) -> Result<(), AgentError> {
        // Only drivers with an image store hold snapshot images.
        for driver in &self.drivers {
            if driver.sandbox_capabilities().snapshot {
                driver
                    .delete_snapshot_image(image_ref)
                    .await
                    .map_err(agent_error)?;
            }
        }
        Ok(())
    }

    fn name(&self) -> &str {
        self.product.name()
    }

    async fn is_available(&self) -> bool {
        self.product.is_available().await
    }

    async fn image_status(&self) -> Result<(bool, String), AgentError> {
        self.product.image_status().await
    }

    async fn rebuild_image(&self) -> Result<String, AgentError> {
        self.product.rebuild_image().await
    }

    async fn rootfs_report(&self) -> Result<RootfsReport, AgentError> {
        self.product.rootfs_report().await
    }

    async fn gc_rootfs(&self) -> Result<RootfsGcReport, AgentError> {
        self.product.gc_rootfs().await
    }

    async fn rebuild_image_with_progress(
        &self,
        on_progress: tokio::sync::mpsc::Sender<String>,
    ) -> Result<String, AgentError> {
        self.product.rebuild_image_with_progress(on_progress).await
    }
}

impl DriverSandboxProvider {
    /// Recovery order: the default driver first, then the rest by name, so
    /// a name present on two drivers resolves stably (as the routing
    /// provider does).
    fn scan_order(&self) -> Vec<(usize, SandboxBackend)> {
        let mut order: Vec<(usize, SandboxBackend)> = self
            .drivers
            .iter()
            .enumerate()
            .filter_map(|(i, d)| backend_named(&d.capabilities().name).map(|b| (i, b)))
            .collect();
        order.sort_by_key(|(_, b)| (*b != self.default, b.to_string()));
        order
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::sandbox::driver::SandboxBackendDriver;
    use std::path::PathBuf;
    use std::sync::Mutex;
    use std::time::Duration;

    /// A backend that records what it was asked and answers like a real
    /// one would for its backend.
    pub(crate) struct FakeBackend {
        pub(crate) backend: SandboxBackend,
        pub(crate) calls: Mutex<Vec<String>>,
    }

    impl FakeBackend {
        pub(crate) fn new(backend: SandboxBackend) -> Arc<Self> {
            Arc::new(Self {
                backend,
                calls: Mutex::new(Vec::new()),
            })
        }

        fn record(&self, call: String) {
            if let Ok(mut calls) = self.calls.lock() {
                calls.push(call);
            }
        }

        pub(crate) fn calls(&self) -> Vec<String> {
            self.calls.lock().map(|c| c.clone()).unwrap_or_default()
        }

        fn handle(&self, name: &str) -> SandboxHandle {
            SandboxHandle {
                node_id: None,
                sandbox_id: format!("{}-id", name),
                sandbox_name: name.to_string(),
                work_dir: crate::sandbox::driver::work_dir_for(self.backend),
                backend: self.backend,
                image: "alpine:3.20".to_string(),
                conditions: Vec::new(),
            }
        }
    }

    #[async_trait]
    impl SandboxProvider for FakeBackend {
        async fn create(&self, config: SandboxCreateConfig) -> Result<SandboxHandle, AgentError> {
            let label = config
                .container_name_override
                .clone()
                .unwrap_or_else(|| config.run_id.to_string());
            self.record(format!(
                "create {} network={:?} disk={:?}",
                label, config.network_mode, config.disk_size_mb
            ));
            Ok(self.handle(&format!("temps-sandbox-{}", label)))
        }
        async fn exec(
            &self,
            handle: &SandboxHandle,
            cmd: Vec<String>,
            _env: HashMap<String, String>,
            on_output: Option<OnEventCallback>,
        ) -> Result<SandboxExecResult, AgentError> {
            self.record(format!("exec {} {}", handle.sandbox_name, cmd.join(" ")));
            if let Some(cb) = on_output {
                cb("line".to_string()).await;
            }
            Ok(SandboxExecResult {
                exit_code: 0,
                stdout: "line\n".to_string(),
                stderr: String::new(),
            })
        }
        async fn is_alive(&self, handle: &SandboxHandle) -> Result<bool, AgentError> {
            self.record(format!("alive {}", handle.sandbox_name));
            Ok(true)
        }
        async fn write_file(
            &self,
            handle: &SandboxHandle,
            path: &str,
            _contents: &[u8],
            _mode: u32,
        ) -> Result<(), AgentError> {
            self.record(format!("write {} {}", handle.sandbox_name, path));
            Ok(())
        }
        async fn read_file(
            &self,
            handle: &SandboxHandle,
            path: &str,
        ) -> Result<Vec<u8>, AgentError> {
            self.record(format!("read {} {}", handle.sandbox_name, path));
            Err(AgentError::SandboxNotFound {
                run_id: 0,
                sandbox: handle.sandbox_name.clone(),
            })
        }
        async fn write_directory(
            &self,
            _handle: &SandboxHandle,
            _local_dir: &Path,
            _target_path: &str,
        ) -> Result<(), AgentError> {
            Ok(())
        }
        async fn kill_processes(
            &self,
            _handle: &SandboxHandle,
            _pattern: &str,
            _signal: KillSignal,
        ) -> Result<(), AgentError> {
            Ok(())
        }
        async fn destroy(
            &self,
            handle: &SandboxHandle,
            purge_volumes: bool,
        ) -> Result<(), AgentError> {
            self.record(format!("destroy {} {}", handle.sandbox_name, purge_volumes));
            Ok(())
        }
        async fn recover(&self, _run_id: i32) -> Result<Option<SandboxHandle>, AgentError> {
            Ok(None)
        }
        async fn recover_by_name(
            &self,
            container_name: &str,
        ) -> Result<Option<SandboxHandle>, AgentError> {
            Ok(
                (self.backend == SandboxBackend::Docker && container_name == "temps-sandbox-x")
                    .then(|| self.handle(container_name)),
            )
        }
        async fn set_egress_policy(
            &self,
            handle: &SandboxHandle,
            policy: &crate::sandbox::SandboxEgressPolicy,
        ) -> Result<(), AgentError> {
            self.record(format!(
                "egress {} {} rules={}",
                handle.sandbox_name,
                policy.mode,
                policy.rules.len()
            ));
            Ok(())
        }
        fn name(&self) -> &str {
            "fake"
        }
        async fn is_available(&self) -> bool {
            true
        }
        async fn image_status(&self) -> Result<(bool, String), AgentError> {
            Ok((true, "alpine:3.20".to_string()))
        }
        async fn rebuild_image(&self) -> Result<String, AgentError> {
            Ok("alpine:3.20".to_string())
        }
    }

    pub(crate) fn config(label: &str) -> SandboxCreateConfig {
        SandboxCreateConfig {
            allow_docker: false,
            run_id: 1,
            container_name_override: Some(label.to_string()),
            host_work_dir: PathBuf::from("/tmp/facade-test"),
            workspace_volume: None,
            image: None,
            cpu_limit: Some(1.0),
            memory_limit_mb: Some(256),
            pids_limit: None,
            disk_size_mb: None,
            egress_policy: Default::default(),
            network_mode: Some("none".to_string()),
            env_vars: HashMap::new(),
            idle_timeout: Duration::from_secs(60),
            backend: None,
            owner_user_id: None,
            node_id: None,
        }
    }

    pub(crate) fn facade(
        default: SandboxBackend,
    ) -> (
        DriverSandboxProvider,
        Arc<FakeBackend>,
        Arc<FakeBackend>,
        Arc<FakeBackend>,
    ) {
        let docker = FakeBackend::new(SandboxBackend::Docker);
        let fc = FakeBackend::new(SandboxBackend::Firecracker);
        let msb = FakeBackend::new(SandboxBackend::Microsandbox);
        let drivers: Vec<Arc<dyn SandboxExt>> = vec![
            Arc::new(SandboxBackendDriver::new(
                SandboxBackend::Docker,
                docker.clone(),
            )),
            Arc::new(SandboxBackendDriver::new(
                SandboxBackend::Firecracker,
                fc.clone(),
            )),
            Arc::new(SandboxBackendDriver::new(
                SandboxBackend::Microsandbox,
                msb.clone(),
            )),
        ];
        (
            DriverSandboxProvider::new(drivers, default, docker.clone()),
            docker,
            fc,
            msb,
        )
    }

    #[tokio::test]
    async fn the_default_driver_serves_a_request_it_can_take() {
        let (facade, docker, fc, _) = facade(SandboxBackend::Firecracker);
        let handle = facade.create(config("abc")).await.unwrap();
        assert_eq!(handle.backend, SandboxBackend::Firecracker);
        assert!(handle.conditions.is_empty(), "{:?}", handle.conditions);
        assert_eq!(
            fc.calls(),
            vec!["create abc network=Some(\"none\") disk=None"]
        );
        assert!(docker.calls().is_empty());
    }

    #[tokio::test]
    async fn a_default_lacking_a_capability_is_passed_over_and_says_so() {
        let (facade, docker, fc, _) = facade(SandboxBackend::Firecracker);
        let mut cfg = config("abc");
        cfg.network_mode = Some("restricted".into());
        let handle = facade.create(cfg).await.unwrap();
        assert_eq!(handle.backend, SandboxBackend::Docker);
        assert!(fc.calls().is_empty(), "firecracker never sees it");
        assert_eq!(docker.calls().len(), 1);
        let scheduled = &handle.conditions[0];
        assert_eq!(scheduled.kind, ConditionKind::Scheduled);
        assert_eq!(scheduled.reason, "PreferredDriverIneligible");
        assert!(
            scheduled
                .message
                .contains("firecracker lacks restricted egress"),
            "{}",
            scheduled.message
        );
    }

    #[tokio::test]
    async fn a_named_driver_lacking_a_capability_is_refused_not_downgraded() {
        let (facade, docker, fc, _) = facade(SandboxBackend::Docker);
        let mut cfg = config("abc");
        cfg.backend = Some(SandboxBackend::Firecracker);
        cfg.network_mode = Some("restricted".into());
        let error = facade.create(cfg).await.unwrap_err();
        assert!(matches!(error, AgentError::Validation { .. }), "{error:?}");
        assert!(
            error
                .to_string()
                .contains("0/1 drivers eligible: firecracker lacks restricted egress"),
            "{error}"
        );
        assert!(fc.calls().is_empty() && docker.calls().is_empty());

        // Guest Docker exists only on microsandbox.
        let mut cfg = config("guest-docker");
        cfg.backend = Some(SandboxBackend::Docker);
        cfg.allow_docker = true;
        let error = facade.create(cfg).await.unwrap_err();
        assert!(
            error.to_string().contains("docker lacks guest Docker"),
            "{error}"
        );
    }

    fn restricted_policy() -> crate::sandbox::SandboxEgressPolicy {
        crate::sandbox::SandboxEgressPolicy::restricted(vec![crate::sandbox::EgressRule::host(
            "api.example.com",
            &[443],
        )])
    }

    #[tokio::test]
    async fn a_restricted_egress_policy_is_placed_on_a_driver_that_enforces_it() {
        let (facade, docker, fc, _) = facade(SandboxBackend::Firecracker);
        let mut cfg = config("abc");
        cfg.egress_policy = restricted_policy();
        let handle = facade.create(cfg).await.unwrap();
        assert_eq!(handle.backend, SandboxBackend::Docker);
        assert!(fc.calls().is_empty(), "firecracker never sees it");
        assert_eq!(docker.calls().len(), 1);
        assert!(
            handle.conditions[0].message.contains("egress allow-lists"),
            "{:?}",
            handle.conditions
        );
    }

    #[tokio::test]
    async fn a_restricted_egress_policy_on_a_named_backend_without_allow_lists_is_a_typed_refusal()
    {
        for backend in [SandboxBackend::Firecracker, SandboxBackend::Microsandbox] {
            let (facade, docker, fc, msb) = facade(SandboxBackend::Docker);
            let mut cfg = config("abc");
            cfg.backend = Some(backend);
            cfg.egress_policy = restricted_policy();
            let error = facade.create(cfg).await.unwrap_err();
            match &error {
                AgentError::SandboxEgressPolicyUnsupported {
                    sandbox,
                    backend: refused,
                } => {
                    assert_eq!(sandbox, "abc");
                    assert_eq!(refused, &backend.to_string());
                }
                other => panic!("{backend}: expected a typed refusal, got {other:?}"),
            }
            assert!(
                error.to_string().contains("use the docker backend"),
                "{error}"
            );
            assert!(docker.calls().is_empty() && fc.calls().is_empty() && msb.calls().is_empty());

            // The default policy is fine everywhere.
            let mut cfg = config("open");
            cfg.backend = Some(backend);
            assert!(facade.create(cfg).await.is_ok(), "{backend}");
        }
    }

    #[tokio::test]
    async fn set_egress_policy_goes_to_the_owning_driver_or_is_refused_by_capability() {
        let (facade, docker, fc, _) = facade(SandboxBackend::Docker);
        let docker_handle = docker.handle("temps-sandbox-d");
        facade
            .set_egress_policy(&docker_handle, &restricted_policy())
            .await
            .unwrap();
        assert_eq!(
            docker.calls(),
            vec!["egress temps-sandbox-d restricted rules=1"]
        );

        let fc_handle = fc.handle("temps-sandbox-f");
        let error = facade
            .set_egress_policy(&fc_handle, &restricted_policy())
            .await
            .unwrap_err();
        assert!(
            matches!(
                &error,
                AgentError::SandboxEgressPolicyUnsupported { sandbox, backend }
                    if sandbox == "temps-sandbox-f" && backend == "firecracker"
            ),
            "{error:?}"
        );
        assert!(fc.calls().is_empty(), "never reaches the backend");
        // `internet` is what every backend already does.
        facade
            .set_egress_policy(&fc_handle, &crate::sandbox::SandboxEgressPolicy::internet())
            .await
            .unwrap();
        assert_eq!(fc.calls(), vec!["egress temps-sandbox-f internet rules=0"]);
    }

    #[tokio::test]
    async fn docker_disk_size_is_reported_as_an_unenforced_limit() {
        let (facade, docker, _, _) = facade(SandboxBackend::Docker);
        let mut cfg = config("abc");
        cfg.disk_size_mb = Some(4096);
        let handle = facade.create(cfg).await.unwrap();
        assert_eq!(
            docker.calls(),
            vec!["create abc network=Some(\"none\") disk=Some(4096)"]
        );
        let limit = handle
            .conditions
            .iter()
            .find(|c| c.kind == ConditionKind::LimitEnforced)
            .expect("a LimitEnforced condition");
        assert_eq!(limit.status, ConditionStatus::False);
        assert_eq!(limit.reason, "LimitNotSupported");
        assert!(limit
            .message
            .starts_with("docker: disk_size_mb is not applied"));
        // Serialized onto the handle, so it reaches the sandbox timeline.
        let json = serde_json::to_value(&handle).unwrap();
        assert_eq!(json["conditions"][0]["status"], "False");
    }

    #[tokio::test]
    async fn handle_calls_go_to_the_owning_driver_and_keep_their_error_type() {
        let (facade, docker, fc, msb) = facade(SandboxBackend::Docker);
        let mut cfg = config("vm");
        cfg.backend = Some(SandboxBackend::Microsandbox);
        let handle = facade.create(cfg).await.unwrap();
        assert_eq!(handle.backend, SandboxBackend::Microsandbox);
        let result = facade
            .exec(
                &handle,
                vec!["echo".into(), "hi".into()],
                HashMap::new(),
                None,
            )
            .await
            .unwrap();
        assert_eq!(result.stdout, "line\n");
        assert!(facade.is_alive(&handle).await.unwrap());
        let error = facade
            .read_file(&handle, "/etc/hostname")
            .await
            .unwrap_err();
        assert!(
            matches!(error, AgentError::SandboxNotFound { .. }),
            "{error:?}"
        );
        facade.destroy(&handle, true).await.unwrap();
        assert_eq!(
            msb.calls(),
            vec![
                "create vm network=Some(\"none\") disk=None".to_string(),
                "exec temps-sandbox-vm echo hi".to_string(),
                "alive temps-sandbox-vm".to_string(),
                "read temps-sandbox-vm /etc/hostname".to_string(),
                "destroy temps-sandbox-vm true".to_string(),
            ]
        );
        assert!(docker.calls().is_empty() && fc.calls().is_empty());
    }

    #[tokio::test]
    async fn operations_a_driver_lacks_are_refused_before_reaching_it() {
        let (facade, _, _, _) = facade(SandboxBackend::Docker);
        let handle = facade.create(config("abc")).await.unwrap();
        let error = facade.resize_disk(&handle, 4096).await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("resize_disk is not supported by the 'docker' driver"),
            "{error}"
        );

        let mut cfg = config("vm");
        cfg.backend = Some(SandboxBackend::Microsandbox);
        let vm = facade.create(cfg).await.unwrap();
        let error = facade.take_snapshot(&vm, None, 1024).await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("take_snapshot is not supported by the 'microsandbox' driver"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn recovery_and_unknown_network_modes() {
        let (facade, _, _, _) = facade(SandboxBackend::Firecracker);
        let found = facade.recover_by_name("temps-sandbox-x").await.unwrap();
        assert_eq!(found.map(|h| h.backend), Some(SandboxBackend::Docker));
        assert!(facade.recover_by_name("nope").await.unwrap().is_none());

        let mut cfg = config("abc");
        cfg.network_mode = Some("bridge".into());
        let error = facade.create(cfg).await.unwrap_err();
        assert!(matches!(error, AgentError::Validation { .. }), "{error:?}");
        assert!(facade.supports_backend(SandboxBackend::Microsandbox));
        assert!(!facade.supports_backend(SandboxBackend::Local));
    }
}
