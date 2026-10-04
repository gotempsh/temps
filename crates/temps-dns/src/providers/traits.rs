// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! DNS provider trait definitions
//!
//! This module defines the core traits and types for DNS provider implementations.
//! The design is inspired by dnscontrol's provider architecture, supporting multiple
//! authentication methods and record types.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::{Ipv4Addr, Ipv6Addr};
use utoipa::ToSchema;

use crate::errors::DnsError;

/// Supported DNS provider types
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum DnsProviderType {
    /// Cloudflare DNS (API Token or API Key + Email)
    Cloudflare,
    /// Bunny DNS (account API key)
    Bunny,
    /// Namecheap DNS (API User + API Key)
    Namecheap,
    /// Route53 (AWS IAM credentials)
    Route53,
    /// DigitalOcean DNS (API Token)
    DigitalOcean,
    /// Google Cloud DNS (Service Account)
    Gcp,
    /// Azure DNS (Service Principal)
    Azure,
    /// Manual DNS (user sets records manually)
    Manual,
    /// Pebble challtestsrv mock DNS (LOCAL DEV/TEST ONLY -- publishes to
    /// `pebble-challtestsrv` instead of a real registrar, so DNS-01
    /// auto-renewal can be exercised against a local Pebble ACME server with
    /// no real domain or DNS account)
    Pebble,
}

impl std::fmt::Display for DnsProviderType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DnsProviderType::Bunny => write!(f, "bunny"),
            DnsProviderType::Cloudflare => write!(f, "cloudflare"),
            DnsProviderType::Namecheap => write!(f, "namecheap"),
            DnsProviderType::Route53 => write!(f, "route53"),
            DnsProviderType::DigitalOcean => write!(f, "digitalocean"),
            DnsProviderType::Gcp => write!(f, "gcp"),
            DnsProviderType::Azure => write!(f, "azure"),
            DnsProviderType::Manual => write!(f, "manual"),
            DnsProviderType::Pebble => write!(f, "pebble"),
        }
    }
}

impl DnsProviderType {
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Result<Self, DnsError> {
        match s.to_lowercase().as_str() {
            "bunny" | "bunny.net" => Ok(DnsProviderType::Bunny),
            "cloudflare" | "cf" => Ok(DnsProviderType::Cloudflare),
            "namecheap" | "nc" => Ok(DnsProviderType::Namecheap),
            "route53" | "aws" | "r53" => Ok(DnsProviderType::Route53),
            "digitalocean" | "do" => Ok(DnsProviderType::DigitalOcean),
            "gcp" | "google" | "googlecloud" | "google-cloud" => Ok(DnsProviderType::Gcp),
            "azure" | "az" => Ok(DnsProviderType::Azure),
            "manual" => Ok(DnsProviderType::Manual),
            "pebble" | "challtestsrv" => Ok(DnsProviderType::Pebble),
            _ => Err(DnsError::InvalidProviderType(s.to_string())),
        }
    }

    /// Returns the required credential fields for this provider type
    pub fn required_credentials(&self) -> Vec<&'static str> {
        match self {
            DnsProviderType::Bunny => vec!["api_key"],
            DnsProviderType::Cloudflare => vec!["api_token"],
            DnsProviderType::Namecheap => vec!["api_user", "api_key"],
            DnsProviderType::Route53 => vec!["access_key_id", "secret_access_key"],
            DnsProviderType::DigitalOcean => vec!["api_token"],
            DnsProviderType::Gcp => vec!["service_account_email", "private_key", "project_id"],
            DnsProviderType::Azure => {
                vec![
                    "tenant_id",
                    "client_id",
                    "client_secret",
                    "subscription_id",
                    "resource_group",
                ]
            }
            DnsProviderType::Manual => vec![],
            DnsProviderType::Pebble => vec!["management_url"],
        }
    }

    /// Returns optional credential fields for this provider type
    pub fn optional_credentials(&self) -> Vec<&'static str> {
        match self {
            DnsProviderType::Bunny => vec![],
            DnsProviderType::Cloudflare => vec!["account_id"],
            DnsProviderType::Namecheap => vec!["client_ip", "sandbox"],
            DnsProviderType::Route53 => vec!["session_token", "region"],
            DnsProviderType::DigitalOcean => vec![],
            DnsProviderType::Gcp => vec![],
            DnsProviderType::Azure => vec![],
            DnsProviderType::Manual => vec![],
            DnsProviderType::Pebble => vec![],
        }
    }

    /// Whether this provider's create/update/delete calls touch ONLY the
    /// targeted (name, type) record or record set.
    ///
    /// Ownership-guarded management (ADR-031) promises never to modify a
    /// record temps did not create. That promise only holds when a write is
    /// scoped to the record being written. A provider whose API can only
    /// replace the whole zone (read every host, write every host back) turns
    /// each guarded write into a rewrite of every unrelated record, and any
    /// record the read path cannot represent losslessly is silently dropped
    /// or altered.
    ///
    /// The match is exhaustive on purpose: adding a provider type forces an
    /// explicit decision here instead of inheriting a permissive default.
    pub fn has_lossless_per_record_writes(&self) -> bool {
        match self {
            // Per-record (or per-RRset) APIs: a write names exactly one
            // (name, type) and leaves the rest of the zone alone.
            DnsProviderType::Cloudflare
            | DnsProviderType::Bunny
            | DnsProviderType::Route53
            | DnsProviderType::DigitalOcean
            | DnsProviderType::Gcp
            | DnsProviderType::Azure
            | DnsProviderType::Pebble => true,
            // Namecheap's only write API is `setHosts`, which replaces the
            // entire host list; writes are a read-modify-write of the whole
            // zone, and the `getHosts` parser cannot round-trip every host
            // type (URL redirects, ALIAS, CAA, …) or mail settings.
            DnsProviderType::Namecheap => false,
            // Manual has no write API at all.
            DnsProviderType::Manual => false,
        }
    }
}

/// Compare two DNS names the way DNS does: ASCII case-insensitively and
/// ignoring a trailing root dot (`App.` == `app`).
pub fn dns_names_equal(left: &str, right: &str) -> bool {
    left.trim_end_matches('.')
        .eq_ignore_ascii_case(right.trim_end_matches('.'))
}

/// Whether two provider records hold the same DNS data: the same record
/// type, the same name (compared the way DNS does, see [`dns_names_equal`]),
/// the same [`DnsRecordContent::canonical`] content and the same proxied
/// flag.
///
/// Provider record IDs, TTL and metadata are ignored. They do not change the
/// answer a record gives, and providers do not report them consistently
/// between a write response and a later read: Route 53 and Google Cloud DNS
/// echo a created record exactly as it was sent, then list it normalized.
pub fn records_equivalent(left: &DnsRecord, right: &DnsRecord) -> bool {
    left.content.record_type() == right.content.record_type()
        && left.proxied == right.proxied
        && dns_names_equal(&left.name, &right.name)
        && left.content.canonical() == right.content.canonical()
}

/// Canonical spelling of a hostname-valued RDATA field (CNAME and PTR
/// target, NS nameserver, MX exchange, SRV target): trimmed,
/// ASCII-lowercased and without a trailing root dot. DNS compares names
/// ASCII case-insensitively, and `origin.example.net.` is only the
/// fully-qualified spelling of `origin.example.net`.
fn canonical_hostname(value: &str) -> String {
    value.trim().trim_end_matches('.').to_ascii_lowercase()
}

/// Canonical text of an IP address: parsed and rendered again, which for
/// IPv6 is the RFC 5952 form (`2001:DB8:0:0:0:0:0:1` → `2001:db8::1`).
/// Text that does not parse is only trimmed; it can never equal the
/// canonical rendering of a valid address, which always parses.
fn canonical_address<A>(value: &str) -> String
where
    A: std::str::FromStr + std::fmt::Display,
{
    let trimmed = value.trim();
    trimmed
        .parse::<A>()
        .map(|address| address.to_string())
        .unwrap_or_else(|_| trimmed.to_string())
}

/// Maximum length of a single DNS TXT character-string (RFC 1035 §3.3).
pub const TXT_CHARACTER_STRING_MAX: usize = 255;

