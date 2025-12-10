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
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::{debug, info, warn};

use super::credentials::GcpCredentials;
use super::traits::{
    DnsProvider, DnsProviderCapabilities, DnsProviderType, DnsRecord, DnsRecordContent,
    DnsRecordRequest, DnsRecordType, DnsZone,
};
use crate::errors::DnsError;

const GCP_DNS_API_BASE: &str = "https://dns.googleapis.com/dns/v1";
const GCP_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

/// Google Cloud DNS provider
pub struct GcpProvider {
    client: Client,
    credentials: GcpCredentials,
    #[allow(dead_code)]
    base_url: String,
    /// Cached access token
    access_token: tokio::sync::RwLock<Option<String>>,
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
}

#[derive(Debug, Deserialize)]
struct ManagedZone {
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
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct ResourceRecordSet {
    name: String,
    #[serde(rename = "type")]
    record_type: String,
    ttl: u32,
    rrdatas: Vec<String>,
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
            let error = response.text().await.unwrap_or_default();
            return Err(DnsError::InvalidCredentials(format!(
                "Failed to get access token: {}",
                error
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
        use sha2::Sha256;

        let private_key = RsaPrivateKey::from_pkcs8_pem(private_key_pem)
            .map_err(|e| DnsError::InvalidCredentials(format!("Invalid private key: {}", e)))?;

        let signing_key = SigningKey::<Sha256>::new_unprefixed(private_key);
        let signature = signing_key.sign(message.as_bytes());

        Ok(signature.to_vec())
    }

    /// Make an authenticated request to GCP DNS API
    async fn api_request<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        path: &str,
        body: Option<&impl Serialize>,
    ) -> Result<T, DnsError> {
        let token = self.get_access_token().await?;
        let url = format!("{}{}", self.base_url, path);

        debug!("GCP DNS API request: {} {}", method, path);

        let mut request = match method {
            "GET" => self.client.get(&url),
            "POST" => self.client.post(&url),
            "DELETE" => self.client.delete(&url),
            "PATCH" => self.client.patch(&url),
            _ => {
                return Err(DnsError::ApiError(format!(
                    "Unsupported method: {}",
                    method
                )))
            }
        };

        request = request
            .header("Authorization", format!("Bearer {}", token))
            .header("Content-Type", "application/json");

        if let Some(body) = body {
            request = request.json(body);
        }

        let response = request
            .send()
            .await
            .map_err(|e| DnsError::ApiError(format!("API request failed: {}", e)))?;

        let status = response.status();

        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            return Err(DnsError::ApiError(format!(
                "GCP API returned status {}: {}",
                status, error_body
            )));
        }

        let response_text = response
            .text()
            .await
            .map_err(|e| DnsError::ApiError(format!("Failed to read response: {}", e)))?;

        if response_text.is_empty() {
            return serde_json::from_str("{}").map_err(|e| DnsError::ApiError(e.to_string()));
        }

        serde_json::from_str(&response_text).map_err(|e| {
            DnsError::ApiError(format!(
                "Failed to parse response: {} - Body: {}",
                e, response_text
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

    /// Get managed zone name for a domain
    async fn get_zone_name(&self, domain: &str) -> Result<String, DnsError> {
        let zones = self.list_zones().await?;
        let normalized = Self::normalize_domain(domain);

        for zone in zones {
            let zone_domain = Self::normalize_domain(&zone.name);
            if normalized == zone_domain || normalized.ends_with(&format!(".{}", zone_domain)) {
                return Ok(zone.id);
            }
        }

        Err(DnsError::ZoneNotFound(domain.to_string()))
    }

    /// Convert GCP record to our DnsRecord type
    fn convert_record(record: &ResourceRecordSet, zone_domain: &str) -> Vec<DnsRecord> {
        let record_type = match record.record_type.to_uppercase().as_str() {
            "A" => DnsRecordType::A,
            "AAAA" => DnsRecordType::AAAA,
            "CNAME" => DnsRecordType::CNAME,
            "TXT" => DnsRecordType::TXT,
            "MX" => DnsRecordType::MX,
            "NS" => DnsRecordType::NS,
            "SRV" => DnsRecordType::SRV,
            "CAA" => DnsRecordType::CAA,
            "PTR" => DnsRecordType::PTR,
            _ => return vec![],
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
                // Remove surrounding quotes if present
                let content = data.trim_matches('"').to_string();
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
                format!("\"{}\"", content)
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

    async fn list_zones(&self) -> Result<Vec<DnsZone>, DnsError> {
        let path = format!("/projects/{}/managedZones", self.credentials.project_id);
        let response: ManagedZonesResponse = self.api_request("GET", &path, None::<&()>).await?;

        Ok(response
            .managed_zones
            .into_iter()
            .map(|zone| DnsZone {
                id: zone.name,
                name: Self::normalize_domain(&zone.dns_name),
                status: "active".to_string(),
                nameservers: zone.name_servers,
                metadata: HashMap::new(),
            })
            .collect())
    }

    async fn get_zone(&self, domain: &str) -> Result<Option<DnsZone>, DnsError> {
        let zones = self.list_zones().await?;
        let normalized = Self::normalize_domain(domain);

        Ok(zones.into_iter().find(|z| z.name == normalized))
    }

    async fn list_records(&self, domain: &str) -> Result<Vec<DnsRecord>, DnsError> {
        let zone_name = self.get_zone_name(domain).await?;
        let zone = self
            .get_zone(domain)
            .await?
            .ok_or_else(|| DnsError::ZoneNotFound(domain.to_string()))?;

        let path = format!(
            "/projects/{}/managedZones/{}/rrsets",
            self.credentials.project_id, zone_name
        );
        let response: ResourceRecordSetsResponse =
            self.api_request("GET", &path, None::<&()>).await?;

        Ok(response
            .rrsets
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
        let records = self.list_records(domain).await?;

        Ok(records
            .into_iter()
            .find(|r| r.name == name && r.content.record_type() == record_type))
    }

    async fn create_record(
        &self,
        domain: &str,
        request: DnsRecordRequest,
    ) -> Result<DnsRecord, DnsError> {
        let zone_name = self.get_zone_name(domain).await?;
        let zone = self
            .get_zone(domain)
            .await?
            .ok_or_else(|| DnsError::ZoneNotFound(domain.to_string()))?;

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
            self.credentials.project_id, zone_name
        );
        let _: serde_json::Value = self.api_request("POST", &path, Some(&change)).await?;

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
        let zone_name = self.get_zone_name(domain).await?;
        let zone = self
            .get_zone(domain)
            .await?
            .ok_or_else(|| DnsError::ZoneNotFound(domain.to_string()))?;

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
            self.credentials.project_id, zone_name
        );
        let _: serde_json::Value = self.api_request("POST", &path, Some(&change)).await?;

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

        let fqdn = parts[0];
        let record_type_str = parts[1];

        // Get the existing record to know its value and TTL
        let records = self.list_records(domain).await?;
        let existing = records
            .iter()
            .find(|r| r.fqdn == fqdn && r.content.record_type().to_string() == record_type_str)
            .ok_or_else(|| DnsError::RecordNotFound(record_id.to_string()))?;

        let zone_name = self.get_zone_name(domain).await?;
        let data = Self::format_record_data(&existing.content);

        let change = ChangeRequest {
            additions: vec![],
            deletions: vec![ResourceRecordSet {
                name: Self::with_trailing_dot(fqdn),
                record_type: record_type_str.to_string(),
                ttl: existing.ttl,
                rrdatas: vec![data],
            }],
        };

        let path = format!(
            "/projects/{}/managedZones/{}/changes",
            self.credentials.project_id, zone_name
        );
        let _: serde_json::Value = self.api_request("POST", &path, Some(&change)).await?;

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
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
