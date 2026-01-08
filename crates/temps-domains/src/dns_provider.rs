use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use cloudflare::endpoints::{dns::dns, zones::zone};
use cloudflare::framework::{
    auth::Credentials, client::async_api::Client, client::ClientConfig, Environment,
};
use hickory_resolver::config::{NameServerConfig, ResolverConfig, ResolverOpts};
use hickory_resolver::name_server::TokioConnectionProvider;
use hickory_resolver::proto::xfer::Protocol;
use hickory_resolver::Resolver;
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use tracing::{debug, info, warn};
#[derive(Debug, Serialize, Deserialize)]
pub struct CFDnsRecord {
    pub id: String,
    pub zone_id: String,
    pub zone_name: String,
    pub name: String,
    pub content: dns::DnsContent,
    pub proxiable: bool,
    pub proxied: bool,
    pub ttl: u32,
    pub created_on: DateTime<Utc>,
    pub modified_on: DateTime<Utc>,
}

#[async_trait]
pub trait DnsProviderService: Send + Sync {
    async fn set_txt_record(&self, domain: &str, name: &str, value: &str) -> Result<()>;
    async fn remove_txt_record(&self, domain: &str, name: &str) -> Result<()>;
    async fn set_a_record(&self, domain: &str, name: &str, ip_address: &str) -> Result<()>;
    async fn get_a_record(&self, domain: &str, name: &str) -> Result<Option<CFDnsRecord>>;
    async fn supports_automatic_challenges(&self, domain: &str) -> bool;
    fn get_provider_type(&self) -> String;
}

pub struct DummyDnsProvider {}

#[async_trait]
impl DnsProviderService for DummyDnsProvider {
    async fn get_a_record(&self, _domain: &str, _name: &str) -> Result<Option<CFDnsRecord>> {
        warn!("Dummy DNS provider does not get A records");
        Ok(None)
    }
    async fn set_txt_record(&self, _domain: &str, _name: &str, _value: &str) -> Result<()> {
        warn!("Dummy DNS provider does not set TXT records");
        Ok(())
    }

    async fn remove_txt_record(&self, _domain: &str, _name: &str) -> Result<()> {
        warn!("Dummy DNS provider does not remove TXT records");
        Ok(())
    }

    async fn set_a_record(&self, _domain: &str, _name: &str, _ip_address: &str) -> Result<()> {
        warn!("Dummy DNS provider does not set A records");
        Ok(())
    }

    fn get_provider_type(&self) -> String {
        "dummy".to_string()
    }

    async fn supports_automatic_challenges(&self, _domain: &str) -> bool {
        false // Dummy provider never supports automatic challenges
    }
}

pub struct ManualDnsProvider {}

impl Default for ManualDnsProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl ManualDnsProvider {
    pub fn new() -> Self {
        Self {}
    }
}

#[async_trait]
impl DnsProviderService for ManualDnsProvider {
    async fn get_a_record(&self, _domain: &str, _name: &str) -> Result<Option<CFDnsRecord>> {
        warn!("Manual DNS provider does not get A records");
        Ok(None)
    }
    async fn set_txt_record(&self, _domain: &str, _name: &str, _value: &str) -> Result<()> {
        warn!("Manual DNS provider does not set TXT records");
        Ok(())
    }

    async fn remove_txt_record(&self, _domain: &str, _name: &str) -> Result<()> {
        warn!("Manual DNS provider does not remove TXT records");
        Ok(())
    }

    async fn set_a_record(&self, _domain: &str, _name: &str, _ip_address: &str) -> Result<()> {
        warn!("Manual DNS provider does not set A records");
        Ok(())
    }

    fn get_provider_type(&self) -> String {
        "manual".to_string()
    }

    async fn supports_automatic_challenges(&self, _domain: &str) -> bool {
        false // Manual provider never supports automatic challenges
    }
}

pub struct CloudflareDnsProvider {
    client: Client,
}

