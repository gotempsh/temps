//! Docusaurus v2 preset provider
//!
//! Detects Docusaurus v2 projects and provides build configuration

use crate::providers::app::App;
use crate::providers::package_json::PackageJson;
use super::package_manager::PackageManager;
use crate::preset_provider::{BuildConfig, Confidence, PresetProvider};
use anyhow::Result;
use temps_entities::preset::Preset;

pub struct DocusaurusPresetProvider;

impl PresetProvider for DocusaurusPresetProvider {
    fn preset(&self) -> Preset {
        Preset::Docusaurus
    }

    fn detect(&self, app: &App) -> Option<Confidence> {
        // Parse package.json to check for @docusaurus/core
        let package_json_str = app.read_file("package.json").ok()?;
        let package_json = PackageJson::from_str(&package_json_str).ok()?;

        // Get @docusaurus/core version
        let version = package_json.get_dependency("@docusaurus/core")?;

        // Reject if it's version 1.x
        if version.starts_with('1') || version.starts_with("^1") || version.starts_with("~1") {
            return None;
        }

        let has_config = app.includes_file("docusaurus.config.js")
            || app.includes_file("docusaurus.config.ts");

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
            start_cmd: "npx serve -s build -l 3000".to_string(),
            output_dir: Some("build".to_string()),
            port: 3000,
            static_serve: true, // Docusaurus produces static files
        })
    }

    fn generate_dockerfile(&self, app: &App) -> Result<String> {
        let build_config = self.get_build_config(app).ok_or_else(|| {
            anyhow::anyhow!("Failed to get build config for Docusaurus")
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
    fn test_detect_docusaurus_v2() {
        let mut files = HashMap::new();
        files.insert(
            "package.json".to_string(),
            r#"{"dependencies":{"@docusaurus/core":"2.4.0"}}"#.to_string(),
        );

        let app = create_test_app(files);
        let provider = DocusaurusPresetProvider;
        let confidence = provider.detect(&app);

        assert_eq!(confidence, Some(Confidence::Medium));
    }

    #[test]
    fn test_detect_docusaurus_v2_with_config() {
        let mut files = HashMap::new();
        files.insert(
            "package.json".to_string(),
            r#"{"dependencies":{"@docusaurus/core":"^2.4.0"}}"#.to_string(),
        );
        files.insert("docusaurus.config.js".to_string(), "module.exports = {}".to_string());

        let app = create_test_app(files);
        let provider = DocusaurusPresetProvider;
        let confidence = provider.detect(&app);

        assert_eq!(confidence, Some(Confidence::High));
    }

    #[test]
    fn test_does_not_detect_docusaurus_v1() {
        let mut files = HashMap::new();
        files.insert(
            "package.json".to_string(),
            r#"{"dependencies":{"@docusaurus/core":"1.14.7"}}"#.to_string(),
        );

        let app = create_test_app(files);
        let provider = DocusaurusPresetProvider;
        let confidence = provider.detect(&app);

        assert_eq!(confidence, None);
    }

    #[test]
    fn test_get_build_config() {
        let mut files = HashMap::new();
        files.insert(
            "package.json".to_string(),
            r#"{"dependencies":{"@docusaurus/core":"2.4.0"}}"#.to_string(),
        );
        files.insert("yarn.lock".to_string(), "".to_string());

        let app = create_test_app(files);
        let provider = DocusaurusPresetProvider;
        let config = provider.get_build_config(&app).unwrap();

        assert_eq!(config.port, 3000);
        assert!(config.install_cmd.unwrap().contains("yarn"));
        assert!(config.static_serve);
        assert_eq!(config.output_dir, Some("build".to_string()));
    }

    #[test]
    fn test_generate_dockerfile() {
        let mut files = HashMap::new();
        files.insert(
            "package.json".to_string(),
            r#"{"dependencies":{"@docusaurus/core":"2.4.0"}}"#.to_string(),
        );

        let app = create_test_app(files);
        let provider = DocusaurusPresetProvider;
        let dockerfile = provider.generate_dockerfile(&app).unwrap();

        assert!(dockerfile.contains("FROM node:22-alpine"));
        assert!(dockerfile.contains("EXPOSE 3000"));
        assert!(dockerfile.contains("npm ci"));
        assert!(dockerfile.contains("FROM nginx:alpine AS runner"));
    }
}
