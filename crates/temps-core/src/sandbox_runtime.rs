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
/// Who a sandbox credential is for: the sandbox, and the project its link was
/// made through. The project, when there is one, names the sandbox's database
/// and bucket so a new chat on the project continues with the same data;
/// without one the sandbox's own id does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxScope {
    pub sandbox_public_id: String,
    pub project_id: Option<i32>,
}

#[async_trait]
pub trait SandboxRuntimeCredentialsProvider: Send + Sync {
    async fn issue(
        &self,
        service_id: i32,
        project_id: i32,
        environment_id: i32,
    ) -> Result<HashMap<String, String>, SandboxRuntimeCredentialsError>;

    /// The containers a sandbox must be able to dial to use `service_id`
    /// with the credentials of [`Self::issue_for_sandbox`], by name on the
    /// control plane's Docker daemon.
    ///
    /// Empty for a service no sandbox can use: a type with no engine, a
    /// backend the engine does not cover, or a service on another node. The
    /// caller puts exactly these on the sandbox's private network, so nothing
    /// else of the service is reachable.
    async fn sandbox_access_containers(
        &self,
        service_id: i32,
    ) -> Result<Vec<String>, SandboxRuntimeCredentialsError>;

    /// Variables for the sandbox in `scope` to use `service_id`,
    /// authenticating as a credential that reaches only a namespace of the
    /// sandbox's own (ADR-051), never the deployment's. When the scope names a
    /// project, the service must be linked to it.
    ///
    /// A service type with no such mechanism returns
    /// [`SandboxRuntimeCredentialsError::NoScopedCredentials`]; the caller
    /// decides whether to go without or to use [`Self::issue`].
    async fn issue_for_sandbox(
        &self,
        service_id: i32,
        scope: &SandboxScope,
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
    #[error("service {service_id} ({service_type}) has no credentials scoped to a sandbox yet")]
    NoScopedCredentials {
        service_id: i32,
        service_type: String,
    },
    #[error("runtime credentials for service {service_id} could not be issued: {reason}")]
    Provider { service_id: i32, reason: String },
}
