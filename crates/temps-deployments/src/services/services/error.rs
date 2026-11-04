// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! [`DeploymentError`], the deployment service's error type.

use super::*;

#[derive(Error, Debug)]
pub enum DeploymentError {
    #[error("Database connection error: {0}")]
    DatabaseConnectionError(String),

    #[error("Deployment not found")]
    NotFound(String),

    #[error("Database error: {reason}")]
    DatabaseError { reason: String },

    #[error("Invalid input: {0}")]
    InvalidInput(String),

    #[error("Invalid deployment state: {0}")]
    InvalidDeploymentState(String),

    /// A lifecycle action (pause, resume) was asked of a deployment whose
    /// current state does not allow it, e.g. resuming a stopped or failed
    /// deployment. Mapped to 409: the request is well formed, the
    /// deployment is just not in a state it applies to.
    #[error(
        "Cannot {operation} deployment {deployment_id}: it is '{state}', and only a deployment \
         that is {expected} can be {operation}d"
    )]
    DeploymentStateConflict {
        deployment_id: i32,
        operation: &'static str,
        state: String,
        expected: &'static str,
    },

    /// A pause could not stop every replica, so it put the deployment back in
    /// the state it was paused from, unless something else had already moved
    /// it. Replicas that did stop stay stopped: the operator asked for them
    /// not to run. `state` is what the deployment is now, or "unknown" when
    /// that could not be read, and `next_step` follows from it: a retry is only
    /// advice while a pause would accept it.
    #[error(
        "Pause of deployment {deployment_id} did not complete: stopped {stopped} of {total} \
         replica(s) and could not stop the rest, which are still running ({failures}). The \
         deployment's state is '{state}': {next_step}"
    )]
    DeploymentPauseIncomplete {
        deployment_id: i32,
        stopped: usize,
        total: usize,
        failures: String,
        state: String,
        next_step: &'static str,
    },

    /// A pause was asked of a deployment with a microVM replica, which the
    /// workload plane can only retire for good: there would be nothing for a
    /// resume to bring back. Refused before anything changes. Mapped to 409,
    /// like the other pauses the deployment's current shape does not allow.
    #[error(
        "Cannot pause deployment {deployment_id}: container {container_id} runs in a Firecracker \
         microVM (workload {workload_id}), which cannot be stopped and started again. Redeploy \
         the environment to replace it, or delete the deployment to remove it"
    )]
    MicroVmReplicaCannotBePaused {
        deployment_id: i32,
        container_id: String,
        workload_id: i64,
    },

    #[error("Pipeline error: {0}")]
    PipelineError(String),

    #[error("Deployment error: {0}")]
    DeploymentError(String),

    #[error("Queue error: {0}")]
    QueueError(String),

    /// The caller asked to deploy a project this control plane declares as
    /// requiring the host Docker socket (ADR 045) without instance-admin
    /// authority.
    #[error(
        "{}",
        temps_core::docker_socket_grant::granted_project_deploy_reason(slug)
    )]
    DockerSocketDeployRequiresAdmin { slug: String },

    /// Bundle path (read from DB and joined to data_dir) resolved outside the
    /// data directory.  `path` is the offending resolved path; `reason`
    /// explains how the check failed.
    #[error("Invalid bundle path '{path}': {reason}")]
    InvalidBundlePath { path: String, reason: String },

    #[error("Container {operation} failed for {container_id} on {location}: {reason}")]
    ContainerOperation {
        container_id: String,
        operation: &'static str,
        location: String,
        reason: String,
    },

    /// A container operation was asked of a process that has no local Docker
    /// daemon (e.g. a control plane started with `--profile control-plane`).
    ///
    /// Kept distinct from [`Self::ContainerOperation`] on purpose: that
    /// variant stringifies its cause into a generic 500, which turned "this
    /// host runs no containers, join a worker node" into an opaque server
    /// error. This one is mapped to the shared 409 `WORKER_NODE_REQUIRED`
    /// problem, so the response tells the operator what to do.
    #[error(transparent)]
    DockerUnavailable(#[from] temps_core::DockerUnavailable),

    #[error("Container exec for {container_id} timed out after {timeout_seconds} seconds")]
    ContainerExecTimeout {
        container_id: String,
        timeout_seconds: u64,
    },

    #[error(
        "Cannot resolve the build artifact for deployment {deployment_id} in project {project_id}: source deployment {source_deployment_id} was not found"
    )]
    AssetOriginNotFound {
        project_id: i32,
        deployment_id: i32,
        source_deployment_id: i32,
    },

    #[error(
        "Cannot resolve the build artifact for deployment {deployment_id} in project {project_id}: deployment reuse metadata contains a cycle at deployment {source_deployment_id}"
    )]
    AssetOriginCycle {
        project_id: i32,
        deployment_id: i32,
        source_deployment_id: i32,
    },

    /// The project's app runs outside Temps (`hosting: external`), so Temps
    /// does not create deployments for it. `reason` is the operator-facing
    /// explanation, which already names the project and how to change it.
    #[error("Project {project_id} ('{project_name}') is hosted externally: {reason}")]
    ProjectHostedExternally {
        project_id: i32,
        project_name: String,
        reason: String,
    },

    #[error(transparent)]
    EnvironmentResolution(#[from] super::env_resolver::DeploymentEnvResolutionError),

    /// A replica the workload plane started (a microVM, or a container on
    /// a worker) could not be looked up or retired through the plane.
    #[error(transparent)]
    ReplicaRetire(#[from] crate::services::plane_deployer::ReplicaRetireError),

    /// The project's next deployment number (the `<n>` in its `<project>-<n>`
    /// slug, which is also the container name) could not be allocated.
    #[error("Failed to allocate the next deployment number for project {project_id}: {source}")]
    DeploymentNumberAllocation {
        project_id: i32,
        #[source]
        source: sea_orm::DbErr,
    },

    #[error("Other error: {0}")]
    Other(String),
}

impl From<sea_orm::DbErr> for DeploymentError {
    fn from(error: sea_orm::DbErr) -> Self {
        match error {
            sea_orm::DbErr::RecordNotFound(_) => DeploymentError::NotFound(error.to_string()),
            _ => DeploymentError::DatabaseError {
                reason: error.to_string(),
            },
        }
    }
}
