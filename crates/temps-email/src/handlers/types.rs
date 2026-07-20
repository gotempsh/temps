//! Handler types for the email service

use crate::providers::{EmailProviderType, SmtpEncryption};
use crate::services::{
    DomainService, EmailService, ProviderService, TrackingService, TrackingSetupService,
    ValidationService,
};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use temps_core::AuditLogger;
use temps_dns::services::DnsProviderService;
use utoipa::{IntoParams, ToSchema};

/// Application state for email handlers
pub struct AppState {
    pub provider_service: Arc<ProviderService>,
    pub domain_service: Arc<DomainService>,
    pub email_service: Arc<EmailService>,
    pub validation_service: Arc<ValidationService>,
    pub tracking_service: Arc<TrackingService>,
    pub audit_service: Arc<dyn AuditLogger>,
    /// DNS provider service for automatic DNS record setup
    pub dns_provider_service: Option<Arc<DnsProviderService>>,
    pub telemetry: Arc<dyn temps_core::telemetry::TelemetryReporter>,
    /// AWS-side auto-setup for SES event tracking (SNS topic + webhook
    /// subscription + SESv2 event destination).
    pub tracking_setup_service: Arc<TrackingSetupService>,
    /// For computing the public tracking webhook URL from the configured
    /// external URL at request time (it can change without a restart).
    pub config_service: Arc<temps_config::ConfigService>,
}

// ========================================
// Provider Types
// ========================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum EmailProviderTypeRoute {
    Ses,
    Scaleway,
    Smtp,
}

impl std::fmt::Display for EmailProviderTypeRoute {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EmailProviderTypeRoute::Ses => write!(f, "ses"),
            EmailProviderTypeRoute::Scaleway => write!(f, "scaleway"),
            EmailProviderTypeRoute::Smtp => write!(f, "smtp"),
        }
    }
}

impl From<EmailProviderType> for EmailProviderTypeRoute {
    fn from(t: EmailProviderType) -> Self {
        match t {
            EmailProviderType::Ses => EmailProviderTypeRoute::Ses,
            EmailProviderType::Scaleway => EmailProviderTypeRoute::Scaleway,
            EmailProviderType::Smtp => EmailProviderTypeRoute::Smtp,
        }
    }
}

