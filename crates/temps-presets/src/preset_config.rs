use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Type-safe configuration for presets
/// This allows each preset to accept custom parameters
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct PresetConfig {
    /// Custom Dockerfile path (relative to build context)
    /// Example: "docker/Dockerfile", "Dockerfile.production"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dockerfile_path: Option<String>,

    /// Custom build context path (relative to repository root)
    /// Example: "./api", "./services/backend"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_context: Option<String>,

    /// Custom install command override
    /// Example: "npm ci", "yarn install --frozen-lockfile"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub install_command: Option<String>,

    /// Custom build command override
    /// Example: "npm run build:prod", "make build"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_command: Option<String>,

    /// Custom output directory
    /// Example: "dist", "build", ".next"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_dir: Option<String>,

    /// Additional preset-specific parameters
    /// Allows presets to accept arbitrary configuration
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

impl PresetConfig {
    pub fn new() -> Self {
        Self::default()
    }

    /// Create from JSON value (from database)
    pub fn from_json(value: &serde_json::Value) -> Result<Self, serde_json::Error> {
        serde_json::from_value(value.clone())
    }

    /// Convert to JSON value (for database storage)
    pub fn to_json(&self) -> Result<serde_json::Value, serde_json::Error> {
        serde_json::to_value(self)
    }

    /// Builder pattern methods
    pub fn with_dockerfile_path(mut self, path: String) -> Self {
        self.dockerfile_path = Some(path);
        self
    }

    pub fn with_build_context(mut self, context: String) -> Self {
        self.build_context = Some(context);
        self
    }

    pub fn with_install_command(mut self, command: String) -> Self {
        self.install_command = Some(command);
        self
    }

    pub fn with_build_command(mut self, command: String) -> Self {
        self.build_command = Some(command);
        self
    }

    pub fn with_output_dir(mut self, dir: String) -> Self {
        self.output_dir = Some(dir);
        self
    }

    pub fn with_extra(mut self, key: String, value: serde_json::Value) -> Self {
        self.extra.insert(key, value);
        self
    }

    /// Get a strongly-typed value from extra fields
    pub fn get_extra<T: serde::de::DeserializeOwned>(&self, key: &str) -> Option<T> {
        self.extra
            .get(key)
            .and_then(|v| serde_json::from_value(v.clone()).ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_preset_config_builder() {
        let config = PresetConfig::new()
            .with_dockerfile_path("docker/Dockerfile".to_string())
            .with_build_context("./api".to_string())
            .with_install_command("npm ci".to_string());

        assert_eq!(
            config.dockerfile_path,
            Some("docker/Dockerfile".to_string())
        );
        assert_eq!(config.build_context, Some("./api".to_string()));
        assert_eq!(config.install_command, Some("npm ci".to_string()));
    }

    #[test]
    fn test_preset_config_json_roundtrip() {
        let config = PresetConfig::new()
            .with_dockerfile_path("Dockerfile.prod".to_string())
            .with_extra("custom_param".to_string(), serde_json::json!("value"));

        let json = config.to_json().unwrap();
        let deserialized = PresetConfig::from_json(&json).unwrap();

        assert_eq!(config, deserialized);
    }

    #[test]
    fn test_preset_config_from_database_json() {
        let json = serde_json::json!({
            "dockerfile_path": "docker/Dockerfile",
            "build_context": "./services/api",
            "extra_field": "extra_value"
        });

        let config = PresetConfig::from_json(&json).unwrap();

        assert_eq!(
            config.dockerfile_path,
            Some("docker/Dockerfile".to_string())
        );
        assert_eq!(
            config.build_context,
            Some("./services/api".to_string())
        );
        assert_eq!(
            config.get_extra::<String>("extra_field"),
            Some("extra_value".to_string())
        );
    }
}
