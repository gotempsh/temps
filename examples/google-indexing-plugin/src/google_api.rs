// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Google Indexing API v3 client.
//!
//! Supports:
//! - Single URL notifications (publish)
//! - URL notification metadata queries
//! - Batch requests (up to 100 URLs per batch)

use serde::{Deserialize, Serialize};

use crate::auth::GoogleAuth;

const INDEXING_API_BASE: &str = "https://indexing.googleapis.com";
const PUBLISH_ENDPOINT: &str = "/v3/urlNotifications:publish";
const METADATA_ENDPOINT: &str = "/v3/urlNotifications/metadata";
const MAX_BATCH_SIZE: usize = 100;

// ============================================================================
// API types
// ============================================================================

#[derive(Debug, Clone, Serialize)]
pub struct UrlNotification {
    pub url: String,
    #[serde(rename = "type")]
    pub notification_type: NotificationType,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[allow(non_camel_case_types)]
pub enum NotificationType {
    URL_UPDATED,
    URL_DELETED,
}

impl std::fmt::Display for NotificationType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NotificationType::URL_UPDATED => write!(f, "URL_UPDATED"),
            NotificationType::URL_DELETED => write!(f, "URL_DELETED"),
        }
    }
}

impl NotificationType {
    pub fn from_str_loose(s: &str) -> Self {
        match s.to_uppercase().as_str() {
            "URL_DELETED" => NotificationType::URL_DELETED,
            _ => NotificationType::URL_UPDATED,
        }
    }
}

/// Response from the publish endpoint.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishResponse {
    pub url_notification_metadata: Option<UrlNotificationMetadata>,
}

/// URL notification metadata (from publish or getMetadata).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UrlNotificationMetadata {
    pub url: Option<String>,
    pub latest_update: Option<UrlNotificationInfo>,
    pub latest_remove: Option<UrlNotificationInfo>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UrlNotificationInfo {
    pub url: Option<String>,
    #[serde(rename = "type")]
    pub notification_type: Option<String>,
    pub notify_time: Option<String>,
}

