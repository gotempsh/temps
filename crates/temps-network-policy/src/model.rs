// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Stored rows ⇄ the enforcement model in `temps-workload-netpol`.
//!
//! Pure functions, so every conversion is tested without a database.

use std::collections::{BTreeMap, HashMap};

use temps_entities::{network_policy_profiles, network_policy_rules};
use temps_workload::{WorkloadKind, WorkloadRef};
use temps_workload_netpol::{
    Action, Cidr, Direction, EgressPolicy, EgressRule, EgressTarget, HostPattern, L4Protocol,
    Origin, Peer, PortRange, Profile, Rule, WorkloadPolicy,
};

use crate::error::NetworkPolicyError;

/// What a rule points at, as stored in `peer_kind` / `peer_ref`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerSpec {
    /// A managed service by id (`peer_ref` = the id).
    Service(i64),
    /// Any workload (`peer_ref` = `<kind>/<id>`).
    Workload(WorkloadRef),
    /// An address block.
    Cidr(Cidr),
    /// A hostname pattern; egress only, enforced by the egress proxy.
    Host(HostPattern),
}

impl PeerSpec {
    pub fn parse(peer_kind: &str, peer_ref: &str) -> Result<Self, String> {
        match peer_kind {
            "service" => peer_ref
                .parse::<i64>()
                .ok()
                .filter(|id| *id > 0)
                .map(PeerSpec::Service)
                .ok_or_else(|| format!("service peer '{peer_ref}' is not a service id")),
            "workload" => peer_ref
                .parse::<WorkloadRef>()
                .map(PeerSpec::Workload)
                .map_err(|e| format!("workload peer '{peer_ref}': {e}")),
            "cidr" => peer_ref
                .parse::<Cidr>()
                .map(PeerSpec::Cidr)
                .map_err(|e| e.to_string()),
            "host" => HostPattern::parse(peer_ref)
                .map(PeerSpec::Host)
                .map_err(|e| e.to_string()),
            other => Err(format!(
                "peer kind '{other}' is not one of service, workload, cidr, host"
            )),
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            PeerSpec::Service(_) => "service",
            PeerSpec::Workload(_) => "workload",
            PeerSpec::Cidr(_) => "cidr",
            PeerSpec::Host(_) => "host",
        }
    }

    pub fn reference(&self) -> String {
        match self {
            PeerSpec::Service(id) => id.to_string(),
            PeerSpec::Workload(w) => w.to_string(),
            PeerSpec::Cidr(c) => c.to_string(),
            PeerSpec::Host(h) => String::from(h.clone()),
        }
    }
}

pub fn parse_direction(value: &str) -> Result<Direction, String> {
    match value {
        "egress" => Ok(Direction::Egress),
        "ingress" => Ok(Direction::Ingress),
        other => Err(format!("direction '{other}' is not egress or ingress")),
    }
}

pub fn parse_action(value: &str) -> Result<Action, String> {
    match value {
        "allow" => Ok(Action::Allow),
        "deny" => Ok(Action::Deny),
        other => Err(format!("action '{other}' is not allow or deny")),
    }
}

pub fn parse_protocol(value: &str) -> Result<L4Protocol, String> {
    match value {
        "tcp" => Ok(L4Protocol::Tcp),
        "udp" => Ok(L4Protocol::Udp),
        "any" => Ok(L4Protocol::Any),
        other => Err(format!("protocol '{other}' is not tcp, udp or any")),
    }
}

pub fn parse_origin(value: &str) -> Result<Origin, String> {
    match value {
        "link" => Ok(Origin::Link),
        "operator" => Ok(Origin::Operator),
        "system" => Ok(Origin::System),
        other => Err(format!("origin '{other}' is not link, operator or system")),
    }
}

pub fn parse_profile(value: &str) -> Result<Profile, String> {
    match value {
        "open" => Ok(Profile::Open),
        "restricted" => Ok(Profile::Restricted),
        "none" => Ok(Profile::None),
        other => Err(format!("profile '{other}' is not open, restricted or none")),
    }
}

pub fn profile_str(profile: Profile) -> &'static str {
    match profile {
        Profile::Open => "open",
        Profile::Restricted => "restricted",
        Profile::None => "none",
    }
}

pub fn parse_ports(start: Option<i32>, end: Option<i32>) -> Result<Option<PortRange>, String> {
    match (start, end) {
        (None, None) => Ok(None),
        (Some(start), end) => {
            let end = end.unwrap_or(start);
            let range = u16::try_from(start)
                .ok()
                .zip(u16::try_from(end).ok())
                .map(|(start, end)| PortRange { start, end })
                .filter(PortRange::is_valid)
                .ok_or_else(|| format!("port range {start}-{end} is not within 1-65535"))?;
            Ok(Some(range))
        }
        (None, Some(end)) => Err(format!("port_end {end} without port_start")),
    }
}

