//! Bridge that plugs [`SandboxService`] into the agents' sandbox registry.
//!
//! `temps-sandbox` depends on `temps-agents`, so agent code can't call this
//! crate directly. Instead this adapter implements the
//! [`RunSandboxService`](temps_agents::sandbox::managed::RunSandboxService)
//! seam and is injected into the agents' `SandboxRegistry` during plugin
//! initialization — from then on every agent-run sandbox is created as a
//! first-class `sandboxes` row visible in the standalone sandbox API.

use std::sync::Arc;

use async_trait::async_trait;
use temps_agents::error::AgentError;
use temps_agents::sandbox::managed::RunSandboxService;
use temps_agents::sandbox::{SandboxCreateConfig, SandboxHandle};

use crate::error::SandboxError;
use crate::services::sandbox_service::SandboxService;

pub struct AgentRunSandboxBridge {
    service: Arc<SandboxService>,
}

impl AgentRunSandboxBridge {
    pub fn new(service: Arc<SandboxService>) -> Self {
        Self { service }
    }
}

/// Map the sandbox-service error back onto the agents' error type at the
/// seam. Creation failures keep their full context in the reason string.
fn to_agent_error(run_id: i32, e: SandboxError) -> AgentError {
    AgentError::SandboxCreationFailed {
        run_id,
        provider: "standalone-sandbox-service".to_string(),
        reason: e.to_string(),
    }
}

#[async_trait]
impl RunSandboxService for AgentRunSandboxBridge {
    async fn create_for_run(
        &self,
        config: SandboxCreateConfig,
    ) -> Result<SandboxHandle, AgentError> {
        let run_id = config.run_id;
        self.service
            .create_for_agent_run(config)
            .await
            .map_err(|e| to_agent_error(run_id, e))
    }

    async fn release_for_run(
        &self,
        run_id: i32,
        handle: Option<&SandboxHandle>,
    ) -> Result<(), AgentError> {
        self.service
            .release_for_agent_run(run_id, handle)
            .await
            .map_err(|e| to_agent_error(run_id, e))
    }
}