impl CloudflareDnsProvider {
    pub fn new(api_token: String) -> Self {
        let credentials = Credentials::UserAuthToken {
            token: api_token.clone(),
        };
        let client = Client::new(
            credentials,
            ClientConfig::default(),
            Environment::Production,
        )
        .expect("Failed to create Cloudflare client");

        Self { client }
    }
}
#[async_trait]
impl DnsProviderService for CloudflareDnsProvider {
    async fn get_a_record(&self, domain: &str, name: &str) -> Result<Option<CFDnsRecord>> {
        let zone_id = self.get_zone_id(domain).await?;
        let endpoint = dns::ListDnsRecords {
            zone_identifier: &zone_id,
            params: dns::ListDnsRecordsParams {
                name: Some(name.to_string()),
                ..Default::default()
            },
        };

        let response = self
            .client
            .request(&endpoint)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to list A records: {:?}", e))?;

        if let Some(record) = response.result.into_iter().find(|r| r.name == name) {
            info!("A record found: {:?}", record);
            Ok(Some(Self::map_cloudflare_record_to_custom(&record)))
        } else {
            info!("No A record found for name: {}", name);
            Ok(None)
        }
    }
    async fn set_txt_record(&self, domain: &str, name: &str, value: &str) -> Result<()> {
        let zone_id = self.get_zone_id(domain).await?;

        // Extract the base domain (zone name)
        let base_domain = domain
            .split('.')
            .rev()
            .take(2)
            .collect::<Vec<&str>>()
            .into_iter()
            .rev()
            .collect::<Vec<&str>>()
            .join(".");

        info!(
            "Setting TXT record for zone: {} base_domain: {} name: {} value: {}",
            zone_id, base_domain, name, value
        );

        // Get all existing TXT records with this name (try both full name and relative name)
        let existing_records = self.get_txt_records_by_full_name(&zone_id, name).await?;

        // Check if a record with the exact same value already exists
        for record in &existing_records {
            if let dns::DnsContent::TXT { content } = &record.content {
                if content == value {
                    info!(
                        "TXT record already exists with same value, skipping creation: {}",
                        name
                    );
                    return Ok(());
                }
            }
        }

        // NOTE: Do NOT remove existing TXT records with different values!
        // DNS-01 challenges for wildcard certificates require MULTIPLE TXT records
        // with the same name but different values to coexist.
        // Each authorization (wildcard + base domain) needs its own TXT record.
        if !existing_records.is_empty() {
            info!(
                "Found {} existing TXT record(s) for {}. Adding new record (multiple TXT records are allowed for DNS-01 challenges).",
                existing_records.len(),
                name
            );
        }

        // Calculate the relative name (without zone suffix) for creation
        // Cloudflare accepts both, but relative names are more reliable
        let relative_name = if name.ends_with(&format!(".{}", base_domain)) {
            name.strip_suffix(&format!(".{}", base_domain))
                .unwrap_or(name)
                .to_string()
        } else {
            name.to_string()
        };

        info!(
            "Creating TXT record with relative name: {} (full: {})",
            relative_name, name
        );

        // Create the new TXT record using the relative name
        let params = dns::CreateDnsRecordParams {
            name: &relative_name,
            content: dns::DnsContent::TXT {
                content: value.to_string(),
            },
            ttl: Some(120),
            priority: None,
            proxied: None,
        };

        let endpoint = dns::CreateDnsRecord {
            zone_identifier: &zone_id,
            params,
        };

        match self.client.request(&endpoint).await {
            Ok(response) => {
                info!("TXT record created successfully: {:?}", response);
                Ok(())
            }
            Err(e) => {
                // If creation failed, check if the record now exists (race condition)
                warn!("Create failed, checking if record exists: {:?}", e);
                let check_records = self.get_txt_records_by_full_name(&zone_id, name).await?;
                for record in &check_records {
                    if let dns::DnsContent::TXT { content } = &record.content {
                        if content == value {
                            info!(
                                "Record was created by another process, continuing: {}",
                                name
                            );
                            return Ok(());
                        }
                    }
                }
                Err(anyhow::anyhow!("Failed to create TXT record: {:?}", e))
            }
        }
    }

