// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Where webhook requests go out, behind a trait so tests need no network.

use std::time::Duration;

use async_trait::async_trait;

use super::payload::USER_AGENT;
use crate::services::outbound::{pinned_public_client, PublicProbeTarget};

/// One delivery attempt's time limit, connect included.
pub const DELIVERY_TIMEOUT: Duration = Duration::from_secs(10);

/// Why a webhook request got no response. The message is what the console
/// shows as the delivery's failure reason.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransportError {
    #[error("the address is not public: {reason}")]
    NonPublic { reason: String },
    #[error("the host did not resolve: {reason}")]
    Unresolved { reason: String },
    #[error("the URL cannot receive webhooks: {reason}")]
    InvalidUrl { reason: String },
    #[error("no answer within {seconds} seconds")]
    Timeout { seconds: u64 },
    #[error("could not connect")]
    Connect,
    #[error("the request failed")]
    Request,
}

#[async_trait]
pub trait WebhookTransport: Send + Sync {
    /// Whether `url` resolves to addresses webhooks may be sent to.
    async fn check(&self, url: &str) -> Result<(), TransportError>;
    /// POST `body` to `url` with `headers`; the response status, or why
    /// nothing was received.
    async fn post(
        &self,
        url: &str,
        headers: Vec<(&'static str, String)>,
        body: String,
    ) -> Result<u16, TransportError>;
}

/// Resolves and checks every address at send time against the outbound
/// policy and pins the client to them; follows no redirect.
pub struct PinnedWebhookTransport;

async fn client(url: &str) -> Result<reqwest::Client, TransportError> {
    match pinned_public_client(url, DELIVERY_TIMEOUT, USER_AGENT).await {
        Ok(PublicProbeTarget::Pinned(client)) => Ok(client),
        Ok(PublicProbeTarget::NonPublic(reason)) => Err(TransportError::NonPublic { reason }),
        Ok(PublicProbeTarget::Unresolved(reason)) => Err(TransportError::Unresolved { reason }),
        Err(error) => Err(TransportError::InvalidUrl {
            reason: error.to_string(),
        }),
    }
}

/// `operation` within `limit` ([`DELIVERY_TIMEOUT`]) from start to finish.
/// The HTTP client's own timeout starts after the address lookup, which has
/// none, so a slow resolver would otherwise hold the whole batch.
async fn within<T>(
    limit: Duration,
    operation: impl std::future::Future<Output = Result<T, TransportError>>,
) -> Result<T, TransportError> {
    tokio::time::timeout(limit, operation)
        .await
        .unwrap_or(Err(TransportError::Timeout {
            seconds: limit.as_secs(),
        }))
}

#[async_trait]
impl WebhookTransport for PinnedWebhookTransport {
    async fn check(&self, url: &str) -> Result<(), TransportError> {
        within(DELIVERY_TIMEOUT, async { client(url).await.map(|_| ()) }).await
    }

    async fn post(
        &self,
        url: &str,
        headers: Vec<(&'static str, String)>,
        body: String,
    ) -> Result<u16, TransportError> {
        within(DELIVERY_TIMEOUT, async move {
            let mut request = client(url)
                .await?
                .post(url)
                .header(reqwest::header::CONTENT_TYPE, "application/json");
            for (name, value) in headers {
                request = request.header(name, value);
            }
            request
                .body(body)
                .send()
                .await
                .map(|response| response.status().as_u16())
                .map_err(|error| {
                    if error.is_timeout() {
                        TransportError::Timeout {
                            seconds: DELIVERY_TIMEOUT.as_secs(),
                        }
                    } else if error.is_connect() {
                        TransportError::Connect
                    } else {
                        TransportError::Request
                    }
                })
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_lookup_that_never_answers_times_out_within_the_delivery_limit() {
        let limit = Duration::from_millis(30);
        let started = std::time::Instant::now();
        let outcome = within(limit, std::future::pending::<Result<u16, TransportError>>()).await;
        assert_eq!(outcome, Err(TransportError::Timeout { seconds: 0 }));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "bounded by the limit"
        );
        assert_eq!(
            TransportError::NonPublic {
                reason: "10.0.0.5".into()
            }
            .to_string(),
            "the address is not public: 10.0.0.5"
        );
    }
}
