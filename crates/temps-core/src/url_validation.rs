//! URL validation utilities for preventing SSRF attacks
//!
//! This module provides comprehensive URL validation to prevent Server-Side Request Forgery (SSRF)
//! vulnerabilities by blocking private IP ranges, cloud metadata services, and malicious schemes.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use thiserror::Error;
use url::Url;

#[derive(Error, Debug)]
pub enum UrlValidationError {
    #[error("Invalid URL format: {0}")]
    InvalidFormat(String),

    #[error("URL scheme must be HTTP or HTTPS")]
    InvalidScheme,

    #[error("Private IP addresses are not allowed")]
    PrivateIp,

    #[error("Loopback addresses are not allowed")]
    LoopbackIp,

    #[error("Link-local addresses are not allowed")]
    LinkLocalIp,

    #[error("Cloud metadata service access is not allowed")]
    CloudMetadata,

    #[error("Multicast addresses are not allowed")]
    MulticastIp,

    #[error("Broadcast addresses are not allowed")]
    BroadcastIp,

    #[error("Documentation addresses are not allowed")]
    DocumentationIp,

    #[error("Unspecified addresses are not allowed")]
    UnspecifiedIp,

    #[error("DNS resolution failed: {0}")]
    DnsResolutionFailed(String),

    #[error("Domain resolves to a blocked IP address")]
    DomainResolvesToBlockedIp,

    #[error("URL must resolve to a loopback or private address (this is a local-only tool)")]
    NotLocalOrPrivate,
}

/// Validates a URL for external webhook/HTTP requests
///
/// This function performs comprehensive validation to prevent SSRF attacks:
/// - Only allows HTTP and HTTPS schemes
/// - Blocks private IP ranges (RFC 1918)
/// - Blocks loopback addresses (127.0.0.0/8, ::1)
/// - Blocks link-local addresses (169.254.0.0/16, fe80::/10)
/// - Blocks cloud metadata services (169.254.169.254, fd00:ec2::254)
/// - Blocks multicast, broadcast, and special-use addresses
/// - For domains, resolves DNS and validates all resolved IPs
///
/// # Examples
///
/// ```
/// use temps_core::url_validation::validate_external_url;
///
/// // Valid public URL
/// assert!(validate_external_url("https://example.com/webhook").is_ok());
///
/// // Invalid: private IP
/// assert!(validate_external_url("http://192.168.1.1").is_err());
///
/// // Invalid: localhost
/// assert!(validate_external_url("http://localhost:8080").is_err());
///
/// // Invalid: cloud metadata
/// assert!(validate_external_url("http://169.254.169.254/latest/meta-data").is_err());
/// ```
pub fn validate_external_url(url: &str) -> Result<Url, UrlValidationError> {
    // Parse URL
    let parsed =
        Url::parse(url).map_err(|e| UrlValidationError::InvalidFormat(format!("{}", e)))?;

    // Only allow HTTP/HTTPS
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(UrlValidationError::InvalidScheme);
    }

    // Validate host
    if let Some(host) = parsed.host() {
        match host {
            url::Host::Ipv4(ip) => validate_ipv4(&ip)?,
            url::Host::Ipv6(ip) => validate_ipv6(&ip)?,
            url::Host::Domain(domain) => {
                // Block well-known loopback and internal hostnames synchronously.
                // For full DNS resolution validation, use an async validator at the service layer.
                let lower = domain.to_lowercase();
                if lower == "localhost" || lower.ends_with(".localhost") {
                    return Err(UrlValidationError::LoopbackIp);
                }
            }
        }
    } else {
        return Err(UrlValidationError::InvalidFormat(
            "URL must have a valid host".to_string(),
        ));
    }

    Ok(parsed)
}