    async fn remove_txt_record(&self, domain: &str, name: &str) -> Result<()> {
        let zone_id = self.get_zone_id(domain).await?;
        info!(
            "Removing TXT record for zone: {} domain: {} name: {}",
            zone_id, domain, name
        );

        // Use the full name to search for records
        let records = self.get_txt_records_by_full_name(&zone_id, name).await?;

        if records.is_empty() {
            info!("No TXT records found with name: {}", name);
            return Ok(());
        }

        for record in records {
            info!("Deleting TXT record: {} (id: {})", record.name, record.id);
            let endpoint = dns::DeleteDnsRecord {
                zone_identifier: &zone_id,
                identifier: &record.id,
            };

            match self.client.request(&endpoint).await {
                Ok(response) => info!("TXT record removed: {:?}", response),
                Err(e) => warn!("Failed to remove TXT record {}: {:?}", record.id, e),
            }
        }
        Ok(())
    }

    async fn set_a_record(&self, domain: &str, name: &str, ip_address: &str) -> Result<()> {
        let zone_id = self.get_zone_id(domain).await?;
        info!(
            "Setting A record for zone: {} domain: {} name: {} ip_address: {}",
            zone_id, domain, name, ip_address
        );

        // Build the full record name for lookup
        let full_name = if name.ends_with(&format!(".{}", domain)) {
            name.to_string()
        } else if name == "*" || name.starts_with("*.") {
            // Handle wildcard records
            format!("{}.{}", name, domain)
        } else {
            format!("{}.{}", name, domain)
        };

        // Remove existing A record if it exists (use proper A record lookup)
        if let Ok(Some(existing_record)) = self.get_a_record_by_name(&zone_id, &full_name).await {
            info!(
                "Found existing A record for {}: id={}, removing before update",
                full_name, existing_record.id
            );
            let delete_endpoint = dns::DeleteDnsRecord {
                zone_identifier: &zone_id,
                identifier: &existing_record.id,
            };
            match self.client.request(&delete_endpoint).await {
                Ok(_) => info!("Removed existing A record: {}", full_name),
                Err(e) => warn!("Failed to remove existing A record {}: {:?}", full_name, e),
            }
        }

        let params = dns::CreateDnsRecordParams {
            name,
            content: dns::DnsContent::A {
                content: ip_address.parse()?,
            },
            ttl: Some(1), // 1 = Auto
            priority: None,
            proxied: Some(false),
        };

        let endpoint = dns::CreateDnsRecord {
            zone_identifier: &zone_id,
            params,
        };

        let response = self
            .client
            .request(&endpoint)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to create A record: {:?}", e))?;

        info!("A record created: {:?}", response);
        Ok(())
    }

    fn get_provider_type(&self) -> String {
        "cloudflare".to_string()
    }

    async fn supports_automatic_challenges(&self, domain: &str) -> bool {
        // Extract base domain by taking last two parts
        let base_domain = domain
            .split('.')
            .rev()
            .take(2)
            .collect::<Vec<&str>>()
            .into_iter()
            .rev()
            .collect::<Vec<&str>>()
            .join(".");

        info!("Checking zone access for domain: {}", base_domain);

        // Try to get zone for the base domain
        match self.get_zone_id(&base_domain).await {
            Ok(_) => {
                info!(
                    "Successfully verified Cloudflare zone access for domain {}",
                    base_domain
                );
                true
            }
            Err(e) => {
                warn!(
                    "Cloudflare zone access test failed for domain {}: {}",
                    base_domain, e
                );
                false
            }
        }
    }
}

impl CloudflareDnsProvider {
    pub async fn get_zones(&self) -> Result<Vec<cloudflare::endpoints::zones::zone::Zone>> {
        let endpoint = zone::ListZones {
            params: Default::default(),
        };
        let response = self.client.request(&endpoint).await?;
        Ok(response.result)
    }
    pub async fn get_zone_id(&self, domain: &str) -> Result<String> {
        // Extract the base domain from the given domain
        let base_domain = domain
            .split('.')
            .rev()
            .take(2)
            .collect::<Vec<&str>>()
            .into_iter()
            .rev()
            .collect::<Vec<&str>>()
            .join(".");

        info!("Fetching zone ID for base domain: {}", base_domain);
        let endpoint = zone::ListZones {
            params: zone::ListZonesParams {
                name: Some(base_domain.to_string()),
                ..Default::default()
            },
        };

        let response = self
            .client
            .request(&endpoint)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to list zones: {:?}", e))?;
        response
            .result
            .first()
            .ok_or_else(|| anyhow::anyhow!("Zone not found"))
            .map(|zone| zone.id.to_string())
    }
}

