// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Shared deployment configuration for projects and environments
//!
//! This module defines configuration structures that can be shared between
//! projects (as defaults) and environments (as overrides).

use sea_orm::FromJsonQueryResult;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use utoipa::ToSchema;

/// `pg_advisory_xact_lock` key that serializes every write of an
/// environment's or project's `target_nodes` with node removal.
///
/// A settings save validates its node IDs and then writes the whole
/// `deployment_config`; node removal rewrites every placement that names the
/// node and deletes it. Without one lock around both, a save could validate
/// against a node that is removed before it commits, or write back a copy of
/// the config read before the removal, and either way put a deleted ID back.
pub const NODE_PLACEMENT_LOCK_KEY: i64 = 0x5445_4D50_504C_4143;

/// Take [`NODE_PLACEMENT_LOCK_KEY`] for the rest of the current transaction.
///
/// Call it first in the transaction, before any row lock, so every holder
/// acquires its locks in the same order. Outside a transaction it is released
/// immediately and protects nothing.
pub async fn lock_node_placements<C: sea_orm::ConnectionTrait>(
    conn: &C,
) -> Result<(), sea_orm::DbErr> {
    conn.execute(sea_orm::Statement::from_sql_and_values(
        sea_orm::DatabaseBackend::Postgres,
        "SELECT pg_advisory_xact_lock($1)",
        [NODE_PLACEMENT_LOCK_KEY.into()],
    ))
    .await
    .map(|_| ())
}

/// Security configuration for projects and environments
///
/// This configuration can be set at three levels:
/// 1. Global (in settings table) - applies to all projects
/// 2. Project level - overrides global settings for specific project
/// 3. Environment level - overrides project settings for specific environment
///
/// The inheritance chain: Environment > Project > Global
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, FromJsonQueryResult)]
#[serde(rename_all = "camelCase")]
#[derive(Default)]
pub struct SecurityConfig {
    /// Enable/disable security features at this level
    /// If None, inherits from parent level
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,

    /// Security headers configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<SecurityHeadersConfig>,

    /// Rate limiting configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_limiting: Option<RateLimitConfig>,

    /// Attack mode configuration (future: "off", "challenge", "block")
    /// Placeholder for DDoS protection, bot detection, etc.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attack_mode: Option<String>,

    /// Challenge configuration (future: CAPTCHA, JS challenge, etc.)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub challenge_config: Option<ChallengeConfig>,

    /// Geographic restrictions (future: country blocking, etc.)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub geo_restrictions: Option<GeoRestrictionsConfig>,

    /// Password protection: shows an HTML password form before allowing access
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password_protection: Option<PasswordProtectionConfig>,
}

/// Security headers configuration (subset of global SecurityHeadersSettings)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, FromJsonQueryResult)]
#[serde(rename_all = "camelCase")]
pub struct SecurityHeadersConfig {
    /// Use a preset: "strict", "moderate", "permissive", "disabled", "custom"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preset: Option<String>,

    /// Custom CSP (only used if preset is "custom")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_security_policy: Option<String>,

    /// X-Frame-Options override
    #[serde(skip_serializing_if = "Option::is_none")]
    pub x_frame_options: Option<String>,

    /// HSTS override
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strict_transport_security: Option<String>,

    /// Referrer-Policy override
    #[serde(skip_serializing_if = "Option::is_none")]
    pub referrer_policy: Option<String>,
}

/// Rate limiting configuration (subset of global RateLimitSettings)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, FromJsonQueryResult)]
#[serde(rename_all = "camelCase")]
pub struct RateLimitConfig {
    /// Override rate limit per minute
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_requests_per_minute: Option<u32>,

    /// Override rate limit per hour
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_requests_per_hour: Option<u32>,

    /// Whitelist specific IPs for this project/environment
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub whitelist_ips: Vec<String>,

    /// Blacklist specific IPs for this project/environment
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blacklist_ips: Vec<String>,
}

/// Challenge configuration (future feature)
/// For CAPTCHA, JS challenges, proof-of-work, etc.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, FromJsonQueryResult)]
#[serde(rename_all = "camelCase")]
pub struct ChallengeConfig {
    /// Challenge type: "captcha", "js_challenge", "proof_of_work"
    pub challenge_type: String,

    /// Challenge difficulty level (1-10)
    pub difficulty: u8,

    /// Paths that require challenges
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub protected_paths: Vec<String>,
}

