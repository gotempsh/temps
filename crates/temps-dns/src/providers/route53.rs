//! AWS Route 53 DNS provider implementation
//!
//! This provider uses the AWS Route 53 API to manage DNS records.
//! It requires IAM credentials with Route53 permissions.
//!
//! Required IAM Policy:
//! - route53:ListHostedZones
//! - route53:ListResourceRecordSets
//! - route53:ChangeResourceRecordSets
//! - route53:GetHostedZone

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::{debug, info, warn};

use super::credentials::Route53Credentials;
use super::traits::{
    DnsProvider, DnsProviderCapabilities, DnsProviderType, DnsRecord, DnsRecordContent,
    DnsRecordRequest, DnsRecordType, DnsZone,
};
use crate::errors::DnsError;

const AWS_ROUTE53_ENDPOINT: &str = "https://route53.amazonaws.com";

/// AWS Route 53 DNS provider
pub struct Route53Provider {
    client: Client,
    credentials: Route53Credentials,
    region: String,
}

/// AWS Signature V4 signing implementation
mod aws_signing {
    use chrono::Utc;
    use hmac::{Hmac, Mac};
    use sha2::{Digest, Sha256};

    type HmacSha256 = Hmac<Sha256>;

    pub fn sign_request(
        method: &str,
        uri: &str,
        query_string: &str,
        headers: &[(&str, &str)],
        payload: &str,
        access_key: &str,
        secret_key: &str,
        region: &str,
        service: &str,
    ) -> (String, String, String) {
        let now = Utc::now();
        let date_stamp = now.format("%Y%m%d").to_string();
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();

        // Create canonical request
        let payload_hash = hex::encode(Sha256::digest(payload.as_bytes()));

        let mut signed_headers: Vec<&str> = headers.iter().map(|(k, _)| *k).collect();
        signed_headers.sort();
        let signed_headers_str = signed_headers.join(";");

        let mut canonical_headers = String::new();
        let mut sorted_headers: Vec<_> = headers.to_vec();
        sorted_headers.sort_by(|a, b| a.0.cmp(b.0));
        for (key, value) in &sorted_headers {
            canonical_headers.push_str(&format!("{}:{}\n", key.to_lowercase(), value.trim()));
        }

        let canonical_request = format!(
            "{}\n{}\n{}\n{}\n{}\n{}",
            method, uri, query_string, canonical_headers, signed_headers_str, payload_hash
        );

        let canonical_request_hash = hex::encode(Sha256::digest(canonical_request.as_bytes()));

        // Create string to sign
        let credential_scope = format!("{}/{}/{}/aws4_request", date_stamp, region, service);
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{}\n{}\n{}",
            amz_date, credential_scope, canonical_request_hash
        );

        // Calculate signature
        let k_date = hmac_sha256(format!("AWS4{}", secret_key).as_bytes(), &date_stamp);
        let k_region = hmac_sha256(&k_date, region);
        let k_service = hmac_sha256(&k_region, service);
        let k_signing = hmac_sha256(&k_service, "aws4_request");
        let signature = hex::encode(hmac_sha256(&k_signing, &string_to_sign));

        // Create authorization header
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential={}/{}, SignedHeaders={}, Signature={}",
            access_key, credential_scope, signed_headers_str, signature
        );

        (authorization, amz_date, payload_hash)
    }

    fn hmac_sha256(key: &[u8], data: &str) -> Vec<u8> {
        let mut mac = HmacSha256::new_from_slice(key).expect("HMAC can take key of any size");
        mac.update(data.as_bytes());
        mac.finalize().into_bytes().to_vec()
    }
}

/// Route 53 API response structures
#[derive(Debug, Deserialize)]
struct ListHostedZonesResponse {
    #[serde(rename = "HostedZones")]
    hosted_zones: Option<HostedZonesWrapper>,
}

#[derive(Debug, Deserialize)]
struct HostedZonesWrapper {
    #[serde(rename = "HostedZone")]
    hosted_zone: Vec<HostedZone>,
}

#[derive(Debug, Deserialize)]
struct HostedZone {
    #[serde(rename = "Id")]
    id: String,
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "CallerReference")]
    #[allow(dead_code)]
    caller_reference: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ListResourceRecordSetsResponse {
    #[serde(rename = "ResourceRecordSets")]
    resource_record_sets: Option<ResourceRecordSetsWrapper>,
}

