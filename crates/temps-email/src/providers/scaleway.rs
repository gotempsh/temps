//! Scaleway Transactional Email provider implementation

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::{debug, error};

use super::traits::{
    DnsRecord, DnsRecordStatus, DomainIdentity, DomainIdentityDetails, EmailProvider,
    EmailProviderType, SendEmailRequest, SendEmailResponse, VerificationStatus,
};
use crate::dns::DnsVerifier;
use crate::errors::EmailError;

/// Scaleway TEM credentials configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScalewayCredentials {
    pub api_key: String,
    pub project_id: String,
}

/// Scaleway TEM provider implementation
pub struct ScalewayProvider {
    client: Client,
    api_key: String,
    project_id: String,
    region: String,
}

impl ScalewayProvider {
    const BASE_URL: &'static str = "https://api.scaleway.com/transactional-email/v1alpha1";

    /// Create a new Scaleway provider with the given credentials
    pub fn new(credentials: &ScalewayCredentials, region: &str) -> Result<Self, EmailError> {
        let client = Client::new();

        Ok(Self {
            client,
            api_key: credentials.api_key.clone(),
            project_id: credentials.project_id.clone(),
            region: region.to_string(),
        })
    }

    /// Get the Scaleway region
    pub fn region(&self) -> &str {
        &self.region
    }

    fn api_url(&self, path: &str) -> String {
        format!("{}/regions/{}{}", Self::BASE_URL, self.region, path)
    }
}

// Scaleway API response types
#[derive(Debug, Deserialize)]
struct ScalewayDomainResponse {
    id: String,
    #[allow(dead_code)]
    name: String,
    status: String,
    spf_config: Option<String>,
    dkim_config: Option<String>,
    last_error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ScalewayEmailResponse {
    emails: Vec<ScalewayEmailInfo>,
}

#[derive(Debug, Deserialize)]
struct ScalewayEmailInfo {
    id: String,
    message_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct ScalewayCreateDomainRequest {
    project_id: String,
    domain_name: String,
}

#[derive(Debug, Serialize)]
struct ScalewaySendEmailRequest {
    project_id: String,
    from: ScalewayEmailAddress,
    to: Vec<ScalewayEmailAddress>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cc: Option<Vec<ScalewayEmailAddress>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bcc: Option<Vec<ScalewayEmailAddress>>,
    subject: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    html: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
}

#[derive(Debug, Serialize)]
struct ScalewayEmailAddress {
    email: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
}

#[async_trait]
impl EmailProvider for ScalewayProvider {
    async fn create_identity(&self, domain: &str) -> Result<DomainIdentity, EmailError> {
        debug!("Creating Scaleway identity for domain: {}", domain);

        let request = ScalewayCreateDomainRequest {
            project_id: self.project_id.clone(),
            domain_name: domain.to_string(),
        };

        let response = self
            .client
            .post(self.api_url("/domains"))
            .header("X-Auth-Token", &self.api_key)
            .json(&request)
            .send()
            .await
            .map_err(|e| EmailError::Scaleway(format!("Failed to create domain: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(EmailError::Scaleway(format!(
                "Failed to create domain ({}): {}",
                status, body
            )));
        }

        let domain_response: ScalewayDomainResponse = response
            .json()
            .await
            .map_err(|e| EmailError::Scaleway(format!("Failed to parse domain response: {}", e)))?;

        // Parse SPF config
        let spf_record = domain_response.spf_config.map(|spf| DnsRecord {
            record_type: "TXT".to_string(),
            name: domain.to_string(),
            value: spf,
            priority: None,
            status: DnsRecordStatus::Pending,
        });

        // Parse DKIM config
        let dkim_records = if let Some(dkim) = domain_response.dkim_config {
            vec![DnsRecord {
                record_type: "TXT".to_string(),
                name: format!("scw._domainkey.{}", domain),
                value: dkim,
                priority: None,
                status: DnsRecordStatus::Pending,
            }]
        } else {
            Vec::new()
        };

