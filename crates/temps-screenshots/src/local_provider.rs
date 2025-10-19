//! Local Screenshot Provider using Headless Chrome

use async_trait::async_trait;
use headless_chrome::{Browser, LaunchOptions};
use std::time::Duration;
use tracing::{debug, error, info};

use crate::error::{ScreenshotError, ScreenshotResult};
use crate::provider::ScreenshotProvider;

/// Local screenshot provider using headless Chrome
pub struct LocalScreenshotProvider {
    /// Timeout for page load in seconds
    timeout_seconds: u64,
    /// Viewport width
    viewport_width: u32,
    /// Viewport height
    viewport_height: u32,
}

impl LocalScreenshotProvider {
    /// Create a new local screenshot provider with default settings
    pub fn new() -> Self {
        Self {
            timeout_seconds: 30,
            viewport_width: 1920,
            viewport_height: 1080,
        }
    }

    /// Create a new local screenshot provider with custom settings
    pub fn with_config(timeout_seconds: u64, viewport_width: u32, viewport_height: u32) -> Self {
        Self {
            timeout_seconds,
            viewport_width,
            viewport_height,
        }
    }
}

impl Default for LocalScreenshotProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ScreenshotProvider for LocalScreenshotProvider {
    async fn capture_screenshot(&self, url: &str) -> ScreenshotResult<Vec<u8>> {
        debug!(
            "Capturing screenshot of {} using local headless Chrome",
            url
        );

        // Validate URL
        if url::Url::parse(url).is_err() {
            return Err(ScreenshotError::InvalidUrl(format!("Invalid URL: {}", url)));
        }

        // Launch browser in a blocking context since headless_chrome is sync
        let browser = tokio::task::spawn_blocking({
            let timeout = self.timeout_seconds;
            let width = self.viewport_width;
            let height = self.viewport_height;
            let url = url.to_string();

            move || -> ScreenshotResult<Vec<u8>> {
                // Use LaunchOptions builder pattern for cleaner config
                let options = LaunchOptions::default_builder()
                    .headless(true) // Must be headless for server environments
                    .sandbox(false) // Disable sandbox for Docker compatibility
                    .idle_browser_timeout(Duration::from_secs(timeout))
                    .window_size(Some((width, height))) // Set window size
                    .build()
                    .map_err(|e| {
                        error!("Failed to build launch options: {}", e);
                        ScreenshotError::ChromeError(format!("Failed to build options: {}", e))
                    })?;

                // Launch browser
                let browser = Browser::new(options).map_err(|e| {
                    error!("Failed to launch Chrome browser: {}", e);
                    ScreenshotError::ChromeError(format!("Failed to launch browser: {}", e))
                })?;

                debug!("Browser launched successfully");

                // Create tab, navigate, and capture screenshot - all in one chain
                let screenshot_data = browser
                    .new_tab()
                    .map_err(|e| {
                        error!("Failed to create new tab: {}", e);
                        ScreenshotError::ChromeError(format!("Failed to create tab: {}", e))
                    })?
                    .navigate_to(&url)
                    .map_err(|e| {
                        error!("Failed to navigate to {}: {}", url, e);
                        ScreenshotError::ChromeError(format!("Failed to navigate: {}", e))
                    })?
                    .wait_until_navigated()
                    .map_err(|e| {
                        error!("Page navigation timeout for {}: {}", url, e);
                        ScreenshotError::ChromeError(format!("Navigation timeout: {}", e))
                    })?
                    .capture_screenshot(
                        headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png,
                        None, // Quality (only for JPEG)
                        None, // Clip region
                        true, // Capture beyond viewport (full page)
                    )
                    .map_err(|e| {
                        error!("Failed to capture screenshot: {}", e);
                        ScreenshotError::ChromeError(format!("Screenshot capture failed: {}", e))
                    })?;

                info!(
                    "Successfully captured screenshot of {} ({} bytes)",
                    url,
                    screenshot_data.len()
                );
                Ok(screenshot_data)
            }
        })
        .await
        .map_err(|e| {
            error!("Screenshot task panicked: {}", e);
            ScreenshotError::CaptureFailed(format!("Task execution failed: {}", e))
        })??;

        Ok(browser)
    }

