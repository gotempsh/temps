//! DNS Record service for automatic DNS management
//!
//! This service is the main entry point for other crates to interact with
//! DNS providers. It handles:
//! - Finding the right provider for a domain
//! - Setting/removing DNS records automatically
//! - Providing fallback instructions when automatic management isn't available

use std::sync::Arc;
use tracing::{debug, info, warn};

use crate::errors::DnsError;
use crate::providers::{DnsRecordContent, DnsRecordRequest, DnsRecordType};
use crate::services::provider_service::DnsProviderService;

/// Result of a DNS operation
#[derive(Debug, Clone)]
pub struct DnsOperationResult {
    /// Whether the operation was performed automatically
    pub automatic: bool,
    /// Whether the operation succeeded
    pub success: bool,
    /// The domain the operation was performed on
    pub domain: String,
    /// Record name
    pub name: String,
    /// Record type
    pub record_type: String,
    /// Human-readable message
    pub message: String,
    /// Manual instructions if automatic operation is not available
    pub manual_instructions: Option<ManualDnsInstructions>,
}

/// Instructions for manual DNS configuration
#[derive(Debug, Clone)]
pub struct ManualDnsInstructions {
    /// Record type to create
    pub record_type: String,
    /// Record name (subdomain or @ for apex)
    pub name: String,
    /// Record value/content
    pub value: String,
    /// Recommended TTL
    pub ttl: u32,
    /// Priority (for MX records)
    pub priority: Option<u16>,
    /// Human-readable instructions
    pub instructions: String,
}

/// Service for managing DNS records across providers
///
/// This service is designed to be used by other crates (like temps-domains,
/// temps-email) to automatically configure DNS records when a domain is
/// managed by a configured DNS provider.
#[derive(Clone)]
pub struct DnsRecordService {
    provider_service: Arc<DnsProviderService>,
}

impl DnsRecordService {
    pub fn new(provider_service: Arc<DnsProviderService>) -> Self {
        Self { provider_service }
    }

    /// Check if automatic DNS management is available for a domain
    pub async fn can_auto_manage(&self, domain: &str) -> bool {
        self.provider_service
            .find_provider_for_domain(domain)
            .await
            .ok()
            .flatten()
            .is_some()
    }

    /// Set an A record for a domain
    ///
    /// Returns instructions for manual configuration if automatic management
    /// is not available.
    pub async fn set_a_record(
        &self,
        domain: &str,
        name: &str,
        ip_address: &str,
        ttl: Option<u32>,
    ) -> Result<DnsOperationResult, DnsError> {
        // Parse IP to validate and determine type
        let ip: std::net::IpAddr = ip_address
            .parse()
            .map_err(|_| DnsError::Validation(format!("Invalid IP address: {}", ip_address)))?;

        let content = if ip.is_ipv4() {
            DnsRecordContent::A {
                address: ip_address.to_string(),
            }
        } else {
            DnsRecordContent::AAAA {
                address: ip_address.to_string(),
            }
        };

        let record_type = if ip.is_ipv4() { "A" } else { "AAAA" };

        self.set_record(domain, name, content, ttl, record_type)
            .await
    }

    /// Set a TXT record for a domain
    pub async fn set_txt_record(
        &self,
        domain: &str,
        name: &str,
        value: &str,
        ttl: Option<u32>,
    ) -> Result<DnsOperationResult, DnsError> {
        let content = DnsRecordContent::TXT {
            content: value.to_string(),
        };

        self.set_record(domain, name, content, ttl, "TXT").await
    }

    /// Set a CNAME record for a domain
    pub async fn set_cname_record(
        &self,
        domain: &str,
        name: &str,
        target: &str,
        ttl: Option<u32>,
    ) -> Result<DnsOperationResult, DnsError> {
        let content = DnsRecordContent::CNAME {
            target: target.to_string(),
        };

        self.set_record(domain, name, content, ttl, "CNAME").await
    }

    /// Set an MX record for a domain
    pub async fn set_mx_record(
        &self,
        domain: &str,
        name: &str,
        target: &str,
        priority: u16,
        ttl: Option<u32>,
    ) -> Result<DnsOperationResult, DnsError> {
        let content = DnsRecordContent::MX {
            priority,
            target: target.to_string(),
        };

        self.set_record(domain, name, content, ttl, "MX").await
    }

