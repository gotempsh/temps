// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Integration seam only. Saving a request is deliberately separate from
//! provisioning it. A future adapter must reauthorize a fresh request, bind it
//! to a sandbox incarnation, and acknowledge enforcement before reporting ready.
use crate::{
    error::AccessError,
    tokenless::{PreparedSession, RevokeSession},
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

pub(crate) const UNAVAILABLE_REASON: &str = "Tokenless policy preparation is available, but live access is not enabled. Hard expiry, confirmed revocation and sandbox transport are still required. Saved requests do not connect sandboxes; enabling access will require a new request.";

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
    /// Preparation alone does not authorize activation.
    async fn provision(&self, session: &PreparedSession) -> Result<(), AccessError>;
    /// Must not acknowledge until the runtime has stopped forwarding traffic.
    async fn revoke(&self, session: &RevokeSession) -> Result<(), AccessError>;
}

pub struct UnavailableTokenlessBroker;
#[async_trait]
impl ServiceAccessBroker for UnavailableTokenlessBroker {
    fn capability(&self) -> AccessCapability {
        AccessCapability {
            state: AccessCapabilityState::IntegrationUnavailable,
            configured: false,
            can_save_requests: true,
            reason: UNAVAILABLE_REASON.into(),
            setup_path: None,
        }
    }
    async fn provision(&self, session: &PreparedSession) -> Result<(), AccessError> {
        Err(AccessError::BrokerUnavailable {
            operation: "provision",
            grant_id: session.grant_id().to_string(),
        })
    }
    async fn revoke(&self, session: &RevokeSession) -> Result<(), AccessError> {
        Err(AccessError::BrokerUnavailable {
            operation: "revoke",
            grant_id: session.grant_id.to_string(),
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
        assert!(broker
            .capability()
            .reason
            .contains("live access is not enabled"));
        assert!(broker.capability().reason.contains("new request"));
        let session = crate::tokenless::tests::prepared_session();
        let grant_id = session.grant_id().to_string();
        let revoke = RevokeSession {
            target: session.target().clone(),
            grant_id: session.grant_id().into(),
        };
        for result in [
            broker.provision(&session).await,
            broker.revoke(&revoke).await,
        ] {
            assert!(
                matches!(result, Err(AccessError::BrokerUnavailable { grant_id: id, .. }) if id == grant_id)
            );
        }
    }
}
