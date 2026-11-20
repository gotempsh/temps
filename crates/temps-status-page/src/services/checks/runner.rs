// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Turning a stored target monitor into a probe, from the control plane
//! and, for a monitor checked from worker nodes, from them too.

use std::sync::Arc;

use temps_core::EncryptionService;
use temps_entities::status_monitors;
use temps_uptime_probe::RemoteProbeRequest;
use tracing::warn;

use super::error::TargetMonitorError;
use super::locations::{combine, MonitorLocationResult, MonitorLocationResults, ProbeLocations};
use super::probe::{ProbeOutcome, TargetProber};
use super::spec::{parse_target, CheckRequest, MonitorCheckKind, StoredCheck, Target};

/// A target monitor's check, decoded from its row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedCheck {
    pub stored: StoredCheck,
    pub target: Target,
    pub request: CheckRequest,
}

/// One check of a target monitor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckResult {
    /// The verdict recorded in `status_checks`.
    pub outcome: ProbeOutcome,
    /// Every location's answer, for a monitor checked from worker nodes.
    pub locations: Option<MonitorLocationResults>,
}

pub struct TargetChecks {
    encryption: Arc<EncryptionService>,
    prober: Arc<dyn TargetProber>,
    locations: Arc<dyn ProbeLocations>,
}

impl TargetChecks {
    pub fn new(
        encryption: Arc<EncryptionService>,
        prober: Arc<dyn TargetProber>,
        locations: Arc<dyn ProbeLocations>,
    ) -> Self {
        Self {
            encryption,
            prober,
            locations,
        }
    }

    pub(crate) fn locations(&self) -> &dyn ProbeLocations {
        self.locations.as_ref()
    }

    pub(crate) fn encryption(&self) -> &EncryptionService {
        &self.encryption
    }

    /// Decode the check stored on `monitor`, decrypting its request.
    pub fn decode(
        &self,
        monitor: &status_monitors::Model,
    ) -> Result<DecodedCheck, TargetMonitorError> {
        let unreadable = |reason: String| TargetMonitorError::StoredCheck {
            monitor_id: monitor.id,
            reason,
        };
        let kind = MonitorCheckKind::from_stored(&monitor.check_kind);
        if kind == MonitorCheckKind::Environment {
            return Err(TargetMonitorError::NotTargetMonitor {
                monitor_id: monitor.id,
            });
        }
        let stored: StoredCheck = monitor
            .check_spec
            .clone()
            .ok_or_else(|| unreadable("it has no check settings".to_string()))
            .and_then(|json| serde_json::from_value(json).map_err(|e| unreadable(e.to_string())))?;
        if stored.spec.kind() != kind {
            return Err(unreadable(format!(
                "its settings are for a {} check but it is a {} monitor",
                stored.spec.kind().as_str(),
                kind.as_str()
            )));
        }
        let target = monitor
            .target
            .as_deref()
            .ok_or_else(|| unreadable("it has no target".to_string()))
            .and_then(|raw| parse_target(kind, raw).map_err(|e| unreadable(e.to_string())))?;
        let request = match &monitor.request_encrypted {
            Some(encrypted) => {
                let json = self
                    .encryption
                    .decrypt_string(encrypted)
                    .map_err(|e| unreadable(format!("its request could not be decrypted: {e}")))?;
                serde_json::from_str(&json).map_err(|e| unreadable(e.to_string()))?
            }
            None => CheckRequest::default(),
        };
        Ok(DecodedCheck {
            stored,
            target,
            request,
        })
    }

    /// Check `monitor`'s target once: from the control plane, and at the
    /// same time from the worker nodes when the monitor asks for it. A
    /// failure to list the nodes degrades to the control plane alone.
    pub async fn run(
        &self,
        monitor: &status_monitors::Model,
    ) -> Result<CheckResult, TargetMonitorError> {
        let check = self.decode(monitor)?;
        let local = self
            .prober
            .probe(&check.stored.spec, &check.target, &check.request);
        if !monitor.probe_from_nodes {
            return Ok(CheckResult {
                outcome: local.await,
                locations: None,
            });
        }
        let nodes = self.locations.nodes().await.unwrap_or_else(|error| {
            warn!(
                monitor_id = monitor.id,
                %error,
                "Could not list worker nodes; checking from the control plane only"
            );
            Vec::new()
        });
        let request = RemoteProbeRequest {
            check: check.stored.spec.clone(),
            target: check.target.stored(),
            request: check.request.clone(),
        };
        let remote = futures::future::join_all(
            nodes
                .iter()
                .map(|node| self.locations.probe(node, &request)),
        );
        let (local, remote) = tokio::join!(local, remote);
        let mut results = vec![MonitorLocationResult::control_plane(local)];
        results.extend(
            nodes
                .iter()
                .zip(remote)
                .map(|(node, answer)| MonitorLocationResult::node(node, answer)),
        );
        Ok(CheckResult {
            outcome: combine(&results),
            locations: Some(MonitorLocationResults {
                checked_at: chrono::Utc::now(),
                results,
            }),
        })
    }
}