impl CloudflareDnsProvider {
    fn map_cloudflare_record_to_custom(
        cf_record: &cloudflare::endpoints::dns::dns::DnsRecord,
    ) -> CFDnsRecord {
        CFDnsRecord {
            id: cf_record.id.clone(),
            zone_id: cf_record.id.clone(),
            zone_name: cf_record.name.clone(),
            name: cf_record.name.clone(),
            content: cf_record.content.clone(),
            proxiable: cf_record.proxiable,
            proxied: cf_record.proxied,
            ttl: cf_record.ttl,
            created_on: cf_record.created_on,
            modified_on: cf_record.modified_on,
        }
    }

    /// Get A record by full DNS name
    /// Returns the first matching A record if found
    async fn get_a_record_by_name(
        &self,
        zone_id: &str,
        full_name: &str,
    ) -> Result<Option<cloudflare::endpoints::dns::dns::DnsRecord>> {
        info!("Searching for A record with full name: {}", full_name);

        let endpoint = dns::ListDnsRecords {
            zone_identifier: zone_id,
            params: dns::ListDnsRecordsParams {
                name: Some(full_name.to_string()),
                ..Default::default()
            },
        };

        let response = self
            .client
            .request(&endpoint)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to list A records: {:?}", e))?;

        // Filter for A records client-side
        let a_record = response
            .result
            .into_iter()
            .find(|r| matches!(r.content, dns::DnsContent::A { .. }));

        if let Some(ref record) = a_record {
            info!("Found A record: id={}, name={}", record.id, record.name);
        } else {
            info!("No A record found for name: {}", full_name);
        }

        Ok(a_record)
    }

    /// Get TXT records by full DNS name (without stripping domain)
    /// Returns the raw Cloudflare DnsRecord objects for more detailed inspection
    async fn get_txt_records_by_full_name(
        &self,
        zone_id: &str,
        full_name: &str,
    ) -> Result<Vec<cloudflare::endpoints::dns::dns::DnsRecord>> {
        info!("Searching for TXT records with full name: {}", full_name);

        // Search by name only (don't filter by record_type in the API call)
        // The record_type filter with empty content can cause issues
        let endpoint = dns::ListDnsRecords {
            zone_identifier: zone_id,
            params: dns::ListDnsRecordsParams {
                name: Some(full_name.to_string()),
                ..Default::default()
            },
        };

        let response = self
            .client
            .request(&endpoint)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to list TXT records: {:?}", e))?;

        // Filter for TXT records client-side
        let txt_records: Vec<_> = response
            .result
            .into_iter()
            .filter(|r| matches!(r.content, dns::DnsContent::TXT { .. }))
            .collect();

        info!(
            "Found {} TXT record(s) for name: {}",
            txt_records.len(),
            full_name
        );

        for record in &txt_records {
            if let dns::DnsContent::TXT { content } = &record.content {
                info!("  - TXT record id={} value={}", record.id, content);
            }
        }

        Ok(txt_records)
    }

    pub async fn test_api_access(&self) -> Result<bool> {
        match self.get_zones().await {
            Ok(_) => {
                info!("Cloudflare API access test successful");
                Ok(true)
            }
            Err(e) => {
                warn!("Cloudflare API access test failed: {}", e);
                Ok(false)
            }
        }
    }
}

