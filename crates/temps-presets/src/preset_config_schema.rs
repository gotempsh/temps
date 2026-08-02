//! Preset configuration schemas
//!
//! Defines the configuration options available for each preset type.
//! These schemas are used in the API and validated when creating/updating projects.

use serde::{Deserialize, Serialize};
use temps_entities::preset::{ComposePublicPort, DockerfileVariant, NixpacksProvider};

#[cfg(feature = "openapi")]
use utoipa::ToSchema;

/// Configuration for Dockerfile preset
/// Allows customizing the Dockerfile path and build context for Docker-based deployments
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct DockerfilePresetConfig {
    /// Catalog variant. Normally omitted; `custom` selects the generated
    /// Dockerfile compatibility preset.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variant: Option<DockerfileVariant>,

    /// Custom Dockerfile path (relative to build context)
    /// If not specified, defaults to "Dockerfile" in the build context
    #[cfg_attr(feature = "openapi", schema(example = "docker/Dockerfile"))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dockerfile_path: Option<String>,

    /// Custom build context path (relative to repository root)
    /// If not specified, uses the project's directory setting
    #[cfg_attr(feature = "openapi", schema(example = "./api"))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_context: Option<String>,
}

/// Configuration for Docker Compose deployments.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct DockerComposePresetConfig {
    /// Path to the Compose file relative to the project directory.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compose_path: Option<String>,

    /// User-provided docker-compose.override.yml content.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compose_override: Option<String>,

    /// Compose service ports that should be publicly routed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub public_ports: Vec<ComposePublicPort>,
}

/// Configuration for Nixpacks preset
/// Nixpacks provider and inline build-plan configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct NixpacksPresetConfig {
    /// Optional inline nixpacks.toml contents.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nixpacks_config: Option<String>,

    /// Ordered Nixpacks providers. Empty means repository config or auto-detect;
    /// include `...` to combine auto-detection with explicit providers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub providers: Vec<NixpacksProvider>,
}

/// Configuration for static site presets (Vite, Next.js, Docusaurus, etc.)
/// These presets build static sites that are served via a web server
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct StaticPresetConfig {
    /// Custom install command (overrides auto-detected package manager)
    #[cfg_attr(feature = "openapi", schema(example = "npm ci"))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub install_command: Option<String>,

    /// Custom build command (overrides preset default)
    #[cfg_attr(feature = "openapi", schema(example = "npm run build:production"))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_command: Option<String>,

    /// Custom output directory (overrides preset default)
    /// Common values: "dist", "build", ".next", "out"
    #[cfg_attr(feature = "openapi", schema(example = "dist"))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_dir: Option<String>,

    /// Custom build context path (relative to repository root)
    /// Useful for monorepo setups where the app is in a subdirectory
    #[cfg_attr(feature = "openapi", schema(example = "./apps/frontend"))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_context: Option<String>,
}

/// Union type for preset configurations
/// Use the appropriate configuration type based on your preset
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
#[serde(untagged)]
pub enum PresetConfigSchema {
    /// Configuration for Dockerfile preset
    Dockerfile(DockerfilePresetConfig),
    /// Configuration for Docker Compose
    DockerCompose(DockerComposePresetConfig),
    /// Configuration for Nixpacks provider selection and inline build plan
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
            variant: None,
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
    fn test_docker_compose_config_serialization() {
        let config = DockerComposePresetConfig {
            compose_path: Some("deploy/compose.yml".to_string()),
            compose_override: None,
            public_ports: vec![ComposePublicPort {
                service: "web".to_string(),
                port: 3000,
            }],
        };

        let json = serde_json::to_value(&config).unwrap();
        assert_eq!(json["composePath"], "deploy/compose.yml");
        assert_eq!(json["publicPorts"][0]["service"], "web");
        assert_eq!(json["publicPorts"][0]["port"], 3000);
    }

    #[test]
    fn test_nixpacks_config_serialization() {
        let config = NixpacksPresetConfig {
            nixpacks_config: Some("[start]\ncmd = \"python main.py\"".to_string()),
            providers: vec![NixpacksProvider::Auto, NixpacksProvider::Python],
        };

        let json = serde_json::to_value(&config).unwrap();
        assert_eq!(json["providers"], serde_json::json!(["...", "python"]));
        assert_eq!(
            json["nixpacksConfig"],
            "[start]\ncmd = \"python main.py\""
        );
    }

    #[test]
    fn test_preset_config_schema_union() {
        let dockerfile_config = PresetConfigSchema::Dockerfile(DockerfilePresetConfig {
            variant: None,
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
