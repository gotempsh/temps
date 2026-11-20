// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Outbound requests to user-supplied public URLs: monitor probes and
//! status page webhooks (ADR 108 D2). The policy lives in
//! `temps-uptime-probe`, shared with worker node agents.

use std::time::Duration;

pub use temps_uptime_probe::outbound::PublicProbeTarget;
use temps_uptime_probe::OutboundError;

use super::StatusPageError;

/// [`temps_uptime_probe::outbound::pinned_public_client`], with this crate's
/// error.
pub async fn pinned_public_client(
    url: &str,
    timeout: Duration,
    user_agent: &str,
) -> Result<PublicProbeTarget, StatusPageError> {
    temps_uptime_probe::outbound::pinned_public_client(url, timeout, user_agent)
        .await
        .map_err(|error| match error {
            OutboundError::InvalidUrl { .. } => StatusPageError::InvalidRequest(error.to_string()),
            OutboundError::ClientBuild { source, .. } => {
                StatusPageError::HttpClientBuild { source }
            }
        })
}
