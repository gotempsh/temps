//! DigitalOcean DNS provider implementation
//!
//! This provider uses the DigitalOcean API to manage DNS records.
//! It requires a Personal Access Token with read/write scope.
//!
//! Create token at: https://cloud.digitalocean.com/account/api/tokens

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::{debug, info, warn};

use super::credentials::DigitalOceanCredentials;
use super::traits::{
    DnsProvider, DnsProviderCapabilities, DnsProviderType, DnsRecord, DnsRecordContent,
    DnsRecordRequest, DnsRecordType, DnsZone,
};
use crate::errors::DnsError;

const DO_API_BASE: &str = "https://api.digitalocean.com/v2";

/// DigitalOcean DNS provider
pub struct DigitalOceanProvider {
    client: Client,
    credentials: DigitalOceanCredentials,
    #[allow(dead_code)]
    base_url: String,
}

/// DigitalOcean API response structures
#[derive(Debug, Deserialize)]
struct DomainsResponse {
    domains: Vec<DoDomain>,
}

#[derive(Debug, Deserialize)]
struct DoDomain {
    name: String,
    ttl: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct DomainRecordsResponse {
    domain_records: Vec<DoDomainRecord>,
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
        })
    }

    /// Make an authenticated request to DigitalOcean API
    async fn api_request<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        path: &str,
        body: Option<&impl Serialize>,
    ) -> Result<T, DnsError> {
        let url = format!("{}{}", self.base_url, path);

        debug!("DigitalOcean API request: {} {}", method, path);

        let mut request = match method {
            "GET" => self.client.get(&url),
            "POST" => self.client.post(&url),
            "PUT" => self.client.put(&url),
            "DELETE" => self.client.delete(&url),
            _ => {
                return Err(DnsError::ApiError(format!(
                    "Unsupported method: {}",
                    method
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

        let response = request
            .send()
            .await
            .map_err(|e| DnsError::ApiError(format!("API request failed: {}", e)))?;

        let status = response.status();

        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            if let Ok(error) = serde_json::from_str::<DoErrorResponse>(&error_body) {
                return Err(DnsError::ApiError(format!(
                    "DigitalOcean API error ({}): {}",
                    error.id, error.message
                )));
            }
            return Err(DnsError::ApiError(format!(
                "API returned status {}: {}",
                status, error_body
            )));
        }

        let response_text = response
            .text()
            .await
            .map_err(|e| DnsError::ApiError(format!("Failed to read response: {}", e)))?;

        if response_text.is_empty() {
            // For DELETE requests that return no content
            return serde_json::from_str("{}").map_err(|e| DnsError::ApiError(e.to_string()));
        }

        serde_json::from_str(&response_text).map_err(|e| {
            DnsError::ApiError(format!(
                "Failed to parse response: {} - Body: {}",
                e, response_text
            ))
        })
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
            return Err(DnsError::ApiError(format!(
                "API returned status {}: {}",
                status, error_body
            )));
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

    async fn list_zones(&self) -> Result<Vec<DnsZone>, DnsError> {
        let response: DomainsResponse = self.api_request("GET", "/domains", None::<&()>).await?;

        Ok(response
            .domains
            .into_iter()
            .map(|d| DnsZone {
                id: d.name.clone(),
                name: d.name,
                status: "active".to_string(),
                nameservers: vec![
                    "ns1.digitalocean.com".to_string(),
                    "ns2.digitalocean.com".to_string(),
                    "ns3.digitalocean.com".to_string(),
                ],
                metadata: HashMap::new(),
            })
            .collect())
    }

    async fn get_zone(&self, domain: &str) -> Result<Option<DnsZone>, DnsError> {
        let zones = self.list_zones().await?;
        Ok(zones.into_iter().find(|z| z.name == domain))
    }

    async fn list_records(&self, domain: &str) -> Result<Vec<DnsRecord>, DnsError> {
        let path = format!("/domains/{}/records", domain);
        let response: DomainRecordsResponse = self.api_request("GET", &path, None::<&()>).await?;

        Ok(response
            .domain_records
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
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn create_mock_provider(mock_server: &MockServer) -> DigitalOceanProvider {
        let creds = DigitalOceanCredentials {
            api_token: "test_token_12345".to_string(),
        };

        DigitalOceanProvider::with_base_url(creds, mock_server.uri()).unwrap()
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
            .and(path("/domains"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "domains": [
                    {"name": "example.com", "ttl": 1800},
                    {"name": "test.org", "ttl": 3600}
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
