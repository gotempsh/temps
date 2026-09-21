// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use async_trait::async_trait;
use std::{collections::BTreeMap, time::Duration};
use temps_core::url_validation::{resolve_and_validate_domain, validate_external_url};
use temps_credential_checks::{
    HttpCheckMethod, HttpCheckRequest, HttpCheckResponse, HttpCheckTransport, TransportFailure,
    VerificationError,
};

pub struct SafeHttpTransport;
#[async_trait]
impl HttpCheckTransport for SafeHttpTransport {
    async fn execute(
        &self,
        request: HttpCheckRequest,
    ) -> Result<HttpCheckResponse, VerificationError> {
        tokio::time::timeout(Duration::from_secs(15), execute(request))
            .await
            .map_err(|_| VerificationError::Transport {
                kind: TransportFailure::Timeout,
            })?
    }
}
async fn execute(request: HttpCheckRequest) -> Result<HttpCheckResponse, VerificationError> {
    let failure = |kind| VerificationError::Transport { kind };
    let url =
        validate_external_url(&request.url).map_err(|_| failure(TransportFailure::Destination))?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(failure(TransportFailure::Destination));
    }
    let host = url
        .host_str()
        .ok_or_else(|| failure(TransportFailure::Destination))?;
    let addresses = resolve_and_validate_domain(host, url.port_or_known_default().unwrap_or(443))
        .await
        .map_err(|_| failure(TransportFailure::Dns))?;
    // Pin the validated addresses to eliminate DNS rebinding. Never inherit a proxy
    // or follow redirects, both of which could bypass this destination policy.
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .resolve_to_addrs(host, &addresses)
        .timeout(Duration::from_secs(10))
        .user_agent("Temps-HTTP-Checks/1.0")
        .build()
        .map_err(|_| failure(TransportFailure::Connection))?;
    let method = match request.method {
        HttpCheckMethod::Get => reqwest::Method::GET,
        HttpCheckMethod::Head => reqwest::Method::HEAD,
    };
    let mut builder = client.request(method, url);
    for (name, value) in request.headers {
        if matches!(
            name.to_ascii_lowercase().as_str(),
            "host"
                | "connection"
                | "content-length"
                | "transfer-encoding"
                | "proxy-authorization"
                | "proxy-connection"
        ) {
            return Err(failure(TransportFailure::Header));
        }
        let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| failure(TransportFailure::Header))?;
        let mut value = reqwest::header::HeaderValue::from_str(&value)
            .map_err(|_| failure(TransportFailure::Header))?;
        value.set_sensitive(true);
        builder = builder.header(name, value);
    }
    let mut response = builder.send().await.map_err(|e| {
        failure(if e.is_timeout() {
            TransportFailure::Timeout
        } else {
            TransportFailure::Connection
        })
    })?;
    const MAX_BODY: usize = 65_536;
    if response
        .content_length()
        .is_some_and(|size| size > MAX_BODY as u64)
    {
        return Err(failure(TransportFailure::ResponseTooLarge));
    }
    let status = response.status().as_u16();
    let headers: BTreeMap<_, _> = response
        .headers()
        .iter()
        .filter_map(|(key, value)| {
            value
                .to_str()
                .ok()
                .map(|v| (key.to_string(), v.to_string()))
        })
        .collect();
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| failure(TransportFailure::Connection))?
    {
        if body.len() + chunk.len() > MAX_BODY {
            return Err(failure(TransportFailure::ResponseTooLarge));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(HttpCheckResponse {
        status,
        headers,
        body,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn rejects_local_and_plaintext_endpoints_before_sending_credentials() {
        for url in [
            "https://127.0.0.1/token",
            "https://169.254.169.254/",
            "http://example.com/",
            "https://user:password@example.com/",
        ] {
            assert!(SafeHttpTransport
                .execute(HttpCheckRequest {
                    url: url.into(),
                    method: HttpCheckMethod::Get,
                    headers: BTreeMap::new()
                })
                .await
                .is_err());
        }
    }
}
