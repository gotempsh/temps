// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! An environment's primary address, the preview domain's status and the
//! deploy warning, from facts the service gathered (ADR 068 D2, D4, D5).
//! Pure: no database, no network.

use std::net::IpAddr;

use temps_core::environment_address::{
    AddressKind, PreviewDomainStatus, PrimaryAddress, ReachabilityFix, ReachabilityFixKind,
    ReachabilityWarning,
};
use temps_core::worker_ingress::{address_scope, AddressScope};

use super::compute::{is_local_only, overall_state, DirectIngress, Resolution, INGRESS_SETUP_PATH};
use super::model::{AddressReachability, AddressSource, Audience, ReachState};

/// Where the preview domain is set.
pub const PREVIEW_DOMAIN_SETTINGS_PATH: &str = "/settings#preview-domain";
/// What a worker runs to take public traffic, with Temps choosing how.
pub const WORKER_INGRESS_COMMAND: &str = "temps agent ingress delegated";

/// D4's rank of an address: lower is better. Anything not for the public
/// is a private address, whatever serves it: a listener, a tailnet name
/// through Serve or a Tailscale Service (ADR 065), or a `tailnet_users`
/// address that admits only the people listed.
pub(super) fn rank(address: &AddressReachability) -> u8 {
    if address.audience != Audience::Public {
        return 3;
    }
    match address.source {
        AddressSource::CustomDomain | AddressSource::EnvironmentDomain => 0,
        AddressSource::TunnelHost => 1,
        AddressSource::Generated => 2,
    }
}

fn kind_of(address: &AddressReachability) -> AddressKind {
    match rank(address) {
        0 => AddressKind::CustomDomain,
        1 => AddressKind::TunnelHost,
        2 => AddressKind::Generated,
        _ => AddressKind::Private,
    }
}

/// D4: the first reachable address, custom domains first, then a tunnel
/// environment host, the generated host and a private address. Among equals
/// the order the addresses were gathered in decides.
pub fn primary_address(addresses: &[AddressReachability]) -> Option<PrimaryAddress> {
    addresses
        .iter()
        .filter(|a| a.state == ReachState::Reachable)
        .min_by_key(|a| rank(a))
        .map(|a| PrimaryAddress {
            address: a.address.clone(),
            kind: kind_of(a),
            way_in: a.way_in.clone(),
        })
}

/// D4: why the environment has no address, only when reachability is sure
/// that no way in was ever set up for any of its addresses. While an
/// address is being set up, when Temps cannot tell, or when a way in that
/// was set up broke (a tunnel in error, a failed direct check, a custom
/// domain whose record went away), there is no reason: the monitor keeps
/// probing, so an outage is recorded as one, and the open button stays.
pub fn no_address_reason(addresses: &[AddressReachability]) -> Option<String> {
    if overall_state(addresses) != ReachState::NotReachable
        || !addresses.iter().all(|a| a.nothing_set_up)
    {
        return None;
    }
    let first = addresses.iter().min_by_key(|a| rank(a));
    Some(match first {
        Some(address) => address.reason.clone(),
        None => "This environment has no address yet.".to_string(),
    })
}

/// What a probe of the preview domain found (D2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewDomainFacts<'a> {
    pub preview_domain: &'a str,
    /// What `temps-probe-<random>.<preview_domain>` resolved to.
    pub probe: Resolution,
    /// The zones of the tunnels that are running.
    pub tunnel_zones: &'a [String],
}

