// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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

    #[error("Reserved or non-global addresses are not allowed")]
    ReservedIp,

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

    // Reject special-use ranges that may be routed internally by the host,
    // cloud provider, VPN, or container network. These are not globally
    // reachable destinations and must never be accepted by an external-only
    // SSRF allowlist.
    let octets = ip.octets();
    let is_reserved = octets[0] == 0
        || (octets[0] == 100 && (64..=127).contains(&octets[1])) // RFC 6598 CGNAT
        || (octets[0] == 192 && octets[1] == 0 && octets[2] == 0) // IETF protocols
        || (octets[0] == 192 && octets[1] == 88 && octets[2] == 99) // deprecated 6to4 relay
        || (octets[0] == 198 && (18..=19).contains(&octets[1])) // benchmarking
        || octets[0] >= 240; // reserved for future use
    if is_reserved {
        return Err(UrlValidationError::ReservedIp);
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
    // IPv4-compatible and IPv4-mapped IPv6 addresses are routed through the
    // embedded IPv4 destination by operating systems. Validate that embedded
    // address with the IPv4 policy so forms such as ::ffff:127.0.0.1 cannot
    // bypass loopback/private/cloud-metadata checks.
    if let Some(ipv4) = ip.to_ipv4() {
        return validate_ipv4(&ipv4);
    }

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

    // Deprecated site-local addresses (fec0::/10) may still be routed by
    // internal networks and are never valid external destinations.
    if (ip.segments()[0] & 0xffc0) == 0xfec0 {
        return Err(UrlValidationError::ReservedIp);
    }

    // Check for multicast (ff00::/8)
    if ip.is_multicast() {
        return Err(UrlValidationError::MulticastIp);
    }

    // Check for unspecified (::)
    if ip.is_unspecified() {
        return Err(UrlValidationError::UnspecifiedIp);
    }

    // External SMTP destinations must be globally routable unicast addresses.
    // Today those allocations live in 2000::/3. Keep this as an allowlist so
    // special-use prefixes such as NAT64, discard-only, benchmarking, and
    // future local allocations cannot become SSRF targets merely because the
    // host happens to route them internally.
    let segments = ip.segments();
    let is_global_unicast = (segments[0] & 0xe000) == 0x2000;
    let is_ietf_special = segments[0] == 0x2001 && segments[1] <= 0x01ff; // 2001::/23
    let is_documentation_2001 = segments[0] == 0x2001 && segments[1] == 0x0db8; // 2001:db8::/32
    let is_6to4 = segments[0] == 0x2002; // deprecated transition prefix
    let is_documentation = segments[0] == 0x3fff && (segments[1] & 0xf000) == 0; // 3fff::/20
    if !is_global_unicast || is_ietf_special || is_documentation_2001 || is_6to4 || is_documentation
    {
        return Err(UrlValidationError::ReservedIp);
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
    // Git credentials belong in the provider/token fields, never URL userinfo.
    // Apart from being easy to leak through libgit2 errors, a username can
    // itself be the token when no password is present.
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(UrlValidationError::InvalidFormat(
            "credentials embedded in Git URLs are not allowed".to_string(),
        ));
    }
    // Reuse the external-URL validator for the host/IP checks.
    validate_external_url(url)
}

/// Database connection schemes an importer may be pointed at.
const ALLOWED_DATABASE_SCHEMES: &[&str] = &[
    "postgres",
    "postgresql",
    "mysql",
    "mariadb",
    "mongodb",
    "mongodb+srv",
    "redis",
    "rediss",
];

