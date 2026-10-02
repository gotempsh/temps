// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Google Cloud DNS provider implementation
//!
//! This provider uses the Google Cloud DNS API to manage DNS records.
//! It requires a service account with DNS Administrator role.
//!
//! Required IAM Roles:
//! - roles/dns.admin (DNS Administrator)
//!
//! Authentication uses a service account JSON key file.

use async_trait::async_trait;
use reqwest::{Client, Method, StatusCode};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use tracing::{debug, info, warn};

use super::credentials::GcpCredentials;
use super::traits::{
    decode_txt_presentation, dns_names_equal, encode_txt_presentation, truncate_error_body,
    DnsProvider, DnsProviderCapabilities, DnsProviderType, DnsRecord, DnsRecordContent,
    DnsRecordRequest, DnsRecordType, DnsZone,
};
use crate::errors::DnsError;

const GCP_DNS_API_BASE: &str = "https://dns.googleapis.com/dns/v1";
const GCP_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
/// Hard cap on pages read by any listing; reaching it is an error, never a
/// silently truncated result.
const MAX_PAGES: usize = 1000;

/// Google Cloud DNS provider
pub struct GcpProvider {
    client: Client,
    credentials: GcpCredentials,
    base_url: String,
    /// Cached access token
    access_token: tokio::sync::RwLock<Option<String>>,
    /// Page cap for listings ([`MAX_PAGES`]; lowered in tests).
    max_pages: usize,
}

/// Service account key structure
#[derive(Debug, Clone, Deserialize)]
pub struct ServiceAccountKey {
    #[serde(rename = "type")]
    pub key_type: String,
    pub project_id: String,
    pub private_key_id: String,
    pub private_key: String,
    pub client_email: String,
    pub client_id: String,
    pub auth_uri: String,
    pub token_uri: String,
}

/// Google Cloud DNS API response structures
#[derive(Debug, Deserialize)]
struct ManagedZonesResponse {
    #[serde(default)]
    #[serde(rename = "managedZones")]
    managed_zones: Vec<ManagedZone>,
    /// More zones follow; pass it back as `pageToken`.
    #[serde(rename = "nextPageToken")]
    next_page_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ManagedZone {
    #[allow(dead_code)]
    id: String,
    name: String,
    #[serde(rename = "dnsName")]
    dns_name: String,
    #[serde(default)]
    #[serde(rename = "nameServers")]
    name_servers: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct ResourceRecordSetsResponse {
    #[serde(default)]
    rrsets: Vec<ResourceRecordSet>,
    /// More rrsets follow; pass it back as `pageToken`.
    #[serde(rename = "nextPageToken")]
    next_page_token: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct ResourceRecordSet {
    name: String,
    #[serde(rename = "type")]
    record_type: String,
    ttl: u32,
    /// Empty for a routing-policy rrset, whose values live in
    /// `routingPolicy` instead.
    #[serde(default)]
    rrdatas: Vec<String>,
}

/// One page of a Cloud DNS list response: its items and `nextPageToken`.
trait GcpListPage: DeserializeOwned {
    type Item;
    fn into_parts(self) -> (Vec<Self::Item>, Option<String>);
}

impl GcpListPage for ManagedZonesResponse {
    type Item = ManagedZone;
    fn into_parts(self) -> (Vec<ManagedZone>, Option<String>) {
        (self.managed_zones, self.next_page_token)
    }
}

impl GcpListPage for ResourceRecordSetsResponse {
    type Item = ResourceRecordSet;
    fn into_parts(self) -> (Vec<ResourceRecordSet>, Option<String>) {
        (self.rrsets, self.next_page_token)
    }
}

/// Change request for Google Cloud DNS
#[derive(Debug, Serialize)]
struct ChangeRequest {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    additions: Vec<ResourceRecordSet>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    deletions: Vec<ResourceRecordSet>,
}

/// Token response from Google OAuth
#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[allow(dead_code)]
    expires_in: u64,
    #[allow(dead_code)]
    token_type: String,
}

impl GcpProvider {
    /// Create a new GCP DNS provider with the given credentials
    pub fn new(credentials: GcpCredentials) -> Result<Self, DnsError> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| DnsError::ApiError(format!("Failed to create HTTP client: {}", e)))?;

        Ok(Self {
            client,
            credentials,
            base_url: GCP_DNS_API_BASE.to_string(),
            access_token: tokio::sync::RwLock::new(None),
            max_pages: MAX_PAGES,
        })
    }

