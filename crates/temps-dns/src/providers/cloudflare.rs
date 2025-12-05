//! Cloudflare DNS provider implementation
//!
//! This provider uses the Cloudflare API to manage DNS records.
//! It requires an API Token with Zone:DNS:Edit permissions.

use async_trait::async_trait;
use cloudflare::endpoints::{dns, zones};
use cloudflare::framework::{
    auth::Credentials, client::async_api::Client, client::ClientConfig, Environment,
};
use std::collections::HashMap;
use tracing::{debug, info, warn};

use super::credentials::CloudflareCredentials;
use super::traits::{
    DnsProvider, DnsProviderCapabilities, DnsProviderType, DnsRecord, DnsRecordContent,
    DnsRecordRequest, DnsRecordType, DnsZone,
};
use crate::errors::DnsError;

/// Cloudflare DNS provider
pub struct CloudflareProvider {
    client: Client,
    /// Stored credentials for reference (e.g., account_id)
    #[allow(dead_code)]
    credentials: CloudflareCredentials,
}

impl CloudflareProvider {
    /// Create a new Cloudflare provider with the given credentials
    pub fn new(credentials: CloudflareCredentials) -> Result<Self, DnsError> {
        let cf_credentials = Credentials::UserAuthToken {
            token: credentials.api_token.clone(),
        };

        let client = Client::new(
            cf_credentials,
            ClientConfig::default(),
            Environment::Production,
        )
        .map_err(|e| DnsError::InvalidCredentials(format!("Failed to create client: {:?}", e)))?;

        Ok(Self {
            client,
            credentials,
        })
    }

    /// Get zone ID for a domain
    async fn get_zone_id(&self, domain: &str) -> Result<String, DnsError> {
        // Extract the base domain (last two parts)
        let base_domain = Self::extract_base_domain(domain);

        debug!("Fetching zone ID for base domain: {}", base_domain);

        let endpoint = zones::zone::ListZones {
            params: zones::zone::ListZonesParams {
                name: Some(base_domain.to_string()),
                ..Default::default()
            },
        };

        let response = self
            .client
            .request(&endpoint)
            .await
            .map_err(|e| DnsError::ApiError(format!("Failed to list zones: {:?}", e)))?;

        response
            .result
            .first()
            .map(|zone| zone.id.to_string())
            .ok_or_else(|| DnsError::ZoneNotFound(domain.to_string()))
    }

    /// Extract base domain from a full domain name
    fn extract_base_domain(domain: &str) -> String {
        domain
            .split('.')
            .rev()
            .take(2)
            .collect::<Vec<&str>>()
            .into_iter()
            .rev()
            .collect::<Vec<&str>>()
            .join(".")
    }

    /// Convert Cloudflare zone status to string
    fn status_to_string(status: &zones::zone::Status) -> String {
        match status {
            zones::zone::Status::Active => "active".to_string(),
            zones::zone::Status::Pending => "pending".to_string(),
            zones::zone::Status::Initializing => "initializing".to_string(),
            zones::zone::Status::Moved => "moved".to_string(),
            zones::zone::Status::Deleted => "deleted".to_string(),
            zones::zone::Status::Deactivated => "deactivated".to_string(),
        }
    }

    /// Convert Cloudflare DNS content to our DnsRecordContent
    fn convert_cf_content(content: &dns::dns::DnsContent) -> Option<DnsRecordContent> {
        match content {
            dns::dns::DnsContent::A { content } => Some(DnsRecordContent::A {
                address: content.to_string(),
            }),
            dns::dns::DnsContent::AAAA { content } => Some(DnsRecordContent::AAAA {
                address: content.to_string(),
            }),
            dns::dns::DnsContent::CNAME { content } => Some(DnsRecordContent::CNAME {
                target: content.clone(),
            }),
            dns::dns::DnsContent::TXT { content } => Some(DnsRecordContent::TXT {
                content: content.clone(),
            }),
            dns::dns::DnsContent::MX { content, priority } => Some(DnsRecordContent::MX {
                priority: *priority,
                target: content.clone(),
            }),
            dns::dns::DnsContent::NS { content } => Some(DnsRecordContent::NS {
                nameserver: content.clone(),
            }),
            dns::dns::DnsContent::SRV { content } => {
                // SRV records have format: priority weight port target
                let parts: Vec<&str> = content.split_whitespace().collect();
                if parts.len() >= 4 {
                    Some(DnsRecordContent::SRV {
                        priority: parts[0].parse().unwrap_or(0),
                        weight: parts[1].parse().unwrap_or(0),
                        port: parts[2].parse().unwrap_or(0),
                        target: parts[3].to_string(),
                    })
                } else {
                    None
                }
            }
        }
    }