    /// Generic method to set any record type
    async fn set_record(
        &self,
        domain: &str,
        name: &str,
        content: DnsRecordContent,
        ttl: Option<u32>,
        record_type_str: &str,
    ) -> Result<DnsOperationResult, DnsError> {
        let base_domain = Self::extract_base_domain(domain);

        // Try to find a provider for this domain
        match self
            .provider_service
            .find_provider_for_domain(&base_domain)
            .await?
        {
            Some((provider, _managed_domain)) => {
                // Automatic management available
                let instance = self.provider_service.create_provider_instance(&provider)?;

                let request = DnsRecordRequest {
                    name: name.to_string(),
                    content: content.clone(),
                    ttl,
                    proxied: false,
                };

                match instance.set_record(&base_domain, request).await {
                    Ok(_record) => {
                        info!(
                            "Automatically set {} record for {}.{} via {}",
                            record_type_str, name, base_domain, provider.name
                        );
                        Ok(DnsOperationResult {
                            automatic: true,
                            success: true,
                            domain: base_domain,
                            name: name.to_string(),
                            record_type: record_type_str.to_string(),
                            message: format!(
                                "Successfully set {} record via {}",
                                record_type_str, provider.name
                            ),
                            manual_instructions: None,
                        })
                    }
                    Err(e) => {
                        warn!(
                            "Failed to set {} record automatically: {}",
                            record_type_str, e
                        );
                        // Return manual instructions as fallback
                        Ok(self.create_manual_result(
                            &base_domain,
                            name,
                            &content,
                            ttl.unwrap_or(300),
                            record_type_str,
                            Some(format!("Automatic configuration failed: {}", e)),
                        ))
                    }
                }
            }
            None => {
                // No automatic management - return manual instructions
                debug!(
                    "No DNS provider configured for {}, returning manual instructions",
                    base_domain
                );
                Ok(self.create_manual_result(
                    &base_domain,
                    name,
                    &content,
                    ttl.unwrap_or(300),
                    record_type_str,
                    None,
                ))
            }
        }
    }

    /// Remove a record by name and type
    pub async fn remove_record(
        &self,
        domain: &str,
        name: &str,
        record_type: DnsRecordType,
    ) -> Result<DnsOperationResult, DnsError> {
        let base_domain = Self::extract_base_domain(domain);

        match self
            .provider_service
            .find_provider_for_domain(&base_domain)
            .await?
        {
            Some((provider, _managed_domain)) => {
                let instance = self.provider_service.create_provider_instance(&provider)?;

                match instance
                    .remove_record(&base_domain, name, record_type)
                    .await
                {
                    Ok(()) => {
                        info!(
                            "Automatically removed {} record for {}.{} via {}",
                            record_type, name, base_domain, provider.name
                        );
                        Ok(DnsOperationResult {
                            automatic: true,
                            success: true,
                            domain: base_domain,
                            name: name.to_string(),
                            record_type: record_type.to_string(),
                            message: format!(
                                "Successfully removed {} record via {}",
                                record_type, provider.name
                            ),
                            manual_instructions: None,
                        })
                    }
                    Err(e) => {
                        warn!(
                            "Failed to remove {} record automatically: {}",
                            record_type, e
                        );
                        Ok(DnsOperationResult {
                            automatic: false,
                            success: false,
                            domain: base_domain,
                            name: name.to_string(),
                            record_type: record_type.to_string(),
                            message: format!("Failed to remove record: {}", e),
                            manual_instructions: Some(ManualDnsInstructions {
                                record_type: record_type.to_string(),
                                name: name.to_string(),
                                value: String::new(),
                                ttl: 0,
                                priority: None,
                                instructions: format!(
                                    "Please manually remove the {} record for '{}' from your DNS provider.",
                                    record_type, name
                                ),
                            }),
                        })
                    }
                }
            }
            None => Ok(DnsOperationResult {
                automatic: false,
                success: false,
                domain: base_domain,
                name: name.to_string(),
                record_type: record_type.to_string(),
                message: "No DNS provider configured for this domain".to_string(),
                manual_instructions: Some(ManualDnsInstructions {
                    record_type: record_type.to_string(),
                    name: name.to_string(),
                    value: String::new(),
                    ttl: 0,
                    priority: None,
                    instructions: format!(
                        "Please manually remove the {} record for '{}' from your DNS provider.",
                        record_type, name
                    ),
                }),
            }),
        }
    }

