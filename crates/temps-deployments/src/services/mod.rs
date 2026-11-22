// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

#[allow(clippy::module_inception)]
pub mod services;
pub use services::*;

pub mod compose_check_service;
pub use temps_nodes::services::compose_placement;
pub mod container_operations;
pub use container_operations::*;

pub mod types;
pub use types::*;

pub use temps_nodes::services::cluster_dns;
pub use temps_nodes::services::cluster_network;
pub use temps_nodes::services::node_pairing;
pub use temps_nodes::services::node_pairing_admin;
pub use temps_nodes::services::node_ssh;
pub use temps_nodes::services::node_ssh_enrollment;
pub use temps_nodes::services::wireguard_mesh;
pub use temps_nodes::services::worker_metrics_collector;

pub mod job_processor;
pub use job_processor::*;

pub mod workflow_planner;
pub use workflow_planner::*;

pub mod managed_environment_variables;
pub use managed_environment_variables::*;

pub mod env_resolver;

/// Variables inherited from the service's project (group)
pub mod group_env;
pub use env_resolver::*;

pub mod port_resolver;
pub use port_resolver::*;

pub mod workflow_execution_service;
pub use workflow_execution_service::*;

pub mod stage_log_writer;
pub use stage_log_writer::*;

pub mod job_tracker;
pub use job_tracker::*;

pub mod cron_jobs;
pub mod cron_workflows;
pub mod database_cron_service;
pub use database_cron_service::*;

pub mod database_metric_alert_service;
pub use database_metric_alert_service::*;

pub mod external_deployment;
pub use external_deployment::*;

pub mod screenshot_operation;
pub use screenshot_operation::*;

pub use docker_cleanup_service::*;
pub use temps_docker_housekeeping::services::docker_cleanup_service;

pub use docker_disk_usage::*;
pub use temps_docker_housekeeping::services::docker_disk_usage;

pub use deployment_token_service::*;
pub use temps_ci_tokens::services::deployment_token_service;

pub mod remote_deployment_service;
pub use remote_deployment_service::*;

pub use node_service::*;
pub use temps_nodes::services::edge_routes;
pub use temps_nodes::services::node_service;

pub use node_scheduler::*;
pub use temps_nodes::services::node_plugin_builder;
pub use temps_nodes::services::node_public_ingress;
pub use temps_nodes::services::node_scheduler;

pub use remote_log_source::*;
pub use temps_nodes::services::remote_log_source;

pub mod sensitive_envelope;
pub use sensitive_envelope::*;

pub mod log_redaction;
pub use log_redaction::*;

pub mod failure_classifier;
pub use failure_classifier::{
    describe_failure, DeploymentFailureCode, DeploymentFailureInfo, DeploymentFailureStage,
    FailureSettingsSection,
};

pub mod failure_report_service;
pub use failure_report_service::*;

pub use temps_traefik_discovery::services::traefik_discovery_service;
pub use traefik_discovery_service::*;

pub use cert_validator::*;
pub use temps_traefik_discovery::services::cert_validator;

pub mod source_drop_service;
pub use source_drop_service::*;

pub(crate) mod deployment_commit;

pub use temps_ci_tokens::services::ci_dispatch;

pub mod runtime_health;

pub mod executor_endpoints;
pub use executor_endpoints::DeploymentExecutorEndpoints;

pub mod workload_placement;
pub use workload_placement::{WorkloadPlacementError, WorkloadPlacementService};
pub mod workload_volumes;
pub use resolver_coverage::{
    DeploymentWithoutResolver, ResolverCoverageError, ResolverCoverageHost, ResolverCoverageService,
};
pub use temps_nodes::services::node_state;
pub use temps_nodes::services::resolver_coverage;
pub use workload_volumes::{WorkloadVolumeError, WorkloadVolumeService};
pub mod plane_deployer;
pub use node_state::{
    AgentApiDriver, NodeStateError, NodeStateService, NodeStateStatus, NodeWorkloadPlacement,
};
