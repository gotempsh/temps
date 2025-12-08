//! Rsbuild preset provider
//!
//! Detects Rsbuild projects and provides build configuration

use crate::providers::app::App;
use super::package_manager::PackageManager;
use crate::preset_provider::{BuildConfig, Confidence, PresetProvider};
use anyhow::Result;
use temps_entities::preset::Preset;

pub struct RsbuildPresetProvider;

impl PresetProvider for RsbuildPresetProvider {
    fn preset(&self) -> Preset {
        Preset::Rsbuild
    }

    fn detect(&self, app: &App) -> Option<Confidence> {
        let has_rsbuild = app.has_any_dependency("@rsbuild/core");

        if !has_rsbuild {
            return None;
        }

        let has_config = app.includes_file("rsbuild.config.ts")
            || app.includes_file("rsbuild.config.js")
            || app.includes_file("rsbuild.config.mjs");

        Some(if has_config {
            Confidence::High
        } else {
            Confidence::Medium
        })
    }

    fn get_build_config(&self, app: &App) -> Option<BuildConfig> {
        let pm = PackageManager::detect(app);

        Some(BuildConfig {
            install_cmd: Some(pm.install_command().to_string()),
            build_cmd: Some(pm.build_command("build")),
            start_cmd: "npx serve -s dist -l 3000".to_string(),
            output_dir: Some("dist".to_string()),
            port: 3000,
            static_serve: true, // Rsbuild produces static files
        })
    }

    fn generate_dockerfile(&self, app: &App) -> Result<String> {
        let build_config = self.get_build_config(app).ok_or_else(|| {
            anyhow::anyhow!("Failed to get build config for Rsbuild")
        })?;

        use super::node_base::{NodeDockerfileConfig, generate_node_dockerfile};

        let dockerfile_config = NodeDockerfileConfig {
            install_cmd: build_config.install_cmd.unwrap_or_else(|| "npm ci".to_string()),
            build_cmd: build_config.build_cmd.unwrap_or_else(|| "npm run build".to_string()),
            start_cmd: build_config.start_cmd,
            output_dir: build_config.output_dir,
            port: build_config.port,
            is_static: build_config.static_serve,
            build_env: Vec::new(),
            is_nextjs_standalone: false,
        };

        Ok(generate_node_dockerfile(app, dockerfile_config))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn create_test_app(files: HashMap<String, String>) -> App {
        App::from_tree(PathBuf::from("/test"), files)
    }

    #[test]
    fn test_detect_rsbuild() {
        let mut files = HashMap::new();
        files.insert(
            "package.json".to_string(),
            r#"{"dependencies":{"@rsbuild/core":"1.0.0"}}"#.to_string(),
        );

        let app = create_test_app(files);
        let provider = RsbuildPresetProvider;
        let confidence = provider.detect(&app);

        assert_eq!(confidence, Some(Confidence::Medium));
    }

    #[test]
    fn test_detect_rsbuild_with_config() {
        let mut files = HashMap::new();
        files.insert(
            "package.json".to_string(),
            r#"{"dependencies":{"@rsbuild/core":"1.0.0"}}"#.to_string(),
        );
        files.insert("rsbuild.config.ts".to_string(), "export default {}".to_string());

        let app = create_test_app(files);
        let provider = RsbuildPresetProvider;
        let confidence = provider.detect(&app);

        assert_eq!(confidence, Some(Confidence::High));
    }

    #[test]
    fn test_get_build_config() {
        let mut files = HashMap::new();
        files.insert(
            "package.json".to_string(),
            r#"{"dependencies":{"@rsbuild/core":"1.0.0"}}"#.to_string(),
        );
        files.insert("pnpm-lock.yaml".to_string(), "".to_string());

        let app = create_test_app(files);
        let provider = RsbuildPresetProvider;
        let config = provider.get_build_config(&app).unwrap();

        assert_eq!(config.port, 3000);
        assert!(config.install_cmd.unwrap().contains("pnpm"));
        assert!(config.static_serve);
    }

    #[test]
    fn test_generate_dockerfile() {
        let mut files = HashMap::new();
        files.insert(
            "package.json".to_string(),
            r#"{"dependencies":{"@rsbuild/core":"1.0.0"}}"#.to_string(),
        );

        let app = create_test_app(files);
        let provider = RsbuildPresetProvider;
        let dockerfile = provider.generate_dockerfile(&app).unwrap();

        assert!(dockerfile.contains("FROM node:22-alpine"));
        assert!(dockerfile.contains("EXPOSE 3000"));
        assert!(dockerfile.contains("npm ci"));
    }
}
