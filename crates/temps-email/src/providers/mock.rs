//! Mock email provider for testing

use async_trait::async_trait;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use crate::errors::EmailError;
use crate::providers::{
    DnsRecord, DnsRecordStatus, DomainIdentity, DomainIdentityDetails, EmailProvider,
    EmailProviderType, SendEmailRequest, SendEmailResponse, VerificationStatus,
};

/// Mock email provider for testing
#[derive(Debug, Clone)]
pub struct MockEmailProvider {
    /// Counter for tracking calls
    pub send_count: Arc<AtomicUsize>,
    pub create_identity_count: Arc<AtomicUsize>,
    pub verify_identity_count: Arc<AtomicUsize>,
    pub delete_identity_count: Arc<AtomicUsize>,

    /// Configurable responses
    pub should_fail_send: bool,
    pub should_fail_verify: bool,
    pub verification_status: VerificationStatus,
}

impl Default for MockEmailProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl MockEmailProvider {
    pub fn new() -> Self {
        Self {
            send_count: Arc::new(AtomicUsize::new(0)),
            create_identity_count: Arc::new(AtomicUsize::new(0)),
            verify_identity_count: Arc::new(AtomicUsize::new(0)),
            delete_identity_count: Arc::new(AtomicUsize::new(0)),
            should_fail_send: false,
            should_fail_verify: false,
            verification_status: VerificationStatus::Verified,
        }
    }

    pub fn with_send_failure(mut self) -> Self {
        self.should_fail_send = true;
        self
    }

    pub fn with_verify_failure(mut self) -> Self {
        self.should_fail_verify = true;
        self
    }

    pub fn with_verification_status(mut self, status: VerificationStatus) -> Self {
        self.verification_status = status;
        self
    }

    pub fn send_call_count(&self) -> usize {
        self.send_count.load(Ordering::SeqCst)
    }

    pub fn create_identity_call_count(&self) -> usize {
        self.create_identity_count.load(Ordering::SeqCst)
    }

    pub fn verify_identity_call_count(&self) -> usize {
        self.verify_identity_count.load(Ordering::SeqCst)
    }