/// A kind's default profiles (ADR-052 §4): a sandbox reaches only what a
/// rule allows; every other kind is open, as today.
pub fn default_profiles(kind: WorkloadKind) -> (Profile, Profile) {
    match kind {
        WorkloadKind::Sandbox => (Profile::Restricted, Profile::Open),
        WorkloadKind::Deployment
        | WorkloadKind::Service
        | WorkloadKind::Function
        | WorkloadKind::Job => (Profile::Open, Profile::Open),
    }
}

/// One stored rule, validated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedRule {
    pub id: i64,
    pub subject: WorkloadRef,
    pub direction: Direction,
    pub peer: PeerSpec,
    pub ports: Option<PortRange>,
    pub protocol: L4Protocol,
    pub action: Action,
    pub origin: Origin,
}

pub fn parse_subject(kind: &str, id: i64) -> Result<WorkloadRef, String> {
    let kind = kind
        .parse::<WorkloadKind>()
        .map_err(|e| format!("subject kind '{kind}': {e}"))?;
    if id <= 0 {
        return Err(format!("subject id {id} is not positive"));
    }
    Ok(WorkloadRef::new(kind, id))
}

impl ParsedRule {
    pub fn from_row(row: &network_policy_rules::Model) -> Result<Self, NetworkPolicyError> {
        let invalid = |reason: String| NetworkPolicyError::InvalidStoredRule {
            rule_id: row.id,
            reason,
        };
        let parsed = ParsedRule {
            id: row.id,
            subject: parse_subject(&row.subject_kind, row.subject_id).map_err(invalid)?,
            direction: parse_direction(&row.direction).map_err(invalid)?,
            peer: PeerSpec::parse(&row.peer_kind, &row.peer_ref).map_err(invalid)?,
            ports: parse_ports(row.port_start, row.port_end).map_err(invalid)?,
            protocol: parse_protocol(&row.protocol).map_err(invalid)?,
            action: parse_action(&row.action).map_err(invalid)?,
            origin: parse_origin(&row.origin).map_err(invalid)?,
        };
        if matches!(parsed.peer, PeerSpec::Host(_)) && parsed.direction != Direction::Egress {
            return Err(invalid("a host rule is egress only".to_string()));
        }
        Ok(parsed)
    }

    /// The nftables rule, for every peer except a hostname (which only the
    /// egress proxy can match).
    fn nft_rule(&self) -> Option<Rule> {
        let peer = match &self.peer {
            PeerSpec::Service(id) => Peer::Workload(WorkloadRef::new(WorkloadKind::Service, *id)),
            PeerSpec::Workload(w) => Peer::Workload(*w),
            PeerSpec::Cidr(c) => Peer::Cidr(*c),
            PeerSpec::Host(_) => return None,
        };
        Some(Rule {
            id: self.id,
            direction: self.direction,
            peer,
            ports: self.ports,
            protocol: self.protocol,
            action: self.action,
            origin: self.origin,
        })
    }

    /// The mirrored ingress rule on the peer, for a link: a service that
    /// isolates its ingress still accepts every workload linked to it.
    fn link_ingress(&self) -> Option<(WorkloadRef, Rule)> {
        if self.origin != Origin::Link
            || self.direction != Direction::Egress
            || self.action != Action::Allow
        {
            return None;
        }
        let target = match &self.peer {
            PeerSpec::Service(id) => WorkloadRef::new(WorkloadKind::Service, *id),
            PeerSpec::Workload(w) => *w,
            PeerSpec::Cidr(_) | PeerSpec::Host(_) => return None,
        };
        Some((
            target,
            Rule {
                id: self.id,
                direction: Direction::Ingress,
                peer: Peer::Workload(self.subject),
                ports: self.ports,
                protocol: self.protocol,
                action: Action::Allow,
                origin: Origin::Link,
            },
        ))
    }

    /// The egress proxy's view: hostnames, and address blocks so a private
    /// range an operator allowed is also reachable through the proxy.
    fn egress_rule(&self) -> Option<EgressRule> {
        if self.direction != Direction::Egress {
            return None;
        }
        let target = match &self.peer {
            PeerSpec::Host(h) => EgressTarget::Host(h.clone()),
            PeerSpec::Cidr(c) => EgressTarget::Cidr(*c),
            PeerSpec::Service(_) | PeerSpec::Workload(_) => return None,
        };
        Some(EgressRule {
            id: self.id,
            target,
            ports: self.ports,
            action: self.action,
        })
    }
}

