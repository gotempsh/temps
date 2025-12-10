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
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::{debug, info, warn};

use super::credentials::AzureCredentials;
use super::traits::{
    DnsProvider, DnsProviderCapabilities, DnsProviderType, DnsRecord, DnsRecordContent,
    DnsRecordRequest, DnsRecordType, DnsZone,
};
use crate::errors::DnsError;

const AZURE_MANAGEMENT_BASE: &str = "https://management.azure.com";
const AZURE_LOGIN_URL: &str = "https://login.microsoftonline.com";

/// Azure DNS provider
pub struct AzureProvider {
    client: Client,
    credentials: AzureCredentials,
    #[allow(dead_code)]
    base_url: String,
    /// Cached access token
    access_token: tokio::sync::RwLock<Option<String>>,
}

/// Azure API response structures
#[derive(Debug, Deserialize)]
struct ZonesResponse {
    value: Vec<AzureZone>,
    #[serde(rename = "nextLink")]
    #[allow(dead_code)]
    next_link: Option<String>,
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

#[derive(Debug, Deserialize)]
struct RecordSetsResponse {
    value: Vec<AzureRecordSet>,
    #[serde(rename = "nextLink")]
    #[allow(dead_code)]
    next_link: Option<String>,
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

    /// Make an authenticated request to Azure DNS API
    async fn api_request<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        path: &str,
        body: Option<&impl Serialize>,
    ) -> Result<T, DnsError> {
        let token = self.get_access_token().await?;
        let url = format!("{}{}?api-version=2018-05-01", self.base_url, path);

        debug!("Azure DNS API request: {} {}", method, path);

        let mut request = match method {
            "GET" => self.client.get(&url),
            "PUT" => self.client.put(&url),
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
                "Azure API returned status {}: {}",
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

    /// Make a DELETE request (returns no body)
    async fn api_delete(&self, path: &str) -> Result<(), DnsError> {
        let token = self.get_access_token().await?;
        let url = format!("{}{}?api-version=2018-05-01", self.base_url, path);

        debug!("Azure DNS API DELETE: {}", path);

        let response = self
            .client
            .delete(&url)
            .header("Authorization", format!("Bearer {}", token))
            .send()
            .await
            .map_err(|e| DnsError::ApiError(format!("API request failed: {}", e)))?;

        let status = response.status();

        if !status.is_success() && status.as_u16() != 204 {
            let error_body = response.text().await.unwrap_or_default();
            return Err(DnsError::ApiError(format!(
                "Azure API returned status {}: {}",
                status, error_body
            )));
        }

        Ok(())
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
        let record_type_str = record_set
            .record_type
            .as_ref()
            .map(|s| s.as_str())
            .unwrap_or("");

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
        let response: ZonesResponse = self.api_request("GET", &path, None::<&()>).await?;

        Ok(response
            .value
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
        let path = format!(
            "/subscriptions/{}/resourceGroups/{}/providers/Microsoft.Network/dnsZones/{}/recordsets",
            self.credentials.subscription_id, self.credentials.resource_group, domain
        );
        let response: RecordSetsResponse = self.api_request("GET", &path, None::<&()>).await?;

        Ok(response
            .value
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
        let record_type = Self::azure_record_type(request.content.record_type());
        let record_set = Self::build_record_set(&request);

        let path = format!(
            "/subscriptions/{}/resourceGroups/{}/providers/Microsoft.Network/dnsZones/{}/{}/{}",
            self.credentials.subscription_id,
            self.credentials.resource_group,
            domain,
            record_type,
            request.name
        );

        let response: AzureRecordSet = self.api_request("PUT", &path, Some(&record_set)).await?;

        let records = Self::convert_record_set(&response, domain);
        let record = records
            .into_iter()
            .next()
            .ok_or_else(|| DnsError::ApiError("Failed to convert created record".to_string()))?;

        info!("Created DNS record {} for domain {}", request.name, domain);

        Ok(record)
    }

    async fn update_record(
        &self,
        domain: &str,
        _record_id: &str,
        request: DnsRecordRequest,
    ) -> Result<DnsRecord, DnsError> {
        // Azure uses PUT for create-or-update, same as create
        self.create_record(domain, request).await
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

        let name = parts[0];
        let record_type = parts[1];

        let path = format!(
            "/subscriptions/{}/resourceGroups/{}/providers/Microsoft.Network/dnsZones/{}/{}/{}",
            self.credentials.subscription_id,
            self.credentials.resource_group,
            domain,
            record_type,
            name
        );

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
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
