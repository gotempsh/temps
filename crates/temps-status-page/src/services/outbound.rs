// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Outbound requests to user-supplied public URLs: monitor probes and
//! status page webhooks (ADR 108 D2). Every address is validated against the
//! shared outbound policy at request time and the client is pinned to it.

use std::net::IpAddr;
use std::time::Duration;

use temps_core::url_validation::{
    resolve_and_validate_domain, validate_outbound_ip, UrlValidationError,
};

use super::StatusPageError;

/// Where a non-local (public URL) probe may connect, decided at probe time.
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

/// Re-resolve and validate the probe host at probe time, and build a client
/// pinned to the validated addresses.
///
/// The probed hostname is validated nowhere else: a domain whose DNS later
/// points at `127.0.0.1`, an RFC 1918 address or `169.254.169.254` would
/// otherwise turn every periodic probe into a request to that internal
/// address. Resolving once, validating every address with the shared outbound
/// policy (which honours the operator's trusted private networks), and
/// pinning the client to those addresses means a rebinding answer cannot slip
/// in between the check and the connect. Inherited proxy settings are
/// disabled for the same reason (a proxy would do its own resolution), and
/// redirects stay unfollowed.
pub async fn pinned_public_client(
    url: &str,
    timeout: Duration,
    user_agent: &str,
) -> Result<PublicProbeTarget, StatusPageError> {
    let parsed = reqwest::Url::parse(url).map_err(|error| {
        StatusPageError::InvalidRequest(format!("URL '{url}' is invalid: {error}"))
    })?;
    let port = parsed.port_or_known_default().ok_or_else(|| {
        StatusPageError::InvalidRequest(format!("URL '{url}' has no usable port"))
    })?;
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
        None => {
            return Err(StatusPageError::InvalidRequest(format!(
                "URL '{url}' has no host"
            )));
        }
    }
    builder
        .build()
        .map(PublicProbeTarget::Pinned)
        .map_err(|source| StatusPageError::HttpClientBuild { source })
}
