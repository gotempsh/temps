//! AWS SES email provider implementation

use async_trait::async_trait;
use aws_config::BehaviorVersion;
use aws_sdk_sesv2::{
    config::{Credentials, Region},
    types::{Body, Content, Destination, EmailContent, Message},
    Client,
};
use serde::{Deserialize, Serialize};
use tracing::{debug, error};

use super::traits::{
    DnsRecord, DnsRecordStatus, DomainIdentity, DomainIdentityDetails, EmailProvider,
    EmailProviderType, SendEmailRequest, SendEmailResponse, VerificationStatus,
    DEFAULT_MAIL_FROM_SUBDOMAIN,
};
use crate::dns::DnsVerifier;
use crate::errors::EmailError;

/// Extract detailed error information from AWS SES SDK errors
fn extract_ses_error_details<E: std::fmt::Display + std::fmt::Debug>(
    e: &aws_sdk_sesv2::error::SdkError<E>,
) -> String {
    use aws_sdk_sesv2::error::SdkError;

    match e {
        SdkError::ServiceError(service_err) => {
            // Get the underlying service error message
            let err = service_err.err();
            format!("{}", err)
        }
        SdkError::TimeoutError(_) => {
            "Request timed out. Please check your network connection and try again.".to_string()
        }
        SdkError::DispatchFailure(dispatch_err) => {
            if dispatch_err.is_io() {
                format!("Network error: Unable to connect to AWS SES. Please verify your network connection and credentials.")
            } else if dispatch_err.is_timeout() {
                "Connection timed out. Please try again.".to_string()
            } else if dispatch_err.is_user() {
                format!("Configuration error: {:?}", dispatch_err)
            } else {
                format!("Connection failed: {:?}", dispatch_err)
            }
        }
        SdkError::ConstructionFailure(_) => {
            "Invalid request configuration. Please check your email parameters.".to_string()
        }
        SdkError::ResponseError(resp_err) => {
            format!("Unexpected response from AWS: {:?}", resp_err)
        }
        _ => {
            // Fallback to the debug representation for unknown errors
            format!("{:?}", e)
        }
    }
}

/// AWS SES credentials configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SesCredentials {
    pub access_key_id: String,
    pub secret_access_key: String,
    /// Optional custom endpoint URL (for LocalStack or other AWS-compatible services)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint_url: Option<String>,
}

/// AWS SES provider implementation
pub struct SesProvider {
    client: Client,
    region: String,
}

impl SesProvider {
    /// Create a new SES provider with the given credentials
    pub async fn new(credentials: &SesCredentials, region: &str) -> Result<Self, EmailError> {
        let creds = Credentials::new(
            &credentials.access_key_id,
            &credentials.secret_access_key,
            None,
            None,
            "temps-email",
        );

        let mut config_builder = aws_config::defaults(BehaviorVersion::latest())
            .region(Region::new(region.to_string()))
            .credentials_provider(creds);

        // If a custom endpoint URL is provided (e.g., for LocalStack), use it
        if let Some(ref endpoint_url) = credentials.endpoint_url {
            config_builder = config_builder.endpoint_url(endpoint_url);
        }

        let config = config_builder.load().await;

        let client = Client::new(&config);

        Ok(Self {
            client,
            region: region.to_string(),
        })
    }

    /// Get the AWS region
    pub fn region(&self) -> &str {
        &self.region
    }
}

