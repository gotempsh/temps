// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Cluster start/stop claims, role reconcilers and cluster retry.

use crate::services::*;
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect, Set,
    TransactionTrait,
};
use temps_entities::{external_services, service_members};
use tracing::{debug, error, info, warn};

impl ExternalServiceManager {
    /// Spawn the per-cluster Postgres role reconciler. Idempotent — if one is
    /// already running for `service_id`, returns immediately.
    /// Discover every running cluster service in the DB and spawn a role
    /// reconciler for each. Idempotent — calling multiple times leaves
    /// existing reconcilers alone (the inner `spawn_role_reconciler`
    /// guards on `reconciler_shutdowns`). Called once during plugin
    /// startup so reconcilers exist after every restart, not just for
    /// clusters created in this process's lifetime.
    pub async fn spawn_reconcilers_for_existing_clusters(&self) {
        let candidates = match external_services::Entity::find()
            .filter(external_services::Column::Topology.eq("cluster"))
            .filter(external_services::Column::Status.eq("running"))
            .filter(external_services::Column::ServiceType.eq("postgres"))
            .all(self.db.as_ref())
            .await
        {
            Ok(rows) => rows,
            Err(e) => {
                warn!(
                    error = %e,
                    "Failed to load running clusters at startup; reconcilers won't run \
                     until a member is added or the cluster is recreated"
                );
                return;
            }
        };
        if candidates.is_empty() {
            debug!("No running cluster services found at startup");
        } else {
            info!(
                count = candidates.len(),
                "Spawning role reconcilers for existing clusters"
            );
            for svc in &candidates {
                self.spawn_role_reconciler(svc.id, svc.name.clone()).await;
            }
            // Upgrade clusters created before SCRAM auth (and re-converge
            // upgraded ones). Sequential and after the reconcilers are up:
            // a slow or unreachable member delays only this loop.
            for svc in &candidates {
                if let Err(e) = self.upgrade_cluster_auth(svc.id).await {
                    warn!(
                        service_id = svc.id,
                        service_name = %svc.name,
                        error = %e,
                        "Could not upgrade cluster to SCRAM auth for its infrastructure \
                         roles; it keeps its previous authentication until the next start"
                    );
                }
            }
        }

        // Run the stuck-row watchdog after the reconcilers come up so
        // failed/stuck members appear as `failed` immediately to the
        // UI, instead of hanging in `creating` forever.
        self.fail_abandoned_provisioning_rows().await;
    }

    /// One-shot scan at startup: any `service_members` row whose
    /// `provisioning_step` is in flight AND whose `updated_at` is
    /// older than `STUCK_ROW_THRESHOLD` is marked `failed`. This
    /// happens when the control plane was killed mid-`add_cluster_member`
    /// — without this, the row would stay at `INSERTING_ROW` /
    /// `PROVISIONING_CONTAINER` forever and the operator would have
    /// no way to clean it up except hand-editing the DB.
    ///
    /// 15 minutes is generous: a cold-cache image pull on a slow
    /// connection can take 5+ minutes; doubling that as a timeout
    /// avoids killing legitimately slow provisions on flaky networks.
    async fn fail_abandoned_provisioning_rows(&self) {
        const STUCK_ROW_THRESHOLD: chrono::Duration = chrono::Duration::minutes(15);

        let cutoff = Utc::now() - STUCK_ROW_THRESHOLD;
        let in_flight = [
            member_provisioning_step::INSERTING_ROW,
            member_provisioning_step::PROVISIONING_CONTAINER,
            member_provisioning_step::REGISTERING_DNS,
        ];

        let stuck = match service_members::Entity::find()
            .filter(service_members::Column::Status.eq("creating"))
            .filter(service_members::Column::ProvisioningStep.is_in(in_flight))
            .filter(service_members::Column::UpdatedAt.lt(cutoff))
            .all(self.db.as_ref())
            .await
        {
            Ok(rows) => rows,
            Err(e) => {
                warn!(
                    error = %e,
                    "Failed to scan for stuck cluster member rows at startup; \
                     any half-provisioned members from a previous run will stay \
                     in 'creating' until manually fixed"
                );
                return;
            }
        };
        if stuck.is_empty() {
            return;
        }

        warn!(
            count = stuck.len(),
            threshold_minutes = STUCK_ROW_THRESHOLD.num_minutes(),
            "Found cluster member rows stuck mid-provisioning across a control \
             plane restart; marking them failed so the operator can retry"
        );
        for m in stuck {
            let member_id = m.id;
            let last_step = m.provisioning_step.clone().unwrap_or_default();
            let mut active: service_members::ActiveModel = m.into();
            active.status = Set("failed".to_string());
            active.provisioning_step = Set(Some(member_provisioning_step::FAILED.to_string()));
            active.provisioning_error = Set(Some(format!(
                "Control plane restart abandoned this provisioning attempt at step '{}'. \
                 No data was lost; click Add Replica again to retry.",
                last_step
            )));
            active.updated_at = Set(Utc::now());
            if let Err(e) = active.update(self.db.as_ref()).await {
                warn!(
                    member_id,
                    error = %e,
                    "Failed to mark abandoned member as failed; will retry next startup"
                );
            }
        }
    }

