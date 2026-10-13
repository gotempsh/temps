// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Azure DNS provider implementation
//!
//! This provider uses the Azure DNS Management API to manage DNS records.
//! It requires a service principal with DNS Zone Contributor role.
//!
//! Required IAM Roles:
//! - DNS Zone Contributor (on the DNS zone or resource group)
//!
//! Authentication uses service principal credentials (client ID, client secret, tenant ID).

use async_trait::async_trait;
use reqwest::{Client, Method, StatusCode, Url};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use tracing::{debug, info, warn};

use super::credentials::AzureCredentials;
use super::traits::{
    dns_names_equal, truncate_error_body, DnsProvider, DnsProviderCapabilities, DnsProviderType,
    DnsRecord, DnsRecordContent, DnsRecordRequest, DnsRecordType, DnsZone,
};
use crate::errors::DnsError;

const AZURE_MANAGEMENT_BASE: &str = "https://management.azure.com";
const AZURE_LOGIN_URL: &str = "https://login.microsoftonline.com";
/// API version sent with every Azure DNS call (and added to a `nextLink`
/// that does not already carry one).
const AZURE_DNS_API_VERSION: &str = "2018-05-01";
/// Hard cap on pages read by any listing; reaching it is an error, never a
/// silently truncated result.
const MAX_PAGES: usize = 1000;

/// Azure DNS provider
pub struct AzureProvider {
    client: Client,
    credentials: AzureCredentials,
    base_url: String,
    /// Cached access token
    access_token: tokio::sync::RwLock<Option<String>>,
    /// Page cap for listings ([`MAX_PAGES`]; lowered in tests).
    max_pages: usize,
}

/// One page of an Azure list response.
#[derive(Debug, Deserialize)]
struct ListPage<T> {
    value: Vec<T>,
    #[serde(rename = "nextLink")]
    next_link: Option<String>,
}