#[derive(Debug, Deserialize)]
struct ResourceRecordSetsWrapper {
    #[serde(rename = "ResourceRecordSet")]
    resource_record_set: Vec<ResourceRecordSet>,
}

#[derive(Debug, Deserialize, Clone)]
struct ResourceRecordSet {
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "Type")]
    record_type: String,
    #[serde(rename = "TTL")]
    ttl: Option<u32>,
    #[serde(rename = "ResourceRecords")]
    resource_records: Option<ResourceRecordsWrapper>,
}

#[derive(Debug, Deserialize, Clone)]
struct ResourceRecordsWrapper {
    #[serde(rename = "ResourceRecord")]
    resource_record: Vec<ResourceRecord>,
}

#[derive(Debug, Deserialize, Clone)]
struct ResourceRecord {
    #[serde(rename = "Value")]
    value: String,
}

/// Change batch request for Route 53
#[derive(Debug, Serialize)]
struct ChangeResourceRecordSetsRequest {
    #[serde(rename = "ChangeBatch")]
    change_batch: ChangeBatch,
}

#[derive(Debug, Serialize)]
struct ChangeBatch {
    #[serde(rename = "Comment")]
    #[serde(skip_serializing_if = "Option::is_none")]
    comment: Option<String>,
    #[serde(rename = "Changes")]
    changes: Changes,
}

#[derive(Debug, Serialize)]
struct Changes {
    #[serde(rename = "Change")]
    change: Vec<Change>,
}

#[derive(Debug, Serialize)]
struct Change {
    #[serde(rename = "Action")]
    action: String,
    #[serde(rename = "ResourceRecordSet")]
    resource_record_set: ChangeResourceRecordSet,
}

#[derive(Debug, Serialize)]
struct ChangeResourceRecordSet {
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "Type")]
    record_type: String,
    #[serde(rename = "TTL")]
    ttl: u32,
    #[serde(rename = "ResourceRecords")]
    resource_records: ChangeResourceRecords,
}

#[derive(Debug, Serialize)]
struct ChangeResourceRecords {
    #[serde(rename = "ResourceRecord")]
    resource_record: Vec<ChangeResourceRecord>,
}

#[derive(Debug, Serialize)]
struct ChangeResourceRecord {
    #[serde(rename = "Value")]
    value: String,
}