    /// Convert our DnsRecordContent to Cloudflare DNS content
    fn to_cf_content(content: &DnsRecordContent) -> Result<dns::dns::DnsContent, DnsError> {
        match content {
            DnsRecordContent::A { address } => {
                let ip: std::net::Ipv4Addr = address.parse().map_err(|e| {
                    DnsError::Validation(format!("Invalid IPv4 address '{}': {}", address, e))
                })?;
                Ok(dns::dns::DnsContent::A { content: ip })
            }
            DnsRecordContent::AAAA { address } => {
                let ip: std::net::Ipv6Addr = address.parse().map_err(|e| {
                    DnsError::Validation(format!("Invalid IPv6 address '{}': {}", address, e))
                })?;
                Ok(dns::dns::DnsContent::AAAA { content: ip })
            }
            DnsRecordContent::CNAME { target } => Ok(dns::dns::DnsContent::CNAME {
                content: target.clone(),
            }),
            DnsRecordContent::TXT { content } => Ok(dns::dns::DnsContent::TXT {
                content: content.clone(),
            }),
            DnsRecordContent::MX { priority, target } => Ok(dns::dns::DnsContent::MX {
                priority: *priority,
                content: target.clone(),
            }),
            DnsRecordContent::NS { nameserver } => Ok(dns::dns::DnsContent::NS {
                content: nameserver.clone(),
            }),
            DnsRecordContent::SRV {
                priority,
                weight,
                port,
                target,
            } => Ok(dns::dns::DnsContent::SRV {
                content: format!("{} {} {} {}", priority, weight, port, target),
            }),
            // CAA and PTR are not directly supported by the cloudflare crate
            DnsRecordContent::CAA { .. } => Err(DnsError::NotSupported(
                "CAA records are not directly supported via Cloudflare API client".to_string(),
            )),
            DnsRecordContent::PTR { .. } => Err(DnsError::NotSupported(
                "PTR records are not directly supported via Cloudflare API client".to_string(),
            )),
        }
    }

    /// Convert Cloudflare record to our DnsRecord
    fn convert_cf_record(record: &dns::dns::DnsRecord, zone_name: &str) -> Option<DnsRecord> {
        let content = Self::convert_cf_content(&record.content)?;

        // Extract the subdomain name from FQDN
        let name = if record.name == zone_name {
            "@".to_string()
        } else {
            record
                .name
                .strip_suffix(&format!(".{}", zone_name))
                .unwrap_or(&record.name)
                .to_string()
        };

        Some(DnsRecord {
            id: Some(record.id.clone()),
            zone: zone_name.to_string(),
            name,
            fqdn: record.name.clone(),
            content,
            ttl: record.ttl,
            proxied: record.proxied,
            metadata: HashMap::new(),
        })
    }

    /// Get the matching record type filter for Cloudflare API
    fn record_type_to_cf_content(record_type: DnsRecordType) -> dns::dns::DnsContent {
        match record_type {
            DnsRecordType::A => dns::dns::DnsContent::A {
                content: std::net::Ipv4Addr::new(0, 0, 0, 0),
            },
            DnsRecordType::AAAA => dns::dns::DnsContent::AAAA {
                content: std::net::Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, 0),
            },
            DnsRecordType::CNAME => dns::dns::DnsContent::CNAME {
                content: String::new(),
            },
            DnsRecordType::TXT => dns::dns::DnsContent::TXT {
                content: String::new(),
            },
            DnsRecordType::MX => dns::dns::DnsContent::MX {
                priority: 0,
                content: String::new(),
            },
            DnsRecordType::NS => dns::dns::DnsContent::NS {
                content: String::new(),
            },
            DnsRecordType::SRV => dns::dns::DnsContent::SRV {
                content: String::new(),
            },
            // CAA and PTR not directly supported by cloudflare crate,
            // return TXT as fallback (will be filtered by name anyway)
            DnsRecordType::CAA | DnsRecordType::PTR => dns::dns::DnsContent::TXT {
                content: String::new(),
            },
        }
    }
}

