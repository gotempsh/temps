// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Reading back the token a service was deployed with.
//!
//! The deploy pipeline gives every container a deployment token as
//! `TEMPS_API_TOKEN`. A component that has to prove to that container that it
//! is this Temps instance (the cron scheduler, the plugin host) reads the same
//! token back through this trait, without depending on the crate that stores
//! deployment tokens.

use async_trait::async_trait;

#[async_trait]
pub trait DeploymentTokenSource: Send + Sync {
    /// The token the containers of `environment_id` of service `project_id`
    /// were started with. The error is a reason safe to show an
    /// administrator: it never contains the token.
    async fn deployment_token(
        &self,
        project_id: i32,
        environment_id: i32,
    ) -> Result<String, String>;
}
