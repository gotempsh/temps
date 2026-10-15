// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Outbound policy for AI provider base URLs.
//!
//! Every base URL an AI provider key (or a BYOK caller) supplies goes through
//! two layers:
//!
//! * [`validate_provider_url`] -- the full check, with DNS resolution. Runs
//!   before Temps sends anything to a new or changed base URL (create, update,
//!   discovery, key verification), so a blocked destination is reported as a
//!   clear `InvalidProviderUrl` instead of an opaque connect error.
//! * [`check_provider_url`] -- the synchronous subset, run on every request.
//!   IP literals are fully decided here; hostnames are re-checked at connect
//!   time by the provider HTTP client's resolvers (`providers::ProviderHttpClient`).
//!
//! Rules:
//! * Private, loopback and other internal addresses are only reachable when
//!   trusted -- by the instance-wide trusted private networks, or by the one
//!   exact `ip:port` a provider key trusts for itself ([`KeyTrust`]).
//! * Cloud metadata and link-local addresses are never reachable.
//! * Plain `http://` is only allowed to trusted private addresses, so an API
//!   key or a prompt is never sent unencrypted over the internet.
//!
//! None of these checks connects to the destination, so their errors cannot
//! reveal whether a port is open.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use temps_core::url_validation::{
    is_trusted_private_destination, validate_external_url, validate_outbound_ip,
    validate_trustable_private_host, UrlValidationError, TRUSTED_NETWORKS_HINT,
};
use url::{Host, Url};

use crate::error::AiGatewayError;

/// Appended to "this private address is not trusted" errors on AI provider
/// URLs, next to the instance-wide [`TRUSTED_NETWORKS_HINT`].
pub const PER_KEY_TRUST_HINT: &str = " To reach a single self-hosted model server without \
     trusting it for every outbound feature, an administrator can instead enable \"Trust this \
     private address\" on the provider key; that requires the base URL to use the server's IP \
     address.";

const PLAINTEXT_PUBLIC_MESSAGE: &str = "plain http:// is only allowed to trusted private \
     addresses, so an API key and prompts are never sent unencrypted over the internet; use \
     https:// for this provider";

const DNS_TIMEOUT: Duration = Duration::from_secs(5);

/// One exact private `ip:port` that a single provider key may reach even
/// though it is not in the instance-wide trusted private networks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyTrust(SocketAddr);

impl KeyTrust {
    /// Derive the trust target from a base URL. Only an IP literal qualifies:
    /// a hostname can resolve differently later, so trusting it would not be
    /// bound to one host. The address must be private by allocation and never
    /// a cloud metadata endpoint.
    pub fn for_base_url(base_url: &str) -> Result<Self, AiGatewayError> {
        let parsed = parse(base_url)?;
        let ip = match parsed.host() {
            Some(Host::Ipv4(ip)) => IpAddr::V4(ip),
            Some(Host::Ipv6(ip)) => IpAddr::V6(ip),
            Some(Host::Domain(_)) => {
                return Err(invalid(
                    "a provider key can only trust a base URL that uses an IP address \
                     (for example http://10.0.0.5:8000/v1); a hostname can resolve to a \
                     different address later. Use the server's IP address, or add the address \
                     to Settings > Security > Trusted private networks instead",
                ))
            }
            None => return Err(invalid("the base URL has no host")),
        };
        let ip = validate_trustable_private_host(ip).map_err(|error| {
            invalid(&format!(
                "this address cannot be trusted on a provider key: {error}"
            ))
        })?;
        let port = parsed
            .port_or_known_default()
            .ok_or_else(|| invalid("the base URL has no port"))?;
        Ok(Self(SocketAddr::new(ip, port)))
    }

    /// Parse the value persisted on a provider key. A value that no longer
    /// passes the eligibility rules is ignored, so the key fails closed.
    pub fn from_stored(value: Option<&str>) -> Option<Self> {
        let address: SocketAddr = value?.parse().ok()?;
        let ip = validate_trustable_private_host(address.ip()).ok()?;
        Some(Self(SocketAddr::new(ip, address.port())))
    }

    /// Canonical form persisted on the provider key, e.g. `10.0.0.5:8000` or
    /// `[fd12::5]:8000`.
    pub fn to_stored(self) -> String {
        self.0.to_string()
    }

