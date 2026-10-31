// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Scale-to-zero workloads (ADR-055 §11; phase 10: functions).
//!
//! A scale-to-zero workload is live while nothing runs. The plane never
//! starts one on its own: the activator calls
//! [`WorkloadPlane::wake_workload`] on the first request and
//! [`WorkloadPlane::sleep_workload`] once it has been idle. The reconciler
//! only observes. When an instance exits or disappears (a crash, a restart
//! of `temps`, which takes in-process WASM instances with it), the plane
//! records it and the workload is asleep again. It is never failed, so the
//! next request wakes it.
//!
//! A failed wake does not fail the workload either. It is reported as
//! `Ready = False` with the reason and as an event, and the requests that
//! waited get the message.
//!
//! Each wake is a new attempt with its own instance row. A sleep removes
//! the instance and the rows of earlier attempts, so a function woken a
//! thousand times keeps one row.
//!
//! The driver is picked by capability ([`driver_fits`]): artifact kind,
//! isolation, and whether the driver enforces the workload's egress itself.
//! The plane wires no network attacher, so a driver that relies on one
//! cannot run restricted egress and is passed over with why. The workload
//! is never run unconfined.

use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use sea_orm::sea_query::Expr;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set};
use temps_entities::{workload_instances, workloads};
use temps_workload::{
    Artifact, AttachmentKind, ConditionKind, ConditionStatus, DriverCapabilities, EgressProfile,
    InstanceHandle, InstanceState, InstanceStatus, Lifetime, RuntimeDriver, WorkloadPhase,
    WorkloadSpec, WorkloadStatus,
};

use crate::env;
use crate::error::PlaneError;
use crate::events;
use crate::plane::{decode_status, view_from, WorkloadPlane, WorkloadView};
use crate::reconciler::{
    driver_error, handle_of, instance_name, is_finished, set_limit_condition, set_scheduled,
    PHASE_FAILED, PHASE_PENDING, PHASE_RUNNING, PHASE_SUCCEEDED,
};

/// A pending (waking) instance older than this was left by a wake that
/// died with its process; the next pass or wake removes it.
pub const STALE_WAKE_SECS: i64 = 300;

/// A scale-to-zero workload that serves requests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Awake {
    /// Where requests go: `ip:port` on the control plane.
    pub address: String,
    pub attempt: i32,
    /// Whether this call started it (it was asleep).
    pub woke: bool,
}

/// Whether a driver with `capabilities` can run `spec`, and why not.
pub fn driver_fits(capabilities: &DriverCapabilities, spec: &WorkloadSpec) -> Result<(), String> {
    let (supported, kind) = match &spec.artifact {
        Artifact::OciImage { .. } => (capabilities.artifacts.oci, "OCI image"),
        Artifact::WasmComponent { .. } => (capabilities.artifacts.wasm, "WASM"),
        Artifact::Rootfs { .. } => (capabilities.artifacts.rootfs, "root filesystem"),
    };
    if !supported {
        return Err(format!("does not run {kind} artifacts"));
    }
    if capabilities.isolation != spec.isolation {
        return Err(format!(
            "isolates as {:?}, and the workload needs {:?}",
            capabilities.isolation, spec.isolation
        ));
    }
    match spec.network.egress {
        None | Some(EgressProfile::Open) => Ok(()),
        Some(egress) => match capabilities.network_attachment {
            // The driver itself confines the guest (an in-host socket
            // allow-list, the sandbox SDK's policy).
            AttachmentKind::InHost | AttachmentKind::SdkPolicy => Ok(()),
            attachment => Err(format!(
                "cannot enforce {egress:?} egress itself (its attachment is {attachment:?}, \
                 which needs the network attacher, and the plane wires none yet)"
            )),
        },
    }
}

/// `Ready` and `Running` are `False` with `reason`: nothing serves.
fn set_asleep(status: &mut WorkloadStatus, reason: &str, message: &str, now: DateTime<Utc>) {
    status.ready_instances = 0;
    status.set_condition(
        ConditionKind::Running,
        ConditionStatus::False,
        reason,
        message,
        now,
    );
    status.set_condition(
        ConditionKind::Ready,
        ConditionStatus::False,
        reason,
        message,
        now,
    );
    // Live, not finished: the next request wakes it.
    status.phase = WorkloadPhase::Running;
}

