// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What the control plane and a node agent exchange about managed services,
//! and the interface through which an agent reaches engine code.
//!
//! The engines (`temps-providers`) need a lot of the workspace: query drivers,
//! DNS, alarms and more. A worker only calls a handful of operations on them,
//! so it depends on [`NodeServiceRuntime`] here, and the binary that starts the
//! agent passes the implementation in.

use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use bollard::models::ContainerInspectResponse;
use bollard::Docker;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ServiceConfig {
    pub name: String,
    pub service_type: ServiceType,
    pub version: Option<String>,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ServiceType {
    Mariadb,
    Mongodb,
    Postgres,
    Redis,
    /// S3-compatible object storage (RustFS-backed by default)
    S3,
    /// RustFS S3-compatible object storage (standalone)
    Rustfs,
    /// Temps KV service (Redis-backed key-value store)
    Kv,
    /// Temps Blob service (RustFS-backed object storage)
    Blob,
    /// MinIO S3-compatible object storage (deprecated, use S3/RustFS instead)
    #[deprecated(
        note = "Use S3 (RustFS-backed) instead. MinIO is kept for backward compatibility with existing services."
    )]
    Minio,
}

impl std::fmt::Display for ServiceType {
    #[allow(deprecated)]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ServiceType::Mariadb => write!(f, "mariadb"),
            ServiceType::Mongodb => write!(f, "mongodb"),
            ServiceType::Postgres => write!(f, "postgres"),
            ServiceType::Redis => write!(f, "redis"),
            ServiceType::S3 => write!(f, "s3"),
            ServiceType::Rustfs => write!(f, "rustfs"),
            ServiceType::Kv => write!(f, "kv"),
            ServiceType::Blob => write!(f, "blob"),
            ServiceType::Minio => write!(f, "minio"),
        }
    }
}

impl ServiceType {
    #[allow(clippy::should_implement_trait)]
    #[allow(deprecated)]
    pub fn from_str(s: &str) -> Result<Self, ServiceTypeParseError> {
        match s.to_lowercase().as_str() {
            "mariadb" => Ok(ServiceType::Mariadb),
            "mongodb" => Ok(ServiceType::Mongodb),
            "postgres" => Ok(ServiceType::Postgres),
            "redis" => Ok(ServiceType::Redis),
            "s3" => Ok(ServiceType::S3),
            "rustfs" => Ok(ServiceType::Rustfs),
            "kv" => Ok(ServiceType::Kv),
            "blob" => Ok(ServiceType::Blob),
            "minio" => Ok(ServiceType::Minio),
            _ => Err(ServiceTypeParseError {
                service_type: s.to_string(),
            }),
        }
    }

    /// Returns a Vec containing all available service types
    #[allow(deprecated)]
    pub fn get_all() -> Vec<ServiceType> {
        vec![
            ServiceType::Mariadb,
            ServiceType::Mongodb,
            ServiceType::Postgres,
            ServiceType::Redis,
            ServiceType::S3,
            ServiceType::Rustfs,
            ServiceType::Kv,
            ServiceType::Blob,
            ServiceType::Minio,
        ]
    }

    /// Returns a Vec containing string representations of all available service types
    pub fn get_all_strings() -> Vec<String> {
        Self::get_all()
            .into_iter()
            .map(|st| st.to_string())
            .collect()
    }
}

/// A service type name no engine has.
#[derive(Debug, thiserror::Error)]
#[error("Invalid service value: Invalid service type: {service_type}")]
pub struct ServiceTypeParseError {
    pub service_type: String,
}

/// Result of a single probe against a managed external service.
/// Returned by `ExternalService::health_probe` so the monitor can record
/// structured health history without the trait having to know about DB rows.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct HealthProbeResult {
    pub status: HealthProbeStatus,
    /// Round-trip probe latency, when measurable.
    pub response_time_ms: Option<i32>,
    /// Present when status is `Degraded` or `Down`. Never contains secrets.
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum HealthProbeStatus {
    Operational,
    Degraded,
    Down,
}

