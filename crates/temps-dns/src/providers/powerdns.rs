// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! PowerDNS Authoritative Server provider
//!
//! Talks to the PowerDNS Authoritative HTTP API
//! (<https://doc.powerdns.com/authoritative/http-api/>), so self-hosted and
//! air-gapped installs that run their own authoritative DNS can use DNS-01
//! challenges, wildcard certificates and managed records.
//!
//! The API is RRset-oriented: a write (`PATCH /zones/{zone}`) replaces every
//! value at one (name, type). Temps' provider contract, like Cloudflare's, is
//! per-value — a wildcard ACME order publishes two TXT values under the same
//! `_acme-challenge` name and later deletes each one by ID. This provider
//! bridges the two:
//!
//! - **Per-value record IDs.** An ID names the RRset *and* the value
//!   (`fqdn::TYPE::value`), so `delete_record` removes exactly one value and
//!   leaves sibling values (another ACME authorization, a user's record) in
//!   place.
//! - **Lossless read-modify-write.** Every write starts from the zone's
//!   unfiltered read, keeps every other value verbatim (including
//!   `disabled` ones), and leaves the RRset's comments untouched by omitting
//!   `comments` from the change. The `rrset_name` filter is deliberately not
//!   used: PowerDNS 4.9 omits disabled values from filtered reads, so a write
//!   built on one would silently delete them.
//! - **Serialized writes.** Writes to one RRset from this process are
//!   serialized by a striped lock, so two concurrent appends cannot lose a
//!   value. PowerDNS has no compare-and-swap for RRsets, so a write made at
//!   the same instant by a different client (another tool, another Temps
//!   process) can still race; that window is one read plus one PATCH.
//!
//! Writes are refused on Secondary/Consumer zones: PowerDNS accepts them,
//! but the next zone transfer from the primary silently discards them.
//!
//! The API URL is operator-supplied and normally points at a private
//! address, so it goes through the same outbound SSRF guard as other
//! self-hosted integrations: a private address must be listed under
//! Settings > Security > Trusted private networks, cloud metadata endpoints
//! are always refused, the resolved address is pinned, and redirects are
//! never followed (they would carry the API key elsewhere).

use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT};
use reqwest::{Client, Method, StatusCode, Url};
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::LazyLock;
use std::time::Duration;
use tokio::sync::{Mutex, OnceCell};
use tracing::{debug, info};

use super::credentials::PowerDnsCredentials;
use super::traits::{
    decode_txt_presentation, dns_names_equal, encode_txt_presentation, truncate_error_body,
    DnsProvider, DnsProviderCapabilities, DnsProviderType, DnsRecord, DnsRecordContent,
    DnsRecordRequest, DnsRecordType, DnsZone,
};
use crate::errors::DnsError;

/// PowerDNS's built-in server ID; the only one a stock install exposes.
pub const DEFAULT_SERVER_ID: &str = "localhost";
/// TTL for a new RRset when the request does not name one.
const DEFAULT_TTL: u32 = 300;
/// Per-request timeout for the PowerDNS API.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Metadata key carrying a zone's PowerDNS kind (Native, Master, Slave, ...).
pub const METADATA_ZONE_KIND: &str = "kind";
/// Metadata key set to `"true"` on a value PowerDNS has disabled (stored but
/// not served).
pub const METADATA_DISABLED: &str = "disabled";
/// Separator between the parts of a record ID: `fqdn::TYPE::value`. A DNS
/// name or type never contains it; the value may (IPv6), so the value is
/// always the last part.
const ID_SEPARATOR: &str = "::";
/// Number of stripes in the per-RRset write lock. Fixed, so the lock table
/// costs constant memory however many names are written.
const WRITE_LOCK_STRIPES: usize = 64;

/// Striped locks serializing read-modify-write of one RRset in this process.
static WRITE_LOCKS: LazyLock<Vec<Mutex<()>>> =
    LazyLock::new(|| (0..WRITE_LOCK_STRIPES).map(|_| Mutex::new(())).collect());

/// PowerDNS Authoritative Server provider.
pub struct PowerDnsProvider {
    /// API root, ending in `/api/v1` (no trailing slash).
    api_root: Url,
    server_id: String,
    api_key: String,
    /// HTTP client, built on first use: building it resolves and validates
    /// the API host, which is async.
    client: OnceCell<Client>,
    /// Skip the outbound address guard (unit tests against a loopback mock).
    #[cfg(test)]
    skip_address_validation: bool,
}

/// One value in a PowerDNS RRset, exactly as the API spells it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PdnsRecord {
    content: String,
    #[serde(default)]
    disabled: bool,
}

/// An RRset as the API returns it. Comments are not modelled: writes omit
/// `comments`, which tells PowerDNS to keep the existing ones.
#[derive(Debug, Clone, Deserialize)]
struct PdnsRrset {
    name: String,
    #[serde(rename = "type")]
    record_type: String,
    #[serde(default)]
    ttl: u32,
    #[serde(default)]
    records: Vec<PdnsRecord>,
}

/// A zone as the zone listing returns it.
#[derive(Debug, Clone, Deserialize)]
struct PdnsZoneSummary {
    id: String,
    name: String,
    #[serde(default)]
    kind: String,
}

/// A zone with its RRsets (`GET /zones/{zone_id}`).
#[derive(Debug, Clone, Deserialize)]
struct PdnsZoneDetail {
    #[serde(default)]
    rrsets: Vec<PdnsRrset>,
}

/// `GET /servers/{server_id}`.
#[derive(Debug, Clone, Deserialize)]
struct PdnsServer {
    #[serde(default)]
    daemon_type: String,
    #[serde(default)]
    version: String,
}

/// Error body PowerDNS returns with 4xx/5xx JSON responses.
#[derive(Debug, Deserialize)]
struct PdnsErrorBody {
    error: String,
}

#[derive(Debug, Serialize)]
struct RrsetPatch {
    rrsets: Vec<RrsetChange>,
}

#[derive(Debug, Serialize)]
struct RrsetChange {
    name: String,
    #[serde(rename = "type")]
    record_type: String,
    changetype: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    ttl: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    records: Option<Vec<PdnsRecord>>,
}

/// A parsed per-value record ID.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RecordId {
    /// Lowercase FQDN without the trailing dot.
    fqdn: String,
    record_type: DnsRecordType,
    /// [`PowerDnsProvider::value_key`] of the value.
    value: String,
}

impl RecordId {
    fn parse(record_id: &str, zone: &str) -> Result<Self, DnsError> {
        let invalid = |reason: &str| {
            DnsError::Validation(format!(
                "Invalid PowerDNS record ID '{record_id}' for zone {zone}: {reason} (expected 'fqdn::TYPE::value')"
            ))
        };
        let mut parts = record_id.splitn(3, ID_SEPARATOR);
        let fqdn = parts.next().unwrap_or_default();
        let type_part = parts.next().ok_or_else(|| invalid("missing record type"))?;
        let value = parts
            .next()
            .ok_or_else(|| invalid("missing record value"))?;
        let record_type = PowerDnsProvider::parse_record_type(type_part)
            .ok_or_else(|| invalid(&format!("'{type_part}' is not a supported record type")))?;
        let fqdn = PowerDnsProvider::normalize_name(fqdn);
        if fqdn.is_empty() {
            return Err(invalid("empty record name"));
        }
        Ok(Self {
            fqdn,
            record_type,
            value: value.to_string(),
        })
    }

    fn render(&self) -> String {
        format!(
            "{}{ID_SEPARATOR}{}{ID_SEPARATOR}{}",
            self.fqdn, self.record_type, self.value
        )
    }
}

impl PowerDnsProvider {
    /// Create a provider, validating the URL, API key and server ID.
    ///
    /// The API URL may be given as the server root (`http://pdns:8081`) or
    /// with the API path (`http://pdns:8081/api/v1`). A literal IP host is
    /// checked against the outbound guard here; a hostname is resolved and
    /// checked when the first request is made.
    pub fn new(credentials: PowerDnsCredentials) -> Result<Self, DnsError> {
        let provider = Self::build(credentials)?;
        temps_core::url_validation::validate_external_url(provider.api_root.as_str()).map_err(
            |e| {
                DnsError::Validation(format!(
                    "PowerDNS API URL {} is not an allowed destination: {e}",
                    provider.api_root
                ))
            },
        )?;
        Ok(provider)
    }

    /// Provider for unit tests: talks to a loopback mock server, skipping
    /// the outbound address guard.
    #[cfg(test)]
    fn new_unchecked(credentials: PowerDnsCredentials) -> Result<Self, DnsError> {
        let mut provider = Self::build(credentials)?;
        provider.skip_address_validation = true;
        Ok(provider)
    }