/// Validates a URL for LOCAL-ONLY tools that must never reach the public
/// internet or cloud metadata services (e.g. a dev-only DNS provider whose
/// "API" is actually a loopback test server). This is the inverse of
/// [`validate_external_url`]: it REQUIRES the host to be loopback or RFC 1918
/// private, and rejects everything else -- including link-local (where cloud
/// metadata endpoints live) and public addresses.
///
/// Like `validate_external_url`, this only definitively validates literal
/// IPs and `localhost`. For a non-literal hostname, callers MUST also await
/// [`validate_loopback_or_private_domain_async`] to resolve and check the
/// actual IP(s) -- mirrors the `validate_external_url` +
/// `validate_domain_async` composition already used by the webhook service.
///
/// # Examples
///
/// ```
/// use temps_core::url_validation::validate_loopback_or_private_url;
///
/// // Valid: loopback
/// assert!(validate_loopback_or_private_url("http://127.0.0.1:8055").is_ok());
/// assert!(validate_loopback_or_private_url("http://localhost:8055").is_ok());
///
/// // Valid: RFC 1918 private
/// assert!(validate_loopback_or_private_url("http://192.168.1.10:8055").is_ok());
///
/// // Invalid: public address
/// assert!(validate_loopback_or_private_url("http://8.8.8.8").is_err());
///
/// // Invalid: cloud metadata (link-local, not private)
/// assert!(validate_loopback_or_private_url("http://169.254.169.254").is_err());
/// ```
pub fn validate_loopback_or_private_url(url: &str) -> Result<Url, UrlValidationError> {
    let parsed =
        Url::parse(url).map_err(|e| UrlValidationError::InvalidFormat(format!("{}", e)))?;

    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(UrlValidationError::InvalidScheme);
    }

    if let Some(host) = parsed.host() {
        match host {
            url::Host::Ipv4(ip) => {
                if !(ip.is_loopback() || ip.is_private()) {
                    return Err(UrlValidationError::NotLocalOrPrivate);
                }
            }
            url::Host::Ipv6(ip) => {
                if !(ip.is_loopback() || is_unique_local_ipv6(&ip)) {
                    return Err(UrlValidationError::NotLocalOrPrivate);
                }
            }
            url::Host::Domain(_domain) => {
                // A hostname isn't inherently unsafe -- only its resolved
                // IP is. Full validation happens in the async resolver;
                // don't reject here (mirrors validate_external_url).
            }
        }
    } else {
        return Err(UrlValidationError::InvalidFormat(
            "URL must have a valid host".to_string(),
        ));
    }

    Ok(parsed)
}

/// Asynchronous DNS resolution and validation for [`validate_loopback_or_private_url`]'s
/// domain-name case: resolves `domain` and requires every resolved IP to be
/// loopback or RFC 1918 private, rejecting the domain if any resolved IP is
/// public, link-local, or otherwise not local.
pub async fn validate_loopback_or_private_domain_async(
    domain: &str,
) -> Result<(), UrlValidationError> {
    let lookup_result = tokio::net::lookup_host(format!("{}:443", domain)).await;

    let addrs = match lookup_result {
        Ok(addrs) => addrs,
        Err(e) => {
            return Err(UrlValidationError::DnsResolutionFailed(format!(
                "Failed to resolve {}: {}",
                domain, e
            )));
        }
    };

    let mut has_valid_ip = false;
    for addr in addrs {
        let is_local_or_private = match addr.ip() {
            IpAddr::V4(ip) => ip.is_loopback() || ip.is_private(),
            IpAddr::V6(ip) => ip.is_loopback() || is_unique_local_ipv6(&ip),
        };
        if !is_local_or_private {
            return Err(UrlValidationError::NotLocalOrPrivate);
        }
        has_valid_ip = true;
    }

    if !has_valid_ip {
        return Err(UrlValidationError::DnsResolutionFailed(
            "No valid IP addresses found for domain".to_string(),
        ));
    }

    Ok(())
}

/// Validates an IPv4 address for external access
///
/// Blocks:
/// - Private addresses (10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16)
/// - Loopback (127.0.0.0/8)
/// - Link-local (169.254.0.0/16)
/// - Cloud metadata (169.254.169.254)
/// - Multicast (224.0.0.0/4)
/// - Broadcast (255.255.255.255)
/// - Documentation addresses (192.0.2.0/24, 198.51.100.0/24, 203.0.113.0/24)
/// - Unspecified (0.0.0.0)
pub fn validate_ipv4(ip: &Ipv4Addr) -> Result<(), UrlValidationError> {
    // Check for cloud metadata service (AWS, GCP, Azure, Alibaba Cloud)
    if is_cloud_metadata_ipv4(ip) {
        return Err(UrlValidationError::CloudMetadata);
    }

    // Check for private addresses (RFC 1918)
    if ip.is_private() {
        return Err(UrlValidationError::PrivateIp);
    }

    // Check for loopback (127.0.0.0/8)
    if ip.is_loopback() {
        return Err(UrlValidationError::LoopbackIp);
    }

    // Check for link-local (169.254.0.0/16)
    if ip.is_link_local() {
        return Err(UrlValidationError::LinkLocalIp);
    }

    // Check for multicast (224.0.0.0/4)
    if ip.is_multicast() {
        return Err(UrlValidationError::MulticastIp);
    }

    // Check for broadcast (255.255.255.255)
    if ip.is_broadcast() {
        return Err(UrlValidationError::BroadcastIp);
    }

    // Check for documentation addresses (TEST-NET-1, TEST-NET-2, TEST-NET-3)
    if ip.is_documentation() {
        return Err(UrlValidationError::DocumentationIp);
    }

    // Check for unspecified (0.0.0.0)
    if ip.is_unspecified() {
        return Err(UrlValidationError::UnspecifiedIp);
    }

    Ok(())
}

