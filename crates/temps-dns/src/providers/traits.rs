//! DNS provider trait definitions
//!
//! This module defines the core traits and types for DNS provider implementations.
//! The design is inspired by dnscontrol's provider architecture, supporting multiple
//! authentication methods and record types.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use utoipa::ToSchema;

use crate::errors::DnsError;

/// Supported DNS provider types
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum DnsProviderType {
    /// Cloudflare DNS (API Token or API Key + Email)
    Cloudflare,
    /// Namecheap DNS (API User + API Key)
    Namecheap,
    /// Route53 (AWS IAM credentials)
    Route53,
    /// DigitalOcean DNS (API Token)
    DigitalOcean,
    /// Manual DNS (user sets records manually)
    Manual,
}

impl std::fmt::Display for DnsProviderType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DnsProviderType::Cloudflare => write!(f, "cloudflare"),
            DnsProviderType::Namecheap => write!(f, "namecheap"),
            DnsProviderType::Route53 => write!(f, "route53"),
            DnsProviderType::DigitalOcean => write!(f, "digitalocean"),
            DnsProviderType::Manual => write!(f, "manual"),
        }
    }
}

impl DnsProviderType {
    pub fn from_str(s: &str) -> Result<Self, DnsError> {
        match s.to_lowercase().as_str() {
            "cloudflare" | "cf" => Ok(DnsProviderType::Cloudflare),
            "namecheap" | "nc" => Ok(DnsProviderType::Namecheap),
            "route53" | "aws" | "r53" => Ok(DnsProviderType::Route53),
            "digitalocean" | "do" => Ok(DnsProviderType::DigitalOcean),
            "manual" => Ok(DnsProviderType::Manual),
            _ => Err(DnsError::InvalidProviderType(s.to_string())),
        }
    }

    /// Returns the required credential fields for this provider type
    pub fn required_credentials(&self) -> Vec<&'static str> {
        match self {
            DnsProviderType::Cloudflare => vec!["api_token"],
            DnsProviderType::Namecheap => vec!["api_user", "api_key"],
            DnsProviderType::Route53 => vec!["access_key_id", "secret_access_key"],
            DnsProviderType::DigitalOcean => vec!["api_token"],
            DnsProviderType::Manual => vec![],
        }
    }

    /// Returns optional credential fields for this provider type
    pub fn optional_credentials(&self) -> Vec<&'static str> {
        match self {
            DnsProviderType::Cloudflare => vec!["account_id"],
            DnsProviderType::Namecheap => vec!["client_ip", "sandbox"],
            DnsProviderType::Route53 => vec!["session_token", "region"],
            DnsProviderType::DigitalOcean => vec![],
            DnsProviderType::Manual => vec![],
        }
    }
}

/// DNS record types
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "UPPERCASE")]
pub enum DnsRecordType {
    A,
    AAAA,
    CNAME,
    TXT,
    MX,
    NS,
    SRV,
    CAA,
    PTR,
}

impl std::fmt::Display for DnsRecordType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DnsRecordType::A => write!(f, "A"),
            DnsRecordType::AAAA => write!(f, "AAAA"),
            DnsRecordType::CNAME => write!(f, "CNAME"),
            DnsRecordType::TXT => write!(f, "TXT"),
            DnsRecordType::MX => write!(f, "MX"),
            DnsRecordType::NS => write!(f, "NS"),
            DnsRecordType::SRV => write!(f, "SRV"),
            DnsRecordType::CAA => write!(f, "CAA"),
            DnsRecordType::PTR => write!(f, "PTR"),
        }
    }
}

