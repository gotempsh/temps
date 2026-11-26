// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A sandbox's own egress policy as the per-node proxy enforces it.
//!
//! A sandbox that records hands its outbound traffic to the egress recorder
//! instead of deciding in its sidecar, so the recorder must decide exactly
//! as the sidecar would:
//!
//! - `internet`: any public destination on ports 80 and 443;
//! - `restricted`: only what a rule names (a host pattern or a CIDR), on the
//!   rule's ports (80 and 443 when it lists none);
//! - in both, never a private, loopback, link-local or metadata address,
//!   even inside a CIDR a rule names (`public_only`).

use temps_sandbox_providers::{EgressMode, SandboxEgressPolicy};
use temps_workload::WorkloadRef;
use temps_workload_netpol::rule::{Action, Cidr, PortRange};
use temps_workload_netpol::{EgressPolicy, EgressRule, EgressTarget, HostPattern};

/// The proxy policy of `subject`, which records. A rule the proxy cannot
/// express (a host pattern or CIDR the API would not have stored) is
/// skipped, which narrows the policy, never widens it.
pub fn proxy_policy(subject: WorkloadRef, policy: &SandboxEgressPolicy) -> EgressPolicy {
    let mut rules = Vec::new();
    let mut next_id = 1i64;
    let mut push = |target: EgressTarget, ports: &[u32]| {
        let ranges: Vec<Option<PortRange>> = if ports.is_empty() {
            vec![None]
        } else {
            ports
                .iter()
                .filter_map(|port| u16::try_from(*port).ok().filter(|p| *p > 0))
                .map(|port| Some(PortRange::single(port)))
                .collect()
        };
        for ports in ranges {
            rules.push(EgressRule {
                id: next_id,
                target: target.clone(),
                ports,
                action: Action::Allow,
            });
            next_id += 1;
        }
    };
    match policy.mode {
        EgressMode::Internet => {
            if let Ok(any) = HostPattern::parse("*") {
                push(EgressTarget::Host(any), &[]);
            }
        }
        EgressMode::Restricted => {
            for rule in &policy.rules {
                if let Some(host) = &rule.host {
                    if let Ok(pattern) = HostPattern::parse(host) {
                        push(EgressTarget::Host(pattern), &rule.ports);
                    }
                } else if let Some(cidr) = rule.cidr.as_deref().and_then(parse_cidr) {
                    push(EgressTarget::Cidr(cidr), &rule.ports);
                }
            }
        }
    }
    EgressPolicy {
        subject,
        rules,
        record: true,
        public_only: true,
    }
}

/// A CIDR, or a single address as a host CIDR.
fn parse_cidr(value: &str) -> Option<Cidr> {
    if value.contains('/') {
        return value.parse().ok();
    }
    value.parse::<std::net::IpAddr>().ok().map(Cidr::host)
}

