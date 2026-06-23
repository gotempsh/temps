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
    /// CPU request in millicores (e.g., 100 = 0.1 CPU, 1000 = 1 CPU)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_request: Option<i32>,

    /// CPU limit in millicores (e.g., 2000 = 2 CPUs)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_limit: Option<i32>,

    /// Memory request in megabytes (e.g., 128 = 128MB)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_request: Option<i32>,

    /// Memory limit in megabytes (e.g., 512 = 512MB)
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
            on_demand: false,
            idle_timeout_seconds: 300,
            wake_timeout_seconds: 30,
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
            // Environment-level target_nodes overrides project-level
            target_nodes: other
                .target_nodes
                .clone()
                .or_else(|| self.target_nodes.clone()),
            // Environment-level target_labels overrides project-level
            target_labels: other
                .target_labels
                .clone()
                .or_else(|| self.target_labels.clone()),
            anti_affinity: other.anti_affinity,
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
        }
    }

    /// Validate the resource configuration
    pub fn validate(&self) -> Result<(), String> {
        // CPU request should not exceed CPU limit
        if let (Some(request), Some(limit)) = (self.cpu_request, self.cpu_limit) {
            if request > limit {
                return Err("CPU request cannot exceed CPU limit".to_string());
            }
        }

        // Memory request should not exceed memory limit
        if let (Some(request), Some(limit)) = (self.memory_request, self.memory_limit) {
            if request > limit {
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

        Ok(())
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
        // CPU request should not exceed CPU limit
        if let (Some(request), Some(limit)) = (self.cpu_request, self.cpu_limit) {
            if request > limit {
                return Err("CPU request cannot exceed CPU limit".to_string());
            }
        }

        // Memory request should not exceed memory limit
        if let (Some(request), Some(limit)) = (self.memory_request, self.memory_limit) {
            if request > limit {
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

        // Both None → None
        let both_none = DeploymentConfig::default().merge(&DeploymentConfig::default());
        assert_eq!(both_none.target_nodes, None);
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