/// Azure error envelope; ARM nests the code under `error`, some resource
/// providers put it at the top level.
#[derive(Debug, Deserialize)]
struct AzureErrorEnvelope {
    error: Option<AzureErrorDetail>,
    code: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AzureErrorDetail {
    code: Option<String>,
}

/// `code` of an Azure error body, when it has one.
fn azure_error_code(body: &str) -> Option<String> {
    let envelope: AzureErrorEnvelope = serde_json::from_str(body).ok()?;
    envelope
        .error
        .and_then(|detail| detail.code)
        .or(envelope.code)
}

/// Status and body of one authenticated Azure call.
struct AzureResponse {
    status: StatusCode,
    body: String,
}

#[derive(Debug, Deserialize)]
struct AzureZone {
    id: String,
    name: String,
    properties: ZoneProperties,
}

#[derive(Debug, Deserialize)]
struct ZoneProperties {
    #[serde(rename = "nameServers")]
    #[serde(default)]
    name_servers: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct AzureRecordSet {
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    name: String,
    #[serde(rename = "type")]
    #[serde(skip_serializing_if = "Option::is_none")]
    record_type: Option<String>,
    properties: RecordSetProperties,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct RecordSetProperties {
    #[serde(rename = "TTL")]
    #[serde(skip_serializing_if = "Option::is_none")]
    ttl: Option<u32>,
    #[serde(rename = "ARecords")]
    #[serde(skip_serializing_if = "Option::is_none")]
    a_records: Option<Vec<ARecord>>,
    #[serde(rename = "AAAARecords")]
    #[serde(skip_serializing_if = "Option::is_none")]
    aaaa_records: Option<Vec<AAAARecord>>,
    #[serde(rename = "CNAMERecord")]
    #[serde(skip_serializing_if = "Option::is_none")]
    cname_record: Option<CNAMERecord>,
    #[serde(rename = "TXTRecords")]
    #[serde(skip_serializing_if = "Option::is_none")]
    txt_records: Option<Vec<TXTRecord>>,
    #[serde(rename = "MXRecords")]
    #[serde(skip_serializing_if = "Option::is_none")]
    mx_records: Option<Vec<MXRecord>>,
    #[serde(rename = "NSRecords")]
    #[serde(skip_serializing_if = "Option::is_none")]
    ns_records: Option<Vec<NSRecord>>,
    #[serde(rename = "SRVRecords")]
    #[serde(skip_serializing_if = "Option::is_none")]
    srv_records: Option<Vec<SRVRecord>>,
    #[serde(rename = "CAARecords")]
    #[serde(skip_serializing_if = "Option::is_none")]
    caa_records: Option<Vec<CAARecord>>,
    #[serde(rename = "PTRRecords")]
    #[serde(skip_serializing_if = "Option::is_none")]
    ptr_records: Option<Vec<PTRRecord>>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct ARecord {
    #[serde(rename = "ipv4Address")]
    ipv4_address: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct AAAARecord {
    #[serde(rename = "ipv6Address")]
    ipv6_address: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct CNAMERecord {
    cname: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct TXTRecord {
    value: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct MXRecord {
    preference: u16,
    exchange: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct NSRecord {
    nsdname: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct SRVRecord {
    priority: u16,
    weight: u16,
    port: u16,
    target: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct CAARecord {
    flags: u8,
    tag: String,
    value: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct PTRRecord {
    ptrdname: String,
}

/// Token response from Azure AD
#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[allow(dead_code)]
    expires_in: u64,
    #[allow(dead_code)]
    token_type: String,
}

impl AzureProvider {
    /// Create a new Azure DNS provider with the given credentials
    pub fn new(credentials: AzureCredentials) -> Result<Self, DnsError> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| DnsError::ApiError(format!("Failed to create HTTP client: {}", e)))?;

        Ok(Self {
            client,
            credentials,
            base_url: AZURE_MANAGEMENT_BASE.to_string(),
            access_token: tokio::sync::RwLock::new(None),
            max_pages: MAX_PAGES,
        })
    }

    /// Create a provider with a custom base URL (for testing)
    #[cfg(test)]
    pub fn with_base_url(
        credentials: AzureCredentials,
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
            access_token: tokio::sync::RwLock::new(None),
            max_pages: MAX_PAGES,
        })
    }

    /// Create a provider with a pre-set access token (for testing)
    #[cfg(test)]
    pub fn with_test_token(
        credentials: AzureCredentials,
        base_url: String,
        token: String,
    ) -> Result<Self, DnsError> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| DnsError::ApiError(format!("Failed to create HTTP client: {}", e)))?;

        Ok(Self {
            client,
            credentials,
            base_url,
            access_token: tokio::sync::RwLock::new(Some(token)),
            max_pages: MAX_PAGES,
        })
    }

    /// Get access token for API requests
    async fn get_access_token(&self) -> Result<String, DnsError> {
        // Check if we have a cached token
        {
            let token = self.access_token.read().await;
            if let Some(ref t) = *token {
                return Ok(t.clone());
            }
        }

        // Get new token from Azure AD
        let token_url = format!(
            "{}/{}/oauth2/v2.0/token",
            AZURE_LOGIN_URL, self.credentials.tenant_id
        );

        let response = self
            .client
            .post(&token_url)
            .form(&[
                ("grant_type", "client_credentials"),
                ("client_id", &self.credentials.client_id),
                ("client_secret", &self.credentials.client_secret),
                ("scope", "https://management.azure.com/.default"),
            ])
            .send()
            .await
            .map_err(|e| DnsError::ApiError(format!("Token request failed: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let error = response.text().await.unwrap_or_default();
            return Err(DnsError::InvalidCredentials(format!(
                "Failed to get Azure access token for tenant {} (HTTP {}): {}",
                self.credentials.tenant_id,
                status,
                truncate_error_body(&error)
            )));
        }

        let token_response: TokenResponse = response
            .json()
            .await
            .map_err(|e| DnsError::ApiError(format!("Failed to parse token response: {}", e)))?;

        // Cache the token
        {
            let mut token = self.access_token.write().await;
            *token = Some(token_response.access_token.clone());
        }

        Ok(token_response.access_token)
    }

    /// Absolute URL of an Azure DNS management path.
    fn api_url(&self, path: &str) -> String {
        format!(
            "{}{}?api-version={}",
            self.base_url, path, AZURE_DNS_API_VERSION
        )
    }

    /// `/subscriptions/.../dnsZones/{zone}` for this provider's resource group.
    fn zone_path(&self, domain: &str) -> String {
        format!(
            "/subscriptions/{}/resourceGroups/{}/providers/Microsoft.Network/dnsZones/{}",
            self.credentials.subscription_id, self.credentials.resource_group, domain
        )
    }

    /// Path of one record set (`.../dnsZones/{zone}/{TYPE}/{relativeName}`).
    fn record_set_path(
        &self,
        domain: &str,
        record_type: DnsRecordType,
        relative_name: &str,
    ) -> String {
        format!(
            "{}/{}/{}",
            self.zone_path(domain),
            Self::azure_record_type(record_type),
            relative_name
        )
    }

    /// Azure's relative record-set name for a temps record name: `@` for the
    /// zone apex (`@` or empty), otherwise the name without a trailing dot.
    ///
    /// The name becomes one URL path segment, so a name that would address a
    /// different resource (`/`, `?`, `#`, `%`, `\`, whitespace, controls) is
    /// refused instead of sent.
    fn relative_record_set_name(domain: &str, name: &str) -> Result<String, DnsError> {
        let trimmed = name.trim_end_matches('.');
        if trimmed.is_empty() || trimmed == "@" {
            return Ok("@".to_string());
        }
        if trimmed.chars().any(|c| {
            matches!(c, '/' | '?' | '#' | '%' | '\\') || c.is_whitespace() || c.is_control()
        }) {
            return Err(DnsError::Validation(format!(
                "DNS record name '{name}' in zone {domain} cannot be addressed through the Azure DNS API: it contains a character that is not valid in a record set name"
            )));
        }
        Ok(trimmed.to_string())
    }

    /// Send one authenticated request and return its status and body,
    /// whatever the status.
    ///
    /// `create_only` adds `If-None-Match: *`, which makes Azure refuse
    /// (HTTP 412) to replace a record set that already exists.
    async fn send<B: Serialize + ?Sized>(
        &self,
        method: Method,
        url: &str,
        body: Option<&B>,
        create_only: bool,
    ) -> Result<AzureResponse, DnsError> {
        let token = self.get_access_token().await?;
        let path = url.split('?').next().unwrap_or(url);

        debug!("Azure DNS API request: {} {}", method, path);

        let mut request = self
            .client
            .request(method.clone(), url)
            .header("Authorization", format!("Bearer {}", token));
        if create_only {
            request = request.header("If-None-Match", "*");
        }
        if let Some(body) = body {
            request = request.json(body);
        }

        let response = request.send().await.map_err(|e| {
            DnsError::ApiError(format!("Azure DNS API {method} {path} failed: {e}"))
        })?;
        let status = response.status();
        let body = response.text().await.map_err(|e| {
            DnsError::ApiError(format!(
                "Failed to read Azure DNS API response for {method} {path} (HTTP {status}): {e}"
            ))
        })?;
        Ok(AzureResponse { status, body })
    }

    /// Error for a non-success response: status, operation, bounded body.
    fn status_error(method: &Method, path: &str, response: &AzureResponse) -> DnsError {
        DnsError::ApiError(format!(
            "Azure API returned status {} for {} {}: {}",
            response.status,
            method,
            path,
            truncate_error_body(&response.body)
        ))
    }

    /// Parse a JSON response body (an empty body parses as `{}`).
    fn parse_body<T: DeserializeOwned>(
        method: &Method,
        path: &str,
        body: &str,
    ) -> Result<T, DnsError> {
        let body = if body.is_empty() { "{}" } else { body };
        serde_json::from_str(body).map_err(|e| {
            DnsError::ApiError(format!(
                "Failed to parse Azure DNS API response for {} {}: {} - Body: {}",
                method,
                path,
                e,
                truncate_error_body(body)
            ))
        })
    }

    /// Make an authenticated request to Azure DNS API
    async fn api_request<T: DeserializeOwned, B: Serialize + ?Sized>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
    ) -> Result<T, DnsError> {
        let response = self
            .send(method.clone(), &self.api_url(path), body, false)
            .await?;
        if !response.status.is_success() {
            return Err(Self::status_error(&method, path, &response));
        }
        Self::parse_body(&method, path, &response.body)
    }

    /// Make a DELETE request (returns no body)
    async fn api_delete(&self, path: &str) -> Result<(), DnsError> {
        let response = self
            .send(Method::DELETE, &self.api_url(path), None::<&()>, false)
            .await?;
        if !response.status.is_success() {
            return Err(Self::status_error(&Method::DELETE, path, &response));
        }
        Ok(())
    }

    /// Validate an Azure `nextLink` before following it.
    ///
    /// Following the link sends the bearer token, so it must point at the
    /// same origin (scheme, host, port) as the configured API endpoint and
    /// carry no userinfo; anything else is refused. A link without an
    /// `api-version` gets ours added.
    fn next_link_url(&self, link: &str, context: &str) -> Result<Url, DnsError> {
        let base = Url::parse(&self.base_url).map_err(|e| {
            DnsError::ApiError(format!(
                "{context}: configured Azure endpoint '{}' is not a valid URL: {e}",
                self.base_url
            ))
        })?;
        let mut next = Url::parse(link).map_err(|e| {
            DnsError::ApiError(format!(
                "{context}: Azure returned a nextLink that is not a valid absolute URL: {e}"
            ))
        })?;
        if next.origin() != base.origin()
            || !next.username().is_empty()
            || next.password().is_some()
        {
            return Err(DnsError::ApiError(format!(
                "{context}: Azure returned a nextLink on origin {} instead of the configured endpoint {}; refusing to send credentials to it",
                next.origin().ascii_serialization(),
                base.origin().ascii_serialization()
            )));
        }
        if !next.query_pairs().any(|(key, _)| key == "api-version") {
            next.query_pairs_mut()
                .append_pair("api-version", AZURE_DNS_API_VERSION);
        }
        Ok(next)
    }

    /// GET a page addressed by a `nextLink` (see [`Self::next_link_url`]).
    async fn get_next_page<T: DeserializeOwned>(
        &self,
        link: &str,
        context: &str,
    ) -> Result<ListPage<T>, DnsError> {
        let url = self.next_link_url(link, context)?;
        let path = url.path().to_string();
        let response = self
            .send(Method::GET, url.as_str(), None::<&()>, false)
            .await?;
        if !response.status.is_success() {
            return Err(Self::status_error(&Method::GET, &path, &response));
        }
        Self::parse_body(&Method::GET, &path, &response.body)
    }

    /// Read every page of an Azure list endpoint, following `nextLink`.
    ///
    /// Never returns a partial list: reaching the page cap, a repeated
    /// `nextLink`, an empty page that still has a `nextLink`, or a `nextLink`
    /// on another origin is an error.
    async fn list_all<T: DeserializeOwned>(
        &self,
        path: &str,
        context: &str,
    ) -> Result<Vec<T>, DnsError> {
        let mut items = Vec::new();
        let mut seen_links: HashSet<String> = HashSet::new();
        let mut page: ListPage<T> = self.api_request(Method::GET, path, None::<&()>).await?;
        for pages_read in 1..=self.max_pages {
            let page_len = page.value.len();
            items.extend(page.value);
            let next_link = match page.next_link {
                Some(link) if !link.trim().is_empty() => link,
                _ => return Ok(items),
            };
            if page_len == 0 {
                return Err(DnsError::ApiError(format!(
                    "{context}: Azure returned empty page {pages_read} with a nextLink; refusing a partial result"
                )));
            }
            if !seen_links.insert(next_link.clone()) {
                return Err(DnsError::ApiError(format!(
                    "{context}: Azure repeated a nextLink after {pages_read} page(s); refusing a partial result"
                )));
            }
            if pages_read == self.max_pages {
                break;
            }
            page = self.get_next_page(&next_link, context).await?;
        }
        Err(DnsError::ApiError(format!(
            "{context} exceeded {} pages; refusing a partial result",
            self.max_pages
        )))
    }

    /// GET one record set directly; `Ok(None)` when Azure reports that it
    /// does not exist. A 404 caused by a missing zone, resource group or
    /// subscription is `ZoneNotFound`, never "absent".
    async fn get_record_set(
        &self,
        domain: &str,
        path: &str,
    ) -> Result<Option<AzureRecordSet>, DnsError> {
        let response = self
            .send(Method::GET, &self.api_url(path), None::<&()>, false)
            .await?;
        if response.status == StatusCode::NOT_FOUND {
            return match azure_error_code(&response.body).as_deref() {
                Some(
                    code @ ("ParentResourceNotFound"
                    | "ResourceGroupNotFound"
                    | "SubscriptionNotFound"),
                ) => Err(DnsError::ZoneNotFound(format!(
                    "{domain} (Azure DNS answered {code} for {path})"
                ))),
                _ => Ok(None),
            };
        }
        if !response.status.is_success() {
            return Err(Self::status_error(&Method::GET, path, &response));
        }
        Self::parse_body(&Method::GET, path, &response.body).map(Some)
    }

    /// PUT one record set built from `request`.
    ///
    /// With `create_only` the write carries `If-None-Match: *`, so Azure
    /// refuses to replace an existing record set and the refusal (HTTP 412)
    /// surfaces as [`DnsError::RecordConflict`]. Without it the PUT is
    /// Azure's create-or-replace.
    async fn put_record_set(
        &self,
        domain: &str,
        request: &DnsRecordRequest,
        create_only: bool,
    ) -> Result<DnsRecord, DnsError> {
        let record_type = request.content.record_type();
        let relative_name = Self::relative_record_set_name(domain, &request.name)?;
        let path = self.record_set_path(domain, record_type, &relative_name);
        let record_set = Self::build_record_set(request);

        let response = self
            .send(
                Method::PUT,
                &self.api_url(&path),
                Some(&record_set),
                create_only,
            )
            .await?;
        if create_only && response.status == StatusCode::PRECONDITION_FAILED {
            return Err(DnsError::RecordConflict {
                domain: domain.to_string(),
                name: request.name.clone(),
                record_type: record_type.to_string(),
                reason: "an Azure DNS record set with this name and type already exists at the provider, and a create never replaces one (Azure refused the create-only write with HTTP 412)".to_string(),
            });
        }
        if !response.status.is_success() {
            return Err(Self::status_error(&Method::PUT, &path, &response));
        }

        let written: AzureRecordSet = Self::parse_body(&Method::PUT, &path, &response.body)?;
        Self::convert_record_set(&written, domain)
            .into_iter()
            .next()
            .ok_or_else(|| {
                DnsError::ApiError(format!(
                    "Azure DNS returned no {record_type} values for record set '{relative_name}' in zone {domain} after writing it"
                ))
            })
    }

    /// Get the record type string for Azure API
    fn azure_record_type(record_type: DnsRecordType) -> &'static str {
        match record_type {
            DnsRecordType::A => "A",
            DnsRecordType::AAAA => "AAAA",
            DnsRecordType::CNAME => "CNAME",
            DnsRecordType::TXT => "TXT",
            DnsRecordType::MX => "MX",
            DnsRecordType::NS => "NS",
            DnsRecordType::SRV => "SRV",
            DnsRecordType::CAA => "CAA",
            DnsRecordType::PTR => "PTR",
        }
    }

    /// Parse Azure record type string
    fn parse_record_type(type_str: &str) -> Option<DnsRecordType> {
        match type_str.to_uppercase().as_str() {
            "A" | "MICROSOFT.NETWORK/DNSZONES/A" => Some(DnsRecordType::A),
            "AAAA" | "MICROSOFT.NETWORK/DNSZONES/AAAA" => Some(DnsRecordType::AAAA),
            "CNAME" | "MICROSOFT.NETWORK/DNSZONES/CNAME" => Some(DnsRecordType::CNAME),
            "TXT" | "MICROSOFT.NETWORK/DNSZONES/TXT" => Some(DnsRecordType::TXT),
            "MX" | "MICROSOFT.NETWORK/DNSZONES/MX" => Some(DnsRecordType::MX),
            "NS" | "MICROSOFT.NETWORK/DNSZONES/NS" => Some(DnsRecordType::NS),
            "SRV" | "MICROSOFT.NETWORK/DNSZONES/SRV" => Some(DnsRecordType::SRV),
            "CAA" | "MICROSOFT.NETWORK/DNSZONES/CAA" => Some(DnsRecordType::CAA),
            "PTR" | "MICROSOFT.NETWORK/DNSZONES/PTR" => Some(DnsRecordType::PTR),
            _ => None,
        }
    }

    /// Convert Azure record set to our DnsRecord type
    fn convert_record_set(record_set: &AzureRecordSet, zone_name: &str) -> Vec<DnsRecord> {
        let record_type_str = record_set.record_type.as_deref().unwrap_or("");

        let record_type = match Self::parse_record_type(record_type_str) {
            Some(t) => t,
            None => return vec![],
        };

        let name = if record_set.name == "@" {
            "@".to_string()
        } else {
            record_set.name.clone()
        };

        let fqdn = if name == "@" {
            zone_name.to_string()
        } else {
            format!("{}.{}", name, zone_name)
        };

        let ttl = record_set.properties.ttl.unwrap_or(3600);

        let mut records = vec![];

        // Convert based on record type
        match record_type {
            DnsRecordType::A => {
                if let Some(ref a_records) = record_set.properties.a_records {
                    for a in a_records {
                        records.push(DnsRecord {
                            id: Some(format!("{}::A", name)),
                            zone: zone_name.to_string(),
                            name: name.clone(),
                            fqdn: fqdn.clone(),
                            content: DnsRecordContent::A {
                                address: a.ipv4_address.clone(),
                            },
                            ttl,
                            proxied: false,
                            metadata: HashMap::new(),
                        });
                    }
                }
            }
            DnsRecordType::AAAA => {
                if let Some(ref aaaa_records) = record_set.properties.aaaa_records {
                    for aaaa in aaaa_records {
                        records.push(DnsRecord {
                            id: Some(format!("{}::AAAA", name)),
                            zone: zone_name.to_string(),
                            name: name.clone(),
                            fqdn: fqdn.clone(),
                            content: DnsRecordContent::AAAA {
                                address: aaaa.ipv6_address.clone(),
                            },
                            ttl,
                            proxied: false,
                            metadata: HashMap::new(),
                        });
                    }
                }
            }
            DnsRecordType::CNAME => {
                if let Some(ref cname) = record_set.properties.cname_record {
                    records.push(DnsRecord {
                        id: Some(format!("{}::CNAME", name)),
                        zone: zone_name.to_string(),
                        name: name.clone(),
                        fqdn: fqdn.clone(),
                        content: DnsRecordContent::CNAME {
                            target: cname.cname.trim_end_matches('.').to_string(),
                        },
                        ttl,
                        proxied: false,
                        metadata: HashMap::new(),
                    });
                }
            }
            DnsRecordType::TXT => {
                if let Some(ref txt_records) = record_set.properties.txt_records {
                    for txt in txt_records {
                        let content = txt.value.join("");
                        records.push(DnsRecord {
                            id: Some(format!("{}::TXT", name)),
                            zone: zone_name.to_string(),
                            name: name.clone(),
                            fqdn: fqdn.clone(),
                            content: DnsRecordContent::TXT { content },
                            ttl,
                            proxied: false,
                            metadata: HashMap::new(),
                        });
                    }
                }
            }
            DnsRecordType::MX => {
                if let Some(ref mx_records) = record_set.properties.mx_records {
                    for mx in mx_records {
                        records.push(DnsRecord {
                            id: Some(format!("{}::MX", name)),
                            zone: zone_name.to_string(),
                            name: name.clone(),
                            fqdn: fqdn.clone(),
                            content: DnsRecordContent::MX {
                                priority: mx.preference,
                                target: mx.exchange.trim_end_matches('.').to_string(),
                            },
                            ttl,
                            proxied: false,
                            metadata: HashMap::new(),
                        });
                    }
                }
            }
            DnsRecordType::NS => {
                if let Some(ref ns_records) = record_set.properties.ns_records {
                    for ns in ns_records {
                        records.push(DnsRecord {
                            id: Some(format!("{}::NS", name)),
                            zone: zone_name.to_string(),
                            name: name.clone(),
                            fqdn: fqdn.clone(),
                            content: DnsRecordContent::NS {
                                nameserver: ns.nsdname.trim_end_matches('.').to_string(),
                            },
                            ttl,
                            proxied: false,
                            metadata: HashMap::new(),
                        });
                    }
                }
            }
            DnsRecordType::SRV => {
                if let Some(ref srv_records) = record_set.properties.srv_records {
                    for srv in srv_records {
                        records.push(DnsRecord {
                            id: Some(format!("{}::SRV", name)),
                            zone: zone_name.to_string(),
                            name: name.clone(),
                            fqdn: fqdn.clone(),
                            content: DnsRecordContent::SRV {
                                priority: srv.priority,
                                weight: srv.weight,
                                port: srv.port,
                                target: srv.target.trim_end_matches('.').to_string(),
                            },
                            ttl,
                            proxied: false,
                            metadata: HashMap::new(),
                        });
                    }
                }
            }
            DnsRecordType::CAA => {
                if let Some(ref caa_records) = record_set.properties.caa_records {
                    for caa in caa_records {
                        records.push(DnsRecord {
                            id: Some(format!("{}::CAA", name)),
                            zone: zone_name.to_string(),
                            name: name.clone(),
                            fqdn: fqdn.clone(),
                            content: DnsRecordContent::CAA {
                                flags: caa.flags,
                                tag: caa.tag.clone(),
                                value: caa.value.clone(),
                            },
                            ttl,
                            proxied: false,
                            metadata: HashMap::new(),
                        });
                    }
                }
            }
            DnsRecordType::PTR => {
                if let Some(ref ptr_records) = record_set.properties.ptr_records {
                    for ptr in ptr_records {
                        records.push(DnsRecord {
                            id: Some(format!("{}::PTR", name)),
                            zone: zone_name.to_string(),
                            name: name.clone(),
                            fqdn: fqdn.clone(),
                            content: DnsRecordContent::PTR {
                                target: ptr.ptrdname.trim_end_matches('.').to_string(),
                            },
                            ttl,
                            proxied: false,
                            metadata: HashMap::new(),
                        });
                    }
                }
            }
        }

        records
    }

    /// Build Azure record set from our request
    fn build_record_set(request: &DnsRecordRequest) -> AzureRecordSet {
        let record_type = request.content.record_type();
        let ttl = request.ttl.unwrap_or(3600);

        let mut properties = RecordSetProperties {
            ttl: Some(ttl),
            a_records: None,
            aaaa_records: None,
            cname_record: None,
            txt_records: None,
            mx_records: None,
            ns_records: None,
            srv_records: None,
            caa_records: None,
            ptr_records: None,
        };

        match &request.content {
            DnsRecordContent::A { address } => {
                properties.a_records = Some(vec![ARecord {
                    ipv4_address: address.clone(),
                }]);
            }
            DnsRecordContent::AAAA { address } => {
                properties.aaaa_records = Some(vec![AAAARecord {
                    ipv6_address: address.clone(),
                }]);
            }
            DnsRecordContent::CNAME { target } => {
                properties.cname_record = Some(CNAMERecord {
                    cname: target.clone(),
                });
            }
            DnsRecordContent::TXT { content } => {
                // Azure TXT records have a max of 255 chars per string
                let chunks: Vec<String> = content
                    .as_bytes()
                    .chunks(255)
                    .map(|chunk| String::from_utf8_lossy(chunk).to_string())
                    .collect();
                properties.txt_records = Some(vec![TXTRecord { value: chunks }]);
            }
            DnsRecordContent::MX { priority, target } => {
                properties.mx_records = Some(vec![MXRecord {
                    preference: *priority,
                    exchange: target.clone(),
                }]);
            }
            DnsRecordContent::NS { nameserver } => {
                properties.ns_records = Some(vec![NSRecord {
                    nsdname: nameserver.clone(),
                }]);
            }
            DnsRecordContent::SRV {
                priority,
                weight,
                port,
                target,
            } => {
                properties.srv_records = Some(vec![SRVRecord {
                    priority: *priority,
                    weight: *weight,
                    port: *port,
                    target: target.clone(),
                }]);
            }
            DnsRecordContent::CAA { flags, tag, value } => {
                properties.caa_records = Some(vec![CAARecord {
                    flags: *flags,
                    tag: tag.clone(),
                    value: value.clone(),
                }]);
            }
            DnsRecordContent::PTR { target } => {
                properties.ptr_records = Some(vec![PTRRecord {
                    ptrdname: target.clone(),
                }]);
            }
        }

        AzureRecordSet {
            id: None,
            name: request.name.clone(),
            record_type: Some(Self::azure_record_type(record_type).to_string()),
            properties,
        }
    }
}

#[async_trait]
impl DnsProvider for AzureProvider {
    fn provider_type(&self) -> DnsProviderType {
        DnsProviderType::Azure
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
                info!("Azure DNS API connection test successful");
                Ok(true)
            }
            Err(e) => {
                warn!("Azure DNS API connection test failed: {}", e);
                Ok(false)
            }
        }
    }

    async fn list_zones(&self) -> Result<Vec<DnsZone>, DnsError> {
        let path = format!(
            "/subscriptions/{}/resourceGroups/{}/providers/Microsoft.Network/dnsZones",
            self.credentials.subscription_id, self.credentials.resource_group
        );
        let context = format!(
            "Azure DNS zone listing for resource group {}",
            self.credentials.resource_group
        );
        let zones: Vec<AzureZone> = self.list_all(&path, &context).await?;

        Ok(zones
            .into_iter()
            .map(|zone| DnsZone {
                id: zone.id,
                name: zone.name,
                status: "active".to_string(),
                nameservers: zone.properties.name_servers,
                metadata: HashMap::new(),
            })
            .collect())
    }

    async fn get_zone(&self, domain: &str) -> Result<Option<DnsZone>, DnsError> {
        let zones = self.list_zones().await?;
        Ok(zones.into_iter().find(|z| z.name == domain))
    }

    async fn list_records(&self, domain: &str) -> Result<Vec<DnsRecord>, DnsError> {
        let path = format!("{}/recordsets", self.zone_path(domain));
        let context = format!("Azure DNS record listing for zone {domain}");
        let record_sets: Vec<AzureRecordSet> = self.list_all(&path, &context).await?;

        Ok(record_sets
            .iter()
            .flat_map(|rs| Self::convert_record_set(rs, domain))
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

    /// Every value of the (name, type) record set, read with one direct GET
    /// of that record set instead of a zone listing.
    ///
    /// A record set that exists but yields no value temps can represent (an
    /// alias record set pointing at an Azure resource, or an empty one) is a
    /// conflict, never "absent": treating it as absent would let a write
    /// target a name that is already in use.
    async fn get_records(
        &self,
        domain: &str,
        name: &str,
        record_type: DnsRecordType,
    ) -> Result<Vec<DnsRecord>, DnsError> {
        let relative_name = Self::relative_record_set_name(domain, name)?;
        let path = self.record_set_path(domain, record_type, &relative_name);
        let Some(mut record_set) = self.get_record_set(domain, &path).await? else {
            return Ok(Vec::new());
        };

        // The GET addressed exactly this (name, type); any other answer is an
        // API anomaly that must not be interpreted as "absent". A missing
        // `type` is the one addressed by the path.
        let returned_type = record_set
            .record_type
            .as_deref()
            .map(Self::parse_record_type);
        let type_matches = match returned_type {
            None => {
                record_set.record_type = Some(Self::azure_record_type(record_type).to_string());
                true
            }
            Some(parsed) => parsed == Some(record_type),
        };
        if !type_matches || !dns_names_equal(&record_set.name, &relative_name) {
            return Err(DnsError::ApiError(format!(
                "Azure DNS returned record set '{}' (type {}) when asked for {record_type} '{relative_name}' in zone {domain}",
                record_set.name,
                record_set.record_type.as_deref().unwrap_or("missing")
            )));
        }

        let records = Self::convert_record_set(&record_set, domain);
        if records.is_empty() {
            return Err(DnsError::RecordConflict {
                domain: domain.to_string(),
                name: name.to_string(),
                record_type: record_type.to_string(),
                reason: "Azure DNS has a record set at this name and type with no value temps can read (for example an alias record set pointing at an Azure resource), so temps will not manage it".to_string(),
            });
        }
        Ok(records)
    }

    /// Create-only: fails with [`DnsError::RecordConflict`] when the record
    /// set already exists, instead of replacing it.
    async fn create_record(
        &self,
        domain: &str,
        request: DnsRecordRequest,
    ) -> Result<DnsRecord, DnsError> {
        let record = self.put_record_set(domain, &request, true).await?;

        info!("Created DNS record {} for domain {}", request.name, domain);

        Ok(record)
    }

    /// Create-or-replace of the whole (name, type) record set.
    async fn update_record(
        &self,
        domain: &str,
        _record_id: &str,
        request: DnsRecordRequest,
    ) -> Result<DnsRecord, DnsError> {
        let record = self.put_record_set(domain, &request, false).await?;

        info!("Updated DNS record {} for domain {}", request.name, domain);

        Ok(record)
    }

    async fn delete_record(&self, domain: &str, record_id: &str) -> Result<(), DnsError> {
        // record_id format: "name::TYPE"
        let parts: Vec<&str> = record_id.split("::").collect();
        if parts.len() != 2 {
            return Err(DnsError::Validation(format!(
                "Invalid record ID format: {}. Expected 'name::TYPE'",
                record_id
            )));
        }

        let relative_name = Self::relative_record_set_name(domain, parts[0])?;
        let record_type = Self::parse_record_type(parts[1]).ok_or_else(|| {
            DnsError::Validation(format!(
                "Invalid record ID {record_id} for zone {domain}: '{}' is not a supported record type",
                parts[1]
            ))
        })?;

        let path = self.record_set_path(domain, record_type, &relative_name);

        self.api_delete(&path).await?;

        info!("Deleted DNS record {} from domain {}", record_id, domain);

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_azure_record_type() {
        assert_eq!(AzureProvider::azure_record_type(DnsRecordType::A), "A");
        assert_eq!(
            AzureProvider::azure_record_type(DnsRecordType::AAAA),
            "AAAA"
        );
        assert_eq!(
            AzureProvider::azure_record_type(DnsRecordType::CNAME),
            "CNAME"
        );
        assert_eq!(AzureProvider::azure_record_type(DnsRecordType::TXT), "TXT");
        assert_eq!(AzureProvider::azure_record_type(DnsRecordType::MX), "MX");
        assert_eq!(AzureProvider::azure_record_type(DnsRecordType::NS), "NS");
        assert_eq!(AzureProvider::azure_record_type(DnsRecordType::SRV), "SRV");
        assert_eq!(AzureProvider::azure_record_type(DnsRecordType::CAA), "CAA");
        assert_eq!(AzureProvider::azure_record_type(DnsRecordType::PTR), "PTR");
    }

    #[test]
    fn test_parse_record_type() {
        assert_eq!(
            AzureProvider::parse_record_type("A"),
            Some(DnsRecordType::A)
        );
        assert_eq!(
            AzureProvider::parse_record_type("Microsoft.Network/dnszones/A"),
            Some(DnsRecordType::A)
        );
        assert_eq!(
            AzureProvider::parse_record_type("TXT"),
            Some(DnsRecordType::TXT)
        );
        assert_eq!(AzureProvider::parse_record_type("UNKNOWN"), None);
    }

    #[test]
    fn test_build_record_set_a() {
        let request = DnsRecordRequest {
            name: "www".to_string(),
            content: DnsRecordContent::A {
                address: "192.0.2.1".to_string(),
            },
            ttl: Some(300),
            proxied: false,
        };

        let record_set = AzureProvider::build_record_set(&request);

        assert_eq!(record_set.name, "www");
        assert_eq!(record_set.properties.ttl, Some(300));
        assert!(record_set.properties.a_records.is_some());
        assert_eq!(
            record_set.properties.a_records.as_ref().unwrap()[0].ipv4_address,
            "192.0.2.1"
        );
    }

    #[test]
    fn test_build_record_set_txt() {
        let request = DnsRecordRequest {
            name: "_acme-challenge".to_string(),
            content: DnsRecordContent::TXT {
                content: "verification-token".to_string(),
            },
            ttl: Some(60),
            proxied: false,
        };

        let record_set = AzureProvider::build_record_set(&request);

        assert!(record_set.properties.txt_records.is_some());
        let txt = &record_set.properties.txt_records.as_ref().unwrap()[0];
        assert_eq!(txt.value, vec!["verification-token"]);
    }

    #[test]
    fn test_build_record_set_mx() {
        let request = DnsRecordRequest {
            name: "@".to_string(),
            content: DnsRecordContent::MX {
                priority: 10,
                target: "mail.example.com".to_string(),
            },
            ttl: Some(3600),
            proxied: false,
        };

        let record_set = AzureProvider::build_record_set(&request);

        assert!(record_set.properties.mx_records.is_some());
        let mx = &record_set.properties.mx_records.as_ref().unwrap()[0];
        assert_eq!(mx.preference, 10);
        assert_eq!(mx.exchange, "mail.example.com");
    }

    #[test]
    fn test_build_record_set_cname() {
        let request = DnsRecordRequest {
            name: "www".to_string(),
            content: DnsRecordContent::CNAME {
                target: "example.com".to_string(),
            },
            ttl: Some(300),
            proxied: false,
        };

        let record_set = AzureProvider::build_record_set(&request);

        assert!(record_set.properties.cname_record.is_some());
        assert_eq!(
            record_set.properties.cname_record.as_ref().unwrap().cname,
            "example.com"
        );
    }

    #[test]
    fn test_convert_record_set_a() {
        let azure_record = AzureRecordSet {
            id: Some("/subscriptions/xxx/resourceGroups/xxx/providers/Microsoft.Network/dnsZones/example.com/A/www".to_string()),
            name: "www".to_string(),
            record_type: Some("Microsoft.Network/dnszones/A".to_string()),
            properties: RecordSetProperties {
                ttl: Some(300),
                a_records: Some(vec![ARecord {
                    ipv4_address: "192.0.2.1".to_string(),
                }]),
                aaaa_records: None,
                cname_record: None,
                txt_records: None,
                mx_records: None,
                ns_records: None,
                srv_records: None,
                caa_records: None,
                ptr_records: None,
            },
        };

        let records = AzureProvider::convert_record_set(&azure_record, "example.com");

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

    #[test]
    fn test_convert_record_set_apex() {
        let azure_record = AzureRecordSet {
            id: None,
            name: "@".to_string(),
            record_type: Some("A".to_string()),
            properties: RecordSetProperties {
                ttl: Some(300),
                a_records: Some(vec![ARecord {
                    ipv4_address: "192.0.2.1".to_string(),
                }]),
                aaaa_records: None,
                cname_record: None,
                txt_records: None,
                mx_records: None,
                ns_records: None,
                srv_records: None,
                caa_records: None,
                ptr_records: None,
            },
        };

        let records = AzureProvider::convert_record_set(&azure_record, "example.com");

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].name, "@");
        assert_eq!(records[0].fqdn, "example.com");
    }

    #[test]
    fn test_convert_record_set_txt() {
        let azure_record = AzureRecordSet {
            id: None,
            name: "@".to_string(),
            record_type: Some("TXT".to_string()),
            properties: RecordSetProperties {
                ttl: Some(3600),
                a_records: None,
                aaaa_records: None,
                cname_record: None,
                txt_records: Some(vec![TXTRecord {
                    value: vec!["v=spf1 ".to_string(), "-all".to_string()],
                }]),
                mx_records: None,
                ns_records: None,
                srv_records: None,
                caa_records: None,
                ptr_records: None,
            },
        };

        let records = AzureProvider::convert_record_set(&azure_record, "example.com");

        assert_eq!(records.len(), 1);
        if let DnsRecordContent::TXT { content } = &records[0].content {
            assert_eq!(content, "v=spf1 -all");
        } else {
            panic!("Expected TXT record");
        }
    }

    #[test]
    fn test_convert_record_set_mx() {
        let azure_record = AzureRecordSet {
            id: None,
            name: "@".to_string(),
            record_type: Some("MX".to_string()),
            properties: RecordSetProperties {
                ttl: Some(3600),
                a_records: None,
                aaaa_records: None,
                cname_record: None,
                txt_records: None,
                mx_records: Some(vec![MXRecord {
                    preference: 10,
                    exchange: "mail.example.com.".to_string(),
                }]),
                ns_records: None,
                srv_records: None,
                caa_records: None,
                ptr_records: None,
            },
        };

        let records = AzureProvider::convert_record_set(&azure_record, "example.com");

        assert_eq!(records.len(), 1);
        if let DnsRecordContent::MX { priority, target } = &records[0].content {
            assert_eq!(*priority, 10);
            assert_eq!(target, "mail.example.com");
        } else {
            panic!("Expected MX record");
        }
    }

    #[test]
    fn test_convert_record_set_multiple_values() {
        let azure_record = AzureRecordSet {
            id: None,
            name: "@".to_string(),
            record_type: Some("A".to_string()),
            properties: RecordSetProperties {
                ttl: Some(300),
                a_records: Some(vec![
                    ARecord {
                        ipv4_address: "192.0.2.1".to_string(),
                    },
                    ARecord {
                        ipv4_address: "192.0.2.2".to_string(),
                    },
                ]),
                aaaa_records: None,
                cname_record: None,
                txt_records: None,
                mx_records: None,
                ns_records: None,
                srv_records: None,
                caa_records: None,
                ptr_records: None,
            },
        };

        let records = AzureProvider::convert_record_set(&azure_record, "example.com");

        assert_eq!(records.len(), 2);
    }

    #[test]
    fn test_default_ttl() {
        let request = DnsRecordRequest {
            name: "www".to_string(),
            content: DnsRecordContent::A {
                address: "192.0.2.1".to_string(),
            },
            ttl: None,
            proxied: false,
        };

        let record_set = AzureProvider::build_record_set(&request);

        assert_eq!(record_set.properties.ttl, Some(3600)); // Default TTL
    }

    fn credentials() -> AzureCredentials {
        AzureCredentials {
            tenant_id: "test-tenant-id".to_string(),
            client_id: "test-client-id".to_string(),
            client_secret: "test-client-secret".to_string(),
            subscription_id: "test-subscription-id".to_string(),
            resource_group: "test-resource-group".to_string(),
        }
    }

    #[test]
    fn relative_record_set_name_maps_apex_and_refuses_path_breaking_names() {
        assert_eq!(
            AzureProvider::relative_record_set_name("example.com", "@").unwrap(),
            "@"
        );
        assert_eq!(
            AzureProvider::relative_record_set_name("example.com", "").unwrap(),
            "@"
        );
        assert_eq!(
            AzureProvider::relative_record_set_name("example.com", "www.").unwrap(),
            "www"
        );
        assert_eq!(
            AzureProvider::relative_record_set_name("example.com", "*.preview").unwrap(),
            "*.preview"
        );
        for name in ["a/b", "a?b", "a#b", "a%2Fb", "a b", "a\\b"] {
            assert!(
                matches!(
                    AzureProvider::relative_record_set_name("example.com", name),
                    Err(DnsError::Validation(_))
                ),
                "{name} must be refused"
            );
        }
    }

    #[test]
    fn next_link_must_stay_on_the_configured_origin() {
        let provider = AzureProvider::with_test_token(
            credentials(),
            "https://management.example.com".to_string(),
            "token".to_string(),
        )
        .unwrap();

        let same = provider
            .next_link_url(
                "https://management.example.com/zones?api-version=2018-05-01&$skipToken=abc",
                "test",
            )
            .unwrap();
        assert_eq!(
            same.as_str(),
            "https://management.example.com/zones?api-version=2018-05-01&$skipToken=abc"
        );

        // The default port is the same origin.
        assert!(provider
            .next_link_url("https://management.example.com:443/zones", "test")
            .is_ok());

        for foreign in [
            "https://attacker.example.net/zones?api-version=2018-05-01",
            "http://management.example.com/zones?api-version=2018-05-01",
            "https://management.example.com:8443/zones?api-version=2018-05-01",
            "https://user:pass@management.example.com/zones?api-version=2018-05-01",
            "/zones?api-version=2018-05-01",
        ] {
            assert!(
                matches!(
                    provider.next_link_url(foreign, "test"),
                    Err(DnsError::ApiError(_))
                ),
                "{foreign} must be refused"
            );
        }
    }

    #[test]
    fn next_link_without_api_version_gets_one() {
        let provider = AzureProvider::with_test_token(
            credentials(),
            "https://management.example.com".to_string(),
            "token".to_string(),
        )
        .unwrap();

        let url = provider
            .next_link_url(
                "https://management.example.com/zones?$skipToken=abc",
                "test",
            )
            .unwrap();
        assert!(url
            .query_pairs()
            .any(|(key, value)| key == "api-version" && value == AZURE_DNS_API_VERSION));
        assert!(url
            .query_pairs()
            .any(|(key, value)| key == "$skipToken" && value == "abc"));
    }

    #[test]
    fn azure_error_code_reads_nested_and_top_level_codes() {
        assert_eq!(
            azure_error_code(r#"{"error":{"code":"ParentResourceNotFound","message":"m"}}"#)
                .as_deref(),
            Some("ParentResourceNotFound")
        );
        assert_eq!(
            azure_error_code(r#"{"code":"NotFound","message":"m"}"#).as_deref(),
            Some("NotFound")
        );
        assert_eq!(azure_error_code("not json"), None);
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use serde_json::{json, Value};
    use wiremock::matchers::{any, header, method, path, query_param, query_param_is_missing};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const ZONE_PATH: &str = "/subscriptions/test-subscription-id/resourceGroups/test-resource-group/providers/Microsoft.Network/dnsZones/example.com";

    fn recordsets_path() -> String {
        format!("{ZONE_PATH}/recordsets")
    }

    fn a_record_set(name: &str, addresses: &[&str]) -> Value {
        json!({
            "id": format!("{ZONE_PATH}/A/{name}"),
            "name": name,
            "type": "Microsoft.Network/dnszones/A",
            "properties": {
                "TTL": 300,
                "ARecords": addresses
                    .iter()
                    .map(|address| json!({"ipv4Address": address}))
                    .collect::<Vec<_>>()
            }
        })
    }

    fn txt_record_set(name: &str, values: &[&str]) -> Value {
        json!({
            "id": format!("{ZONE_PATH}/TXT/{name}"),
            "name": name,
            "type": "Microsoft.Network/dnszones/TXT",
            "properties": {
                "TTL": 300,
                "TXTRecords": values
                    .iter()
                    .map(|value| json!({"value": [value]}))
                    .collect::<Vec<_>>()
            }
        })
    }

    /// A `nextLink` on `server` for the record-set listing.
    fn next_link(server: &MockServer, skip_token: &str) -> String {
        format!(
            "{}{}?api-version=2018-05-01&$skipToken={skip_token}",
            server.uri(),
            recordsets_path()
        )
    }

    async fn mount_first_page(server: &MockServer, body: Value) {
        Mock::given(method("GET"))
            .and(path(recordsets_path()))
            .and(query_param_is_missing("$skipToken"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(server)
            .await;
    }

    async fn mount_page(server: &MockServer, skip_token: &str, body: Value) {
        Mock::given(method("GET"))
            .and(path(recordsets_path()))
            .and(query_param("$skipToken", skip_token))
            .and(header("Authorization", "Bearer test-access-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
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
    async fn list_records_follows_next_link_across_pages() {
        let server = MockServer::start().await;
        mount_first_page(
            &server,
            json!({
                "value": [a_record_set("www", &["203.0.113.1"])],
                "nextLink": next_link(&server, "page2")
            }),
        )
        .await;
        mount_page(
            &server,
            "page2",
            json!({
                "value": [a_record_set("api", &["203.0.113.2"])],
                "nextLink": next_link(&server, "page3")
            }),
        )
        .await;
        // The registry TXT only exists on the last page: a one-page read
        // would report it absent.
        mount_page(
            &server,
            "page3",
            json!({"value": [txt_record_set("_temps-owned-a.app", &["marker"])]}),
        )
        .await;

        let provider = create_mock_provider(&server).await;
        let records = provider.list_records("example.com").await.unwrap();

        let names: Vec<&str> = records.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["www", "api", "_temps-owned-a.app"]);
    }

    #[tokio::test]
    async fn list_zones_follows_next_link() {
        let server = MockServer::start().await;
        let zones_path = "/subscriptions/test-subscription-id/resourceGroups/test-resource-group/providers/Microsoft.Network/dnsZones";
        let zone = |name: &str| json!({"id": format!("{zones_path}/{name}"), "name": name, "properties": {"nameServers": []}});
        Mock::given(method("GET"))
            .and(path(zones_path))
            .and(query_param_is_missing("$skipToken"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "value": [zone("example.com")],
                "nextLink": format!("{}{zones_path}?api-version=2018-05-01&$skipToken=z2", server.uri())
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(zones_path))
            .and(query_param("$skipToken", "z2"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"value": [zone("example.net")]})),
            )
            .mount(&server)
            .await;

        let provider = create_mock_provider(&server).await;
        let zones = provider.list_zones().await.unwrap();

        let names: Vec<&str> = zones.iter().map(|z| z.name.as_str()).collect();
        assert_eq!(names, vec!["example.com", "example.net"]);
        assert!(provider.get_zone("example.net").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn list_records_refuses_next_link_on_another_origin() {
        let server = MockServer::start().await;
        let foreign = MockServer::start().await;
        Mock::given(any())
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": []})))
            .expect(0)
            .mount(&foreign)
            .await;
        mount_first_page(
            &server,
            json!({
                "value": [a_record_set("www", &["203.0.113.1"])],
                "nextLink": next_link(&foreign, "page2")
            }),
        )
        .await;

        let provider = create_mock_provider(&server).await;
        let error = provider.list_records("example.com").await.unwrap_err();

        assert!(
            matches!(&error, DnsError::ApiError(message)
                if message.contains("refusing to send credentials")
                    && message.contains("example.com")),
            "unexpected error: {error}"
        );
        // The bearer token never reached the other origin.
        assert!(foreign.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn list_records_fails_closed_on_repeated_next_link() {
        let server = MockServer::start().await;
        mount_first_page(
            &server,
            json!({
                "value": [a_record_set("www", &["203.0.113.1"])],
                "nextLink": next_link(&server, "page2")
            }),
        )
        .await;
        mount_page(
            &server,
            "page2",
            json!({
                "value": [a_record_set("api", &["203.0.113.2"])],
                "nextLink": next_link(&server, "page2")
            }),
        )
        .await;

        let provider = create_mock_provider(&server).await;
        let error = provider.list_records("example.com").await.unwrap_err();

        assert!(
            matches!(&error, DnsError::ApiError(message) if message.contains("repeated a nextLink after 2 page(s)")),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn list_records_fails_closed_on_empty_page_with_next_link() {
        let server = MockServer::start().await;
        mount_first_page(
            &server,
            json!({"value": [], "nextLink": next_link(&server, "page2")}),
        )
        .await;

        let provider = create_mock_provider(&server).await;
        let error = provider.list_records("example.com").await.unwrap_err();

        assert!(
            matches!(&error, DnsError::ApiError(message) if message.contains("empty page 1 with a nextLink")),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn list_records_fails_closed_at_the_page_cap() {
        let server = MockServer::start().await;
        mount_first_page(
            &server,
            json!({"value": [a_record_set("a", &["203.0.113.1"])], "nextLink": next_link(&server, "p2")}),
        )
        .await;
        mount_page(
            &server,
            "p2",
            json!({"value": [a_record_set("b", &["203.0.113.2"])], "nextLink": next_link(&server, "p3")}),
        )
        .await;
        Mock::given(method("GET"))
            .and(query_param("$skipToken", "p3"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": []})))
            .expect(0)
            .mount(&server)
            .await;

        let mut provider = create_mock_provider(&server).await;
        provider.max_pages = 2;
        let error = provider.list_records("example.com").await.unwrap_err();

        assert!(
            matches!(&error, DnsError::ApiError(message)
                if message.contains("Azure DNS record listing for zone example.com exceeded 2 pages")),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn get_records_reads_the_record_set_directly_with_every_value() {
        let server = MockServer::start().await;
        // A full listing would only reach this record set on a later page;
        // the exact lookup must never fall back to paging the zone.
        Mock::given(method("GET"))
            .and(path(recordsets_path()))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": []})))
            .expect(0)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("{ZONE_PATH}/TXT/_temps-owned-a.app")))
            .and(header("Authorization", "Bearer test-access-token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(txt_record_set("_temps-owned-a.app", &["first", "second"])),
            )
            .mount(&server)
            .await;

        let provider = create_mock_provider(&server).await;
        let records = provider
            .get_records("example.com", "_temps-owned-a.app", DnsRecordType::TXT)
            .await
            .unwrap();

        let values: Vec<String> = records
            .iter()
            .map(|r| r.content.to_value_string())
            .collect();
        assert_eq!(values, vec!["first", "second"]);
    }

    #[tokio::test]
    async fn get_records_matches_names_case_insensitively() {
        let server = MockServer::start().await;
        // Azure resolves record-set names case-insensitively and answers
        // with the stored spelling.
        Mock::given(method("GET"))
            .and(path(format!("{ZONE_PATH}/A/App")))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(a_record_set("app", &["203.0.113.1", "203.0.113.2"])),
            )
            .mount(&server)
            .await;

        let provider = create_mock_provider(&server).await;
        let records = provider
            .get_records("example.com", "App", DnsRecordType::A)
            .await
            .unwrap();
        assert_eq!(records.len(), 2);

        let record = provider
            .get_record("example.com", "App", DnsRecordType::A)
            .await
            .unwrap();
        assert_eq!(record.map(|r| r.name), Some("app".to_string()));
    }

    #[tokio::test]
    async fn get_records_addresses_the_apex_as_at_sign() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("{ZONE_PATH}/A/@")))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(a_record_set("@", &["203.0.113.1"])),
            )
            .expect(2)
            .mount(&server)
            .await;

        let provider = create_mock_provider(&server).await;
        for apex in ["@", ""] {
            let records = provider
                .get_records("example.com", apex, DnsRecordType::A)
                .await
                .unwrap();
            assert_eq!(records.len(), 1);
            assert_eq!(records[0].fqdn, "example.com");
        }
    }

    #[tokio::test]
    async fn get_records_treats_a_missing_record_set_as_absent_but_a_missing_zone_as_an_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("{ZONE_PATH}/A/missing")))
            .respond_with(ResponseTemplate::new(404).set_body_json(json!({
                "code": "NotFound",
                "message": "The resource record 'missing' does not exist."
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/subscriptions/test-subscription-id/resourceGroups/test-resource-group/providers/Microsoft.Network/dnsZones/example.net/A/www"))
            .respond_with(ResponseTemplate::new(404).set_body_json(json!({
                "error": {"code": "ParentResourceNotFound", "message": "Parent resource 'example.net' not found."}
            })))
            .mount(&server)
            .await;

        let provider = create_mock_provider(&server).await;
        let absent = provider
            .get_records("example.com", "missing", DnsRecordType::A)
            .await
            .unwrap();
        assert!(absent.is_empty());

        let error = provider
            .get_records("example.net", "www", DnsRecordType::A)
            .await
            .unwrap_err();
        assert!(
            matches!(&error, DnsError::ZoneNotFound(message) if message.contains("example.net")),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn get_records_refuses_a_record_set_it_cannot_represent() {
        let server = MockServer::start().await;
        // An alias record set: it exists, but has no ARecords.
        Mock::given(method("GET"))
            .and(path(format!("{ZONE_PATH}/A/app")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "name": "app",
                "type": "Microsoft.Network/dnszones/A",
                "properties": {
                    "TTL": 300,
                    "targetResource": {"id": "/subscriptions/test-subscription-id/resourceGroups/test-resource-group/providers/Microsoft.Network/publicIPAddresses/ip"}
                }
            })))
            .mount(&server)
            .await;

        let provider = create_mock_provider(&server).await;
        let error = provider
            .get_records("example.com", "app", DnsRecordType::A)
            .await
            .unwrap_err();

        assert!(
            matches!(&error, DnsError::RecordConflict { name, record_type, .. }
                if name == "app" && record_type == "A"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn get_records_refuses_an_answer_for_a_different_record_set() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("{ZONE_PATH}/A/app")))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(a_record_set("other", &["203.0.113.1"])),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("{ZONE_PATH}/A/www")))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(txt_record_set("www", &["text"])),
            )
            .mount(&server)
            .await;

        let provider = create_mock_provider(&server).await;
        for name in ["app", "www"] {
            let error = provider
                .get_records("example.com", name, DnsRecordType::A)
                .await
                .unwrap_err();
            assert!(matches!(error, DnsError::ApiError(_)), "{name}: {error}");
        }
    }

    #[tokio::test]
    async fn get_records_reads_a_record_set_without_a_type_as_the_addressed_type() {
        let server = MockServer::start().await;
        let mut body = a_record_set("app", &["203.0.113.1"]);
        body.as_object_mut().map(|object| object.remove("type"));
        Mock::given(method("GET"))
            .and(path(format!("{ZONE_PATH}/A/app")))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        let provider = create_mock_provider(&server).await;
        let records = provider
            .get_records("example.com", "app", DnsRecordType::A)
            .await
            .unwrap();

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].content.to_value_string(), "203.0.113.1");
    }

    #[tokio::test]
    async fn create_record_is_create_only() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path(format!("{ZONE_PATH}/A/api")))
            .and(header("If-None-Match", "*"))
            .respond_with(
                ResponseTemplate::new(201).set_body_json(a_record_set("api", &["203.0.113.2"])),
            )
            .expect(1)
            .mount(&server)
            .await;

        let provider = create_mock_provider(&server).await;
        let record = provider
            .create_record("example.com", a_request("api", "203.0.113.2"))
            .await
            .unwrap();

        assert_eq!(record.fqdn, "api.example.com");
    }

    #[tokio::test]
    async fn create_record_maps_an_existing_record_set_to_a_conflict() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path(format!("{ZONE_PATH}/A/api")))
            .and(header("If-None-Match", "*"))
            .respond_with(ResponseTemplate::new(412).set_body_json(json!({
                "error": {"code": "PreconditionFailed", "message": "The Record set api exists already and hence cannot be created again."}
            })))
            .mount(&server)
            .await;

        let provider = create_mock_provider(&server).await;
        let error = provider
            .create_record("example.com", a_request("api", "203.0.113.2"))
            .await
            .unwrap_err();

        assert!(
            matches!(&error, DnsError::RecordConflict { domain, name, record_type, reason }
                if domain == "example.com"
                    && name == "api"
                    && record_type == "A"
                    && reason.contains("already exists")),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn update_record_replaces_without_if_none_match() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path(format!("{ZONE_PATH}/A/api")))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(a_record_set("api", &["203.0.113.3"])),
            )
            .expect(1)
            .mount(&server)
            .await;

        let provider = create_mock_provider(&server).await;
        provider
            .update_record("example.com", "api::A", a_request("api", "203.0.113.3"))
            .await
            .unwrap();

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].headers.get("if-none-match").is_none());
    }

    #[tokio::test]
    async fn delete_record_targets_the_apex_and_rejects_unknown_types() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path(format!("{ZONE_PATH}/TXT/@")))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let provider = create_mock_provider(&server).await;
        provider
            .delete_record("example.com", "@::TXT")
            .await
            .unwrap();

        let error = provider
            .delete_record("example.com", "www::A/../NS")
            .await
            .unwrap_err();
        assert!(matches!(error, DnsError::Validation(_)), "{error}");
    }

    #[tokio::test]
    async fn api_errors_embed_a_bounded_slice_of_the_body() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(recordsets_path()))
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
    }

    fn test_credentials() -> AzureCredentials {
        AzureCredentials {
            tenant_id: "test-tenant-id".to_string(),
            client_id: "test-client-id".to_string(),
            client_secret: "test-client-secret".to_string(),
            subscription_id: "test-subscription-id".to_string(),
            resource_group: "test-resource-group".to_string(),
        }
    }

    async fn create_mock_provider(mock_server: &MockServer) -> AzureProvider {
        AzureProvider::with_test_token(
            test_credentials(),
            mock_server.uri(),
            "test-access-token".to_string(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn test_list_zones() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/subscriptions/test-subscription-id/resourceGroups/test-resource-group/providers/Microsoft.Network/dnsZones"))
            .and(header("Authorization", "Bearer test-access-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "value": [
                    {
                        "id": "/subscriptions/xxx/resourceGroups/xxx/providers/Microsoft.Network/dnsZones/example.com",
                        "name": "example.com",
                        "properties": {
                            "nameServers": ["ns1-01.azure-dns.com", "ns2-01.azure-dns.net"]
                        }
                    },
                    {
                        "id": "/subscriptions/xxx/resourceGroups/xxx/providers/Microsoft.Network/dnsZones/test.org",
                        "name": "test.org",
                        "properties": {
                            "nameServers": ["ns1-02.azure-dns.com", "ns2-02.azure-dns.net"]
                        }
                    }
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
            .and(path("/subscriptions/test-subscription-id/resourceGroups/test-resource-group/providers/Microsoft.Network/dnsZones/example.com/recordsets"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "value": [
                    {
                        "id": "/subscriptions/xxx/resourceGroups/xxx/providers/Microsoft.Network/dnsZones/example.com/A/www",
                        "name": "www",
                        "type": "Microsoft.Network/dnszones/A",
                        "properties": {
                            "TTL": 300,
                            "ARecords": [
                                {"ipv4Address": "192.0.2.1"}
                            ]
                        }
                    },
                    {
                        "id": "/subscriptions/xxx/resourceGroups/xxx/providers/Microsoft.Network/dnsZones/example.com/TXT/@",
                        "name": "@",
                        "type": "Microsoft.Network/dnszones/TXT",
                        "properties": {
                            "TTL": 3600,
                            "TXTRecords": [
                                {"value": ["v=spf1 -all"]}
                            ]
                        }
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

        Mock::given(method("PUT"))
            .and(path("/subscriptions/test-subscription-id/resourceGroups/test-resource-group/providers/Microsoft.Network/dnsZones/example.com/A/api"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": "/subscriptions/xxx/resourceGroups/xxx/providers/Microsoft.Network/dnsZones/example.com/A/api",
                "name": "api",
                "type": "Microsoft.Network/dnszones/A",
                "properties": {
                    "TTL": 300,
                    "ARecords": [
                        {"ipv4Address": "192.0.2.2"}
                    ]
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

        assert_eq!(record.name, "api");
        assert_eq!(record.fqdn, "api.example.com");
    }

    #[tokio::test]
    async fn test_delete_record() {
        let mock_server = MockServer::start().await;

        Mock::given(method("DELETE"))
            .and(path("/subscriptions/test-subscription-id/resourceGroups/test-resource-group/providers/Microsoft.Network/dnsZones/example.com/A/www"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&mock_server)
            .await;

        let provider = create_mock_provider(&mock_server).await;
        let result = provider.delete_record("example.com", "www::A").await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_get_zone() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/subscriptions/test-subscription-id/resourceGroups/test-resource-group/providers/Microsoft.Network/dnsZones"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "value": [
                    {
                        "id": "/subscriptions/xxx/resourceGroups/xxx/providers/Microsoft.Network/dnsZones/example.com",
                        "name": "example.com",
                        "properties": {
                            "nameServers": []
                        }
                    }
                ]
            })))
            .mount(&mock_server)
            .await;

        let provider = create_mock_provider(&mock_server).await;

        let zone = provider.get_zone("example.com").await.unwrap();
        assert!(zone.is_some());
        assert_eq!(zone.unwrap().name, "example.com");

        let zone = provider.get_zone("notfound.com").await.unwrap();
        assert!(zone.is_none());
    }

    #[tokio::test]
    async fn test_test_connection_success() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/subscriptions/test-subscription-id/resourceGroups/test-resource-group/providers/Microsoft.Network/dnsZones"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "value": []
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
            .and(path("/subscriptions/test-subscription-id/resourceGroups/test-resource-group/providers/Microsoft.Network/dnsZones"))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
                "error": {
                    "code": "AuthenticationFailed",
                    "message": "Authentication failed"
                }
            })))
            .mount(&mock_server)
            .await;

        let provider = create_mock_provider(&mock_server).await;
        let result = provider.test_connection().await.unwrap();

        assert!(!result);
    }
}