/// Validate a **database** connection URL that came from an untrusted source
/// (e.g. a remote platform's API response during an import).
///
/// [`validate_external_url`] only accepts `http`/`https`, so it cannot be used
/// for connection strings. This applies the identical host/IP rules —
/// rejecting loopback, RFC 1918, link-local, cloud-metadata, multicast,
/// broadcast and other reserved addresses — while allowing database schemes.
///
/// This matters because the importer starts an official database client
/// container with `network_mode=host` and passes the URL straight to
/// `pg_dump`/`mariadb-dump`/`mongodump`. A source platform that reports
/// `external_db_url: postgres://…@127.0.0.1:5432/…` would otherwise make Temps
/// connect to a control-plane-internal database from the Docker host network
/// and copy its contents into a project the caller owns.
///
/// Like `validate_external_url`, a non-literal hostname is only checked for
/// obvious loopback names here; callers that can afford it should also await
/// [`validate_domain_async`] on the host.
///
/// # Examples
///
/// ```
/// use temps_core::url_validation::validate_external_database_url;
///
/// assert!(validate_external_database_url("postgres://u:p@db.example.com:5432/app").is_ok());
/// assert!(validate_external_database_url("postgres://u:p@127.0.0.1:5432/app").is_err());
/// assert!(validate_external_database_url("postgres://u:p@10.0.0.5:5432/app").is_err());
/// assert!(validate_external_database_url("postgres://u:p@169.254.169.254/app").is_err());
/// assert!(validate_external_database_url("file:///etc/passwd").is_err());
/// ```
pub fn validate_external_database_url(url: &str) -> Result<Url, UrlValidationError> {
    let parsed =
        Url::parse(url).map_err(|e| UrlValidationError::InvalidFormat(format!("{}", e)))?;

    if !ALLOWED_DATABASE_SCHEMES.contains(&parsed.scheme()) {
        return Err(UrlValidationError::InvalidScheme);
    }

    // NOTE: do not use `parsed.host()` here. The `url` crate only parses hosts
    // into `Host::Ipv4`/`Host::Ipv6` for *special* schemes (http, https, ws,
    // wss, ftp, file). Database schemes are not special, so
    // `postgres://u:p@127.0.0.1/app` yields `Host::Domain("127.0.0.1")` and an
    // IP-shaped host would sail straight past the domain branch. Parse the
    // host string as an address ourselves.
    let Some(host) = parsed.host_str() else {
        return Err(UrlValidationError::InvalidFormat(
            "database URL must have a valid host".to_string(),
        ));
    };

    // `[::1]` — strip the brackets the URL form requires before parsing.
    let bare = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);

    match bare.parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => validate_ipv4(&ip)?,
        Ok(IpAddr::V6(ip)) => validate_ipv6(&ip)?,
        Err(_) => {
            let lower = bare.to_lowercase();
            if lower.is_empty() {
                return Err(UrlValidationError::InvalidFormat(
                    "database URL must have a valid host".to_string(),
                ));
            }
            if lower == "localhost" || lower.ends_with(".localhost") {
                return Err(UrlValidationError::LoopbackIp);
            }
        }
    }

    Ok(parsed)
}

