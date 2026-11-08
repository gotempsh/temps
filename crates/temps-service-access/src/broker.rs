// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Integration seam only. Saving a request is deliberately separate from
//! provisioning it. A future adapter must reauthorize a fresh request, bind it
//! to a sandbox incarnation, and acknowledge enforcement before reporting ready.
use crate::error::AccessError;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AccessCapabilityState {
    IntegrationUnavailable,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AccessCapability {
    pub state: AccessCapabilityState,
    pub configured: bool,
    pub can_save_requests: bool,
    pub reason: String,
    pub setup_path: Option<String>,
}

#[async_trait]
pub trait ServiceAccessBroker: Send + Sync {
    fn capability(&self) -> AccessCapability;
    /// No implementation may interpret an old saved intent as fresh consent.
    /// The integration implementation must add a typed, incarnation-bound
    /// provisioning command before enabling this operation.
    async fn provision(&self, grant_id: &str) -> Result<(), AccessError>;
    /// Must not acknowledge until the runtime has stopped forwarding traffic.
    async fn revoke(&self, grant_id: &str) -> Result<(), AccessError>;
}

pub struct UnavailableTokenlessBroker;
#[async_trait]
impl ServiceAccessBroker for UnavailableTokenlessBroker {
    fn capability(&self) -> AccessCapability {
        AccessCapability {
            state: AccessCapabilityState::IntegrationUnavailable,
            configured: false,
            can_save_requests: true,
            reason: "The Tokenless integration is not installed. You can save services and access requests, but they do not connect sandboxes. Installing the integration will require a new access request.".into(),
            setup_path: None,
        }
    }
    async fn provision(&self, grant_id: &str) -> Result<(), AccessError> {
        Err(AccessError::BrokerUnavailable {
            operation: "provision",
            grant_id: grant_id.into(),
        })
    }
    async fn revoke(&self, grant_id: &str) -> Result<(), AccessError> {
        Err(AccessError::BrokerUnavailable {
            operation: "revoke",
            grant_id: grant_id.into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn unavailable_adapter_never_claims_enforcement() {
        let broker = UnavailableTokenlessBroker;
        assert!(!broker.capability().configured);
        for result in [
            broker.provision("grant-test").await,
            broker.revoke("grant-test").await,
        ] {
            assert!(
                matches!(result, Err(AccessError::BrokerUnavailable { grant_id, .. }) if grant_id == "grant-test")
            );
        }
    }
}
