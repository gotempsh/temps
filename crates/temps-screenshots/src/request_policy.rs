// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What a page opened by the local screenshot service may request.
//!
//! The browser loads a deployed app, and an app is code its owner wrote: its
//! page, a redirect it answers with or a resource it embeds can name any
//! address the Temps host can reach. The capture is therefore limited to the
//! address that was asked for and to public addresses, judged with the same
//! rules as every other outbound request Temps makes
//! ([`temps_core::url_validation`], including the administrator's trusted
//! private networks).
//!
//! Two checks apply. [`RequestPolicy::decide`] runs in the browser's request
//! filter and judges the URL: its scheme, and any literal address or
//! well-known internal name in it. It cannot judge where a name leads,
//! because Chromium resolves names on its own and later. That is
//! [`RequestPolicy::resolve`], run by [`crate::pinning_proxy`] when the
//! connection is opened: it resolves the name once and returns only addresses
//! that pass, and the proxy dials exactly those.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use temps_core::url_validation::{validate_external_url, validate_outbound_ip};
use url::Url;

use crate::error::ScreenshotError;
use crate::pinning_proxy::Lookup;

/// The verdict for one request the page is about to make.
#[derive(Debug, PartialEq, Eq)]
pub enum RequestDecision {
    Allow,
    Block { reason: String },
}

/// The origin a capture was started for, and the rule for everything else.
#[derive(Debug, Clone)]
pub struct RequestPolicy {
    target_host: String,
    target_port: u16,
    /// How long one DNS lookup in [`Self::resolve`] may take.
    lookup_timeout: Duration,
}

/// How long a DNS lookup made for the captured page may take. A name that
/// does not answer in time is refused, so it cannot hold a proxy slot.
pub const LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);

impl RequestPolicy {
    /// A policy for capturing `target`. The target's own host and port are
    /// always allowed; it is the address the caller asked to be captured.
    pub fn for_target(target: &str) -> Result<Self, ScreenshotError> {
        let url = Url::parse(target)
            .map_err(|error| ScreenshotError::InvalidUrl(format!("{target}: {error}")))?;
        let host = url
            .host_str()
            .ok_or_else(|| ScreenshotError::InvalidUrl(format!("{target}: URL has no host")))?;
        let port = url.port_or_known_default().ok_or_else(|| {
            ScreenshotError::InvalidUrl(format!("{target}: URL has no known port"))
        })?;
        Ok(Self {
            target_host: host.to_ascii_lowercase(),
            target_port: port,
            lookup_timeout: LOOKUP_TIMEOUT,
        })
    }

    /// Decide one request from its URL alone. Does no lookup: where a name
    /// resolves is judged by [`Self::resolve`] when the connection is opened.
    pub fn decide(&self, request_url: &str) -> RequestDecision {
        let block = |reason: String| RequestDecision::Block { reason };

        let url = match Url::parse(request_url) {
            Ok(url) => url,
            Err(error) => return block(format!("not a valid URL ({error})")),
        };
        match url.scheme() {
            // Carry their content inline; nothing leaves the browser.
            "data" | "blob" | "about" => return RequestDecision::Allow,
            "http" | "https" | "ws" | "wss" => {}
            other => return block(format!("scheme '{other}' is not allowed")),
        }
        let Some(host) = url.host_str() else {
            return block("the URL has no host".to_string());
        };
        if host.eq_ignore_ascii_case(&self.target_host)
            && url.port_or_known_default() == Some(self.target_port)
        {
            return RequestDecision::Allow;
        }

        // Literal addresses and `localhost` names are decided here. Any other
        // name (a metadata hostname included) is judged by the address it
        // resolves to, in `resolve`.
        if let Err(error) = validate_external_url(request_url) {
            return block(error.to_string());
        }
        RequestDecision::Allow
    }

