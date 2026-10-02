// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Temps Agent — lightweight HTTP server wrapping the local Docker runtime.
//!
//! Runs on worker nodes. Exposes a small bearer-token–authenticated API that
//! the control plane (or `RemoteNodeDeployer`) calls to manage containers
//! and external services.

pub mod auth;
pub mod build_handler;
mod exec_timeout;
pub mod handlers;
pub mod internal_proxy;
pub mod network_sync;
mod output_buffer;
pub mod public_ingress;
pub mod route_store;
pub mod route_sync_client;
pub mod sandbox_handlers;
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

    #[error("TLS configuration failed ({context}): {reason}")]
    TlsConfig { context: String, reason: String },

    #[error(
        "Cannot resolve the agent data directory from dns_data_dir '{dns_data_dir}': {reason}"
    )]
    DataDirResolution {
        dns_data_dir: String,
        reason: String,
    },
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
    /// Container platform of this node's Docker daemon (`linux/amd64`,
    /// `linux/arm64`). The control plane reads this as a fallback when the
    /// `nodes` row has no architecture yet (agent upgraded but not yet
    /// heartbeated), so it can validate an image before transferring it.
    #[serde(default)]
    pub platform: String,
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
    /// Path to the per-node leaf certificate (PEM) the agent serves as its TLS
    /// server cert (ADR-020 WS-2.1). `None` => the agent serves plaintext HTTP
    /// (legacy / not-yet-enrolled-with-mTLS). `#[serde(default)]` so old
    /// `agent.json` files without these fields still parse.
    #[serde(default)]
    pub tls_cert_path: Option<std::path::PathBuf>,
    /// Path to the node private key (PEM) for the leaf above. Never leaves the node.
    #[serde(default)]
    pub tls_key_path: Option<std::path::PathBuf>,
    /// Path to the cluster CA certificate (PEM), used as the single root that
    /// verifies the control plane's client certificate.
    #[serde(default)]
    pub cluster_ca_path: Option<std::path::PathBuf>,
    /// Refuse to start the agent listener without a complete mTLS identity.
    /// Newly enrolled workers set this to `true`. The serde default remains
    /// `false` so legacy `agent.json` files can be upgraded deliberately.
    #[serde(default)]
    pub require_mtls: bool,
    /// Network device the VXLAN overlay should bind to as its underlay
    /// parent (e.g. `enp6s0`). `None` (the default) auto-detects the
    /// device carrying the host's IPv4 default route at startup — set
    /// this only when a host has multiple candidate interfaces and the
    /// default route doesn't point at the one that should carry overlay
    /// traffic. `#[serde(default)]` so older `agent.json` files without
    /// this field still parse.
    #[serde(default)]
    pub underlay_dev: Option<String>,
    /// Optional MTU ceiling for the selected underlay. When absent, the
    /// agent reads the interface MTU from the kernel. A configured value can
    /// lower that detected ceiling for tunnels with a smaller path MTU, but
    /// it can never raise the overlay beyond what the link supports.
    #[serde(default)]
    pub underlay_mtu: Option<u32>,
    /// This node's private/underlay address as registered with the control
    /// plane (`nodes.private_address`) — the WireGuard tunnel IP assigned by
    /// the relay, or the user-supplied address in direct mode. Always an IP
    /// already bound to a local interface by the time `temps agent` starts,
    /// since relay mode configures the WireGuard interface and direct mode
    /// requires the operator's networking to already own it.
    ///
    /// Used to bind published Docker container ports to this address
    /// instead of `0.0.0.0`, so deployed app containers are reachable only
    /// over the private/overlay network (where the control-plane proxy
    /// connects from) and never on the node's public interface.
    /// `#[serde(default)]` so `agent.json` files saved before this field
    /// existed still parse as `None` rather than failing deserialization —
    /// but `temps agent`'s config resolution then hard-errors at startup
    /// when it's missing (see `resolve_config` in `temps-cli`), directing
    /// the operator to re-run `temps join`. There is no insecure fallback:
    /// `build_router`'s own defensive fallback for a `None` config
    /// substitutes loopback (`127.0.0.1`), never `0.0.0.0` — and is
    /// unreachable in the real `temps agent` binary, since `resolve_config`
    /// always rejects a `None` config before `build_router` is called.
    #[serde(default)]
    pub private_address: Option<String>,
    /// Explicit public interface address for HTTP/HTTPS ingress. `None` keeps
    /// public listeners disabled even if the control-plane toggle is on.
    #[serde(default)]
    pub public_ingress_address: Option<std::net::IpAddr>,
    #[serde(default = "default_public_http_port")]
    pub public_ingress_http_port: u16,
    #[serde(default = "default_public_https_port")]
    pub public_ingress_https_port: u16,
    /// X25519 private key used only to decrypt this node's certificate bundles.
    #[serde(default)]
    pub public_ingress_private_key: Option<String>,
}

