// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! HTTP client for OpenAI's native Responses, Files and Batch endpoints.
//!
//! Unlike the Chat Completions adapters, nothing here translates: request
//! bodies are forwarded as the caller wrote them and provider replies are
//! returned byte for byte, so official OpenAI SDKs work unchanged against the
//! gateway. Every call goes through the hardened provider client (no
//! redirects, public addresses only).

use std::time::Duration;

use bytes::Bytes;
use futures_util::StreamExt;
use reqwest::{Method, StatusCode};

use crate::error::AiGatewayError;
use crate::providers::external_http_client;

/// Largest JSON reply read into memory from a native endpoint. Responses and
/// batch objects are far smaller; this only bounds a misbehaving upstream.
pub const MAX_NATIVE_JSON_BYTES: usize = 32 * 1024 * 1024;

// Shared with reconciliation so its deadline cannot cancel a supported transfer.
pub(crate) const NATIVE_REQUEST_TIMEOUT: Duration = Duration::from_secs(300);
pub(crate) const FILE_TRANSFER_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// Upstream identifiers (`file-…`, `batch_…`) are interpolated into request
/// paths, so anything outside this alphabet is rejected before it can change
/// the path (`../`, `?`, `#`, encoded separators).
const MAX_UPSTREAM_ID_LEN: usize = 128;

/// A provider reply forwarded to the caller as-is.
#[derive(Debug, Clone)]
pub struct UpstreamReply {
    pub status: StatusCode,
    pub content_type: Option<String>,
    pub body: Bytes,
}

impl UpstreamReply {
    pub fn is_success(&self) -> bool {
        self.status.is_success()
    }
}

/// Check that `id` is a plain provider identifier safe to place in a path.
pub fn validate_upstream_id(kind: &str, id: &str) -> Result<(), AiGatewayError> {
    let valid = !id.is_empty()
        && id.len() <= MAX_UPSTREAM_ID_LEN
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if valid {
        Ok(())
    } else {
        Err(AiGatewayError::Validation {
            message: format!(
                "{kind} id '{}' is invalid: expected 1-{MAX_UPSTREAM_ID_LEN} letters, digits, '-' or '_'",
                id.chars().take(MAX_UPSTREAM_ID_LEN).collect::<String>()
            ),
        })
    }
}

pub struct OpenAiNativeClient {
    /// Inference and object metadata calls.
    client: reqwest::Client,
    /// File uploads and downloads, which can move up to 200 MB and so need a
    /// longer overall deadline than a single model call.
    transfer_client: reqwest::Client,
}

impl Default for OpenAiNativeClient {
    fn default() -> Self {
        Self::new()
    }
}

