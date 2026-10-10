// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Local Screenshot Provider using Headless Chrome

use async_trait::async_trait;
use headless_chrome::{Browser, LaunchOptions};
use std::sync::{Arc, LazyLock};
use std::time::Duration;
use temps_core::log_transitions::{FailureLatch, FailureLog, DEFAULT_REMINDER_INTERVAL};
use tokio::sync::Mutex as AsyncMutex;
use tracing::{debug, error, info, warn};

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

/// Run blocking `work` while holding `lock` for exactly as long as it runs.
///
/// The guard is moved into the blocking task rather than kept on the caller's
/// stack, because dropping the returned future (a timeout, a cancelled
/// request) does not stop work already running on a blocking thread.
async fn run_blocking_holding_lock<T, F>(
    lock: Arc<AsyncMutex<()>>,
    work: F,
) -> Result<T, tokio::task::JoinError>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let guard = lock.lock_owned().await;
    tokio::task::spawn_blocking(move || {
        let _guard = guard;
        work()
    })
    .await
}

/// Whether headless Chrome is currently failing to launch on this host.
///
/// Chrome is an optional dependency: hosts without it simply don't get local
/// deployment screenshots (or switch to a remote provider in Settings). It is
/// reported once at WARN when first observed, with the fix, plus an hourly
/// reminder — not as an ERROR on every availability probe and every
/// deployment's screenshot attempt.
static CHROME_UNAVAILABLE: ChromeStatus = ChromeStatus::new();

/// Whether Chrome is currently failing to launch, and the last reason why.
///
/// The reason is kept so a probe that cannot run its own launch -- because
/// an earlier one is still holding the launch lock -- can still answer with
/// what is known instead of waiting for that launch.
struct ChromeStatus {
    latch: FailureLatch,
    reason: std::sync::Mutex<Option<String>>,
}

impl ChromeStatus {
    const fn new() -> Self {
        Self {
            latch: FailureLatch::new(DEFAULT_REMINDER_INTERVAL),
            reason: std::sync::Mutex::new(None),
        }
    }

    fn is_failing(&self) -> bool {
        self.latch.is_failing()
    }

    /// The reason recorded with the latest failure, while Chrome is failing.
    fn failure_reason(&self) -> Option<String> {
        if !self.is_failing() {
            return None;
        }
        self.reason
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn set_reason(&self, reason: Option<String>) {
        *self
            .reason
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = reason;
    }
}

/// Report that Chrome could not be launched. `reason` should name the fix.
fn report_chrome_unavailable(status: &ChromeStatus, reason: &str) -> FailureLog {
    status.set_reason(Some(reason.to_string()));
    let outcome = status.latch.record_failure();
    match outcome {
        FailureLog::Started => warn!(
            "Local screenshots are unavailable: headless Chrome could not be launched: {}",
            reason
        ),
        FailureLog::Reminder { consecutive } => warn!(
            consecutive_failures = consecutive,
            "Local screenshots are still unavailable: headless Chrome could not be launched: {}",
            reason
        ),
        FailureLog::Suppressed { consecutive } => debug!(
            consecutive_failures = consecutive,
            "Headless Chrome is still unavailable: {}", reason
        ),
    }
    outcome
}

/// Report that Chrome launched, logging the recovery if it had been failing.
fn report_chrome_available(status: &ChromeStatus) {
    status.set_reason(None);
    if let Some(failures) = status.latch.record_success() {
        info!(
            previous_failures = failures,
            "Headless Chrome is available again; local screenshots are enabled"
        );
    }
}

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

        // Hold the launch lock for the whole capture (not just the launch),
        // and inside the blocking task: a caller's timeout drops this future
        // but cannot stop the blocking Chrome work, so a lock held here would
        // be released while that Chrome still runs and let a retry start a
        // second browser beside it. The browser is owned by the closure, so it
        // is shut down exactly when the lock is released.
        let browser = run_blocking_holding_lock(CHROME_LAUNCH_LOCK.clone(), {
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
                        report_chrome_unavailable(
                            &CHROME_UNAVAILABLE,
                            &format!("failed to build launch options: {}", e),
                        );
                        ScreenshotError::ChromeError(format!("Failed to build options: {}", e))
                    })?;

