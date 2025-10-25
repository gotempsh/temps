use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Configuration for Dockerfile preset
/// Allows customizing the Dockerfile path and build context for Docker-based deployments
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DockerfilePresetConfig {
    /// Custom Dockerfile path (relative to build context)
    /// If not specified, defaults to "Dockerfile" in the build context
    #[schema(example = "docker/Dockerfile")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dockerfile_path: Option<String>,

    /// Custom build context path (relative to repository root)
    /// If not specified, uses the project's directory setting
    #[schema(example = "./api")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_context: Option<String>,
}

/// Configuration for Nixpacks preset
/// Nixpacks auto-detects your application and uses nixpacks.toml for configuration
/// No additional parameters needed - configuration is expressed in nixpacks.toml file
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NixpacksPresetConfig {
    /// This preset uses nixpacks.toml for configuration
    /// Place a nixpacks.toml file in your project directory with your settings
    /// See: https://nixpacks.com/docs/configuration/file
    #[serde(skip)]
    _marker: (),
}

/// Configuration for static site presets (Vite, Next.js, Docusaurus, etc.)
/// These presets build static sites that are served via a web server
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StaticPresetConfig {
    /// Custom install command (overrides auto-detected package manager)
    #[schema(example = "npm ci")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub install_command: Option<String>,

    /// Custom build command (overrides preset default)
    #[schema(example = "npm run build:production")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_command: Option<String>,

    /// Custom output directory (overrides preset default)
    /// Common values: "dist", "build", ".next", "out"
    #[schema(example = "dist")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_dir: Option<String>,

    /// Custom build context path (relative to repository root)
    /// Useful for monorepo setups where the app is in a subdirectory
    #[schema(example = "./apps/frontend")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_context: Option<String>,
}

/// Union type for preset configurations
/// Use the appropriate configuration type based on your preset
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(untagged)]
pub enum PresetConfigSchema {
    /// Configuration for Dockerfile preset
    Dockerfile(DockerfilePresetConfig),
    /// Configuration for Nixpacks preset (uses nixpacks.toml, no params needed)
    Nixpacks(NixpacksPresetConfig),
    /// Configuration for static site presets (Vite, Next.js, etc.)
    Static(StaticPresetConfig),
}

impl PresetConfigSchema {
    /// Convert to generic JSON value for database storage
    pub fn to_json(&self) -> Result<serde_json::Value, serde_json::Error> {
        serde_json::to_value(self)
    }

    /// Create DockerfilePresetConfig from JSON
    pub fn from_dockerfile_json(
        value: &serde_json::Value,
    ) -> Result<DockerfilePresetConfig, serde_json::Error> {
        serde_json::from_value(value.clone())
    }

    /// Create StaticPresetConfig from JSON
    pub fn from_static_json(
        value: &serde_json::Value,
    ) -> Result<StaticPresetConfig, serde_json::Error> {
        serde_json::from_value(value.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dockerfile_config_serialization() {
        let config = DockerfilePresetConfig {
            dockerfile_path: Some("docker/Dockerfile".to_string()),
            build_context: Some("./api".to_string()),
        };

        let json = serde_json::to_value(&config).unwrap();
        assert_eq!(json["dockerfilePath"], "docker/Dockerfile");
        assert_eq!(json["buildContext"], "./api");
    }

    #[test]
    fn test_static_config_serialization() {
        let config = StaticPresetConfig {
            install_command: Some("bun install".to_string()),
            build_command: Some("bun run build".to_string()),
            output_dir: Some("dist".to_string()),
            build_context: Some("./frontend".to_string()),
        };

        let json = serde_json::to_value(&config).unwrap();
        assert_eq!(json["installCommand"], "bun install");
        assert_eq!(json["buildCommand"], "bun run build");
    }

    #[test]
    fn test_preset_config_schema_union() {
        let dockerfile_config = PresetConfigSchema::Dockerfile(DockerfilePresetConfig {
            dockerfile_path: Some("Dockerfile.prod".to_string()),
            build_context: None,
        });

        let json = serde_json::to_value(&dockerfile_config).unwrap();
        let deserialized: PresetConfigSchema = serde_json::from_value(json).unwrap();

        match deserialized {
            PresetConfigSchema::Dockerfile(config) => {
                assert_eq!(config.dockerfile_path, Some("Dockerfile.prod".to_string()));
            }
            _ => panic!("Expected Dockerfile variant"),
        }
    }
}
