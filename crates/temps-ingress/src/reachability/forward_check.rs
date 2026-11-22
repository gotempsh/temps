// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Does Temps answer at a public address whose DNS points somewhere else?
//!
//! A domain behind a CDN or a reverse proxy resolves to that proxy's
//! addresses, so comparing DNS with the ways in Temps knows says nothing about
//! it. The control plane asks for `https://<host>/` instead: Temps' proxy marks
//! what it serves with [`TEMPS_ANSWER_HEADER`] and strips that header from what
//! apps send, and CDNs pass it through. The marker says a Temps proxy answered,
//! which is what a visitor needs; it is an informational signal for this page,
//! never an authorization decision.
//!
//! The probe dials only addresses the shared outbound policy accepts, pinned
//! so a rebinding answer cannot slip in, follows no redirect, uses no proxy,
//! sends `HEAD` and reads only the status and headers.

use std::future::Future;
use std::net::SocketAddr;
use std::time::Duration;

use reqwest::header::{HeaderMap, SERVER};
use temps_core::url_validation::{resolve_and_validate_domain, UrlValidationError};

use super::compute::Resolution;
use super::model::{AddressReachability, Audience, ReachState};

const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
/// The HTTP client's timeout does not cover this lookup, and a slow resolver
/// would otherwise hold up the verdict for the whole environment.
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(3);

/// Set by `temps-proxy` on what it serves, and stripped from apps' responses.
pub const TEMPS_ANSWER_HEADER: &str = "x-temps-proxy-probe-capable";

/// What stands in front of the address, when the answer says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Cloudflare,
    Bunny,
}

impl Edge {
    fn name(self) -> &'static str {
        match self {
            Edge::Cloudflare => "Cloudflare",
            Edge::Bunny => "bunny.net",
        }
    }

    fn way_in(self) -> &'static str {
        match self {
            Edge::Cloudflare => "cdn:cloudflare",
            Edge::Bunny => "cdn:bunny",
        }
    }
}

/// What answered `https://<host>/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForwardProbe {
    /// Temps' proxy, so what is in front of the address forwards to it.
    Temps { edge: Option<Edge> },
    /// Something else, or something in front of Temps that refused the check.
    Other { status: u16, edge: Option<Edge> },
    /// Temps could not ask; why.
    Unchecked(String),
}

/// Whether the verdict is the one the probe is for: a public address that
/// resolved to public addresses Temps does not recognise. Every other unknown
/// verdict is a lookup that failed, or a private address with checks of its own.
pub fn applies(verdict: &AddressReachability, resolution: &Resolution) -> bool {
    verdict.state == ReachState::Unknown
        && verdict.audience == Audience::Public
        && matches!(resolution, Resolution::Ips(ips) if !ips.is_empty())
}

fn edge_from(headers: &HeaderMap) -> Option<Edge> {
    let server = headers
        .get(SERVER)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if headers.contains_key("cf-ray") || server == "cloudflare" {
        Some(Edge::Cloudflare)
    } else if server.starts_with("bunnycdn") {
        Some(Edge::Bunny)
    } else {
        None
    }
}

/// Classify an answer.
pub fn classify(status: u16, headers: &HeaderMap) -> ForwardProbe {
    let edge = edge_from(headers);
    if headers.contains_key(TEMPS_ANSWER_HEADER) {
        ForwardProbe::Temps { edge }
    } else {
        ForwardProbe::Other { status, edge }
    }
}

/// The addresses to dial, or why the probe cannot run, within `deadline`.
async fn lookup_within(
    deadline: Duration,
    host: &str,
    lookup: impl Future<Output = Result<Vec<SocketAddr>, UrlValidationError>>,
) -> Result<Vec<SocketAddr>, ForwardProbe> {
    match tokio::time::timeout(deadline, lookup).await {
        Ok(Ok(addresses)) => Ok(addresses),
        Ok(Err(UrlValidationError::DnsResolutionFailed(reason))) => Err(ForwardProbe::Unchecked(
            format!("{host} did not resolve: {reason}"),
        )),
        Ok(Err(error)) => Err(ForwardProbe::Unchecked(format!(
            "{host} resolves to an address Temps does not probe ({error})"
        ))),
        Err(_) => Err(ForwardProbe::Unchecked(format!(
            "{host} did not resolve within {} seconds",
            deadline.as_secs()
        ))),
    }
}