    fn build(credentials: PowerDnsCredentials) -> Result<Self, DnsError> {
        let api_root = Self::normalize_api_url(&credentials.api_url)?;

        let api_key = credentials.api_key.trim().to_string();
        if api_key.is_empty() {
            return Err(DnsError::InvalidCredentials(
                "PowerDNS API key is empty: set it to the server's `api-key` setting".to_string(),
            ));
        }
        if HeaderValue::from_str(&api_key).is_err() {
            return Err(DnsError::InvalidCredentials(
                "PowerDNS API key contains characters that cannot be sent in an HTTP header"
                    .to_string(),
            ));
        }

        let server_id = credentials
            .server_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .unwrap_or(DEFAULT_SERVER_ID)
            .to_string();
        if !server_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        {
            return Err(DnsError::InvalidCredentials(format!(
                "PowerDNS server ID '{server_id}' may only contain letters, digits, '-', '_' and '.' (a stock install uses '{DEFAULT_SERVER_ID}')"
            )));
        }

        Ok(Self {
            api_root,
            server_id,
            api_key,
            client: OnceCell::new(),
            #[cfg(test)]
            skip_address_validation: false,
        })
    }

    /// `http(s)://host[:port][/prefix]/api/v1`, from either the server root
    /// or the API URL.
    fn normalize_api_url(raw: &str) -> Result<Url, DnsError> {
        let trimmed = raw.trim();
        let invalid = |reason: String| {
            DnsError::InvalidCredentials(format!("Invalid PowerDNS API URL '{trimmed}': {reason}"))
        };
        let url = Url::parse(trimmed).map_err(|e| invalid(e.to_string()))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(invalid("the scheme must be http or https".to_string()));
        }
        if url.host_str().is_none_or(str::is_empty) {
            return Err(invalid("the URL has no host".to_string()));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(invalid(
                "credentials in the URL are not supported; put the API key in the API key field"
                    .to_string(),
            ));
        }
        if url.query().is_some() || url.fragment().is_some() {
            return Err(invalid(
                "the URL must not have a query string or fragment".to_string(),
            ));
        }
        let path = url.path().trim_end_matches('/');
        let prefix = path
            .strip_suffix("/api/v1")
            .or_else(|| path.strip_suffix("/api"))
            .unwrap_or(path);
        let mut api_root = url.clone();
        api_root.set_path(&format!("{prefix}/api/v1"));
        Ok(api_root)
    }

    /// The HTTP client, built (and the API host resolved and validated) on
    /// first use.
    async fn client(&self) -> Result<&Client, DnsError> {
        self.client.get_or_try_init(|| self.build_client()).await
    }

    async fn build_client(&self) -> Result<Client, DnsError> {
        let mut key = HeaderValue::from_str(&self.api_key).map_err(|_| {
            DnsError::InvalidCredentials(
                "PowerDNS API key contains characters that cannot be sent in an HTTP header"
                    .to_string(),
            )
        })?;
        key.set_sensitive(true);
        let mut headers = HeaderMap::new();
        headers.insert("X-API-Key", key);
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));

        let mut builder = Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .default_headers(headers)
            // A redirect would carry the API key to wherever it points, and
            // would bypass the address pinning below.
            .redirect(reqwest::redirect::Policy::none());

        if !self.address_validation_skipped() {
            // A literal IP was already checked by `new`; a hostname is
            // resolved now and the client pinned to the checked addresses
            // (DNS rebinding).
            let host = self.api_root.host_str().unwrap_or_default();
            let is_ip_literal = host
                .trim_start_matches('[')
                .trim_end_matches(']')
                .parse::<std::net::IpAddr>()
                .is_ok();
            if !is_ip_literal {
                let domain = host;
                let port = self.api_root.port_or_known_default().unwrap_or(443);
                let addresses =
                    temps_core::url_validation::resolve_and_validate_domain(domain, port)
                        .await
                        .map_err(|e| {
                            DnsError::Validation(format!(
                                "PowerDNS API URL {} is not an allowed destination: {e}",
                                self.api_root
                            ))
                        })?;
                builder = builder.resolve_to_addrs(domain, &addresses);
            }
        }

        builder.build().map_err(|e| {
            DnsError::ApiError(format!(
                "Failed to build the HTTP client for PowerDNS API {}: {e}",
                self.api_root
            ))
        })
    }

    #[cfg(test)]
    fn address_validation_skipped(&self) -> bool {
        self.skip_address_validation
    }

    #[cfg(not(test))]
    fn address_validation_skipped(&self) -> bool {
        false
    }

    /// Path-segment encoding that keeps PowerDNS zone IDs readable: they are
    /// already escaped by PowerDNS (`=2F` for `/`), so `=` must pass through.
    fn encode_segment(segment: &str) -> String {
        let mut encoded = String::with_capacity(segment.len());
        for byte in segment.bytes() {
            match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'=' => {
                    encoded.push(byte as char)
                }
                _ => encoded.push_str(&format!("%{byte:02X}")),
            }
        }
        encoded
    }

    fn server_path(&self) -> String {
        format!("/servers/{}", Self::encode_segment(&self.server_id))
    }

    fn zone_path(&self, zone_id: &str) -> String {
        format!(
            "{}/zones/{}",
            self.server_path(),
            Self::encode_segment(zone_id)
        )
    }

    /// Send one request; returns status and body whatever the status.
    async fn send<B: Serialize + ?Sized>(
        &self,
        method: Method,
        path_and_query: &str,
        body: Option<&B>,
    ) -> Result<(StatusCode, String), DnsError> {
        let client = self.client().await?;
        let url = format!(
            "{}{}",
            self.api_root.as_str().trim_end_matches('/'),
            path_and_query
        );
        debug!("PowerDNS API request: {} {}", method, url);

        let mut request = client.request(method.clone(), &url);
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request.send().await.map_err(|e| {
            DnsError::ConnectionFailed(format!(
                "PowerDNS API {method} {url} failed: {e} (check that the API URL is reachable from this server and that the PowerDNS webserver/API is enabled)"
            ))
        })?;
        let status = response.status();
        let text = response.text().await.map_err(|e| {
            DnsError::ApiError(format!(
                "Failed to read PowerDNS API response for {method} {url} (HTTP {status}): {e}"
            ))
        })?;
        Ok((status, text))
    }

    /// The `error` field of a PowerDNS error body, or the bounded raw body.
    fn error_message(body: &str) -> String {
        serde_json::from_str::<PdnsErrorBody>(body)
            .map(|parsed| parsed.error)
            .unwrap_or_else(|_| truncate_error_body(body.trim()))
    }

    /// Typed error for a non-success response.
    fn status_error(&self, operation: &str, status: StatusCode, body: &str) -> DnsError {
        let message = Self::error_message(body);
        match status {
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => DnsError::PermissionDenied(format!(
                "PowerDNS API at {} rejected the API key for {operation} (HTTP {status}: {message}); check that it matches the server's `api-key` setting",
                self.api_root
            )),
            StatusCode::TOO_MANY_REQUESTS => DnsError::RateLimited(format!(
                "PowerDNS API rate limited {operation} (HTTP {status}): {message}"
            )),
            _ => DnsError::ApiError(format!(
                "PowerDNS API returned HTTP {status} for {operation}: {message}"
            )),
        }
    }

    async fn get_json<T: serde::de::DeserializeOwned>(
        &self,
        path_and_query: &str,
        operation: &str,
    ) -> Result<T, DnsError> {
        let (status, body) = self.send(Method::GET, path_and_query, None::<&()>).await?;
        if !status.is_success() {
            return Err(self.status_error(operation, status, &body));
        }
        serde_json::from_str(&body).map_err(|e| {
            DnsError::ApiError(format!(
                "Failed to parse PowerDNS API response for {operation}: {e} - Body: {}",
                truncate_error_body(&body)
            ))
        })
    }

    /// Lowercase name without the trailing dot.
    fn normalize_name(name: &str) -> String {
        name.trim().trim_end_matches('.').to_ascii_lowercase()
    }

    /// Lowercase name with the trailing dot, as PowerDNS requires names in
    /// requests (`Example.TEST` is rejected as "not canonical").
    fn canonical_name(name: &str) -> String {
        format!("{}.", Self::normalize_name(name))
    }

    /// FQDN (normalized, no trailing dot) of a temps record name in `zone`.
    fn record_fqdn(name: &str, zone: &str) -> String {
        let name = Self::normalize_name(name);
        if name.is_empty() || name == "@" {
            zone.to_string()
        } else {
            format!("{name}.{zone}")
        }
    }

    /// Temps record name (`@` for the apex) of an FQDN in `zone`.
    fn relative_name(fqdn: &str, zone: &str) -> String {
        if fqdn == zone {
            "@".to_string()
        } else {
            fqdn.strip_suffix(&format!(".{zone}"))
                .unwrap_or(fqdn)
                .to_string()
        }
    }

    fn parse_record_type(type_str: &str) -> Option<DnsRecordType> {
        match type_str.to_ascii_uppercase().as_str() {
            "A" => Some(DnsRecordType::A),
            "AAAA" => Some(DnsRecordType::AAAA),
            "CNAME" => Some(DnsRecordType::CNAME),
            "TXT" => Some(DnsRecordType::TXT),
            "MX" => Some(DnsRecordType::MX),
            "NS" => Some(DnsRecordType::NS),
            "SRV" => Some(DnsRecordType::SRV),
            "CAA" => Some(DnsRecordType::CAA),
            "PTR" => Some(DnsRecordType::PTR),
            _ => None,
        }
    }

    /// Parse PowerDNS (zone-file presentation) content. `None` when it does
    /// not parse; numbers never fall back to a default.
    fn parse_content(record_type: DnsRecordType, content: &str) -> Option<DnsRecordContent> {
        let content = content.trim();
        let mut fields = content.split_whitespace();
        let parsed = match record_type {
            DnsRecordType::A => DnsRecordContent::A {
                address: content.to_string(),
            },
            DnsRecordType::AAAA => DnsRecordContent::AAAA {
                address: content.to_string(),
            },
            DnsRecordType::CNAME => DnsRecordContent::CNAME {
                target: Self::normalize_name(content),
            },
            DnsRecordType::NS => DnsRecordContent::NS {
                nameserver: Self::normalize_name(content),
            },
            DnsRecordType::PTR => DnsRecordContent::PTR {
                target: Self::normalize_name(content),
            },
            DnsRecordType::TXT => DnsRecordContent::TXT {
                content: decode_txt_presentation(content),
            },
            DnsRecordType::MX => {
                let priority = fields.next()?.parse().ok()?;
                let target = Self::normalize_name(fields.next()?);
                if fields.next().is_some() {
                    return None;
                }
                DnsRecordContent::MX { priority, target }
            }
            DnsRecordType::SRV => {
                let priority = fields.next()?.parse().ok()?;
                let weight = fields.next()?.parse().ok()?;
                let port = fields.next()?.parse().ok()?;
                let target = Self::normalize_name(fields.next()?);
                if fields.next().is_some() {
                    return None;
                }
                DnsRecordContent::SRV {
                    priority,
                    weight,
                    port,
                    target,
                }
            }
            DnsRecordType::CAA => {
                let mut parts = content.splitn(3, char::is_whitespace);
                let flags = parts.next()?.parse().ok()?;
                let tag = parts.next()?.to_string();
                let value = decode_txt_presentation(parts.next()?.trim());
                DnsRecordContent::CAA { flags, tag, value }
            }
        };
        Some(parsed)
    }

    /// PowerDNS content for a temps record: absolute names with a trailing
    /// dot, TXT/CAA values quoted and escaped (TXT split into ≤255-byte
    /// character-strings so an ownership marker is stored intact).
    fn format_content(content: &DnsRecordContent) -> String {
        match content {
            DnsRecordContent::A { address } | DnsRecordContent::AAAA { address } => {
                address.trim().to_string()
            }
            DnsRecordContent::CNAME { target }
            | DnsRecordContent::NS { nameserver: target }
            | DnsRecordContent::PTR { target } => Self::canonical_name(target),
            DnsRecordContent::TXT { content } => encode_txt_presentation(content),
            DnsRecordContent::MX { priority, target } => {
                format!("{priority} {}", Self::canonical_name(target))
            }
            DnsRecordContent::SRV {
                priority,
                weight,
                port,
                target,
            } => format!(
                "{priority} {weight} {port} {}",
                Self::canonical_name(target)
            ),
            DnsRecordContent::CAA { flags, tag, value } => {
                format!("{flags} {tag} {}", encode_txt_presentation(value))
            }
        }
    }

    /// Spelling-independent identity of a value, used in record IDs: the
    /// PowerDNS content of its canonical form. PowerDNS re-spells some
    /// content on storage (`2001:DB8:0::1` → `2001:db8::1`), so raw strings
    /// must never be compared directly.
    fn value_key(content: &DnsRecordContent) -> String {
        Self::format_content(&content.canonical())
    }

    /// Key of a stored PowerDNS value, or `None` if it does not parse.
    fn stored_value_key(record_type: DnsRecordType, record: &PdnsRecord) -> Option<String> {
        Self::parse_content(record_type, &record.content).map(|content| Self::value_key(&content))
    }

    /// Convert one RRset into temps records; values that do not parse are
    /// skipped. Disabled values are kept and flagged in metadata.
    fn convert_rrset(rrset: &PdnsRrset, zone: &str) -> Vec<DnsRecord> {
        let Some(record_type) = Self::parse_record_type(&rrset.record_type) else {
            return Vec::new();
        };
        let fqdn = Self::normalize_name(&rrset.name);
        let name = Self::relative_name(&fqdn, zone);
        rrset
            .records
            .iter()
            .filter_map(|record| {
                let content = Self::parse_content(record_type, &record.content)?;
                let id = RecordId {
                    fqdn: fqdn.clone(),
                    record_type,
                    value: Self::value_key(&content),
                };
                let mut metadata = HashMap::new();
                if record.disabled {
                    metadata.insert(METADATA_DISABLED.to_string(), "true".to_string());
                }
                Some(DnsRecord {
                    id: Some(id.render()),
                    zone: zone.to_string(),
                    name: name.clone(),
                    fqdn: fqdn.clone(),
                    content,
                    ttl: rrset.ttl,
                    proxied: false,
                    metadata,
                })
            })
            .collect()
    }

    fn dns_zone(zone: PdnsZoneSummary) -> DnsZone {
        let mut metadata = HashMap::new();
        metadata.insert(METADATA_ZONE_KIND.to_string(), zone.kind.clone());
        DnsZone {
            id: zone.id,
            name: Self::normalize_name(&zone.name),
            status: "active".to_string(),
            nameservers: Vec::new(),
            metadata,
        }
    }

    /// The zone named exactly `domain`, or [`DnsError::ZoneNotFound`].
    async fn resolve_zone(&self, domain: &str) -> Result<DnsZone, DnsError> {
        self.get_zone(domain)
            .await?
            .ok_or_else(|| DnsError::ZoneNotFound(domain.to_string()))
    }

    /// The zone named `domain`, refusing kinds whose records are owned by
    /// another server.
    async fn resolve_writable_zone(&self, domain: &str) -> Result<DnsZone, DnsError> {
        let zone = self.resolve_zone(domain).await?;
        let kind = zone
            .metadata
            .get(METADATA_ZONE_KIND)
            .map(String::as_str)
            .unwrap_or_default();
        if matches!(
            kind.to_ascii_lowercase().as_str(),
            "slave" | "secondary" | "consumer"
        ) {
            return Err(DnsError::NotSupported(format!(
                "PowerDNS zone {} on server '{}' is a {kind} zone: its records come from a zone transfer and a change made here would be discarded by the next one. Change records on the zone's primary server instead",
                zone.name, self.server_id
            )));
        }
        Ok(zone)
    }

    /// Every RRset of `zone`, from the unfiltered read. See the module docs
    /// for why the `rrset_name` filter is not used.
    async fn zone_rrsets(&self, zone: &DnsZone) -> Result<Vec<PdnsRrset>, DnsError> {
        let operation = format!(
            "read of zone {} on PowerDNS server '{}'",
            zone.name, self.server_id
        );
        let detail: PdnsZoneDetail = self.get_json(&self.zone_path(&zone.id), &operation).await?;
        Ok(detail.rrsets)
    }

    /// The RRset at exactly (`fqdn`, `record_type`), if any.
    async fn find_rrset(
        &self,
        zone: &DnsZone,
        fqdn: &str,
        record_type: DnsRecordType,
    ) -> Result<Option<PdnsRrset>, DnsError> {
        let type_name = record_type.to_string();
        Ok(self.zone_rrsets(zone).await?.into_iter().find(|rrset| {
            dns_names_equal(&rrset.name, fqdn) && rrset.record_type.eq_ignore_ascii_case(&type_name)
        }))
    }

    /// Lock serializing writes to one RRset in this process.
    fn write_lock(
        &self,
        zone: &DnsZone,
        fqdn: &str,
        record_type: DnsRecordType,
    ) -> &'static Mutex<()> {
        let mut hasher = DefaultHasher::new();
        (
            self.api_root.as_str(),
            self.server_id.as_str(),
            zone.id.as_str(),
            fqdn,
            record_type.to_string(),
        )
            .hash(&mut hasher);
        let index = (hasher.finish() % WRITE_LOCK_STRIPES as u64) as usize;
        &WRITE_LOCKS[index]
    }

    /// Apply one RRset change. 422 conflicts become
    /// [`DnsError::RecordConflict`]; other rejections keep PowerDNS's message.
    async fn patch_rrset(
        &self,
        zone: &DnsZone,
        name: &str,
        change: RrsetChange,
    ) -> Result<(), DnsError> {
        let record_type = change.record_type.clone();
        let operation = format!(
            "{} of {record_type} {} in zone {} on PowerDNS server '{}'",
            change.changetype, change.name, zone.name, self.server_id
        );
        let patch = RrsetPatch {
            rrsets: vec![change],
        };
        let (status, body) = self
            .send(Method::PATCH, &self.zone_path(&zone.id), Some(&patch))
            .await?;
        if status.is_success() {
            return Ok(());
        }
        if status == StatusCode::UNPROCESSABLE_ENTITY {
            let message = Self::error_message(&body);
            let lowered = message.to_ascii_lowercase();
            if lowered.contains("conflicts with")
                || lowered.contains("duplicate record")
                || lowered.contains("more than one record")
            {
                return Err(DnsError::RecordConflict {
                    domain: zone.name.clone(),
                    name: name.to_string(),
                    record_type,
                    reason: format!("PowerDNS rejected the change: {message}"),
                });
            }
            return Err(DnsError::Validation(format!(
                "PowerDNS rejected the {operation}: {message}"
            )));
        }
        Err(self.status_error(&operation, status, &body))
    }

    fn replace(
        fqdn: &str,
        record_type: DnsRecordType,
        ttl: u32,
        records: Vec<PdnsRecord>,
    ) -> RrsetChange {
        RrsetChange {
            name: Self::canonical_name(fqdn),
            record_type: record_type.to_string(),
            changetype: "REPLACE",
            ttl: Some(ttl),
            records: Some(records),
        }
    }

    fn delete(fqdn: &str, record_type: DnsRecordType) -> RrsetChange {
        RrsetChange {
            name: Self::canonical_name(fqdn),
            record_type: record_type.to_string(),
            changetype: "DELETE",
            ttl: None,
            records: None,
        }
    }

    /// `fqdn` must be the zone apex or below it.
    fn ensure_in_zone(fqdn: &str, zone: &DnsZone, record_id: &str) -> Result<(), DnsError> {
        if fqdn == zone.name || fqdn.ends_with(&format!(".{}", zone.name)) {
            Ok(())
        } else {
            Err(DnsError::Validation(format!(
                "PowerDNS record ID '{record_id}' names {fqdn}, which is not in zone {}",
                zone.name
            )))
        }
    }

    fn build_record(zone: &DnsZone, fqdn: &str, content: DnsRecordContent, ttl: u32) -> DnsRecord {
        let id = RecordId {
            fqdn: fqdn.to_string(),
            record_type: content.record_type(),
            value: Self::value_key(&content),
        };
        DnsRecord {
            id: Some(id.render()),
            zone: zone.name.clone(),
            name: Self::relative_name(fqdn, &zone.name),
            fqdn: fqdn.to_string(),
            content,
            ttl,
            proxied: false,
            metadata: HashMap::new(),
        }
    }
}

