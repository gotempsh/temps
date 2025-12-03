//! Handler types for the email service

use crate::providers::EmailProviderType;
use crate::services::{DomainService, EmailService, ProviderService};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use temps_core::AuditLogger;
use utoipa::{IntoParams, ToSchema};

/// Application state for email handlers
pub struct AppState {
    pub provider_service: Arc<ProviderService>,
    pub domain_service: Arc<DomainService>,
    pub email_service: Arc<EmailService>,
    pub audit_service: Arc<dyn AuditLogger>,
}

// ========================================
// Provider Types
// ========================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum EmailProviderTypeRoute {
    Ses,
    Scaleway,
}

impl std::fmt::Display for EmailProviderTypeRoute {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EmailProviderTypeRoute::Ses => write!(f, "ses"),
            EmailProviderTypeRoute::Scaleway => write!(f, "scaleway"),
        }
    }
}

impl From<EmailProviderType> for EmailProviderTypeRoute {
    fn from(t: EmailProviderType) -> Self {
        match t {
            EmailProviderType::Ses => EmailProviderTypeRoute::Ses,
            EmailProviderType::Scaleway => EmailProviderTypeRoute::Scaleway,
        }
    }
}

impl From<EmailProviderTypeRoute> for EmailProviderType {
    fn from(t: EmailProviderTypeRoute) -> Self {
        match t {
            EmailProviderTypeRoute::Ses => EmailProviderType::Ses,
            EmailProviderTypeRoute::Scaleway => EmailProviderType::Scaleway,
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SesCredentialsRequest {
    #[schema(example = "AKIAIOSFODNN7EXAMPLE")]
    pub access_key_id: String,
    #[schema(example = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY")]
    pub secret_access_key: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ScalewayCredentialsRequest {
    #[schema(example = "scw-secret-key-12345")]
    pub api_key: String,
    #[schema(example = "12345678-1234-1234-1234-123456789012")]
    pub project_id: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateEmailProviderRequest {
    /// User-friendly name for the provider
    #[schema(example = "My AWS SES")]
    pub name: String,
    /// Provider type
    pub provider_type: EmailProviderTypeRoute,
    /// Cloud region
    #[schema(example = "us-east-1")]
    pub region: String,
    /// AWS SES credentials (required if provider_type is ses)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ses_credentials: Option<SesCredentialsRequest>,
    /// Scaleway credentials (required if provider_type is scaleway)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scaleway_credentials: Option<ScalewayCredentialsRequest>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct EmailProviderResponse {
    pub id: i32,
    #[schema(example = "My AWS SES")]
    pub name: String,
    pub provider_type: EmailProviderTypeRoute,
    #[schema(example = "us-east-1")]
    pub region: String,
    pub is_active: bool,
    /// Masked credentials for display
    pub credentials: serde_json::Value,
    #[schema(example = "2025-12-03T10:30:00Z")]
    pub created_at: String,
    #[schema(example = "2025-12-03T10:30:00Z")]
    pub updated_at: String,
}

// ========================================
// Domain Types
// ========================================

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateEmailDomainRequest {
    /// Provider ID to use for this domain
    pub provider_id: i32,
    /// Domain name (e.g., "updates.example.com")
    #[schema(example = "updates.example.com")]
    pub domain: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DnsRecordResponse {
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

#[derive(Debug, Serialize, ToSchema)]
pub struct EmailDomainResponse {
    pub id: i32,
    pub provider_id: i32,
    #[schema(example = "updates.example.com")]
    pub domain: String,
    #[schema(example = "verified")]
    pub status: String,
    pub last_verified_at: Option<String>,
    pub verification_error: Option<String>,
    #[schema(example = "2025-12-03T10:30:00Z")]
    pub created_at: String,
    #[schema(example = "2025-12-03T10:30:00Z")]
    pub updated_at: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct EmailDomainWithDnsResponse {
    pub domain: EmailDomainResponse,
    pub dns_records: Vec<DnsRecordResponse>,
}

// ========================================
// Email Types
// ========================================

#[derive(Debug, Deserialize, ToSchema)]
pub struct SendEmailRequestBody {
    /// Domain ID to send from
    pub domain_id: i32,
    /// Optional project ID for tracking
    pub project_id: Option<i32>,
    /// Sender email address
    #[schema(example = "hello@updates.example.com")]
    pub from: String,
    /// Sender display name
    #[schema(example = "My App")]
    pub from_name: Option<String>,
    /// Recipient email addresses
    #[schema(example = json!(["user@example.com"]))]
    pub to: Vec<String>,
    /// CC recipients
    pub cc: Option<Vec<String>>,
    /// BCC recipients
    pub bcc: Option<Vec<String>>,
    /// Reply-to address
    pub reply_to: Option<String>,
    /// Email subject
    #[schema(example = "Welcome to our platform!")]
    pub subject: String,
    /// HTML body content
    #[schema(example = "<h1>Hello World</h1>")]
    pub html: Option<String>,
    /// Plain text body content
    #[schema(example = "Hello World")]
    pub text: Option<String>,
    /// Custom headers
    pub headers: Option<HashMap<String, String>>,
    /// Tags for categorization
    #[schema(example = json!(["welcome", "onboarding"]))]
    pub tags: Option<Vec<String>>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SendEmailResponseBody {
    /// Email ID
    #[schema(example = "550e8400-e29b-41d4-a716-446655440000")]
    pub id: String,
    /// Email status
    #[schema(example = "sent")]
    pub status: String,
    /// Provider message ID
    pub provider_message_id: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct EmailResponse {
    #[schema(example = "550e8400-e29b-41d4-a716-446655440000")]
    pub id: String,
    pub domain_id: i32,
    pub project_id: Option<i32>,
    #[schema(example = "hello@updates.example.com")]
    pub from_address: String,
    pub from_name: Option<String>,
    pub to_addresses: Vec<String>,
    pub cc_addresses: Option<Vec<String>>,
    pub bcc_addresses: Option<Vec<String>>,
    pub reply_to: Option<String>,
    pub subject: String,
    pub html_body: Option<String>,
    pub text_body: Option<String>,
    pub headers: Option<HashMap<String, String>>,
    pub tags: Option<Vec<String>>,
    #[schema(example = "sent")]
    pub status: String,
    pub provider_message_id: Option<String>,
    pub error_message: Option<String>,
    pub sent_at: Option<String>,
    #[schema(example = "2025-12-03T10:30:00Z")]
    pub created_at: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct EmailStatsResponse {
    pub total: u64,
    pub sent: u64,
    pub failed: u64,
    pub queued: u64,
    /// Emails captured without sending (Mailhog mode - no provider configured)
    pub captured: u64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PaginatedEmailsResponse {
    pub data: Vec<EmailResponse>,
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
}

#[derive(Debug, Deserialize, ToSchema, IntoParams)]
pub struct ListEmailsQuery {
    pub domain_id: Option<i32>,
    pub project_id: Option<i32>,
    pub status: Option<String>,
    pub from_address: Option<String>,
    #[schema(example = 1)]
    pub page: Option<u64>,
    #[schema(example = 20)]
    pub page_size: Option<u64>,
}