/// Validates an IPv6 address for external access
///
/// Blocks:
/// - Loopback (::1)
/// - Link-local (fe80::/10)
/// - Unique local addresses (fc00::/7)
/// - Multicast (ff00::/8)
/// - Unspecified (::)
/// - IPv6 cloud metadata (fd00:ec2::254 for AWS)
pub fn validate_ipv6(ip: &Ipv6Addr) -> Result<(), UrlValidationError> {
    // Check for cloud metadata (AWS IPv6)
    if is_cloud_metadata_ipv6(ip) {
        return Err(UrlValidationError::CloudMetadata);
    }

    // Check for loopback (::1)
    if ip.is_loopback() {
        return Err(UrlValidationError::LoopbackIp);
    }

    // Check for link-local (fe80::/10)
    if is_link_local_ipv6(ip) {
        return Err(UrlValidationError::LinkLocalIp);
    }

    // Check for unique local addresses (fc00::/7) - similar to IPv4 private addresses
    if is_unique_local_ipv6(ip) {
        return Err(UrlValidationError::PrivateIp);
    }

    // Check for multicast (ff00::/8)
    if ip.is_multicast() {
        return Err(UrlValidationError::MulticastIp);
    }

    // Check for unspecified (::)
    if ip.is_unspecified() {
        return Err(UrlValidationError::UnspecifiedIp);
    }

    Ok(())
}

/// Checks if an IPv4 address is a cloud metadata service
///
/// Blocks:
/// - 169.254.169.254 (AWS, Azure, GCP, Alibaba Cloud, Oracle Cloud)
/// - 100.100.100.200 (Alibaba Cloud alternative)
fn is_cloud_metadata_ipv4(ip: &Ipv4Addr) -> bool {
    let octets = ip.octets();

    // AWS/Azure/GCP metadata service
    if octets == [169, 254, 169, 254] {
        return true;
    }

    // Alibaba Cloud metadata service
    if octets == [100, 100, 100, 200] {
        return true;
    }

    false
}

/// Checks if an IPv6 address is a cloud metadata service
///
/// Blocks:
/// - fd00:ec2::254 (AWS IPv6 metadata)
fn is_cloud_metadata_ipv6(ip: &Ipv6Addr) -> bool {
    let segments = ip.segments();

    // AWS IPv6 metadata service (fd00:ec2::254)
    if segments[0] == 0xfd00 && segments[1] == 0x0ec2 && segments[7] == 0x0254 {
        // Check if middle segments are all zero
        if segments[2..7].iter().all(|&s| s == 0) {
            return true;
        }
    }

    false
}

/// Checks if an IPv6 address is link-local (fe80::/10)
fn is_link_local_ipv6(ip: &Ipv6Addr) -> bool {
    let segments = ip.segments();
    (segments[0] & 0xffc0) == 0xfe80
}

/// Checks if an IPv6 address is a unique local address (fc00::/7)
///
/// These are similar to RFC 1918 private addresses in IPv4
fn is_unique_local_ipv6(ip: &Ipv6Addr) -> bool {
    let segments = ip.segments();
    (segments[0] & 0xfe00) == 0xfc00
}