impl From<EmailProviderTypeRoute> for EmailProviderType {
    fn from(t: EmailProviderTypeRoute) -> Self {
        match t {
            EmailProviderTypeRoute::Ses => EmailProviderType::Ses,
            EmailProviderTypeRoute::Scaleway => EmailProviderType::Scaleway,
            EmailProviderTypeRoute::Smtp => EmailProviderType::Smtp,
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

/// TLS mode for the SMTP relay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, ToSchema, Default)]
#[serde(rename_all = "lowercase")]
pub enum SmtpEncryptionRoute {
    /// STARTTLS — start plain then upgrade. Default; used by AWS SES SMTP, Sendgrid, Mailgun.
    #[default]
    Starttls,
    /// Implicit TLS / SMTPS — TLS from byte 0 (typically port 465).
    Tls,
    /// No encryption. Only for local testing (Mailhog, etc.).
    None,
}

impl From<SmtpEncryptionRoute> for SmtpEncryption {
    fn from(t: SmtpEncryptionRoute) -> Self {
        match t {
            SmtpEncryptionRoute::Starttls => SmtpEncryption::Starttls,
            SmtpEncryptionRoute::Tls => SmtpEncryption::Tls,
            SmtpEncryptionRoute::None => SmtpEncryption::None,
        }
    }
}

/// Generic SMTP credentials request body.
///
/// Works with any SMTP relay — AWS SES SMTP endpoints, Sendgrid, Mailgun,
/// Postmark, or a self-hosted Postfix. Use this when you only have SMTP
/// credentials (i.e. you cannot create identities via the upstream API).
#[derive(Debug, Deserialize, ToSchema)]
pub struct SmtpCredentialsRequest {
    /// SMTP host, e.g. `email-smtp.eu-west-1.amazonaws.com`.
    #[schema(example = "email-smtp.eu-west-1.amazonaws.com")]
    pub host: String,
    /// SMTP port (587 for STARTTLS, 465 for implicit TLS, 25/1025 for plain).
    #[schema(example = 587)]
    pub port: u16,
    /// SMTP username. Leave empty for unauthenticated relays.
    #[serde(default)]
    #[schema(example = "AKIAIOSFODNN7EXAMPLE")]
    pub username: Option<String>,
    /// SMTP password / API token. Required when `username` is set.
    #[serde(default)]
    pub password: Option<String>,
    /// TLS mode. Defaults to STARTTLS.
    #[serde(default)]
    pub encryption: SmtpEncryptionRoute,
    /// Accept self-signed certificates. Only safe for local testing.
    #[serde(default)]
    pub accept_invalid_certs: bool,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateEmailProviderRequest {
    /// User-friendly name for the provider
    #[schema(example = "My AWS SES")]
    pub name: String,
    /// Provider type
    pub provider_type: EmailProviderTypeRoute,
    /// Cloud region. For SMTP this is informational only — the host/port carry the real routing.
    #[schema(example = "us-east-1")]
    pub region: String,
    /// Exact SNS topic allowed to deliver SES events for this provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sns_topic_arn: Option<String>,
    /// AWS SES credentials (required if provider_type is ses)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ses_credentials: Option<SesCredentialsRequest>,
    /// Scaleway credentials (required if provider_type is scaleway)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scaleway_credentials: Option<ScalewayCredentialsRequest>,
    /// Generic SMTP credentials (required if provider_type is smtp). Use when
    /// you only have SMTP creds and want to import an already-set-up domain.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub smtp_credentials: Option<SmtpCredentialsRequest>,
}

/// Request body for `PATCH /email-providers/{id}`.
///
/// All fields are optional. Omit any field to leave it unchanged. The
/// `provider_type` is immutable — to switch providers, delete the row and
/// create a new one. For credentials, supplying any credential variant
/// re-encrypts the stored blob; omitting them preserves the existing secret
/// (so operators can rename without re-typing passwords).
#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateEmailProviderRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(example = "My AWS SES")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(example = "us-east-1")]
    pub region: Option<String>,
    /// Rotate or clear the exact SNS topic allowed for this SES provider.
    /// Omit to preserve it, send `null` to clear it, or send a string to set it.
    #[serde(
        default,
        deserialize_with = "deserialize_present_optional",
        skip_serializing_if = "Option::is_none"
    )]
    pub sns_topic_arn: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_active: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ses_credentials: Option<SesCredentialsRequest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scaleway_credentials: Option<ScalewayCredentialsRequest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smtp_credentials: Option<SmtpCredentialsRequest>,
}

fn deserialize_present_optional<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

#[derive(Debug, Serialize, ToSchema)]
pub struct EmailProviderResponse {
    pub id: i32,
    #[schema(example = "My AWS SES")]
    pub name: String,
    pub provider_type: EmailProviderTypeRoute,
    #[schema(example = "us-east-1")]
    pub region: String,
    pub sns_topic_arn: Option<String>,
    pub is_active: bool,
    /// Masked credentials for display
    pub credentials: serde_json::Value,
    #[schema(example = "2025-12-03T10:30:00Z")]
    pub created_at: String,
    #[schema(example = "2025-12-03T10:30:00Z")]
    pub updated_at: String,
}

/// Live status of the SES event-tracking pipeline for one provider.
#[derive(Debug, Serialize, ToSchema)]
pub struct EmailTrackingStatusResponse {
    /// Public webhook endpoint SNS must deliver events to.
    #[schema(example = "https://temps.example.com/api/t/webhook/ses")]
    pub webhook_url: String,
    /// Only SES providers support SNS event tracking.
    pub supports_event_tracking: bool,
    pub sns_topic_arn: Option<String>,
    /// When the SNS subscription for the current topic was confirmed.
    /// `null` with a topic set usually means the subscription is still
    /// pending — most often because the endpoint was subscribed before the
    /// topic ARN was saved here.
    #[schema(example = "2026-07-18T10:30:00Z")]
    pub subscription_confirmed_at: Option<String>,
    /// Most recent delivered/bounced/complained event recorded for an email
    /// sent through this provider. `null` means no provider feedback has
    /// arrived yet.
    #[schema(example = "2026-07-18T10:31:00Z")]
    pub last_event_at: Option<String>,
}

/// Result of the one-click AWS-side event-tracking setup.
#[derive(Debug, Serialize, ToSchema)]
pub struct EmailTrackingSetupResponse {
    #[schema(example = "arn:aws:sns:us-east-1:123456789012:temps-email-events-1")]
    pub topic_arn: String,
    pub webhook_url: String,
    /// The webhook subscription was requested; SNS confirms it
    /// asynchronously through the webhook itself.
    pub subscription_requested: bool,
    /// The SESv2 event destination (bounce/complaint/delivery) is attached
    /// to the `temps-tracking` configuration set.
    pub event_destination_attached: bool,
}

/// Request body for testing an email provider
#[derive(Debug, Deserialize, ToSchema)]
pub struct TestEmailRequest {
    /// Sender email address (must be verified with the provider)
    #[schema(example = "test@example.com")]
    pub from: String,
    /// Sender display name
    #[schema(example = "My App")]
    pub from_name: Option<String>,
}

/// Response for test email endpoint
#[derive(Debug, Serialize, ToSchema)]
pub struct TestEmailResponse {
    /// Whether the test email was sent successfully
    pub success: bool,
    /// The email address the test was sent to
    #[schema(example = "user@example.com")]
    pub sent_to: String,
    /// Provider message ID if successful
    pub provider_message_id: Option<String>,
    /// Error message if the test failed
    pub error: Option<String>,
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
    /// Verification status: unknown, verified, pending, failed
    #[schema(example = "verified")]
    pub status: DnsRecordStatusResponse,
}

/// DNS record verification status
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum DnsRecordStatusResponse {
    Unknown,
    Verified,
    Pending,
    Failed,
}

impl From<crate::providers::DnsRecordStatus> for DnsRecordStatusResponse {
    fn from(status: crate::providers::DnsRecordStatus) -> Self {
        match status {
            crate::providers::DnsRecordStatus::Unknown => DnsRecordStatusResponse::Unknown,
            crate::providers::DnsRecordStatus::Verified => DnsRecordStatusResponse::Verified,
            crate::providers::DnsRecordStatus::Pending => DnsRecordStatusResponse::Pending,
            crate::providers::DnsRecordStatus::Failed => DnsRecordStatusResponse::Failed,
        }
    }
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

#[derive(Debug, Deserialize, ToSchema, IntoParams)]
pub struct ListDomainsQuery {
    /// Only return domains belonging to this provider
    pub provider_id: Option<i32>,
}

/// Request to setup DNS records using a configured DNS provider
#[derive(Debug, Deserialize, ToSchema)]
pub struct SetupDnsRequest {
    /// The ID of the DNS provider to use for creating records
    pub dns_provider_id: i32,
}

/// Result of a single DNS record creation
#[derive(Debug, Serialize, ToSchema)]
pub struct DnsRecordSetupResult {
    /// Record type (TXT, CNAME, MX)
    pub record_type: String,
    /// Record name
    pub name: String,
    /// Whether the record was created successfully
    pub success: bool,
    /// Whether the operation was automatic or manual
    pub automatic: bool,
    /// Human-readable message
    pub message: String,
}

/// Response from DNS setup operation
#[derive(Debug, Serialize, ToSchema)]
pub struct SetupDnsResponse {
    /// Overall success status
    pub success: bool,
    /// Number of records that were successfully created
    pub records_created: u32,
    /// Total number of records attempted
    pub total_records: u32,
    /// Results for each individual record
    pub results: Vec<DnsRecordSetupResult>,
    /// Human-readable summary message
    pub message: String,
}

// ========================================
// Email Types
// ========================================

#[derive(Debug, Deserialize, ToSchema)]
pub struct SendEmailRequestBody {
    /// Sender email address (domain will be auto-extracted for lookup)
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
    /// Enable open tracking (tracking pixel injection). Defaults to false.
    #[serde(default)]
    pub track_opens: Option<bool>,
    /// Enable click tracking (link rewriting). Defaults to false.
    #[serde(default)]
    pub track_clicks: Option<bool>,
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
    pub domain_id: Option<i32>,
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
    /// The final HTML sent to the provider (with tracking pixel and rewritten links)
    pub tracked_html_body: Option<String>,
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
    /// Whether open tracking is enabled
    pub track_opens: bool,
    /// Whether click tracking is enabled
    pub track_clicks: bool,
    /// Number of times the email was opened
    pub open_count: i32,
    /// Number of times links in the email were clicked
    pub click_count: i32,
    /// When the email was first opened
    pub first_opened_at: Option<String>,
    /// When a link was first clicked
    pub first_clicked_at: Option<String>,
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

#[cfg(test)]
mod tests {
    use super::UpdateEmailProviderRequest;

    #[test]
    fn update_provider_sns_topic_is_tri_state() {
        let omitted: UpdateEmailProviderRequest =
            serde_json::from_value(serde_json::json!({})).expect("omitted topic request");
        assert_eq!(omitted.sns_topic_arn, None);

        let cleared: UpdateEmailProviderRequest =
            serde_json::from_value(serde_json::json!({ "sns_topic_arn": null }))
                .expect("cleared topic request");
        assert_eq!(cleared.sns_topic_arn, Some(None));

        let topic = "arn:aws:sns:us-east-1:123456789012:temps-events";
        let rotated: UpdateEmailProviderRequest =
            serde_json::from_value(serde_json::json!({ "sns_topic_arn": topic }))
                .expect("rotated topic request");
        assert_eq!(rotated.sns_topic_arn, Some(Some(topic.to_string())));
    }
}
