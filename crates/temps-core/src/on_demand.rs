// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! On-demand environment wake/sleep traits
//!
//! These traits avoid circular dependencies between temps-environments (handlers)
//! and temps-proxy (OnDemandManager). The proxy implements these traits and they
//! are injected into the environments AppState via the plugin system.

use async_trait::async_trait;

/// Trait for waking/sleeping on-demand environments with full container lifecycle.
///
/// Unlike `EnvironmentService::set_sleeping` (which only flips the DB flag),
/// implementations of this trait start/stop containers and wait for health checks.
#[async_trait]
pub trait OnDemandWaker: Send + Sync {
    /// Wake an environment: start containers, wait for health, set sleeping=false.
    /// Returns Ok(()) when the environment is fully running and ready for traffic.
    async fn wake_environment(
        &self,
        environment_id: i32,
        wake_timeout_seconds: i32,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;

    /// Sleep an environment: stop containers, set sleeping=true.
    /// Returns Ok(true) if this call performed the sleep, Ok(false) if already sleeping.
    async fn sleep_environment(
        &self,
        environment_id: i32,
    ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>>;

    /// Record that an environment was just used by something other than a
    /// proxied request, so it is not stopped for being idle while in use.
    /// Must be cheap: it is called once per request to the environment.
    ///
    /// The default does nothing, for implementations that track no idleness.
    fn record_activity(&self, _environment_id: i32) {}
}
