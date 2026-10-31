// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! [`DeploymentService`]: deployments, their containers and their lifecycle.
//!
//! The service is defined here; its behaviour is split by concern:
//!
//! - `error`: [`DeploymentError`]
//! - `domains`: deployment and environment URLs, environment links, custom
//!   domains
//! - `queries`: deployment reads, captured container logs, jobs, activity and
//!   the row-to-DTO mapping
//! - `settings`: environment deployment settings and the static asset cache
//!   purge
//! - `containers`: container runtime per node, logs, listings, detail,
//!   start/stop/restart, env vars and metrics
//! - `cleanup`: owner cleanup of a deleted project's or environment's
//!   containers, and its ownership checks
//! - `guards`: the Docker-socket grant and external-hosting checks every deploy
//!   path runs
//! - `trigger`: manual pipeline and image deployment triggers
//! - `redeploy`: redeploys from the recorded source, node drain and failover
//!   planning
//! - `reuse`: what rollback and promotion share: the reused artifact, its image
//!   provenance, health and resources, static reuse completion and failure
//!   handling
//! - `rollback`: rollback to an earlier deployment of the same environment
//! - `promote`: promotion of a deployment's build into another environment
//! - `lifecycle`: teardown, pause and resume
//! - `cancel`: cancellation of running deployments, and the bulk cancel and
//!   source-archive cleanup a deletion runs

use futures::Stream;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, ConnectionTrait, DatabaseBackend,
    DatabaseTransaction, DbErr, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder, QuerySelect,
    Set, Statement, TransactionTrait,
};
use std::collections::HashMap;
use std::collections::HashSet;
use std::pin::Pin;
use std::sync::Arc;
use temps_entities::{
    deployment_container_logs, deployment_containers, deployment_domains, deployments,
    environments, nodes, projects,
};
use thiserror::Error;
use tracing::{debug, error, info, warn};

use super::container_operations::{
    ContainerOperations, LocalContainerOperations, RemoteContainerOperations,
};
use crate::services::node_service::{CONTAINER_STATUS_ORPHANED, CONTAINER_STATUS_REMOVED};
use crate::services::types::{
    Deployment, DeploymentDomain, DeploymentEnvironment, DeploymentListResponse,
    LatestDeploymentMedia,
};
use crate::UpdateDeploymentSettingsRequest;
use temps_core::PublicHostnameStrategy;
use temps_core::WorkflowTask;

// Sibling service modules, imported so the submodules below keep reaching
// them as `super::<module>`.
use super::{env_resolver, job_processor, port_resolver, workflow_execution_service};

mod cancel;
mod cleanup;
mod containers;
mod domains;
mod error;
mod guards;
mod lifecycle;
mod promote;
mod queries;
mod redeploy;
mod reuse;
mod rollback;
mod settings;
mod trigger;

pub use containers::{
    ContainerLogParams, ContainerLogStream, ContainerPresentationContext, EnvironmentContainer,
    ResolvedContainerResourceLimits,
};
pub use domains::ComposePublicUrlContext;
pub use error::DeploymentError;
pub use queries::RepoReference;
pub use redeploy::{RedeploySource, MAX_REDEPLOY_PLAN_BATCH};

/// Lock the environment row that orders deployment generations. All code paths
/// that insert a deployment must take this lock before assigning `created_at`
/// and inserting, so failover recovery can reliably detect newer user work.
pub(crate) async fn lock_environment_for_deployment_generation(
    transaction: &DatabaseTransaction,
    project_id: i32,
    environment_id: i32,
) -> Result<environments::Model, DbErr> {
    environments::Entity::find_by_id(environment_id)
        .filter(environments::Column::ProjectId.eq(project_id))
        .filter(environments::Column::DeletedAt.is_null())
        .lock(sea_orm::sea_query::LockType::Update)
        .one(transaction)
        .await?
        .ok_or_else(|| {
            DbErr::RecordNotFound(format!(
                "environment {environment_id} for project {project_id} was not found or is deleted"
            ))
        })
}

/// Insert a deployment under the shared environment generation lock.
pub(crate) async fn insert_deployment_with_generation_lock(
    db: &temps_database::DbConnection,
    project_id: i32,
    environment_id: i32,
    mut deployment: deployments::ActiveModel,
) -> Result<deployments::Model, DbErr> {
    let transaction = db.begin().await?;
    lock_environment_for_deployment_generation(&transaction, project_id, environment_id).await?;
    let generation_time = chrono::Utc::now();
    deployment.created_at = Set(generation_time);
    deployment.updated_at = Set(generation_time);
    let deployment = deployment.insert(&transaction).await?;
    transaction.commit().await?;
    Ok(deployment)
}

