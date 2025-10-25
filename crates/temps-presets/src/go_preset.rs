//! Go preset implementation using Nixpacks
//!
//! This preset detects Go projects (go.mod) and uses Nixpacks for building.

use crate::{DockerfileConfig, DockerfileWithArgs, NixpacksPreset, NixpacksProvider, Preset, ProjectType};
use async_trait::async_trait;
use std::fmt;
use std::path::Path;

/// Go preset - delegates to Nixpacks with Go provider
#[derive(Debug, Clone, Copy)]
pub struct GoPreset;

impl GoPreset {
    /// Create a new Go preset instance
    pub fn new() -> Self {
        Self
    }
}

impl Default for GoPreset {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Preset for GoPreset {
    fn project_type(&self) -> ProjectType {
        ProjectType::Server
    }

    fn label(&self) -> String {
        "Go".to_string()
    }

    fn icon_url(&self) -> String {
        "/presets/go.svg".to_string()
    }

    fn description(&self) -> String {
        "Go web applications and services".to_string()
    }

    async fn dockerfile(&self, config: DockerfileConfig<'_>) -> DockerfileWithArgs {
        // Delegate to Nixpacks with Go provider
        let nixpacks = NixpacksPreset::new(NixpacksProvider::Go);
        nixpacks.dockerfile(config).await
    }

    async fn dockerfile_with_build_dir(&self, local_path: &Path) -> DockerfileWithArgs {
        // Delegate to Nixpacks with Go provider
        let nixpacks = NixpacksPreset::new(NixpacksProvider::Go);
        nixpacks.dockerfile_with_build_dir(local_path).await
    }

    fn install_command(&self, _local_path: &Path) -> String {
        // Go mod handles dependencies automatically
        "go mod download".to_string()
    }

    fn build_command(&self, _local_path: &Path) -> String {
        "go build -o ./bin/app .".to_string()
    }

    fn dirs_to_upload(&self) -> Vec<String> {
        // Server applications don't need to upload static files
        vec![]
    }

    fn slug(&self) -> String {
        "go".to_string()
    }
}

impl fmt::Display for GoPreset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "go")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_go_preset_properties() {
        let preset = GoPreset::new();
        assert_eq!(preset.label(), "Go");
        assert_eq!(preset.to_string(), "go");
        assert_eq!(preset.slug(), "go");
        assert_eq!(preset.project_type(), ProjectType::Server);
        assert_eq!(preset.icon_url(), "/presets/go.svg");
    }

    #[test]
    fn test_go_commands() {
        let preset = GoPreset::new();
        let path = Path::new(".");

        assert_eq!(preset.install_command(path), "go mod download");
        assert_eq!(preset.build_command(path), "go build -o ./bin/app .");
    }
}