impl Route53Provider {
    /// Create a new Route 53 provider with the given credentials
    pub fn new(credentials: Route53Credentials) -> Result<Self, DnsError> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| DnsError::ApiError(format!("Failed to create HTTP client: {}", e)))?;

        let region = credentials
            .region
            .clone()
            .unwrap_or_else(|| "us-east-1".to_string());

        Ok(Self {
            client,
            credentials,
            region,
        })
    }

    /// Make a signed request to Route 53 API
    async fn api_request(
        &self,
        method: &str,
        path: &str,
        body: Option<&str>,
    ) -> Result<String, DnsError> {
        let url = format!("{}{}", AWS_ROUTE53_ENDPOINT, path);
        let payload = body.unwrap_or("");

        let host = "route53.amazonaws.com";
        let headers = vec![("host", host)];

        let (authorization, amz_date, _content_hash) = aws_signing::sign_request(
            method,
            path,
            "",
            &headers,
            payload,
            &self.credentials.access_key_id,
            &self.credentials.secret_access_key,
            &self.region,
            "route53",
        );

        let mut request = match method {
            "GET" => self.client.get(&url),
            "POST" => self.client.post(&url),
            "DELETE" => self.client.delete(&url),
            _ => {
                return Err(DnsError::ApiError(format!(
                    "Unsupported method: {}",
                    method
                )))
            }
        };

        request = request
            .header("Host", host)
            .header("X-Amz-Date", amz_date)
            .header("Authorization", authorization)
            .header("Content-Type", "application/xml");

        if let Some(body) = body {
            request = request.body(body.to_string());
        }

        debug!("Route53 API request: {} {}", method, path);

        let response = request
            .send()
            .await
            .map_err(|e| DnsError::ApiError(format!("API request failed: {}", e)))?;

        let status = response.status();
        let response_body = response
            .text()
            .await
            .map_err(|e| DnsError::ApiError(format!("Failed to read response: {}", e)))?;

        if !status.is_success() {
            return Err(DnsError::ApiError(format!(
                "API returned status {}: {}",
                status, response_body
            )));
        }

        Ok(response_body)
    }

    /// Get hosted zone ID for a domain (strips /hostedzone/ prefix)
    async fn get_zone_id(&self, domain: &str) -> Result<String, DnsError> {
        let zones = self.list_zones().await?;
        let normalized_domain = Self::normalize_domain(domain);

        for zone in zones {
            let zone_domain = Self::normalize_domain(&zone.name);
            if normalized_domain == zone_domain
                || normalized_domain.ends_with(&format!(".{}", zone_domain))
            {
                // Extract just the ID part (remove /hostedzone/ prefix)
                let id = zone.id.trim_start_matches("/hostedzone/").to_string();
                return Ok(id);
            }
        }

        Err(DnsError::ZoneNotFound(domain.to_string()))
    }

    /// Normalize domain name (remove trailing dot, lowercase)
    fn normalize_domain(domain: &str) -> String {
        domain.trim_end_matches('.').to_lowercase()
    }

    /// Convert Route 53 record type string to our type
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

    /// Convert a Route 53 record to our DnsRecord type
    fn convert_record(record: &ResourceRecordSet, zone_name: &str) -> Vec<DnsRecord> {
        let Some(records_wrapper) = &record.resource_records else {
            return vec![];
        };

        let record_type = match Self::parse_record_type(&record.record_type) {
            Some(t) => t,
            None => return vec![],
        };

        let zone_normalized = Self::normalize_domain(zone_name);
        let fqdn = Self::normalize_domain(&record.name);
        let name = if fqdn == zone_normalized {
            "@".to_string()
        } else {
            fqdn.strip_suffix(&format!(".{}", zone_normalized))
                .unwrap_or(&fqdn)
                .to_string()
        };

        records_wrapper
            .resource_record
            .iter()
            .filter_map(|rr| {
                let content = Self::parse_record_content(record_type, &rr.value)?;
                Some(DnsRecord {
                    id: Some(format!("{}::{}", fqdn, record.record_type)),
                    zone: zone_normalized.clone(),
                    name: name.clone(),
                    fqdn: fqdn.clone(),
                    content,
                    ttl: record.ttl.unwrap_or(300),
                    proxied: false,
                    metadata: HashMap::new(),
                })
            })
            .collect()
    }

    /// Parse record value into DnsRecordContent
    fn parse_record_content(record_type: DnsRecordType, value: &str) -> Option<DnsRecordContent> {
        match record_type {
            DnsRecordType::A => Some(DnsRecordContent::A {
                address: value.to_string(),
            }),
            DnsRecordType::AAAA => Some(DnsRecordContent::AAAA {
                address: value.to_string(),
            }),
            DnsRecordType::CNAME => Some(DnsRecordContent::CNAME {
                target: Self::normalize_domain(value),
            }),
            DnsRecordType::TXT => {
                // Remove surrounding quotes if present
                let content = value.trim_matches('"').to_string();
                Some(DnsRecordContent::TXT { content })
            }
            DnsRecordType::MX => {
                let parts: Vec<&str> = value.split_whitespace().collect();
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
                nameserver: Self::normalize_domain(value),
            }),
            DnsRecordType::SRV => {
                let parts: Vec<&str> = value.split_whitespace().collect();
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
                let parts: Vec<&str> = value.splitn(3, ' ').collect();
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
                target: Self::normalize_domain(value),
            }),
        }
    }

    /// Format record content for Route 53 API
    fn format_record_value(content: &DnsRecordContent) -> String {
        match content {
            DnsRecordContent::A { address } | DnsRecordContent::AAAA { address } => address.clone(),
            DnsRecordContent::CNAME { target }
            | DnsRecordContent::NS { nameserver: target }
            | DnsRecordContent::PTR { target } => {
                // Route 53 requires trailing dot for FQDN
                if target.ends_with('.') {
                    target.clone()
                } else {
                    format!("{}.", target)
                }
            }
            DnsRecordContent::TXT { content } => {
                // TXT records need to be quoted
                format!("\"{}\"", content)
            }
            DnsRecordContent::MX { priority, target } => {
                let target_fqdn = if target.ends_with('.') {
                    target.clone()
                } else {
                    format!("{}.", target)
                };
                format!("{} {}", priority, target_fqdn)
            }
            DnsRecordContent::SRV {
                priority,
                weight,
                port,
                target,
            } => {
                let target_fqdn = if target.ends_with('.') {
                    target.clone()
                } else {
                    format!("{}.", target)
                };
                format!("{} {} {} {}", priority, weight, port, target_fqdn)
            }
            DnsRecordContent::CAA { flags, tag, value } => {
                format!("{} {} \"{}\"", flags, tag, value)
            }
        }
    }

    /// Build FQDN with trailing dot for Route 53
    fn build_fqdn(name: &str, zone: &str) -> String {
        let fqdn = if name == "@" || name.is_empty() {
            zone.to_string()
        } else {
            format!("{}.{}", name, zone)
        };

        if fqdn.ends_with('.') {
            fqdn
        } else {
            format!("{}.", fqdn)
        }
    }
}