    pub(super) async fn spawn_role_reconciler(&self, service_id: i32, service_name: String) {
        let registry = self.dns_registry.clone();

        let mut shutdowns = self.reconciler_shutdowns.lock().await;
        if shutdowns.contains_key(&service_id) {
            debug!(service_id, "role reconciler already running");
            return;
        }
        let shutdown = crate::externalsvc::postgres_role_reconciler::ReconcilerShutdown::new();
        shutdowns.insert(service_id, shutdown.clone());
        drop(shutdowns);

        let db = self.db.clone();
        let encryption = self.encryption_service.clone();
        // Supervised loop: a panic inside `run` (e.g. unexpected enum
        // value from a future pg_auto_failover release that breaks
        // `query_monitor`) used to silently kill DNS sync for one
        // cluster forever. Now we re-spawn after a 30s backoff. Bounded
        // restart rate (max 6 panics per hour) so a deterministic crash
        // doesn't become an infinite restart loop hammering the
        // monitor.
        const RESTART_BACKOFF: std::time::Duration = std::time::Duration::from_secs(30);
        const RESTART_WINDOW: std::time::Duration = std::time::Duration::from_secs(3600);
        const MAX_RESTARTS_PER_WINDOW: usize = 6;

        tokio::spawn(async move {
            let mut crash_times: Vec<std::time::Instant> = Vec::new();
            loop {
                let task_db = db.clone();
                let task_encryption = encryption.clone();
                let task_registry = registry.clone();
                let task_name = service_name.clone();
                let task_shutdown = shutdown.clone();
                // Wrap the future in AssertUnwindSafe + catch_unwind so
                // a panic in the reconciler returns Err instead of
                // killing this supervisor task.
                use futures::future::FutureExt;
                let result = std::panic::AssertUnwindSafe(
                    crate::externalsvc::postgres_role_reconciler::run(
                        task_db,
                        task_encryption,
                        task_registry,
                        service_id,
                        task_name,
                        task_shutdown,
                    ),
                )
                .catch_unwind()
                .await;

                match result {
                    Ok(()) => {
                        // Clean exit (shutdown was notified). Don't restart.
                        debug!(service_id, "role reconciler exited cleanly");
                        return;
                    }
                    Err(panic) => {
                        let now = std::time::Instant::now();
                        crash_times.retain(|t| now.duration_since(*t) < RESTART_WINDOW);
                        crash_times.push(now);

                        let panic_msg = panic
                            .downcast_ref::<&'static str>()
                            .map(|s| s.to_string())
                            .or_else(|| panic.downcast_ref::<String>().cloned())
                            .unwrap_or_else(|| "<non-string panic payload>".to_string());

                        if crash_times.len() > MAX_RESTARTS_PER_WINDOW {
                            error!(
                                service_id,
                                panic = %panic_msg,
                                crashes_in_last_hour = crash_times.len(),
                                "Role reconciler crashed too many times; giving up. \
                                 DNS records for this cluster will go stale until \
                                 the control plane is restarted."
                            );
                            return;
                        }

                        error!(
                            service_id,
                            panic = %panic_msg,
                            crashes_in_last_hour = crash_times.len(),
                            backoff_secs = RESTART_BACKOFF.as_secs(),
                            "Role reconciler panicked; restarting after backoff"
                        );
                    }
                }

                // Backoff respects shutdown so a delete_service called
                // mid-backoff doesn't have to wait the full 30s. Also
                // re-checks `is_stopped()` after waking in case the signal
                // landed just before this select armed (same race the loop
                // in `run()` guards against — see `ReconcilerShutdown`).
                if shutdown.is_stopped() {
                    debug!(
                        service_id,
                        "role reconciler shutdown during restart backoff"
                    );
                    return;
                }
                tokio::select! {
                    _ = tokio::time::sleep(RESTART_BACKOFF) => {}
                    _ = shutdown.wait() => {
                        debug!(service_id, "role reconciler shutdown during restart backoff");
                        return;
                    }
                }
            }
        });
    }

    /// Stop the per-cluster role reconciler if one is running. Called from
    /// `delete_service` after the DB tx commits — paired with
    /// `DnsRegistry::delete_by_owner` so role records get dropped after the
    /// reconciler has stopped writing them.
    pub(super) async fn stop_role_reconciler(&self, service_id: i32) {
        let mut shutdowns = self.reconciler_shutdowns.lock().await;
        if let Some(notifier) = shutdowns.remove(&service_id) {
            notifier.signal();
            debug!(service_id, "role reconciler shutdown signalled");
        }
    }