/// DNS record content - varies by record type
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", content = "value")]
pub enum DnsRecordContent {
    /// A record - IPv4 address (as string, e.g., "192.0.2.1")
    A {
        #[schema(example = "192.0.2.1")]
        address: String,
    },
    /// AAAA record - IPv6 address (as string, e.g., "2001:db8::1")
    AAAA {
        #[schema(example = "2001:db8::1")]
        address: String,
    },
    /// CNAME record - canonical name
    CNAME { target: String },
    /// TXT record - text content
    TXT { content: String },
    /// MX record - mail exchange
    MX { priority: u16, target: String },
    /// NS record - nameserver
    NS { nameserver: String },
    /// SRV record - service
    SRV {
        priority: u16,
        weight: u16,
        port: u16,
        target: String,
    },
    /// CAA record - certification authority authorization
    CAA {
        flags: u8,
        tag: String,
        value: String,
    },
    /// PTR record - pointer
    PTR { target: String },
}

impl DnsRecordContent {
    /// Get the record type for this content
    pub fn record_type(&self) -> DnsRecordType {
        match self {
            DnsRecordContent::A { .. } => DnsRecordType::A,
            DnsRecordContent::AAAA { .. } => DnsRecordType::AAAA,
            DnsRecordContent::CNAME { .. } => DnsRecordType::CNAME,
            DnsRecordContent::TXT { .. } => DnsRecordType::TXT,
            DnsRecordContent::MX { .. } => DnsRecordType::MX,
            DnsRecordContent::NS { .. } => DnsRecordType::NS,
            DnsRecordContent::SRV { .. } => DnsRecordType::SRV,
            DnsRecordContent::CAA { .. } => DnsRecordType::CAA,
            DnsRecordContent::PTR { .. } => DnsRecordType::PTR,
        }
    }

    /// Convert to string representation for display
    pub fn to_value_string(&self) -> String {
        match self {
            DnsRecordContent::A { address } | DnsRecordContent::AAAA { address } => address.clone(),
            DnsRecordContent::CNAME { target }
            | DnsRecordContent::NS { nameserver: target }
            | DnsRecordContent::PTR { target } => target.clone(),
            DnsRecordContent::TXT { content } => content.clone(),
            DnsRecordContent::MX { priority, target } => format!("{} {}", priority, target),
            DnsRecordContent::SRV {
                priority,
                weight,
                port,
                target,
            } => format!("{} {} {} {}", priority, weight, port, target),
            DnsRecordContent::CAA { flags, tag, value } => {
                format!("{} {} \"{}\"", flags, tag, value)
            }
        }
    }
}

/// A DNS record
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DnsRecord {
    /// Provider-specific record ID (if exists)
    #[schema(example = "abc123")]
    pub id: Option<String>,

    /// Zone/domain this record belongs to
    #[schema(example = "example.com")]
    pub zone: String,

    /// Record name (without zone, e.g., "www" or "@" for root)
    #[schema(example = "www")]
    pub name: String,

    /// Fully qualified domain name
    #[schema(example = "www.example.com")]
    pub fqdn: String,

    /// Record content
    pub content: DnsRecordContent,

    /// Time to live in seconds
    #[schema(example = 300)]
    pub ttl: u32,

    /// Whether this record is proxied (Cloudflare-specific)
    #[serde(default)]
    pub proxied: bool,

    /// Provider-specific metadata
    #[serde(default)]
    pub metadata: HashMap<String, String>,
}

/// Request to create or update a DNS record
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DnsRecordRequest {
    /// Record name (without zone)
    #[schema(example = "www")]
    pub name: String,

    /// Record content
    pub content: DnsRecordContent,

    /// TTL in seconds (None = auto/default)
    #[schema(example = 300)]
    pub ttl: Option<u32>,

    /// Whether to proxy through CDN (if supported)
    #[serde(default)]
    pub proxied: bool,
}

/// A DNS zone (domain managed by the provider)
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DnsZone {
    /// Provider-specific zone ID
    #[schema(example = "zone123")]
    pub id: String,

    /// Zone name (domain)
    #[schema(example = "example.com")]
    pub name: String,

    /// Zone status
    #[schema(example = "active")]
    pub status: String,

    /// Nameservers for this zone
    pub nameservers: Vec<String>,

    /// Provider-specific metadata
    #[serde(default)]
    pub metadata: HashMap<String, String>,
}

