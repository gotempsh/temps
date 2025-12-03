//! Email provider trait definitions

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::errors::EmailError;

/// Supported email provider types
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum EmailProviderType {
    /// Amazon Simple Email Service
    Ses,
    /// Scaleway Transactional Email
    Scaleway,
}

impl std::fmt::Display for EmailProviderType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EmailProviderType::Ses => write!(f, "ses"),
            EmailProviderType::Scaleway => write!(f, "scaleway"),
        }
    }
}

impl EmailProviderType {
    pub fn from_str(s: &str) -> Result<Self, EmailError> {
        match s.to_lowercase().as_str() {
            "ses" | "aws_ses" | "aws-ses" => Ok(EmailProviderType::Ses),
            "scaleway" | "scw" => Ok(EmailProviderType::Scaleway),
            _ => Err(EmailError::InvalidProviderType(s.to_string())),
        }
    }
}

/// DNS record for domain verification
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DnsRecord {
    /// Record type: TXT, CNAME, MX
    #[schema(example = "TXT")]
    pub record_type: String,
    /// DNS record name (host)
    #[schema(example = "temps._domainkey.example.com")]
    pub name: String,
    /// DNS record value
    #[schema(example = "v=DKIM1; k=rsa; p=MIGfMA0GCSqGSIb3...")]
    pub value: String,
    /// Priority (for MX records)
    #[schema(example = "10")]
    pub priority: Option<u16>,
}

/// Domain identity with required DNS records
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainIdentity {
    /// Provider-specific identity ID
    pub provider_identity_id: String,
    /// SPF record
    pub spf_record: Option<DnsRecord>,
    /// DKIM records (SES has multiple CNAME records)
    pub dkim_records: Vec<DnsRecord>,
    /// DKIM selector
    pub dkim_selector: Option<String>,
    /// MX record for bounce handling
    pub mx_record: Option<DnsRecord>,
}

/// Domain verification status
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum VerificationStatus {
    /// Verification not yet started
    NotStarted,
    /// Verification in progress
    Pending,
    /// Domain successfully verified
    Verified,
    /// Verification failed
    Failed(String),
    /// Previously verified but DNS records no longer valid
    TemporaryFailure,
}

impl std::fmt::Display for VerificationStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VerificationStatus::NotStarted => write!(f, "not_started"),
            VerificationStatus::Pending => write!(f, "pending"),
            VerificationStatus::Verified => write!(f, "verified"),
            VerificationStatus::Failed(_) => write!(f, "failed"),
            VerificationStatus::TemporaryFailure => write!(f, "temporary_failure"),
        }
    }
}

/// Request to send an email
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendEmailRequest {
    /// Sender email address
    pub from: String,
    /// Sender display name (optional)
    pub from_name: Option<String>,
    /// Recipient email addresses
    pub to: Vec<String>,
    /// CC recipients
    pub cc: Option<Vec<String>>,
    /// BCC recipients
    pub bcc: Option<Vec<String>>,
    /// Reply-to address
    pub reply_to: Option<String>,
    /// Email subject
    pub subject: String,
    /// HTML body content
    pub html: Option<String>,
    /// Plain text body content
    pub text: Option<String>,
    /// Custom headers
    pub headers: Option<std::collections::HashMap<String, String>>,
}

/// Response from sending an email
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendEmailResponse {
    /// Provider's message ID
    pub message_id: String,
}

/// Email provider trait for abstracting different email services
#[async_trait]
pub trait EmailProvider: Send + Sync {
    /// Register a domain and get the required DNS records
    async fn create_identity(&self, domain: &str) -> Result<DomainIdentity, EmailError>;

    /// Verify domain DNS configuration
    async fn verify_identity(&self, domain: &str) -> Result<VerificationStatus, EmailError>;

    /// Delete domain identity
    async fn delete_identity(&self, domain: &str) -> Result<(), EmailError>;

    /// Send an email
    async fn send(&self, email: &SendEmailRequest) -> Result<SendEmailResponse, EmailError>;

    /// Get the provider type
    fn provider_type(&self) -> EmailProviderType;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_provider_type_from_str() {
        assert_eq!(
            EmailProviderType::from_str("ses").unwrap(),
            EmailProviderType::Ses
        );
        assert_eq!(
            EmailProviderType::from_str("SES").unwrap(),
            EmailProviderType::Ses
        );
        assert_eq!(
            EmailProviderType::from_str("aws_ses").unwrap(),
            EmailProviderType::Ses
        );
        assert_eq!(
            EmailProviderType::from_str("scaleway").unwrap(),
            EmailProviderType::Scaleway
        );
        assert_eq!(
            EmailProviderType::from_str("scw").unwrap(),
            EmailProviderType::Scaleway
        );
        assert!(EmailProviderType::from_str("invalid").is_err());
    }

    #[test]
    fn test_provider_type_display() {
        assert_eq!(EmailProviderType::Ses.to_string(), "ses");
        assert_eq!(EmailProviderType::Scaleway.to_string(), "scaleway");
    }

    #[test]
    fn test_verification_status_display() {
        assert_eq!(VerificationStatus::NotStarted.to_string(), "not_started");
        assert_eq!(VerificationStatus::Pending.to_string(), "pending");
        assert_eq!(VerificationStatus::Verified.to_string(), "verified");
        assert_eq!(
            VerificationStatus::Failed("test".to_string()).to_string(),
            "failed"
        );
        assert_eq!(
            VerificationStatus::TemporaryFailure.to_string(),
            "temporary_failure"
        );
    }
}
