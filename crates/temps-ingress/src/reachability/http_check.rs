// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Does a tunnel address answer plain HTTP (ADR 070 D4)?
//!
//! Cloudflare's edge proxies `http://` to a tunnel like `https://`. Unless a
//! redirect rule is in front of it, the app answers there, and what the
//! visitor sends and gets back crosses the internet unencrypted as far as
//! Cloudflare. The control plane asks for `http://<host>/` and expects a
//! redirect; the app's own answer is a finding on the address, unless the
//! user chose to serve plain HTTP there ([`HttpPolicy`]), which is only a
//! note and is never probed.
//!
//! The probe dials only addresses the shared outbound policy accepts,
//! pinned so a rebinding answer cannot slip in, follows no redirect, uses no
//! proxy and reads only the status.

use std::time::Duration;

use temps_core::url_validation::{resolve_and_validate_domain, UrlValidationError};

use super::edge_rules::HttpPolicy;
use super::model::{
    ActionKind, AddressReachability, FindingKind, FindingLevel, ReachAction, ReachFinding,
    ReachState,
};

const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// What answered `http://<host>/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlainHttp {
    /// The app itself: plain HTTP is served.
    Served { status: u16 },
    /// A redirect, a refusal, or nothing: plain HTTP serves no content.
    NotServed,
    /// Temps could not ask; why. Never a finding.
    Unchecked(String),
}

/// Classify an answer to `http://<host>/`.
pub fn classify(status: u16) -> PlainHttp {
    if (200..300).contains(&status) {
        PlainHttp::Served { status }
    } else {
        PlainHttp::NotServed
    }
}

/// Whether the verdict is one the probe is for: a reachable public or
/// signed-in address served through a tunnel.
pub fn applies(verdict: &AddressReachability) -> bool {
    verdict.state == ReachState::Reachable
        && verdict
            .way_in
            .as_deref()
            .is_some_and(|w| w.starts_with("tunnel:"))
}

/// Ask `http://<host>/` the way a browser would.
pub async fn probe(host: &str) -> PlainHttp {
    let addresses = match resolve_and_validate_domain(host, 80).await {
        Ok(addresses) => addresses,
        Err(UrlValidationError::DnsResolutionFailed(reason)) => {
            return PlainHttp::Unchecked(format!("{host} did not resolve: {reason}"))
        }
        Err(error) => {
            return PlainHttp::Unchecked(format!(
                "{host} resolves to an address Temps does not probe ({error})"
            ))
        }
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
        Err(error) => return PlainHttp::Unchecked(format!("no HTTP client: {error}")),
    };
    match client
        .get(format!("http://{host}/"))
        .header("accept", "text/html")
        .send()
        .await
    {
        Ok(response) => classify(response.status().as_u16()),
        Err(error) if error.is_connect() || error.is_timeout() => PlainHttp::NotServed,
        Err(error) => PlainHttp::Unchecked(format!("http://{host}/ did not answer: {error}")),
    }
}

/// Whether the probe is worth making for an address with `policy`: not
/// when the user chose to serve plain HTTP.
pub fn wants_probe(policy: Option<HttpPolicy>) -> bool {
    matches!(policy, None | Some(HttpPolicy::HttpsOnly))
}

