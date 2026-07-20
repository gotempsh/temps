//! Email service for sending and managing emails

use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait,
    PaginatorTrait, QueryFilter, QueryOrder,
};
use std::sync::Arc;
use temps_entities::emails;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::errors::EmailError;
use crate::providers::SendEmailRequest as ProviderSendRequest;
use crate::services::{DomainService, ProviderService, SuppressionService, TrackingService};

/// Trait for rewriting HTML to inject tracking (pixel + click links).
/// Implemented by `temps-email-tracking::HtmlTrackingRewriter`.
pub trait TrackingRewriter: Send + Sync {
    fn rewrite(&self, email_id: &Uuid, html: &str) -> Result<String, String>;
}

/// Service for sending and managing emails
pub struct EmailService {
    db: Arc<DatabaseConnection>,
    provider_service: Arc<ProviderService>,
    domain_service: Arc<DomainService>,
    tracking_rewriter: Option<Arc<dyn TrackingRewriter>>,
    tracking_service: Arc<TrackingService>,
    suppression_service: Arc<SuppressionService>,
}

/// Request to send an email
#[derive(Debug, Clone)]
pub struct SendEmailRequest {
    /// Sender email address (domain will be auto-extracted for lookup)
    pub from: String,
    pub from_name: Option<String>,
    pub to: Vec<String>,
    pub cc: Option<Vec<String>>,
    pub bcc: Option<Vec<String>>,
    pub reply_to: Option<String>,
    pub subject: String,
    pub html: Option<String>,
    pub text: Option<String>,
    pub headers: Option<std::collections::HashMap<String, String>>,
    pub tags: Option<Vec<String>>,
    /// Enable open tracking (tracking pixel injection)
    pub track_opens: bool,
    /// Enable click tracking (link rewriting)
    pub track_clicks: bool,
}

/// Response from sending an email
#[derive(Debug, Clone)]
pub struct SendEmailResponse {
    pub id: Uuid,
    pub status: String,
    pub provider_message_id: Option<String>,
}

/// Query options for listing emails
#[derive(Debug, Clone, Default)]
pub struct ListEmailsOptions {
    pub domain_id: Option<i32>,
    pub project_id: Option<i32>,
    pub status: Option<String>,
    pub from_address: Option<String>,
    pub page: Option<u64>,
    pub page_size: Option<u64>,
}

impl EmailService {
    pub fn new(
        db: Arc<DatabaseConnection>,
        provider_service: Arc<ProviderService>,
        domain_service: Arc<DomainService>,
        tracking_service: Arc<TrackingService>,
        suppression_service: Arc<SuppressionService>,
    ) -> Self {
        Self {
            db,
            provider_service,
            domain_service,
            tracking_rewriter: None,
            tracking_service,
            suppression_service,
        }
    }

    /// Set the tracking rewriter for injecting open/click tracking into emails
    pub fn with_tracking_rewriter(mut self, rewriter: Arc<dyn TrackingRewriter>) -> Self {
        self.tracking_rewriter = Some(rewriter);
        self
    }

