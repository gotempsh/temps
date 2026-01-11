//! Python preset implementation using Nixpacks
//!
//! This preset detects Python projects (requirements.txt, pyproject.toml, etc.)
//! and uses Nixpacks for building.

use crate::{DockerfileConfig, DockerfileWithArgs, NixpacksPreset, NixpacksProvider, Preset, ProjectType};
use async_trait::async_trait;
use std::fmt;
use std::path::Path;

/// Python preset - delegates to Nixpacks with Python provider
#[derive(Debug, Clone, Copy)]
pub struct PythonPreset;

impl PythonPreset {
    /// Create a new Python preset instance
    pub fn new() -> Self {
        Self
    }
}

impl Default for PythonPreset {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Preset for PythonPreset {
    fn project_type(&self) -> ProjectType {
        ProjectType::Server
    }

    fn label(&self) -> String {
        "Python".to_string()
    }

    fn icon_url(&self) -> String {
        "/presets/python.svg".to_string()
    }

    fn description(&self) -> String {
        "Python web applications (Flask, FastAPI, Django, etc.)".to_string()
    }

    async fn dockerfile(&self, config: DockerfileConfig<'_>) -> DockerfileWithArgs {
        // Delegate to Nixpacks with Python provider
        let nixpacks = NixpacksPreset::new(NixpacksProvider::Python);
        nixpacks.dockerfile(config).await
    }

    async fn dockerfile_with_build_dir(&self, local_path: &Path) -> DockerfileWithArgs {
        // Delegate to Nixpacks with Python provider
        let nixpacks = NixpacksPreset::new(NixpacksProvider::Python);
        nixpacks.dockerfile_with_build_dir(local_path).await
    }

    fn install_command(&self, _local_path: &Path) -> String {
        // Pip handles dependencies from requirements.txt
        "pip install -r requirements.txt".to_string()
    }

    fn build_command(&self, _local_path: &Path) -> String {
        // Python typically doesn't have a build step
        "echo 'No build required for Python'".to_string()
    }

    fn dirs_to_upload(&self) -> Vec<String> {
        // Server applications don't need to upload static files
        vec![]
    }

    fn slug(&self) -> String {
        "python".to_string()
    }

    fn default_port(&self) -> u16 {
        8000 // FastAPI, Django, and most Python web frameworks default port
    }
}

impl fmt::Display for PythonPreset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "python")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_python_preset_properties() {
        let preset = PythonPreset::new();
        assert_eq!(preset.label(), "Python");
        assert_eq!(preset.to_string(), "python");
        assert_eq!(preset.slug(), "python");
        assert_eq!(preset.project_type(), ProjectType::Server);
        assert_eq!(preset.icon_url(), "/presets/python.svg");
    }

    #[test]
    fn test_python_commands() {
        let preset = PythonPreset::new();
        let path = Path::new(".");

        assert_eq!(preset.install_command(path), "pip install -r requirements.txt");
        assert_eq!(preset.build_command(path), "echo 'No build required for Python'");
    }
}