fn default_public_http_port() -> u16 {
    80
}
fn default_public_https_port() -> u16 {
    443
}

fn default_dns_data_dir() -> std::path::PathBuf {
    std::path::PathBuf::from("/var/lib/temps/dns")
}

/// Sandbox work root (ADR-048) for a given `dns_data_dir`:
/// `<agent data dir>/sandboxes`, where the agent data dir is the parent of
/// `dns_data_dir` (`temps agent` always sets it to `<agent data dir>/dns`).
///
/// The result is always absolute: sandbox work dirs become Docker bind-mount
/// sources, and Docker rejects a relative source, so a relative
/// `TEMPS_DATA_DIR` (e.g. `data`) is resolved against the current directory
/// here rather than surfacing as a failed create on every sandbox request.
/// A `dns_data_dir` with no parent directory (empty, or a filesystem root)
/// is rejected instead of being replaced by a guessed default.
pub fn sandbox_work_root_for(
    dns_data_dir: &std::path::Path,
) -> Result<std::path::PathBuf, AgentError> {
    let absolute = absolute_dns_data_dir(dns_data_dir)?;
    let data_dir = absolute
        .parent()
        .ok_or_else(|| AgentError::DataDirResolution {
            dns_data_dir: dns_data_dir.display().to_string(),
            reason: format!(
                "'{}' is a filesystem root and has no parent agent data directory; \
                 expected '<agent data dir>/dns'",
                absolute.display()
            ),
        })?;
    Ok(data_dir.join("sandboxes"))
}

/// `dns_data_dir` made absolute against the current directory, without
/// touching the filesystem (no symlink resolution, no existence check).
fn absolute_dns_data_dir(dns_data_dir: &std::path::Path) -> Result<std::path::PathBuf, AgentError> {
    if dns_data_dir.as_os_str().is_empty() {
        return Err(AgentError::DataDirResolution {
            dns_data_dir: String::new(),
            reason: "the path is empty; expected '<agent data dir>/dns'".to_string(),
        });
    }
    std::path::absolute(dns_data_dir).map_err(|e| AgentError::DataDirResolution {
        dns_data_dir: dns_data_dir.display().to_string(),
        reason: format!("failed to make the path absolute against the current directory: {e}"),
    })
}

impl AgentConfig {
    /// Resolve this config's on-disk paths once at startup, before the agent
    /// serves any request: `dns_data_dir` is made absolute in place and the
    /// sandbox work root derived from it is validated. Fails with
    /// [`AgentError::DataDirResolution`] when either cannot be resolved, so a
    /// misconfigured data dir stops `temps agent` with a clear error instead
    /// of failing every sandbox create later. Already-absolute paths are
    /// left unchanged.
    ///
    /// Returns the effective sandbox work root, which is also what
    /// [`AgentConfig::sandbox_work_root`] returns afterwards.
    pub fn resolve_paths(&mut self) -> Result<std::path::PathBuf, AgentError> {
        let work_root = sandbox_work_root_for(&self.dns_data_dir)?;
        self.dns_data_dir = absolute_dns_data_dir(&self.dns_data_dir)?;
        tracing::info!(
            node = %self.node_name,
            dns_data_dir = %self.dns_data_dir.display(),
            sandbox_work_root = %work_root.display(),
            "Resolved agent data paths; sandbox work directories on this node live under sandbox_work_root"
        );
        Ok(work_root)
    }