/// The verdict with what the address's choice and the probe say (ADR 070
/// D4). The state never changes: visitors reach the address either way.
///
/// - Plain HTTP chosen: an informational note that it was the user's
///   choice; nothing is probed.
/// - HTTPS only chosen, plain HTTP still served: a warning, said in the
///   reason too, with *Make it reachable* to add the missing rule.
/// - No choice recorded (an address set up before the choice existed),
///   plain HTTP served: a gentle note offering *Make it reachable*.
pub fn judged(
    mut verdict: AddressReachability,
    policy: Option<HttpPolicy>,
    probe: Option<PlainHttp>,
) -> AddressReachability {
    let host = verdict.address.clone();
    let action = || {
        Some(ReachAction::new(
            ActionKind::MakeReachable,
            "Make it HTTPS only",
        ))
    };
    let finding = match (policy, probe) {
        (Some(chosen @ (HttpPolicy::AllowHttp | HttpPolicy::AllowAll)), _) => ReachFinding {
            kind: FindingKind::PlainHttpAllowed,
            level: FindingLevel::Info,
            message: format!(
                "{host} answers {}, as you chose when you made it reachable. Make it reachable \
                 again to change that.",
                chosen.describe()
            ),
            action: None,
        },
        (Some(HttpPolicy::HttpsOnly), Some(PlainHttp::Served { status })) => {
            let finding = ReachFinding {
                kind: FindingKind::PlainHttpServed,
                level: FindingLevel::Warning,
                message: format!(
                    "HTTPS only was chosen for {host}, but http://{host} answers HTTP {status} \
                     with the app instead of redirecting, so visitors on http:// are not \
                     encrypted between them and Cloudflare. The Cloudflare redirect rule is \
                     missing or could not be created (the token needs Zone · Single Redirect · \
                     Edit). Make it reachable again to add it."
                ),
                action: action(),
            };
            verdict.reason = format!("{} {}", verdict.reason, finding.message)
                .trim()
                .to_string();
            finding
        }
        (None, Some(PlainHttp::Served { status })) => ReachFinding {
            kind: FindingKind::PlainHttpNotChosen,
            level: FindingLevel::Info,
            message: format!(
                "http://{host} answers HTTP {status} too, without encryption between visitors \
                 and Cloudflare. To answer only HTTPS, make it reachable again and keep HTTPS \
                 only."
            ),
            action: action(),
        },
        _ => return verdict,
    };
    // Only a warning is a problem the address's own action should point
    // at; a note keeps its action to itself.
    if finding.level == FindingLevel::Warning && verdict.action.is_none() {
        verdict.action = finding.action.clone();
    }
    verdict.findings.push(finding);
    verdict
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reachability::model::{AddressSource, Audience};

    fn tunneled() -> AddressReachability {
        AddressReachability {
            address: "myapp.example.test".into(),
            audience: Audience::Public,
            source: AddressSource::CustomDomain,
            custom_domain_id: Some(1),
            way_in: Some("tunnel:homelab".into()),
            state: ReachState::Reachable,
            reason: "Published through tunnel homelab.".into(),
            action: None,
            network: None,
            nothing_set_up: false,
            findings: Vec::new(),
        }
    }

    #[test]
    fn only_the_apps_own_answer_is_plain_http_served() {
        assert_eq!(classify(200), PlainHttp::Served { status: 200 });
        assert_eq!(classify(204), PlainHttp::Served { status: 204 });
        for status in [301, 302, 307, 308, 403, 404, 502] {
            assert_eq!(classify(status), PlainHttp::NotServed, "{status}");
        }
    }

    #[test]
    fn https_only_chosen_but_plain_http_served_is_a_warning() {
        let verdict = judged(
            tunneled(),
            Some(HttpPolicy::HttpsOnly),
            Some(PlainHttp::Served { status: 200 }),
        );
        // Visitors still reach it: the state does not change.
        assert_eq!(verdict.state, ReachState::Reachable);
        assert_eq!(verdict.findings.len(), 1);
        let finding = &verdict.findings[0];
        assert_eq!(finding.kind, FindingKind::PlainHttpServed);
        assert_eq!(finding.level, FindingLevel::Warning);
        assert!(finding
            .message
            .starts_with("HTTPS only was chosen for myapp.example.test, but"));
        assert!(finding.message.contains("Zone · Single Redirect · Edit"));
        assert!(verdict.reason.contains("instead of redirecting"));
        assert_eq!(
            verdict.action.as_ref().map(|a| a.kind),
            Some(ActionKind::MakeReachable)
        );
    }

    #[test]
    fn plain_http_the_user_chose_is_a_note_and_never_a_problem() {
        for policy in [HttpPolicy::AllowHttp, HttpPolicy::AllowAll] {
            assert!(!wants_probe(Some(policy)));
            let verdict = judged(tunneled(), Some(policy), None);
            assert_eq!(verdict.state, ReachState::Reachable);
            assert_eq!(verdict.reason, tunneled().reason);
            assert!(verdict.action.is_none());
            assert_eq!(verdict.findings.len(), 1);
            assert_eq!(verdict.findings[0].kind, FindingKind::PlainHttpAllowed);
            assert_eq!(verdict.findings[0].level, FindingLevel::Info);
            assert!(verdict.findings[0].message.contains("as you chose"));
        }
    }

    #[test]
    fn an_address_with_no_choice_gets_a_gentle_note_when_plain_http_answers() {
        assert!(wants_probe(None));
        let verdict = judged(tunneled(), None, Some(PlainHttp::Served { status: 200 }));
        assert_eq!(verdict.state, ReachState::Reachable);
        // Not said in the reason: nothing was chosen, nothing is broken.
        assert_eq!(verdict.reason, tunneled().reason);
        assert_eq!(verdict.findings[0].kind, FindingKind::PlainHttpNotChosen);
        assert_eq!(verdict.findings[0].level, FindingLevel::Info);
        // The note offers the way to HTTPS only; the address's own action
        // is left alone.
        assert!(verdict.action.is_none());
        assert_eq!(
            verdict.findings[0].action.as_ref().map(|a| a.kind),
            Some(ActionKind::MakeReachable)
        );
    }

    #[test]
    fn a_redirect_or_an_unchecked_probe_is_not_a_finding() {
        for policy in [None, Some(HttpPolicy::HttpsOnly)] {
            for probe in [PlainHttp::NotServed, PlainHttp::Unchecked("timeout".into())] {
                let verdict = judged(tunneled(), policy, Some(probe));
                assert_eq!(verdict, tunneled());
            }
        }
    }

    #[test]
    fn only_reachable_tunnel_addresses_are_probed() {
        assert!(applies(&tunneled()));
        let mut direct = tunneled();
        direct.way_in = Some("direct:edge-1".into());
        assert!(!applies(&direct));
        let mut down = tunneled();
        down.state = ReachState::NotReachable;
        assert!(!applies(&down));
    }

    #[tokio::test]
    async fn the_probe_never_dials_a_loopback_address() {
        assert!(matches!(probe("localhost").await, PlainHttp::Unchecked(_)));
    }
}