#[async_trait]
impl DnsProvider for Route53Provider {
    fn provider_type(&self) -> DnsProviderType {
        DnsProviderType::Route53
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
                info!("Route53 API connection test successful");
                Ok(true)
            }
            Err(e) => {
                warn!("Route53 API connection test failed: {}", e);
                Ok(false)
            }
        }
    }

    async fn list_zones(&self) -> Result<Vec<DnsZone>, DnsError> {
        let response = self
            .api_request("GET", "/2013-04-01/hostedzone", None)
            .await?;

        // Parse XML response
        let parsed: ListHostedZonesResponse = quick_xml::de::from_str(&response)
            .map_err(|e| DnsError::ApiError(format!("Failed to parse response: {}", e)))?;

        let zones = parsed
            .hosted_zones
            .map(|w| w.hosted_zone)
            .unwrap_or_default();

        Ok(zones
            .into_iter()
            .map(|zone| DnsZone {
                id: zone.id.trim_start_matches("/hostedzone/").to_string(),
                name: Self::normalize_domain(&zone.name),
                status: "active".to_string(),
                nameservers: vec![],
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
        let zone_id = self.get_zone_id(domain).await?;
        let zone = self
            .get_zone(domain)
            .await?
            .ok_or_else(|| DnsError::ZoneNotFound(domain.to_string()))?;

        let path = format!("/2013-04-01/hostedzone/{}/rrset", zone_id);
        let response = self.api_request("GET", &path, None).await?;

        let parsed: ListResourceRecordSetsResponse = quick_xml::de::from_str(&response)
            .map_err(|e| DnsError::ApiError(format!("Failed to parse response: {}", e)))?;

        let record_sets = parsed
            .resource_record_sets
            .map(|w| w.resource_record_set)
            .unwrap_or_default();

        Ok(record_sets
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
        let zone_id = self.get_zone_id(domain).await?;
        let zone = self
            .get_zone(domain)
            .await?
            .ok_or_else(|| DnsError::ZoneNotFound(domain.to_string()))?;

        let fqdn = Self::build_fqdn(&request.name, &zone.name);
        let record_type = request.content.record_type().to_string();
        let value = Self::format_record_value(&request.content);

        let change_request = ChangeResourceRecordSetsRequest {
            change_batch: ChangeBatch {
                comment: Some("Created by Temps".to_string()),
                changes: Changes {
                    change: vec![Change {
                        action: "CREATE".to_string(),
                        resource_record_set: ChangeResourceRecordSet {
                            name: fqdn.clone(),
                            record_type: record_type.clone(),
                            ttl: request.ttl.unwrap_or(300),
                            resource_records: ChangeResourceRecords {
                                resource_record: vec![ChangeResourceRecord { value }],
                            },
                        },
                    }],
                },
            },
        };

        let body = quick_xml::se::to_string(&change_request)
            .map_err(|e| DnsError::ApiError(format!("Failed to serialize request: {}", e)))?;

        // Add XML namespace
        let body = body.replace(
            "<ChangeResourceRecordSetsRequest>",
            "<ChangeResourceRecordSetsRequest xmlns=\"https://route53.amazonaws.com/doc/2013-04-01/\">",
        );

        let path = format!("/2013-04-01/hostedzone/{}/rrset", zone_id);
        self.api_request("POST", &path, Some(&body)).await?;

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
        let zone_id = self.get_zone_id(domain).await?;
        let zone = self
            .get_zone(domain)
            .await?
            .ok_or_else(|| DnsError::ZoneNotFound(domain.to_string()))?;

        let fqdn = Self::build_fqdn(&request.name, &zone.name);
        let record_type = request.content.record_type().to_string();
        let value = Self::format_record_value(&request.content);

        // Route 53 uses UPSERT for create-or-update
        let change_request = ChangeResourceRecordSetsRequest {
            change_batch: ChangeBatch {
                comment: Some("Updated by Temps".to_string()),
                changes: Changes {
                    change: vec![Change {
                        action: "UPSERT".to_string(),
                        resource_record_set: ChangeResourceRecordSet {
                            name: fqdn.clone(),
                            record_type: record_type.clone(),
                            ttl: request.ttl.unwrap_or(300),
                            resource_records: ChangeResourceRecords {
                                resource_record: vec![ChangeResourceRecord { value }],
                            },
                        },
                    }],
                },
            },
        };

        let body = quick_xml::se::to_string(&change_request)
            .map_err(|e| DnsError::ApiError(format!("Failed to serialize request: {}", e)))?;

        let body = body.replace(
            "<ChangeResourceRecordSetsRequest>",
            "<ChangeResourceRecordSetsRequest xmlns=\"https://route53.amazonaws.com/doc/2013-04-01/\">",
        );

        let path = format!("/2013-04-01/hostedzone/{}/rrset", zone_id);
        self.api_request("POST", &path, Some(&body)).await?;

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

        let zone_id = self.get_zone_id(domain).await?;
        let value = Self::format_record_value(&existing.content);

        let change_request = ChangeResourceRecordSetsRequest {
            change_batch: ChangeBatch {
                comment: Some("Deleted by Temps".to_string()),
                changes: Changes {
                    change: vec![Change {
                        action: "DELETE".to_string(),
                        resource_record_set: ChangeResourceRecordSet {
                            name: format!("{}.", fqdn),
                            record_type: record_type_str.to_string(),
                            ttl: existing.ttl,
                            resource_records: ChangeResourceRecords {
                                resource_record: vec![ChangeResourceRecord { value }],
                            },
                        },
                    }],
                },
            },
        };

        let body = quick_xml::se::to_string(&change_request)
            .map_err(|e| DnsError::ApiError(format!("Failed to serialize request: {}", e)))?;

        let body = body.replace(
            "<ChangeResourceRecordSetsRequest>",
            "<ChangeResourceRecordSetsRequest xmlns=\"https://route53.amazonaws.com/doc/2013-04-01/\">",
        );

        let path = format!("/2013-04-01/hostedzone/{}/rrset", zone_id);
        self.api_request("POST", &path, Some(&body)).await?;

        info!("Deleted DNS record {} from domain {}", record_id, domain);

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ==================== Helper function tests ====================

    #[test]
    fn test_normalize_domain() {
        assert_eq!(
            Route53Provider::normalize_domain("example.com."),
            "example.com"
        );
        assert_eq!(
            Route53Provider::normalize_domain("example.com"),
            "example.com"
        );
        assert_eq!(
            Route53Provider::normalize_domain("SUB.Example.COM."),
            "sub.example.com"
        );
    }

    #[test]
    fn test_build_fqdn() {
        assert_eq!(
            Route53Provider::build_fqdn("www", "example.com"),
            "www.example.com."
        );
        assert_eq!(
            Route53Provider::build_fqdn("@", "example.com"),
            "example.com."
        );
        assert_eq!(
            Route53Provider::build_fqdn("", "example.com"),
            "example.com."
        );
        assert_eq!(
            Route53Provider::build_fqdn("sub.www", "example.com"),
            "sub.www.example.com."
        );
    }

    #[test]
    fn test_format_record_value_a() {
        let content = DnsRecordContent::A {
            address: "192.0.2.1".to_string(),
        };
        assert_eq!(Route53Provider::format_record_value(&content), "192.0.2.1");
    }

    #[test]
    fn test_format_record_value_txt() {
        let content = DnsRecordContent::TXT {
            content: "v=spf1 -all".to_string(),
        };
        assert_eq!(
            Route53Provider::format_record_value(&content),
            "\"v=spf1 -all\""
        );
    }

    #[test]
    fn test_format_record_value_cname() {
        let content = DnsRecordContent::CNAME {
            target: "www.example.com".to_string(),
        };
        assert_eq!(
            Route53Provider::format_record_value(&content),
            "www.example.com."
        );
    }

    #[test]
    fn test_format_record_value_mx() {
        let content = DnsRecordContent::MX {
            priority: 10,
            target: "mail.example.com".to_string(),
        };
        assert_eq!(
            Route53Provider::format_record_value(&content),
            "10 mail.example.com."
        );
    }

    #[test]
    fn test_parse_record_type() {
        assert_eq!(
            Route53Provider::parse_record_type("A"),
            Some(DnsRecordType::A)
        );
        assert_eq!(
            Route53Provider::parse_record_type("aaaa"),
            Some(DnsRecordType::AAAA)
        );
        assert_eq!(
            Route53Provider::parse_record_type("TXT"),
            Some(DnsRecordType::TXT)
        );
        assert_eq!(Route53Provider::parse_record_type("UNKNOWN"), None);
    }

    #[test]
    fn test_parse_record_content_a() {
        let content = Route53Provider::parse_record_content(DnsRecordType::A, "192.0.2.1");
        assert!(content.is_some());
        if let Some(DnsRecordContent::A { address }) = content {
            assert_eq!(address, "192.0.2.1");
        } else {
            panic!("Expected A record");
        }
    }

    #[test]
    fn test_parse_record_content_txt() {
        let content = Route53Provider::parse_record_content(DnsRecordType::TXT, "\"v=spf1 -all\"");
        assert!(content.is_some());
        if let Some(DnsRecordContent::TXT { content }) = content {
            assert_eq!(content, "v=spf1 -all");
        } else {
            panic!("Expected TXT record");
        }
    }

    #[test]
    fn test_parse_record_content_mx() {
        let content =
            Route53Provider::parse_record_content(DnsRecordType::MX, "10 mail.example.com.");
        assert!(content.is_some());
        if let Some(DnsRecordContent::MX { priority, target }) = content {
            assert_eq!(priority, 10);
            assert_eq!(target, "mail.example.com");
        } else {
            panic!("Expected MX record");
        }
    }

    // ==================== Provider tests ====================

    #[test]
    fn test_provider_type() {
        let creds = Route53Credentials {
            access_key_id: "AKIATEST".to_string(),
            secret_access_key: "secret".to_string(),
            session_token: None,
            region: None,
        };
        let provider = Route53Provider::new(creds).unwrap();
        assert_eq!(provider.provider_type(), DnsProviderType::Route53);
    }

    #[test]
    fn test_capabilities() {
        let creds = Route53Credentials {
            access_key_id: "AKIATEST".to_string(),
            secret_access_key: "secret".to_string(),
            session_token: None,
            region: None,
        };
        let provider = Route53Provider::new(creds).unwrap();
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
    fn test_default_region() {
        let creds = Route53Credentials {
            access_key_id: "AKIATEST".to_string(),
            secret_access_key: "secret".to_string(),
            session_token: None,
            region: None,
        };
        let provider = Route53Provider::new(creds).unwrap();
        assert_eq!(provider.region, "us-east-1");
    }

    #[test]
    fn test_custom_region() {
        let creds = Route53Credentials {
            access_key_id: "AKIATEST".to_string(),
            secret_access_key: "secret".to_string(),
            session_token: None,
            region: Some("eu-west-1".to_string()),
        };
        let provider = Route53Provider::new(creds).unwrap();
        assert_eq!(provider.region, "eu-west-1");
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use wiremock::matchers::{method, path, path_regex};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn create_mock_provider(mock_server: &MockServer) -> Route53Provider {
        let creds = Route53Credentials {
            access_key_id: "AKIATESTKEY".to_string(),
            secret_access_key: "testsecretkey".to_string(),
            session_token: None,
            region: Some("us-east-1".to_string()),
        };

        // Create provider with custom endpoint
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap();

        // We'll need to modify the provider to use the mock server URL
        // For now, create the provider normally but we'll test the parsing logic
        Route53Provider {
            client,
            credentials: creds,
            region: "us-east-1".to_string(),
        }
    }

    #[tokio::test]
    async fn test_list_zones_parsing() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
        <ListHostedZonesResponse xmlns="https://route53.amazonaws.com/doc/2013-04-01/">
            <HostedZones>
                <HostedZone>
                    <Id>/hostedzone/Z1234567890ABC</Id>
                    <Name>example.com.</Name>
                    <CallerReference>test-ref-1</CallerReference>
                </HostedZone>
                <HostedZone>
                    <Id>/hostedzone/Z0987654321XYZ</Id>
                    <Name>test.org.</Name>
                    <CallerReference>test-ref-2</CallerReference>
                </HostedZone>
            </HostedZones>
        </ListHostedZonesResponse>"#;

        let parsed: ListHostedZonesResponse = quick_xml::de::from_str(xml).unwrap();
        let zones = parsed.hosted_zones.unwrap().hosted_zone;

        assert_eq!(zones.len(), 2);
        assert_eq!(zones[0].id, "/hostedzone/Z1234567890ABC");
        assert_eq!(zones[0].name, "example.com.");
        assert_eq!(zones[1].id, "/hostedzone/Z0987654321XYZ");
        assert_eq!(zones[1].name, "test.org.");
    }

    #[tokio::test]
    async fn test_list_records_parsing() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
        <ListResourceRecordSetsResponse xmlns="https://route53.amazonaws.com/doc/2013-04-01/">
            <ResourceRecordSets>
                <ResourceRecordSet>
                    <Name>www.example.com.</Name>
                    <Type>A</Type>
                    <TTL>300</TTL>
                    <ResourceRecords>
                        <ResourceRecord>
                            <Value>192.0.2.1</Value>
                        </ResourceRecord>
                    </ResourceRecords>
                </ResourceRecordSet>
                <ResourceRecordSet>
                    <Name>example.com.</Name>
                    <Type>TXT</Type>
                    <TTL>3600</TTL>
                    <ResourceRecords>
                        <ResourceRecord>
                            <Value>"v=spf1 -all"</Value>
                        </ResourceRecord>
                    </ResourceRecords>
                </ResourceRecordSet>
            </ResourceRecordSets>
        </ListResourceRecordSetsResponse>"#;

        let parsed: ListResourceRecordSetsResponse = quick_xml::de::from_str(xml).unwrap();
        let records = parsed.resource_record_sets.unwrap().resource_record_set;

        assert_eq!(records.len(), 2);

        // Check A record
        assert_eq!(records[0].name, "www.example.com.");
        assert_eq!(records[0].record_type, "A");
        assert_eq!(records[0].ttl, Some(300));

        // Check TXT record
        assert_eq!(records[1].name, "example.com.");
        assert_eq!(records[1].record_type, "TXT");
        assert_eq!(records[1].ttl, Some(3600));
    }

    #[tokio::test]
    async fn test_convert_record() {
        let record_set = ResourceRecordSet {
            name: "www.example.com.".to_string(),
            record_type: "A".to_string(),
            ttl: Some(300),
            resource_records: Some(ResourceRecordsWrapper {
                resource_record: vec![ResourceRecord {
                    value: "192.0.2.1".to_string(),
                }],
            }),
        };

        let records = Route53Provider::convert_record(&record_set, "example.com");

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

    #[tokio::test]
    async fn test_convert_record_apex() {
        let record_set = ResourceRecordSet {
            name: "example.com.".to_string(),
            record_type: "A".to_string(),
            ttl: Some(300),
            resource_records: Some(ResourceRecordsWrapper {
                resource_record: vec![ResourceRecord {
                    value: "192.0.2.1".to_string(),
                }],
            }),
        };

        let records = Route53Provider::convert_record(&record_set, "example.com");

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].name, "@");
        assert_eq!(records[0].fqdn, "example.com");
    }

    #[tokio::test]
    async fn test_convert_record_txt() {
        let record_set = ResourceRecordSet {
            name: "_acme-challenge.example.com.".to_string(),
            record_type: "TXT".to_string(),
            ttl: Some(60),
            resource_records: Some(ResourceRecordsWrapper {
                resource_record: vec![ResourceRecord {
                    value: "\"verification-token-here\"".to_string(),
                }],
            }),
        };

        let records = Route53Provider::convert_record(&record_set, "example.com");

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].name, "_acme-challenge");
        if let DnsRecordContent::TXT { content } = &records[0].content {
            assert_eq!(content, "verification-token-here");
        } else {
            panic!("Expected TXT record");
        }
    }

    #[tokio::test]
    async fn test_convert_record_mx() {
        let record_set = ResourceRecordSet {
            name: "example.com.".to_string(),
            record_type: "MX".to_string(),
            ttl: Some(3600),
            resource_records: Some(ResourceRecordsWrapper {
                resource_record: vec![ResourceRecord {
                    value: "10 mail.example.com.".to_string(),
                }],
            }),
        };

        let records = Route53Provider::convert_record(&record_set, "example.com");

        assert_eq!(records.len(), 1);
        if let DnsRecordContent::MX { priority, target } = &records[0].content {
            assert_eq!(*priority, 10);
            assert_eq!(target, "mail.example.com");
        } else {
            panic!("Expected MX record");
        }
    }
}
