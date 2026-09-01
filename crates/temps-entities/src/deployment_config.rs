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

impl SecurityConfig {
    /// Create a new security configuration with default values
    pub fn new() -> Self {
        Self::default()
    }

    /// Merge this config with another, preferring values from `other`
    ///
    /// This is used for inheritance chain: Environment > Project > Global
    /// When resolving effective security config, call: global.merge(&project).merge(&environment)
    pub fn merge(&self, other: &SecurityConfig) -> SecurityConfig {
        SecurityConfig {
            enabled: other.enabled.or(self.enabled),
            headers: match (&self.headers, &other.headers) {
                (Some(base), Some(override_headers)) => Some(base.merge(override_headers)),
                (Some(base), None) => Some(base.clone()),
                (None, Some(override_headers)) => Some(override_headers.clone()),
                (None, None) => None,
            },
            rate_limiting: match (&self.rate_limiting, &other.rate_limiting) {
                (Some(base), Some(override_rl)) => Some(base.merge(override_rl)),
                (Some(base), None) => Some(base.clone()),
                (None, Some(override_rl)) => Some(override_rl.clone()),
                (None, None) => None,
            },
            attack_mode: other
                .attack_mode
                .clone()
                .or_else(|| self.attack_mode.clone()),
            challenge_config: other
                .challenge_config
                .clone()
                .or_else(|| self.challenge_config.clone()),
            geo_restrictions: other
                .geo_restrictions
                .clone()
                .or_else(|| self.geo_restrictions.clone()),
            password_protection: other
                .password_protection
                .clone()
                .or_else(|| self.password_protection.clone()),
        }
    }
}

impl SecurityHeadersConfig {
    /// Merge headers config, preferring values from `other`
    fn merge(&self, other: &SecurityHeadersConfig) -> SecurityHeadersConfig {
        SecurityHeadersConfig {
            preset: other.preset.clone().or_else(|| self.preset.clone()),
            content_security_policy: other
                .content_security_policy
                .clone()
                .or_else(|| self.content_security_policy.clone()),
            x_frame_options: other
                .x_frame_options
                .clone()
                .or_else(|| self.x_frame_options.clone()),
            strict_transport_security: other
                .strict_transport_security
                .clone()
                .or_else(|| self.strict_transport_security.clone()),
            referrer_policy: other
                .referrer_policy
                .clone()
                .or_else(|| self.referrer_policy.clone()),
        }
    }
}