impl OpenAiNativeClient {
    pub fn new() -> Self {
        Self {
            client: external_http_client(NATIVE_REQUEST_TIMEOUT),
            transfer_client: external_http_client(FILE_TRANSFER_TIMEOUT),
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test() -> Self {
        Self {
            client: reqwest::Client::new(),
            transfer_client: reqwest::Client::new(),
        }
    }

    fn url(base_url: &str, path: &str) -> String {
        format!("{}/{}", base_url.trim_end_matches('/'), path)
    }

    /// Send a request and return the raw response, for callers that stream
    /// the body (Responses SSE, file downloads).
    pub async fn send(
        &self,
        method: Method,
        base_url: &str,
        api_key: &str,
        path: &str,
        json_body: Option<Bytes>,
        transfer: bool,
    ) -> Result<reqwest::Response, AiGatewayError> {
        let client = if transfer {
            &self.transfer_client
        } else {
            &self.client
        };
        let mut request = client
            .request(method, Self::url(base_url, path))
            .bearer_auth(api_key);
        if let Some(body) = json_body {
            request = request
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(body);
        }
        Ok(request.send().await?)
    }

    /// Send a request and read the (bounded) reply into memory.
    pub async fn send_buffered(
        &self,
        method: Method,
        base_url: &str,
        api_key: &str,
        path: &str,
        json_body: Option<Bytes>,
    ) -> Result<UpstreamReply, AiGatewayError> {
        let response = self
            .send(method, base_url, api_key, path, json_body, false)
            .await?;
        read_reply(response, path).await
    }

    /// Upload a batch input file as `multipart/form-data`, streaming it from
    /// disk so memory use does not grow with the file size.
    #[allow(clippy::too_many_arguments)]
    pub async fn upload_file(
        &self,
        base_url: &str,
        api_key: &str,
        purpose: &str,
        filename: &str,
        file: tokio::fs::File,
        length: u64,
        text_fields: &[(String, String)],
    ) -> Result<UpstreamReply, AiGatewayError> {
        let part = reqwest::multipart::Part::stream_with_length(reqwest::Body::from(file), length)
            .file_name(filename.to_string())
            .mime_str("application/jsonl")?;
        let mut form = reqwest::multipart::Form::new().text("purpose", purpose.to_string());
        for (name, value) in text_fields {
            form = form.text(name.clone(), value.clone());
        }
        let form = form.part("file", part);
        let response = self
            .transfer_client
            .post(Self::url(base_url, "files"))
            .bearer_auth(api_key)
            .multipart(form)
            .send()
            .await?;
        read_reply(response, "files").await
    }
}

/// Read a provider reply into memory, refusing more than
/// [`MAX_NATIVE_JSON_BYTES`].
pub async fn read_reply(
    response: reqwest::Response,
    path: &str,
) -> Result<UpstreamReply, AiGatewayError> {
    let status = response.status();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    if response
        .content_length()
        .is_some_and(|length| length > MAX_NATIVE_JSON_BYTES as u64)
    {
        return Err(oversized(path));
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if body.len().saturating_add(chunk.len()) > MAX_NATIVE_JSON_BYTES {
            return Err(oversized(path));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(UpstreamReply {
        status,
        content_type,
        body: Bytes::from(body),
    })
}

fn oversized(path: &str) -> AiGatewayError {
    AiGatewayError::TranslationError {
        provider: "openai".to_string(),
        reason: format!(
            "Reply from /{path} exceeded the {} MiB limit",
            MAX_NATIVE_JSON_BYTES / (1024 * 1024)
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn native_transport_forwards_json_authentication_and_provider_errors() {
        use wiremock::{
            matchers::{body_json, header, method, path},
            Mock, MockServer, ResponseTemplate,
        };
        let server = MockServer::start().await;
        let body = serde_json::json!({"model":"gpt-4o","input":"hi","reasoning":{"effort":"low"}});
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .and(header("authorization", "Bearer sk-test"))
            .and(body_json(&body))
            .respond_with(
                ResponseTemplate::new(429)
                    .set_body_raw(r#"{"error":{"message":"slow down"}}"#, "application/json"),
            )
            .expect(1)
            .mount(&server)
            .await;
        // Loopback is allowed only by this test client; production always
        // uses the hardened external_http_client.
        let client = OpenAiNativeClient {
            client: reqwest::Client::new(),
            transfer_client: reqwest::Client::new(),
        };
        let reply = client
            .send_buffered(
                Method::POST,
                &format!("{}/v1", server.uri()),
                "sk-test",
                "responses",
                Some(Bytes::from(serde_json::to_vec(&body).unwrap())),
            )
            .await
            .unwrap();
        assert_eq!(reply.status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(&reply.body[..], br#"{"error":{"message":"slow down"}}"#);
    }

    #[test]
    fn upstream_ids_accept_provider_shapes() {
        assert!(validate_upstream_id("file", "file-abc123XYZ").is_ok());
        assert!(validate_upstream_id("batch", "batch_6801a2b3c4").is_ok());
    }

    #[test]
    fn upstream_ids_reject_path_changing_characters() {
        for id in [
            "",
            "../responses",
            "file-1/content",
            "file-1?x=1",
            "file-1#frag",
            "file%2F1",
            "file 1",
        ] {
            let error = validate_upstream_id("file", id).expect_err(id);
            assert!(
                matches!(error, AiGatewayError::Validation { ref message } if message.contains("file id")),
                "unexpected error for {id:?}: {error:?}"
            );
        }
        let long = "a".repeat(MAX_UPSTREAM_ID_LEN + 1);
        assert!(validate_upstream_id("batch", &long).is_err());
    }

    #[test]
    fn url_joins_without_double_slash() {
        assert_eq!(
            OpenAiNativeClient::url("https://api.openai.com/v1/", "responses"),
            "https://api.openai.com/v1/responses"
        );
        assert_eq!(
            OpenAiNativeClient::url("https://api.openai.com/v1", "files/file-1/content"),
            "https://api.openai.com/v1/files/file-1/content"
        );
    }
}
