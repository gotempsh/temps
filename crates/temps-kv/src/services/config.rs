//! KV Service configuration types

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Default Redis Docker image
pub const DEFAULT_REDIS_IMAGE: &str = "redis:7-alpine";
/// Default container name
pub const DEFAULT_CONTAINER_NAME: &str = "temps-kv-redis";
/// Default volume name
pub const DEFAULT_VOLUME_NAME: &str = "temps-kv-redis_data";
/// Default max memory
pub const DEFAULT_MAX_MEMORY: &str = "256mb";

// Helper functions for serde defaults
fn default_docker_image() -> String {
    DEFAULT_REDIS_IMAGE.to_string()
}

fn default_max_memory() -> String {
    DEFAULT_MAX_MEMORY.to_string()
}

fn default_persistence() -> bool {
    true
}

// Helper functions for schemars examples
fn example_docker_image() -> &'static str {
    "redis:7.2-alpine"
}

fn example_port() -> &'static str {
    "6379"
}

fn example_max_memory() -> &'static str {
    "512mb"
}

/// Input configuration for creating a Temps KV service
/// This is what users provide when creating the service
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[schemars(
    title = "Temps KV Configuration",
    description = "Configuration for Temps KV service (Redis-backed)"
)]
pub struct KvInputConfig {
    /// Docker image to use (e.g., "redis:7-alpine", "redis:7.2-alpine")
    #[serde(default = "default_docker_image")]
    #[schemars(example = "example_docker_image", default = "default_docker_image")]
    pub docker_image: String,

    /// Host port (auto-assigned if not provided)
    #[schemars(example = "example_port")]
    pub port: Option<String>,

    /// Maximum memory for Redis (e.g., "256mb", "1gb")
    #[serde(default = "default_max_memory")]
    #[schemars(example = "example_max_memory", default = "default_max_memory")]
    pub max_memory: String,

    /// Enable persistence (AOF)
    #[serde(default = "default_persistence")]
    #[schemars(default = "default_persistence")]
    pub persistence: bool,
}

impl Default for KvInputConfig {
    fn default() -> Self {
        Self {
            docker_image: default_docker_image(),
            port: None,
            max_memory: default_max_memory(),
            persistence: default_persistence(),
        }
    }
}

/// Internal configuration for KV service (with generated/resolved values)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KvConfig {
    /// Docker image name with tag
    pub docker_image: String,

    /// Host port (0 = auto-assign on creation)
    pub port: u16,

    /// Maximum memory for Redis
    pub max_memory: String,

    /// Enable persistence (AOF)
    pub persistence: bool,

    /// Container name
    pub container_name: String,

    /// Volume name for persistent data
    pub volume_name: String,
}

impl Default for KvConfig {
    fn default() -> Self {
        Self {
            docker_image: DEFAULT_REDIS_IMAGE.to_string(),
            port: 0, // Auto-assign
            max_memory: DEFAULT_MAX_MEMORY.to_string(),
            persistence: true,
            container_name: DEFAULT_CONTAINER_NAME.to_string(),
            volume_name: DEFAULT_VOLUME_NAME.to_string(),
        }
    }
}

impl From<KvInputConfig> for KvConfig {
    fn from(input: KvInputConfig) -> Self {
        // Parse port from string, use 0 for auto-assign if not provided or invalid
        let port = input.port.and_then(|p| p.parse().ok()).unwrap_or(0);

        Self {
            docker_image: input.docker_image,
            port,
            max_memory: input.max_memory,
            persistence: input.persistence,
            container_name: DEFAULT_CONTAINER_NAME.to_string(),
            volume_name: DEFAULT_VOLUME_NAME.to_string(),
        }
    }
}

impl KvConfig {
    /// Extract the image name (without tag)
    pub fn image_name(&self) -> &str {
        self.docker_image
            .split(':')
            .next()
            .unwrap_or(&self.docker_image)
    }

    /// Extract the image tag
    pub fn image_tag(&self) -> &str {
        self.docker_image.split(':').nth(1).unwrap_or("latest")
    }

    /// Get the version from the image tag (e.g., "7" from "redis:7-alpine")
    pub fn version(&self) -> String {
        let tag = self.image_tag();
        // Extract numeric version: "7-alpine" -> "7", "7.2.4-alpine" -> "7.2.4"
        tag.split('-').next().unwrap_or(tag).to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_kv_config_default() {
        let config = KvConfig::default();
        assert_eq!(config.docker_image, "redis:7-alpine");
        assert_eq!(config.port, 0);
        assert_eq!(config.max_memory, "256mb");
        assert!(config.persistence);
        assert_eq!(config.container_name, "temps-kv-redis");
    }

    #[test]
    fn test_kv_input_config_to_config() {
        let input = KvInputConfig {
            docker_image: "redis:7.2-alpine".to_string(),
            port: Some("6380".to_string()),
            max_memory: "512mb".to_string(),
            persistence: false,
        };

        let config: KvConfig = input.into();
        assert_eq!(config.docker_image, "redis:7.2-alpine");
        assert_eq!(config.port, 6380);
        assert_eq!(config.max_memory, "512mb");
        assert!(!config.persistence);
    }

    #[test]
    fn test_kv_input_config_defaults() {
        let input = KvInputConfig::default();
        let config: KvConfig = input.into();

        assert_eq!(config.docker_image, "redis:7-alpine");
        assert_eq!(config.port, 0);
        assert!(config.persistence);
    }

    #[test]
    fn test_image_name_and_tag() {
        let config = KvConfig {
            docker_image: "redis:7.2-alpine".to_string(),
            ..Default::default()
        };

        assert_eq!(config.image_name(), "redis");
        assert_eq!(config.image_tag(), "7.2-alpine");
        assert_eq!(config.version(), "7.2");
    }

    #[test]
    fn test_version_extraction() {
        let tests = vec![
            ("redis:7-alpine", "7"),
            ("redis:7.2-alpine", "7.2"),
            ("redis:7.2.4", "7.2.4"),
            ("redis:latest", "latest"),
        ];

        for (image, expected_version) in tests {
            let config = KvConfig {
                docker_image: image.to_string(),
                ..Default::default()
            };
            assert_eq!(config.version(), expected_version, "Failed for {}", image);
        }
    }
}
