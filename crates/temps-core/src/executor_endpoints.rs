// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Where an environment's code can be invoked (ADR-103 §0, §7).
//!
//! The durable workflows engine never creates or scales containers: it asks
//! the workload layer for the healthy replicas of an environment's current
//! deployment and their internal addresses, and invokes the SDK there. The
//! deployments crate implements [`ExecutorEndpoints`]; `temps-workflows` only
//! reads it, so neither crate depends on the other.

use async_trait::async_trait;
use thiserror::Error;

/// One replica that can take an invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutorEndpoint {
    /// `http://host:port`, an internal address: the node's data (mesh)
    /// address for a worker, the container address on the control plane.
    /// Never a public URL.
    pub base_url: String,
    /// `deployment_containers.id` of the replica.
    pub container_id: i32,
    /// The worker node it runs on; `None` on the control plane.
    pub node_id: Option<i32>,
}

/// The replicas of an environment's current deployment that can be invoked.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExecutorEndpointSet {
    /// The deployment the replicas belong to; `None` when the environment has
    /// no current deployment. Callers key caches (the workflow manifest) on
    /// it, so a new deployment invalidates them.
    pub deployment_id: Option<i32>,
    pub endpoints: Vec<ExecutorEndpoint>,
    /// Why `endpoints` is empty, in words the console can show as is
    /// ("No deployment is live in this environment yet").
    pub unavailable_reason: Option<String>,
}

impl ExecutorEndpointSet {
    /// An empty set with the reason nothing can be invoked.
    pub fn unavailable(deployment_id: Option<i32>, reason: impl Into<String>) -> Self {
        Self {
            deployment_id,
            endpoints: Vec::new(),
            unavailable_reason: Some(reason.into()),
        }
    }
}

#[derive(Debug, Error)]
pub enum ExecutorEndpointsError {
    #[error("Environment {environment_id} of project {project_id} was not found while resolving executor endpoints")]
    EnvironmentNotFound {
        project_id: i32,
        environment_id: i32,
    },
    #[error("Could not resolve executor endpoints of environment {environment_id} in project {project_id}: {reason}")]
    Lookup {
        project_id: i32,
        environment_id: i32,
        reason: String,
    },
}

#[async_trait]
pub trait ExecutorEndpoints: Send + Sync {
    /// The invokable replicas of `environment_id`'s current deployment.
    ///
    /// `service` selects a Compose service by name; `None` means the
    /// environment's main (single-container) service. Unhealthy replicas are
    /// excluded; replicas whose health is unknown are included, as the proxy
    /// routes to them too.
    async fn endpoints(
        &self,
        project_id: i32,
        environment_id: i32,
        service: Option<&str>,
    ) -> Result<ExecutorEndpointSet, ExecutorEndpointsError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_set_carries_its_reason_and_no_endpoint() {
        let set = ExecutorEndpointSet::unavailable(Some(4), "No healthy replica");
        assert_eq!(set.deployment_id, Some(4));
        assert!(set.endpoints.is_empty());
        assert_eq!(
            set.unavailable_reason.as_deref(),
            Some("No healthy replica")
        );
    }
}
