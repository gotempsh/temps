//! Integration tests for temps-screenshots crate

use std::sync::Arc;
use temps_screenshots::{
    LocalScreenshotProvider, RemoteScreenshotProvider, ScreenshotError, ScreenshotProvider,
    ScreenshotService,
};

/// Test that the local provider can be created with default settings
#[tokio::test]
async fn test_local_provider_creation() {
    let provider = LocalScreenshotProvider::new();
    assert_eq!(provider.provider_name(), "local-headless-chrome");
}

/// Test that the local provider can be created with custom settings
#[tokio::test]
async fn test_local_provider_custom_config() {
    let provider = LocalScreenshotProvider::with_config(45, 1280, 720);
    assert_eq!(provider.provider_name(), "local-headless-chrome");
}

/// Test that invalid URLs are rejected by the local provider
#[tokio::test]
async fn test_local_provider_invalid_url() {
    let provider = LocalScreenshotProvider::new();
    let result = provider.capture_screenshot("not-a-valid-url").await;

    assert!(result.is_err());
    match result.unwrap_err() {
        ScreenshotError::InvalidUrl(msg) => {
            assert!(msg.contains("not-a-valid-url"));
        }
        e => panic!("Expected InvalidUrl error, got: {:?}", e),
    }
}

/// Test that empty URLs are rejected
#[tokio::test]
async fn test_local_provider_empty_url() {
    let provider = LocalScreenshotProvider::new();
    let result = provider.capture_screenshot("").await;
    assert!(result.is_err());
}

/// Test remote provider creation
#[tokio::test]
async fn test_remote_provider_creation() {
    let provider = RemoteScreenshotProvider::new(
        "https://screenshot.example.com/api".to_string(),
        Some("test-api-key".to_string()),
    );

    assert!(provider.is_ok());
    let provider = provider.unwrap();
    assert_eq!(provider.provider_name(), "remote-api");
}

/// Test remote provider creation without API key
#[tokio::test]
async fn test_remote_provider_no_api_key() {
    let provider =
        RemoteScreenshotProvider::new("https://screenshot.example.com/api".to_string(), None);

    assert!(provider.is_ok());
}

/// Test that remote provider rejects invalid URLs
#[tokio::test]
async fn test_remote_provider_invalid_url() {
    let provider =
        RemoteScreenshotProvider::new("https://screenshot.example.com/api".to_string(), None)
            .unwrap();

    let result = provider.capture_screenshot("invalid-url").await;
    assert!(result.is_err());
    match result.unwrap_err() {
        ScreenshotError::InvalidUrl(_) => (),
        e => panic!("Expected InvalidUrl error, got: {:?}", e),
    }
}

/// Test provider trait object
#[tokio::test]
async fn test_provider_trait_object() {
    let provider: Box<dyn ScreenshotProvider> = Box::new(LocalScreenshotProvider::new());
    assert_eq!(provider.provider_name(), "local-headless-chrome");
}

/// Test error types
#[test]
fn test_error_types() {
    // Test InvalidUrl error
    let err = ScreenshotError::InvalidUrl("test".to_string());
    assert!(format!("{}", err).contains("Invalid URL"));

    // Test CaptureFailed error
    let err = ScreenshotError::CaptureFailed("test".to_string());
    assert!(format!("{}", err).contains("Screenshot capture failed"));

    // Test ProviderNotConfigured error
    let err = ScreenshotError::ProviderNotConfigured;
    assert!(format!("{}", err).contains("not configured"));

    // Test ConfigError
    let err = ScreenshotError::ConfigError("test".to_string());
    assert!(format!("{}", err).contains("Configuration error"));

    // Test ProviderError
    let err = ScreenshotError::ProviderError("test".to_string());
    assert!(format!("{}", err).contains("Provider error"));
}

/// Test that error types implement std::error::Error
#[test]
fn test_error_trait() {
    let err = ScreenshotError::InvalidUrl("test".to_string());
    let _: &dyn std::error::Error = &err;
}

/// Test provider name consistency
#[test]
fn test_provider_names() {
    let local = LocalScreenshotProvider::new();
    assert_eq!(local.provider_name(), "local-headless-chrome");

    let remote = RemoteScreenshotProvider::new("https://example.com".to_string(), None).unwrap();
    assert_eq!(remote.provider_name(), "remote-api");
}

/// Test default trait implementations
#[test]
fn test_default_implementations() {
    let provider = LocalScreenshotProvider::default();
    assert_eq!(provider.provider_name(), "local-headless-chrome");
}

/// Test Arc wrapping (common pattern in the codebase)
#[test]
fn test_arc_wrapping() {
    let provider = Arc::new(LocalScreenshotProvider::new());
    assert_eq!(provider.provider_name(), "local-headless-chrome");

    // Test that Arc can be cloned
    let provider2 = provider.clone();
    assert_eq!(provider2.provider_name(), "local-headless-chrome");
}

/// Test that providers are Send + Sync
#[test]
fn test_send_sync() {
    fn assert_send<T: Send>() {}
    fn assert_sync<T: Sync>() {}

    assert_send::<LocalScreenshotProvider>();
    assert_sync::<LocalScreenshotProvider>();

    assert_send::<RemoteScreenshotProvider>();
    assert_sync::<RemoteScreenshotProvider>();
}

/// Test concurrent provider usage (Arc + async)
#[tokio::test]
async fn test_concurrent_usage() {
    let provider = Arc::new(LocalScreenshotProvider::new());

    let handles: Vec<_> = (0..3)
        .map(|_| {
            let p = provider.clone();
            tokio::spawn(async move {
                assert_eq!(p.provider_name(), "local-headless-chrome");
            })
        })
        .collect();

    for handle in handles {
        handle.await.unwrap();
    }
}