const ASLEEP_MESSAGE: &str = "scaled to zero; the next request wakes it";

/// The address requests reach the instance on: its first published port.
fn address_of(observed: &InstanceStatus) -> Option<String> {
    observed.published_ports.iter().find_map(|binding| {
        binding
            .host_port
            .map(|port| format!("{}:{port}", binding.host_ip))
    })
}

impl WorkloadPlane {
    /// The local drivers, the node driver first.
    fn local_candidates(&self) -> impl Iterator<Item = &Arc<dyn RuntimeDriver>> {
        self.driver.0.iter().chain(self.local_drivers.iter())
    }

    /// The control-plane driver called `name`: a local driver, or the node
    /// driver (every instance row written before local drivers existed
    /// names it).
    pub(crate) fn local_driver_named(&self, name: &str) -> Result<Arc<dyn RuntimeDriver>, String> {
        if let Some(driver) = self
            .local_drivers
            .iter()
            .find(|driver| driver.capabilities().name == name)
        {
            return Ok(Arc::clone(driver));
        }
        let primary = self.control_plane_driver()?;
        if primary.capabilities().name == name {
            return Ok(primary);
        }
        Err(format!(
            "the instance was created by runtime driver '{name}', which this process does not \
             run (it runs: {})",
            self.local_driver_names()
        ))
    }

    /// Names of the control-plane drivers, the node driver first.
    pub fn local_driver_names_list(&self) -> Vec<String> {
        self.local_candidates()
            .map(|driver| driver.capabilities().name.clone())
            .collect()
    }

    fn local_driver_names(&self) -> String {
        let names = self.local_driver_names_list();
        if names.is_empty() {
            "none".to_string()
        } else {
            names.join(", ")
        }
    }

    /// The first control-plane driver that can run `spec`, or each one
    /// passed over and why.
    pub fn local_driver_for(&self, spec: &WorkloadSpec) -> Result<Arc<dyn RuntimeDriver>, String> {
        let mut passed = Vec::new();
        for driver in self.local_candidates() {
            match driver_fits(driver.capabilities(), spec) {
                Ok(()) => return Ok(Arc::clone(driver)),
                Err(why) => passed.push(format!("{}: {why}", driver.capabilities().name)),
            }
        }
        if passed.is_empty() {
            return Err("the control plane (node 0) has no runtime driver".to_string());
        }
        Err(passed.join("; "))
    }

    async fn scale_rows(
        &self,
        id: i64,
    ) -> Result<
        (
            workloads::Model,
            WorkloadView,
            Vec<workload_instances::Model>,
        ),
        PlaneError,
    > {
        let row = workloads::Entity::find_by_id(id)
            .one(self.db.as_ref())
            .await
            .map_err(PlaneError::db("reading the scale-to-zero workload", id))?
            .ok_or(PlaneError::NotFound { workload_id: id })?;
        let instances = workload_instances::Entity::find()
            .filter(workload_instances::Column::WorkloadId.eq(id))
            .order_by_asc(workload_instances::Column::Attempt)
            .all(self.db.as_ref())
            .await
            .map_err(PlaneError::db("reading the workload's instances", id))?;
        let view = view_from(row.clone(), instances.last())?;
        if !matches!(view.spec.lifetime, Lifetime::ScaleToZero { .. }) {
            return Err(PlaneError::NotWakeable {
                workload_id: id,
                reason: format!(
                    "its lifetime is {:?}; only scale_to_zero workloads are woken and put to \
                     sleep",
                    view.spec.lifetime
                ),
            });
        }
        if row.deleted_at.is_some() {
            return Err(PlaneError::NotWakeable {
                workload_id: id,
                reason: "it is being deleted".to_string(),
            });
        }
        if view.status.phase.is_terminal() {
            return Err(PlaneError::NotWakeable {
                workload_id: id,
                reason: format!("it is {}", view.status.phase.as_str()),
            });
        }
        Ok((row, view, instances))
    }

