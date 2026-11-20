// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::{future::Future, time::Duration};
use temps_agent_core::error::AgentError;
use temps_core::retry::RetryConfig;

#[derive(Debug, thiserror::Error)]
enum AttemptError {
    #[error("Worker service-access capacity is full")]
    Capacity(reqwest::Response),
    #[error(transparent)]
    Failed(AgentError),
}

// Only HTTP 429 is known to precede worker processing. Never replay an
// ambiguous network failure or an accepted command that failed on the worker.
// The caller's total deadline includes these bounded backoff delays.
pub(super) async fn send<F, Fut>(send: F) -> Result<reqwest::Response, AgentError>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<reqwest::Response, AgentError>>,
{
    let result = RetryConfig::new(8)
        .with_base_delay(Duration::from_millis(100))
        .with_max_delay(Duration::from_secs(1))
        .retry_if(
            || async {
                let response = send().await.map_err(AttemptError::Failed)?;
                if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
                    Err(AttemptError::Capacity(response))
                } else {
                    Ok(response)
                }
            },
            |error| matches!(error, AttemptError::Capacity(_)),
        )
        .await;
    match result {
        Ok(response) | Err(AttemptError::Capacity(response)) => Ok(response),
        Err(AttemptError::Failed(error)) => Err(error),
    }
}