    pub fn delete_identity_call_count(&self) -> usize {
        self.delete_identity_count.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl EmailProvider for MockEmailProvider {
    async fn create_identity(&self, domain: &str) -> Result<DomainIdentity, EmailError> {
        self.create_identity_count.fetch_add(1, Ordering::SeqCst);

        let mail_from_domain = format!("send.{}", domain);
        Ok(DomainIdentity {
            provider_identity_id: format!("mock-identity-{}", domain),
            // SPF on MAIL FROM subdomain (send.domain.com)
            spf_record: Some(DnsRecord {
                record_type: "TXT".to_string(),
                name: mail_from_domain.clone(),
                value: "v=spf1 include:mock.example.com ~all".to_string(),
                priority: None,
                status: DnsRecordStatus::Pending,
            }),
            // DKIM on root domain
            dkim_records: vec![DnsRecord {
                record_type: "CNAME".to_string(),
                name: format!("mock._domainkey.{}", domain),
                value: "mock.dkim.example.com".to_string(),
                priority: None,
                status: DnsRecordStatus::Pending,
            }],
            dkim_selector: Some("mock".to_string()),
            // MX on MAIL FROM subdomain (send.domain.com)
            mx_record: Some(DnsRecord {
                record_type: "MX".to_string(),
                name: mail_from_domain,
                value: "feedback-smtp.mock.example.com".to_string(),
                priority: Some(10),
                status: DnsRecordStatus::Pending,
            }),
            mail_from_subdomain: Some("send".to_string()),
        })
    }

    async fn verify_identity(&self, _domain: &str) -> Result<VerificationStatus, EmailError> {
        self.verify_identity_count.fetch_add(1, Ordering::SeqCst);

        if self.should_fail_verify {
            return Err(EmailError::ProviderError(
                "Mock verification failure".to_string(),
            ));
        }

        Ok(self.verification_status.clone())
    }

    async fn get_identity_details(
        &self,
        domain: &str,
    ) -> Result<DomainIdentityDetails, EmailError> {
        // Map verification status to DNS record status
        let record_status = match &self.verification_status {
            VerificationStatus::Verified => DnsRecordStatus::Verified,
            VerificationStatus::Pending => DnsRecordStatus::Pending,
            VerificationStatus::Failed(_) => DnsRecordStatus::Failed,
            _ => DnsRecordStatus::Unknown,
        };

        let mail_from_domain = format!("send.{}", domain);
        Ok(DomainIdentityDetails {
            overall_status: self.verification_status.clone(),
            // SPF on MAIL FROM subdomain (send.domain.com)
            spf_record: Some(DnsRecord {
                record_type: "TXT".to_string(),
                name: mail_from_domain.clone(),
                value: "v=spf1 include:mock.example.com ~all".to_string(),
                priority: None,
                status: record_status,
            }),
            // DKIM on root domain
            dkim_records: vec![DnsRecord {
                record_type: "CNAME".to_string(),
                name: format!("mock._domainkey.{}", domain),
                value: "mock.dkim.example.com".to_string(),
                priority: None,
                status: record_status,
            }],
            // MX on MAIL FROM subdomain (send.domain.com)
            mx_record: Some(DnsRecord {
                record_type: "MX".to_string(),
                name: mail_from_domain,
                value: "feedback-smtp.mock.example.com".to_string(),
                priority: Some(10),
                status: record_status,
            }),
            mail_from_subdomain: Some("send".to_string()),
        })
    }

    async fn delete_identity(&self, _domain: &str) -> Result<(), EmailError> {
        self.delete_identity_count.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn send(&self, _email: &SendEmailRequest) -> Result<SendEmailResponse, EmailError> {
        self.send_count.fetch_add(1, Ordering::SeqCst);

        if self.should_fail_send {
            return Err(EmailError::ProviderError("Mock send failure".to_string()));
        }

        Ok(SendEmailResponse {
            message_id: format!("mock-message-{}", uuid::Uuid::new_v4()),
        })
    }

    fn provider_type(&self) -> EmailProviderType {
        EmailProviderType::Ses // Use SES as default mock type
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_mock_provider_create_identity() {
        let provider = MockEmailProvider::new();

        let identity = provider.create_identity("example.com").await.unwrap();

        assert_eq!(identity.provider_identity_id, "mock-identity-example.com");
        assert!(identity.spf_record.is_some());
        assert_eq!(identity.dkim_records.len(), 1);
        assert!(identity.mx_record.is_some());
        assert_eq!(provider.create_identity_call_count(), 1);
    }

    #[tokio::test]
    async fn test_mock_provider_verify_identity() {
        let provider = MockEmailProvider::new();

        let status = provider.verify_identity("example.com").await.unwrap();

        assert!(matches!(status, VerificationStatus::Verified));
        assert_eq!(provider.verify_identity_call_count(), 1);
    }

    #[tokio::test]
    async fn test_mock_provider_verify_pending() {
        let provider =
            MockEmailProvider::new().with_verification_status(VerificationStatus::Pending);

        let status = provider.verify_identity("example.com").await.unwrap();

        assert!(matches!(status, VerificationStatus::Pending));
    }

    #[tokio::test]
    async fn test_mock_provider_verify_failure() {
        let provider = MockEmailProvider::new().with_verify_failure();

        let result = provider.verify_identity("example.com").await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_mock_provider_send_email() {
        let provider = MockEmailProvider::new();

        let request = SendEmailRequest {
            from: "sender@example.com".to_string(),
            from_name: None,
            to: vec!["recipient@example.com".to_string()],
            cc: None,
            bcc: None,
            reply_to: None,
            subject: "Test".to_string(),
            html: Some("<p>Test</p>".to_string()),
            text: None,
            headers: None,
        };

        let response = provider.send(&request).await.unwrap();

        assert!(response.message_id.starts_with("mock-message-"));
        assert_eq!(provider.send_call_count(), 1);
    }

    #[tokio::test]
    async fn test_mock_provider_send_failure() {
        let provider = MockEmailProvider::new().with_send_failure();

        let request = SendEmailRequest {
            from: "sender@example.com".to_string(),
            from_name: None,
            to: vec!["recipient@example.com".to_string()],
            cc: None,
            bcc: None,
            reply_to: None,
            subject: "Test".to_string(),
            html: Some("<p>Test</p>".to_string()),
            text: None,
            headers: None,
        };

        let result = provider.send(&request).await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_mock_provider_delete_identity() {
        let provider = MockEmailProvider::new();

        provider.delete_identity("example.com").await.unwrap();

        assert_eq!(provider.delete_identity_call_count(), 1);
    }

    #[test]
    fn test_mock_provider_type() {
        let provider = MockEmailProvider::new();
        assert_eq!(provider.provider_type(), EmailProviderType::Ses);
    }
}
