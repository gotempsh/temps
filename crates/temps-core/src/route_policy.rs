// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Rules for operator-defined proxy routes: which public hostnames a route may
//! claim and which upstream addresses it may point at.
//!
//! The same rules run twice. The console applies them when a route is saved,
//! so the operator gets an error naming the problem. The route table applies
//! them again when it loads rows, because a row written by an older version, or
//! by hand, never went through the console.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::Duration;

use thiserror::Error;

use crate::url_validation::{
    resolve_and_validate_domain, validate_external_url, validate_outbound_ip, UrlValidationError,
};

/// Longest hostname DNS allows, without a trailing dot.
const MAX_HOSTNAME_LEN: usize = 253;
/// Longest single DNS label.
const MAX_LABEL_LEN: usize = 63;
/// How long one upstream name may take to resolve before the route is judged.
const UPSTREAM_RESOLVE_TIMEOUT: Duration = Duration::from_secs(5);

/// Why a route's public hostname is not acceptable.
#[derive(Error, Debug, PartialEq, Eq)]
pub enum RouteHostnameError {
    #[error("route domain '{domain}' is not a valid hostname: {reason}")]
    Malformed {
        domain: String,
        reason: &'static str,
    },
}

/// Why a route's upstream address is not acceptable.
#[derive(Error, Debug)]
pub enum RouteUpstreamError {
    #[error("route upstream host '{host}' is not valid: {reason}")]
    Malformed { host: String, reason: &'static str },

    #[error("route upstream port {port} is outside 1-65535")]
    InvalidPort { port: i32 },

    /// The address is one the control plane must not forward traffic to
    /// (loopback, private network, link-local, cloud metadata, ...), and no
    /// trusted private network covers it.
    #[error("route upstream '{host}' is not an allowed destination: {source}")]
    Blocked {
        host: String,
        #[source]
        source: UrlValidationError,
    },

    /// The name could not be resolved, so it could not be checked.
    #[error("route upstream '{host}' could not be resolved to check it: {detail}")]
    Unresolvable { host: String, detail: String },
}

impl RouteUpstreamError {
    /// True when the problem is that the name did not resolve, not that the
    /// address is forbidden.
    pub fn is_unresolvable(&self) -> bool {
        matches!(self, Self::Unresolvable { .. })
    }
}

/// Lower-case a route domain, drop one trailing dot and check it is a
/// well-formed hostname, optionally with a leading `*.` wildcard label.
///
/// Returns the normalised form. Ports, schemes, paths, userinfo, whitespace and
/// non-ASCII are all rejected: the value is compared with `Host` headers and
/// TLS server names, which carry none of those.
pub fn normalize_route_hostname(raw: &str) -> Result<String, RouteHostnameError> {
    let malformed = |reason: &'static str| RouteHostnameError::Malformed {
        domain: raw.trim().to_string(),
        reason,
    };

    let host = raw.trim().trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() {
        return Err(malformed("it is empty"));
    }
    if host.len() > MAX_HOSTNAME_LEN {
        return Err(malformed("it is longer than 253 characters"));
    }

    let name = host.strip_prefix("*.").unwrap_or(host.as_str());
    if name.is_empty() {
        return Err(malformed("a wildcard needs a domain after '*.'"));
    }
    if name.contains('*') {
        return Err(malformed("'*' is only allowed as the first label"));
    }

    for label in name.split('.') {
        if label.is_empty() {
            return Err(malformed("it contains an empty label"));
        }
        if label.len() > MAX_LABEL_LEN {
            return Err(malformed("a label exceeds the 63-character DNS limit"));
        }
        if !label
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err(malformed(
                "a label has characters outside a-z, 0-9 and '-' (no ports, paths or credentials)",
            ));
        }
        if label.starts_with('-') || label.ends_with('-') {
            return Err(malformed("a label starts or ends with '-'"));
        }
    }

    Ok(host)
}

/// The hostname without a wildcard prefix, for comparing against a base domain.
pub fn route_hostname_base(host: &str) -> &str {
    host.strip_prefix("*.").unwrap_or(host)
}

/// True when `host` is `base` or a name below it. Both must already be
/// lower-case and free of a trailing dot; a wildcard prefix on `host` is
/// ignored, so `*.apps.example.com` counts as below `apps.example.com`.
pub fn is_at_or_below(host: &str, base: &str) -> bool {
    let host = route_hostname_base(host);
    if base.is_empty() {
        return false;
    }
    host == base
        || host
            .strip_suffix(base)
            .is_some_and(|prefix| prefix.ends_with('.'))
}

