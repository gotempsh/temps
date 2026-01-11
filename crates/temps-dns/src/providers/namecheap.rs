//! Namecheap DNS provider implementation
//!
//! This provider uses the Namecheap API to manage DNS records.
//! Namecheap's DNS API works differently from Cloudflare:
//! - DNS records are managed per-domain, not individually
//! - You must send ALL records when updating (replace operation)
//! - Rate limits: ~20 calls/min, 700/hour, 8000/day

use async_trait::async_trait;
use reqwest::Client;
use std::collections::HashMap;
use tracing::{debug, info, warn};

use super::credentials::NamecheapCredentials;
use super::traits::{
    DnsProvider, DnsProviderCapabilities, DnsProviderType, DnsRecord, DnsRecordContent,
    DnsRecordRequest, DnsRecordType, DnsZone,
};
use crate::errors::DnsError;

const NAMECHEAP_API_URL: &str = "https://api.namecheap.com/xml.response";
const NAMECHEAP_SANDBOX_URL: &str = "https://api.sandbox.namecheap.com/xml.response";

/// Namecheap DNS provider
pub struct NamecheapProvider {
    client: Client,
    credentials: NamecheapCredentials,
    base_url: String,
}

impl NamecheapProvider {
    /// Create a new Namecheap provider with the given credentials
    pub fn new(credentials: NamecheapCredentials) -> Result<Self, DnsError> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| DnsError::ApiError(format!("Failed to create HTTP client: {}", e)))?;

        let base_url = if credentials.sandbox {
            NAMECHEAP_SANDBOX_URL.to_string()
        } else {
            NAMECHEAP_API_URL.to_string()
        };

        Ok(Self {
            client,
            credentials,
            base_url,
        })
    }

    /// Get the client IP to use for API requests
    async fn get_client_ip(&self) -> Result<String, DnsError> {
        if let Some(ip) = &self.credentials.client_ip {
            return Ok(ip.clone());
        }

        // Try to get public IP from a service
        let response = self
            .client
            .get("https://api.ipify.org")
            .send()
            .await
            .map_err(|e| DnsError::ApiError(format!("Failed to get public IP: {}", e)))?;

        response
            .text()
            .await
            .map_err(|e| DnsError::ApiError(format!("Failed to read IP response: {}", e)))
    }

    /// Make an API request to Namecheap
    async fn api_request(
        &self,
        command: &str,
        params: &[(&str, &str)],
    ) -> Result<String, DnsError> {
        let client_ip = self.get_client_ip().await?;

        let mut query_params: Vec<(&str, &str)> = vec![
            ("ApiUser", &self.credentials.api_user),
            ("ApiKey", &self.credentials.api_key),
            ("UserName", &self.credentials.api_user),
            ("ClientIp", &client_ip),
            ("Command", command),
        ];

        query_params.extend(params);

        debug!(
            "Namecheap API request: {} with params: {:?}",
            command, params
        );

        let response = self
            .client
            .get(&self.base_url)
            .query(&query_params)
            .send()
            .await
            .map_err(|e| DnsError::ApiError(format!("API request failed: {}", e)))?;

        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|e| DnsError::ApiError(format!("Failed to read response: {}", e)))?;

        if !status.is_success() {
            return Err(DnsError::ApiError(format!(
                "API returned status {}: {}",
                status, body
            )));
        }

        // Check for API errors in the XML response
        if body.contains("<Errors>") && body.contains("<Error ") {
            // Extract error message
            if let Some(error_start) = body.find("<Error ") {
                if let Some(error_end) = body[error_start..].find("</Error>") {
                    let error_xml = &body[error_start..error_start + error_end + 8];
                    return Err(DnsError::ApiError(format!(
                        "Namecheap API error: {}",
                        error_xml
                    )));
                }
            }
            return Err(DnsError::ApiError(format!("Namecheap API error: {}", body)));
        }

        Ok(body)
    }

    /// Parse domain list response
    fn parse_domains_response(&self, xml: &str) -> Result<Vec<DnsZone>, DnsError> {
        let mut zones = Vec::new();

        // Simple XML parsing (in production, use a proper XML parser)
        for line in xml.lines() {
            if line.contains("<Domain ") {
                let name = Self::extract_xml_attr(line, "Name");
                let id = Self::extract_xml_attr(line, "ID");

                if let (Some(name), Some(id)) = (name, id) {
                    zones.push(DnsZone {
                        id,
                        name,
                        status: "active".to_string(),
                        nameservers: vec![], // Would need separate API call
                        metadata: HashMap::new(),
                    });
                }
            }
        }

        Ok(zones)
    }

    /// Parse DNS hosts response
    fn parse_hosts_response(&self, xml: &str, domain: &str) -> Result<Vec<DnsRecord>, DnsError> {
        let mut records = Vec::new();

        for line in xml.lines() {
            if line.contains("<host ") {
                let name = Self::extract_xml_attr(line, "Name").unwrap_or_default();
                let record_type_str = Self::extract_xml_attr(line, "Type").unwrap_or_default();
                let address = Self::extract_xml_attr(line, "Address").unwrap_or_default();
                let ttl: u32 = Self::extract_xml_attr(line, "TTL")
                    .unwrap_or_default()
                    .parse()
                    .unwrap_or(1800);
                let host_id = Self::extract_xml_attr(line, "HostId");
                let mx_pref: u16 = Self::extract_xml_attr(line, "MXPref")
                    .unwrap_or_default()
                    .parse()
                    .unwrap_or(10);

                let content = match record_type_str.to_uppercase().as_str() {
                    "A" => address
                        .parse()
                        .ok()
                        .map(|ip| DnsRecordContent::A { address: ip }),
                    "AAAA" => address
                        .parse()
                        .ok()
                        .map(|ip| DnsRecordContent::AAAA { address: ip }),
                    "CNAME" => Some(DnsRecordContent::CNAME { target: address }),
                    "TXT" => Some(DnsRecordContent::TXT { content: address }),
                    "MX" => Some(DnsRecordContent::MX {
                        priority: mx_pref,
                        target: address,
                    }),
                    "NS" => Some(DnsRecordContent::NS {
                        nameserver: address,
                    }),
                    _ => None,
                };

                if let Some(content) = content {
                    let fqdn = if name == "@" || name.is_empty() {
                        domain.to_string()
                    } else {
                        format!("{}.{}", name, domain)
                    };

                    records.push(DnsRecord {
                        id: host_id,
                        zone: domain.to_string(),
                        name: name.clone(),
                        fqdn,
                        content,
                        ttl,
                        proxied: false,
                        metadata: HashMap::new(),
                    });
                }
            }
        }

        Ok(records)
    }

    /// Extract XML attribute value
    fn extract_xml_attr(line: &str, attr: &str) -> Option<String> {
        let pattern = format!("{}=\"", attr);
        if let Some(start) = line.find(&pattern) {
            let value_start = start + pattern.len();
            if let Some(end) = line[value_start..].find('"') {
                return Some(line[value_start..value_start + end].to_string());
            }
        }
        None
    }

    /// Split domain into SLD and TLD
    fn split_domain(domain: &str) -> (String, String) {
        let parts: Vec<&str> = domain.split('.').collect();
        if parts.len() >= 2 {
            let tld = parts[parts.len() - 1].to_string();
            let sld = parts[..parts.len() - 1].join(".");
            (sld, tld)
        } else {
            (domain.to_string(), String::new())
        }
    }

    /// Build host record parameters for SetHosts API call
    fn build_host_params(&self, records: &[DnsRecord]) -> Vec<(String, String)> {
        let mut params = Vec::new();

        for (i, record) in records.iter().enumerate() {
            let idx = i + 1;
            params.push((format!("HostName{}", idx), record.name.clone()));
            params.push((
                format!("RecordType{}", idx),
                record.content.record_type().to_string(),
            ));

            // For MX records, Namecheap expects Address to be just the target,
            // not the combined "priority target" format from to_value_string()
            let address = match &record.content {
                DnsRecordContent::MX { target, .. } => target.clone(),
                _ => record.content.to_value_string(),
            };
            params.push((format!("Address{}", idx), address));
            params.push((format!("TTL{}", idx), record.ttl.to_string()));

            if let DnsRecordContent::MX { priority, .. } = &record.content {
                params.push((format!("MXPref{}", idx), priority.to_string()));
            }
        }

        params
    }
}