/// Geographic restrictions configuration (future feature)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, FromJsonQueryResult)]
#[serde(rename_all = "camelCase")]
pub struct GeoRestrictionsConfig {
    /// Block traffic from specific countries (ISO 3166-1 alpha-2 codes)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_countries: Vec<String>,

    /// Allow traffic only from specific countries
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_countries: Vec<String>,
}

/// Password protection configuration
///
/// When enabled, the proxy shows an HTML password form before allowing access.
/// After the user enters the correct password, an HMAC-signed cookie is set
/// so subsequent requests pass through without re-entering the password.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, FromJsonQueryResult)]
#[serde(rename_all = "camelCase")]
pub struct PasswordProtectionConfig {
    /// Whether password protection is enabled
    pub enabled: bool,

    /// The bcrypt-hashed password (never stored or returned in plaintext)
    pub password_hash: String,
}

/// Where a source deployment's image is built.
///
/// The control plane is the default and the historical behaviour. `Node`
/// moves the build — the most CPU- and memory-hungry step of a deployment —
/// onto a worker node so it stops competing with the proxy, the database
/// and running workloads on the control plane. A node labelled
/// `temps.sh/role=builder` is preferred; otherwise the node the deployment
/// is placed on builds it.
///
/// `Node` is a preference, not a requirement: when no worker can take the
/// build (none joined, none of the right architecture, or a
/// multi-architecture build) the build runs on the control plane and the
/// build log says why. A static site may build on a worker of any
/// architecture, since its output is read from that node and never runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BuildLocation {
    /// Build with the control plane's own Docker daemon.
    ControlPlane,
    /// Build on a worker node, falling back to the control plane.
    Node,
    /// Dispatch source builds to the installation CI executor. Fail closed.
    Ci,
}

