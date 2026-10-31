// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Readiness gates and readiness watches for starting services.

use crate::externalsvc::{ExternalService, ServiceConfig, ServiceType};
use crate::readiness::{
    self, AttemptStatus, DbReadinessSink, ReadinessAttempt, ReadinessPhase, ReadinessTarget,
    ServiceReadiness, WatchRegistration,
};
use crate::services::*;
use chrono::Utc;
use sea_orm::Set;
use temps_entities::external_services;
use tracing::{debug, info, warn};

impl ExternalServiceManager {
    /// The readiness gate a just-started service must pass before it is
    /// `running`, if its engine has one. Only local standalone services are
    /// gated: a remote node's published port is not reachable from here, and
    /// clusters track their members separately.
    fn readiness_gate(
        service: &external_services::Model,
        instance: &dyn ExternalService,
        config: &ServiceConfig,
    ) -> Option<Box<dyn ReadinessTarget>> {
        if service.node_id.is_some() || service.topology != "standalone" {
            return None;
        }
        instance.readiness_target(config)
    }

    /// Set the post-start lifecycle status on `update`: `running`, or
    /// `starting` with a fresh readiness snapshot when `gate` must confirm
    /// it first. Returns the snapshot and the id of this start attempt, to
    /// hand to [`Self::spawn_readiness_watch`] once the row is written.
    pub(super) fn apply_started_status(
        service: &external_services::Model,
        update: &mut external_services::ActiveModel,
        gate: Option<&dyn ReadinessTarget>,
    ) -> Option<(ServiceReadiness, ReadinessAttempt)> {
        let Some(gate) = gate else {
            update.status = Set("running".to_string());
            return None;
        };
        let readiness = ServiceReadiness::starting(Utc::now(), &gate.policy());
        let attempt = ReadinessAttempt::new();
        update.status = Set(readiness::STARTING_STATUS.to_string());
        update.error_message = Set(None);
        update.health_metadata = Set(readiness::with_readiness_metadata(
            service.health_metadata.as_ref(),
            Some((&readiness, &attempt)),
        ));
        Some((readiness, attempt))
    }

    /// Watch a `starting` service in the background until its readiness
    /// gate promotes it to `running` or reports why it failed. A fresh start
    /// (`supersede`) takes over from any older watch on the same service.
    /// The watch only ever writes for `attempt`; see [`ReadinessAttempt`].
    pub(super) fn spawn_readiness_watch(
        &self,
        service_id: i32,
        target: Box<dyn ReadinessTarget>,
        (readiness, attempt): (ServiceReadiness, ReadinessAttempt),
        supersede: bool,
    ) {
        let Some(registration) = WatchRegistration::claim(service_id, supersede) else {
            return;
        };
        let sink = DbReadinessSink {
            db: self.db.clone(),
            service_id,
            attempt,
        };
        info!(
            "Service {} is starting; waiting up to {}s for it to serve a real request",
            service_id, readiness.deadline_secs
        );
        tokio::spawn(async move {
            let outcome = readiness::drive_readiness(target.as_ref(), &sink, readiness, || {
                !registration.is_current()
            })
            .await;
            debug!(
                "Readiness watch for service {} ended: {:?}",
                service_id, outcome
            );
        });
    }

    /// Pick a `starting` service's readiness watch back up when nothing in
    /// this process is watching it, e.g. after Temps restarted mid-start.
    /// Called by the health monitor for every `starting` row; a no-op when
    /// a watch is already running. The original start time and restart
    /// count carry over, so a restart of Temps does not extend the deadline.
    pub async fn resume_readiness_watch(&self, service: &external_services::Model) {
        if service.status != readiness::STARTING_STATUS
            || service.node_id.is_some()
            || !self.local_workloads_enabled
        {
            return;
        }
        let Some(registration) = WatchRegistration::claim(service.id, false) else {
            return;
        };
        // Released at once: `spawn_readiness_watch` claims it again. This
        // only answered "is anybody watching?".
        drop(registration);

        let gate = match self.readiness_gate_for(service).await {
            Ok(Some(gate)) => gate,
            Ok(None) => {
                debug!(
                    "Service {} is '{}' but its engine has no readiness gate to resume",
                    service.id, service.status
                );
                return;
            }
            Err(e) => {
                warn!(
                    "Could not resume the readiness watch for service {} ('{}'): {}",
                    service.id, service.name, e
                );
                return;
            }
        };
        let stored_attempt =
            ReadinessAttempt::from_health_metadata(service.health_metadata.as_ref());
        let stored = ServiceReadiness::from_health_metadata(service.health_metadata.as_ref())
            .filter(|r| r.phase == ReadinessPhase::Starting);
        let resumed = match (stored, stored_attempt.clone()) {
            (Some(readiness), Some(attempt)) => (readiness, attempt),
            _ => {
                // No usable snapshot to continue: start a new attempt from
                // the row's last update, and take the row over only if it
                // is still exactly as read.
                let readiness = ServiceReadiness::starting(service.updated_at, &gate.policy());
                let sink = DbReadinessSink {
                    db: self.db.clone(),
                    service_id: service.id,
                    attempt: ReadinessAttempt::new(),
                };
                match sink.adopt(stored_attempt.as_ref(), &readiness).await {
                    AttemptStatus::Current => (readiness, sink.attempt),
                    AttemptStatus::Gone | AttemptStatus::Unavailable => return,
                }
            }
        };
        info!(
            "Resuming the readiness watch for service {} ('{}'), started at {}",
            service.id, service.name, resumed.0.started_at
        );
        self.spawn_readiness_watch(service.id, gate, resumed, false);
    }

    /// Build the readiness gate for a stored service from its parameters.
    pub(super) async fn readiness_gate_for(
        &self,
        service: &external_services::Model,
    ) -> Result<Option<Box<dyn ReadinessTarget>>, ExternalServiceError> {
        let service_type = ServiceType::from_str(&service.service_type).map_err(|_| {
            ExternalServiceError::InvalidServiceType {
                id: service.id,
                service_type: service.service_type.clone(),
            }
        })?;
        let parameters = self.get_service_parameters(service.id).await?;
        let instance = self.create_service_instance_for_parameters(
            service.name.clone(),
            service_type,
            &parameters,
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
        Ok(Self::readiness_gate(service, instance.as_ref(), &config))
    }
}
