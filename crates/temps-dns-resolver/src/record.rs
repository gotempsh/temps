//! Wire-format records consumed from the sync API and the on-disk snapshot.
//!
//! Mirrors `temps-dns::handlers::dns_sync::EndpointDto` but is defined here
//! to keep `temps-dns-resolver` free of any dependency on `temps-dns`. The
//! types serialise identically — the sync client deserialises the same JSON
//! the control plane emits.

use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use std::str::FromStr;

use crate::error::ResolverError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RecordKind {
    A,
    Aaaa,
    Cname,
    Srv,
}

impl RecordKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            RecordKind::A => "A",
            RecordKind::Aaaa => "AAAA",
            RecordKind::Cname => "CNAME",
            RecordKind::Srv => "SRV",
        }
    }
}

impl FromStr for RecordKind {
    type Err = ResolverError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "A" => Ok(RecordKind::A),
            "AAAA" => Ok(RecordKind::Aaaa),
            "CNAME" => Ok(RecordKind::Cname),
            "SRV" => Ok(RecordKind::Srv),
            other => Err(ResolverError::InvalidRecordType {
                fqdn: String::new(),
                value: other.to_string(),
            }),
        }
    }
}

/// Owner-kind tag. The resolver doesn't act on this, but it's serialised
/// in the snapshot for completeness and future use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerKind {
    ServiceMember,
    ServiceRole,
    Node,
    Static,
}

impl OwnerKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            OwnerKind::ServiceMember => "service_member",
            OwnerKind::ServiceRole => "service_role",
            OwnerKind::Node => "node",
            OwnerKind::Static => "static",
        }
    }
}

impl FromStr for OwnerKind {
    type Err = ResolverError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "service_member" => Ok(OwnerKind::ServiceMember),
            "service_role" => Ok(OwnerKind::ServiceRole),
            "node" => Ok(OwnerKind::Node),
            "static" => Ok(OwnerKind::Static),
            other => Ok(OwnerKind::Static).and({
                // Unknown owner kinds shouldn't break resolution — fall
                // back to Static and log at the call site.
                tracing::debug!(
                    "unknown owner_kind {:?} from sync API; treating as Static",
                    other
                );
                Ok(OwnerKind::Static)
            }),
        }
    }
}

/// One record as the resolver holds it in memory and as we serialise it on
/// disk. Wire format (with stringly-typed `record_type` + `target_ip`) so
/// it deserialises directly from the control-plane sync response.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ZoneRecord {
    pub id: i64,
    pub fqdn: String,
    pub record_type: String,
    pub target_ip: Option<String>,
    pub target_port: Option<i32>,
    pub ttl: i32,
    pub owner_kind: String,
    pub owner_id: i64,
    pub node_id: Option<i32>,
    pub generation: i64,
}

impl ZoneRecord {
    pub fn kind(&self) -> Result<RecordKind, ResolverError> {
        RecordKind::from_str(&self.record_type).map_err(|_| ResolverError::InvalidRecordType {
            fqdn: self.fqdn.clone(),
            value: self.record_type.clone(),
        })
    }

    /// Resolve `target_ip` to an `IpAddr`. Returns `None` for CNAME (where
    /// the target is a hostname, not an IP).
    pub fn ip(&self) -> Result<Option<IpAddr>, ResolverError> {
        let kind = self.kind()?;
        if matches!(kind, RecordKind::Cname) {
            return Ok(None);
        }
        let raw = self
            .target_ip
            .as_deref()
            .ok_or_else(|| ResolverError::InvalidIp {
                fqdn: self.fqdn.clone(),
                value: String::new(),
            })?;
        IpAddr::from_str(raw)
            .map(Some)
            .map_err(|_| ResolverError::InvalidIp {
                fqdn: self.fqdn.clone(),
                value: raw.to_string(),
            })
    }

    /// CNAME target, if this is a CNAME record.
    pub fn cname_target(&self) -> Option<&str> {
        if self.record_type == "CNAME" {
            self.target_ip.as_deref()
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(record_type: &str, target_ip: Option<&str>) -> ZoneRecord {
        ZoneRecord {
            id: 1,
            fqdn: "x.temps.local".into(),
            record_type: record_type.into(),
            target_ip: target_ip.map(str::to_string),
            target_port: None,
            ttl: 30,
            owner_kind: "static".into(),
            owner_id: 1,
            node_id: None,
            generation: 1,
        }
    }

    #[test]
    fn ip_parses_ipv4_for_a() {
        let rec = r("A", Some("172.20.5.10"));
        assert_eq!(rec.ip().unwrap().unwrap().to_string(), "172.20.5.10");
    }

    #[test]
    fn ip_parses_ipv6_for_aaaa() {
        let rec = r("AAAA", Some("fd00::1"));
        let ip = rec.ip().unwrap().unwrap();
        assert!(ip.is_ipv6());
    }

    #[test]
    fn ip_returns_none_for_cname() {
        let rec = r("CNAME", Some("other.temps.local"));
        assert!(rec.ip().unwrap().is_none());
        assert_eq!(rec.cname_target(), Some("other.temps.local"));
    }

    #[test]
    fn ip_rejects_garbage() {
        let rec = r("A", Some("not.an.ip"));
        assert!(matches!(
            rec.ip().unwrap_err(),
            ResolverError::InvalidIp { .. }
        ));
    }

    #[test]
    fn ip_rejects_missing_target_for_a() {
        let rec = r("A", None);
        assert!(matches!(
            rec.ip().unwrap_err(),
            ResolverError::InvalidIp { .. }
        ));
    }

    #[test]
    fn record_kind_round_trip() {
        for s in ["A", "AAAA", "CNAME", "SRV"] {
            assert_eq!(RecordKind::from_str(s).unwrap().as_str(), s);
        }
    }

    #[test]
    fn unknown_record_type_errors() {
        let rec = r("TXT", Some("hello"));
        assert!(matches!(
            rec.kind().unwrap_err(),
            ResolverError::InvalidRecordType { .. }
        ));
    }
}
