//! Repository Configuration (.temps.yaml)
//!
//! Central definition for the .temps.yaml configuration file format
//! that can be placed in user repositories to configure deployment behavior.

use serde::{Deserialize, Serialize};

/// Complete configuration structure for .temps.yaml
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TempsConfig {
    /// Cron job configurations
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cron: Option<Vec<CronJobConfig>>,

    /// Build configuration
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<BuildConfig>,

    /// Environment variables to inject at build/runtime
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<std::collections::HashMap<String, String>>,

    /// Health check configuration
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health: Option<HealthCheckConfig>,
}

/// Cron job configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronJobConfig {
    /// HTTP path to invoke for this cron job
    pub path: String,

    /// Cron schedule in standard cron format
    /// Format: "minute hour day month weekday"
    /// Example: "0 0 * * *" (daily at midnight)
    pub schedule: String,

    /// Optional name/description for the cron job
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Build configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildConfig {
    /// Custom Dockerfile path (relative to repository root)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dockerfile: Option<String>,

    /// Build context path (relative to repository root)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,

    /// Build arguments to pass to Docker build
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<std::collections::HashMap<String, String>>,

    /// Install command (overrides preset detection)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub install_command: Option<String>,

    /// Build command (overrides preset detection)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_command: Option<String>,

    /// Output directory for static builds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_dir: Option<String>,
}

/// Health check configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthCheckConfig {
    /// HTTP path for health checks
    pub path: String,

    /// Expected HTTP status code (default: 200)
    #[serde(default = "default_health_status")]
    pub status: u16,

    /// Interval between health checks in seconds (default: 30)
    #[serde(default = "default_health_interval")]
    pub interval: u64,

    /// Timeout for health check requests in seconds (default: 5)
    #[serde(default = "default_health_timeout")]
    pub timeout: u64,

    /// Number of consecutive failures before marking unhealthy (default: 3)
    #[serde(default = "default_health_retries")]
    pub retries: u32,
}

fn default_health_status() -> u16 {
    200
}

fn default_health_interval() -> u64 {
    30
}

fn default_health_timeout() -> u64 {
    5
}

fn default_health_retries() -> u32 {
    3
}

impl TempsConfig {
    /// Parse configuration from YAML string
    pub fn from_yaml(yaml: &str) -> Result<Self, serde_yaml::Error> {
        serde_yaml::from_str(yaml)
    }

    /// Serialize configuration to YAML string
    pub fn to_yaml(&self) -> Result<String, serde_yaml::Error> {
        serde_yaml::to_string(self)
    }

    /// Check if configuration has any cron jobs defined
    pub fn has_crons(&self) -> bool {
        self.cron.as_ref().map_or(false, |c| !c.is_empty())
    }

    /// Get cron jobs, or empty vec if none defined
    pub fn cron_jobs(&self) -> Vec<&CronJobConfig> {
        self.cron.as_ref().map_or(vec![], |c| c.iter().collect())
    }

    /// Check if configuration has custom build settings
    pub fn has_build_config(&self) -> bool {
        self.build.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_complete_config() {
        let yaml = r#"
cron:
  - path: /api/cron/cleanup
    schedule: "0 0 * * *"
    name: "Daily Cleanup"
  - path: /api/cron/reports
    schedule: "0 9 * * 1"
    name: "Weekly Reports"

build:
  dockerfile: docker/Dockerfile
  context: .
  args:
    NODE_ENV: production
    API_URL: https://api.example.com

env:
  DATABASE_URL: postgres://localhost/db
  REDIS_URL: redis://localhost:6379

health:
  path: /health
  status: 200
  interval: 30
  timeout: 5
  retries: 3
"#;

        let config = TempsConfig::from_yaml(yaml).unwrap();

        // Verify cron jobs
        assert!(config.has_crons());
        let crons = config.cron.as_ref().unwrap();
        assert_eq!(crons.len(), 2);
        assert_eq!(crons[0].path, "/api/cron/cleanup");
        assert_eq!(crons[0].schedule, "0 0 * * *");
        assert_eq!(crons[0].name.as_deref(), Some("Daily Cleanup"));
        assert_eq!(crons[1].path, "/api/cron/reports");
        assert_eq!(crons[1].schedule, "0 9 * * 1");

        // Verify build config
        assert!(config.has_build_config());
        let build = config.build.as_ref().unwrap();
        assert_eq!(build.dockerfile.as_deref(), Some("docker/Dockerfile"));
        assert_eq!(build.context.as_deref(), Some("."));
        assert_eq!(build.args.as_ref().unwrap().get("NODE_ENV"), Some(&"production".to_string()));

        // Verify env vars
        let env = config.env.as_ref().unwrap();
        assert_eq!(env.get("DATABASE_URL"), Some(&"postgres://localhost/db".to_string()));

        // Verify health check
        let health = config.health.as_ref().unwrap();
        assert_eq!(health.path, "/health");
        assert_eq!(health.status, 200);
        assert_eq!(health.interval, 30);
        assert_eq!(health.timeout, 5);
        assert_eq!(health.retries, 3);
    }

    #[test]
    fn test_parse_minimal_config() {
        let yaml = r#"
cron:
  - path: /api/cron/task
    schedule: "*/5 * * * *"
"#;

        let config = TempsConfig::from_yaml(yaml).unwrap();
        assert!(config.has_crons());
        assert!(!config.has_build_config());
        assert!(config.env.is_none());
        assert!(config.health.is_none());

        let crons = config.cron_jobs();
        assert_eq!(crons.len(), 1);
        assert_eq!(crons[0].path, "/api/cron/task");
    }

    #[test]
    fn test_parse_empty_config() {
        let yaml = "";
        let config = TempsConfig::from_yaml(yaml).unwrap();
        assert!(!config.has_crons());
        assert!(!config.has_build_config());
        assert_eq!(config.cron_jobs().len(), 0);
    }

    #[test]
    fn test_parse_config_no_crons() {
        let yaml = r#"
build:
  dockerfile: Dockerfile
"#;

        let config = TempsConfig::from_yaml(yaml).unwrap();
        assert!(!config.has_crons());
        assert!(config.has_build_config());
    }

    #[test]
    fn test_serialize_config() {
        let config = TempsConfig {
            cron: Some(vec![CronJobConfig {
                path: "/api/cron/test".to_string(),
                schedule: "0 0 * * *".to_string(),
                name: Some("Test Cron".to_string()),
            }]),
            build: None,
            env: None,
            health: None,
        };

        let yaml = config.to_yaml().unwrap();
        assert!(yaml.contains("path: /api/cron/test"));
        assert!(yaml.contains("schedule: 0 0 * * *"));
    }

    #[test]
    fn test_health_check_defaults() {
        let yaml = r#"
health:
  path: /health
"#;

        let config = TempsConfig::from_yaml(yaml).unwrap();
        let health = config.health.as_ref().unwrap();
        assert_eq!(health.path, "/health");
        assert_eq!(health.status, 200); // default
        assert_eq!(health.interval, 30); // default
        assert_eq!(health.timeout, 5); // default
        assert_eq!(health.retries, 3); // default
    }
}