/// Ask `https://<host>/` once, without reading a body.
pub async fn probe(host: &str, key: &str) -> ForwardProbe {
    let lookup = resolve_and_validate_domain(host, 443);
    let addresses = match lookup_within(LOOKUP_TIMEOUT, host, lookup).await {
        Ok(addresses) => addresses,
        Err(unchecked) => return unchecked,
    };
    let client = match reqwest::Client::builder()
        .timeout(PROBE_TIMEOUT)
        .user_agent("Temps-Reachability/1.0")
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .resolve_to_addrs(host, &addresses)
        .build()
    {
        Ok(client) => client,
        Err(error) => return ForwardProbe::Unchecked(format!("no HTTP client: {error}")),
    };
    match client
        .head(format!("https://{host}/"))
        .header(
            temps_core::probe_auth::HEADER,
            temps_core::probe_auth::sign(key, host, temps_core::probe_auth::now()),
        )
        .header("cache-control", "no-cache, no-store")
        .send()
        .await
    {
        Ok(response) => classify(response.status().as_u16(), response.headers()),
        Err(error) => ForwardProbe::Unchecked(format!("https://{host}/ did not answer: {error}")),
    }
}

/// The verdict once the probe has answered. Only Temps' own answer makes the
/// address reachable; anything else stays unknown, because a CDN's bot rule or
/// an origin error also answers without the marker.
pub fn judged(mut verdict: AddressReachability, probe: ForwardProbe) -> AddressReachability {
    let host = verdict.address.clone();
    match probe {
        ForwardProbe::Temps { edge } => {
            verdict.state = ReachState::Reachable;
            verdict.action = None;
            match edge {
                Some(edge) => {
                    verdict.way_in = Some(edge.way_in().to_string());
                    verdict.reason = format!("{host} is served by Temps through {}.", edge.name());
                }
                None => {
                    verdict.way_in = Some("forwarded".to_string());
                    verdict.reason =
                        format!("{host} is served by Temps through a proxy in front of it.");
                }
            }
        }
        ForwardProbe::Other { status, edge } => {
            let through = edge
                .map(|edge| format!(" through {}", edge.name()))
                .unwrap_or_default();
            verdict.reason = format!(
                "{host} answered HTTP {status}{through}, but the answer did not come from \
                 Temps. Something else may be serving it, or a rule in front of it refused \
                 the check."
            );
        }
        ForwardProbe::Unchecked(why) => {
            verdict.reason = format!(
                "{} Temps could not check it from here: {why}.",
                verdict.reason
            );
        }
    }
    verdict
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reachability::model::AddressSource;
    use reqwest::header::HeaderValue;

    fn elsewhere() -> AddressReachability {
        AddressReachability {
            address: "myapp.example.test".into(),
            audience: Audience::Public,
            source: AddressSource::CustomDomain,
            custom_domain_id: Some(1),
            way_in: None,
            state: ReachState::Unknown,
            reason: "myapp.example.test points at 203.0.113.7.".into(),
            action: None,
            network: None,
            nothing_set_up: false,
            findings: Vec::new(),
        }
    }

    fn resolved() -> Resolution {
        Resolution::Ips(vec!["203.0.113.7".parse().expect("test IP")])
    }

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(*name, HeaderValue::from_static(value));
        }
        map
    }

    #[test]
    fn only_an_unknown_public_address_that_resolved_is_probed() {
        assert!(applies(&elsewhere(), &resolved()));

        let mut reachable = elsewhere();
        reachable.state = ReachState::Reachable;
        assert!(!applies(&reachable, &resolved()));

        let mut down = elsewhere();
        down.state = ReachState::NotReachable;
        assert!(!applies(&down, &resolved()));

        for audience in [
            Audience::Private,
            Audience::SignedIn,
            Audience::TailnetUsers,
            Audience::Unsupported,
        ] {
            let mut other = elsewhere();
            other.audience = audience;
            assert!(!applies(&other, &resolved()), "{audience:?}");
        }

        // A failed lookup is unknown too, and there is nothing to dial.
        for resolution in [
            Resolution::Failed("timed out".into()),
            Resolution::Skipped,
            Resolution::NoRecord,
            Resolution::Ips(Vec::new()),
        ] {
            assert!(!applies(&elsewhere(), &resolution), "{resolution:?}");
        }
    }

    #[test]
    fn the_marker_decides_who_answered_and_the_headers_name_the_edge() {
        assert_eq!(
            classify(
                200,
                &headers(&[
                    (TEMPS_ANSWER_HEADER, "1"),
                    ("cf-ray", "0000000000000000-AAA")
                ])
            ),
            ForwardProbe::Temps {
                edge: Some(Edge::Cloudflare)
            }
        );
        assert_eq!(
            classify(
                301,
                &headers(&[(TEMPS_ANSWER_HEADER, "1"), ("server", "BunnyCDN-DE1-123")])
            ),
            ForwardProbe::Temps {
                edge: Some(Edge::Bunny)
            }
        );
        assert_eq!(
            classify(200, &headers(&[(TEMPS_ANSWER_HEADER, "1")])),
            ForwardProbe::Temps { edge: None }
        );
        // A CDN's own refusal or origin error carries no marker.
        assert_eq!(
            classify(522, &headers(&[("server", "cloudflare")])),
            ForwardProbe::Other {
                status: 522,
                edge: Some(Edge::Cloudflare)
            }
        );
        assert_eq!(
            classify(200, &headers(&[("server", "nginx")])),
            ForwardProbe::Other {
                status: 200,
                edge: None
            }
        );
    }

    #[test]
    fn temps_answering_makes_the_address_reachable_through_the_edge() {
        let verdict = judged(
            elsewhere(),
            ForwardProbe::Temps {
                edge: Some(Edge::Cloudflare),
            },
        );
        assert_eq!(verdict.state, ReachState::Reachable);
        assert_eq!(verdict.way_in.as_deref(), Some("cdn:cloudflare"));
        assert_eq!(
            verdict.reason,
            "myapp.example.test is served by Temps through Cloudflare."
        );

        let bunny = judged(
            elsewhere(),
            ForwardProbe::Temps {
                edge: Some(Edge::Bunny),
            },
        );
        assert_eq!(bunny.way_in.as_deref(), Some("cdn:bunny"));

        let proxy = judged(elsewhere(), ForwardProbe::Temps { edge: None });
        assert_eq!(proxy.state, ReachState::Reachable);
        assert_eq!(proxy.way_in.as_deref(), Some("forwarded"));
    }

    #[test]
    fn another_answer_stays_unknown_and_says_what_answered() {
        let verdict = judged(
            elsewhere(),
            ForwardProbe::Other {
                status: 403,
                edge: Some(Edge::Cloudflare),
            },
        );
        assert_eq!(verdict.state, ReachState::Unknown);
        assert_eq!(verdict.way_in, None);
        assert!(verdict
            .reason
            .starts_with("myapp.example.test answered HTTP 403 through Cloudflare"));
        assert!(verdict.reason.contains("did not come from Temps"));
    }

    #[test]
    fn a_check_that_could_not_run_keeps_the_dns_reason_and_says_why() {
        let verdict = judged(elsewhere(), ForwardProbe::Unchecked("timed out".into()));
        assert_eq!(verdict.state, ReachState::Unknown);
        assert_eq!(verdict.way_in, None);
        assert_eq!(
            verdict.reason,
            "myapp.example.test points at 203.0.113.7. Temps could not check it from here: \
             timed out."
        );
    }

    #[tokio::test]
    async fn a_resolver_that_never_answers_ends_the_check_with_a_reason() {
        let result = lookup_within(
            Duration::from_millis(10),
            "myapp.example.test",
            std::future::pending(),
        )
        .await;
        assert!(matches!(
            result,
            Err(ForwardProbe::Unchecked(why)) if why.starts_with("myapp.example.test did not resolve within")
        ));
    }

    #[tokio::test]
    async fn a_lookup_in_time_returns_its_addresses_or_says_why_not() {
        let addresses = vec!["203.0.113.7:443".parse().expect("test address")];
        let found = lookup_within(Duration::from_secs(1), "myapp.example.test", async {
            Ok(addresses.clone())
        })
        .await;
        assert_eq!(found, Ok(addresses));

        let failed = lookup_within(Duration::from_secs(1), "myapp.example.test", async {
            Err(UrlValidationError::DnsResolutionFailed("no record".into()))
        })
        .await;
        assert_eq!(
            failed,
            Err(ForwardProbe::Unchecked(
                "myapp.example.test did not resolve: no record".into()
            ))
        );
    }

    #[tokio::test]
    async fn the_probe_never_dials_a_loopback_address() {
        assert!(matches!(
            probe("localhost", "test-key").await,
            ForwardProbe::Unchecked(_)
        ));
    }
}
