// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A sandbox's outbound (egress) policy: which destinations the code inside
//! it may open connections to.
//!
//! The policy is declared by whoever owns the sandbox, stored on its row and
//! enforced by the driver (on Docker, by the per-sandbox egress proxy, the
//! sandbox's only route out). Two modes:
//!
//! - [`EgressMode::Internet`] (the default): any public destination on the
//!   web ports 80 and 443;
//! - [`EgressMode::Restricted`]: only destinations an [`EgressRule`] names,
//!   on the ports that rule lists.
//!
//! In both modes private, loopback, link-local, carrier-grade NAT,
//! documentation, multicast and metadata addresses are always refused, even
//! when a rule names them, and the Temps relays (model, MCP, git) keep
//! working: they are reached through the proxy itself, not as an outbound
//! destination. So `restricted` with no rules means "only the Temps relays".
//!
//! The matcher here is the reference for the proxy's JavaScript one; a
//! parity test runs the same cases through both.

use std::collections::HashMap;
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use serde::{Deserialize, Serialize};

/// The most rules one policy may hold. The proxy evaluates rules per
/// connection, so the bound keeps that cost and the stored row small.
pub const MAX_EGRESS_RULES: usize = 100;

/// The most ports one rule may list. With [`MAX_EGRESS_RULES`] this bounds
/// the serialized policy (the proxy receives it as one environment value,
/// which Linux caps at 128 KiB) and the per-connection matching cost; a
/// rule needing more ports is better split by destination.
pub const MAX_EGRESS_PORTS_PER_RULE: usize = 32;

/// Ports a rule permits when it lists none, and the only ports
/// [`EgressMode::Internet`] permits.
pub const DEFAULT_EGRESS_PORTS: [u16; 2] = [80, 443];

/// How a sandbox may reach the outside world.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum EgressMode {
    /// Any public destination on ports 80 and 443.
    #[default]
    Internet,
    /// Only the destinations the rules name.
    Restricted,
}

impl EgressMode {
    pub fn as_str(self) -> &'static str {
        match self {
            EgressMode::Internet => "internet",
            EgressMode::Restricted => "restricted",
        }
    }
}

impl fmt::Display for EgressMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One allowed destination. Exactly one of `host` or `cidr` is set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct EgressRule {
    /// A hostname, matched case-insensitively against the name the sandbox
    /// asked for (`api.example.com`). A leading `*.` matches subdomains only:
    /// `*.example.com` matches `a.example.com` and `a.b.example.com`, never
    /// `example.com` itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(example = "api.example.com"))]
    pub host: Option<String>,
    /// An IPv4 or IPv6 address or CIDR block (`198.51.100.7`,
    /// `2606:4700::/32`), matched against a literal IP the sandbox asked for
    /// or the address a hostname resolved to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(example = "1.1.1.0/24"))]
    pub cidr: Option<String>,
    /// Destination ports this rule permits, 1-65535. Empty means 80 and 443.
    #[serde(default)]
    pub ports: Vec<u32>,
}

impl EgressRule {
    pub fn host(host: impl Into<String>, ports: &[u32]) -> Self {
        Self {
            host: Some(host.into()),
            cidr: None,
            ports: ports.to_vec(),
        }
    }

    pub fn cidr(cidr: impl Into<String>, ports: &[u32]) -> Self {
        Self {
            host: None,
            cidr: Some(cidr.into()),
            ports: ports.to_vec(),
        }
    }

    /// What the rule names, for messages.
    pub fn target(&self) -> &str {
        self.host
            .as_deref()
            .or(self.cidr.as_deref())
            .unwrap_or("<empty rule>")
    }
}

/// A sandbox's egress policy. Build it from untrusted input with
/// [`SandboxEgressPolicy::normalized`], which validates it and puts it in its
/// canonical form; only a normalized policy is stored or enforced.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct SandboxEgressPolicy {
    #[serde(default)]
    pub mode: EgressMode,
    /// Allowed destinations, in `restricted` mode. At most 100.
    #[serde(default)]
    pub rules: Vec<EgressRule>,
}