/// D2: the status and, in words, why.
pub fn preview_domain_status(
    facts: &PreviewDomainFacts<'_>,
    directs: &[DirectIngress],
) -> (PreviewDomainStatus, String) {
    let domain = facts
        .preview_domain
        .trim()
        .trim_start_matches("*.")
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if domain.is_empty() || is_local_only(&domain) {
        let shown = if domain.is_empty() {
            "No preview domain is set".to_string()
        } else {
            format!("{domain} only resolves to the computer it is opened on")
        };
        return (
            PreviewDomainStatus::LocalOnly,
            format!("{shown}, so nobody else can open the addresses Temps generates under it."),
        );
    }
    let ips = match &facts.probe {
        Resolution::Ips(ips) if !ips.is_empty() => ips,
        Resolution::Failed(error) => {
            return (
                PreviewDomainStatus::Unresolved,
                format!("Names under {domain} could not be looked up: {error}"),
            )
        }
        _ => {
            return (
                PreviewDomainStatus::Unresolved,
                format!(
                    "Names under {domain} do not resolve: add a wildcard DNS record \
                     (*.{domain}) pointing at this install."
                ),
            )
        }
    };
    if let Some(direct) = directs
        .iter()
        .find(|d| d.check_failed.is_none() && d.ips.iter().any(|ip| ips.contains(ip)))
    {
        return (
            PreviewDomainStatus::Usable,
            format!("*.{domain} points at {} ({}).", direct.name, ip_list(ips)),
        );
    }
    if let Some(zone) = facts
        .tunnel_zones
        .iter()
        .map(|z| z.trim_end_matches('.').to_ascii_lowercase())
        .find(|zone| domain == *zone || domain.ends_with(&format!(".{zone}")))
    {
        return (
            PreviewDomainStatus::Usable,
            format!("*.{domain} resolves, and the tunnel for {zone} serves it."),
        );
    }
    let reason = if ips
        .iter()
        .all(|ip| address_scope(*ip) != AddressScope::Public)
    {
        format!(
            "*.{domain} points at {}, a private address nobody on the internet can reach.",
            ip_list(ips)
        )
    } else {
        format!(
            "*.{domain} points at {}, which is not this control plane or a worker that takes \
             public traffic.",
            ip_list(ips)
        )
    };
    (PreviewDomainStatus::NoWayIn, reason)
}

fn ip_list(ips: &[IpAddr]) -> String {
    ips.iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// A tunnel that could carry the environment's address (D3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TunnelOffer {
    pub connector_id: i32,
    pub name: String,
    /// The hostname the environment would get under its zone.
    pub hostname: String,
}

/// What D5 decides from.
#[derive(Debug, Clone)]
pub struct WarningFacts<'a> {
    pub environment_id: i32,
    /// The environment's label (`environments.subdomain`).
    pub label: &'a str,
    pub project_slug: &'a str,
    pub deployed: bool,
    /// `hosting = external`: Temps does not serve it.
    pub hosted_externally: bool,
    /// A background worker with no port: nobody opens it.
    pub portless: bool,
    pub addresses: &'a [AddressReachability],
    pub preview: PreviewDomainStatus,
    /// The tunnels that could carry it, best first.
    pub tunnels: &'a [TunnelOffer],
    /// Some listener of this install takes public traffic.
    pub public_ingress: bool,
}

