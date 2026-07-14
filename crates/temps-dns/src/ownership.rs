//! DNS record ownership markers (ADR-031)
//!
//! Temps writes public A/AAAA/CNAME records into zones it does not own.
//! The one mistake this feature must never make is touching a record temps
//! did not create. Ownership is therefore recorded *at the provider*, next to
//! the record itself, as a companion TXT "registry" record whose content is a
//! typed JSON marker. Before any update or delete, the marker is fetched and
//! must parse AND match this install's instance ID AND cover the record's
//! type; anything else refuses the write.
//!
//! The registry name is scoped by record type — `_temps-owned-a.<name>`,
//! `_temps-owned-aaaa.<name>`, … — so owning `app` A never grants ownership
//! of a user's `app` AAAA. The record name is escaped injectively (`_` → `__`
//! before `*` → `_w`) so no two distinct record names can share a registry
//! name (`*.staging` vs a literal `wildcard.staging`).
//!
//! The companion-TXT scheme works uniformly across every provider. Cloudflare
//! additionally has a per-record `comment` field, but the `cloudflare` crate's
//! DNS params don't expose it, so comment stamping is deferred (the TXT
//! registry is used there too).
//!
//! The marker JSON is a compatibility surface once it exists in user zones —
//! it carries a `v` field so the format can evolve. Unknown fields are
//! tolerated on parse so a `v: 2` writer doesn't brick a `v: 1` reader.

use serde::{Deserialize, Serialize};

use crate::errors::DnsError;
use crate::providers::{DnsProviderCapabilities, DnsRecordType};

/// Current marker format version.
pub const OWNERSHIP_MARKER_VERSION: u32 = 1;

/// Value of `managed_by` in every marker temps writes.
pub const OWNERSHIP_MANAGED_BY: &str = "temps";

/// Label prefix of the companion TXT registry record. The record type is
/// appended (`_temps-owned-a`, `_temps-owned-cname`, …) so ownership is
/// scoped per (name, type), not per name.
pub const OWNERSHIP_REGISTRY_PREFIX: &str = "_temps-owned";

/// Maximum accepted length for the `instance` field when parsing markers.
/// Our own IDs are 36-char UUIDs; anything longer is not ours.
const MAX_INSTANCE_LEN: usize = 64;

/// Ownership marker stored in the companion TXT record.
///
/// `instance` is the install-scoped random ID from
/// [`crate::services::ManagedDnsRecordService`]; two temps installs managing
/// the same zone will refuse to touch each other's records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnershipMarker {
    /// Always [`OWNERSHIP_MANAGED_BY`]. Anything else fails to parse as ours.
    pub managed_by: String,

    /// Install-scoped random ID of the temps instance that created the record.
    pub instance: String,

    /// Record type this marker covers (e.g. "A"). Belt-and-braces on top of
    /// the type-scoped registry name; a mismatch means "not ours".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record_type: Option<String>,

    /// Project the record was created for, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<i32>,

    /// Environment the record was created for, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_id: Option<i32>,

    /// Marker format version.
    pub v: u32,
}

impl OwnershipMarker {
    pub fn new(
        instance: &str,
        record_type: DnsRecordType,
        project_id: Option<i32>,
        environment_id: Option<i32>,
    ) -> Self {
        Self {
            managed_by: OWNERSHIP_MANAGED_BY.to_string(),
            instance: instance.to_string(),
            record_type: Some(record_type.to_string()),
            project_id,
            environment_id,
            v: OWNERSHIP_MARKER_VERSION,
        }
    }

    /// Serialize to the TXT record content.
    pub fn to_txt_content(&self) -> Result<String, DnsError> {
        serde_json::to_string(self).map_err(DnsError::Serialization)
    }

    /// Parse a TXT record content as an ownership marker.
    ///
    /// Returns `None` for anything that is not a well-formed temps marker —
    /// unparsable JSON, wrong `managed_by`, missing fields, or an `instance`
    /// outside the ID charset. Callers treat `None` as "not ours: hands off".
    ///
    /// The instance charset check ([A-Za-z0-9-], ≤ 64 chars) also keeps
    /// attacker-written TXT content (newlines, ANSI, oversized strings) out of
    /// temps' logs and error messages, where the field is interpolated.
    pub fn parse(content: &str) -> Option<Self> {
        let marker: Self = serde_json::from_str(content.trim()).ok()?;
        if marker.managed_by != OWNERSHIP_MANAGED_BY {
            return None;
        }
        if marker.instance.is_empty()
            || marker.instance.len() > MAX_INSTANCE_LEN
            || !marker
                .instance
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-')
        {
            return None;
        }
        Some(marker)
    }

    /// Whether this marker was written by the given temps instance AND covers
    /// the given record type. A marker without a `record_type` (never written
    /// by temps) does not cover anything.
    pub fn covers(&self, instance: &str, record_type: DnsRecordType) -> bool {
        self.instance == instance && self.record_type.as_deref() == Some(&record_type.to_string())
    }

    /// Whether this marker was written by the given temps instance.
    pub fn is_owned_by(&self, instance: &str) -> bool {
        self.instance == instance
    }
}

