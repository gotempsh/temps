// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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

use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::errors::DnsError;
use crate::providers::{DnsProviderCapabilities, DnsRecordContent, DnsRecordType};

type HmacSha256 = Hmac<Sha256>;

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

/// Hex digits a marker keeps of a pending fingerprint: its first 128 bits.
/// Markers must fit DigitalOcean's 512-character TXT limit, the smallest
/// among supported providers, and the full 64 digits would leave less room
/// for long record names. 128 bits still cannot be matched by content
/// anyone else writes.
const PENDING_FINGERPRINT_LEN: usize = 32;

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
    pub record_type: String,

    /// Canonical location covered by this marker. Including the location in
    /// the signed payload prevents a valid public marker from being copied to
    /// another record name.
    pub zone: String,
    pub name: String,

    /// SHA-256 fingerprint of the canonical record content and proxied flag
    /// (see [`record_fingerprint`]). A stale marker therefore cannot
    /// authorize a replacement record at the same name and type.
    pub record_fingerprint: String,

    /// The content a guarded update is writing, as the first 32 hex digits
    /// (128 bits) of its fingerprint. Signed into the marker before the
    /// record changes and dropped once the update completes. While present
    /// the marker covers both values, so an update interrupted at any step
    /// leaves a record this install still owns, whichever value the provider
    /// ended up with. Content written by anyone else matches neither and
    /// stays unmanaged.
    #[serde(rename = "pending", default, skip_serializing_if = "Option::is_none")]
    pub pending_fingerprint: Option<String>,

    /// Project the record was created for, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<i32>,

    /// Environment the record was created for, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_id: Option<i32>,

    /// Automation controller that created the record. Signed so one
    /// reconciler can never claim records belonging to another workflow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controller: Option<String>,

    /// Marker format version.
    pub v: u32,

    /// HMAC-SHA256 over every authority-bearing field above.
    pub signature: String,
}

impl OwnershipMarker {
    #[allow(clippy::too_many_arguments)]
    pub fn new_signed(
        signing_key: &[u8; 32],
        instance: &str,
        zone: &str,
        name: &str,
        record_type: DnsRecordType,
        record_fingerprint: &str,
        project_id: Option<i32>,
        environment_id: Option<i32>,
        controller: Option<&str>,
    ) -> Result<Self, DnsError> {
        let mut marker = Self {
            managed_by: OWNERSHIP_MANAGED_BY.to_string(),
            instance: instance.to_string(),
            record_type: record_type.to_string(),
            zone: normalize_dns_name(zone),
            name: normalize_dns_name(name),
            record_fingerprint: record_fingerprint.to_string(),
            pending_fingerprint: None,
            project_id,
            environment_id,
            controller: controller.map(str::to_string),
            v: OWNERSHIP_MARKER_VERSION,
            signature: String::new(),
        };
        marker.signature = marker.compute_signature(signing_key)?;
        Ok(marker)
    }

    /// This marker, also covering the content with `fingerprint` (a full
    /// [`record_fingerprint`]) while a guarded update writes it; re-signed.
    /// See [`Self::pending_fingerprint`].
    pub fn with_pending_fingerprint(
        mut self,
        signing_key: &[u8; 32],
        fingerprint: &str,
    ) -> Result<Self, DnsError> {
        if !is_fingerprint(fingerprint) {
            return Err(DnsError::Validation(format!(
                "Cannot mark an update of {} record '{}' in zone {} as pending: '{}' is not a record fingerprint",
                self.record_type, self.name, self.zone, fingerprint
            )));
        }
        self.pending_fingerprint = Some(fingerprint[..PENDING_FINGERPRINT_LEN].to_string());
        self.signature = self.compute_signature(signing_key)?;
        Ok(self)
    }

