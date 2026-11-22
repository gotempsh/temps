// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Errors of the database workload path (ADR-055 Phase 4).

use thiserror::Error;

use axum::http::StatusCode;
use temps_core::problemdetails::{self, Problem};

use crate::service_errors::{worker_node_required, ExternalServiceError};

#[derive(Error, Debug)]
pub enum ServiceWorkloadError {
    #[error("External service {service_id} not found")]
    NotFound { service_id: i32 },

    #[error(
        "Service {service_id} ('{name}', type '{service_type}') cannot use workload mode \
         '{mode}': {reason}"
    )]
    Unsupported {
        service_id: i32,
        name: String,
        service_type: String,
        mode: String,
        reason: String,
    },

    #[error(
        "Workload spec of database container '{container}' (service {service_id}) was refused \
         at admission ({rule}): {message}"
    )]
    Admission {
        service_id: i32,
        container: String,
        rule: String,
        message: String,
    },

    #[error(
        "Cannot read the process of database container '{container}' (service {service_id}): \
         {reason}"
    )]
    Process {
        service_id: i32,
        container: String,
        reason: String,
    },

    #[error("Database container '{container}' of service {service_id} does not exist on {node}")]
    ContainerMissing {
        service_id: i32,
        container: String,
        node: String,
    },

    #[error("Docker '{operation}' on database container '{container}' failed: {reason}")]
    Docker {
        operation: &'static str,
        container: String,
        reason: String,
    },

    #[error(
        "Database container '{container}' did not become healthy within {timeout_secs}s: \
         {reason}"
    )]
    Unhealthy {
        container: String,
        timeout_secs: u64,
        reason: String,
    },

    #[error(
        "The pending changes of service {service_id} are now {current}, not the {confirmed} \
         that was confirmed; review them again before recreating"
    )]
    StaleConfirmation {
        service_id: i32,
        confirmed: String,
        current: String,
    },

    #[error(
        "Service {service_id} ('{name}') is in workload mode '{mode}'; only an enforce-mode \
         database is recreated from its workload spec"
    )]
    NotEnforced {
        service_id: i32,
        name: String,
        mode: String,
    },

    #[error(
        "The agent on node {node} does not support {operation}; upgrade it to this release \
         before using enforce or shadow mode for databases on that node"
    )]
    AgentUnsupported {
        node: String,
        operation: &'static str,
    },

    #[error(
        "Agent on node {node} failed to {operation} database container '{container}': {reason}"
    )]
    Agent {
        node: String,
        operation: &'static str,
        container: String,
        reason: String,
    },

    #[error(
        "Replacing database container '{container}' of service {service_id} failed and the \
         original was kept: {reason}"
    )]
    Recreate {
        service_id: i32,
        container: String,
        reason: String,
    },

    #[error("Workload-mode operation on service {service_id} failed: {source}")]
    Service {
        service_id: i32,
        #[source]
        source: ExternalServiceError,
    },

    #[error("Database error during a workload-mode operation: {0}")]
    Database(#[from] sea_orm::DbErr),
}

impl ServiceWorkloadError {
    pub fn service(service_id: i32) -> impl FnOnce(ExternalServiceError) -> Self {
        move |source| match source {
            ExternalServiceError::ServiceNotFound { id } => Self::NotFound { service_id: id },
            source => Self::Service { service_id, source },
        }
    }
}

impl From<ServiceWorkloadError> for Problem {
    fn from(error: ServiceWorkloadError) -> Self {
        let (status, title) = match &error {
            ServiceWorkloadError::NotFound { .. } => (StatusCode::NOT_FOUND, "Service Not Found"),
            ServiceWorkloadError::Unsupported { .. } => {
                (StatusCode::BAD_REQUEST, "Workload Mode Not Supported")
            }
            ServiceWorkloadError::Admission { .. } => {
                (StatusCode::UNPROCESSABLE_ENTITY, "Workload Spec Refused")
            }
            ServiceWorkloadError::ContainerMissing { .. } => {
                (StatusCode::CONFLICT, "Database Container Missing")
            }
            ServiceWorkloadError::StaleConfirmation { .. } => {
                (StatusCode::CONFLICT, "Pending Changes Changed")
            }
            ServiceWorkloadError::NotEnforced { .. } => {
                (StatusCode::CONFLICT, "Database Not In Enforce Mode")
            }
            ServiceWorkloadError::AgentUnsupported { .. } => {
                (StatusCode::CONFLICT, "Agent Upgrade Required")
            }
            ServiceWorkloadError::Agent { .. } => (StatusCode::BAD_GATEWAY, "Agent Request Failed"),
            ServiceWorkloadError::Service { source, .. } => {
                if let Some(problem) = worker_node_required(source) {
                    return problem;
                }
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Workload Operation Failed",
                )
            }
            ServiceWorkloadError::Process { .. } => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Database Container Unreadable",
            ),
            ServiceWorkloadError::Docker { .. } => {
                (StatusCode::INTERNAL_SERVER_ERROR, "Docker Operation Failed")
            }
            ServiceWorkloadError::Unhealthy { .. } => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Database Did Not Become Healthy",
            ),
            ServiceWorkloadError::Recreate { .. } => {
                (StatusCode::INTERNAL_SERVER_ERROR, "Recreate Failed")
            }
            ServiceWorkloadError::Database(_) => {
                (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error")
            }
        };
        problemdetails::new(status)
            .with_title(title)
            .with_detail(error.to_string())
    }
}