    /// The checks and the switch to `creating` that [`Self::retry_cluster`]
    /// makes under the cluster's row lock, in `lock`. Returns the leftover
    /// member rows and the member list to rebuild. The caller commits on
    /// success and rolls back on error.
    async fn claim_cluster_retry(
        &self,
        lock: &sea_orm::DatabaseTransaction,
        service_id: i32,
        member_requests: &[ClusterMemberRequest],
    ) -> Result<(Vec<service_members::Model>, Vec<ClusterMemberRequest>), ExternalServiceError>
    {
        let service = external_services::Entity::find_by_id(service_id)
            .lock_exclusive()
            .one(lock)
            .await?
            .ok_or(ExternalServiceError::ServiceNotFound { id: service_id })?;

        if service.topology != "cluster" {
            return Err(ExternalServiceError::ParameterValidationFailed {
                service_id,
                reason: "retry_cluster is only valid for cluster topology services".to_string(),
            });
        }

        if service.status != "failed" && service.status != "creating" {
            return Err(ExternalServiceError::ParameterValidationFailed {
                service_id,
                reason: format!(
                    "Service must be in 'failed' or 'creating' status to retry, current status: '{}'",
                    service.status
                ),
            });
        }

        info!(
            "Retrying cluster initialization for service {} (current status: {})",
            service_id, service.status
        );

        // Clean up any leftover service_members and their containers
        let leftover_members = service_members::Entity::find()
            .filter(service_members::Column::ServiceId.eq(service_id))
            .order_by_asc(service_members::Column::Ordinal)
            .all(lock)
            .await?;

        // Reconstruct member specs from preserved records if none were provided
        let effective_members: Vec<ClusterMemberRequest> = if !member_requests.is_empty() {
            resolve_cluster_members(&service.name, member_requests)?
        } else if leftover_members.is_empty() {
            // Nothing to rebuild from: the cluster failed before any member
            // was recorded, which is what a create without a member list used
            // to do. Give it the default layout rather than asking the caller
            // to guess the format.
            info!(
                "No member records for failed cluster {}; retrying with the default layout",
                service_id
            );
            default_cluster_members()
        } else {
            info!(
                "Reconstructing member config from {} preserved records for service {}",
                leftover_members.len(),
                service_id
            );
            leftover_members
                .iter()
                .map(|m| ClusterMemberRequest {
                    role: m.role.clone(),
                    node_id: m.node_id,
                })
                .collect()
        };
        // Before tearing anything down: a member list that cannot fit would
        // only fail again after the leftover members were removed.
        validate_cluster_member_count(&service.name, effective_members.len())?;

        // Update status to "creating" and clear previous error
        let mut service_update: external_services::ActiveModel = service.into();
        service_update.status = Set("creating".to_string());
        service_update.error_message = Set(None);
        service_update.updated_at = Set(Utc::now());
        service_update.update(lock).await?;
        Ok((leftover_members, effective_members))
    }

    /// Retry a failed cluster service initialization.
    ///
    /// Cleans up any leftover containers and service_members from the previous
    /// attempt, then re-runs `initialize_cluster`.
    ///
    /// If `member_requests` is empty, the original member configuration is
    /// reconstructed from the preserved `service_members` records (which are
    /// now kept with "failed" status instead of being deleted on rollback).
    pub async fn retry_cluster(
        &self,
        service_id: i32,
        member_requests: &[ClusterMemberRequest],
    ) -> Result<ExternalServiceInfo, ExternalServiceError> {
        // Checked and switched to `creating` under the row lock, in one short
        // transaction committed before any container is removed: from then
        // on Stop, Start and Add Replica refuse the cluster, so none of them
        // acts on the members this retry tears down. See `claim_cluster`.
        let lock = self.db.begin().await?;
        let claimed = self
            .claim_cluster_retry(&lock, service_id, member_requests)
            .await;
        let (leftover_members, effective_members) = match claimed {
            Ok(claimed) => claimed,
            Err(error) => return Err(rollback_before_error(lock, error).await),
        };
        lock.commit().await?;

        for member in &leftover_members {
            // Try to remove the container (ignore errors — it may not exist)
            if let Some(node_id) = member.node_id {
                if let Ok(client) = self.get_remote_client(node_id).await {
                    if let Err(e) = client.remove_service(&member.container_name).await {
                        warn!(
                            "Retry cleanup: failed to remove remote container '{}' on node {}: {}",
                            member.container_name, node_id, e
                        );
                    }
                }
            } else if let Some(docker) = self.docker.get() {
                let _ = docker
                    .remove_container(
                        &member.container_name,
                        Some(bollard::query_parameters::RemoveContainerOptions {
                            force: true,
                            ..Default::default()
                        }),
                    )
                    .await;

                // Also remove the volume
                let volume_name = format!("{}_data", member.container_name);
                let _ = docker
                    .remove_volume(
                        &volume_name,
                        None::<bollard::query_parameters::RemoveVolumeOptions>,
                    )
                    .await;
            } else {
                debug!(
                    container_name = %member.container_name,
                    "Retry cleanup: no local Docker daemon in this process; nothing to remove locally"
                );
            }
        }

        // Delete leftover member records
        if !leftover_members.is_empty() {
            service_members::Entity::delete_many()
                .filter(service_members::Column::ServiceId.eq(service_id))
                .exec(self.db.as_ref())
                .await?;
            info!(
                "Retry cleanup: removed {} leftover member records for service {}",
                leftover_members.len(),
                service_id
            );
        }

        // Spawn background task to re-initialize (same pattern as create).
        // `self.clone()`, not `ExternalServiceManager::new(...)` -- see the
        // struct's doc comment: a fresh instance would allocate its own empty
        // `reconciler_shutdowns` map, orphaning any reconciler this retry
        // spawns from `stop_role_reconciler` on the real, shared manager.
        let manager = self.clone();
        let db = self.db.clone();
        let members = effective_members;

        tokio::spawn(async move {
            let result = manager.initialize_cluster(service_id, &members).await;

            match result {
                Ok(()) => {
                    info!(
                        "Cluster service {} retry succeeded (background)",
                        service_id
                    );
                }
                Err(e) => {
                    error!(
                        "Cluster service {} retry failed (background): {}",
                        service_id, e
                    );

                    let update_result: Result<_, sea_orm::DbErr> = async {
                        let mut svc: external_services::ActiveModel =
                            external_services::Entity::find_by_id(service_id)
                                .one(db.as_ref())
                                .await?
                                .ok_or(sea_orm::DbErr::RecordNotFound(
                                    "Service not found during retry rollback".to_string(),
                                ))?
                                .into();
                        svc.status = Set("failed".to_string());
                        svc.error_message = Set(Some(e.to_string()));
                        svc.updated_at = Set(Utc::now());
                        svc.update(db.as_ref()).await?;
                        Ok(())
                    }
                    .await;

                    if let Err(db_err) = update_result {
                        error!(
                            "Failed to update service {} status to 'failed' after retry: {}",
                            service_id, db_err
                        );
                    }
                }
            }
        });

        self.get_service_info(service_id).await
    }