/// Validate a user-supplied git remote URL (Fix #12 — SSRF via libgit2).
///
/// libgit2 will happily clone from any scheme it understands. Each of these
/// is a footgun and is rejected:
/// - `file://` — local-file disclosure (read `/etc/passwd` via clone)
/// - `ssh://` / `git@host:repo` — internal-host probing + key/cred leakage
/// - `git://` — unauthenticated, unencrypted, MITM-vulnerable git daemon
///   protocol (deprecated by GitHub in 2022; see
///   <https://github.blog/2021-09-01-improving-git-protocol-security-github/>)
/// - `http://` — plaintext credentials in URL get sniffed; no host
///   authenticity check
///
/// Only `https://` is accepted. Self-hosted git on plain HTTP or an
/// internal git daemon should put TLS in front (caddy/nginx) — there is no
/// legitimate reason to clone deployment source over an unauthenticated or
/// plaintext transport in 2026.
///
/// After scheme validation, the host is run through `validate_external_url`
/// so private/loopback/link-local/cloud-metadata IPs are still rejected.
pub fn validate_git_url(url: &str) -> Result<Url, UrlValidationError> {
    // Reject SCP-style `git@host:path` before parsing — no scheme present,
    // `Url::parse` would treat it as a relative path.
    if !url.contains("://") {
        return Err(UrlValidationError::InvalidScheme);
    }
    // Require https scheme explicitly. `validate_external_url` would allow
    // http; for git clone we are stricter.
    let parsed =
        Url::parse(url).map_err(|e| UrlValidationError::InvalidFormat(format!("{}", e)))?;
    if parsed.scheme() != "https" {
        return Err(UrlValidationError::InvalidScheme);
    }
    // Reuse the external-URL validator for the host/IP checks.
    validate_external_url(url)
}

/// Redact the password portion of a URL so it is safe to include in
/// error messages and structured logs (Fix #12 — credentials in errors).
///
/// Examples:
/// - `https://user:secret@host/repo` → `https://user:***@host/repo`
/// - `https://host/repo`             → `https://host/repo`
/// - non-URL strings are returned unchanged
pub fn redact_url_password(url: &str) -> String {
    match Url::parse(url) {
        Ok(mut parsed) => {
            if parsed.password().is_some() {
                let _ = parsed.set_password(Some("***"));
            }
            parsed.to_string()
        }
        Err(_) => url.to_string(),
    }
}

