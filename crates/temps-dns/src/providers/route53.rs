// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! AWS Route 53 DNS provider implementation
//!
//! This provider uses the AWS Route 53 API to manage DNS records.
//! It requires IAM credentials with Route53 permissions.
//!
//! Required IAM Policy:
//! - route53:ListHostedZones
//! - route53:ListHostedZonesByName (exact zone lookup; without it, zone
//!   lookups fall back to paging through every hosted zone)
//! - route53:ListResourceRecordSets
//! - route53:ChangeResourceRecordSets
//! - route53:GetHostedZone

use async_trait::async_trait;
use reqwest::{Client, Method, StatusCode, Url};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use tracing::{debug, info, warn};

use super::credentials::Route53Credentials;
use super::traits::{
    decode_txt_presentation, dns_names_equal, encode_txt_presentation, truncate_error_body,
    DnsProvider, DnsProviderCapabilities, DnsProviderType, DnsRecord, DnsRecordContent,
    DnsRecordRequest, DnsRecordType, DnsZone,
};
use crate::errors::DnsError;

const AWS_ROUTE53_ENDPOINT: &str = "https://route53.amazonaws.com";
/// Hard cap on pages read by any listing; reaching it is an error, never a
/// silently truncated result.
const MAX_PAGES: usize = 1000;
/// `maxitems` for an exact (name, type) lookup: one page almost always
/// holds every record set of the name plus the first one past it.
const EXACT_LOOKUP_PAGE_SIZE: u32 = 100;

/// SigV4 canonical query string, which is also the query string sent:
/// parameters sorted by name, names and values percent-encoded per RFC 3986
/// (`urlencoding` leaves exactly the unreserved characters unencoded and
/// writes uppercase hex).
fn canonical_query_string(params: &[(&str, &str)]) -> String {
    let mut encoded: Vec<(String, String)> = params
        .iter()
        .map(|(key, value)| {
            (
                urlencoding::encode(key).into_owned(),
                urlencoding::encode(value).into_owned(),
            )
        })
        .collect();
    encoded.sort();
    encoded
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("&")
}

/// Decode the `\NNN` octal escapes Route 53 uses in the DNS names it
/// returns: every character outside `a-z 0-9 - _` comes back escaped, so a
/// wildcard `*.app.example.com.` is listed as `\052.app.example.com.`.
/// Anything that is not a valid three-digit octal escape is kept as-is.
fn decode_route53_name(name: &str) -> String {
    let bytes = name.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\' {
            if let Some(digits) = bytes.get(index + 1..index + 4) {
                if digits.iter().all(|digit| (b'0'..=b'7').contains(digit)) {
                    let value = digits
                        .iter()
                        .fold(0u32, |acc, digit| acc * 8 + u32::from(digit - b'0'));
                    if let Ok(byte) = u8::try_from(value) {
                        decoded.push(byte);
                        index += 4;
                        continue;
                    }
                }
            }
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

/// Encode a DNS name the way Route 53 lists it (lowercase, every byte outside
/// `a-z 0-9 - _ .` as a `\NNN` octal escape), so a `StartRecordName` sorts
/// exactly where Route 53 keeps the record.
fn encode_route53_name(name: &str) -> String {
    let mut encoded = String::with_capacity(name.len());
    for byte in name.to_ascii_lowercase().bytes() {
        match byte {
            b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => encoded.push(byte as char),
            _ => encoded.push_str(&format!("\\{byte:03o}")),
        }
    }
    encoded
}

/// What a record-set scan does after visiting one record set.
enum Scan {
    /// Keep reading.
    Continue,
    /// Stop: everything the caller needs has been seen.
    Done,
}

/// Position in a ListResourceRecordSets listing (Route 53's cursor).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct RecordSetCursor {
    name: String,
    record_type: String,
    identifier: Option<String>,
}

/// AWS Route 53 DNS provider
pub struct Route53Provider {
    client: Client,
    credentials: Route53Credentials,
    region: String,
    /// API endpoint (`https://route53.amazonaws.com`; a mock server in tests).
    endpoint: String,
    /// `host[:port]` of `endpoint`, sent and signed as the `Host` header.
    host: String,
    /// Page cap for listings ([`MAX_PAGES`]; lowered in tests).
    max_pages: usize,
}

/// AWS Signature V4 signing implementation
mod aws_signing {
    use chrono::Utc;
    use hmac::{Hmac, KeyInit, Mac};
    use sha2::{Digest, Sha256};

    type HmacSha256 = Hmac<Sha256>;

    #[allow(clippy::too_many_arguments)]
    pub fn sign_request(
        method: &str,
        uri: &str,
        query_string: &str,
        headers: &[(&str, &str)],
        payload: &str,
        access_key: &str,
        secret_key: &str,
        region: &str,
        service: &str,
    ) -> (String, String, String) {
        let now = Utc::now();
        let date_stamp = now.format("%Y%m%d").to_string();
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();

        // Create canonical request
        let payload_hash = hex::encode(Sha256::digest(payload.as_bytes()));

        let mut signed_headers: Vec<&str> = headers.iter().map(|(k, _)| *k).collect();
        signed_headers.sort();
        let signed_headers_str = signed_headers.join(";");

        let mut canonical_headers = String::new();
        let mut sorted_headers: Vec<_> = headers.to_vec();
        sorted_headers.sort_by(|a, b| a.0.cmp(b.0));
        for (key, value) in &sorted_headers {
            canonical_headers.push_str(&format!("{}:{}\n", key.to_lowercase(), value.trim()));
        }

        let canonical_request = format!(
            "{}\n{}\n{}\n{}\n{}\n{}",
            method, uri, query_string, canonical_headers, signed_headers_str, payload_hash
        );

        let canonical_request_hash = hex::encode(Sha256::digest(canonical_request.as_bytes()));

        // Create string to sign
        let credential_scope = format!("{}/{}/{}/aws4_request", date_stamp, region, service);
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{}\n{}\n{}",
            amz_date, credential_scope, canonical_request_hash
        );

        // Calculate signature
        let k_date = hmac_sha256(format!("AWS4{}", secret_key).as_bytes(), &date_stamp);
        let k_region = hmac_sha256(&k_date, region);
        let k_service = hmac_sha256(&k_region, service);
        let k_signing = hmac_sha256(&k_service, "aws4_request");
        let signature = hex::encode(hmac_sha256(&k_signing, &string_to_sign));

        // Create authorization header
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential={}/{}, SignedHeaders={}, Signature={}",
            access_key, credential_scope, signed_headers_str, signature
        );

        (authorization, amz_date, payload_hash)
    }

    fn hmac_sha256(key: &[u8], data: &str) -> Vec<u8> {
        let mut mac = HmacSha256::new_from_slice(key).expect("HMAC can take key of any size");
        mac.update(data.as_bytes());
        mac.finalize().into_bytes().to_vec()
    }
}

/// Route 53 API response structures
#[derive(Debug, Deserialize)]
struct ListHostedZonesResponse {
    #[serde(rename = "HostedZones")]
    hosted_zones: Option<HostedZonesWrapper>,
    /// More hosted zones follow; continue at `NextMarker`.
    #[serde(rename = "IsTruncated", default)]
    is_truncated: bool,
    #[serde(rename = "NextMarker")]
    next_marker: Option<String>,
}

/// ListHostedZonesByName response: zones in name order, starting at the
/// requested `dnsname`.
#[derive(Debug, Deserialize)]
struct ListHostedZonesByNameResponse {
    #[serde(rename = "HostedZones")]
    hosted_zones: Option<HostedZonesWrapper>,
}

#[derive(Debug, Deserialize)]
struct HostedZonesWrapper {
    #[serde(rename = "HostedZone", default)]
    hosted_zone: Vec<HostedZone>,
}

#[derive(Debug, Deserialize)]
struct HostedZone {
    #[serde(rename = "Id")]
    id: String,
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "CallerReference")]
    #[allow(dead_code)]
    caller_reference: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ListResourceRecordSetsResponse {
    #[serde(rename = "ResourceRecordSets")]
    resource_record_sets: Option<ResourceRecordSetsWrapper>,
    /// More record sets follow; continue at `Next*`.
    #[serde(rename = "IsTruncated", default)]
    is_truncated: bool,
    #[serde(rename = "NextRecordName")]
    next_record_name: Option<String>,
    #[serde(rename = "NextRecordType")]
    next_record_type: Option<String>,
    #[serde(rename = "NextRecordIdentifier")]
    next_record_identifier: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ResourceRecordSetsWrapper {
    #[serde(rename = "ResourceRecordSet", default)]
    resource_record_set: Vec<ResourceRecordSet>,
}

#[derive(Debug, Deserialize, Clone)]
struct ResourceRecordSet {
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "Type")]
    record_type: String,
    #[serde(rename = "TTL")]
    ttl: Option<u32>,
    #[serde(rename = "ResourceRecords")]
    resource_records: Option<ResourceRecordsWrapper>,
    /// Set on alias record sets, which carry no `ResourceRecords`.
    #[serde(rename = "AliasTarget")]
    alias_target: Option<AliasTarget>,
}

#[derive(Debug, Deserialize, Clone)]
struct AliasTarget {
    #[serde(rename = "DNSName")]
    dns_name: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
struct ResourceRecordsWrapper {
    #[serde(rename = "ResourceRecord")]
    resource_record: Vec<ResourceRecord>,
}

#[derive(Debug, Deserialize, Clone)]
struct ResourceRecord {
    #[serde(rename = "Value")]
    value: String,
}

/// Change batch request for Route 53
#[derive(Debug, Serialize)]
struct ChangeResourceRecordSetsRequest {
    #[serde(rename = "ChangeBatch")]
    change_batch: ChangeBatch,
}

#[derive(Debug, Serialize)]
struct ChangeBatch {
    #[serde(rename = "Comment")]
    #[serde(skip_serializing_if = "Option::is_none")]
    comment: Option<String>,
    #[serde(rename = "Changes")]
    changes: Changes,
}

#[derive(Debug, Serialize)]
struct Changes {
    #[serde(rename = "Change")]
    change: Vec<Change>,
}

#[derive(Debug, Serialize)]
struct Change {
    #[serde(rename = "Action")]
    action: String,
    #[serde(rename = "ResourceRecordSet")]
    resource_record_set: ChangeResourceRecordSet,
}

#[derive(Debug, Serialize)]
struct ChangeResourceRecordSet {
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "Type")]
    record_type: String,
    #[serde(rename = "TTL")]
    ttl: u32,
    #[serde(rename = "ResourceRecords")]
    resource_records: ChangeResourceRecords,
}