    fn matches(self, ip: IpAddr, port: u16) -> bool {
        let ip = match ip {
            IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(ip),
            IpAddr::V4(_) => ip,
        };
        self.0.ip() == ip && self.0.port() == port
    }
}

fn invalid(reason: &str) -> AiGatewayError {
    AiGatewayError::InvalidProviderUrl {
        reason: reason.to_string(),
    }
}

/// Map a core validation error, adding the per-key hint where trusting the
/// address is actually possible.
fn blocked(error: UrlValidationError) -> AiGatewayError {
    let liftable = matches!(
        error,
        UrlValidationError::PrivateIp
            | UrlValidationError::LoopbackIp
            | UrlValidationError::DomainResolvesToBlockedIp
    );
    let mut reason = error.to_string();
    if liftable {
        reason.push('.');
        reason.push_str(PER_KEY_TRUST_HINT);
    }
    AiGatewayError::InvalidProviderUrl { reason }
}

/// Map a blocked address. Any address that is private by allocation can be
/// trusted, so it always gets both hints -- including CGNAT/overlay addresses
/// (`100.64.0.0/10`, e.g. Tailscale), which the core reports as "reserved".
fn blocked_address(ip: IpAddr, error: UrlValidationError) -> AiGatewayError {
    if matches!(
        error,
        UrlValidationError::PrivateIp | UrlValidationError::LoopbackIp
    ) || validate_trustable_private_host(ip).is_err()
    {
        return blocked(error);
    }
    AiGatewayError::InvalidProviderUrl {
        reason: format!(
            "Private or overlay-network addresses are not allowed{TRUSTED_NETWORKS_HINT}.{PER_KEY_TRUST_HINT}"
        ),
    }
}

fn parse(url: &str) -> Result<Url, AiGatewayError> {
    let parsed = Url::parse(url.trim())
        .map_err(|error| invalid(&format!("the base URL is not a valid URL: {error}")))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(invalid("the base URL must start with http:// or https://"));
    }
    Ok(parsed)
}

/// Check one resolved or literal address against the policy.
fn check_address(
    ip: IpAddr,
    port: u16,
    plaintext: bool,
    key_trust: Option<KeyTrust>,
) -> Result<(), AiGatewayError> {
    if key_trust.is_some_and(|trust| trust.matches(ip, port)) {
        // `KeyTrust` is only ever built from a trustable private address.
        return Ok(());
    }
    validate_outbound_ip(ip).map_err(|error| blocked_address(ip, error))?;
    if plaintext && !is_trusted_private_destination(ip) {
        return Err(invalid(PLAINTEXT_PUBLIC_MESSAGE));
    }
    Ok(())
}

/// Synchronous request-time check of a provider base URL.
///
/// IP literals are fully validated here (they never reach a DNS resolver).
/// For hostnames only the syntactic checks run; the resolved addresses are
/// checked when connecting, by the provider HTTP client.
pub fn check_provider_url(url: &str, key_trust: Option<KeyTrust>) -> Result<Url, AiGatewayError> {
    let parsed = parse(url)?;
    let plaintext = parsed.scheme() == "http";
    let port = parsed
        .port_or_known_default()
        .ok_or_else(|| invalid("the base URL has no port"))?;
    match parsed.host() {
        Some(Host::Ipv4(ip)) => check_address(IpAddr::V4(ip), port, plaintext, key_trust)?,
        Some(Host::Ipv6(ip)) => check_address(IpAddr::V6(ip), port, plaintext, key_trust)?,
        Some(Host::Domain(_)) => {
            validate_external_url(parsed.as_str()).map_err(blocked)?;
        }
        None => return Err(invalid("the base URL has no host")),
    }
    Ok(parsed)
}

/// Full validation of a provider base URL, resolving hostnames.
///
/// Run before Temps sends anything to a new or changed base URL. Every
/// address a hostname resolves to must pass the same rules as an IP literal.
pub async fn validate_provider_url(
    url: &str,
    key_trust: Option<KeyTrust>,
) -> Result<Url, AiGatewayError> {
    let parsed = check_provider_url(url, key_trust)?;
    let Some(Host::Domain(domain)) = parsed.host() else {
        return Ok(parsed);
    };
    let domain = domain.to_string();
    let plaintext = parsed.scheme() == "http";
    let port = parsed
        .port_or_known_default()
        .ok_or_else(|| invalid("the base URL has no port"))?;

    let addresses: Vec<SocketAddr> = tokio::time::timeout(
        DNS_TIMEOUT,
        tokio::net::lookup_host((domain.as_str(), port)),
    )
    .await
    .map_err(|_| {
        invalid(&format!(
            "resolving provider hostname '{domain}' timed out after {}s",
            DNS_TIMEOUT.as_secs()
        ))
    })?
    .map_err(|error| {
        invalid(&format!(
            "provider hostname '{domain}' could not be resolved: {error}"
        ))
    })?
    .collect();
    if addresses.is_empty() {
        return Err(invalid(&format!(
            "provider hostname '{domain}' resolved to no addresses"
        )));
    }
    check_resolved_addresses(&domain, &addresses, plaintext)?;
    Ok(parsed)
}