#[async_trait]
impl EmailProvider for SesProvider {
    /// Create identity with split architecture (Resend-like):
    /// - Root domain: DKIM records (allows sending FROM @domain.com)
    /// - send.domain.com: SPF + MX records (handles bounces via Custom MAIL FROM)
    async fn create_identity(&self, domain: &str) -> Result<DomainIdentity, EmailError> {
        debug!(
            "Creating SES identity for domain: {} with split architecture",
            domain
        );

        let mail_from_subdomain = DEFAULT_MAIL_FROM_SUBDOMAIN;
        let mail_from_domain = format!("{}.{}", mail_from_subdomain, domain);

        // Step 1: Create email identity for the ROOT domain in SES
        let result = self
            .client
            .create_email_identity()
            .email_identity(domain)
            .send()
            .await
            .map_err(|e| EmailError::AwsSes(format!("Failed to create identity: {}", e)))?;

        // Step 2: Configure Custom MAIL FROM domain (send.domain.com)
        // This enables the split architecture where bounces go to the subdomain
        debug!(
            "Configuring Custom MAIL FROM: {} for domain: {}",
            mail_from_domain, domain
        );

        if let Err(e) = self
            .client
            .put_email_identity_mail_from_attributes()
            .email_identity(domain)
            .mail_from_domain(&mail_from_domain)
            .send()
            .await
        {
            // Log but don't fail - MAIL FROM can be configured later
            error!(
                "Failed to configure Custom MAIL FROM (will retry on verify): {}",
                e
            );
        }

        // Extract DKIM attributes - these go on the ROOT domain
        let dkim_attributes = result.dkim_attributes();

        // DKIM records: CNAME records on ROOT domain pointing to amazonses.com
        let dkim_records: Vec<DnsRecord> =
            if let Some(tokens) = dkim_attributes.as_ref().and_then(|a| a.tokens.as_ref()) {
                tokens
                    .iter()
                    .map(|token| DnsRecord {
                        record_type: "CNAME".to_string(),
                        name: format!("{}._domainkey.{}", token, domain),
                        value: format!("{}.dkim.amazonses.com", token),
                        priority: None,
                        status: DnsRecordStatus::Pending,
                    })
                    .collect()
            } else {
                Vec::new()
            };

        // SPF record: Goes on the MAIL FROM subdomain (send.domain.com)
        let spf_record = Some(DnsRecord {
            record_type: "TXT".to_string(),
            name: mail_from_domain.clone(),
            value: "v=spf1 include:amazonses.com ~all".to_string(),
            priority: None,
            status: DnsRecordStatus::Pending,
        });

        // MX record: Goes on the MAIL FROM subdomain (send.domain.com) for bounce handling
        let mx_record = Some(DnsRecord {
            record_type: "MX".to_string(),
            name: mail_from_domain.clone(),
            value: format!("feedback-smtp.{}.amazonses.com", self.region),
            priority: Some(10),
            status: DnsRecordStatus::Pending,
        });

        Ok(DomainIdentity {
            provider_identity_id: domain.to_string(),
            spf_record,
            dkim_records,
            dkim_selector: None, // SES uses its own selectors
            mx_record,
            mail_from_subdomain: Some(mail_from_subdomain.to_string()),
        })
    }

    async fn verify_identity(&self, domain: &str) -> Result<VerificationStatus, EmailError> {
        debug!("Verifying SES identity for domain: {}", domain);

        let result = self
            .client
            .get_email_identity()
            .email_identity(domain)
            .send()
            .await
            .map_err(|e| EmailError::AwsSes(format!("Failed to get identity: {}", e)))?;

        // Check if identity is verified
        let verified = result.verified_for_sending_status();

        if verified {
            Ok(VerificationStatus::Verified)
        } else {
            // Check DKIM status
            if let Some(dkim_attrs) = result.dkim_attributes() {
                match dkim_attrs.status() {
                    Some(status) => {
                        let status_str = status.as_str();
                        match status_str {
                            "SUCCESS" => Ok(VerificationStatus::Verified),
                            "PENDING" => Ok(VerificationStatus::Pending),
                            "FAILED" => Ok(VerificationStatus::Failed(
                                "DKIM verification failed".to_string(),
                            )),
                            "TEMPORARY_FAILURE" => Ok(VerificationStatus::TemporaryFailure),
                            "NOT_STARTED" => Ok(VerificationStatus::NotStarted),
                            _ => Ok(VerificationStatus::Pending),
                        }
                    }
                    None => Ok(VerificationStatus::NotStarted),
                }
            } else {
                Ok(VerificationStatus::NotStarted)
            }
        }
    }