/// Validate the shape of an upstream host and, for an address literal or a
/// `localhost`-style name, whether it is an allowed destination. Does no DNS.
///
/// Accepts an IPv4 literal, a bracketed IPv6 literal (`[2001:db8::1]`, the form
/// the proxy needs to join it with a port) or a DNS name. Underscores are
/// allowed in names because container and compose service names use them.
pub fn validate_upstream_host_literal(host: &str) -> Result<(), RouteUpstreamError> {
    let malformed = |reason: &'static str| RouteUpstreamError::Malformed {
        host: host.to_string(),
        reason,
    };
    let blocked = |source: UrlValidationError| RouteUpstreamError::Blocked {
        host: host.to_string(),
        source,
    };

    if host.is_empty() {
        return Err(malformed("it is empty"));
    }
    if host.len() > MAX_HOSTNAME_LEN {
        return Err(malformed("it is longer than 253 characters"));
    }
    if host != host.trim() {
        return Err(malformed("it has leading or trailing whitespace"));
    }

    if let Some(inner) = host.strip_prefix('[') {
        let Some(inner) = inner.strip_suffix(']') else {
            return Err(malformed("an IPv6 address needs both '[' and ']'"));
        };
        let ip: Ipv6Addr = inner
            .parse()
            .map_err(|_| malformed("the bracketed value is not an IPv6 address"))?;
        return validate_outbound_ip(IpAddr::V6(ip)).map_err(blocked);
    }
    if host.parse::<Ipv6Addr>().is_ok() {
        return Err(malformed(
            "write an IPv6 address in brackets, for example [2001:db8::1]",
        ));
    }
    if let Ok(ip) = host.parse::<Ipv4Addr>() {
        return validate_outbound_ip(IpAddr::V4(ip)).map_err(blocked);
    }

    // A name. Everything that makes a string something other than a bare host
    // (scheme, port, path, query, credentials, whitespace) is outside this set.
    let name = host.strip_suffix('.').unwrap_or(host);
    if name.is_empty() {
        return Err(malformed("it is empty"));
    }
    for label in name.split('.') {
        if label.is_empty() {
            return Err(malformed("it contains an empty label"));
        }
        if label.len() > MAX_LABEL_LEN {
            return Err(malformed("a label exceeds the 63-character DNS limit"));
        }
        if !label
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(malformed(
                "it must be a bare host name or address: no scheme, port, path or credentials",
            ));
        }
        if label.starts_with('-') || label.ends_with('-') {
            return Err(malformed("a label starts or ends with '-'"));
        }
    }

    // `localhost` and `*.localhost` names, unless loopback itself is trusted.
    validate_external_url(&format!("http://{name}/"))
        .map(|_| ())
        .map_err(blocked)
}