/// Injective escaping of a record name for use inside a registry name.
///
/// `_` → `__` first, then `*` → `_w`; because every escape sequence starts
/// with `_` and literal underscores are doubled, no two distinct record names
/// map to the same escaped form (a literal `_w` becomes `__w`).
fn escape_record_name(record_name: &str) -> String {
    record_name.replace('_', "__").replace('*', "_w")
}

/// Name of the companion TXT registry record for a managed record.
///
/// - (`@` / empty, A) → `_temps-owned-a`
/// - (`www`, A) → `_temps-owned-a.www`
/// - (`*-staging`, CNAME) → `_temps-owned-cname._w-staging`
/// - (`*.staging`, A) → `_temps-owned-a._w.staging`
///
/// Type-scoped and injective — see the module docs for why both matter.
pub fn registry_record_name(record_name: &str, record_type: DnsRecordType) -> String {
    let prefix = format!(
        "{}-{}",
        OWNERSHIP_REGISTRY_PREFIX,
        record_type.to_string().to_lowercase()
    );
    if record_name == "@" || record_name.is_empty() {
        return prefix;
    }
    format!("{}.{}", prefix, escape_record_name(record_name))
}

/// Number of subdomain levels a record name adds below the zone apex.
///
/// `@` → 0, `www` → 1, `*-staging` → 1, `*.staging` → 2, `a.b.c` → 3.
pub fn subdomain_depth(record_name: &str) -> usize {
    if record_name == "@" || record_name.is_empty() {
        return 0;
    }
    record_name.split('.').filter(|l| !l.is_empty()).count()
}

/// Guardrail for Cloudflare's Universal SSL depth limit (ADR-031 §3).
///
/// Cloudflare's free/pro certificates only cover ONE subdomain level below
/// the apex. A *proxied* record at depth ≥ 2 (`a.b.example.com`,
/// `*.foo.example.com`) passes DNS but fails TLS at the edge with an opaque
/// 526/525 unless the user pays for Advanced Certificate Manager. Detect it
/// at write time and refuse with an actionable message instead.
///
/// Only applies to proxied records — unproxied deep records are fine.
pub fn check_proxied_depth(zone: &str, record_name: &str) -> Result<(), DnsError> {
    let depth = subdomain_depth(record_name);
    if depth < 2 {
        return Ok(());
    }
    let flat_suggestion = record_name.replace('.', "-");
    Err(DnsError::ProxiedDepthUnsupported {
        fqdn: format!("{}.{}", record_name, zone),
        levels: depth,
        flat_suggestion: format!("{}.{}", flat_suggestion, zone),
    })
}