#[async_trait]
impl DnsProvider for CloudflareProvider {
    fn provider_type(&self) -> DnsProviderType {
        DnsProviderType::Cloudflare
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
            caa_record: false, // Not directly supported by cloudflare crate
            proxy: true,
            auto_ssl: true,
            wildcard: true,
        }
    }

    async fn test_connection(&self) -> Result<bool, DnsError> {
        match self.list_zones().await {
            Ok(_) => {
                info!("Cloudflare API connection test successful");
                Ok(true)
            }
            Err(e) => {
                warn!("Cloudflare API connection test failed: {}", e);
                Ok(false)
            }
        }
    }

    async fn list_zones(&self) -> Result<Vec<DnsZone>, DnsError> {
        let endpoint = zones::zone::ListZones {
            params: Default::default(),
        };

        let response = self
            .client
            .request(&endpoint)
            .await
            .map_err(|e| DnsError::ApiError(format!("Failed to list zones: {:?}", e)))?;

        Ok(response
            .result
            .into_iter()
            .map(|zone| DnsZone {
                id: zone.id,
                name: zone.name,
                status: Self::status_to_string(&zone.status),
                nameservers: zone.name_servers,
                metadata: HashMap::new(),
            })
            .collect())
    }

    async fn get_zone(&self, domain: &str) -> Result<Option<DnsZone>, DnsError> {
        let base_domain = Self::extract_base_domain(domain);

        let endpoint = zones::zone::ListZones {
            params: zones::zone::ListZonesParams {
                name: Some(base_domain.clone()),
                ..Default::default()
            },
        };

        let response = self
            .client
            .request(&endpoint)
            .await
            .map_err(|e| DnsError::ApiError(format!("Failed to get zone: {:?}", e)))?;

        Ok(response.result.into_iter().next().map(|zone| DnsZone {
            id: zone.id,
            name: zone.name,
            status: Self::status_to_string(&zone.status),
            nameservers: zone.name_servers,
            metadata: HashMap::new(),
        }))
    }

    async fn list_records(&self, domain: &str) -> Result<Vec<DnsRecord>, DnsError> {
        let zone_id = self.get_zone_id(domain).await?;
        let zone = self
            .get_zone(domain)
            .await?
            .ok_or_else(|| DnsError::ZoneNotFound(domain.to_string()))?;

        let endpoint = dns::dns::ListDnsRecords {
            zone_identifier: &zone_id,
            params: Default::default(),
        };

        let response = self
            .client
            .request(&endpoint)
            .await
            .map_err(|e| DnsError::ApiError(format!("Failed to list records: {:?}", e)))?;

        Ok(response
            .result
            .iter()
            .filter_map(|record| Self::convert_cf_record(record, &zone.name))
            .collect())
    }

    async fn get_record(
        &self,
        domain: &str,
        name: &str,
        record_type: DnsRecordType,
    ) -> Result<Option<DnsRecord>, DnsError> {
        let zone_id = self.get_zone_id(domain).await?;
        let zone = self
            .get_zone(domain)
            .await?
            .ok_or_else(|| DnsError::ZoneNotFound(domain.to_string()))?;

        // Build FQDN for the search
        let fqdn = if name == "@" || name.is_empty() {
            zone.name.clone()
        } else {
            format!("{}.{}", name, zone.name)
        };

        let endpoint = dns::dns::ListDnsRecords {
            zone_identifier: &zone_id,
            params: dns::dns::ListDnsRecordsParams {
                name: Some(fqdn),
                record_type: Some(Self::record_type_to_cf_content(record_type)),
                ..Default::default()
            },
        };

        let response = self
            .client
            .request(&endpoint)
            .await
            .map_err(|e| DnsError::ApiError(format!("Failed to get record: {:?}", e)))?;

        Ok(response
            .result
            .first()
            .and_then(|record| Self::convert_cf_record(record, &zone.name)))
    }

    async fn create_record(
        &self,
        domain: &str,
        request: DnsRecordRequest,
    ) -> Result<DnsRecord, DnsError> {
        let zone_id = self.get_zone_id(domain).await?;
        let zone = self
            .get_zone(domain)
            .await?
            .ok_or_else(|| DnsError::ZoneNotFound(domain.to_string()))?;

        let cf_content = Self::to_cf_content(&request.content)?;

        // Determine the name to use
        let record_name = if request.name == "@" || request.name.is_empty() {
            &zone.name
        } else {
            &request.name
        };

        let params = dns::dns::CreateDnsRecordParams {
            name: record_name,
            content: cf_content,
            ttl: request.ttl,
            priority: None,
            proxied: Some(request.proxied),
        };

        let endpoint = dns::dns::CreateDnsRecord {
            zone_identifier: &zone_id,
            params,
        };

        let response = self
            .client
            .request(&endpoint)
            .await
            .map_err(|e| DnsError::ApiError(format!("Failed to create record: {:?}", e)))?;

        Self::convert_cf_record(&response.result, &zone.name)
            .ok_or_else(|| DnsError::ApiError("Failed to convert created record".to_string()))
    }

    async fn update_record(
        &self,
        domain: &str,
        record_id: &str,
        request: DnsRecordRequest,
    ) -> Result<DnsRecord, DnsError> {
        let zone_id = self.get_zone_id(domain).await?;
        let zone = self
            .get_zone(domain)
            .await?
            .ok_or_else(|| DnsError::ZoneNotFound(domain.to_string()))?;

        let cf_content = Self::to_cf_content(&request.content)?;

        let record_name = if request.name == "@" || request.name.is_empty() {
            &zone.name
        } else {
            &request.name
        };

        let params = dns::dns::UpdateDnsRecordParams {
            name: record_name,
            content: cf_content,
            ttl: Some(request.ttl.unwrap_or(1)), // 1 = auto in Cloudflare
            proxied: Some(request.proxied),
        };

        let endpoint = dns::dns::UpdateDnsRecord {
            zone_identifier: &zone_id,
            identifier: record_id,
            params,
        };

        let response = self
            .client
            .request(&endpoint)
            .await
            .map_err(|e| DnsError::ApiError(format!("Failed to update record: {:?}", e)))?;

        Self::convert_cf_record(&response.result, &zone.name)
            .ok_or_else(|| DnsError::ApiError("Failed to convert updated record".to_string()))
    }

    async fn delete_record(&self, domain: &str, record_id: &str) -> Result<(), DnsError> {
        let zone_id = self.get_zone_id(domain).await?;

        let endpoint = dns::dns::DeleteDnsRecord {
            zone_identifier: &zone_id,
            identifier: record_id,
        };

        self.client
            .request(&endpoint)
            .await
            .map_err(|e| DnsError::ApiError(format!("Failed to delete record: {:?}", e)))?;

        info!("Deleted DNS record {} from zone {}", record_id, zone_id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ==================== Helper function tests ====================

    #[test]
    fn test_extract_base_domain() {
        assert_eq!(
            CloudflareProvider::extract_base_domain("example.com"),
            "example.com"
        );
        assert_eq!(
            CloudflareProvider::extract_base_domain("sub.example.com"),
            "example.com"
        );
        assert_eq!(
            CloudflareProvider::extract_base_domain("deep.sub.example.com"),
            "example.com"
        );
    }

    #[test]
    fn test_extract_base_domain_single_part() {
        // Edge case: single part domain
        assert_eq!(
            CloudflareProvider::extract_base_domain("localhost"),
            "localhost"
        );
    }

    #[test]
    fn test_extract_base_domain_tld_only() {
        // Edge case: only two parts
        assert_eq!(CloudflareProvider::extract_base_domain("co.uk"), "co.uk");
    }

    // ==================== Status conversion tests ====================

    #[test]
    fn test_status_to_string() {
        assert_eq!(
            CloudflareProvider::status_to_string(&zones::zone::Status::Active),
            "active"
        );
        assert_eq!(
            CloudflareProvider::status_to_string(&zones::zone::Status::Pending),
            "pending"
        );
        assert_eq!(
            CloudflareProvider::status_to_string(&zones::zone::Status::Initializing),
            "initializing"
        );
        assert_eq!(
            CloudflareProvider::status_to_string(&zones::zone::Status::Moved),
            "moved"
        );
        assert_eq!(
            CloudflareProvider::status_to_string(&zones::zone::Status::Deleted),
            "deleted"
        );
        assert_eq!(
            CloudflareProvider::status_to_string(&zones::zone::Status::Deactivated),
            "deactivated"
        );
    }

    // ==================== Record type conversion tests ====================

    #[test]
    fn test_record_type_conversion() {
        // Test that we can convert record types to CF content types
        let a_content = CloudflareProvider::record_type_to_cf_content(DnsRecordType::A);
        assert!(matches!(a_content, dns::dns::DnsContent::A { .. }));

        let txt_content = CloudflareProvider::record_type_to_cf_content(DnsRecordType::TXT);
        assert!(matches!(txt_content, dns::dns::DnsContent::TXT { .. }));
    }

    #[test]
    fn test_record_type_to_cf_content_all_types() {
        // A record
        let content = CloudflareProvider::record_type_to_cf_content(DnsRecordType::A);
        assert!(matches!(content, dns::dns::DnsContent::A { .. }));

        // AAAA record
        let content = CloudflareProvider::record_type_to_cf_content(DnsRecordType::AAAA);
        assert!(matches!(content, dns::dns::DnsContent::AAAA { .. }));

        // CNAME record
        let content = CloudflareProvider::record_type_to_cf_content(DnsRecordType::CNAME);
        assert!(matches!(content, dns::dns::DnsContent::CNAME { .. }));

        // TXT record
        let content = CloudflareProvider::record_type_to_cf_content(DnsRecordType::TXT);
        assert!(matches!(content, dns::dns::DnsContent::TXT { .. }));

        // MX record
        let content = CloudflareProvider::record_type_to_cf_content(DnsRecordType::MX);
        assert!(matches!(content, dns::dns::DnsContent::MX { .. }));

        // NS record
        let content = CloudflareProvider::record_type_to_cf_content(DnsRecordType::NS);
        assert!(matches!(content, dns::dns::DnsContent::NS { .. }));

        // SRV record
        let content = CloudflareProvider::record_type_to_cf_content(DnsRecordType::SRV);
        assert!(matches!(content, dns::dns::DnsContent::SRV { .. }));

        // CAA and PTR fall back to TXT
        let content = CloudflareProvider::record_type_to_cf_content(DnsRecordType::CAA);
        assert!(matches!(content, dns::dns::DnsContent::TXT { .. }));

        let content = CloudflareProvider::record_type_to_cf_content(DnsRecordType::PTR);
        assert!(matches!(content, dns::dns::DnsContent::TXT { .. }));
    }

    // ==================== to_cf_content tests ====================

    #[test]
    fn test_to_cf_content_a_record() {
        let content = DnsRecordContent::A {
            address: "1.2.3.4".to_string(),
        };
        let cf_content = CloudflareProvider::to_cf_content(&content).unwrap();
        assert!(matches!(cf_content, dns::dns::DnsContent::A { .. }));
    }

    #[test]
    fn test_to_cf_content_a_record_invalid_ip() {
        let content = DnsRecordContent::A {
            address: "not-an-ip".to_string(),
        };
        let result = CloudflareProvider::to_cf_content(&content);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), DnsError::Validation(_)));
    }

    #[test]
    fn test_to_cf_content_aaaa_record() {
        let content = DnsRecordContent::AAAA {
            address: "2001:db8::1".to_string(),
        };
        let cf_content = CloudflareProvider::to_cf_content(&content).unwrap();
        assert!(matches!(cf_content, dns::dns::DnsContent::AAAA { .. }));
    }

    #[test]
    fn test_to_cf_content_aaaa_record_invalid_ip() {
        let content = DnsRecordContent::AAAA {
            address: "not-an-ipv6".to_string(),
        };
        let result = CloudflareProvider::to_cf_content(&content);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), DnsError::Validation(_)));
    }

    #[test]
    fn test_to_cf_content_cname_record() {
        let content = DnsRecordContent::CNAME {
            target: "www.example.com".to_string(),
        };
        let cf_content = CloudflareProvider::to_cf_content(&content).unwrap();
        match cf_content {
            dns::dns::DnsContent::CNAME { content } => {
                assert_eq!(content, "www.example.com");
            }
            _ => panic!("Expected CNAME content"),
        }
    }

    #[test]
    fn test_to_cf_content_txt_record() {
        let content = DnsRecordContent::TXT {
            content: "v=spf1 include:_spf.google.com ~all".to_string(),
        };
        let cf_content = CloudflareProvider::to_cf_content(&content).unwrap();
        match cf_content {
            dns::dns::DnsContent::TXT { content } => {
                assert_eq!(content, "v=spf1 include:_spf.google.com ~all");
            }
            _ => panic!("Expected TXT content"),
        }
    }

    #[test]
    fn test_to_cf_content_txt_record_empty() {
        let content = DnsRecordContent::TXT {
            content: "".to_string(),
        };
        let cf_content = CloudflareProvider::to_cf_content(&content).unwrap();
        match cf_content {
            dns::dns::DnsContent::TXT { content } => {
                assert_eq!(content, "");
            }
            _ => panic!("Expected TXT content"),
        }
    }

    #[test]
    fn test_to_cf_content_mx_record() {
        let content = DnsRecordContent::MX {
            priority: 10,
            target: "mail.example.com".to_string(),
        };
        let cf_content = CloudflareProvider::to_cf_content(&content).unwrap();
        match cf_content {
            dns::dns::DnsContent::MX { priority, content } => {
                assert_eq!(priority, 10);
                assert_eq!(content, "mail.example.com");
            }
            _ => panic!("Expected MX content"),
        }
    }

    #[test]
    fn test_to_cf_content_mx_record_high_priority() {
        let content = DnsRecordContent::MX {
            priority: 65535,
            target: "backup.example.com".to_string(),
        };
        let cf_content = CloudflareProvider::to_cf_content(&content).unwrap();
        match cf_content {
            dns::dns::DnsContent::MX { priority, content } => {
                assert_eq!(priority, 65535);
                assert_eq!(content, "backup.example.com");
            }
            _ => panic!("Expected MX content"),
        }
    }

    #[test]
    fn test_to_cf_content_ns_record() {
        let content = DnsRecordContent::NS {
            nameserver: "ns1.example.com".to_string(),
        };
        let cf_content = CloudflareProvider::to_cf_content(&content).unwrap();
        match cf_content {
            dns::dns::DnsContent::NS { content } => {
                assert_eq!(content, "ns1.example.com");
            }
            _ => panic!("Expected NS content"),
        }
    }

    #[test]
    fn test_to_cf_content_srv_record() {
        let content = DnsRecordContent::SRV {
            priority: 10,
            weight: 5,
            port: 5060,
            target: "sip.example.com".to_string(),
        };
        let cf_content = CloudflareProvider::to_cf_content(&content).unwrap();
        match cf_content {
            dns::dns::DnsContent::SRV { content } => {
                assert_eq!(content, "10 5 5060 sip.example.com");
            }
            _ => panic!("Expected SRV content"),
        }
    }

    #[test]
    fn test_to_cf_content_caa_not_supported() {
        let content = DnsRecordContent::CAA {
            flags: 0,
            tag: "issue".to_string(),
            value: "letsencrypt.org".to_string(),
        };
        let result = CloudflareProvider::to_cf_content(&content);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), DnsError::NotSupported(_)));
    }

    #[test]
    fn test_to_cf_content_ptr_not_supported() {
        let content = DnsRecordContent::PTR {
            target: "host.example.com".to_string(),
        };
        let result = CloudflareProvider::to_cf_content(&content);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), DnsError::NotSupported(_)));
    }

    // ==================== convert_cf_content tests ====================

    #[test]
    fn test_convert_cf_content_a_record() {
        let cf_content = dns::dns::DnsContent::A {
            content: std::net::Ipv4Addr::new(192, 0, 2, 1),
        };
        let result = CloudflareProvider::convert_cf_content(&cf_content);
        assert!(result.is_some());
        if let Some(DnsRecordContent::A { address }) = result {
            assert_eq!(address, "192.0.2.1");
        } else {
            panic!("Expected A record");
        }
    }

    #[test]
    fn test_convert_cf_content_aaaa_record() {
        let cf_content = dns::dns::DnsContent::AAAA {
            content: "2001:db8::1".parse().unwrap(),
        };
        let result = CloudflareProvider::convert_cf_content(&cf_content);
        assert!(result.is_some());
        if let Some(DnsRecordContent::AAAA { address }) = result {
            assert_eq!(address, "2001:db8::1");
        } else {
            panic!("Expected AAAA record");
        }
    }

    #[test]
    fn test_convert_cf_content_cname_record() {
        let cf_content = dns::dns::DnsContent::CNAME {
            content: "www.example.com".to_string(),
        };
        let result = CloudflareProvider::convert_cf_content(&cf_content);
        assert!(result.is_some());
        if let Some(DnsRecordContent::CNAME { target }) = result {
            assert_eq!(target, "www.example.com");
        } else {
            panic!("Expected CNAME record");
        }
    }

    #[test]
    fn test_convert_cf_content_txt_record() {
        let cf_content = dns::dns::DnsContent::TXT {
            content: "v=spf1 -all".to_string(),
        };
        let result = CloudflareProvider::convert_cf_content(&cf_content);
        assert!(result.is_some());
        if let Some(DnsRecordContent::TXT { content }) = result {
            assert_eq!(content, "v=spf1 -all");
        } else {
            panic!("Expected TXT record");
        }
    }

    #[test]
    fn test_convert_cf_content_mx_record() {
        let cf_content = dns::dns::DnsContent::MX {
            priority: 10,
            content: "mail.example.com".to_string(),
        };
        let result = CloudflareProvider::convert_cf_content(&cf_content);
        assert!(result.is_some());
        if let Some(DnsRecordContent::MX { priority, target }) = result {
            assert_eq!(priority, 10);
            assert_eq!(target, "mail.example.com");
        } else {
            panic!("Expected MX record");
        }
    }

    #[test]
    fn test_convert_cf_content_ns_record() {
        let cf_content = dns::dns::DnsContent::NS {
            content: "ns1.example.com".to_string(),
        };
        let result = CloudflareProvider::convert_cf_content(&cf_content);
        assert!(result.is_some());
        if let Some(DnsRecordContent::NS { nameserver }) = result {
            assert_eq!(nameserver, "ns1.example.com");
        } else {
            panic!("Expected NS record");
        }
    }

    #[test]
    fn test_convert_cf_content_srv_record() {
        let cf_content = dns::dns::DnsContent::SRV {
            content: "10 5 5060 sip.example.com".to_string(),
        };
        let result = CloudflareProvider::convert_cf_content(&cf_content);
        assert!(result.is_some());
        if let Some(DnsRecordContent::SRV {
            priority,
            weight,
            port,
            target,
        }) = result
        {
            assert_eq!(priority, 10);
            assert_eq!(weight, 5);
            assert_eq!(port, 5060);
            assert_eq!(target, "sip.example.com");
        } else {
            panic!("Expected SRV record");
        }
    }

    #[test]
    fn test_convert_cf_content_srv_record_invalid_format() {
        // SRV with less than 4 parts should return None
        let cf_content = dns::dns::DnsContent::SRV {
            content: "10 5".to_string(),
        };
        let result = CloudflareProvider::convert_cf_content(&cf_content);
        assert!(result.is_none());
    }

    // ==================== Capabilities tests ====================

    #[test]
    fn test_cloudflare_credentials_creation() {
        let creds = CloudflareCredentials {
            api_token: "test_token".to_string(),
            account_id: Some("account123".to_string()),
        };

        assert_eq!(creds.api_token, "test_token");
        assert_eq!(creds.account_id, Some("account123".to_string()));
    }

    #[test]
    fn test_cloudflare_credentials_without_account_id() {
        let creds = CloudflareCredentials {
            api_token: "test_token".to_string(),
            account_id: None,
        };

        assert_eq!(creds.api_token, "test_token");
        assert!(creds.account_id.is_none());
    }

    // ==================== Provider instance tests ====================

    #[test]
    fn test_cloudflare_provider_creation_success() {
        let creds = CloudflareCredentials {
            api_token: "test_token_12345".to_string(),
            account_id: None,
        };

        let result = CloudflareProvider::new(creds);
        assert!(result.is_ok());
    }

    #[test]
    fn test_cloudflare_provider_type() {
        let creds = CloudflareCredentials {
            api_token: "test_token".to_string(),
            account_id: None,
        };
        let provider = CloudflareProvider::new(creds).unwrap();

        assert_eq!(provider.provider_type(), DnsProviderType::Cloudflare);
    }

    #[test]
    fn test_cloudflare_capabilities() {
        let creds = CloudflareCredentials {
            api_token: "test_token".to_string(),
            account_id: None,
        };
        let provider = CloudflareProvider::new(creds).unwrap();

        let caps = provider.capabilities();

        assert!(caps.a_record);
        assert!(caps.aaaa_record);
        assert!(caps.cname_record);
        assert!(caps.txt_record);
        assert!(caps.mx_record);
        assert!(caps.ns_record);
        assert!(caps.srv_record);
        assert!(!caps.caa_record); // Not directly supported by cloudflare crate
        assert!(caps.proxy); // Cloudflare supports proxying
        assert!(caps.auto_ssl);
        assert!(caps.wildcard);
    }

    // ==================== Round-trip conversion tests ====================

    #[test]
    fn test_a_record_roundtrip() {
        let original = DnsRecordContent::A {
            address: "192.0.2.1".to_string(),
        };

        let cf_content = CloudflareProvider::to_cf_content(&original).unwrap();
        let back = CloudflareProvider::convert_cf_content(&cf_content).unwrap();

        if let DnsRecordContent::A { address } = back {
            assert_eq!(address, "192.0.2.1");
        } else {
            panic!("Expected A record");
        }
    }

    #[test]
    fn test_aaaa_record_roundtrip() {
        let original = DnsRecordContent::AAAA {
            address: "2001:db8::1".to_string(),
        };

        let cf_content = CloudflareProvider::to_cf_content(&original).unwrap();
        let back = CloudflareProvider::convert_cf_content(&cf_content).unwrap();

        if let DnsRecordContent::AAAA { address } = back {
            assert_eq!(address, "2001:db8::1");
        } else {
            panic!("Expected AAAA record");
        }
    }

    #[test]
    fn test_txt_record_roundtrip() {
        let original = DnsRecordContent::TXT {
            content: "v=spf1 include:_spf.google.com ~all".to_string(),
        };

        let cf_content = CloudflareProvider::to_cf_content(&original).unwrap();
        let back = CloudflareProvider::convert_cf_content(&cf_content).unwrap();

        if let DnsRecordContent::TXT { content } = back {
            assert_eq!(content, "v=spf1 include:_spf.google.com ~all");
        } else {
            panic!("Expected TXT record");
        }
    }

    #[test]
    fn test_mx_record_roundtrip() {
        let original = DnsRecordContent::MX {
            priority: 10,
            target: "mail.example.com".to_string(),
        };

        let cf_content = CloudflareProvider::to_cf_content(&original).unwrap();
        let back = CloudflareProvider::convert_cf_content(&cf_content).unwrap();

        if let DnsRecordContent::MX { priority, target } = back {
            assert_eq!(priority, 10);
            assert_eq!(target, "mail.example.com");
        } else {
            panic!("Expected MX record");
        }
    }

    #[test]
    fn test_cname_record_roundtrip() {
        let original = DnsRecordContent::CNAME {
            target: "www.example.com".to_string(),
        };

        let cf_content = CloudflareProvider::to_cf_content(&original).unwrap();
        let back = CloudflareProvider::convert_cf_content(&cf_content).unwrap();

        if let DnsRecordContent::CNAME { target } = back {
            assert_eq!(target, "www.example.com");
        } else {
            panic!("Expected CNAME record");
        }
    }

    #[test]
    fn test_ns_record_roundtrip() {
        let original = DnsRecordContent::NS {
            nameserver: "ns1.example.com".to_string(),
        };

        let cf_content = CloudflareProvider::to_cf_content(&original).unwrap();
        let back = CloudflareProvider::convert_cf_content(&cf_content).unwrap();

        if let DnsRecordContent::NS { nameserver } = back {
            assert_eq!(nameserver, "ns1.example.com");
        } else {
            panic!("Expected NS record");
        }
    }
}
