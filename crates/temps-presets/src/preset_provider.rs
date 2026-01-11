//! Preset provider system
//!
//! Provides a provider-based architecture for preset detection and configuration.
//! Each preset has a provider that implements detection, build configuration, and Dockerfile generation.
//!
//! This system bridges the existing Preset enum (stored in database) with a modern provider architecture.

use crate::providers::app::App;
use anyhow::Result;
use std::collections::HashMap;
use temps_entities::preset::Preset;

/// Confidence level for preset detection
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Confidence {
    Low = 1,
    Medium = 2,
    High = 3,
}

/// Detection result for a preset
#[derive(Debug, Clone)]
pub struct PresetDetection {
    /// The detected preset
    pub preset: Preset,
    /// Confidence level of the detection
    pub confidence: Confidence,
    /// Optional build configuration
    pub build_config: Option<BuildConfig>,
}

/// Build configuration for a preset
#[derive(Debug, Clone)]
pub struct BuildConfig {
    /// Install command (e.g., "npm install", "pip install -r requirements.txt")
    pub install_cmd: Option<String>,
    /// Build command (e.g., "npm run build", "cargo build --release")
    pub build_cmd: Option<String>,
    /// Start command for running the application
    pub start_cmd: String,
    /// Output directory for built files (for static presets)
    pub output_dir: Option<String>,
    /// Port the application listens on
    pub port: u16,
    /// Whether this produces static files that need to be served
    pub static_serve: bool,
}

/// Trait for preset providers
///
/// Each preset provider implements detection logic and configuration
pub trait PresetProvider: Send + Sync {
    /// Get the preset this provider implements
    fn preset(&self) -> Preset;

    /// Detect if this preset matches the project
    ///
    /// Returns None if the preset doesn't match,
    /// or Some(confidence) if it does
    fn detect(&self, app: &App) -> Option<Confidence>;

    /// Get build configuration for this preset
    fn get_build_config(&self, app: &App) -> Option<BuildConfig>;

    /// Get the default port for this preset
    fn default_port(&self) -> Option<u16> {
        // Delegate to the Preset enum's exposed_port() method
        self.preset().exposed_port()
    }

    /// Get human-readable name
    fn name(&self) -> &'static str {
        self.preset().display_name()
    }

    /// Get slug (same as database value)
    fn slug(&self) -> &'static str {
        self.preset().as_str()
    }

    /// Generate Dockerfile for this preset
    fn generate_dockerfile(&self, app: &App) -> Result<String>;
}

/// Registry of all preset providers
pub struct PresetProviderRegistry {
    providers: Vec<Box<dyn PresetProvider>>,
    // Cache for quick lookup by slug
    by_slug: HashMap<String, usize>,
}

impl PresetProviderRegistry {
    /// Create a new registry with all providers
    pub fn new() -> Self {


        // Register all providers
        // registry.register(Box::new(providers::NextJsProvider));
        // TODO: Register other providers as they are implemented

        Self {
            providers: Vec::new(),
            by_slug: HashMap::new(),
        }
    }

    /// Register a provider
    pub fn register(&mut self, provider: Box<dyn PresetProvider>) {
        let slug = provider.slug().to_string();
        let index = self.providers.len();
        self.providers.push(provider);
        self.by_slug.insert(slug, index);
    }

    /// Detect preset from project files
    ///
    /// Returns the preset with the highest confidence match
    pub fn detect(&self, app: &App) -> Option<PresetDetection> {
        let mut best_match: Option<PresetDetection> = None;

        for provider in &self.providers {
            if let Some(confidence) = provider.detect(app) {
                let detection = PresetDetection {
                    preset: provider.preset(),
                    confidence,
                    build_config: provider.get_build_config(app),
                };

                // Update best match if this has higher confidence
                if best_match.as_ref().is_none_or(|m| confidence > m.confidence) {
                    best_match = Some(detection);
                }
            }
        }

        best_match
    }

    /// Get provider by preset enum
    pub fn get_provider(&self, preset: Preset) -> Option<&dyn PresetProvider> {
        let slug = preset.as_str();
        self.by_slug
            .get(slug)
            .and_then(|&index| self.providers.get(index))
            .map(|provider| provider.as_ref())
    }

    /// Get provider by slug
    pub fn get_provider_by_slug(&self, slug: &str) -> Option<&dyn PresetProvider> {
        self.by_slug
            .get(slug)
            .and_then(|&index| self.providers.get(index))
            .map(|provider| provider.as_ref())
    }

    /// Get all registered providers
    pub fn all_providers(&self) -> &[Box<dyn PresetProvider>] {
        &self.providers
    }
}

impl Default for PresetProviderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // Mock provider for testing
    struct MockProvider;

    impl PresetProvider for MockProvider {
        fn preset(&self) -> Preset {
            Preset::NextJs
        }

        fn detect(&self, app: &App) -> Option<Confidence> {
            if app.includes_file("next.config.js") {
                Some(Confidence::High)
            } else {
                None
            }
        }

        fn get_build_config(&self, _app: &App) -> Option<BuildConfig> {
            Some(BuildConfig {
                install_cmd: Some("npm install".to_string()),
                build_cmd: Some("npm run build".to_string()),
                start_cmd: "npm start".to_string(),
                output_dir: Some(".next".to_string()),
                port: 3000,
                static_serve: false,
            })
        }

        fn generate_dockerfile(&self, _app: &App) -> Result<String> {
            Ok("FROM node:22-alpine\n".to_string())
        }
    }

    #[test]
    fn test_registry_register() {
        let mut registry = PresetProviderRegistry::new();
        registry.register(Box::new(MockProvider));

        assert_eq!(registry.providers.len(), 1);
        assert!(registry.by_slug.contains_key("nextjs"));
    }

    #[test]
    fn test_registry_get_provider() {
        let mut registry = PresetProviderRegistry::new();
        registry.register(Box::new(MockProvider));

        let provider = registry.get_provider(Preset::NextJs);
        assert!(provider.is_some());
        assert_eq!(provider.unwrap().slug(), "nextjs");
    }

    #[test]
    fn test_registry_get_provider_by_slug() {
        let mut registry = PresetProviderRegistry::new();
        registry.register(Box::new(MockProvider));

        let provider = registry.get_provider_by_slug("nextjs");
        assert!(provider.is_some());
        assert_eq!(provider.unwrap().name(), "Next.js");
    }

    #[test]
    fn test_registry_detect() {
        let mut registry = PresetProviderRegistry::new();
        registry.register(Box::new(MockProvider));

        let mut files = HashMap::new();
        files.insert("next.config.js".to_string(), "module.exports = {}".to_string());
        let app = App::from_tree(PathBuf::from("/test"), files);

        let detection = registry.detect(&app);
        assert!(detection.is_some());

        let detection = detection.unwrap();
        assert_eq!(detection.preset, Preset::NextJs);
        assert_eq!(detection.confidence, Confidence::High);
    }
}