        Ok(DomainIdentity {
            provider_identity_id: domain_response.id,
            spf_record,
            dkim_records,
            dkim_selector: Some("scw".to_string()),
            mx_record: None,
            // Scaleway doesn't support custom MAIL FROM - SPF/DKIM go directly on root domain
            mail_from_subdomain: None,
        })
    }

    async fn verify_identity(&self, domain: &str) -> Result<VerificationStatus, EmailError> {
        debug!("Verifying Scaleway identity for domain: {}", domain);

        // First, trigger the check
        let check_response = self
            .client
            .post(self.api_url(&format!("/domains/{}/check", urlencoding::encode(domain))))
            .header("X-Auth-Token", &self.api_key)
            .send()
            .await
            .map_err(|e| EmailError::Scaleway(format!("Failed to check domain: {}", e)))?;

        if !check_response.status().is_success() {
            let status = check_response.status();
            let body = check_response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(EmailError::Scaleway(format!(
                "Failed to check domain ({}): {}",
                status, body
            )));
        }

        // Then get the domain status
        let response = self
            .client
            .get(self.api_url(&format!("/domains/{}", urlencoding::encode(domain))))
            .header("X-Auth-Token", &self.api_key)
            .send()
            .await
            .map_err(|e| EmailError::Scaleway(format!("Failed to get domain: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(EmailError::Scaleway(format!(
                "Failed to get domain ({}): {}",
                status, body
            )));
        }

        let domain_response: ScalewayDomainResponse = response
            .json()
            .await
            .map_err(|e| EmailError::Scaleway(format!("Failed to parse domain response: {}", e)))?;

        match domain_response.status.as_str() {
            "checked" | "verified" => Ok(VerificationStatus::Verified),
            "pending" | "unchecked" => Ok(VerificationStatus::Pending),
            "invalid" => Ok(VerificationStatus::Failed(
                domain_response
                    .last_error
                    .unwrap_or_else(|| "DNS verification failed".to_string()),
            )),
            _ => Ok(VerificationStatus::NotStarted),
        }
    }

    async fn get_identity_details(
        &self,
        domain: &str,
    ) -> Result<DomainIdentityDetails, EmailError> {
        debug!("Getting Scaleway identity details for domain: {}", domain);

        // Get the domain status from Scaleway
        let response = self
            .client
            .get(self.api_url(&format!("/domains/{}", urlencoding::encode(domain))))
            .header("X-Auth-Token", &self.api_key)
            .send()
            .await
            .map_err(|e| EmailError::Scaleway(format!("Failed to get domain: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(EmailError::Scaleway(format!(
                "Failed to get domain ({}): {}",
                status, body
            )));
        }

        let domain_response: ScalewayDomainResponse = response
            .json()
            .await
            .map_err(|e| EmailError::Scaleway(format!("Failed to parse domain response: {}", e)))?;

        // Determine overall verification status
        let overall_status = match domain_response.status.as_str() {
            "checked" | "verified" => VerificationStatus::Verified,
            "pending" | "unchecked" => VerificationStatus::Pending,
            "invalid" => VerificationStatus::Failed(
                domain_response
                    .last_error
                    .clone()
                    .unwrap_or_else(|| "DNS verification failed".to_string()),
            ),
            _ => VerificationStatus::NotStarted,
        };

        // Verify records via DNS lookup for accurate per-record status
        let dns_verifier = DnsVerifier::new();

        // Build SPF record with DNS-verified status
        let spf_record = if let Some(spf) = domain_response.spf_config {
            // Scaleway SPF includes "include:_spf.scw-tem.cloud"
            let spf_status = dns_verifier
                .verify_spf_record(domain, "_spf.scw-tem.cloud")
                .await;
            Some(DnsRecord {
                record_type: "TXT".to_string(),
                name: domain.to_string(),
                value: spf,
                priority: None,
                status: spf_status,
            })
        } else {
            None
        };

