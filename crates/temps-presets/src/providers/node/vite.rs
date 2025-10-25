//! Vite preset provider
//!
//! Detects Vite projects and provides build configuration

use crate::providers::app::App;
use super::package_manager::PackageManager;
use crate::preset_provider::{BuildConfig, Confidence, PresetProvider};
use anyhow::Result;
use temps_entities::preset::Preset;

pub struct VitePresetProvider;

impl PresetProvider for VitePresetProvider {
    fn preset(&self) -> Preset {
        Preset::Vite
    }

    fn detect(&self, app: &App) -> Option<Confidence> {
        let has_vite = app.has_any_dependency("vite");

        if !has_vite {
            return None;
        }

        let has_config = app.includes_file("vite.config.js")
            || app.includes_file("vite.config.ts")
            || app.includes_file("vite.config.mjs");

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
            start_cmd: "npx serve -s dist -l 5173".to_string(),
            output_dir: Some("dist".to_string()),
            port: 5173,
            static_serve: true, // Vite builds static files
        })
    }

    fn generate_dockerfile(&self, app: &App) -> Result<String> {
        let build_config = self.get_build_config(app).ok_or_else(|| {
            anyhow::anyhow!("Failed to get build config for Vite")
        })?;

        // Use shared Node.js Dockerfile generation
        use super::node_base::{NodeDockerfileConfig, generate_node_dockerfile};

        let dockerfile_config = NodeDockerfileConfig {
            install_cmd: build_config.install_cmd.unwrap_or_else(|| "npm ci".to_string()),
            build_cmd: build_config.build_cmd.unwrap_or_else(|| "npm run build".to_string()),
            start_cmd: build_config.start_cmd,
            output_dir: build_config.output_dir,
            port: 80, // nginx default port
            is_static: build_config.static_serve,
            build_env: Vec::new(),
        };

        Ok(generate_node_dockerfile(app, dockerfile_config))
    }
}