/// Why a policy was refused. Every variant names the offending value.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EgressPolicyError {
    #[error(
        "egress mode 'internet' takes no rules, but {count} were given; \
         use mode 'restricted' to allow only the listed destinations"
    )]
    RulesWithoutRestriction { count: usize },

    #[error("egress policy has {count} rules; at most {max} are allowed")]
    TooManyRules { count: usize, max: usize },

    #[error("egress rule {index}: give exactly one of 'host' or 'cidr' (got {given})")]
    RuleTarget { index: usize, given: &'static str },

    #[error("egress rule {index}: host '{host}' is not a valid hostname: {reason}")]
    InvalidHost {
        index: usize,
        host: String,
        reason: String,
    },

    #[error(
        "egress rule {index}: cidr '{cidr}' is not a valid IP address or CIDR block: {reason}"
    )]
    InvalidCidr {
        index: usize,
        cidr: String,
        reason: String,
    },

    #[error(
        "egress rule {index}: cidr '{cidr}' lies entirely inside {range} ({kind}), \
         which sandboxes can never reach"
    )]
    UnreachableCidr {
        index: usize,
        cidr: String,
        range: String,
        kind: &'static str,
    },

    #[error("egress rule {index} ('{target}'): port {port} is outside 1-65535")]
    InvalidPort {
        index: usize,
        target: String,
        port: u32,
    },

    #[error(
        "egress rule {index} ('{target}') lists {count} ports; at most {max} are allowed per rule"
    )]
    TooManyPorts {
        index: usize,
        target: String,
        count: usize,
        max: usize,
    },

    #[error("egress rule {index} ('{target}'): port {port} is listed twice")]
    DuplicatePort {
        index: usize,
        target: String,
        port: u32,
    },

    #[error("egress rule {index}: '{target}' repeats rule {first}; merge their ports instead")]
    DuplicateRule {
        index: usize,
        first: usize,
        target: String,
    },
}

/// Why the matcher refused a destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EgressDenial {
    /// The destination is private, loopback, link-local, metadata or
    /// otherwise not on the public internet. Refused in every mode.
    Private,
    /// The policy does not allow this destination and port.
    Policy,
}

impl EgressDenial {
    pub fn as_str(self) -> &'static str {
        match self {
            EgressDenial::Private => "private",
            EgressDenial::Policy => "policy",
        }
    }
}

impl SandboxEgressPolicy {
    /// The default: public destinations on ports 80 and 443.
    pub fn internet() -> Self {
        Self::default()
    }

    /// Only the given destinations (plus the Temps relays).
    pub fn restricted(rules: Vec<EgressRule>) -> Self {
        Self {
            mode: EgressMode::Restricted,
            rules,
        }
    }

    pub fn is_restricted(&self) -> bool {
        self.mode == EgressMode::Restricted
    }

    /// Validate the policy and return its canonical form: hosts lowercased
    /// without a trailing dot, CIDR blocks reduced to their network address
    /// (a bare IP becomes a `/32` or `/128`), and every rule's ports filled
    /// in and sorted. Rule order is kept. Normalizing a normalized policy is
    /// a no-op, so the canonical form is safe to compare and hash.
    pub fn normalized(&self) -> Result<Self, EgressPolicyError> {
        if self.mode == EgressMode::Internet && !self.rules.is_empty() {
            return Err(EgressPolicyError::RulesWithoutRestriction {
                count: self.rules.len(),
            });
        }
        if self.rules.len() > MAX_EGRESS_RULES {
            return Err(EgressPolicyError::TooManyRules {
                count: self.rules.len(),
                max: MAX_EGRESS_RULES,
            });
        }
        let mut seen: HashMap<String, usize> = HashMap::new();
        let mut rules = Vec::with_capacity(self.rules.len());
        for (index, rule) in self.rules.iter().enumerate() {
            let rule = normalize_rule(index, rule)?;
            let key = rule.target().to_string();
            if let Some(first) = seen.get(&key) {
                return Err(EgressPolicyError::DuplicateRule {
                    index,
                    first: *first,
                    target: key,
                });
            }
            seen.insert(key, index);
            rules.push(rule);
        }
        Ok(Self {
            mode: self.mode,
            rules,
        })
    }

    /// Whether a connection to `host` (the name or IP literal the sandbox
    /// asked for), resolved to `address`, on `port` is allowed. Mirrors the
    /// egress proxy's `egressDecision`. `self` must be normalized.
    pub fn decide(&self, host: &str, address: IpAddr, port: u16) -> Result<(), EgressDenial> {
        if never_reachable(address).is_some() {
            return Err(EgressDenial::Private);
        }
        match self.mode {
            EgressMode::Internet => {
                if DEFAULT_EGRESS_PORTS.contains(&port) {
                    Ok(())
                } else {
                    Err(EgressDenial::Policy)
                }
            }
            EgressMode::Restricted => {
                let host = normalize_requested_host(host);
                let literal = host.parse::<IpAddr>().is_ok();
                let address = canonical_address(address);
                let allowed = self.rules.iter().any(|rule| {
                    if !rule.ports.contains(&u32::from(port)) {
                        return false;
                    }
                    match (rule.host.as_deref(), rule.cidr.as_deref()) {
                        (Some(pattern), _) => !literal && host_matches(pattern, &host),
                        (None, Some(cidr)) => {
                            parse_cidr(cidr).is_ok_and(|block| block.contains(address))
                        }
                        (None, None) => false,
                    }
                });
                if allowed {
                    Ok(())
                } else {
                    Err(EgressDenial::Policy)
                }
            }
        }
    }
}

