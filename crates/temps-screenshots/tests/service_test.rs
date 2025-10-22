//! Service-level integration tests

use std::sync::Arc;
use temps_screenshots::{LocalScreenshotProvider, ScreenshotError, ScreenshotProvider};

/// Mock provider for testing without actual browser/network calls
struct MockScreenshotProvider {
    should_fail: bool,
    captured_urls: Arc<tokio::sync::Mutex<Vec<String>>>,
}

impl MockScreenshotProvider {
    fn new(should_fail: bool) -> Self {
        Self {
            should_fail,
            captured_urls: Arc::new(tokio::sync::Mutex::new(Vec::new())),
        }
    }

    async fn get_captured_urls(&self) -> Vec<String> {
        self.captured_urls.lock().await.clone()
    }
}

#[async_trait::async_trait]
impl ScreenshotProvider for MockScreenshotProvider {
    async fn capture_screenshot(&self, url: &str) -> Result<Vec<u8>, ScreenshotError> {
        // Record the URL
        self.captured_urls.lock().await.push(url.to_string());

        if self.should_fail {
            return Err(ScreenshotError::CaptureFailed("Mock failure".to_string()));
        }

        // Return a minimal PNG header
        Ok(vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A])
    }

    fn provider_name(&self) -> &'static str {
        "mock-provider"
    }

    async fn is_available(&self) -> bool {
        !self.should_fail
    }
}

/// Test mock provider directly
#[tokio::test]
async fn test_mock_provider_capture() {
    let mock_provider = MockScreenshotProvider::new(false);

    let result = mock_provider
        .capture_screenshot("https://example.com")
        .await;
    assert!(result.is_ok());

    let image_bytes = result.unwrap();
    assert_eq!(image_bytes.len(), 8); // PNG header size

    // Verify the URL was captured
    let captured = mock_provider.get_captured_urls().await;
    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0], "https://example.com");
}

#[tokio::test]
async fn test_mock_provider_failure() {
    let mock_provider = MockScreenshotProvider::new(true); // Will fail

    let result = mock_provider
        .capture_screenshot("https://example.com")
        .await;
    assert!(result.is_err());

    match result.unwrap_err() {
        ScreenshotError::CaptureFailed(msg) => {
            assert_eq!(msg, "Mock failure");
        }
        e => panic!("Expected CaptureFailed error, got: {:?}", e),
    }
}

#[tokio::test]
async fn test_mock_provider_name() {
    let mock_provider = MockScreenshotProvider::new(false);
    assert_eq!(mock_provider.provider_name(), "mock-provider");
}

#[tokio::test]
async fn test_mock_provider_availability() {
    let available_provider = MockScreenshotProvider::new(false);
    assert!(available_provider.is_available().await);

    let unavailable_provider = MockScreenshotProvider::new(true);
    assert!(!unavailable_provider.is_available().await);
}

#[tokio::test]
async fn test_mock_provider_multiple_captures() {
    let mock_provider = MockScreenshotProvider::new(false);

    // Capture multiple screenshots
    let urls = vec![
        "https://example.com",
        "https://test.com",
        "https://demo.com",
    ];

    for url in &urls {
        let result = mock_provider.capture_screenshot(url).await;
        assert!(result.is_ok());
    }

    // Verify all URLs were captured
    let captured = mock_provider.get_captured_urls().await;
    assert_eq!(captured.len(), 3);
    assert_eq!(captured, urls);
}

#[tokio::test]
async fn test_mock_provider_concurrent_captures() {
    let mock_provider = Arc::new(MockScreenshotProvider::new(false));

    // Spawn multiple concurrent capture tasks
    let mut handles = vec![];
    for i in 0..5 {
        let provider_clone = mock_provider.clone();
        let url = format!("https://example-{}.com", i);
        let handle = tokio::spawn(async move { provider_clone.capture_screenshot(&url).await });
        handles.push(handle);
    }

    // Wait for all tasks to complete
    for handle in handles {
        let result = handle.await.unwrap();
        assert!(result.is_ok());
    }

    // Verify all URLs were captured
    let captured = mock_provider.get_captured_urls().await;
    assert_eq!(captured.len(), 5);
}

#[tokio::test]
async fn test_local_provider_invalid_urls() {
    let provider = LocalScreenshotProvider::new();

    // Test various invalid URLs
    let invalid_urls = vec!["", "not-a-url", "ftp://invalid", "javascript:alert(1)"];

    for url in invalid_urls {
        let result = provider.capture_screenshot(url).await;
        assert!(result.is_err(), "Expected error for URL: {}", url);
    }
}