pub fn create_dns_provider_from_settings(
    dns_provider: &str,
    cloudflare_api_key: &str,
) -> Box<dyn DnsProviderService> {
    match dns_provider {
        "cloudflare" => Box::new(CloudflareDnsProvider::new(cloudflare_api_key.to_string())),
        "manual" => Box::new(ManualDnsProvider {}),
        // Add other providers here as needed
        _ => {
            tracing::warn!(
                "Unsupported DNS provider: {}, falling back to manual",
                dns_provider
            );
            Box::new(ManualDnsProvider {})
        }
    }
}

/// DNS propagation verification using multiple public DNS servers
/// This helps ensure TXT records are visible globally before ACME validation
pub struct DnsPropagationChecker {
    /// Public DNS servers to query for verification
    /// Using diverse providers increases confidence in global propagation
    dns_servers: Vec<DnsServerInfo>,
}

#[derive(Clone)]
struct DnsServerInfo {
    name: &'static str,
    ip: Ipv4Addr,
}

/// Result of checking DNS propagation across multiple servers
#[derive(Debug, Clone)]
pub struct DnsPropagationResult {
    /// Name of the DNS record being checked
    pub record_name: String,
    /// Expected value(s) to find
    pub expected_values: Vec<String>,
    /// Results from each DNS server
    pub server_results: Vec<DnsServerResult>,
    /// Whether propagation is considered complete (majority of servers see the record)
    pub is_propagated: bool,
    /// Percentage of servers that see the expected record
    pub propagation_percentage: u8,
}

#[derive(Debug, Clone)]
pub struct DnsServerResult {
    /// Name of the DNS server (e.g., "Google", "Cloudflare")
    pub server_name: String,
    /// IP address of the DNS server
    pub server_ip: String,
    /// Whether the expected TXT record was found
    pub found: bool,
    /// Values found at the record (if any)
    pub values_found: Vec<String>,
    /// Error message if the query failed
    pub error: Option<String>,
}

impl Default for DnsPropagationChecker {
    fn default() -> Self {
        Self::new()
    }
}

impl DnsPropagationChecker {
    /// Create a new DNS propagation checker with default public DNS servers
    pub fn new() -> Self {
        Self {
            dns_servers: vec![
                DnsServerInfo {
                    name: "Google",
                    ip: Ipv4Addr::new(8, 8, 8, 8),
                },
                DnsServerInfo {
                    name: "Google Secondary",
                    ip: Ipv4Addr::new(8, 8, 4, 4),
                },
                DnsServerInfo {
                    name: "Cloudflare",
                    ip: Ipv4Addr::new(1, 1, 1, 1),
                },
                DnsServerInfo {
                    name: "Cloudflare Secondary",
                    ip: Ipv4Addr::new(1, 0, 0, 1),
                },
                DnsServerInfo {
                    name: "Quad9",
                    ip: Ipv4Addr::new(9, 9, 9, 9),
                },
                DnsServerInfo {
                    name: "OpenDNS",
                    ip: Ipv4Addr::new(208, 67, 222, 222),
                },
            ],
        }
    }

    /// Check if TXT records have propagated to multiple DNS servers
    /// Returns true if at least the specified percentage of servers see all expected values
    pub async fn verify_txt_propagation(
        &self,
        record_name: &str,
        expected_values: &[String],
        min_propagation_percent: u8,
    ) -> DnsPropagationResult {
        info!(
            "Verifying DNS propagation for {} with {} expected value(s) across {} servers",
            record_name,
            expected_values.len(),
            self.dns_servers.len()
        );

        let mut server_results = Vec::new();
        let mut servers_with_all_records = 0;

        for server in &self.dns_servers {
            let result = self
                .query_txt_record(server, record_name, expected_values)
                .await;

            if result.found {
                servers_with_all_records += 1;
            }

            debug!(
                "DNS server {} ({}): found={}, values={:?}",
                server.name, server.ip, result.found, result.values_found
            );

            server_results.push(result);
        }

        let propagation_percentage = if self.dns_servers.is_empty() {
            0
        } else {
            ((servers_with_all_records as f32 / self.dns_servers.len() as f32) * 100.0) as u8
        };

        let is_propagated = propagation_percentage >= min_propagation_percent;

        info!(
            "DNS propagation check complete: {}/{} servers ({}%) see all records. Threshold: {}%. Propagated: {}",
            servers_with_all_records,
            self.dns_servers.len(),
            propagation_percentage,
            min_propagation_percent,
            is_propagated
        );

        DnsPropagationResult {
            record_name: record_name.to_string(),
            expected_values: expected_values.to_vec(),
            server_results,
            is_propagated,
            propagation_percentage,
        }
    }