/// Deployment configuration shared between projects and environments
///
/// This configuration can be set at the project level (as defaults) and
/// overridden at the environment level for specific deployments.
///
/// Note: Environment variables are managed separately and are not part of this config.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, FromJsonQueryResult)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentConfig {
    /// CPU request in microcores, where 1_000_000 = 1 full CPU core
    /// (e.g., 100_000 = 0.1 CPU, 500_000 = 0.5 CPU, 2_000_000 = 2 CPUs).
    /// NOT millicores — the deployer formats this as `{n}u` and converts
    /// `n / 1_000_000` cores into Docker nano_cpus.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_request: Option<i32>,

    /// CPU limit in microcores, where 1_000_000 = 1 full CPU core
    /// (e.g., 2_000_000 = 2 CPUs). NOT millicores. `None` = uncapped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_limit: Option<i32>,

    /// Memory request in megabytes (e.g., 128 = 128MB)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_request: Option<i32>,

    /// Memory limit in megabytes. Three-state semantics:
    /// - `None`     → inherit the parent layer (env inherits project, project
    ///   inherits the seeded default); used by the settings UI's "Use default".
    /// - `Some(0)`  → explicit **uncapped**: stop inheriting and run with no
    ///   memory limit. This is the deliberate escape hatch for dedicated
    ///   workloads, distinct from `None`.
    /// - `Some(n)`  → hard cap of `n` MB.
    ///
    /// `merge`/resolution keep `Some(0)` as a present value (it wins precedence
    /// over a parent cap), and the deployer collapses it to "no limit" before
    /// talking to Docker.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_limit: Option<i32>,

    /// Port exposed by the container
    /// If not specified, will be auto-detected from Docker image or default to 3000
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exposed_port: Option<i32>,

    /// Enable automatic deployments on git push.
    /// `None` = inherit from project config; `Some(true/false)` = explicit override.
    /// Stored as JSONB so absent key → `None` (inherit), never silently defaults to false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub automatic_deploy: Option<bool>,

    /// Enable performance metrics collection (speed insights)
    #[serde(default)]
    pub performance_metrics_enabled: bool,

    /// Enable session recording for analytics
    #[serde(default)]
    pub session_recording_enabled: bool,

    /// Enable container exec/shell access (disabled by default for security)
    #[serde(default)]
    pub container_exec_enabled: bool,

    /// Number of replicas/instances to run
    /// Defaults to 1 replica
    #[serde(default = "default_replicas")]
    pub replicas: i32,

    /// Security configuration (headers, rate limiting, attack mode, etc.)
    /// These settings inherit and override from parent level (Environment > Project > Global)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub security: Option<SecurityConfig>,

    /// Optional list of node IDs to deploy to. When set, replicas are distributed
    /// only across these nodes (round-robin). When None, the scheduler distributes
    /// across all active nodes (or deploys locally if no nodes exist).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_nodes: Option<Vec<i32>>,

    /// Label selector for node-based scheduling. Replicas are only deployed to
    /// nodes whose labels match the selector.
    ///
    /// Matching rules:
    /// - **Same key, array value** → OR: node must match any value
    /// - **Different keys** → AND: node must satisfy all keys
    ///
    /// Example: `{"region": ["us", "asia"], "gpu": "true"}`
    /// → (region=us OR region=asia) AND gpu=true
    ///
    /// Applied after `target_nodes` filtering (they stack).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_labels: Option<serde_json::Value>,

    /// Anti-affinity: spread replicas across different nodes.
    ///
    /// When enabled, the scheduler avoids placing two replicas of the same
    /// environment on the same node. If there are fewer eligible nodes than
    /// replicas, remaining replicas wrap around (best-effort spreading).
    ///
    /// Defaults to `true` — replicas spread by default.
    #[serde(default = "default_anti_affinity")]
    pub anti_affinity: bool,

    /// Build one image per architecture the eligible nodes run.
    ///
    /// `None`/`false` (the default) builds exactly once, on the control
    /// plane's native platform — byte-for-byte the behaviour of a
    /// single-architecture cluster. When enabled and the nodes this
    /// deployment could land on span more than one architecture, the build
    /// job produces one image per architecture; the non-native ones go
    /// through the daemon's `platform` option, which requires QEMU binfmt
    /// handlers registered on the control plane.
    ///
    /// **Opt-in on purpose.** Cross-architecture builds are emulated and
    /// substantially slower, and deriving them from cluster topology would
    /// mean a single node joining silently changes build behaviour for every
    /// deployment in the cluster. It also keeps the decision on operator
    /// config rather than on a value each node reports about itself.
    ///
    /// `Option<bool>` so an environment inherits the project's setting
    /// (`None`) or overrides it, matching `automatic_deploy`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cross_architecture_builds: Option<bool>,

    /// Where source images are built. `None` inherits (environment → project)
    /// and resolves to [`BuildLocation::ControlPlane`]; see
    /// [`DeploymentConfig::resolve_build_location`].
    ///
    /// Worker builds never receive build-argument values, so a Dockerfile
    /// that declares `ARG` fails on a node with an explicit error instead of
    /// building without its inputs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_location: Option<BuildLocation>,

    /// Enable on-demand mode (scale-to-zero).
    /// When enabled, containers are stopped after `idle_timeout_seconds` of no traffic
    /// and automatically started when a new request arrives.
    #[serde(default)]
    pub on_demand: bool,

    /// Seconds of inactivity before containers are stopped in on-demand mode.
    /// Only used when `on_demand` is true. Min: 60, Max: 86400 (24h).
    /// Default: 300 (5 minutes).
    #[serde(default = "default_idle_timeout")]
    pub idle_timeout_seconds: i32,

    /// Max seconds to wait for containers to start when waking from on-demand sleep.
    /// Requests return 503 if exceeded. Default: 30.
    #[serde(default = "default_wake_timeout")]
    pub wake_timeout_seconds: i32,

    /// Override for the proxy's upstream timeout on regular (non-streaming)
    /// HTTP requests to this project/environment, in seconds.
    /// `None` = inherit the global `request_timeouts.default_http_timeout_seconds`
    /// (which itself defaults to "no timeout"). `Some(0)` explicitly forces
    /// "no timeout" for this project/environment, overriding a nonzero
    /// global default. `Some(n)` for `n > 0` sets an explicit timeout,
    /// always clamped to the global hard ceiling
    /// (`request_timeouts.max_request_timeout_seconds`) at resolution time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_timeout_seconds: Option<i32>,

    /// Override for the proxy's idle timeout on Server-Sent Events streams to
    /// this project/environment, in seconds. `None` = inherit the global
    /// `request_timeouts.default_sse_idle_timeout_seconds`. `Some(0)`
    /// explicitly forces "no timeout". `Some(n)` for `n > 0` is clamped to
    /// the global hard ceiling at resolution time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sse_idle_timeout_seconds: Option<i32>,

    /// Override for the proxy's idle timeout on WebSocket connections to this
    /// project/environment, in seconds. `None` = inherit the global
    /// `request_timeouts.default_websocket_idle_timeout_seconds`. `Some(0)`
    /// explicitly forces "no timeout". `Some(n)` for `n > 0` is clamped to
    /// the global hard ceiling at resolution time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub websocket_idle_timeout_seconds: Option<i32>,

    /// Override for the proxy's cap on concurrent in-flight requests to this
    /// project/environment's upstream, independent of the timeout overrides
    /// above. `None` = inherit the global
    /// `connection_limits.default_max_concurrent_connections`. `Some(0)`
    /// explicitly forces "unlimited" for this project/environment, overriding
    /// a nonzero global default. `Some(n)` for `n > 0` sets an explicit cap —
    /// there is no global ceiling clamp here (unlike the timeout settings):
    /// an operator who has explicitly set a per-project value has made a
    /// deliberate choice and the platform should respect it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrent_connections: Option<i32>,

    /// How long, in seconds, a new container may take to start and pass its
    /// readiness check before the deployment fails. Covers apps that boot
    /// slowly (migrations, JIT warm-up, large model loads). `None` = the
    /// platform default ([`DEFAULT_HEALTH_CHECK_TIMEOUT_SECONDS`]); valid
    /// overrides are [`MIN_HEALTH_CHECK_TIMEOUT_SECONDS`]..=
    /// [`MAX_HEALTH_CHECK_TIMEOUT_SECONDS`]. Environments inherit the project
    /// value and may override it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health_check_timeout_seconds: Option<i32>,
    /// How long, in seconds, a superseded deployment's containers get to
    /// finish in-flight work after SIGTERM before they are killed, once a new
    /// deployment has gone live. Raise it for long requests (uploads,
    /// exports, streaming responses, WebSockets).
    /// An override is 1 to 3600 seconds. Absent on a project means the
    /// platform default of 10 seconds; absent on an environment means it
    /// inherits the project's value, else the platform default.
    // The numbers above are spelled out because this comment is the OpenAPI
    // description, where an intra-doc link would show as raw brackets. They
    // are DEFAULT/MIN/MAX_STOP_TIMEOUT_SECONDS, which a test keeps in step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_timeout_seconds: Option<i32>,
    /// Deploy-time probe; environment overrides project, repository config overrides both.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health: Option<temps_core::repo_config::HealthCheckConfig>,
}

