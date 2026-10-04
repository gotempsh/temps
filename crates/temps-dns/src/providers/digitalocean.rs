// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! DigitalOcean DNS provider implementation
//!
//! This provider uses the DigitalOcean API to manage DNS records.
//! It requires a Personal Access Token with read/write scope.
//!
//! Create token at: https://cloud.digitalocean.com/account/api/tokens

use async_trait::async_trait;
use reqwest::{Client, StatusCode};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use tracing::{debug, info, warn};

use super::credentials::DigitalOceanCredentials;
use super::traits::{
    dns_names_equal, truncate_error_body, DnsProvider, DnsProviderCapabilities, DnsProviderType,
    DnsRecord, DnsRecordContent, DnsRecordRequest, DnsRecordType, DnsZone,
};
use crate::errors::DnsError;

const DO_API_BASE: &str = "https://api.digitalocean.com/v2";
/// Items per listing page; DigitalOcean's maximum (its default is only 20).
const PAGE_SIZE: usize = 200;
/// Hard cap on pages read by any listing; reaching it is an error, never a
/// silently truncated result.
const MAX_PAGES: usize = 1000;

/// DigitalOcean DNS provider
pub struct DigitalOceanProvider {
    client: Client,
    credentials: DigitalOceanCredentials,
    base_url: String,
    /// Page cap for listings ([`MAX_PAGES`]; lowered in tests).
    max_pages: usize,
}

/// DigitalOcean API response structures
#[derive(Debug, Deserialize)]
struct DomainsResponse {
    domains: Vec<DoDomain>,
    #[serde(default)]
    links: Option<DoLinks>,
    #[serde(default)]
    meta: Option<DoMeta>,
}

/// `GET /domains/{name}` response.
#[derive(Debug, Deserialize)]
struct DomainResponse {
    domain: DoDomain,
}

#[derive(Debug, Deserialize)]
struct DoDomain {
    name: String,
    #[allow(dead_code)]
    ttl: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct DomainRecordsResponse {
    domain_records: Vec<DoDomainRecord>,
    #[serde(default)]
    links: Option<DoLinks>,
    #[serde(default)]
    meta: Option<DoMeta>,
}

/// `links` of a paginated DigitalOcean response; `pages.next` is present
/// while more pages follow.
#[derive(Debug, Deserialize)]
struct DoLinks {
    #[serde(default)]
    pages: Option<DoPageLinks>,
}

#[derive(Debug, Deserialize)]
struct DoPageLinks {
    #[serde(default)]
    next: Option<String>,
}

/// `meta` of a paginated DigitalOcean response.
#[derive(Debug, Deserialize)]
struct DoMeta {
    #[serde(default)]
    total: Option<u64>,
}

/// One page of a paginated DigitalOcean list response.
trait DoListPage: DeserializeOwned {
    type Item;
    /// Identity of an item, used to drop repeats across pages.
    type Key: Eq + Hash;
    /// What the items are, for error messages.
    const NOUN: &'static str;
    fn key(item: &Self::Item) -> Self::Key;
    fn into_parts(self) -> (Vec<Self::Item>, Option<DoLinks>, Option<DoMeta>);
}

impl DoListPage for DomainsResponse {
    type Item = DoDomain;
    type Key = String;
    const NOUN: &'static str = "domains";
    fn key(domain: &DoDomain) -> String {
        domain.name.to_ascii_lowercase()
    }
    fn into_parts(self) -> (Vec<DoDomain>, Option<DoLinks>, Option<DoMeta>) {
        (self.domains, self.links, self.meta)
    }
}

impl DoListPage for DomainRecordsResponse {
    type Item = DoDomainRecord;
    type Key = i64;
    const NOUN: &'static str = "records";
    fn key(record: &DoDomainRecord) -> i64 {
        record.id
    }
    fn into_parts(self) -> (Vec<DoDomainRecord>, Option<DoLinks>, Option<DoMeta>) {
        (self.domain_records, self.links, self.meta)
    }
}

#[derive(Debug, Deserialize)]
struct DomainRecordResponse {
    domain_record: DoDomainRecord,
}

#[derive(Debug, Clone, Deserialize)]
struct DoDomainRecord {
    id: i64,
    #[serde(rename = "type")]
    record_type: String,
    name: String,
    data: String,
    #[serde(default)]
    priority: Option<u16>,
    #[serde(default)]
    port: Option<u16>,
    #[serde(default)]
    weight: Option<u16>,
    ttl: u32,
    #[serde(default)]
    flags: Option<u8>,
    #[serde(default)]
    tag: Option<String>,
}

/// Request to create/update a domain record
#[derive(Debug, Serialize)]
struct CreateRecordRequest {
    #[serde(rename = "type")]
    record_type: String,
    name: String,
    data: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    priority: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    weight: Option<u16>,
    ttl: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    flags: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tag: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DoErrorResponse {
    id: String,
    message: String,
}

impl DigitalOceanProvider {
    /// Create a new DigitalOcean provider with the given credentials
    pub fn new(credentials: DigitalOceanCredentials) -> Result<Self, DnsError> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| DnsError::ApiError(format!("Failed to create HTTP client: {}", e)))?;

        Ok(Self {
            client,
            credentials,
            base_url: DO_API_BASE.to_string(),
            max_pages: MAX_PAGES,
        })
    }

