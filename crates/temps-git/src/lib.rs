//! Git provider integrations and repository management

// Module removed for initial build
// Module removed for initial build
// Module removed for initial build

// Re-export removed
// Re-export removed
pub mod handlers;
pub mod services;
pub mod plugin;

// Re-export the plugin for easy access
pub use plugin::GitPlugin;

// Re-export commonly used types for external crates
pub use services::git_provider_manager_trait::{
    GitProviderManagerTrait,
    GitProviderManagerError,
    RepositoryInfo,
};
pub use services::git_provider_manager::GitProviderManager;