fn normalize_rule(index: usize, rule: &EgressRule) -> Result<EgressRule, EgressPolicyError> {
    let (host, cidr) = match (rule.host.as_deref(), rule.cidr.as_deref()) {
        (Some(host), None) => (Some(normalize_host(index, host)?), None),
        (None, Some(cidr)) => (None, Some(normalize_cidr(index, cidr)?)),
        (Some(_), Some(_)) => {
            return Err(EgressPolicyError::RuleTarget {
                index,
                given: "both",
            })
        }
        (None, None) => {
            return Err(EgressPolicyError::RuleTarget {
                index,
                given: "neither",
            })
        }
    };
    let target = host.as_deref().or(cidr.as_deref()).unwrap_or_default();
    if rule.ports.len() > MAX_EGRESS_PORTS_PER_RULE {
        return Err(EgressPolicyError::TooManyPorts {
            index,
            target: target.to_string(),
            count: rule.ports.len(),
            max: MAX_EGRESS_PORTS_PER_RULE,
        });
    }
    let mut ports = Vec::with_capacity(rule.ports.len().max(2));
    for &port in &rule.ports {
        if port == 0 || port > u32::from(u16::MAX) {
            return Err(EgressPolicyError::InvalidPort {
                index,
                target: target.to_string(),
                port,
            });
        }
        if ports.contains(&port) {
            return Err(EgressPolicyError::DuplicatePort {
                index,
                target: target.to_string(),
                port,
            });
        }
        ports.push(port);
    }
    if ports.is_empty() {
        ports.extend(DEFAULT_EGRESS_PORTS.iter().map(|p| u32::from(*p)));
    }
    ports.sort_unstable();
    Ok(EgressRule { host, cidr, ports })
}

fn normalize_host(index: usize, raw: &str) -> Result<String, EgressPolicyError> {
    let invalid = |reason: &str| EgressPolicyError::InvalidHost {
        index,
        host: raw.to_string(),
        reason: reason.to_string(),
    };
    let lowered = raw.trim().to_ascii_lowercase();
    let host = lowered.strip_suffix('.').unwrap_or(&lowered);
    if host.is_empty() {
        return Err(invalid("it is empty"));
    }
    if !host.is_ascii() {
        return Err(invalid(
            "it must be ASCII; write an internationalized name in its punycode (xn--) form",
        ));
    }
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    if bare.parse::<IpAddr>().is_ok() {
        return Err(invalid(
            "it is an IP address; use a 'cidr' rule for addresses",
        ));
    }
    let (wildcard, name) = match host.strip_prefix("*.") {
        Some(rest) => (true, rest),
        None => (false, host),
    };
    if name.contains('*') {
        return Err(invalid(
            "a wildcard is only allowed as a leading '*.' (as in '*.example.com')",
        ));
    }
    if name.len() > 253 {
        return Err(invalid("it is longer than 253 characters"));
    }
    for label in name.split('.') {
        if label.is_empty() {
            return Err(invalid("it has an empty label"));
        }
        if label.len() > 63 {
            return Err(invalid("a label is longer than 63 characters"));
        }
        if !label
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err(invalid(
                "labels may only contain letters, digits and hyphens",
            ));
        }
        if label.starts_with('-') || label.ends_with('-') {
            return Err(invalid("a label starts or ends with a hyphen"));
        }
    }
    if name
        .split('.')
        .next_back()
        .is_some_and(|tld| tld.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err(invalid(
            "its last label is numeric, which no public name has",
        ));
    }
    Ok(if wildcard {
        format!("*.{name}")
    } else {
        name.to_string()
    })
}

fn normalize_cidr(index: usize, raw: &str) -> Result<String, EgressPolicyError> {
    let block = parse_cidr(raw.trim()).map_err(|reason| EgressPolicyError::InvalidCidr {
        index,
        cidr: raw.to_string(),
        reason,
    })?;
    if let Some((range, kind)) = block.unreachable_container() {
        return Err(EgressPolicyError::UnreachableCidr {
            index,
            cidr: raw.to_string(),
            range,
            kind,
        });
    }
    Ok(block.to_string())
}

/// An IP network: an address with its host bits cleared, and a prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Cidr {
    network: IpAddr,
    prefix: u8,
}

impl fmt::Display for Cidr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.network, self.prefix)
    }
}