/// Full upstream check: [`validate_upstream_host_literal`], the port range, and
/// for a name, resolving it and refusing the route when any resolved address is
/// not an allowed destination.
///
/// A name that does not resolve in time is reported as
/// [`RouteUpstreamError::Unresolvable`] so the caller can choose between
/// refusing (a save) and keeping a row the table already serves (a reload).
pub async fn validate_upstream(host: &str, port: i32) -> Result<(), RouteUpstreamError> {
    let port = u16::try_from(port)
        .ok()
        .filter(|port| *port != 0)
        .ok_or(RouteUpstreamError::InvalidPort { port })?;

    validate_upstream_host_literal(host)?;

    let is_literal = host.starts_with('[') || host.parse::<Ipv4Addr>().is_ok();
    if is_literal {
        return Ok(());
    }

    let name = host.strip_suffix('.').unwrap_or(host);
    match tokio::time::timeout(
        UPSTREAM_RESOLVE_TIMEOUT,
        resolve_and_validate_domain(name, port),
    )
    .await
    {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(UrlValidationError::DnsResolutionFailed(detail))) => {
            Err(RouteUpstreamError::Unresolvable {
                host: host.to_string(),
                detail,
            })
        }
        Ok(Err(source)) => Err(RouteUpstreamError::Blocked {
            host: host.to_string(),
            source,
        }),
        Err(_elapsed) => Err(RouteUpstreamError::Unresolvable {
            host: host.to_string(),
            detail: format!("no answer within {}s", UPSTREAM_RESOLVE_TIMEOUT.as_secs()),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostnames_are_normalised_and_checked() {
        assert_eq!(
            normalize_route_hostname(" App.Example.COM. ").unwrap(),
            "app.example.com"
        );
        assert_eq!(
            normalize_route_hostname("*.example.com").unwrap(),
            "*.example.com"
        );
        for bad in [
            "",
            "   ",
            "*",
            "*.",
            "a.*.example.com",
            "exa mple.com",
            "example.com:8080",
            "https://example.com",
            "user@example.com",
            "example.com/path",
            "-bad.example.com",
            "bad-.example.com",
            "a..example.com",
            "under_score.example.com",
            "exämple.com",
        ] {
            assert!(
                normalize_route_hostname(bad).is_err(),
                "'{bad}' must be refused"
            );
        }
        assert!(normalize_route_hostname(&"a".repeat(64)).is_err());
        assert!(normalize_route_hostname(&format!("{}.com", "a".repeat(250))).is_err());
    }

    #[test]
    fn at_or_below_matches_whole_labels_only() {
        assert!(is_at_or_below("apps.example.com", "apps.example.com"));
        assert!(is_at_or_below("shop.apps.example.com", "apps.example.com"));
        assert!(is_at_or_below("*.apps.example.com", "apps.example.com"));
        assert!(!is_at_or_below("myapps.example.com", "apps.example.com"));
        assert!(!is_at_or_below("example.com", "apps.example.com"));
        assert!(!is_at_or_below("anything.example.com", ""));
    }

    #[test]
    fn upstream_literals_that_must_never_be_routed_to_are_refused() {
        for host in [
            "127.0.0.1",
            "127.1.2.3",
            "localhost",
            "api.localhost",
            "10.0.0.5",
            "192.168.1.10",
            "172.16.0.1",
            "169.254.169.254",
            "100.100.100.200",
            "0.0.0.0",
            "[::1]",
            "[fd00:ec2::254]",
            "[fe80::1]",
            "[::ffff:127.0.0.1]",
        ] {
            assert!(
                matches!(
                    validate_upstream_host_literal(host),
                    Err(RouteUpstreamError::Blocked { .. })
                ),
                "'{host}' must be refused as a destination: {:?}",
                validate_upstream_host_literal(host)
            );
        }
    }

    #[test]
    fn upstream_hosts_with_more_than_a_host_are_malformed() {
        for host in [
            "",
            " 203.0.113.9",
            "http://backend.example.com",
            "backend.example.com:8080",
            "backend.example.com/path",
            "user@backend.example.com",
            "back end.example.com",
            "::1",
            "[::1",
            "[not-an-address]",
            "a..b",
            "-x.example.com",
        ] {
            assert!(
                matches!(
                    validate_upstream_host_literal(host),
                    Err(RouteUpstreamError::Malformed { .. })
                ),
                "'{host}' must be malformed: {:?}",
                validate_upstream_host_literal(host)
            );
        }
    }

    #[test]
    fn public_literals_and_container_style_names_pass_the_literal_check() {
        for host in [
            "203.0.113.9",
            "[2001:db8::1]",
            "backend.example.com",
            "my_service",
            "backend.example.com.",
        ] {
            // 203.0.113.0/24 and 2001:db8::/32 are documentation ranges the
            // classifier refuses; only the shape matters for the others.
            let result = validate_upstream_host_literal(host);
            assert!(
                !matches!(result, Err(RouteUpstreamError::Malformed { .. })),
                "'{host}' is well formed: {result:?}"
            );
        }
        assert!(validate_upstream_host_literal("backend.example.com").is_ok());
        assert!(validate_upstream_host_literal("my_service").is_ok());
        assert!(validate_upstream_host_literal("8.8.8.8").is_ok());
    }

    #[tokio::test]
    async fn the_port_is_range_checked_before_anything_else() {
        for port in [0, -1, 65_536, i32::MAX] {
            assert!(matches!(
                validate_upstream("8.8.8.8", port).await,
                Err(RouteUpstreamError::InvalidPort { port: p }) if p == port
            ));
        }
        assert!(validate_upstream("8.8.8.8", 443).await.is_ok());
    }

    #[tokio::test]
    async fn an_upstream_literal_in_a_blocked_range_is_refused_with_its_port_valid() {
        assert!(matches!(
            validate_upstream("127.0.0.1", 8080).await,
            Err(RouteUpstreamError::Blocked { .. })
        ));
        assert!(matches!(
            validate_upstream("169.254.169.254", 80).await,
            Err(RouteUpstreamError::Blocked { .. })
        ));
    }

    #[tokio::test]
    async fn a_name_that_resolves_to_loopback_is_refused() {
        // `localtest.me`-style names need the network; `localhost` resolves
        // from the hosts file, and the literal check refuses it by name first.
        let error = validate_upstream("localhost", 8080)
            .await
            .expect_err("localhost must be refused");
        assert!(
            matches!(error, RouteUpstreamError::Blocked { .. }),
            "{error:?}"
        );
    }

    #[test]
    fn only_a_resolution_failure_counts_as_unresolvable() {
        let unresolved = RouteUpstreamError::Unresolvable {
            host: "x.invalid".into(),
            detail: "no such host".into(),
        };
        assert!(unresolved.is_unresolvable());
        let blocked = RouteUpstreamError::Blocked {
            host: "127.0.0.1".into(),
            source: UrlValidationError::LoopbackIp,
        };
        assert!(!blocked.is_unresolvable());
    }
}
