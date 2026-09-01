// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Seam that lets agent runs create their sandboxes through the standalone
//! sandbox service (`temps-sandbox`) instead of the raw [`SandboxProvider`].
//!
//! `temps-sandbox` depends on this crate for the provider trait, so agent
//! code cannot call `SandboxService` at compile time. Instead, the sandbox
//! plugin implements [`RunSandboxService`] and injects it into the
//! [`SandboxRegistry`](crate::services::sandbox_registry::SandboxRegistry)
//! during plugin initialization. When present, every agent-run sandbox gets
//! a first-class `sandboxes` row (visible in the sandbox API, with lifecycle
//! events); when absent (e.g. the sandbox plugin is disabled), the registry
//! falls back to the raw provider exactly as before.

use async_trait::async_trait;

use super::{SandboxCreateConfig, SandboxHandle};
use crate::error::AgentError;

/// Managed creation/teardown of agent-run sandboxes, implemented by the
/// standalone sandbox service. All container work still goes through the
/// shared `SandboxProvider`; this trait only adds the DB row + event
/// bookkeeping around it.
#[async_trait]
pub trait RunSandboxService: Send + Sync {
    /// Create a sandbox for an agent run: insert a `sandboxes` row linked
    /// via `agent_run_id`, then create the container through the provider.
    async fn create_for_run(
        &self,
        config: SandboxCreateConfig,
    ) -> Result<SandboxHandle, AgentError>;

    /// Destroy the container for a run's sandbox and mark its row destroyed.
    /// `handle` is the registry's cached handle when it has one; when `None`
    /// the implementation recovers the container by run id.
    async fn release_for_run(
        &self,
        run_id: i32,
        handle: Option<&SandboxHandle>,
    ) -> Result<(), AgentError>;
}