/// D5: the warning a successful deploy carries, or `None`.
///
/// Only when reachability is sure nothing reaches the environment: no
/// address is reachable (a reachable private one included), none is being
/// set up, and none is one Temps cannot judge. A custom domain or tunnel
/// host that serves it is a reachable address, so it never warns.
pub fn reachability_warning(facts: &WarningFacts<'_>) -> Option<ReachabilityWarning> {
    if !facts.deployed || facts.hosted_externally || facts.portless {
        return None;
    }
    if facts.preview == PreviewDomainStatus::Usable {
        return None;
    }
    let reason = no_address_reason(facts.addresses)?;
    let mut fixes = Vec::new();
    if let Some(tunnel) = facts.tunnels.first() {
        fixes.push(ReachabilityFix {
            kind: ReachabilityFixKind::TunnelEnvironmentHosts,
            label: format!(
                "Publish it through tunnel '{}' as {}",
                tunnel.name, tunnel.hostname
            ),
            href: Some(INGRESS_SETUP_PATH.to_string()),
            command: None,
            connector_id: Some(tunnel.connector_id),
            hostname: Some(tunnel.hostname.clone()),
        });
    }
    fixes.push(ReachabilityFix {
        kind: ReachabilityFixKind::PreviewDomain,
        label: "Set a wildcard preview domain in Settings → Platform".to_string(),
        href: Some(PREVIEW_DOMAIN_SETTINGS_PATH.to_string()),
        command: None,
        connector_id: None,
        hostname: None,
    });
    fixes.push(ReachabilityFix {
        kind: ReachabilityFixKind::CustomDomain,
        label: "Add a domain to this environment".to_string(),
        href: Some(format!("/projects/{}/settings/domains", facts.project_slug)),
        command: None,
        connector_id: None,
        hostname: None,
    });
    if !facts.public_ingress {
        fixes.push(ReachabilityFix {
            kind: ReachabilityFixKind::WorkerIngress,
            label: "Let a worker take public traffic".to_string(),
            href: Some(INGRESS_SETUP_PATH.to_string()),
            command: Some(WORKER_INGRESS_COMMAND.to_string()),
            connector_id: None,
            hostname: None,
        });
    }
    Some(ReachabilityWarning {
        environment_id: facts.environment_id,
        environment: facts.label.to_string(),
        headline: format!("Deployed, but nobody can reach {} yet", facts.label),
        reason,
        fixes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reachability::compute::{
        address_reachability, AddressFacts, AddressInput, TunnelState,
    };
    use crate::reachability::model::ReachAction;

    fn address(
        host: &str,
        source: AddressSource,
        audience: Audience,
        state: ReachState,
    ) -> AddressReachability {
        AddressReachability {
            address: host.into(),
            audience,
            source,
            custom_domain_id: None,
            way_in: (state == ReachState::Reachable).then(|| "direct:control-plane".into()),
            state,
            reason: format!("{host} reason"),
            action: None::<ReachAction>,
            network: None,
            nothing_set_up: state == ReachState::NotReachable,
        }
    }

    fn public(host: &str, source: AddressSource, state: ReachState) -> AddressReachability {
        address(host, source, Audience::Public, state)
    }

    fn ip(value: &str) -> IpAddr {
        value.parse().expect("test IP")
    }

    #[test]
    fn the_primary_address_follows_d4s_order() {
        use AddressSource::*;
        use ReachState::*;
        let private = address(
            "app.tail.ts.net",
            CustomDomain,
            Audience::Private,
            Reachable,
        );
        let generated = public("app-production.example.dev", Generated, Reachable);
        let tunnel = public("app-production.example.com", TunnelHost, Reachable);
        let custom = public("app.example.org", CustomDomain, Reachable);
        let bound = public("www.example.org", EnvironmentDomain, Reachable);

        let all = [
            private.clone(),
            generated.clone(),
            tunnel.clone(),
            custom.clone(),
        ];
        let primary = primary_address(&all).expect("primary");
        assert_eq!(primary.address, "app.example.org");
        assert_eq!(primary.kind, AddressKind::CustomDomain);

        let primary = primary_address(&[private.clone(), generated.clone(), tunnel.clone()])
            .expect("primary");
        assert_eq!(primary.kind, AddressKind::TunnelHost);
        let primary = primary_address(&[private.clone(), generated.clone()]).expect("primary");
        assert_eq!(primary.kind, AddressKind::Generated);
        let primary = primary_address(std::slice::from_ref(&private)).expect("primary");
        assert_eq!(primary.kind, AddressKind::Private);

        // A hostname the user bound ranks with custom domains; the first
        // gathered wins among equals.
        let primary = primary_address(&[bound, custom]).expect("primary");
        assert_eq!(primary.address, "www.example.org");

        // An unreachable custom domain never wins over a reachable host.
        let down = public("down.example.org", CustomDomain, NotReachable);
        let primary = primary_address(&[down, generated.clone()]).expect("primary");
        assert_eq!(primary.address, "app-production.example.dev");

        // A tailnet name for listed people only is a private address too
        // (ADR 065 D6): it never outranks a public one.
        let tailnet_users = address(
            "app.tail.ts.net",
            EnvironmentDomain,
            Audience::TailnetUsers,
            Reachable,
        );
        let primary = primary_address(&[tailnet_users.clone(), generated]).expect("primary");
        assert_eq!(primary.address, "app-production.example.dev");
        let primary = primary_address(&[tailnet_users]).expect("primary");
        assert_eq!(primary.kind, AddressKind::Private);
    }

    #[test]
    fn no_address_only_when_nothing_is_reachable_or_pending() {
        use AddressSource::*;
        use ReachState::*;
        let local = public("app-production.localho.st", Generated, NotReachable);
        let custom = public("app.example.org", CustomDomain, NotReachable);
        assert_eq!(
            no_address_reason(&[local.clone(), custom.clone()]).as_deref(),
            Some("app.example.org reason"),
            "the reason is about the address visitors would try first"
        );
        assert_eq!(primary_address(std::slice::from_ref(&local)), None);
        for state in [Checking, Unknown, Reachable] {
            let other = public("app.example.org", CustomDomain, state);
            assert_eq!(
                no_address_reason(&[local.clone(), other]),
                None,
                "{state:?}"
            );
        }
        assert!(no_address_reason(&[]).is_some());
    }

    fn judged(
        host: &str,
        source: AddressSource,
        tunnel: Option<TunnelState>,
        resolution: Resolution,
        directs: &[DirectIngress],
    ) -> AddressReachability {
        let facts = AddressFacts {
            input: AddressInput {
                address: host.into(),
                audience: Audience::Public,
                source,
                custom_domain_id: None,
                private: None,
            },
            tunnel,
            resolution,
            temps_managed_dns: false,
            private: None,
        };
        address_reachability(&facts, directs, true, "production")
    }

    #[test]
    fn a_way_in_that_broke_is_an_outage_not_no_address() {
        // A tunnel host whose tunnel is in error.
        let tunnel = judged(
            "app-production.example.com",
            AddressSource::TunnelHost,
            Some(TunnelState {
                state: "error".into(),
                connector_name: Some("edge".into()),
                detail: "The tunnel rejected the hostname.".into(),
                has_candidates: true,
            }),
            Resolution::NoRecord,
            &[],
        );
        assert_eq!(tunnel.state, ReachState::NotReachable);
        assert!(!tunnel.nothing_set_up);
        let local = judged(
            "app-production.localho.st",
            AddressSource::Generated,
            None,
            Resolution::Skipped,
            &[],
        );
        assert!(local.nothing_set_up);
        assert_eq!(no_address_reason(&[local.clone(), tunnel.clone()]), None);
        assert!(reachability_warning(&warning_facts(&[local.clone(), tunnel], &[])).is_none());

        // DNS points at a worker that failed its last check.
        let mut failed = direct(&["203.0.113.10"]);
        failed.check_failed = Some("connection refused".into());
        let direct = judged(
            "app-production.example.dev",
            AddressSource::Generated,
            None,
            Resolution::Ips(vec![ip("203.0.113.10")]),
            &[failed],
        );
        assert_eq!(direct.state, ReachState::NotReachable);
        assert!(!direct.nothing_set_up);
        assert_eq!(no_address_reason(std::slice::from_ref(&direct)), None);

        // A custom domain whose record went away.
        let custom = judged(
            "app.example.org",
            AddressSource::CustomDomain,
            None,
            Resolution::NoRecord,
            &[],
        );
        assert!(!custom.nothing_set_up);
        assert_eq!(no_address_reason(&[local.clone(), custom]), None);

        // Only ways in never set up read as no address: local-only, the
        // generated host without a record or a tunnel covering it, or
        // nothing deployed.
        let unrecorded = judged(
            "app-production.example.dev",
            AddressSource::Generated,
            None,
            Resolution::NoRecord,
            &[],
        );
        assert!(unrecorded.nothing_set_up);
        assert!(no_address_reason(&[local.clone(), unrecorded]).is_some());
        let covered = judged(
            "app-production.example.com",
            AddressSource::Generated,
            Some(TunnelState {
                state: "not_published".into(),
                connector_name: Some("edge".into()),
                detail: "The edge tunnel covers it but does not publish it.".into(),
                has_candidates: true,
            }),
            Resolution::NoRecord,
            &[],
        );
        assert!(!covered.nothing_set_up);
    }

    fn direct(ips: &[&str]) -> DirectIngress {
        DirectIngress {
            name: "control-plane".into(),
            node_id: None,
            ips: ips.iter().map(|i| ip(i)).collect(),
            takes_port_80: true,
            check_failed: None,
        }
    }

    fn status(domain: &str, probe: Resolution, zones: &[String]) -> PreviewDomainStatus {
        preview_domain_status(
            &PreviewDomainFacts {
                preview_domain: domain,
                probe,
                tunnel_zones: zones,
            },
            &[direct(&["203.0.113.10"])],
        )
        .0
    }

    #[test]
    fn the_preview_domain_is_classified() {
        use PreviewDomainStatus::*;
        let resolves = |i: &str| Resolution::Ips(vec![ip(i)]);
        assert_eq!(status("localho.st", resolves("127.0.0.1"), &[]), LocalOnly);
        assert_eq!(status("dev.localhost", Resolution::Skipped, &[]), LocalOnly);
        assert_eq!(status("", Resolution::Skipped, &[]), LocalOnly);
        assert_eq!(
            status("apps.example.com", Resolution::NoRecord, &[]),
            Unresolved
        );
        assert_eq!(
            status(
                "apps.example.com",
                Resolution::Failed("timeout".into()),
                &[]
            ),
            Unresolved
        );
        assert_eq!(
            status("apps.example.com", Resolution::Ips(vec![]), &[]),
            Unresolved
        );
        assert_eq!(
            status("apps.example.com", resolves("203.0.113.10"), &[]),
            Usable
        );
        assert_eq!(
            status("*.apps.example.com", resolves("203.0.113.10"), &[]),
            Usable,
            "a wildcard spelling is the same domain"
        );
        assert_eq!(
            status("apps.example.com", resolves("198.51.100.4"), &[]),
            NoWayIn
        );
        assert_eq!(
            status("apps.example.com", resolves("10.0.0.4"), &[]),
            NoWayIn
        );
        // Resolving under a running tunnel's zone: the tunnel serves it.
        assert_eq!(
            status(
                "apps.example.com",
                resolves("198.51.100.4"),
                &["example.com".into()]
            ),
            Usable
        );
        assert_eq!(
            status(
                "apps.example.com",
                resolves("198.51.100.4"),
                &["other.com".into()]
            ),
            NoWayIn
        );
        // A worker whose last check failed is no way in.
        let mut failed = direct(&["203.0.113.10"]);
        failed.check_failed = Some("timed out".into());
        let (state, _) = preview_domain_status(
            &PreviewDomainFacts {
                preview_domain: "apps.example.com",
                probe: resolves("203.0.113.10"),
                tunnel_zones: &[],
            },
            &[failed],
        );
        assert_eq!(state, NoWayIn);
    }

    fn warning_facts<'a>(
        addresses: &'a [AddressReachability],
        tunnels: &'a [TunnelOffer],
    ) -> WarningFacts<'a> {
        WarningFacts {
            environment_id: 7,
            label: "hello-temps-production",
            project_slug: "hello-temps",
            deployed: true,
            hosted_externally: false,
            portless: false,
            addresses,
            preview: PreviewDomainStatus::LocalOnly,
            tunnels,
            public_ingress: true,
        }
    }

    #[test]
    fn a_deploy_nobody_can_open_warns_with_the_fixes_in_order() {
        let local = [public(
            "hello-temps-production.localho.st",
            AddressSource::Generated,
            ReachState::NotReachable,
        )];
        let tunnels = [TunnelOffer {
            connector_id: 3,
            name: "edge".into(),
            hostname: "hello-temps-production.example.com".into(),
        }];
        let warning = reachability_warning(&warning_facts(&local, &tunnels)).expect("warning");
        assert_eq!(
            warning.headline,
            "Deployed, but nobody can reach hello-temps-production yet"
        );
        assert_eq!(warning.reason, "hello-temps-production.localho.st reason");
        let kinds: Vec<_> = warning.fixes.iter().map(|f| f.kind).collect();
        assert_eq!(
            kinds,
            vec![
                ReachabilityFixKind::TunnelEnvironmentHosts,
                ReachabilityFixKind::PreviewDomain,
                ReachabilityFixKind::CustomDomain,
            ]
        );
        assert_eq!(warning.fixes[0].connector_id, Some(3));
        assert_eq!(
            warning.fixes[2].href.as_deref(),
            Some("/projects/hello-temps/settings/domains")
        );

        // No tunnel, no public ingress: the worker command comes last.
        let mut facts = warning_facts(&local, &[]);
        facts.public_ingress = false;
        let warning = reachability_warning(&facts).expect("warning");
        let last = warning.fixes.last().expect("fix");
        assert_eq!(last.kind, ReachabilityFixKind::WorkerIngress);
        assert_eq!(last.command.as_deref(), Some(WORKER_INGRESS_COMMAND));
        assert_eq!(warning.fixes[0].kind, ReachabilityFixKind::PreviewDomain);
    }

    #[test]
    fn the_warning_has_its_exclusions() {
        let local = [public(
            "hello-temps-production.localho.st",
            AddressSource::Generated,
            ReachState::NotReachable,
        )];
        let base = warning_facts(&local, &[]);
        assert!(reachability_warning(&base).is_some());

        let mut external = base.clone();
        external.hosted_externally = true;
        assert!(
            reachability_warning(&external).is_none(),
            "hosting = external"
        );

        let mut portless = base.clone();
        portless.portless = true;
        assert!(reachability_warning(&portless).is_none(), "portless worker");

        let mut undeployed = base.clone();
        undeployed.deployed = false;
        assert!(reachability_warning(&undeployed).is_none(), "not deployed");

        let mut usable = base.clone();
        usable.preview = PreviewDomainStatus::Usable;
        assert!(reachability_warning(&usable).is_none(), "usable preview");

        for preview in [
            PreviewDomainStatus::Unresolved,
            PreviewDomainStatus::NoWayIn,
        ] {
            let mut other = base.clone();
            other.preview = preview;
            assert!(reachability_warning(&other).is_some(), "{preview:?}");
        }

        // A reachable private address, custom domain or tunnel host serves it.
        for reachable in [
            address(
                "app.tail.ts.net",
                AddressSource::CustomDomain,
                Audience::Private,
                ReachState::Reachable,
            ),
            public(
                "app.example.org",
                AddressSource::CustomDomain,
                ReachState::Reachable,
            ),
            public(
                "hello-temps-production.example.com",
                AddressSource::TunnelHost,
                ReachState::Reachable,
            ),
        ] {
            let addresses = [local[0].clone(), reachable];
            let facts = warning_facts(&addresses, &[]);
            assert!(reachability_warning(&facts).is_none());
        }

        // Being set up, or impossible to tell: no warning yet.
        for state in [ReachState::Checking, ReachState::Unknown] {
            let addresses = [
                local[0].clone(),
                public("app.example.org", AddressSource::CustomDomain, state),
            ];
            let facts = warning_facts(&addresses, &[]);
            assert!(reachability_warning(&facts).is_none(), "{state:?}");
        }
    }
}