#[async_trait]
impl DnsProvider for PowerDnsProvider {
    fn provider_type(&self) -> DnsProviderType {
        DnsProviderType::PowerDns
    }

    fn capabilities(&self) -> DnsProviderCapabilities {
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

    /// Checks the server ID exists, is an Authoritative server (not a
    /// Recursor, whose API looks alike) and that the key can list zones.
    /// Failures are errors that say what to fix, not a bare `false`.
    async fn test_connection(&self) -> Result<bool, DnsError> {
        let (status, body) = self
            .send(Method::GET, &self.server_path(), None::<&()>)
            .await?;
        if status == StatusCode::NOT_FOUND {
            return Err(DnsError::InvalidCredentials(format!(
                "PowerDNS API at {} has no server '{}' (HTTP 404). A stock install uses '{DEFAULT_SERVER_ID}'; also check that the API URL points at the PowerDNS webserver (default port 8081)",
                self.api_root, self.server_id
            )));
        }
        if !status.is_success() {
            return Err(self.status_error(
                &format!("connection test of server '{}'", self.server_id),
                status,
                &body,
            ));
        }
        let server: PdnsServer = serde_json::from_str(&body).map_err(|e| {
            DnsError::ApiError(format!(
                "PowerDNS API at {} answered the server request with something that is not a PowerDNS server object: {e} - Body: {}",
                self.api_root,
                truncate_error_body(&body)
            ))
        })?;
        if !server.daemon_type.eq_ignore_ascii_case("authoritative") {
            return Err(DnsError::InvalidCredentials(format!(
                "PowerDNS API at {} is a '{}' server, not an Authoritative server; DNS records can only be managed through the PowerDNS Authoritative Server API",
                self.api_root, server.daemon_type
            )));
        }
        let zones = self.list_zones().await?;
        info!(
            "PowerDNS API connection test successful: server '{}' (PowerDNS {}) at {} with {} zone(s)",
            self.server_id,
            server.version,
            self.api_root,
            zones.len()
        );
        Ok(true)
    }

    async fn list_zones(&self) -> Result<Vec<DnsZone>, DnsError> {
        let operation = format!("zone listing on PowerDNS server '{}'", self.server_id);
        let zones: Vec<PdnsZoneSummary> = self
            .get_json(&format!("{}/zones", self.server_path()), &operation)
            .await?;
        Ok(zones.into_iter().map(Self::dns_zone).collect())
    }

    /// The zone named exactly `domain`, found with the listing's `zone`
    /// filter and matched again client-side; a parent zone is never
    /// substituted.
    async fn get_zone(&self, domain: &str) -> Result<Option<DnsZone>, DnsError> {
        let normalized = Self::normalize_name(domain);
        if normalized.is_empty() {
            return Ok(None);
        }
        let operation = format!(
            "lookup of zone {normalized} on PowerDNS server '{}'",
            self.server_id
        );
        let path = format!(
            "{}/zones?zone={}",
            self.server_path(),
            urlencoding::encode(&Self::canonical_name(&normalized))
        );
        let zones: Vec<PdnsZoneSummary> = self.get_json(&path, &operation).await?;
        Ok(zones
            .into_iter()
            .map(Self::dns_zone)
            .find(|zone| zone.name == normalized))
    }

    async fn list_records(&self, domain: &str) -> Result<Vec<DnsRecord>, DnsError> {
        let zone = self.resolve_zone(domain).await?;
        Ok(self
            .zone_rrsets(&zone)
            .await?
            .iter()
            .flat_map(|rrset| Self::convert_rrset(rrset, &zone.name))
            .collect())
    }

    async fn get_record(
        &self,
        domain: &str,
        name: &str,
        record_type: DnsRecordType,
    ) -> Result<Option<DnsRecord>, DnsError> {
        Ok(self
            .get_records(domain, name, record_type)
            .await?
            .into_iter()
            .next())
    }

    /// Every value at (name, type).
    ///
    /// An RRset holding a value temps cannot read, or a disabled value, is a
    /// conflict rather than "absent" or a partial answer: either would let an
    /// ownership-guarded write treat a name that is in use as free.
    async fn get_records(
        &self,
        domain: &str,
        name: &str,
        record_type: DnsRecordType,
    ) -> Result<Vec<DnsRecord>, DnsError> {
        let zone = self.resolve_zone(domain).await?;
        let fqdn = Self::record_fqdn(name, &zone.name);
        let Some(rrset) = self.find_rrset(&zone, &fqdn, record_type).await? else {
            return Ok(Vec::new());
        };
        let conflict = |reason: &str| DnsError::RecordConflict {
            domain: domain.to_string(),
            name: name.to_string(),
            record_type: record_type.to_string(),
            reason: reason.to_string(),
        };
        if rrset.records.iter().any(|record| record.disabled) {
            return Err(conflict(
                "the PowerDNS record set at this name and type contains a disabled value; temps will not manage a record set it cannot see completely. Enable or remove the disabled value in PowerDNS",
            ));
        }
        let records = Self::convert_rrset(&rrset, &zone.name);
        if records.len() != rrset.records.len() {
            return Err(conflict(
                "the PowerDNS record set at this name and type contains a value temps cannot read, so temps will not manage it",
            ));
        }
        Ok(records)
    }

    /// Adds one value to the (name, type) RRset, keeping every value already
    /// there. Adding a value the RRset already holds is a conflict, never a
    /// silent success: the returned ID would let a later delete remove a
    /// value temps did not create.
    ///
    /// The TTL is an RRset property: it applies when the RRset is new, and
    /// an existing RRset keeps its TTL so other values are not changed.
    async fn create_record(
        &self,
        domain: &str,
        request: DnsRecordRequest,
    ) -> Result<DnsRecord, DnsError> {
        let zone = self.resolve_writable_zone(domain).await?;
        let record_type = request.content.record_type();
        let fqdn = Self::record_fqdn(&request.name, &zone.name);
        let key = Self::value_key(&request.content);

        let _guard = self.write_lock(&zone, &fqdn, record_type).lock().await;
        let existing = self.find_rrset(&zone, &fqdn, record_type).await?;
        let (ttl, mut records) = match existing {
            Some(rrset) if !rrset.records.is_empty() => {
                if rrset.records.iter().any(|record| {
                    Self::stored_value_key(record_type, record).as_deref() == Some(key.as_str())
                }) {
                    return Err(DnsError::RecordConflict {
                        domain: domain.to_string(),
                        name: request.name.clone(),
                        record_type: record_type.to_string(),
                        reason: "PowerDNS already holds this exact value at this name and type, and a create never claims a value it did not add".to_string(),
                    });
                }
                if let Some(requested) = request.ttl.filter(|ttl| *ttl != rrset.ttl) {
                    debug!(
                        "PowerDNS RRset {} {} keeps its TTL {} (requested {}): it holds other values",
                        fqdn, record_type, rrset.ttl, requested
                    );
                }
                (rrset.ttl, rrset.records)
            }
            _ => (request.ttl.unwrap_or(DEFAULT_TTL), Vec::new()),
        };
        records.push(PdnsRecord {
            content: Self::format_content(&request.content),
            disabled: false,
        });

        self.patch_rrset(
            &zone,
            &request.name,
            Self::replace(&fqdn, record_type, ttl, records),
        )
        .await?;
        info!(
            "Created PowerDNS {} record {} in zone {}",
            record_type, fqdn, zone.name
        );
        Ok(Self::build_record(&zone, &fqdn, request.content, ttl))
    }

    /// Replaces the one value `record_id` names, keeping the RRset's other
    /// values. The request must target the same (name, type) as the ID.
    async fn update_record(
        &self,
        domain: &str,
        record_id: &str,
        request: DnsRecordRequest,
    ) -> Result<DnsRecord, DnsError> {
        let zone = self.resolve_writable_zone(domain).await?;
        let id = RecordId::parse(record_id, &zone.name)?;
        Self::ensure_in_zone(&id.fqdn, &zone, record_id)?;
        let record_type = request.content.record_type();
        let fqdn = Self::record_fqdn(&request.name, &zone.name);
        if fqdn != id.fqdn || record_type != id.record_type {
            return Err(DnsError::Validation(format!(
                "PowerDNS record {record_id} is {} {}, but the update targets {record_type} {fqdn}; a value cannot move to another name or type, delete it and create the new one",
                id.record_type, id.fqdn
            )));
        }
        let new_key = Self::value_key(&request.content);

        let _guard = self.write_lock(&zone, &fqdn, record_type).lock().await;
        let rrset = self
            .find_rrset(&zone, &fqdn, record_type)
            .await?
            .ok_or_else(|| DnsError::RecordNotFound(record_id.to_string()))?;
        let keys: Vec<Option<String>> = rrset
            .records
            .iter()
            .map(|record| Self::stored_value_key(record_type, record))
            .collect();
        let position = keys
            .iter()
            .position(|key| key.as_deref() == Some(id.value.as_str()))
            .ok_or_else(|| DnsError::RecordNotFound(record_id.to_string()))?;
        if new_key != id.value
            && keys
                .iter()
                .any(|key| key.as_deref() == Some(new_key.as_str()))
        {
            return Err(DnsError::RecordConflict {
                domain: domain.to_string(),
                name: request.name.clone(),
                record_type: record_type.to_string(),
                reason:
                    "another value in this PowerDNS record set already holds the requested content"
                        .to_string(),
            });
        }

        let ttl = if rrset.records.len() == 1 {
            request.ttl.unwrap_or(rrset.ttl)
        } else {
            rrset.ttl
        };
        let mut records = rrset.records;
        records[position] = PdnsRecord {
            content: Self::format_content(&request.content),
            disabled: false,
        };

        self.patch_rrset(
            &zone,
            &request.name,
            Self::replace(&fqdn, record_type, ttl, records),
        )
        .await?;
        info!(
            "Updated PowerDNS {} record {} in zone {}",
            record_type, fqdn, zone.name
        );
        Ok(Self::build_record(&zone, &fqdn, request.content, ttl))
    }

    /// Removes only the value `record_id` names; the RRset is deleted when
    /// that was its last value. A value that is already gone is
    /// [`DnsError::RecordNotFound`].
    async fn delete_record(&self, domain: &str, record_id: &str) -> Result<(), DnsError> {
        let zone = self.resolve_writable_zone(domain).await?;
        let id = RecordId::parse(record_id, &zone.name)?;
        Self::ensure_in_zone(&id.fqdn, &zone, record_id)?;
        let name = Self::relative_name(&id.fqdn, &zone.name);

        let _guard = self
            .write_lock(&zone, &id.fqdn, id.record_type)
            .lock()
            .await;
        let rrset = self
            .find_rrset(&zone, &id.fqdn, id.record_type)
            .await?
            .ok_or_else(|| DnsError::RecordNotFound(record_id.to_string()))?;
        let before = rrset.records.len();
        let remaining: Vec<PdnsRecord> = rrset
            .records
            .into_iter()
            .filter(|record| {
                Self::stored_value_key(id.record_type, record).as_deref() != Some(id.value.as_str())
            })
            .collect();
        if remaining.len() == before {
            return Err(DnsError::RecordNotFound(record_id.to_string()));
        }

        let change = if remaining.is_empty() {
            Self::delete(&id.fqdn, id.record_type)
        } else {
            Self::replace(&id.fqdn, id.record_type, rrset.ttl, remaining)
        };
        self.patch_rrset(&zone, &name, change).await?;
        info!(
            "Deleted PowerDNS {} record value from {} in zone {}",
            id.record_type, id.fqdn, zone.name
        );
        Ok(())
    }

    /// Deletes the whole (name, type) RRset in one change.
    async fn remove_record(
        &self,
        domain: &str,
        name: &str,
        record_type: DnsRecordType,
    ) -> Result<(), DnsError> {
        let zone = self.resolve_writable_zone(domain).await?;
        let fqdn = Self::record_fqdn(name, &zone.name);
        let _guard = self.write_lock(&zone, &fqdn, record_type).lock().await;
        if self.find_rrset(&zone, &fqdn, record_type).await?.is_none() {
            return Ok(());
        }
        self.patch_rrset(&zone, name, Self::delete(&fqdn, record_type))
            .await?;
        info!(
            "Removed PowerDNS {} record set {} in zone {}",
            record_type, fqdn, zone.name
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn credentials(api_url: &str) -> PowerDnsCredentials {
        PowerDnsCredentials {
            api_url: api_url.to_string(),
            api_key: "test-key".to_string(),
            server_id: None,
        }
    }

    #[test]
    fn api_url_accepts_the_server_root_or_the_api_path() {
        for (input, expected) in [
            ("http://10.0.0.5:8081", "http://10.0.0.5:8081/api/v1"),
            ("http://10.0.0.5:8081/", "http://10.0.0.5:8081/api/v1"),
            ("http://10.0.0.5:8081/api", "http://10.0.0.5:8081/api/v1"),
            (
                "http://10.0.0.5:8081/api/v1/",
                "http://10.0.0.5:8081/api/v1",
            ),
            (
                " https://dns.example.net/pdns/api/v1 ",
                "https://dns.example.net/pdns/api/v1",
            ),
        ] {
            let url = PowerDnsProvider::normalize_api_url(input).unwrap();
            assert_eq!(url.as_str(), expected, "input {input:?}");
        }
    }

    #[test]
    fn api_url_rejects_unusable_urls() {
        for input in [
            "",
            "pdns:8081",
            "ftp://10.0.0.5",
            "http://user:pass@10.0.0.5:8081",
            "http://10.0.0.5:8081/api/v1?x=1",
        ] {
            assert!(
                matches!(
                    PowerDnsProvider::normalize_api_url(input),
                    Err(DnsError::InvalidCredentials(_))
                ),
                "{input:?} should be rejected"
            );
        }
    }

    #[test]
    fn new_refuses_cloud_metadata_and_untrusted_private_addresses() {
        let metadata = PowerDnsProvider::new(credentials("http://169.254.169.254:8081"))
            .err()
            .expect("metadata endpoint must be refused");
        assert!(
            metadata.to_string().contains("not an allowed destination"),
            "{metadata}"
        );

        // A private address is refused until an operator trusts its network
        // (Settings > Security > Trusted private networks).
        let private = PowerDnsProvider::new(credentials("http://10.20.30.40:8081"))
            .err()
            .expect("an untrusted private address must be refused");
        assert!(
            private.to_string().contains("Trusted private networks"),
            "{private}"
        );
    }

    #[test]
    fn new_validates_the_key_and_server_id() {
        let mut empty_key = credentials("http://203.0.113.10:8081");
        empty_key.api_key = "  ".to_string();
        assert!(matches!(
            PowerDnsProvider::new_unchecked(empty_key),
            Err(DnsError::InvalidCredentials(_))
        ));

        let mut bad_server = credentials("http://203.0.113.10:8081");
        bad_server.server_id = Some("../zones".to_string());
        assert!(matches!(
            PowerDnsProvider::new_unchecked(bad_server),
            Err(DnsError::InvalidCredentials(_))
        ));

        let mut blank_server = credentials("http://203.0.113.10:8081");
        blank_server.server_id = Some(" ".to_string());
        let provider = PowerDnsProvider::new_unchecked(blank_server).unwrap();
        assert_eq!(provider.server_id, DEFAULT_SERVER_ID);
    }

    #[test]
    fn record_ids_round_trip_and_keep_ipv6_values_intact() {
        let id = RecordId {
            fqdn: "app.example.com".to_string(),
            record_type: DnsRecordType::AAAA,
            value: "2001:db8::1".to_string(),
        };
        let rendered = id.render();
        assert_eq!(rendered, "app.example.com::AAAA::2001:db8::1");
        assert_eq!(RecordId::parse(&rendered, "example.com").unwrap(), id);

        for invalid in [
            "app.example.com",
            "app.example.com::AAAA",
            "app.example.com::ALIAS::x",
            "::A::192.0.2.1",
        ] {
            assert!(
                matches!(
                    RecordId::parse(invalid, "example.com"),
                    Err(DnsError::Validation(_))
                ),
                "{invalid:?}"
            );
        }
    }

    #[test]
    fn value_keys_ignore_powerdns_respelling() {
        // PowerDNS stores `2001:DB8:0::1` as `2001:db8::1` and keeps the
        // case of names; the key must match either way.
        let sent = DnsRecordContent::AAAA {
            address: "2001:DB8:0::1".to_string(),
        };
        let stored = PdnsRecord {
            content: "2001:db8::1".to_string(),
            disabled: false,
        };
        assert_eq!(
            PowerDnsProvider::stored_value_key(DnsRecordType::AAAA, &stored),
            Some(PowerDnsProvider::value_key(&sent))
        );

        let mx = PdnsRecord {
            content: "10 Mail.Example.NET.".to_string(),
            disabled: false,
        };
        assert_eq!(
            PowerDnsProvider::stored_value_key(DnsRecordType::MX, &mx).as_deref(),
            Some("10 mail.example.net.")
        );
    }

    #[test]
    fn content_round_trips_through_powerdns_presentation_format() {
        let contents = [
            DnsRecordContent::A {
                address: "192.0.2.1".to_string(),
            },
            DnsRecordContent::CNAME {
                target: "origin.example.net".to_string(),
            },
            DnsRecordContent::TXT {
                content: format!("quote\" backslash\\ é {}", "x".repeat(400)),
            },
            DnsRecordContent::MX {
                priority: 10,
                target: "mail.example.net".to_string(),
            },
            DnsRecordContent::SRV {
                priority: 1,
                weight: 2,
                port: 443,
                target: "svc.example.net".to_string(),
            },
            DnsRecordContent::CAA {
                flags: 0,
                tag: "issue".to_string(),
                value: "letsencrypt.org".to_string(),
            },
            DnsRecordContent::NS {
                nameserver: "ns1.example.net".to_string(),
            },
            DnsRecordContent::PTR {
                target: "host.example.net".to_string(),
            },
        ];
        for content in contents {
            let formatted = PowerDnsProvider::format_content(&content);
            let parsed = PowerDnsProvider::parse_content(content.record_type(), &formatted)
                .unwrap_or_else(|| panic!("{formatted:?} did not parse"));
            assert_eq!(parsed, content, "{formatted:?}");
        }
        assert_eq!(
            PowerDnsProvider::format_content(&DnsRecordContent::CNAME {
                target: "Origin.Example.NET".to_string()
            }),
            "origin.example.net."
        );
    }

    #[test]
    fn malformed_numeric_content_does_not_parse() {
        assert!(
            PowerDnsProvider::parse_content(DnsRecordType::MX, "ten mail.example.net.").is_none()
        );
        assert!(
            PowerDnsProvider::parse_content(DnsRecordType::SRV, "1 2 mail.example.net.").is_none()
        );
        assert!(PowerDnsProvider::parse_content(DnsRecordType::CAA, "x issue \"ca\"").is_none());
    }
}

#[cfg(test)]
mod api_tests {
    use super::*;
    use serde_json::{json, Value};
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    const ZONES: &str = "/api/v1/servers/localhost/zones";
    const ZONE: &str = "/api/v1/servers/localhost/zones/example.com.";

    fn provider(server: &MockServer) -> PowerDnsProvider {
        PowerDnsProvider::new_unchecked(PowerDnsCredentials {
            api_url: server.uri(),
            api_key: "test-key".to_string(),
            server_id: None,
        })
        .unwrap()
    }

    fn zone_summary(kind: &str) -> Value {
        json!({"id": "example.com.", "name": "example.com.", "kind": kind, "url": ZONE})
    }

    async fn mount_zone(server: &MockServer, kind: &str, rrsets: Value) {
        Mock::given(method("GET"))
            .and(path(ZONES))
            .and(query_param("zone", "example.com."))
            .and(header("X-API-Key", "test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([zone_summary(kind)])))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path(ZONE))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "example.com.", "name": "example.com.", "kind": kind, "rrsets": rrsets
            })))
            .mount(server)
            .await;
    }

    async fn mount_patch(server: &MockServer) {
        Mock::given(method("PATCH"))
            .and(path(ZONE))
            .respond_with(ResponseTemplate::new(204))
            .mount(server)
            .await;
    }

    async fn patches(server: &MockServer) -> Vec<Value> {
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|request: &&Request| request.method.as_str() == "PATCH")
            .map(|request| serde_json::from_slice(&request.body).unwrap())
            .collect()
    }

