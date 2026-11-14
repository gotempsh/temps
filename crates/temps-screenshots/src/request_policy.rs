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

use std::net::ToSocketAddrs;

use temps_core::url_validation::{validate_external_url, validate_outbound_ip};
use url::Url;

use crate::error::ScreenshotError;

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
}

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
        })
    }

    /// Decide one request. Blocking: a name that is not an IP literal is
    /// resolved here, because the browser's own lookup cannot be inspected.
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

        // Literal addresses and well-known internal names (`localhost`,
        // `*.internal`, metadata hostnames) are decided without a lookup.
        if let Err(error) = validate_external_url(request_url) {
            return block(error.to_string());
        }
        if let Some(url::Host::Domain(domain)) = url.host() {
            let port = url.port_or_known_default().unwrap_or(443);
            let addresses = match (domain, port).to_socket_addrs() {
                Ok(addresses) => addresses.collect::<Vec<_>>(),
                Err(error) => return block(format!("'{domain}' does not resolve ({error})")),
            };
            if addresses.is_empty() {
                return block(format!("'{domain}' does not resolve to any address"));
            }
            for address in addresses {
                if let Err(error) = validate_outbound_ip(address.ip()) {
                    return block(format!("'{domain}' resolves to {}: {error}", address.ip()));
                }
            }
        }
        RequestDecision::Allow
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
            "http://metadata.google.internal/",
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

    #[test]
    fn a_name_that_does_not_resolve_is_blocked() {
        // `.invalid` never resolves (RFC 2606), so the lookup fails closed.
        assert!(blocked(&policy(), "http://nothing-here.invalid/"));
    }

    #[test]
    fn malformed_requests_are_blocked() {
        assert!(blocked(&policy(), "not a url"));
        assert!(RequestPolicy::for_target("not a url").is_err());
    }
}
