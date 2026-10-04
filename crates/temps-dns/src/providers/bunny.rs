// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Bunny DNS API adapter. Account credentials never leave the fixed API origin.
use super::{
    BunnyCredentials, DnsProvider, DnsProviderCapabilities, DnsProviderType, DnsRecord,
    DnsRecordContent, DnsRecordRequest, DnsRecordType, DnsZone,
};
use crate::errors::DnsError;
use async_trait::async_trait;
use reqwest::{header::HeaderValue, Method};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{Mutex, PoisonError},
    time::Duration,
};

/// Largest response body the adapter buffers. A single zone with tens of
/// thousands of records stays well below this; anything larger is refused
/// rather than letting an upstream (or a hostile proxy) grow memory unbounded.
const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
/// Page size for `/dnszone` listings and searches.
const ZONE_PAGE_SIZE: u32 = 100;
/// Hard cap on pages read while listing every zone in the account.
const MAX_LIST_PAGES: u32 = 10_000;
/// Hard cap on pages read for one zone search; a search term is a full domain
/// name, so needing more than this means the result cannot be trusted.
const MAX_SEARCH_PAGES: u32 = 20;
/// Deepest name for which candidate parent zones are probed.
const MAX_ZONE_LABELS: usize = 16;
/// Bounded per-instance cache of apex domain -> zone id.
const MAX_CACHED_ZONES: usize = 64;