/// Capabilities of a DNS provider
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DnsProviderCapabilities {
    /// Can manage A records
    pub a_record: bool,
    /// Can manage AAAA records
    pub aaaa_record: bool,
    /// Can manage CNAME records
    pub cname_record: bool,
    /// Can manage TXT records
    pub txt_record: bool,
    /// Can manage MX records
    pub mx_record: bool,
    /// Can manage NS records
    pub ns_record: bool,
    /// Can manage SRV records
    pub srv_record: bool,
    /// Can manage CAA records
    pub caa_record: bool,
    /// Supports proxying (like Cloudflare)
    pub proxy: bool,
    /// Supports automatic SSL/TLS
    pub auto_ssl: bool,
    /// Supports wildcard records
    pub wildcard: bool,
}

/// Core DNS provider trait
///
/// All DNS providers must implement this trait to provide a unified interface
/// for managing DNS records across different providers.
#[async_trait]
pub trait DnsProvider: Send + Sync {
    /// Get the provider type
    fn provider_type(&self) -> DnsProviderType;

    /// Get provider capabilities
    fn capabilities(&self) -> DnsProviderCapabilities;

    /// Test the credentials/connection to the provider
    async fn test_connection(&self) -> Result<bool, DnsError>;

    /// List all zones (domains) managed by this provider
    async fn list_zones(&self) -> Result<Vec<DnsZone>, DnsError>;

    /// Get a specific zone by domain name
    async fn get_zone(&self, domain: &str) -> Result<Option<DnsZone>, DnsError>;

    /// Check if the provider can manage a specific domain
    async fn can_manage_domain(&self, domain: &str) -> bool {
        self.get_zone(domain).await.ok().flatten().is_some()
    }

    /// List all records in a zone
    async fn list_records(&self, domain: &str) -> Result<Vec<DnsRecord>, DnsError>;

    /// Get a specific record by name and type
    async fn get_record(
        &self,
        domain: &str,
        name: &str,
        record_type: DnsRecordType,
    ) -> Result<Option<DnsRecord>, DnsError>;

    /// Create a new DNS record
    async fn create_record(
        &self,
        domain: &str,
        request: DnsRecordRequest,
    ) -> Result<DnsRecord, DnsError>;

    /// Update an existing DNS record
    async fn update_record(
        &self,
        domain: &str,
        record_id: &str,
        request: DnsRecordRequest,
    ) -> Result<DnsRecord, DnsError>;

    /// Delete a DNS record
    async fn delete_record(&self, domain: &str, record_id: &str) -> Result<(), DnsError>;

    /// Set or update a record by name and type (upsert operation)
    ///
    /// This will create the record if it doesn't exist, or update it if it does.
    async fn set_record(
        &self,
        domain: &str,
        request: DnsRecordRequest,
    ) -> Result<DnsRecord, DnsError> {
        let record_type = request.content.record_type();
        if let Some(existing) = self.get_record(domain, &request.name, record_type).await? {
            if let Some(id) = existing.id {
                return self.update_record(domain, &id, request).await;
            }
        }
        self.create_record(domain, request).await
    }

    /// Remove a record by name and type
    async fn remove_record(
        &self,
        domain: &str,
        name: &str,
        record_type: DnsRecordType,
    ) -> Result<(), DnsError> {
        if let Some(record) = self.get_record(domain, name, record_type).await? {
            if let Some(id) = record.id {
                return self.delete_record(domain, &id).await;
            }
        }
        Ok(())
    }
}

/// Manual DNS provider that doesn't actually manage records
///
/// This provider is used when the user manages DNS manually.
/// All operations return instructions for the user instead of making API calls.
pub struct ManualDnsProvider;

impl Default for ManualDnsProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl ManualDnsProvider {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl DnsProvider for ManualDnsProvider {
    fn provider_type(&self) -> DnsProviderType {
        DnsProviderType::Manual
    }

