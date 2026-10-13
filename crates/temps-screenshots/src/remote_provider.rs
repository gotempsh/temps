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

/// Largest response accepted from a screenshot service. The image arrives
/// base64-encoded inside JSON, so this allows an image of about 48 MiB.
const MAX_RESPONSE_BYTES: usize = 64 * 1024 * 1024;

/// How much of an error response is kept for the error message.
const MAX_ERROR_BODY_BYTES: usize = 4 * 1024;

/// Remote screenshot provider that calls an external API
pub struct RemoteScreenshotProvider {
    /// Base URL of the screenshot service
    service_url: String,
    /// API key for authentication (if required)
    api_key: Option<String>,
    /// HTTP client
    client: Client,
    /// Responses larger than this are refused without being buffered whole.
    max_response_bytes: usize,
}

/// A response body read with a size cap.
enum CappedBody {
    Complete(Vec<u8>),
    TooLarge,
}

/// Read `response`'s body, giving up as soon as it exceeds `limit` bytes so a
/// misbehaving service cannot make Temps buffer an unbounded response.
async fn read_body_capped(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<CappedBody, reqwest::Error> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Ok(CappedBody::TooLarge);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if body.len() + chunk.len() > limit {
            return Ok(CappedBody::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(CappedBody::Complete(body))
}

/// The first `limit` bytes of `response`'s body as text, for error messages.
async fn read_body_prefix(mut response: reqwest::Response, limit: usize) -> String {
    let mut body = Vec::new();
    while body.len() < limit {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                let take = chunk.len().min(limit - body.len());
                body.extend_from_slice(&chunk[..take]);
            }
            Ok(None) | Err(_) => break,
        }
    }
    String::from_utf8_lossy(&body).into_owned()
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
            max_response_bytes: MAX_RESPONSE_BYTES,
        })
    }

    #[cfg(test)]
    fn with_max_response_bytes(mut self, max_response_bytes: usize) -> Self {
        self.max_response_bytes = max_response_bytes;
        self
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
            let error_text = read_body_prefix(response, MAX_ERROR_BODY_BYTES).await;
            error!(
                "Screenshot service returned error {}: {}",
                status, error_text
            );
            return Err(ScreenshotError::HttpRequest(format!(
                "Service returned error {}: {}",
                status, error_text
            )));
        }

        let body = match read_body_capped(response, self.max_response_bytes)
            .await
            .map_err(|e| {
                error!("Failed to read screenshot service response: {}", e);
                ScreenshotError::HttpRequest(format!("Failed to read response: {}", e))
            })? {
            CappedBody::Complete(body) => body,
            CappedBody::TooLarge => {
                return Err(ScreenshotError::ProviderError(format!(
                    "the screenshot service at {} sent a response larger than {} bytes",
                    redact_url(&self.service_url),
                    self.max_response_bytes
                )))
            }
        };
        let screenshot_response: ScreenshotResponse =
            serde_json::from_slice(&body).map_err(|e| {
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
    async fn oversized_responses_are_refused() {
        let mut server = mockito::Server::new_async().await;
        let image = "A".repeat(4096);
        server
            .mock("POST", "/")
            .with_status(200)
            .with_body(format!(r#"{{"success":true,"image":"{image}"}}"#))
            .create_async()
            .await;
        let provider = RemoteScreenshotProvider::new(server.url(), None)
            .unwrap()
            .with_max_response_bytes(1024);

        let err = provider
            .capture_screenshot("http://app.local")
            .await
            .unwrap_err();

        assert!(err.to_string().contains("larger than 1024 bytes"), "{err}");
    }

    #[tokio::test]
    async fn responses_within_the_limit_return_the_image() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/")
            .with_status(200)
            .with_body(r#"{"success":true,"image":"iVBORw0KGgo="}"#)
            .create_async()
            .await;
        let provider = RemoteScreenshotProvider::new(server.url(), None)
            .unwrap()
            .with_max_response_bytes(1024);

        let bytes = provider
            .capture_screenshot("http://app.local")
            .await
            .unwrap();

        assert_eq!(bytes, b"\x89PNG\r\n\x1a\n");
    }

    #[tokio::test]
    async fn error_bodies_are_truncated() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/")
            .with_status(502)
            .with_body("x".repeat(MAX_ERROR_BODY_BYTES * 4))
            .create_async()
            .await;
        let provider = RemoteScreenshotProvider::new(server.url(), None).unwrap();

        let err = provider
            .capture_screenshot("http://app.local")
            .await
            .unwrap_err()
            .to_string();

        assert!(err.contains("502"), "{err}");
        assert!(
            err.len() < MAX_ERROR_BODY_BYTES + 200,
            "error message kept {} bytes",
            err.len()
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