    /// Create a result with manual instructions
    fn create_manual_result(
        &self,
        domain: &str,
        name: &str,
        content: &DnsRecordContent,
        ttl: u32,
        record_type_str: &str,
        error_message: Option<String>,
    ) -> DnsOperationResult {
        let (value, priority) = match content {
            DnsRecordContent::A { address } | DnsRecordContent::AAAA { address } => {
                (address.clone(), None)
            }
            DnsRecordContent::CNAME { target }
            | DnsRecordContent::NS { nameserver: target }
            | DnsRecordContent::PTR { target } => (target.clone(), None),
            DnsRecordContent::TXT { content } => (content.clone(), None),
            DnsRecordContent::MX { priority, target } => (target.clone(), Some(*priority)),
            DnsRecordContent::SRV {
                priority,
                weight,
                port,
                target,
            } => (
                format!("{} {} {} {}", priority, weight, port, target),
                Some(*priority),
            ),
            DnsRecordContent::CAA { flags, tag, value } => {
                (format!("{} {} \"{}\"", flags, tag, value), None)
            }
        };

        let instructions = format!(
            "Add a {} record to your DNS provider:\n\
             - Name: {}\n\
             - Value: {}\n\
             - TTL: {} seconds{}",
            record_type_str,
            if name == "@" || name.is_empty() {
                domain
            } else {
                name
            },
            value,
            ttl,
            priority
                .map(|p| format!("\n- Priority: {}", p))
                .unwrap_or_default()
        );

        DnsOperationResult {
            automatic: false,
            success: false,
            domain: domain.to_string(),
            name: name.to_string(),
            record_type: record_type_str.to_string(),
            message: error_message.unwrap_or_else(|| {
                "No DNS provider configured for automatic management".to_string()
            }),
            manual_instructions: Some(ManualDnsInstructions {
                record_type: record_type_str.to_string(),
                name: name.to_string(),
                value,
                ttl,
                priority,
                instructions,
            }),
        }
    }

    /// Extract base domain from a full domain name
    fn extract_base_domain(domain: &str) -> String {
        let parts: Vec<&str> = domain.split('.').collect();
        if parts.len() >= 2 {
            parts[parts.len() - 2..].join(".")
        } else {
            domain.to_string()
        }
    }

