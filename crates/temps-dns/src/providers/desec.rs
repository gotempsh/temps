// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! deSEC DNS provider implementation.
//!
//! deSEC's API manages record sets (RRsets), rather than individual DNS
//! records. Writes below preserve the other values in an RRset so concurrent
//! DNS-01 authorizations and unrelated records remain intact.

use async_trait::async_trait;
use reqwest::{Client, StatusCode};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use tracing::warn;
use url::Url;

use super::credentials::DeSecCredentials;
use super::traits::{
    decode_txt_presentation, dns_names_equal, encode_txt_presentation, truncate_error_body,
    DnsProvider, DnsProviderCapabilities, DnsProviderType, DnsRecord, DnsRecordContent,
    DnsRecordRequest, DnsRecordType, DnsZone,
};
use crate::errors::DnsError;

const DESEC_API_BASE: &str = "https://desec.io/api/v1";
const MAX_PAGES: usize = 100;
const MAX_PAGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_LISTING_BYTES: usize = 32 * 1024 * 1024;
const MAX_LISTING_ITEMS: usize = 25_000;
const DEFAULT_TTL: u32 = 3600;

#[derive(Debug, Deserialize)]
struct DeSecDomain {
    name: String,
    #[serde(default)]
    minimum_ttl: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
struct DeSecRrset {
    domain: String,
    subname: String,
    #[serde(rename = "type")]
    record_type: String,
    records: Vec<String>,
    ttl: u32,
}

#[derive(Debug, Serialize)]
struct DeSecRrsetWrite<'a> {
    subname: &'a str,
    #[serde(rename = "type")]
    record_type: &'a str,
    records: &'a [String],
    ttl: u32,
}

/// Provider for deSEC's DNS API.
pub struct DeSecProvider {
    client: Client,
    credentials: DeSecCredentials,
    base_url: Url,
}

impl std::fmt::Debug for DeSecProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeSecProvider")
            .field("credentials", &"[REDACTED]")
            .field("base_url", &self.base_url)
            .finish()
    }
}

impl DeSecProvider {
    pub fn new(credentials: DeSecCredentials) -> Result<Self, DnsError> {
        Self::with_base_url(credentials, DESEC_API_BASE)
    }

    fn with_base_url(credentials: DeSecCredentials, base_url: &str) -> Result<Self, DnsError> {
        let mut base_url = Url::parse(base_url).map_err(|error| {
            DnsError::Validation(format!("Invalid deSEC API base URL: {error}"))
        })?;
        if base_url.scheme() != "https"
            && !base_url
                .host_str()
                .is_some_and(|host| host == "127.0.0.1" || host == "localhost")
        {
            return Err(DnsError::Validation(
                "deSEC API base URL must use HTTPS except for localhost tests".to_string(),
            ));
        }
        base_url.set_query(None);
        base_url.set_fragment(None);

        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| {
                DnsError::ApiError(format!("Failed to create deSEC HTTP client: {error}"))
            })?;

