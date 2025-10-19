//! Screenshot Provider Trait
//!
//! Defines the interface for screenshot providers (local, remote, etc.)

use async_trait::async_trait;
use crate::error::ScreenshotResult;

/// Screenshot provider trait - implement this for different screenshot backends
#[async_trait]
pub trait ScreenshotProvider: Send + Sync {
    /// Capture a screenshot of the given URL and return the image bytes
    async fn capture_screenshot(&self, url: &str) -> ScreenshotResult<Vec<u8>>;

    /// Get the name of this provider (for logging/debugging)
    fn provider_name(&self) -> &'static str;

    /// Check if the provider is available/configured
    async fn is_available(&self) -> bool;
}