    /// Stop and remove `instance` if it still exists, and record it
    /// finished. Best effort on the runtime: an instance that cannot be
    /// reached is recorded finished all the same, so a wake is never
    /// blocked by a dead one.
    async fn retire_instance(
        &self,
        instance: &workload_instances::Model,
        phase: &str,
        now: DateTime<Utc>,
    ) -> Result<(), PlaneError> {
        let handle = handle_of(instance)?;
        let mut exit_code = None;
        match self.driver_of(instance).await {
            Ok(driver) => {
                let observed = driver.inspect(&handle).await;
                if let Ok(observed) = &observed {
                    exit_code = observed.exit_code;
                }
                let exists = !matches!(
                    observed,
                    Ok(InstanceStatus {
                        restart_at: None,
                        state: InstanceState::Missing,
                        ..
                    })
                );
                if exists {
                    if let Err(error) = driver.stop(&handle).await {
                        tracing::warn!(
                            workload_id = instance.workload_id,
                            instance = %handle.name,
                            "stopping a scale-to-zero instance failed: {error}"
                        );
                    }
                    driver
                        .remove(&handle, true)
                        .await
                        .map_err(driver_error(instance.workload_id, "remove the instance"))?;
                }
            }
            Err(reason) => tracing::warn!(
                workload_id = instance.workload_id,
                instance_id = instance.id,
                "the instance's runtime is not available; recording it finished: {reason}"
            ),
        }
        if !is_finished(instance) {
            self.finish_instance(instance.id, phase, exit_code, now)
                .await?;
        }
        Ok(())
    }

