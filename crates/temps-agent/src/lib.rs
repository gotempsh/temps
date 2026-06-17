//! Temps Agent — lightweight HTTP server wrapping the local Docker runtime.
//!
//! Runs on worker nodes. Exposes a small bearer-token–authenticated API that
//! the control plane (or `RemoteNodeDeployer`) calls to manage containers
//! and external services.

pub mod auth;
pub mod handlers;
pub mod internal_proxy;
pub mod network_sync;
pub mod route_store;
pub mod route_sync_client;
pub mod server;
pub mod service_handlers;

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum AgentError {
    #[error("Container operation failed for '{container_id}': {reason}")]
    ContainerOperation {
        container_id: String,
        reason: String,
    },

    #[error("Image operation failed for '{image_name}': {reason}")]
    ImageOperation { image_name: String, reason: String },

    #[error("Authentication failed: {0}")]
    AuthenticationFailed(String),

    #[error("Agent server error: {0}")]
    ServerError(String),

    #[error("Service operation failed for '{service_name}': {reason}")]
    ServiceOperation {
        service_name: String,
        reason: String,
    },

    #[error("Deployer error: {0}")]
    Deployer(#[from] temps_deployer::DeployerError),

    #[error("Builder error: {0}")]
    Builder(#[from] temps_deployer::BuilderError),

    #[error("Docker error: {0}")]
    Docker(String),
}

/// Health report sent in heartbeats and returned from GET /agent/health.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct NodeHealthReport {
    /// CPU usage percentage (0–100)
    pub cpu_percent: f64,
    /// Memory used in bytes
    pub memory_used_bytes: u64,
    /// Total memory in bytes
    pub memory_total_bytes: u64,
    /// Disk used in bytes
    pub disk_used_bytes: u64,
    /// Disk total in bytes
    pub disk_total_bytes: u64,
    /// Number of running containers
    pub running_containers: u64,
}

/// Configuration for the agent server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Listen address, e.g. "0.0.0.0:3100"
    pub listen_address: String,
    /// Pre-shared bearer token for authenticating requests from control plane
    pub token: String,
    /// Node name
    pub node_name: String,
    /// Control plane URL for registration and heartbeats
    pub control_plane_url: String,
    /// Node ID assigned by the control plane (used for heartbeat endpoint)
    pub node_id: i32,
    /// Node labels for scheduling (e.g., {"region": "us-east", "gpu": "true"}).
    /// Sent in every heartbeat so the control plane has up-to-date label info.
    #[serde(default)]
    pub labels: serde_json::Value,
    /// Directory for the per-node DNS resolver's zone snapshot
    /// (`<dir>/zone.json`, ADR-011). Defaults to `/var/lib/temps/dns` on
    /// Linux. The resolver tolerates missing/unreadable snapshots — start-up
    /// proceeds with an empty zone in that case.
    #[serde(default = "default_dns_data_dir")]
    pub dns_data_dir: std::path::PathBuf,
}

fn default_dns_data_dir() -> std::path::PathBuf {
    std::path::PathBuf::from("/var/lib/temps/dns")
}

// ---------------------------------------------------------------------------
// Service operation request/response types
// ---------------------------------------------------------------------------

/// Request to create an external service container on this node.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ServiceCreateRequest {
    /// Service name (used for container naming)
    pub name: String,
    /// Service type (postgres, redis, mongodb, s3)
    pub service_type: String,
    /// Docker image to use
    pub image: String,
    /// Environment variables for the container
    pub environment: std::collections::HashMap<String, String>,
    /// Port mappings (host_port -> container_port)
    pub port_mappings: Vec<ServicePortMapping>,
    /// Volume mounts (volume_name -> container_path)
    pub volumes: std::collections::HashMap<String, String>,
    /// Docker network to attach to
    #[serde(default)]
    pub network: Option<String>,
    /// Optional command override
    #[serde(default)]
    pub command: Option<Vec<String>>,
    /// Optional cgroup limits applied to the container. `None` = unlimited.
    /// Older control planes that don't send this field still parse correctly
    /// thanks to `#[serde(default)]`.
    #[serde(default)]
    pub resource_limits: Option<ServiceResourceLimits>,
}

