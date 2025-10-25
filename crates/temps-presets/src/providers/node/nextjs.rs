//! Next.js preset provider
//!
//! Detects Next.js projects and provides build configuration

use crate::preset_provider::{BuildConfig, Confidence, PresetProvider};
use crate::providers::app::App;
use anyhow::Result;
use super::package_manager::PackageManager;
use temps_entities::preset::Preset;

pub struct NextJsPresetProvider;

impl PresetProvider for NextJsPresetProvider {
    fn preset(&self) -> Preset {
        Preset::NextJs
    }

    fn detect(&self, app: &App) -> Option<Confidence> {
        // Check for "next" in dependencies
        let has_next = app.has_any_dependency("next");

        if !has_next {
            return None;
        }

        // Check for config files for additional confidence
        let has_config = app.includes_file("next.config.js")
            || app.includes_file("next.config.mjs")
            || app.includes_file("next.config.ts");

        Some(if has_config {
            Confidence::High
        } else {
            Confidence::Medium
        })
    }

    fn get_build_config(&self, app: &App) -> Option<BuildConfig> {
        // Detect package manager
        let pm = PackageManager::detect(app);

        // Check if using standalone output (production optimization)
        let is_standalone = app
            .read_file("next.config.js")
            .ok()
            .as_ref()
            .is_some_and(|content| content.contains("output: 'standalone'"))
            || app
                .read_file("next.config.mjs")
                .ok()
                .as_ref()
                .is_some_and(|content| content.contains("output: 'standalone'"))
            || app
                .read_file("next.config.ts")
                .ok()
                .as_ref()
                .is_some_and(|content| content.contains("output: 'standalone'"));

        let start_cmd = if is_standalone {
            "node server.js".to_string()
        } else {
            // Use npx next start for production server (not npm run start which may run dev server)
            "npx next start".to_string()
        };

        Some(BuildConfig {
            install_cmd: Some(pm.install_command().to_string()),
            build_cmd: Some(pm.build_command("build")),
            start_cmd,
            output_dir: if is_standalone {
                Some(".next/standalone".to_string())
            } else {
                Some(".next".to_string())
            },
            port: 3000,
            static_serve: false,
        })
    }

    fn generate_dockerfile(&self, app: &App) -> Result<String> {
        let build_config = self.get_build_config(app).ok_or_else(|| {
            anyhow::anyhow!("Failed to get build config for Next.js")
        })?;

        // Use shared Node.js Dockerfile generation
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
    fn test_detect_nextjs() {
        let mut files = HashMap::new();
        files.insert(
            "package.json".to_string(),
            r#"{"dependencies":{"next":"14.0.0"}}"#.to_string(),
        );

        let app = create_test_app(files);
        let provider = NextJsPresetProvider;
        let confidence = provider.detect(&app);

        assert_eq!(confidence, Some(Confidence::Medium));
    }

    #[test]
    fn test_detect_nextjs_with_config() {
        let mut files = HashMap::new();
        files.insert(
            "package.json".to_string(),
            r#"{"dependencies":{"next":"14.0.0"}}"#.to_string(),
        );
        files.insert("next.config.js".to_string(), "module.exports = {}".to_string());

        let app = create_test_app(files);
        let provider = NextJsPresetProvider;
        let confidence = provider.detect(&app);

        assert_eq!(confidence, Some(Confidence::High));
    }

    #[test]
    fn test_get_build_config() {
        let mut files = HashMap::new();
        files.insert(
            "package.json".to_string(),
            r#"{"dependencies":{"next":"14.0.0"}}"#.to_string(),
        );
        files.insert("pnpm-lock.yaml".to_string(), "".to_string());

        let app = create_test_app(files);
        let provider = NextJsPresetProvider;
        let config = provider.get_build_config(&app).unwrap();

        assert_eq!(config.port, 3000);
        assert!(config.install_cmd.unwrap().contains("pnpm"));
        assert!(!config.static_serve);
    }

    #[test]
    fn test_generate_dockerfile() {
        let mut files = HashMap::new();
        files.insert(
            "package.json".to_string(),
            r#"{"dependencies":{"next":"14.0.0"}}"#.to_string(),
        );

        let app = create_test_app(files);
        let provider = NextJsPresetProvider;
        let dockerfile = provider.generate_dockerfile(&app).unwrap();

        assert!(dockerfile.contains("FROM node:22-alpine"));
        assert!(dockerfile.contains("EXPOSE 3000"));
        assert!(dockerfile.contains("npm ci"));
    }
}