#[async_trait]
impl DnsProvider for NamecheapProvider {
    fn provider_type(&self) -> DnsProviderType {
        DnsProviderType::Namecheap
    }

    fn capabilities(&self) -> DnsProviderCapabilities {
        DnsProviderCapabilities {
            a_record: true,
            aaaa_record: true,
            cname_record: true,
            txt_record: true,
            mx_record: true,
            ns_record: false, // Namecheap doesn't allow NS record management via API
            srv_record: false, // Limited support
            caa_record: false, // Not supported
            proxy: false,
            auto_ssl: false,
            wildcard: true,
        }
    }

    async fn test_connection(&self) -> Result<bool, DnsError> {
        match self.list_zones().await {
            Ok(_) => {
                info!("Namecheap API connection test successful");
                Ok(true)
            }
            Err(e) => {
                warn!("Namecheap API connection test failed: {}", e);
                Ok(false)
            }
        }
    }

    async fn list_zones(&self) -> Result<Vec<DnsZone>, DnsError> {
        let xml = self
            .api_request("namecheap.domains.getList", &[("PageSize", "100")])
            .await?;

        self.parse_domains_response(&xml)
    }

    async fn get_zone(&self, domain: &str) -> Result<Option<DnsZone>, DnsError> {
        let zones = self.list_zones().await?;
        Ok(zones.into_iter().find(|z| z.name == domain))
    }

