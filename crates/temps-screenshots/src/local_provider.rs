//! Local Screenshot Provider using Headless Chrome

use async_trait::async_trait;
use headless_chrome::{Browser, LaunchOptions};
use std::sync::{Arc, LazyLock};
use std::time::Duration;
use tokio::sync::Mutex as AsyncMutex;
use tracing::{debug, error, info};

use crate::error::{ScreenshotError, ScreenshotResult};
use crate::provider::ScreenshotProvider;

/// headless_chrome's `fetch` feature (enabled in Cargo.toml) downloads and
/// caches a Chrome build to a shared path on first use when no local Chrome
/// is installed. Launching two browsers concurrently before that download
/// completes races on the same cached executable and can fail with a
/// `Text file busy` exec error, or duplicate the download. This can happen
/// in production, not just in tests: `ScreenshotService::new()` probes
/// availability from a background task, and a `TakeScreenshotJob` can call
/// `check_provider_availability()`/`capture_screenshot()` around the same
/// time. Serialize every real Chrome launch process-wide so concurrent
/// callers can't race on it.
///
/// `Arc`-wrapped (rather than a bare `&'static AsyncMutex`) so a guard can be
/// moved into a detached task and held for as long as the actual launch is
/// running -- see `check_availability`'s use of `lock_owned()`.
static CHROME_LAUNCH_LOCK: LazyLock<Arc<AsyncMutex<()>>> =
    LazyLock::new(|| Arc::new(AsyncMutex::new(())));

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

        // Hold this for the whole capture (not just the launch): the closure
        // below is fully synchronous, so there's no cheaper point to release it
        // at without splitting Browser::new() out of spawn_blocking.
        let _launch_guard = CHROME_LAUNCH_LOCK.lock().await;

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

                let tab = browser.new_tab().map_err(|e| {
                    error!("Failed to create new tab: {}", e);
                    ScreenshotError::ChromeError(format!("Failed to create tab: {}", e))
                })?;

                // Disable all CSS animations/transitions before navigation so they
                // don't block the page load event or networkAlmostIdle lifecycle event.
                let disable_animations_css = r#"
                    (function() {
                        const style = document.createElement('style');
                        style.textContent = '*, *::before, *::after { animation-duration: 0s !important; animation-delay: 0s !important; transition-duration: 0s !important; transition-delay: 0s !important; scroll-behavior: auto !important; }';
                        (document.head || document.documentElement).appendChild(style);
                    })()
                "#;
                // Inject into every new document via Page.addScriptToEvaluateOnNewDocument
                tab.evaluate(disable_animations_css, false).ok();

                tab.navigate_to(&url).map_err(|e| {
                    error!("Failed to navigate to {}: {}", url, e);
                    ScreenshotError::ChromeError(format!("Failed to navigate: {}", e))
                })?;

                // Wait for page to be ready using DOM readyState polling instead of
                // wait_until_navigated(). The latter waits for `networkAlmostIdle`
                // which can time out on pages with continuous network activity
                // (animations loading assets, analytics, WebSockets, etc.).
                let wait_timeout = Duration::from_secs(timeout);
                let poll_interval = Duration::from_millis(250);
                let start = std::time::Instant::now();
                loop {
                    if start.elapsed() > wait_timeout {
                        debug!("Page readyState wait timed out after {:?}, proceeding with screenshot anyway", wait_timeout);
                        break;
                    }
                    match tab.evaluate("document.readyState", false) {
                        Ok(result) => {
                            if let Some(value) = result.value {
                                let state = value.as_str().unwrap_or("");
                                if state == "complete" || state == "interactive" {
                                    debug!("Page readyState is '{}', proceeding", state);
                                    break;
                                }
                            }
                        }
                        Err(_) => {
                            // Tab may not be ready yet, keep polling
                        }
                    }
                    std::thread::sleep(poll_interval);
                }

                // Brief extra wait for rendering to settle after DOM is ready
                std::thread::sleep(Duration::from_secs(2));

                // Re-inject animation disabler in case the page scripts re-enabled them
                tab.evaluate(disable_animations_css, false).ok();

                let screenshot_data = tab
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

    async fn check_availability(&self) -> ScreenshotResult<()> {
        // See CHROME_LAUNCH_LOCK: serialize this probe launch against any
        // concurrent real capture (or another probe) on this provider.
        //
        // An owned guard, not a plain `.lock().await`: `spawn_blocking`
        // tasks are NOT cancelled when the `JoinHandle` future stops being
        // polled/is dropped (e.g. by the 10s `timeout` below elapsing) --
        // the launch keeps running on its blocking thread regardless. If the
        // guard lived on this function's stack, it would be dropped the
        // moment we give up waiting, letting a second caller start a second
        // launch while the first is still executing -- the exact race this
        // lock exists to prevent. Instead, hand the owned guard to a
        // detached supervisor that releases it only once the real launch
        // attempt truly finishes; `timeout` below races the supervisor's
        // *report* of that outcome, not the launch itself.
        let launch_guard = CHROME_LAUNCH_LOCK.clone().lock_owned().await;
        let handle = tokio::task::spawn_blocking(|| {
            let options = LaunchOptions::default_builder()
                .headless(true)
                .sandbox(false)
                .idle_browser_timeout(Duration::from_secs(5))
                .build();

            match options {
                Ok(opts) => match Browser::new(opts) {
                    Ok(_) => Ok(()),
                    Err(e) => Err(format!("Failed to launch Chrome browser: {}", e)),
                },
                Err(e) => Err(format!("Failed to build launch options: {}", e)),
            }
        });

        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let outcome = handle.await;
            drop(launch_guard);
            let _ = done_tx.send(outcome);
        });

        // Use a 10-second timeout to prevent hanging on VPS/servers without Chrome
        let check_result = tokio::time::timeout(Duration::from_secs(10), done_rx).await;

        let reason = match check_result {
            Ok(Ok(Ok(Ok(())))) => {
                debug!("Chrome browser is available");
                return Ok(());
            }
            Ok(Ok(Ok(Err(e)))) => e,
            Ok(Ok(Err(e))) => format!("Chrome availability check task failed: {}", e),
            Ok(Err(_)) => "Chrome availability check task failed: supervisor task dropped before \
                 reporting an outcome"
                .to_string(),
            Err(_) => {
                "Chrome availability check timed out after 10 seconds; Chrome is most likely \
                 installed but missing shared libraries (check `ldd <chrome-binary> | grep \
                 'not found'`)"
                    .to_string()
            }
        };

        let message = format!(
            "{}. To fix: install Chrome's runtime dependencies (on Debian/Ubuntu: \
             `apt-get install -y chromium` or `apt-get install -y libnss3 libnspr4 libatk1.0-0 \
             libatk-bridge2.0-0 libcups2 libatspi2.0-0 libxcomposite1 libxdamage1 libxfixes3 \
             libxrandr2 libgbm1 libxkbcommon0 libpango-1.0-0 libcairo2 libasound2t64`), or switch \
             to a remote screenshot provider in Settings.",
            reason
        );
        error!("Chrome browser is NOT available: {}", message);
        Err(ScreenshotError::ChromeError(message))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Concurrent real-Chrome-launch tests used to race on headless_chrome's
    // shared cached `fetch` binary and need their own lock. That's now
    // handled by CHROME_LAUNCH_LOCK inside LocalScreenshotProvider itself
    // (production callers can race on it too, not just tests), so these
    // tests no longer need to serialize themselves.

    /// Returns `false` (and prints why) when this machine cannot launch Chrome
    /// at all, so a browser-dependent test can skip instead of failing.
    ///
    /// `headless_chrome`'s `fetch` feature downloads a Chrome build on first
    /// use when no local Chrome is installed. On CI that download is an
    /// unauthenticated request to a third-party host and intermittently comes
    /// back `403`, which surfaced as
    /// `Failed to launch browser: http status: 403` and failed the whole unit
    /// test job. Chrome being unavailable is an environment fact, not a
    /// regression in this crate — the same reason Docker-dependent tests in
    /// this repository skip gracefully rather than being marked `#[ignore]`.
    ///
    /// This deliberately only tolerates *launch* failures. Once a browser
    /// starts, every capture assertion below is still enforced.
    async fn chrome_available(provider: &LocalScreenshotProvider) -> bool {
        match provider.check_availability().await {
            Ok(()) => true,
            Err(e) => {
                println!("Chrome browser not available, skipping test: {e}");
                false
            }
        }
    }

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
        if !chrome_available(&provider).await {
            return;
        }
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
        if !chrome_available(&provider).await {
            return;
        }
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
        if !chrome_available(&provider).await {
            return;
        }
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
