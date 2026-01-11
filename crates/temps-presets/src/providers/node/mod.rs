//! Node.js / TypeScript framework providers
//!
//! Providers for Node.js-based frameworks and build tools

// Shared Node.js functionality
pub mod node_base;
pub mod package_manager;

// Node.js framework providers
pub mod angular;
pub mod astro;
pub mod docusaurus;
pub mod docusaurus_v1;
pub mod nestjs;
pub mod nextjs;
pub mod rsbuild;
pub mod vite;

// Re-export for convenience
pub use angular::AngularPresetProvider;
pub use astro::AstroPresetProvider;
pub use docusaurus::DocusaurusPresetProvider;
pub use docusaurus_v1::DocusaurusV1PresetProvider;
pub use nestjs::NestJsPresetProvider;
pub use nextjs::NextJsPresetProvider;
pub use package_manager::PackageManager;
pub use rsbuild::RsbuildPresetProvider;
pub use vite::VitePresetProvider;