struct PipelineTriggerOptions {
    branch: Option<String>,
    tag: Option<String>,
    commit: Option<String>,
    rollback_from_deployment_id: Option<i32>,
    recovery_of_deployment_id: Option<i32>,
}

/// True only for a Postgres unique-constraint violation (SQLSTATE 23505).
///
/// Used to detect the race where two requests carrying the same
/// `upload_request_id` both pass the existence pre-check before either
/// inserts: the loser's plain (non-`on_conflict`) insert hits
/// `idx_deployments_upload_request_id` instead of succeeding, and that
/// specific failure -- not a generic database error -- means the winner's
/// row should be looked up and returned instead of surfacing a 500.
pub(crate) fn is_unique_violation(err: &sea_orm::DbErr) -> bool {
    err.sql_err()
        .is_some_and(|e| matches!(e, sea_orm::SqlErr::UniqueConstraintViolation(_)))
}

#[derive(Clone)]
pub struct DeploymentService {
    db: Arc<temps_database::DbConnection>,
    log_service: Arc<temps_deployment_logs::LogService>,
    config_service: Arc<temps_config::ConfigService>,
    queue_service: Arc<dyn temps_core::JobQueue>,
    docker_log_service: Arc<temps_deployment_logs::DockerLogService>,
    docker_handle: Arc<temps_core::DockerHandle>,
    deployer: Arc<dyn temps_deployer::ContainerDeployer>,
    encryption_service: Arc<temps_core::EncryptionService>,
    /// Anonymous product telemetry reporter (late-bound, optional). Set via
    /// [`Self::set_telemetry`]; defaults to a no-op when unset.
    telemetry: std::sync::OnceLock<Arc<dyn temps_core::telemetry::TelemetryReporter>>,
    /// ADR 068: an environment's primary address and the deploy warning
    /// (late-bound, optional: empty without the ingress plugin).
    environment_addresses:
        std::sync::OnceLock<Arc<temps_core::environment_address::EnvironmentAddressSlot>>,
    /// Resolves a container's environment variables from the selected
    /// environment (late-bound; the resolver's six service deps only exist later
    /// in plugin init). Set via [`Self::set_env_resolver`]. Used by the inline
    /// promote/rollback deploy paths so a promoted/rolled-back container gets the
    /// SAME resolved env (user vars, external-service vars, Sentry/OTel, API
    /// token) as a normal deploy — see [`crate::services::env_resolver`].
    env_resolver: std::sync::OnceLock<Arc<crate::services::env_resolver::DeploymentEnvResolver>>,
    /// Late-bound Compose executor (the `Arc<bollard::Docker>` client it needs
    /// is only constructed later in plugin init, after `DeploymentService`
    /// itself). Set via [`Self::set_compose_executor`]. Used by
    /// `cleanup_containers` preserves ambiguous Compose resources until an
    /// operator verifies their ownership before reclaiming them.
    compose_executor: std::sync::OnceLock<Arc<temps_deployer::compose::ComposeExecutor>>,
    /// Audit sink for deploy-path security events (late-bound, optional).
    ///
    /// The rollback and promotion paths build their own `DeployImageJob`
    /// rather than going through `WorkflowExecutionService`, so without this
    /// they would always take the "no audit sink" branch and the ADR-045
    /// `DEPLOYMENT_DOCKER_SOCKET_MOUNTED` record would silently never exist
    /// for a rolled-back or promoted granted project. Late-bound like
    /// `telemetry`: a missing sink degrades to a log line, never a failed
    /// deployment.
    audit_logger: std::sync::OnceLock<Arc<dyn temps_core::AuditLogger>>,
    /// Scheduler shared with the normal workflow path. Promotion and rollback
    /// build deploy jobs inline, so they must receive it explicitly or every
    /// reused image is implicitly assigned to the control plane.
    node_scheduler: std::sync::OnceLock<Arc<crate::services::NodeScheduler>>,
    /// Image source used when a selected worker needs an image transferred.
    image_builder: Arc<dyn temps_deployer::ImageBuilder>,
    /// Planner shared with normal deployments for worker-specific environment
    /// rewrites and cross-node managed-service blockers.
    workflow_planner: std::sync::OnceLock<Arc<crate::services::WorkflowPlanner>>,
    /// The workload plane, bound once it starts (ADR-055). Rollback and
    /// promotion retire the superseded deployment's `enforce` replicas
    /// through it; unset, those replicas are left for a later sweep rather
    /// than removed behind the plane's back.
    workload_plane: std::sync::OnceLock<Arc<temps_workload_plane::WorkloadPlane>>,
}

