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
    DnsRecord, DomainIdentity, EmailProvider, EmailProviderType, SendEmailRequest,
    SendEmailResponse, VerificationStatus,
};
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
    async fn create_identity(&self, domain: &str) -> Result<DomainIdentity, EmailError> {
        debug!("Creating SES identity for domain: {}", domain);

        // Create email identity in SES
        let result = self
            .client
            .create_email_identity()
            .email_identity(domain)
            .send()
            .await
            .map_err(|e| EmailError::AwsSes(format!("Failed to create identity: {}", e)))?;

        // Extract DKIM attributes
        let dkim_attributes = result.dkim_attributes();

        // SES returns DKIM tokens that need to be set up as CNAME records
        let dkim_records: Vec<DnsRecord> =
            if let Some(tokens) = dkim_attributes.as_ref().and_then(|a| a.tokens.as_ref()) {
                tokens
                    .iter()
                    .map(|token| DnsRecord {
                        record_type: "CNAME".to_string(),
                        name: format!("{}._domainkey.{}", token, domain),
                        value: format!("{}.dkim.amazonses.com", token),
                        priority: None,
                    })
                    .collect()
            } else {
                Vec::new()
            };

        // Build SPF record for SES
        let spf_record = Some(DnsRecord {
            record_type: "TXT".to_string(),
            name: domain.to_string(),
            value: "v=spf1 include:amazonses.com ~all".to_string(),
            priority: None,
        });

        // MX record for receiving bounces/complaints
        let mx_record = Some(DnsRecord {
            record_type: "MX".to_string(),
            name: domain.to_string(),
            value: format!("feedback-smtp.{}.amazonses.com", self.region),
            priority: Some(10),
        });

        Ok(DomainIdentity {
            provider_identity_id: domain.to_string(),
            spf_record,
            dkim_records,
            dkim_selector: None, // SES uses its own selectors
            mx_record,
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