    async fn list_records(&self, domain: &str) -> Result<Vec<DnsRecord>, DnsError> {
        let (sld, tld) = Self::split_domain(domain);

        let xml = self
            .api_request(
                "namecheap.domains.dns.getHosts",
                &[("SLD", &sld), ("TLD", &tld)],
            )
            .await?;

        self.parse_hosts_response(&xml, domain)
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
        // Namecheap requires sending ALL records when updating
        // So we need to get existing records, add the new one, and send all
        let mut records = self.list_records(domain).await?;

        let fqdn = if request.name == "@" || request.name.is_empty() {
            domain.to_string()
        } else {
            format!("{}.{}", request.name, domain)
        };

        let new_record = DnsRecord {
            id: None,
            zone: domain.to_string(),
            name: request.name.clone(),
            fqdn,
            content: request.content,
            ttl: request.ttl.unwrap_or(1800),
            proxied: false,
            metadata: HashMap::new(),
        };

        records.push(new_record.clone());

        // Build params and send
        let (sld, tld) = Self::split_domain(domain);
        let host_params = self.build_host_params(&records);

        let params: Vec<(&str, &str)> = std::iter::once(("SLD", sld.as_str()))
            .chain(std::iter::once(("TLD", tld.as_str())))
            .chain(host_params.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .collect();

        self.api_request("namecheap.domains.dns.setHosts", &params)
            .await?;

        info!("Created DNS record {} for domain {}", request.name, domain);

        Ok(new_record)
    }

    async fn update_record(
        &self,
        domain: &str,
        _record_id: &str,
        request: DnsRecordRequest,
    ) -> Result<DnsRecord, DnsError> {
        // For Namecheap, update is essentially delete + create
        // We get all records, modify the matching one, and send all
        let mut records = self.list_records(domain).await?;
        let record_type = request.content.record_type();

        // Find and update the matching record
        let mut found = false;
        for record in &mut records {
            if record.name == request.name && record.content.record_type() == record_type {
                record.content = request.content.clone();
                record.ttl = request.ttl.unwrap_or(record.ttl);
                found = true;
                break;
            }
        }

        if !found {
            return Err(DnsError::RecordNotFound(format!(
                "{} {} in {}",
                request.name, record_type, domain
            )));
        }

        // Build params and send
        let (sld, tld) = Self::split_domain(domain);
        let host_params = self.build_host_params(&records);

        let params: Vec<(&str, &str)> = std::iter::once(("SLD", sld.as_str()))
            .chain(std::iter::once(("TLD", tld.as_str())))
            .chain(host_params.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .collect();

        self.api_request("namecheap.domains.dns.setHosts", &params)
            .await?;

        info!("Updated DNS record {} for domain {}", request.name, domain);

        // Return the updated record
        let fqdn = if request.name == "@" || request.name.is_empty() {
            domain.to_string()
        } else {
            format!("{}.{}", request.name, domain)
        };

        Ok(DnsRecord {
            id: None,
            zone: domain.to_string(),
            name: request.name,
            fqdn,
            content: request.content,
            ttl: request.ttl.unwrap_or(1800),
            proxied: false,
            metadata: HashMap::new(),
        })
    }

    async fn delete_record(&self, domain: &str, record_id: &str) -> Result<(), DnsError> {
        // Namecheap doesn't have real record IDs, so we parse name:type from record_id
        // Format: "name:type" (e.g., "www:A" or "@:TXT")
        let parts: Vec<&str> = record_id.split(':').collect();
        if parts.len() != 2 {
            return Err(DnsError::Validation(format!(
                "Invalid record ID format: {}. Expected 'name:type'",
                record_id
            )));
        }

        let name = parts[0];
        let record_type_str = parts[1];

        let mut records = self.list_records(domain).await?;
        let original_len = records.len();

        // Remove matching record
        records.retain(|r| {
            !(r.name == name && r.content.record_type().to_string() == record_type_str)
        });

        if records.len() == original_len {
            return Err(DnsError::RecordNotFound(format!(
                "{} {} in {}",
                name, record_type_str, domain
            )));
        }

        // Build params and send
        let (sld, tld) = Self::split_domain(domain);
        let host_params = self.build_host_params(&records);

        let params: Vec<(&str, &str)> = std::iter::once(("SLD", sld.as_str()))
            .chain(std::iter::once(("TLD", tld.as_str())))
            .chain(host_params.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .collect();

        self.api_request("namecheap.domains.dns.setHosts", &params)
            .await?;

        info!("Deleted DNS record {} from domain {}", record_id, domain);

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_split_domain() {
        let (sld, tld) = NamecheapProvider::split_domain("example.com");
        assert_eq!(sld, "example");
        assert_eq!(tld, "com");

        let (sld, tld) = NamecheapProvider::split_domain("sub.example.com");
        assert_eq!(sld, "sub.example");
        assert_eq!(tld, "com");
    }

    #[test]
    fn test_split_domain_co_uk() {
        // Note: Namecheap treats co.uk as sld.tld
        let (sld, tld) = NamecheapProvider::split_domain("example.co.uk");
        assert_eq!(sld, "example.co");
        assert_eq!(tld, "uk");
    }

    #[test]
    fn test_split_domain_single_part() {
        let (sld, tld) = NamecheapProvider::split_domain("localhost");
        assert_eq!(sld, "localhost");
        assert_eq!(tld, "");
    }

    #[test]
    fn test_extract_xml_attr() {
        let line = r#"<Domain Name="example.com" ID="12345" Status="active">"#;

        assert_eq!(
            NamecheapProvider::extract_xml_attr(line, "Name"),
            Some("example.com".to_string())
        );
        assert_eq!(
            NamecheapProvider::extract_xml_attr(line, "ID"),
            Some("12345".to_string())
        );
        assert_eq!(
            NamecheapProvider::extract_xml_attr(line, "Status"),
            Some("active".to_string())
        );
        assert_eq!(NamecheapProvider::extract_xml_attr(line, "NotExists"), None);
    }

    #[test]
    fn test_extract_xml_attr_with_special_chars() {
        let line = r#"<host Address="v=spf1 include:_spf.google.com ~all" />"#;
        assert_eq!(
            NamecheapProvider::extract_xml_attr(line, "Address"),
            Some("v=spf1 include:_spf.google.com ~all".to_string())
        );
    }

    #[test]
    fn test_extract_xml_attr_empty_value() {
        let line = r#"<host Name="" Type="A" />"#;
        assert_eq!(
            NamecheapProvider::extract_xml_attr(line, "Name"),
            Some("".to_string())
        );
    }

    #[test]
    fn test_parse_hosts_response() {
        let xml = r#"
            <host HostId="123" Name="www" Type="A" Address="1.2.3.4" TTL="1800" MXPref="10" />
            <host HostId="124" Name="@" Type="TXT" Address="v=spf1 test" TTL="3600" MXPref="10" />
            <host HostId="125" Name="mail" Type="MX" Address="mail.example.com" TTL="1800" MXPref="10" />
        "#;

        let creds = NamecheapCredentials {
            api_user: "test".to_string(),
            api_key: "test".to_string(),
            client_ip: Some("1.2.3.4".to_string()),
            sandbox: true,
        };
        let provider = NamecheapProvider::new(creds).unwrap();

        let records = provider.parse_hosts_response(xml, "example.com").unwrap();

        assert_eq!(records.len(), 3);

        // Check A record
        assert_eq!(records[0].name, "www");
        assert!(matches!(records[0].content, DnsRecordContent::A { .. }));

        // Check TXT record
        assert_eq!(records[1].name, "@");
        assert!(matches!(records[1].content, DnsRecordContent::TXT { .. }));

        // Check MX record
        assert_eq!(records[2].name, "mail");
        assert!(matches!(records[2].content, DnsRecordContent::MX { .. }));
    }

    #[test]
    fn test_parse_hosts_response_all_record_types() {
        let xml = r#"
            <host HostId="1" Name="www" Type="A" Address="192.0.2.1" TTL="300" MXPref="10" />
            <host HostId="2" Name="ipv6" Type="AAAA" Address="2001:db8::1" TTL="300" MXPref="10" />
            <host HostId="3" Name="alias" Type="CNAME" Address="www.example.com" TTL="300" MXPref="10" />
            <host HostId="4" Name="@" Type="TXT" Address="v=spf1 -all" TTL="300" MXPref="10" />
            <host HostId="5" Name="@" Type="MX" Address="mail.example.com" TTL="300" MXPref="10" />
            <host HostId="6" Name="@" Type="NS" Address="ns1.example.com" TTL="300" MXPref="10" />
        "#;

        let creds = NamecheapCredentials {
            api_user: "test".to_string(),
            api_key: "test".to_string(),
            client_ip: Some("1.2.3.4".to_string()),
            sandbox: true,
        };
        let provider = NamecheapProvider::new(creds).unwrap();

        let records = provider.parse_hosts_response(xml, "example.com").unwrap();

        assert_eq!(records.len(), 6);

        // Verify each record type
        assert!(matches!(records[0].content, DnsRecordContent::A { .. }));
        assert!(matches!(records[1].content, DnsRecordContent::AAAA { .. }));
        assert!(matches!(records[2].content, DnsRecordContent::CNAME { .. }));
        assert!(matches!(records[3].content, DnsRecordContent::TXT { .. }));
        assert!(matches!(records[4].content, DnsRecordContent::MX { .. }));
        assert!(matches!(records[5].content, DnsRecordContent::NS { .. }));
    }

    #[test]
    fn test_parse_hosts_response_mx_priority() {
        let xml = r#"
            <host HostId="1" Name="@" Type="MX" Address="mail1.example.com" TTL="300" MXPref="10" />
            <host HostId="2" Name="@" Type="MX" Address="mail2.example.com" TTL="300" MXPref="20" />
        "#;

        let creds = NamecheapCredentials {
            api_user: "test".to_string(),
            api_key: "test".to_string(),
            client_ip: Some("1.2.3.4".to_string()),
            sandbox: true,
        };
        let provider = NamecheapProvider::new(creds).unwrap();

        let records = provider.parse_hosts_response(xml, "example.com").unwrap();

        assert_eq!(records.len(), 2);

        if let DnsRecordContent::MX { priority, target } = &records[0].content {
            assert_eq!(*priority, 10);
            assert_eq!(target, "mail1.example.com");
        } else {
            panic!("Expected MX record");
        }

        if let DnsRecordContent::MX { priority, target } = &records[1].content {
            assert_eq!(*priority, 20);
            assert_eq!(target, "mail2.example.com");
        } else {
            panic!("Expected MX record");
        }
    }

    #[test]
    fn test_parse_hosts_response_fqdn_generation() {
        let xml = r#"
            <host HostId="1" Name="www" Type="A" Address="1.2.3.4" TTL="300" MXPref="10" />
            <host HostId="2" Name="@" Type="A" Address="1.2.3.5" TTL="300" MXPref="10" />
            <host HostId="3" Name="" Type="A" Address="1.2.3.6" TTL="300" MXPref="10" />
        "#;

        let creds = NamecheapCredentials {
            api_user: "test".to_string(),
            api_key: "test".to_string(),
            client_ip: Some("1.2.3.4".to_string()),
            sandbox: true,
        };
        let provider = NamecheapProvider::new(creds).unwrap();

        let records = provider.parse_hosts_response(xml, "example.com").unwrap();

        assert_eq!(records[0].fqdn, "www.example.com");
        assert_eq!(records[1].fqdn, "example.com"); // @ becomes apex
        assert_eq!(records[2].fqdn, "example.com"); // empty becomes apex
    }

    #[test]
    fn test_parse_hosts_response_ttl_parsing() {
        let xml = r#"
            <host HostId="1" Name="www" Type="A" Address="1.2.3.4" TTL="60" MXPref="10" />
            <host HostId="2" Name="api" Type="A" Address="1.2.3.5" TTL="86400" MXPref="10" />
            <host HostId="3" Name="bad" Type="A" Address="1.2.3.6" TTL="invalid" MXPref="10" />
        "#;

        let creds = NamecheapCredentials {
            api_user: "test".to_string(),
            api_key: "test".to_string(),
            client_ip: Some("1.2.3.4".to_string()),
            sandbox: true,
        };
        let provider = NamecheapProvider::new(creds).unwrap();

        let records = provider.parse_hosts_response(xml, "example.com").unwrap();

        assert_eq!(records[0].ttl, 60);
        assert_eq!(records[1].ttl, 86400);
        assert_eq!(records[2].ttl, 1800); // Default when parsing fails
    }

    #[test]
    fn test_parse_hosts_response_unknown_type() {
        let xml = r#"
            <host HostId="1" Name="www" Type="A" Address="1.2.3.4" TTL="300" MXPref="10" />
            <host HostId="2" Name="_dmarc" Type="DMARC" Address="v=DMARC1" TTL="300" MXPref="10" />
        "#;

        let creds = NamecheapCredentials {
            api_user: "test".to_string(),
            api_key: "test".to_string(),
            client_ip: Some("1.2.3.4".to_string()),
            sandbox: true,
        };
        let provider = NamecheapProvider::new(creds).unwrap();

        let records = provider.parse_hosts_response(xml, "example.com").unwrap();

        // Unknown type should be skipped
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].name, "www");
    }

    #[test]
    fn test_parse_domains_response() {
        let xml = r#"
            <?xml version="1.0" encoding="UTF-8"?>
            <ApiResponse Status="OK" xmlns="http://api.namecheap.com/xml.response">
              <CommandResponse Type="namecheap.domains.getList">
                <DomainGetListResult>
                  <Domain ID="12345" Name="example.com" User="testuser" Created="01/01/2020" Expires="01/01/2025" IsExpired="false" IsLocked="false" AutoRenew="true" WhoisGuard="ENABLED" IsPremium="false" IsOurDNS="true"/>
                  <Domain ID="12346" Name="test.org" User="testuser" Created="02/01/2020" Expires="02/01/2025" IsExpired="false" IsLocked="false" AutoRenew="false" WhoisGuard="DISABLED" IsPremium="false" IsOurDNS="true"/>
                </DomainGetListResult>
              </CommandResponse>
            </ApiResponse>
        "#;

        let creds = NamecheapCredentials {
            api_user: "test".to_string(),
            api_key: "test".to_string(),
            client_ip: Some("1.2.3.4".to_string()),
            sandbox: true,
        };
        let provider = NamecheapProvider::new(creds).unwrap();

        let zones = provider.parse_domains_response(xml).unwrap();

        assert_eq!(zones.len(), 2);
        assert_eq!(zones[0].name, "example.com");
        assert_eq!(zones[0].id, "12345");
        assert_eq!(zones[1].name, "test.org");
        assert_eq!(zones[1].id, "12346");
    }

    #[test]
    fn test_parse_domains_response_empty() {
        let xml = r#"
            <?xml version="1.0" encoding="UTF-8"?>
            <ApiResponse Status="OK" xmlns="http://api.namecheap.com/xml.response">
              <CommandResponse Type="namecheap.domains.getList">
                <DomainGetListResult>
                </DomainGetListResult>
              </CommandResponse>
            </ApiResponse>
        "#;

        let creds = NamecheapCredentials {
            api_user: "test".to_string(),
            api_key: "test".to_string(),
            client_ip: Some("1.2.3.4".to_string()),
            sandbox: true,
        };
        let provider = NamecheapProvider::new(creds).unwrap();

        let zones = provider.parse_domains_response(xml).unwrap();
        assert!(zones.is_empty());
    }

    #[test]
    fn test_build_host_params() {
        let creds = NamecheapCredentials {
            api_user: "test".to_string(),
            api_key: "test".to_string(),
            client_ip: Some("1.2.3.4".to_string()),
            sandbox: true,
        };
        let provider = NamecheapProvider::new(creds).unwrap();

        let records = vec![
            DnsRecord {
                id: Some("1".to_string()),
                zone: "example.com".to_string(),
                name: "www".to_string(),
                fqdn: "www.example.com".to_string(),
                content: DnsRecordContent::A {
                    address: "1.2.3.4".to_string(),
                },
                ttl: 300,
                proxied: false,
                metadata: HashMap::new(),
            },
            DnsRecord {
                id: Some("2".to_string()),
                zone: "example.com".to_string(),
                name: "@".to_string(),
                fqdn: "example.com".to_string(),
                content: DnsRecordContent::MX {
                    priority: 10,
                    target: "mail.example.com".to_string(),
                },
                ttl: 1800,
                proxied: false,
                metadata: HashMap::new(),
            },
        ];

        let params = provider.build_host_params(&records);

        // Check A record params
        assert!(params.contains(&("HostName1".to_string(), "www".to_string())));
        assert!(params.contains(&("RecordType1".to_string(), "A".to_string())));
        assert!(params.contains(&("Address1".to_string(), "1.2.3.4".to_string())));
        assert!(params.contains(&("TTL1".to_string(), "300".to_string())));

        // Check MX record params
        assert!(params.contains(&("HostName2".to_string(), "@".to_string())));
        assert!(params.contains(&("RecordType2".to_string(), "MX".to_string())));
        assert!(params.contains(&("Address2".to_string(), "mail.example.com".to_string())));
        assert!(params.contains(&("TTL2".to_string(), "1800".to_string())));
        assert!(params.contains(&("MXPref2".to_string(), "10".to_string())));
    }

    #[test]
    fn test_provider_type() {
        let creds = NamecheapCredentials {
            api_user: "test".to_string(),
            api_key: "test".to_string(),
            client_ip: Some("1.2.3.4".to_string()),
            sandbox: true,
        };
        let provider = NamecheapProvider::new(creds).unwrap();

        assert_eq!(provider.provider_type(), DnsProviderType::Namecheap);
    }

    #[test]
    fn test_capabilities() {
        let creds = NamecheapCredentials {
            api_user: "test".to_string(),
            api_key: "test".to_string(),
            client_ip: Some("1.2.3.4".to_string()),
            sandbox: true,
        };
        let provider = NamecheapProvider::new(creds).unwrap();

        let caps = provider.capabilities();

        assert!(caps.a_record);
        assert!(caps.aaaa_record);
        assert!(caps.cname_record);
        assert!(caps.txt_record);
        assert!(caps.mx_record);
        assert!(!caps.ns_record); // Namecheap doesn't support NS via API
        assert!(!caps.srv_record); // Limited support
        assert!(!caps.caa_record);
        assert!(!caps.proxy); // No proxy support
        assert!(!caps.auto_ssl);
        assert!(caps.wildcard);
    }

    #[test]
    fn test_sandbox_vs_production_url() {
        let sandbox_creds = NamecheapCredentials {
            api_user: "test".to_string(),
            api_key: "test".to_string(),
            client_ip: Some("1.2.3.4".to_string()),
            sandbox: true,
        };
        let sandbox_provider = NamecheapProvider::new(sandbox_creds).unwrap();
        assert_eq!(sandbox_provider.base_url, NAMECHEAP_SANDBOX_URL);

        let prod_creds = NamecheapCredentials {
            api_user: "test".to_string(),
            api_key: "test".to_string(),
            client_ip: Some("1.2.3.4".to_string()),
            sandbox: false,
        };
        let prod_provider = NamecheapProvider::new(prod_creds).unwrap();
        assert_eq!(prod_provider.base_url, NAMECHEAP_API_URL);
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use wiremock::matchers::{method, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn create_mock_provider(mock_server: &MockServer) -> NamecheapProvider {
        let creds = NamecheapCredentials {
            api_user: "testuser".to_string(),
            api_key: "testapikey".to_string(),
            client_ip: Some("1.2.3.4".to_string()),
            sandbox: true,
        };

        // Create provider with custom base URL
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap();

        NamecheapProvider {
            client,
            credentials: creds,
            base_url: mock_server.uri(),
        }
    }

    #[tokio::test]
    async fn test_list_zones_success() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(query_param("Command", "namecheap.domains.getList"))
            .and(query_param("ApiUser", "testuser"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"<?xml version="1.0" encoding="UTF-8"?>
                <ApiResponse Status="OK">
                  <CommandResponse Type="namecheap.domains.getList">
                    <DomainGetListResult>
                      <Domain ID="12345" Name="example.com" />
                      <Domain ID="12346" Name="test.org" />
                    </DomainGetListResult>
                  </CommandResponse>
                </ApiResponse>"#,
            ))
            .mount(&mock_server)
            .await;

        let provider = create_mock_provider(&mock_server).await;
        let zones = provider.list_zones().await.unwrap();

        assert_eq!(zones.len(), 2);
        assert_eq!(zones[0].name, "example.com");
        assert_eq!(zones[1].name, "test.org");
    }

    #[tokio::test]
    async fn test_list_zones_api_error() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(query_param("Command", "namecheap.domains.getList"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"<?xml version="1.0" encoding="UTF-8"?>
                <ApiResponse Status="ERROR">
                  <Errors>
                    <Error Number="1011150">Parameter ApiKey is invalid</Error>
                  </Errors>
                </ApiResponse>"#,
            ))
            .mount(&mock_server)
            .await;

        let provider = create_mock_provider(&mock_server).await;
        let result = provider.list_zones().await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, DnsError::ApiError(_)));
    }