    /// Query a specific DNS server for TXT records
    async fn query_txt_record(
        &self,
        server: &DnsServerInfo,
        record_name: &str,
        expected_values: &[String],
    ) -> DnsServerResult {
        // Create resolver config for this specific DNS server using hickory-resolver 0.25+ API
        let name_server =
            NameServerConfig::new(SocketAddr::new(IpAddr::V4(server.ip), 53), Protocol::Udp);

        let mut resolver_config = ResolverConfig::new();
        resolver_config.add_name_server(name_server);

        // Configure resolver options
        let mut resolver_opts = ResolverOpts::default();
        resolver_opts.timeout = Duration::from_secs(5);
        resolver_opts.attempts = 2;
        resolver_opts.cache_size = 0; // Disable caching to get fresh results

        // Build resolver using the new builder API
        let resolver =
            Resolver::builder_with_config(resolver_config, TokioConnectionProvider::default())
                .with_options(resolver_opts)
                .build();

        // Query TXT records
        match resolver.txt_lookup(record_name).await {
            Ok(lookup) => {
                let values_found: Vec<String> = lookup
                    .iter()
                    .flat_map(|txt| {
                        txt.iter()
                            .map(|data| String::from_utf8_lossy(data).to_string())
                    })
                    .collect();

                // Check if all expected values are present
                let all_found = expected_values
                    .iter()
                    .all(|expected| values_found.iter().any(|found| found == expected));

                DnsServerResult {
                    server_name: server.name.to_string(),
                    server_ip: server.ip.to_string(),
                    found: all_found,
                    values_found,
                    error: None,
                }
            }
            Err(e) => {
                // NXDOMAIN or no records is not necessarily an error - just means not propagated yet
                let error_str = e.to_string();
                let is_not_found = error_str.contains("no records found")
                    || error_str.contains("NXDomain")
                    || error_str.contains("NoRecordsFound");

                DnsServerResult {
                    server_name: server.name.to_string(),
                    server_ip: server.ip.to_string(),
                    found: false,
                    values_found: vec![],
                    error: if is_not_found {
                        None // Not found is expected during propagation
                    } else {
                        Some(error_str)
                    },
                }
            }
        }
    }

    /// Wait for DNS propagation with polling
    /// Returns the final propagation result, or None if timeout is reached
    pub async fn wait_for_propagation(
        &self,
        record_name: &str,
        expected_values: &[String],
        min_propagation_percent: u8,
        max_wait_seconds: u32,
        poll_interval_seconds: u32,
    ) -> Option<DnsPropagationResult> {
        let start = std::time::Instant::now();
        let max_duration = Duration::from_secs(max_wait_seconds as u64);
        let poll_interval = Duration::from_secs(poll_interval_seconds as u64);

        info!(
            "Waiting up to {}s for DNS propagation of {} (polling every {}s)",
            max_wait_seconds, record_name, poll_interval_seconds
        );

        loop {
            let result = self
                .verify_txt_propagation(record_name, expected_values, min_propagation_percent)
                .await;

            if result.is_propagated {
                info!(
                    "DNS propagation complete for {} after {:?}",
                    record_name,
                    start.elapsed()
                );
                return Some(result);
            }

            if start.elapsed() >= max_duration {
                warn!(
                    "DNS propagation timeout for {} after {:?}. Only {}% of servers see the record.",
                    record_name,
                    start.elapsed(),
                    result.propagation_percentage
                );
                return Some(result); // Return partial result
            }

            info!(
                "DNS not fully propagated yet ({}%). Waiting {}s before next check...",
                result.propagation_percentage, poll_interval_seconds
            );
            tokio::time::sleep(poll_interval).await;
        }
    }
}
