// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Where webhook requests go out, behind a trait so tests need no network.

use std::time::Duration;

use async_trait::async_trait;

use super::payload::USER_AGENT;
use crate::services::outbound::{pinned_public_client, PublicProbeTarget};

/// One delivery attempt's time limit, connect included.
pub const DELIVERY_TIMEOUT: Duration = Duration::from_secs(10);

#[async_trait]
pub trait WebhookTransport: Send + Sync {
    /// Whether `url` resolves to addresses webhooks may be sent to.
    async fn check(&self, url: &str) -> Result<(), String>;
    /// POST `body` to `url` with `headers`; the response status, or why
    /// nothing was received.
    async fn post(
        &self,
        url: &str,
        headers: Vec<(&'static str, String)>,
        body: String,
    ) -> Result<u16, String>;
}

/// Resolves and checks every address at send time against the outbound
/// policy and pins the client to them; follows no redirect.
pub struct PinnedWebhookTransport;

async fn client(url: &str) -> Result<reqwest::Client, String> {
    match pinned_public_client(url, DELIVERY_TIMEOUT, USER_AGENT).await {
        Ok(PublicProbeTarget::Pinned(client)) => Ok(client),
        Ok(PublicProbeTarget::NonPublic(reason)) => {
            Err(format!("the address is not public: {reason}"))
        }
        Ok(PublicProbeTarget::Unresolved(reason)) => {
            Err(format!("the host did not resolve: {reason}"))
        }
        Err(error) => Err(error.to_string()),
    }
}

#[async_trait]
impl WebhookTransport for PinnedWebhookTransport {
    async fn check(&self, url: &str) -> Result<(), String> {
        client(url).await.map(|_| ())
    }

    async fn post(
        &self,
        url: &str,
        headers: Vec<(&'static str, String)>,
        body: String,
    ) -> Result<u16, String> {
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
                    format!("no answer within {} seconds", DELIVERY_TIMEOUT.as_secs())
                } else if error.is_connect() {
                    "could not connect".to_string()
                } else {
                    "the request failed".to_string()
                }
            })
    }
}