impl Cidr {
    fn new(address: IpAddr, prefix: u8) -> Self {
        let network = match address {
            IpAddr::V4(v4) => IpAddr::V4(Ipv4Addr::from(mask_v4(u32::from(v4), prefix))),
            IpAddr::V6(v6) => IpAddr::V6(Ipv6Addr::from(mask_v6(u128::from(v6), prefix))),
        };
        Self { network, prefix }
    }

    fn contains(&self, address: IpAddr) -> bool {
        match (self.network, address) {
            (IpAddr::V4(network), IpAddr::V4(address)) => {
                mask_v4(u32::from(address), self.prefix) == u32::from(network)
            }
            (IpAddr::V6(network), IpAddr::V6(address)) => {
                mask_v6(u128::from(address), self.prefix) == u128::from(network)
            }
            _ => false,
        }
    }

    /// Whether `self` lies entirely inside `outer`.
    fn within(&self, outer: &Cidr) -> bool {
        self.prefix >= outer.prefix && outer.contains(self.network)
    }

    /// The never-reachable range this whole block falls inside, if any.
    fn unreachable_container(&self) -> Option<(String, &'static str)> {
        match self.network {
            IpAddr::V4(_) => NEVER_REACHABLE_V4.iter().find_map(|(addr, prefix, kind)| {
                let outer = Cidr::new(IpAddr::V4(Ipv4Addr::from(*addr)), *prefix);
                self.within(&outer).then(|| (outer.to_string(), *kind))
            }),
            IpAddr::V6(_) => {
                let global = Cidr::new(IpAddr::V6(Ipv6Addr::new(0x2000, 0, 0, 0, 0, 0, 0, 0)), 3);
                if self.prefix >= 3 && !global.contains(self.network) {
                    return Some((
                        "everything outside 2000::/3".to_string(),
                        "not global unicast",
                    ));
                }
                NEVER_REACHABLE_V6.iter().find_map(|(addr, prefix, kind)| {
                    let outer = Cidr::new(IpAddr::V6(*addr), *prefix);
                    self.within(&outer).then(|| (outer.to_string(), *kind))
                })
            }
        }
    }
}

fn mask_v4(value: u32, prefix: u8) -> u32 {
    if prefix == 0 {
        0
    } else {
        value & (u32::MAX << (32 - u32::from(prefix)))
    }
}

fn mask_v6(value: u128, prefix: u8) -> u128 {
    if prefix == 0 {
        0
    } else {
        value & (u128::MAX << (128 - u32::from(prefix)))
    }
}

fn parse_cidr(raw: &str) -> Result<Cidr, String> {
    let (address, prefix) = match raw.split_once('/') {
        Some((address, prefix)) => (address, Some(prefix)),
        None => (raw, None),
    };
    let address = address
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<IpAddr>()
        .map_err(|_| format!("'{address}' is not an IPv4 or IPv6 address"))?;
    let address = canonical_address(address);
    let max = if address.is_ipv4() { 32 } else { 128 };
    let prefix = match prefix {
        None => max,
        Some(prefix) => prefix
            .parse::<u8>()
            .ok()
            .filter(|p| *p <= max)
            .ok_or_else(|| format!("prefix '/{prefix}' must be a number from 0 to {max}"))?,
    };
    Ok(Cidr::new(address, prefix))
}

/// An IPv4-mapped IPv6 address (`::ffff:a.b.c.d`) is its IPv4 address.
fn canonical_address(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6(v6),
        },
        v4 => v4,
    }
}

/// IPv4 ranges a sandbox can never reach, in any mode. Must stay in step
/// with `isPrivateAddress` in the egress proxy script.
const NEVER_REACHABLE_V4: &[(u32, u8, &str)] = &[
    (0x0000_0000, 8, "this network"),
    (0x0A00_0000, 8, "private"),
    (0x6440_0000, 10, "carrier-grade NAT"),
    (0x7F00_0000, 8, "loopback"),
    (0xA9FE_0000, 16, "link-local and cloud metadata"),
    (0xAC10_0000, 12, "private"),
    (0xC000_0000, 24, "IETF protocol assignments"),
    (0xC000_0200, 24, "documentation"),
    (0xC058_6300, 24, "6to4 relay anycast"),
    (0xC0A8_0000, 16, "private"),
    (0xC612_0000, 15, "benchmarking"),
    (0xC633_6400, 24, "documentation"),
    (0xCB00_7100, 24, "documentation"),
    (0xE000_0000, 3, "multicast and reserved"),
];

