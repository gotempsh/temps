//! Nixpacks preset provider

use crate::providers::app::App;
use crate::preset_provider::{BuildConfig, Confidence, PresetProvider};
use anyhow::Result;
use temps_entities::preset::Preset;

pub struct NixpacksPresetProvider;

impl PresetProvider for NixpacksPresetProvider {
    fn preset(&self) -> Preset {
        Preset::Nixpacks
    }

    fn detect(&self, _app: &App) -> Option<Confidence> {
        // Nixpacks is a fallback preset - always returns low confidence
        Some(Confidence::Low)
    }

    fn get_build_config(&self, _app: &App) -> Option<BuildConfig> {
        // Nixpacks auto-detects everything
        None
    }

    fn generate_dockerfile(&self, _app: &App) -> Result<String> {
        Err(anyhow::anyhow!("Nixpacks uses auto-detection, no Dockerfile generation needed"))
    }
}