/// Google API error response.
#[derive(Debug, Clone, Deserialize)]
pub struct GoogleApiErrorResponse {
    pub error: GoogleApiErrorBody,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct GoogleApiErrorBody {
    pub code: u16,
    pub message: String,
    #[serde(default)]
    pub errors: Vec<GoogleApiErrorDetail>,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct GoogleApiErrorDetail {
    #[serde(default)]
    pub domain: String,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub message: String,
}

// ============================================================================
// Client
// ============================================================================

/// Result of submitting a single URL.
#[derive(Debug, Clone)]
pub struct SingleSubmitResult {
    pub url: String,
    pub success: bool,
    pub status_code: u16,
    pub error: Option<String>,
    pub notify_time: Option<String>,
}

/// Google Indexing API client.
pub struct GoogleIndexingClient {
    auth: GoogleAuth,
    http_client: reqwest::Client,
}

impl GoogleIndexingClient {
    pub fn new(auth: GoogleAuth, http_client: reqwest::Client) -> Self {
        Self { auth, http_client }
    }

    /// Publish a single URL notification.
    pub async fn publish_url(
        &self,
        url: &str,
        notification_type: NotificationType,
    ) -> Result<SingleSubmitResult, GoogleApiError> {
        let access_token = self
            .auth
            .get_access_token()
            .await
            .map_err(|e| GoogleApiError::Auth(e.to_string()))?;

        let notification = UrlNotification {
            url: url.to_string(),
            notification_type,
        };

        let response = self
            .http_client
            .post(format!("{}{}", INDEXING_API_BASE, PUBLISH_ENDPOINT))
            .bearer_auth(&access_token)
            .json(&notification)
            .send()
            .await
            .map_err(|e| GoogleApiError::Network {
                reason: format!("HTTP request failed: {}", e),
            })?;

        let status = response.status().as_u16();

        if status == 401 {
            // Token might be expired, invalidate and retry once
            self.auth.invalidate_token().await;
            return self.publish_url_inner(url, notification_type).await;
        }

        Self::parse_publish_response(url, status, response).await
    }

    /// Internal publish without retry (used after token refresh).
    async fn publish_url_inner(
        &self,
        url: &str,
        notification_type: NotificationType,
    ) -> Result<SingleSubmitResult, GoogleApiError> {
        let access_token = self
            .auth
            .get_access_token()
            .await
            .map_err(|e| GoogleApiError::Auth(e.to_string()))?;

        let notification = UrlNotification {
            url: url.to_string(),
            notification_type,
        };

        let response = self
            .http_client
            .post(format!("{}{}", INDEXING_API_BASE, PUBLISH_ENDPOINT))
            .bearer_auth(&access_token)
            .json(&notification)
            .send()
            .await
            .map_err(|e| GoogleApiError::Network {
                reason: format!("HTTP request failed: {}", e),
            })?;

        let status = response.status().as_u16();
        Self::parse_publish_response(url, status, response).await
    }

    /// Parse the response from a publish request.
    async fn parse_publish_response(
        url: &str,
        status: u16,
        response: reqwest::Response,
    ) -> Result<SingleSubmitResult, GoogleApiError> {
        let body = response.text().await.unwrap_or_default();

        if (200..300).contains(&status) {
            let notify_time = serde_json::from_str::<PublishResponse>(&body)
                .ok()
                .and_then(|r| r.url_notification_metadata)
                .and_then(|m| m.latest_update)
                .and_then(|u| u.notify_time);

            Ok(SingleSubmitResult {
                url: url.to_string(),
                success: true,
                status_code: status,
                error: None,
                notify_time,
            })
        } else {
            let error_msg = serde_json::from_str::<GoogleApiErrorResponse>(&body)
                .map(|e| e.error.message)
                .unwrap_or_else(|_| format!("HTTP {}: {}", status, body));

            Ok(SingleSubmitResult {
                url: url.to_string(),
                success: false,
                status_code: status,
                error: Some(error_msg),
                notify_time: None,
            })
        }
    }

    /// Submit multiple URLs. Sends individual requests (more reliable than
    /// batch multipart which has complex parsing). Respects the given quota limit.
    pub async fn publish_urls(
        &self,
        urls: &[String],
        notification_type: NotificationType,
        max_count: usize,
    ) -> Result<Vec<SingleSubmitResult>, GoogleApiError> {
        let count = urls.len().min(max_count).min(MAX_BATCH_SIZE);
        let mut results = Vec::with_capacity(count);

        for url in urls.iter().take(count) {
            let result = self.publish_url(url, notification_type).await?;

            // Stop on quota exceeded
            if result.status_code == 429 {
                tracing::warn!(url = %url, "Google Indexing API quota exceeded, stopping batch");
                results.push(result);
                break;
            }

            results.push(result);
        }

        Ok(results)
    }

    /// Get notification metadata for a URL.
    pub async fn get_url_metadata(
        &self,
        url: &str,
    ) -> Result<UrlNotificationMetadata, GoogleApiError> {
        let access_token = self
            .auth
            .get_access_token()
            .await
            .map_err(|e| GoogleApiError::Auth(e.to_string()))?;

        let response = self
            .http_client
            .get(format!("{}{}", INDEXING_API_BASE, METADATA_ENDPOINT))
            .bearer_auth(&access_token)
            .query(&[("url", url)])
            .send()
            .await
            .map_err(|e| GoogleApiError::Network {
                reason: format!("HTTP request failed: {}", e),
            })?;

        let status = response.status().as_u16();
        let body = response.text().await.unwrap_or_default();

        if (200..300).contains(&status) {
            serde_json::from_str::<UrlNotificationMetadata>(&body).map_err(|e| {
                GoogleApiError::ParseResponse {
                    reason: format!("Failed to parse metadata response: {}", e),
                }
            })
        } else {
            let error_msg = serde_json::from_str::<GoogleApiErrorResponse>(&body)
                .map(|e| e.error.message)
                .unwrap_or_else(|_| format!("HTTP {}: {}", status, body));

            Err(GoogleApiError::Api {
                status_code: status,
                message: error_msg,
            })
        }
    }

    /// Query the Google Service Usage API to get the real quota limit for the
    /// Indexing API on this project.
    ///
    /// Returns `Some(daily_limit)` if successful, `None` if the API call fails
    /// (e.g., missing permissions). The service account needs the
    /// `serviceusage.quotas.get` IAM permission (granted by the "Service Usage
    /// Consumer" role).
    pub async fn get_real_quota_limit(&self) -> Result<Option<i64>, GoogleApiError> {
        let access_token = self
            .auth
            .get_access_token()
            .await
            .map_err(|e| GoogleApiError::Auth(e.to_string()))?;

        let project_id = self.auth.project_id();
        let url = format!(
            "https://serviceusage.googleapis.com/v1beta1/projects/{}/services/indexing.googleapis.com/consumerQuotaMetrics",
            project_id
        );

        let response = self
            .http_client
            .get(&url)
            .bearer_auth(&access_token)
            .query(&[("view", "FULL")])
            .send()
            .await
            .map_err(|e| GoogleApiError::Network {
                reason: format!("Service Usage API request failed: {}", e),
            })?;

        let status = response.status().as_u16();
        let body = response.text().await.unwrap_or_default();

        if !(200..300).contains(&status) {
            tracing::debug!(
                status = status,
                body = %body,
                "Failed to query Service Usage API for quota (this is normal if the service account \
                 doesn't have serviceusage.quotas.get permission)"
            );
            return Ok(None);
        }

        // Parse the response to find the daily publish quota
        let parsed: serde_json::Value =
            serde_json::from_str(&body).map_err(|e| GoogleApiError::ParseResponse {
                reason: format!("Failed to parse Service Usage response: {}", e),
            })?;

        // Walk the metrics → limits → quotaBuckets to find the default daily limit
        if let Some(metrics) = parsed.get("metrics").and_then(|m| m.as_array()) {
            for metric in metrics {
                // Look for the publish requests metric
                let metric_name = metric.get("metric").and_then(|m| m.as_str()).unwrap_or("");
                if !metric_name.contains("default_requests") && !metric_name.contains("publish") {
                    continue;
                }

                if let Some(limits) = metric.get("consumerQuotaLimits").and_then(|l| l.as_array()) {
                    for limit in limits {
                        // Check if this is a per-day limit
                        let unit = limit.get("unit").and_then(|u| u.as_str()).unwrap_or("");
                        if !unit.contains("d") {
                            continue; // Skip per-minute limits
                        }

                        if let Some(buckets) = limit.get("quotaBuckets").and_then(|b| b.as_array())
                        {
                            for bucket in buckets {
                                // effectiveLimit is the actual limit after any overrides
                                if let Some(effective) =
                                    bucket.get("effectiveLimit").and_then(|e| e.as_i64())
                                {
                                    return Ok(Some(effective));
                                }
                                // Fall back to defaultLimit
                                if let Some(default) =
                                    bucket.get("defaultLimit").and_then(|d| d.as_i64())
                                {
                                    return Ok(Some(default));
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(None)
    }
}

// ============================================================================
// Error
// ============================================================================

#[derive(Debug, thiserror::Error)]
pub enum GoogleApiError {
    #[error("Authentication failed: {0}")]
    Auth(String),

    #[error("Network error: {reason}")]
    Network { reason: String },

    #[error("Google API error ({status_code}): {message}")]
    Api { status_code: u16, message: String },

    #[error("Failed to parse API response: {reason}")]
    ParseResponse { reason: String },

    #[error("Quota exceeded: {message}")]
    #[allow(dead_code)]
    QuotaExceeded { message: String },
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_notification_type_display() {
        assert_eq!(NotificationType::URL_UPDATED.to_string(), "URL_UPDATED");
        assert_eq!(NotificationType::URL_DELETED.to_string(), "URL_DELETED");
    }

    #[test]
    fn test_notification_type_from_str() {
        assert_eq!(
            NotificationType::from_str_loose("URL_UPDATED"),
            NotificationType::URL_UPDATED
        );
        assert_eq!(
            NotificationType::from_str_loose("URL_DELETED"),
            NotificationType::URL_DELETED
        );
        assert_eq!(
            NotificationType::from_str_loose("url_updated"),
            NotificationType::URL_UPDATED
        );
        assert_eq!(
            NotificationType::from_str_loose("url_deleted"),
            NotificationType::URL_DELETED
        );
        // Unknown defaults to URL_UPDATED
        assert_eq!(
            NotificationType::from_str_loose("something_else"),
            NotificationType::URL_UPDATED
        );
    }

    #[test]
    fn test_parse_publish_success_response() {
        let body = r#"{
            "urlNotificationMetadata": {
                "url": "https://example.com/page1",
                "latestUpdate": {
                    "url": "https://example.com/page1",
                    "type": "URL_UPDATED",
                    "notifyTime": "2025-01-15T10:30:00.000Z"
                }
            }
        }"#;

        let parsed: PublishResponse = serde_json::from_str(body).unwrap();
        let meta = parsed.url_notification_metadata.unwrap();
        assert_eq!(meta.url.as_deref(), Some("https://example.com/page1"));
        let update = meta.latest_update.unwrap();
        assert_eq!(
            update.notify_time.as_deref(),
            Some("2025-01-15T10:30:00.000Z")
        );
    }

    #[test]
    fn test_parse_error_response() {
        let body = r#"{
            "error": {
                "errors": [{
                    "domain": "global",
                    "reason": "forbidden",
                    "message": "Permission denied. Failed to verify the URL ownership."
                }],
                "code": 403,
                "message": "Permission denied. Failed to verify the URL ownership."
            }
        }"#;

        let parsed: GoogleApiErrorResponse = serde_json::from_str(body).unwrap();
        assert_eq!(parsed.error.code, 403);
        assert!(parsed.error.message.contains("Permission denied"));
    }

    #[test]
    fn test_parse_metadata_response() {
        let body = r#"{
            "url": "https://example.com/page1",
            "latestUpdate": {
                "url": "https://example.com/page1",
                "type": "URL_UPDATED",
                "notifyTime": "2025-01-15T10:30:00.000Z"
            },
            "latestRemove": {
                "url": "https://example.com/page1",
                "type": "URL_DELETED",
                "notifyTime": "2025-02-01T08:00:00.000Z"
            }
        }"#;

        let parsed: UrlNotificationMetadata = serde_json::from_str(body).unwrap();
        assert_eq!(parsed.url.as_deref(), Some("https://example.com/page1"));
        assert!(parsed.latest_update.is_some());
        assert!(parsed.latest_remove.is_some());
    }
}
