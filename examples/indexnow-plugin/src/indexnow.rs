// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! IndexNow API client.
//!
//! Handles submission of URLs to the IndexNow API (single or batch).
//! See: https://www.indexnow.org/documentation

use reqwest::Client;

/// Submit a batch of URLs to the IndexNow API.
///
/// Uses the batch endpoint (POST /indexnow) which supports up to 10,000 URLs.
/// Returns the HTTP status code from the API.
pub async fn submit_urls(
    client: &Client,
    search_engine: &str,
    api_key: &str,
    host: &str,
    urls: &[String],
) -> Result<SubmitResponse, IndexNowError> {
    if urls.is_empty() {
        return Ok(SubmitResponse {
            status_code: 200,
            message: "No URLs to submit".to_string(),
        });
    }

    if urls.len() > 10_000 {
        return Err(IndexNowError::TooManyUrls {
            count: urls.len(),
            max: 10_000,
        });
    }

    let endpoint = format!("https://{}/indexnow", search_engine);

    let body = serde_json::json!({
        "host": host,
        "key": api_key,
        "urlList": urls,
    });

    tracing::info!(
        search_engine = %search_engine,
        host = %host,
        url_count = urls.len(),
        "Submitting URLs to IndexNow"
    );

    let response = client
        .post(&endpoint)
        .header("Content-Type", "application/json; charset=utf-8")
        .json(&body)
        .send()
        .await
        .map_err(|e| IndexNowError::Request {
            endpoint: endpoint.clone(),
            reason: e.to_string(),
        })?;

    let status = response.status().as_u16();
    let message = match status {
        200 => "URLs submitted successfully".to_string(),
        202 => "URLs received, key validation pending".to_string(),
        400 => "Bad request — check URL format".to_string(),
        403 => "Forbidden — API key not valid or key file not found".to_string(),
        422 => "Unprocessable — URLs don't belong to host or key mismatch".to_string(),
        429 => "Too many requests — rate limited".to_string(),
        _ => format!("Unexpected response: {}", status),
    };

    if status >= 400 {
        tracing::warn!(
            status = status,
            message = %message,
            endpoint = %endpoint,
            "IndexNow submission returned error"
        );
    } else {
        tracing::info!(
            status = status,
            url_count = urls.len(),
            "IndexNow submission successful"
        );
    }

    Ok(SubmitResponse {
        status_code: status,
        message,
    })
}

/// Submit a single URL to IndexNow (GET method).
#[allow(dead_code)]
pub async fn submit_single_url(
    client: &Client,
    search_engine: &str,
    api_key: &str,
    url: &str,
) -> Result<SubmitResponse, IndexNowError> {
    let endpoint = format!(
        "https://{}/indexnow?url={}&key={}",
        search_engine,
        urlencoding::encode(url),
        api_key
    );

    let response = client
        .get(&endpoint)
        .send()
        .await
        .map_err(|e| IndexNowError::Request {
            endpoint: endpoint.clone(),
            reason: e.to_string(),
        })?;

    let status = response.status().as_u16();
    let message = match status {
        200 => "URL submitted successfully".to_string(),
        202 => "URL received, key validation pending".to_string(),
        400 => "Bad request — check URL format".to_string(),
        403 => "Forbidden — API key not valid".to_string(),
        422 => "Unprocessable — URL doesn't belong to host or key mismatch".to_string(),
        429 => "Too many requests — rate limited".to_string(),
        _ => format!("Unexpected response: {}", status),
    };

    Ok(SubmitResponse {
        status_code: status,
        message,
    })
}

// ============================================================================
// Types
// ============================================================================

#[derive(Debug, Clone)]
pub struct SubmitResponse {
    pub status_code: u16,
    pub message: String,
}

impl SubmitResponse {
    pub fn is_success(&self) -> bool {
        self.status_code == 200 || self.status_code == 202
    }
}

// ============================================================================
// Error
// ============================================================================

#[derive(Debug, thiserror::Error)]
pub enum IndexNowError {
    #[error("Too many URLs ({count}), max is {max}")]
    TooManyUrls { count: usize, max: usize },

    #[error("HTTP request to {endpoint} failed: {reason}")]
    Request { endpoint: String, reason: String },
}

// We need urlencoding for the single-URL GET endpoint
mod urlencoding {
    /// Percent-encode a URL string for use as a query parameter value.
    #[allow(dead_code)]
    pub fn encode(input: &str) -> String {
        let mut result = String::with_capacity(input.len() * 3);
        for byte in input.bytes() {
            match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    result.push(byte as char)
                }
                _ => {
                    result.push('%');
                    result.push_str(&format!("{:02X}", byte));
                }
            }
        }
        result
    }
}