impl HealthProbeStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Operational => "operational",
            Self::Degraded => "degraded",
            Self::Down => "down",
        }
    }
}

impl HealthProbeResult {
    pub fn operational(response_time_ms: Option<i32>) -> Self {
        Self {
            status: HealthProbeStatus::Operational,
            response_time_ms,
            error_message: None,
        }
    }

    pub fn down(message: impl Into<String>) -> Self {
        Self {
            status: HealthProbeStatus::Down,
            response_time_ms: None,
            error_message: Some(message.into()),
        }
    }

    pub fn degraded(message: impl Into<String>, response_time_ms: Option<i32>) -> Self {
        Self {
            status: HealthProbeStatus::Degraded,
            response_time_ms,
            error_message: Some(message.into()),
        }
    }
}

/// Request to initialize a service and provision one project's logical
/// resource on the node that owns the service container.
#[derive(Debug, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct RemoteRuntimeEnvRequest {
    pub service_config: ServiceConfig,
    pub project_slug: String,
    pub environment_slug: String,
}

/// Runtime environment returned after the logical resource is provisioned.
#[derive(Debug, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct RemoteRuntimeEnvResponse {
    pub environment: HashMap<String, String>,
}

/// Request for a provider-authenticated probe on the service's owning node.
/// The agent accepts no caller-selected command or free-form target.
#[derive(Debug, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoteHealthProbeRequest {
    pub service_config: ServiceConfig,
}

#[derive(Debug, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct RemoteHealthProbeResponse {
    pub result: HealthProbeResult,
}

/// A database container as Docker reports it, and its image's defaults
/// (which Docker merged into the container's environment and command).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContainerSnapshot {
    pub found: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inspect: Option<ContainerInspectResponse>,
    #[serde(default)]
    pub image_env: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_cmd: Option<Vec<String>>,
}

/// Where on its node an instance is attached. The only part of a database
/// request that differs by node, on purpose.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NodeAttachment {
    /// The interface the port is published on: loopback on the control
    /// plane, the private address on a worker.
    pub host_ip: String,
    pub network: Option<String>,
    /// Resolvers: the overlay's on a worker, Docker's default otherwise.
    pub dns: Option<Vec<String>>,
}

/// The body of `POST /agent/services/workload`, as the agent reads it: the
/// plan stays opaque until [`NodeServiceRuntime::admit_workload`] parses and
/// admits it. The control plane's side is
/// `temps_providers::remote_service_client::ServiceWorkloadCreateRequest`.
///
/// `Debug` prints the names of the secrets, never their values.
#[derive(Clone, Serialize, Deserialize)]
pub struct WorkloadCreateBody {
    pub service_name: String,
    pub plan: serde_json::Value,
    #[serde(default)]
    pub secrets: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<String>,
    #[serde(default)]
    pub replace_existing: bool,
}

impl std::fmt::Debug for WorkloadCreateBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkloadCreateBody")
            .field("service_name", &self.service_name)
            .field("plan", &self.plan)
            .field("secrets", &self.secrets.keys().collect::<Vec<_>>())
            .field("network", &self.network)
            .field("replace_existing", &self.replace_existing)
            .finish()
    }
}

/// A failed engine operation. `message` is the engine's own error text, with
/// the service, container and operation it names.
#[derive(Debug, thiserror::Error)]
pub enum NodeServiceError {
    /// The workload plan is not a plan at all.
    #[error("{message}")]
    MalformedPlan { message: String },
    /// Admission refused the plan.
    #[error("{message}")]
    Refused { container: String, message: String },
    /// The engine ran and failed.
    #[error("{message}")]
    Engine { message: String },
}

/// Runs once between creating a container and starting it, with its id.
pub type AfterCreate<'a> =
    Box<dyn FnOnce(String) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> + Send + 'a>;

/// A database workload plan that passed admission on this node.
#[async_trait]
pub trait AdmittedWorkload: Send + Sync {
    fn container_name(&self) -> &str;
    fn engine(&self) -> String;
    /// The Docker volumes the plan mounts.
    fn volumes(&self) -> Vec<String>;
    /// The image to pull. Admission refuses anything but an OCI image.
    fn image(&self) -> Option<&str>;
    fn host_port(&self) -> Option<u16>;