/// Full proxied-write gate: the provider must support proxying and the
/// record must pass the depth guardrail. Pure over the capabilities so it is
/// unit-testable without a database or provider API.
pub fn check_proxy_allowed(
    capabilities: &DnsProviderCapabilities,
    provider_name: &str,
    zone: &str,
    record_name: &str,
) -> Result<(), DnsError> {
    if !capabilities.proxy {
        return Err(DnsError::ProxyNotSupportedByProvider {
            provider: provider_name.to_string(),
        });
    }
    check_proxied_depth(zone, record_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marker(record_type: DnsRecordType) -> OwnershipMarker {
        OwnershipMarker::new("inst-abc123", record_type, Some(7), Some(42))
    }

    #[test]
    fn marker_round_trips_through_txt_content() {
        let marker = marker(DnsRecordType::A);
        let content = marker.to_txt_content().unwrap();
        let parsed = OwnershipMarker::parse(&content).unwrap();
        assert_eq!(parsed, marker);
        assert_eq!(parsed.v, OWNERSHIP_MARKER_VERSION);
        assert_eq!(parsed.record_type.as_deref(), Some("A"));
    }

    #[test]
    fn marker_without_scope_omits_ids_in_json() {
        let marker = OwnershipMarker::new("inst-abc123", DnsRecordType::A, None, None);
        let content = marker.to_txt_content().unwrap();
        assert!(!content.contains("project_id"));
        assert!(!content.contains("environment_id"));
        assert_eq!(OwnershipMarker::parse(&content).unwrap(), marker);
    }

    #[test]
    fn parse_rejects_non_marker_content() {
        // Existing user TXT records must never parse as ours.
        assert!(OwnershipMarker::parse("v=spf1 -all").is_none());
        assert!(OwnershipMarker::parse("").is_none());
        assert!(OwnershipMarker::parse("{\"foo\": 1}").is_none());
    }

    #[test]
    fn parse_rejects_wrong_managed_by() {
        let content = r#"{"managed_by":"other-tool","instance":"x","v":1}"#;
        assert!(OwnershipMarker::parse(content).is_none());
    }

    #[test]
    fn parse_rejects_invalid_instance() {
        // Empty
        assert!(OwnershipMarker::parse(r#"{"managed_by":"temps","instance":"","v":1}"#).is_none());
        // Charset: log/UI injection payloads must not survive parse
        assert!(OwnershipMarker::parse(
            r#"{"managed_by":"temps","instance":"evil\nFORGED LOG LINE","v":1}"#
        )
        .is_none());
        assert!(OwnershipMarker::parse(
            r#"{"managed_by":"temps","instance":"<script>x</script>","v":1}"#
        )
        .is_none());
        // Oversized
        let long = "a".repeat(65);
        assert!(OwnershipMarker::parse(&format!(
            r#"{{"managed_by":"temps","instance":"{}","v":1}}"#,
            long
        ))
        .is_none());
    }

    #[test]
    fn parse_tolerates_unknown_fields_from_future_versions() {
        let content = r#"{"managed_by":"temps","instance":"x","v":2,"new_field":"y"}"#;
        let marker = OwnershipMarker::parse(content).unwrap();
        assert_eq!(marker.v, 2);
    }

    #[test]
    fn covers_requires_instance_and_record_type() {
        let m = marker(DnsRecordType::A);
        assert!(m.covers("inst-abc123", DnsRecordType::A));
        assert!(!m.covers("inst-abc123", DnsRecordType::AAAA));
        assert!(!m.covers("other", DnsRecordType::A));

        // A marker with no record_type (not something temps writes) covers nothing.
        let untyped =
            OwnershipMarker::parse(r#"{"managed_by":"temps","instance":"inst-abc123","v":1}"#)
                .unwrap();
        assert!(!untyped.covers("inst-abc123", DnsRecordType::A));
    }

    #[test]
    fn registry_name_is_type_scoped() {
        assert_eq!(
            registry_record_name("app", DnsRecordType::A),
            "_temps-owned-a.app"
        );
        assert_eq!(
            registry_record_name("app", DnsRecordType::AAAA),
            "_temps-owned-aaaa.app"
        );
        assert_ne!(
            registry_record_name("app", DnsRecordType::A),
            registry_record_name("app", DnsRecordType::CNAME)
        );
        assert_eq!(
            registry_record_name("@", DnsRecordType::A),
            "_temps-owned-a"
        );
        assert_eq!(registry_record_name("", DnsRecordType::A), "_temps-owned-a");
    }

    #[test]
    fn registry_name_escaping_is_injective_for_wildcards() {
        // The classic collision: a wildcard vs a literal name that the old
        // '*' -> "wildcard" replacement would have merged.
        let wildcard = registry_record_name("*.staging", DnsRecordType::A);
        let literal = registry_record_name("wildcard.staging", DnsRecordType::A);
        assert_ne!(wildcard, literal);
        assert_eq!(wildcard, "_temps-owned-a._w.staging");

        // A literal that looks like the escape sequence itself.
        let escaped_literal = registry_record_name("_w.staging", DnsRecordType::A);
        assert_ne!(wildcard, escaped_literal);
        assert_eq!(escaped_literal, "_temps-owned-a.__w.staging");

        // Underscore doubling round-trip distinctness.
        assert_ne!(
            registry_record_name("a_b", DnsRecordType::A),
            registry_record_name("a__b", DnsRecordType::A)
        );
    }

    #[test]
    fn subdomain_depth_counts_labels() {
        assert_eq!(subdomain_depth("@"), 0);
        assert_eq!(subdomain_depth(""), 0);
        assert_eq!(subdomain_depth("www"), 1);
        assert_eq!(subdomain_depth("*-staging"), 1);
        assert_eq!(subdomain_depth("*.staging"), 2);
        assert_eq!(subdomain_depth("a.b.c"), 3);
    }

    #[test]
    fn proxied_depth_guardrail_allows_single_level() {
        assert!(check_proxied_depth("example.com", "@").is_ok());
        assert!(check_proxied_depth("example.com", "www").is_ok());
        assert!(check_proxied_depth("example.com", "*-staging").is_ok());
    }

    #[test]
    fn proxied_depth_guardrail_rejects_two_levels_with_flat_suggestion() {
        let err = check_proxied_depth("example.com", "*.staging").unwrap_err();
        match err {
            DnsError::ProxiedDepthUnsupported {
                fqdn,
                levels,
                flat_suggestion,
            } => {
                assert_eq!(fqdn, "*.staging.example.com");
                assert_eq!(levels, 2);
                assert_eq!(flat_suggestion, "*-staging.example.com");
            }
            other => panic!("expected ProxiedDepthUnsupported, got {:?}", other),
        }
    }

    #[test]
    fn proxy_gate_requires_capability_then_depth() {
        let no_proxy = DnsProviderCapabilities::default();
        let err = check_proxy_allowed(&no_proxy, "route53-prod", "example.com", "www").unwrap_err();
        match err {
            DnsError::ProxyNotSupportedByProvider { provider } => {
                assert_eq!(provider, "route53-prod");
            }
            other => panic!("expected ProxyNotSupportedByProvider, got {:?}", other),
        }

        let with_proxy = DnsProviderCapabilities {
            proxy: true,
            ..Default::default()
        };
        assert!(check_proxy_allowed(&with_proxy, "cf", "example.com", "www").is_ok());
        assert!(matches!(
            check_proxy_allowed(&with_proxy, "cf", "example.com", "*.staging"),
            Err(DnsError::ProxiedDepthUnsupported { .. })
        ));
    }
}
