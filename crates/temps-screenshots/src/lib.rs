//! Screenshot Service
//!
//! Provides screenshot capture functionality for deployed applications.
//! Supports both local (headless Chrome) and remote screenshot providers.

use std::path::PathBuf;

pub mod error;
pub mod local_provider;
pub mod noop_provider;
pub mod plugin;
pub mod provider;
pub mod remote_provider;
pub mod service;

pub use error::{ScreenshotError, ScreenshotResult};
pub use local_provider::LocalScreenshotProvider;
pub use noop_provider::NoopScreenshotProvider;
pub use plugin::ScreenshotsPlugin;
pub use provider::ScreenshotProvider;
pub use remote_provider::RemoteScreenshotProvider;
pub use service::ScreenshotService;

/// Trait for screenshot service operations (used for dependency injection and testing)
#[async_trait::async_trait]
pub trait ScreenshotServiceTrait: Send + Sync {
    /// Capture a screenshot and save it to the static files directory
    async fn capture_and_save(&self, url: &str, filename: &str) -> ScreenshotResult<PathBuf>;

    /// Capture a screenshot and return the image bytes (without saving)
    async fn capture(&self, url: &str) -> ScreenshotResult<Vec<u8>>;

    /// Check if screenshots are enabled in configuration
    async fn is_enabled(&self) -> bool;

    /// Get the name of the current provider
    fn provider_name(&self) -> &'static str;

    /// Check if the provider is available
    async fn is_provider_available(&self) -> bool;
}

/// Implement the trait for the concrete ScreenshotService
#[async_trait::async_trait]
impl ScreenshotServiceTrait for ScreenshotService {
    async fn capture_and_save(&self, url: &str, filename: &str) -> ScreenshotResult<PathBuf> {
        self.capture_and_save(url, filename).await
    }

    async fn capture(&self, url: &str) -> ScreenshotResult<Vec<u8>> {
        self.capture(url).await
    }

    async fn is_enabled(&self) -> bool {
        self.is_enabled().await
    }

    fn provider_name(&self) -> &'static str {
        self.provider_name()
    }

    async fn is_provider_available(&self) -> bool {
        self.is_provider_available().await
    }
}

#[cfg(test)]
mod tests;
