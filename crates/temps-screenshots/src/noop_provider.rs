//! No-op Screenshot Provider
//!
//! A provider that does nothing - useful for environments where screenshots
//! are not needed (e.g., VPS without Chrome, CI/CD environments, etc.)

use async_trait::async_trait;
use tracing::debug;

use crate::error::{ScreenshotError, ScreenshotResult};
use crate::provider::ScreenshotProvider;

/// No-op screenshot provider that always succeeds but returns empty data
///
/// Use this provider when screenshot functionality is not needed or when
/// running in environments without Chrome/browser support.
///
/// Enable via environment variable: `TEMPS_SCREENSHOT_PROVIDER=noop`
pub struct NoopScreenshotProvider;

impl NoopScreenshotProvider {
    /// Create a new no-op screenshot provider
    pub fn new() -> Self {
        Self
    }
}

impl Default for NoopScreenshotProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ScreenshotProvider for NoopScreenshotProvider {
    async fn capture_screenshot(&self, url: &str) -> ScreenshotResult<Vec<u8>> {
        debug!(
            "NoopScreenshotProvider: Skipping screenshot capture for {} (noop mode)",
            url
        );
        // Return an error indicating screenshots are disabled
        // This is more honest than returning empty data
        Err(ScreenshotError::CaptureFailed(
            "Screenshot provider is disabled (noop mode). Set TEMPS_SCREENSHOT_PROVIDER to 'local' or 'remote' to enable.".to_string()
        ))
    }

    fn provider_name(&self) -> &'static str {
        "noop"
    }

    async fn is_available(&self) -> bool {
        // Always available since it doesn't do anything
        debug!("NoopScreenshotProvider: is_available() returning true (noop mode)");
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_noop_provider_creation() {
        let provider = NoopScreenshotProvider::new();
        assert_eq!(provider.provider_name(), "noop");
    }

    #[tokio::test]
    async fn test_noop_provider_is_always_available() {
        let provider = NoopScreenshotProvider::new();
        assert!(provider.is_available().await);
    }

    #[tokio::test]
    async fn test_noop_provider_capture_returns_error() {
        let provider = NoopScreenshotProvider::new();
        let result = provider.capture_screenshot("https://example.com").await;
        assert!(result.is_err());
        match result {
            Err(ScreenshotError::CaptureFailed(msg)) => {
                assert!(msg.contains("noop mode"));
            }
            _ => panic!("Expected CaptureFailed error"),
        }
    }
}