/// IPv6 ranges inside 2000::/3 a sandbox can never reach. Everything outside
/// 2000::/3 (loopback, link-local, ULA, multicast, mapped) is refused too.
const NEVER_REACHABLE_V6: &[(Ipv6Addr, u8, &str)] = &[
    (
        Ipv6Addr::new(0x2001, 0x0002, 0, 0, 0, 0, 0, 0),
        32,
        "benchmarking",
    ),
    (
        Ipv6Addr::new(0x2001, 0x0db8, 0, 0, 0, 0, 0, 0),
        32,
        "documentation",
    ),
];

/// Why `address` can never be reached from a sandbox, or `None` if it is a
/// public address.
pub fn never_reachable(address: IpAddr) -> Option<&'static str> {
    match canonical_address(address) {
        IpAddr::V4(v4) => {
            let value = u32::from(v4);
            NEVER_REACHABLE_V4
                .iter()
                .find(|(network, prefix, _)| mask_v4(value, *prefix) == *network)
                .map(|(_, _, kind)| *kind)
        }
        IpAddr::V6(v6) => {
            let value = u128::from(v6);
            if value >> 125 != 0b001 {
                return Some("not global unicast");
            }
            NEVER_REACHABLE_V6
                .iter()
                .find(|(network, prefix, _)| mask_v6(value, *prefix) == u128::from(*network))
                .map(|(_, _, kind)| *kind)
        }
    }
}

fn normalize_requested_host(host: &str) -> String {
    let lowered = host.trim().to_ascii_lowercase();
    let bare = lowered.trim_start_matches('[').trim_end_matches(']');
    bare.strip_suffix('.').unwrap_or(bare).to_string()
}