    /// Wake scale-to-zero workload `id`: start an instance on a
    /// control-plane driver that can run it and return where it serves.
    /// An instance that already serves is returned as it is. Callers
    /// serialise wakes of one workload (the activator does).
    pub async fn wake_workload(&self, id: i64) -> Result<Awake, PlaneError> {
        let now = Utc::now();
        let (row, view, instances) = self.scale_rows(id).await?;
        let latest = instances.last().cloned();
        if let Some(latest) = latest.as_ref().filter(|i| !is_finished(i)) {
            if latest.phase == PHASE_RUNNING {
                if let Ok(driver) = self.driver_of(latest).await {
                    let handle = handle_of(latest)?;
                    if let Ok(observed) = driver.inspect(&handle).await {
                        if observed.state == InstanceState::Running {
                            if let Some(address) = address_of(&observed) {
                                return Ok(Awake {
                                    address,
                                    attempt: latest.attempt,
                                    woke: false,
                                });
                            }
                        }
                    }
                }
            } else if now - latest.created_at < Duration::seconds(STALE_WAKE_SECS) {
                return Err(PlaneError::NotWakeable {
                    workload_id: id,
                    reason: format!(
                        "attempt {} is already starting (since {}); retry once it is up",
                        latest.attempt,
                        latest.created_at.to_rfc3339()
                    ),
                });
            }
            // Dead or stale: out of the way of the new attempt.
            self.retire_instance(latest, PHASE_FAILED, now).await?;
        }

        let mut status = view.status.clone();
        let driver = match self.local_driver_for(&view.spec) {
            Ok(driver) => driver,
            Err(reason) => {
                status.set_condition(
                    ConditionKind::Scheduled,
                    ConditionStatus::False,
                    "NoEligibleDriver",
                    &reason,
                    now,
                );
                set_asleep(&mut status, "NoEligibleDriver", &reason, now);
                if self.write_status(&row, &status, None).await? {
                    events::record(self.db.as_ref(), id, None, "NoEligibleDriver", &reason, now)
                        .await?;
                }
                return Err(PlaneError::NoEligibleDriver {
                    workload_id: id,
                    reason,
                });
            }
        };
        set_scheduled(&mut status, None, now);
        set_limit_condition(&mut status, &view.spec.resources, &driver, now);

        let attempt = latest.as_ref().map(|i| i.attempt + 1).unwrap_or(1);
        let name = instance_name(
            &self.config.scope,
            view.spec.owner.kind.as_str(),
            id,
            u32::try_from(attempt).unwrap_or(0),
        );
        let handle = InstanceHandle {
            driver: driver.capabilities().name.clone(),
            id: name.clone(),
            name,
        };
        let handle_json = serde_json::to_value(&handle).map_err(|e| PlaneError::Encode {
            workload: row.name.clone(),
            field: "instance handle",
            reason: e.to_string(),
        })?;
        let instance = workload_instances::ActiveModel {
            workload_id: Set(id),
            revision: Set(row.generation),
            attempt: Set(attempt),
            node_id: Set(None),
            driver: Set(handle.driver.clone()),
            handle: Set(handle_json),
            phase: Set(PHASE_PENDING.to_string()),
            ready: Set(false),
            conditions: Set(serde_json::json!([])),
            ..Default::default()
        }
        .insert(self.db.as_ref())
        .await
        .map_err(PlaneError::db("inserting the woken instance row", id))?;
        self.launch_awake(row, view, status, instance, handle, driver, now)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn launch_awake(
        &self,
        row: workloads::Model,
        view: WorkloadView,
        status: WorkloadStatus,
        instance: workload_instances::Model,
        handle: InstanceHandle,
        driver: Arc<dyn RuntimeDriver>,
        now: DateTime<Utc>,
    ) -> Result<Awake, PlaneError> {
        let attempt = instance.attempt;
        let spec = &view.spec;
        let failed = |reason: &'static str, message: String, created: Option<InstanceHandle>| {
            (reason, message, created)
        };
        let outcome = async {
            let prepared = driver.prepare(&spec.artifact).await.map_err(|error| {
                failed(
                    "ArtifactPullFailed",
                    format!("attempt {attempt} could not prepare its artifact: {error}"),
                    None,
                )
            })?;
            let resolved = env::resolve_with_staged(&spec.env, self.env.as_ref(), &|import| {
                self.staged(import)
            })
            .await
            .map_err(|reason| {
                failed(
                    "EnvironmentUnavailable",
                    format!("attempt {attempt} could not resolve its environment: {reason}"),
                    None,
                )
            })?;
            let template = self.instance_template(
                spec,
                row.id,
                instance.revision,
                u32::try_from(attempt).unwrap_or(0),
                &handle.name,
            );
            let attach = self.local_attachments(&template.attachments);
            let instance_spec = template.into_spec(prepared.local_ref.clone(), resolved.env);
            let created = driver
                .create(&instance_spec, &attach)
                .await
                .map_err(|error| {
                    failed(
                        "CreateFailed",
                        format!("attempt {attempt}: {error}"),
                        Some(handle.clone()),
                    )
                })?;
            let created_json = serde_json::to_value(&created).map_err(|e| {
                failed(
                    "CreateFailed",
                    format!("attempt {attempt}: the instance handle could not be encoded: {e}"),
                    Some(created.clone()),
                )
            })?;
            workload_instances::Entity::update_many()
                .col_expr(
                    workload_instances::Column::Handle,
                    Expr::value(created_json),
                )
                .filter(workload_instances::Column::Id.eq(instance.id))
                .exec(self.db.as_ref())
                .await
                .map_err(|e| {
                    failed(
                        "CreateFailed",
                        format!("attempt {attempt}: recording the instance handle failed: {e}"),
                        Some(created.clone()),
                    )
                })?;
            driver.start(&created).await.map_err(|error| {
                failed(
                    "StartFailed",
                    format!("attempt {attempt}: {error}"),
                    Some(created.clone()),
                )
            })?;
            let observed = driver.inspect(&created).await.map_err(|error| {
                failed(
                    "StartFailed",
                    format!("attempt {attempt}: inspecting the started instance failed: {error}"),
                    Some(created.clone()),
                )
            })?;
            let address = address_of(&observed).ok_or_else(|| {
                failed(
                    "NoAddress",
                    format!(
                        "attempt {attempt} ('{}') started but publishes no port to send requests \
                         to",
                        created.name
                    ),
                    Some(created.clone()),
                )
            })?;
            Ok::<_, (&'static str, String, Option<InstanceHandle>)>((created, address))
        }
        .await;

        let (created, address) = match outcome {
            Ok(done) => done,
            Err((reason, message, created)) => {
                if let Some(created) = created {
                    if let Err(cleanup) = driver.remove(&created, true).await {
                        tracing::warn!(
                            workload_id = row.id,
                            instance = %created.name,
                            "could not remove an instance whose wake failed: {cleanup}"
                        );
                    }
                }
                return Err(self
                    .wake_failed(&row, status, &instance, reason, message, now)
                    .await);
            }
        };
        workload_instances::Entity::update_many()
            .col_expr(
                workload_instances::Column::Address,
                Expr::value(address.clone()),
            )
            .col_expr(workload_instances::Column::Ready, Expr::value(true))
            .filter(workload_instances::Column::Id.eq(instance.id))
            .exec(self.db.as_ref())
            .await
            .map_err(PlaneError::db(
                "recording the woken instance's address",
                row.id,
            ))?;
        events::record(
            self.db.as_ref(),
            row.id,
            Some(instance.id),
            "Woken",
            &format!(
                "attempt {attempt} woke as '{}' on {} at {address}",
                created.name,
                driver.capabilities().name
            ),
            now,
        )
        .await?;
        tracing::info!(
            workload_id = row.id,
            attempt,
            driver = %driver.capabilities().name,
            %address,
            "woke a scale-to-zero workload"
        );
        self.mark_started(row, status, &instance, created, None, &driver, now)
            .await?;
        Ok(Awake {
            address,
            attempt,
            woke: true,
        })
    }

    /// Record a failed wake: the attempt is finished, the workload stays
    /// live and asleep with the reason.
    async fn wake_failed(
        &self,
        row: &workloads::Model,
        mut status: WorkloadStatus,
        instance: &workload_instances::Model,
        reason: &'static str,
        message: String,
        now: DateTime<Utc>,
    ) -> PlaneError {
        tracing::warn!(
            workload_id = row.id,
            attempt = instance.attempt,
            reason,
            "waking a scale-to-zero workload failed: {message}"
        );
        if reason == "ArtifactPullFailed" {
            status.set_condition(
                ConditionKind::ArtifactReady,
                ConditionStatus::False,
                reason,
                &message,
                now,
            );
        }
        set_asleep(&mut status, reason, &message, now);
        let recorded = async {
            self.finish_instance(instance.id, PHASE_FAILED, None, now)
                .await?;
            self.write_status(row, &status, None).await?;
            events::record(
                self.db.as_ref(),
                row.id,
                Some(instance.id),
                reason,
                &message,
                now,
            )
            .await
        }
        .await;
        if let Err(error) = recorded {
            tracing::error!(
                workload_id = row.id,
                "recording a failed wake failed: {error}"
            );
        }
        PlaneError::WakeFailed {
            workload_id: row.id,
            attempt: instance.attempt,
            reason,
            message,
        }
    }

    /// Put scale-to-zero workload `id` to sleep: its instance is stopped
    /// and removed, and the rows of earlier attempts go. `Ok(false)` when
    /// nothing was running.
    pub async fn sleep_workload(&self, id: i64) -> Result<bool, PlaneError> {
        let now = Utc::now();
        let (row, view, instances) = self.scale_rows(id).await?;
        let mut slept = false;
        for instance in instances.iter().filter(|i| !is_finished(i)) {
            self.retire_instance(instance, PHASE_SUCCEEDED, now).await?;
            slept = true;
        }
        if let Some(latest) = instances.last() {
            workload_instances::Entity::delete_many()
                .filter(workload_instances::Column::WorkloadId.eq(id))
                .filter(workload_instances::Column::Attempt.lt(latest.attempt))
                .exec(self.db.as_ref())
                .await
                .map_err(PlaneError::db("removing earlier attempts' rows", id))?;
        }
        let mut status = decode_status(&row)?;
        set_asleep(&mut status, "ScaledToZero", ASLEEP_MESSAGE, now);
        self.write_status(&row, &status, None).await?;
        if slept {
            let attempt = view.attempt;
            events::record(
                self.db.as_ref(),
                id,
                None,
                "ScaledToZero",
                &format!("attempt {attempt} was idle and was stopped; {ASLEEP_MESSAGE}"),
                now,
            )
            .await?;
            tracing::info!(
                workload_id = id,
                attempt,
                "put a scale-to-zero workload to sleep"
            );
        }
        Ok(slept)
    }

    /// The reconciler's part: observe, never start.
    pub(crate) async fn drive_scale_to_zero(
        &self,
        row: workloads::Model,
        view: WorkloadView,
        instances: Vec<workload_instances::Model>,
        now: DateTime<Utc>,
    ) -> Result<Option<DateTime<Utc>>, PlaneError> {
        let mut status = view.status.clone();
        let Some(latest) = instances.last().filter(|i| !is_finished(i)).cloned() else {
            set_asleep(&mut status, "ScaledToZero", ASLEEP_MESSAGE, now);
            self.write_status(&row, &status, None).await?;
            return Ok(None);
        };
        if latest.phase == PHASE_PENDING {
            let stale_at = latest.created_at + Duration::seconds(STALE_WAKE_SECS);
            if now < stale_at {
                // A wake in progress: it reports for itself.
                return Ok(Some(stale_at));
            }
            self.retire_instance(&latest, PHASE_FAILED, now).await?;
            let message = format!(
                "attempt {} never finished starting (its wake was interrupted); {ASLEEP_MESSAGE}",
                latest.attempt
            );
            set_asleep(&mut status, "WakeInterrupted", &message, now);
            self.write_status(&row, &status, None).await?;
            events::record(
                self.db.as_ref(),
                row.id,
                Some(latest.id),
                "WakeInterrupted",
                &message,
                now,
            )
            .await?;
            return Ok(None);
        }
        let handle = handle_of(&latest)?;
        let observed = match self.driver_of(&latest).await {
            Ok(driver) => match driver.inspect(&handle).await {
                Ok(observed) if observed.state == InstanceState::Running => {
                    return self
                        .mark_started(row, status, &latest, handle, None, &driver, now)
                        .await;
                }
                Ok(observed) => Some(observed),
                Err(error) => {
                    return Err(driver_error(row.id, "inspect the scale-to-zero instance")(
                        error,
                    ))
                }
            },
            Err(reason) => {
                tracing::warn!(workload_id = row.id, "{reason}");
                None
            }
        };
        let exit_code = observed.as_ref().and_then(|o| o.exit_code);
        let phase = if exit_code == Some(0) {
            PHASE_SUCCEEDED
        } else {
            PHASE_FAILED
        };
        self.retire_instance(&latest, phase, now).await?;
        let message = format!(
            "attempt {} ('{}') stopped outside the activator (exit code {exit_code:?}); \
             {ASLEEP_MESSAGE}",
            latest.attempt, handle.name
        );
        status.exit_code = exit_code;
        set_asleep(&mut status, "InstanceExited", &message, now);
        self.write_status(&row, &status, None).await?;
        events::record(
            self.db.as_ref(),
            row.id,
            Some(latest.id),
            "InstanceExited",
            &message,
            now,
        )
        .await?;
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{apply_request, function_spec, plane, test_db, FakeDriver};
    use crate::NodeDriver;
    use temps_workload::{ArtifactKinds, Isolation};

    fn function_request(name: &str) -> crate::ApplyRequest {
        let mut request = apply_request(9, name, function_spec("sha256:ab12"));
        request.owner = crate::Owner::new("function", 9);
        request
    }

    #[test]
    fn a_driver_fits_by_artifact_isolation_and_egress() {
        let docker = FakeDriver::new();
        let wasm = FakeDriver::wasm();
        let mut spec = function_spec("sha256:ab12");
        spec.network.egress = Some(EgressProfile::Restricted);
        let error = driver_fits(docker.capabilities(), &spec).expect_err("docker runs no wasm");
        assert!(error.contains("WASM"), "{error}");
        driver_fits(wasm.capabilities(), &spec).expect("wasm runs it, confined in-host");

        let mut oci = spec.clone();
        oci.artifact = Artifact::oci("registry.example.test/fn:1");
        oci.isolation = Isolation::Process;
        let error = driver_fits(docker.capabilities(), &oci).expect_err("no attacher");
        assert!(error.contains("Restricted egress"), "{error}");
        oci.network.egress = Some(EgressProfile::Open);
        driver_fits(docker.capabilities(), &oci).expect("open egress needs no attacher");

        let caps = DriverCapabilities {
            artifacts: ArtifactKinds {
                wasm: true,
                ..Default::default()
            },
            isolation: Isolation::Process,
            ..wasm.capabilities().clone()
        };
        assert!(driver_fits(&caps, &spec)
            .expect_err("wrong isolation")
            .contains("isolates as Process"));
    }

    #[tokio::test]
    async fn a_function_sleeps_until_woken_then_serves_then_sleeps_again() {
        let Some(db) = test_db().await else { return };
        let docker = FakeDriver::new();
        let wasm = FakeDriver::wasm();
        let (plane, _audit) = plane(&db, NodeDriver(Some(docker.clone())), Vec::new());
        let plane = Arc::new(
            Arc::try_unwrap(plane)
                .expect("sole owner")
                .with_local_driver(wasm.clone()),
        );
        let id = plane
            .apply(function_request("fn-a"))
            .await
            .expect("applied");

        // The reconciler never starts it.
        plane.reconcile_at(Utc::now(), false).await;
        assert!(wasm.running().is_empty() && docker.running().is_empty());
        let view = plane.get(id).await.expect("read");
        assert_eq!(view.status.phase, WorkloadPhase::Running);
        let ready = view.status.condition(ConditionKind::Ready).expect("ready");
        assert_eq!(ready.reason, "ScaledToZero");

        // The first wake starts it on the WASM driver, not Docker.
        let awake = plane.wake_workload(id).await.expect("woken");
        assert!(awake.woke);
        assert_eq!(awake.attempt, 1);
        assert!(awake.address.starts_with("127.0.0.1:"), "{}", awake.address);
        assert_eq!(wasm.running().len(), 1);
        assert!(docker.running().is_empty());
        let view = plane.get(id).await.expect("read");
        assert_eq!(view.status.ready_instances, 1);

        // Waking an awake function returns the same instance.
        let again = plane.wake_workload(id).await.expect("still awake");
        assert!(!again.woke);
        assert_eq!(again.address, awake.address);

        // A pass leaves it running.
        plane.reconcile_at(Utc::now(), false).await;
        assert_eq!(wasm.running().len(), 1);

        // Sleep stops and removes it.
        assert!(plane.sleep_workload(id).await.expect("slept"));
        assert!(wasm.running().is_empty());
        assert!(!plane.sleep_workload(id).await.expect("already asleep"));
        let view = plane.get(id).await.expect("read");
        assert_eq!(view.status.ready_instances, 0);

        // Woken again, as attempt 2; attempt 1's row is gone after the
        // next sleep.
        let second = plane.wake_workload(id).await.expect("woken again");
        assert_eq!(second.attempt, 2);
        plane.sleep_workload(id).await.expect("slept");
        let rows = workload_instances::Entity::find()
            .filter(workload_instances::Column::WorkloadId.eq(id))
            .all(db.db.as_ref())
            .await
            .expect("rows");
        assert_eq!(rows.len(), 1, "one row however often it is woken");
        assert_eq!(rows[0].attempt, 2);
    }

    #[tokio::test]
    async fn an_instance_that_exits_leaves_the_function_asleep_not_failed() {
        let Some(db) = test_db().await else { return };
        let wasm = FakeDriver::wasm();
        let (plane, _audit) = plane(&db, NodeDriver(None), Vec::new());
        let plane = Arc::new(
            Arc::try_unwrap(plane)
                .expect("sole owner")
                .with_local_driver(wasm.clone()),
        );
        let id = plane
            .apply(function_request("fn-b"))
            .await
            .expect("applied");
        plane.wake_workload(id).await.expect("woken");
        let running = wasm.running();
        wasm.exit(&running[0], 1);
        plane.reconcile_at(Utc::now(), false).await;
        let view = plane.get(id).await.expect("read");
        assert_eq!(view.status.phase, WorkloadPhase::Running, "never failed");
        let ready = view.status.condition(ConditionKind::Ready).expect("ready");
        assert_eq!(ready.reason, "InstanceExited");
        assert!(
            ready.message.contains("exit code Some(1)"),
            "{}",
            ready.message
        );
        // The next request wakes it again.
        let awake = plane.wake_workload(id).await.expect("woken again");
        assert_eq!(awake.attempt, 2);
    }

    #[tokio::test]
    async fn a_failed_wake_reports_why_and_keeps_the_function_live() {
        let Some(db) = test_db().await else { return };
        let wasm = FakeDriver::wasm();
        wasm.fail_prepare(true);
        let (plane, _audit) = plane(&db, NodeDriver(None), Vec::new());
        let plane = Arc::new(
            Arc::try_unwrap(plane)
                .expect("sole owner")
                .with_local_driver(wasm.clone()),
        );
        let id = plane
            .apply(function_request("fn-c"))
            .await
            .expect("applied");
        let error = plane.wake_workload(id).await.expect_err("prepare fails");
        assert!(
            matches!(&error, PlaneError::WakeFailed { reason: "ArtifactPullFailed", workload_id, .. } if *workload_id == id),
            "{error}"
        );
        let view = plane.get(id).await.expect("read");
        assert_eq!(view.status.phase, WorkloadPhase::Running);
        assert_eq!(
            view.status
                .condition(ConditionKind::Ready)
                .expect("ready")
                .reason,
            "ArtifactPullFailed"
        );
        wasm.fail_prepare(false);
        plane.wake_workload(id).await.expect("woken once fixed");
    }

    #[tokio::test]
    async fn without_a_fitting_driver_the_wake_names_each_one_passed_over() {
        let Some(db) = test_db().await else { return };
        let docker = FakeDriver::new();
        let (plane, _audit) = plane(&db, NodeDriver(Some(docker)), Vec::new());
        let id = plane
            .apply(function_request("fn-d"))
            .await
            .expect("applied");
        let error = plane.wake_workload(id).await.expect_err("no wasm driver");
        let PlaneError::NoEligibleDriver { reason, .. } = &error else {
            panic!("expected NoEligibleDriver, got {error}");
        };
        assert!(reason.contains("fake: does not run WASM"), "{reason}");
        let view = plane.get(id).await.expect("read");
        assert_eq!(
            view.status
                .condition(ConditionKind::Scheduled)
                .expect("scheduled")
                .reason,
            "NoEligibleDriver"
        );
    }

    #[tokio::test]
    async fn only_scale_to_zero_workloads_are_woken() {
        let Some(db) = test_db().await else { return };
        let (plane, _audit) = plane(&db, NodeDriver(Some(FakeDriver::new())), Vec::new());
        let id = plane
            .apply(apply_request(9, "job-a", crate::testing::job_spec("app:1")))
            .await
            .expect("applied");
        let error = plane.wake_workload(id).await.expect_err("a job");
        assert!(matches!(error, PlaneError::NotWakeable { workload_id, .. } if workload_id == id));
        assert!(matches!(
            plane.wake_workload(i64::MAX).await,
            Err(PlaneError::NotFound { .. })
        ));
    }

    #[tokio::test]
    async fn deleting_a_woken_function_removes_its_instance() {
        let Some(db) = test_db().await else { return };
        let wasm = FakeDriver::wasm();
        let (plane, _audit) = plane(&db, NodeDriver(None), Vec::new());
        let plane = Arc::new(
            Arc::try_unwrap(plane)
                .expect("sole owner")
                .with_local_driver(wasm.clone()),
        );
        let id = plane
            .apply(function_request("fn-e"))
            .await
            .expect("applied");
        plane.wake_workload(id).await.expect("woken");
        let name = wasm.running()[0].clone();
        plane
            .delete(id, "Deleted", "the function was deleted")
            .await
            .expect("deleted");
        plane.reconcile_at(Utc::now(), false).await;
        assert!(!wasm.exists(&name), "its instance is removed");
        assert!(matches!(
            plane.get(id).await,
            Err(PlaneError::NotFound { .. })
        ));
    }
}