#[derive(Debug, Serialize)]
struct ChangeResourceRecords {
    #[serde(rename = "ResourceRecord")]
    resource_record: Vec<ChangeResourceRecord>,
}

#[derive(Debug, Serialize)]
struct ChangeResourceRecord {
    #[serde(rename = "Value")]
    value: String,
}

impl Route53Provider {
    /// Create a new Route 53 provider with the given credentials
    pub fn new(credentials: Route53Credentials) -> Result<Self, DnsError> {
        Self::with_endpoint(credentials, AWS_ROUTE53_ENDPOINT)
    }

    /// Create a provider that talks to `endpoint` instead of AWS (tests point
    /// it at a mock server).
    fn with_endpoint(credentials: Route53Credentials, endpoint: &str) -> Result<Self, DnsError> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| DnsError::ApiError(format!("Failed to create HTTP client: {}", e)))?;

        let region = credentials
            .region
            .clone()
            .unwrap_or_else(|| "us-east-1".to_string());

        let url = Url::parse(endpoint).map_err(|e| {
            DnsError::ApiError(format!(
                "Route 53 endpoint '{endpoint}' is not a valid URL: {e}"
            ))
        })?;
        let host_name = url.host_str().ok_or_else(|| {
            DnsError::ApiError(format!("Route 53 endpoint '{endpoint}' has no host"))
        })?;
        let host = match url.port() {
            Some(port) => format!("{host_name}:{port}"),
            None => host_name.to_string(),
        };

        Ok(Self {
            client,
            credentials,
            region,
            endpoint: endpoint.trim_end_matches('/').to_string(),
            host,
            max_pages: MAX_PAGES,
        })
    }

    /// Send one SigV4-signed request and return its status and body,
    /// whatever the status. `query` is signed and sent in canonical form.
    async fn send_request(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, &str)],
        body: Option<&str>,
    ) -> Result<(StatusCode, String), DnsError> {
        let query_string = canonical_query_string(query);
        let url = if query_string.is_empty() {
            format!("{}{}", self.endpoint, path)
        } else {
            format!("{}{}?{}", self.endpoint, path, query_string)
        };
        let payload = body.unwrap_or("");

        let headers = vec![("host", self.host.as_str())];

        let (authorization, amz_date, _content_hash) = aws_signing::sign_request(
            method.as_str(),
            path,
            &query_string,
            &headers,
            payload,
            &self.credentials.access_key_id,
            &self.credentials.secret_access_key,
            &self.region,
            "route53",
        );

        let mut request = self
            .client
            .request(method.clone(), &url)
            .header("Host", &self.host)
            .header("X-Amz-Date", amz_date)
            .header("Authorization", authorization)
            .header("Content-Type", "application/xml");

        if let Some(body) = body {
            request = request.body(body.to_string());
        }

        debug!("Route53 API request: {} {}", method, path);

        let response = request
            .send()
            .await
            .map_err(|e| DnsError::ApiError(format!("Route 53 API {method} {path} failed: {e}")))?;

        let status = response.status();
        let response_body = response.text().await.map_err(|e| {
            DnsError::ApiError(format!(
                "Failed to read Route 53 API response for {method} {path} (HTTP {status}): {e}"
            ))
        })?;

        Ok((status, response_body))
    }

    /// Error for a non-success response: status, operation, bounded body.
    fn status_error(method: &Method, path: &str, status: StatusCode, body: &str) -> DnsError {
        DnsError::ApiError(format!(
            "Route 53 API returned status {status} for {method} {path}: {}",
            truncate_error_body(body)
        ))
    }

    /// Make a signed request to Route 53 API
    async fn api_request(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, &str)],
        body: Option<&str>,
    ) -> Result<String, DnsError> {
        let (status, response_body) = self.send_request(method.clone(), path, query, body).await?;
        if !status.is_success() {
            return Err(Self::status_error(&method, path, status, &response_body));
        }
        Ok(response_body)
    }

    /// Whether a ChangeResourceRecordSets failure says a CREATE hit a record
    /// set that already exists.
    fn is_already_exists_error(status: StatusCode, body: &str) -> bool {
        status == StatusCode::BAD_REQUEST
            && body.contains("InvalidChangeBatch")
            && body.contains("already exists")
    }

    /// The hosted zone named exactly `domain`, or [`DnsError::ZoneNotFound`]
    /// (see [`DnsProvider::get_zone`]). A parent zone is never substituted
    /// for a missing one.
    async fn resolve_zone(&self, domain: &str) -> Result<DnsZone, DnsError> {
        self.get_zone(domain)
            .await?
            .ok_or_else(|| DnsError::ZoneNotFound(domain.to_string()))
    }

    /// A Route 53 hosted zone as a [`DnsZone`] (ID without `/hostedzone/`,
    /// normalized name).
    fn dns_zone(zone: HostedZone) -> DnsZone {
        DnsZone {
            id: zone.id.trim_start_matches("/hostedzone/").to_string(),
            name: Self::normalize_domain(&decode_route53_name(&zone.name)),
            status: "active".to_string(),
            nameservers: vec![],
            metadata: HashMap::new(),
        }
    }

    /// The first hosted zone named exactly `normalized`, looked up with
    /// ListHostedZonesByName (`dnsname` + `maxitems=1`) so the answer never
    /// depends on where the zone falls in a full listing.
    ///
    /// `Ok(Err(status))` reports a 403: the credentials lack
    /// `route53:ListHostedZonesByName`, so the caller falls back to a
    /// complete listing.
    async fn lookup_zone_by_name(
        &self,
        normalized: &str,
    ) -> Result<Result<Option<DnsZone>, StatusCode>, DnsError> {
        let path = "/2013-04-01/hostedzonesbyname";
        let dns_name = encode_route53_name(&format!("{normalized}."));
        let (status, body) = self
            .send_request(
                Method::GET,
                path,
                &[("dnsname", dns_name.as_str()), ("maxitems", "1")],
                None,
            )
            .await?;
        if status == StatusCode::FORBIDDEN {
            return Ok(Err(status));
        }
        if !status.is_success() {
            return Err(Self::status_error(&Method::GET, path, status, &body));
        }
        let parsed: ListHostedZonesByNameResponse =
            quick_xml::de::from_str(&body).map_err(|e| {
                DnsError::ApiError(format!(
                    "Failed to parse Route 53 ListHostedZonesByName response for {normalized}: {e}"
                ))
            })?;
        // The first zone at or after `dnsname`; anything else is a miss.
        Ok(Ok(parsed
            .hosted_zones
            .map(|wrapper| wrapper.hosted_zone)
            .unwrap_or_default()
            .into_iter()
            .map(Self::dns_zone)
            .find(|zone| zone.name == normalized)))
    }

    /// Visit record sets of a hosted zone in Route 53's listing order,
    /// starting at `start` (the beginning of the zone when `None`) and
    /// following `IsTruncated` / `NextRecord*` until the listing ends or
    /// `visit` returns [`Scan::Done`].
    ///
    /// Never stops early on its own: reaching the page cap, a repeated
    /// cursor, a truncated page without a cursor, or an empty truncated page
    /// is an error rather than a partial result.
    async fn scan_record_sets<F>(
        &self,
        zone_id: &str,
        start: Option<RecordSetCursor>,
        page_size: Option<u32>,
        context: &str,
        mut visit: F,
    ) -> Result<(), DnsError>
    where
        F: FnMut(ResourceRecordSet) -> Scan + Send,
    {
        let path = format!("/2013-04-01/hostedzone/{zone_id}/rrset");
        let page_size = page_size.map(|size| size.to_string());
        let mut seen_cursors: HashSet<RecordSetCursor> = HashSet::new();
        if let Some(start) = &start {
            seen_cursors.insert(start.clone());
        }
        let mut cursor = start;

        for page in 1..=self.max_pages {
            let mut query: Vec<(&str, &str)> = Vec::new();
            if let Some(cursor) = &cursor {
                query.push(("name", cursor.name.as_str()));
                query.push(("type", cursor.record_type.as_str()));
                if let Some(identifier) = &cursor.identifier {
                    query.push(("identifier", identifier.as_str()));
                }
            }
            if let Some(size) = &page_size {
                query.push(("maxitems", size.as_str()));
            }
            let body = self.api_request(Method::GET, &path, &query, None).await?;

            let parsed: ListResourceRecordSetsResponse = quick_xml::de::from_str(&body)
                .map_err(|e| {
                    DnsError::ApiError(format!(
                        "{context}: failed to parse Route 53 ListResourceRecordSets page {page}: {e}"
                    ))
                })?;
            let record_sets = parsed
                .resource_record_sets
                .map(|wrapper| wrapper.resource_record_set)
                .unwrap_or_default();
            let page_len = record_sets.len();
            for record_set in record_sets {
                if let Scan::Done = visit(record_set) {
                    return Ok(());
                }
            }

            if !parsed.is_truncated {
                return Ok(());
            }
            if page_len == 0 {
                return Err(DnsError::ApiError(format!(
                    "{context}: Route 53 returned empty page {page} marked IsTruncated; refusing a partial result"
                )));
            }
            let next = match (parsed.next_record_name, parsed.next_record_type) {
                (Some(name), Some(record_type)) => RecordSetCursor {
                    name,
                    record_type,
                    identifier: parsed.next_record_identifier,
                },
                _ => {
                    return Err(DnsError::ApiError(format!(
                        "{context}: Route 53 marked page {page} IsTruncated without NextRecordName/NextRecordType; refusing a partial result"
                    )))
                }
            };
            if !seen_cursors.insert(next.clone()) {
                return Err(DnsError::ApiError(format!(
                    "{context}: Route 53 repeated the pagination cursor {} {} after {page} page(s); refusing a partial result",
                    next.name, next.record_type
                )));
            }
            cursor = Some(next);
        }

        Err(DnsError::ApiError(format!(
            "{context} exceeded {} pages; refusing a partial result",
            self.max_pages
        )))
    }

    /// Every record set at exactly (`fqdn`, `record_type`), including several
    /// with different set identifiers. The listing starts at that name and
    /// type, so it reads only the matching record sets plus the first one past
    /// them, never the whole zone.
    async fn exact_record_sets(
        &self,
        zone: &DnsZone,
        fqdn: &str,
        record_type: DnsRecordType,
    ) -> Result<Vec<ResourceRecordSet>, DnsError> {
        let type_name = record_type.to_string();
        let context = format!(
            "Route 53 lookup of {type_name} {} in zone {} ({})",
            Self::normalize_domain(fqdn),
            zone.name,
            zone.id
        );
        let start = RecordSetCursor {
            name: encode_route53_name(fqdn),
            record_type: type_name.clone(),
            identifier: None,
        };
        let mut matches = Vec::new();
        self.scan_record_sets(
            &zone.id,
            Some(start),
            Some(EXACT_LOOKUP_PAGE_SIZE),
            &context,
            |record_set| {
                if dns_names_equal(&decode_route53_name(&record_set.name), fqdn)
                    && record_set.record_type.eq_ignore_ascii_case(&type_name)
                {
                    matches.push(record_set);
                    Scan::Continue
                } else {
                    // Sorted listing: the first other (name, type) means
                    // every match has been seen.
                    Scan::Done
                }
            },
        )
        .await?;
        Ok(matches)
    }

    /// Normalize domain name (remove trailing dot, lowercase)
    fn normalize_domain(domain: &str) -> String {
        domain.trim_end_matches('.').to_lowercase()
    }

    /// Convert Route 53 record type string to our type
    fn parse_record_type(type_str: &str) -> Option<DnsRecordType> {
        match type_str.to_uppercase().as_str() {
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

    /// Convert a Route 53 record to our DnsRecord type
    fn convert_record(record: &ResourceRecordSet, zone_name: &str) -> Vec<DnsRecord> {
        let Some(records_wrapper) = &record.resource_records else {
            return vec![];
        };

        let record_type = match Self::parse_record_type(&record.record_type) {
            Some(t) => t,
            None => return vec![],
        };

        let zone_normalized = Self::normalize_domain(zone_name);
        // Route 53 lists `*` (and anything outside a-z0-9-_) as `\NNN`.
        let fqdn = Self::normalize_domain(&decode_route53_name(&record.name));
        let name = if fqdn == zone_normalized {
            "@".to_string()
        } else {
            fqdn.strip_suffix(&format!(".{}", zone_normalized))
                .unwrap_or(&fqdn)
                .to_string()
        };

        records_wrapper
            .resource_record
            .iter()
            .filter_map(|rr| {
                let content = Self::parse_record_content(record_type, &rr.value)?;
                Some(DnsRecord {
                    id: Some(format!("{}::{}", fqdn, record.record_type)),
                    zone: zone_normalized.clone(),
                    name: name.clone(),
                    fqdn: fqdn.clone(),
                    content,
                    ttl: record.ttl.unwrap_or(300),
                    proxied: false,
                    metadata: HashMap::new(),
                })
            })
            .collect()
    }

    /// Parse record value into DnsRecordContent
    fn parse_record_content(record_type: DnsRecordType, value: &str) -> Option<DnsRecordContent> {
        match record_type {
            DnsRecordType::A => Some(DnsRecordContent::A {
                address: value.to_string(),
            }),
            DnsRecordType::AAAA => Some(DnsRecordContent::AAAA {
                address: value.to_string(),
            }),
            DnsRecordType::CNAME => Some(DnsRecordContent::CNAME {
                target: Self::normalize_domain(value),
            }),
            DnsRecordType::TXT => {
                // Presentation format: one or more quoted ≤255-byte
                // character-strings with `\"`/`\\`/`\DDD` escapes,
                // concatenated back into the original content.
                let content = decode_txt_presentation(value);
                Some(DnsRecordContent::TXT { content })
            }
            DnsRecordType::MX => {
                let parts: Vec<&str> = value.split_whitespace().collect();
                if parts.len() >= 2 {
                    Some(DnsRecordContent::MX {
                        priority: parts[0].parse().unwrap_or(10),
                        target: Self::normalize_domain(parts[1]),
                    })
                } else {
                    None
                }
            }
            DnsRecordType::NS => Some(DnsRecordContent::NS {
                nameserver: Self::normalize_domain(value),
            }),
            DnsRecordType::SRV => {
                let parts: Vec<&str> = value.split_whitespace().collect();
                if parts.len() >= 4 {
                    Some(DnsRecordContent::SRV {
                        priority: parts[0].parse().unwrap_or(0),
                        weight: parts[1].parse().unwrap_or(0),
                        port: parts[2].parse().unwrap_or(0),
                        target: Self::normalize_domain(parts[3]),
                    })
                } else {
                    None
                }
            }
            DnsRecordType::CAA => {
                let parts: Vec<&str> = value.splitn(3, ' ').collect();
                if parts.len() >= 3 {
                    Some(DnsRecordContent::CAA {
                        flags: parts[0].parse().unwrap_or(0),
                        tag: parts[1].to_string(),
                        value: parts[2].trim_matches('"').to_string(),
                    })
                } else {
                    None
                }
            }
            DnsRecordType::PTR => Some(DnsRecordContent::PTR {
                target: Self::normalize_domain(value),
            }),
        }
    }

    /// Format record content for Route 53 API
    fn format_record_value(content: &DnsRecordContent) -> String {
        match content {
            DnsRecordContent::A { address } | DnsRecordContent::AAAA { address } => address.clone(),
            DnsRecordContent::CNAME { target }
            | DnsRecordContent::NS { nameserver: target }
            | DnsRecordContent::PTR { target } => {
                // Route 53 requires trailing dot for FQDN
                if target.ends_with('.') {
                    target.clone()
                } else {
                    format!("{}.", target)
                }
            }
            DnsRecordContent::TXT { content } => {
                // Quoted, escaped, and split into ≤255-byte character-strings:
                // an ownership marker (~370 bytes of JSON) would otherwise be
                // rejected or corrupted.
                encode_txt_presentation(content)
            }
            DnsRecordContent::MX { priority, target } => {
                let target_fqdn = if target.ends_with('.') {
                    target.clone()
                } else {
                    format!("{}.", target)
                };
                format!("{} {}", priority, target_fqdn)
            }
            DnsRecordContent::SRV {
                priority,
                weight,
                port,
                target,
            } => {
                let target_fqdn = if target.ends_with('.') {
                    target.clone()
                } else {
                    format!("{}.", target)
                };
                format!("{} {} {} {}", priority, weight, port, target_fqdn)
            }
            DnsRecordContent::CAA { flags, tag, value } => {
                format!("{} {} \"{}\"", flags, tag, value)
            }
        }
    }

    /// Build FQDN with trailing dot for Route 53
    fn build_fqdn(name: &str, zone: &str) -> String {
        let fqdn = if name == "@" || name.is_empty() {
            zone.to_string()
        } else {
            format!("{}.{}", name, zone)
        };

        if fqdn.ends_with('.') {
            fqdn
        } else {
            format!("{}.", fqdn)
        }
    }
}

#[async_trait]
impl DnsProvider for Route53Provider {
    fn provider_type(&self) -> DnsProviderType {
        DnsProviderType::Route53
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

    async fn test_connection(&self) -> Result<bool, DnsError> {
        match self.list_zones().await {
            Ok(_) => {
                info!("Route53 API connection test successful");
                Ok(true)
            }
            Err(e) => {
                warn!("Route53 API connection test failed: {}", e);
                Ok(false)
            }
        }
    }

    /// Every hosted zone, following `IsTruncated` / `NextMarker`.
    ///
    /// Never returns a partial list: reaching the page cap, a repeated
    /// marker, a truncated page without a marker, or an empty truncated page
    /// is an error.
    async fn list_zones(&self) -> Result<Vec<DnsZone>, DnsError> {
        let path = "/2013-04-01/hostedzone";
        let context = "Route 53 hosted zone listing";
        let mut zones = Vec::new();
        let mut marker: Option<String> = None;
        let mut seen_markers: HashSet<String> = HashSet::new();

        for page in 1..=self.max_pages {
            let query: Vec<(&str, &str)> = match &marker {
                Some(marker) => vec![("marker", marker.as_str())],
                None => Vec::new(),
            };
            let response = self.api_request(Method::GET, path, &query, None).await?;
            let parsed: ListHostedZonesResponse =
                quick_xml::de::from_str(&response).map_err(|e| {
                    DnsError::ApiError(format!(
                        "{context}: failed to parse Route 53 ListHostedZones page {page}: {e}"
                    ))
                })?;

            let page_zones = parsed
                .hosted_zones
                .map(|wrapper| wrapper.hosted_zone)
                .unwrap_or_default();
            let page_len = page_zones.len();
            zones.extend(page_zones.into_iter().map(Self::dns_zone));

            if !parsed.is_truncated {
                return Ok(zones);
            }
            if page_len == 0 {
                return Err(DnsError::ApiError(format!(
                    "{context}: Route 53 returned empty page {page} marked IsTruncated; refusing a partial result"
                )));
            }
            let next = match parsed.next_marker {
                Some(next) if !next.is_empty() => next,
                _ => {
                    return Err(DnsError::ApiError(format!(
                        "{context}: Route 53 marked page {page} IsTruncated without a NextMarker; refusing a partial result"
                    )))
                }
            };
            if !seen_markers.insert(next.clone()) {
                return Err(DnsError::ApiError(format!(
                    "{context}: Route 53 repeated the NextMarker {next} after {page} page(s); refusing a partial result"
                )));
            }
            marker = Some(next);
        }
        Err(DnsError::ApiError(format!(
            "{context} exceeded {} pages; refusing a partial result",
            self.max_pages
        )))
    }

    /// The hosted zone named exactly `domain` (case-insensitive, trailing dot
    /// ignored), found with an exact ListHostedZonesByName lookup.
    ///
    /// Credentials without `route53:ListHostedZonesByName` (HTTP 403) fall
    /// back to the complete [`Self::list_zones`]. When several hosted zones
    /// share the name (for example a public and a private zone), the first
    /// one Route 53 returns is used; there is no public/private preference.
    async fn get_zone(&self, domain: &str) -> Result<Option<DnsZone>, DnsError> {
        let normalized = Self::normalize_domain(domain);
        if normalized.is_empty() {
            return Ok(None);
        }
        match self.lookup_zone_by_name(&normalized).await? {
            Ok(zone) => Ok(zone),
            Err(status) => {
                debug!(
                    "Route 53 ListHostedZonesByName for {} returned {}; falling back to a full hosted zone listing (grant route53:ListHostedZonesByName to avoid it)",
                    normalized, status
                );
                Ok(self
                    .list_zones()
                    .await?
                    .into_iter()
                    .find(|zone| zone.name == normalized))
            }
        }
    }

    async fn list_records(&self, domain: &str) -> Result<Vec<DnsRecord>, DnsError> {
        let zone = self.resolve_zone(domain).await?;
        let context = format!(
            "Route 53 record listing for zone {} ({})",
            zone.name, zone.id
        );

        let mut records = Vec::new();
        self.scan_record_sets(&zone.id, None, None, &context, |record_set| {
            records.extend(Self::convert_record(&record_set, &zone.name));
            Scan::Continue
        })
        .await?;
        Ok(records)
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

    /// Every value at (name, type), read from a listing that starts at that
    /// exact name and type instead of from a zone listing.
    ///
    /// A matching record set whose values temps cannot represent (an alias
    /// record set, or a value that does not parse) is a conflict, never
    /// "absent": treating it as absent would let a write target a name that
    /// is already in use.
    async fn get_records(
        &self,
        domain: &str,
        name: &str,
        record_type: DnsRecordType,
    ) -> Result<Vec<DnsRecord>, DnsError> {
        let zone = self.resolve_zone(domain).await?;
        let fqdn = Self::build_fqdn(name.trim_end_matches('.'), &zone.name);

        let mut records = Vec::new();
        for record_set in self.exact_record_sets(&zone, &fqdn, record_type).await? {
            let values = record_set
                .resource_records
                .as_ref()
                .map_or(0, |wrapper| wrapper.resource_record.len());
            let converted = Self::convert_record(&record_set, &zone.name);
            if values == 0 || converted.len() != values {
                let reason = match record_set
                    .alias_target
                    .as_ref()
                    .and_then(|alias| alias.dns_name.as_deref())
                {
                    Some(target) => format!(
                        "Route 53 has an alias record set at this name and type (to {target}); temps cannot read its value, so it will not manage it"
                    ),
                    None => "Route 53 has a record set at this name and type whose values temps cannot read, so it will not manage it".to_string(),
                };
                return Err(DnsError::RecordConflict {
                    domain: domain.to_string(),
                    name: name.to_string(),
                    record_type: record_type.to_string(),
                    reason,
                });
            }
            records.extend(converted);
        }
        Ok(records)
    }

    /// Create-only: Route 53's `CREATE` action fails when the record set
    /// already exists, which surfaces as [`DnsError::RecordConflict`].
    async fn create_record(
        &self,
        domain: &str,
        request: DnsRecordRequest,
    ) -> Result<DnsRecord, DnsError> {
        let zone = self.resolve_zone(domain).await?;

        let fqdn = Self::build_fqdn(&request.name, &zone.name);
        let record_type = request.content.record_type().to_string();
        let value = Self::format_record_value(&request.content);

        let change_request = ChangeResourceRecordSetsRequest {
            change_batch: ChangeBatch {
                comment: Some("Created by Temps".to_string()),
                changes: Changes {
                    change: vec![Change {
                        action: "CREATE".to_string(),
                        resource_record_set: ChangeResourceRecordSet {
                            name: fqdn.clone(),
                            record_type: record_type.clone(),
                            ttl: request.ttl.unwrap_or(300),
                            resource_records: ChangeResourceRecords {
                                resource_record: vec![ChangeResourceRecord { value }],
                            },
                        },
                    }],
                },
            },
        };

        let body = quick_xml::se::to_string(&change_request)
            .map_err(|e| DnsError::ApiError(format!("Failed to serialize request: {}", e)))?;

        // Add XML namespace
        let body = body.replace(
            "<ChangeResourceRecordSetsRequest>",
            "<ChangeResourceRecordSetsRequest xmlns=\"https://route53.amazonaws.com/doc/2013-04-01/\">",
        );

        let path = format!("/2013-04-01/hostedzone/{}/rrset", zone.id);
        let (status, response_body) = self
            .send_request(Method::POST, &path, &[], Some(&body))
            .await?;
        if Self::is_already_exists_error(status, &response_body) {
            return Err(DnsError::RecordConflict {
                domain: domain.to_string(),
                name: request.name.clone(),
                record_type,
                reason: "a Route 53 record set with this name and type already exists at the provider, and a create never replaces one (Route 53 rejected the CREATE change)".to_string(),
            });
        }
        if !status.is_success() {
            return Err(Self::status_error(
                &Method::POST,
                &path,
                status,
                &response_body,
            ));
        }

        info!("Created DNS record {} for domain {}", request.name, domain);

        Ok(DnsRecord {
            id: Some(format!(
                "{}::{}",
                Self::normalize_domain(&fqdn),
                record_type
            )),
            zone: zone.name.clone(),
            name: request.name,
            fqdn: Self::normalize_domain(&fqdn),
            content: request.content,
            ttl: request.ttl.unwrap_or(300),
            proxied: false,
            metadata: HashMap::new(),
        })
    }

    async fn update_record(
        &self,
        domain: &str,
        _record_id: &str,
        request: DnsRecordRequest,
    ) -> Result<DnsRecord, DnsError> {
        let zone = self.resolve_zone(domain).await?;

        let fqdn = Self::build_fqdn(&request.name, &zone.name);
        let record_type = request.content.record_type().to_string();
        let value = Self::format_record_value(&request.content);

        // Route 53 uses UPSERT for create-or-update
        let change_request = ChangeResourceRecordSetsRequest {
            change_batch: ChangeBatch {
                comment: Some("Updated by Temps".to_string()),
                changes: Changes {
                    change: vec![Change {
                        action: "UPSERT".to_string(),
                        resource_record_set: ChangeResourceRecordSet {
                            name: fqdn.clone(),
                            record_type: record_type.clone(),
                            ttl: request.ttl.unwrap_or(300),
                            resource_records: ChangeResourceRecords {
                                resource_record: vec![ChangeResourceRecord { value }],
                            },
                        },
                    }],
                },
            },
        };

        let body = quick_xml::se::to_string(&change_request)
            .map_err(|e| DnsError::ApiError(format!("Failed to serialize request: {}", e)))?;

        let body = body.replace(
            "<ChangeResourceRecordSetsRequest>",
            "<ChangeResourceRecordSetsRequest xmlns=\"https://route53.amazonaws.com/doc/2013-04-01/\">",
        );

        let path = format!("/2013-04-01/hostedzone/{}/rrset", zone.id);
        self.api_request(Method::POST, &path, &[], Some(&body))
            .await?;

        info!("Updated DNS record {} for domain {}", request.name, domain);

        Ok(DnsRecord {
            id: Some(format!(
                "{}::{}",
                Self::normalize_domain(&fqdn),
                record_type
            )),
            zone: zone.name.clone(),
            name: request.name,
            fqdn: Self::normalize_domain(&fqdn),
            content: request.content,
            ttl: request.ttl.unwrap_or(300),
            proxied: false,
            metadata: HashMap::new(),
        })
    }

    async fn delete_record(&self, domain: &str, record_id: &str) -> Result<(), DnsError> {
        // record_id format: "fqdn::TYPE"
        let parts: Vec<&str> = record_id.split("::").collect();
        if parts.len() != 2 {
            return Err(DnsError::Validation(format!(
                "Invalid record ID format: {}. Expected 'fqdn::TYPE'",
                record_id
            )));
        }

        let fqdn = parts[0].trim_end_matches('.');
        let record_type = Self::parse_record_type(parts[1]).ok_or_else(|| {
            DnsError::Validation(format!(
                "Invalid record ID {record_id} for zone {domain}: '{}' is not a supported record type",
                parts[1]
            ))
        })?;

        // Get the existing record to know its value and TTL (exact lookup,
        // not a zone listing)
        let zone = self.resolve_zone(domain).await?;
        let existing = self
            .exact_record_sets(&zone, &format!("{fqdn}."), record_type)
            .await?
            .iter()
            .flat_map(|record_set| Self::convert_record(record_set, &zone.name))
            .next()
            .ok_or_else(|| DnsError::RecordNotFound(record_id.to_string()))?;

        let value = Self::format_record_value(&existing.content);

        let change_request = ChangeResourceRecordSetsRequest {
            change_batch: ChangeBatch {
                comment: Some("Deleted by Temps".to_string()),
                changes: Changes {
                    change: vec![Change {
                        action: "DELETE".to_string(),
                        resource_record_set: ChangeResourceRecordSet {
                            name: format!("{}.", fqdn),
                            record_type: record_type.to_string(),
                            ttl: existing.ttl,
                            resource_records: ChangeResourceRecords {
                                resource_record: vec![ChangeResourceRecord { value }],
                            },
                        },
                    }],
                },
            },
        };

        let body = quick_xml::se::to_string(&change_request)
            .map_err(|e| DnsError::ApiError(format!("Failed to serialize request: {}", e)))?;

        let body = body.replace(
            "<ChangeResourceRecordSetsRequest>",
            "<ChangeResourceRecordSetsRequest xmlns=\"https://route53.amazonaws.com/doc/2013-04-01/\">",
        );

        let path = format!("/2013-04-01/hostedzone/{}/rrset", zone.id);
        self.api_request(Method::POST, &path, &[], Some(&body))
            .await?;

        info!("Deleted DNS record {} from domain {}", record_id, domain);

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ==================== Helper function tests ====================

    #[test]
    fn test_normalize_domain() {
        assert_eq!(
            Route53Provider::normalize_domain("example.com."),
            "example.com"
        );
        assert_eq!(
            Route53Provider::normalize_domain("example.com"),
            "example.com"
        );
        assert_eq!(
            Route53Provider::normalize_domain("SUB.Example.COM."),
            "sub.example.com"
        );
    }

    #[test]
    fn test_build_fqdn() {
        assert_eq!(
            Route53Provider::build_fqdn("www", "example.com"),
            "www.example.com."
        );
        assert_eq!(
            Route53Provider::build_fqdn("@", "example.com"),
            "example.com."
        );
        assert_eq!(
            Route53Provider::build_fqdn("", "example.com"),
            "example.com."
        );
        assert_eq!(
            Route53Provider::build_fqdn("sub.www", "example.com"),
            "sub.www.example.com."
        );
    }

    #[test]
    fn test_format_record_value_a() {
        let content = DnsRecordContent::A {
            address: "192.0.2.1".to_string(),
        };
        assert_eq!(Route53Provider::format_record_value(&content), "192.0.2.1");
    }

    #[test]
    fn test_format_record_value_txt() {
        let content = DnsRecordContent::TXT {
            content: "v=spf1 -all".to_string(),
        };
        assert_eq!(
            Route53Provider::format_record_value(&content),
            "\"v=spf1 -all\""
        );
    }

    #[test]
    fn test_format_record_value_cname() {
        let content = DnsRecordContent::CNAME {
            target: "www.example.com".to_string(),
        };
        assert_eq!(
            Route53Provider::format_record_value(&content),
            "www.example.com."
        );
    }

    #[test]
    fn test_format_record_value_mx() {
        let content = DnsRecordContent::MX {
            priority: 10,
            target: "mail.example.com".to_string(),
        };
        assert_eq!(
            Route53Provider::format_record_value(&content),
            "10 mail.example.com."
        );
    }

    #[test]
    fn test_parse_record_type() {
        assert_eq!(
            Route53Provider::parse_record_type("A"),
            Some(DnsRecordType::A)
        );
        assert_eq!(
            Route53Provider::parse_record_type("aaaa"),
            Some(DnsRecordType::AAAA)
        );
        assert_eq!(
            Route53Provider::parse_record_type("TXT"),
            Some(DnsRecordType::TXT)
        );
        assert_eq!(Route53Provider::parse_record_type("UNKNOWN"), None);
    }

    #[test]
    fn test_parse_record_content_a() {
        let content = Route53Provider::parse_record_content(DnsRecordType::A, "192.0.2.1");
        assert!(content.is_some());
        if let Some(DnsRecordContent::A { address }) = content {
            assert_eq!(address, "192.0.2.1");
        } else {
            panic!("Expected A record");
        }
    }

    #[test]
    fn test_parse_record_content_txt() {
        let content = Route53Provider::parse_record_content(DnsRecordType::TXT, "\"v=spf1 -all\"");
        assert!(content.is_some());
        if let Some(DnsRecordContent::TXT { content }) = content {
            assert_eq!(content, "v=spf1 -all");
        } else {
            panic!("Expected TXT record");
        }
    }

    #[test]
    fn test_txt_ownership_marker_round_trips_through_presentation_format() {
        // Shaped like a temps ownership marker: ~400 bytes of JSON, full of
        // quotes, with a backslash for good measure. Must be split into
        // ≤255-byte character-strings and read back byte-for-byte.
        let marker = format!(
            r#"{{"managed_by":"temps","instance":"{}","zone":"example.com","name":"app","note":"a\\b","pad":"{}"}}"#,
            "0".repeat(36),
            "f".repeat(300)
        );
        assert!(marker.len() >= 400);
        let original = DnsRecordContent::TXT {
            content: marker.clone(),
        };

        let rdata = Route53Provider::format_record_value(&original);
        assert!(rdata.starts_with('"') && rdata.ends_with('"'));
        assert!(
            rdata.contains("\" \""),
            "long content must be split: {rdata}"
        );
        assert!(rdata.contains("\\\""), "embedded quotes must be escaped");

        match Route53Provider::parse_record_content(DnsRecordType::TXT, &rdata) {
            Some(DnsRecordContent::TXT { content }) => assert_eq!(content, marker),
            other => panic!("Expected TXT record, got {other:?}"),
        }
    }

    #[test]
    fn test_txt_multi_string_rdata_is_concatenated() {
        match Route53Provider::parse_record_content(
            DnsRecordType::TXT,
            "\"v=DKIM1; k=rsa; \" \"p=abc\"",
        ) {
            Some(DnsRecordContent::TXT { content }) => {
                assert_eq!(content, "v=DKIM1; k=rsa; p=abc")
            }
            other => panic!("Expected TXT record, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_record_content_mx() {
        let content =
            Route53Provider::parse_record_content(DnsRecordType::MX, "10 mail.example.com.");
        assert!(content.is_some());
        if let Some(DnsRecordContent::MX { priority, target }) = content {
            assert_eq!(priority, 10);
            assert_eq!(target, "mail.example.com");
        } else {
            panic!("Expected MX record");
        }
    }

    // ==================== Provider tests ====================

    #[test]
    fn test_provider_type() {
        let creds = Route53Credentials {
            access_key_id: "AKIATEST".to_string(),
            secret_access_key: "secret".to_string(),
            session_token: None,
            region: None,
        };
        let provider = Route53Provider::new(creds).unwrap();
        assert_eq!(provider.provider_type(), DnsProviderType::Route53);
    }

    #[test]
    fn test_capabilities() {
        let creds = Route53Credentials {
            access_key_id: "AKIATEST".to_string(),
            secret_access_key: "secret".to_string(),
            session_token: None,
            region: None,
        };
        let provider = Route53Provider::new(creds).unwrap();
        let caps = provider.capabilities();

        assert!(caps.a_record);
        assert!(caps.aaaa_record);
        assert!(caps.cname_record);
        assert!(caps.txt_record);
        assert!(caps.mx_record);
        assert!(caps.ns_record);
        assert!(caps.srv_record);
        assert!(caps.caa_record);
        assert!(!caps.proxy);
        assert!(!caps.auto_ssl);
        assert!(caps.wildcard);
    }

    #[test]
    fn test_default_region() {
        let creds = Route53Credentials {
            access_key_id: "AKIATEST".to_string(),
            secret_access_key: "secret".to_string(),
            session_token: None,
            region: None,
        };
        let provider = Route53Provider::new(creds).unwrap();
        assert_eq!(provider.region, "us-east-1");
    }

    #[test]
    fn test_custom_region() {
        let creds = Route53Credentials {
            access_key_id: "AKIATEST".to_string(),
            secret_access_key: "secret".to_string(),
            session_token: None,
            region: Some("eu-west-1".to_string()),
        };
        let provider = Route53Provider::new(creds).unwrap();
        assert_eq!(provider.region, "eu-west-1");
    }

    #[test]
    fn production_endpoint_signs_the_aws_host() {
        let creds = Route53Credentials {
            access_key_id: "AKIATEST".to_string(),
            secret_access_key: "secret".to_string(),
            session_token: None,
            region: None,
        };
        let provider = Route53Provider::new(creds).unwrap();
        assert_eq!(provider.endpoint, "https://route53.amazonaws.com");
        assert_eq!(provider.host, "route53.amazonaws.com");
        assert_eq!(provider.max_pages, MAX_PAGES);
    }

    #[test]
    fn canonical_query_string_is_sorted_and_rfc3986_encoded() {
        assert_eq!(canonical_query_string(&[]), "");
        assert_eq!(
            canonical_query_string(&[
                ("type", "A"),
                ("name", "\\052.app.example.com."),
                ("maxitems", "100"),
                ("identifier", "weight one/2"),
            ]),
            "identifier=weight%20one%2F2&maxitems=100&name=%5C052.app.example.com.&type=A"
        );
    }

    #[test]
    fn route53_names_decode_octal_escapes() {
        assert_eq!(
            decode_route53_name("\\052.app.example.com."),
            "*.app.example.com."
        );
        assert_eq!(decode_route53_name("\\052-staging"), "*-staging");
        // Not a valid octal escape: kept literally, never panics.
        assert_eq!(decode_route53_name("a\\08x"), "a\\08x");
        assert_eq!(decode_route53_name("trailing\\05"), "trailing\\05");
        assert_eq!(
            decode_route53_name("plain.example.com."),
            "plain.example.com."
        );
    }

    #[test]
    fn route53_names_encode_like_the_listing() {
        assert_eq!(
            encode_route53_name("*.App.example.com."),
            "\\052.app.example.com."
        );
        assert_eq!(
            encode_route53_name("_temps-owned-a._w.app.example.com."),
            "_temps-owned-a._w.app.example.com."
        );
        for name in [
            "*.preview.example.com.",
            "a b.example.com.",
            "x\\y.example.com.",
        ] {
            assert_eq!(
                decode_route53_name(&encode_route53_name(name)),
                name.to_ascii_lowercase()
            );
        }
    }

    #[test]
    fn already_exists_detection_needs_the_invalid_change_batch_shape() {
        let body = "<InvalidChangeBatch><Messages><Message>Tried to create resource record set [name='app.example.com.', type='A'] but it already exists</Message></Messages></InvalidChangeBatch>";
        assert!(Route53Provider::is_already_exists_error(
            StatusCode::BAD_REQUEST,
            body
        ));
        assert!(!Route53Provider::is_already_exists_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            body
        ));
        assert!(!Route53Provider::is_already_exists_error(
            StatusCode::BAD_REQUEST,
            "<ErrorResponse><Error><Code>InvalidInput</Code></Error></ErrorResponse>"
        ));
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;

    use wiremock::matchers::{
        any, body_string_contains, header_exists, method, path, query_param, query_param_is_missing,
    };
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const RRSET_PATH: &str = "/2013-04-01/hostedzone/ZEXAMPLE/rrset";

    fn create_mock_provider(mock_server: &MockServer) -> Route53Provider {
        let creds = Route53Credentials {
            access_key_id: "AKIATESTKEY".to_string(),
            secret_access_key: "testsecretkey".to_string(),
            session_token: None,
            region: Some("us-east-1".to_string()),
        };
        Route53Provider::with_endpoint(creds, &mock_server.uri()).unwrap()
    }

    fn hosted_zone(id: &str, name: &str) -> String {
        format!(
            "<HostedZone><Id>/hostedzone/{id}</Id><Name>{name}</Name><CallerReference>ref</CallerReference></HostedZone>"
        )
    }

    /// One ListHostedZones page; `next_marker` marks it truncated.
    fn zones_page(zones: &[String], next_marker: Option<&str>) -> String {
        let cursor = match next_marker {
            Some(marker) => {
                format!("<IsTruncated>true</IsTruncated><NextMarker>{marker}</NextMarker>")
            }
            None => "<IsTruncated>false</IsTruncated>".to_string(),
        };
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><ListHostedZonesResponse xmlns="https://route53.amazonaws.com/doc/2013-04-01/"><HostedZones>{}</HostedZones>{cursor}<MaxItems>100</MaxItems></ListHostedZonesResponse>"#,
            zones.concat()
        )
    }

    /// ListHostedZonesByName answer starting at `dnsname`.
    fn zones_by_name(dns_name: &str, zones: &[String]) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><ListHostedZonesByNameResponse xmlns="https://route53.amazonaws.com/doc/2013-04-01/"><HostedZones>{}</HostedZones><DNSName>{dns_name}</DNSName><IsTruncated>false</IsTruncated><MaxItems>1</MaxItems></ListHostedZonesByNameResponse>"#,
            zones.concat()
        )
    }

    /// Exact zone lookup answering `example.com` (`ZEXAMPLE`).
    async fn mount_zone(server: &MockServer) {
        Mock::given(method("GET"))
            .and(path("/2013-04-01/hostedzonesbyname"))
            .and(query_param("dnsname", "example.com."))
            .and(query_param("maxitems", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_string(zones_by_name(
                "example.com.",
                &[hosted_zone("ZEXAMPLE", "example.com.")],
            )))
            .mount(server)
            .await;
    }

    fn rrset(name: &str, record_type: &str, values: &[&str]) -> String {
        let values: String = values
            .iter()
            .map(|value| format!("<ResourceRecord><Value>{value}</Value></ResourceRecord>"))
            .collect();
        format!(
            "<ResourceRecordSet><Name>{name}</Name><Type>{record_type}</Type><TTL>300</TTL><ResourceRecords>{values}</ResourceRecords></ResourceRecordSet>"
        )
    }

    fn weighted_rrset(name: &str, identifier: &str, value: &str) -> String {
        format!(
            "<ResourceRecordSet><Name>{name}</Name><Type>A</Type><SetIdentifier>{identifier}</SetIdentifier><Weight>10</Weight><TTL>300</TTL><ResourceRecords><ResourceRecord><Value>{value}</Value></ResourceRecord></ResourceRecords></ResourceRecordSet>"
        )
    }

    /// One ListResourceRecordSets page; `next` = (name, type, identifier)
    /// marks it truncated.
    fn page(record_sets: &[String], next: Option<(&str, &str, Option<&str>)>) -> String {
        let cursor = match next {
            Some((name, record_type, identifier)) => format!(
                "<IsTruncated>true</IsTruncated><NextRecordName>{name}</NextRecordName><NextRecordType>{record_type}</NextRecordType>{}",
                identifier
                    .map(|id| format!("<NextRecordIdentifier>{id}</NextRecordIdentifier>"))
                    .unwrap_or_default()
            ),
            None => "<IsTruncated>false</IsTruncated>".to_string(),
        };
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><ListResourceRecordSetsResponse xmlns="https://route53.amazonaws.com/doc/2013-04-01/"><ResourceRecordSets>{}</ResourceRecordSets>{cursor}<MaxItems>300</MaxItems></ListResourceRecordSetsResponse>"#,
            record_sets.concat()
        )
    }

    /// The zone's first page (no start cursor).
    async fn mount_first_page(server: &MockServer, body: String) {
        Mock::given(method("GET"))
            .and(path(RRSET_PATH))
            .and(query_param_is_missing("name"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(server)
            .await;
    }

    /// A page starting at (`name`, `record_type`) with no set identifier.
    async fn mount_page_at(server: &MockServer, name: &str, record_type: &str, body: String) {
        Mock::given(method("GET"))
            .and(path(RRSET_PATH))
            .and(query_param("name", name))
            .and(query_param("type", record_type))
            .and(query_param_is_missing("identifier"))
            .and(header_exists("authorization"))
            .and(header_exists("x-amz-date"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(server)
            .await;
    }

    fn a_request(name: &str, address: &str) -> DnsRecordRequest {
        DnsRecordRequest {
            name: name.to_string(),
            content: DnsRecordContent::A {
                address: address.to_string(),
            },
            ttl: Some(300),
            proxied: false,
        }
    }

    #[tokio::test]
    async fn list_records_follows_truncated_pages() {
        let server = MockServer::start().await;
        mount_zone(&server).await;
        mount_first_page(
            &server,
            page(
                &[rrset("a.example.com.", "A", &["203.0.113.1"])],
                Some(("b.example.com.", "A", None)),
            ),
        )
        .await;
        mount_page_at(
            &server,
            "b.example.com.",
            "A",
            page(
                &[weighted_rrset("b.example.com.", "one", "203.0.113.2")],
                Some(("b.example.com.", "A", Some("two"))),
            ),
        )
        .await;
        // The third page continues inside the same (name, type), so the
        // identifier has to be sent too.
        Mock::given(method("GET"))
            .and(path(RRSET_PATH))
            .and(query_param("name", "b.example.com."))
            .and(query_param("identifier", "two"))
            .respond_with(ResponseTemplate::new(200).set_body_string(page(
                &[
                    weighted_rrset("b.example.com.", "two", "203.0.113.3"),
                    rrset("_temps-owned-a.app.example.com.", "TXT", &["\"marker\""]),
                ],
                None,
            )))
            .mount(&server)
            .await;

        let provider = create_mock_provider(&server);
        let records = provider.list_records("example.com").await.unwrap();

        let names: Vec<&str> = records.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b", "b", "_temps-owned-a.app"]);
    }

    #[tokio::test]
    async fn list_records_fails_closed_on_a_repeated_cursor() {
        let server = MockServer::start().await;
        mount_zone(&server).await;
        mount_first_page(
            &server,
            page(
                &[rrset("a.example.com.", "A", &["203.0.113.1"])],
                Some(("b.example.com.", "A", None)),
            ),
        )
        .await;
        mount_page_at(
            &server,
            "b.example.com.",
            "A",
            page(
                &[rrset("b.example.com.", "A", &["203.0.113.2"])],
                Some(("b.example.com.", "A", None)),
            ),
        )
        .await;

        let provider = create_mock_provider(&server);
        let error = provider.list_records("example.com").await.unwrap_err();

        assert!(
            matches!(&error, DnsError::ApiError(message)
                if message.contains("repeated the pagination cursor b.example.com. A after 2 page(s)")),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn list_records_fails_closed_on_a_truncated_page_without_a_cursor() {
        let server = MockServer::start().await;
        mount_zone(&server).await;
        mount_first_page(
            &server,
            page(&[rrset("a.example.com.", "A", &["203.0.113.1"])], None).replace(
                "<IsTruncated>false</IsTruncated>",
                "<IsTruncated>true</IsTruncated>",
            ),
        )
        .await;

        let provider = create_mock_provider(&server);
        let error = provider.list_records("example.com").await.unwrap_err();

        assert!(
            matches!(&error, DnsError::ApiError(message) if message.contains("without NextRecordName")),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn list_records_fails_closed_on_an_empty_truncated_page() {
        let server = MockServer::start().await;
        mount_zone(&server).await;
        mount_first_page(&server, page(&[], Some(("b.example.com.", "A", None)))).await;

        let provider = create_mock_provider(&server);
        let error = provider.list_records("example.com").await.unwrap_err();

        assert!(
            matches!(&error, DnsError::ApiError(message) if message.contains("empty page 1 marked IsTruncated")),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn list_records_fails_closed_at_the_page_cap() {
        let server = MockServer::start().await;
        mount_zone(&server).await;
        mount_first_page(
            &server,
            page(
                &[rrset("a.example.com.", "A", &["203.0.113.1"])],
                Some(("b.example.com.", "A", None)),
            ),
        )
        .await;
        mount_page_at(
            &server,
            "b.example.com.",
            "A",
            page(
                &[rrset("b.example.com.", "A", &["203.0.113.2"])],
                Some(("c.example.com.", "A", None)),
            ),
        )
        .await;
        Mock::given(method("GET"))
            .and(query_param("name", "c.example.com."))
            .respond_with(ResponseTemplate::new(200).set_body_string(page(&[], None)))
            .expect(0)
            .mount(&server)
            .await;

        let mut provider = create_mock_provider(&server);
        provider.max_pages = 2;
        let error = provider.list_records("example.com").await.unwrap_err();

        assert!(
            matches!(&error, DnsError::ApiError(message)
                if message.contains("Route 53 record listing for zone example.com (ZEXAMPLE) exceeded 2 pages")),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn get_records_starts_at_the_exact_name_and_returns_every_value() {
        let server = MockServer::start().await;
        mount_zone(&server).await;
        // A zone listing must never be used for an exact lookup.
        Mock::given(method("GET"))
            .and(path(RRSET_PATH))
            .and(query_param_is_missing("name"))
            .respond_with(ResponseTemplate::new(200).set_body_string(page(&[], None)))
            .expect(0)
            .mount(&server)
            .await;
        // The name is sent lowercased; the next record set ends the lookup.
        Mock::given(method("GET"))
            .and(path(RRSET_PATH))
            .and(query_param("name", "app.example.com."))
            .and(query_param("type", "A"))
            .and(query_param("maxitems", "100"))
            .respond_with(ResponseTemplate::new(200).set_body_string(page(
                &[
                    rrset("app.example.com.", "A", &["203.0.113.1", "203.0.113.2"]),
                    rrset("app.example.com.", "TXT", &["\"unrelated\""]),
                ],
                Some(("b.example.com.", "A", None)),
            )))
            .expect(2)
            .mount(&server)
            .await;

        let provider = create_mock_provider(&server);
        let records = provider
            .get_records("example.com", "App", DnsRecordType::A)
            .await
            .unwrap();

        let values: Vec<String> = records
            .iter()
            .map(|r| r.content.to_value_string())
            .collect();
        assert_eq!(values, vec!["203.0.113.1", "203.0.113.2"]);
        assert!(records.iter().all(|r| r.name == "app"));

        let record = provider
            .get_record("example.com", "APP", DnsRecordType::A)
            .await
            .unwrap();
        assert_eq!(record.map(|r| r.fqdn), Some("app.example.com".to_string()));
    }

    #[tokio::test]
    async fn get_records_reports_absent_when_the_listing_starts_past_the_name() {
        let server = MockServer::start().await;
        mount_zone(&server).await;
        mount_page_at(
            &server,
            "missing.example.com.",
            "A",
            page(
                &[rrset("www.example.com.", "A", &["203.0.113.1"])],
                Some(("x.example.com.", "A", None)),
            ),
        )
        .await;

        let provider = create_mock_provider(&server);
        let records = provider
            .get_records("example.com", "missing", DnsRecordType::A)
            .await
            .unwrap();

        assert!(records.is_empty());
    }

    #[tokio::test]
    async fn get_records_follows_pages_within_the_same_name_and_type() {
        let server = MockServer::start().await;
        mount_zone(&server).await;
        mount_page_at(
            &server,
            "app.example.com.",
            "A",
            page(
                &[weighted_rrset("app.example.com.", "one", "203.0.113.1")],
                Some(("app.example.com.", "A", Some("two"))),
            ),
        )
        .await;
        Mock::given(method("GET"))
            .and(path(RRSET_PATH))
            .and(query_param("name", "app.example.com."))
            .and(query_param("identifier", "two"))
            .respond_with(ResponseTemplate::new(200).set_body_string(page(
                &[
                    weighted_rrset("app.example.com.", "two", "203.0.113.2"),
                    rrset("b.example.com.", "A", &["203.0.113.9"]),
                ],
                None,
            )))
            .mount(&server)
            .await;

        let provider = create_mock_provider(&server);
        let records = provider
            .get_records("example.com", "app", DnsRecordType::A)
            .await
            .unwrap();

        // Both weighted record sets are visible, so the ownership layer sees
        // more than one value and refuses to manage the name.
        assert_eq!(records.len(), 2);
    }

    #[tokio::test]
    async fn get_records_decodes_escaped_wildcard_names() {
        let server = MockServer::start().await;
        mount_zone(&server).await;
        mount_page_at(
            &server,
            "\\052.preview.example.com.",
            "A",
            page(
                &[rrset("\\052.preview.example.com.", "A", &["203.0.113.1"])],
                None,
            ),
        )
        .await;

        let provider = create_mock_provider(&server);
        let records = provider
            .get_records("example.com", "*.preview", DnsRecordType::A)
            .await
            .unwrap();

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].name, "*.preview");
        assert_eq!(records[0].fqdn, "*.preview.example.com");
    }

    #[tokio::test]
    async fn get_records_refuses_alias_record_sets() {
        let server = MockServer::start().await;
        mount_zone(&server).await;
        mount_page_at(
            &server,
            "app.example.com.",
            "A",
            page(
                &["<ResourceRecordSet><Name>app.example.com.</Name><Type>A</Type><AliasTarget><HostedZoneId>ZALIAS</HostedZoneId><DNSName>lb.example.net.</DNSName><EvaluateTargetHealth>false</EvaluateTargetHealth></AliasTarget></ResourceRecordSet>".to_string()],
                None,
            ),
        )
        .await;

        let provider = create_mock_provider(&server);
        let error = provider
            .get_records("example.com", "app", DnsRecordType::A)
            .await
            .unwrap_err();

        assert!(
            matches!(&error, DnsError::RecordConflict { name, reason, .. }
                if name == "app" && reason.contains("alias record set") && reason.contains("lb.example.net.")),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn create_record_uses_create_and_maps_an_existing_record_set_to_a_conflict() {
        let server = MockServer::start().await;
        mount_zone(&server).await;
        Mock::given(method("POST"))
            .and(path(RRSET_PATH))
            .and(body_string_contains("<Action>CREATE</Action>"))
            .respond_with(ResponseTemplate::new(400).set_body_string(
                r#"<?xml version="1.0"?><InvalidChangeBatch xmlns="https://route53.amazonaws.com/doc/2013-04-01/"><Messages><Message>Tried to create resource record set [name='app.example.com.', type='A'] but it already exists</Message></Messages><RequestId>req</RequestId></InvalidChangeBatch>"#,
            ))
            .expect(1)
            .mount(&server)
            .await;

        let provider = create_mock_provider(&server);
        let error = provider
            .create_record("example.com", a_request("app", "203.0.113.1"))
            .await
            .unwrap_err();

        assert!(
            matches!(&error, DnsError::RecordConflict { domain, name, record_type, .. }
                if domain == "example.com" && name == "app" && record_type == "A"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn delete_record_reads_only_the_target_record_set() {
        let server = MockServer::start().await;
        mount_zone(&server).await;
        Mock::given(method("GET"))
            .and(path(RRSET_PATH))
            .and(query_param_is_missing("name"))
            .respond_with(ResponseTemplate::new(200).set_body_string(page(&[], None)))
            .expect(0)
            .mount(&server)
            .await;
        mount_page_at(
            &server,
            "app.example.com.",
            "A",
            page(&[rrset("app.example.com.", "A", &["203.0.113.1"])], None),
        )
        .await;
        Mock::given(method("POST"))
            .and(path(RRSET_PATH))
            .and(body_string_contains("<Action>DELETE</Action>"))
            .and(body_string_contains("<Value>203.0.113.1</Value>"))
            .respond_with(ResponseTemplate::new(200).set_body_string("<ok/>"))
            .expect(1)
            .mount(&server)
            .await;

        let provider = create_mock_provider(&server);
        provider
            .delete_record("example.com", "app.example.com::A")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn api_errors_embed_a_bounded_slice_of_the_body() {
        let server = MockServer::start().await;
        Mock::given(any())
            .respond_with(ResponseTemplate::new(500).set_body_string("e".repeat(20_000)))
            .mount(&server)
            .await;

        let provider = create_mock_provider(&server);
        let error = provider.list_records("example.com").await.unwrap_err();

        let message = error.to_string();
        assert!(message.contains("500"), "{message}");
        assert!(
            message.contains("truncated, 20000 bytes total"),
            "{message}"
        );
        assert!(message.len() < 1_000, "error is {} bytes", message.len());
    }

    /// The first ListHostedZones page (no marker).
    async fn mount_first_zones_page(server: &MockServer, body: String) {
        Mock::given(method("GET"))
            .and(path("/2013-04-01/hostedzone"))
            .and(query_param_is_missing("marker"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(server)
            .await;
    }

    /// The ListHostedZones page starting at `marker`.
    async fn mount_zones_page_at(server: &MockServer, marker: &str, body: String) {
        Mock::given(method("GET"))
            .and(path("/2013-04-01/hostedzone"))
            .and(query_param("marker", marker))
            .and(header_exists("authorization"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn list_zones_follows_next_marker() {
        let server = MockServer::start().await;
        mount_first_zones_page(
            &server,
            zones_page(&[hosted_zone("ZONE1", "example.com.")], Some("ZONE2")),
        )
        .await;
        mount_zones_page_at(
            &server,
            "ZONE2",
            zones_page(&[hosted_zone("ZONE2", "example.net.")], None),
        )
        .await;

        let zones = create_mock_provider(&server).list_zones().await.unwrap();

        let zones: Vec<(&str, &str)> = zones
            .iter()
            .map(|zone| (zone.id.as_str(), zone.name.as_str()))
            .collect();
        assert_eq!(
            zones,
            vec![("ZONE1", "example.com"), ("ZONE2", "example.net")]
        );
    }

    #[tokio::test]
    async fn list_zones_fails_closed() {
        // A NextMarker that was already followed.
        let server = MockServer::start().await;
        mount_first_zones_page(
            &server,
            zones_page(&[hosted_zone("ZONE1", "example.com.")], Some("ZONE2")),
        )
        .await;
        mount_zones_page_at(
            &server,
            "ZONE2",
            zones_page(&[hosted_zone("ZONE2", "example.net.")], Some("ZONE2")),
        )
        .await;
        let error = create_mock_provider(&server)
            .list_zones()
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("repeated the NextMarker ZONE2"),
            "{error}"
        );

        // A truncated page without a NextMarker.
        let server = MockServer::start().await;
        mount_first_zones_page(
            &server,
            zones_page(&[hosted_zone("ZONE1", "example.com.")], None).replace(
                "<IsTruncated>false</IsTruncated>",
                "<IsTruncated>true</IsTruncated>",
            ),
        )
        .await;
        let error = create_mock_provider(&server)
            .list_zones()
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("without a NextMarker"),
            "{error}"
        );

        // An empty page that claims more follow.
        let server = MockServer::start().await;
        mount_first_zones_page(&server, zones_page(&[], Some("ZONE2"))).await;
        let error = create_mock_provider(&server)
            .list_zones()
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("empty page 1 marked IsTruncated"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn list_zones_fails_closed_at_the_page_cap() {
        let server = MockServer::start().await;
        mount_first_zones_page(
            &server,
            zones_page(&[hosted_zone("ZONE1", "example.com.")], Some("ZONE2")),
        )
        .await;
        mount_zones_page_at(
            &server,
            "ZONE2",
            zones_page(&[hosted_zone("ZONE2", "example.net.")], Some("ZONE3")),
        )
        .await;
        Mock::given(method("GET"))
            .and(path("/2013-04-01/hostedzone"))
            .and(query_param("marker", "ZONE3"))
            .respond_with(ResponseTemplate::new(200).set_body_string(zones_page(&[], None)))
            .expect(0)
            .mount(&server)
            .await;

        let mut provider = create_mock_provider(&server);
        provider.max_pages = 2;
        let error = provider.list_zones().await.unwrap_err();

        assert!(
            error
                .to_string()
                .contains("Route 53 hosted zone listing exceeded 2 pages"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn get_zone_uses_an_exact_lookup_by_name() {
        let server = MockServer::start().await;
        mount_zone(&server).await;
        // ListHostedZonesByName starts at the requested name; for a name with
        // no hosted zone the first entry is some other zone, never a match.
        Mock::given(method("GET"))
            .and(path("/2013-04-01/hostedzonesbyname"))
            .and(query_param("dnsname", "missing.example.com."))
            .and(query_param("maxitems", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_string(zones_by_name(
                "missing.example.com.",
                &[hosted_zone("ZOTHER", "example.net.")],
            )))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/2013-04-01/hostedzone"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;

        let provider = create_mock_provider(&server);
        let zone = provider.get_zone("Example.COM.").await.unwrap().unwrap();
        assert_eq!(zone.id, "ZEXAMPLE");
        assert_eq!(zone.name, "example.com");

        assert!(provider
            .get_zone("missing.example.com")
            .await
            .unwrap()
            .is_none());
        assert!(matches!(
            provider
                .get_records("missing.example.com", "www", DnsRecordType::A)
                .await,
            Err(DnsError::ZoneNotFound(zone)) if zone == "missing.example.com"
        ));
    }

    #[tokio::test]
    async fn get_zone_without_the_by_name_permission_falls_back_to_a_full_listing() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/2013-04-01/hostedzonesbyname"))
            .respond_with(ResponseTemplate::new(403).set_body_string(
                "<ErrorResponse><Error><Code>AccessDenied</Code></Error></ErrorResponse>",
            ))
            .expect(1)
            .mount(&server)
            .await;
        // The zone is only on the second listing page.
        mount_first_zones_page(
            &server,
            zones_page(&[hosted_zone("ZONE1", "example.net.")], Some("ZEXAMPLE")),
        )
        .await;
        mount_zones_page_at(
            &server,
            "ZEXAMPLE",
            zones_page(&[hosted_zone("ZEXAMPLE", "example.com.")], None),
        )
        .await;

        let zone = create_mock_provider(&server)
            .get_zone("example.com")
            .await
            .unwrap()
            .unwrap();

        assert_eq!(zone.id, "ZEXAMPLE");
        assert_eq!(zone.name, "example.com");
    }

    #[tokio::test]
    async fn get_zone_propagates_lookup_failures_instead_of_reporting_absent() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/2013-04-01/hostedzonesbyname"))
            .respond_with(ResponseTemplate::new(500).set_body_string("internal failure"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/2013-04-01/hostedzone"))
            .respond_with(ResponseTemplate::new(200).set_body_string(zones_page(&[], None)))
            .expect(0)
            .mount(&server)
            .await;

        let error = create_mock_provider(&server)
            .get_zone("example.com")
            .await
            .unwrap_err();

        let message = error.to_string();
        assert!(message.contains("500"), "{message}");
        assert!(message.contains("hostedzonesbyname"), "{message}");
    }

    #[tokio::test]
    async fn test_list_zones_parsing() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
        <ListHostedZonesResponse xmlns="https://route53.amazonaws.com/doc/2013-04-01/">
            <HostedZones>
                <HostedZone>
                    <Id>/hostedzone/Z1234567890ABC</Id>
                    <Name>example.com.</Name>
                    <CallerReference>test-ref-1</CallerReference>
                </HostedZone>
                <HostedZone>
                    <Id>/hostedzone/Z0987654321XYZ</Id>
                    <Name>test.org.</Name>
                    <CallerReference>test-ref-2</CallerReference>
                </HostedZone>
            </HostedZones>
        </ListHostedZonesResponse>"#;

        let parsed: ListHostedZonesResponse = quick_xml::de::from_str(xml).unwrap();
        let zones = parsed.hosted_zones.unwrap().hosted_zone;

        assert_eq!(zones.len(), 2);
        assert_eq!(zones[0].id, "/hostedzone/Z1234567890ABC");
        assert_eq!(zones[0].name, "example.com.");
        assert_eq!(zones[1].id, "/hostedzone/Z0987654321XYZ");
        assert_eq!(zones[1].name, "test.org.");
    }

    #[tokio::test]
    async fn test_list_records_parsing() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
        <ListResourceRecordSetsResponse xmlns="https://route53.amazonaws.com/doc/2013-04-01/">
            <ResourceRecordSets>
                <ResourceRecordSet>
                    <Name>www.example.com.</Name>
                    <Type>A</Type>
                    <TTL>300</TTL>
                    <ResourceRecords>
                        <ResourceRecord>
                            <Value>192.0.2.1</Value>
                        </ResourceRecord>
                    </ResourceRecords>
                </ResourceRecordSet>
                <ResourceRecordSet>
                    <Name>example.com.</Name>
                    <Type>TXT</Type>
                    <TTL>3600</TTL>
                    <ResourceRecords>
                        <ResourceRecord>
                            <Value>"v=spf1 -all"</Value>
                        </ResourceRecord>
                    </ResourceRecords>
                </ResourceRecordSet>
            </ResourceRecordSets>
        </ListResourceRecordSetsResponse>"#;

        let parsed: ListResourceRecordSetsResponse = quick_xml::de::from_str(xml).unwrap();
        let records = parsed.resource_record_sets.unwrap().resource_record_set;

        assert_eq!(records.len(), 2);

        // Check A record
        assert_eq!(records[0].name, "www.example.com.");
        assert_eq!(records[0].record_type, "A");
        assert_eq!(records[0].ttl, Some(300));

        // Check TXT record
        assert_eq!(records[1].name, "example.com.");
        assert_eq!(records[1].record_type, "TXT");
        assert_eq!(records[1].ttl, Some(3600));
    }

    #[tokio::test]
    async fn test_convert_record() {
        let record_set = ResourceRecordSet {
            name: "www.example.com.".to_string(),
            record_type: "A".to_string(),
            ttl: Some(300),
            resource_records: Some(ResourceRecordsWrapper {
                resource_record: vec![ResourceRecord {
                    value: "192.0.2.1".to_string(),
                }],
            }),
            alias_target: None,
        };

        let records = Route53Provider::convert_record(&record_set, "example.com");

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].name, "www");
        assert_eq!(records[0].fqdn, "www.example.com");
        assert_eq!(records[0].ttl, 300);
        if let DnsRecordContent::A { address } = &records[0].content {
            assert_eq!(address, "192.0.2.1");
        } else {
            panic!("Expected A record");
        }
    }

    #[tokio::test]
    async fn test_convert_record_apex() {
        let record_set = ResourceRecordSet {
            name: "example.com.".to_string(),
            record_type: "A".to_string(),
            ttl: Some(300),
            resource_records: Some(ResourceRecordsWrapper {
                resource_record: vec![ResourceRecord {
                    value: "192.0.2.1".to_string(),
                }],
            }),
            alias_target: None,
        };

        let records = Route53Provider::convert_record(&record_set, "example.com");

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].name, "@");
        assert_eq!(records[0].fqdn, "example.com");
    }

    #[tokio::test]
    async fn test_convert_record_txt() {
        let record_set = ResourceRecordSet {
            name: "_acme-challenge.example.com.".to_string(),
            record_type: "TXT".to_string(),
            ttl: Some(60),
            resource_records: Some(ResourceRecordsWrapper {
                resource_record: vec![ResourceRecord {
                    value: "\"verification-token-here\"".to_string(),
                }],
            }),
            alias_target: None,
        };

        let records = Route53Provider::convert_record(&record_set, "example.com");

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].name, "_acme-challenge");
        if let DnsRecordContent::TXT { content } = &records[0].content {
            assert_eq!(content, "verification-token-here");
        } else {
            panic!("Expected TXT record");
        }
    }

    #[tokio::test]
    async fn test_convert_record_mx() {
        let record_set = ResourceRecordSet {
            name: "example.com.".to_string(),
            record_type: "MX".to_string(),
            ttl: Some(3600),
            resource_records: Some(ResourceRecordsWrapper {
                resource_record: vec![ResourceRecord {
                    value: "10 mail.example.com.".to_string(),
                }],
            }),
            alias_target: None,
        };

        let records = Route53Provider::convert_record(&record_set, "example.com");

        assert_eq!(records.len(), 1);
        if let DnsRecordContent::MX { priority, target } = &records[0].content {
            assert_eq!(*priority, 10);
            assert_eq!(target, "mail.example.com");
        } else {
            panic!("Expected MX record");
        }
    }
}