        Ok(Self {
            client,
            credentials,
            base_url,
        })
    }

    fn endpoint(&self, segments: &[&str]) -> Result<Url, DnsError> {
        let mut url = self.base_url.clone();
        {
            let mut path = url.path_segments_mut().map_err(|_| {
                DnsError::ApiError("deSEC API base URL cannot accept path segments".to_string())
            })?;
            path.pop_if_empty();
            for segment in segments {
                path.push(segment);
            }
            path.push("");
        }
        Ok(url)
    }

    fn rrsets_endpoint(&self, domain: &str) -> Result<Url, DnsError> {
        self.endpoint(&["domains", domain.trim_end_matches('.'), "rrsets"])
    }

    fn rrset_endpoint(
        &self,
        domain: &str,
        subname: &str,
        record_type: &str,
    ) -> Result<Url, DnsError> {
        // deSEC reserves `@` for the apex in the item endpoint; the empty
        // subname cannot be represented there because URL normalization eats
        // the double slash.
        let subname = if subname.is_empty() { "@" } else { subname };
        self.endpoint(&[
            "domains",
            domain.trim_end_matches('.'),
            "rrsets",
            subname,
            record_type,
        ])
    }

    fn auth(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        request
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Token {}", self.credentials.api_token),
            )
            .header(reqwest::header::ACCEPT, "application/json")
    }

    async fn read_body_bounded(
        &self,
        mut response: reqwest::Response,
        context: &str,
    ) -> Result<Vec<u8>, DnsError> {
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|error| {
            DnsError::ApiError(format!("Failed to read deSEC {context} response: {error}"))
        })? {
            if body.len().saturating_add(chunk.len()) > MAX_PAGE_BYTES {
                return Err(DnsError::ApiError(format!(
                    "deSEC {context} response exceeded the {MAX_PAGE_BYTES}-byte safety limit"
                )));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }

    async fn request_json<T: DeserializeOwned>(
        &self,
        method: reqwest::Method,
        url: Url,
        body: Option<&impl Serialize>,
        context: &str,
    ) -> Result<T, DnsError> {
        let request = self.auth(self.client.request(method.clone(), url.clone()));
        let request = match body {
            Some(body) => request.json(body),
            None => request,
        };
        let response = request.send().await.map_err(|error| {
            DnsError::ApiError(format!("deSEC {context} request failed: {error}"))
        })?;
        let status = response.status();
        let body = self.read_body_bounded(response, context).await?;
        let text = String::from_utf8_lossy(&body);
        if !status.is_success() {
            if status == StatusCode::NOT_FOUND {
                return Err(DnsError::RecordNotFound(context.to_string()));
            }
            return Err(DnsError::ApiError(format!(
                "deSEC {context} returned HTTP {status}: {}",
                truncate_error_body(&text)
            )));
        }
        serde_json::from_slice(&body).map_err(|error| {
            DnsError::ApiError(format!(
                "Failed to parse deSEC {context} response: {error}; body: {}",
                truncate_error_body(&text)
            ))
        })
    }

    async fn request_empty(
        &self,
        method: reqwest::Method,
        url: Url,
        body: Option<&impl Serialize>,
        context: &str,
    ) -> Result<(), DnsError> {
        let request = self.auth(self.client.request(method.clone(), url));
        let request = match body {
            Some(body) => request.json(body),
            None => request,
        };
        let response = request.send().await.map_err(|error| {
            DnsError::ApiError(format!("deSEC {context} request failed: {error}"))
        })?;
        let status = response.status();
        if status.is_success() {
            return Ok(());
        }
        let text = self
            .read_body_bounded(response, context)
            .await
            .map(|body| String::from_utf8_lossy(&body).into_owned())
            .unwrap_or_else(|_| "response body exceeded the safety limit".to_string());
        Err(DnsError::ApiError(format!(
            "deSEC {context} returned HTTP {status}: {}",
            truncate_error_body(&text)
        )))
    }

    async fn list_all<T: DeserializeOwned>(
        &self,
        mut url: Url,
        context: &str,
    ) -> Result<Vec<T>, DnsError> {
        let base_path = self.base_url.path().trim_end_matches('/');
        // deSEC requires `cursor=` to explicitly enter pagination mode when
        // an account or zone has more than the first response page.
        url.query_pairs_mut().append_pair("cursor", "");
        let mut seen_urls = HashSet::new();
        let mut values = Vec::new();
        let mut total_bytes = 0usize;
        for page in 1..=MAX_PAGES {
            if !seen_urls.insert(url.as_str().to_string()) {
                return Err(DnsError::ApiError(format!(
                    "deSEC {context} pagination repeated a URL on page {page}"
                )));
            }
            let response = self
                .auth(self.client.get(url.clone()))
                .send()
                .await
                .map_err(|error| {
                    DnsError::ApiError(format!(
                        "deSEC {context} page {page} request failed: {error}"
                    ))
                })?;
            let status = response.status();
            let next = response
                .headers()
                .get(reqwest::header::LINK)
                .and_then(|value| value.to_str().ok())
                .and_then(next_link)
                .map(|href| Url::parse(href).or_else(|_| url.join(href)))
                .transpose()
                .map_err(|error| {
                    DnsError::ApiError(format!("Invalid deSEC {context} pagination link: {error}"))
                })?;
            let body = self.read_body_bounded(response, context).await?;
            total_bytes = total_bytes.saturating_add(body.len());
            if total_bytes > MAX_LISTING_BYTES {
                return Err(DnsError::ApiError(format!(
                    "deSEC {context} exceeded the {MAX_LISTING_BYTES}-byte listing limit; refusing a partial result"
                )));
            }
            if !status.is_success() {
                let body = String::from_utf8_lossy(&body);
                return Err(DnsError::ApiError(format!(
                    "deSEC {context} page {page} returned HTTP {status}: {}",
                    truncate_error_body(&body)
                )));
            }
            let items = serde_json::from_slice::<Vec<T>>(&body).map_err(|error| {
                DnsError::ApiError(format!(
                    "Failed to parse deSEC {context} page {page}: {error}"
                ))
            })?;
            values.extend(items);
            if values.len() > MAX_LISTING_ITEMS {
                return Err(DnsError::ApiError(format!(
                    "deSEC {context} exceeded the {MAX_LISTING_ITEMS}-item listing limit; refusing a partial result"
                )));
            }

            match next {
                Some(next) => {
                    if next.origin() != url.origin() || !next.path().starts_with(base_path) {
                        return Err(DnsError::ApiError(format!(
                            "deSEC {context} page {page} returned a pagination link outside its API origin"
                        )));
                    }
                    url = next;
                }
                None => return Ok(values),
            }
        }
        Err(DnsError::ApiError(format!(
            "deSEC {context} exceeded {MAX_PAGES} pages; refusing a partial result"
        )))
    }

    async fn get_rrset(
        &self,
        domain: &str,
        subname: &str,
        record_type: &str,
    ) -> Result<Option<DeSecRrset>, DnsError> {
        let url = self.rrset_endpoint(domain, subname, record_type)?;
        let response = self
            .auth(self.client.get(url.clone()))
            .send()
            .await
            .map_err(|error| {
                DnsError::ApiError(format!(
                    "deSEC lookup of {record_type} RRset '{subname}' in zone {domain} failed: {error}"
                ))
            })?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let status = response.status();
        let body = self
            .read_body_bounded(
                response,
                &format!("{record_type} RRset '{subname}' in zone {domain}"),
            )
            .await?;
        if !status.is_success() {
            let body_text = String::from_utf8_lossy(&body);
            return Err(DnsError::ApiError(format!(
                "deSEC lookup of {record_type} RRset '{subname}' in zone {domain} returned HTTP {status}: {}",
                truncate_error_body(&body_text)
            )));
        }
        serde_json::from_slice::<DeSecRrset>(&body)
            .map(Some)
            .map_err(|error| {
                DnsError::ApiError(format!(
                "Failed to parse deSEC {record_type} RRset '{subname}' in zone {domain}: {error}"
            ))
            })
    }

    async fn write_rrset(
        &self,
        domain: &str,
        subname: &str,
        record_type: &str,
        records: &[String],
        ttl: u32,
        exists: bool,
    ) -> Result<(), DnsError> {
        let request = DeSecRrsetWrite {
            subname,
            record_type,
            records,
            ttl,
        };
        let url = if exists {
            self.rrset_endpoint(domain, subname, record_type)?
        } else {
            self.rrsets_endpoint(domain)?
        };
        let method = if exists {
            reqwest::Method::PATCH
        } else {
            reqwest::Method::POST
        };
        let operation = if exists { "update" } else { "create" };
        self.request_empty(
            method,
            url,
            Some(&request),
            &format!("{operation} {record_type} RRset '{subname}' in zone {domain}"),
        )
        .await
    }

    async fn resolve_zone(&self, domain: &str) -> Result<DeSecDomain, DnsError> {
        let mut url = self.endpoint(&["domains"])?;
        url.query_pairs_mut()
            .append_pair("owns_qname", domain.trim_end_matches('.'));
        let zones: Vec<DeSecDomain> = self
            .request_json(
                reqwest::Method::GET,
                url,
                None::<&()>,
                &format!("zone lookup for {domain}"),
            )
            .await?;
        match zones.len() {
            0 => Err(DnsError::ZoneNotFound(domain.to_string())),
            1 => zones
                .into_iter()
                .next()
                .ok_or_else(|| DnsError::ZoneNotFound(domain.to_string())),
            count => Err(DnsError::ApiError(format!(
                "deSEC returned {count} responsible zones for DNS name {domain}"
            ))),
        }
    }

    fn to_subname(name: &str, zone: &str) -> Result<String, DnsError> {
        let name = name.trim().trim_end_matches('.');
        if name.is_empty() || name == "@" {
            return Ok(String::new());
        }
        let zone = zone.trim_end_matches('.');
        if dns_names_equal(name, zone) {
            return Ok(String::new());
        }
        let suffix = format!(".{zone}");
        if name.len() > suffix.len()
            && name[name.len() - suffix.len()..].eq_ignore_ascii_case(&suffix)
        {
            return Ok(name[..name.len() - suffix.len()].to_string());
        }
        if name.contains('.') {
            return Err(DnsError::Validation(format!(
                "DNS record name {name} is not within deSEC zone {zone}"
            )));
        }
        Ok(name.to_string())
    }

    fn display_name(subname: &str) -> String {
        if subname.is_empty() {
            "@".to_string()
        } else {
            subname.to_string()
        }
    }

    fn effective_ttl(
        minimum_ttl: Option<u32>,
        requested_ttl: Option<u32>,
        existing_ttl: Option<u32>,
        fallback_ttl: u32,
    ) -> u32 {
        let minimum_ttl = minimum_ttl.unwrap_or(fallback_ttl);
        requested_ttl
            .map(|requested| requested.max(minimum_ttl))
            .or(existing_ttl)
            .unwrap_or(minimum_ttl)
    }

    fn fqdn(subname: &str, zone: &str) -> String {
        if subname.is_empty() {
            zone.trim_end_matches('.').to_string()
        } else {
            format!("{}.{}", subname, zone.trim_end_matches('.'))
        }
    }

    fn record_id(subname: &str, record_type: &str, value: &str) -> String {
        format!(
            "{}|{}|{}",
            subname,
            record_type,
            hex::encode(value.as_bytes())
        )
    }

    fn parse_record_id(record_id: &str) -> Result<(String, String, String), DnsError> {
        let (name, rest) = record_id
            .split_once('|')
            .ok_or_else(|| DnsError::Validation(format!("Invalid deSEC record ID {record_id}")))?;
        let (record_type, encoded) = rest
            .split_once('|')
            .ok_or_else(|| DnsError::Validation(format!("Invalid deSEC record ID {record_id}")))?;
        let bytes = hex::decode(encoded).map_err(|error| {
            DnsError::Validation(format!("Invalid deSEC record ID {record_id}: {error}"))
        })?;
        let value = String::from_utf8(bytes).map_err(|error| {
            DnsError::Validation(format!("Invalid deSEC record ID {record_id}: {error}"))
        })?;
        Ok((name.to_string(), record_type.to_string(), value))
    }

    fn parse_content(record_type: DnsRecordType, raw: &str) -> Option<DnsRecordContent> {
        let fields: Vec<&str> = raw.split_whitespace().collect();
        match record_type {
            DnsRecordType::A => Some(DnsRecordContent::A {
                address: raw.to_string(),
            }),
            DnsRecordType::AAAA => Some(DnsRecordContent::AAAA {
                address: raw.to_string(),
            }),
            DnsRecordType::CNAME => Some(DnsRecordContent::CNAME {
                target: raw.trim_end_matches('.').to_string(),
            }),
            DnsRecordType::TXT => Some(DnsRecordContent::TXT {
                content: decode_txt_presentation(raw),
            }),
            DnsRecordType::MX => Some(DnsRecordContent::MX {
                priority: fields.first()?.parse().ok()?,
                target: fields.get(1)?.trim_end_matches('.').to_string(),
            }),
            DnsRecordType::NS => Some(DnsRecordContent::NS {
                nameserver: raw.trim_end_matches('.').to_string(),
            }),
            DnsRecordType::SRV => Some(DnsRecordContent::SRV {
                priority: fields.first()?.parse().ok()?,
                weight: fields.get(1)?.parse().ok()?,
                port: fields.get(2)?.parse().ok()?,
                target: fields.get(3)?.trim_end_matches('.').to_string(),
            }),
            DnsRecordType::CAA => {
                let (flags, rest) = raw.split_once(char::is_whitespace)?;
                let (tag, value) = rest.trim().split_once(char::is_whitespace)?;
                let value = value.trim();
                Some(DnsRecordContent::CAA {
                    flags: flags.parse().ok()?,
                    tag: tag.to_string(),
                    value: value
                        .strip_prefix('"')?
                        .strip_suffix('"')?
                        .replace("\\\"", "\"")
                        .replace("\\\\", "\\"),
                })
            }
            DnsRecordType::PTR => Some(DnsRecordContent::PTR {
                target: raw.trim_end_matches('.').to_string(),
            }),
        }
    }

    fn format_content(content: &DnsRecordContent) -> String {
        match content {
            DnsRecordContent::A { address } | DnsRecordContent::AAAA { address } => address.clone(),
            DnsRecordContent::CNAME { target }
            | DnsRecordContent::NS { nameserver: target }
            | DnsRecordContent::PTR { target } => {
                format!("{}.", target.trim_end_matches('.'))
            }
            DnsRecordContent::TXT { content } => encode_txt_presentation(content),
            DnsRecordContent::MX { priority, target } => {
                format!("{priority} {}.", target.trim_end_matches('.'))
            }
            DnsRecordContent::SRV {
                priority,
                weight,
                port,
                target,
            } => {
                format!(
                    "{priority} {weight} {port} {}.",
                    target.trim_end_matches('.')
                )
            }
            DnsRecordContent::CAA { flags, tag, value } => {
                format!(
                    "{flags} {tag} \"{}\"",
                    value.replace('\\', "\\\\").replace('"', "\\\"")
                )
            }
        }
    }

    fn convert_rrset(rrset: &DeSecRrset) -> Vec<DnsRecord> {
        let record_type = match rrset.record_type.to_ascii_uppercase().as_str() {
            "A" => DnsRecordType::A,
            "AAAA" => DnsRecordType::AAAA,
            "CNAME" => DnsRecordType::CNAME,
            "TXT" => DnsRecordType::TXT,
            "MX" => DnsRecordType::MX,
            "NS" => DnsRecordType::NS,
            "SRV" => DnsRecordType::SRV,
            "CAA" => DnsRecordType::CAA,
            "PTR" => DnsRecordType::PTR,
            _ => return Vec::new(),
        };
        let name = Self::display_name(&rrset.subname);
        let fqdn = Self::fqdn(&rrset.subname, &rrset.domain);
        rrset
            .records
            .iter()
            .filter_map(|raw| {
                let content = Self::parse_content(record_type, raw)?;
                Some(DnsRecord {
                    id: Some(Self::record_id(&rrset.subname, &rrset.record_type, raw)),
                    zone: rrset.domain.clone(),
                    name: name.clone(),
                    fqdn: fqdn.clone(),
                    content,
                    ttl: rrset.ttl,
                    proxied: false,
                    metadata: HashMap::new(),
                })
            })
            .collect()
    }

    async fn remove_value(
        &self,
        domain: &str,
        subname: &str,
        record_type: &str,
        value: &str,
    ) -> Result<(), DnsError> {
        let Some(mut rrset) = self.get_rrset(domain, subname, record_type).await? else {
            return Ok(());
        };
        let original_len = rrset.records.len();
        rrset.records.retain(|record| record != value);
        if rrset.records.len() == original_len {
            return Err(DnsError::RecordNotFound(format!(
                "{record_type} value at {}.{}",
                subname, domain
            )));
        }
        if rrset.records.is_empty() {
            self.request_empty(
                reqwest::Method::DELETE,
                self.rrset_endpoint(domain, subname, record_type)?,
                None::<&()>,
                &format!("delete {record_type} RRset '{subname}' in zone {domain}"),
            )
            .await
        } else {
            self.write_rrset(
                domain,
                subname,
                record_type,
                &rrset.records,
                rrset.ttl,
                true,
            )
            .await
        }
    }
}

