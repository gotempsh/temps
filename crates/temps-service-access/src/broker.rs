// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Capability reporting only. The coordinator owns fresh consent, delivery and
//! verified worker receipts; saving a request never calls the data plane.
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

pub(crate) const UNAVAILABLE_REASON: &str = "Tokenless policy preparation is available, but live access is not enabled. Hard expiry, confirmed revocation and sandbox transport are still required. Saved requests do not connect sandboxes; enabling access will require a new request.";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AccessCapabilityState {
    Available,
    IntegrationUnavailable,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AccessCapability {
    pub state: AccessCapabilityState,
    pub configured: bool,
    pub can_save_requests: bool,
    /// Caller may edit service policies in this project. Set by the handler.
    pub can_manage_services: bool,
    /// Caller may use the project-bound GitHub credential source.
    pub can_use_project_github: bool,
    /// Caller has project and sandbox write permissions. Ownership is still
    /// checked on each sandbox mutation.
    pub can_manage_requests: bool,
    pub reason: String,
    pub setup_path: Option<String>,
}

pub trait ServiceAccessBroker: Send + Sync {
    fn capability(&self) -> AccessCapability;
}

/// Production adapter: Linux Docker workers support host-bound proxy sockets.
/// The approval path checks the selected sandbox's actual provider support.
pub struct EmbeddedTokenlessBroker;
impl ServiceAccessBroker for EmbeddedTokenlessBroker {
    fn capability(&self) -> AccessCapability {
        let mut capability = UnavailableTokenlessBroker.capability();
        capability.state = AccessCapabilityState::Available;
        capability.configured = true;
        capability.reason = "Approve time-limited access for Docker sandboxes on Linux workers. MicroVMs and Docker Desktop hosts do not yet support the proxy attachment. Approval checks the assigned worker; a saved request alone grants no access.".into();
        capability
    }
}

pub struct UnavailableTokenlessBroker;
impl ServiceAccessBroker for UnavailableTokenlessBroker {
    fn capability(&self) -> AccessCapability {
        AccessCapability {
            state: AccessCapabilityState::IntegrationUnavailable,
            configured: false,
            can_save_requests: true,
            can_manage_services: false,
            can_use_project_github: false,
            can_manage_requests: false,
            reason: UNAVAILABLE_REASON.into(),
            setup_path: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capabilities_distinguish_saved_requests_from_embedded_approval() {
        let unavailable = UnavailableTokenlessBroker.capability();
        assert!(!unavailable.configured);
        assert!(unavailable.can_save_requests);
        let embedded = EmbeddedTokenlessBroker.capability();
        assert!(embedded.configured);
        assert_eq!(embedded.state, AccessCapabilityState::Available);
        assert!(embedded.reason.contains("MicroVMs"));
        assert!(!embedded.can_manage_requests);
    }
}
