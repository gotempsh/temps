//! Git provider integrations and repository management

// Module removed for initial build
// Module removed for initial build
// Module removed for initial build

// Re-export removed
// Re-export removed
pub mod handlers;
pub mod plugin;
pub mod services;

// Re-export the plugin for easy access
pub use plugin::GitPlugin;

// Re-export commonly used types for external crates
pub use services::git_provider_manager::GitProviderManager;
pub use services::git_provider_manager_trait::{
    GitProviderManagerError, GitProviderManagerTrait, RepositoryInfo,
};