/// Default readiness budget for a new deployment's containers, in seconds.
pub const DEFAULT_HEALTH_CHECK_TIMEOUT_SECONDS: i32 = 300;
/// Smallest accepted readiness-budget override, in seconds.
pub const MIN_HEALTH_CHECK_TIMEOUT_SECONDS: i32 = 30;
/// Largest accepted readiness-budget override, in seconds (1 hour).
pub const MAX_HEALTH_CHECK_TIMEOUT_SECONDS: i32 = 3600;

/// Default grace period between SIGTERM and SIGKILL when a superseded
/// deployment's containers are stopped, in seconds.
pub const DEFAULT_STOP_TIMEOUT_SECONDS: i32 = 10;
/// Smallest accepted stop grace period override, in seconds.
pub const MIN_STOP_TIMEOUT_SECONDS: i32 = 1;
/// Largest accepted stop grace period override, in seconds (1 hour).
pub const MAX_STOP_TIMEOUT_SECONDS: i32 = 3600;

/// Deployment configuration snapshot for deployments
///
/// This extends DeploymentConfig with environment variables to capture
/// the complete state of a deployment at the time it was created.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, FromJsonQueryResult)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentConfigSnapshot {
    /// CPU request in millicores
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_request: Option<i32>,

    /// CPU limit in millicores
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_limit: Option<i32>,

    /// Memory request in megabytes
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_request: Option<i32>,

    /// Memory limit in megabytes
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_limit: Option<i32>,

    /// Port exposed by the container
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exposed_port: Option<i32>,

    /// Environment variables used for this deployment
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub environment_variables: HashMap<String, String>,

    /// Enable automatic deployments on git push
    #[serde(default)]
    pub automatic_deploy: bool,

    /// Enable performance metrics collection
    #[serde(default)]
    pub performance_metrics_enabled: bool,

    /// Enable session recording
    #[serde(default)]
    pub session_recording_enabled: bool,

    /// Enable container exec/shell access
    #[serde(default)]
    pub container_exec_enabled: bool,

    /// Number of replicas
    #[serde(default = "default_replicas")]
    pub replicas: i32,
}

/// Most replicas one environment can ask for. Deploying allocates and starts
/// per replica, so an unbounded value exhausts the server's memory and takes
/// every other project down with it.
pub const MAX_REPLICAS: i32 = 100;

fn default_replicas() -> i32 {
    1
}