    /// Create a provider with a custom base URL (for testing)
    #[cfg(test)]
    pub fn with_base_url(
        credentials: DigitalOceanCredentials,
        base_url: String,
    ) -> Result<Self, DnsError> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| DnsError::ApiError(format!("Failed to create HTTP client: {}", e)))?;

        Ok(Self {
            client,
            credentials,
            base_url,
            max_pages: MAX_PAGES,
        })
    }

    /// `path` without its query string: page/filter parameters stay out of
    /// log lines and error messages.
    fn without_query(path: &str) -> &str {
        path.split('?').next().unwrap_or(path)
    }

    /// Send one authenticated request and return its status and body,
    /// whatever the status.
    async fn send(
        &self,
        method: &str,
        path_and_query: &str,
        body: Option<&impl Serialize>,
    ) -> Result<(StatusCode, String), DnsError> {
        let url = format!("{}{}", self.base_url, path_and_query);
        let path = Self::without_query(path_and_query);

        debug!("DigitalOcean API request: {} {}", method, path);

        let mut request = match method {
            "GET" => self.client.get(&url),
            "POST" => self.client.post(&url),
            "PUT" => self.client.put(&url),
            "DELETE" => self.client.delete(&url),
            _ => {
                return Err(DnsError::ApiError(format!(
                    "Unsupported method {method} for DigitalOcean API {path}"
                )))
            }
        };

        request = request
            .header(
                "Authorization",
                format!("Bearer {}", self.credentials.api_token),
            )
            .header("Content-Type", "application/json");

        if let Some(body) = body {
            request = request.json(body);
        }

        let response = request.send().await.map_err(|e| {
            DnsError::ApiError(format!("DigitalOcean API {method} {path} failed: {e}"))
        })?;

        let status = response.status();
        let response_text = response.text().await.map_err(|e| {
            DnsError::ApiError(format!(
                "Failed to read DigitalOcean API response for {method} {path} (HTTP {status}): {e}"
            ))
        })?;
        Ok((status, response_text))
    }

    /// Error for a non-success response: status, operation, bounded body.
    fn status_error(method: &str, path: &str, status: StatusCode, body: &str) -> DnsError {
        if let Ok(error) = serde_json::from_str::<DoErrorResponse>(body) {
            return DnsError::ApiError(format!(
                "DigitalOcean API error for {} {} (HTTP {}, {}): {}",
                method,
                path,
                status,
                truncate_error_body(&error.id),
                truncate_error_body(&error.message)
            ));
        }
        DnsError::ApiError(format!(
            "DigitalOcean API returned status {} for {} {}: {}",
            status,
            method,
            path,
            truncate_error_body(body)
        ))
    }

    /// Make an authenticated request to DigitalOcean API
    async fn api_request<T: DeserializeOwned>(
        &self,
        method: &str,
        path_and_query: &str,
        body: Option<&impl Serialize>,
    ) -> Result<T, DnsError> {
        let (status, response_text) = self.send(method, path_and_query, body).await?;
        let path = Self::without_query(path_and_query);
        if !status.is_success() {
            return Err(Self::status_error(method, path, status, &response_text));
        }

        if response_text.is_empty() {
            // For DELETE requests that return no content
            return serde_json::from_str("{}").map_err(|e| {
                DnsError::ApiError(format!(
                    "Failed to parse the empty DigitalOcean API response for {method} {path}: {e}"
                ))
            });
        }

        serde_json::from_str(&response_text).map_err(|e| {
            DnsError::ApiError(format!(
                "Failed to parse DigitalOcean API response for {} {}: {} - Body: {}",
                method,
                path,
                e,
                truncate_error_body(&response_text)
            ))
        })
    }

    /// Every item of the paginated list at `path` — optionally narrowed by
    /// the already-encoded server-side `filter_query` (`&key=value...`) —
    /// reading [`PAGE_SIZE`] items per page until `links.pages.next` is
    /// gone.
    ///
    /// Never returns a partial list: reaching the page cap, an empty page
    /// that still has a next link, a page holding only items already seen,
    /// or (for an unfiltered listing) fewer items than `meta.total` is an
    /// error. Items repeated across pages by a concurrent change are
    /// returned once.
    async fn list_all<P: DoListPage>(
        &self,
        path: &str,
        filter_query: &str,
        context: &str,
    ) -> Result<Vec<P::Item>, DnsError> {
        let mut items = Vec::new();
        let mut seen: HashSet<P::Key> = HashSet::new();

        for page in 1..=self.max_pages {
            let page_path = format!("{path}?per_page={PAGE_SIZE}&page={page}{filter_query}");
            let response: P = self.api_request("GET", &page_path, None::<&()>).await?;
            let (page_items, links, meta) = response.into_parts();

            let page_len = page_items.len();
            let before = items.len();
            for item in page_items {
                if seen.insert(P::key(&item)) {
                    items.push(item);
                }
            }
            let has_next = links
                .and_then(|links| links.pages)
                .and_then(|pages| pages.next)
                .is_some_and(|next| !next.is_empty());

            if !has_next {
                if filter_query.is_empty() {
                    if let Some(total) = meta.and_then(|meta| meta.total) {
                        if (items.len() as u64) < total {
                            return Err(DnsError::ApiError(format!(
                                "{context}: DigitalOcean reported {total} {} but only {} were returned across {page} page(s); refusing a partial result",
                                P::NOUN,
                                items.len()
                            )));
                        }
                    }
                }
                return Ok(items);
            }
            if page_len == 0 {
                return Err(DnsError::ApiError(format!(
                    "{context}: DigitalOcean returned empty page {page} with a next link; refusing a partial result"
                )));
            }
            if items.len() == before {
                return Err(DnsError::ApiError(format!(
                    "{context}: DigitalOcean returned page {page} with only {} already seen and a next link; refusing a partial result",
                    P::NOUN
                )));
            }
        }
        Err(DnsError::ApiError(format!(
            "{context} exceeded {} pages; refusing a partial result",
            self.max_pages
        )))
    }

    /// Every domain record DigitalOcean returns for `domain`, optionally
    /// narrowed by the server-side `filter` query parameters (`type`,
    /// `name`); see [`Self::list_all`].
    async fn list_domain_records(
        &self,
        domain: &str,
        filter: &[(&str, String)],
        context: &str,
    ) -> Result<Vec<DoDomainRecord>, DnsError> {
        let filter_query: String = filter
            .iter()
            .map(|(key, value)| format!("&{key}={}", urlencoding::encode(value)))
            .collect();
        self.list_all::<DomainRecordsResponse>(
            &format!("/domains/{domain}/records"),
            &filter_query,
            context,
        )
        .await
    }

    /// A DigitalOcean domain as a [`DnsZone`] (`id` = domain name).
    fn dns_zone(domain: DoDomain) -> DnsZone {
        DnsZone {
            id: domain.name.clone(),
            name: domain.name,
            status: "active".to_string(),
            nameservers: vec![
                "ns1.digitalocean.com".to_string(),
                "ns2.digitalocean.com".to_string(),
                "ns3.digitalocean.com".to_string(),
            ],
            metadata: HashMap::new(),
        }
    }

    /// Make a DELETE request (returns no body)
    async fn api_delete(&self, path: &str) -> Result<(), DnsError> {
        let url = format!("{}{}", self.base_url, path);

        debug!("DigitalOcean API DELETE: {}", path);

        let response = self
            .client
            .delete(&url)
            .header(
                "Authorization",
                format!("Bearer {}", self.credentials.api_token),
            )
            .send()
            .await
            .map_err(|e| DnsError::ApiError(format!("API request failed: {}", e)))?;

        let status = response.status();

        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            return Err(map_delete_failure(path, status, &error_body));
        }

        Ok(())
    }

    /// Convert DigitalOcean record to our DnsRecord type
    fn convert_record(record: &DoDomainRecord, domain: &str) -> Option<DnsRecord> {
        let record_type = match record.record_type.to_uppercase().as_str() {
            "A" => DnsRecordType::A,
            "AAAA" => DnsRecordType::AAAA,
            "CNAME" => DnsRecordType::CNAME,
            "TXT" => DnsRecordType::TXT,
            "MX" => DnsRecordType::MX,
            "NS" => DnsRecordType::NS,
            "SRV" => DnsRecordType::SRV,
            "CAA" => DnsRecordType::CAA,
            _ => return None,
        };

        let content = Self::parse_record_content(record, record_type)?;

        let name = if record.name == "@" {
            "@".to_string()
        } else {
            record.name.clone()
        };

        let fqdn = if name == "@" {
            domain.to_string()
        } else {
            format!("{}.{}", name, domain)
        };

        Some(DnsRecord {
            id: Some(record.id.to_string()),
            zone: domain.to_string(),
            name,
            fqdn,
            content,
            ttl: record.ttl,
            proxied: false,
            metadata: HashMap::new(),
        })
    }

    /// Parse record data into DnsRecordContent
    fn parse_record_content(
        record: &DoDomainRecord,
        record_type: DnsRecordType,
    ) -> Option<DnsRecordContent> {
        match record_type {
            DnsRecordType::A => Some(DnsRecordContent::A {
                address: record.data.clone(),
            }),
            DnsRecordType::AAAA => Some(DnsRecordContent::AAAA {
                address: record.data.clone(),
            }),
            DnsRecordType::CNAME => Some(DnsRecordContent::CNAME {
                target: record.data.trim_end_matches('.').to_string(),
            }),
            DnsRecordType::TXT => Some(DnsRecordContent::TXT {
                content: record.data.clone(),
            }),
            DnsRecordType::MX => Some(DnsRecordContent::MX {
                priority: record.priority.unwrap_or(10),
                target: record.data.trim_end_matches('.').to_string(),
            }),
            DnsRecordType::NS => Some(DnsRecordContent::NS {
                nameserver: record.data.trim_end_matches('.').to_string(),
            }),
            DnsRecordType::SRV => Some(DnsRecordContent::SRV {
                priority: record.priority.unwrap_or(0),
                weight: record.weight.unwrap_or(0),
                port: record.port.unwrap_or(0),
                target: record.data.trim_end_matches('.').to_string(),
            }),
            DnsRecordType::CAA => Some(DnsRecordContent::CAA {
                flags: record.flags.unwrap_or(0),
                tag: record.tag.clone().unwrap_or_default(),
                value: record.data.clone(),
            }),
            DnsRecordType::PTR => None, // Not commonly used
        }
    }

    /// Build create record request from DnsRecordRequest
    fn build_create_request(request: &DnsRecordRequest) -> CreateRecordRequest {
        let record_type = request.content.record_type().to_string();

        match &request.content {
            DnsRecordContent::A { address } => CreateRecordRequest {
                record_type,
                name: request.name.clone(),
                data: address.clone(),
                priority: None,
                port: None,
                weight: None,
                ttl: request.ttl.unwrap_or(1800),
                flags: None,
                tag: None,
            },
            DnsRecordContent::AAAA { address } => CreateRecordRequest {
                record_type,
                name: request.name.clone(),
                data: address.clone(),
                priority: None,
                port: None,
                weight: None,
                ttl: request.ttl.unwrap_or(1800),
                flags: None,
                tag: None,
            },
            DnsRecordContent::CNAME { target } => CreateRecordRequest {
                record_type,
                name: request.name.clone(),
                data: format!("{}.", target.trim_end_matches('.')),
                priority: None,
                port: None,
                weight: None,
                ttl: request.ttl.unwrap_or(1800),
                flags: None,
                tag: None,
            },
            DnsRecordContent::TXT { content } => CreateRecordRequest {
                record_type,
                name: request.name.clone(),
                data: content.clone(),
                priority: None,
                port: None,
                weight: None,
                ttl: request.ttl.unwrap_or(1800),
                flags: None,
                tag: None,
            },
            DnsRecordContent::MX { priority, target } => CreateRecordRequest {
                record_type,
                name: request.name.clone(),
                data: format!("{}.", target.trim_end_matches('.')),
                priority: Some(*priority),
                port: None,
                weight: None,
                ttl: request.ttl.unwrap_or(1800),
                flags: None,
                tag: None,
            },
            DnsRecordContent::NS { nameserver } => CreateRecordRequest {
                record_type,
                name: request.name.clone(),
                data: format!("{}.", nameserver.trim_end_matches('.')),
                priority: None,
                port: None,
                weight: None,
                ttl: request.ttl.unwrap_or(1800),
                flags: None,
                tag: None,
            },
            DnsRecordContent::SRV {
                priority,
                weight,
                port,
                target,
            } => CreateRecordRequest {
                record_type,
                name: request.name.clone(),
                data: format!("{}.", target.trim_end_matches('.')),
                priority: Some(*priority),
                port: Some(*port),
                weight: Some(*weight),
                ttl: request.ttl.unwrap_or(1800),
                flags: None,
                tag: None,
            },
            DnsRecordContent::CAA { flags, tag, value } => CreateRecordRequest {
                record_type,
                name: request.name.clone(),
                data: value.clone(),
                priority: None,
                port: None,
                weight: None,
                ttl: request.ttl.unwrap_or(1800),
                flags: Some(*flags),
                tag: Some(tag.clone()),
            },
            DnsRecordContent::PTR { target } => CreateRecordRequest {
                record_type,
                name: request.name.clone(),
                data: format!("{}.", target.trim_end_matches('.')),
                priority: None,
                port: None,
                weight: None,
                ttl: request.ttl.unwrap_or(1800),
                flags: None,
                tag: None,
            },
        }
    }
}