fn next_link(link_header: &str) -> Option<&str> {
    link_header.split(',').find_map(|link| {
        let (target, params) = link.split_once(';')?;
        if !params.contains("rel=\"next\"") && !params.contains("rel=next") {
            return None;
        }
        target.trim().strip_prefix('<')?.strip_suffix('>')
    })
}

#[async_trait]
impl DnsProvider for DeSecProvider {
    fn provider_type(&self) -> DnsProviderType {
        DnsProviderType::DeSec
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
        self.list_zones().await?;
        Ok(true)
    }

    async fn list_zones(&self) -> Result<Vec<DnsZone>, DnsError> {
        let url = self.endpoint(&["domains"])?;
        let domains: Vec<DeSecDomain> = self.list_all(url, "domain listing").await?;
        Ok(domains
            .into_iter()
            .map(|domain| DnsZone {
                id: domain.name.clone(),
                name: domain.name,
                status: "active".to_string(),
                nameservers: Vec::new(),
                metadata: HashMap::new(),
            })
            .collect())
    }

    async fn get_zone(&self, domain: &str) -> Result<Option<DnsZone>, DnsError> {
        match self.resolve_zone(domain).await {
            Ok(zone) => Ok(Some(DnsZone {
                id: zone.name.clone(),
                name: zone.name,
                status: "active".to_string(),
                nameservers: Vec::new(),
                metadata: zone
                    .minimum_ttl
                    .map(|ttl| HashMap::from([("minimum_ttl".to_string(), ttl.to_string())]))
                    .unwrap_or_default(),
            })),
            Err(DnsError::ZoneNotFound(_)) => Ok(None),
            Err(error) => Err(error),
        }
    }