    /// Stop or start every member of cluster `service_id`, in
    /// [`cluster_lifecycle_order`], and record the cluster's new status.
    /// Volumes are never touched.
    ///
    /// The cluster is first claimed (`stopping`/`starting`, see
    /// [`Self::claim_cluster`]); no transaction or connection is held while
    /// containers change. Each member's row is written on its own as soon as
    /// that member changed, so a failed write cannot undo the others; it
    /// counts as that member's failure, which halts a Stop like any other.
    ///
    /// Stopping halts at the first member that cannot be stopped: carrying on
    /// would stop the primary while a standby still runs, which is exactly
    /// the failover the order exists to avoid. Starting attempts every
    /// member. Either way the error names each failed member and what was
    /// and was not done. A member whose provisioning failed keeps its
    /// `failed` status, so the next Start still skips it.
    pub(super) async fn run_cluster_lifecycle(
        &self,
        service_id: i32,
        action: ClusterLifecycleAction,
    ) -> Result<(), ExternalServiceError> {
        let service = self.claim_cluster(service_id, action).await?;
        let outcome = self.drive_cluster_members(&service, action).await;

        let previous = cluster_status_before_claim(&service.status);
        let (status, error_message) = match &outcome {
            // A cluster whose provisioning failed stays `failed` after a
            // Stop, so Retry remains available.
            Ok(report) if report.failures.is_empty() && service.status == "failed" => {
                ("failed", service.error_message.clone())
            }
            Ok(report) if report.failures.is_empty() => (action.member_status(), None),
            Ok(report) => (
                previous,
                Some(cluster_lifecycle_failure(&service, action, report).to_string()),
            ),
            // Refused, or the members could not be read: nothing changed.
            Err(_) => (previous, service.error_message.clone()),
        };

        // The role reconciler keeps the cluster's primary DNS record
        // pointed at the live primary. It is only started for running
        // clusters at boot, so a cluster started here needs its own.
        // Changed while the claim is held, so a Stop finishing late cannot
        // shut down the reconciler of a Start that ran after it.
        if let Ok(report) = &outcome {
            match action {
                ClusterLifecycleAction::Start if !report.done.is_empty() => {
                    self.spawn_role_reconciler(service_id, service.name.clone())
                        .await;
                }
                ClusterLifecycleAction::Stop if report.failures.is_empty() => {
                    self.stop_role_reconciler(service_id).await;
                }
                _ => {}
            }
        }
        self.release_cluster_claim(service_id, action, status, error_message)
            .await?;

        let report = outcome?;
        if report.failures.is_empty() {
            Ok(())
        } else {
            Err(cluster_lifecycle_failure(&service, action, &report))
        }
    }

    /// Claim cluster `service_id` for `action`: under the row lock, check
    /// [`cluster_claim_decision`] and move it to `stopping`/`starting`.
    /// Returns the cluster as it was before the claim.
    ///
    /// The row lock lives only for this check and write. Add Replica and
    /// Retry check the status under the same lock, so once the claim
    /// commits neither can change the member set a Stop/Start is acting
    /// on, and a second Stop/Start is refused as busy.
    async fn claim_cluster(
        &self,
        service_id: i32,
        action: ClusterLifecycleAction,
    ) -> Result<external_services::Model, ExternalServiceError> {
        let txn = self.db.begin().await?;
        let claimed = async {
            let service = external_services::Entity::find_by_id(service_id)
                .lock_exclusive()
                .one(&txn)
                .await?
                .ok_or(ExternalServiceError::ServiceNotFound { id: service_id })?;
            cluster_claim_decision(
                service_id,
                &service.status,
                service.updated_at,
                Utc::now(),
                action,
            )?;
            external_services::ActiveModel {
                id: Set(service_id),
                status: Set(action.claim_status().to_string()),
                updated_at: Set(Utc::now()),
                ..Default::default()
            }
            .update(&txn)
            .await?;
            Ok(service)
        }
        .await;
        let service = match claimed {
            Ok(service) => service,
            Err(error) => return Err(rollback_before_error(txn, error).await),
        };
        txn.commit().await?;
        Ok(service)
    }

