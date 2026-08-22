//! Mock email provider for testing

use async_trait::async_trait;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::errors::EmailError;
use crate::providers::{
    DnsRecord, DnsRecordStatus, DomainIdentity, DomainIdentityDetails, EmailProvider,
    EmailProviderType, SendEmailRequest, SendEmailResponse, VerificationStatus,
};

/// Pre-scripted outcome for a single `MockEmailProvider::send()` call.
///
/// Consumed in order from the front of the queue; when the queue is empty the
/// provider falls back to the `should_fail_send` flag.
#[derive(Debug, Clone)]
pub enum MockSendResult {
    /// Succeed and return a generated `mock-message-<uuid>` ID.
    Succeed,
    /// Return a `SendFailed` error with the specified retryability.
    Fail { retryable: bool },
    /// Return a `ProviderDeliveryUnknown` error (ambiguous — never retried).
    Unknown,
}

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
    pub send_delay: std::time::Duration,

    /// Pre-scripted per-call send outcomes. Consumed from the front of the
    /// queue on each `send()` call. When the queue is exhausted the provider
    /// falls back to `should_fail_send`.
    scripted_responses: Arc<Mutex<VecDeque<MockSendResult>>>,
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
            send_delay: std::time::Duration::ZERO,
            scripted_responses: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    pub fn with_send_failure(mut self) -> Self {
        self.should_fail_send = true;
        self
    }

    pub fn with_send_delay(mut self, delay: std::time::Duration) -> Self {
        self.send_delay = delay;
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

    /// Set a queue of scripted per-call outcomes for `send()`.
    /// They are consumed in order; when the queue is empty, `should_fail_send`
    /// determines the outcome.
    pub fn with_scripted_responses(
        self,
        responses: impl IntoIterator<Item = MockSendResult>,
    ) -> Self {
        *self.scripted_responses.lock().unwrap() = responses.into_iter().collect();
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

        if !self.send_delay.is_zero() {
            tokio::time::sleep(self.send_delay).await;
        }

        // Consume a scripted response if one is queued, otherwise fall back to
        // the blanket `should_fail_send` flag.
        let scripted = self.scripted_responses.lock().unwrap().pop_front();

        match scripted {
            Some(MockSendResult::Fail { retryable }) => {
                return Err(EmailError::SendFailed {
                    provider: "mock".to_string(),
                    retryable,
                    message: format!("Mock scripted failure (retryable={retryable})"),
                });
            }
            Some(MockSendResult::Unknown) => {
                return Err(EmailError::ProviderDeliveryUnknown(
                    "Mock scripted unknown outcome".to_string(),
                ));
            }
            Some(MockSendResult::Succeed) | None => {
                // Succeed branch also covers the queue-exhausted case;
                // if should_fail_send was set, apply it now.
                if scripted.is_none() && self.should_fail_send {
                    return Err(EmailError::ProviderError("Mock send failure".to_string()));
                }
            }
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

    #[tokio::test]
    async fn scripted_retryable_failure_is_send_failed_retryable() {
        let provider = MockEmailProvider::new()
            .with_scripted_responses([MockSendResult::Fail { retryable: true }]);

        let request = SendEmailRequest {
            from: "sender@example.com".to_string(),
            from_name: None,
            to: vec!["recipient@example.com".to_string()],
            cc: None,
            bcc: None,
            reply_to: None,
            subject: "Test".to_string(),
            html: None,
            text: Some("hi".to_string()),
            headers: None,
        };

        let err = provider.send(&request).await.unwrap_err();
        assert!(
            matches!(
                err,
                EmailError::SendFailed {
                    retryable: true,
                    ..
                }
            ),
            "scripted retryable failure must be SendFailed {{ retryable: true }}"
        );
        assert_eq!(provider.send_call_count(), 1);
    }

    #[tokio::test]
    async fn scripted_non_retryable_failure_is_send_failed_not_retryable() {
        let provider = MockEmailProvider::new()
            .with_scripted_responses([MockSendResult::Fail { retryable: false }]);

        let request = SendEmailRequest {
            from: "sender@example.com".to_string(),
            from_name: None,
            to: vec!["recipient@example.com".to_string()],
            cc: None,
            bcc: None,
            reply_to: None,
            subject: "Test".to_string(),
            html: None,
            text: Some("hi".to_string()),
            headers: None,
        };

        let err = provider.send(&request).await.unwrap_err();
        assert!(
            matches!(
                err,
                EmailError::SendFailed {
                    retryable: false,
                    ..
                }
            ),
            "scripted non-retryable failure must be SendFailed {{ retryable: false }}"
        );
    }

    #[tokio::test]
    async fn scripted_unknown_outcome_is_provider_delivery_unknown() {
        let provider = MockEmailProvider::new().with_scripted_responses([MockSendResult::Unknown]);

        let request = SendEmailRequest {
            from: "sender@example.com".to_string(),
            from_name: None,
            to: vec!["recipient@example.com".to_string()],
            cc: None,
            bcc: None,
            reply_to: None,
            subject: "Test".to_string(),
            html: None,
            text: Some("hi".to_string()),
            headers: None,
        };

        let err = provider.send(&request).await.unwrap_err();
        assert!(
            matches!(err, EmailError::ProviderDeliveryUnknown(_)),
            "scripted Unknown must be ProviderDeliveryUnknown"
        );
    }

    #[tokio::test]
    async fn scripted_fail_then_succeed_succeeds_on_second_call() {
        let provider = MockEmailProvider::new().with_scripted_responses([
            MockSendResult::Fail { retryable: true },
            MockSendResult::Succeed,
        ]);

        let request = SendEmailRequest {
            from: "sender@example.com".to_string(),
            from_name: None,
            to: vec!["recipient@example.com".to_string()],
            cc: None,
            bcc: None,
            reply_to: None,
            subject: "Test".to_string(),
            html: None,
            text: Some("hi".to_string()),
            headers: None,
        };

        // First call: fails
        let first = provider.send(&request).await;
        assert!(first.is_err());
        assert_eq!(provider.send_call_count(), 1);

        // Second call: succeeds (scripted Succeed)
        let second = provider.send(&request).await;
        assert!(second.is_ok());
        assert_eq!(provider.send_call_count(), 2);
    }
}
