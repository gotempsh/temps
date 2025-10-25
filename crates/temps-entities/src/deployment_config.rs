//! Shared deployment configuration for projects and environments
//!
//! This module defines configuration structures that can be shared between
//! projects (as defaults) and environments (as overrides).

use sea_orm::FromJsonQueryResult;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Deployment configuration shared between projects and environments
///
/// This configuration can be set at the project level (as defaults) and
/// overridden at the environment level for specific deployments.
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
            if port < 1 || port > 65535 {
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
}