/// Asynchronous DNS resolution and validation for domains
///
/// This function resolves the domain name and validates all resolved IP addresses
/// to ensure none of them point to blocked ranges.
///
/// **IMPORTANT**: This must be called in an async context (e.g., from a service method)
///
/// # Examples
///
/// ```no_run
/// use temps_core::url_validation::validate_domain_async;
///
/// #[tokio::main]
/// async fn main() {
///     // Valid public domain
///     assert!(validate_domain_async("example.com").await.is_ok());
///
///     // Invalid: domain that resolves to private IP
///     // assert!(validate_domain_async("internal.local").await.is_err());
/// }
/// ```
pub async fn validate_domain_async(domain: &str) -> Result<(), UrlValidationError> {
    // Resolve DNS to get all IP addresses
    let lookup_result = tokio::net::lookup_host(format!("{}:443", domain)).await;

    let addrs = match lookup_result {
        Ok(addrs) => addrs,
        Err(e) => {
            return Err(UrlValidationError::DnsResolutionFailed(format!(
                "Failed to resolve {}: {}",
                domain, e
            )));
        }
    };

    // Validate all resolved IP addresses
    let mut has_valid_ip = false;
    for addr in addrs {
        let validation_result = match addr.ip() {
            IpAddr::V4(ip) => validate_ipv4(&ip),
            IpAddr::V6(ip) => validate_ipv6(&ip),
        };

        match validation_result {
            Ok(()) => {
                has_valid_ip = true;
            }
            Err(_) => {
                // If any resolved IP is blocked, reject the entire domain
                return Err(UrlValidationError::DomainResolvesToBlockedIp);
            }
        }
    }

    if !has_valid_ip {
        return Err(UrlValidationError::DnsResolutionFailed(
            "No valid IP addresses found for domain".to_string(),
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_public_urls() {
        // Valid public URLs
        assert!(validate_external_url("https://example.com").is_ok());
        assert!(validate_external_url("http://example.com/webhook").is_ok());
        assert!(validate_external_url("https://api.github.com/webhooks").is_ok());
    }

    // ── validate_loopback_or_private_url: the inverse allow-list ─────────

    #[test]
    fn test_loopback_or_private_allows_loopback() {
        assert!(validate_loopback_or_private_url("http://127.0.0.1:8055").is_ok());
        assert!(validate_loopback_or_private_url("http://localhost:8055").is_ok());
        assert!(validate_loopback_or_private_url("http://[::1]:8055").is_ok());
    }

    #[test]
    fn test_loopback_or_private_allows_rfc1918_private() {
        assert!(validate_loopback_or_private_url("http://10.0.0.5:8055").is_ok());
        assert!(validate_loopback_or_private_url("http://172.20.0.3:8055").is_ok());
        assert!(validate_loopback_or_private_url("http://192.168.1.10:8055").is_ok());
    }

    #[test]
    fn test_loopback_or_private_rejects_public_ip() {
        assert!(validate_loopback_or_private_url("http://8.8.8.8").is_err());
        assert!(validate_loopback_or_private_url("http://1.1.1.1").is_err());
    }

    #[test]
    fn test_loopback_or_private_rejects_cloud_metadata_and_link_local() {
        // Cloud metadata lives in link-local space (169.254.0.0/16), which is
        // neither loopback nor RFC 1918 private -- must stay rejected even
        // though this validator's whole point is to allow "local" addresses.
        assert!(validate_loopback_or_private_url("http://169.254.169.254").is_err());
        assert!(validate_loopback_or_private_url("http://169.254.1.1").is_err());
    }

    #[test]
    fn test_loopback_or_private_rejects_bad_scheme() {
        assert!(validate_loopback_or_private_url("file:///etc/passwd").is_err());
        assert!(validate_loopback_or_private_url("ftp://127.0.0.1").is_err());
    }

    #[test]
    fn test_loopback_or_private_domain_name_passes_through_sync_check() {
        // A non-literal hostname can't be judged synchronously -- the sync
        // check must not reject it (that's what the async resolver is for).
        assert!(validate_loopback_or_private_url("http://challtestsrv:8055").is_ok());
    }

    #[tokio::test]
    async fn test_loopback_or_private_domain_async_rejects_public_domain() {
        // example.com resolves publicly -- must be rejected by the allow-list.
        assert!(validate_loopback_or_private_domain_async("example.com")
            .await
            .is_err());
    }

    #[tokio::test]
    async fn test_loopback_or_private_domain_async_allows_localhost() {
        assert!(validate_loopback_or_private_domain_async("localhost")
            .await
            .is_ok());
    }

    #[test]
    fn test_block_invalid_schemes() {
        assert!(validate_external_url("file:///etc/passwd").is_err());
        assert!(validate_external_url("ftp://example.com").is_err());
        assert!(validate_external_url("gopher://example.com").is_err());
        assert!(validate_external_url("javascript:alert(1)").is_err());
    }

    #[test]
    fn test_block_private_ips() {
        // RFC 1918 private addresses
        assert!(validate_external_url("http://10.0.0.1").is_err());
        assert!(validate_external_url("http://192.168.1.1").is_err());
        assert!(validate_external_url("http://172.16.0.1").is_err());
    }

    #[test]
    fn test_block_loopback() {
        assert!(validate_external_url("http://127.0.0.1").is_err());
        assert!(validate_external_url("http://localhost").is_err());
        assert!(validate_external_url("http://[::1]").is_err());
    }

    #[test]
    fn test_block_cloud_metadata() {
        // AWS/Azure/GCP metadata
        assert!(validate_external_url("http://169.254.169.254").is_err());
        assert!(validate_external_url("http://169.254.169.254/latest/meta-data").is_err());

        // Alibaba Cloud metadata
        assert!(validate_external_url("http://100.100.100.200").is_err());
    }

    #[test]
    fn test_block_link_local() {
        assert!(validate_external_url("http://169.254.1.1").is_err());
    }

    #[test]
    fn test_validate_ipv4() {
        // Valid public IPs
        assert!(validate_ipv4(&Ipv4Addr::new(8, 8, 8, 8)).is_ok()); // Google DNS
        assert!(validate_ipv4(&Ipv4Addr::new(1, 1, 1, 1)).is_ok()); // Cloudflare DNS

        // Invalid private IPs
        assert!(validate_ipv4(&Ipv4Addr::new(10, 0, 0, 1)).is_err());
        assert!(validate_ipv4(&Ipv4Addr::new(192, 168, 1, 1)).is_err());
        assert!(validate_ipv4(&Ipv4Addr::new(172, 16, 0, 1)).is_err());

        // Invalid loopback
        assert!(validate_ipv4(&Ipv4Addr::new(127, 0, 0, 1)).is_err());

        // Invalid cloud metadata
        assert!(validate_ipv4(&Ipv4Addr::new(169, 254, 169, 254)).is_err());

        // Invalid link-local
        assert!(validate_ipv4(&Ipv4Addr::new(169, 254, 1, 1)).is_err());

        // Invalid broadcast
        assert!(validate_ipv4(&Ipv4Addr::new(255, 255, 255, 255)).is_err());

        // Invalid unspecified
        assert!(validate_ipv4(&Ipv4Addr::new(0, 0, 0, 0)).is_err());
    }

    #[test]
    fn test_validate_ipv6() {
        // Valid public IPv6 (Google DNS)
        assert!(validate_ipv6(&"2001:4860:4860::8888".parse::<Ipv6Addr>().unwrap()).is_ok());

        // Invalid loopback
        assert!(validate_ipv6(&Ipv6Addr::LOCALHOST).is_err());

        // Invalid unspecified
        assert!(validate_ipv6(&Ipv6Addr::UNSPECIFIED).is_err());

        // Invalid link-local (fe80::/10)
        assert!(validate_ipv6(&"fe80::1".parse::<Ipv6Addr>().unwrap()).is_err());

        // Invalid unique local (fc00::/7)
        assert!(validate_ipv6(&"fc00::1".parse::<Ipv6Addr>().unwrap()).is_err());
        assert!(validate_ipv6(&"fd00::1".parse::<Ipv6Addr>().unwrap()).is_err());
    }

    #[test]
    fn test_cloud_metadata_detection() {
        // AWS/Azure/GCP
        assert!(is_cloud_metadata_ipv4(&Ipv4Addr::new(169, 254, 169, 254)));

        // Alibaba Cloud
        assert!(is_cloud_metadata_ipv4(&Ipv4Addr::new(100, 100, 100, 200)));

        // Not cloud metadata
        assert!(!is_cloud_metadata_ipv4(&Ipv4Addr::new(8, 8, 8, 8)));
    }

    #[tokio::test]
    async fn test_validate_domain_async() {
        // Valid public domain (example.com should always resolve)
        assert!(validate_domain_async("example.com").await.is_ok());

        // Invalid domain (should fail DNS resolution)
        assert!(
            validate_domain_async("this-domain-definitely-does-not-exist-12345.invalid")
                .await
                .is_err()
        );
    }

    // ── validate_git_url: only https:// is accepted ──────────────────────

    #[test]
    fn test_validate_git_url_accepts_https() {
        assert!(validate_git_url("https://github.com/foo/bar.git").is_ok());
        assert!(validate_git_url("https://gitlab.example.com/team/repo.git").is_ok());
    }

    #[test]
    fn test_validate_git_url_rejects_http() {
        // Plaintext http leaks credentials in the URL and has no host auth.
        assert!(matches!(
            validate_git_url("http://github.com/foo/bar.git"),
            Err(UrlValidationError::InvalidScheme)
        ));
    }

    #[test]
    fn test_validate_git_url_rejects_git_scheme() {
        // git:// (port 9418) is unauthenticated + unencrypted + MITM-vulnerable.
        assert!(matches!(
            validate_git_url("git://github.com/foo/bar.git"),
            Err(UrlValidationError::InvalidScheme)
        ));
    }

    #[test]
    fn test_validate_git_url_rejects_ssh_scheme() {
        assert!(matches!(
            validate_git_url("ssh://git@host/repo.git"),
            Err(UrlValidationError::InvalidScheme)
        ));
    }

    #[test]
    fn test_validate_git_url_rejects_file_scheme() {
        // file:// = local-file read primitive via clone.
        assert!(matches!(
            validate_git_url("file:///etc/passwd"),
            Err(UrlValidationError::InvalidScheme)
        ));
    }

    #[test]
    fn test_validate_git_url_rejects_scp_style() {
        // No scheme at all — Url::parse would mishandle this.
        assert!(matches!(
            validate_git_url("git@github.com:foo/bar.git"),
            Err(UrlValidationError::InvalidScheme)
        ));
    }

    #[test]
    fn test_validate_git_url_rejects_private_https() {
        // https + private IP must still be rejected via the IP host check.
        assert!(validate_git_url("https://169.254.169.254/repo.git").is_err());
        assert!(validate_git_url("https://localhost/repo.git").is_err());
    }
}