pub struct BunnyProvider {
    client: reqwest::Client,
    key: HeaderValue,
    base: String,
    max_response_bytes: usize,
    /// Apex domain -> zone id, only for domains that resolved to a zone whose
    /// name equals the domain exactly (so no more specific zone can shadow
    /// it). Every hit is re-verified by fetching the zone and checking its
    /// identity; a 404 or mismatch evicts the entry and re-resolves.
    zone_ids: Mutex<HashMap<String, u64>>,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Page {
    items: Vec<Zone>,
    has_more_items: bool,
}
/// Search page that only keeps zone identity; per-zone `Records` arrays are
/// skipped by serde instead of being materialized.
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct SearchPage {
    items: Vec<ZoneRef>,
    has_more_items: bool,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ZoneRef {
    id: u64,
    domain: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Zone {
    id: u64,
    domain: String,
    #[serde(default)]
    records: Vec<Record>,
    #[serde(default)]
    nameservers_detected: bool,
    #[serde(default)]
    nameserver1: String,
    #[serde(default)]
    nameserver2: String,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Record {
    #[serde(default)]
    id: u64,
    #[serde(rename = "Type")]
    kind: u8,
    ttl: u32,
    name: String,
    value: String,
    #[serde(default)]
    priority: u16,
    #[serde(default)]
    weight: u16,
    #[serde(default)]
    port: u16,
    #[serde(default)]
    flags: u8,
    #[serde(default)]
    tag: String,
    #[serde(default)]
    comment: Option<String>,
    #[serde(default)]
    disabled: bool,
    #[serde(default)]
    accelerated: bool,
    #[serde(flatten)]
    extra: HashMap<String, serde_json::Value>,
}
/// Outcome of fetching a zone by id.
enum FetchedZone {
    Found(Zone),
    /// Bunny answered 404 for the id.
    Missing,
    /// The zone at that id is not the expected domain.
    Mismatch,
}
impl BunnyProvider {
    pub fn new(credentials: BunnyCredentials) -> Result<Self, DnsError> {
        if credentials.api_key.trim().is_empty() {
            return Err(DnsError::InvalidCredentials(
                "Bunny DNS API key is empty".into(),
            ));
        }
        let mut key = HeaderValue::from_str(&credentials.api_key).map_err(|_| {
            DnsError::InvalidCredentials(
                "Bunny DNS API key contains invalid header characters".into(),
            )
        })?;
        key.set_sensitive(true);
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| DnsError::ApiError("Failed to initialize Bunny DNS API client".into()))?;
        Ok(Self {
            client,
            key,
            base: "https://api.bunny.net".into(),
            max_response_bytes: MAX_RESPONSE_BYTES,
            zone_ids: Mutex::new(HashMap::new()),
        })
    }
    async fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<&Record>,
    ) -> Result<reqwest::Response, DnsError> {
        let mut request = self
            .client
            .request(method, format!("{}{path}", self.base))
            .header("AccessKey", self.key.clone());
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request
            .send()
            .await
            .map_err(|_| DnsError::ApiError(format!("Bunny DNS request failed for {path}")))?;
        match response.status().as_u16() {
            200..=299 => Ok(response),
            401 | 403 => Err(DnsError::PermissionDenied(format!(
                "Bunny DNS key lacks access to {path}"
            ))),
            404 => Err(DnsError::RecordNotFound(path.into())),
            429 => Err(DnsError::RateLimited(format!(
                "Bunny DNS rate limited request for {path}"
            ))),
            status => Err(DnsError::ApiError(format!(
                "Bunny DNS request for {path} failed (HTTP {status})"
            ))),
        }
    }
    /// Read a response body, refusing anything above `max_response_bytes`
    /// both from the advertised Content-Length and while streaming (the
    /// header can be absent or wrong).
    async fn read_body(
        &self,
        mut response: reqwest::Response,
        path: &str,
    ) -> Result<Vec<u8>, DnsError> {
        let max = self.max_response_bytes;
        let too_large = || {
            DnsError::ApiError(format!(
                "Bunny DNS response for {path} exceeded the {max}-byte limit; refusing to buffer it"
            ))
        };
        if response
            .content_length()
            .is_some_and(|length| length > max as u64)
        {
            return Err(too_large());
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| {
            DnsError::ApiError(format!(
                "Bunny DNS response body for {path} could not be read"
            ))
        })? {
            if body.len().saturating_add(chunk.len()) > max {
                return Err(too_large());
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }
    async fn json<T: serde::de::DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<&Record>,
    ) -> Result<T, DnsError> {
        let response = self.request(method, path, body).await?;
        let bytes = self.read_body(response, path).await?;
        serde_json::from_slice(&bytes).map_err(|_| {
            DnsError::ApiError(format!("Bunny DNS returned an invalid response for {path}"))
        })
    }
    async fn zones(&self) -> Result<Vec<Zone>, DnsError> {
        let mut zones = Vec::new();
        for page in 1..=MAX_LIST_PAGES {
            let result: Page = self
                .json(
                    Method::GET,
                    &format!("/dnszone?page={page}&perPage={ZONE_PAGE_SIZE}"),
                    None,
                )
                .await?;
            if result.has_more_items && result.items.is_empty() {
                return Err(DnsError::ApiError(format!(
                    "Bunny DNS pagination returned empty page {page} with more items"
                )));
            }
            zones.extend(result.items);
            if !result.has_more_items {
                return Ok(zones);
            }
        }
        Err(DnsError::ApiError(format!(
            "Bunny DNS zone pagination exceeded {MAX_LIST_PAGES} pages"
        )))
    }
    /// Normalized `domain` followed by each parent suffix that could be a
    /// zone, longest first (`a.b.example.com`, `b.example.com`,
    /// `example.com`). A bare TLD is never probed for a multi-label name.
    fn zone_candidates(domain: &str) -> Result<Vec<String>, DnsError> {
        let normalized = domain.trim().trim_end_matches('.').to_ascii_lowercase();
        let labels: Vec<&str> = normalized.split('.').collect();
        if normalized.is_empty() || labels.iter().any(|label| label.is_empty()) {
            return Err(DnsError::Validation(format!(
                "Bunny DNS zone lookup requires a valid domain name, got '{domain}'"
            )));
        }
        if labels.len() > MAX_ZONE_LABELS {
            return Err(DnsError::Validation(format!(
                "Bunny DNS zone lookup for '{normalized}' has {} labels; at most {MAX_ZONE_LABELS} are supported",
                labels.len()
            )));
        }
        let last = labels.len().saturating_sub(1).max(1);
        Ok((0..last).map(|start| labels[start..].join(".")).collect())
    }
    /// Find the id of the zone whose name is exactly `candidate` using
    /// Bunny's server-side `search` filter, so a lookup never downloads the
    /// whole account. Fails closed on more than one exact match.
    async fn search_zone_id(&self, candidate: &str) -> Result<Option<u64>, DnsError> {
        let mut matches = Vec::new();
        for page in 1..=MAX_SEARCH_PAGES {
            let result: SearchPage = self
                .json(
                    Method::GET,
                    &format!(
                        "/dnszone?page={page}&perPage={ZONE_PAGE_SIZE}&search={}",
                        urlencoding::encode(candidate)
                    ),
                    None,
                )
                .await?;
            if result.has_more_items && result.items.is_empty() {
                return Err(DnsError::ApiError(format!(
                    "Bunny DNS zone search for {candidate} returned empty page {page} with more items"
                )));
            }
            matches.extend(
                result
                    .items
                    .into_iter()
                    .filter(|zone| {
                        zone.domain
                            .trim_end_matches('.')
                            .eq_ignore_ascii_case(candidate)
                    })
                    .map(|zone| zone.id),
            );
            if !result.has_more_items {
                return match matches.as_slice() {
                    [] => Ok(None),
                    [id] => Ok(Some(*id)),
                    ids => Err(DnsError::ApiError(format!(
                        "Bunny DNS returned {} zones named {candidate} (ids {ids:?}); refusing to pick one",
                        ids.len()
                    ))),
                };
            }
        }
        Err(DnsError::ApiError(format!(
            "Bunny DNS zone search for {candidate} exceeded {MAX_SEARCH_PAGES} pages; refusing a partial result"
        )))
    }
    /// Fetch one zone and check it is the zone `domain` resolved to.
    async fn fetch_zone(&self, id: u64, domain: &str) -> Result<FetchedZone, DnsError> {
        let fetched: Zone = match self
            .json(Method::GET, &format!("/dnszone/{id}"), None)
            .await
        {
            Ok(zone) => zone,
            Err(DnsError::RecordNotFound(_)) => return Ok(FetchedZone::Missing),
            Err(error) => return Err(error),
        };
        if fetched.id != id
            || !fetched
                .domain
                .trim_end_matches('.')
                .eq_ignore_ascii_case(domain)
        {
            return Ok(FetchedZone::Mismatch);
        }
        Ok(FetchedZone::Found(fetched))
    }
    fn cached_zone_id(&self, domain: &str) -> Option<u64> {
        self.zone_ids
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(domain)
            .copied()
    }
    fn cache_zone_id(&self, domain: &str, id: u64) {
        let mut cache = self.zone_ids.lock().unwrap_or_else(PoisonError::into_inner);
        if cache.len() >= MAX_CACHED_ZONES && !cache.contains_key(domain) {
            cache.clear();
        }
        cache.insert(domain.to_string(), id);
    }
    fn evict_zone_id(&self, domain: &str) {
        self.zone_ids
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(domain);
    }
    /// Resolve the most specific zone containing `domain`.
    async fn zone(&self, domain: &str) -> Result<Zone, DnsError> {
        let candidates = Self::zone_candidates(domain)?;
        let requested = candidates.first().cloned().ok_or_else(|| {
            DnsError::Validation(format!(
                "Bunny DNS zone lookup for '{domain}' has no candidates"
            ))
        })?;
        if let Some(id) = self.cached_zone_id(&requested) {
            match self.fetch_zone(id, &requested).await? {
                FetchedZone::Found(zone) => return Ok(zone),
                FetchedZone::Missing | FetchedZone::Mismatch => {
                    tracing::debug!(
                        "Cached Bunny DNS zone {id} for {requested} is stale; re-resolving"
                    );
                    self.evict_zone_id(&requested);
                }
            }
        }
        for candidate in &candidates {
            let Some(id) = self.search_zone_id(candidate).await? else {
                continue;
            };
            return match self.fetch_zone(id, candidate).await? {
                FetchedZone::Found(zone) => {
                    if *candidate == requested {
                        self.cache_zone_id(&requested, id);
                    }
                    Ok(zone)
                }
                FetchedZone::Missing => Err(DnsError::ZoneNotFound(format!(
                    "Bunny DNS zone {id} for {candidate} disappeared while resolving {requested}"
                ))),
                FetchedZone::Mismatch => Err(DnsError::ApiError(format!(
                    "Bunny DNS zone {id} returned mismatched identity for {requested}"
                ))),
            };
        }
        Err(DnsError::ZoneNotFound(requested))
    }
    fn public_zone(zone: Zone) -> DnsZone {
        DnsZone {
            id: zone.id.to_string(),
            name: zone.domain,
            status: if zone.nameservers_detected {
                "active"
            } else {
                "pending"
            }
            .into(),
            nameservers: vec![zone.nameserver1, zone.nameserver2]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect(),
            metadata: HashMap::new(),
        }
    }
    /// Record types that map onto [`DnsRecordContent`]; must match `convert`.
    fn is_supported_kind(kind: u8) -> bool {
        matches!(kind, 0 | 1 | 2 | 3 | 4 | 8 | 9 | 10 | 12)
    }
    /// Human-readable name of a Bunny-only record type, for error messages.
    fn bunny_kind_label(kind: u8) -> String {
        match kind {
            5 => "Redirect".into(),
            6 => "Flatten".into(),
            7 => "PullZone".into(),
            11 => "Script".into(),
            other => format!("type {other}"),
        }
    }
    /// Canonical relative record name: lowercase, no trailing dot, and the
    /// zone apex as `@` (Bunny stores it as an empty name).
    fn normalize_name(name: &str) -> String {
        let name = name.trim().trim_end_matches('.').to_ascii_lowercase();
        if name.is_empty() {
            "@".into()
        } else {
            name
        }
    }
    fn convert(record: Record, domain: &str) -> Result<DnsRecord, DnsError> {
        let content = match record.kind {
            0 => DnsRecordContent::A {
                address: record.value,
            },
            1 => DnsRecordContent::AAAA {
                address: record.value,
            },
            2 => DnsRecordContent::CNAME {
                target: record.value,
            },
            3 => DnsRecordContent::TXT {
                content: record.value,
            },
            4 => DnsRecordContent::MX {
                priority: record.priority,
                target: record.value,
            },
            8 => DnsRecordContent::SRV {
                priority: record.priority,
                weight: record.weight,
                port: record.port,
                target: record.value,
            },
            9 => DnsRecordContent::CAA {
                flags: record.flags,
                tag: record.tag,
                value: record.value,
            },
            10 => DnsRecordContent::PTR {
                target: record.value,
            },
            12 => DnsRecordContent::NS {
                nameserver: record.value,
            },
            kind => {
                return Err(DnsError::Validation(format!(
                    "Bunny DNS record {} in {domain} has unsupported type {kind}",
                    record.id
                )))
            }
        };
        let domain = Self::normalize_name(domain);
        let domain = domain.as_str();
        let name = Self::normalize_name(&record.name);
        let fqdn = if name == "@" {
            domain.into()
        } else {
            format!("{name}.{domain}")
        };
        let mut metadata = HashMap::new();
        if let Some(comment) = record.comment {
            metadata.insert("comment".into(), comment);
        }
        metadata.insert("disabled".into(), record.disabled.to_string());
        metadata.insert("accelerated".into(), record.accelerated.to_string());
        Ok(DnsRecord {
            id: Some(record.id.to_string()),
            zone: domain.into(),
            name,
            fqdn,
            content,
            ttl: record.ttl,
            proxied: record.accelerated,
            metadata,
        })
    }
    fn payload(request: DnsRecordRequest, domain: &str) -> Result<Record, DnsError> {
        if request.proxied {
            return Err(DnsError::Validation(format!("Bunny DNS record {} in {domain}: CDN acceleration must be configured through delivery settings",request.name)));
        }
        let ttl = request.ttl.unwrap_or(300);
        let ttl = if ttl == 1 { 300 } else { ttl };
        if !(30..=86400).contains(&ttl) {
            return Err(DnsError::Validation(format!(
                "Bunny DNS record {} in {domain} requires TTL between 30 and 86400 seconds",
                request.name
            )));
        }
        let mut r = Record {
            id: 0,
            kind: 0,
            ttl,
            name: if request.name == "@" {
                String::new()
            } else {
                request.name
            },
            value: String::new(),
            priority: 0,
            weight: 0,
            port: 0,
            flags: 0,
            tag: String::new(),
            comment: Some("Managed by Temps".into()),
            disabled: false,
            accelerated: false,
            extra: HashMap::new(),
        };
        match request.content {
            DnsRecordContent::A { address } => {
                address.parse::<std::net::Ipv4Addr>().map_err(|_| {
                    DnsError::Validation(format!("Invalid Bunny DNS IPv4 record in {domain}"))
                })?;
                r.value = address;
            }
            DnsRecordContent::AAAA { address } => {
                address.parse::<std::net::Ipv6Addr>().map_err(|_| {
                    DnsError::Validation(format!("Invalid Bunny DNS IPv6 record in {domain}"))
                })?;
                r.kind = 1;
                r.value = address;
            }
            DnsRecordContent::CNAME { target } => {
                r.kind = 2;
                r.value = target;
            }
            DnsRecordContent::TXT { content } => {
                r.kind = 3;
                r.value = content;
            }
            DnsRecordContent::MX { priority, target } => {
                r.kind = 4;
                r.priority = priority;
                r.value = target;
            }
            DnsRecordContent::SRV {
                priority,
                weight,
                port,
                target,
            } => {
                r.kind = 8;
                r.priority = priority;
                r.weight = weight;
                r.port = port;
                r.value = target;
            }
            DnsRecordContent::CAA { flags, tag, value } => {
                r.kind = 9;
                r.flags = flags;
                r.tag = tag;
                r.value = value;
            }
            DnsRecordContent::PTR { target } => {
                r.kind = 10;
                r.value = target;
            }
            DnsRecordContent::NS { nameserver } => {
                r.kind = 12;
                r.value = nameserver;
            }
        }
        Ok(r)
    }
    fn record_id(id: &str) -> Result<u64, DnsError> {
        id.parse()
            .map_err(|_| DnsError::Validation(format!("Invalid Bunny DNS record ID {id}")))
    }
}
#[async_trait]
impl DnsProvider for BunnyProvider {
    fn provider_type(&self) -> DnsProviderType {
        DnsProviderType::Bunny
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
            wildcard: true,
            ..Default::default()
        }
    }
    async fn test_connection(&self) -> Result<bool, DnsError> {
        self.json::<Page>(Method::GET, "/dnszone?page=1&perPage=5", None)
            .await?;
        Ok(true)
    }
    async fn list_zones(&self) -> Result<Vec<DnsZone>, DnsError> {
        Ok(self
            .zones()
            .await?
            .into_iter()
            .map(Self::public_zone)
            .collect())
    }
    async fn get_zone(&self, domain: &str) -> Result<Option<DnsZone>, DnsError> {
        match self.zone(domain).await {
            Ok(z) => Ok(Some(Self::public_zone(z))),
            Err(DnsError::ZoneNotFound(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }
    async fn list_records(&self, domain: &str) -> Result<Vec<DnsRecord>, DnsError> {
        let z = self.zone(domain).await?;
        // Bunny-only types (Redirect, Flatten, PullZone, Script) have no
        // DnsRecordContent equivalent. Skip them here rather than failing the
        // whole zone listing; they are never Temps-managed. Ownership checks
        // go through `get_records`, which refuses a routing lookup at a name
        // one of these occupies, so a skipped record is never mistaken for a
        // free name.
        let (supported, skipped): (Vec<_>, Vec<_>) = z
            .records
            .into_iter()
            .partition(|record| Self::is_supported_kind(record.kind));
        if !skipped.is_empty() {
            tracing::debug!(
                "Skipping {} Bunny-specific DNS record(s) in zone {} (ids: {:?})",
                skipped.len(),
                z.domain,
                skipped.iter().map(|record| record.id).collect::<Vec<_>>()
            );
        }
        supported
            .into_iter()
            .map(|r| Self::convert(r, &z.domain))
            .collect()
    }
    /// Records at `name` of `record_type`, compared on normalized names.
    ///
    /// Fails closed with [`DnsError::RecordConflict`] when an A/AAAA/CNAME
    /// lookup hits a name occupied by a Bunny-only record (Redirect, Flatten,
    /// PullZone, Script): those answer for the name but are invisible to the
    /// generic record model, so reporting the name as free would let the
    /// ownership layer write alongside a record it cannot see.
    async fn get_records(
        &self,
        domain: &str,
        name: &str,
        record_type: DnsRecordType,
    ) -> Result<Vec<DnsRecord>, DnsError> {
        let z = self.zone(domain).await?;
        let wanted = Self::normalize_name(name);
        if matches!(
            record_type,
            DnsRecordType::A | DnsRecordType::AAAA | DnsRecordType::CNAME
        ) {
            if let Some(blocking) = z.records.iter().find(|record| {
                !Self::is_supported_kind(record.kind)
                    && Self::normalize_name(&record.name) == wanted
            }) {
                return Err(DnsError::RecordConflict {
                    domain: z.domain.clone(),
                    name: wanted,
                    record_type: record_type.to_string(),
                    reason: format!(
                        "Bunny DNS {} record {} already occupies this name and cannot be managed through temps",
                        Self::bunny_kind_label(blocking.kind),
                        blocking.id
                    ),
                });
            }
        }
        let zone_domain = z.domain;
        z.records
            .into_iter()
            .filter(|record| {
                Self::is_supported_kind(record.kind)
                    && Self::normalize_name(&record.name) == wanted
            })
            .map(|record| Self::convert(record, &zone_domain))
            .filter(|record| {
                !matches!(record, Ok(record) if record.content.record_type() != record_type)
            })
            .collect()
    }
    async fn get_record(
        &self,
        domain: &str,
        name: &str,
        kind: DnsRecordType,
    ) -> Result<Option<DnsRecord>, DnsError> {
        Ok(self
            .get_records(domain, name, kind)
            .await?
            .into_iter()
            .next())
    }
    async fn create_record(
        &self,
        domain: &str,
        request: DnsRecordRequest,
    ) -> Result<DnsRecord, DnsError> {
        let body = Self::payload(request, domain)?;
        let z = self.zone(domain).await?;
        let record = self
            .json(
                Method::PUT,
                &format!("/dnszone/{}/records", z.id),
                Some(&body),
            )
            .await?;
        Self::convert(record, &z.domain)
    }
    async fn update_record(
        &self,
        domain: &str,
        id: &str,
        request: DnsRecordRequest,
    ) -> Result<DnsRecord, DnsError> {
        let id = Self::record_id(id)?;
        let mut body = Self::payload(request, domain)?;
        let z = self.zone(domain).await?;
        let existing = z
            .records
            .into_iter()
            .find(|r| r.id == id)
            .ok_or_else(|| DnsError::RecordNotFound(id.to_string()))?;
        if existing.accelerated || existing.disabled {
            return Err(DnsError::Validation(format!("Bunny DNS record {id} in {domain} is accelerated or disabled; change these settings in Bunny before updating through Temps")));
        }
        body.id = id;
        body.comment = existing.comment;
        body.disabled = existing.disabled;
        body.accelerated = existing.accelerated;
        body.extra = existing.extra;
        let record = self
            .json(
                Method::POST,
                &format!("/dnszone/{}/records/{id}", z.id),
                Some(&body),
            )
            .await?;
        Self::convert(record, &z.domain)
    }
    async fn delete_record(&self, domain: &str, id: &str) -> Result<(), DnsError> {
        let id = Self::record_id(id)?;
        let z = self.zone(domain).await?;
        if !z.records.iter().any(|r| r.id == id) {
            return Err(DnsError::RecordNotFound(id.to_string()));
        }
        self.request(
            Method::DELETE,
            &format!("/dnszone/{}/records/{id}", z.id),
            None,
        )
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::ProviderCredentials;
    use serde_json::json;
    use wiremock::{
        matchers::{body_partial_json, header, method, path, query_param, query_param_is_missing},
        Mock, MockServer, ResponseTemplate,
    };
    fn fixture_record(id: u64, kind: u8) -> serde_json::Value {
        json!({"Id":id,"Type":kind,"Ttl":300,"Name":"www","Value":"192.0.2.1","Comment":"foreign owner","Priority":0,"Weight":0,"Port":0,"Flags":0,"Tag":""})
    }
    fn fixture_zone(id: u64, domain: &str, records: Vec<serde_json::Value>) -> serde_json::Value {
        json!({"Id":id,"Domain":domain,"Records":records,"NameserversDetected":true,"Nameserver1":"ns1.example.net","Nameserver2":"ns2.example.net"})
    }
    fn request() -> DnsRecordRequest {
        DnsRecordRequest {
            name: "www".into(),
            content: DnsRecordContent::A {
                address: "192.0.2.2".into(),
            },
            ttl: Some(300),
            proxied: false,
        }
    }
    fn provider(server: &MockServer) -> BunnyProvider {
        let mut p = BunnyProvider::new(BunnyCredentials {
            api_key: "test-secret-key".into(),
        })
        .unwrap();
        p.base = server.uri();
        p
    }
    async fn zone_mocks(server: &MockServer, records: Vec<serde_json::Value>) {
        let z = fixture_zone(10, "example.com", records);
        Mock::given(method("GET"))
            .and(path("/dnszone"))
            .and(header("AccessKey", "test-secret-key"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"Items":[z.clone()],"HasMoreItems":false})),
            )
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path("/dnszone/10"))
            .respond_with(ResponseTemplate::new(200).set_body_json(z))
            .mount(server)
            .await;
    }
    #[test]
    fn credentials_and_validation() {
        let creds = BunnyCredentials {
            api_key: "test-secret-key".into(),
        };
        assert!(!format!("{creds:?}").contains("test-secret-key"));
        assert_eq!(
            ProviderCredentials::Bunny(creds.clone()).masked()["api_key"],
            "***"
        );
        assert!(BunnyProvider::new(BunnyCredentials {
            api_key: "\n".into()
        })
        .is_err());
        assert_eq!(
            DnsProviderType::from_str("bunny.net").unwrap(),
            DnsProviderType::Bunny
        );
        assert_eq!(
            DnsProviderType::Bunny.required_credentials(),
            vec!["api_key"]
        );
        let mut r = request();
        r.proxied = true;
        assert!(BunnyProvider::payload(r, "example.com").is_err());
        let mut r = request();
        r.ttl = Some(2);
        assert!(BunnyProvider::payload(r, "example.com").is_err());
        assert!(BunnyProvider::record_id("../../foreign").is_err());
    }
    async fn search_mock(server: &MockServer, term: &str, zones: Vec<serde_json::Value>) {
        Mock::given(method("GET"))
            .and(path("/dnszone"))
            .and(query_param("search", term))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"Items":zones,"HasMoreItems":false})),
            )
            .mount(server)
            .await;
    }
    fn searches(requests: &[wiremock::Request]) -> Vec<String> {
        requests
            .iter()
            .filter_map(|r| {
                r.url
                    .query_pairs()
                    .find(|(k, _)| k == "search")
                    .map(|(_, v)| v.into_owned())
            })
            .collect()
    }
    #[tokio::test]
    async fn pagination_and_longest_zone() {
        let server = MockServer::start().await;
        for (page, z, more) in [
            (1, fixture_zone(1, "example.com", vec![]), true),
            (2, fixture_zone(2, "sub.example.com", vec![]), false),
        ] {
            Mock::given(method("GET"))
                .and(path("/dnszone"))
                .and(query_param("page", page.to_string()))
                .and(query_param_is_missing("search"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_json(json!({"Items":[z],"HasMoreItems":more})),
                )
                .expect(1)
                .mount(&server)
                .await;
        }
        // Bunny's search is a substring filter: near misses must be ignored.
        search_mock(
            &server,
            "app.sub.example.com",
            vec![fixture_zone(3, "myapp.sub.example.com", vec![])],
        )
        .await;
        search_mock(
            &server,
            "sub.example.com",
            vec![
                fixture_zone(4, "notsub.example.com", vec![]),
                fixture_zone(2, "SUB.example.com", vec![]),
            ],
        )
        .await;
        Mock::given(method("GET"))
            .and(path("/dnszone/2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fixture_zone(
                2,
                "sub.example.com",
                vec![],
            )))
            .expect(1)
            .mount(&server)
            .await;
        let p = provider(&server);
        assert_eq!(p.list_zones().await.unwrap().len(), 2);
        assert_eq!(
            p.get_zone("app.sub.example.com.")
                .await
                .unwrap()
                .unwrap()
                .id,
            "2"
        );
        // Resolution stops at the most specific zone: the parent
        // `example.com` is never searched and the account is never listed
        // in full outside `list_zones`.
        let requests = server.received_requests().await.unwrap();
        assert_eq!(
            searches(&requests),
            vec!["app.sub.example.com", "sub.example.com"]
        );
    }
    #[test]
    fn zone_candidates_are_longest_first_and_bounded() {
        assert_eq!(
            BunnyProvider::zone_candidates("App.Sub.Example.COM.").unwrap(),
            vec!["app.sub.example.com", "sub.example.com", "example.com"]
        );
        assert_eq!(
            BunnyProvider::zone_candidates("example.com").unwrap(),
            vec!["example.com"]
        );
        assert_eq!(
            BunnyProvider::zone_candidates("localhost").unwrap(),
            vec!["localhost"]
        );
        for invalid in ["", ".", "a..example.com"] {
            assert!(matches!(
                BunnyProvider::zone_candidates(invalid),
                Err(DnsError::Validation(_))
            ));
        }
        let deep = format!("{}example.com", "a.".repeat(MAX_ZONE_LABELS));
        assert!(matches!(
            BunnyProvider::zone_candidates(&deep),
            Err(DnsError::Validation(_))
        ));
    }
    #[tokio::test]
    async fn duplicate_or_unbounded_zone_search_fails_closed() {
        let server = MockServer::start().await;
        search_mock(
            &server,
            "example.com",
            vec![
                fixture_zone(1, "example.com", vec![]),
                fixture_zone(2, "Example.com.", vec![]),
            ],
        )
        .await;
        let error = provider(&server).get_zone("example.com").await.unwrap_err();
        assert!(
            matches!(&error, DnsError::ApiError(m) if m.contains("2 zones named example.com")),
            "{error}"
        );

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/dnszone"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"Items":[fixture_zone(5, "other.example.com", vec![])],"HasMoreItems":true}),
            ))
            .mount(&server)
            .await;
        let error = provider(&server).get_zone("example.com").await.unwrap_err();
        assert!(
            matches!(&error, DnsError::ApiError(m) if m.contains("exceeded")),
            "{error}"
        );
        assert_eq!(
            server.received_requests().await.unwrap().len(),
            MAX_SEARCH_PAGES as usize
        );
    }
    #[tokio::test]
    async fn apex_zone_id_is_cached_and_reverified() {
        let server = MockServer::start().await;
        let z = fixture_zone(10, "example.com", vec![fixture_record(20, 0)]);
        Mock::given(method("GET"))
            .and(path("/dnszone"))
            .and(query_param("search", "example.com"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"Items":[z.clone()],"HasMoreItems":false})),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/dnszone/10"))
            .respond_with(ResponseTemplate::new(200).set_body_json(z))
            .up_to_n_times(2)
            .mount(&server)
            .await;
        let p = provider(&server);
        for _ in 0..2 {
            assert_eq!(
                p.get_records("example.com", "www", DnsRecordType::A)
                    .await
                    .unwrap()
                    .len(),
                1
            );
        }
        let requests = server.received_requests().await.unwrap();
        assert_eq!(searches(&requests).len(), 1, "second lookup hits the cache");

        // The zone was deleted and recreated under a new id: the cached id
        // now 404s, so the entry is evicted and the domain re-resolved.
        let recreated = fixture_zone(11, "example.com", vec![]);
        search_mock(&server, "example.com", vec![recreated.clone()]).await;
        Mock::given(method("GET"))
            .and(path("/dnszone/10"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/dnszone/11"))
            .respond_with(ResponseTemplate::new(200).set_body_json(recreated))
            .mount(&server)
            .await;
        assert_eq!(p.get_zone("example.com").await.unwrap().unwrap().id, "11");
        assert_eq!(p.cached_zone_id("example.com"), Some(11));

        // Subdomain lookups resolve to the parent zone but are never cached,
        // so a more specific zone created later is still found.
        let server = MockServer::start().await;
        search_mock(&server, "app.example.com", vec![]).await;
        search_mock(
            &server,
            "example.com",
            vec![fixture_zone(10, "example.com", vec![])],
        )
        .await;
        Mock::given(method("GET"))
            .and(path("/dnszone/10"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fixture_zone(
                10,
                "example.com",
                vec![],
            )))
            .mount(&server)
            .await;
        let p = provider(&server);
        p.get_zone("app.example.com").await.unwrap();
        assert_eq!(p.cached_zone_id("app.example.com"), None);
    }
    #[tokio::test]
    async fn oversized_responses_are_refused() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "Items":[fixture_zone(1, "example.com", vec![fixture_record(1, 0); 50])],
                "HasMoreItems":false
            })))
            .mount(&server)
            .await;
        let mut p = provider(&server);
        assert!(p.test_connection().await.unwrap());
        p.max_response_bytes = 256;
        let error = p.test_connection().await.unwrap_err();
        assert!(
            matches!(&error, DnsError::ApiError(m) if m.contains("256-byte limit")),
            "{error}"
        );
    }
    #[tokio::test]
    async fn create_update_delete_preserves_provider_fields() {
        let server = MockServer::start().await;
        let mut existing = fixture_record(20, 0);
        existing["MonitorType"] = json!(2);
        zone_mocks(&server, vec![existing.clone()]).await;
        Mock::given(method("PUT")).and(path("/dnszone/10/records")).and(body_partial_json(json!({"Type":0,"Name":"www","Value":"192.0.2.2","Ttl":300,"Comment":"Managed by Temps"}))).respond_with(ResponseTemplate::new(201).set_body_json(fixture_record(21,0))).expect(1).mount(&server).await;
        Mock::given(method("POST"))
            .and(path("/dnszone/10/records/20"))
            .and(body_partial_json(
                json!({"Value":"192.0.2.2","Comment":"foreign owner","MonitorType":2}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(existing))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/dnszone/10/records/20"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        let p = provider(&server);
        assert_eq!(
            p.create_record("example.com", request())
                .await
                .unwrap()
                .id
                .as_deref(),
            Some("21")
        );
        assert_eq!(
            p.update_record("example.com", "20", request())
                .await
                .unwrap()
                .metadata["comment"],
            "foreign owner"
        );
        p.delete_record("example.com", "20").await.unwrap();
        assert!(matches!(
            p.delete_record("example.com", "999").await,
            Err(DnsError::RecordNotFound(_))
        ));
    }
    #[tokio::test]
    async fn permission_transport_and_upstream_errors_are_sanitized() {
        for status in [401, 403, 429, 500, 302] {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .respond_with(
                    ResponseTemplate::new(status).set_body_string("test-secret-key upstream debug"),
                )
                .mount(&server)
                .await;
            let error = provider(&server).test_connection().await.unwrap_err();
            assert!(!error.to_string().contains("test-secret-key"));
            if status == 401 || status == 403 {
                assert!(matches!(error, DnsError::PermissionDenied(_)));
            }
        }
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("invalid test-secret-key"))
            .mount(&server)
            .await;
        assert!(!provider(&server)
            .test_connection()
            .await
            .unwrap_err()
            .to_string()
            .contains("test-secret-key"));
    }
    #[tokio::test]
    async fn unknown_types_are_skipped_and_disabled_foreign_records_remain_visible() {
        let server = MockServer::start().await;
        let mut disabled = fixture_record(20, 0);
        disabled["Disabled"] = json!(true);
        zone_mocks(&server, vec![disabled]).await;
        let records = provider(&server)
            .get_records("example.com", "www", DnsRecordType::A)
            .await
            .unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].metadata["disabled"], "true");
        // A Bunny PullZone (7) or Redirect (5) record must not make the
        // whole zone unlistable; supported records are still returned.
        let server = MockServer::start().await;
        zone_mocks(
            &server,
            vec![
                fixture_record(20, 0),
                fixture_record(21, 7),
                fixture_record(22, 5),
            ],
        )
        .await;
        let p = provider(&server);
        let records = p.list_records("example.com").await.unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].id.as_deref(), Some("20"));
        // ...but a routing lookup at the name they occupy must not report it
        // as free, or the ownership layer would write next to a record it
        // cannot see.
        for kind in [DnsRecordType::A, DnsRecordType::AAAA, DnsRecordType::CNAME] {
            let error = p
                .get_records("example.com", "WWW.", kind)
                .await
                .unwrap_err();
            assert!(
                matches!(&error, DnsError::RecordConflict { name, reason, .. }
                    if name == "www" && reason.contains("PullZone record 21")),
                "{error}"
            );
        }
        // Non-routing lookups (e.g. the ownership TXT registry) still work.
        assert!(p
            .get_records("example.com", "www", DnsRecordType::TXT)
            .await
            .unwrap()
            .is_empty());
    }
    #[test]
    fn record_names_are_normalized() {
        for (raw, name, fqdn) in [
            ("WWW", "www", "www.example.com"),
            ("Api.Example.", "api.example", "api.example.example.com"),
            ("", "@", "example.com"),
            ("@", "@", "example.com"),
        ] {
            let mut record = BunnyProvider::payload(request(), "example.com").unwrap();
            record.name = raw.into();
            let converted = BunnyProvider::convert(record, "Example.COM.").unwrap();
            assert_eq!(converted.name, name);
            assert_eq!(converted.fqdn, fqdn);
            assert_eq!(converted.zone, "example.com");
        }
    }
    #[tokio::test]
    async fn get_records_matches_names_case_insensitively() {
        let server = MockServer::start().await;
        let mut upper = fixture_record(20, 0);
        upper["Name"] = json!("WWW");
        let mut apex = fixture_record(21, 0);
        apex["Name"] = json!("");
        zone_mocks(&server, vec![upper, apex, fixture_record(22, 3)]).await;
        let p = provider(&server);
        let records = p
            .get_records("example.com", "www", DnsRecordType::A)
            .await
            .unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].name, "www");
        let apex = p
            .get_records("example.com", "@", DnsRecordType::A)
            .await
            .unwrap();
        assert_eq!(apex[0].id.as_deref(), Some("21"));
    }
    #[tokio::test]
    async fn missing_zone_and_broken_pagination() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"Items":[],"HasMoreItems":false})),
            )
            .mount(&server)
            .await;
        assert!(provider(&server)
            .get_zone("missing.example")
            .await
            .unwrap()
            .is_none());
        assert!(matches!(
            provider(&server).check_zone_access("missing.example").await,
            Err(DnsError::ZoneNotFound(_))
        ));
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"Items":[],"HasMoreItems":true})),
            )
            .mount(&server)
            .await;
        assert!(provider(&server).list_zones().await.is_err());
    }
    #[tokio::test]
    async fn unsafe_updates_and_mismatched_zones_never_mutate() {
        for flag in ["Accelerated", "Disabled"] {
            let server = MockServer::start().await;
            let mut record = fixture_record(20, 0);
            record[flag] = json!(true);
            zone_mocks(&server, vec![record]).await;
            let result = provider(&server)
                .update_record("example.com", "20", request())
                .await;
            assert!(matches!(result, Err(DnsError::Validation(_))));
            assert!(server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .all(|r| r.method == "GET"));
        }
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/dnszone"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"Items":[fixture_zone(10,"example.com",vec![])],"HasMoreItems":false}),
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/dnszone/10"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fixture_zone(
                11,
                "foreign.example",
                vec![],
            )))
            .mount(&server)
            .await;
        assert!(matches!(
            provider(&server)
                .create_record("example.com", request())
                .await,
            Err(DnsError::ApiError(_))
        ));
        assert!(server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|r| r.method == "GET"));
    }

    #[test]
    fn structured_records_roundtrip() {
        for content in [
            DnsRecordContent::MX {
                priority: 10,
                target: "mail.example.com".into(),
            },
            DnsRecordContent::SRV {
                priority: 20,
                weight: 30,
                port: 443,
                target: "service.example.com".into(),
            },
            DnsRecordContent::CAA {
                flags: 0,
                tag: "issue".into(),
                value: "ca.example".into(),
            },
            DnsRecordContent::NS {
                nameserver: "ns.example.com".into(),
            },
            DnsRecordContent::PTR {
                target: "host.example.com".into(),
            },
            DnsRecordContent::TXT {
                content: "opaque challenge".into(),
            },
            DnsRecordContent::AAAA {
                address: "2001:db8::1".into(),
            },
            DnsRecordContent::CNAME {
                target: "target.example.com".into(),
            },
        ] {
            let expected = content.to_value_string();
            let mut r = request();
            r.content = content;
            let r = BunnyProvider::payload(r, "example.com").unwrap();
            assert_eq!(
                BunnyProvider::convert(r, "example.com")
                    .unwrap()
                    .content
                    .to_value_string(),
                expected
            );
        }
    }
}