    /// Send an email
    ///
    /// The flow is:
    /// 1. Extract domain from 'from' email address
    /// 2. Look up domain in database by domain name
    /// 3. Always store the email in the database for visualization
    /// 4. If domain is configured and verified, send via provider and mark as "sent"
    /// 5. If domain is not configured or not verified, mark as "captured" (Mailhog-like behavior)
    pub async fn send(&self, request: SendEmailRequest) -> Result<SendEmailResponse, EmailError> {
        debug!("Sending email from {} to {:?}", request.from, request.to);

        // Extract domain from 'from' address
        let from_domain = request
            .from
            .split('@')
            .nth(1)
            .ok_or_else(|| EmailError::Validation("Invalid from address".to_string()))?;

        // Look up domain by extracted domain name
        let domain = self.domain_service.find_by_domain_name(from_domain).await?;

        // Generate email ID
        let email_id = Uuid::new_v4();

        // Apply tracking transformations if enabled
        let track_opens = request.track_opens;
        let track_clicks = request.track_clicks;
        let mut tracked_html = request.html.clone();
        let mut extracted_links = Vec::new();

        if let Some(html) = &request.html {
            if track_opens || track_clicks {
                let transform_result = self
                    .tracking_service
                    .transform_html(email_id, html, track_opens, track_clicks)
                    .await;
                tracked_html = Some(transform_result.html);
                extracted_links = transform_result.links;
            }
        }

        // Create email record - always store for visualization
        let email = emails::ActiveModel {
            id: Set(email_id),
            domain_id: Set(domain.as_ref().map(|d| d.id)),
            project_id: Set(None),
            from_address: Set(request.from.clone()),
            from_name: Set(request.from_name.clone()),
            to_addresses: Set(serde_json::to_value(&request.to)?),
            cc_addresses: Set(request.cc.as_ref().map(serde_json::to_value).transpose()?),
            bcc_addresses: Set(request.bcc.as_ref().map(serde_json::to_value).transpose()?),
            reply_to: Set(request.reply_to.clone()),
            subject: Set(request.subject.clone()),
            html_body: Set(request.html.clone()),
            tracked_html_body: Set(tracked_html.clone()),
            text_body: Set(request.text.clone()),
            headers: Set(request
                .headers
                .as_ref()
                .map(serde_json::to_value)
                .transpose()?),
            tags: Set(request
                .tags
                .as_ref()
                .map(serde_json::to_value)
                .transpose()?),
            status: Set("queued".to_string()),
            track_opens: Set(track_opens),
            track_clicks: Set(track_clicks),
            open_count: Set(0),
            click_count: Set(0),
            ..Default::default()
        };

        let email_model = email.insert(self.db.as_ref()).await?;

        // Store extracted links for click tracking
        if !extracted_links.is_empty() {
            if let Err(e) = self
                .tracking_service
                .store_links(email_id, &extracted_links)
                .await
            {
                warn!(
                    "Failed to store tracking links for email {}: {}",
                    email_id, e
                );
            }
        }

        // Refuse to send to previously hard-bounced/complained addresses —
        // repeatedly emailing one is exactly what gets a sending domain's
        // reputation downgraded by receiving mail providers. Checked after
        // the row is inserted (still visible for debugging) but before any
        // domain/provider work, since it's independent of both.
        //
        // Checks to/cc/bcc together (a suppressed address left in cc/bcc
        // would otherwise still receive mail), and drops only the
        // suppressed addresses rather than capturing the whole send — a
        // suppressed address mixed into `to` alongside legitimate
        // recipients used to silently deny delivery to everyone on the
        // email, not just the bad address.
        let mut all_recipients: Vec<String> = request.to.clone();
        all_recipients.extend(request.cc.iter().flatten().cloned());
        all_recipients.extend(request.bcc.iter().flatten().cloned());

        let suppressed = match domain.as_ref() {
            Some(domain) => {
                self.suppression_service
                    .suppressed_among(domain.id, &all_recipients)
                    .await?
            }
            None => Vec::new(),
        };

        if !suppressed.is_empty() {
            // Addresses go to `debug!` only — `info!` (and any field
            // persisted where a caller with send+read access could read it
            // back, like `error_message` below) must not let one recipient
            // enumerate another recipient's bounce/complaint history.
            info!(
                "Dropping {} suppressed recipient(s) from email {}",
                suppressed.len(),
                email_id
            );
            debug!(
                "Suppressed recipient(s) dropped from email {}: {:?}",
                email_id, suppressed
            );
        }
        let (to, cc, bcc) =
            filter_suppressed_recipients(request.to, request.cc, request.bcc, &suppressed);

        // Nothing left to send to (either every `to` address was
        // suppressed, or `to` was already empty) — capture instead of
        // sending an email with no primary recipient.
        if to.is_empty() {
            info!(
                "Refusing to send email {} — {} recipient(s) suppressed",
                email_id,
                suppressed.len()
            );
            debug!(
                "Email {} refused — suppressed recipient(s): {:?}",
                email_id, suppressed
            );

            let mut active_model: emails::ActiveModel = email_model.into();
            active_model.status = Set("captured".to_string());
            active_model.error_message = Set(Some(
                "Recipient(s) suppressed (previous hard bounce or complaint)".to_string(),
            ));
            active_model.sent_at = Set(Some(Utc::now()));

            active_model.update(self.db.as_ref()).await?;

            return Ok(SendEmailResponse {
                id: email_id,
                status: "captured".to_string(),
                provider_message_id: None,
            });
        }

        // If no domain configured, capture email without sending (Mailhog-like behavior)
        let domain = match domain {
            Some(d) => d,
            None => {
                info!(
                    "No domain configured for '{}', capturing email without sending (Mailhog mode)",
                    from_domain
                );

                let mut active_model: emails::ActiveModel = email_model.into();
                active_model.status = Set("captured".to_string());
                active_model.sent_at = Set(Some(Utc::now()));

                active_model.update(self.db.as_ref()).await?;

                info!(
                    "Email captured (no domain configured), id: {}, from: {}, to: {:?}",
                    email_id, request.from, to
                );

                return Ok(SendEmailResponse {
                    id: email_id,
                    status: "captured".to_string(),
                    provider_message_id: None,
                });
            }
        };

        // Check if domain is verified
        if domain.status != "verified" {
            info!(
                "Domain '{}' is not verified (status: {}), capturing email without sending",
                domain.domain, domain.status
            );

            let mut active_model: emails::ActiveModel = email_model.into();
            active_model.status = Set("captured".to_string());
            active_model.error_message = Set(Some(format!(
                "Domain '{}' not verified (status: {})",
                domain.domain, domain.status
            )));
            active_model.sent_at = Set(Some(Utc::now()));

            active_model.update(self.db.as_ref()).await?;

            return Ok(SendEmailResponse {
                id: email_id,
                status: "captured".to_string(),
                provider_message_id: None,
            });
        }

        // Try to get provider - if not configured, capture email
        let provider = match self.provider_service.get(domain.provider_id).await {
            Ok(p) => Some(p),
            Err(e) => {
                info!(
                    "No provider configured for domain '{}', capturing email without sending (Mailhog mode)",
                    domain.domain
                );
                debug!("Provider lookup error: {}", e);
                None
            }
        };

        // If no provider, mark as captured and return success
        if provider.is_none() {
            let mut active_model: emails::ActiveModel = email_model.into();
            active_model.status = Set("captured".to_string());
            active_model.sent_at = Set(Some(Utc::now()));

            active_model.update(self.db.as_ref()).await?;

            info!(
                "Email captured (no provider), id: {}, from: {}, to: {:?}",
                email_id, request.from, to
            );

            return Ok(SendEmailResponse {
                id: email_id,
                status: "captured".to_string(),
                provider_message_id: None,
            });
        }

        let provider = provider.unwrap();

        let provider_instance = match self
            .provider_service
            .create_provider_instance(&provider)
            .await
        {
            Ok(instance) => instance,
            Err(e) => {
                // Provider exists but failed to create instance - capture email instead of failing
                info!(
                    "Failed to create provider instance, capturing email without sending: {}",
                    e
                );
                let mut active_model: emails::ActiveModel = email_model.into();
                active_model.status = Set("captured".to_string());
                active_model.error_message = Set(Some(format!("Provider unavailable: {}", e)));
                active_model.sent_at = Set(Some(Utc::now()));
                active_model.update(self.db.as_ref()).await?;

                return Ok(SendEmailResponse {
                    id: email_id,
                    status: "captured".to_string(),
                    provider_message_id: None,
                });
            }
        };

        // Use tracked HTML (with open/click tracking injected) if available

        let provider_request = ProviderSendRequest {
            from: request.from,
            from_name: request.from_name,
            to,
            cc,
            bcc,
            reply_to: request.reply_to,
            subject: request.subject,
            html: tracked_html,
            text: request.text,
            headers: request.headers,
        };

        match provider_instance.send(&provider_request).await {
            Ok(response) => {
                // Update email with success status
                let mut active_model: emails::ActiveModel = email_model.clone().into();
                active_model.status = Set("sent".to_string());
                active_model.provider_message_id = Set(Some(response.message_id.clone()));
                active_model.sent_at = Set(Some(Utc::now()));

                let _email_model = active_model.update(self.db.as_ref()).await?;

                info!(
                    "Email sent successfully, id: {}, provider_message_id: {}",
                    email_id, response.message_id
                );

                Ok(SendEmailResponse {
                    id: email_id,
                    status: "sent".to_string(),
                    provider_message_id: Some(response.message_id),
                })
            }
            Err(e) => {
                // Provider send failed - capture email instead of failing
                info!(
                    "Failed to send email via provider, capturing instead: {}",
                    e
                );

                let mut active_model: emails::ActiveModel = email_model.into();
                active_model.status = Set("captured".to_string());
                active_model.error_message = Set(Some(format!("Send failed: {}", e)));
                active_model.sent_at = Set(Some(Utc::now()));

                active_model.update(self.db.as_ref()).await?;

                Ok(SendEmailResponse {
                    id: email_id,
                    status: "captured".to_string(),
                    provider_message_id: None,
                })
            }
        }
    }

