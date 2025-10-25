//! Rust preset implementation using Nixpacks
//!
//! This preset detects Rust projects (Cargo.toml) and uses Nixpacks for building.

use crate::{DockerfileConfig, DockerfileWithArgs, NixpacksPreset, NixpacksProvider, Preset, ProjectType};
use async_trait::async_trait;
use std::fmt;
use std::path::Path;

/// Rust preset - delegates to Nixpacks with Rust provider
#[derive(Debug, Clone, Copy)]
pub struct RustPreset;

impl RustPreset {
    /// Create a new Rust preset instance
    pub fn new() -> Self {
        Self
    }
}

impl Default for RustPreset {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Preset for RustPreset {
    fn project_type(&self) -> ProjectType {
        ProjectType::Server
    }

    fn label(&self) -> String {
        "Rust".to_string()
    }

    fn icon_url(&self) -> String {
        "/presets/rust.svg".to_string()
    }

    fn description(&self) -> String {
        "Rust web applications and services".to_string()
    }

    async fn dockerfile(&self, config: DockerfileConfig<'_>) -> DockerfileWithArgs {
        // Delegate to Nixpacks with Rust provider
        let nixpacks = NixpacksPreset::new(NixpacksProvider::Rust);
        nixpacks.dockerfile(config).await
    }

    async fn dockerfile_with_build_dir(&self, local_path: &Path) -> DockerfileWithArgs {
        // Delegate to Nixpacks with Rust provider
        let nixpacks = NixpacksPreset::new(NixpacksProvider::Rust);
        nixpacks.dockerfile_with_build_dir(local_path).await
    }

    fn install_command(&self, _local_path: &Path) -> String {
        // Cargo handles dependencies automatically
        "cargo fetch".to_string()
    }

    fn build_command(&self, _local_path: &Path) -> String {
        "cargo build --release".to_string()
    }

    fn dirs_to_upload(&self) -> Vec<String> {
        // Server applications don't need to upload static files
        vec![]
    }

    fn slug(&self) -> String {
        "rust".to_string()
    }
}

impl fmt::Display for RustPreset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "rust")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rust_preset_properties() {
        let preset = RustPreset::new();
        assert_eq!(preset.label(), "Rust");
        assert_eq!(preset.to_string(), "rust");
        assert_eq!(preset.project_type(), ProjectType::Server);
        assert_eq!(preset.icon_url(), "/presets/rust.svg");
    }

    #[test]
    fn test_rust_commands() {
        let preset = RustPreset::new();
        let path = Path::new(".");

        assert_eq!(preset.install_command(path), "cargo fetch");
        assert_eq!(preset.build_command(path), "cargo build --release");
    }
}