/// Subset of bollard `HostConfig` fields exposed for runtime caps. Mirrors
/// `temps_providers::externalsvc::ResourceLimits` over the wire — keeping
/// them as separate structs avoids coupling the agent crate to providers.
#[derive(Debug, Clone, Default, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ServiceResourceLimits {
    /// Hard memory limit in MiB. None = unlimited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_mb: Option<i64>,
    /// Memory + swap limit in MiB. None = unlimited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_swap_mb: Option<i64>,
    /// CPU quota in nano-cpus (1e9 = 1 full CPU). None = unlimited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nano_cpus: Option<i64>,
    /// Relative CPU weight (default 1024). Only used when `nano_cpus` is None.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_shares: Option<i64>,
    /// Shared memory (/dev/shm) size in MiB. None = Docker default (64 MiB).
    /// Create-time only; changing it requires recreating the container.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shm_size_mb: Option<i64>,
}

/// Port mapping for a service container.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ServicePortMapping {
    pub host_port: u16,
    pub container_port: u16,
}

/// Response after creating a service container.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ServiceCreateResponse {
    pub container_id: String,
    pub container_name: String,
    pub host_port: u16,
    /// Container's IP on the `temps-overlay` network, when the container
    /// is attached to it (multi-host deployments only). NULL on
    /// single-host clusters and when the inspect call fails — callers
    /// treat NULL as "fall back to legacy single-host routing".
    /// Used by the control plane to populate `service_members.compute_ip`
    /// and the DNS registry's A record (ADR-011).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compute_ip: Option<String>,
}

/// Request to execute a command inside a service container (for backups, etc.).
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ServiceExecRequest {
    /// Container name or ID
    pub container_name: String,
    /// Command to execute
    pub command: Vec<String>,
    /// Environment variables for the exec session
    #[serde(default)]
    pub environment: std::collections::HashMap<String, String>,
    /// Run as this user (e.g., "postgres")
    #[serde(default)]
    pub user: Option<String>,
    /// Detach and run in background
    #[serde(default)]
    pub detach: bool,
}

/// Response from a container exec operation.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ServiceExecResponse {
    pub exit_code: i64,
    pub stdout: String,
    pub stderr: String,
}

/// Request to back up a service directly to S3.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ServiceBackupRequest {
    /// Container name of the service to back up
    pub container_name: String,
    /// Service type (postgres, redis, mongodb)
    pub service_type: String,
    /// S3 credentials for upload (distributed from control plane)
    pub s3: S3CredentialsPayload,
    /// S3 key prefix for this backup
    pub s3_path: String,
    /// Backup method (e.g., "pg_dump", "walg", "rdb_copy")
    #[serde(default)]
    pub method: Option<String>,
}

/// S3 credentials distributed from the control plane.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct S3CredentialsPayload {
    pub access_key_id: String,
    pub secret_key: String,
    pub region: String,
    pub endpoint: Option<String>,
    pub bucket_name: String,
    pub force_path_style: bool,
}

/// Response after a backup completes.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ServiceBackupResponse {
    pub s3_location: String,
    pub size_bytes: u64,
    pub compression_type: String,
    pub checksum: Option<String>,
}

/// Request to restore a service from S3.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ServiceRestoreRequest {
    /// Container name of the service to restore into
    pub container_name: String,
    /// Service type (postgres, redis, mongodb)
    pub service_type: String,
    /// S3 credentials
    pub s3: S3CredentialsPayload,
    /// S3 key of the backup to restore
    pub s3_location: String,
    /// Compression type of the backup
    #[serde(default)]
    pub compression_type: Option<String>,
}