    fn provider_name(&self) -> &'static str {
        "local-headless-chrome"
    }

    async fn is_available(&self) -> bool {
        // Try to launch browser to check if Chrome is available
        tokio::task::spawn_blocking(|| {
            let options = LaunchOptions::default_builder()
                .headless(true)
                .sandbox(false)
                .idle_browser_timeout(Duration::from_secs(5))
                .build();

            match options {
                Ok(opts) => Browser::new(opts).is_ok(),
                Err(_) => false,
            }
        })
        .await
        .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_local_provider_creation() {
        let provider = LocalScreenshotProvider::new();
        assert_eq!(provider.provider_name(), "local-headless-chrome");
        assert_eq!(provider.viewport_width, 1920);
        assert_eq!(provider.viewport_height, 1080);
    }

    #[tokio::test]
    async fn test_local_provider_with_config() {
        let provider = LocalScreenshotProvider::with_config(60, 1024, 768);
        assert_eq!(provider.timeout_seconds, 60);
        assert_eq!(provider.viewport_width, 1024);
        assert_eq!(provider.viewport_height, 768);
    }

    #[tokio::test]
    async fn test_invalid_url() {
        let provider = LocalScreenshotProvider::new();
        let result = provider.capture_screenshot("not-a-valid-url").await;
        assert!(result.is_err());
        match result {
            Err(ScreenshotError::InvalidUrl(_)) => (),
            _ => panic!("Expected InvalidUrl error"),
        }
    }

    #[tokio::test]
    async fn test_capture_screenshot_example_com() {
        use std::fs;

        let provider = LocalScreenshotProvider::new();
        let result = provider.capture_screenshot("https://example.com").await;

        match result {
            Ok(screenshot_data) => {
                // Save to temp directory for inspection
                let output_path = std::env::temp_dir().join("test_screenshot_example_com.png");
                fs::write(&output_path, &screenshot_data).expect("Failed to write screenshot");

                println!("✅ Screenshot saved to: {}", output_path.display());
                println!("📊 Screenshot size: {} bytes", screenshot_data.len());

                // Verify it's a valid PNG
                assert!(screenshot_data.len() > 100, "Screenshot data too small");
                assert_eq!(
                    &screenshot_data[0..8],
                    b"\x89PNG\r\n\x1a\n",
                    "Not a valid PNG file"
                );
            }
            Err(e) => {
                panic!("Failed to capture screenshot: {}", e);
            }
        }
    }

    #[tokio::test]
    async fn test_capture_screenshot_github() {
        use std::fs;

        let provider = LocalScreenshotProvider::with_config(30, 1920, 1080);
        let result = provider.capture_screenshot("https://github.com").await;

        match result {
            Ok(screenshot_data) => {
                // Save to temp directory for inspection
                let output_path = std::env::temp_dir().join("test_screenshot_github.png");
                fs::write(&output_path, &screenshot_data).expect("Failed to write screenshot");

                println!("✅ Screenshot saved to: {}", output_path.display());
                println!("📊 Screenshot size: {} bytes", screenshot_data.len());

                // Verify it's a valid PNG
                assert!(
                    screenshot_data.len() > 1000,
                    "Screenshot data seems too small for a complex page"
                );
                assert_eq!(
                    &screenshot_data[0..8],
                    b"\x89PNG\r\n\x1a\n",
                    "Not a valid PNG file"
                );
            }
            Err(e) => {
                panic!("Failed to capture screenshot: {}", e);
            }
        }
    }

    #[tokio::test]
    async fn test_capture_screenshot_mobile_viewport() {
        use std::fs;

        // Test with mobile viewport dimensions
        let provider = LocalScreenshotProvider::with_config(30, 375, 812); // iPhone X dimensions
        let result = provider.capture_screenshot("https://example.com").await;

        match result {
            Ok(screenshot_data) => {
                // Save to temp directory for inspection
                let output_path = std::env::temp_dir().join("test_screenshot_mobile.png");
                fs::write(&output_path, &screenshot_data).expect("Failed to write screenshot");

                println!("✅ Mobile screenshot saved to: {}", output_path.display());
                println!("📊 Screenshot size: {} bytes", screenshot_data.len());

                // Verify it's a valid PNG
                assert!(screenshot_data.len() > 100, "Screenshot data too small");
                assert_eq!(
                    &screenshot_data[0..8],
                    b"\x89PNG\r\n\x1a\n",
                    "Not a valid PNG file"
                );
            }
            Err(e) => {
                panic!("Failed to capture mobile screenshot: {}", e);
            }
        }
    }
}