    /// Whether an update this marker was prepared for may not have
    /// completed: the next write of the record finalizes the marker.
    pub fn has_pending_fingerprint(&self) -> bool {
        self.pending_fingerprint.is_some()
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
        if marker.managed_by != OWNERSHIP_MANAGED_BY || marker.v != OWNERSHIP_MARKER_VERSION {
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
        if marker.record_type.is_empty()
            || marker.zone.is_empty()
            || marker.name.is_empty()
            || !is_fingerprint(&marker.record_fingerprint)
            || marker
                .pending_fingerprint
                .as_deref()
                .is_some_and(|pending| !is_pending_fingerprint(pending))
            || marker.signature.len() != 64
            || !marker.signature.chars().all(|c| c.is_ascii_hexdigit())
        {
            return None;
        }
        Some(marker)
    }

    /// Verify both the authenticated payload and the exact DNS location.
    pub fn covers(
        &self,
        signing_key: &[u8; 32],
        instance: &str,
        zone: &str,
        name: &str,
        record_type: DnsRecordType,
    ) -> bool {
        if self.instance != instance
            || self.zone != normalize_dns_name(zone)
            || self.name != normalize_dns_name(name)
            || self.record_type != record_type.to_string()
        {
            return false;
        }
        let Ok(signature) = hex::decode(&self.signature) else {
            return false;
        };
        let Ok(payload) = self.signing_payload() else {
            return false;
        };
        let Ok(mut mac) = HmacSha256::new_from_slice(signing_key) else {
            return false;
        };
        mac.update(&payload);
        mac.verify_slice(&signature).is_ok()
    }

    /// Whether this marker was written by the given temps instance.
    pub fn is_owned_by(&self, instance: &str) -> bool {
        self.instance == instance
    }

    /// Whether this marker covers a record with `fingerprint`: the content it
    /// was signed for, or the content an interrupted update was writing.
    pub fn matches_fingerprint(&self, fingerprint: &str) -> bool {
        self.record_fingerprint == fingerprint
            || self.pending_fingerprint.as_deref().is_some_and(|pending| {
                is_pending_fingerprint(pending)
                    && is_fingerprint(fingerprint)
                    && fingerprint.starts_with(pending)
            })
    }

    fn compute_signature(&self, signing_key: &[u8; 32]) -> Result<String, DnsError> {
        let mut mac = HmacSha256::new_from_slice(signing_key).map_err(|error| {
            DnsError::Validation(format!(
                "Failed to initialize DNS ownership signer: {error}"
            ))
        })?;
        mac.update(&self.signing_payload()?);
        Ok(hex::encode(mac.finalize().into_bytes()))
    }

    fn signing_payload(&self) -> Result<Vec<u8>, DnsError> {
        #[derive(Serialize)]
        struct Payload<'a> {
            managed_by: &'a str,
            instance: &'a str,
            record_type: &'a str,
            zone: &'a str,
            name: &'a str,
            record_fingerprint: &'a str,
            // Omitted when absent, so markers without one sign exactly the
            // payload they always did.
            #[serde(skip_serializing_if = "Option::is_none")]
            pending: Option<&'a str>,
            project_id: Option<i32>,
            environment_id: Option<i32>,
            controller: Option<&'a str>,
            v: u32,
        }

        serde_json::to_vec(&Payload {
            managed_by: &self.managed_by,
            instance: &self.instance,
            record_type: &self.record_type,
            zone: &self.zone,
            name: &self.name,
            record_fingerprint: &self.record_fingerprint,
            pending: self.pending_fingerprint.as_deref(),
            project_id: self.project_id,
            environment_id: self.environment_id,
            controller: self.controller.as_deref(),
            v: self.v,
        })
        .map_err(DnsError::Serialization)
    }
}

/// SHA-256 fingerprint of a record's DNS data and proxied flag, as signed
/// into an [`OwnershipMarker`].
///
/// The content is fingerprinted in its [`DnsRecordContent::canonical`] form,
/// so every spelling of the same data gets the same fingerprint. Route 53
/// and Google Cloud DNS answer a CNAME create with the target exactly as
/// sent (`Origin.Example.NET.`) but list it as `origin.example.net`; a
/// marker bound to the echoed spelling would stop matching on the very next
/// read, and the record would be refused as unmanaged. Content that is
/// already canonical serializes exactly as before, so markers written for it
/// (every Cloudflare and Bunny record, which list targets canonically) keep
/// their fingerprints.
pub fn record_fingerprint(content: &DnsRecordContent, proxied: bool) -> Result<String, DnsError> {
    let encoded =
        serde_json::to_vec(&(content.canonical(), proxied)).map_err(DnsError::Serialization)?;
    Ok(hex::encode(Sha256::digest(encoded)))
}

/// A [`record_fingerprint`]: 64 hex digits.
fn is_fingerprint(value: &str) -> bool {
    value.len() == 64 && value.chars().all(|c| c.is_ascii_hexdigit())
}

/// A marker's pending fingerprint: the first [`PENDING_FINGERPRINT_LEN`] hex
/// digits of a [`record_fingerprint`].
fn is_pending_fingerprint(value: &str) -> bool {
    value.len() == PENDING_FINGERPRINT_LEN && value.chars().all(|c| c.is_ascii_hexdigit())
}