    async fn get_identity_details(
        &self,
        domain: &str,
    ) -> Result<DomainIdentityDetails, EmailError> {
        debug!("Getting SES identity details for domain: {}", domain);

        let result = self
            .client
            .get_email_identity()
            .email_identity(domain)
            .send()
            .await
            .map_err(|e| EmailError::AwsSes(format!("Failed to get identity: {}", e)))?;

        // Get the configured MAIL FROM subdomain (default to "send" if not set)
        let mail_from_subdomain = result
            .mail_from_attributes()
            .map(|attrs| {
                let full_domain = &attrs.mail_from_domain;
                if full_domain.is_empty() {
                    DEFAULT_MAIL_FROM_SUBDOMAIN.to_string()
                } else {
                    // Extract subdomain from "send.domain.com" -> "send"
                    full_domain
                        .strip_suffix(&format!(".{}", domain))
                        .unwrap_or(DEFAULT_MAIL_FROM_SUBDOMAIN)
                        .to_string()
                }
            })
            .unwrap_or_else(|| DEFAULT_MAIL_FROM_SUBDOMAIN.to_string());

        let mail_from_domain = format!("{}.{}", mail_from_subdomain, domain);

        // Determine overall verification status
        let verified = result.verified_for_sending_status();
        let overall_status = if verified {
            VerificationStatus::Verified
        } else if let Some(dkim_attrs) = result.dkim_attributes() {
            match dkim_attrs.status() {
                Some(status) => match status.as_str() {
                    "SUCCESS" => VerificationStatus::Verified,
                    "PENDING" => VerificationStatus::Pending,
                    "FAILED" => VerificationStatus::Failed("DKIM verification failed".to_string()),
                    "TEMPORARY_FAILURE" => VerificationStatus::TemporaryFailure,
                    "NOT_STARTED" => VerificationStatus::NotStarted,
                    _ => VerificationStatus::Pending,
                },
                None => VerificationStatus::NotStarted,
            }
        } else {
            VerificationStatus::NotStarted
        };

        // Create DNS verifier for record verification
        let dns_verifier = DnsVerifier::new();

        // Build DKIM records with DNS-verified status
        // DKIM CNAME records go on the ROOT domain
        let mut dkim_records: Vec<DnsRecord> = Vec::new();
        if let Some(tokens) = result
            .dkim_attributes()
            .and_then(|attrs| attrs.tokens.as_ref())
        {
            for token in tokens {
                let dkim_name = format!("{}._domainkey.{}", token, domain);
                let dkim_value = format!("{}.dkim.amazonses.com", token);

                // Verify DKIM CNAME record via DNS lookup
                let dkim_status = dns_verifier
                    .verify_cname_record(&dkim_name, &dkim_value)
                    .await;

                dkim_records.push(DnsRecord {
                    record_type: "CNAME".to_string(),
                    name: dkim_name,
                    value: dkim_value,
                    priority: None,
                    status: dkim_status,
                });
            }
        }

        // SPF record - goes on MAIL FROM subdomain (send.domain.com)
        let spf_status = dns_verifier
            .verify_spf_record(&mail_from_domain, "amazonses.com")
            .await;
        let spf_record = Some(DnsRecord {
            record_type: "TXT".to_string(),
            name: mail_from_domain.clone(),
            value: "v=spf1 include:amazonses.com ~all".to_string(),
            priority: None,
            status: spf_status,
        });

        // MX record for bounce handling - goes on MAIL FROM subdomain (send.domain.com)
        let mx_value = format!("feedback-smtp.{}.amazonses.com", self.region);
        let mx_status = dns_verifier
            .verify_mx_record(&mail_from_domain, &mx_value, Some(10))
            .await;
        let mx_record = Some(DnsRecord {
            record_type: "MX".to_string(),
            name: mail_from_domain,
            value: mx_value,
            priority: Some(10),
            status: mx_status,
        });

        Ok(DomainIdentityDetails {
            overall_status,
            spf_record,
            dkim_records,
            mx_record,
            mail_from_subdomain: Some(mail_from_subdomain),
        })
    }