    fn acme_rrset() -> Value {
        json!([{
            "name": "_acme-challenge.example.com.",
            "type": "TXT",
            "ttl": 120,
            "records": [
                {"content": "\"sibling-order\"", "disabled": false},
                {"content": "\"parked\"", "disabled": true}
            ],
            "comments": [{"content": "keep me", "account": "ops", "modified_at": 1}]
        }])
    }

    fn txt_request(value: &str) -> DnsRecordRequest {
        DnsRecordRequest {
            name: "_acme-challenge".to_string(),
            content: DnsRecordContent::TXT {
                content: value.to_string(),
            },
            ttl: Some(60),
            proxied: false,
        }
    }

    #[tokio::test]
    async fn create_appends_and_keeps_every_existing_value_verbatim() {
        let server = MockServer::start().await;
        mount_zone(&server, "Native", acme_rrset()).await;
        mount_patch(&server).await;

        let record = provider(&server)
            .create_record("example.com", txt_request("this-order"))
            .await
            .unwrap();

        assert_eq!(
            record.id.as_deref(),
            Some("_acme-challenge.example.com::TXT::\"this-order\"")
        );
        // The RRset already existed, so its TTL is kept for its other values.
        assert_eq!(record.ttl, 120);
        let patches = patches(&server).await;
        assert_eq!(patches.len(), 1);
        assert_eq!(
            patches[0],
            json!({"rrsets": [{
                "name": "_acme-challenge.example.com.",
                "type": "TXT",
                "changetype": "REPLACE",
                "ttl": 120,
                "records": [
                    {"content": "\"sibling-order\"", "disabled": false},
                    {"content": "\"parked\"", "disabled": true},
                    {"content": "\"this-order\"", "disabled": false}
                ]
            }]})
        );
        // `comments` is never sent, so PowerDNS keeps the existing ones.
        assert!(patches[0]["rrsets"][0].get("comments").is_none());
    }