    /// Get an email by ID
    pub async fn get(&self, id: Uuid) -> Result<emails::Model, EmailError> {
        emails::Entity::find_by_id(id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| EmailError::EmailNotFound(id.to_string()))
    }

    /// List emails with optional filtering
    pub async fn list(
        &self,
        options: ListEmailsOptions,
    ) -> Result<(Vec<emails::Model>, u64), EmailError> {
        let page = options.page.unwrap_or(1);
        let page_size = std::cmp::min(options.page_size.unwrap_or(20), 100);

        let mut query = emails::Entity::find().order_by_desc(emails::Column::CreatedAt);

        if let Some(domain_id) = options.domain_id {
            query = query.filter(emails::Column::DomainId.eq(domain_id));
        }

        if let Some(project_id) = options.project_id {
            query = query.filter(emails::Column::ProjectId.eq(project_id));
        }

        if let Some(status) = options.status {
            query = query.filter(emails::Column::Status.eq(status));
        }

        if let Some(from_address) = options.from_address {
            query = query.filter(emails::Column::FromAddress.eq(from_address));
        }

        let paginator = query.paginate(self.db.as_ref(), page_size);
        let total = paginator.num_items().await?;
        let items = paginator.fetch_page(page - 1).await?;

        Ok((items, total))
    }