fn default_anti_affinity() -> bool {
    true
}

fn default_idle_timeout() -> i32 {
    300
}

fn default_wake_timeout() -> i32 {
    30
}

impl Default for DeploymentConfig {
    fn default() -> Self {
        Self {
            cpu_request: None,
            cpu_limit: None,
            memory_request: None,
            memory_limit: None,
            exposed_port: None,
            automatic_deploy: None,
            performance_metrics_enabled: false,
            session_recording_enabled: false,
            container_exec_enabled: false,
            replicas: 1,
            security: None,
            target_nodes: None,
            target_labels: None,
            anti_affinity: true,
            cross_architecture_builds: None,
            build_location: None,
            on_demand: false,
            idle_timeout_seconds: 300,
            wake_timeout_seconds: 30,
            request_timeout_seconds: None,
            sse_idle_timeout_seconds: None,
            websocket_idle_timeout_seconds: None,
            max_concurrent_connections: None,
            health_check_timeout_seconds: None,
            stop_timeout_seconds: None,
            health: None,
        }
    }
}

impl Default for DeploymentConfigSnapshot {
    fn default() -> Self {
        Self {
            cpu_request: None,
            cpu_limit: None,
            memory_request: None,
            memory_limit: None,
            exposed_port: None,
            environment_variables: HashMap::new(),
            automatic_deploy: false,
            performance_metrics_enabled: false,
            session_recording_enabled: false,
            container_exec_enabled: false,
            replicas: 1,
        }
    }
}

impl DeploymentConfig {
    /// Create a new deployment configuration with default values
    pub fn new() -> Self {
        Self::default()
    }
}