/// Status of a service on this node.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ServiceStatus {
    pub container_name: String,
    pub container_id: Option<String>,
    pub running: bool,
    pub health: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_service_create_request_serialization() {
        let req = ServiceCreateRequest {
            name: "postgres-main".to_string(),
            service_type: "postgres".to_string(),
            image: "timescale/timescaledb-ha:pg18".to_string(),
            environment: HashMap::from([
                ("POSTGRES_PASSWORD".to_string(), "secret".to_string()),
                ("POSTGRES_DB".to_string(), "temps".to_string()),
            ]),
            port_mappings: vec![ServicePortMapping {
                host_port: 30001,
                container_port: 5432,
            }],
            volumes: HashMap::from([(
                "postgres-main_data".to_string(),
                "/var/lib/postgresql".to_string(),
            )]),
            network: Some("temps".to_string()),
            command: None,
            resource_limits: None,
        };

        let json = serde_json::to_string(&req).unwrap();
        let parsed: ServiceCreateRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.name, "postgres-main");
        assert_eq!(parsed.service_type, "postgres");
        assert_eq!(parsed.port_mappings.len(), 1);
        assert_eq!(parsed.port_mappings[0].host_port, 30001);
        assert!(parsed.resource_limits.is_none());
    }

    #[test]
    fn test_service_exec_request_defaults() {
        let json = r#"{"container_name":"pg","command":["pg_dump","-Fc"]}"#;
        let req: ServiceExecRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.container_name, "pg");
        assert_eq!(req.command, vec!["pg_dump", "-Fc"]);
        assert!(req.environment.is_empty());
        assert!(req.user.is_none());
        assert!(!req.detach);
    }

    #[test]
    fn test_s3_credentials_payload_serialization() {
        let creds = S3CredentialsPayload {
            access_key_id: "AKIA...".to_string(),
            secret_key: "secret".to_string(),
            region: "us-east-1".to_string(),
            endpoint: Some("https://s3.example.com".to_string()),
            bucket_name: "backups".to_string(),
            force_path_style: true,
        };

        let json = serde_json::to_string(&creds).unwrap();
        let parsed: S3CredentialsPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.bucket_name, "backups");
        assert!(parsed.force_path_style);
        assert_eq!(parsed.endpoint.unwrap(), "https://s3.example.com");
    }

    #[test]
    fn test_service_status_not_running() {
        let status = ServiceStatus {
            container_name: "redis-cache".to_string(),
            container_id: None,
            running: false,
            health: None,
        };
        assert!(!status.running);
        assert!(status.container_id.is_none());
    }

    #[test]
    fn test_service_backup_request_serialization() {
        let req = ServiceBackupRequest {
            container_name: "postgres-main".to_string(),
            service_type: "postgres".to_string(),
            s3: S3CredentialsPayload {
                access_key_id: "key".to_string(),
                secret_key: "secret".to_string(),
                region: "eu-central-1".to_string(),
                endpoint: None,
                bucket_name: "backups".to_string(),
                force_path_style: false,
            },
            s3_path: "external_services/postgres/main/2026/03/12/".to_string(),
            method: Some("pg_dump".to_string()),
        };

        let json = serde_json::to_string(&req).unwrap();
        let parsed: ServiceBackupRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.container_name, "postgres-main");
        assert_eq!(parsed.s3.region, "eu-central-1");
        assert_eq!(parsed.method.unwrap(), "pg_dump");
    }

    #[test]
    fn test_agent_config_serialization_with_defaults() {
        let config = AgentConfig {
            listen_address: "0.0.0.0:3100".to_string(),
            token: "test-token".to_string(),
            node_name: "worker-1".to_string(),
            control_plane_url: "https://control:3000".to_string(),
            node_id: 1,
            labels: serde_json::json!({}),
            dns_data_dir: default_dns_data_dir(),
        };

        let json = serde_json::to_string(&config).unwrap();
        let parsed: AgentConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.node_name, "worker-1");
        assert_eq!(parsed.node_id, 1);
    }
}