    #[tokio::test]
    async fn create_of_a_new_rrset_uses_the_requested_ttl() {
        let server = MockServer::start().await;
        mount_zone(&server, "Master", json!([])).await;
        mount_patch(&server).await;

        let record = provider(&server)
            .create_record("example.com", txt_request("first"))
            .await
            .unwrap();

        assert_eq!(record.ttl, 60);
        assert_eq!(record.name, "_acme-challenge");
        assert_eq!(patches(&server).await[0]["rrsets"][0]["ttl"], 60);
    }

    #[tokio::test]
    async fn create_of_a_value_already_present_is_a_conflict() {
        let server = MockServer::start().await;
        mount_zone(&server, "Native", acme_rrset()).await;
        mount_patch(&server).await;

        let error = provider(&server)
            .create_record("example.com", txt_request("sibling-order"))
            .await
            .unwrap_err();

        assert!(matches!(error, DnsError::RecordConflict { .. }), "{error}");
        assert!(patches(&server).await.is_empty());
    }

    #[tokio::test]
    async fn delete_removes_only_the_named_value() {
        let server = MockServer::start().await;
        mount_zone(&server, "Native", acme_rrset()).await;
        mount_patch(&server).await;

        provider(&server)
            .delete_record(
                "example.com",
                "_acme-challenge.example.com::TXT::\"sibling-order\"",
            )
            .await
            .unwrap();

        assert_eq!(
            patches(&server).await[0]["rrsets"][0]["records"],
            json!([{"content": "\"parked\"", "disabled": true}])
        );
    }