    fn capabilities(&self) -> DnsProviderCapabilities {
        // Manual provider supports all record types conceptually
        // but doesn't actually manage them
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
        // Manual provider always "works"
        Ok(true)
    }

    async fn list_zones(&self) -> Result<Vec<DnsZone>, DnsError> {
        // Manual provider doesn't track zones
        Ok(vec![])
    }

    async fn get_zone(&self, _domain: &str) -> Result<Option<DnsZone>, DnsError> {
        Ok(None)
    }

    async fn can_manage_domain(&self, _domain: &str) -> bool {
        // Manual provider can "manage" any domain (user does the work)
        false // Return false to indicate automatic management is not available
    }

    async fn list_records(&self, _domain: &str) -> Result<Vec<DnsRecord>, DnsError> {
        Err(DnsError::NotSupported(
            "Manual DNS provider cannot list records".to_string(),
        ))
    }

    async fn get_record(
        &self,
        _domain: &str,
        _name: &str,
        _record_type: DnsRecordType,
    ) -> Result<Option<DnsRecord>, DnsError> {
        Err(DnsError::NotSupported(
            "Manual DNS provider cannot query records".to_string(),
        ))
    }

    async fn create_record(
        &self,
        _domain: &str,
        _request: DnsRecordRequest,
    ) -> Result<DnsRecord, DnsError> {
        Err(DnsError::NotSupported(
            "Manual DNS provider cannot create records - user must configure DNS manually"
                .to_string(),
        ))
    }

    async fn update_record(
        &self,
        _domain: &str,
        _record_id: &str,
        _request: DnsRecordRequest,
    ) -> Result<DnsRecord, DnsError> {
        Err(DnsError::NotSupported(
            "Manual DNS provider cannot update records - user must configure DNS manually"
                .to_string(),
        ))
    }

