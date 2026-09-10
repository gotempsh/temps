// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Shared resolver for a container's explicit port override, keyed on the
//! *selected environment*.
//!
//! Every deploy path — the normal pipeline (via [`WorkflowPlanner`]), a
//! promotion, and a rollback — must resolve this the same way, since it is
//! what tells [`DeployImageJob::resolve_container_port`] to trust an
//! operator's explicit configuration over image `EXPOSE` auto-detection. A
//! path that recomputes this inline instead of calling
//! [`configured_port_override`] risks losing that precedence and silently
//! reintroducing the bug this function exists to prevent.
//!
//! [`WorkflowPlanner`]: super::workflow_planner::WorkflowPlanner
//! [`DeployImageJob::resolve_container_port`]: crate::jobs::deploy_image::DeployImageJob

use temps_entities::{environments, projects};
use tracing::debug;

/// Explicit port override configured at the environment or project scope,
/// in that priority order. `None` means neither scope configures one, in
/// which case the deploy job falls back to image `EXPOSE` auto-detection
/// and finally the default port.
pub fn configured_port_override(
    environment: &environments::Model,
    project: &projects::Model,
) -> Option<u16> {
    // 1. Environment-level port override (from deployment_config)
    if let Some(port) = environment
        .deployment_config
        .as_ref()
        .and_then(|c| c.exposed_port)
    {
        debug!(
            "Using environment-level port override: {} (environment: {})",
            port, environment.name
        );
        return Some(port as u16);
    }

    // 2. Project-level port override (from deployment_config)
    if let Some(port) = project
        .deployment_config
        .as_ref()
        .and_then(|c| c.exposed_port)
    {
        debug!(
            "Using project-level port override: {} (project: {})",
            port, project.name
        );
        return Some(port as u16);
    }

    None
}