impl DeploymentConfigSnapshot {
    /// Create a snapshot from a DeploymentConfig and environment variables
    pub fn from_config(
        config: &DeploymentConfig,
        environment_variables: HashMap<String, String>,
    ) -> Self {
        Self {
            cpu_request: config.cpu_request,
            cpu_limit: config.cpu_limit,
            memory_request: config.memory_request,
            memory_limit: config.memory_limit,
            exposed_port: config.exposed_port,
            environment_variables,
            automatic_deploy: config.automatic_deploy.unwrap_or(false),
            performance_metrics_enabled: config.performance_metrics_enabled,
            session_recording_enabled: config.session_recording_enabled,
            container_exec_enabled: config.container_exec_enabled,
            replicas: config.replicas,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The stop timeout's field docs are its OpenAPI description, so they
    /// spell the bounds out instead of linking the constants; this keeps the
    /// two in step and the description free of intra-doc link brackets.
    #[test]
    fn stop_timeout_description_states_the_enforced_bounds() {
        let schema = serde_json::to_value(<DeploymentConfig as utoipa::PartialSchema>::schema())
            .expect("schema serializes");
        let description = schema["properties"]["stopTimeoutSeconds"]["description"]
            .as_str()
            .expect("stopTimeoutSeconds has a description")
            // Doc comment line breaks are kept; match the words, not the wrap.
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            description.contains(&format!(
                "default of {DEFAULT_STOP_TIMEOUT_SECONDS} seconds"
            )),
            "{description}"
        );
        assert!(
            description.contains(&format!(
                "{MIN_STOP_TIMEOUT_SECONDS} to {MAX_STOP_TIMEOUT_SECONDS} seconds"
            )),
            "{description}"
        );
        assert!(!description.contains("[`"), "{description}");
        // The same schema describes project and environment settings, where
        // an absent value means different things.
        assert!(
            description.contains("absent on an environment means it inherits the project's value"),
            "{description}"
        );
    }

    /// Rows written before the setting existed must keep deserializing, and
    /// the wire format is the documented snake_case value.
    #[test]
    fn test_build_location_serde() {
        let legacy: DeploymentConfig = serde_json::from_str("{}").expect("empty config parses");
        assert_eq!(legacy.build_location, None);
        assert!(!serde_json::to_string(&legacy)
            .expect("serializes")
            .contains("buildLocation"));

        let parsed: DeploymentConfig =
            serde_json::from_str(r#"{"buildLocation":"node"}"#).expect("node parses");
        assert_eq!(parsed.build_location, Some(BuildLocation::Node));
        assert_eq!(
            serde_json::to_value(BuildLocation::ControlPlane).expect("serializes"),
            serde_json::json!("control_plane")
        );
        assert!(serde_json::from_str::<DeploymentConfig>(r#"{"buildLocation":"cloud"}"#).is_err());
    }

    #[test]
    fn test_default_config() {
        let config = DeploymentConfig::default();
        assert_eq!(config.cpu_request, None);
        assert_eq!(config.cpu_limit, None);
        assert_eq!(config.automatic_deploy, None);
        assert!(!config.performance_metrics_enabled);
        assert!(!config.session_recording_enabled);
    }

    #[test]
    fn test_target_nodes_serialization() {
        let config = DeploymentConfig {
            target_nodes: Some(vec![1, 3, 5]),
            ..Default::default()
        };
        let json = serde_json::to_value(&config).unwrap();
        let deserialized: DeploymentConfig = serde_json::from_value(json).unwrap();
        assert_eq!(deserialized.target_nodes, Some(vec![1, 3, 5]));

        // target_nodes: None should be omitted from JSON
        let config_no_nodes = DeploymentConfig {
            target_nodes: None,
            ..Default::default()
        };
        let json2 = serde_json::to_value(&config_no_nodes).unwrap();
        assert!(!json2.as_object().unwrap().contains_key("target_nodes"));
    }

    #[test]
    fn request_timeout_fields_default_to_inherit() {
        let config = DeploymentConfig::default();
        assert_eq!(config.request_timeout_seconds, None);
        assert_eq!(config.sse_idle_timeout_seconds, None);
        assert_eq!(config.websocket_idle_timeout_seconds, None);
    }

    #[test]
    fn request_timeout_fields_round_trip_and_omit_when_none() {
        let config = DeploymentConfig {
            request_timeout_seconds: Some(45),
            sse_idle_timeout_seconds: Some(900),
            websocket_idle_timeout_seconds: Some(1200),
            ..Default::default()
        };
        let json = serde_json::to_value(&config).unwrap();
        let deserialized: DeploymentConfig = serde_json::from_value(json).unwrap();
        assert_eq!(deserialized, config);

        let default_config = DeploymentConfig::default();
        let json2 = serde_json::to_value(&default_config).unwrap();
        let obj = json2.as_object().unwrap();
        assert!(!obj.contains_key("requestTimeoutSeconds"));
        assert!(!obj.contains_key("sseIdleTimeoutSeconds"));
        assert!(!obj.contains_key("websocketIdleTimeoutSeconds"));
    }

    #[test]
    fn test_serialization() {
        let config = DeploymentConfig {
            cpu_request: Some(100),
            cpu_limit: Some(1000),
            memory_request: Some(128),
            memory_limit: Some(512),
            exposed_port: Some(3000),
            automatic_deploy: Some(true),
            performance_metrics_enabled: true,
            session_recording_enabled: false,
            replicas: 3,
            security: None,
            target_nodes: None,
            target_labels: None,
            anti_affinity: true,
            ..Default::default()
        };

        let json = serde_json::to_value(&config).unwrap();
        let deserialized: DeploymentConfig = serde_json::from_value(json).unwrap();

        assert_eq!(config, deserialized);
    }

    #[test]
    fn test_snapshot_from_config() {
        let config = DeploymentConfig {
            cpu_request: Some(100),
            cpu_limit: Some(1000),
            memory_request: Some(128),
            memory_limit: Some(512),
            exposed_port: Some(3000),
            automatic_deploy: Some(true),
            performance_metrics_enabled: true,
            session_recording_enabled: false,
            replicas: 2,
            security: None,
            target_nodes: None,
            target_labels: None,
            anti_affinity: true,
            ..Default::default()
        };

        let mut env_vars = HashMap::new();
        env_vars.insert("NODE_ENV".to_string(), "production".to_string());
        env_vars.insert("DB_HOST".to_string(), "localhost".to_string());

        let snapshot = DeploymentConfigSnapshot::from_config(&config, env_vars);

        assert_eq!(snapshot.cpu_request, Some(100));
        assert_eq!(snapshot.cpu_limit, Some(1000));
        assert_eq!(snapshot.memory_request, Some(128));
        assert_eq!(snapshot.memory_limit, Some(512));
        assert_eq!(snapshot.exposed_port, Some(3000));
        assert_eq!(snapshot.environment_variables.len(), 2);
        assert_eq!(
            snapshot.environment_variables.get("NODE_ENV"),
            Some(&"production".to_string())
        );
        assert_eq!(
            snapshot.environment_variables.get("DB_HOST"),
            Some(&"localhost".to_string())
        );
        assert!(snapshot.automatic_deploy);
        assert_eq!(snapshot.replicas, 2);
    }
}