    /// Refresh this action's claim so it is not mistaken for one whose
    /// process died (see [`CLUSTER_CLAIM_STALE_AFTER`]).
    async fn refresh_cluster_claim(&self, service_id: i32, action: ClusterLifecycleAction) {
        if let Err(e) = external_services::Entity::update_many()
            .col_expr(
                external_services::Column::UpdatedAt,
                sea_orm::sea_query::Expr::value(Utc::now()),
            )
            .filter(external_services::Column::Id.eq(service_id))
            .filter(external_services::Column::Status.eq(action.claim_status()))
            .exec(self.db.as_ref())
            .await
        {
            warn!(
                service_id,
                error = %e,
                "Could not refresh the cluster's {} claim", action.verb()
            );
        }
    }

    /// End this action's claim with the cluster's resulting status. Only a
    /// claim this action still holds is replaced.
    async fn release_cluster_claim(
        &self,
        service_id: i32,
        action: ClusterLifecycleAction,
        status: &str,
        error_message: Option<String>,
    ) -> Result<(), ExternalServiceError> {
        external_services::Entity::update_many()
            .col_expr(
                external_services::Column::Status,
                sea_orm::sea_query::Expr::value(status),
            )
            .col_expr(
                external_services::Column::ErrorMessage,
                sea_orm::sea_query::Expr::value(error_message),
            )
            .col_expr(
                external_services::Column::UpdatedAt,
                sea_orm::sea_query::Expr::value(Utc::now()),
            )
            .filter(external_services::Column::Id.eq(service_id))
            .filter(external_services::Column::Status.eq(action.claim_status()))
            .exec(self.db.as_ref())
            .await
            .map_err(|e| {
                error!(
                    service_id,
                    error = %e,
                    "Cluster {} finished on its members but its status stayed '{}'; \
                     another Stop or Start may take it over after {} minutes",
                    action.verb(),
                    action.claim_status(),
                    CLUSTER_CLAIM_STALE_AFTER.num_minutes()
                );
                ExternalServiceError::from(e)
            })?;
        Ok(())
    }

    /// Act on the members of a claimed cluster. `Err` only when nothing
    /// was done: the members could not be read, or one is still being
    /// provisioned.
    async fn drive_cluster_members(
        &self,
        service: &external_services::Model,
        action: ClusterLifecycleAction,
    ) -> Result<ClusterLifecycleReport, ExternalServiceError> {
        let service_id = service.id;
        let mut members = service_members::Entity::find()
            .filter(service_members::Column::ServiceId.eq(service_id))
            .order_by_asc(service_members::Column::Ordinal)
            .all(self.db.as_ref())
            .await?;
        if service.status == "failed" {
            let abandoned = settle_members_of_failed_cluster(&mut members);
            if !abandoned.is_empty() {
                // Saved before any container changes, so the API stops
                // reporting these members as still being provisioned.
                service_members::Entity::update_many()
                    .col_expr(
                        service_members::Column::Status,
                        sea_orm::sea_query::Expr::value("failed"),
                    )
                    .col_expr(
                        service_members::Column::ProvisioningStep,
                        sea_orm::sea_query::Expr::value(Some(
                            member_provisioning_step::FAILED.to_string(),
                        )),
                    )
                    .col_expr(
                        service_members::Column::UpdatedAt,
                        sea_orm::sea_query::Expr::value(Utc::now()),
                    )
                    .filter(service_members::Column::Id.is_in(abandoned))
                    .filter(service_members::Column::Status.is_in(["pending", "creating"]))
                    .exec(self.db.as_ref())
                    .await?;
            }
        }
        if let Some(busy) = members
            .iter()
            .find(|m| !cluster_member_is_settled(&m.status))
        {
            return Err(ExternalServiceError::ClusterMemberProvisioning {
                service_id,
                action: action.verb(),
                container_name: busy.container_name.clone(),
                status: busy.status.clone(),
            });
        }
        let live_primary = match action {
            ClusterLifecycleAction::Stop => self
                .find_live_primary_member(service, &members)
                .await
                .ok()
                .flatten()
                .map(|member| member.container_name.clone()),
            ClusterLifecycleAction::Start => None,
        };

        let mut report = ClusterLifecycleReport::default();
        for member in cluster_lifecycle_order(&members, live_primary.as_deref(), action) {
            if action == ClusterLifecycleAction::Stop && !report.failures.is_empty() {
                report.not_attempted.push(member.container_name.clone());
                continue;
            }
            let result = match self.set_cluster_member_running(member, action).await {
                Ok(()) => match recorded_member_status(member, action) {
                    Some(status) => (service_members::ActiveModel {
                        id: Set(member.id),
                        status: Set(status.to_string()),
                        updated_at: Set(Utc::now()),
                        ..Default::default()
                    })
                    .update(self.db.as_ref())
                    .await
                    .map(|_| ())
                    .map_err(|e| {
                        format!(
                            "its container is now {} but its row could not be updated: {}",
                            action.member_status(),
                            e
                        )
                    }),
                    None => Ok(()),
                },
                Err(reason) => Err(reason),
            };
            match result {
                Ok(()) => {
                    info!(
                        service_id,
                        member = %member.container_name,
                        role = %member.role,
                        "Cluster member is now {}",
                        action.member_status()
                    );
                    report.done.push(member.container_name.clone());
                }
                Err(reason) => {
                    error!(
                        service_id,
                        member = %member.container_name,
                        "Failed to {} cluster member: {}",
                        action.verb(),
                        reason
                    );
                    report
                        .failures
                        .push(format!("'{}' ({})", member.container_name, reason));
                }
            }
            self.refresh_cluster_claim(service_id, action).await;
        }
        Ok(report)
    }

