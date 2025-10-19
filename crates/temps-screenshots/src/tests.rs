//! Integration tests for screenshot service

use super::*;
use async_trait::async_trait;
use std::sync::Mutex;

// Mock provider for comprehensive testing
struct TestProvider {
    call_log: Mutex<Vec<String>>,
    response: Vec<u8>,
    should_fail: bool,
}

impl TestProvider {
    fn new(response: Vec<u8>, should_fail: bool) -> Self {
        Self {
            call_log: Mutex::new(Vec::new()),
            response,
            should_fail,
        }
    }

    fn get_call_log(&self) -> Vec<String> {
        self.call_log.lock().unwrap().clone()
    }
}

#[async_trait]
impl ScreenshotProvider for TestProvider {
    async fn capture_screenshot(&self, url: &str) -> ScreenshotResult<Vec<u8>> {
        self.call_log.lock().unwrap().push(url.to_string());

        if self.should_fail {
            return Err(ScreenshotError::CaptureFailed("Test failure".to_string()));
        }

        Ok(self.response.clone())
    }

    fn provider_name(&self) -> &'static str {
        "test-provider"
    }

    async fn is_available(&self) -> bool {
        !self.should_fail
    }
}

#[tokio::test]
async fn test_screenshot_provider_trait() {
    let provider = TestProvider::new(vec![1, 2, 3, 4], false);

    let result = provider.capture_screenshot("https://example.com").await;
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), vec![1, 2, 3, 4]);

    let call_log = provider.get_call_log();
    assert_eq!(call_log.len(), 1);
    assert_eq!(call_log[0], "https://example.com");
}

#[tokio::test]
async fn test_provider_failure() {
    let provider = TestProvider::new(vec![], true);

    let result = provider.capture_screenshot("https://example.com").await;
    assert!(result.is_err());

    match result {
        Err(ScreenshotError::CaptureFailed(_)) => (),
        _ => panic!("Expected CaptureFailed error"),
    }
}

#[tokio::test]
async fn test_provider_availability() {
    let available_provider = TestProvider::new(vec![1, 2, 3], false);
    assert!(available_provider.is_available().await);

    let unavailable_provider = TestProvider::new(vec![], true);
    assert!(!unavailable_provider.is_available().await);
}

#[tokio::test]
async fn test_error_types() {
    // Test InvalidUrl error
    let invalid_url_error = ScreenshotError::InvalidUrl("bad-url".to_string());
    assert!(matches!(invalid_url_error, ScreenshotError::InvalidUrl(_)));

    // Test CaptureFailed error
    let capture_error = ScreenshotError::CaptureFailed("Failed".to_string());
    assert!(matches!(capture_error, ScreenshotError::CaptureFailed(_)));

    // Test ProviderNotConfigured error
    let not_configured_error = ScreenshotError::ProviderNotConfigured;
    assert!(matches!(
        not_configured_error,
        ScreenshotError::ProviderNotConfigured
    ));
}

#[tokio::test]
async fn test_multiple_captures() {
    let provider = TestProvider::new(vec![1, 2, 3, 4], false);

    // Capture multiple screenshots
    let urls = vec![
        "https://example.com",
        "https://test.com",
        "https://demo.com",
    ];

    for url in &urls {
        let result = provider.capture_screenshot(url).await;
        assert!(result.is_ok());
    }

    let call_log = provider.get_call_log();
    assert_eq!(call_log.len(), 3);
    assert_eq!(call_log, urls);
}

#[test]
fn test_error_display() {
    let error = ScreenshotError::InvalidUrl("test".to_string());
    assert!(format!("{}", error).contains("Invalid URL"));

    let error = ScreenshotError::CaptureFailed("test".to_string());
    assert!(format!("{}", error).contains("Screenshot capture failed"));

    let error = ScreenshotError::ProviderNotConfigured;
    assert!(format!("{}", error).contains("not configured"));
}