    /// Get the DNS provider service (for advanced operations)
    pub fn provider_service(&self) -> &DnsProviderService {
        &self.provider_service
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ==================== Helper function tests ====================

    #[test]
    fn test_extract_base_domain() {
        assert_eq!(
            DnsRecordService::extract_base_domain("example.com"),
            "example.com"
        );
        assert_eq!(
            DnsRecordService::extract_base_domain("sub.example.com"),
            "example.com"
        );
        assert_eq!(
            DnsRecordService::extract_base_domain("a.b.c.example.com"),
            "example.com"
        );
    }

    #[test]
    fn test_extract_base_domain_edge_cases() {
        // Single part domain
        assert_eq!(
            DnsRecordService::extract_base_domain("localhost"),
            "localhost"
        );

        // Two parts only
        assert_eq!(
            DnsRecordService::extract_base_domain("example.com"),
            "example.com"
        );

        // Empty string
        assert_eq!(DnsRecordService::extract_base_domain(""), "");
    }

    // ==================== ManualDnsInstructions tests ====================

    #[test]
    fn test_manual_instructions_format() {
        let instructions = ManualDnsInstructions {
            record_type: "A".to_string(),
            name: "www".to_string(),
            value: "1.2.3.4".to_string(),
            ttl: 300,
            priority: None,
            instructions: "Test instructions".to_string(),
        };

        assert_eq!(instructions.record_type, "A");
        assert_eq!(instructions.name, "www");
        assert_eq!(instructions.value, "1.2.3.4");
        assert_eq!(instructions.ttl, 300);
        assert!(instructions.priority.is_none());
    }

    #[test]
    fn test_manual_instructions_with_priority() {
        let instructions = ManualDnsInstructions {
            record_type: "MX".to_string(),
            name: "@".to_string(),
            value: "mail.example.com".to_string(),
            ttl: 3600,
            priority: Some(10),
            instructions: "Add MX record".to_string(),
        };

        assert_eq!(instructions.record_type, "MX");
        assert_eq!(instructions.name, "@");
        assert_eq!(instructions.value, "mail.example.com");
        assert_eq!(instructions.ttl, 3600);
        assert_eq!(instructions.priority, Some(10));
    }

    // ==================== DnsOperationResult tests ====================

    #[test]
    fn test_operation_result_automatic_success() {
        let result = DnsOperationResult {
            automatic: true,
            success: true,
            domain: "example.com".to_string(),
            name: "www".to_string(),
            record_type: "A".to_string(),
            message: "Successfully set A record".to_string(),
            manual_instructions: None,
        };

        assert!(result.automatic);
        assert!(result.success);
        assert!(result.manual_instructions.is_none());
    }

    #[test]
    fn test_operation_result_manual_fallback() {
        let result = DnsOperationResult {
            automatic: false,
            success: false,
            domain: "example.com".to_string(),
            name: "www".to_string(),
            record_type: "A".to_string(),
            message: "No DNS provider configured".to_string(),
            manual_instructions: Some(ManualDnsInstructions {
                record_type: "A".to_string(),
                name: "www".to_string(),
                value: "192.0.2.1".to_string(),
                ttl: 300,
                priority: None,
                instructions: "Add A record manually".to_string(),
            }),
        };

        assert!(!result.automatic);
        assert!(!result.success);
        assert!(result.manual_instructions.is_some());
    }

    // ==================== IP address validation tests ====================

    #[test]
    fn test_valid_ipv4_address() {
        let ip = "192.0.2.1";
        let parsed: std::net::IpAddr = ip.parse().unwrap();
        assert!(parsed.is_ipv4());
    }

    #[test]
    fn test_valid_ipv6_address() {
        let ip = "2001:db8::1";
        let parsed: std::net::IpAddr = ip.parse().unwrap();
        assert!(parsed.is_ipv6());
    }

    #[test]
    fn test_invalid_ip_address() {
        let ip = "not-an-ip";
        let result: Result<std::net::IpAddr, _> = ip.parse();
        assert!(result.is_err());
    }

    // ==================== Content extraction tests ====================

    #[test]
    fn test_content_value_extraction_a_record() {
        let content = DnsRecordContent::A {
            address: "192.0.2.1".to_string(),
        };
        assert_eq!(content.to_value_string(), "192.0.2.1");
    }

    #[test]
    fn test_content_value_extraction_aaaa_record() {
        let content = DnsRecordContent::AAAA {
            address: "2001:db8::1".to_string(),
        };
        assert_eq!(content.to_value_string(), "2001:db8::1");
    }

    #[test]
    fn test_content_value_extraction_cname_record() {
        let content = DnsRecordContent::CNAME {
            target: "www.example.com".to_string(),
        };
        assert_eq!(content.to_value_string(), "www.example.com");
    }

    #[test]
    fn test_content_value_extraction_txt_record() {
        let content = DnsRecordContent::TXT {
            content: "v=spf1 -all".to_string(),
        };
        assert_eq!(content.to_value_string(), "v=spf1 -all");
    }

    #[test]
    fn test_content_value_extraction_mx_record() {
        let content = DnsRecordContent::MX {
            priority: 10,
            target: "mail.example.com".to_string(),
        };
        assert_eq!(content.to_value_string(), "10 mail.example.com");
    }

    #[test]
    fn test_content_value_extraction_ns_record() {
        let content = DnsRecordContent::NS {
            nameserver: "ns1.example.com".to_string(),
        };
        assert_eq!(content.to_value_string(), "ns1.example.com");
    }

    #[test]
    fn test_content_value_extraction_srv_record() {
        let content = DnsRecordContent::SRV {
            priority: 10,
            weight: 5,
            port: 5060,
            target: "sip.example.com".to_string(),
        };
        assert_eq!(content.to_value_string(), "10 5 5060 sip.example.com");
    }

    #[test]
    fn test_content_value_extraction_caa_record() {
        let content = DnsRecordContent::CAA {
            flags: 0,
            tag: "issue".to_string(),
            value: "letsencrypt.org".to_string(),
        };
        assert_eq!(content.to_value_string(), "0 issue \"letsencrypt.org\"");
    }

    #[test]
    fn test_content_value_extraction_ptr_record() {
        let content = DnsRecordContent::PTR {
            target: "host.example.com".to_string(),
        };
        assert_eq!(content.to_value_string(), "host.example.com");
    }

    // ==================== Record type determination tests ====================

    #[test]
    fn test_ipv4_uses_a_record() {
        let ip = "192.0.2.1";
        let parsed: std::net::IpAddr = ip.parse().unwrap();
        let is_v4 = parsed.is_ipv4();
        assert!(is_v4);
        // A record should be used for IPv4
    }

    #[test]
    fn test_ipv6_uses_aaaa_record() {
        let ip = "2001:db8::1";
        let parsed: std::net::IpAddr = ip.parse().unwrap();
        let is_v6 = parsed.is_ipv6();
        assert!(is_v6);
        // AAAA record should be used for IPv6
    }
}