/// What a sidecar still handing its traffic to the recorder is held to once
/// its sandbox no longer records (an opt-out the sidecar has not followed
/// yet): any public destination, on any port, never recorded. The sidecar
/// applies its sandbox's own policy before handing anything over, so this
/// widens nothing; it only keeps the sandbox's way out until the sidecar is
/// re-pointed.
pub fn stray_policy(subject: WorkloadRef) -> EgressPolicy {
    let rules = match HostPattern::parse("*") {
        Ok(any) => vec![EgressRule {
            id: 0,
            target: EgressTarget::Host(any),
            ports: Some(PortRange {
                start: 1,
                end: u16::MAX,
            }),
            action: Action::Allow,
        }],
        Err(_) => Vec::new(),
    };
    EgressPolicy {
        subject,
        rules,
        record: false,
        public_only: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::IpAddr;
    use temps_sandbox_providers::EgressRule as SandboxEgressRule;
    use temps_workload::WorkloadKind;
    use temps_workload_netpol::egress::{Admission, CompiledRules, Refusal};

    fn sandbox() -> WorkloadRef {
        WorkloadRef::new(WorkloadKind::Sandbox, 1)
    }

    fn compiled(policy: SandboxEgressPolicy) -> CompiledRules {
        CompiledRules::new(&proxy_policy(sandbox(), &policy))
    }

    fn ip(value: &str) -> IpAddr {
        value.parse().unwrap()
    }

    /// What the sidecar decides for `host:port` resolving to `addresses`,
    /// as the proxy decides it.
    fn decide(
        rules: &CompiledRules,
        host: &str,
        port: u16,
        addresses: &[&str],
    ) -> Result<(), Refusal> {
        let admission = rules.check_host(host, port)?;
        let addresses: Vec<IpAddr> = addresses.iter().map(|a| ip(a)).collect();
        rules.check_addresses(&addresses, port, admission)
    }

    #[test]
    fn internet_reaches_any_public_host_on_the_web_ports_only() {
        let rules = compiled(SandboxEgressPolicy::internet());
        assert!(rules.record && rules.public_only);
        assert_eq!(
            decide(&rules, "api.example.com", 443, &["93.184.216.34"]),
            Ok(())
        );
        assert_eq!(
            decide(&rules, "api.example.com", 80, &["93.184.216.34"]),
            Ok(())
        );
        assert_eq!(
            decide(&rules, "api.example.com", 22, &["93.184.216.34"]),
            Err(Refusal::Port)
        );
        assert_eq!(
            decide(&rules, "internal.example.com", 443, &["10.0.0.5"]),
            Err(Refusal::Private)
        );
        assert_eq!(
            decide(&rules, "93.184.216.34", 443, &["93.184.216.34"]),
            Ok(())
        );
    }

    #[test]
    fn restricted_reaches_only_what_a_rule_names_on_its_ports() {
        let rules = compiled(SandboxEgressPolicy {
            mode: EgressMode::Restricted,
            rules: vec![
                SandboxEgressRule::host("api.example.com", &[443]),
                SandboxEgressRule::host("*.example.org", &[]),
                SandboxEgressRule::cidr("203.0.113.0/24", &[8443, 9443]),
                SandboxEgressRule::cidr("198.51.100.7", &[443]),
                SandboxEgressRule::cidr("93.184.216.0/24", &[8443]),
            ],
        });
        assert_eq!(
            decide(&rules, "svc.example.net", 8443, &["93.184.216.34"]),
            Ok(()),
            "a name resolving into a named public CIDR, on its port"
        );
        assert_eq!(
            decide(&rules, "svc.example.net", 443, &["93.184.216.34"]),
            Err(Refusal::NotAllowed)
        );
        assert_eq!(
            decide(&rules, "api.example.com", 443, &["93.184.216.34"]),
            Ok(())
        );
        assert_eq!(
            decide(&rules, "api.example.com", 80, &["93.184.216.34"]),
            Err(Refusal::Port),
            "the rule lists only 443"
        );
        assert_eq!(
            decide(&rules, "a.example.org", 80, &["93.184.216.34"]),
            Ok(())
        );
        assert_eq!(
            decide(&rules, "example.org", 443, &["93.184.216.34"]),
            Err(Refusal::NotAllowed),
            "a subdomain pattern does not match the apex"
        );
        assert_eq!(
            decide(&rules, "other.example.net", 443, &["93.184.216.34"]),
            Err(Refusal::NotAllowed)
        );
        // A name resolving into a named CIDR, on one of its ports.
        let documentation = "203.0.113.9";
        assert!(!temps_workload_netpol::egress::is_public(ip(documentation)));
        assert_eq!(
            decide(&rules, "svc.example.net", 9443, &[documentation]),
            Err(Refusal::Private),
            "a documentation range is not public: refused even though a rule names it"
        );
        assert_eq!(
            decide(&rules, "198.51.100.7", 443, &["198.51.100.7"]),
            Err(Refusal::Private)
        );
        assert_eq!(rules.check_host("198.51.100.7", 443), Ok(Admission::ByRule));
    }

    #[test]
    fn a_rule_naming_a_private_range_never_opens_it() {
        let rules = compiled(SandboxEgressPolicy {
            mode: EgressMode::Restricted,
            rules: vec![SandboxEgressRule::cidr("10.0.0.0/8", &[443])],
        });
        assert_eq!(
            decide(&rules, "10.1.2.3", 443, &["10.1.2.3"]),
            Err(Refusal::Private)
        );
    }

    #[test]
    fn restricted_without_rules_reaches_nothing() {
        let rules = compiled(SandboxEgressPolicy {
            mode: EgressMode::Restricted,
            rules: vec![],
        });
        assert_eq!(
            decide(&rules, "api.example.com", 443, &["93.184.216.34"]),
            Err(Refusal::NotAllowed)
        );
    }

    #[test]
    fn single_addresses_become_host_cidrs() {
        assert_eq!(parse_cidr("198.51.100.7"), "198.51.100.7/32".parse().ok());
        assert_eq!(parse_cidr("2606:4700::1"), "2606:4700::1/128".parse().ok());
        assert_eq!(parse_cidr("not an address"), None);
    }
}