/// Apply the address rules to every address a provider hostname resolved to.
/// Per-key trust is bound to an IP literal, so it never applies here.
fn check_resolved_addresses(
    domain: &str,
    addresses: &[SocketAddr],
    plaintext: bool,
) -> Result<(), AiGatewayError> {
    for address in addresses {
        check_address(address.ip(), address.port(), plaintext, None).map_err(
            |error| match error {
                AiGatewayError::InvalidProviderUrl { reason } => {
                    AiGatewayError::InvalidProviderUrl {
                        reason: format!(
                            "provider hostname '{domain}' resolves to a blocked address: {reason}"
                        ),
                    }
                }
                other => other,
            },
        )?;
    }
    Ok(())
}

/// Text appended to the core hint, exposed for the connect-time resolver.
pub(crate) fn untrusted_hint() -> String {
    format!("{TRUSTED_NETWORKS_HINT}.{PER_KEY_TRUST_HINT}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reason(result: Result<Url, AiGatewayError>) -> String {
        match result {
            Err(AiGatewayError::InvalidProviderUrl { reason }) => reason,
            other => panic!("expected InvalidProviderUrl, got {other:?}"),
        }
    }

    #[test]
    fn public_https_is_allowed() {
        assert!(check_provider_url("https://api.openai.com/v1", None).is_ok());
        assert!(check_provider_url("https://8.8.8.8/v1", None).is_ok());
    }

    #[test]
    fn plaintext_http_to_a_public_address_is_refused() {
        let message = reason(check_provider_url("http://8.8.8.8:8000/v1", None));
        assert!(message.contains("plain http://"), "{message}");
        assert!(message.contains("https://"), "{message}");
    }

    fn resolved(addresses: &[&str]) -> Vec<SocketAddr> {
        addresses
            .iter()
            .map(|address| SocketAddr::new(address.parse().unwrap(), 9))
            .collect()
    }

    fn resolved_reason(domain: &str, addresses: &[&str], plaintext: bool) -> String {
        match check_resolved_addresses(domain, &resolved(addresses), plaintext) {
            Err(AiGatewayError::InvalidProviderUrl { reason }) => reason,
            other => panic!("expected InvalidProviderUrl, got {other:?}"),
        }
    }

    #[test]
    fn hostname_resolving_to_untrusted_loopback_names_the_block_and_hints() {
        // #1410: the hostname case must explain the block like the IP case.
        let message = resolved_reason("qa-private.example.test", &["127.0.0.1"], true);
        assert!(message.contains("qa-private.example.test"), "{message}");
        assert!(message.contains("Loopback"), "{message}");
        assert!(message.contains("Trusted private networks"), "{message}");
        // Nothing about the port or the service may leak.
        assert!(!message.contains(":9"), "{message}");
        let message = resolved_reason("models.example.test", &["8.8.8.8", "10.1.2.3"], false);
        assert!(message.contains("Private IP"), "{message}");
    }

    #[test]
    fn untrusted_cgnat_overlay_address_gets_the_trust_hints() {
        // Tailscale-style 100.64.0.0/10 is trustable, so say how.
        let message = resolved_reason("models.tailnet.example", &["100.101.102.103"], true);
        assert!(message.contains("Trusted private networks"), "{message}");
        assert!(message.contains("Trust this private address"), "{message}");
        let message = reason(check_provider_url("http://100.101.102.103:52415/v1", None));
        assert!(message.contains("Trusted private networks"), "{message}");
        // Metadata inside that range never gets a hint.
        let message = reason(check_provider_url("http://100.100.100.200/", None));
        assert!(!message.contains("Trust this private address"), "{message}");
    }

    #[test]
    fn plaintext_http_to_a_public_hostname_is_refused_after_resolution() {
        let message = resolved_reason("models.example.test", &["8.8.8.8"], true);
        assert!(message.contains("plain http://"), "{message}");
        assert!(
            check_resolved_addresses("models.example.test", &resolved(&["8.8.8.8"]), false).is_ok()
        );
    }

    #[tokio::test]
    async fn async_validation_applies_the_ip_literal_rules() {
        let message = reason(validate_provider_url("http://1.1.1.1/v1", None).await);
        assert!(message.contains("plain http://"), "{message}");
        let message = reason(validate_provider_url("http://10.9.8.7:8000/v1", None).await);
        assert!(message.contains("Private IP"), "{message}");
        let trust = KeyTrust::for_base_url("http://10.9.8.7:8000/v1").unwrap();
        assert!(
            validate_provider_url("http://10.9.8.7:8000/v1", Some(trust))
                .await
                .is_ok()
        );
    }

    #[test]
    fn lenient_http_spellings_are_treated_as_plain_http() {
        // WHATWG parsing (what reqwest uses) turns these into http://8.8.8.8.
        for url in ["http:8.8.8.8/v1", "http:/8.8.8.8/v1", " HTTP://8.8.8.8/v1"] {
            let message = reason(check_provider_url(url, None));
            assert!(message.contains("plain http://"), "{url}: {message}");
        }
    }

    #[test]
    fn untrusted_private_ip_literal_includes_both_hints() {
        let message = reason(check_provider_url("http://127.0.0.1:9/v1", None));
        assert!(message.contains("Loopback"), "{message}");
        assert!(message.contains("Trusted private networks"), "{message}");
        assert!(message.contains("Trust this private address"), "{message}");
    }

    #[test]
    fn metadata_is_blocked_even_with_key_trust_attempt() {
        assert!(KeyTrust::for_base_url("http://169.254.169.254/latest").is_err());
        assert!(KeyTrust::for_base_url("http://100.100.100.200/").is_err());
        assert!(KeyTrust::for_base_url("http://[fd00:ec2::254]/").is_err());
        let message = reason(check_provider_url("http://169.254.169.254/latest", None));
        assert!(message.contains("metadata"), "{message}");
        assert!(!message.contains("Trust this private address"), "{message}");
    }

    #[test]
    fn key_trust_requires_an_ip_literal() {
        let message = match KeyTrust::for_base_url("http://models.internal:8000/v1") {
            Err(AiGatewayError::InvalidProviderUrl { reason }) => reason,
            other => panic!("expected InvalidProviderUrl, got {other:?}"),
        };
        assert!(message.contains("IP address"), "{message}");
        assert!(KeyTrust::for_base_url("http://8.8.8.8:8000/v1").is_err());
    }

    #[test]
    fn key_trust_is_bound_to_the_exact_address_and_port() {
        let trust = KeyTrust::for_base_url("http://127.0.0.1:52415/v1").unwrap();
        assert_eq!(trust.to_stored(), "127.0.0.1:52415");
        assert!(check_provider_url("http://127.0.0.1:52415/v1", Some(trust)).is_ok());
        assert!(check_provider_url("http://127.0.0.1:52416/v1", Some(trust)).is_err());
        assert!(check_provider_url("http://127.0.0.2:52415/v1", Some(trust)).is_err());
        // Default ports are part of the binding too.
        let default_port = KeyTrust::for_base_url("http://10.0.0.5/v1").unwrap();
        assert_eq!(default_port.to_stored(), "10.0.0.5:80");
        assert!(check_provider_url("https://10.0.0.5/v1", Some(default_port)).is_err());
    }

    #[test]
    fn stored_key_trust_fails_closed_when_ineligible() {
        assert!(KeyTrust::from_stored(Some("10.0.0.5:8000")).is_some());
        assert!(KeyTrust::from_stored(Some("[::1]:8000")).is_some());
        assert!(KeyTrust::from_stored(Some("169.254.169.254:80")).is_none());
        assert!(KeyTrust::from_stored(Some("8.8.8.8:80")).is_none());
        assert!(KeyTrust::from_stored(Some("not an address")).is_none());
        assert!(KeyTrust::from_stored(None).is_none());
    }

    #[test]
    fn rejects_non_http_schemes() {
        assert!(check_provider_url("file:///etc/passwd", None).is_err());
        assert!(check_provider_url("ftp://10.0.0.5/", None).is_err());
    }
}