impl RateLimitConfig {
    /// Merge rate limit config, preferring values from `other`
    fn merge(&self, other: &RateLimitConfig) -> RateLimitConfig {
        let mut merged_whitelist = self.whitelist_ips.clone();
        merged_whitelist.extend(other.whitelist_ips.clone());
        merged_whitelist.sort();
        merged_whitelist.dedup();

        let mut merged_blacklist = self.blacklist_ips.clone();
        merged_blacklist.extend(other.blacklist_ips.clone());
        merged_blacklist.sort();
        merged_blacklist.dedup();

        RateLimitConfig {
            max_requests_per_minute: other
                .max_requests_per_minute
                .or(self.max_requests_per_minute),
            max_requests_per_hour: other.max_requests_per_hour.or(self.max_requests_per_hour),
            whitelist_ips: merged_whitelist,
            blacklist_ips: merged_blacklist,
        }
    }
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
}

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
            on_demand: false,
            idle_timeout_seconds: 300,
            wake_timeout_seconds: 30,
            request_timeout_seconds: None,
            sse_idle_timeout_seconds: None,
            websocket_idle_timeout_seconds: None,
            max_concurrent_connections: None,
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

    /// Return a node selector only when it names at least one node.
    ///
    /// Empty lists are the API sentinel for clearing an environment override.
    /// Treating a legacy stored empty list as an override would either make the
    /// workload unschedulable or erase a project-level placement constraint.
    pub fn configured_target_nodes(&self) -> Option<&[i32]> {
        self.target_nodes
            .as_deref()
            .filter(|nodes| !nodes.is_empty())
    }

    /// Return a label selector only when it contains at least one label.
    /// Empty objects are the API sentinel for clearing an environment override.
    pub fn configured_target_labels(&self) -> Option<&serde_json::Value> {
        self.target_labels
            .as_ref()
            .filter(|labels| !labels.as_object().is_some_and(serde_json::Map::is_empty))
    }

    /// Merge this config with another, preferring values from `other`
    ///
    /// This is useful for merging environment-level config (other) with
    /// project-level config (self) to get the effective configuration.
    pub fn merge(&self, other: &DeploymentConfig) -> DeploymentConfig {
        DeploymentConfig {
            cpu_request: other.cpu_request.or(self.cpu_request),
            cpu_limit: other.cpu_limit.or(self.cpu_limit),
            memory_request: other.memory_request.or(self.memory_request),
            memory_limit: other.memory_limit.or(self.memory_limit),
            exposed_port: other.exposed_port.or(self.exposed_port),
            // Env-wins semantics: if the environment has an explicit value use it,
            // otherwise inherit the project-level setting.
            automatic_deploy: other.automatic_deploy.or(self.automatic_deploy),
            performance_metrics_enabled: other.performance_metrics_enabled
                || self.performance_metrics_enabled,
            session_recording_enabled: other.session_recording_enabled
                || self.session_recording_enabled,
            container_exec_enabled: other.container_exec_enabled || self.container_exec_enabled,
            // Use other's replicas if > 0, otherwise use self's replicas
            replicas: if other.replicas > 0 {
                other.replicas
            } else {
                self.replicas
            },
            // Merge security configurations
            security: match (&self.security, &other.security) {
                (Some(base), Some(override_security)) => Some(base.merge(override_security)),
                (Some(base), None) => Some(base.clone()),
                (None, Some(override_security)) => Some(override_security.clone()),
                (None, None) => None,
            },
            // A non-empty environment selector overrides the project. Empty
            // selectors mean "clear this override", so inherit the project
            // selector instead of erasing its placement boundary.
            target_nodes: other
                .configured_target_nodes()
                .map(<[i32]>::to_vec)
                .or_else(|| self.configured_target_nodes().map(<[i32]>::to_vec)),
            target_labels: other
                .configured_target_labels()
                .cloned()
                .or_else(|| self.configured_target_labels().cloned()),
            anti_affinity: other.anti_affinity,
            // Env-wins, inheriting the project when unset — same semantics as
            // `automatic_deploy`. Deliberately not `||`: an environment must be
            // able to turn cross-builds *off* for a project that has them on,
            // which an OR would make impossible.
            cross_architecture_builds: other
                .cross_architecture_builds
                .or(self.cross_architecture_builds),
            on_demand: other.on_demand || self.on_demand,
            idle_timeout_seconds: if other.idle_timeout_seconds != 300 {
                other.idle_timeout_seconds
            } else {
                self.idle_timeout_seconds
            },
            wake_timeout_seconds: if other.wake_timeout_seconds != 30 {
                other.wake_timeout_seconds
            } else {
                self.wake_timeout_seconds
            },
            request_timeout_seconds: other
                .request_timeout_seconds
                .or(self.request_timeout_seconds),
            sse_idle_timeout_seconds: other
                .sse_idle_timeout_seconds
                .or(self.sse_idle_timeout_seconds),
            websocket_idle_timeout_seconds: other
                .websocket_idle_timeout_seconds
                .or(self.websocket_idle_timeout_seconds),
            max_concurrent_connections: other
                .max_concurrent_connections
                .or(self.max_concurrent_connections),
        }
    }

    /// Validate the resource configuration
    pub fn validate(&self) -> Result<(), String> {
        // CPU request should not exceed CPU limit. A limit of 0 is the explicit
        // "uncapped" sentinel, so it never constrains the request.
        if let (Some(request), Some(limit)) = (self.cpu_request, self.cpu_limit) {
            if limit != 0 && request > limit {
                return Err("CPU request cannot exceed CPU limit".to_string());
            }
        }

        // Negative values are never meaningful here, and for `memory_limit` a
        // negative is actively dangerous: `parse_memory_mb` rejects it and the
        // deployer then leaves Docker's memory cgroup unset, so `-1` runs the
        // container *uncapped* while reading like a tighter limit than `0`.
        for (name, value) in [
            ("CPU request", self.cpu_request),
            ("CPU limit", self.cpu_limit),
            ("Memory request", self.memory_request),
            ("Memory limit", self.memory_limit),
        ] {
            if let Some(value) = value {
                if value < 0 {
                    return Err(format!(
                        "{name} cannot be negative, got {value} (use 0 for unlimited)"
                    ));
                }
            }
        }

        // Memory request should not exceed memory limit. A limit of 0 is the
        // explicit "uncapped" sentinel, so it never constrains the request.
        if let (Some(request), Some(limit)) = (self.memory_request, self.memory_limit) {
            if limit != 0 && request > limit {
                return Err("Memory request cannot exceed memory limit".to_string());
            }
        }

        // Replicas must be at least 1
        if self.replicas < 1 {
            return Err(format!(
                "Replicas must be at least 1, got {}",
                self.replicas
            ));
        }

        // Port should be in valid range
        if let Some(port) = self.exposed_port {
            if !(1..=65535).contains(&port) {
                return Err(format!("Port {} is not in valid range (1-65535)", port));
            }
        }

        // Idle timeout should be in valid range when on-demand is enabled
        if self.on_demand {
            if !(60..=86400).contains(&self.idle_timeout_seconds) {
                return Err(format!(
                    "Idle timeout {} is not in valid range (60-86400 seconds)",
                    self.idle_timeout_seconds
                ));
            }
            if !(5..=120).contains(&self.wake_timeout_seconds) {
                return Err(format!(
                    "Wake timeout {} is not in valid range (5-120 seconds)",
                    self.wake_timeout_seconds
                ));
            }
        }

        // Sanity bounds only — the operator's hard ceiling
        // (`AppSettings.request_timeouts.max_request_timeout_seconds`) is
        // enforced separately at resolution time, since this type has no
        // access to global settings. `0` is a valid explicit "no timeout"
        // override (same uncapped-sentinel convention as `cpu_limit`/
        // `memory_limit` above), so the floor is 0, not 1.
        for (name, value) in [
            ("Request timeout", self.request_timeout_seconds),
            ("SSE idle timeout", self.sse_idle_timeout_seconds),
            (
                "WebSocket idle timeout",
                self.websocket_idle_timeout_seconds,
            ),
        ] {
            if let Some(seconds) = value {
                if !(0..=86400).contains(&seconds) {
                    return Err(format!(
                        "{name} {seconds} is not in valid range (0-86400 seconds, 0 = no timeout)"
                    ));
                }
            }
        }

        if let Some(max_conn) = self.max_concurrent_connections {
            if max_conn < 0 {
                return Err(format!(
                    "max_concurrent_connections cannot be negative, got {}",
                    max_conn
                ));
            }
        }

        Ok(())
    }

    /// Check this config against the operator's tenant resource ceilings.
    ///
    /// Three of the per-project overrides above use `Some(0)` as an explicit
    /// "unlimited" sentinel that beats the instance-wide default: memory limit,
    /// concurrent-connection cap, and the request/SSE/WebSocket timeouts. That
    /// is deliberate — a dedicated workload sometimes needs it — but on a
    /// shared instance it also means anyone who can edit a project's config can
    /// opt that project out of the operator's global guardrails.
    ///
    /// Ceilings close that off *only when an operator has set them*. Every
    /// field defaults to unenforced, so an upgrade changes nothing until the
    /// operator opts in. Violations are **rejected with a reason**, never
    /// silently clamped, so the user sees why their value did not take.
    ///
    /// Returns every violation rather than the first, so a form with two bad
    /// fields does not need two round trips to fix.
    pub fn check_against_tenant_ceilings(
        &self,
        ceilings: &temps_core::TenantResourceCeilings,
    ) -> Result<(), Vec<String>> {
        if ceilings.is_unenforced() {
            return Ok(());
        }

        let mut violations = Vec::new();

        // `<= 0`, not `== 0`, on both of these. `validate()` rejects negatives,
        // but the ceiling must not depend on that: a negative memory limit is
        // *also* uncapped in practice — `parse_memory_mb` returns `None` for
        // anything below zero and the deployer then leaves Docker's cgroup cap
        // unset entirely. Matching only the `0` sentinel would let `-1` through
        // as "some finite value below the ceiling" and produce exactly the
        // unlimited container the ceiling exists to prevent.
        if ceilings.max_memory_limit_mb > 0 {
            let ceiling = ceilings.max_memory_limit_mb as i64;
            match self.memory_limit {
                Some(mb) if mb <= 0 => violations.push(format!(
                    "Memory limit cannot be set to unlimited: this instance caps it at {ceiling} MB"
                )),
                Some(mb) if i64::from(mb) > ceiling => violations.push(format!(
                    "Memory limit {mb} MB exceeds this instance's maximum of {ceiling} MB"
                )),
                _ => {}
            }
        }

        if ceilings.max_concurrent_connections > 0 {
            let ceiling = ceilings.max_concurrent_connections as i64;
            match self.max_concurrent_connections {
                Some(n) if n <= 0 => violations.push(format!(
                    "Concurrent connections cannot be set to unlimited: this instance caps them at {ceiling}"
                )),
                Some(n) if i64::from(n) > ceiling => violations.push(format!(
                    "Concurrent connection limit {n} exceeds this instance's maximum of {ceiling}"
                )),
                _ => {}
            }
        }

        // Nonzero timeouts need no ceiling here — they are already clamped to
        // `request_timeouts.max_*` at resolution time. Only the `Some(0)`
        // "no timeout" sentinel escapes that clamp, so that is all this checks.
        if !ceilings.allow_unlimited_request_timeouts {
            for (name, value) in [
                ("Request timeout", self.request_timeout_seconds),
                ("SSE idle timeout", self.sse_idle_timeout_seconds),
                (
                    "WebSocket idle timeout",
                    self.websocket_idle_timeout_seconds,
                ),
            ] {
                if value == Some(0) {
                    violations.push(format!(
                        "{name} cannot be set to unlimited (0) on this instance"
                    ));
                }
            }
        }

        if violations.is_empty() {
            Ok(())
        } else {
            Err(violations)
        }
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

    /// Validate the resource configuration
    pub fn validate(&self) -> Result<(), String> {
        // CPU request should not exceed CPU limit. A limit of 0 is the explicit
        // "uncapped" sentinel, so it never constrains the request.
        if let (Some(request), Some(limit)) = (self.cpu_request, self.cpu_limit) {
            if limit != 0 && request > limit {
                return Err("CPU request cannot exceed CPU limit".to_string());
            }
        }

        // Memory request should not exceed memory limit. A limit of 0 is the
        // explicit "uncapped" sentinel, so it never constrains the request.
        if let (Some(request), Some(limit)) = (self.memory_request, self.memory_limit) {
            if limit != 0 && request > limit {
                return Err("Memory request cannot exceed memory limit".to_string());
            }
        }

        // Port should be in valid range
        if let Some(port) = self.exposed_port {
            if !(1..=65535).contains(&port) {
                return Err(format!("Port {} is not in valid range (1-65535)", port));
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cross-architecture builds are emulated and slow, so they must stay off
    /// until someone asks for them — and an environment must be able to turn
    /// them off again for a project that has them on.
    #[test]
    fn test_cross_architecture_builds_merge_semantics() {
        let project_on = DeploymentConfig {
            cross_architecture_builds: Some(true),
            ..Default::default()
        };
        let env_unset = DeploymentConfig::default();
        let env_off = DeploymentConfig {
            cross_architecture_builds: Some(false),
            ..Default::default()
        };

        // Off by default: nobody pays for an emulated build they didn't ask for.
        assert_eq!(env_unset.cross_architecture_builds, None);

        // An environment that says nothing inherits the project.
        assert_eq!(
            project_on.merge(&env_unset).cross_architecture_builds,
            Some(true)
        );

        // ...and one that says `false` overrides it. An `||` merge (which is
        // what the neighbouring booleans use) would make this impossible.
        assert_eq!(
            project_on.merge(&env_off).cross_architecture_builds,
            Some(false)
        );

        // Neither set stays unset; the read site treats that as disabled.
        assert_eq!(
            DeploymentConfig::default()
                .merge(&env_unset)
                .cross_architecture_builds,
            None
        );
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
    fn uncapped_limit_sentinel_does_not_constrain_request() {
        // memory_limit/cpu_limit of 0 is the explicit "uncapped" sentinel and
        // must not trip the request-exceeds-limit validation.
        let config = DeploymentConfig {
            cpu_request: Some(500_000),
            cpu_limit: Some(0),
            memory_request: Some(128),
            memory_limit: Some(0),
            ..DeploymentConfig::default()
        };
        assert_eq!(config.validate(), Ok(()));
    }

    #[test]
    fn positive_memory_limit_below_request_is_rejected() {
        let config = DeploymentConfig {
            memory_request: Some(256),
            memory_limit: Some(128),
            ..DeploymentConfig::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn merge_keeps_env_uncapped_over_project_cap() {
        // An env that explicitly opts into uncapped (Some(0)) must win over a
        // project-level hard cap rather than inheriting it.
        let project = DeploymentConfig {
            memory_limit: Some(512),
            ..DeploymentConfig::default()
        };
        let env = DeploymentConfig {
            memory_limit: Some(0),
            ..DeploymentConfig::default()
        };
        assert_eq!(project.merge(&env).memory_limit, Some(0));
    }

    #[test]
    fn test_merge_configs() {
        let project_config = DeploymentConfig {
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

        let env_config = DeploymentConfig {
            cpu_request: Some(200),   // Override
            cpu_limit: None,          // Use project default
            memory_request: None,     // Use project default
            memory_limit: Some(1024), // Override
            exposed_port: Some(8080), // Override
            // Env explicitly opts out — env-wins semantics means this is respected
            automatic_deploy: Some(false),
            performance_metrics_enabled: false,
            session_recording_enabled: true, // Override
            replicas: 5,                     // Override
            security: None,
            target_nodes: None,
            target_labels: None,
            anti_affinity: true,
            ..Default::default()
        };

        let merged = project_config.merge(&env_config);

        assert_eq!(merged.cpu_request, Some(200));
        assert_eq!(merged.cpu_limit, Some(1000));
        assert_eq!(merged.memory_request, Some(128));
        assert_eq!(merged.memory_limit, Some(1024));
        assert_eq!(merged.exposed_port, Some(8080));
        // Env explicitly sets false → env wins, merged is false
        assert_eq!(merged.automatic_deploy, Some(false));
        assert!(merged.performance_metrics_enabled); // true || false = true
        assert!(merged.session_recording_enabled);
        assert_eq!(merged.replicas, 5);
        assert_eq!(merged.target_nodes, None);
    }

    #[test]
    fn test_merge_target_nodes() {
        // Environment target_nodes overrides project-level
        let project_config = DeploymentConfig {
            target_nodes: Some(vec![1, 2]),
            ..Default::default()
        };
        let env_config = DeploymentConfig {
            target_nodes: Some(vec![3]),
            ..Default::default()
        };
        let merged = project_config.merge(&env_config);
        assert_eq!(merged.target_nodes, Some(vec![3]));

        // Falls back to project-level when env has None
        let env_no_nodes = DeploymentConfig {
            target_nodes: None,
            ..Default::default()
        };
        let merged2 = project_config.merge(&env_no_nodes);
        assert_eq!(merged2.target_nodes, Some(vec![1, 2]));

        // A legacy persisted empty environment selector means the override was
        // cleared; it must inherit rather than erase the project constraint.
        let env_empty_nodes = DeploymentConfig {
            target_nodes: Some(vec![]),
            ..Default::default()
        };
        let merged3 = project_config.merge(&env_empty_nodes);
        assert_eq!(merged3.target_nodes, Some(vec![1, 2]));

        // Both None → None
        let both_none = DeploymentConfig::default().merge(&DeploymentConfig::default());
        assert_eq!(both_none.target_nodes, None);
    }

    #[test]
    fn test_empty_environment_target_labels_inherit_project_constraint() {
        let project_config = DeploymentConfig {
            target_labels: Some(serde_json::json!({"region": "eu"})),
            ..Default::default()
        };
        let env_config = DeploymentConfig {
            target_labels: Some(serde_json::json!({})),
            ..Default::default()
        };

        let merged = project_config.merge(&env_config);

        assert_eq!(
            merged.target_labels,
            Some(serde_json::json!({"region": "eu"}))
        );
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
    fn merge_env_request_timeout_overrides_project() {
        // Three-state semantics (like memory_limit, not the idle_timeout_seconds
        // sentinel pattern): env None must inherit project's value, env Some
        // must win outright.
        let project = DeploymentConfig {
            request_timeout_seconds: Some(30),
            sse_idle_timeout_seconds: Some(1800),
            websocket_idle_timeout_seconds: Some(1800),
            ..Default::default()
        };

        let env_override = DeploymentConfig {
            request_timeout_seconds: Some(10),
            ..Default::default()
        };
        let merged = project.merge(&env_override);
        assert_eq!(merged.request_timeout_seconds, Some(10));
        // Env left these unset -> inherit project's values.
        assert_eq!(merged.sse_idle_timeout_seconds, Some(1800));
        assert_eq!(merged.websocket_idle_timeout_seconds, Some(1800));

        let env_no_override = DeploymentConfig::default();
        let merged2 = project.merge(&env_no_override);
        assert_eq!(merged2.request_timeout_seconds, Some(30));
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
    fn request_timeout_validation_rejects_out_of_range() {
        // 0 is a valid explicit "no timeout" override, not a validation error.
        let no_timeout_override = DeploymentConfig {
            request_timeout_seconds: Some(0),
            ..Default::default()
        };
        assert!(no_timeout_override.validate().is_ok());

        let negative = DeploymentConfig {
            request_timeout_seconds: Some(-1),
            ..Default::default()
        };
        assert!(negative.validate().is_err());

        let too_high = DeploymentConfig {
            sse_idle_timeout_seconds: Some(90_000),
            ..Default::default()
        };
        assert!(too_high.validate().is_err());

        let ok = DeploymentConfig {
            websocket_idle_timeout_seconds: Some(3600),
            ..Default::default()
        };
        assert!(ok.validate().is_ok());
    }

    #[test]
    fn test_validation() {
        let valid_config = DeploymentConfig {
            cpu_request: Some(100),
            cpu_limit: Some(1000),
            memory_request: Some(128),
            memory_limit: Some(512),
            exposed_port: Some(3000),
            security: None,
            ..Default::default()
        };
        assert!(valid_config.validate().is_ok());

        let invalid_cpu = DeploymentConfig {
            cpu_request: Some(2000),
            cpu_limit: Some(1000),
            security: None,
            ..Default::default()
        };
        assert!(invalid_cpu.validate().is_err());

        let invalid_memory = DeploymentConfig {
            memory_request: Some(1024),
            memory_limit: Some(512),
            security: None,
            ..Default::default()
        };
        assert!(invalid_memory.validate().is_err());

        let invalid_port = DeploymentConfig {
            exposed_port: Some(70000),
            ..Default::default()
        };
        assert!(invalid_port.validate().is_err());
    }

    /// The whole contract of the default ceilings: an instance that has never
    /// configured them accepts exactly what it accepted before, including the
    /// `0 = unlimited` sentinels.
    #[test]
    fn default_ceilings_accept_every_unlimited_sentinel() {
        let ceilings = temps_core::TenantResourceCeilings::default();
        let config = DeploymentConfig {
            memory_limit: Some(0),
            max_concurrent_connections: Some(0),
            request_timeout_seconds: Some(0),
            sse_idle_timeout_seconds: Some(0),
            websocket_idle_timeout_seconds: Some(0),
            ..Default::default()
        };

        assert!(config.check_against_tenant_ceilings(&ceilings).is_ok());
    }

    #[test]
    fn configured_ceilings_reject_unlimited_and_over_ceiling_values() {
        let ceilings = temps_core::TenantResourceCeilings {
            max_memory_limit_mb: 4096,
            max_concurrent_connections: 200,
            allow_unlimited_request_timeouts: false,
        };

        // `0` means "opt out of the global cap" — the exact thing a ceiling exists to stop.
        let unlimited = DeploymentConfig {
            memory_limit: Some(0),
            max_concurrent_connections: Some(0),
            request_timeout_seconds: Some(0),
            sse_idle_timeout_seconds: Some(0),
            websocket_idle_timeout_seconds: Some(0),
            ..Default::default()
        };
        let violations = unlimited
            .check_against_tenant_ceilings(&ceilings)
            .expect_err("unlimited overrides must be rejected under ceilings");
        // All five reported at once, not just the first.
        assert_eq!(violations.len(), 5, "got: {violations:?}");

        let over = DeploymentConfig {
            memory_limit: Some(8192),
            max_concurrent_connections: Some(500),
            ..Default::default()
        };
        let violations = over
            .check_against_tenant_ceilings(&ceilings)
            .expect_err("values above the ceiling must be rejected");
        assert_eq!(violations.len(), 2, "got: {violations:?}");
        // The message has to name the ceiling, or the user cannot pick a legal value.
        assert!(violations[0].contains("4096"), "got: {violations:?}");
        assert!(violations[1].contains("200"), "got: {violations:?}");
    }

    #[test]
    fn configured_ceilings_accept_values_at_and_below_the_ceiling() {
        let ceilings = temps_core::TenantResourceCeilings {
            max_memory_limit_mb: 4096,
            max_concurrent_connections: 200,
            allow_unlimited_request_timeouts: false,
        };

        let at_ceiling = DeploymentConfig {
            memory_limit: Some(4096),
            max_concurrent_connections: Some(200),
            // Nonzero timeouts are clamped at resolution time, not here.
            request_timeout_seconds: Some(3600),
            ..Default::default()
        };
        assert!(at_ceiling.check_against_tenant_ceilings(&ceilings).is_ok());

        // `None` = inherit the instance default, which is always within the ceiling.
        let inheriting = DeploymentConfig::default();
        assert!(inheriting.check_against_tenant_ceilings(&ceilings).is_ok());
    }

    /// Regression: a negative limit is uncapped in practice, so it must not be
    /// able to slip past a ceiling by looking like "a small finite value".
    ///
    /// `-1` formats to `"-1Mi"`, `parse_memory_mb` returns `None` for anything
    /// below zero, and the deployer leaves Docker's memory cgroup unset — the
    /// exact outcome `Some(0)` is refused for.
    #[test]
    fn negative_limits_are_treated_as_unlimited_by_the_ceiling() {
        let ceilings = temps_core::TenantResourceCeilings {
            max_memory_limit_mb: 4096,
            max_concurrent_connections: 200,
            ..temps_core::TenantResourceCeilings::default()
        };
        let negative = DeploymentConfig {
            memory_limit: Some(-1),
            max_concurrent_connections: Some(-1),
            ..Default::default()
        };

        let violations = negative
            .check_against_tenant_ceilings(&ceilings)
            .expect_err("a negative limit is uncapped and must be refused");
        assert_eq!(violations.len(), 2, "got: {violations:?}");
        assert!(violations[0].contains("unlimited"), "got: {violations:?}");
    }

    /// Defence in depth for the same hole: a negative never reaches the ceiling
    /// check because it is not a storable value in the first place.
    #[test]
    fn validation_rejects_negative_resource_values() {
        for config in [
            DeploymentConfig {
                memory_limit: Some(-1),
                ..Default::default()
            },
            DeploymentConfig {
                memory_request: Some(-1),
                ..Default::default()
            },
            DeploymentConfig {
                cpu_limit: Some(-1),
                ..Default::default()
            },
            DeploymentConfig {
                cpu_request: Some(-1),
                ..Default::default()
            },
        ] {
            let error = config
                .validate()
                .expect_err("negative resource values must be rejected");
            assert!(error.contains("cannot be negative"), "got: {error}");
        }

        // `0` stays valid — it is the documented "unlimited" sentinel.
        let uncapped = DeploymentConfig {
            memory_limit: Some(0),
            cpu_limit: Some(0),
            ..Default::default()
        };
        assert!(uncapped.validate().is_ok());
    }

    /// Ceilings are independent: setting one must not start enforcing the others.
    #[test]
    fn ceilings_are_enforced_independently() {
        let memory_only = temps_core::TenantResourceCeilings {
            max_memory_limit_mb: 4096,
            ..temps_core::TenantResourceCeilings::default()
        };
        let config = DeploymentConfig {
            memory_limit: Some(1024),
            max_concurrent_connections: Some(0),
            request_timeout_seconds: Some(0),
            ..Default::default()
        };

        assert!(config.check_against_tenant_ceilings(&memory_only).is_ok());
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
