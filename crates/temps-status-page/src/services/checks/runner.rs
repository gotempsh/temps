// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Turning a stored target monitor into a probe.

use std::sync::Arc;

use temps_core::EncryptionService;
use temps_entities::status_monitors;

use super::error::TargetMonitorError;
use super::probe::{ProbeOutcome, TargetProber};
use super::spec::{parse_target, CheckKind, CheckRequest, StoredCheck, Target};

/// A target monitor's check, decoded from its row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedCheck {
    pub stored: StoredCheck,
    pub target: Target,
    pub request: CheckRequest,
}

pub struct TargetChecks {
    encryption: Arc<EncryptionService>,
    prober: Arc<dyn TargetProber>,
}

impl TargetChecks {
    pub fn new(encryption: Arc<EncryptionService>, prober: Arc<dyn TargetProber>) -> Self {
        Self { encryption, prober }
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
        let kind = CheckKind::from_stored(&monitor.check_kind);
        if kind == CheckKind::Environment {
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

    /// Probe `monitor`'s target once.
    pub async fn probe(
        &self,
        monitor: &status_monitors::Model,
    ) -> Result<ProbeOutcome, TargetMonitorError> {
        let check = self.decode(monitor)?;
        Ok(self
            .prober
            .probe(&check.stored.spec, &check.target, &check.request)
            .await)
    }
}