    #[tokio::test]
    async fn delete_of_the_last_value_deletes_the_rrset() {
        let server = MockServer::start().await;
        mount_zone(
            &server,
            "Native",
            json!([{"name": "app.example.com.", "type": "A", "ttl": 300,
                    "records": [{"content": "192.0.2.1", "disabled": false}]}]),
        )
        .await;
        mount_patch(&server).await;

        provider(&server)
            .delete_record("example.com", "app.example.com::A::192.0.2.1")
            .await
            .unwrap();

        assert_eq!(
            patches(&server).await[0],
            json!({"rrsets": [{"name": "app.example.com.", "type": "A", "changetype": "DELETE"}]})
        );
    }

    #[tokio::test]
    async fn delete_of_a_missing_value_is_record_not_found() {
        let server = MockServer::start().await;
        mount_zone(&server, "Native", acme_rrset()).await;
        mount_patch(&server).await;
        let provider = provider(&server);

        for id in [
            "_acme-challenge.example.com::TXT::\"already-gone\"",
            "other.example.com::TXT::\"x\"",
        ] {
            let error = provider.delete_record("example.com", id).await.unwrap_err();
            assert!(matches!(error, DnsError::RecordNotFound(_)), "{error}");
        }
        let error = provider
            .delete_record("example.com", "app.example.org::A::192.0.2.1")
            .await
            .unwrap_err();
        assert!(matches!(error, DnsError::Validation(_)), "{error}");
        assert!(patches(&server).await.is_empty());
    }

