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

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            enabled: None,
            headers: None,
            rate_limiting: None,
            attack_mode: None,
            challenge_config: None,
            geo_restrictions: None,
        }
    }
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

    /// Enable automatic deployments on git push
    #[serde(default)]
    pub automatic_deploy: bool,

    /// Enable performance metrics collection (speed insights)
    #[serde(default)]
    pub performance_metrics_enabled: bool,

    /// Enable session recording for analytics
    #[serde(default)]
    pub session_recording_enabled: bool,

    /// Number of replicas/instances to run
    /// Defaults to 1 replica
    #[serde(default = "default_replicas")]
    pub replicas: i32,

    /// Security configuration (headers, rate limiting, attack mode, etc.)
    /// These settings inherit and override from parent level (Environment > Project > Global)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub security: Option<SecurityConfig>,
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

    /// Number of replicas
    #[serde(default = "default_replicas")]
    pub replicas: i32,
}

fn default_replicas() -> i32 {
    1
}

impl Default for DeploymentConfig {
    fn default() -> Self {
        Self {
            cpu_request: None,
            cpu_limit: None,
            memory_request: None,
            memory_limit: None,
            exposed_port: None,
            automatic_deploy: false,
            performance_metrics_enabled: false,
            session_recording_enabled: false,
            replicas: 1,
            security: None,
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
            automatic_deploy: other.automatic_deploy || self.automatic_deploy,
            performance_metrics_enabled: other.performance_metrics_enabled
                || self.performance_metrics_enabled,
            session_recording_enabled: other.session_recording_enabled
                || self.session_recording_enabled,
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
            automatic_deploy: config.automatic_deploy,
            performance_metrics_enabled: config.performance_metrics_enabled,
            session_recording_enabled: config.session_recording_enabled,
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
        assert!(!config.automatic_deploy);
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
            automatic_deploy: true,
            performance_metrics_enabled: true,
            session_recording_enabled: false,
            replicas: 2,
        };

        let env_config = DeploymentConfig {
            cpu_request: Some(200),   // Override
            cpu_limit: None,          // Use project default
            memory_request: None,     // Use project default
            memory_limit: Some(1024), // Override
            exposed_port: Some(8080), // Override
            automatic_deploy: false,
            performance_metrics_enabled: false,
            session_recording_enabled: true, // Override
            replicas: 5,                     // Override
        };

        let merged = project_config.merge(&env_config);

        assert_eq!(merged.cpu_request, Some(200));
        assert_eq!(merged.cpu_limit, Some(1000));
        assert_eq!(merged.memory_request, Some(128));
        assert_eq!(merged.memory_limit, Some(1024));
        assert_eq!(merged.exposed_port, Some(8080));
        assert!(merged.automatic_deploy); // true || false = true
        assert!(merged.performance_metrics_enabled); // true || false = true
        assert!(merged.session_recording_enabled);
        assert_eq!(merged.replicas, 5);
    }

    #[test]
    fn test_validation() {
        let valid_config = DeploymentConfig {
            cpu_request: Some(100),
            cpu_limit: Some(1000),
            memory_request: Some(128),
            memory_limit: Some(512),
            exposed_port: Some(3000),
            ..Default::default()
        };
        assert!(valid_config.validate().is_ok());

        let invalid_cpu = DeploymentConfig {
            cpu_request: Some(2000),
            cpu_limit: Some(1000),
            ..Default::default()
        };
        assert!(invalid_cpu.validate().is_err());

        let invalid_memory = DeploymentConfig {
            memory_request: Some(1024),
            memory_limit: Some(512),
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
            automatic_deploy: true,
            performance_metrics_enabled: true,
            session_recording_enabled: false,
            replicas: 3,
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
            automatic_deploy: true,
            performance_metrics_enabled: true,
            session_recording_enabled: false,
            replicas: 2,
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