    #[tokio::test]
    async fn test_list_records_success() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(query_param("Command", "namecheap.domains.dns.getHosts"))
            .and(query_param("SLD", "example"))
            .and(query_param("TLD", "com"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"<?xml version="1.0" encoding="UTF-8"?>
                <ApiResponse Status="OK">
                  <CommandResponse Type="namecheap.domains.dns.getHosts">
                    <DomainDNSGetHostsResult>
                      <host HostId="1" Name="www" Type="A" Address="192.0.2.1" TTL="300" MXPref="10" />
                      <host HostId="2" Name="@" Type="TXT" Address="v=spf1 -all" TTL="3600" MXPref="10" />
                    </DomainDNSGetHostsResult>
                  </CommandResponse>
                </ApiResponse>"#,
            ))
            .mount(&mock_server)
            .await;

        let provider = create_mock_provider(&mock_server).await;
        let records = provider.list_records("example.com").await.unwrap();

        assert_eq!(records.len(), 2);
        assert_eq!(records[0].name, "www");
        assert_eq!(records[0].fqdn, "www.example.com");
        if let DnsRecordContent::A { address } = &records[0].content {
            assert_eq!(address, "192.0.2.1");
        } else {
            panic!("Expected A record");
        }
    }

    #[tokio::test]
    async fn test_get_record_found() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(query_param("Command", "namecheap.domains.dns.getHosts"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"<?xml version="1.0" encoding="UTF-8"?>
                <ApiResponse Status="OK">
                  <CommandResponse>
                    <DomainDNSGetHostsResult>
                      <host HostId="1" Name="www" Type="A" Address="192.0.2.1" TTL="300" MXPref="10" />
                      <host HostId="2" Name="api" Type="A" Address="192.0.2.2" TTL="300" MXPref="10" />
                    </DomainDNSGetHostsResult>
                  </CommandResponse>
                </ApiResponse>"#,
            ))
            .mount(&mock_server)
            .await;

        let provider = create_mock_provider(&mock_server).await;
        let record = provider
            .get_record("example.com", "www", DnsRecordType::A)
            .await
            .unwrap();

        assert!(record.is_some());
        let record = record.unwrap();
        assert_eq!(record.name, "www");
        if let DnsRecordContent::A { address } = &record.content {
            assert_eq!(address, "192.0.2.1");
        } else {
            panic!("Expected A record");
        }
    }

    #[tokio::test]
    async fn test_get_record_not_found() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(query_param("Command", "namecheap.domains.dns.getHosts"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"<?xml version="1.0" encoding="UTF-8"?>
                <ApiResponse Status="OK">
                  <CommandResponse>
                    <DomainDNSGetHostsResult>
                      <host HostId="1" Name="www" Type="A" Address="192.0.2.1" TTL="300" MXPref="10" />
                    </DomainDNSGetHostsResult>
                  </CommandResponse>
                </ApiResponse>"#,
            ))
            .mount(&mock_server)
            .await;

        let provider = create_mock_provider(&mock_server).await;
        let record = provider
            .get_record("example.com", "nonexistent", DnsRecordType::A)
            .await
            .unwrap();

        assert!(record.is_none());
    }

    #[tokio::test]
    async fn test_create_record_success() {
        let mock_server = MockServer::start().await;

        // First call: get existing hosts
        Mock::given(method("GET"))
            .and(query_param("Command", "namecheap.domains.dns.getHosts"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"<?xml version="1.0" encoding="UTF-8"?>
                <ApiResponse Status="OK">
                  <CommandResponse>
                    <DomainDNSGetHostsResult>
                      <host HostId="1" Name="www" Type="A" Address="192.0.2.1" TTL="300" MXPref="10" />
                    </DomainDNSGetHostsResult>
                  </CommandResponse>
                </ApiResponse>"#,
            ))
            .expect(1)
            .mount(&mock_server)
            .await;

        // Second call: set hosts (includes old + new)
        Mock::given(method("GET"))
            .and(query_param("Command", "namecheap.domains.dns.setHosts"))
            .and(query_param("HostName1", "www"))
            .and(query_param("HostName2", "api"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"<?xml version="1.0" encoding="UTF-8"?>
                <ApiResponse Status="OK">
                  <CommandResponse Type="namecheap.domains.dns.setHosts">
                    <DomainDNSSetHostsResult Domain="example.com" IsSuccess="true" />
                  </CommandResponse>
                </ApiResponse>"#,
            ))
            .expect(1)
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
        assert_eq!(record.ttl, 300);
    }

    #[tokio::test]
    async fn test_update_record_success() {
        let mock_server = MockServer::start().await;

        // Get existing hosts
        Mock::given(method("GET"))
            .and(query_param("Command", "namecheap.domains.dns.getHosts"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"<?xml version="1.0" encoding="UTF-8"?>
                <ApiResponse Status="OK">
                  <CommandResponse>
                    <DomainDNSGetHostsResult>
                      <host HostId="1" Name="www" Type="A" Address="192.0.2.1" TTL="300" MXPref="10" />
                    </DomainDNSGetHostsResult>
                  </CommandResponse>
                </ApiResponse>"#,
            ))
            .expect(1)
            .mount(&mock_server)
            .await;

        // Set hosts with updated address
        Mock::given(method("GET"))
            .and(query_param("Command", "namecheap.domains.dns.setHosts"))
            .and(query_param("Address1", "192.0.2.99"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"<?xml version="1.0" encoding="UTF-8"?>
                <ApiResponse Status="OK">
                  <CommandResponse Type="namecheap.domains.dns.setHosts">
                    <DomainDNSSetHostsResult Domain="example.com" IsSuccess="true" />
                  </CommandResponse>
                </ApiResponse>"#,
            ))
            .expect(1)
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
            .update_record("example.com", "1", request)
            .await
            .unwrap();

        assert_eq!(record.name, "www");
        if let DnsRecordContent::A { address } = &record.content {
            assert_eq!(address, "192.0.2.99");
        } else {
            panic!("Expected A record");
        }
    }

    #[tokio::test]
    async fn test_update_record_not_found() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(query_param("Command", "namecheap.domains.dns.getHosts"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"<?xml version="1.0" encoding="UTF-8"?>
                <ApiResponse Status="OK">
                  <CommandResponse>
                    <DomainDNSGetHostsResult>
                      <host HostId="1" Name="www" Type="A" Address="192.0.2.1" TTL="300" MXPref="10" />
                    </DomainDNSGetHostsResult>
                  </CommandResponse>
                </ApiResponse>"#,
            ))
            .mount(&mock_server)
            .await;

        let provider = create_mock_provider(&mock_server).await;
        let request = DnsRecordRequest {
            name: "nonexistent".to_string(),
            content: DnsRecordContent::A {
                address: "192.0.2.99".to_string(),
            },
            ttl: Some(600),
            proxied: false,
        };

        let result = provider.update_record("example.com", "999", request).await;

        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), DnsError::RecordNotFound(_)));
    }

    #[tokio::test]
    async fn test_delete_record_success() {
        let mock_server = MockServer::start().await;

        // Get existing hosts (multiple records)
        Mock::given(method("GET"))
            .and(query_param("Command", "namecheap.domains.dns.getHosts"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"<?xml version="1.0" encoding="UTF-8"?>
                <ApiResponse Status="OK">
                  <CommandResponse>
                    <DomainDNSGetHostsResult>
                      <host HostId="1" Name="www" Type="A" Address="192.0.2.1" TTL="300" MXPref="10" />
                      <host HostId="2" Name="api" Type="A" Address="192.0.2.2" TTL="300" MXPref="10" />
                    </DomainDNSGetHostsResult>
                  </CommandResponse>
                </ApiResponse>"#,
            ))
            .expect(1)
            .mount(&mock_server)
            .await;

        // Set hosts without the deleted record (only www remains)
        Mock::given(method("GET"))
            .and(query_param("Command", "namecheap.domains.dns.setHosts"))
            .and(query_param("HostName1", "www"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"<?xml version="1.0" encoding="UTF-8"?>
                <ApiResponse Status="OK">
                  <CommandResponse Type="namecheap.domains.dns.setHosts">
                    <DomainDNSSetHostsResult Domain="example.com" IsSuccess="true" />
                  </CommandResponse>
                </ApiResponse>"#,
            ))
            .expect(1)
            .mount(&mock_server)
            .await;

        let provider = create_mock_provider(&mock_server).await;
        let result = provider.delete_record("example.com", "api:A").await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_delete_record_not_found() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(query_param("Command", "namecheap.domains.dns.getHosts"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"<?xml version="1.0" encoding="UTF-8"?>
                <ApiResponse Status="OK">
                  <CommandResponse>
                    <DomainDNSGetHostsResult>
                      <host HostId="1" Name="www" Type="A" Address="192.0.2.1" TTL="300" MXPref="10" />
                    </DomainDNSGetHostsResult>
                  </CommandResponse>
                </ApiResponse>"#,
            ))
            .mount(&mock_server)
            .await;

        let provider = create_mock_provider(&mock_server).await;
        let result = provider.delete_record("example.com", "nonexistent:A").await;

        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), DnsError::RecordNotFound(_)));
    }

    #[tokio::test]
    async fn test_delete_record_invalid_format() {
        let mock_server = MockServer::start().await;
        let provider = create_mock_provider(&mock_server).await;

        // Invalid format (no colon)
        let result = provider.delete_record("example.com", "invalidformat").await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), DnsError::Validation(_)));

        // Invalid format (too many colons)
        let result = provider.delete_record("example.com", "www:A:extra").await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), DnsError::Validation(_)));
    }

    #[tokio::test]
    async fn test_test_connection_success() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(query_param("Command", "namecheap.domains.getList"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"<?xml version="1.0" encoding="UTF-8"?>
                <ApiResponse Status="OK">
                  <CommandResponse Type="namecheap.domains.getList">
                    <DomainGetListResult>
                    </DomainGetListResult>
                  </CommandResponse>
                </ApiResponse>"#,
            ))
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
            .and(query_param("Command", "namecheap.domains.getList"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"<?xml version="1.0" encoding="UTF-8"?>
                <ApiResponse Status="ERROR">
                  <Errors>
                    <Error Number="1011150">Parameter ApiKey is invalid</Error>
                  </Errors>
                </ApiResponse>"#,
            ))
            .mount(&mock_server)
            .await;

        let provider = create_mock_provider(&mock_server).await;
        let result = provider.test_connection().await.unwrap();

        assert!(!result);
    }

    #[tokio::test]
    async fn test_http_error() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(500).set_body_string("Internal Server Error"))
            .mount(&mock_server)
            .await;

        let provider = create_mock_provider(&mock_server).await;
        let result = provider.list_zones().await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_get_zone_found() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(query_param("Command", "namecheap.domains.getList"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"<?xml version="1.0" encoding="UTF-8"?>
                <ApiResponse Status="OK">
                  <CommandResponse>
                    <DomainGetListResult>
                      <Domain ID="12345" Name="example.com" />
                      <Domain ID="12346" Name="other.org" />
                    </DomainGetListResult>
                  </CommandResponse>
                </ApiResponse>"#,
            ))
            .mount(&mock_server)
            .await;

        let provider = create_mock_provider(&mock_server).await;
        let zone = provider.get_zone("example.com").await.unwrap();

        assert!(zone.is_some());
        let zone = zone.unwrap();
        assert_eq!(zone.name, "example.com");
        assert_eq!(zone.id, "12345");
    }

    #[tokio::test]
    async fn test_get_zone_not_found() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(query_param("Command", "namecheap.domains.getList"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"<?xml version="1.0" encoding="UTF-8"?>
                <ApiResponse Status="OK">
                  <CommandResponse>
                    <DomainGetListResult>
                      <Domain ID="12345" Name="example.com" />
                    </DomainGetListResult>
                  </CommandResponse>
                </ApiResponse>"#,
            ))
            .mount(&mock_server)
            .await;

        let provider = create_mock_provider(&mock_server).await;
        let zone = provider.get_zone("notfound.com").await.unwrap();

        assert!(zone.is_none());
    }
}