/// A subject's profiles: the stored override, else the kind's default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Profiles {
    pub egress: Profile,
    pub ingress: Profile,
    pub egress_overridden: bool,
    pub ingress_overridden: bool,
}

pub fn profiles_for(
    subject: WorkloadRef,
    stored: Option<&network_policy_profiles::Model>,
) -> Result<Profiles, NetworkPolicyError> {
    let (egress, ingress) = default_profiles(subject.kind);
    let invalid = |reason: String| NetworkPolicyError::InvalidStoredProfile {
        subject: subject.to_string(),
        reason,
    };
    let stored_egress = stored
        .and_then(|p| p.egress.as_deref())
        .map(parse_profile)
        .transpose()
        .map_err(invalid)?;
    let stored_ingress = stored
        .and_then(|p| p.ingress.as_deref())
        .map(parse_profile)
        .transpose()
        .map_err(invalid)?;
    Ok(Profiles {
        egress: stored_egress.unwrap_or(egress),
        ingress: stored_ingress.unwrap_or(ingress),
        egress_overridden: stored_egress.is_some(),
        ingress_overridden: stored_ingress.is_some(),
    })
}

/// Every subject's enforcement policy and proxy rules.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Compiled {
    pub policies: Vec<WorkloadPolicy>,
    pub egress: Vec<EgressPolicy>,
}