    /// Root for the work directories of sandboxes hosted on this node
    /// (ADR-048): `<agent data dir>/sandboxes`, always absolute.
    ///
    /// `temps agent` calls [`AgentConfig::resolve_paths`] at startup, which
    /// fails fast on a data dir that cannot be resolved, so the error branch
    /// here is unreachable for a running agent. It is still handled without
    /// producing a relative path: a config that skipped `resolve_paths` gets
    /// `/var/lib/temps/sandboxes` and an error log naming the cause, never a
    /// relative bind-mount source.
    pub fn sandbox_work_root(&self) -> std::path::PathBuf {
        match sandbox_work_root_for(&self.dns_data_dir) {
            Ok(work_root) => work_root,
            Err(error) => {
                let fallback = std::path::PathBuf::from("/var/lib/temps/sandboxes");
                tracing::error!(
                    node = %self.node_name,
                    %error,
                    fallback = %fallback.display(),
                    "Sandbox work root could not be resolved from dns_data_dir; \
                     using the default. AgentConfig::resolve_paths should have \
                     rejected this config at startup."
                );
                fallback
            }
        }
    }
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

// ---------------------------------------------------------------------------
// Image pull request/response types
// ---------------------------------------------------------------------------

/// Request to pull an image from a registry on this worker node.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct PullImageRequest {
    /// Image reference, e.g. `"ghcr.io/org/app:v1.0"` or `"nginx:latest"`.
    pub image: String,
    /// Optional registry credentials for private registries.
    /// When absent the Docker daemon uses whatever credentials it has cached
    /// (e.g. from a prior `docker login`). When present the credentials are
    /// forwarded to the daemon via the `X-Registry-Auth` header; they are
    /// **never** logged or echoed in error messages.
    #[serde(default)]
    pub credentials: Option<RegistryCredentials>,
}

/// Registry credentials for private-registry access.
///
/// Mirrors the fields of [`bollard::auth::DockerCredentials`]. `password` and
/// `identity_token` are intentionally excluded from `Debug` output so they do
/// not appear in log files.
#[derive(Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct RegistryCredentials {
    /// Registry username.
    #[serde(default)]
    pub username: Option<String>,
    /// Password or access-token for the registry user.
    /// **Never logged.**
    #[serde(default)]
    pub password: Option<String>,
    /// OAuth/OIDC identity token (mutually exclusive with `username`/`password`).
    /// **Never logged.**
    #[serde(default)]
    pub identity_token: Option<String>,
    /// Registry server address, e.g. `"ghcr.io"`. Derived from the image
    /// reference when absent.
    #[serde(default)]
    pub server_address: Option<String>,
}

impl std::fmt::Debug for RegistryCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegistryCredentials")
            .field("username", &self.username)
            .field("password", &"[redacted]")
            .field("identity_token", &"[redacted]")
            .field("server_address", &self.server_address)
            .finish()
    }
}