        // Build DKIM record with DNS-verified status
        let dkim_records = if let Some(dkim) = domain_response.dkim_config {
            let dkim_name = format!("scw._domainkey.{}", domain);
            let dkim_status = dns_verifier.verify_txt_record(&dkim_name, &dkim).await;
            vec![DnsRecord {
                record_type: "TXT".to_string(),
                name: dkim_name,
                value: dkim,
                priority: None,
                status: dkim_status,
            }]
        } else {
            Vec::new()
        };

        Ok(DomainIdentityDetails {
            overall_status,
            spf_record,
            dkim_records,
            mx_record: None, // Scaleway doesn't use MX records
            // Scaleway doesn't support custom MAIL FROM - all records on root domain
            mail_from_subdomain: None,
        })
    }

    async fn delete_identity(&self, domain: &str) -> Result<(), EmailError> {
        debug!("Deleting Scaleway identity for domain: {}", domain);

        let response = self
            .client
            .delete(self.api_url(&format!("/domains/{}", urlencoding::encode(domain))))
            .header("X-Auth-Token", &self.api_key)
            .send()
            .await
            .map_err(|e| EmailError::Scaleway(format!("Failed to delete domain: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(EmailError::Scaleway(format!(
                "Failed to delete domain ({}): {}",
                status, body
            )));
        }

        Ok(())
    }

    async fn send(&self, email: &SendEmailRequest) -> Result<SendEmailResponse, EmailError> {
        debug!("Sending email via Scaleway from: {}", email.from);

        let request = ScalewaySendEmailRequest {
            project_id: self.project_id.clone(),
            from: ScalewayEmailAddress {
                email: email.from.clone(),
                name: email.from_name.clone(),
            },
            to: email
                .to
                .iter()
                .map(|e| ScalewayEmailAddress {
                    email: e.clone(),
                    name: None,
                })
                .collect(),
            cc: email.cc.as_ref().map(|addrs| {
                addrs
                    .iter()
                    .map(|e| ScalewayEmailAddress {
                        email: e.clone(),
                        name: None,
                    })
                    .collect()
            }),
            bcc: email.bcc.as_ref().map(|addrs| {
                addrs
                    .iter()
                    .map(|e| ScalewayEmailAddress {
                        email: e.clone(),
                        name: None,
                    })
                    .collect()
            }),
            subject: email.subject.clone(),
            html: email.html.clone(),
            text: email.text.clone(),
        };

        let response = self
            .client
            .post(self.api_url("/emails"))
            .header("X-Auth-Token", &self.api_key)
            .json(&request)
            .send()
            .await
            .map_err(|e| EmailError::Scaleway(format!("Failed to send email: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            error!("Failed to send email via Scaleway ({}): {}", status, body);
            return Err(EmailError::Scaleway(format!(
                "Failed to send email ({}): {}",
                status, body
            )));
        }

        let email_response: ScalewayEmailResponse = response
            .json()
            .await
            .map_err(|e| EmailError::Scaleway(format!("Failed to parse email response: {}", e)))?;

        let message_id = email_response
            .emails
            .first()
            .and_then(|e| e.message_id.clone())
            .or_else(|| email_response.emails.first().map(|e| e.id.clone()))
            .ok_or_else(|| EmailError::Scaleway("No message ID returned".to_string()))?;

        debug!("Email sent successfully, message_id: {}", message_id);

        Ok(SendEmailResponse { message_id })
    }

    fn provider_type(&self) -> EmailProviderType {
        EmailProviderType::Scaleway
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scaleway_credentials_serialization() {
        let creds = ScalewayCredentials {
            api_key: "scw-secret-key-123".to_string(),
            project_id: "12345678-1234-1234-1234-123456789012".to_string(),
        };

        let json = serde_json::to_string(&creds).unwrap();
        assert!(json.contains("api_key"));
        assert!(json.contains("project_id"));

        let deserialized: ScalewayCredentials = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.api_key, creds.api_key);
        assert_eq!(deserialized.project_id, creds.project_id);
    }
}