    #[tokio::test]
    async fn update_replaces_one_value_in_place() {
        let server = MockServer::start().await;
        mount_zone(
            &server,
            "Native",
            json!([{"name": "app.example.com.", "type": "A", "ttl": 300, "records": [
                {"content": "192.0.2.1", "disabled": false},
                {"content": "192.0.2.2", "disabled": false}
            ]}]),
        )
        .await;
        mount_patch(&server).await;
        let request = DnsRecordRequest {
            name: "app".to_string(),
            content: DnsRecordContent::A {
                address: "192.0.2.9".to_string(),
            },
            ttl: Some(60),
            proxied: false,
        };

        let record = provider(&server)
            .update_record(
                "example.com",
                "app.example.com::A::192.0.2.2",
                request.clone(),
            )
            .await
            .unwrap();

        assert_eq!(record.id.as_deref(), Some("app.example.com::A::192.0.2.9"));
        assert_eq!(record.ttl, 300, "a shared RRset keeps its TTL");
        assert_eq!(
            patches(&server).await[0]["rrsets"][0]["records"],
            json!([
                {"content": "192.0.2.1", "disabled": false},
                {"content": "192.0.2.9", "disabled": false}
            ])
        );

        let moved = provider(&server)
            .update_record("example.com", "www.example.com::A::192.0.2.2", request)
            .await
            .unwrap_err();
        assert!(matches!(moved, DnsError::Validation(_)), "{moved}");
    }

    #[tokio::test]
    async fn get_records_refuses_an_rrset_with_a_disabled_value() {
        let server = MockServer::start().await;
        mount_zone(&server, "Native", acme_rrset()).await;

        let error = provider(&server)
            .get_records("example.com", "_acme-challenge", DnsRecordType::TXT)
            .await
            .unwrap_err();
        assert!(matches!(error, DnsError::RecordConflict { .. }), "{error}");

        // The listing still shows it, flagged.
        let records = provider(&server).list_records("example.com").await.unwrap();
        let parked = records
            .iter()
            .find(|record| record.content.to_value_string() == "parked")
            .unwrap();
        assert_eq!(
            parked.metadata.get(METADATA_DISABLED).map(String::as_str),
            Some("true")
        );
    }

    #[tokio::test]
    async fn get_records_matches_names_case_insensitively() {
        let server = MockServer::start().await;
        mount_zone(
            &server,
            "Native",
            json!([{"name": "App.Example.com.", "type": "CNAME", "ttl": 300,
                    "records": [{"content": "Origin.Example.NET.", "disabled": false}]}]),
        )
        .await;

        let records = provider(&server)
            .get_records("example.com", "app", DnsRecordType::CNAME)
            .await
            .unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].name, "app");
        assert_eq!(
            records[0].content,
            DnsRecordContent::CNAME {
                target: "origin.example.net".to_string()
            }
        );
    }

    #[tokio::test]
    async fn remove_record_deletes_the_rrset_in_one_change() {
        let server = MockServer::start().await;
        mount_zone(&server, "Native", acme_rrset()).await;
        mount_patch(&server).await;

        provider(&server)
            .remove_record("example.com", "_acme-challenge", DnsRecordType::TXT)
            .await
            .unwrap();
        // Absent RRset: no write at all.
        provider(&server)
            .remove_record("example.com", "nothing-here", DnsRecordType::TXT)
            .await
            .unwrap();

        let patches = patches(&server).await;
        assert_eq!(patches.len(), 1);
        assert_eq!(patches[0]["rrsets"][0]["changetype"], "DELETE");
    }

    #[tokio::test]
    async fn writes_to_a_secondary_zone_are_refused() {
        let server = MockServer::start().await;
        mount_zone(&server, "Slave", json!([])).await;
        mount_patch(&server).await;

        let error = provider(&server)
            .create_record("example.com", txt_request("x"))
            .await
            .unwrap_err();
        assert!(matches!(error, DnsError::NotSupported(_)), "{error}");
        assert!(error.to_string().contains("primary"), "{error}");
        assert!(patches(&server).await.is_empty());
    }

    #[tokio::test]
    async fn powerdns_rejections_keep_their_message() {
        let server = MockServer::start().await;
        mount_zone(&server, "Native", json!([])).await;
        Mock::given(method("PATCH"))
            .and(path(ZONE))
            .respond_with(ResponseTemplate::new(422).set_body_json(json!({
                "error": "RRset app.example.com. IN CNAME: Conflicts with pre-existing RRset"
            })))
            .mount(&server)
            .await;
        let request = DnsRecordRequest {
            name: "app".to_string(),
            content: DnsRecordContent::CNAME {
                target: "origin.example.net".to_string(),
            },
            ttl: None,
            proxied: false,
        };

        let error = provider(&server)
            .create_record("example.com", request)
            .await
            .unwrap_err();
        assert!(matches!(error, DnsError::RecordConflict { .. }), "{error}");
        assert!(error
            .to_string()
            .contains("Conflicts with pre-existing RRset"));
    }

    #[tokio::test]
    async fn get_zone_matches_exactly_and_never_substitutes_a_parent() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(ZONES))
            .and(query_param("zone", "app.example.com."))
            // A server that ignored the filter would return other zones.
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([zone_summary("Native")])))
            .mount(&server)
            .await;

        assert!(provider(&server)
            .get_zone("App.Example.com.")
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn test_connection_explains_each_failure() {
        let server = MockServer::start().await;
        let provider = provider(&server);

        Mock::given(method("GET"))
            .and(path("/api/v1/servers/localhost"))
            .respond_with(ResponseTemplate::new(401).set_body_string("Unauthorized"))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        let error = provider.test_connection().await.unwrap_err();
        assert!(matches!(error, DnsError::PermissionDenied(_)), "{error}");
        assert!(error.to_string().contains("api-key"), "{error}");

        Mock::given(method("GET"))
            .and(path("/api/v1/servers/localhost"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"id": "localhost", "daemon_type": "recursor"})),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;
        let error = provider.test_connection().await.unwrap_err();
        assert!(
            error.to_string().contains("not an Authoritative"),
            "{error}"
        );

        Mock::given(method("GET"))
            .and(path("/api/v1/servers/localhost"))
            .respond_with(ResponseTemplate::new(404).set_body_string("Not Found"))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        let error = provider.test_connection().await.unwrap_err();
        assert!(
            error.to_string().contains("no server 'localhost'"),
            "{error}"
        );

        Mock::given(method("GET"))
            .and(path("/api/v1/servers/localhost"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"id": "localhost", "daemon_type": "authoritative", "version": "4.9.20"}),
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(ZONES))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([zone_summary("Native")])))
            .mount(&server)
            .await;
        assert!(provider.test_connection().await.unwrap());
    }

    #[tokio::test]
    async fn redirects_are_not_followed() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(ZONES))
            .respond_with(
                ResponseTemplate::new(302).insert_header("Location", "http://169.254.169.254/"),
            )
            .mount(&server)
            .await;

        let error = provider(&server).list_zones().await.unwrap_err();
        assert!(error.to_string().contains("302"), "{error}");
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn concurrent_creates_on_one_rrset_are_serialized() {
        // Each write must read the RRset after the previous write landed;
        // the mock records the PATCHes so the second must contain the first.
        use std::sync::{Arc, Mutex as StdMutex};

        let server = MockServer::start().await;
        let state: Arc<StdMutex<Vec<Value>>> = Arc::new(StdMutex::new(Vec::new()));
        Mock::given(method("GET"))
            .and(path(ZONES))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([zone_summary("Native")])))
            .mount(&server)
            .await;
        let read_state = state.clone();
        Mock::given(method("GET"))
            .and(path(ZONE))
            .respond_with(move |_: &Request| {
                let records = read_state.lock().unwrap().clone();
                let rrsets = if records.is_empty() {
                    json!([])
                } else {
                    json!([{"name": "_acme-challenge.example.com.", "type": "TXT",
                            "ttl": 60, "records": records}])
                };
                ResponseTemplate::new(200)
                    .set_body_json(json!({"rrsets": rrsets}))
                    .set_delay(Duration::from_millis(30))
            })
            .mount(&server)
            .await;
        let write_state = state.clone();
        Mock::given(method("PATCH"))
            .and(path(ZONE))
            .respond_with(move |request: &Request| {
                let body: Value = serde_json::from_slice(&request.body).unwrap();
                let records = body["rrsets"][0]["records"].as_array().cloned().unwrap();
                *write_state.lock().unwrap() = records;
                ResponseTemplate::new(204)
            })
            .mount(&server)
            .await;

        let first = provider(&server);
        let second = provider(&server);
        let (a, b) = tokio::join!(
            first.create_record("example.com", txt_request("apex-authorization")),
            second.create_record("example.com", txt_request("wildcard-authorization")),
        );
        a.unwrap();
        b.unwrap();

        let stored = state.lock().unwrap().clone();
        assert_eq!(stored.len(), 2, "a concurrent append was lost: {stored:?}");
    }
}

/// Against a real PowerDNS Authoritative Server in Docker. Skips (passes)
/// only when no Docker daemon answers a ping; any later setup failure fails.
#[cfg(test)]
mod real_server_tests {
    use super::*;
    use serde_json::{json, Value};
    use testcontainers::core::{IntoContainerPort, WaitFor};
    use testcontainers::runners::AsyncRunner;
    use testcontainers::{ContainerAsync, GenericImage, ImageExt};