    /// Create the container and start it.
    async fn create(
        &self,
        docker: &Docker,
        secrets: &BTreeMap<String, String>,
        node: &NodeAttachment,
        after_create: AfterCreate<'_>,
    ) -> Result<(), NodeServiceError>;

    /// Replace the running container through the recreate guard, which keeps
    /// the original until the replacement is healthy on the same data.
    async fn replace(
        &self,
        docker: &Docker,
        service_name: &str,
        secrets: &BTreeMap<String, String>,
        node: &NodeAttachment,
        after_create: AfterCreate<'_>,
    ) -> Result<(), NodeServiceError>;
}

/// The engine operations a worker runs on the services it hosts.
#[async_trait]
pub trait NodeServiceRuntime: Send + Sync {
    /// Parse `plan` and admit it. A request is never trusted to have been
    /// admitted by its sender, so every implementation must run admission.
    fn admit_workload(
        &self,
        plan: serde_json::Value,
    ) -> Result<Box<dyn AdmittedWorkload>, NodeServiceError>;

    /// Inspect a container and its image. A missing container is
    /// `found: false`, not an error.
    async fn container_snapshot(
        &self,
        docker: &Docker,
        container_name: &str,
    ) -> Result<ContainerSnapshot, NodeServiceError>;

    /// Initialize a service and provision one project's logical resource,
    /// returning its runtime environment.
    async fn provision_runtime_environment(
        &self,
        name: String,
        service_type: ServiceType,
        config: ServiceConfig,
        project_slug: &str,
        environment_slug: &str,
        docker: Arc<Docker>,
    ) -> Result<HashMap<String, String>, NodeServiceError>;

    /// Revoke a project environment's managed S3 credentials.
    async fn revoke_runtime_environment(
        &self,
        name: String,
        service_type: ServiceType,
        config: ServiceConfig,
        project_slug: &str,
        environment_slug: &str,
        docker: Arc<Docker>,
    ) -> Result<(), NodeServiceError>;

    /// Run the provider's own health probe for `config`.
    async fn probe_service_health(
        &self,
        config: ServiceConfig,
        docker: Arc<Docker>,
    ) -> Result<HealthProbeResult, NodeServiceError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_type_names_round_trip_and_unknown_ones_are_named() {
        for service_type in ServiceType::get_all() {
            let parsed = ServiceType::from_str(&service_type.to_string()).expect("known name");
            assert_eq!(parsed, service_type);
        }
        let error = ServiceType::from_str("oracle").expect_err("unknown name");
        assert_eq!(
            error.to_string(),
            "Invalid service value: Invalid service type: oracle"
        );
    }

    /// The agent reads what the control plane sends, field for field, and a
    /// log line of the body never carries a secret value.
    #[test]
    fn workload_body_parses_with_defaults_and_hides_secret_values() {
        let body: WorkloadCreateBody = serde_json::from_value(serde_json::json!({
            "service_name": "db",
            "plan": { "container_name": "temps-db" },
            "secrets": { "POSTGRES_PASSWORD": "hunter2" },
        }))
        .expect("parse body");
        assert!(!body.replace_existing);
        assert_eq!(body.network, None);
        let logged = format!("{body:?}");
        assert!(logged.contains("POSTGRES_PASSWORD"));
        assert!(!logged.contains("hunter2"));
    }

    #[test]
    fn health_probe_results_carry_their_status() {
        assert_eq!(
            HealthProbeResult::operational(Some(3)).status,
            HealthProbeStatus::Operational
        );
        let down = HealthProbeResult::down("refused");
        assert_eq!(down.status.as_str(), "down");
        assert_eq!(down.error_message.as_deref(), Some("refused"));
        assert_eq!(
            HealthProbeResult::degraded("slow", Some(900)).status,
            HealthProbeStatus::Degraded
        );
    }
}