fn map_delete_failure(path: &str, status: reqwest::StatusCode, body: &str) -> DnsError {
    if status == reqwest::StatusCode::NOT_FOUND {
        DnsError::RecordNotFound(path.to_string())
    } else {
        DnsError::ApiError(format!(
            "DigitalOcean API returned status {} for DELETE {}: {}",
            status,
            path,
            truncate_error_body(body)
        ))
    }
}

#[async_trait]
impl DnsProvider for DigitalOceanProvider {
    fn provider_type(&self) -> DnsProviderType {
        DnsProviderType::DigitalOcean
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
                info!("DigitalOcean API connection test successful");
                Ok(true)
            }
            Err(e) => {
                warn!("DigitalOcean API connection test failed: {}", e);
                Ok(false)
            }
        }
    }

    /// Every domain on the account, reading [`PAGE_SIZE`] per page until
    /// `links.pages.next` is gone.
    ///
    /// Never returns a partial list: reaching the page cap, an empty page
    /// that still has a next link, a page holding only domains already seen,
    /// or fewer domains than `meta.total` is an error.
    async fn list_zones(&self) -> Result<Vec<DnsZone>, DnsError> {
        Ok(self
            .list_all::<DomainsResponse>("/domains", "", "DigitalOcean domain listing")
            .await?
            .into_iter()
            .map(Self::dns_zone)
            .collect())
    }

    /// The domain named exactly `domain` (case-insensitive, trailing dot
    /// ignored), read directly with `GET /domains/{domain}` instead of a
    /// listing. HTTP 404 means the account has no such domain; any other
    /// failure is an error, never "absent".
    async fn get_zone(&self, domain: &str) -> Result<Option<DnsZone>, DnsError> {
        let normalized = domain.trim_end_matches('.').to_ascii_lowercase();
        if normalized.is_empty() {
            return Ok(None);
        }
        let path = format!("/domains/{}", urlencoding::encode(&normalized));
        let (status, body) = self.send("GET", &path, None::<&()>).await?;
        if status == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !status.is_success() {
            return Err(Self::status_error("GET", &path, status, &body));
        }

        let response: DomainResponse = serde_json::from_str(&body).map_err(|e| {
            DnsError::ApiError(format!(
                "Failed to parse DigitalOcean API response for GET {path}: {e} - Body: {}",
                truncate_error_body(&body)
            ))
        })?;
        if !dns_names_equal(&response.domain.name, &normalized) {
            return Err(DnsError::ApiError(format!(
                "DigitalOcean answered the lookup of domain {normalized} with domain {}; refusing to use it",
                truncate_error_body(&response.domain.name)
            )));
        }
        Ok(Some(Self::dns_zone(response.domain)))
    }

    async fn list_records(&self, domain: &str) -> Result<Vec<DnsRecord>, DnsError> {
        let context = format!("DigitalOcean record listing for zone {domain}");
        Ok(self
            .list_domain_records(domain, &[], &context)
            .await?
            .iter()
            .filter_map(|r| Self::convert_record(r, domain))
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

    /// Every value at (name, type), read with DigitalOcean's server-side
    /// `type` and fully-qualified `name` filters instead of a zone listing.
    ///
    /// The zone apex is looked up by `type` alone and matched client-side:
    /// DigitalOcean stores apex records as `@`, so the result never depends
    /// on how the API's `name` filter treats the bare zone name. Every
    /// result is matched again client-side (case-insensitively), so a filter
    /// the API ignored cannot leak other records.
    async fn get_records(
        &self,
        domain: &str,
        name: &str,
        record_type: DnsRecordType,
    ) -> Result<Vec<DnsRecord>, DnsError> {
        let relative = name.trim_end_matches('.');
        let apex = relative.is_empty() || relative == "@";
        let wanted = if apex { "@" } else { relative };

        let mut filter = vec![("type", record_type.to_string())];
        if !apex {
            filter.push((
                "name",
                format!("{relative}.{}", domain.trim_end_matches('.')).to_ascii_lowercase(),
            ));
        }
        let context = format!("DigitalOcean lookup of {record_type} '{wanted}' in zone {domain}");

        Ok(self
            .list_domain_records(domain, &filter, &context)
            .await?
            .iter()
            .filter_map(|r| Self::convert_record(r, domain))
            .filter(|r| dns_names_equal(&r.name, wanted) && r.content.record_type() == record_type)
            .collect())
    }

    /// Not create-only: DigitalOcean has no conditional create, and
    /// `POST /domains/{domain}/records` always adds a value — next to any
    /// values already at that (name, type), which turns the name into a
    /// round-robin set with them. Callers that must never touch a foreign
    /// record (the ownership-guarded core) prove the name is free with a
    /// complete [`DnsProvider::get_records`] read under their per-record lock
    /// immediately before calling this.
    async fn create_record(
        &self,
        domain: &str,
        request: DnsRecordRequest,
    ) -> Result<DnsRecord, DnsError> {
        let create_request = Self::build_create_request(&request);
        let path = format!("/domains/{}/records", domain);

        let response: DomainRecordResponse = self
            .api_request("POST", &path, Some(&create_request))
            .await?;

        let record = Self::convert_record(&response.domain_record, domain)
            .ok_or_else(|| DnsError::ApiError("Failed to convert created record".to_string()))?;

        info!("Created DNS record {} for domain {}", request.name, domain);

        Ok(record)
    }

    async fn update_record(
        &self,
        domain: &str,
        record_id: &str,
        request: DnsRecordRequest,
    ) -> Result<DnsRecord, DnsError> {
        let update_request = Self::build_create_request(&request);
        let path = format!("/domains/{}/records/{}", domain, record_id);

        let response: DomainRecordResponse = self
            .api_request("PUT", &path, Some(&update_request))
            .await?;

        let record = Self::convert_record(&response.domain_record, domain)
            .ok_or_else(|| DnsError::ApiError("Failed to convert updated record".to_string()))?;

        info!("Updated DNS record {} for domain {}", request.name, domain);

        Ok(record)
    }

    async fn delete_record(&self, domain: &str, record_id: &str) -> Result<(), DnsError> {
        let path = format!("/domains/{}/records/{}", domain, record_id);
        self.api_delete(&path).await?;

        info!("Deleted DNS record {} from domain {}", record_id, domain);

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delete_not_found_is_typed_for_idempotent_callers() {
        let error = map_delete_failure(
            "/domains/example.com/records/42",
            reqwest::StatusCode::NOT_FOUND,
            "not found",
        );

        assert!(matches!(
            error,
            DnsError::RecordNotFound(path)
                if path == "/domains/example.com/records/42"
        ));
    }

    #[test]
    fn test_provider_type() {
        let creds = DigitalOceanCredentials {
            api_token: "test_token".to_string(),
        };
        let provider = DigitalOceanProvider::new(creds).unwrap();
        assert_eq!(provider.provider_type(), DnsProviderType::DigitalOcean);
    }

    #[test]
    fn test_capabilities() {
        let creds = DigitalOceanCredentials {
            api_token: "test_token".to_string(),
        };
        let provider = DigitalOceanProvider::new(creds).unwrap();
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
    fn test_convert_record_a() {
        let do_record = DoDomainRecord {
            id: 12345,
            record_type: "A".to_string(),
            name: "www".to_string(),
            data: "192.0.2.1".to_string(),
            priority: None,
            port: None,
            weight: None,
            ttl: 300,
            flags: None,
            tag: None,
        };

        let record = DigitalOceanProvider::convert_record(&do_record, "example.com").unwrap();

        assert_eq!(record.id, Some("12345".to_string()));
        assert_eq!(record.name, "www");
        assert_eq!(record.fqdn, "www.example.com");
        assert_eq!(record.ttl, 300);
        if let DnsRecordContent::A { address } = &record.content {
            assert_eq!(address, "192.0.2.1");
        } else {
            panic!("Expected A record");
        }
    }

    #[test]
    fn test_convert_record_apex() {
        let do_record = DoDomainRecord {
            id: 12345,
            record_type: "A".to_string(),
            name: "@".to_string(),
            data: "192.0.2.1".to_string(),
            priority: None,
            port: None,
            weight: None,
            ttl: 300,
            flags: None,
            tag: None,
        };

        let record = DigitalOceanProvider::convert_record(&do_record, "example.com").unwrap();

        assert_eq!(record.name, "@");
        assert_eq!(record.fqdn, "example.com");
    }

    #[test]
    fn test_convert_record_txt() {
        let do_record = DoDomainRecord {
            id: 12346,
            record_type: "TXT".to_string(),
            name: "@".to_string(),
            data: "v=spf1 -all".to_string(),
            priority: None,
            port: None,
            weight: None,
            ttl: 3600,
            flags: None,
            tag: None,
        };

        let record = DigitalOceanProvider::convert_record(&do_record, "example.com").unwrap();

        if let DnsRecordContent::TXT { content } = &record.content {
            assert_eq!(content, "v=spf1 -all");
        } else {
            panic!("Expected TXT record");
        }
    }

    #[test]
    fn test_convert_record_mx() {
        let do_record = DoDomainRecord {
            id: 12347,
            record_type: "MX".to_string(),
            name: "@".to_string(),
            data: "mail.example.com.".to_string(),
            priority: Some(10),
            port: None,
            weight: None,
            ttl: 3600,
            flags: None,
            tag: None,
        };

        let record = DigitalOceanProvider::convert_record(&do_record, "example.com").unwrap();

        if let DnsRecordContent::MX { priority, target } = &record.content {
            assert_eq!(*priority, 10);
            assert_eq!(target, "mail.example.com");
        } else {
            panic!("Expected MX record");
        }
    }

    #[test]
    fn test_convert_record_srv() {
        let do_record = DoDomainRecord {
            id: 12348,
            record_type: "SRV".to_string(),
            name: "_sip._tcp".to_string(),
            data: "sip.example.com.".to_string(),
            priority: Some(10),
            port: Some(5060),
            weight: Some(5),
            ttl: 3600,
            flags: None,
            tag: None,
        };

        let record = DigitalOceanProvider::convert_record(&do_record, "example.com").unwrap();

        if let DnsRecordContent::SRV {
            priority,
            weight,
            port,
            target,
        } = &record.content
        {
            assert_eq!(*priority, 10);
            assert_eq!(*weight, 5);
            assert_eq!(*port, 5060);
            assert_eq!(target, "sip.example.com");
        } else {
            panic!("Expected SRV record");
        }
    }

    #[test]
    fn test_convert_record_caa() {
        let do_record = DoDomainRecord {
            id: 12349,
            record_type: "CAA".to_string(),
            name: "@".to_string(),
            data: "letsencrypt.org".to_string(),
            priority: None,
            port: None,
            weight: None,
            ttl: 3600,
            flags: Some(0),
            tag: Some("issue".to_string()),
        };

        let record = DigitalOceanProvider::convert_record(&do_record, "example.com").unwrap();

        if let DnsRecordContent::CAA { flags, tag, value } = &record.content {
            assert_eq!(*flags, 0);
            assert_eq!(tag, "issue");
            assert_eq!(value, "letsencrypt.org");
        } else {
            panic!("Expected CAA record");
        }
    }

    #[test]
    fn test_build_create_request_a() {
        let request = DnsRecordRequest {
            name: "www".to_string(),
            content: DnsRecordContent::A {
                address: "192.0.2.1".to_string(),
            },
            ttl: Some(300),
            proxied: false,
        };

        let create_req = DigitalOceanProvider::build_create_request(&request);

        assert_eq!(create_req.record_type, "A");
        assert_eq!(create_req.name, "www");
        assert_eq!(create_req.data, "192.0.2.1");
        assert_eq!(create_req.ttl, 300);
        assert!(create_req.priority.is_none());
    }

    #[test]
    fn test_build_create_request_mx() {
        let request = DnsRecordRequest {
            name: "@".to_string(),
            content: DnsRecordContent::MX {
                priority: 10,
                target: "mail.example.com".to_string(),
            },
            ttl: Some(3600),
            proxied: false,
        };

        let create_req = DigitalOceanProvider::build_create_request(&request);

        assert_eq!(create_req.record_type, "MX");
        assert_eq!(create_req.name, "@");
        assert_eq!(create_req.data, "mail.example.com.");
        assert_eq!(create_req.priority, Some(10));
        assert_eq!(create_req.ttl, 3600);
    }

    #[test]
    fn test_build_create_request_txt() {
        let request = DnsRecordRequest {
            name: "_acme-challenge".to_string(),
            content: DnsRecordContent::TXT {
                content: "verification-token".to_string(),
            },
            ttl: Some(60),
            proxied: false,
        };

        let create_req = DigitalOceanProvider::build_create_request(&request);

        assert_eq!(create_req.record_type, "TXT");
        assert_eq!(create_req.name, "_acme-challenge");
        assert_eq!(create_req.data, "verification-token");
        assert_eq!(create_req.ttl, 60);
    }

    #[test]
    fn test_build_create_request_cname_trailing_dot() {
        let request = DnsRecordRequest {
            name: "www".to_string(),
            content: DnsRecordContent::CNAME {
                target: "example.com".to_string(),
            },
            ttl: Some(300),
            proxied: false,
        };

        let create_req = DigitalOceanProvider::build_create_request(&request);

        assert_eq!(create_req.record_type, "CNAME");
        assert_eq!(create_req.data, "example.com.");
    }

    #[test]
    fn test_build_create_request_default_ttl() {
        let request = DnsRecordRequest {
            name: "www".to_string(),
            content: DnsRecordContent::A {
                address: "192.0.2.1".to_string(),
            },
            ttl: None,
            proxied: false,
        };

        let create_req = DigitalOceanProvider::build_create_request(&request);

        assert_eq!(create_req.ttl, 1800); // Default TTL
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use serde_json::{json, Value};
    use wiremock::matchers::{any, header, method, path, query_param, query_param_is_missing};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn create_mock_provider(mock_server: &MockServer) -> DigitalOceanProvider {
        let creds = DigitalOceanCredentials {
            api_token: "test_token_12345".to_string(),
        };

        DigitalOceanProvider::with_base_url(creds, mock_server.uri()).unwrap()
    }

    fn do_record(id: i64, record_type: &str, name: &str, data: &str) -> Value {
        json!({"id": id, "type": record_type, "name": name, "data": data, "ttl": 300})
    }

    /// A records page; `next_page` adds `links.pages.next`.
    fn records_page(records: Vec<Value>, next_page: Option<usize>, total: Option<u64>) -> Value {
        let mut body = json!({"domain_records": records, "links": {}});
        if let Some(next) = next_page {
            body["links"] = json!({"pages": {
                "next": format!("https://api.example.com/v2/domains/example.com/records?page={next}&per_page=200")
            }});
        }
        if let Some(total) = total {
            body["meta"] = json!({"total": total});
        }
        body
    }

    /// Page `page` of the unfiltered zone listing.
    async fn mount_listing_page(server: &MockServer, page: &str, body: Value) {
        Mock::given(method("GET"))
            .and(path("/domains/example.com/records"))
            .and(query_param("per_page", "200"))
            .and(query_param("page", page))
            .and(query_param_is_missing("type"))
            .and(header("Authorization", "Bearer test_token_12345"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn list_records_follows_next_links_with_full_pages() {
        let server = MockServer::start().await;
        mount_listing_page(
            &server,
            "1",
            records_page(
                vec![do_record(1, "A", "www", "203.0.113.1")],
                Some(2),
                Some(3),
            ),
        )
        .await;
        mount_listing_page(
            &server,
            "2",
            records_page(
                vec![do_record(2, "A", "api", "203.0.113.2")],
                Some(3),
                Some(3),
            ),
        )
        .await;
        // The registry TXT only exists on the last page: a one-page read
        // would report it absent.
        mount_listing_page(
            &server,
            "3",
            records_page(
                vec![do_record(3, "TXT", "_temps-owned-a.app", "marker")],
                None,
                Some(3),
            ),
        )
        .await;

        let provider = create_mock_provider(&server).await;
        let records = provider.list_records("example.com").await.unwrap();

        let names: Vec<&str> = records.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["www", "api", "_temps-owned-a.app"]);
    }

    #[tokio::test]
    async fn list_records_returns_records_repeated_across_pages_once() {
        let server = MockServer::start().await;
        mount_listing_page(
            &server,
            "1",
            records_page(
                vec![
                    do_record(1, "A", "www", "203.0.113.1"),
                    do_record(2, "A", "api", "203.0.113.2"),
                ],
                Some(2),
                None,
            ),
        )
        .await;
        // A concurrent insert shifted record 2 onto the next page.
        mount_listing_page(
            &server,
            "2",
            records_page(
                vec![
                    do_record(2, "A", "api", "203.0.113.2"),
                    do_record(3, "A", "mail", "203.0.113.3"),
                ],
                None,
                None,
            ),
        )
        .await;

        let provider = create_mock_provider(&server).await;
        let records = provider.list_records("example.com").await.unwrap();

        let ids: Vec<Option<String>> = records.iter().map(|r| r.id.clone()).collect();
        assert_eq!(
            ids,
            vec![
                Some("1".to_string()),
                Some("2".to_string()),
                Some("3".to_string())
            ]
        );
    }

    #[tokio::test]
    async fn list_records_fails_closed_on_an_empty_page_with_a_next_link() {
        let server = MockServer::start().await;
        mount_listing_page(&server, "1", records_page(vec![], Some(2), None)).await;

        let provider = create_mock_provider(&server).await;
        let error = provider.list_records("example.com").await.unwrap_err();

        assert!(
            matches!(&error, DnsError::ApiError(message)
                if message.contains("empty page 1 with a next link")),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn list_records_fails_closed_on_a_page_of_already_seen_records() {
        let server = MockServer::start().await;
        let page = || records_page(vec![do_record(1, "A", "www", "203.0.113.1")], Some(2), None);
        mount_listing_page(&server, "1", page()).await;
        mount_listing_page(&server, "2", page()).await;

        let provider = create_mock_provider(&server).await;
        let error = provider.list_records("example.com").await.unwrap_err();

        assert!(
            matches!(&error, DnsError::ApiError(message)
                if message.contains("page 2 with only records already seen")),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn list_records_fails_closed_below_the_reported_total() {
        let server = MockServer::start().await;
        mount_listing_page(
            &server,
            "1",
            records_page(vec![do_record(1, "A", "www", "203.0.113.1")], None, Some(5)),
        )
        .await;

        let provider = create_mock_provider(&server).await;
        let error = provider.list_records("example.com").await.unwrap_err();

        assert!(
            matches!(&error, DnsError::ApiError(message)
                if message.contains("reported 5 records but only 1 were returned")),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn list_records_fails_closed_at_the_page_cap() {
        let server = MockServer::start().await;
        mount_listing_page(
            &server,
            "1",
            records_page(vec![do_record(1, "A", "a", "203.0.113.1")], Some(2), None),
        )
        .await;
        mount_listing_page(
            &server,
            "2",
            records_page(vec![do_record(2, "A", "b", "203.0.113.2")], Some(3), None),
        )
        .await;
        Mock::given(method("GET"))
            .and(query_param("page", "3"))
            .respond_with(ResponseTemplate::new(200).set_body_json(records_page(
                vec![],
                None,
                None,
            )))
            .expect(0)
            .mount(&server)
            .await;

        let mut provider = create_mock_provider(&server).await;
        provider.max_pages = 2;
        let error = provider.list_records("example.com").await.unwrap_err();

        assert!(
            matches!(&error, DnsError::ApiError(message)
                if message.contains("DigitalOcean record listing for zone example.com exceeded 2 pages")),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn get_records_uses_the_type_and_fqdn_filters() {
        let server = MockServer::start().await;
        // A zone listing must never be used for an exact lookup.
        Mock::given(method("GET"))
            .and(path("/domains/example.com/records"))
            .and(query_param_is_missing("type"))
            .respond_with(ResponseTemplate::new(200).set_body_json(records_page(
                vec![],
                None,
                None,
            )))
            .expect(0)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/domains/example.com/records"))
            .and(query_param("type", "A"))
            .and(query_param("name", "app.example.com"))
            .and(query_param("per_page", "200"))
            .respond_with(ResponseTemplate::new(200).set_body_json(records_page(
                vec![
                    do_record(1, "A", "app", "203.0.113.1"),
                    do_record(2, "A", "App", "203.0.113.2"),
                    // Never returned for this filter; must not leak through.
                    do_record(3, "A", "other", "203.0.113.9"),
                ],
                None,
                None,
            )))
            .mount(&server)
            .await;

        let provider = create_mock_provider(&server).await;
        let records = provider
            .get_records("example.com", "APP", DnsRecordType::A)
            .await
            .unwrap();

        let ids: Vec<String> = records.iter().filter_map(|r| r.id.clone()).collect();
        assert_eq!(ids, vec!["1".to_string(), "2".to_string()]);
    }

    #[tokio::test]
    async fn get_records_follows_pages_of_a_filtered_lookup() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/domains/example.com/records"))
            .and(query_param("name", "_acme-challenge.example.com"))
            .and(query_param("page", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(records_page(
                vec![do_record(1, "TXT", "_acme-challenge", "token-a")],
                Some(2),
                None,
            )))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/domains/example.com/records"))
            .and(query_param("name", "_acme-challenge.example.com"))
            .and(query_param("page", "2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(records_page(
                vec![do_record(2, "TXT", "_acme-challenge", "token-b")],
                None,
                None,
            )))
            .mount(&server)
            .await;

        let provider = create_mock_provider(&server).await;
        let records = provider
            .get_records("example.com", "_acme-challenge", DnsRecordType::TXT)
            .await
            .unwrap();

        let values: Vec<String> = records
            .iter()
            .map(|r| r.content.to_value_string())
            .collect();
        assert_eq!(values, vec!["token-a", "token-b"]);
    }

    #[tokio::test]
    async fn get_records_looks_up_the_apex_by_type_only() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/domains/example.com/records"))
            .and(query_param("type", "TXT"))
            .and(query_param_is_missing("name"))
            .respond_with(ResponseTemplate::new(200).set_body_json(records_page(
                vec![
                    do_record(1, "TXT", "@", "v=spf1 -all"),
                    do_record(2, "TXT", "www", "unrelated"),
                ],
                None,
                None,
            )))
            .expect(2)
            .mount(&server)
            .await;

        let provider = create_mock_provider(&server).await;
        for apex in ["@", ""] {
            let records = provider
                .get_records("example.com", apex, DnsRecordType::TXT)
                .await
                .unwrap();
            assert_eq!(records.len(), 1);
            assert_eq!(records[0].id, Some("1".to_string()));
        }
    }

    #[tokio::test]
    async fn api_errors_embed_a_bounded_slice_of_the_body() {
        let server = MockServer::start().await;
        Mock::given(any())
            .respond_with(ResponseTemplate::new(500).set_body_string("e".repeat(20_000)))
            .mount(&server)
            .await;

        let provider = create_mock_provider(&server).await;
        let error = provider.list_records("example.com").await.unwrap_err();

        let message = error.to_string();
        assert!(message.contains("500"), "{message}");
        assert!(
            message.contains("truncated, 20000 bytes total"),
            "{message}"
        );
        assert!(message.len() < 1_000, "error is {} bytes", message.len());
        // Page parameters stay out of the message.
        assert!(!message.contains("per_page"), "{message}");
    }

    fn do_domain(name: &str) -> Value {
        json!({"name": name, "ttl": 1800, "zone_file": ""})
    }

    /// A domains page; `next_page` adds `links.pages.next`.
    fn domains_page(domains: Vec<Value>, next_page: Option<usize>, total: Option<u64>) -> Value {
        let mut body = json!({"domains": domains, "links": {}});
        if let Some(next) = next_page {
            body["links"] = json!({"pages": {
                "next": format!("https://api.example.com/v2/domains?page={next}&per_page=200")
            }});
        }
        if let Some(total) = total {
            body["meta"] = json!({"total": total});
        }
        body
    }

    /// Page `page` of the domain listing.
    async fn mount_domains_page(server: &MockServer, page: &str, body: Value) {
        Mock::given(method("GET"))
            .and(path("/domains"))
            .and(query_param("per_page", "200"))
            .and(query_param("page", page))
            .and(header("Authorization", "Bearer test_token_12345"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn list_zones_follows_next_links() {
        let server = MockServer::start().await;
        mount_domains_page(
            &server,
            "1",
            domains_page(vec![do_domain("example.com")], Some(2), Some(2)),
        )
        .await;
        mount_domains_page(
            &server,
            "2",
            domains_page(vec![do_domain("example.net")], None, Some(2)),
        )
        .await;

        let zones = create_mock_provider(&server)
            .await
            .list_zones()
            .await
            .unwrap();

        let names: Vec<&str> = zones.iter().map(|zone| zone.name.as_str()).collect();
        assert_eq!(names, vec!["example.com", "example.net"]);
    }

    #[tokio::test]
    async fn list_zones_fails_closed() {
        // An empty page that still has a next link.
        let server = MockServer::start().await;
        mount_domains_page(&server, "1", domains_page(vec![], Some(2), None)).await;
        let error = create_mock_provider(&server)
            .await
            .list_zones()
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("empty page 1 with a next link"),
            "{error}"
        );

        // A page holding only domains already seen.
        let server = MockServer::start().await;
        let page = || domains_page(vec![do_domain("example.com")], Some(2), None);
        mount_domains_page(&server, "1", page()).await;
        mount_domains_page(&server, "2", page()).await;
        let error = create_mock_provider(&server)
            .await
            .list_zones()
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("page 2 with only domains already seen"),
            "{error}"
        );

        // Fewer domains than the reported total.
        let server = MockServer::start().await;
        mount_domains_page(
            &server,
            "1",
            domains_page(vec![do_domain("example.com")], None, Some(3)),
        )
        .await;
        let error = create_mock_provider(&server)
            .await
            .list_zones()
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("reported 3 domains but only 1 were returned"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn list_zones_fails_closed_at_the_page_cap() {
        let server = MockServer::start().await;
        mount_domains_page(
            &server,
            "1",
            domains_page(vec![do_domain("example.com")], Some(2), None),
        )
        .await;
        mount_domains_page(
            &server,
            "2",
            domains_page(vec![do_domain("example.net")], Some(3), None),
        )
        .await;
        Mock::given(method("GET"))
            .and(path("/domains"))
            .and(query_param("page", "3"))
            .respond_with(ResponseTemplate::new(200).set_body_json(domains_page(
                vec![],
                None,
                None,
            )))
            .expect(0)
            .mount(&server)
            .await;

        let mut provider = create_mock_provider(&server).await;
        provider.max_pages = 2;
        let error = provider.list_zones().await.unwrap_err();

        assert!(
            error
                .to_string()
                .contains("DigitalOcean domain listing exceeded 2 pages"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn get_zone_reads_the_domain_directly() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/domains/example.com"))
            .and(header("Authorization", "Bearer test_token_12345"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"domain": do_domain("example.com")})),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/domains/missing.example.com"))
            .respond_with(ResponseTemplate::new(404).set_body_json(json!({
                "id": "not_found",
                "message": "The resource you were accessing could not be found."
            })))
            .mount(&server)
            .await;
        // Neither the listing nor another endpoint is ever reached.
        Mock::given(method("GET"))
            .and(path("/domains"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/domains/example.com/records"))
            .respond_with(ResponseTemplate::new(200).set_body_json(records_page(
                vec![],
                None,
                None,
            )))
            .expect(0)
            .mount(&server)
            .await;

        let provider = create_mock_provider(&server).await;
        let zone = provider.get_zone("Example.COM.").await.unwrap().unwrap();
        assert_eq!(zone.id, "example.com");
        assert_eq!(zone.name, "example.com");

        assert!(provider
            .get_zone("missing.example.com")
            .await
            .unwrap()
            .is_none());
        // A name that is not a single path segment stays one: the lookup
        // misses instead of reaching the records endpoint.
        assert!(provider
            .get_zone("example.com/records")
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn get_zone_fails_closed_on_errors_and_mismatched_answers() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/domains/example.com"))
            .respond_with(ResponseTemplate::new(500).set_body_string("internal failure"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/domains/example.net"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"domain": do_domain("other.example.net")})),
            )
            .mount(&server)
            .await;

        let provider = create_mock_provider(&server).await;
        let error = provider.get_zone("example.com").await.unwrap_err();
        assert!(error.to_string().contains("500"), "{error}");

        let error = provider.get_zone("example.net").await.unwrap_err();
        assert!(
            error.to_string().contains(
                "answered the lookup of domain example.net with domain other.example.net"
            ),
            "{error}"
        );
    }

    #[tokio::test]
    async fn test_list_zones() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/domains"))
            .and(header("Authorization", "Bearer test_token_12345"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "domains": [
                    {"name": "example.com", "ttl": 1800},
                    {"name": "test.org", "ttl": 3600}
                ]
            })))
            .mount(&mock_server)
            .await;

        let provider = create_mock_provider(&mock_server).await;
        let zones = provider.list_zones().await.unwrap();

        assert_eq!(zones.len(), 2);
        assert_eq!(zones[0].name, "example.com");
        assert_eq!(zones[1].name, "test.org");
    }

    #[tokio::test]
    async fn test_list_records() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/domains/example.com/records"))
            .and(header("Authorization", "Bearer test_token_12345"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "domain_records": [
                    {
                        "id": 12345,
                        "type": "A",
                        "name": "www",
                        "data": "192.0.2.1",
                        "ttl": 300
                    },
                    {
                        "id": 12346,
                        "type": "TXT",
                        "name": "@",
                        "data": "v=spf1 -all",
                        "ttl": 3600
                    }
                ]
            })))
            .mount(&mock_server)
            .await;

        let provider = create_mock_provider(&mock_server).await;
        let records = provider.list_records("example.com").await.unwrap();

        assert_eq!(records.len(), 2);
        assert_eq!(records[0].name, "www");
        assert_eq!(records[1].name, "@");
    }

    #[tokio::test]
    async fn test_create_record() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/domains/example.com/records"))
            .and(header("Authorization", "Bearer test_token_12345"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "domain_record": {
                    "id": 99999,
                    "type": "A",
                    "name": "api",
                    "data": "192.0.2.2",
                    "ttl": 300
                }
            })))
            .mount(&mock_server)
            .await;

        let provider = create_mock_provider(&mock_server).await;
        let request = DnsRecordRequest {
            name: "api".to_string(),
            content: DnsRecordContent::A {
                address: "192.0.2.2".to_string(),
            },
            ttl: Some(300),
            proxied: false,
        };

        let record = provider
            .create_record("example.com", request)
            .await
            .unwrap();

        assert_eq!(record.id, Some("99999".to_string()));
        assert_eq!(record.name, "api");
    }

    #[tokio::test]
    async fn test_update_record() {
        let mock_server = MockServer::start().await;

        Mock::given(method("PUT"))
            .and(path("/domains/example.com/records/12345"))
            .and(header("Authorization", "Bearer test_token_12345"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "domain_record": {
                    "id": 12345,
                    "type": "A",
                    "name": "www",
                    "data": "192.0.2.99",
                    "ttl": 600
                }
            })))
            .mount(&mock_server)
            .await;

        let provider = create_mock_provider(&mock_server).await;
        let request = DnsRecordRequest {
            name: "www".to_string(),
            content: DnsRecordContent::A {
                address: "192.0.2.99".to_string(),
            },
            ttl: Some(600),
            proxied: false,
        };

        let record = provider
            .update_record("example.com", "12345", request)
            .await
            .unwrap();

        assert_eq!(record.id, Some("12345".to_string()));
        if let DnsRecordContent::A { address } = &record.content {
            assert_eq!(address, "192.0.2.99");
        } else {
            panic!("Expected A record");
        }
    }

    #[tokio::test]
    async fn test_delete_record() {
        let mock_server = MockServer::start().await;

        Mock::given(method("DELETE"))
            .and(path("/domains/example.com/records/12345"))
            .and(header("Authorization", "Bearer test_token_12345"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&mock_server)
            .await;

        let provider = create_mock_provider(&mock_server).await;
        let result = provider.delete_record("example.com", "12345").await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_get_zone() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/domains/example.com"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "domain": {"name": "example.com", "ttl": 1800}
            })))
            .mount(&mock_server)
            .await;
        Mock::given(method("GET"))
            .and(path("/domains/missing.example.com"))
            .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
                "id": "not_found",
                "message": "The resource you were accessing could not be found."
            })))
            .mount(&mock_server)
            .await;

        let provider = create_mock_provider(&mock_server).await;

        let zone = provider.get_zone("example.com").await.unwrap();
        assert!(zone.is_some());
        assert_eq!(zone.unwrap().name, "example.com");

        let zone = provider.get_zone("missing.example.com").await.unwrap();
        assert!(zone.is_none());
    }

    #[tokio::test]
    async fn test_test_connection_success() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/domains"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "domains": []
            })))
            .mount(&mock_server)
            .await;

        let provider = create_mock_provider(&mock_server).await;
        let result = provider.test_connection().await.unwrap();

        assert!(result);
    }

    #[tokio::test]
    async fn test_test_connection_failure() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/domains"))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
                "id": "unauthorized",
                "message": "Unable to authenticate you."
            })))
            .mount(&mock_server)
            .await;

        let provider = create_mock_provider(&mock_server).await;
        let result = provider.test_connection().await.unwrap();

        assert!(!result);
    }

    #[tokio::test]
    async fn test_get_record() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/domains/example.com/records"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "domain_records": [
                    {
                        "id": 12345,
                        "type": "A",
                        "name": "www",
                        "data": "192.0.2.1",
                        "ttl": 300
                    },
                    {
                        "id": 12346,
                        "type": "A",
                        "name": "api",
                        "data": "192.0.2.2",
                        "ttl": 300
                    }
                ]
            })))
            .mount(&mock_server)
            .await;

        let provider = create_mock_provider(&mock_server).await;

        let record = provider
            .get_record("example.com", "www", DnsRecordType::A)
            .await
            .unwrap();
        assert!(record.is_some());
        assert_eq!(record.unwrap().name, "www");

        let record = provider
            .get_record("example.com", "nonexistent", DnsRecordType::A)
            .await
            .unwrap();
        assert!(record.is_none());
    }
}