    /// Create a provider with a custom base URL (for testing)
    #[cfg(test)]
    pub fn with_base_url(credentials: GcpCredentials, base_url: String) -> Result<Self, DnsError> {
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
        credentials: GcpCredentials,
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

        // Create JWT for token request using credentials directly
        let jwt = self.create_jwt()?;

        // Exchange JWT for access token
        let response = self
            .client
            .post(GCP_TOKEN_URL)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
                ("assertion", &jwt),
            ])
            .send()
            .await
            .map_err(|e| DnsError::ApiError(format!("Token request failed: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let error = response.text().await.unwrap_or_default();
            return Err(DnsError::InvalidCredentials(format!(
                "Failed to get GCP access token for service account {} (HTTP {}): {}",
                self.credentials.service_account_email,
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

    /// Create JWT for service account authentication
    fn create_jwt(&self) -> Result<String, DnsError> {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};

        let now = chrono::Utc::now().timestamp();
        let exp = now + 3600; // 1 hour

        let header = serde_json::json!({
            "alg": "RS256",
            "typ": "JWT"
        });

        let claims = serde_json::json!({
            "iss": self.credentials.service_account_email,
            "scope": "https://www.googleapis.com/auth/ndev.clouddns.readwrite",
            "aud": GCP_TOKEN_URL,
            "iat": now,
            "exp": exp
        });

        let header_b64 = URL_SAFE_NO_PAD.encode(header.to_string().as_bytes());
        let claims_b64 = URL_SAFE_NO_PAD.encode(claims.to_string().as_bytes());

        let message = format!("{}.{}", header_b64, claims_b64);

        // Sign with RSA private key
        let signature = self.sign_rs256(&message, &self.credentials.private_key)?;
        let signature_b64 = URL_SAFE_NO_PAD.encode(&signature);

        Ok(format!("{}.{}", message, signature_b64))
    }

    /// Sign message with RS256 (RSA-SHA256)
    fn sign_rs256(&self, message: &str, private_key_pem: &str) -> Result<Vec<u8>, DnsError> {
        use rsa::pkcs1v15::SigningKey;
        use rsa::pkcs8::DecodePrivateKey;
        use rsa::signature::{SignatureEncoding, Signer};
        use rsa::RsaPrivateKey;
        use sha2_010::Sha256;

        let private_key = RsaPrivateKey::from_pkcs8_pem(private_key_pem)
            .map_err(|e| DnsError::InvalidCredentials(format!("Invalid private key: {}", e)))?;

        let signing_key = SigningKey::<Sha256>::new_unprefixed(private_key);
        let signature = signing_key.sign(message.as_bytes());

        Ok(signature.to_vec())
    }

    /// Send one authenticated request and return its status and body,
    /// whatever the status. `path_and_query` is appended to the API base.
    async fn send<B: Serialize + ?Sized>(
        &self,
        method: Method,
        path_and_query: &str,
        body: Option<&B>,
    ) -> Result<(StatusCode, String), DnsError> {
        let token = self.get_access_token().await?;
        let url = format!("{}{}", self.base_url, path_and_query);
        let path = Self::without_query(path_and_query);

        debug!("GCP DNS API request: {} {}", method, path);

        let mut request = self
            .client
            .request(method.clone(), &url)
            .header("Authorization", format!("Bearer {}", token));
        if let Some(body) = body {
            request = request.json(body);
        }

        let response = request
            .send()
            .await
            .map_err(|e| DnsError::ApiError(format!("GCP DNS API {method} {path} failed: {e}")))?;

        let status = response.status();
        let response_text = response.text().await.map_err(|e| {
            DnsError::ApiError(format!(
                "Failed to read GCP DNS API response for {method} {path} (HTTP {status}): {e}"
            ))
        })?;
        Ok((status, response_text))
    }

    /// `path` without its query string (page tokens stay out of messages).
    fn without_query(path_and_query: &str) -> &str {
        path_and_query.split('?').next().unwrap_or(path_and_query)
    }

    /// Error for a non-success response: status, operation, bounded body.
    fn status_error(method: &Method, path: &str, status: StatusCode, body: &str) -> DnsError {
        DnsError::ApiError(format!(
            "GCP API returned status {status} for {method} {path}: {}",
            truncate_error_body(body)
        ))
    }

    /// Make an authenticated request to GCP DNS API
    async fn api_request<T: DeserializeOwned, B: Serialize + ?Sized>(
        &self,
        method: Method,
        path_and_query: &str,
        body: Option<&B>,
    ) -> Result<T, DnsError> {
        let (status, response_text) = self.send(method.clone(), path_and_query, body).await?;
        let path = Self::without_query(path_and_query);
        if !status.is_success() {
            return Err(Self::status_error(&method, path, status, &response_text));
        }

        let response_text = if response_text.is_empty() {
            "{}"
        } else {
            response_text.as_str()
        };
        serde_json::from_str(response_text).map_err(|e| {
            DnsError::ApiError(format!(
                "Failed to parse GCP DNS API response for {} {}: {} - Body: {}",
                method,
                path,
                e,
                truncate_error_body(response_text)
            ))
        })
    }

    /// Normalize domain name (remove trailing dot)
    fn normalize_domain(domain: &str) -> String {
        domain.trim_end_matches('.').to_lowercase()
    }

    /// Add trailing dot for GCP API
    fn with_trailing_dot(domain: &str) -> String {
        if domain.ends_with('.') {
            domain.to_string()
        } else {
            format!("{}.", domain)
        }
    }

    /// The managed zone whose DNS name is exactly `domain`, or
    /// [`DnsError::ZoneNotFound`] (see [`DnsProvider::get_zone`]). A parent
    /// zone is never substituted for a missing one. `id` is the managed
    /// zone's resource name.
    async fn resolve_zone(&self, domain: &str) -> Result<DnsZone, DnsError> {
        self.get_zone(domain)
            .await?
            .ok_or_else(|| DnsError::ZoneNotFound(domain.to_string()))
    }

    /// A Cloud DNS managed zone as a [`DnsZone`] (`id` = resource name,
    /// normalized DNS name).
    fn dns_zone(zone: ManagedZone) -> DnsZone {
        DnsZone {
            id: zone.name,
            name: Self::normalize_domain(&zone.dns_name),
            status: "active".to_string(),
            nameservers: zone.name_servers,
            metadata: HashMap::new(),
        }
    }

    /// Managed zones of the project, following `nextPageToken`; with
    /// `dns_name` (lowercase, trailing dot) only those Cloud DNS returns for
    /// that `dnsName` filter.
    async fn list_managed_zones(
        &self,
        dns_name: Option<&str>,
        context: &str,
    ) -> Result<Vec<DnsZone>, DnsError> {
        let path = format!("/projects/{}/managedZones", self.credentials.project_id);
        let query: Vec<String> = dns_name
            .map(|dns_name| format!("dnsName={}", urlencoding::encode(dns_name)))
            .into_iter()
            .collect();
        Ok(self
            .list_all::<ManagedZonesResponse>(&path, &query, context)
            .await?
            .into_iter()
            .map(Self::dns_zone)
            .collect())
    }

    /// Lowercase FQDN, with trailing dot, of a temps record name in `zone`.
    fn record_fqdn(name: &str, zone: &str) -> String {
        let name = name.trim_end_matches('.');
        let fqdn = if name.is_empty() || name == "@" {
            zone.to_string()
        } else {
            format!("{name}.{zone}")
        };
        Self::with_trailing_dot(&fqdn.to_ascii_lowercase())
    }

    /// Every item of a Cloud DNS list endpoint, following `nextPageToken`.
    /// `base_query` holds already-encoded `key=value` pairs sent with every
    /// page.
    ///
    /// Never returns a partial list: reaching the page cap, a repeated page
    /// token, or an empty page that still has a token is an error.
    async fn list_all<P: GcpListPage>(
        &self,
        path: &str,
        base_query: &[String],
        context: &str,
    ) -> Result<Vec<P::Item>, DnsError> {
        let mut items = Vec::new();
        let mut page_token: Option<String> = None;
        let mut seen_tokens: HashSet<String> = HashSet::new();
        for page in 1..=self.max_pages {
            let mut query = base_query.to_vec();
            if let Some(token) = &page_token {
                query.push(format!("pageToken={}", urlencoding::encode(token)));
            }
            let path_and_query = if query.is_empty() {
                path.to_string()
            } else {
                format!("{path}?{}", query.join("&"))
            };
            let response: P = self
                .api_request(Method::GET, &path_and_query, None::<&()>)
                .await?;
            let (page_items, next_page_token) = response.into_parts();

            let page_len = page_items.len();
            items.extend(page_items);
            let token = match next_page_token {
                Some(token) if !token.is_empty() => token,
                _ => return Ok(items),
            };
            if page_len == 0 {
                return Err(DnsError::ApiError(format!(
                    "{context}: GCP returned empty page {page} with a nextPageToken; refusing a partial result"
                )));
            }
            if !seen_tokens.insert(token.clone()) {
                return Err(DnsError::ApiError(format!(
                    "{context}: GCP repeated a nextPageToken after {page} page(s); refusing a partial result"
                )));
            }
            page_token = Some(token);
        }
        Err(DnsError::ApiError(format!(
            "{context} exceeded {} pages; refusing a partial result",
            self.max_pages
        )))
    }

    /// Every rrset of `zone` — or, with `filter`, only those Cloud DNS
    /// returns for that (FQDN, type) — following `nextPageToken` (see
    /// [`Self::list_all`]).
    async fn list_rrsets(
        &self,
        zone: &DnsZone,
        filter: Option<(&str, DnsRecordType)>,
        context: &str,
    ) -> Result<Vec<ResourceRecordSet>, DnsError> {
        let path = format!(
            "/projects/{}/managedZones/{}/rrsets",
            self.credentials.project_id, zone.id
        );
        let mut query: Vec<String> = Vec::new();
        if let Some((fqdn, record_type)) = filter {
            query.push(format!("name={}", urlencoding::encode(fqdn)));
            query.push(format!("type={record_type}"));
        }
        self.list_all::<ResourceRecordSetsResponse>(&path, &query, context)
            .await
    }

    /// Every rrset at exactly (`fqdn`, `record_type`), read with Cloud DNS's
    /// `name`/`type` filters instead of a zone listing. Results are matched
    /// again client-side, so a filter the API ignored cannot leak other rrsets.
    async fn exact_rrsets(
        &self,
        zone: &DnsZone,
        fqdn: &str,
        record_type: DnsRecordType,
    ) -> Result<Vec<ResourceRecordSet>, DnsError> {
        let type_name = record_type.to_string();
        let context = format!(
            "GCP Cloud DNS lookup of {type_name} {} in zone {} ({})",
            Self::normalize_domain(fqdn),
            zone.name,
            zone.id
        );
        Ok(self
            .list_rrsets(zone, Some((fqdn, record_type)), &context)
            .await?
            .into_iter()
            .filter(|rrset| {
                dns_names_equal(&rrset.name, fqdn)
                    && rrset.record_type.eq_ignore_ascii_case(&type_name)
            })
            .collect())
    }

    /// Parse a record type name (`A`, `txt`, ...).
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

    /// Convert GCP record to our DnsRecord type
    fn convert_record(record: &ResourceRecordSet, zone_domain: &str) -> Vec<DnsRecord> {
        let Some(record_type) = Self::parse_record_type(&record.record_type) else {
            return vec![];
        };

        let zone_normalized = Self::normalize_domain(zone_domain);
        let fqdn = Self::normalize_domain(&record.name);
        let name = if fqdn == zone_normalized {
            "@".to_string()
        } else {
            fqdn.strip_suffix(&format!(".{}", zone_normalized))
                .unwrap_or(&fqdn)
                .to_string()
        };

        record
            .rrdatas
            .iter()
            .filter_map(|data| {
                let content = Self::parse_record_content(record_type, data)?;
                Some(DnsRecord {
                    id: Some(format!("{}::{}", fqdn, record.record_type)),
                    zone: zone_normalized.clone(),
                    name: name.clone(),
                    fqdn: fqdn.clone(),
                    content,
                    ttl: record.ttl,
                    proxied: false,
                    metadata: HashMap::new(),
                })
            })
            .collect()
    }

    /// Parse record data into DnsRecordContent
    fn parse_record_content(record_type: DnsRecordType, data: &str) -> Option<DnsRecordContent> {
        match record_type {
            DnsRecordType::A => Some(DnsRecordContent::A {
                address: data.to_string(),
            }),
            DnsRecordType::AAAA => Some(DnsRecordContent::AAAA {
                address: data.to_string(),
            }),
            DnsRecordType::CNAME => Some(DnsRecordContent::CNAME {
                target: Self::normalize_domain(data),
            }),
            DnsRecordType::TXT => {
                // Presentation format: one or more quoted ≤255-byte
                // character-strings with `\"`/`\\`/`\DDD` escapes,
                // concatenated back into the original content.
                let content = decode_txt_presentation(data);
                Some(DnsRecordContent::TXT { content })
            }
            DnsRecordType::MX => {
                let parts: Vec<&str> = data.split_whitespace().collect();
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
                nameserver: Self::normalize_domain(data),
            }),
            DnsRecordType::SRV => {
                let parts: Vec<&str> = data.split_whitespace().collect();
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
                let parts: Vec<&str> = data.splitn(3, ' ').collect();
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
                target: Self::normalize_domain(data),
            }),
        }
    }

    /// Format record content for GCP API
    fn format_record_data(content: &DnsRecordContent) -> String {
        match content {
            DnsRecordContent::A { address } | DnsRecordContent::AAAA { address } => address.clone(),
            DnsRecordContent::CNAME { target }
            | DnsRecordContent::NS { nameserver: target }
            | DnsRecordContent::PTR { target } => Self::with_trailing_dot(target),
            DnsRecordContent::TXT { content } => {
                // Quoted, escaped, and split into ≤255-byte character-strings:
                // an ownership marker (~370 bytes of JSON) would otherwise be
                // rejected or corrupted.
                encode_txt_presentation(content)
            }
            DnsRecordContent::MX { priority, target } => {
                format!("{} {}", priority, Self::with_trailing_dot(target))
            }
            DnsRecordContent::SRV {
                priority,
                weight,
                port,
                target,
            } => {
                format!(
                    "{} {} {} {}",
                    priority,
                    weight,
                    port,
                    Self::with_trailing_dot(target)
                )
            }
            DnsRecordContent::CAA { flags, tag, value } => {
                format!("{} {} \"{}\"", flags, tag, value)
            }
        }
    }
}

#[async_trait]
impl DnsProvider for GcpProvider {
    fn provider_type(&self) -> DnsProviderType {
        DnsProviderType::Gcp
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
                info!("GCP DNS API connection test successful");
                Ok(true)
            }
            Err(e) => {
                warn!("GCP DNS API connection test failed: {}", e);
                Ok(false)
            }
        }
    }

    /// Every managed zone of the project, following `nextPageToken`.
    ///
    /// Never returns a partial list: reaching the page cap, a repeated page
    /// token, or an empty page that still has a token is an error.
    async fn list_zones(&self) -> Result<Vec<DnsZone>, DnsError> {
        let context = format!(
            "GCP Cloud DNS managed zone listing for project {}",
            self.credentials.project_id
        );
        self.list_managed_zones(None, &context).await
    }

    /// The managed zone whose DNS name is exactly `domain` (case-insensitive,
    /// trailing dot ignored), found with Cloud DNS's `dnsName` filter instead
    /// of a full zone listing. Results are matched again client-side, so a
    /// filter the API ignored cannot return another zone.
    ///
    /// When several managed zones share the DNS name (for example a public
    /// and a private zone), the first one Cloud DNS returns is used; there is
    /// no public/private preference.
    async fn get_zone(&self, domain: &str) -> Result<Option<DnsZone>, DnsError> {
        let normalized = Self::normalize_domain(domain);
        if normalized.is_empty() {
            return Ok(None);
        }
        let context = format!(
            "GCP Cloud DNS lookup of zone {normalized} in project {}",
            self.credentials.project_id
        );
        Ok(self
            .list_managed_zones(Some(&Self::with_trailing_dot(&normalized)), &context)
            .await?
            .into_iter()
            .find(|zone| zone.name == normalized))
    }

    async fn list_records(&self, domain: &str) -> Result<Vec<DnsRecord>, DnsError> {
        let zone = self.resolve_zone(domain).await?;
        let context = format!(
            "GCP Cloud DNS record listing for zone {} ({})",
            zone.name, zone.id
        );

        Ok(self
            .list_rrsets(&zone, None, &context)
            .await?
            .iter()
            .flat_map(|rs| Self::convert_record(rs, &zone.name))
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

    /// Every value at (name, type), read with Cloud DNS's exact `name`/`type`
    /// filters instead of a zone listing.
    ///
    /// A matching rrset whose values temps cannot represent (a
    /// routing-policy rrset, or a value that does not parse) is a conflict,
    /// never "absent": treating it as absent would let a write target a name
    /// that is already in use.
    async fn get_records(
        &self,
        domain: &str,
        name: &str,
        record_type: DnsRecordType,
    ) -> Result<Vec<DnsRecord>, DnsError> {
        let zone = self.resolve_zone(domain).await?;
        let fqdn = Self::record_fqdn(name, &zone.name);

        let mut records = Vec::new();
        for rrset in self.exact_rrsets(&zone, &fqdn, record_type).await? {
            let converted = Self::convert_record(&rrset, &zone.name);
            if rrset.rrdatas.is_empty() || converted.len() != rrset.rrdatas.len() {
                return Err(DnsError::RecordConflict {
                    domain: domain.to_string(),
                    name: name.to_string(),
                    record_type: record_type.to_string(),
                    reason: "GCP Cloud DNS has a record set at this name and type whose values temps cannot read (for example a routing-policy record set), so temps will not manage it".to_string(),
                });
            }
            records.extend(converted);
        }
        Ok(records)
    }

    /// Create-only: a change whose only addition is an rrset that already
    /// exists is rejected by Cloud DNS (HTTP 409 `alreadyExists`), which
    /// surfaces as [`DnsError::RecordConflict`].
    async fn create_record(
        &self,
        domain: &str,
        request: DnsRecordRequest,
    ) -> Result<DnsRecord, DnsError> {
        let zone = self.resolve_zone(domain).await?;

        let fqdn = if request.name == "@" || request.name.is_empty() {
            Self::with_trailing_dot(&zone.name)
        } else {
            Self::with_trailing_dot(&format!("{}.{}", request.name, zone.name))
        };

        let record_type = request.content.record_type().to_string();
        let data = Self::format_record_data(&request.content);

        let change = ChangeRequest {
            additions: vec![ResourceRecordSet {
                name: fqdn.clone(),
                record_type: record_type.clone(),
                ttl: request.ttl.unwrap_or(300),
                rrdatas: vec![data],
            }],
            deletions: vec![],
        };

        let path = format!(
            "/projects/{}/managedZones/{}/changes",
            self.credentials.project_id, zone.id
        );
        let (status, response_text) = self.send(Method::POST, &path, Some(&change)).await?;
        if status == StatusCode::CONFLICT && response_text.contains("alreadyExists") {
            return Err(DnsError::RecordConflict {
                domain: domain.to_string(),
                name: request.name.clone(),
                record_type,
                reason: "a GCP Cloud DNS record set with this name and type already exists at the provider, and a create never replaces one (Cloud DNS rejected the addition with HTTP 409 alreadyExists)".to_string(),
            });
        }
        if !status.is_success() {
            return Err(Self::status_error(
                &Method::POST,
                &path,
                status,
                &response_text,
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

        // GCP requires delete + add for updates
        // First, get the existing record to delete it
        let existing = self
            .get_record(domain, &request.name, request.content.record_type())
            .await?
            .ok_or_else(|| {
                DnsError::RecordNotFound(format!(
                    "{} {} in {}",
                    request.name,
                    request.content.record_type(),
                    domain
                ))
            })?;

        let fqdn = if request.name == "@" || request.name.is_empty() {
            Self::with_trailing_dot(&zone.name)
        } else {
            Self::with_trailing_dot(&format!("{}.{}", request.name, zone.name))
        };

        let record_type = request.content.record_type().to_string();
        let old_data = Self::format_record_data(&existing.content);
        let new_data = Self::format_record_data(&request.content);

        let change = ChangeRequest {
            deletions: vec![ResourceRecordSet {
                name: fqdn.clone(),
                record_type: record_type.clone(),
                ttl: existing.ttl,
                rrdatas: vec![old_data],
            }],
            additions: vec![ResourceRecordSet {
                name: fqdn.clone(),
                record_type: record_type.clone(),
                ttl: request.ttl.unwrap_or(300),
                rrdatas: vec![new_data],
            }],
        };

        let path = format!(
            "/projects/{}/managedZones/{}/changes",
            self.credentials.project_id, zone.id
        );
        let _: serde_json::Value = self.api_request(Method::POST, &path, Some(&change)).await?;

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
            .exact_rrsets(&zone, &Self::with_trailing_dot(fqdn), record_type)
            .await?
            .iter()
            .flat_map(|rrset| Self::convert_record(rrset, &zone.name))
            .next()
            .ok_or_else(|| DnsError::RecordNotFound(record_id.to_string()))?;

        let data = Self::format_record_data(&existing.content);

        let change = ChangeRequest {
            additions: vec![],
            deletions: vec![ResourceRecordSet {
                name: Self::with_trailing_dot(fqdn),
                record_type: record_type.to_string(),
                ttl: existing.ttl,
                rrdatas: vec![data],
            }],
        };

        let path = format!(
            "/projects/{}/managedZones/{}/changes",
            self.credentials.project_id, zone.id
        );
        let _: serde_json::Value = self.api_request(Method::POST, &path, Some(&change)).await?;

        info!("Deleted DNS record {} from domain {}", record_id, domain);

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_domain() {
        assert_eq!(GcpProvider::normalize_domain("example.com."), "example.com");
        assert_eq!(GcpProvider::normalize_domain("example.com"), "example.com");
        assert_eq!(
            GcpProvider::normalize_domain("SUB.Example.COM."),
            "sub.example.com"
        );
    }

    #[test]
    fn record_fqdn_is_lowercase_with_a_trailing_dot() {
        assert_eq!(
            GcpProvider::record_fqdn("App", "example.com"),
            "app.example.com."
        );
        assert_eq!(GcpProvider::record_fqdn("@", "example.com"), "example.com.");
        assert_eq!(GcpProvider::record_fqdn("", "example.com"), "example.com.");
        assert_eq!(
            GcpProvider::record_fqdn("*.preview.", "example.com"),
            "*.preview.example.com."
        );
    }

    #[test]
    fn test_with_trailing_dot() {
        assert_eq!(
            GcpProvider::with_trailing_dot("example.com"),
            "example.com."
        );
        assert_eq!(
            GcpProvider::with_trailing_dot("example.com."),
            "example.com."
        );
    }

    #[test]
    fn test_format_record_data_a() {
        let content = DnsRecordContent::A {
            address: "192.0.2.1".to_string(),
        };
        assert_eq!(GcpProvider::format_record_data(&content), "192.0.2.1");
    }

    #[test]
    fn test_format_record_data_txt() {
        let content = DnsRecordContent::TXT {
            content: "v=spf1 -all".to_string(),
        };
        assert_eq!(GcpProvider::format_record_data(&content), "\"v=spf1 -all\"");
    }

    #[test]
    fn test_format_record_data_cname() {
        let content = DnsRecordContent::CNAME {
            target: "www.example.com".to_string(),
        };
        assert_eq!(
            GcpProvider::format_record_data(&content),
            "www.example.com."
        );
    }

    #[test]
    fn test_format_record_data_mx() {
        let content = DnsRecordContent::MX {
            priority: 10,
            target: "mail.example.com".to_string(),
        };
        assert_eq!(
            GcpProvider::format_record_data(&content),
            "10 mail.example.com."
        );
    }

    #[test]
    fn test_parse_record_content_a() {
        let content = GcpProvider::parse_record_content(DnsRecordType::A, "192.0.2.1");
        assert!(content.is_some());
        if let Some(DnsRecordContent::A { address }) = content {
            assert_eq!(address, "192.0.2.1");
        } else {
            panic!("Expected A record");
        }
    }

    #[test]
    fn test_parse_record_content_txt() {
        let content = GcpProvider::parse_record_content(DnsRecordType::TXT, "\"v=spf1 -all\"");
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

        let rdata = GcpProvider::format_record_data(&original);
        assert!(rdata.starts_with('"') && rdata.ends_with('"'));
        assert!(
            rdata.contains("\" \""),
            "long content must be split: {rdata}"
        );
        assert!(rdata.contains("\\\""), "embedded quotes must be escaped");

        match GcpProvider::parse_record_content(DnsRecordType::TXT, &rdata) {
            Some(DnsRecordContent::TXT { content }) => assert_eq!(content, marker),
            other => panic!("Expected TXT record, got {other:?}"),
        }
    }

    #[test]
    fn test_txt_multi_string_rdata_is_concatenated() {
        match GcpProvider::parse_record_content(
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
        let content = GcpProvider::parse_record_content(DnsRecordType::MX, "10 mail.example.com.");
        assert!(content.is_some());
        if let Some(DnsRecordContent::MX { priority, target }) = content {
            assert_eq!(priority, 10);
            assert_eq!(target, "mail.example.com");
        } else {
            panic!("Expected MX record");
        }
    }

    #[test]
    fn test_convert_record() {
        let gcp_record = ResourceRecordSet {
            name: "www.example.com.".to_string(),
            record_type: "A".to_string(),
            ttl: 300,
            rrdatas: vec!["192.0.2.1".to_string()],
        };

        let records = GcpProvider::convert_record(&gcp_record, "example.com");

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].name, "www");
        assert_eq!(records[0].fqdn, "www.example.com");
        assert_eq!(records[0].ttl, 300);
    }

    #[test]
    fn test_convert_record_apex() {
        let gcp_record = ResourceRecordSet {
            name: "example.com.".to_string(),
            record_type: "A".to_string(),
            ttl: 300,
            rrdatas: vec!["192.0.2.1".to_string()],
        };

        let records = GcpProvider::convert_record(&gcp_record, "example.com");

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].name, "@");
        assert_eq!(records[0].fqdn, "example.com");
    }

    #[test]
    fn test_convert_record_multiple_values() {
        let gcp_record = ResourceRecordSet {
            name: "example.com.".to_string(),
            record_type: "A".to_string(),
            ttl: 300,
            rrdatas: vec!["192.0.2.1".to_string(), "192.0.2.2".to_string()],
        };

        let records = GcpProvider::convert_record(&gcp_record, "example.com");

        assert_eq!(records.len(), 2);
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use serde_json::{json, Value};
    use wiremock::matchers::{
        any, body_partial_json, header, method, path, query_param, query_param_is_missing,
    };
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const ZONES_PATH: &str = "/projects/test-project/managedZones";
    const RRSETS_PATH: &str = "/projects/test-project/managedZones/example-com/rrsets";

    fn managed_zone(name: &str, dns_name: &str) -> Value {
        json!({"id": "1", "name": name, "dnsName": dns_name})
    }

    /// Exact zone lookup (`dnsName` filter) answering `example.com`.
    async fn mount_zone(server: &MockServer) {
        Mock::given(method("GET"))
            .and(path(ZONES_PATH))
            .and(query_param("dnsName", "example.com."))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "managedZones": [managed_zone("example-com", "example.com.")]
            })))
            .mount(server)
            .await;
    }

    /// An unfiltered managedZones page: the first one when `token` is `None`.
    async fn mount_zones_page(server: &MockServer, token: Option<&str>, body: Value) {
        let mock = Mock::given(method("GET"))
            .and(path(ZONES_PATH))
            .and(query_param_is_missing("dnsName"))
            .and(header("Authorization", "Bearer test-access-token"));
        let mock = match token {
            Some(token) => mock.and(query_param("pageToken", token)),
            None => mock.and(query_param_is_missing("pageToken")),
        };
        mock.respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(server)
            .await;
    }

    fn rrset(name: &str, record_type: &str, rrdatas: &[&str]) -> Value {
        json!({"name": name, "type": record_type, "ttl": 300, "rrdatas": rrdatas})
    }

    /// The zone's first page (no filters, no page token).
    async fn mount_first_page(server: &MockServer, body: Value) {
        Mock::given(method("GET"))
            .and(path(RRSETS_PATH))
            .and(query_param_is_missing("pageToken"))
            .and(query_param_is_missing("name"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(server)
            .await;
    }

    async fn mount_page(server: &MockServer, token: &str, body: Value) {
        Mock::given(method("GET"))
            .and(path(RRSETS_PATH))
            .and(query_param("pageToken", token))
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
    async fn list_records_follows_next_page_token() {
        let server = MockServer::start().await;
        mount_zone(&server).await;
        mount_first_page(
            &server,
            json!({"rrsets": [rrset("www.example.com.", "A", &["203.0.113.1"])], "nextPageToken": "t/2+="}),
        )
        .await;
        mount_page(
            &server,
            "t/2+=",
            json!({"rrsets": [rrset("api.example.com.", "A", &["203.0.113.2"])], "nextPageToken": "t3"}),
        )
        .await;
        // The registry TXT only exists on the last page: a one-page read
        // would report it absent.
        mount_page(
            &server,
            "t3",
            json!({"rrsets": [rrset("_temps-owned-a.app.example.com.", "TXT", &["\"marker\""])]}),
        )
        .await;

        let provider = create_mock_provider(&server).await;
        let records = provider.list_records("example.com").await.unwrap();

        let names: Vec<&str> = records.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["www", "api", "_temps-owned-a.app"]);
    }

    #[tokio::test]
    async fn list_records_fails_closed_on_a_repeated_page_token() {
        let server = MockServer::start().await;
        mount_zone(&server).await;
        mount_first_page(
            &server,
            json!({"rrsets": [rrset("www.example.com.", "A", &["203.0.113.1"])], "nextPageToken": "t2"}),
        )
        .await;
        mount_page(
            &server,
            "t2",
            json!({"rrsets": [rrset("api.example.com.", "A", &["203.0.113.2"])], "nextPageToken": "t2"}),
        )
        .await;

        let provider = create_mock_provider(&server).await;
        let error = provider.list_records("example.com").await.unwrap_err();

        assert!(
            matches!(&error, DnsError::ApiError(message)
                if message.contains("repeated a nextPageToken after 2 page(s)")),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn list_records_fails_closed_on_an_empty_page_with_a_token() {
        let server = MockServer::start().await;
        mount_zone(&server).await;
        mount_first_page(&server, json!({"rrsets": [], "nextPageToken": "t2"})).await;

        let provider = create_mock_provider(&server).await;
        let error = provider.list_records("example.com").await.unwrap_err();

        assert!(
            matches!(&error, DnsError::ApiError(message)
                if message.contains("empty page 1 with a nextPageToken")),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn list_records_fails_closed_at_the_page_cap() {
        let server = MockServer::start().await;
        mount_zone(&server).await;
        mount_first_page(
            &server,
            json!({"rrsets": [rrset("a.example.com.", "A", &["203.0.113.1"])], "nextPageToken": "t2"}),
        )
        .await;
        mount_page(
            &server,
            "t2",
            json!({"rrsets": [rrset("b.example.com.", "A", &["203.0.113.2"])], "nextPageToken": "t3"}),
        )
        .await;
        Mock::given(method("GET"))
            .and(query_param("pageToken", "t3"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"rrsets": []})))
            .expect(0)
            .mount(&server)
            .await;

        let mut provider = create_mock_provider(&server).await;
        provider.max_pages = 2;
        let error = provider.list_records("example.com").await.unwrap_err();

        assert!(
            matches!(&error, DnsError::ApiError(message)
                if message.contains("GCP Cloud DNS record listing for zone example.com (example-com) exceeded 2 pages")),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn get_records_uses_the_exact_name_and_type_filters() {
        let server = MockServer::start().await;
        mount_zone(&server).await;
        // A full listing must never be used for an exact lookup.
        Mock::given(method("GET"))
            .and(path(RRSETS_PATH))
            .and(query_param_is_missing("name"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"rrsets": []})))
            .expect(0)
            .mount(&server)
            .await;
        // The name is sent lowercased with a trailing dot.
        Mock::given(method("GET"))
            .and(path(RRSETS_PATH))
            .and(query_param("name", "app.example.com."))
            .and(query_param("type", "A"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "rrsets": [rrset("app.example.com.", "A", &["203.0.113.1", "203.0.113.2"])]
            })))
            .expect(2)
            .mount(&server)
            .await;

        let provider = create_mock_provider(&server).await;
        let records = provider
            .get_records("example.com", "App", DnsRecordType::A)
            .await
            .unwrap();

        let values: Vec<String> = records
            .iter()
            .map(|r| r.content.to_value_string())
            .collect();
        assert_eq!(values, vec!["203.0.113.1", "203.0.113.2"]);

        let record = provider
            .get_record("example.com", "APP", DnsRecordType::A)
            .await
            .unwrap();
        assert_eq!(record.map(|r| r.fqdn), Some("app.example.com".to_string()));
    }

    #[tokio::test]
    async fn get_records_follows_pages_and_ignores_rrsets_outside_the_filter() {
        let server = MockServer::start().await;
        mount_zone(&server).await;
        Mock::given(method("GET"))
            .and(path(RRSETS_PATH))
            .and(query_param("name", "example.com."))
            .and(query_param("type", "TXT"))
            .and(query_param_is_missing("pageToken"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                // An rrset the filter should have excluded is never returned.
                "rrsets": [rrset("other.example.com.", "TXT", &["\"x\""])],
                "nextPageToken": "t2"
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(RRSETS_PATH))
            .and(query_param("name", "example.com."))
            .and(query_param("pageToken", "t2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "rrsets": [rrset("example.com.", "TXT", &["\"v=spf1 -all\""])]
            })))
            .mount(&server)
            .await;

        let provider = create_mock_provider(&server).await;
        let records = provider
            .get_records("example.com", "@", DnsRecordType::TXT)
            .await
            .unwrap();

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].name, "@");
        assert_eq!(records[0].content.to_value_string(), "v=spf1 -all");
    }

    #[tokio::test]
    async fn get_records_refuses_routing_policy_rrsets() {
        let server = MockServer::start().await;
        mount_zone(&server).await;
        Mock::given(method("GET"))
            .and(path(RRSETS_PATH))
            .and(query_param("name", "app.example.com."))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "rrsets": [{
                    "name": "app.example.com.",
                    "type": "A",
                    "ttl": 300,
                    "routingPolicy": {"wrr": {"items": [{"weight": 1, "rrdatas": ["203.0.113.1"]}]}}
                }]
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
    async fn create_record_maps_an_existing_rrset_to_a_conflict() {
        let server = MockServer::start().await;
        mount_zone(&server).await;
        Mock::given(method("POST"))
            .and(path("/projects/test-project/managedZones/example-com/changes"))
            .and(body_partial_json(json!({
                "additions": [{"name": "app.example.com.", "type": "A"}]
            })))
            .respond_with(ResponseTemplate::new(409).set_body_json(json!({
                "error": {
                    "code": 409,
                    "message": "The resource 'entity.change.additions[0]' named 'app.example.com. (A)' already exists",
                    "errors": [{"reason": "alreadyExists", "domain": "global"}]
                }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let provider = create_mock_provider(&server).await;
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
    async fn delete_record_reads_only_the_target_rrset() {
        let server = MockServer::start().await;
        mount_zone(&server).await;
        Mock::given(method("GET"))
            .and(path(RRSETS_PATH))
            .and(query_param_is_missing("name"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"rrsets": []})))
            .expect(0)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(RRSETS_PATH))
            .and(query_param("name", "app.example.com."))
            .and(query_param("type", "A"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "rrsets": [rrset("app.example.com.", "A", &["203.0.113.1"])]
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(
                "/projects/test-project/managedZones/example-com/changes",
            ))
            .and(body_partial_json(json!({
                "deletions": [{"name": "app.example.com.", "type": "A", "rrdatas": ["203.0.113.1"]}]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status": "pending"})))
            .expect(1)
            .mount(&server)
            .await;

        let provider = create_mock_provider(&server).await;
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

    #[tokio::test]
    async fn list_zones_follows_next_page_token() {
        let server = MockServer::start().await;
        mount_zones_page(
            &server,
            None,
            json!({"managedZones": [managed_zone("example-com", "example.com.")], "nextPageToken": "z/2+="}),
        )
        .await;
        mount_zones_page(
            &server,
            Some("z/2+="),
            json!({"managedZones": [managed_zone("example-net", "example.net.")]}),
        )
        .await;

        let zones = create_mock_provider(&server)
            .await
            .list_zones()
            .await
            .unwrap();

        let zones: Vec<(&str, &str)> = zones
            .iter()
            .map(|zone| (zone.id.as_str(), zone.name.as_str()))
            .collect();
        assert_eq!(
            zones,
            vec![
                ("example-com", "example.com"),
                ("example-net", "example.net")
            ]
        );
    }

    #[tokio::test]
    async fn list_zones_fails_closed() {
        // A page token that was already followed.
        let server = MockServer::start().await;
        mount_zones_page(
            &server,
            None,
            json!({"managedZones": [managed_zone("example-com", "example.com.")], "nextPageToken": "z2"}),
        )
        .await;
        mount_zones_page(
            &server,
            Some("z2"),
            json!({"managedZones": [managed_zone("example-net", "example.net.")], "nextPageToken": "z2"}),
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
                .contains("repeated a nextPageToken after 2 page(s)"),
            "{error}"
        );

        // An empty page that still has a token.
        let server = MockServer::start().await;
        mount_zones_page(
            &server,
            None,
            json!({"managedZones": [], "nextPageToken": "z2"}),
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
                .contains("empty page 1 with a nextPageToken"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn list_zones_fails_closed_at_the_page_cap() {
        let server = MockServer::start().await;
        mount_zones_page(
            &server,
            None,
            json!({"managedZones": [managed_zone("example-com", "example.com.")], "nextPageToken": "z2"}),
        )
        .await;
        mount_zones_page(
            &server,
            Some("z2"),
            json!({"managedZones": [managed_zone("example-net", "example.net.")], "nextPageToken": "z3"}),
        )
        .await;
        Mock::given(method("GET"))
            .and(path(ZONES_PATH))
            .and(query_param("pageToken", "z3"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .expect(0)
            .mount(&server)
            .await;

        let mut provider = create_mock_provider(&server).await;
        provider.max_pages = 2;
        let error = provider.list_zones().await.unwrap_err();

        assert!(
            error.to_string().contains(
                "GCP Cloud DNS managed zone listing for project test-project exceeded 2 pages"
            ),
            "{error}"
        );
    }

    #[tokio::test]
    async fn get_zone_uses_the_dns_name_filter_instead_of_a_listing() {
        let server = MockServer::start().await;
        mount_zone(&server).await;
        // The filtered lookup follows its own pages, and a zone returned
        // despite the filter is never taken for a match.
        Mock::given(method("GET"))
            .and(path(ZONES_PATH))
            .and(query_param("dnsName", "example.net."))
            .and(query_param_is_missing("pageToken"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "managedZones": [managed_zone("sub-example-net", "sub.example.net.")],
                "nextPageToken": "n2"
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(ZONES_PATH))
            .and(query_param("dnsName", "example.net."))
            .and(query_param("pageToken", "n2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "managedZones": [managed_zone("example-net", "example.net.")]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(ZONES_PATH))
            .and(query_param("dnsName", "missing.example.com."))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(ZONES_PATH))
            .and(query_param_is_missing("dnsName"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;

        let provider = create_mock_provider(&server).await;
        let zone = provider.get_zone("Example.COM.").await.unwrap().unwrap();
        assert_eq!(zone.id, "example-com");
        assert_eq!(zone.name, "example.com");

        let zone = provider.get_zone("example.net").await.unwrap().unwrap();
        assert_eq!(zone.id, "example-net");

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

    fn test_credentials() -> GcpCredentials {
        GcpCredentials {
            service_account_email: "test@project.iam.gserviceaccount.com".to_string(),
            private_key: "-----BEGIN RSA PRIVATE KEY-----\ntest\n-----END RSA PRIVATE KEY-----"
                .to_string(),
            project_id: "test-project".to_string(),
        }
    }

    async fn create_mock_provider(mock_server: &MockServer) -> GcpProvider {
        GcpProvider::with_test_token(
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
            .and(path("/projects/test-project/managedZones"))
            .and(header("Authorization", "Bearer test-access-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "managedZones": [
                    {
                        "id": "123456789",
                        "name": "example-com",
                        "dnsName": "example.com.",
                        "description": "Test zone"
                    },
                    {
                        "id": "987654321",
                        "name": "test-org",
                        "dnsName": "test.org.",
                        "description": "Another zone"
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

        // Mock list zones to get zone name
        Mock::given(method("GET"))
            .and(path("/projects/test-project/managedZones"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "managedZones": [
                    {
                        "id": "123456789",
                        "name": "example-com",
                        "dnsName": "example.com."
                    }
                ]
            })))
            .mount(&mock_server)
            .await;

        // Mock list records
        Mock::given(method("GET"))
            .and(path(
                "/projects/test-project/managedZones/example-com/rrsets",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "rrsets": [
                    {
                        "name": "www.example.com.",
                        "type": "A",
                        "ttl": 300,
                        "rrdatas": ["192.0.2.1"]
                    },
                    {
                        "name": "example.com.",
                        "type": "TXT",
                        "ttl": 3600,
                        "rrdatas": ["\"v=spf1 -all\""]
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

        // Mock list zones
        Mock::given(method("GET"))
            .and(path("/projects/test-project/managedZones"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "managedZones": [
                    {
                        "id": "123456789",
                        "name": "example-com",
                        "dnsName": "example.com."
                    }
                ]
            })))
            .mount(&mock_server)
            .await;

        // Mock create record
        Mock::given(method("POST"))
            .and(path(
                "/projects/test-project/managedZones/example-com/changes",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": "pending",
                "additions": [
                    {
                        "name": "api.example.com.",
                        "type": "A",
                        "ttl": 300,
                        "rrdatas": ["192.0.2.2"]
                    }
                ]
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
    async fn test_get_zone() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/projects/test-project/managedZones"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "managedZones": [
                    {
                        "id": "123456789",
                        "name": "example-com",
                        "dnsName": "example.com."
                    },
                    {
                        "id": "987654321",
                        "name": "test-org",
                        "dnsName": "test.org."
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
            .and(path("/projects/test-project/managedZones"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "managedZones": []
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
            .and(path("/projects/test-project/managedZones"))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
                "error": {
                    "code": 401,
                    "message": "Invalid credentials"
                }
            })))
            .mount(&mock_server)
            .await;

        let provider = create_mock_provider(&mock_server).await;
        let result = provider.test_connection().await.unwrap();

        assert!(!result);
    }
}
