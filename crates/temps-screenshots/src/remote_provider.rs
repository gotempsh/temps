// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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

/// Strip credentials and query parameters from a URL before it appears in an
/// error message.
///
/// The remote screenshot service URL is operator-supplied free text and is the
/// only place credentials for that service can be configured (`api_key` is not
/// wired through from settings), so it routinely contains userinfo
/// (`https://user:pass@host`) or a key in the query string. Availability errors
/// are persisted to `deployment_jobs.error_message` and rendered to every
/// project member, so only scheme, host, port and path may survive.
///
/// An unparsable URL yields a placeholder rather than the raw string — falling
/// back to the original would defeat the whole point.
fn redact_url(raw: &str) -> String {
    let Ok(mut parsed) = url::Url::parse(raw) else {
        return "<invalid screenshot service URL>".to_string();
    };

    // Errors here only occur for URLs that cannot have credentials (e.g. `data:`),
    // in which case there is nothing to strip.
    let _ = parsed.set_username("");
    let _ = parsed.set_password(None);
    parsed.set_query(None);
    parsed.set_fragment(None);

    parsed.to_string()
}

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

    async fn check_availability(&self) -> ScreenshotResult<()> {
        // Try a simple health check to the service URL
        let health_url = format!("{}/health", self.service_url.trim_end_matches('/'));
        // This error reaches the deployment job's error_message, which is visible to
        // every project member — never let the configured URL through verbatim, it
        // is the only place an operator can put credentials for the remote service.
        let safe_url = redact_url(&health_url);

        let response = self
            .client
            .get(&health_url)
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .map_err(|e| {
                // `without_url()` strips the request URL from reqwest's own Display
                // output, which would otherwise re-introduce the unredacted URL.
                ScreenshotError::ProviderError(format!(
                    "Remote screenshot service health check failed for {}: {}. \
                     Verify the service URL is correct and reachable from this server.",
                    safe_url,
                    e.without_url()
                ))
            })?;

        if !response.status().is_success() {
            return Err(ScreenshotError::ProviderError(format!(
                "Remote screenshot service health check for {} returned HTTP {}. \
                 Verify the service is healthy and the API key is valid.",
                safe_url,
                response.status()
            )));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Availability errors are stored in `deployment_jobs.error_message` and shown
    /// to every project member, so credentials configured in the service URL must
    /// never survive into them.
    #[test]
    fn test_redact_url_strips_credentials_and_query() {
        assert_eq!(
            redact_url("https://admin:hunter2@shots.internal:8443/health"),
            "https://shots.internal:8443/health"
        );
        assert_eq!(
            redact_url("https://shots.example.com/health?api_key=sk_live_secret"),
            "https://shots.example.com/health"
        );
        assert_eq!(
            redact_url("https://token@shots.example.com/health"),
            "https://shots.example.com/health"
        );
        // Host, port and path are preserved — the operator still needs to know
        // which endpoint failed.
        assert_eq!(
            redact_url("http://127.0.0.1:9000/api/v1/health"),
            "http://127.0.0.1:9000/api/v1/health"
        );
    }

    #[test]
    fn test_redact_url_does_not_echo_unparsable_input() {
        let redacted = redact_url("not a url with pass:word@ in it");
        assert!(
            !redacted.contains("pass:word"),
            "unparsable input must not be echoed back, got: {}",
            redacted
        );
    }

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