/// Successful result of pulling an image from a registry.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct PullImageResponse {
    /// Resolved image ID, e.g. `"sha256:abc123..."`.
    pub image_id: String,
    /// Registry digest, e.g. `"sha256:abc123..."`, when the registry reported
    /// one. `null` for images that were already present locally before the
    /// pull (or when the registry did not include a digest in the manifest).
    pub digest: Option<String>,
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
            tls_cert_path: None,
            tls_key_path: None,
            cluster_ca_path: None,
            require_mtls: false,
            underlay_dev: None,
            underlay_mtu: None,
            private_address: Some("10.100.0.2".to_string()),
            public_ingress_address: None,
            public_ingress_http_port: 80,
            public_ingress_https_port: 443,
            public_ingress_private_key: None,
        };

        let json = serde_json::to_string(&config).unwrap();
        let parsed: AgentConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.node_name, "worker-1");
        assert_eq!(parsed.node_id, 1);
        assert!(!parsed.require_mtls);
        assert_eq!(parsed.private_address.as_deref(), Some("10.100.0.2"));
    }

    #[test]
    fn test_agent_config_without_private_address_remains_compatible() {
        let json = r#"{
            "listen_address": "0.0.0.0:3100",
            "token": "test-token",
            "node_name": "worker-1",
            "control_plane_url": "https://control:3000",
            "node_id": 1
        }"#;

        let parsed: AgentConfig = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.private_address, None);
        assert_eq!(parsed.public_ingress_address, None);
        assert_eq!(parsed.public_ingress_http_port, 80);
        assert_eq!(parsed.public_ingress_https_port, 443);
    }

    #[test]
    fn test_agent_config_without_underlay_mtu_remains_compatible() {
        let json = r#"{
            "listen_address":"0.0.0.0:3100",
            "token":"test-token",
            "node_name":"worker-1",
            "control_plane_url":"https://control:3000",
            "node_id":1,
            "labels":{},
            "dns_data_dir":"/tmp/temps-dns"
        }"#;

        let parsed: AgentConfig = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.underlay_mtu, None);
        assert!(!parsed.require_mtls);
    }

    fn config_with_dns_data_dir(dns_data_dir: &str) -> AgentConfig {
        let json = serde_json::json!({
            "listen_address": "0.0.0.0:3100",
            "token": "test-token",
            "node_name": "worker-1",
            "control_plane_url": "https://control:3000",
            "node_id": 1,
            "dns_data_dir": dns_data_dir,
        });
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn test_sandbox_work_root_relative_data_dir_becomes_absolute() {
        // TEMPS_DATA_DIR=data => dns_data_dir = data/dns. The work root must
        // not be the relative `data/sandboxes`, which Docker rejects as a
        // bind-mount source.
        let cwd = std::env::current_dir().unwrap();
        let mut config = config_with_dns_data_dir("data/dns");

        let expected = cwd.join("data").join("sandboxes");
        assert_eq!(config.sandbox_work_root(), expected);

        let resolved = config.resolve_paths().unwrap();
        assert!(resolved.is_absolute());
        assert_eq!(resolved, expected);
        assert_eq!(config.dns_data_dir, cwd.join("data").join("dns"));
        assert_eq!(config.sandbox_work_root(), expected);
    }

    #[test]
    fn test_sandbox_work_root_single_component_relative_dns_dir() {
        // `dns` has an empty (not missing) parent; it must resolve to the
        // current directory rather than a relative `sandboxes`.
        let cwd = std::env::current_dir().unwrap();
        let mut config = config_with_dns_data_dir("dns");

        assert_eq!(config.resolve_paths().unwrap(), cwd.join("sandboxes"));
        assert_eq!(config.sandbox_work_root(), cwd.join("sandboxes"));
    }

    #[test]
    fn test_sandbox_work_root_absolute_data_dir_unchanged() {
        let mut config = config_with_dns_data_dir("/var/lib/temps/dns");

        let resolved = config.resolve_paths().unwrap();
        assert_eq!(
            resolved,
            std::path::PathBuf::from("/var/lib/temps/sandboxes")
        );
        assert_eq!(
            config.dns_data_dir,
            std::path::PathBuf::from("/var/lib/temps/dns")
        );
        assert_eq!(config.sandbox_work_root(), resolved);

        // The serde default is absolute too.
        let default_config = AgentConfig {
            dns_data_dir: default_dns_data_dir(),
            ..config
        };
        assert_eq!(
            default_config.sandbox_work_root(),
            std::path::PathBuf::from("/var/lib/temps/sandboxes")
        );
    }

    #[test]
    fn test_sandbox_work_root_dns_dir_directly_under_root() {
        let mut config = config_with_dns_data_dir("/dns");
        assert_eq!(
            config.resolve_paths().unwrap(),
            std::path::PathBuf::from("/sandboxes")
        );
    }

    #[test]
    fn test_resolve_paths_rejects_filesystem_root_dns_dir() {
        let mut config = config_with_dns_data_dir("/");

        let error = config.resolve_paths().unwrap_err();
        assert!(
            matches!(
                &error,
                AgentError::DataDirResolution { dns_data_dir, reason }
                    if dns_data_dir == "/" && reason.contains("filesystem root")
            ),
            "unexpected error: {error}"
        );
        // Not rewritten on failure.
        assert_eq!(config.dns_data_dir, std::path::PathBuf::from("/"));
        // The infallible accessor never yields a relative path either.
        let fallback = config.sandbox_work_root();
        assert!(fallback.is_absolute());
        assert_eq!(
            fallback,
            std::path::PathBuf::from("/var/lib/temps/sandboxes")
        );
    }

    #[test]
    fn test_resolve_paths_rejects_empty_dns_dir() {
        let mut config = config_with_dns_data_dir("");

        let error = config.resolve_paths().unwrap_err();
        assert!(
            matches!(
                &error,
                AgentError::DataDirResolution { reason, .. } if reason.contains("empty")
            ),
            "unexpected error: {error}"
        );
        assert!(config.sandbox_work_root().is_absolute());
    }
}