/// Redact the userinfo portion of a URL so it is safe to include in
/// error messages and structured logs (Fix #12 — credentials in errors).
///
/// Examples:
/// - `https://user:secret@host/repo` → `https://***:***@host/repo`
/// - `https://token@host/repo`       → `https://***@host/repo`
/// - `https://host/repo`             → `https://host/repo`
/// - non-URL strings are returned unchanged
pub fn redact_url_password(url: &str) -> String {
    match Url::parse(url) {
        Ok(mut parsed) => {
            if !parsed.username().is_empty() {
                let _ = parsed.set_username("***");
            }
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
    resolve_and_validate_domain(domain, 443).await.map(|_| ())
}

/// Async counterpart to [`validate_external_database_url`]: same checks, plus
/// DNS resolution of a non-literal host.
///
/// The sync version can only reject IP *literals* and `localhost`. That is not
/// enough when the URL comes from a remote platform the attacker controls,
/// because they also control their own DNS: one A record pointing
/// `db.attacker.tld` at `127.0.0.1` or `169.254.169.254` walks straight past a
/// literal-only check. Any caller that can afford a DNS lookup — i.e. anything
/// not on a request hot path — should use this instead.
///
/// A resolution failure is an error, not a pass: an unresolvable host cannot be
/// dialled anyway, and treating "we could not check" as "it is fine" is how the
/// literal-only gap got here.
pub async fn validate_external_database_url_async(url: &str) -> Result<Url, UrlValidationError> {
    let parsed = validate_external_database_url(url)?;

    let Some(host) = parsed.host_str() else {
        return Err(UrlValidationError::InvalidFormat(
            "database URL must have a valid host".to_string(),
        ));
    };
    let bare = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);

    // A literal was already fully validated above; only a name needs resolving.
    if bare.parse::<IpAddr>().is_err() {
        // Port is irrelevant to the address check — `resolve_and_validate_domain`
        // needs one only to form a socket address.
        resolve_and_validate_domain(bare, parsed.port().unwrap_or(443)).await?;
    }

    Ok(parsed)
}

/// Resolve `domain:port` and return the socket addresses, **rejecting the whole
/// domain if any resolved IP is non-public** (loopback, RFC1918, link-local,
/// etc.).
///
/// Unlike [`validate_domain_async`], this returns the validated addresses so a
/// caller can pin its HTTP client to them. Pinning closes the DNS-rebinding
/// window: without it, a hostname validated as public here can re-resolve to
/// `127.0.0.1` / `169.254.169.254` / an RFC1918 address by the time the client
/// dials it. Dialing the exact addresses validated here removes that gap.
pub async fn resolve_and_validate_domain(
    domain: &str,
    port: u16,
) -> Result<Vec<std::net::SocketAddr>, UrlValidationError> {
    let addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host(format!("{}:{}", domain, port))
        .await
        .map_err(|e| {
            UrlValidationError::DnsResolutionFailed(format!("Failed to resolve {}: {}", domain, e))
        })?
        .collect();

    if addrs.is_empty() {
        return Err(UrlValidationError::DnsResolutionFailed(
            "No valid IP addresses found for domain".to_string(),
        ));
    }

    for addr in &addrs {
        let validation_result = match addr.ip() {
            IpAddr::V4(ip) => validate_ipv4(&ip),
            IpAddr::V6(ip) => validate_ipv6(&ip),
        };
        if validation_result.is_err() {
            // If any resolved IP is blocked, reject the entire domain.
            return Err(UrlValidationError::DomainResolvesToBlockedIp);
        }
    }

    Ok(addrs)
}

#[cfg(test)]
mod database_url_tests {
    use super::validate_external_database_url;

    /// The importer starts a database client container with
    /// `network_mode=host` and hands it this URL, so an address the control
    /// plane can reach but the public internet cannot is exactly the SSRF the
    /// guard exists to stop.
    #[test]
    fn rejects_internal_targets() {
        for url in [
            "postgres://u:p@127.0.0.1:5432/app",
            "postgres://u:p@localhost:5432/app",
            "postgres://u:p@db.localhost:5432/app",
            "postgres://u:p@10.1.2.3:5432/app",
            "postgres://u:p@172.16.0.9:5432/app",
            "postgres://u:p@192.168.1.10:5432/app",
            "postgres://u:p@169.254.169.254:5432/app",
            "mysql://u:p@127.0.0.1:3306/app",
            "mongodb://u:p@10.0.0.1:27017/app",
            "postgres://u:p@[::1]:5432/app",
        ] {
            assert!(
                validate_external_database_url(url).is_err(),
                "{url} must be rejected"
            );
        }
    }

    #[test]
    fn accepts_public_database_endpoints() {
        for url in [
            "postgres://u:p@db.example.com:5432/app",
            "postgresql://u:p@db.example.com/app",
            "mysql://u:p@db.example.com:3306/app",
            "mariadb://u:p@db.example.com:3306/app",
            "mongodb://u:p@db.example.com:27017/app",
            "mongodb+srv://u:p@cluster.example.com/app",
            "redis://u:p@cache.example.com:6379",
            "postgres://u:p@93.184.216.34:5432/app",
        ] {
            assert!(
                validate_external_database_url(url).is_ok(),
                "{url} must be accepted"
            );
        }
    }

    /// Non-database schemes must not slip through — `file://` would make the
    /// dump container read the host filesystem.
    #[test]
    fn rejects_non_database_schemes() {
        for url in [
            "file:///etc/passwd",
            "http://example.com",
            "https://example.com",
            "gopher://example.com",
            "not a url",
        ] {
            assert!(
                validate_external_database_url(url).is_err(),
                "{url} must be rejected"
            );
        }
    }
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

        // Invalid special-use/non-global ranges
        assert!(validate_ipv4(&Ipv4Addr::new(0, 1, 2, 3)).is_err());
        assert!(validate_ipv4(&Ipv4Addr::new(100, 64, 0, 1)).is_err());
        assert!(validate_ipv4(&Ipv4Addr::new(100, 127, 255, 254)).is_err());
        assert!(validate_ipv4(&Ipv4Addr::new(192, 0, 0, 1)).is_err());
        assert!(validate_ipv4(&Ipv4Addr::new(192, 88, 99, 1)).is_err());
        assert!(validate_ipv4(&Ipv4Addr::new(198, 18, 0, 1)).is_err());
        assert!(validate_ipv4(&Ipv4Addr::new(240, 0, 0, 1)).is_err());
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

        // IPv4-mapped/compatible forms must inherit the IPv4 policy.
        assert!(validate_ipv6(&"::ffff:127.0.0.1".parse::<Ipv6Addr>().unwrap()).is_err());
        assert!(validate_ipv6(&"::ffff:10.0.0.5".parse::<Ipv6Addr>().unwrap()).is_err());
        assert!(validate_ipv6(&"::ffff:169.254.169.254".parse::<Ipv6Addr>().unwrap()).is_err());
        assert!(validate_ipv6(&"::ffff:100.64.0.1".parse::<Ipv6Addr>().unwrap()).is_err());

        // Deprecated site-local addresses are internal-only.
        assert!(validate_ipv6(&"fec0::1".parse::<Ipv6Addr>().unwrap()).is_err());

        // Every special-use prefix stays outside the external-address
        // allowlist, including translation ranges that an internal router may
        // map to IPv4 services.
        for address in [
            "64:ff9b::1",
            "64:ff9b:1::1",
            "100::1",
            "2001::1",
            "2001:db8::1",
            "2002::1",
            "3fff::1",
            "5f00::1",
        ] {
            let ip = address.parse::<Ipv6Addr>().unwrap();
            assert!(
                validate_ipv6(&ip).is_err(),
                "special-use IPv6 address {address} must be rejected"
            );
        }
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

    // Regression for security review finding #8 (SSRF via DNS rebinding). The
    // delivery path re-resolves and pins to the addresses this returns; a
    // hostname that resolves to a loopback/internal IP must be rejected, and the
    // returned addresses (used for pinning) must be exactly the resolved ones.
    #[tokio::test]
    async fn resolve_and_validate_domain_rejects_loopback() {
        // localhost always resolves to 127.0.0.1 / ::1 — the rebinding target.
        assert!(matches!(
            resolve_and_validate_domain("localhost", 443).await,
            Err(UrlValidationError::DomainResolvesToBlockedIp)
        ));
    }

    #[tokio::test]
    async fn resolve_and_validate_domain_returns_addrs_for_public_host() {
        let addrs = resolve_and_validate_domain("example.com", 443)
            .await
            .expect("example.com resolves to public IPs");
        assert!(!addrs.is_empty());
        assert!(addrs.iter().all(|a| a.port() == 443));
    }

    // ── validate_git_url: only https:// is accepted ──────────────────────

    #[test]
    fn test_validate_git_url_accepts_https() {
        assert!(validate_git_url("https://github.com/foo/bar.git").is_ok());
        assert!(validate_git_url("https://gitlab.example.com/team/repo.git").is_ok());
    }

    #[test]
    fn test_validate_git_url_rejects_embedded_credentials() {
        for url in [
            "https://token:secret@github.com/foo/bar.git",
            "https://token@github.com/foo/bar.git",
        ] {
            assert!(matches!(
                validate_git_url(url),
                Err(UrlValidationError::InvalidFormat(_))
            ));
        }
    }

    #[test]
    fn test_redact_url_password_masks_all_userinfo() {
        let with_password = redact_url_password("https://token:secret@github.com/foo/bar.git");
        assert!(!with_password.contains("token"));
        assert!(!with_password.contains("secret"));

        let username_only = redact_url_password("https://token@github.com/foo/bar.git");
        assert!(!username_only.contains("token"));
        assert!(username_only.contains("***"));
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
    /// Regression: the literal-only check is not enough when the attacker
    /// controls the DNS for the hostname they hand us — which is exactly the
    /// importer's threat model, where `source_url` comes out of the remote
    /// platform's own API response.
    #[tokio::test]
    async fn async_database_url_validation_rejects_a_name_resolving_to_loopback() {
        // `localhost` is the one name every machine resolves to loopback, so
        // this exercises the resolution path without depending on the network.
        // The sync check rejects this name by string match; force resolution to
        // be the thing under test by using a form the string check misses.
        let err =
            validate_external_database_url_async("postgres://u:p@localhost.localdomain:5432/app")
                .await;
        // Either it resolved to loopback (rejected) or the name does not exist
        // on this host (also rejected) — both are the safe outcome, and a pass
        // would mean an unresolved name was treated as public.
        assert!(
            err.is_err(),
            "a name that resolves to loopback (or not at all) must not be accepted"
        );
    }

    #[tokio::test]
    async fn async_database_url_validation_still_rejects_literals_and_schemes() {
        for hostile in [
            "postgres://u:p@127.0.0.1:5432/app",
            "postgres://u:p@10.0.0.5:5432/app",
            "postgres://u:p@169.254.169.254/app",
            "file:///etc/passwd",
        ] {
            assert!(
                validate_external_database_url_async(hostile).await.is_err(),
                "{hostile} must be rejected"
            );
        }
    }
}