    /// The addresses a connection to `host:port` may be made to. `host` is a
    /// name or a bare IP address. A name is resolved once with `lookup`, and
    /// refused whole if any address in the answer is internal; the caller
    /// must connect only to the addresses returned.
    ///
    /// The capture's own target is resolved without that check, because it
    /// is the address the caller asked to be captured.
    pub async fn resolve(
        &self,
        host: &str,
        port: u16,
        lookup: &Lookup,
    ) -> Result<Vec<SocketAddr>, String> {
        let literal = host.parse::<IpAddr>().ok();
        let target_host = self
            .target_host
            .trim_start_matches('[')
            .trim_end_matches(']');
        let is_target = host.eq_ignore_ascii_case(target_host) && port == self.target_port;

        if !is_target {
            let url_host = match literal {
                Some(IpAddr::V6(ip)) => format!("[{ip}]"),
                _ => host.to_string(),
            };
            validate_external_url(&format!("http://{url_host}:{port}/"))
                .map_err(|error| error.to_string())?;
        }
        if let Some(ip) = literal {
            return Ok(vec![SocketAddr::new(ip, port)]);
        }

        let addresses = tokio::time::timeout(self.lookup_timeout, lookup(host.to_string(), port))
            .await
            .map_err(|_| {
                format!(
                    "'{host}' did not resolve within {}ms",
                    self.lookup_timeout.as_millis()
                )
            })?
            .map_err(|error| format!("'{host}' does not resolve ({error})"))?;
        if addresses.is_empty() {
            return Err(format!("'{host}' does not resolve to any address"));
        }
        if !is_target {
            for address in &addresses {
                validate_outbound_ip(address.ip())
                    .map_err(|error| format!("'{host}' resolves to {}: {error}", address.ip()))?;
            }
        }
        Ok(addresses)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> RequestPolicy {
        RequestPolicy::for_target("http://app.example.test:8080/page").expect("policy")
    }

    fn blocked(policy: &RequestPolicy, url: &str) -> bool {
        matches!(policy.decide(url), RequestDecision::Block { .. })
    }

    #[test]
    fn the_target_origin_is_allowed_whatever_it_resolves_to() {
        let policy = RequestPolicy::for_target("http://127.0.0.1:4100/").expect("loopback target");
        assert_eq!(
            policy.decide("http://127.0.0.1:4100/app.js"),
            RequestDecision::Allow
        );
        assert_eq!(
            policy.decide("ws://127.0.0.1:4100/socket"),
            RequestDecision::Allow
        );
        // Another port on the same host is another service.
        assert!(blocked(&policy, "http://127.0.0.1:4101/"));
        assert!(blocked(&policy, "http://127.0.0.1/"));
    }

    #[test]
    fn internal_addresses_are_blocked_even_for_a_public_target() {
        let policy = policy();
        for url in [
            "http://127.0.0.1/",
            "http://127.0.0.1:8080/",
            "http://localhost:8080/admin",
            "http://10.0.0.5:8500/v1/agent/self",
            "http://192.168.1.10/",
            "http://172.16.0.9:2375/",
            "http://169.254.169.254/latest/meta-data/",
            "http://[::1]/",
            "http://[fd00::1]/",
        ] {
            assert!(blocked(&policy, url), "{url}");
        }
    }

    #[test]
    fn a_redirect_target_is_judged_like_any_other_request() {
        let policy = policy();
        assert!(blocked(&policy, "http://169.254.169.254/"));
        assert_eq!(
            policy.decide("http://app.example.test:8080/after-redirect"),
            RequestDecision::Allow
        );
    }

    #[test]
    fn public_literal_addresses_are_allowed() {
        assert_eq!(policy().decide("https://8.8.8.8/"), RequestDecision::Allow);
    }

    #[test]
    fn schemes_that_reach_the_host_or_the_network_are_blocked() {
        let policy = policy();
        for url in [
            "file:///etc/passwd",
            "ftp://8.8.8.8/",
            "gopher://8.8.8.8/",
            "chrome://version",
        ] {
            assert!(blocked(&policy, url), "{url}");
        }
        for url in [
            "data:text/plain;base64,aGk=",
            "blob:http://app.example.test:8080/x",
            "about:blank",
        ] {
            assert_eq!(policy.decide(url), RequestDecision::Allow, "{url}");
        }
    }

    fn answering(addresses: Vec<SocketAddr>) -> Lookup {
        std::sync::Arc::new(move |_host: String, _port: u16| {
            let addresses = addresses.clone();
            Box::pin(async move { Ok(addresses) })
        })
    }

    fn address(text: &str) -> SocketAddr {
        text.parse().expect("socket address")
    }

    #[tokio::test]
    async fn a_name_is_judged_by_every_address_it_resolves_to() {
        let policy = policy();
        let public = answering(vec![address("8.8.8.8:443")]);
        assert_eq!(
            policy.resolve("cdn.example.net", 443, &public).await,
            Ok(vec![address("8.8.8.8:443")])
        );

        for internal in [
            "127.0.0.1:443",
            "10.0.0.5:443",
            "169.254.169.254:80",
            "[::1]:443",
        ] {
            let lookup = answering(vec![address("8.8.8.8:443"), address(internal)]);
            assert!(
                policy
                    .resolve("cdn.example.net", 443, &lookup)
                    .await
                    .is_err(),
                "{internal}"
            );
        }
    }

    #[tokio::test]
    async fn literals_and_internal_names_are_judged_without_a_lookup() {
        let policy = policy();
        let never: Lookup = std::sync::Arc::new(|host: String, _port: u16| {
            Box::pin(async move { panic!("unexpected lookup of {host}") })
        });
        assert_eq!(
            policy.resolve("8.8.8.8", 443, &never).await,
            Ok(vec![address("8.8.8.8:443")])
        );
        assert_eq!(
            policy.resolve("2001:4860:4860::8888", 443, &never).await,
            Ok(vec![address("[2001:4860:4860::8888]:443")])
        );
        for host in ["127.0.0.1", "::1", "169.254.169.254", "localhost"] {
            assert!(policy.resolve(host, 80, &never).await.is_err(), "{host}");
        }
        // A metadata hostname is refused by the address it resolves to.
        let metadata = answering(vec![address("169.254.169.254:80")]);
        assert!(policy
            .resolve("metadata.google.internal", 80, &metadata)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn the_target_is_resolved_without_the_check() {
        let policy = RequestPolicy::for_target("http://app.example.test:8080/").expect("policy");
        let loopback = answering(vec![address("127.0.0.1:8080")]);
        assert_eq!(
            policy.resolve("app.example.test", 8080, &loopback).await,
            Ok(vec![address("127.0.0.1:8080")])
        );
        // The same name on another port is another service.
        assert!(policy
            .resolve("app.example.test", 8081, &loopback)
            .await
            .is_err());

        let literal = RequestPolicy::for_target("http://[::1]:4100/").expect("policy");
        assert_eq!(
            literal.resolve("::1", 4100, &loopback).await,
            Ok(vec![address("[::1]:4100")])
        );
    }

    #[tokio::test]
    async fn a_lookup_that_does_not_answer_is_refused_at_its_deadline() {
        let mut policy = policy();
        policy.lookup_timeout = Duration::from_millis(50);
        let stalled: Lookup = std::sync::Arc::new(|_host: String, _port: u16| {
            Box::pin(std::future::pending::<std::io::Result<Vec<SocketAddr>>>())
        });
        let started = std::time::Instant::now();
        let verdict = tokio::time::timeout(
            Duration::from_secs(5),
            policy.resolve("slow.example.net", 443, &stalled),
        )
        .await
        .expect("resolve must give up on its own");
        assert!(verdict.is_err(), "{verdict:?}");
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[tokio::test]
    async fn a_name_that_does_not_resolve_is_refused() {
        let nothing = answering(Vec::new());
        assert!(policy()
            .resolve("nothing.example.net", 80, &nothing)
            .await
            .is_err());
        let failing: Lookup = std::sync::Arc::new(|_host: String, _port: u16| {
            Box::pin(async { Err(std::io::Error::other("lookup failed")) })
        });
        assert!(policy()
            .resolve("nothing.example.net", 80, &failing)
            .await
            .is_err());
    }

    #[test]
    fn malformed_requests_are_blocked() {
        assert!(blocked(&policy(), "not a url"));
        assert!(RequestPolicy::for_target("not a url").is_err());
    }
}