                // Launch browser. Failing here means Chrome (an optional
                // dependency) is missing or broken on this host — reported
                // once by the latch, and returned to the caller.
                let browser = Browser::new(options).map_err(|e| {
                    report_chrome_unavailable(
                        &CHROME_UNAVAILABLE,
                        &format!("failed to launch browser: {}", e),
                    );
                    ScreenshotError::ChromeError(format!("Failed to launch browser: {}", e))
                })?;

                report_chrome_available(&CHROME_UNAVAILABLE);
                debug!("Browser launched successfully");

                let tab = browser.new_tab().map_err(|e| {
                    warn!("Failed to create new Chrome tab for screenshot of {}: {}", url, e);
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

                // The target is the user's deployed app; it being unreachable
                // is the app's state, not a Temps fault.
                tab.navigate_to(&url).map_err(|e| {
                    warn!("Failed to navigate to {} for screenshot: {}", url, e);
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
                        warn!("Failed to capture screenshot of {}: {}", url, e);
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
        probe_chrome_launch(
            CHROME_LAUNCH_LOCK.clone(),
            &CHROME_UNAVAILABLE,
            CHROME_PROBE_TIMEOUT,
            || {
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
            },
        )
        .await
    }
}

/// How long an availability probe waits for its own Chrome launch to report,
/// and, separately, for a launch or capture already holding the launch lock.
const CHROME_PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Check that `launch` can start Chrome, holding `lock` while it runs.
///
/// Every wait is bounded, so a caller (an HTTP request, a deployment job)
/// always gets an answer within about twice `timeout`:
///
/// - The launch lock is held by a launch or capture already in progress.
///   When Chrome is already known to be failing, its recorded reason is
///   returned at once: that holder is most likely a launch that will never
///   finish (an installed Chrome missing shared libraries can block before
///   printing its DevTools address), and queueing behind it is what kept a
///   screenshot request open indefinitely. Otherwise the probe waits up to
///   `timeout` for the lock; a holder that keeps it longer is a capture still
///   running, which means Chrome launches, so the probe does not start a
///   second browser beside it.
/// - The probe's own launch runs on a blocking thread that cannot be
///   cancelled. The owned guard is handed to a detached supervisor that
///   releases it only once that launch truly finishes, and the probe waits
///   `timeout` for the supervisor's report, not for the launch itself.
async fn probe_chrome_launch<F>(
    lock: Arc<AsyncMutex<()>>,
    status: &ChromeStatus,
    timeout: Duration,
    launch: F,
) -> ScreenshotResult<()>
where
    F: FnOnce() -> Result<(), String> + Send + 'static,
{
    let launch_guard = match lock.clone().try_lock_owned() {
        Ok(guard) => guard,
        Err(_) => {
            if let Some(reason) = status.failure_reason() {
                return Err(ScreenshotError::ChromeError(still_running_message(&reason)));
            }
            match tokio::time::timeout(timeout, lock.lock_owned()).await {
                Ok(guard) => guard,
                Err(_) => {
                    if let Some(reason) = status.failure_reason() {
                        return Err(ScreenshotError::ChromeError(still_running_message(&reason)));
                    }
                    debug!(
                        "Chrome is in use by a capture that has run for over {}s; \
                         treating it as available",
                        timeout.as_secs()
                    );
                    return Ok(());
                }
            }
        }
    };

    let handle = tokio::task::spawn_blocking(launch);
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let outcome = handle.await;
        drop(launch_guard);
        let _ = done_tx.send(outcome);
    });

    // Bounded so a host where Chrome cannot start still gets an answer.
    let check_result = tokio::time::timeout(timeout, done_rx).await;

    let reason = match check_result {
        Ok(Ok(Ok(Ok(())))) => {
            debug!("Chrome browser is available");
            report_chrome_available(status);
            return Ok(());
        }
        Ok(Ok(Ok(Err(e)))) => e,
        Ok(Ok(Err(e))) => format!("Chrome availability check task failed: {}", e),
        Ok(Err(_)) => "Chrome availability check task failed: supervisor task dropped before \
             reporting an outcome"
            .to_string(),
        Err(_) => format!(
            "Chrome availability check timed out after {} seconds; Chrome is most likely \
             installed but missing shared libraries (check `ldd <chrome-binary> | grep \
             'not found'`)",
            timeout.as_secs()
        ),
    };

    let message = format!(
        "{}. To fix: install Chrome's runtime dependencies (on Debian/Ubuntu: \
         `apt-get install -y chromium` or `apt-get install -y libnss3 libnspr4 libatk1.0-0 \
         libatk-bridge2.0-0 libcups2 libatspi2.0-0 libxcomposite1 libxdamage1 libxfixes3 \
         libxrandr2 libgbm1 libxkbcommon0 libpango-1.0-0 libcairo2 libasound2t64`), or switch \
         to a remote screenshot provider in Settings.",
        reason
    );
    report_chrome_unavailable(status, &message);
    Err(ScreenshotError::ChromeError(message))
}

/// The answer given while an earlier launch that already failed its check
/// still holds the launch lock.
fn still_running_message(reason: &str) -> String {
    format!(
        "{} (an earlier Chrome launch on this server is still running and has not finished; \
         no new launch was attempted)",
        reason
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn lock_outlives_a_timed_out_caller_until_the_blocking_work_ends() {
        let lock = Arc::new(AsyncMutex::new(()));
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();

        // The caller gives up long before the blocking work finishes.
        let gave_up = tokio::time::timeout(
            Duration::from_millis(50),
            run_blocking_holding_lock(lock.clone(), move || {
                let _ = release_rx.recv();
            }),
        )
        .await;
        assert!(gave_up.is_err(), "the caller should have timed out");

        // The work is still running, so a retry must not get the lock.
        assert!(
            lock.try_lock().is_err(),
            "the lock was released while the blocking work still ran"
        );

        release_tx.send(()).expect("release the blocking work");
        let reacquired = tokio::time::timeout(Duration::from_secs(5), lock.lock()).await;
        assert!(
            reacquired.is_ok(),
            "the lock must be released once the blocking work ends"
        );
    }

    /// #1383: a Chrome launch that never returns (an installed Chrome
    /// missing shared libraries can block before printing its DevTools URL)
    /// keeps the launch lock. A later probe must report the failure already
    /// observed instead of queueing behind it, or the request that asked
    /// for a screenshot never gets a response.
    #[tokio::test]
    async fn a_probe_behind_a_stuck_launch_reports_the_known_failure_promptly() {
        let stuck_launch = CHROME_LAUNCH_LOCK.clone().lock_owned().await;
        report_chrome_unavailable(
            &CHROME_UNAVAILABLE,
            "Chrome availability check timed out after 10 seconds",
        );

        let probe = tokio::time::timeout(
            Duration::from_secs(3),
            LocalScreenshotProvider::new().check_availability(),
        )
        .await;
        drop(stuck_launch);

        let error = probe
            .expect("the probe must not wait for a launch that is not finishing")
            .expect_err("Chrome is known to be failing");
        let message = error.to_string();
        assert!(message.contains("timed out after 10 seconds"), "{message}");
        assert!(message.contains("still running"), "{message}");
    }

    /// A launch that never returns, released only when the test says so.
    fn stuck_launch() -> (
        impl FnOnce() -> Result<(), String> + Send + 'static,
        std::sync::mpsc::Sender<()>,
    ) {
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let launch = move || {
            let _ = release_rx.recv();
            Err("released by the test".to_string())
        };
        (launch, release_tx)
    }

    #[tokio::test]
    async fn a_launch_that_never_returns_is_reported_and_blocks_no_later_probe() {
        let lock = Arc::new(AsyncMutex::new(()));
        let status = ChromeStatus::new();
        let timeout = Duration::from_millis(200);
        let (launch, release) = stuck_launch();

        let first = probe_chrome_launch(lock.clone(), &status, timeout, launch)
            .await
            .expect_err("a launch that does not report in time is a failure");
        assert!(
            first.to_string().contains("timed out after 0 seconds"),
            "{first}"
        );
        assert!(status.is_failing());

        // The stuck launch still holds the lock: the next probe answers at
        // once, with the recorded reason, and launches nothing.
        let launched = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let started = std::time::Instant::now();
        let second = probe_chrome_launch(lock.clone(), &status, Duration::from_secs(30), {
            let launched = launched.clone();
            move || {
                launched.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            }
        })
        .await
        .expect_err("Chrome is known to be failing");
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(second.to_string().contains("still running"), "{second}");
        assert!(second.to_string().contains("timed out"), "{second}");
        assert!(!launched.load(std::sync::atomic::Ordering::SeqCst));

        release.send(()).expect("release the stuck launch");
    }

    #[tokio::test]
    async fn a_busy_browser_that_is_not_failing_is_waited_for_then_probed() {
        let lock = Arc::new(AsyncMutex::new(()));
        let status = ChromeStatus::new();
        let capture = lock.clone().lock_owned().await;
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            drop(capture);
        });

        let result = probe_chrome_launch(lock, &status, Duration::from_secs(5), || Ok(())).await;
        assert!(result.is_ok(), "{result:?}");
        assert!(!status.is_failing());
    }

