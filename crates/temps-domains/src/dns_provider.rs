use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use cloudflare::endpoints::{dns::dns, zones::zone};
use cloudflare::framework::{
    auth::Credentials, client::async_api::Client, client::ClientConfig, Environment,
};
use serde::{Deserialize, Serialize};

use tracing::{info, warn};
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
        // delete record if it exists
        match self.remove_txt_record(domain, name).await {
            Ok(_) => (),
            Err(e) => {
                info!("Failed to remove TXT record {}: {:?}", name, e);
            }
        }
        let zone_id = self.get_zone_id(domain).await?;
        info!(
            "Setting TXT record for zone: {} domain: {} name: {} value: {}",
            zone_id, domain, name, value
        );
        let params = dns::CreateDnsRecordParams {
            name,
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

        let response = self
            .client
            .request(&endpoint)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to create TXT record: {:?}", e))?;

        info!("TXT record created: {:?}", response);
        Ok(())
    }

    async fn remove_txt_record(&self, domain: &str, name: &str) -> Result<()> {
        let zone_id = self.get_zone_id(domain).await?;
        info!(
            "Removing TXT record for zone: {} domain: {} name: {}",
            zone_id, domain, name
        );
        let base_domain = domain
            .split('.')
            .rev()
            .take(2)
            .collect::<Vec<&str>>()
            .into_iter()
            .rev()
            .collect::<Vec<&str>>()
            .join(".");
        // Remove domain from name if it exists
        let record_name = if name.ends_with(&base_domain) {
            name[..name.len() - base_domain.len() - 1].to_string()
        } else {
            name.to_string()
        };
        info!("Record name: {}", record_name);

        let records = self.get_records(&zone_id, &record_name).await?;
        for record in records {
            let endpoint = dns::DeleteDnsRecord {
                zone_identifier: &zone_id,
                identifier: &record.id,
            };

            let response = self.client.request(&endpoint).await?;

            info!("TXT record removed: {:?}", response);
        }
        Ok(())
    }

    async fn set_a_record(&self, domain: &str, name: &str, ip_address: &str) -> Result<()> {
        let zone_id = self.get_zone_id(domain).await?;
        info!(
            "Setting A record for zone: {} domain: {} name: {} ip_address: {}",
            zone_id, domain, name, ip_address
        );

        // Remove existing A record if it exists
        if let Ok(existing_record) = self.get_record(&zone_id, name).await {
            let delete_endpoint = dns::DeleteDnsRecord {
                zone_identifier: &zone_id,
                identifier: &existing_record.id,
            };
            self.client.request(&delete_endpoint).await?;
            info!("Removed existing A record: {}", name);
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
        return response
            .result
            .first()
            .ok_or_else(|| anyhow::anyhow!("Zone not found"))
            .map(|zone| zone.id.to_string());
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

    async fn get_record(&self, zone_id: &str, name: &str) -> Result<CFDnsRecord> {
        let endpoint = dns::ListDnsRecords {
            zone_identifier: zone_id,
            params: dns::ListDnsRecordsParams {
                record_type: Some(dns::DnsContent::TXT {
                    content: "".to_string(),
                }),
                name: Some(name.to_string()),
                ..Default::default()
            },
        };

        let response = self
            .client
            .request(&endpoint)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to list DNS records: {:?}", e))?;

        response
            .result
            .first()
            .ok_or_else(|| {
                anyhow::anyhow!("Record not found for zone_id {} and name {}", zone_id, name)
            })
            .map(|cf_record| Self::map_cloudflare_record_to_custom(cf_record))
    }
    async fn get_records(&self, zone_id: &str, name: &str) -> Result<Vec<CFDnsRecord>> {
        let endpoint = dns::ListDnsRecords {
            zone_identifier: zone_id,
            params: dns::ListDnsRecordsParams {
                record_type: Some(dns::DnsContent::TXT {
                    content: "".to_string(),
                }),
                name: Some(name.to_string()),
                ..Default::default()
            },
        };

        let response = self
            .client
            .request(&endpoint)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to list DNS records: {:?}", e))?;

        Ok(response
            .result
            .into_iter()
            .map(|cf_record| Self::map_cloudflare_record_to_custom(&cf_record))
            .collect())
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