    /// Get email count by status
    pub async fn count_by_status(&self, domain_id: Option<i32>) -> Result<EmailStats, EmailError> {
        let mut base_query = emails::Entity::find();

        if let Some(domain_id) = domain_id {
            base_query = base_query.filter(emails::Column::DomainId.eq(domain_id));
        }

        let total = base_query.clone().count(self.db.as_ref()).await?;

        let sent = base_query
            .clone()
            .filter(emails::Column::Status.eq("sent"))
            .count(self.db.as_ref())
            .await?;

        let failed = base_query
            .clone()
            .filter(emails::Column::Status.eq("failed"))
            .count(self.db.as_ref())
            .await?;

        let queued = base_query
            .clone()
            .filter(emails::Column::Status.eq("queued"))
            .count(self.db.as_ref())
            .await?;

        let captured = base_query
            .filter(emails::Column::Status.eq("captured"))
            .count(self.db.as_ref())
            .await?;

        Ok(EmailStats {
            total,
            sent,
            failed,
            queued,
            captured,
        })
    }
}

/// Email statistics
#[derive(Debug, Clone)]
pub struct EmailStats {
    pub total: u64,
    pub sent: u64,
    pub failed: u64,
    pub queued: u64,
    pub captured: u64,
}

/// Drop suppressed addresses from `to`/`cc`/`bcc`. `suppressed` is the
/// (already-normalized) output of `SuppressionService::suppressed_among`.
///
/// Filters each list independently rather than rejecting the whole send —
/// a suppressed address mixed into `to` alongside legitimate recipients
/// must not deny delivery to everyone on the email, just to itself.
fn filter_suppressed_recipients(
    to: Vec<String>,
    cc: Option<Vec<String>>,
    bcc: Option<Vec<String>>,
    suppressed: &[String],
) -> (Vec<String>, Option<Vec<String>>, Option<Vec<String>>) {
    if suppressed.is_empty() {
        return (to, cc, bcc);
    }

    let suppressed_set: std::collections::HashSet<&str> =
        suppressed.iter().map(String::as_str).collect();
    let keep =
        |addr: &String| !suppressed_set.contains(SuppressionService::normalize(addr).as_str());

    (
        to.into_iter().filter(&keep).collect(),
        cc.map(|list| list.into_iter().filter(&keep).collect()),
        bcc.map(|list| list.into_iter().filter(&keep).collect()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::{EmailProviderType, SesCredentials};
    use crate::services::provider_service::{CreateProviderRequest, ProviderCredentials};
    use crate::services::TrackingService;
    use temps_core::EncryptionService;
    use temps_database::test_utils::TestDatabase;

    // Helper to create a test encryption service
    fn create_test_encryption_service() -> Arc<EncryptionService> {
        let key = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        Arc::new(EncryptionService::new(key).unwrap())
    }

    // Helper to setup test environment with real database
    async fn setup_test_env() -> (TestDatabase, EmailService, ProviderService, DomainService) {
        let db = TestDatabase::with_migrations().await.unwrap();
        let encryption_service = create_test_encryption_service();
        let provider_service = ProviderService::new(db.db.clone(), encryption_service);
        let domain_service = DomainService::new(db.db.clone(), Arc::new(provider_service.clone()));
        let server_config = Arc::new(temps_config::ServerConfig {
            address: "0.0.0.0:3000".to_string(),
            database_url: "postgres://localhost/test".to_string(),
            tls_address: None,
            console_address: "0.0.0.0:3001".to_string(),
            console_admin_address: None,
            admin_allowed_ips: Vec::new(),
            admin_allowed_hosts: Vec::new(),
            admin_trust_forwarded_for: false,
            data_dir: std::path::PathBuf::from("/tmp/temps-test"),
            auth_secret: "test-secret".to_string(),
            encryption_key: "test-encryption-key-32bytes!!!!!".to_string(),
            api_base_url: "http://localhost:3000".to_string(),
            postgres_max_connections: None,
            postgres_min_connections: None,
            postgres_connect_timeout_secs: None,
            postgres_acquire_timeout_secs: None,
            postgres_idle_timeout_secs: None,
            postgres_max_lifetime_secs: None,
            clickhouse_url: None,
            clickhouse_database: None,
            clickhouse_user: None,
            clickhouse_password: None,
        });
        let config_service = Arc::new(temps_config::ConfigService::new(
            server_config,
            db.db.clone(),
        ));
        let tracking_service = Arc::new(TrackingService::with_base_url(
            db.db.clone(),
            config_service,
            "http://localhost:3000".to_string(),
        ));
        let suppression_service = Arc::new(SuppressionService::new(db.db.clone()));
        let email_service = EmailService::new(
            db.db.clone(),
            Arc::new(provider_service.clone()),
            Arc::new(domain_service.clone()),
            tracking_service,
            suppression_service,
        );
        (db, email_service, provider_service, domain_service)
    }

    // Helper to create a test provider
    async fn create_test_provider(
        service: &ProviderService,
    ) -> temps_entities::email_providers::Model {
        let request = CreateProviderRequest {
            name: format!("Test Provider {}", uuid::Uuid::new_v4()),
            provider_type: EmailProviderType::Ses,
            region: "us-east-1".to_string(),
            credentials: ProviderCredentials::Ses(SesCredentials {
                access_key_id: "AKIAIOSFODNN7EXAMPLE".to_string(),
                secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_string(),
                endpoint_url: None,
            }),
        };
        service.create(request).await.unwrap()
    }

    // Helper to create a test domain directly in database (bypasses provider's create_identity)
    // This is needed for integration tests because we don't have valid AWS/Scaleway credentials
    async fn create_test_domain(
        db: &Arc<sea_orm::DatabaseConnection>,
        provider_id: i32,
        domain_name: &str,
    ) -> temps_entities::email_domains::Model {
        use sea_orm::{ActiveModelTrait, ActiveValue::Set};
        use temps_entities::email_domains;

        let domain = email_domains::ActiveModel {
            provider_id: Set(provider_id),
            domain: Set(domain_name.to_string()),
            status: Set("pending".to_string()),
            spf_record_name: Set(Some(domain_name.to_string())),
            spf_record_value: Set(Some("v=spf1 include:mock.example.com ~all".to_string())),
            dkim_selector: Set(Some("mock".to_string())),
            dkim_record_name: Set(Some(format!("mock._domainkey.{}", domain_name))),
            dkim_record_value: Set(Some("v=DKIM1; k=rsa; p=MOCKPUBLICKEY".to_string())),
            mx_record_name: Set(Some(domain_name.to_string())),
            mx_record_value: Set(Some("feedback-smtp.mock.example.com".to_string())),
            mx_record_priority: Set(Some(10)),
            provider_identity_id: Set(Some(format!("mock-identity-{}", domain_name))),
            ..Default::default()
        };

        domain.insert(db.as_ref()).await.unwrap()
    }

    // ============================================
    // Unit Tests (No database required)
    // ============================================

    #[test]
    fn test_send_email_request_builder() {
        let request = SendEmailRequest {
            from: "sender@example.com".to_string(),
            from_name: Some("Sender Name".to_string()),
            to: vec!["recipient@example.com".to_string()],
            cc: Some(vec!["cc@example.com".to_string()]),
            bcc: Some(vec!["bcc@example.com".to_string()]),
            reply_to: Some("reply@example.com".to_string()),
            subject: "Test Subject".to_string(),
            html: Some("<h1>Hello</h1>".to_string()),
            text: Some("Hello".to_string()),
            headers: Some(std::collections::HashMap::from([(
                "X-Custom-Header".to_string(),
                "value".to_string(),
            )])),
            tags: Some(vec!["tag1".to_string(), "tag2".to_string()]),
            track_opens: false,
            track_clicks: false,
        };

        assert_eq!(request.from, "sender@example.com");
        assert_eq!(request.from_name, Some("Sender Name".to_string()));
        assert_eq!(request.to, vec!["recipient@example.com".to_string()]);
        assert_eq!(request.subject, "Test Subject");
        assert!(request.html.is_some());
        assert!(request.text.is_some());
        assert!(request.headers.is_some());
        assert_eq!(request.tags.as_ref().unwrap().len(), 2);
    }

    #[test]
    fn test_send_email_response() {
        let response = SendEmailResponse {
            id: Uuid::new_v4(),
            status: "sent".to_string(),
            provider_message_id: Some("msg-123".to_string()),
        };

        assert_eq!(response.status, "sent");
        assert!(response.provider_message_id.is_some());
    }

    #[test]
    fn test_list_emails_options_default() {
        let options = ListEmailsOptions::default();

        assert!(options.domain_id.is_none());
        assert!(options.project_id.is_none());
        assert!(options.status.is_none());
        assert!(options.from_address.is_none());
        assert!(options.page.is_none());
        assert!(options.page_size.is_none());
    }

    #[test]
    fn test_list_emails_options_with_filters() {
        let options = ListEmailsOptions {
            domain_id: Some(1),
            project_id: Some(100),
            status: Some("sent".to_string()),
            from_address: Some("sender@example.com".to_string()),
            page: Some(1),
            page_size: Some(20),
        };

        assert_eq!(options.domain_id, Some(1));
        assert_eq!(options.project_id, Some(100));
        assert_eq!(options.status, Some("sent".to_string()));
        assert_eq!(options.from_address, Some("sender@example.com".to_string()));
        assert_eq!(options.page, Some(1));
        assert_eq!(options.page_size, Some(20));
    }

    #[test]
    fn test_email_stats() {
        let stats = EmailStats {
            total: 100,
            sent: 70,
            failed: 10,
            queued: 5,
            captured: 15,
        };

        assert_eq!(stats.total, 100);
        assert_eq!(stats.sent, 70);
        assert_eq!(stats.failed, 10);
        assert_eq!(stats.queued, 5);
        assert_eq!(stats.captured, 15);
    }

    #[test]
    fn test_from_address_domain_extraction() {
        let from = "sender@example.com";
        let domain = from.split('@').nth(1);
        assert_eq!(domain, Some("example.com"));

        let invalid_from = "invalid-email";
        let domain = invalid_from.split('@').nth(1);
        assert!(domain.is_none());
    }

    #[test]
    fn filter_suppressed_recipients_passes_through_when_nothing_suppressed() {
        let (to, cc, bcc) = filter_suppressed_recipients(
            vec!["a@example.com".to_string()],
            Some(vec!["b@example.com".to_string()]),
            None,
            &[],
        );
        assert_eq!(to, vec!["a@example.com"]);
        assert_eq!(cc, Some(vec!["b@example.com".to_string()]));
        assert_eq!(bcc, None);
    }

    #[test]
    fn filter_suppressed_recipients_drops_only_the_suppressed_cc_address() {
        // A suppressed address in `cc` used to be invisible to the check
        // entirely — it must be dropped from the send, and it must not take
        // the legitimate `to` recipient down with it.
        let (to, cc, bcc) = filter_suppressed_recipients(
            vec!["good@example.com".to_string()],
            Some(vec![
                "bad@example.com".to_string(),
                "also-good@example.com".to_string(),
            ]),
            None,
            &["bad@example.com".to_string()],
        );
        assert_eq!(to, vec!["good@example.com"]);
        assert_eq!(cc, Some(vec!["also-good@example.com".to_string()]));
        assert_eq!(bcc, None);
    }

    #[test]
    fn filter_suppressed_recipients_keeps_other_to_addresses() {
        // One suppressed address mixed into `to` used to capture the whole
        // send — the other `to` recipients must still get the email.
        let (to, cc, bcc) = filter_suppressed_recipients(
            vec![
                "bad@example.com".to_string(),
                "good@example.com".to_string(),
            ],
            None,
            None,
            &["bad@example.com".to_string()],
        );
        assert_eq!(to, vec!["good@example.com"]);
        assert_eq!(cc, None);
        assert_eq!(bcc, None);
    }

    #[test]
    fn filter_suppressed_recipients_matches_case_and_whitespace_insensitively() {
        // `suppressed_among` returns normalized (trimmed/lowercased) forms
        // from the DB — the filter must normalize candidates the same way,
        // not compare raw strings.
        let (to, _, _) = filter_suppressed_recipients(
            vec![
                "  Bad@Example.COM  ".to_string(),
                "good@example.com".to_string(),
            ],
            None,
            None,
            &["bad@example.com".to_string()],
        );
        assert_eq!(to, vec!["good@example.com"]);
    }

    #[test]
    fn filter_suppressed_recipients_empties_to_when_all_suppressed() {
        let (to, _, _) = filter_suppressed_recipients(
            vec!["bad@example.com".to_string()],
            None,
            None,
            &["bad@example.com".to_string()],
        );
        assert!(to.is_empty());
    }

    #[test]
    fn test_list_emails_options_builder() {
        // Test that list options can be constructed with various filters
        let options = ListEmailsOptions {
            domain_id: Some(1),
            project_id: Some(100),
            status: Some("sent".to_string()),
            from_address: Some("sender@example.com".to_string()),
            page: Some(2),
            page_size: Some(50),
        };

        assert_eq!(options.domain_id, Some(1));
        assert_eq!(options.project_id, Some(100));
        assert_eq!(options.status, Some("sent".to_string()));
        assert_eq!(options.page, Some(2));
        assert_eq!(options.page_size, Some(50));
    }

    #[test]
    fn test_email_stats_struct() {
        // Test EmailStats struct construction
        let stats = EmailStats {
            total: 100,
            sent: 70,
            failed: 10,
            queued: 5,
            captured: 15,
        };

        assert_eq!(stats.total, 100);
        assert_eq!(stats.sent, 70);
        assert_eq!(stats.failed, 10);
        assert_eq!(stats.queued, 5);
        assert_eq!(stats.captured, 15);
        // Verify counts add up
        assert_eq!(
            stats.sent + stats.failed + stats.queued + stats.captured,
            stats.total
        );
    }

    #[test]
    fn test_page_size_clamping() {
        // Test that page size is clamped to max 100
        let options = ListEmailsOptions {
            domain_id: None,
            project_id: None,
            status: None,
            from_address: None,
            page: Some(1),
            page_size: Some(200), // Exceeds max
        };

        // The clamping happens in the list() method, not here
        // but we test the options struct accepts any value
        assert_eq!(options.page_size, Some(200));
    }

    #[test]
    fn test_invalid_from_address_no_at() {
        let from = "invalid-email-no-at";
        let domain = from.split('@').nth(1);
        assert!(domain.is_none());
    }

    #[test]
    fn test_from_address_with_subdomain() {
        let from = "sender@mail.example.com";
        let domain = from.split('@').nth(1);
        assert_eq!(domain, Some("mail.example.com"));
    }

    // ============================================
    // Integration Tests (Require Docker)
    // ============================================

    #[tokio::test]
    async fn test_list_emails_empty() {
        let (_db, email_service, _provider_service, _domain_service) = setup_test_env().await;

        let options = ListEmailsOptions::default();
        let (emails, total) = email_service.list(options).await.unwrap();

        assert!(emails.is_empty());
        assert_eq!(total, 0);
    }

    #[tokio::test]
    async fn test_get_email_not_found() {
        let (_db, email_service, _provider_service, _domain_service) = setup_test_env().await;

        let email_id = Uuid::new_v4();
        let result = email_service.get(email_id).await;

        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), EmailError::EmailNotFound(_)));
    }

    #[tokio::test]
    async fn test_count_by_status_empty() {
        let (_db, email_service, _provider_service, _domain_service) = setup_test_env().await;

        let stats = email_service.count_by_status(None).await.unwrap();

        assert_eq!(stats.total, 0);
        assert_eq!(stats.sent, 0);
        assert_eq!(stats.failed, 0);
        assert_eq!(stats.queued, 0);
        assert_eq!(stats.captured, 0);
    }

    #[tokio::test]
    async fn test_send_email_domain_not_verified() {
        let (db, email_service, provider_service, _domain_service) = setup_test_env().await;

        // Create a provider and domain (domain will be in pending status by default)
        let provider = create_test_provider(&provider_service).await;
        let _domain = create_test_domain(&db.db, provider.id, "test-pending.example.com").await;

        // Try to send email - should be captured because domain is not verified
        let request = SendEmailRequest {
            from: "sender@test-pending.example.com".to_string(),
            from_name: None,
            to: vec!["recipient@test.com".to_string()],
            cc: None,
            bcc: None,
            reply_to: None,
            subject: "Test".to_string(),
            html: Some("<p>Test</p>".to_string()),
            text: None,
            headers: None,
            tags: None,
            track_opens: false,
            track_clicks: false,
        };

        let result = email_service.send(request).await;

        // Email should be captured (not an error), since domain exists but is not verified
        assert!(result.is_ok());
        let response = result.unwrap();
        assert_eq!(response.status, "captured");
    }

    #[tokio::test]
    async fn test_send_email_no_domain_configured() {
        let (_db, email_service, _provider_service, _domain_service) = setup_test_env().await;

        // Try to send email from a domain that doesn't exist - should be captured
        let request = SendEmailRequest {
            from: "sender@unconfigured-domain.com".to_string(),
            from_name: None,
            to: vec!["recipient@test.com".to_string()],
            cc: None,
            bcc: None,
            reply_to: None,
            subject: "Test".to_string(),
            html: Some("<p>Test</p>".to_string()),
            text: None,
            headers: None,
            tags: None,
            track_opens: false,
            track_clicks: false,
        };

        let result = email_service.send(request).await;

        // Email should be captured (Mailhog mode), not an error
        assert!(result.is_ok());
        let response = result.unwrap();
        assert_eq!(response.status, "captured");
    }

    #[tokio::test]
    async fn test_list_emails_with_filters() {
        let (_db, email_service, _provider_service, _domain_service) = setup_test_env().await;

        // Test filtering by domain_id
        let options = ListEmailsOptions {
            domain_id: Some(999),
            ..Default::default()
        };
        let (emails, total) = email_service.list(options).await.unwrap();

        assert!(emails.is_empty());
        assert_eq!(total, 0);

        // Test filtering by status
        let options = ListEmailsOptions {
            status: Some("sent".to_string()),
            ..Default::default()
        };
        let (emails, total) = email_service.list(options).await.unwrap();

        assert!(emails.is_empty());
        assert_eq!(total, 0);
    }
}
