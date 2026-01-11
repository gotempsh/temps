//! Remote Screenshot Provider
//!
//! Uses an external screenshot service API

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tracing::{debug, error, info};

use crate::error::{ScreenshotError, ScreenshotResult};
use crate::provider::ScreenshotProvider;

/// Remote screenshot provider that calls an external API
pub struct RemoteScreenshotProvider {
    /// Base URL of the screenshot service
    service_url: String,
    /// API key for authentication (if required)
    api_key: Option<String>,
    /// HTTP client
    client: Client,
}

#[derive(Serialize)]
struct ScreenshotRequest {
    url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    height: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    full_page: Option<bool>,
}

#[derive(Deserialize)]
struct ScreenshotResponse {
    #[serde(default)]
    success: bool,
    #[serde(default)]
    image: Option<String>, // Base64 encoded image
    #[serde(default)]
    error: Option<String>,
}

impl RemoteScreenshotProvider {
    /// Create a new remote screenshot provider
    pub fn new(service_url: String, api_key: Option<String>) -> ScreenshotResult<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|e| {
                error!("Failed to create HTTP client: {}", e);
                ScreenshotError::HttpRequest(format!("Failed to create HTTP client: {}", e))
            })?;

        Ok(Self {
            service_url,
            api_key,
            client,
        })
    }
}

#[async_trait]
impl ScreenshotProvider for RemoteScreenshotProvider {
    async fn capture_screenshot(&self, url: &str) -> ScreenshotResult<Vec<u8>> {
        debug!(
            "Capturing screenshot of {} using remote service at {}",
            url, self.service_url
        );

        // Validate URL
        if url::Url::parse(url).is_err() {
            return Err(ScreenshotError::InvalidUrl(format!("Invalid URL: {}", url)));
        }

        let request_body = ScreenshotRequest {
            url: url.to_string(),
            width: Some(1920),
            height: Some(1080),
            full_page: Some(false),
        };

        let mut request = self.client.post(&self.service_url).json(&request_body);

        // Add API key if configured
        if let Some(ref api_key) = self.api_key {
            request = request.header("Authorization", format!("Bearer {}", api_key));
        }

        debug!("Sending screenshot request to remote service");

        let response = request.send().await.map_err(|e| {
            error!("HTTP request to screenshot service failed: {}", e);
            ScreenshotError::HttpRequest(format!("Request failed: {}", e))
        })?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response.text().await.unwrap_or_default();
            error!(
                "Screenshot service returned error {}: {}",
                status, error_text
            );
            return Err(ScreenshotError::HttpRequest(format!(
                "Service returned error {}: {}",
                status, error_text
            )));
        }

        let screenshot_response: ScreenshotResponse = response.json().await.map_err(|e| {
            error!("Failed to parse screenshot service response: {}", e);
            ScreenshotError::HttpRequest(format!("Failed to parse response: {}", e))
        })?;

        if !screenshot_response.success {
            let error_msg = screenshot_response
                .error
                .unwrap_or_else(|| "Unknown error".to_string());
            error!("Screenshot service reported failure: {}", error_msg);
            return Err(ScreenshotError::ProviderError(error_msg));
        }

        let image_data = screenshot_response.image.ok_or_else(|| {
            ScreenshotError::ProviderError("No image data in response".to_string())
        })?;

        // Decode base64 image
        use base64::Engine;
        let image_bytes = base64::engine::general_purpose::STANDARD
            .decode(&image_data)
            .map_err(|e| {
                error!("Failed to decode base64 image: {}", e);
                ScreenshotError::ProviderError(format!("Failed to decode image: {}", e))
            })?;

        info!(
            "Successfully captured screenshot of {} using remote service ({} bytes)",
            url,
            image_bytes.len()
        );

        Ok(image_bytes)
    }

    fn provider_name(&self) -> &'static str {
        "remote-api"
    }

    async fn is_available(&self) -> bool {
        // Try a simple health check to the service URL
        let health_url = format!("{}/health", self.service_url.trim_end_matches('/'));
        self.client
            .get(&health_url)
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_remote_provider_creation() {
        let provider = RemoteScreenshotProvider::new(
            "https://screenshot.example.com/api".to_string(),
            Some("test-key".to_string()),
        )
        .unwrap();
        assert_eq!(provider.provider_name(), "remote-api");
    }

    #[tokio::test]
    async fn test_invalid_url() {
        let provider =
            RemoteScreenshotProvider::new("https://screenshot.example.com/api".to_string(), None)
                .unwrap();
        let result = provider.capture_screenshot("not-a-valid-url").await;
        assert!(result.is_err());
        match result {
            Err(ScreenshotError::InvalidUrl(_)) => (),
            _ => panic!("Expected InvalidUrl error"),
        }
    }
}