fn normalize_dns_name(value: &str) -> String {
    value.trim().trim_end_matches('.').to_ascii_lowercase()
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

    const KEY: [u8; 32] = [7; 32];
    const FINGERPRINT: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn marker(record_type: DnsRecordType) -> OwnershipMarker {
        OwnershipMarker::new_signed(
            &KEY,
            "inst-abc123",
            "example.com",
            "app",
            record_type,
            FINGERPRINT,
            Some(7),
            Some(42),
            None,
        )
        .unwrap()
    }

    #[test]
    fn marker_round_trips_through_txt_content() {
        let marker = marker(DnsRecordType::A);
        let content = marker.to_txt_content().unwrap();
        let parsed = OwnershipMarker::parse(&content).unwrap();
        assert_eq!(parsed, marker);
        assert_eq!(parsed.v, OWNERSHIP_MARKER_VERSION);
        assert_eq!(parsed.record_type, "A");
    }

    #[test]
    fn marker_without_scope_omits_ids_in_json() {
        let marker = OwnershipMarker::new_signed(
            &KEY,
            "inst-abc123",
            "example.com",
            "app",
            DnsRecordType::A,
            FINGERPRINT,
            None,
            None,
            None,
        )
        .unwrap();
        let content = marker.to_txt_content().unwrap();
        assert!(!content.contains("project_id"));
        assert!(!content.contains("environment_id"));
        assert_eq!(OwnershipMarker::parse(&content).unwrap(), marker);
    }

    /// Markers already in user zones carry no pending fingerprint; they must
    /// keep verifying, so their signed payload is byte-for-byte the original.
    #[test]
    fn marker_without_pending_fingerprint_signs_the_original_payload() {
        let marker = OwnershipMarker::new_signed(
            &KEY,
            "inst-abc123",
            "example.com",
            "app",
            DnsRecordType::A,
            FINGERPRINT,
            Some(7),
            None,
            Some("generated-hostname"),
        )
        .unwrap();
        let original_payload = format!(
            r#"{{"managed_by":"temps","instance":"inst-abc123","record_type":"A","zone":"example.com","name":"app","record_fingerprint":"{FINGERPRINT}","project_id":7,"environment_id":null,"controller":"generated-hostname","v":1}}"#
        );
        let mut mac = HmacSha256::new_from_slice(&KEY).unwrap();
        mac.update(original_payload.as_bytes());
        assert_eq!(marker.signature, hex::encode(mac.finalize().into_bytes()));
        assert!(!marker.to_txt_content().unwrap().contains("pending"));
    }

    #[test]
    fn pending_marker_covers_both_values_and_round_trips() {
        let next = "b".repeat(64);
        let pending = marker(DnsRecordType::A)
            .with_pending_fingerprint(&KEY, &next)
            .unwrap();
        assert!(pending.has_pending_fingerprint());
        assert!(pending.covers(&KEY, "inst-abc123", "example.com", "app", DnsRecordType::A));
        assert!(pending.matches_fingerprint(FINGERPRINT));
        assert!(pending.matches_fingerprint(&next));
        assert!(!pending.matches_fingerprint(&"c".repeat(64)));
        // The marker keeps the first 128 bits: a fingerprint that differs
        // there is not covered, and neither is the stored prefix itself.
        assert!(!pending.matches_fingerprint(&format!("{}{}", "c".repeat(32), "b".repeat(32))));
        assert!(!pending.matches_fingerprint(&"b".repeat(32)));
        let content = pending.to_txt_content().unwrap();
        assert!(
            content.contains(&format!(r#""pending":"{}""#, "b".repeat(32))),
            "{content}"
        );
        let parsed = OwnershipMarker::parse(&content).unwrap();
        assert_eq!(parsed, pending);
        assert!(!marker(DnsRecordType::A).has_pending_fingerprint());
    }

    /// The pending value grants ownership, so it is signed: a marker whose
    /// pending fingerprint was swapped for another value no longer verifies,
    /// and a malformed one does not even parse as a marker.
    #[test]
    fn pending_fingerprint_is_signed_and_validated() {
        let pending = marker(DnsRecordType::A)
            .with_pending_fingerprint(&KEY, &"b".repeat(64))
            .unwrap();
        let content = pending.to_txt_content().unwrap();
        let prefix = "b".repeat(32);
        let swapped = OwnershipMarker::parse(&content.replace(&prefix, &"c".repeat(32))).unwrap();
        assert!(!swapped.covers(&KEY, "inst-abc123", "example.com", "app", DnsRecordType::A));
        assert!(OwnershipMarker::parse(&content.replace(&prefix, "not-hex")).is_none());
        assert!(OwnershipMarker::parse(&content.replace(&prefix, &"b".repeat(64))).is_none());
        assert!(matches!(
            marker(DnsRecordType::A).with_pending_fingerprint(&KEY, "short"),
            Err(DnsError::Validation(_))
        ));
    }

    /// DigitalOcean caps TXT values at 512 characters, the smallest limit
    /// among supported providers. A marker carrying a pending fingerprint
    /// still fits it for a generated hostname with a full 63-character label
    /// under a preview subdomain, in a 20-character zone.
    #[test]
    fn pending_marker_fits_the_smallest_provider_txt_limit() {
        let name = format!("{}.preview", "a".repeat(63));
        let marker = OwnershipMarker::new_signed(
            &KEY,
            "0b5e3d6c-9a1f-4f7e-8c2d-3e4f5a6b7c8d",
            "preview-zone.example",
            &name,
            DnsRecordType::CNAME,
            FINGERPRINT,
            Some(99_999),
            Some(99_999),
            Some("generated-hostname"),
        )
        .unwrap()
        .with_pending_fingerprint(&KEY, &"b".repeat(64))
        .unwrap();
        let content = marker.to_txt_content().unwrap();
        assert!(
            content.len() <= 512,
            "{} characters: {content}",
            content.len()
        );
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
    fn parse_rejects_unsupported_future_versions() {
        let mut future = marker(DnsRecordType::A);
        future.v = 2;
        assert!(OwnershipMarker::parse(&future.to_txt_content().unwrap()).is_none());
    }

    #[test]
    fn covers_requires_instance_and_record_type() {
        let m = marker(DnsRecordType::A);
        assert!(m.covers(&KEY, "inst-abc123", "example.com", "app", DnsRecordType::A));
        assert!(!m.covers(
            &KEY,
            "inst-abc123",
            "example.com",
            "app",
            DnsRecordType::AAAA
        ));
        assert!(!m.covers(&KEY, "other", "example.com", "app", DnsRecordType::A));
        assert!(!m.covers(
            &[8; 32],
            "inst-abc123",
            "example.com",
            "app",
            DnsRecordType::A
        ));
        assert!(!m.covers(
            &KEY,
            "inst-abc123",
            "example.com",
            "other",
            DnsRecordType::A
        ));
    }

    // ==================== record_fingerprint ====================

    fn cname(target: &str) -> DnsRecordContent {
        DnsRecordContent::CNAME {
            target: target.to_string(),
        }
    }

    #[test]
    fn fingerprint_is_independent_of_spelling() {
        // The spelling a write echoes back and the one the next read lists.
        let echoed = record_fingerprint(&cname("Origin.Example.NET."), false).unwrap();
        let listed = record_fingerprint(&cname("origin.example.net"), false).unwrap();
        assert_eq!(echoed, listed);

        let expanded = record_fingerprint(
            &DnsRecordContent::AAAA {
                address: "2001:DB8:0:0:0:0:0:1".to_string(),
            },
            true,
        )
        .unwrap();
        let compressed = record_fingerprint(
            &DnsRecordContent::AAAA {
                address: "2001:db8::1".to_string(),
            },
            true,
        )
        .unwrap();
        assert_eq!(expanded, compressed);
    }

    #[test]
    fn fingerprint_of_canonical_content_matches_the_original_serialization() {
        // Markers already in user zones were signed over the raw
        // `(content, proxied)` serialization. For canonical content the
        // canonical fingerprint must be byte-for-byte that same value, or
        // every existing marker would stop matching its record.
        for (content, proxied) in [
            (
                DnsRecordContent::A {
                    address: "192.0.2.10".to_string(),
                },
                false,
            ),
            (
                DnsRecordContent::A {
                    address: "203.0.113.7".to_string(),
                },
                true,
            ),
            (cname("origin.example.net"), false),
            (cname("edge.example.com"), true),
        ] {
            let original = hex::encode(Sha256::digest(
                serde_json::to_vec(&(&content, proxied)).unwrap(),
            ));
            assert_eq!(
                record_fingerprint(&content, proxied).unwrap(),
                original,
                "{content:?} (proxied: {proxied})"
            );
        }
    }

    #[test]
    fn fingerprint_still_distinguishes_data_and_proxying() {
        let base = record_fingerprint(&cname("origin.example.net"), false).unwrap();
        assert_ne!(
            base,
            record_fingerprint(&cname("origin.example.org"), false).unwrap()
        );
        assert_ne!(
            base,
            record_fingerprint(&cname("origin.example.net"), true).unwrap()
        );
        // TXT data is case-sensitive, so its fingerprint is too.
        assert_ne!(
            record_fingerprint(
                &DnsRecordContent::TXT {
                    content: "Token".to_string()
                },
                false
            )
            .unwrap(),
            record_fingerprint(
                &DnsRecordContent::TXT {
                    content: "token".to_string()
                },
                false
            )
            .unwrap()
        );
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
