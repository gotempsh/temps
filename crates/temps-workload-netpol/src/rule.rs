// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The rule model (ADR-052 §4): one row of `network_policy_rules`.

use std::fmt;
use std::net::IpAddr;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use temps_workload::{EgressProfile, WorkloadRef};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// Traffic the subject sends. The peer is the destination.
    Egress,
    /// Traffic the subject receives. The peer is the source.
    Ingress,
}

/// An address block, with no host bits set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Cidr {
    addr: IpAddr,
    prefix: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InvalidCidr {
    #[error("'{value}' is not an address block: expected <address>/<prefix>")]
    Shape { value: String },
    #[error("'{value}' has an invalid address")]
    Address { value: String },
    #[error("'{value}' has prefix /{prefix}, longer than the address allows ({max})")]
    Prefix { value: String, prefix: u8, max: u8 },
    #[error("'{value}' has host bits set; did you mean {network}?")]
    HostBits { value: String, network: String },
}

impl Cidr {
    pub fn new(addr: IpAddr, prefix: u8) -> Result<Self, InvalidCidr> {
        let value = format!("{addr}/{prefix}");
        let max = if addr.is_ipv4() { 32 } else { 128 };
        if prefix > max {
            return Err(InvalidCidr::Prefix { value, prefix, max });
        }
        let network = mask(addr, prefix);
        if network != addr {
            return Err(InvalidCidr::HostBits {
                value,
                network: format!("{network}/{prefix}"),
            });
        }
        Ok(Self { addr, prefix })
    }

    /// The block holding exactly `addr`.
    pub fn host(addr: IpAddr) -> Self {
        let prefix = if addr.is_ipv4() { 32 } else { 128 };
        Self { addr, prefix }
    }

    pub fn addr(&self) -> IpAddr {
        self.addr
    }

    pub fn prefix(&self) -> u8 {
        self.prefix
    }

    pub fn is_ipv4(&self) -> bool {
        self.addr.is_ipv4()
    }

    /// Whether `addr` is inside the block. Addresses of the other family
    /// never are.
    pub fn contains(&self, addr: IpAddr) -> bool {
        addr.is_ipv4() == self.addr.is_ipv4() && mask(addr, self.prefix) == self.addr
    }
}

fn mask(addr: IpAddr, prefix: u8) -> IpAddr {
    match addr {
        IpAddr::V4(v4) => {
            let bits = u32::from(v4);
            let mask = if prefix == 0 {
                0
            } else {
                u32::MAX << (32 - prefix)
            };
            IpAddr::V4((bits & mask).into())
        }
        IpAddr::V6(v6) => {
            let bits = u128::from(v6);
            let mask = if prefix == 0 {
                0
            } else {
                u128::MAX << (128 - prefix)
            };
            IpAddr::V6((bits & mask).into())
        }
    }
}

impl fmt::Display for Cidr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.addr, self.prefix)
    }
}

impl FromStr for Cidr {
    type Err = InvalidCidr;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (addr, prefix) = value.split_once('/').ok_or_else(|| InvalidCidr::Shape {
            value: value.to_string(),
        })?;
        let addr: IpAddr = addr.parse().map_err(|_| InvalidCidr::Address {
            value: value.to_string(),
        })?;
        let prefix: u8 = prefix.parse().map_err(|_| InvalidCidr::Shape {
            value: value.to_string(),
        })?;
        Cidr::new(addr, prefix)
    }
}

impl Serialize for Cidr {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Cidr {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

/// What a rule matches on the other end of the connection.
///
/// A managed service is a workload of kind `service` (ADR-055 §3), so
/// ADR-052's `peer_kind = service` is `Peer::Workload` with that kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Peer {
    Workload(WorkloadRef),
    Cidr(Cidr),
}

/// An inclusive port range, `1..=65535`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PortRange {
    pub start: u16,
    pub end: u16,
}

impl PortRange {
    pub fn single(port: u16) -> Self {
        Self {
            start: port,
            end: port,
        }
    }

    pub fn is_valid(&self) -> bool {
        self.start >= 1 && self.start <= self.end
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum L4Protocol {
    #[default]
    Tcp,
    Udp,
    /// TCP or UDP when ports are set; any protocol when they are not.
    Any,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Allow,
    Deny,
}

/// Who wrote the rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// A service link: created and removed with it.
    Link,
    /// An administrator.
    Operator,
    /// The plane itself (audit-logged).
    System,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    pub id: i64,
    pub direction: Direction,
    pub peer: Peer,
    /// `None` = every port.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ports: Option<PortRange>,
    #[serde(default)]
    pub protocol: L4Protocol,
    pub action: Action,
    pub origin: Origin,
}

/// What a workload may do in one direction when no rule matches.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Profile {
    /// Everything not denied passes: today's behaviour.
    #[default]
    Open,
    /// Only what an allow rule names passes.
    Restricted,
    /// Nothing passes, whatever the rules say.
    None,
}