    async fn delete_identity(&self, domain: &str) -> Result<(), EmailError> {
        debug!("Deleting SES identity for domain: {}", domain);

        self.client
            .delete_email_identity()
            .email_identity(domain)
            .send()
            .await
            .map_err(|e| EmailError::AwsSes(format!("Failed to delete identity: {}", e)))?;

        Ok(())
    }

    async fn send(&self, email: &SendEmailRequest) -> Result<SendEmailResponse, EmailError> {
        debug!("Sending email via SES from: {}", email.from);

        // Build the from address with optional display name
        let from_address = if let Some(ref name) = email.from_name {
            format!("{} <{}>", name, email.from)
        } else {
            email.from.clone()
        };

        // Build destination
        let mut destination = Destination::builder();
        destination = destination.set_to_addresses(Some(email.to.clone()));

        if let Some(ref cc) = email.cc {
            destination = destination.set_cc_addresses(Some(cc.clone()));
        }
        if let Some(ref bcc) = email.bcc {
            destination = destination.set_bcc_addresses(Some(bcc.clone()));
        }

        // Build email body
        let mut body_builder = Body::builder();

        if let Some(ref html) = email.html {
            body_builder =
                body_builder.html(Content::builder().data(html).build().map_err(|e| {
                    EmailError::AwsSes(format!("Failed to build HTML content: {}", e))
                })?);
        }

        if let Some(ref text) = email.text {
            body_builder =
                body_builder.text(Content::builder().data(text).build().map_err(|e| {
                    EmailError::AwsSes(format!("Failed to build text content: {}", e))
                })?);
        }

        // Build the message
        let message = Message::builder()
            .subject(
                Content::builder()
                    .data(&email.subject)
                    .build()
                    .map_err(|e| EmailError::AwsSes(format!("Failed to build subject: {}", e)))?,
            )
            .body(body_builder.build())
            .build();

        // Build email content
        let content = EmailContent::builder().simple(message).build();

        // Send the email
        let mut send_request = self
            .client
            .send_email()
            .from_email_address(&from_address)
            .destination(destination.build())
            .content(content);

        // Add reply-to if specified
        if let Some(ref reply_to) = email.reply_to {
            send_request = send_request.reply_to_addresses(reply_to);
        }

        let result = send_request.send().await.map_err(|e| {
            // Extract detailed error information from AWS SDK error
            let error_message = extract_ses_error_details(&e);
            error!("Failed to send email via SES: {}", error_message);
            EmailError::AwsSes(error_message)
        })?;

        let message_id = result
            .message_id()
            .ok_or_else(|| EmailError::AwsSes("No message ID returned".to_string()))?
            .to_string();

        debug!("Email sent successfully, message_id: {}", message_id);

        Ok(SendEmailResponse { message_id })
    }

    fn provider_type(&self) -> EmailProviderType {
        EmailProviderType::Ses
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ses_credentials_serialization() {
        let creds = SesCredentials {
            access_key_id: "AKIAIOSFODNN7EXAMPLE".to_string(),
            secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_string(),
            endpoint_url: None,
        };

        let json = serde_json::to_string(&creds).unwrap();
        assert!(json.contains("access_key_id"));
        assert!(json.contains("secret_access_key"));
        // endpoint_url should not be serialized when None
        assert!(!json.contains("endpoint_url"));

        let deserialized: SesCredentials = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.access_key_id, creds.access_key_id);
    }

    #[test]
    fn test_ses_credentials_with_endpoint_serialization() {
        let creds = SesCredentials {
            access_key_id: "AKIAIOSFODNN7EXAMPLE".to_string(),
            secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_string(),
            endpoint_url: Some("http://localhost:4566".to_string()),
        };

        let json = serde_json::to_string(&creds).unwrap();
        assert!(json.contains("endpoint_url"));
        assert!(json.contains("http://localhost:4566"));

        let deserialized: SesCredentials = serde_json::from_str(&json).unwrap();
        assert_eq!(
            deserialized.endpoint_url,
            Some("http://localhost:4566".to_string())
        );
    }
}