    async fn delete_record(&self, _domain: &str, _record_id: &str) -> Result<(), DnsError> {
        Err(DnsError::NotSupported(
            "Manual DNS provider cannot delete records - user must configure DNS manually"
                .to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ==================== DnsProviderType tests ====================

    #[test]
    fn test_provider_type_from_str() {
        assert_eq!(
            DnsProviderType::from_str("cloudflare").unwrap(),
            DnsProviderType::Cloudflare
        );
        assert_eq!(
            DnsProviderType::from_str("CF").unwrap(),
            DnsProviderType::Cloudflare
        );
        assert_eq!(
            DnsProviderType::from_str("namecheap").unwrap(),
            DnsProviderType::Namecheap
        );
        assert_eq!(
            DnsProviderType::from_str("route53").unwrap(),
            DnsProviderType::Route53
        );
        assert_eq!(
            DnsProviderType::from_str("digitalocean").unwrap(),
            DnsProviderType::DigitalOcean
        );
        assert_eq!(
            DnsProviderType::from_str("manual").unwrap(),
            DnsProviderType::Manual
        );
        assert!(DnsProviderType::from_str("invalid").is_err());
    }

    #[test]
    fn test_provider_type_from_str_aliases() {
        // Test all the aliases
        assert_eq!(
            DnsProviderType::from_str("cf").unwrap(),
            DnsProviderType::Cloudflare
        );
        assert_eq!(
            DnsProviderType::from_str("nc").unwrap(),
            DnsProviderType::Namecheap
        );
        assert_eq!(
            DnsProviderType::from_str("aws").unwrap(),
            DnsProviderType::Route53
        );
        assert_eq!(
            DnsProviderType::from_str("r53").unwrap(),
            DnsProviderType::Route53
        );
        assert_eq!(
            DnsProviderType::from_str("do").unwrap(),
            DnsProviderType::DigitalOcean
        );
    }

    #[test]
    fn test_provider_type_from_str_case_insensitive() {
        assert_eq!(
            DnsProviderType::from_str("CLOUDFLARE").unwrap(),
            DnsProviderType::Cloudflare
        );
        assert_eq!(
            DnsProviderType::from_str("CloudFlare").unwrap(),
            DnsProviderType::Cloudflare
        );
        assert_eq!(
            DnsProviderType::from_str("NAMECHEAP").unwrap(),
            DnsProviderType::Namecheap
        );
    }

    #[test]
    fn test_provider_type_display() {
        assert_eq!(DnsProviderType::Cloudflare.to_string(), "cloudflare");
        assert_eq!(DnsProviderType::Namecheap.to_string(), "namecheap");
        assert_eq!(DnsProviderType::Route53.to_string(), "route53");
        assert_eq!(DnsProviderType::DigitalOcean.to_string(), "digitalocean");
        assert_eq!(DnsProviderType::Manual.to_string(), "manual");
    }

    #[test]
    fn test_required_credentials() {
        assert_eq!(
            DnsProviderType::Cloudflare.required_credentials(),
            vec!["api_token"]
        );
        assert_eq!(
            DnsProviderType::Namecheap.required_credentials(),
            vec!["api_user", "api_key"]
        );
        assert_eq!(
            DnsProviderType::Route53.required_credentials(),
            vec!["access_key_id", "secret_access_key"]
        );
        assert_eq!(
            DnsProviderType::DigitalOcean.required_credentials(),
            vec!["api_token"]
        );
        assert!(DnsProviderType::Manual.required_credentials().is_empty());
    }

    #[test]
    fn test_optional_credentials() {
        assert_eq!(
            DnsProviderType::Cloudflare.optional_credentials(),
            vec!["account_id"]
        );
        assert_eq!(
            DnsProviderType::Namecheap.optional_credentials(),
            vec!["client_ip", "sandbox"]
        );
        assert_eq!(
            DnsProviderType::Route53.optional_credentials(),
            vec!["session_token", "region"]
        );
        assert!(DnsProviderType::DigitalOcean
            .optional_credentials()
            .is_empty());
        assert!(DnsProviderType::Manual.optional_credentials().is_empty());
    }

    // ==================== DnsRecordType tests ====================

    #[test]
    fn test_record_type_display() {
        assert_eq!(DnsRecordType::A.to_string(), "A");
        assert_eq!(DnsRecordType::AAAA.to_string(), "AAAA");
        assert_eq!(DnsRecordType::CNAME.to_string(), "CNAME");
        assert_eq!(DnsRecordType::TXT.to_string(), "TXT");
        assert_eq!(DnsRecordType::MX.to_string(), "MX");
        assert_eq!(DnsRecordType::NS.to_string(), "NS");
        assert_eq!(DnsRecordType::SRV.to_string(), "SRV");
        assert_eq!(DnsRecordType::CAA.to_string(), "CAA");
        assert_eq!(DnsRecordType::PTR.to_string(), "PTR");
    }

    // ==================== DnsRecordContent tests ====================

    #[test]
    fn test_record_content_type() {
        let a_record = DnsRecordContent::A {
            address: "1.2.3.4".to_string(),
        };
        assert_eq!(a_record.record_type(), DnsRecordType::A);

        let aaaa_record = DnsRecordContent::AAAA {
            address: "2001:db8::1".to_string(),
        };
        assert_eq!(aaaa_record.record_type(), DnsRecordType::AAAA);

        let cname_record = DnsRecordContent::CNAME {
            target: "www.example.com".to_string(),
        };
        assert_eq!(cname_record.record_type(), DnsRecordType::CNAME);

        let txt_record = DnsRecordContent::TXT {
            content: "test".to_string(),
        };
        assert_eq!(txt_record.record_type(), DnsRecordType::TXT);

        let mx_record = DnsRecordContent::MX {
            priority: 10,
            target: "mail.example.com".to_string(),
        };
        assert_eq!(mx_record.record_type(), DnsRecordType::MX);

        let ns_record = DnsRecordContent::NS {
            nameserver: "ns1.example.com".to_string(),
        };
        assert_eq!(ns_record.record_type(), DnsRecordType::NS);

        let srv_record = DnsRecordContent::SRV {
            priority: 10,
            weight: 5,
            port: 5060,
            target: "sip.example.com".to_string(),
        };
        assert_eq!(srv_record.record_type(), DnsRecordType::SRV);

        let caa_record = DnsRecordContent::CAA {
            flags: 0,
            tag: "issue".to_string(),
            value: "letsencrypt.org".to_string(),
        };
        assert_eq!(caa_record.record_type(), DnsRecordType::CAA);

        let ptr_record = DnsRecordContent::PTR {
            target: "host.example.com".to_string(),
        };
        assert_eq!(ptr_record.record_type(), DnsRecordType::PTR);
    }

    #[test]
    fn test_record_content_to_string() {
        let a_record = DnsRecordContent::A {
            address: "1.2.3.4".to_string(),
        };
        assert_eq!(a_record.to_value_string(), "1.2.3.4");

        let aaaa_record = DnsRecordContent::AAAA {
            address: "2001:db8::1".to_string(),
        };
        assert_eq!(aaaa_record.to_value_string(), "2001:db8::1");

        let cname_record = DnsRecordContent::CNAME {
            target: "www.example.com".to_string(),
        };
        assert_eq!(cname_record.to_value_string(), "www.example.com");

        let txt_record = DnsRecordContent::TXT {
            content: "v=spf1 -all".to_string(),
        };
        assert_eq!(txt_record.to_value_string(), "v=spf1 -all");

        let mx_record = DnsRecordContent::MX {
            priority: 10,
            target: "mail.example.com".to_string(),
        };
        assert_eq!(mx_record.to_value_string(), "10 mail.example.com");

        let ns_record = DnsRecordContent::NS {
            nameserver: "ns1.example.com".to_string(),
        };
        assert_eq!(ns_record.to_value_string(), "ns1.example.com");

        let srv_record = DnsRecordContent::SRV {
            priority: 10,
            weight: 5,
            port: 5060,
            target: "sip.example.com".to_string(),
        };
        assert_eq!(srv_record.to_value_string(), "10 5 5060 sip.example.com");

        let caa_record = DnsRecordContent::CAA {
            flags: 0,
            tag: "issue".to_string(),
            value: "letsencrypt.org".to_string(),
        };
        assert_eq!(caa_record.to_value_string(), "0 issue \"letsencrypt.org\"");

        let ptr_record = DnsRecordContent::PTR {
            target: "host.example.com".to_string(),
        };
        assert_eq!(ptr_record.to_value_string(), "host.example.com");
    }

    // ==================== DnsRecord tests ====================

    #[test]
    fn test_dns_record_creation() {
        let record = DnsRecord {
            id: Some("rec123".to_string()),
            zone: "example.com".to_string(),
            name: "www".to_string(),
            fqdn: "www.example.com".to_string(),
            content: DnsRecordContent::A {
                address: "192.0.2.1".to_string(),
            },
            ttl: 300,
            proxied: true,
            metadata: HashMap::new(),
        };

        assert_eq!(record.id, Some("rec123".to_string()));
        assert_eq!(record.zone, "example.com");
        assert_eq!(record.name, "www");
        assert_eq!(record.fqdn, "www.example.com");
        assert_eq!(record.ttl, 300);
        assert!(record.proxied);
    }

    #[test]
    fn test_dns_record_with_metadata() {
        let mut metadata = HashMap::new();
        metadata.insert("created_by".to_string(), "test".to_string());
        metadata.insert("priority".to_string(), "high".to_string());

        let record = DnsRecord {
            id: None,
            zone: "example.com".to_string(),
            name: "@".to_string(),
            fqdn: "example.com".to_string(),
            content: DnsRecordContent::TXT {
                content: "v=spf1 -all".to_string(),
            },
            ttl: 3600,
            proxied: false,
            metadata,
        };

        assert_eq!(record.metadata.get("created_by"), Some(&"test".to_string()));
        assert_eq!(record.metadata.get("priority"), Some(&"high".to_string()));
    }

    // ==================== DnsRecordRequest tests ====================

    #[test]
    fn test_dns_record_request() {
        let request = DnsRecordRequest {
            name: "www".to_string(),
            content: DnsRecordContent::A {
                address: "192.0.2.1".to_string(),
            },
            ttl: Some(300),
            proxied: true,
        };

        assert_eq!(request.name, "www");
        assert_eq!(request.ttl, Some(300));
        assert!(request.proxied);
    }

    #[test]
    fn test_dns_record_request_without_ttl() {
        let request = DnsRecordRequest {
            name: "@".to_string(),
            content: DnsRecordContent::TXT {
                content: "test".to_string(),
            },
            ttl: None, // Auto TTL
            proxied: false,
        };

        assert!(request.ttl.is_none());
    }

    // ==================== DnsZone tests ====================

    #[test]
    fn test_dns_zone_creation() {
        let zone = DnsZone {
            id: "zone123".to_string(),
            name: "example.com".to_string(),
            status: "active".to_string(),
            nameservers: vec!["ns1.example.com".to_string(), "ns2.example.com".to_string()],
            metadata: HashMap::new(),
        };

        assert_eq!(zone.id, "zone123");
        assert_eq!(zone.name, "example.com");
        assert_eq!(zone.status, "active");
        assert_eq!(zone.nameservers.len(), 2);
    }

    // ==================== DnsProviderCapabilities tests ====================

    #[test]
    fn test_capabilities_default() {
        let caps = DnsProviderCapabilities::default();

        assert!(!caps.a_record);
        assert!(!caps.aaaa_record);
        assert!(!caps.cname_record);
        assert!(!caps.txt_record);
        assert!(!caps.mx_record);
        assert!(!caps.ns_record);
        assert!(!caps.srv_record);
        assert!(!caps.caa_record);
        assert!(!caps.proxy);
        assert!(!caps.auto_ssl);
        assert!(!caps.wildcard);
    }

    // ==================== ManualDnsProvider tests ====================

    #[tokio::test]
    async fn test_manual_provider() {
        let provider = ManualDnsProvider::new();
        assert_eq!(provider.provider_type(), DnsProviderType::Manual);
        assert!(provider.test_connection().await.unwrap());
        assert!(!provider.can_manage_domain("example.com").await);
        assert!(provider.list_zones().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_manual_provider_default() {
        let provider = ManualDnsProvider::default();
        assert_eq!(provider.provider_type(), DnsProviderType::Manual);
    }

    #[tokio::test]
    async fn test_manual_provider_capabilities() {
        let provider = ManualDnsProvider::new();
        let caps = provider.capabilities();

        // Manual provider supports all record types conceptually
        assert!(caps.a_record);
        assert!(caps.aaaa_record);
        assert!(caps.cname_record);
        assert!(caps.txt_record);
        assert!(caps.mx_record);
        assert!(caps.ns_record);
        assert!(caps.srv_record);
        assert!(caps.caa_record);
        assert!(caps.wildcard);

        // But doesn't support proxy or auto_ssl
        assert!(!caps.proxy);
        assert!(!caps.auto_ssl);
    }

    #[tokio::test]
    async fn test_manual_provider_get_zone() {
        let provider = ManualDnsProvider::new();
        let zone = provider.get_zone("example.com").await.unwrap();
        assert!(zone.is_none());
    }

    #[tokio::test]
    async fn test_manual_provider_list_records_not_supported() {
        let provider = ManualDnsProvider::new();
        let result = provider.list_records("example.com").await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), DnsError::NotSupported(_)));
    }

    #[tokio::test]
    async fn test_manual_provider_get_record_not_supported() {
        let provider = ManualDnsProvider::new();
        let result = provider
            .get_record("example.com", "www", DnsRecordType::A)
            .await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), DnsError::NotSupported(_)));
    }

