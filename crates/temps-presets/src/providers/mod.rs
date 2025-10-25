//! Preset provider implementations
//!
//! Providers are organized by language/runtime for better maintainability and extensibility.
//! Each provider implements the PresetProvider trait and handles detection, configuration, and Dockerfile generation.

// Shared utilities
pub mod app;
pub mod package_json;

// Language-specific provider modules
pub mod node; // Node.js / TypeScript frameworks

// Generic presets
pub mod dockerfile;
pub mod nixpacks;

// Re-export commonly used types
pub use app::App;
pub use package_json::PackageJson;
pub use node::{
    AngularPresetProvider, AstroPresetProvider, DocusaurusPresetProvider,
    DocusaurusV1PresetProvider, NestJsPresetProvider, NextJsPresetProvider,
    RsbuildPresetProvider, VitePresetProvider, PackageManager,
};
pub use dockerfile::DockerfilePresetProvider;
pub use nixpacks::NixpacksPresetProvider;