impl From<EgressProfile> for Profile {
    fn from(profile: EgressProfile) -> Self {
        match profile {
            EgressProfile::Open => Profile::Open,
            EgressProfile::Restricted => Profile::Restricted,
            EgressProfile::None => Profile::None,
        }
    }
}

/// Everything enforced for one workload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkloadPolicy {
    pub subject: WorkloadRef,
    #[serde(default)]
    pub egress: Profile,
    /// Ingress isolation is explicit: an ingress allow rule under an `Open`
    /// profile does not isolate the workload by itself, so a new link can
    /// never cut off traffic that worked before it.
    #[serde(default)]
    pub ingress: Profile,
    #[serde(default)]
    pub rules: Vec<Rule>,
}

impl WorkloadPolicy {
    /// Open in both directions with no rules: nothing to enforce.
    pub fn is_noop(&self) -> bool {
        self.egress == Profile::Open && self.ingress == Profile::Open && self.rules.is_empty()
    }

    pub fn profile(&self, direction: Direction) -> Profile {
        match direction {
            Direction::Egress => self.egress,
            Direction::Ingress => self.ingress,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_workload::WorkloadKind;

    #[test]
    fn cidrs_parse_validate_and_print() {
        let c: Cidr = "10.20.0.0/16".parse().unwrap();
        assert_eq!(c.to_string(), "10.20.0.0/16");
        assert!(c.is_ipv4());
        assert_eq!("fd00::/8".parse::<Cidr>().unwrap().prefix(), 8);
        assert_eq!(
            "10.20.0.1/16".parse::<Cidr>(),
            Err(InvalidCidr::HostBits {
                value: "10.20.0.1/16".into(),
                network: "10.20.0.0/16".into()
            })
        );
        assert!(matches!(
            "10.0.0.0/33".parse::<Cidr>(),
            Err(InvalidCidr::Prefix { max: 32, .. })
        ));
        assert!(matches!(
            "10.0.0.0".parse::<Cidr>(),
            Err(InvalidCidr::Shape { .. })
        ));
        assert!(matches!(
            "ten/8".parse::<Cidr>(),
            Err(InvalidCidr::Address { .. })
        ));
        assert_eq!("0.0.0.0/0".parse::<Cidr>().unwrap().prefix(), 0);
        assert_eq!(
            Cidr::host("172.20.0.5".parse().unwrap()).to_string(),
            "172.20.0.5/32"
        );
    }

    #[test]
    fn rules_round_trip_through_json() {
        let rule = Rule {
            id: 4,
            direction: Direction::Egress,
            peer: Peer::Workload(WorkloadRef::new(WorkloadKind::Service, 9)),
            ports: Some(PortRange::single(5432)),
            protocol: L4Protocol::Tcp,
            action: Action::Allow,
            origin: Origin::Link,
        };
        let json = serde_json::to_value(&rule).unwrap();
        assert_eq!(json["peer"]["workload"]["kind"], "service");
        assert_eq!(serde_json::from_value::<Rule>(json).unwrap(), rule);
        let cidr: Peer = serde_json::from_str(r#"{"cidr":"10.0.0.0/8"}"#).unwrap();
        assert_eq!(cidr, Peer::Cidr("10.0.0.0/8".parse().unwrap()));
        assert!(serde_json::from_str::<Peer>(r#"{"cidr":"10.0.0.1/8"}"#).is_err());
    }

    #[test]
    fn cidrs_contain_their_own_family_only() {
        let block: Cidr = "10.237.0.0/16".parse().unwrap();
        assert!(block.contains("10.237.4.9".parse().unwrap()));
        assert!(!block.contains("10.238.0.1".parse().unwrap()));
        assert!(!block.contains("::ffff:10.237.4.9".parse().unwrap()));
        let all: Cidr = "0.0.0.0/0".parse().unwrap();
        assert!(all.contains("192.0.2.1".parse().unwrap()));
        assert!(!all.contains("2001:db8::1".parse().unwrap()));
        assert!(Cidr::host("fd00::9".parse().unwrap()).contains("fd00::9".parse().unwrap()));
    }

    #[test]
    fn port_ranges_must_be_ordered_and_non_zero() {
        assert!(PortRange::single(80).is_valid());
        assert!(PortRange {
            start: 5000,
            end: 5010
        }
        .is_valid());
        assert!(!PortRange { start: 10, end: 9 }.is_valid());
        assert!(!PortRange::single(0).is_valid());
    }
}