impl DeploymentService {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        db: Arc<temps_database::DbConnection>,
        log_service: Arc<temps_deployment_logs::LogService>,
        config_service: Arc<temps_config::ConfigService>,
        queue_service: Arc<dyn temps_core::JobQueue>,
        docker_log_service: Arc<temps_deployment_logs::DockerLogService>,
        docker_handle: Arc<temps_core::DockerHandle>,
        deployer: Arc<dyn temps_deployer::ContainerDeployer>,
        image_builder: Arc<dyn temps_deployer::ImageBuilder>,
        encryption_service: Arc<temps_core::EncryptionService>,
    ) -> Self {
        DeploymentService {
            db,
            log_service,
            config_service,
            queue_service,
            docker_log_service,
            docker_handle,
            deployer,
            image_builder,
            encryption_service,
            telemetry: std::sync::OnceLock::new(),
            environment_addresses: std::sync::OnceLock::new(),
            env_resolver: std::sync::OnceLock::new(),
            compose_executor: std::sync::OnceLock::new(),
            audit_logger: std::sync::OnceLock::new(),
            node_scheduler: std::sync::OnceLock::new(),
            workflow_planner: std::sync::OnceLock::new(),
            workload_plane: std::sync::OnceLock::new(),
        }
    }

    /// Late-bind the environment-variable resolver (see the field docs). Called
    /// once during plugin init after the resolver's service deps exist.
    pub fn set_env_resolver(
        &self,
        resolver: Arc<crate::services::env_resolver::DeploymentEnvResolver>,
    ) {
        let _ = self.env_resolver.set(resolver);
    }

    /// Late-bind the Compose executor (see the field docs). Called once
    /// during plugin init after the `Arc<bollard::Docker>` client exists.
    pub fn set_compose_executor(&self, executor: Arc<temps_deployer::compose::ComposeExecutor>) {
        let _ = self.compose_executor.set(executor);
    }

    /// Set the anonymous telemetry reporter used to emit deploy-funnel events
    /// (currently `rollback_triggered`).
    pub fn set_telemetry(&self, reporter: Arc<dyn temps_core::telemetry::TelemetryReporter>) {
        let _ = self.telemetry.set(reporter);
    }

    /// Wire the ADR 068 primary address and deploy warning.
    pub fn set_environment_addresses(
        &self,
        slot: Arc<temps_core::environment_address::EnvironmentAddressSlot>,
    ) {
        let _ = self.environment_addresses.set(slot);
    }

    /// Set the audit sink used for deploy-path security events (ADR 045), so
    /// the rollback and promotion paths record a host-Docker-socket mount the
    /// same way an ordinary deployment does.
    pub fn set_audit_logger(&self, logger: Arc<dyn temps_core::AuditLogger>) {
        let _ = self.audit_logger.set(logger);
    }

    /// Use the cluster scheduler for inline artifact-reuse deployments.
    pub fn set_node_scheduler(&self, scheduler: Arc<crate::services::NodeScheduler>) {
        let _ = self.node_scheduler.set(scheduler);
    }

    pub fn set_workflow_planner(&self, planner: Arc<crate::services::WorkflowPlanner>) {
        let _ = self.workflow_planner.set(planner);
    }

    /// Bind the started workload plane (see the field docs). Later calls are
    /// ignored: a process runs one plane.
    pub fn set_workload_plane(&self, plane: Arc<temps_workload_plane::WorkloadPlane>) {
        let _ = self.workload_plane.set(plane);
    }

    /// The telemetry reporter, or a no-op when none has been wired.
    fn telemetry(&self) -> Arc<dyn temps_core::telemetry::TelemetryReporter> {
        self.telemetry
            .get()
            .cloned()
            .unwrap_or_else(|| Arc::new(temps_core::telemetry::NoopTelemetryReporter))
    }
}

#[cfg(test)]
mod tests;