    const API_KEY: &str = "integration-test-key";

    /// Whether a Docker daemon answers. Only an unreachable daemon is a reason
    /// to skip: once Docker is up, every setup failure (image pull, readiness
    /// message, startup timeout, port mapping) fails the test instead of
    /// silently passing it.
    async fn docker_reachable() -> bool {
        match bollard::Docker::connect_with_defaults() {
            Ok(docker) => docker.ping().await.is_ok(),
            Err(_) => false,
        }
    }

    async fn start_powerdns() -> Option<(ContainerAsync<GenericImage>, String)> {
        if !docker_reachable().await {
            eprintln!("⏭️  Docker unavailable, skipping PowerDNS integration test");
            return None;
        }
        let container = match GenericImage::new("powerdns/pdns-auth-49", "4.9.20")
            .with_exposed_port(8081.tcp())
            .with_wait_for(WaitFor::message_on_either_std(
                "Done launching threads, ready to distribute questions",
            ))
            .with_env_var("PDNS_AUTH_API_KEY", API_KEY)
            .with_startup_timeout(Duration::from_secs(120))
            .start()
            .await
        {
            Ok(container) => container,
            Err(e) => {
                panic!("PowerDNS container powerdns/pdns-auth-49:4.9.20 failed to start: {e}")
            }
        };
        let port = match container.get_host_port_ipv4(8081).await {
            Ok(port) => port,
            Err(e) => panic!("PowerDNS API port 8081 is not mapped to the host: {e}"),
        };
        Some((container, format!("http://127.0.0.1:{port}")))
    }

    async fn admin(base: &str, method: Method, path: &str, body: Value) -> (StatusCode, String) {
        let response = Client::new()
            .request(method, format!("{base}/api/v1/servers/localhost{path}"))
            .header("X-API-Key", API_KEY)
            .json(&body)
            .send()
            .await
            .unwrap();
        let status = response.status();
        (status, response.text().await.unwrap())
    }

    async fn rrset(base: &str, fqdn: &str, record_type: &str) -> Option<Value> {
        let response = Client::new()
            .get(format!(
                "{base}/api/v1/servers/localhost/zones/example.test."
            ))
            .header("X-API-Key", API_KEY)
            .send()
            .await
            .unwrap();
        let zone: Value = response.json().await.unwrap();
        zone["rrsets"]
            .as_array()
            .unwrap()
            .iter()
            .find(|rrset| rrset["name"] == fqdn && rrset["type"] == record_type)
            .cloned()
    }

    fn txt(value: &str) -> DnsRecordRequest {
        DnsRecordRequest {
            name: "_acme-challenge".to_string(),
            content: DnsRecordContent::TXT {
                content: value.to_string(),
            },
            ttl: Some(120),
            proxied: false,
        }
    }

    #[tokio::test]
    async fn wildcard_acme_lifecycle_against_a_real_powerdns() {
        let Some((_container, base)) = start_powerdns().await else {
            return;
        };
        let (status, body) = admin(
            &base,
            Method::POST,
            "/zones",
            json!({"name": "example.test.", "kind": "Native", "nameservers": ["ns1.example.test."]}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        // A user's own value, disabled, with a comment: all must survive.
        let (status, body) = admin(
            &base,
            Method::PATCH,
            "/zones/example.test.",
            json!({"rrsets": [{
                "name": "_acme-challenge.example.test.", "type": "TXT", "ttl": 300,
                "changetype": "REPLACE",
                "records": [{"content": "\"user-parked\"", "disabled": true}],
                "comments": [{"content": "do not touch", "account": "ops"}]
            }]}),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

        let provider = PowerDnsProvider::new_unchecked(PowerDnsCredentials {
            api_url: base.clone(),
            api_key: API_KEY.to_string(),
            server_id: None,
        })
        .unwrap();
        assert!(provider.test_connection().await.unwrap());
        assert!(provider.can_manage_domain("example.test").await);
        assert!(!provider.can_manage_domain("missing.test").await);

        // A wildcard + apex order publishes two values under one name.
        let marker = format!("temps-ownership {}", "m".repeat(400));
        let apex = provider
            .create_record("example.test", txt("apex-token"))
            .await
            .unwrap();
        let wildcard = provider
            .create_record("example.test", txt("wildcard-token"))
            .await
            .unwrap();
        let long = provider
            .create_record("example.test", txt(&marker))
            .await
            .unwrap();

        let records = provider.list_records("example.test").await.unwrap();
        let values: Vec<String> = records
            .iter()
            .filter(|record| record.name == "_acme-challenge")
            .map(|record| record.content.to_value_string())
            .collect();
        assert!(values.contains(&"apex-token".to_string()), "{values:?}");
        assert!(values.contains(&"wildcard-token".to_string()), "{values:?}");
        assert!(
            values.contains(&marker),
            "a >255-byte TXT must round-trip intact"
        );
        // Every ID a create returned is the ID the listing reports.
        for created in [&apex, &wildcard, &long] {
            assert!(
                records.iter().any(|record| record.id == created.id),
                "{:?} not listed",
                created.id
            );
        }

        // Cleanup of one authorization leaves the sibling in place.
        provider
            .delete_record("example.test", apex.id.as_deref().unwrap())
            .await
            .unwrap();
        let stored = rrset(&base, "_acme-challenge.example.test.", "TXT")
            .await
            .unwrap();
        let contents: Vec<&str> = stored["records"]
            .as_array()
            .unwrap()
            .iter()
            .map(|record| record["content"].as_str().unwrap())
            .collect();
        assert!(!contents.contains(&"\"apex-token\""), "{contents:?}");
        assert!(contents.contains(&"\"wildcard-token\""), "{contents:?}");
        assert!(
            contents.contains(&"\"user-parked\""),
            "disabled value lost: {contents:?}"
        );
        assert_eq!(stored["comments"][0]["content"], "do not touch");
        assert_eq!(stored["ttl"], 300, "the existing RRset keeps its TTL");

        // Deleting it again is RecordNotFound (ACME treats that as done).
        let again = provider
            .delete_record("example.test", apex.id.as_deref().unwrap())
            .await
            .unwrap_err();
        assert!(matches!(again, DnsError::RecordNotFound(_)), "{again}");

        // Ownership-guarded reads refuse the RRset holding a disabled value.
        let guarded = provider
            .get_records("example.test", "_acme-challenge", DnsRecordType::TXT)
            .await
            .unwrap_err();
        assert!(
            matches!(guarded, DnsError::RecordConflict { .. }),
            "{guarded}"
        );

        // Managed records: A with update in place, CNAME conflicts.
        let a = provider
            .create_record(
                "example.test",
                DnsRecordRequest {
                    name: "app".to_string(),
                    content: DnsRecordContent::A {
                        address: "192.0.2.10".to_string(),
                    },
                    ttl: Some(60),
                    proxied: false,
                },
            )
            .await
            .unwrap();
        let updated = provider
            .update_record(
                "example.test",
                a.id.as_deref().unwrap(),
                DnsRecordRequest {
                    name: "app".to_string(),
                    content: DnsRecordContent::A {
                        address: "192.0.2.11".to_string(),
                    },
                    ttl: Some(90),
                    proxied: false,
                },
            )
            .await
            .unwrap();
        let fetched = provider
            .get_records("example.test", "APP", DnsRecordType::A)
            .await
            .unwrap();
        assert_eq!(fetched.len(), 1);
        assert_eq!(fetched[0].id, updated.id);
        assert_eq!(fetched[0].ttl, 90);
        let cname = provider
            .create_record(
                "example.test",
                DnsRecordRequest {
                    name: "app".to_string(),
                    content: DnsRecordContent::CNAME {
                        target: "origin.example.net".to_string(),
                    },
                    ttl: None,
                    proxied: false,
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(cname, DnsError::RecordConflict { .. }), "{cname}");

        // AAAA is re-spelled by PowerDNS; the ID must still delete it.
        let aaaa = provider
            .create_record(
                "example.test",
                DnsRecordRequest {
                    name: "v6".to_string(),
                    content: DnsRecordContent::AAAA {
                        address: "2001:DB8:0::1".to_string(),
                    },
                    ttl: None,
                    proxied: false,
                },
            )
            .await
            .unwrap();
        provider
            .delete_record("example.test", aaaa.id.as_deref().unwrap())
            .await
            .unwrap();
        assert!(rrset(&base, "v6.example.test.", "AAAA").await.is_none());

        // Stale-challenge removal drops the whole RRset.
        provider
            .remove_record("example.test", "_acme-challenge", DnsRecordType::TXT)
            .await
            .unwrap();
        assert!(rrset(&base, "_acme-challenge.example.test.", "TXT")
            .await
            .is_none());

        // A wrong key is a permission error that names the fix.
        let wrong = PowerDnsProvider::new_unchecked(PowerDnsCredentials {
            api_url: format!("{base}/api/v1"),
            api_key: "wrong".to_string(),
            server_id: None,
        })
        .unwrap()
        .test_connection()
        .await
        .unwrap_err();
        assert!(matches!(wrong, DnsError::PermissionDenied(_)), "{wrong}");
    }
}