    #[tokio::test]
    async fn a_long_capture_is_not_mistaken_for_a_broken_browser() {
        let lock = Arc::new(AsyncMutex::new(()));
        let status = ChromeStatus::new();
        let _capture = lock.clone().lock_owned().await;
        let launched = Arc::new(std::sync::atomic::AtomicBool::new(false));

        let result = probe_chrome_launch(lock, &status, Duration::from_millis(100), {
            let launched = launched.clone();
            move || {
                launched.store(true, std::sync::atomic::Ordering::SeqCst);
                Err("must not run".to_string())
            }
        })
        .await;
        assert!(result.is_ok(), "{result:?}");
        assert!(!launched.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn a_failed_launch_names_the_reason_and_a_later_success_clears_it() {
        let lock = Arc::new(AsyncMutex::new(()));
        let status = ChromeStatus::new();
        let error = probe_chrome_launch(lock.clone(), &status, Duration::from_secs(5), || {
            Err("Failed to launch Chrome browser: libnss3.so missing".to_string())
        })
        .await
        .expect_err("launch failed");
        assert!(error.to_string().contains("libnss3.so missing"), "{error}");
        assert!(status
            .failure_reason()
            .is_some_and(|reason| reason.contains("libnss3.so missing")));

        probe_chrome_launch(lock, &status, Duration::from_secs(5), || Ok(()))
            .await
            .expect("Chrome launches again");
        assert_eq!(status.failure_reason(), None);
    }

    #[test]
    fn missing_chrome_is_reported_once_until_it_becomes_available() {
        let latch = ChromeStatus::new();

        assert_eq!(
            report_chrome_unavailable(&latch, "no chrome binary"),
            FailureLog::Started
        );
        // Every later probe or deployment screenshot stays quiet.
        for _ in 0..10 {
            assert!(!report_chrome_unavailable(&latch, "no chrome binary").should_log());
        }
        report_chrome_available(&latch);
        assert!(!latch.is_failing());
        assert_eq!(latch.failure_reason(), None);
        // Breaking again (e.g. a package removed) is a new transition.
        assert_eq!(
            report_chrome_unavailable(&latch, "no chrome binary"),
            FailureLog::Started
        );
    }

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
