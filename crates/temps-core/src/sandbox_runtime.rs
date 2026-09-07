// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::collections::HashMap;

use async_trait::async_trait;
use thiserror::Error;

/// Provider-neutral seam used by sandboxes to obtain the same scoped runtime
/// variables that Temps injects into a deployed project environment.
///
/// Implementations may provision a tenant database as part of issuance. The
/// returned values are secrets: callers must never log or persist them.
#[async_trait]
pub trait SandboxRuntimeCredentialsProvider: Send + Sync {
    async fn issue(
        &self,
        service_id: i32,
        project_id: i32,
        environment_id: i32,
    ) -> Result<HashMap<String, String>, SandboxRuntimeCredentialsError>;
}

#[derive(Debug, Error)]
pub enum SandboxRuntimeCredentialsError {
    #[error("service {service_id} was not found")]
    ServiceNotFound { service_id: i32 },
    #[error("environment {environment_id} was not found in project {project_id}")]
    EnvironmentNotFound {
        environment_id: i32,
        project_id: i32,
    },
    #[error("service {service_id} is not linked to project {project_id}")]
    ServiceNotLinked { service_id: i32, project_id: i32 },
    #[error("runtime credentials for service {service_id} could not be issued: {reason}")]
    Provider { service_id: i32, reason: String },
}