/// Encode TXT content in zone-file presentation format for providers whose
/// API takes raw RDATA text (Route 53, Google Cloud DNS).
///
/// The content is split into ≤255-byte character-strings, each quoted, with
/// `"` and `\` escaped and non-printable / non-ASCII bytes written as `\DDD`
/// so the provider stores exactly the bytes we meant:
/// `"chunk1" "chunk2"`. Empty content encodes as `""`.
pub fn encode_txt_presentation(content: &str) -> String {
    let bytes = content.as_bytes();
    if bytes.is_empty() {
        return "\"\"".to_string();
    }
    bytes
        .chunks(TXT_CHARACTER_STRING_MAX)
        .map(|chunk| {
            let mut encoded = String::with_capacity(chunk.len() + 2);
            encoded.push('"');
            for &byte in chunk {
                match byte {
                    b'"' => encoded.push_str("\\\""),
                    b'\\' => encoded.push_str("\\\\"),
                    0x20..=0x7e => encoded.push(byte as char),
                    _ => encoded.push_str(&format!("\\{byte:03}")),
                }
            }
            encoded.push('"');
            encoded
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Decode zone-file presentation TXT RDATA (one or more character-strings,
/// quoted or bare, with `\X` and `\DDD` escapes) into the concatenated
/// content. Inverse of [`encode_txt_presentation`].
///
/// Malformed input never fails: an unterminated quote ends at the end of the
/// input and an invalid `\DDD` is kept literally, so a hand-written record
/// still reads back as *something* (which will simply not parse as a temps
/// ownership marker).
pub fn decode_txt_presentation(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0;
    let mut in_quotes = false;
    while index < bytes.len() {
        let byte = bytes[index];
        match byte {
            b'"' => {
                in_quotes = !in_quotes;
                index += 1;
            }
            b'\\' if index + 1 < bytes.len() => {
                let digits = &bytes[index + 1..bytes.len().min(index + 4)];
                if digits.len() == 3 && digits.iter().all(u8::is_ascii_digit) {
                    let decimal = digits
                        .iter()
                        .fold(0u32, |acc, digit| acc * 10 + u32::from(digit - b'0'));
                    if let Ok(decoded) = u8::try_from(decimal) {
                        out.push(decoded);
                        index += 4;
                        continue;
                    }
                }
                out.push(bytes[index + 1]);
                index += 2;
            }
            // Whitespace outside quotes separates character-strings.
            b' ' | b'\t' if !in_quotes => index += 1,
            _ => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Longest slice, in characters, of an upstream response body that a
/// provider embeds in an error message.
pub(crate) const MAX_ERROR_BODY_CHARS: usize = 512;

/// Bound an upstream response body before it goes into an error message, so
/// a huge (or hostile) body cannot bloat errors and logs. Cuts on a character
/// boundary and says how long the full body was.
pub(crate) fn truncate_error_body(body: &str) -> String {
    let mut chars = body.chars();
    let head: String = chars.by_ref().take(MAX_ERROR_BODY_CHARS).collect();
    if chars.next().is_some() {
        format!("{head}... [truncated, {} bytes total]", body.len())
    } else {
        head
    }
}

/// DNS record types
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "UPPERCASE")]
pub enum DnsRecordType {
    A,
    AAAA,
    CNAME,
    TXT,
    MX,
    NS,
    SRV,
    CAA,
    PTR,
}

impl std::fmt::Display for DnsRecordType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DnsRecordType::A => write!(f, "A"),
            DnsRecordType::AAAA => write!(f, "AAAA"),
            DnsRecordType::CNAME => write!(f, "CNAME"),
            DnsRecordType::TXT => write!(f, "TXT"),
            DnsRecordType::MX => write!(f, "MX"),
            DnsRecordType::NS => write!(f, "NS"),
            DnsRecordType::SRV => write!(f, "SRV"),
            DnsRecordType::CAA => write!(f, "CAA"),
            DnsRecordType::PTR => write!(f, "PTR"),
        }
    }
}

/// DNS record content - varies by record type
// `==` compares spellings exactly. To ask whether two contents mean the
// same DNS data, compare their `canonical()` forms. (A plain comment, so it
// stays out of the OpenAPI schema description.)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", content = "value")]
pub enum DnsRecordContent {
    /// A record - IPv4 address (as string, e.g., "192.0.2.1")
    A {
        #[schema(example = "192.0.2.1")]
        address: String,
    },
    /// AAAA record - IPv6 address (as string, e.g., "2001:db8::1")
    AAAA {
        #[schema(example = "2001:db8::1")]
        address: String,
    },
    /// CNAME record - canonical name
    CNAME { target: String },
    /// TXT record - text content
    TXT { content: String },
    /// MX record - mail exchange
    MX { priority: u16, target: String },
    /// NS record - nameserver
    NS { nameserver: String },
    /// SRV record - service
    SRV {
        priority: u16,
        weight: u16,
        port: u16,
        target: String,
    },
    /// CAA record - certification authority authorization
    CAA {
        flags: u8,
        tag: String,
        value: String,
    },
    /// PTR record - pointer
    PTR { target: String },
}

impl DnsRecordContent {
    /// Get the record type for this content
    pub fn record_type(&self) -> DnsRecordType {
        match self {
            DnsRecordContent::A { .. } => DnsRecordType::A,
            DnsRecordContent::AAAA { .. } => DnsRecordType::AAAA,
            DnsRecordContent::CNAME { .. } => DnsRecordType::CNAME,
            DnsRecordContent::TXT { .. } => DnsRecordType::TXT,
            DnsRecordContent::MX { .. } => DnsRecordType::MX,
            DnsRecordContent::NS { .. } => DnsRecordType::NS,
            DnsRecordContent::SRV { .. } => DnsRecordType::SRV,
            DnsRecordContent::CAA { .. } => DnsRecordType::CAA,
            DnsRecordContent::PTR { .. } => DnsRecordType::PTR,
        }
    }

    /// The canonical spelling of this content, so that two contents holding
    /// the same DNS data are `==`.
    ///
    /// Providers spell the same data differently: Route 53 and Google Cloud
    /// DNS echo a created CNAME exactly as sent (`Origin.Example.NET.`) but
    /// list it as `origin.example.net`, and an IPv6 address may come back
    /// compressed. Anything that compares or fingerprints record content —
    /// ownership markers above all — must use this form.
    ///
    /// - Hostname-valued fields (CNAME/PTR target, NS nameserver, MX
    ///   exchange, SRV target) are trimmed, ASCII-lowercased and lose a
    ///   trailing root dot.
    /// - A/AAAA addresses are parsed and rendered again
    ///   (`2001:DB8:0:0:0:0:0:1` → `2001:db8::1`); an address that does not
    ///   parse is only trimmed, so it never equals a valid one.
    /// - TXT and CAA data are free-form and case-sensitive, so they are
    ///   returned unchanged.
    ///
    /// Canonical content is a fixed point: canonicalizing it again changes
    /// nothing.
    pub fn canonical(&self) -> DnsRecordContent {
        match self {
            DnsRecordContent::A { address } => DnsRecordContent::A {
                address: canonical_address::<Ipv4Addr>(address),
            },
            DnsRecordContent::AAAA { address } => DnsRecordContent::AAAA {
                address: canonical_address::<Ipv6Addr>(address),
            },
            DnsRecordContent::CNAME { target } => DnsRecordContent::CNAME {
                target: canonical_hostname(target),
            },
            DnsRecordContent::NS { nameserver } => DnsRecordContent::NS {
                nameserver: canonical_hostname(nameserver),
            },
            DnsRecordContent::PTR { target } => DnsRecordContent::PTR {
                target: canonical_hostname(target),
            },
            DnsRecordContent::MX { priority, target } => DnsRecordContent::MX {
                priority: *priority,
                target: canonical_hostname(target),
            },
            DnsRecordContent::SRV {
                priority,
                weight,
                port,
                target,
            } => DnsRecordContent::SRV {
                priority: *priority,
                weight: *weight,
                port: *port,
                target: canonical_hostname(target),
            },
            DnsRecordContent::TXT { .. } | DnsRecordContent::CAA { .. } => self.clone(),
        }
    }

    /// Convert to string representation for display
    pub fn to_value_string(&self) -> String {
        match self {
            DnsRecordContent::A { address } | DnsRecordContent::AAAA { address } => address.clone(),
            DnsRecordContent::CNAME { target }
            | DnsRecordContent::NS { nameserver: target }
            | DnsRecordContent::PTR { target } => target.clone(),
            DnsRecordContent::TXT { content } => content.clone(),
            DnsRecordContent::MX { priority, target } => format!("{} {}", priority, target),
            DnsRecordContent::SRV {
                priority,
                weight,
                port,
                target,
            } => format!("{} {} {} {}", priority, weight, port, target),
            DnsRecordContent::CAA { flags, tag, value } => {
                format!("{} {} \"{}\"", flags, tag, value)
            }
        }
    }
}

/// A DNS record
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DnsRecord {
    /// Provider-specific record ID (if exists)
    #[schema(example = "abc123")]
    pub id: Option<String>,

    /// Zone/domain this record belongs to
    #[schema(example = "example.com")]
    pub zone: String,

    /// Record name (without zone, e.g., "www" or "@" for root)
    #[schema(example = "www")]
    pub name: String,

    /// Fully qualified domain name
    #[schema(example = "www.example.com")]
    pub fqdn: String,

    /// Record content
    pub content: DnsRecordContent,

    /// Time to live in seconds
    #[schema(example = 300)]
    pub ttl: u32,

    /// Whether this record is proxied (Cloudflare-specific)
    #[serde(default)]
    pub proxied: bool,

    /// Provider-specific metadata
    #[serde(default)]
    pub metadata: HashMap<String, String>,
}

/// Request to create or update a DNS record
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DnsRecordRequest {
    /// Record name (without zone)
    #[schema(example = "www")]
    pub name: String,

    /// Record content
    pub content: DnsRecordContent,

    /// TTL in seconds (None = auto/default)
    #[schema(example = 300)]
    pub ttl: Option<u32>,

    /// Whether to proxy through CDN (if supported)
    #[serde(default)]
    pub proxied: bool,
}

/// A DNS zone (domain managed by the provider)
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DnsZone {
    /// Provider-specific zone ID
    #[schema(example = "zone123")]
    pub id: String,

    /// Zone name (domain)
    #[schema(example = "example.com")]
    pub name: String,

    /// Zone status
    #[schema(example = "active")]
    pub status: String,

    /// Nameservers for this zone
    pub nameservers: Vec<String>,

    /// Provider-specific metadata
    #[serde(default)]
    pub metadata: HashMap<String, String>,
}

/// Capabilities of a DNS provider
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DnsProviderCapabilities {
    /// Can manage A records
    pub a_record: bool,
    /// Can manage AAAA records
    pub aaaa_record: bool,
    /// Can manage CNAME records
    pub cname_record: bool,
    /// Can manage TXT records
    pub txt_record: bool,
    /// Can manage MX records
    pub mx_record: bool,
    /// Can manage NS records
    pub ns_record: bool,
    /// Can manage SRV records
    pub srv_record: bool,
    /// Can manage CAA records
    pub caa_record: bool,
    /// Supports proxying (like Cloudflare)
    pub proxy: bool,
    /// Supports automatic SSL/TLS
    pub auto_ssl: bool,
    /// Supports wildcard records
    pub wildcard: bool,
    /// Benefits from the flat (single-label) generated-hostname layout. True for
    /// providers whose wildcard TLS only covers one label below the apex (e.g.
    /// Cloudflare Free/Pro Universal SSL); the UI surfaces and recommends the
    /// Flat hostname mode for such providers.
    pub flat_hostnames: bool,
}

/// Core DNS provider trait
///
/// All DNS providers must implement this trait to provide a unified interface
/// for managing DNS records across different providers.
#[async_trait]
pub trait DnsProvider: Send + Sync {
    /// Get the provider type
    fn provider_type(&self) -> DnsProviderType;

    /// Get provider capabilities
    fn capabilities(&self) -> DnsProviderCapabilities;

    /// Whether create/update/delete touch only the targeted (name, type).
    ///
    /// Ownership-guarded management refuses providers that return `false`;
    /// see [`DnsProviderType::has_lossless_per_record_writes`].
    fn lossless_per_record_writes(&self) -> bool {
        self.provider_type().has_lossless_per_record_writes()
    }

    /// Test the credentials/connection to the provider
    async fn test_connection(&self) -> Result<bool, DnsError>;

    /// List all zones (domains) managed by this provider
    async fn list_zones(&self) -> Result<Vec<DnsZone>, DnsError>;

    /// Get a specific zone by domain name
    async fn get_zone(&self, domain: &str) -> Result<Option<DnsZone>, DnsError>;

    /// Check if the provider can manage a specific domain
    async fn can_manage_domain(&self, domain: &str) -> bool {
        self.get_zone(domain).await.ok().flatten().is_some()
    }

    /// Verify the provider's token can actually manage this zone.
    ///
    /// Unlike [`can_manage_domain`], this distinguishes a permission failure
    /// (token lacks zone scope → [`DnsError::PermissionDenied`]) from a missing
    /// zone ([`DnsError::ZoneNotFound`]), so the UI can flag a token that was
    /// configured without access to the zones it needs to manage.
    async fn check_zone_access(&self, domain: &str) -> Result<(), DnsError> {
        match self.get_zone(domain).await {
            Ok(Some(_)) => Ok(()),
            Ok(None) => Err(DnsError::ZoneNotFound(domain.to_string())),
            Err(e) => Err(e),
        }
    }

    /// List all records in a zone
    async fn list_records(&self, domain: &str) -> Result<Vec<DnsRecord>, DnsError>;

    /// Get a specific record by name and type
    async fn get_record(
        &self,
        domain: &str,
        name: &str,
        record_type: DnsRecordType,
    ) -> Result<Option<DnsRecord>, DnsError>;

    /// Get every value in a name/type RRset.
    ///
    /// Ownership-sensitive callers must use this instead of `get_record` so
    /// foreign values cannot be hidden behind the first provider result.
    ///
    /// Names compare case-insensitively and ignore a trailing dot, as DNS
    /// does: a user's `App` A record IS the `app` A record, and missing it
    /// would let temps add a sibling value next to it.
    async fn get_records(
        &self,
        domain: &str,
        name: &str,
        record_type: DnsRecordType,
    ) -> Result<Vec<DnsRecord>, DnsError> {
        Ok(self
            .list_records(domain)
            .await?
            .into_iter()
            .filter(|record| {
                dns_names_equal(&record.name, name) && record.content.record_type() == record_type
            })
            .collect())
    }

    /// Create a new DNS record
    async fn create_record(
        &self,
        domain: &str,
        request: DnsRecordRequest,
    ) -> Result<DnsRecord, DnsError>;

    /// Update an existing DNS record
    async fn update_record(
        &self,
        domain: &str,
        record_id: &str,
        request: DnsRecordRequest,
    ) -> Result<DnsRecord, DnsError>;

    /// Delete a DNS record
    async fn delete_record(&self, domain: &str, record_id: &str) -> Result<(), DnsError>;

    /// Delete the exact provider record returned by `get_records`.
    async fn delete_exact_record(&self, domain: &str, record: &DnsRecord) -> Result<(), DnsError> {
        let record_id = record.id.as_deref().ok_or_else(|| {
            DnsError::Validation(format!(
                "Provider returned no record ID for {} {} in zone {}",
                record.content.record_type(),
                record.name,
                domain
            ))
        })?;
        self.delete_record(domain, record_id).await
    }

    /// Set or update a record by name and type (upsert operation)
    ///
    /// This will create the record if it doesn't exist, or update it if it does.
    async fn set_record(
        &self,
        domain: &str,
        request: DnsRecordRequest,
    ) -> Result<DnsRecord, DnsError> {
        let record_type = request.content.record_type();
        if let Some(existing) = self.get_record(domain, &request.name, record_type).await? {
            if let Some(id) = existing.id {
                return self.update_record(domain, &id, request).await;
            }
        }
        self.create_record(domain, request).await
    }

    /// Remove all records matching a name and type
    ///
    /// Deletes every record with this name and type, not just the first match.
    /// This matters for ACME DNS-01 challenges: a wildcard order creates two
    /// `_acme-challenge` TXT records (one per authorization) with the same name
    /// but different values, and both must be removed on cleanup/renewal or
    /// they accumulate until Let's Encrypt rejects the validation.
    async fn remove_record(
        &self,
        domain: &str,
        name: &str,
        record_type: DnsRecordType,
    ) -> Result<(), DnsError> {
        let records = self.list_records(domain).await?;
        for record in records {
            if dns_names_equal(&record.name, name) && record.content.record_type() == record_type {
                if let Some(id) = record.id {
                    self.delete_record(domain, &id).await?;
                }
            }
        }
        Ok(())
    }
}

/// Manual DNS provider that doesn't actually manage records
///
/// This provider is used when the user manages DNS manually.
/// All operations return instructions for the user instead of making API calls.
pub struct ManualDnsProvider;

impl Default for ManualDnsProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl ManualDnsProvider {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl DnsProvider for ManualDnsProvider {
    fn provider_type(&self) -> DnsProviderType {
        DnsProviderType::Manual
    }

    fn capabilities(&self) -> DnsProviderCapabilities {
        // Manual provider supports all record types conceptually
        // but doesn't actually manage them
        DnsProviderCapabilities {
            a_record: true,
            aaaa_record: true,
            cname_record: true,
            txt_record: true,
            mx_record: true,
            ns_record: true,
            srv_record: true,
            caa_record: true,
            proxy: false,
            auto_ssl: false,
            wildcard: true,
            flat_hostnames: false,
        }
    }

    async fn test_connection(&self) -> Result<bool, DnsError> {
        // Manual provider always "works"
        Ok(true)
    }

    async fn list_zones(&self) -> Result<Vec<DnsZone>, DnsError> {
        // Manual provider doesn't track zones
        Ok(vec![])
    }

    async fn get_zone(&self, _domain: &str) -> Result<Option<DnsZone>, DnsError> {
        Ok(None)
    }

    async fn can_manage_domain(&self, _domain: &str) -> bool {
        // Manual provider can "manage" any domain (user does the work)
        false // Return false to indicate automatic management is not available
    }

    async fn list_records(&self, _domain: &str) -> Result<Vec<DnsRecord>, DnsError> {
        Err(DnsError::NotSupported(
            "Manual DNS provider cannot list records".to_string(),
        ))
    }

    async fn get_record(
        &self,
        _domain: &str,
        _name: &str,
        _record_type: DnsRecordType,
    ) -> Result<Option<DnsRecord>, DnsError> {
        Err(DnsError::NotSupported(
            "Manual DNS provider cannot query records".to_string(),
        ))
    }

    async fn create_record(
        &self,
        _domain: &str,
        _request: DnsRecordRequest,
    ) -> Result<DnsRecord, DnsError> {
        Err(DnsError::NotSupported(
            "Manual DNS provider cannot create records - user must configure DNS manually"
                .to_string(),
        ))
    }

    async fn update_record(
        &self,
        _domain: &str,
        _record_id: &str,
        _request: DnsRecordRequest,
    ) -> Result<DnsRecord, DnsError> {
        Err(DnsError::NotSupported(
            "Manual DNS provider cannot update records - user must configure DNS manually"
                .to_string(),
        ))
    }

    async fn delete_record(&self, _domain: &str, _record_id: &str) -> Result<(), DnsError> {
        Err(DnsError::NotSupported(
            "Manual DNS provider cannot delete records - user must configure DNS manually"
                .to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ==================== Error body truncation ====================

    #[test]
    fn truncate_error_body_bounds_long_bodies_on_char_boundaries() {
        assert_eq!(truncate_error_body(""), "");
        assert_eq!(truncate_error_body("short body"), "short body");
        let exact = "x".repeat(MAX_ERROR_BODY_CHARS);
        assert_eq!(truncate_error_body(&exact), exact);

        let long = "x".repeat(10_000);
        let truncated = truncate_error_body(&long);
        assert!(truncated.starts_with(&"x".repeat(MAX_ERROR_BODY_CHARS)));
        assert!(truncated.ends_with("... [truncated, 10000 bytes total]"));
        assert!(truncated.len() < 600, "got {} bytes", truncated.len());

        // Multi-byte characters are counted as characters and never split.
        let accented = "é".repeat(MAX_ERROR_BODY_CHARS + 1);
        let truncated = truncate_error_body(&accented);
        assert!(truncated.starts_with(&"é".repeat(MAX_ERROR_BODY_CHARS)));
        assert!(truncated.contains(&format!("truncated, {} bytes total", accented.len())));
    }

    // ==================== DnsProviderType tests ====================

    #[test]
    fn test_provider_type_from_str() {
        assert_eq!(
            DnsProviderType::from_str("cloudflare").unwrap(),
            DnsProviderType::Cloudflare
        );
        assert_eq!(
            DnsProviderType::from_str("CF").unwrap(),
            DnsProviderType::Cloudflare
        );
        assert_eq!(
            DnsProviderType::from_str("namecheap").unwrap(),
            DnsProviderType::Namecheap
        );
        assert_eq!(
            DnsProviderType::from_str("route53").unwrap(),
            DnsProviderType::Route53
        );
        assert_eq!(
            DnsProviderType::from_str("digitalocean").unwrap(),
            DnsProviderType::DigitalOcean
        );
        assert_eq!(
            DnsProviderType::from_str("gcp").unwrap(),
            DnsProviderType::Gcp
        );
        assert_eq!(
            DnsProviderType::from_str("azure").unwrap(),
            DnsProviderType::Azure
        );
        assert_eq!(
            DnsProviderType::from_str("manual").unwrap(),
            DnsProviderType::Manual
        );
        assert_eq!(
            DnsProviderType::from_str("pebble").unwrap(),
            DnsProviderType::Pebble
        );
        assert_eq!(
            DnsProviderType::from_str("challtestsrv").unwrap(),
            DnsProviderType::Pebble
        );
        assert!(DnsProviderType::from_str("invalid").is_err());
    }

    #[test]
    fn test_provider_type_from_str_aliases() {
        // Test all the aliases
        assert_eq!(
            DnsProviderType::from_str("cf").unwrap(),
            DnsProviderType::Cloudflare
        );
        assert_eq!(
            DnsProviderType::from_str("nc").unwrap(),
            DnsProviderType::Namecheap
        );
        assert_eq!(
            DnsProviderType::from_str("aws").unwrap(),
            DnsProviderType::Route53
        );
        assert_eq!(
            DnsProviderType::from_str("r53").unwrap(),
            DnsProviderType::Route53
        );
        assert_eq!(
            DnsProviderType::from_str("do").unwrap(),
            DnsProviderType::DigitalOcean
        );
        assert_eq!(
            DnsProviderType::from_str("google").unwrap(),
            DnsProviderType::Gcp
        );
        assert_eq!(
            DnsProviderType::from_str("googlecloud").unwrap(),
            DnsProviderType::Gcp
        );
        assert_eq!(
            DnsProviderType::from_str("google-cloud").unwrap(),
            DnsProviderType::Gcp
        );
        assert_eq!(
            DnsProviderType::from_str("az").unwrap(),
            DnsProviderType::Azure
        );
    }

    #[test]
    fn test_provider_type_from_str_case_insensitive() {
        assert_eq!(
            DnsProviderType::from_str("CLOUDFLARE").unwrap(),
            DnsProviderType::Cloudflare
        );
        assert_eq!(
            DnsProviderType::from_str("CloudFlare").unwrap(),
            DnsProviderType::Cloudflare
        );
        assert_eq!(
            DnsProviderType::from_str("NAMECHEAP").unwrap(),
            DnsProviderType::Namecheap
        );
    }

    #[test]
    fn test_provider_type_display() {
        assert_eq!(DnsProviderType::Cloudflare.to_string(), "cloudflare");
        assert_eq!(DnsProviderType::Namecheap.to_string(), "namecheap");
        assert_eq!(DnsProviderType::Route53.to_string(), "route53");
        assert_eq!(DnsProviderType::DigitalOcean.to_string(), "digitalocean");
        assert_eq!(DnsProviderType::Gcp.to_string(), "gcp");
        assert_eq!(DnsProviderType::Azure.to_string(), "azure");
        assert_eq!(DnsProviderType::Manual.to_string(), "manual");
        assert_eq!(DnsProviderType::Pebble.to_string(), "pebble");
    }

    #[test]
    fn test_required_credentials() {
        assert_eq!(
            DnsProviderType::Cloudflare.required_credentials(),
            vec!["api_token"]
        );
        assert_eq!(
            DnsProviderType::Namecheap.required_credentials(),
            vec!["api_user", "api_key"]
        );
        assert_eq!(
            DnsProviderType::Route53.required_credentials(),
            vec!["access_key_id", "secret_access_key"]
        );
        assert_eq!(
            DnsProviderType::DigitalOcean.required_credentials(),
            vec!["api_token"]
        );
        assert_eq!(
            DnsProviderType::Gcp.required_credentials(),
            vec!["service_account_email", "private_key", "project_id"]
        );
        assert_eq!(
            DnsProviderType::Azure.required_credentials(),
            vec![
                "tenant_id",
                "client_id",
                "client_secret",
                "subscription_id",
                "resource_group"
            ]
        );
        assert!(DnsProviderType::Manual.required_credentials().is_empty());
        assert_eq!(
            DnsProviderType::Pebble.required_credentials(),
            vec!["management_url"]
        );
    }

    #[test]
    fn test_optional_credentials() {
        assert_eq!(
            DnsProviderType::Cloudflare.optional_credentials(),
            vec!["account_id"]
        );
        assert_eq!(
            DnsProviderType::Namecheap.optional_credentials(),
            vec!["client_ip", "sandbox"]
        );
        assert_eq!(
            DnsProviderType::Route53.optional_credentials(),
            vec!["session_token", "region"]
        );
        assert!(DnsProviderType::DigitalOcean
            .optional_credentials()
            .is_empty());
        assert!(DnsProviderType::Gcp.optional_credentials().is_empty());
        assert!(DnsProviderType::Azure.optional_credentials().is_empty());
        assert!(DnsProviderType::Manual.optional_credentials().is_empty());
    }

    // ==================== DnsRecordType tests ====================

    #[test]
    fn test_record_type_display() {
        assert_eq!(DnsRecordType::A.to_string(), "A");
        assert_eq!(DnsRecordType::AAAA.to_string(), "AAAA");
        assert_eq!(DnsRecordType::CNAME.to_string(), "CNAME");
        assert_eq!(DnsRecordType::TXT.to_string(), "TXT");
        assert_eq!(DnsRecordType::MX.to_string(), "MX");
        assert_eq!(DnsRecordType::NS.to_string(), "NS");
        assert_eq!(DnsRecordType::SRV.to_string(), "SRV");
        assert_eq!(DnsRecordType::CAA.to_string(), "CAA");
        assert_eq!(DnsRecordType::PTR.to_string(), "PTR");
    }

    // ==================== DnsRecordContent tests ====================

    #[test]
    fn test_record_content_type() {
        let a_record = DnsRecordContent::A {
            address: "1.2.3.4".to_string(),
        };
        assert_eq!(a_record.record_type(), DnsRecordType::A);

        let aaaa_record = DnsRecordContent::AAAA {
            address: "2001:db8::1".to_string(),
        };
        assert_eq!(aaaa_record.record_type(), DnsRecordType::AAAA);

        let cname_record = DnsRecordContent::CNAME {
            target: "www.example.com".to_string(),
        };
        assert_eq!(cname_record.record_type(), DnsRecordType::CNAME);

        let txt_record = DnsRecordContent::TXT {
            content: "test".to_string(),
        };
        assert_eq!(txt_record.record_type(), DnsRecordType::TXT);

        let mx_record = DnsRecordContent::MX {
            priority: 10,
            target: "mail.example.com".to_string(),
        };
        assert_eq!(mx_record.record_type(), DnsRecordType::MX);

        let ns_record = DnsRecordContent::NS {
            nameserver: "ns1.example.com".to_string(),
        };
        assert_eq!(ns_record.record_type(), DnsRecordType::NS);

        let srv_record = DnsRecordContent::SRV {
            priority: 10,
            weight: 5,
            port: 5060,
            target: "sip.example.com".to_string(),
        };
        assert_eq!(srv_record.record_type(), DnsRecordType::SRV);

        let caa_record = DnsRecordContent::CAA {
            flags: 0,
            tag: "issue".to_string(),
            value: "letsencrypt.org".to_string(),
        };
        assert_eq!(caa_record.record_type(), DnsRecordType::CAA);

        let ptr_record = DnsRecordContent::PTR {
            target: "host.example.com".to_string(),
        };
        assert_eq!(ptr_record.record_type(), DnsRecordType::PTR);
    }

    #[test]
    fn test_record_content_to_string() {
        let a_record = DnsRecordContent::A {
            address: "1.2.3.4".to_string(),
        };
        assert_eq!(a_record.to_value_string(), "1.2.3.4");

        let aaaa_record = DnsRecordContent::AAAA {
            address: "2001:db8::1".to_string(),
        };
        assert_eq!(aaaa_record.to_value_string(), "2001:db8::1");

        let cname_record = DnsRecordContent::CNAME {
            target: "www.example.com".to_string(),
        };
        assert_eq!(cname_record.to_value_string(), "www.example.com");

        let txt_record = DnsRecordContent::TXT {
            content: "v=spf1 -all".to_string(),
        };
        assert_eq!(txt_record.to_value_string(), "v=spf1 -all");

        let mx_record = DnsRecordContent::MX {
            priority: 10,
            target: "mail.example.com".to_string(),
        };
        assert_eq!(mx_record.to_value_string(), "10 mail.example.com");

        let ns_record = DnsRecordContent::NS {
            nameserver: "ns1.example.com".to_string(),
        };
        assert_eq!(ns_record.to_value_string(), "ns1.example.com");

        let srv_record = DnsRecordContent::SRV {
            priority: 10,
            weight: 5,
            port: 5060,
            target: "sip.example.com".to_string(),
        };
        assert_eq!(srv_record.to_value_string(), "10 5 5060 sip.example.com");

        let caa_record = DnsRecordContent::CAA {
            flags: 0,
            tag: "issue".to_string(),
            value: "letsencrypt.org".to_string(),
        };
        assert_eq!(caa_record.to_value_string(), "0 issue \"letsencrypt.org\"");

        let ptr_record = DnsRecordContent::PTR {
            target: "host.example.com".to_string(),
        };
        assert_eq!(ptr_record.to_value_string(), "host.example.com");
    }

    // ==================== DnsRecord tests ====================

    #[test]
    fn test_dns_record_creation() {
        let record = DnsRecord {
            id: Some("rec123".to_string()),
            zone: "example.com".to_string(),
            name: "www".to_string(),
            fqdn: "www.example.com".to_string(),
            content: DnsRecordContent::A {
                address: "192.0.2.1".to_string(),
            },
            ttl: 300,
            proxied: true,
            metadata: HashMap::new(),
        };

        assert_eq!(record.id, Some("rec123".to_string()));
        assert_eq!(record.zone, "example.com");
        assert_eq!(record.name, "www");
        assert_eq!(record.fqdn, "www.example.com");
        assert_eq!(record.ttl, 300);
        assert!(record.proxied);
    }

    #[test]
    fn test_dns_record_with_metadata() {
        let mut metadata = HashMap::new();
        metadata.insert("created_by".to_string(), "test".to_string());
        metadata.insert("priority".to_string(), "high".to_string());

        let record = DnsRecord {
            id: None,
            zone: "example.com".to_string(),
            name: "@".to_string(),
            fqdn: "example.com".to_string(),
            content: DnsRecordContent::TXT {
                content: "v=spf1 -all".to_string(),
            },
            ttl: 3600,
            proxied: false,
            metadata,
        };

        assert_eq!(record.metadata.get("created_by"), Some(&"test".to_string()));
        assert_eq!(record.metadata.get("priority"), Some(&"high".to_string()));
    }

    // ==================== DnsRecordRequest tests ====================

    #[test]
    fn test_dns_record_request() {
        let request = DnsRecordRequest {
            name: "www".to_string(),
            content: DnsRecordContent::A {
                address: "192.0.2.1".to_string(),
            },
            ttl: Some(300),
            proxied: true,
        };

        assert_eq!(request.name, "www");
        assert_eq!(request.ttl, Some(300));
        assert!(request.proxied);
    }

    #[test]
    fn test_dns_record_request_without_ttl() {
        let request = DnsRecordRequest {
            name: "@".to_string(),
            content: DnsRecordContent::TXT {
                content: "test".to_string(),
            },
            ttl: None, // Auto TTL
            proxied: false,
        };

        assert!(request.ttl.is_none());
    }

    // ==================== DnsZone tests ====================

    #[test]
    fn test_dns_zone_creation() {
        let zone = DnsZone {
            id: "zone123".to_string(),
            name: "example.com".to_string(),
            status: "active".to_string(),
            nameservers: vec!["ns1.example.com".to_string(), "ns2.example.com".to_string()],
            metadata: HashMap::new(),
        };

        assert_eq!(zone.id, "zone123");
        assert_eq!(zone.name, "example.com");
        assert_eq!(zone.status, "active");
        assert_eq!(zone.nameservers.len(), 2);
    }

    // ==================== DnsProviderCapabilities tests ====================

    #[test]
    fn test_capabilities_default() {
        let caps = DnsProviderCapabilities::default();

        assert!(!caps.a_record);
        assert!(!caps.aaaa_record);
        assert!(!caps.cname_record);
        assert!(!caps.txt_record);
        assert!(!caps.mx_record);
        assert!(!caps.ns_record);
        assert!(!caps.srv_record);
        assert!(!caps.caa_record);
        assert!(!caps.proxy);
        assert!(!caps.auto_ssl);
        assert!(!caps.wildcard);
        assert!(!caps.flat_hostnames);
    }

    // ==================== ManualDnsProvider tests ====================

    #[tokio::test]
    async fn test_manual_provider() {
        let provider = ManualDnsProvider::new();
        assert_eq!(provider.provider_type(), DnsProviderType::Manual);
        assert!(provider.test_connection().await.unwrap());
        assert!(!provider.can_manage_domain("example.com").await);
        assert!(provider.list_zones().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_manual_provider_default() {
        let provider = ManualDnsProvider;
        assert_eq!(provider.provider_type(), DnsProviderType::Manual);
    }

    #[tokio::test]
    async fn test_manual_provider_capabilities() {
        let provider = ManualDnsProvider::new();
        let caps = provider.capabilities();

        // Manual provider supports all record types conceptually
        assert!(caps.a_record);
        assert!(caps.aaaa_record);
        assert!(caps.cname_record);
        assert!(caps.txt_record);
        assert!(caps.mx_record);
        assert!(caps.ns_record);
        assert!(caps.srv_record);
        assert!(caps.caa_record);
        assert!(caps.wildcard);

        // But doesn't support proxy or auto_ssl
        assert!(!caps.proxy);
        assert!(!caps.auto_ssl);
    }

    #[tokio::test]
    async fn test_manual_provider_get_zone() {
        let provider = ManualDnsProvider::new();
        let zone = provider.get_zone("example.com").await.unwrap();
        assert!(zone.is_none());
    }

    #[tokio::test]
    async fn test_manual_provider_list_records_not_supported() {
        let provider = ManualDnsProvider::new();
        let result = provider.list_records("example.com").await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), DnsError::NotSupported(_)));
    }

    #[tokio::test]
    async fn test_manual_provider_get_record_not_supported() {
        let provider = ManualDnsProvider::new();
        let result = provider
            .get_record("example.com", "www", DnsRecordType::A)
            .await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), DnsError::NotSupported(_)));
    }

    #[tokio::test]
    async fn test_manual_provider_create_record_not_supported() {
        let provider = ManualDnsProvider::new();
        let request = DnsRecordRequest {
            name: "www".to_string(),
            content: DnsRecordContent::A {
                address: "192.0.2.1".to_string(),
            },
            ttl: Some(300),
            proxied: false,
        };

        let result = provider.create_record("example.com", request).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), DnsError::NotSupported(_)));
    }

    #[tokio::test]
    async fn test_manual_provider_update_record_not_supported() {
        let provider = ManualDnsProvider::new();
        let request = DnsRecordRequest {
            name: "www".to_string(),
            content: DnsRecordContent::A {
                address: "192.0.2.1".to_string(),
            },
            ttl: Some(300),
            proxied: false,
        };

        let result = provider
            .update_record("example.com", "rec123", request)
            .await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), DnsError::NotSupported(_)));
    }

    #[tokio::test]
    async fn test_manual_provider_delete_record_not_supported() {
        let provider = ManualDnsProvider::new();
        let result = provider.delete_record("example.com", "rec123").await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), DnsError::NotSupported(_)));
    }

    // ==================== remove_record default impl tests ====================

    /// In-memory provider used to test the default `remove_record` implementation
    /// against a real multi-record `list_records` result set (all real providers
    /// route `remove_record` through this same default).
    struct MockDnsProvider {
        records: std::sync::Mutex<Vec<DnsRecord>>,
        next_id: std::sync::atomic::AtomicU32,
    }

    impl MockDnsProvider {
        fn new(records: Vec<DnsRecord>) -> Self {
            Self {
                records: std::sync::Mutex::new(records),
                next_id: std::sync::atomic::AtomicU32::new(1000),
            }
        }
    }

    #[async_trait]
    impl DnsProvider for MockDnsProvider {
        fn provider_type(&self) -> DnsProviderType {
            DnsProviderType::Cloudflare
        }

        fn capabilities(&self) -> DnsProviderCapabilities {
            DnsProviderCapabilities::default()
        }

        async fn test_connection(&self) -> Result<bool, DnsError> {
            Ok(true)
        }

        async fn list_zones(&self) -> Result<Vec<DnsZone>, DnsError> {
            Ok(vec![])
        }

        async fn get_zone(&self, _domain: &str) -> Result<Option<DnsZone>, DnsError> {
            Ok(None)
        }

        async fn list_records(&self, _domain: &str) -> Result<Vec<DnsRecord>, DnsError> {
            Ok(self.records.lock().unwrap().clone())
        }

        async fn get_record(
            &self,
            _domain: &str,
            name: &str,
            record_type: DnsRecordType,
        ) -> Result<Option<DnsRecord>, DnsError> {
            Ok(self
                .records
                .lock()
                .unwrap()
                .iter()
                .find(|r| r.name == name && r.content.record_type() == record_type)
                .cloned())
        }

        async fn create_record(
            &self,
            domain: &str,
            request: DnsRecordRequest,
        ) -> Result<DnsRecord, DnsError> {
            let id = self
                .next_id
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                .to_string();
            let record = DnsRecord {
                id: Some(id),
                zone: domain.to_string(),
                fqdn: format!("{}.{}", request.name, domain),
                name: request.name,
                content: request.content,
                ttl: request.ttl.unwrap_or(300),
                proxied: request.proxied,
                metadata: HashMap::new(),
            };
            self.records.lock().unwrap().push(record.clone());
            Ok(record)
        }

        async fn update_record(
            &self,
            _domain: &str,
            record_id: &str,
            request: DnsRecordRequest,
        ) -> Result<DnsRecord, DnsError> {
            let mut records = self.records.lock().unwrap();
            let record = records
                .iter_mut()
                .find(|r| r.id.as_deref() == Some(record_id))
                .ok_or_else(|| DnsError::RecordNotFound(record_id.to_string()))?;
            record.content = request.content;
            record.name = request.name;
            Ok(record.clone())
        }

        async fn delete_record(&self, _domain: &str, record_id: &str) -> Result<(), DnsError> {
            self.records
                .lock()
                .unwrap()
                .retain(|r| r.id.as_deref() != Some(record_id));
            Ok(())
        }
    }

    fn txt_record(id: &str, name: &str, content: &str) -> DnsRecord {
        DnsRecord {
            id: Some(id.to_string()),
            zone: "example.com".to_string(),
            name: name.to_string(),
            fqdn: format!("{}.example.com", name),
            content: DnsRecordContent::TXT {
                content: content.to_string(),
            },
            ttl: 120,
            proxied: false,
            metadata: HashMap::new(),
        }
    }

    #[tokio::test]
    async fn test_remove_record_deletes_all_matching_records() {
        // Mirrors the wildcard ACME DNS-01 scenario: two `_acme-challenge` TXT
        // records with the same name but different values (one per authorization),
        // plus an unrelated record that must survive the cleanup.
        let provider = MockDnsProvider::new(vec![
            txt_record("1", "_acme-challenge", "token-a"),
            txt_record("2", "_acme-challenge", "token-b"),
            txt_record("3", "www", "unrelated"),
        ]);

        provider
            .remove_record("example.com", "_acme-challenge", DnsRecordType::TXT)
            .await
            .unwrap();

        let remaining = provider.list_records("example.com").await.unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, Some("3".to_string()));
    }

    #[tokio::test]
    async fn test_remove_record_no_matching_records_is_noop() {
        let provider = MockDnsProvider::new(vec![txt_record("1", "www", "unrelated")]);

        provider
            .remove_record("example.com", "_acme-challenge", DnsRecordType::TXT)
            .await
            .unwrap();

        let remaining = provider.list_records("example.com").await.unwrap();
        assert_eq!(remaining.len(), 1);
    }

    // ==================== Name matching ====================

    #[test]
    fn dns_names_equal_ignores_case_and_trailing_dot() {
        assert!(dns_names_equal("App", "app"));
        assert!(dns_names_equal("app.", "APP"));
        assert!(dns_names_equal("_Temps-Owned-A.App", "_temps-owned-a.app"));
        assert!(!dns_names_equal("app", "app2"));
        assert!(!dns_names_equal("app", "pp"));
    }

    #[tokio::test]
    async fn get_records_matches_names_case_insensitively() {
        // A user's `App` record must be visible to a lookup for `app`; if it
        // were invisible, a guarded write would add a sibling value next to it.
        let provider = MockDnsProvider::new(vec![
            txt_record("1", "App", "user-owned"),
            txt_record("2", "app.", "trailing-dot"),
            txt_record("3", "other", "unrelated"),
        ]);

        let records = provider
            .get_records("example.com", "app", DnsRecordType::TXT)
            .await
            .unwrap();

        let ids: Vec<_> = records.iter().filter_map(|r| r.id.clone()).collect();
        assert_eq!(ids, vec!["1".to_string(), "2".to_string()]);
    }

    #[tokio::test]
    async fn remove_record_matches_names_case_insensitively() {
        let provider = MockDnsProvider::new(vec![
            txt_record("1", "_ACME-Challenge", "token-a"),
            txt_record("2", "www", "unrelated"),
        ]);

        provider
            .remove_record("example.com", "_acme-challenge", DnsRecordType::TXT)
            .await
            .unwrap();

        let remaining = provider.list_records("example.com").await.unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, Some("2".to_string()));
    }

    // ==================== Canonical content ====================

    #[test]
    fn canonical_lowercases_hostname_targets_and_drops_the_root_dot() {
        assert_eq!(
            DnsRecordContent::CNAME {
                target: " Origin.Example.NET. ".to_string()
            }
            .canonical(),
            DnsRecordContent::CNAME {
                target: "origin.example.net".to_string()
            }
        );
        assert_eq!(
            DnsRecordContent::NS {
                nameserver: "NS1.Example.COM.".to_string()
            }
            .canonical(),
            DnsRecordContent::NS {
                nameserver: "ns1.example.com".to_string()
            }
        );
        assert_eq!(
            DnsRecordContent::PTR {
                target: "Host.Example.COM.".to_string()
            }
            .canonical(),
            DnsRecordContent::PTR {
                target: "host.example.com".to_string()
            }
        );
        assert_eq!(
            DnsRecordContent::MX {
                priority: 10,
                target: "Mail.Example.COM.".to_string()
            }
            .canonical(),
            DnsRecordContent::MX {
                priority: 10,
                target: "mail.example.com".to_string()
            }
        );
        assert_eq!(
            DnsRecordContent::SRV {
                priority: 10,
                weight: 5,
                port: 5060,
                target: "SIP.Example.COM.".to_string()
            }
            .canonical(),
            DnsRecordContent::SRV {
                priority: 10,
                weight: 5,
                port: 5060,
                target: "sip.example.com".to_string()
            }
        );
    }

    #[test]
    fn canonical_renders_addresses_in_canonical_form() {
        assert_eq!(
            DnsRecordContent::AAAA {
                address: "2001:DB8:0:0:0:0:0:1".to_string()
            }
            .canonical(),
            DnsRecordContent::AAAA {
                address: "2001:db8::1".to_string()
            }
        );
        assert_eq!(
            DnsRecordContent::AAAA {
                address: "2001:0db8:0000::0001".to_string()
            }
            .canonical(),
            DnsRecordContent::AAAA {
                address: "2001:db8::1".to_string()
            }
        );
        assert_eq!(
            DnsRecordContent::A {
                address: " 192.0.2.1 ".to_string()
            }
            .canonical(),
            DnsRecordContent::A {
                address: "192.0.2.1".to_string()
            }
        );
    }

    #[test]
    fn canonical_keeps_unparseable_addresses_trimmed_and_distinct() {
        let invalid = DnsRecordContent::AAAA {
            address: " 2001:db8::zz ".to_string(),
        };
        assert_eq!(
            invalid.canonical(),
            DnsRecordContent::AAAA {
                address: "2001:db8::zz".to_string()
            }
        );
        // An IPv6 address in an A record is not an IPv4 address: kept as-is.
        let wrong_family = DnsRecordContent::A {
            address: "2001:db8::1".to_string(),
        };
        assert_eq!(wrong_family.canonical(), wrong_family);
    }

    #[test]
    fn canonical_leaves_free_form_data_unchanged() {
        // TXT data is case-sensitive and whitespace-significant.
        let txt = DnsRecordContent::TXT {
            content: " Mixed Case Token. ".to_string(),
        };
        assert_eq!(txt.canonical(), txt);
        let caa = DnsRecordContent::CAA {
            flags: 0,
            tag: "Issue".to_string(),
            value: "CA.Example.NET.".to_string(),
        };
        assert_eq!(caa.canonical(), caa);
    }

    #[test]
    fn canonical_is_a_fixed_point() {
        for content in [
            DnsRecordContent::A {
                address: " 192.0.2.1".to_string(),
            },
            DnsRecordContent::AAAA {
                address: "2001:DB8::0:1".to_string(),
            },
            DnsRecordContent::CNAME {
                target: "Origin.Example.NET.".to_string(),
            },
            DnsRecordContent::MX {
                priority: 5,
                target: "MX.Example.NET.".to_string(),
            },
            DnsRecordContent::TXT {
                content: "Token".to_string(),
            },
        ] {
            let canonical = content.canonical();
            assert_eq!(canonical.canonical(), canonical, "{content:?}");
        }
    }

    fn cname_record(id: &str, name: &str, target: &str) -> DnsRecord {
        DnsRecord {
            id: Some(id.to_string()),
            zone: "example.com".to_string(),
            name: name.to_string(),
            fqdn: format!("{}.example.com", name.trim_end_matches('.')),
            content: DnsRecordContent::CNAME {
                target: target.to_string(),
            },
            ttl: 300,
            proxied: false,
            metadata: HashMap::new(),
        }
    }

    #[test]
    fn records_equivalent_ignores_spelling_ids_ttl_and_metadata() {
        // What a create echoes back versus what the next listing returns.
        let written = cname_record("change-1", "App", "Origin.Example.NET.");
        let mut listed = cname_record("rrset-app-cname", "app.", "origin.example.net");
        listed.ttl = 60;
        listed
            .metadata
            .insert("provider_note".to_string(), "listed".to_string());

        assert!(records_equivalent(&written, &listed));
        assert!(records_equivalent(&listed, &written));
    }

    #[test]
    fn records_equivalent_distinguishes_data_name_type_and_proxying() {
        let record = cname_record("1", "app", "origin.example.net");

        let other_target = cname_record("1", "app", "origin.example.org");
        assert!(!records_equivalent(&record, &other_target));

        let other_name = cname_record("1", "api", "origin.example.net");
        assert!(!records_equivalent(&record, &other_name));

        let mut proxied = record.clone();
        proxied.proxied = true;
        assert!(!records_equivalent(&record, &proxied));

        // Same text in a different record type is different data.
        let mut ptr = record.clone();
        ptr.content = DnsRecordContent::PTR {
            target: "origin.example.net".to_string(),
        };
        assert!(!records_equivalent(&record, &ptr));

        // TXT data is case-sensitive.
        let mut token = record.clone();
        token.content = DnsRecordContent::TXT {
            content: "Token".to_string(),
        };
        let mut lower_token = record.clone();
        lower_token.content = DnsRecordContent::TXT {
            content: "token".to_string(),
        };
        assert!(!records_equivalent(&token, &lower_token));
    }

    // ==================== Lossless per-record writes ====================

    #[test]
    fn only_whole_zone_writers_lack_lossless_per_record_writes() {
        for provider_type in [
            DnsProviderType::Cloudflare,
            DnsProviderType::Bunny,
            DnsProviderType::Route53,
            DnsProviderType::DigitalOcean,
            DnsProviderType::Gcp,
            DnsProviderType::Azure,
            DnsProviderType::Pebble,
        ] {
            assert!(
                provider_type.has_lossless_per_record_writes(),
                "{provider_type} writes one record at a time"
            );
        }
        assert!(!DnsProviderType::Namecheap.has_lossless_per_record_writes());
        assert!(!DnsProviderType::Manual.has_lossless_per_record_writes());
    }

    #[test]
    fn trait_default_lossless_writes_follows_provider_type() {
        assert!(MockDnsProvider::new(vec![]).lossless_per_record_writes());
        assert!(!ManualDnsProvider::new().lossless_per_record_writes());
    }

    // ==================== TXT presentation encoding ====================

    #[test]
    fn txt_presentation_round_trips_short_content() {
        let encoded = encode_txt_presentation("v=spf1 -all");
        assert_eq!(encoded, "\"v=spf1 -all\"");
        assert_eq!(decode_txt_presentation(&encoded), "v=spf1 -all");
    }

    #[test]
    fn txt_presentation_escapes_quotes_and_backslashes() {
        let content = r#"{"a":"b\c"}"#;
        let encoded = encode_txt_presentation(content);
        assert_eq!(encoded, r#""{\"a\":\"b\\c\"}""#);
        assert_eq!(decode_txt_presentation(&encoded), content);
    }

    #[test]
    fn txt_presentation_splits_long_content_into_255_byte_strings() {
        // Shaped like an ownership marker: JSON full of quotes, ~400 bytes.
        let content = format!(
            r#"{{"managed_by":"temps","instance":"{}","pad":"{}"}}"#,
            "0".repeat(36),
            "x".repeat(330)
        );
        assert!(content.len() > 400);

        let encoded = encode_txt_presentation(&content);
        // Every character-string decodes to at most 255 bytes.
        let strings: Vec<String> = split_quoted(&encoded);
        assert_eq!(strings.len(), 2);
        for string in &strings {
            assert!(decode_txt_presentation(string).len() <= TXT_CHARACTER_STRING_MAX);
        }
        assert_eq!(decode_txt_presentation(&encoded), content);
    }

    #[test]
    fn txt_presentation_escapes_non_printable_bytes() {
        let content = "line1\nline2 é";
        let encoded = encode_txt_presentation(content);
        assert!(encoded.contains("\\010"));
        assert!(!encoded.contains('\n'));
        assert_eq!(decode_txt_presentation(&encoded), content);
    }

    #[test]
    fn txt_presentation_decodes_bare_and_empty_values() {
        assert_eq!(decode_txt_presentation("\"\""), "");
        assert_eq!(encode_txt_presentation(""), "\"\"");
        assert_eq!(decode_txt_presentation("plain"), "plain");
        assert_eq!(decode_txt_presentation("\"a\" \"b\""), "ab");
        // Unterminated quote and invalid \DDD never panic.
        assert_eq!(decode_txt_presentation("\"abc"), "abc");
        assert_eq!(decode_txt_presentation("\"\\999\""), "999");
    }

    /// Split encoded RDATA into its quoted character-strings (test helper).
    fn split_quoted(encoded: &str) -> Vec<String> {
        let mut strings = Vec::new();
        let mut current = String::new();
        let mut in_quotes = false;
        let mut escaped = false;
        for character in encoded.chars() {
            if in_quotes {
                current.push(character);
                if escaped {
                    escaped = false;
                } else if character == '\\' {
                    escaped = true;
                } else if character == '"' {
                    in_quotes = false;
                    strings.push(std::mem::take(&mut current));
                }
            } else if character == '"' {
                in_quotes = true;
                current.push(character);
            }
        }
        strings
    }

    // ==================== Serialization tests ====================

    #[test]
    fn test_dns_provider_type_serialization() {
        let cloudflare = DnsProviderType::Cloudflare;
        let json = serde_json::to_string(&cloudflare).unwrap();
        assert_eq!(json, "\"cloudflare\"");

        let namecheap = DnsProviderType::Namecheap;
        let json = serde_json::to_string(&namecheap).unwrap();
        assert_eq!(json, "\"namecheap\"");
    }

    #[test]
    fn test_dns_provider_type_deserialization() {
        let cloudflare: DnsProviderType = serde_json::from_str("\"cloudflare\"").unwrap();
        assert_eq!(cloudflare, DnsProviderType::Cloudflare);

        let namecheap: DnsProviderType = serde_json::from_str("\"namecheap\"").unwrap();
        assert_eq!(namecheap, DnsProviderType::Namecheap);
    }

    #[test]
    fn test_dns_record_type_serialization() {
        let a_type = DnsRecordType::A;
        let json = serde_json::to_string(&a_type).unwrap();
        assert_eq!(json, "\"A\"");

        let mx_type = DnsRecordType::MX;
        let json = serde_json::to_string(&mx_type).unwrap();
        assert_eq!(json, "\"MX\"");
    }

    #[test]
    fn test_dns_record_content_serialization() {
        let content = DnsRecordContent::A {
            address: "192.0.2.1".to_string(),
        };
        let json = serde_json::to_string(&content).unwrap();
        assert!(json.contains("\"type\":\"A\""));
        assert!(json.contains("\"address\":\"192.0.2.1\""));

        let content = DnsRecordContent::MX {
            priority: 10,
            target: "mail.example.com".to_string(),
        };
        let json = serde_json::to_string(&content).unwrap();
        assert!(json.contains("\"type\":\"MX\""));
        assert!(json.contains("\"priority\":10"));
        assert!(json.contains("\"target\":\"mail.example.com\""));
    }

    #[test]
    fn test_dns_record_serialization_roundtrip() {
        let original = DnsRecord {
            id: Some("rec123".to_string()),
            zone: "example.com".to_string(),
            name: "www".to_string(),
            fqdn: "www.example.com".to_string(),
            content: DnsRecordContent::A {
                address: "192.0.2.1".to_string(),
            },
            ttl: 300,
            proxied: true,
            metadata: HashMap::new(),
        };

        let json = serde_json::to_string(&original).unwrap();
        let deserialized: DnsRecord = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.id, original.id);
        assert_eq!(deserialized.zone, original.zone);
        assert_eq!(deserialized.name, original.name);
        assert_eq!(deserialized.fqdn, original.fqdn);
        assert_eq!(deserialized.ttl, original.ttl);
        assert_eq!(deserialized.proxied, original.proxied);
    }
}