    #[tokio::test]
    async fn test_manual_provider_create_record_not_supported() {
        let provider = ManualDnsProvider::new();
        let request = DnsRecordRequest {
            name: "www".to_string(),
            content: DnsRecordContent::A {
                address: "192.0.2.1".to_string(),
            },
            ttl: Some(300),
            proxied: false,
        };

        let result = provider.create_record("example.com", request).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), DnsError::NotSupported(_)));
    }

    #[tokio::test]
    async fn test_manual_provider_update_record_not_supported() {
        let provider = ManualDnsProvider::new();
        let request = DnsRecordRequest {
            name: "www".to_string(),
            content: DnsRecordContent::A {
                address: "192.0.2.1".to_string(),
            },
            ttl: Some(300),
            proxied: false,
        };

        let result = provider
            .update_record("example.com", "rec123", request)
            .await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), DnsError::NotSupported(_)));
    }

    #[tokio::test]
    async fn test_manual_provider_delete_record_not_supported() {
        let provider = ManualDnsProvider::new();
        let result = provider.delete_record("example.com", "rec123").await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), DnsError::NotSupported(_)));
    }

    // ==================== Serialization tests ====================

    #[test]
    fn test_dns_provider_type_serialization() {
        let cloudflare = DnsProviderType::Cloudflare;
        let json = serde_json::to_string(&cloudflare).unwrap();
        assert_eq!(json, "\"cloudflare\"");

        let namecheap = DnsProviderType::Namecheap;
        let json = serde_json::to_string(&namecheap).unwrap();
        assert_eq!(json, "\"namecheap\"");
    }

    #[test]
    fn test_dns_provider_type_deserialization() {
        let cloudflare: DnsProviderType = serde_json::from_str("\"cloudflare\"").unwrap();
        assert_eq!(cloudflare, DnsProviderType::Cloudflare);

        let namecheap: DnsProviderType = serde_json::from_str("\"namecheap\"").unwrap();
        assert_eq!(namecheap, DnsProviderType::Namecheap);
    }

    #[test]
    fn test_dns_record_type_serialization() {
        let a_type = DnsRecordType::A;
        let json = serde_json::to_string(&a_type).unwrap();
        assert_eq!(json, "\"A\"");

        let mx_type = DnsRecordType::MX;
        let json = serde_json::to_string(&mx_type).unwrap();
        assert_eq!(json, "\"MX\"");
    }

    #[test]
    fn test_dns_record_content_serialization() {
        let content = DnsRecordContent::A {
            address: "192.0.2.1".to_string(),
        };
        let json = serde_json::to_string(&content).unwrap();
        assert!(json.contains("\"type\":\"A\""));
        assert!(json.contains("\"address\":\"192.0.2.1\""));

        let content = DnsRecordContent::MX {
            priority: 10,
            target: "mail.example.com".to_string(),
        };
        let json = serde_json::to_string(&content).unwrap();
        assert!(json.contains("\"type\":\"MX\""));
        assert!(json.contains("\"priority\":10"));
        assert!(json.contains("\"target\":\"mail.example.com\""));
    }

    #[test]
    fn test_dns_record_serialization_roundtrip() {
        let original = DnsRecord {
            id: Some("rec123".to_string()),
            zone: "example.com".to_string(),
            name: "www".to_string(),
            fqdn: "www.example.com".to_string(),
            content: DnsRecordContent::A {
                address: "192.0.2.1".to_string(),
            },
            ttl: 300,
            proxied: true,
            metadata: HashMap::new(),
        };

        let json = serde_json::to_string(&original).unwrap();
        let deserialized: DnsRecord = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.id, original.id);
        assert_eq!(deserialized.zone, original.zone);
        assert_eq!(deserialized.name, original.name);
        assert_eq!(deserialized.fqdn, original.fqdn);
        assert_eq!(deserialized.ttl, original.ttl);
        assert_eq!(deserialized.proxied, original.proxied);
    }
}
