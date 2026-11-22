// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Outbound requests to user-supplied public URLs: monitor probes and
//! status page webhooks (ADR 108). Every address is validated against the
//! shared outbound policy at request time and the client is pinned to it.

use std::net::IpAddr;
use std::time::Duration;

use temps_core::url_validation::{
    resolve_and_validate_domain, validate_outbound_ip, UrlValidationError,
};

use crate::error::OutboundError;

/// Where a request to a public URL may connect, decided at request time.
pub enum PublicProbeTarget {
    /// Every resolved address passed the outbound policy; the client is pinned
    /// to exactly those addresses.
    Pinned(reqwest::Client),
    /// The host is (or resolves to) an address outside the outbound policy:
    /// loopback, RFC 1918, link-local/metadata, etc., and not trusted.
    NonPublic(String),
    /// The host did not resolve.
    Unresolved(String),
}

/// Re-resolve and validate the host at request time, and build a client
/// pinned to the validated addresses.
///
/// The hostname is validated nowhere else: a domain whose DNS later points
/// at `127.0.0.1`, an RFC 1918 address or `169.254.169.254` would otherwise
/// turn every periodic probe into a request to that internal address.
/// Resolving once, validating every address with the shared outbound policy
/// (which honours the operator's trusted private networks where this process
/// was told about them), and pinning the client to those addresses means a
/// rebinding answer cannot slip in between the check and the connect.
/// Inherited proxy settings are disabled for the same reason (a proxy would
/// do its own resolution), and redirects stay unfollowed.
pub async fn pinned_public_client(
    url: &str,
    timeout: Duration,
    user_agent: &str,
) -> Result<PublicProbeTarget, OutboundError> {
    let invalid = |reason: String| OutboundError::InvalidUrl {
        url: url.to_string(),
        reason,
    };
    let parsed = reqwest::Url::parse(url).map_err(|error| invalid(error.to_string()))?;
    let port = parsed
        .port_or_known_default()
        .ok_or_else(|| invalid("it has no usable port".to_string()))?;
    let mut builder = reqwest::Client::builder()
        .timeout(timeout)
        .user_agent(user_agent)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none());
    match parsed.host() {
        Some(url::Host::Ipv4(ip)) => {
            if let Err(e) = validate_outbound_ip(IpAddr::V4(ip)) {
                return Ok(PublicProbeTarget::NonPublic(e.to_string()));
            }
        }
        Some(url::Host::Ipv6(ip)) => {
            if let Err(e) = validate_outbound_ip(IpAddr::V6(ip)) {
                return Ok(PublicProbeTarget::NonPublic(e.to_string()));
            }
        }
        Some(url::Host::Domain(domain)) => match resolve_and_validate_domain(domain, port).await {
            Ok(addresses) => builder = builder.resolve_to_addrs(domain, &addresses),
            Err(UrlValidationError::DnsResolutionFailed(reason)) => {
                return Ok(PublicProbeTarget::Unresolved(reason));
            }
            Err(e) => return Ok(PublicProbeTarget::NonPublic(e.to_string())),
        },
        None => return Err(invalid("it has no host".to_string())),
    }
    builder
        .build()
        .map(PublicProbeTarget::Pinned)
        .map_err(|source| OutboundError::ClientBuild {
            url: url.to_string(),
            source,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn refuses_private_literals_and_hostless_urls() {
        let private = pinned_public_client("http://10.0.0.5/", Duration::from_secs(1), "t")
            .await
            .unwrap();
        assert!(matches!(private, PublicProbeTarget::NonPublic(_)));
        let loopback = pinned_public_client("http://[::1]:8080/", Duration::from_secs(1), "t")
            .await
            .unwrap();
        assert!(matches!(loopback, PublicProbeTarget::NonPublic(_)));
        let error = pinned_public_client("mailto:ops@example.com", Duration::from_secs(1), "t")
            .await
            .err()
            .unwrap();
        assert!(error.to_string().contains("mailto:ops@example.com"));
    }
}