    async fn check_zone_access(&self, domain: &str) -> Result<(), DnsError> {
        self.resolve_zone(domain).await.map(|_| ())
    }

    async fn list_records(&self, domain: &str) -> Result<Vec<DnsRecord>, DnsError> {
        let zone = self.resolve_zone(domain).await?;
        let url = self.rrsets_endpoint(&zone.name)?;
        let rrsets: Vec<DeSecRrset> = self
            .list_all(url, &format!("RRset listing for zone {}", zone.name))
            .await?;
        Ok(rrsets.iter().flat_map(Self::convert_rrset).collect())
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

    async fn get_records(
        &self,
        domain: &str,
        name: &str,
        record_type: DnsRecordType,
    ) -> Result<Vec<DnsRecord>, DnsError> {
        let zone = self.resolve_zone(domain).await?;
        let subname = Self::to_subname(name, &zone.name)?;
        let type_name = record_type.to_string().to_ascii_uppercase();
        Ok(self
            .get_rrset(&zone.name, &subname, &type_name)
            .await?
            .map(|rrset| Self::convert_rrset(&rrset))
            .unwrap_or_default())
    }

    async fn create_record(
        &self,
        domain: &str,
        request: DnsRecordRequest,
    ) -> Result<DnsRecord, DnsError> {
        let zone = self.resolve_zone(domain).await?;
        let subname = Self::to_subname(&request.name, &zone.name)?;
        let type_name = request
            .content
            .record_type()
            .to_string()
            .to_ascii_uppercase();
        let value = Self::format_content(&request.content);
        let existing = self.get_rrset(&zone.name, &subname, &type_name).await?;
        let mut records = existing
            .as_ref()
            .map(|rrset| rrset.records.clone())
            .unwrap_or_default();
        if !records.contains(&value) {
            records.push(value.clone());
        }
        let minimum_ttl = zone.minimum_ttl.unwrap_or(DEFAULT_TTL);
        if let Some(requested_ttl) = request.ttl.filter(|ttl| *ttl < minimum_ttl) {
            warn!(
                zone = %zone.name,
                requested_ttl,
                minimum_ttl,
                "deSEC zone minimum TTL exceeds the requested DNS record TTL; using the zone minimum"
            );
        }
        let ttl = Self::effective_ttl(
            Some(minimum_ttl),
            request.ttl,
            existing.as_ref().map(|rrset| rrset.ttl),
            DEFAULT_TTL,
        );
        if existing.as_ref().is_none_or(|rrset| {
            rrset.records != records || request.ttl.is_some_and(|requested| requested != rrset.ttl)
        }) {
            self.write_rrset(
                &zone.name,
                &subname,
                &type_name,
                &records,
                ttl,
                existing.is_some(),
            )
            .await?;
        }
        Ok(DnsRecord {
            id: Some(Self::record_id(&subname, &type_name, &value)),
            zone: zone.name.clone(),
            name: Self::display_name(&subname),
            fqdn: Self::fqdn(&subname, &zone.name),
            content: request.content,
            ttl,
            proxied: false,
            metadata: HashMap::new(),
        })
    }

    async fn update_record(
        &self,
        domain: &str,
        record_id: &str,
        request: DnsRecordRequest,
    ) -> Result<DnsRecord, DnsError> {
        let zone = self.resolve_zone(domain).await?;
        let (old_subname, old_type, old_value) = Self::parse_record_id(record_id)?;
        let new_subname = Self::to_subname(&request.name, &zone.name)?;
        let new_type = request
            .content
            .record_type()
            .to_string()
            .to_ascii_uppercase();
        let new_value = Self::format_content(&request.content);
        if old_subname != new_subname || old_type != new_type {
            self.remove_value(&zone.name, &old_subname, &old_type, &old_value)
                .await?;
            return self.create_record(domain, request).await;
        }
        let rrset = self
            .get_rrset(&zone.name, &old_subname, &old_type)
            .await?
            .ok_or_else(|| {
                DnsError::RecordNotFound(format!(
                    "{old_type} RRset '{old_subname}' in zone {}",
                    zone.name
                ))
            })?;
        let mut records = rrset.records.clone();
        let old_index = records
            .iter()
            .position(|value| value == &old_value)
            .ok_or_else(|| {
                DnsError::RecordNotFound(format!(
                    "{old_type} record '{old_value}' at {old_subname}.{}",
                    zone.name
                ))
            })?;
        if !records.contains(&new_value) || new_value == old_value {
            records[old_index] = new_value.clone();
        } else {
            records.remove(old_index);
        }
        if let Some(requested_ttl) = request
            .ttl
            .filter(|ttl| zone.minimum_ttl.is_some_and(|minimum| *ttl < minimum))
        {
            warn!(
                zone = %zone.name,
                requested_ttl,
                minimum_ttl = zone.minimum_ttl.unwrap_or_default(),
                "deSEC zone minimum TTL exceeds the requested DNS record TTL; using the zone minimum"
            );
        }
        let ttl = Self::effective_ttl(zone.minimum_ttl, request.ttl, Some(rrset.ttl), 0);
        self.write_rrset(&zone.name, &old_subname, &old_type, &records, ttl, true)
            .await?;
        Ok(DnsRecord {
            id: Some(Self::record_id(&old_subname, &old_type, &new_value)),
            zone: zone.name.clone(),
            name: Self::display_name(&old_subname),
            fqdn: Self::fqdn(&old_subname, &zone.name),
            content: request.content,
            ttl,
            proxied: false,
            metadata: HashMap::new(),
        })
    }

    async fn delete_record(&self, domain: &str, record_id: &str) -> Result<(), DnsError> {
        let zone = self.resolve_zone(domain).await?;
        let (subname, record_type, value) = Self::parse_record_id(record_id)?;
        self.remove_value(&zone.name, &subname, &record_type, &value)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::{
        matchers::{body_json, header, method, path, query_param},
        Mock, MockServer, ResponseTemplate,
    };

    fn credentials() -> DeSecCredentials {
        DeSecCredentials {
            api_token: "test-token".to_string(),
        }
    }

    fn domain_json(name: &str) -> serde_json::Value {
        json!({"name": name, "minimum_ttl": 60})
    }

    fn rrset_json(subname: &str, records: &[&str]) -> serde_json::Value {
        json!({"domain":"example.com", "subname":subname, "name":format!("{subname}.example.com."), "type":"TXT", "records":records, "ttl":300})
    }

    fn provider(server: &MockServer) -> DeSecProvider {
        DeSecProvider::with_base_url(credentials(), &server.uri()).unwrap()
    }

    #[test]
    fn parses_zone_names_and_record_ids_safely() {
        assert_eq!(
            DeSecProvider::to_subname("_acme-challenge.www.example.com.", "example.com").unwrap(),
            "_acme-challenge.www"
        );
        assert_eq!(DeSecProvider::to_subname("@", "example.com").unwrap(), "");
        assert!(DeSecProvider::to_subname("outside.test", "example.com").is_err());
        let id = DeSecProvider::record_id("_acme-challenge", "TXT", "token|with|separators");
        assert_eq!(
            DeSecProvider::parse_record_id(&id).unwrap(),
            (
                "_acme-challenge".into(),
                "TXT".into(),
                "token|with|separators".into()
            )
        );
    }

    #[test]
    fn honors_de_sec_minimum_ttl_without_losing_existing_ttl() {
        assert_eq!(
            DeSecProvider::effective_ttl(Some(3600), Some(300), None, DEFAULT_TTL),
            3600
        );
        assert_eq!(
            DeSecProvider::effective_ttl(Some(60), Some(300), None, DEFAULT_TTL),
            300
        );
        assert_eq!(
            DeSecProvider::effective_ttl(Some(60), None, Some(120), DEFAULT_TTL),
            120
        );
        assert_eq!(
            DeSecProvider::effective_ttl(None, None, None, DEFAULT_TTL),
            DEFAULT_TTL
        );
    }

    #[test]
    fn encodes_and_decodes_desec_txt_presentation() {
        let raw = "challenge value \"quoted\"";
        let encoded = DeSecProvider::format_content(&DnsRecordContent::TXT {
            content: raw.to_string(),
        });
        assert_eq!(encoded, "\"challenge value \\\"quoted\\\"\"");
        assert_eq!(
            DeSecProvider::parse_content(DnsRecordType::TXT, &encoded),
            Some(DnsRecordContent::TXT {
                content: raw.to_string()
            })
        );
    }

    #[tokio::test]
    async fn zone_lookup_uses_desec_responsible_zone_query_and_auth_token() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/domains/"))
            .and(query_param("owns_qname", "_acme-challenge.app.example.com"))
            .and(header("authorization", "Token test-token"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(vec![domain_json("example.com")]),
            )
            .expect(1)
            .mount(&server)
            .await;
        let result = provider(&server)
            .get_zone("_acme-challenge.app.example.com")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.name, "example.com");
        assert_eq!(result.metadata["minimum_ttl"], "60");
    }

    #[tokio::test]
    async fn creates_acme_value_without_overwriting_existing_rrset() {
        let server = MockServer::start().await;
        let rrset_path = "/domains/example.com/rrsets/_acme-challenge/TXT/";
        Mock::given(method("GET"))
            .and(path("/domains/"))
            .and(query_param("owns_qname", "example.com"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(vec![domain_json("example.com")]),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(rrset_path))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(rrset_json("_acme-challenge", &["\"existing-challenge\""])),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PATCH")).and(path(rrset_path))
            .and(body_json(json!({"subname":"_acme-challenge","type":"TXT","records":["\"existing-challenge\"", "\"new-challenge\""],"ttl":300})))
            .respond_with(ResponseTemplate::new(200)).expect(1).mount(&server).await;
        let provider = provider(&server);
        let record = provider
            .create_record(
                "example.com",
                DnsRecordRequest {
                    name: "_acme-challenge".into(),
                    content: DnsRecordContent::TXT {
                        content: "new-challenge".into(),
                    },
                    ttl: None,
                    proxied: false,
                },
            )
            .await
            .unwrap();
        assert_eq!(
            record.content,
            DnsRecordContent::TXT {
                content: "new-challenge".into()
            }
        );
    }

    #[tokio::test]
    async fn deleting_acme_value_preserves_other_values_in_the_rrset() {
        let server = MockServer::start().await;
        let rrset_path = "/domains/example.com/rrsets/_acme-challenge/TXT/";
        Mock::given(method("GET"))
            .and(path("/domains/"))
            .and(query_param("owns_qname", "example.com"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(vec![domain_json("example.com")]),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(rrset_path))
            .respond_with(ResponseTemplate::new(200).set_body_json(rrset_json(
                "_acme-challenge",
                &["\"existing-challenge\"", "\"new-challenge\""],
            )))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path(rrset_path))
            .and(body_json(json!({"subname":"_acme-challenge","type":"TXT","records":["\"existing-challenge\""],"ttl":300})))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let provider = provider(&server);
        let id = DeSecProvider::record_id("_acme-challenge", "TXT", "\"new-challenge\"");
        provider.delete_record("example.com", &id).await.unwrap();
    }

    #[tokio::test]
    async fn pagination_rejects_links_to_another_origin() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/domains/"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header(
                        "link",
                        "<https://elsewhere.invalid/domains/?cursor=next>; rel=\"next\"",
                    )
                    .set_body_json(vec![domain_json("example.com")]),
            )
            .mount(&server)
            .await;
        assert!(
            matches!(provider(&server).list_zones().await, Err(DnsError::ApiError(message)) if message.contains("outside its API origin"))
        );
    }
}