/// Compiles stored rules and profiles. `subjects` adds workloads that have
/// neither (a sandbox is restricted with no rules at all).
pub fn compile_policies(
    rules: &[ParsedRule],
    profiles: &[network_policy_profiles::Model],
    subjects: impl IntoIterator<Item = WorkloadRef>,
) -> Result<Compiled, NetworkPolicyError> {
    let mut stored = HashMap::new();
    for profile in profiles {
        let subject =
            parse_subject(&profile.subject_kind, profile.subject_id).map_err(|reason| {
                NetworkPolicyError::InvalidStoredProfile {
                    subject: format!("{}/{}", profile.subject_kind, profile.subject_id),
                    reason,
                }
            })?;
        stored.insert(subject, profile);
    }
    let mut nft: BTreeMap<WorkloadRef, Vec<Rule>> = BTreeMap::new();
    let mut proxy: BTreeMap<WorkloadRef, Vec<EgressRule>> = BTreeMap::new();
    for subject in subjects.into_iter().chain(stored.keys().copied()) {
        nft.entry(subject).or_default();
    }
    for rule in rules {
        let entry = nft.entry(rule.subject).or_default();
        if let Some(r) = rule.nft_rule() {
            entry.push(r);
        }
        if let Some((target, mirrored)) = rule.link_ingress() {
            nft.entry(target).or_default().push(mirrored);
        }
        if let Some(r) = rule.egress_rule() {
            proxy.entry(rule.subject).or_default().push(r);
        }
    }
    let mut compiled = Compiled::default();
    for (subject, rules) in nft {
        let profiles = profiles_for(subject, stored.get(&subject).copied())?;
        // Under an open profile an allow changes nothing (only a deny
        // does), so it is not sent: an open deployment with links stays a
        // no-op instead of growing a table per link.
        let rules = rules
            .into_iter()
            .filter(|r| {
                let profile = match r.direction {
                    Direction::Egress => profiles.egress,
                    Direction::Ingress => profiles.ingress,
                };
                r.action == Action::Deny || profile != Profile::Open
            })
            .collect();
        let policy = WorkloadPolicy {
            subject,
            egress: profiles.egress,
            ingress: profiles.ingress,
            rules,
        };
        if !policy.is_noop() {
            compiled.policies.push(policy);
        }
    }
    compiled.egress = proxy
        .into_iter()
        .map(|(subject, rules)| EgressPolicy { subject, rules })
        .collect();
    Ok(compiled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn row(
        id: i64,
        subject: (&str, i64),
        peer: (&str, &str),
        origin: &str,
    ) -> network_policy_rules::Model {
        network_policy_rules::Model {
            id,
            subject_kind: subject.0.into(),
            subject_id: subject.1,
            direction: "egress".into(),
            peer_kind: peer.0.into(),
            peer_ref: peer.1.into(),
            port_start: None,
            port_end: None,
            protocol: "any".into(),
            action: "allow".into(),
            origin: origin.into(),
            link_table: (origin == "link").then(|| "sandbox_services".to_string()),
            link_id: (origin == "link").then_some(1),
            description: None,
            created_by: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn sandbox(id: i64) -> WorkloadRef {
        WorkloadRef::new(WorkloadKind::Sandbox, id)
    }

    fn service(id: i64) -> WorkloadRef {
        WorkloadRef::new(WorkloadKind::Service, id)
    }

    #[test]
    fn peers_parse_and_print_back() {
        for (kind, reference) in [
            ("service", "12"),
            ("workload", "sandbox/3"),
            ("cidr", "10.0.0.0/8"),
            ("host", "*.example.test"),
        ] {
            let peer = PeerSpec::parse(kind, reference).unwrap();
            assert_eq!((peer.kind(), peer.reference().as_str()), (kind, reference));
        }
        assert!(PeerSpec::parse("service", "x").is_err());
        assert!(PeerSpec::parse("service", "0").is_err());
        assert!(PeerSpec::parse("cidr", "10.0.0.1/8")
            .unwrap_err()
            .contains("host bits"));
        assert!(PeerSpec::parse("host", "a b").is_err());
        assert!(PeerSpec::parse("node", "1").is_err());
    }

    #[test]
    fn ports_are_validated() {
        assert_eq!(parse_ports(None, None), Ok(None));
        assert_eq!(parse_ports(Some(80), None), Ok(Some(PortRange::single(80))));
        assert_eq!(
            parse_ports(Some(8000), Some(8080)),
            Ok(Some(PortRange {
                start: 8000,
                end: 8080
            }))
        );
        assert!(parse_ports(Some(0), None).is_err());
        assert!(parse_ports(Some(90), Some(80)).is_err());
        assert!(parse_ports(Some(70000), None).is_err());
        assert!(parse_ports(None, Some(80)).is_err());
    }

    #[test]
    fn a_stored_host_rule_must_be_egress() {
        let mut bad = row(4, ("sandbox", 1), ("host", "example.test"), "operator");
        bad.direction = "ingress".into();
        assert!(matches!(
            ParsedRule::from_row(&bad),
            Err(NetworkPolicyError::InvalidStoredRule { rule_id: 4, .. })
        ));
    }

    #[test]
    fn sandboxes_are_restricted_and_everything_else_open_by_default() {
        let compiled = compile_policies(
            &[],
            &[],
            [sandbox(1), WorkloadRef::new(WorkloadKind::Deployment, 2)],
        )
        .unwrap();
        assert_eq!(
            compiled.policies.len(),
            1,
            "an open deployment has nothing to enforce"
        );
        assert_eq!(compiled.policies[0].subject, sandbox(1));
        assert_eq!(compiled.policies[0].egress, Profile::Restricted);
    }

    #[test]
    fn a_link_allows_the_subject_and_is_mirrored_on_the_service() {
        let link = ParsedRule::from_row(&row(7, ("sandbox", 1), ("service", "5"), "link")).unwrap();
        let isolated = network_policy_profiles::Model {
            subject_kind: "service".into(),
            subject_id: 5,
            egress: None,
            ingress: Some("restricted".into()),
            updated_by: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let compiled = compile_policies(&[link], &[isolated], []).unwrap();
        let sandbox_policy = compiled
            .policies
            .iter()
            .find(|p| p.subject == sandbox(1))
            .unwrap();
        assert_eq!(sandbox_policy.rules[0].peer, Peer::Workload(service(5)));
        let service_policy = compiled
            .policies
            .iter()
            .find(|p| p.subject == service(5))
            .unwrap();
        assert_eq!(service_policy.ingress, Profile::Restricted);
        assert_eq!(service_policy.rules[0].direction, Direction::Ingress);
        assert_eq!(service_policy.rules[0].peer, Peer::Workload(sandbox(1)));
    }

    #[test]
    fn hosts_go_to_the_proxy_and_cidrs_to_both() {
        let host = ParsedRule::from_row(&row(
            1,
            ("sandbox", 1),
            ("host", "registry.example.test"),
            "operator",
        ))
        .unwrap();
        let cidr =
            ParsedRule::from_row(&row(2, ("sandbox", 1), ("cidr", "10.1.0.0/16"), "operator"))
                .unwrap();
        let compiled = compile_policies(&[host, cidr], &[], []).unwrap();
        assert_eq!(
            compiled.policies[0].rules.len(),
            1,
            "only the CIDR is an nftables rule"
        );
        assert_eq!(compiled.egress[0].rules.len(), 2, "the proxy sees both");
    }

    #[test]
    fn an_invalid_stored_profile_is_an_error_not_a_default() {
        let bad = network_policy_profiles::Model {
            subject_kind: "sandbox".into(),
            subject_id: 1,
            egress: Some("closed".into()),
            ingress: None,
            updated_by: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        assert!(matches!(
            compile_policies(&[], &[bad], []),
            Err(NetworkPolicyError::InvalidStoredProfile { .. })
        ));
    }
}
