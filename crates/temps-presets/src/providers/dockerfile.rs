//! Dockerfile preset provider

use crate::providers::app::App;
use crate::preset_provider::{BuildConfig, Confidence, PresetProvider};
use anyhow::Result;
use temps_entities::preset::Preset;

pub struct DockerfilePresetProvider;

impl PresetProvider for DockerfilePresetProvider {
    fn preset(&self) -> Preset {
        Preset::Dockerfile
    }

    fn detect(&self, app: &App) -> Option<Confidence> {
        if app.includes_file("Dockerfile") {
            Some(Confidence::High)
        } else {
            None
        }
    }

    fn get_build_config(&self, _app: &App) -> Option<BuildConfig> {
        // Dockerfile preset doesn't provide build config
        // as the Dockerfile itself defines everything
        None
    }

    fn generate_dockerfile(&self, app: &App) -> Result<String> {
        // Return the existing Dockerfile
        app.read_file("Dockerfile")
            .map_err(|e| anyhow::anyhow!("Failed to read Dockerfile: {}", e))
    }
}