fn host_matches(pattern: &str, host: &str) -> bool {
    match pattern.strip_prefix("*.") {
        Some(suffix) => host
            .strip_suffix(suffix)
            .is_some_and(|prefix| prefix.len() > 1 && prefix.ends_with('.')),
        None => pattern == host,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn restricted(rules: Vec<EgressRule>) -> SandboxEgressPolicy {
        SandboxEgressPolicy::restricted(rules)
            .normalized()
            .expect("valid test policy")
    }

    fn ip(value: &str) -> IpAddr {
        value.parse().expect("test address")
    }

    #[test]
    fn the_default_is_internet_with_no_rules() {
        let policy = SandboxEgressPolicy::default();
        assert_eq!(policy.mode, EgressMode::Internet);
        assert!(policy.rules.is_empty());
        let json = serde_json::to_string(&policy).expect("serialize");
        assert_eq!(json, r#"{"mode":"internet","rules":[]}"#);
        let parsed: SandboxEgressPolicy = serde_json::from_str("{}").expect("empty object");
        assert_eq!(parsed, policy);
    }

    #[test]
    fn the_wire_shape_round_trips() {
        let json = r#"{"mode":"restricted","rules":[
            {"host":"API.Example.COM.","ports":[443]},
            {"host":"*.npmjs.org"},
            {"cidr":"1.1.1.7/24","ports":[8443,22]}
        ]}"#;
        let policy: SandboxEgressPolicy = serde_json::from_str(json).expect("parse");
        let normalized = policy.normalized().expect("valid");
        assert_eq!(
            normalized.rules,
            vec![
                EgressRule::host("api.example.com", &[443]),
                EgressRule::host("*.npmjs.org", &[80, 443]),
                EgressRule::cidr("1.1.1.0/24", &[22, 8443]),
            ]
        );
        assert_eq!(normalized.normalized().expect("idempotent"), normalized);
        let reserialized = serde_json::to_string(&normalized).expect("serialize");
        assert_eq!(
            reserialized,
            r#"{"mode":"restricted","rules":[{"host":"api.example.com","ports":[443]},{"host":"*.npmjs.org","ports":[80,443]},{"cidr":"1.1.1.0/24","ports":[22,8443]}]}"#
        );
    }

    #[test]
    fn unknown_fields_are_refused() {
        let err = serde_json::from_str::<SandboxEgressPolicy>(
            r#"{"mode":"restricted","rules":[{"hostname":"a.example.com"}]}"#,
        )
        .expect_err("unknown field");
        assert!(err.to_string().contains("hostname"), "{err}");
        assert!(serde_json::from_str::<SandboxEgressPolicy>(r#"{"mode":"open"}"#).is_err());
    }

    #[test]
    fn a_bare_ip_becomes_a_single_address_block() {
        let policy = restricted(vec![
            EgressRule::cidr("1.1.1.1", &[]),
            EgressRule::cidr("2606:4700:4700::1111", &[]),
            EgressRule::cidr("::ffff:8.8.8.8", &[]),
        ]);
        let targets: Vec<&str> = policy.rules.iter().map(EgressRule::target).collect();
        assert_eq!(
            targets,
            vec!["1.1.1.1/32", "2606:4700:4700::1111/128", "8.8.8.8/32"]
        );
    }

    #[test]
    fn internet_mode_takes_no_rules() {
        let err = SandboxEgressPolicy {
            mode: EgressMode::Internet,
            rules: vec![EgressRule::host("a.example.com", &[])],
        }
        .normalized()
        .expect_err("rules in internet mode");
        assert_eq!(err, EgressPolicyError::RulesWithoutRestriction { count: 1 });
        assert!(err.to_string().contains("restricted"));
    }

    #[test]
    fn restricted_with_no_rules_is_valid() {
        let policy = restricted(vec![]);
        assert!(policy.is_restricted());
        assert_eq!(
            policy.decide("a.example.com", ip("1.1.1.1"), 443),
            Err(EgressDenial::Policy)
        );
    }

    #[test]
    fn more_than_one_hundred_rules_are_refused() {
        let rules: Vec<EgressRule> = (0..=MAX_EGRESS_RULES)
            .map(|i| EgressRule::host(format!("h{i}.example.com"), &[]))
            .collect();
        let err = SandboxEgressPolicy::restricted(rules)
            .normalized()
            .expect_err("too many");
        assert_eq!(
            err,
            EgressPolicyError::TooManyRules {
                count: 101,
                max: 100
            }
        );
        let rules: Vec<EgressRule> = (0..MAX_EGRESS_RULES)
            .map(|i| EgressRule::host(format!("h{i}.example.com"), &[]))
            .collect();
        assert!(SandboxEgressPolicy::restricted(rules).normalized().is_ok());
    }

    #[test]
    fn a_rule_needs_exactly_one_target() {
        let both = EgressRule {
            host: Some("a.example.com".into()),
            cidr: Some("1.1.1.1".into()),
            ports: vec![],
        };
        let neither = EgressRule {
            host: None,
            cidr: None,
            ports: vec![443],
        };
        assert_eq!(
            SandboxEgressPolicy::restricted(vec![both]).normalized(),
            Err(EgressPolicyError::RuleTarget {
                index: 0,
                given: "both"
            })
        );
        assert_eq!(
            SandboxEgressPolicy::restricted(vec![EgressRule::host("ok.example.com", &[]), neither])
                .normalized(),
            Err(EgressPolicyError::RuleTarget {
                index: 1,
                given: "neither"
            })
        );
    }

    #[test]
    fn invalid_hosts_are_named() {
        for bad in [
            "",
            "*",
            "*.",
            "a.*.example.com",
            "*example.com",
            "**.example.com",
            "exa mple.com",
            "under_score.example.com",
            "-lead.example.com",
            "trail-.example.com",
            "a..example.com",
            "1.2.3.4",
            "[2606:4700::1]",
            "bücher.example",
            "example.123",
            &format!("{}.example.com", "a".repeat(64)),
        ] {
            let err = SandboxEgressPolicy::restricted(vec![EgressRule::host(bad, &[])])
                .normalized()
                .expect_err(bad);
            match &err {
                EgressPolicyError::InvalidHost { index: 0, host, .. } => {
                    assert_eq!(host, bad);
                    assert!(err.to_string().contains(&format!("'{bad}'")), "{err}");
                }
                other => panic!("{bad}: unexpected {other:?}"),
            }
        }
    }

    #[test]
    fn valid_hosts_normalize() {
        for (raw, expected) in [
            ("Example.COM", "example.com"),
            ("example.com.", "example.com"),
            ("*.Example.com", "*.example.com"),
            ("xn--bcher-kva.example", "xn--bcher-kva.example"),
            ("localhost", "localhost"),
            ("a-b.c-d.example", "a-b.c-d.example"),
        ] {
            let policy = restricted(vec![EgressRule::host(raw, &[])]);
            assert_eq!(policy.rules[0].host.as_deref(), Some(expected), "{raw}");
        }
    }

    #[test]
    fn invalid_cidrs_are_named() {
        for bad in [
            "",
            "1.2.3",
            "1.2.3.4/33",
            "::/129",
            "1.2.3.4/x",
            "example.com/24",
            "1.2.3.4/-1",
        ] {
            let err = SandboxEgressPolicy::restricted(vec![EgressRule::cidr(bad, &[])])
                .normalized()
                .expect_err(bad);
            match &err {
                EgressPolicyError::InvalidCidr { cidr, .. } => assert_eq!(cidr, bad),
                other => panic!("{bad}: unexpected {other:?}"),
            }
        }
    }

    #[test]
    fn a_cidr_the_sandbox_can_never_reach_is_refused() {
        for (bad, range) in [
            ("10.1.0.0/16", "10.0.0.0/8"),
            ("127.0.0.1", "127.0.0.0/8"),
            ("169.254.169.254", "169.254.0.0/16"),
            ("192.168.1.0/24", "192.168.0.0/16"),
            ("203.0.113.0/24", "203.0.113.0/24"),
            ("fd00::/8", "everything outside 2000::/3"),
            ("::1", "everything outside 2000::/3"),
            ("2001:db8::/48", "2001:db8::/32"),
        ] {
            let err = SandboxEgressPolicy::restricted(vec![EgressRule::cidr(bad, &[])])
                .normalized()
                .expect_err(bad);
            match &err {
                EgressPolicyError::UnreachableCidr { cidr, range: r, .. } => {
                    assert_eq!(cidr, bad);
                    assert_eq!(r, range, "{bad}");
                }
                other => panic!("{bad}: unexpected {other:?}"),
            }
        }
        // A block that merely overlaps a private range is allowed: the
        // public part of it is reachable, the private part never is.
        for ok in ["0.0.0.0/0", "8.0.0.0/7", "::/0", "2000::/3"] {
            assert!(
                SandboxEgressPolicy::restricted(vec![EgressRule::cidr(ok, &[])])
                    .normalized()
                    .is_ok(),
                "{ok}"
            );
        }
    }

    #[test]
    fn ports_must_be_in_range_and_unique() {
        for port in [0u32, 65_536, 100_000] {
            let err = SandboxEgressPolicy::restricted(vec![EgressRule::host(
                "a.example.com",
                &[443, port],
            )])
            .normalized()
            .expect_err("bad port");
            assert_eq!(
                err,
                EgressPolicyError::InvalidPort {
                    index: 0,
                    target: "a.example.com".into(),
                    port
                }
            );
        }
        let err =
            SandboxEgressPolicy::restricted(vec![EgressRule::host("a.example.com", &[22, 22])])
                .normalized()
                .expect_err("dup port");
        assert!(matches!(
            err,
            EgressPolicyError::DuplicatePort { port: 22, .. }
        ));
        let policy = restricted(vec![EgressRule::host("a.example.com", &[65_535, 1])]);
        assert_eq!(policy.rules[0].ports, vec![1, 65_535]);
    }

    #[test]
    fn a_rule_lists_a_bounded_number_of_ports() {
        let ports: Vec<u32> = (1..=MAX_EGRESS_PORTS_PER_RULE as u32).collect();
        restricted(vec![EgressRule::host("a.example.com", &ports)]);
        let too_many: Vec<u32> = (1..=MAX_EGRESS_PORTS_PER_RULE as u32 + 1).collect();
        let err =
            SandboxEgressPolicy::restricted(vec![EgressRule::host("a.example.com", &too_many)])
                .normalized()
                .expect_err("too many ports");
        assert_eq!(
            err,
            EgressPolicyError::TooManyPorts {
                index: 0,
                target: "a.example.com".into(),
                count: MAX_EGRESS_PORTS_PER_RULE + 1,
                max: MAX_EGRESS_PORTS_PER_RULE,
            }
        );
        assert!(err.to_string().contains("a.example.com"), "{err}");
    }

    #[test]
    fn the_largest_policy_fits_in_one_environment_value() {
        let ports: Vec<u32> = (65_000..65_000 + MAX_EGRESS_PORTS_PER_RULE as u32).collect();
        let label = "a".repeat(63);
        let rules = (0..MAX_EGRESS_RULES)
            .map(|i| {
                let host = format!("{label}.{label}.{label}.h{i:03}.example.com");
                EgressRule::host(host, &ports)
            })
            .collect();
        let policy = restricted(rules);
        let json = serde_json::to_string(&policy).expect("serializes");
        // MAX_ARG_STRLEN, minus room for the variable name.
        assert!(json.len() < 128 * 1024 - 64, "{} bytes", json.len());
    }

    #[test]
    fn duplicate_targets_are_refused_after_normalization() {
        let err = SandboxEgressPolicy::restricted(vec![
            EgressRule::host("A.example.com", &[443]),
            EgressRule::host("a.example.com.", &[80]),
        ])
        .normalized()
        .expect_err("dup host");
        assert_eq!(
            err,
            EgressPolicyError::DuplicateRule {
                index: 1,
                first: 0,
                target: "a.example.com".into()
            }
        );
        let err = SandboxEgressPolicy::restricted(vec![
            EgressRule::cidr("1.1.1.0/24", &[]),
            EgressRule::cidr("1.1.1.9/24", &[]),
        ])
        .normalized()
        .expect_err("dup cidr");
        assert!(matches!(
            err,
            EgressPolicyError::DuplicateRule {
                index: 1,
                first: 0,
                ..
            }
        ));
    }

    #[test]
    fn internet_mode_allows_public_web_ports_only() {
        let policy = SandboxEgressPolicy::internet();
        assert_eq!(policy.decide("example.com", ip("1.1.1.1"), 443), Ok(()));
        assert_eq!(policy.decide("example.com", ip("1.1.1.1"), 80), Ok(()));
        assert_eq!(
            policy.decide("example.com", ip("1.1.1.1"), 22),
            Err(EgressDenial::Policy)
        );
        assert_eq!(
            policy.decide("example.com", ip("10.0.0.1"), 443),
            Err(EgressDenial::Private)
        );
    }

    /// The case table shared with the proxy script's parity test.
    fn parity_policy() -> SandboxEgressPolicy {
        restricted(vec![
            EgressRule::host("api.example.com", &[443]),
            EgressRule::host("*.pkg.example.org", &[]),
            EgressRule::cidr("1.1.1.0/24", &[443, 8443]),
            EgressRule::cidr("2606:4700::/32", &[443]),
            // Never reachable even though a rule names it: only private
            // answers for it can come back, and those are always refused.
            EgressRule::host("internal.example.com", &[443]),
        ])
    }

    const PARITY_CASES: &[(&str, &str, u16, Option<&str>)] = &[
        // host rule, exact, case and trailing dot insensitive
        ("api.example.com", "93.184.216.34", 443, None),
        ("API.Example.com.", "93.184.216.34", 443, None),
        ("api.example.com", "93.184.216.34", 80, Some("policy")),
        ("other.example.com", "93.184.216.34", 443, Some("policy")),
        // wildcard: subdomains only, default ports
        ("registry.pkg.example.org", "93.184.216.35", 443, None),
        ("a.b.pkg.example.org", "93.184.216.35", 80, None),
        ("pkg.example.org", "93.184.216.35", 443, Some("policy")),
        ("evilpkg.example.org", "93.184.216.35", 443, Some("policy")),
        (
            "registry.pkg.example.org",
            "93.184.216.35",
            22,
            Some("policy"),
        ),
        // cidr: literal IPs and resolved addresses
        ("1.1.1.1", "1.1.1.1", 443, None),
        ("1.1.1.1", "1.1.1.1", 8443, None),
        ("1.1.1.1", "1.1.1.1", 80, Some("policy")),
        ("1.1.2.1", "1.1.2.1", 443, Some("policy")),
        ("dns.example.net", "1.1.1.9", 443, None),
        ("2606:4700:4700::1111", "2606:4700:4700::1111", 443, None),
        ("[2606:4700:4700::1111]", "2606:4700:4700::1111", 443, None),
        ("2a00:1450::1", "2a00:1450::1", 443, Some("policy")),
        // a host rule never matches a literal IP
        ("93.184.216.34", "93.184.216.34", 443, Some("policy")),
        // private destinations are refused even when a rule names them
        ("internal.example.com", "10.0.0.5", 443, Some("private")),
        ("api.example.com", "169.254.169.254", 443, Some("private")),
        ("api.example.com", "::ffff:127.0.0.1", 443, Some("private")),
        ("api.example.com", "fd00::1", 443, Some("private")),
    ];

    #[test]
    fn restricted_mode_matches_the_parity_table() {
        let policy = parity_policy();
        for (host, address, port, expected) in PARITY_CASES {
            let got = policy
                .decide(host, ip(address), *port)
                .err()
                .map(EgressDenial::as_str);
            assert_eq!(got, *expected, "{host} -> {address}:{port}");
        }
    }

    #[test]
    fn never_reachable_matches_the_proxy_ranges() {
        for private in [
            "0.1.2.3",
            "10.0.0.1",
            "100.64.0.1",
            "127.0.0.1",
            "169.254.169.254",
            "172.16.0.1",
            "172.31.255.255",
            "192.0.0.1",
            "192.0.2.1",
            "192.88.99.1",
            "192.168.1.1",
            "198.18.0.1",
            "198.19.255.255",
            "198.51.100.1",
            "203.0.113.1",
            "224.0.0.1",
            "255.255.255.255",
            "::1",
            "fe80::1",
            "fd00::1",
            "ff02::1",
            "2001:db8::1",
            "2001:2::1",
            "::ffff:10.0.0.1",
        ] {
            assert!(never_reachable(ip(private)).is_some(), "{private}");
        }
        for public in [
            "1.1.1.1",
            "100.63.255.255",
            "100.128.0.0",
            "172.15.255.255",
            "172.32.0.0",
            "198.20.0.0",
            "2606:4700::1",
            "2001:20::1",
            "::ffff:8.8.8.8",
        ] {
            assert!(never_reachable(ip(public)).is_none(), "{public}");
        }
    }
}
