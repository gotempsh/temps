//! Authorization seam for unattended DNS mutations.
//!
//! DNS provider credentials can modify production infrastructure. Human API
//! access is protected separately by `temps-auth` permissions; this module
//! governs background work, where no authenticated principal exists. The
//! default is deliberately fail-closed: a plugin must explicitly install a
//! gate before the certificate scheduler may publish ACME DNS-01 records.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// The only background DNS purpose currently supported by Temps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DnsAutomationPurpose {
    AcmeDns01,
}

/// Exact DNS replacement authorized for an unattended operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DnsAutomationMutation {
    pub record_type: String,
    pub name: String,
    pub value: String,
}

/// Context for an unattended ACME DNS-01 mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DnsAutomationRequest {
    pub purpose: DnsAutomationPurpose,
    pub domain: String,
    pub zone: String,
    pub provider_id: i32,
    pub provider_name: String,
    /// Each entry means replace stale values at this exact name, then publish
    /// the supplied value. No broader zone mutation is authorized.
    pub mutations: Vec<DnsAutomationMutation>,
}

/// Result of evaluating an unattended DNS mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DnsAutomationDecision {
    Allow,
    Deny { reason: String },
}

/// Policy extension point for unattended DNS mutations.
#[async_trait]
pub trait DnsAutomationGate: Send + Sync {
    async fn authorize(
        &self,
        request: &DnsAutomationRequest,
    ) -> Result<DnsAutomationDecision, Box<dyn std::error::Error + Send + Sync>>;
}

/// Deferred, write-once gate used across the plugin registration boundary.
///
/// Core services are constructed before optional plugins register. They keep
/// this slot, while an authorized plugin may claim it later. An unclaimed slot
/// denies automation, so missing or failed plugin registration cannot expose
/// DNS credentials to background mutation.
#[derive(Default)]
pub struct DnsAutomationGateSlot {
    gate: OnceLock<Arc<dyn DnsAutomationGate>>,
}

impl DnsAutomationGateSlot {
    pub fn new() -> Self {
        Self::default()
    }

    /// Install the process-wide DNS automation policy. Only the first caller
    /// succeeds; later callers receive their gate back unchanged.
    pub fn set(&self, gate: Arc<dyn DnsAutomationGate>) -> Result<(), Arc<dyn DnsAutomationGate>> {
        self.gate.set(gate)
    }
}

#[async_trait]
impl DnsAutomationGate for DnsAutomationGateSlot {
    async fn authorize(
        &self,
        request: &DnsAutomationRequest,
    ) -> Result<DnsAutomationDecision, Box<dyn std::error::Error + Send + Sync>> {
        let Some(gate) = self.gate.get() else {
            return Ok(DnsAutomationDecision::Deny {
                reason: "unattended DNS automation is not enabled for this installation"
                    .to_string(),
            });
        };

        gate.authorize(request).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct AllowGate;

    #[async_trait]
    impl DnsAutomationGate for AllowGate {
        async fn authorize(
            &self,
            _request: &DnsAutomationRequest,
        ) -> Result<DnsAutomationDecision, Box<dyn std::error::Error + Send + Sync>> {
            Ok(DnsAutomationDecision::Allow)
        }
    }

    fn request() -> DnsAutomationRequest {
        DnsAutomationRequest {
            purpose: DnsAutomationPurpose::AcmeDns01,
            domain: "example.com".to_string(),
            zone: "example.com".to_string(),
            provider_id: 7,
            provider_name: "production-dns".to_string(),
            mutations: vec![DnsAutomationMutation {
                record_type: "TXT".to_string(),
                name: "_acme-challenge.example.com".to_string(),
                value: "challenge-token".to_string(),
            }],
        }
    }

    #[tokio::test]
    async fn unclaimed_slot_denies_automation() {
        let slot = DnsAutomationGateSlot::new();
        let decision = slot.authorize(&request()).await.unwrap();

        assert!(matches!(decision, DnsAutomationDecision::Deny { .. }));
    }

    #[tokio::test]
    async fn claimed_slot_delegates_to_registered_gate() {
        let slot = DnsAutomationGateSlot::new();
        assert!(slot.set(Arc::new(AllowGate)).is_ok());

        assert_eq!(
            slot.authorize(&request()).await.unwrap(),
            DnsAutomationDecision::Allow
        );
    }

    #[test]
    fn slot_cannot_be_replaced_after_it_is_claimed() {
        let slot = DnsAutomationGateSlot::new();
        assert!(slot.set(Arc::new(AllowGate)).is_ok());
        assert!(slot.set(Arc::new(AllowGate)).is_err());
    }
}