    /// Stop or start one cluster member's container, locally or through
    /// its node's agent. A container already in the wanted state is
    /// success; so is a missing container of a member whose provisioning
    /// failed, which has nothing to stop.
    async fn set_cluster_member_running(
        &self,
        member: &service_members::Model,
        action: ClusterLifecycleAction,
    ) -> Result<(), String> {
        if let Some(node_id) = member.node_id {
            let client = self
                .get_remote_client(node_id)
                .await
                .map_err(|e| format!("cannot reach node {node_id}: {e}"))?;
            let observe = || async {
                client
                    .service_status(&member.container_name)
                    .await
                    .map_err(|e| format!("on node {node_id}: cannot read its state: {e}"))
            };
            if let Some(outcome) = remote_member_outcome(member, action, &observe().await?) {
                return outcome;
            }
            let result = match action {
                ClusterLifecycleAction::Stop => client.stop_service(&member.container_name).await,
                ClusterLifecycleAction::Start => client.start_service(&member.container_name).await,
            };
            return match result {
                Ok(()) => Ok(()),
                // The container may have reached the wanted state anyway
                // (it was stopped or started concurrently): judge by state.
                Err(e) => match remote_member_outcome(member, action, &observe().await?) {
                    Some(Ok(())) => Ok(()),
                    _ => Err(format!("on node {node_id}: {e}")),
                },
            };
        }

        let docker = self.docker.get().ok_or_else(|| {
            "this process has no local Docker daemon to manage the member".to_string()
        })?;
        let result = match action {
            ClusterLifecycleAction::Stop => {
                docker
                    .stop_container(
                        &member.container_name,
                        None::<bollard::query_parameters::StopContainerOptions>,
                    )
                    .await
            }
            ClusterLifecycleAction::Start => {
                docker
                    .start_container(
                        &member.container_name,
                        None::<bollard::query_parameters::StartContainerOptions>,
                    )
                    .await
            }
        };
        match result {
            Ok(()) => Ok(()),
            // 304: already stopped / already running.
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 304, ..
            }) => Ok(()),
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) if action == ClusterLifecycleAction::Stop && member.status == "failed" => Ok(()),
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => Err(format!(
                "its container '{}' does not exist on this host",
                member.container_name
            )),
            Err(e) => Err(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::test_support::*;
    use chrono::Utc;
    #[cfg(feature = "docker-tests")]
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, TransactionTrait};
    use std::sync::Arc;
    use temps_entities::{external_services, service_members};

    /// Regression for the `ExternalServiceManager::Clone` fix: background
    /// tasks (create_service's cluster-init task and its retry path) must
    /// `self.clone()` rather than `::new(...)` so a role reconciler they
    /// spawn registers its shutdown handle where `stop_role_reconciler` --
    /// called on the real, shared manager -- can actually find it.
    /// `::new(...)` would silently allocate a fresh, empty
    /// `reconciler_shutdowns` map, reintroducing the leak this PR fixed.
    #[tokio::test]
    async fn clone_shares_reconciler_shutdowns_with_original() {
        let manager = mock_service_manager(vec![]);
        let cloned = manager.clone();

        let shutdown = crate::externalsvc::postgres_role_reconciler::ReconcilerShutdown::new();
        manager
            .reconciler_shutdowns
            .lock()
            .await
            .insert(99, shutdown.clone());

        assert!(
            cloned.reconciler_shutdowns.lock().await.contains_key(&99),
            "Clone must share the same reconciler_shutdowns map as the \
             original, not construct a fresh empty one -- otherwise \
             stop_role_reconciler on the original can never see a handle \
             registered through the clone, and the reconciler leaks forever"
        );

        // And the sharing is bidirectional / live, not a one-shot copy at
        // clone time: something inserted through the clone must also be
        // visible on the original.
        cloned.reconciler_shutdowns.lock().await.insert(
            100,
            crate::externalsvc::postgres_role_reconciler::ReconcilerShutdown::new(),
        );
        assert!(manager.reconciler_shutdowns.lock().await.contains_key(&100));
    }

    /// Run a cluster Stop or Start against a mock database holding a
    /// cluster with `parent_status`, last changed `age` ago. With `members`
    /// the claim is expected to succeed and the members to be refused;
    /// without, the claim itself must be refused. Returns the error and the
    /// statements issued.
    async fn refused_cluster_lifecycle(
        action: &str,
        parent_status: &str,
        age: chrono::Duration,
        members: Option<Vec<service_members::Model>>,
    ) -> (ExternalServiceError, Vec<sea_orm::Statement>) {
        let mut cluster = encrypted_service_model(7, serde_json::json!({}));
        cluster.topology = "cluster".to_string();
        cluster.status = parent_status.to_string();
        cluster.updated_at = Utc::now() - age;
        let mut db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres);
        if action == "start" {
            // `start_service` first checks for an active major upgrade.
            db = db.append_query_results([
                Vec::<temps_entities::postgres_major_upgrades::Model>::new(),
            ]);
        }
        // `get_service`, the duplicate-name check (no other service has this
        // name), then the claim's locked read.
        db = db
            .append_query_results([vec![cluster.clone()]])
            .append_query_results([Vec::<external_services::Model>::new()])
            .append_query_results([vec![cluster.clone()]]);
        if let Some(members) = members {
            let mut claimed = cluster.clone();
            claimed.status = format!("{action}ping").replace("startping", "starting");
            db = db
                .append_query_results([vec![claimed]])
                .append_query_results([members])
                .append_exec_results([sea_orm::MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 1,
                }]);
        }
        let db = Arc::new(db.into_connection());
        let manager = mock_service_manager_with_db(db.clone());
        let error = if action == "stop" {
            manager.stop_service(7).await.unwrap_err()
        } else {
            manager.start_service(7).await.unwrap_err()
        };
        drop(manager);
        let statements = Arc::try_unwrap(db)
            .expect("the manager holds no other database handle")
            .into_transaction_log()
            .iter()
            .flat_map(|transaction| transaction.statements().to_vec())
            .collect();
        (error, statements)
    }

    /// Regression for #1353: Stop/Start of a cluster never targets the
    /// standalone container name, and refuses with a 409-class error while a
    /// member is still being provisioned (`creating`, or `pending` before
    /// its provisioning started) instead of a Docker 404. The members are
    /// read only after the cluster is claimed, and the claim is released
    /// back to the cluster's previous status.
    #[tokio::test]
    async fn cluster_stop_and_start_refuse_while_a_member_is_provisioning() {
        for action in ["stop", "start"] {
            for busy_status in ["creating", "pending"] {
                let previous = if action == "stop" {
                    "running"
                } else {
                    "stopped"
                };
                let (error, statements) = refused_cluster_lifecycle(
                    action,
                    previous,
                    chrono::Duration::zero(),
                    Some(vec![
                        cluster_member(0, "monitor", "running"),
                        cluster_member(1, "primary", "running"),
                        cluster_member(2, "replica", busy_status),
                    ]),
                )
                .await;
                assert!(
                    matches!(
                        &error,
                        ExternalServiceError::ClusterMemberProvisioning {
                            service_id: 7,
                            container_name,
                            status,
                            ..
                        } if container_name == "postgres-ha-2" && status == busy_status
                    ),
                    "{action}/{busy_status}: {error:?}"
                );
                assert!(error.to_string().contains(action), "{error}");
                assert!(!error.to_string().contains("No such container"));

                let position = |needle: &str| {
                    statements
                        .iter()
                        .position(|s| s.sql.contains(needle))
                        .unwrap_or_else(|| panic!("{action}: no statement with {needle}"))
                };
                assert!(position("FOR UPDATE") < position("service_members"));
                let release = statements.last().expect("the claim must be released");
                assert!(release.sql.starts_with("UPDATE"), "{}", release.sql);
                let values = format!("{:?}", release.values);
                assert!(values.contains(&format!("\"{previous}\"")), "{values}");
                let claim = if action == "stop" {
                    "stopping"
                } else {
                    "starting"
                };
                assert!(values.contains(&format!("\"{claim}\"")), "{values}");
            }
        }
    }

    /// Greptile on #1366: a cluster still in its initial provisioning has
    /// no member rows yet; Stop must not report success and mark it stopped
    /// while the background job goes on to start it. A cluster whose
    /// provisioning failed is not started either (Retry provisions it), and
    /// one another Stop/Start holds is refused as busy. None of them is
    /// claimed or read further.
    #[tokio::test]
    async fn cluster_lifecycle_refuses_a_cluster_it_cannot_claim() {
        for (action, parent_status) in [
            ("stop", "creating"),
            ("start", "creating"),
            ("start", "failed"),
            ("stop", "starting"),
            ("start", "stopping"),
        ] {
            let (error, statements) = refused_cluster_lifecycle(
                action,
                parent_status,
                chrono::Duration::minutes(1),
                None,
            )
            .await;
            match parent_status {
                "stopping" | "starting" => assert!(
                    matches!(
                        &error,
                        ExternalServiceError::ClusterBusy { service_id: 7, .. }
                    ),
                    "{action}/{parent_status}: {error:?}"
                ),
                _ => assert!(
                    matches!(
                        &error,
                        ExternalServiceError::ClusterNotSettled { service_id: 7, status, .. }
                            if status == parent_status
                    ),
                    "{action}/{parent_status}: {error:?}"
                ),
            }
            assert!(error.to_string().contains(action), "{error}");
            assert!(statements.iter().any(|s| s.sql.contains("FOR UPDATE")));
            assert!(statements
                .iter()
                .all(|s| !s.sql.contains("service_members")));
            assert!(statements.iter().all(|s| !s.sql.starts_with("UPDATE")));
        }
    }

    /// Greptile on #1366: a Stop/Start claims the cluster in a short
    /// committed write, so no connection is held while containers change; a
    /// second action is refused as busy, Add Replica's locked status check
    /// refuses a claimed cluster, and releasing the claim records the
    /// result only while this action still holds it.
    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn cluster_claim_excludes_other_actions_until_released() {
        let (manager, _test_db) = setup_test_manager_or_skip!();
        let service_id =
            insert_cluster_at_auth_version(&manager, "claim-test", None, &[("monitor", "running")])
                .await;

        let before = manager
            .claim_cluster(service_id, ClusterLifecycleAction::Stop)
            .await
            .unwrap();
        assert_eq!(before.status, "running");
        assert_eq!(
            manager.get_service(service_id).await.unwrap().status,
            "stopping"
        );

        let busy = manager
            .claim_cluster(service_id, ClusterLifecycleAction::Start)
            .await
            .unwrap_err();
        assert!(
            matches!(busy, ExternalServiceError::ClusterBusy { .. }),
            "{busy:?}"
        );

        // A release by an action that no longer holds the claim changes nothing.
        manager
            .release_cluster_claim(service_id, ClusterLifecycleAction::Start, "running", None)
            .await
            .unwrap();
        assert_eq!(
            manager.get_service(service_id).await.unwrap().status,
            "stopping"
        );

        manager
            .release_cluster_claim(service_id, ClusterLifecycleAction::Stop, "stopped", None)
            .await
            .unwrap();
        assert_eq!(
            manager.get_service(service_id).await.unwrap().status,
            "stopped"
        );

        // A claim whose process died is taken over once stale.
        manager
            .claim_cluster(service_id, ClusterLifecycleAction::Start)
            .await
            .unwrap();
        external_services::Entity::update_many()
            .col_expr(
                external_services::Column::UpdatedAt,
                sea_orm::sea_query::Expr::value(
                    Utc::now() - CLUSTER_CLAIM_STALE_AFTER - chrono::Duration::minutes(1),
                ),
            )
            .filter(external_services::Column::Id.eq(service_id))
            .exec(manager.db.as_ref())
            .await
            .unwrap();
        let taken_over = manager
            .claim_cluster(service_id, ClusterLifecycleAction::Stop)
            .await
            .unwrap();
        assert_eq!(taken_over.status, "starting");
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_retry_cluster_not_found() {
        let (manager, _test_db) = setup_test_manager_or_skip!();

        let result = manager.retry_cluster(99999, &[]).await;

        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ExternalServiceError::ServiceNotFound { id: 99999 }
        ));
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_retry_cluster_standalone_rejected() {
        let (manager, _test_db) = setup_test_manager_or_skip!();

        let service_id = insert_test_service(
            manager.db.as_ref(),
            "test-standalone-retry",
            "postgres",
            "standalone",
            "failed",
        )
        .await;

        let result = manager.retry_cluster(service_id, &[]).await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, ExternalServiceError::ParameterValidationFailed { .. }),
            "Expected ParameterValidationFailed for standalone topology, got: {:?}",
            err
        );
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_retry_cluster_wrong_status() {
        let (manager, _test_db) = setup_test_manager_or_skip!();

        let service_id = insert_test_service(
            manager.db.as_ref(),
            "test-running-retry",
            "postgres",
            "cluster",
            "running",
        )
        .await;

        let result = manager.retry_cluster(service_id, &[]).await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, ExternalServiceError::ParameterValidationFailed { .. }),
            "Expected ParameterValidationFailed for running status, got: {:?}",
            err
        );
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_retry_cluster_no_members() {
        let (manager, _test_db) = setup_test_manager_or_skip!();

        let service_id = insert_test_service(
            manager.db.as_ref(),
            "test-no-members-retry",
            "postgres",
            "cluster",
            "failed",
        )
        .await;

        // Empty member request + no preserved members in DB: the retry falls
        // back to the default layout instead of asking for a member list.
        // Claimed inside a transaction that is rolled back, so no cluster is
        // actually rebuilt.
        let lock = manager.db.begin().await.expect("begin transaction");
        let (leftover, members) = manager
            .claim_cluster_retry(&lock, service_id, &[])
            .await
            .expect("a failed cluster with no members retries with the default layout");
        lock.rollback().await.expect("rollback transaction");

        assert!(leftover.is_empty());
        let roles: Vec<_> = members
            .iter()
            .map(|m| (m.role.as_str(), m.node_id))
            .collect();
        assert_eq!(
            roles,
            vec![("monitor", None), ("primary", None), ("replica", None)]
        );
    }

    // ── add_cluster_member validation ──────────────────────────────────
}
