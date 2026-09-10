// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Email service for sending and managing emails

use chrono::Utc;
use sea_orm::{
    sea_query::Expr, ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection,
    EntityTrait, FromQueryResult, PaginatorTrait, QueryFilter, QueryOrder, QuerySelect,
    TransactionTrait,
};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::Arc;
use temps_entities::{email_idempotency_keys, emails};
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::errors::EmailError;
use crate::providers::{EmailProvider, SendEmailRequest as ProviderSendRequest};
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

enum IdempotencyDecision {
    New,
    Return(SendEmailResponse),
    Resume(Box<emails::Model>),
}

enum DeliveryClaim {
    Acquired(chrono::DateTime<Utc>),
    Return(SendEmailResponse),
}

/// Result of a provider call as observed by Temps.
///
/// A timeout is deliberately distinct from a provider rejection. Once the
/// request has left this process, a timeout cannot prove the provider rejected
/// it; retrying that ambiguous result could deliver the same email twice.
enum ProviderDeliveryOutcome {
    Accepted(crate::providers::SendEmailResponse),
    Rejected(EmailError),
    Unknown(String),
}

#[derive(Debug, FromQueryResult)]
struct EmailStatusCount {
    status: String,
    count: i64,
}

fn classify_provider_result(
    result: Result<crate::providers::SendEmailResponse, EmailError>,
) -> ProviderDeliveryOutcome {
    match result {
        Ok(response) => ProviderDeliveryOutcome::Accepted(response),
        Err(EmailError::ProviderDeliveryUnknown(reason)) => {
            ProviderDeliveryOutcome::Unknown(reason)
        }
        Err(error) => ProviderDeliveryOutcome::Rejected(error),
    }
}

// Keep this shorter than the Cloud outbox retry horizon. A process crash after
// the claim commit must become recoverable before the caller exhausts its
// retries, while still preventing concurrent provider calls.
const DELIVERY_LEASE_SECONDS: i64 = 60;
const PROVIDER_SEND_TIMEOUT_SECONDS: u64 = 20;

/// Hard ceiling on the entire retry sequence: first attempt + 500 ms delay +
/// second attempt.  Worst case: 2 × 20 s + 0.5 s ≈ 40.5 s, well inside this
/// cap.  The fenced delivery path leases the claim for DELIVERY_LEASE_SECONDS
/// (60 s); 45 s stays safely below that so a timed-out retry cannot let
/// another worker steal the claim mid-send.
const RETRY_OUTER_TIMEOUT_SECS: u64 = 45;

/// Attempt `provider.send(request)` up to two times when the first attempt
/// yields a retryable rejection.
///
/// # Safety invariant
///
/// `ProviderDeliveryUnknown` is **never** retried.  Once a request has left
/// the process, a transport failure cannot prove the provider did not accept
/// it; retrying that outcome risks delivering the same email twice.
///
/// # Timeout budget
///
/// Each attempt is individually wrapped in `PROVIDER_SEND_TIMEOUT_SECONDS`
/// (20 s) for defense in depth.  The entire sequence — both attempts and the
/// 500 ms inter-attempt delay — is further bounded by `RETRY_OUTER_TIMEOUT_SECS`
/// (45 s).  If the outer deadline fires we return `Unknown` because we cannot
/// know whether an in-flight attempt was accepted.
///
/// Returns the final outcome and the number of provider calls actually made.
async fn send_with_retry(
    provider: &dyn crate::providers::EmailProvider,
    request: &crate::providers::SendEmailRequest,
) -> (ProviderDeliveryOutcome, u32) {
    let inner = async {
        // ── Attempt 1 ───────────────────────────────────────────────────────
        let attempt1 = tokio::time::timeout(
            std::time::Duration::from_secs(PROVIDER_SEND_TIMEOUT_SECONDS),
            provider.send(request),
        )
        .await;

        let first_result = match attempt1 {
            Ok(result) => result,
            Err(_elapsed) => {
                return (
                    ProviderDeliveryOutcome::Unknown(format!(
                        "Provider did not return within {PROVIDER_SEND_TIMEOUT_SECONDS}s (attempt 1)"
                    )),
                    1u32,
                );
            }
        };

        match first_result {
            Ok(response) => return (ProviderDeliveryOutcome::Accepted(response), 1u32),
            Err(EmailError::ProviderDeliveryUnknown(reason)) => {
                // Ambiguous outcome — never retry.
                return (ProviderDeliveryOutcome::Unknown(reason), 1u32);
            }
            Err(ref err)
                if matches!(
                    err,
                    EmailError::SendFailed {
                        retryable: true,
                        ..
                    }
                ) =>
            {
                // Retryable throttle — fall through to attempt 2.
            }
            Err(err) => {
                // Non-retryable rejection.
                return (ProviderDeliveryOutcome::Rejected(err), 1u32);
            }
        }

        // ── 500 ms back-off ─────────────────────────────────────────────────
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // ── Attempt 2 ───────────────────────────────────────────────────────
        let attempt2 = tokio::time::timeout(
            std::time::Duration::from_secs(PROVIDER_SEND_TIMEOUT_SECONDS),
            provider.send(request),
        )
        .await;

        let second_result = match attempt2 {
            Ok(result) => result,
            Err(_elapsed) => {
                return (
                    ProviderDeliveryOutcome::Unknown(format!(
                        "Provider did not return within {PROVIDER_SEND_TIMEOUT_SECONDS}s (attempt 2)"
                    )),
                    2u32,
                );
            }
        };

        (classify_provider_result(second_result), 2u32)
    };

    match tokio::time::timeout(
        std::time::Duration::from_secs(RETRY_OUTER_TIMEOUT_SECS),
        inner,
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(_elapsed) => (
            ProviderDeliveryOutcome::Unknown(format!(
                "Provider retry sequence did not complete within {RETRY_OUTER_TIMEOUT_SECS}s"
            )),
            // We know at least one attempt was started when the outer deadline
            // fired, but we cannot determine how many completed.
            1u32,
        ),
    }
}

/// Validate caller-supplied headers before any authorization or provider work.
///
/// SMTP headers such as `From`, `To`, and `Bcc` are security-sensitive: lettre
/// derives the delivery envelope from them, so allowing callers to replace
/// those values after sender and suppression checks would bypass both checks.
/// Only the Temps-owned `X-Temps-*` metadata namespace is accepted. Generic
/// `X-*` headers are not safe: SMTP gateways such as SendGrid and Mailgun use
/// provider-specific `X-*` headers to change routing and recipient behavior.
fn validate_custom_headers(
    headers: Option<&std::collections::HashMap<String, String>>,
) -> Result<(), EmailError> {
    let Some(headers) = headers else {
        return Ok(());
    };

    for (name, value) in headers {
        let valid_name = name.len() > "X-Temps-".len()
            && name
                .get(.."X-Temps-".len())
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("X-Temps-"))
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-');
        if !valid_name {
            return Err(EmailError::Validation(
                "A custom email header is not allowed; only X-Temps-* metadata headers are accepted"
                    .to_string(),
            ));
        }
        if value.bytes().any(|byte| byte == b'\r' || byte == b'\n') {
            return Err(EmailError::Validation(
                "A custom email header contains a line break".to_string(),
            ));
        }
    }

    Ok(())
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
        self.send_with_context(request, None).await
    }

    /// Send from a deployment token's project scope.
    ///
    /// The domain must have been explicitly authorized for the project, and a
    /// durable idempotency claim is committed before the provider is called.
    pub async fn send_for_project(
        &self,
        request: SendEmailRequest,
        project_id: i32,
        idempotency_key: String,
    ) -> Result<SendEmailResponse, EmailError> {
        self.send_with_context(request, Some((project_id, idempotency_key)))
            .await
    }

    async fn send_with_context(
        &self,
        request: SendEmailRequest,
        project_context: Option<(i32, String)>,
    ) -> Result<SendEmailResponse, EmailError> {
        debug!("Sending email from {} to {:?}", request.from, request.to);

        validate_custom_headers(request.headers.as_ref())?;

        // Extract domain from 'from' address
        let from_domain = request
            .from
            .split('@')
            .nth(1)
            .ok_or_else(|| EmailError::Validation("Invalid from address".to_string()))?;

        // Look up domain by extracted domain name
        let domain = self.domain_service.find_by_domain_name(from_domain).await?;

        if let Some((project_id, _)) = project_context.as_ref() {
            let Some(domain) = domain.as_ref() else {
                return Err(EmailError::DomainNotAuthorized {
                    domain: from_domain.to_string(),
                    project_id: *project_id,
                });
            };
            if !self
                .domain_service
                .is_authorized_for_project(domain.id, *project_id)
                .await?
            {
                return Err(EmailError::DomainNotAuthorized {
                    domain: domain.domain.clone(),
                    project_id: *project_id,
                });
            }
        }

        let payload_hash = project_context
            .as_ref()
            .map(|_| email_payload_hash(&request));
        let resume_email = if let (Some((project_id, idempotency_key)), Some(payload_hash)) =
            (project_context.as_ref(), payload_hash.as_ref())
        {
            match self
                .idempotency_decision(*project_id, idempotency_key, payload_hash)
                .await?
            {
                IdempotencyDecision::Return(existing) => return Ok(existing),
                IdempotencyDecision::Resume(email) => Some(*email),
                IdempotencyDecision::New => None,
            }
        } else {
            None
        };

        // A stale lease resumes with the original stable ID. At-least-once
        // delivery is deliberate: a crash after the provider accepted the
        // email but before status persistence can produce one duplicate, but
        // it cannot silently lose an operational alert forever.
        let email_id = resume_email
            .as_ref()
            .map(|email| email.id)
            .unwrap_or_else(Uuid::new_v4);

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
            project_id: Set(project_context.as_ref().map(|(project_id, _)| *project_id)),
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

        let email_model = if let Some(email) = resume_email {
            email
        } else if let (Some((project_id, idempotency_key)), Some(payload_hash)) =
            (project_context.as_ref(), payload_hash.as_ref())
        {
            let transaction = self.db.begin().await?;
            let email_model = email.insert(&transaction).await?;
            let claim = email_idempotency_keys::ActiveModel {
                project_id: Set(*project_id),
                idempotency_key: Set(idempotency_key.clone()),
                payload_hash: Set(payload_hash.clone()),
                email_id: Set(email_id),
                lease_expires_at: Set(
                    Utc::now() + chrono::Duration::seconds(DELIVERY_LEASE_SECONDS)
                ),
                ..Default::default()
            };

            if let Err(insert_error) = claim.insert(&transaction).await {
                transaction.rollback().await?;
                match self
                    .idempotency_decision(*project_id, idempotency_key, payload_hash)
                    .await?
                {
                    IdempotencyDecision::Return(existing) => return Ok(existing),
                    IdempotencyDecision::Resume(email) => *email,
                    IdempotencyDecision::New => return Err(insert_error.into()),
                }
            } else {
                transaction.commit().await?;
                email_model
            }
        } else {
            email.insert(self.db.as_ref()).await?
        };

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

        // An inactive provider must never be used for delivery.  Treat it
        // exactly like "no provider configured" so callers get a predictable
        // `captured` state rather than a mysterious send failure.
        if !provider.is_active {
            info!(
                "Provider {} is inactive for domain '{}', capturing email without sending",
                provider.id, domain.domain
            );
            let mut active_model: emails::ActiveModel = email_model.into();
            active_model.status = Set("captured".to_string());
            active_model.error_message =
                Set(Some(format!("Email provider {} is inactive", provider.id)));
            active_model.sent_at = Set(Some(Utc::now()));
            active_model.update(self.db.as_ref()).await?;

            return Ok(SendEmailResponse {
                id: email_id,
                status: "captured".to_string(),
                provider_message_id: None,
            });
        }

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

        if project_context.is_some() {
            return self
                .send_project_email_with_fence(
                    email_id,
                    provider_instance.as_ref(),
                    Some(provider.id),
                    &provider_request,
                )
                .await;
        }

        let (outcome, attempt_count) =
            send_with_retry(provider_instance.as_ref(), &provider_request).await;
        self.finalize_unfenced_delivery(
            email_model,
            outcome,
            Some(provider.id),
            Some(attempt_count as i32),
        )
        .await
    }

    async fn finalize_unfenced_delivery(
        &self,
        email_model: emails::Model,
        outcome: ProviderDeliveryOutcome,
        provider_id: Option<i32>,
        attempt_count: Option<i32>,
    ) -> Result<SendEmailResponse, EmailError> {
        let email_id = email_model.id;
        match outcome {
            ProviderDeliveryOutcome::Accepted(response) => {
                // Update email with success status
                let mut active_model: emails::ActiveModel = email_model.clone().into();
                active_model.status = Set("sent".to_string());
                active_model.provider_message_id = Set(Some(response.message_id.clone()));
                active_model.sent_at = Set(Some(Utc::now()));
                active_model.provider_id = Set(provider_id);
                active_model.attempt_count = Set(attempt_count);

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
            ProviderDeliveryOutcome::Rejected(e) => {
                // Provider send failed - capture email instead of failing
                info!(
                    "Failed to send email via provider, capturing instead: {}",
                    e
                );

                let mut active_model: emails::ActiveModel = email_model.into();
                active_model.status = Set("captured".to_string());
                active_model.error_message = Set(Some(format!("Send failed: {}", e)));
                active_model.sent_at = Set(Some(Utc::now()));
                active_model.provider_id = Set(provider_id);
                active_model.attempt_count = Set(attempt_count);

                active_model.update(self.db.as_ref()).await?;

                Ok(SendEmailResponse {
                    id: email_id,
                    status: "captured".to_string(),
                    provider_message_id: None,
                })
            }
            ProviderDeliveryOutcome::Unknown(reason) => {
                warn!(
                    email_id = %email_id,
                    reason = %reason,
                    "Provider delivery result is unknown; refusing automatic or implied retry"
                );

                let mut active_model: emails::ActiveModel = email_model.into();
                active_model.status = Set("delivery_unknown".to_string());
                active_model.error_message = Set(Some(reason));
                active_model.sent_at = Set(Some(Utc::now()));
                active_model.provider_id = Set(provider_id);
                active_model.attempt_count = Set(attempt_count);
                active_model.update(self.db.as_ref()).await?;

                Ok(SendEmailResponse {
                    id: email_id,
                    status: "delivery_unknown".to_string(),
                    provider_message_id: None,
                })
            }
        }
    }

    /// Deliver one project-scoped email with a durable, fenced attempt.
    ///
    /// Claiming and finalization use short row-lock transactions. The provider
    /// call happens after the claim transaction commits, so slow or malicious
    /// providers cannot consume a database-pool connection for their whole
    /// timeout. The lease timestamp is also the fencing token: a stale attempt
    /// cannot overwrite a newer retry's state.
    async fn send_project_email_with_fence(
        &self,
        email_id: Uuid,
        provider: &dyn EmailProvider,
        provider_id: Option<i32>,
        request: &ProviderSendRequest,
    ) -> Result<SendEmailResponse, EmailError> {
        let attempt_token = match self.claim_project_email_delivery(email_id).await? {
            DeliveryClaim::Acquired(token) => token,
            DeliveryClaim::Return(response) => return Ok(response),
        };

        let (provider_outcome, attempt_count) = send_with_retry(provider, request).await;

        self.finalize_project_email_delivery(
            email_id,
            attempt_token,
            provider_outcome,
            provider_id,
            Some(attempt_count as i32),
        )
        .await
    }

    /// Atomically move a retryable email into `sending` and return a freshly
    /// renewed lease as its fencing token. No external I/O occurs in this
    /// short transaction.
    async fn claim_project_email_delivery(
        &self,
        email_id: Uuid,
    ) -> Result<DeliveryClaim, EmailError> {
        let transaction = self.db.begin().await?;
        let claim = email_idempotency_keys::Entity::find()
            .filter(email_idempotency_keys::Column::EmailId.eq(email_id))
            .lock_exclusive()
            .one(&transaction)
            .await?
            .ok_or_else(|| {
                EmailError::EmailNotFound(format!("delivery claim for project email {email_id}"))
            })?;

        let current = emails::Entity::find_by_id(email_id)
            .lock_exclusive()
            .one(&transaction)
            .await?
            .ok_or_else(|| EmailError::EmailNotFound(email_id.to_string()))?;

        if !email_delivery_is_retryable(&current) {
            transaction.commit().await?;
            return Ok(DeliveryClaim::Return(SendEmailResponse {
                id: current.id,
                status: current.status,
                provider_message_id: current.provider_message_id,
            }));
        }

        // Refresh the token at the last possible moment before provider I/O.
        // The original lease may have been consumed by HTML rewriting,
        // suppression checks, or provider setup; this guarantees the entire
        // provider timeout fits inside the new attempt's fence window.
        let attempt_token = Utc::now() + chrono::Duration::seconds(DELIVERY_LEASE_SECONDS);
        let mut active_claim: email_idempotency_keys::ActiveModel = claim.into();
        active_claim.lease_expires_at = Set(attempt_token);
        // PostgreSQL stores TIMESTAMPTZ at microsecond precision. Use the value
        // returned by PostgreSQL as the fence token so finalization never
        // compares it with a higher-precision in-memory timestamp.
        let persisted_claim = active_claim.update(&transaction).await?;
        let attempt_token = persisted_claim.lease_expires_at;

        let mut active: emails::ActiveModel = current.into();
        active.status = Set("sending".to_string());
        active.update(&transaction).await?;
        transaction.commit().await?;

        Ok(DeliveryClaim::Acquired(attempt_token))
    }

    /// Persist a provider result only when the attempt still owns the lease it
    /// claimed. A newer retry changes the lease before sending, fencing stale
    /// completions out of the state transition.
    async fn finalize_project_email_delivery(
        &self,
        email_id: Uuid,
        attempt_token: chrono::DateTime<Utc>,
        provider_outcome: ProviderDeliveryOutcome,
        provider_id: Option<i32>,
        attempt_count: Option<i32>,
    ) -> Result<SendEmailResponse, EmailError> {
        let transaction = self.db.begin().await?;
        let claim = email_idempotency_keys::Entity::find()
            .filter(email_idempotency_keys::Column::EmailId.eq(email_id))
            .lock_exclusive()
            .one(&transaction)
            .await?
            .ok_or_else(|| {
                EmailError::EmailNotFound(format!("delivery claim for project email {email_id}"))
            })?;
        let current = emails::Entity::find_by_id(email_id)
            .lock_exclusive()
            .one(&transaction)
            .await?
            .ok_or_else(|| EmailError::EmailNotFound(email_id.to_string()))?;

        if claim.lease_expires_at != attempt_token || current.status != "sending" {
            transaction.commit().await?;
            return Ok(SendEmailResponse {
                id: current.id,
                status: current.status,
                provider_message_id: current.provider_message_id,
            });
        }

        let response = match provider_outcome {
            ProviderDeliveryOutcome::Accepted(provider_response) => {
                let mut active: emails::ActiveModel = current.into();
                active.status = Set("sent".to_string());
                active.provider_message_id = Set(Some(provider_response.message_id.clone()));
                active.sent_at = Set(Some(Utc::now()));
                active.provider_id = Set(provider_id);
                active.attempt_count = Set(attempt_count);
                active.update(&transaction).await?;

                SendEmailResponse {
                    id: email_id,
                    status: "sent".to_string(),
                    provider_message_id: Some(provider_response.message_id),
                }
            }
            ProviderDeliveryOutcome::Rejected(error) => {
                info!(
                    email_id = %email_id,
                    error = %error,
                    "Project-scoped provider delivery failed; capturing for retry"
                );
                let mut active: emails::ActiveModel = current.into();
                active.status = Set("captured".to_string());
                active.error_message = Set(Some(format!("Send failed: {error}")));
                active.sent_at = Set(Some(Utc::now()));
                active.provider_id = Set(provider_id);
                active.attempt_count = Set(attempt_count);
                active.update(&transaction).await?;

                SendEmailResponse {
                    id: email_id,
                    status: "captured".to_string(),
                    provider_message_id: None,
                }
            }
            ProviderDeliveryOutcome::Unknown(reason) => {
                warn!(
                    email_id = %email_id,
                    "Project-scoped provider delivery resulted in an unknown outcome; automatic retry disabled"
                );
                let message = format!(
                    "Provider outcome unknown ({reason}); automatic retry disabled to avoid duplicate delivery"
                );
                let mut active: emails::ActiveModel = current.into();
                active.status = Set("delivery_unknown".to_string());
                active.error_message = Set(Some(message));
                active.sent_at = Set(Some(Utc::now()));
                active.provider_id = Set(provider_id);
                active.attempt_count = Set(attempt_count);
                active.update(&transaction).await?;

                SendEmailResponse {
                    id: email_id,
                    status: "delivery_unknown".to_string(),
                    provider_message_id: None,
                }
            }
        };

        transaction.commit().await?;
        Ok(response)
    }

    async fn idempotency_decision(
        &self,
        project_id: i32,
        idempotency_key: &str,
        payload_hash: &str,
    ) -> Result<IdempotencyDecision, EmailError> {
        let Some(claim) =
            email_idempotency_keys::Entity::find_by_id((project_id, idempotency_key.to_string()))
                .one(self.db.as_ref())
                .await?
        else {
            return Ok(IdempotencyDecision::New);
        };

        if claim.payload_hash != payload_hash {
            return Err(EmailError::IdempotencyConflict {
                key: idempotency_key.to_string(),
            });
        }

        let email = emails::Entity::find_by_id(claim.email_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                EmailError::EmailNotFound(format!(
                    "idempotency claim for project {project_id} references {}",
                    claim.email_id
                ))
            })?;
        let now = Utc::now();
        let stale_sending_attempt = email.status == "sending" && claim.lease_expires_at <= now;
        if !email_delivery_is_retryable(&email) && !stale_sending_attempt {
            return Ok(IdempotencyDecision::Return(SendEmailResponse {
                id: email.id,
                status: email.status,
                provider_message_id: email.provider_message_id,
            }));
        }

        let new_lease = now + chrono::Duration::seconds(DELIVERY_LEASE_SECONDS);
        let transaction = self.db.begin().await?;
        let acquired = email_idempotency_keys::Entity::update_many()
            .col_expr(
                email_idempotency_keys::Column::LeaseExpiresAt,
                Expr::value(new_lease),
            )
            .filter(email_idempotency_keys::Column::ProjectId.eq(project_id))
            .filter(email_idempotency_keys::Column::IdempotencyKey.eq(idempotency_key.to_string()))
            .filter(email_idempotency_keys::Column::LeaseExpiresAt.lte(now))
            .exec(&transaction)
            .await?
            .rows_affected
            == 1;

        if acquired {
            let mut email = email;
            if email.status == "sending" {
                let reset = emails::Entity::update_many()
                    .col_expr(emails::Column::Status, Expr::value("queued"))
                    .filter(emails::Column::Id.eq(email.id))
                    .filter(emails::Column::Status.eq("sending"))
                    .exec(&transaction)
                    .await?;
                if reset.rows_affected == 1 {
                    email.status = "queued".to_string();
                }
            }
            transaction.commit().await?;
            Ok(IdempotencyDecision::Resume(Box::new(email)))
        } else {
            transaction.commit().await?;
            Ok(IdempotencyDecision::Return(SendEmailResponse {
                id: email.id,
                status: email.status,
                provider_message_id: email.provider_message_id,
            }))
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

        let grouped = base_query
            .select_only()
            .column(emails::Column::Status)
            .column_as(emails::Column::Id.count(), "count")
            .group_by(emails::Column::Status)
            .into_model::<EmailStatusCount>()
            .all(self.db.as_ref())
            .await?;

        let mut counts = BTreeMap::new();
        let mut total = 0_u64;
        for row in grouped {
            let count = u64::try_from(row.count).map_err(|_| {
                EmailError::Database(sea_orm::DbErr::Custom(format!(
                    "Email status aggregate for '{}' returned a negative count: {}",
                    row.status, row.count
                )))
            })?;
            total = total.saturating_add(count);
            counts.insert(row.status, count);
        }

        let count = |status: &str| counts.get(status).copied().unwrap_or_default();
        let sent = count("sent");
        let failed = count("failed");
        let queued = count("queued");
        let captured = count("captured");
        let sending = count("sending");
        let delivery_unknown = count("delivery_unknown");

        Ok(EmailStats {
            total,
            sent,
            failed,
            queued,
            captured,
            sending,
            delivery_unknown,
        })
    }
}

fn email_delivery_is_retryable(email: &emails::Model) -> bool {
    delivery_status_is_retryable(&email.status, email.error_message.as_deref())
}

fn delivery_status_is_retryable(status: &str, error_message: Option<&str>) -> bool {
    status == "queued"
        || (status == "captured"
            && !error_message.is_some_and(|message| message.starts_with("Recipient(s) suppressed")))
}

fn email_payload_hash(request: &SendEmailRequest) -> String {
    let headers = request.headers.as_ref().map(|headers| {
        headers
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<BTreeMap<_, _>>()
    });
    let payload = serde_json::json!({
        "from": request.from,
        "from_name": request.from_name,
        "to": request.to,
        "cc": request.cc,
        "bcc": request.bcc,
        "reply_to": request.reply_to,
        "subject": request.subject,
        "html": request.html,
        "text": request.text,
        "headers": headers,
        "tags": request.tags,
        "track_opens": request.track_opens,
        "track_clicks": request.track_clicks,
    });
    let mut hasher = Sha256::new();
    hasher.update(payload.to_string().as_bytes());
    hex::encode(hasher.finalize())
}

/// Email statistics
#[derive(Debug, Clone)]
pub struct EmailStats {
    pub total: u64,
    pub sent: u64,
    pub failed: u64,
    pub queued: u64,
    pub captured: u64,
    pub sending: u64,
    pub delivery_unknown: u64,
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

    #[test]
    fn ambiguous_provider_transport_error_is_never_retryable() {
        let outcome = classify_provider_result(Err(EmailError::ProviderDeliveryUnknown(
            "connection closed while reading provider response".to_string(),
        )));

        assert!(matches!(
            outcome,
            ProviderDeliveryOutcome::Unknown(reason)
                if reason.contains("connection closed while reading provider response")
        ));
    }
    use crate::providers::{EmailProviderType, MockEmailProvider, MockSendResult, SesCredentials};
    use crate::services::provider_service::{CreateProviderRequest, ProviderCredentials};
    use crate::services::TrackingService;
    use temps_core::EncryptionService;
    use temps_database::test_utils::TestDatabase;

    fn test_provider_send_request() -> ProviderSendRequest {
        ProviderSendRequest {
            from: "sender@example.test".to_string(),
            from_name: None,
            to: vec!["recipient@example.test".to_string()],
            cc: None,
            bcc: None,
            reply_to: None,
            subject: "retry unit test".to_string(),
            html: None,
            text: Some("body".to_string()),
            headers: None,
        }
    }

    // ── send_with_retry unit tests (no database needed) ─────────────────────

    #[tokio::test]
    async fn send_with_retry_success_on_first_attempt_returns_one_attempt() {
        let provider = MockEmailProvider::new();
        let request = test_provider_send_request();

        let (outcome, attempts) = send_with_retry(&provider, &request).await;

        assert!(matches!(outcome, ProviderDeliveryOutcome::Accepted(_)));
        assert_eq!(attempts, 1, "no retry needed when first attempt succeeds");
    }

    #[tokio::test]
    async fn send_with_retry_non_retryable_rejection_stops_after_one_attempt() {
        let provider = MockEmailProvider::new()
            .with_scripted_responses([MockSendResult::Fail { retryable: false }]);
        let request = test_provider_send_request();

        let (outcome, attempts) = send_with_retry(&provider, &request).await;

        assert!(
            matches!(outcome, ProviderDeliveryOutcome::Rejected(_)),
            "non-retryable rejection must not retry"
        );
        assert_eq!(
            attempts, 1,
            "non-retryable rejection must not make a second call"
        );
        assert_eq!(provider.send_call_count(), 1);
    }

    #[tokio::test]
    async fn send_with_retry_unknown_outcome_stops_immediately_without_retry() {
        let provider = MockEmailProvider::new().with_scripted_responses([MockSendResult::Unknown]);
        let request = test_provider_send_request();

        let (outcome, attempts) = send_with_retry(&provider, &request).await;

        assert!(
            matches!(outcome, ProviderDeliveryOutcome::Unknown(_)),
            "unknown outcome must never be retried"
        );
        assert_eq!(
            attempts, 1,
            "ProviderDeliveryUnknown must abort after one attempt"
        );
        assert_eq!(provider.send_call_count(), 1);
    }

    #[tokio::test]
    async fn send_with_retry_retryable_failure_then_success_returns_two_attempts_and_accepted() {
        let provider = MockEmailProvider::new().with_scripted_responses([
            MockSendResult::Fail { retryable: true },
            MockSendResult::Succeed,
        ]);
        let request = test_provider_send_request();

        let (outcome, attempts) = send_with_retry(&provider, &request).await;

        assert!(
            matches!(outcome, ProviderDeliveryOutcome::Accepted(_)),
            "second attempt succeeds"
        );
        assert_eq!(attempts, 2, "retry path must record both attempts");
        assert_eq!(provider.send_call_count(), 2);
    }

    #[tokio::test]
    async fn send_with_retry_two_retryable_failures_returns_two_attempts_and_rejected() {
        let provider = MockEmailProvider::new().with_scripted_responses([
            MockSendResult::Fail { retryable: true },
            MockSendResult::Fail { retryable: true },
        ]);
        let request = test_provider_send_request();

        let (outcome, attempts) = send_with_retry(&provider, &request).await;

        assert!(
            matches!(outcome, ProviderDeliveryOutcome::Rejected(_)),
            "exhausted retries must yield a final Rejected outcome"
        );
        assert_eq!(attempts, 2, "both attempts must be counted");
        assert_eq!(provider.send_call_count(), 2);
    }

    fn test_send_request(subject: &str) -> SendEmailRequest {
        SendEmailRequest {
            from: "sender@test-pending.example.com".to_string(),
            from_name: Some("Test Sender".to_string()),
            to: vec!["recipient@test.com".to_string()],
            cc: None,
            bcc: None,
            reply_to: None,
            subject: subject.to_string(),
            html: Some("<p>Test</p>".to_string()),
            text: Some("Test".to_string()),
            headers: None,
            tags: None,
            track_opens: false,
            track_clicks: false,
        }
    }

    // Helper to create a test encryption service
    fn create_test_encryption_service() -> Arc<EncryptionService> {
        let key = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        Arc::new(EncryptionService::new(key).unwrap())
    }

    // Helper to setup test environment with real database
    async fn setup_test_env() -> Option<(TestDatabase, EmailService, ProviderService, DomainService)>
    {
        let db = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(error) => {
                if temps_database::test_utils::is_container_runtime_unavailable(&error.to_string())
                {
                    eprintln!("Skipping Docker-dependent email test: {error}");
                    return None;
                }
                panic!("Email test database or migrations failed: {error}");
            }
        };
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
            docker_extra_networks: Vec::new(),
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
        Some((db, email_service, provider_service, domain_service))
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
                "X-Temps-Custom".to_string(),
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
            sending: 0,
            delivery_unknown: 0,
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
            sending: 0,
            delivery_unknown: 0,
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

    #[test]
    fn idempotency_hash_is_stable_across_header_insertion_order() {
        let mut first = test_send_request("same payload");
        first.headers = Some(std::collections::HashMap::from([
            ("X-Temps-B".to_string(), "2".to_string()),
            ("X-Temps-A".to_string(), "1".to_string()),
        ]));
        let mut second = test_send_request("same payload");
        second.headers = Some(std::collections::HashMap::from([
            ("X-Temps-A".to_string(), "1".to_string()),
            ("X-Temps-B".to_string(), "2".to_string()),
        ]));

        assert_eq!(email_payload_hash(&first), email_payload_hash(&second));
    }

    #[test]
    fn custom_headers_reject_envelope_and_routing_overrides() {
        for protected in [
            "From",
            "to",
            "CC",
            "bcc",
            "Reply-To",
            "Subject",
            "X-SMTPAPI",
            "X-Mailgun-Recipient-Variables",
            "X-SES-CONFIGURATION-SET",
        ] {
            let headers = std::collections::HashMap::from([(
                protected.to_string(),
                "attacker@example.test".to_string(),
            )]);
            assert!(matches!(
                validate_custom_headers(Some(&headers)),
                Err(EmailError::Validation(_))
            ));
        }
    }

    #[test]
    fn custom_headers_allow_only_single_line_x_metadata() {
        let accepted = std::collections::HashMap::from([
            ("X-Temps-Trace-Id".to_string(), "trace-123".to_string()),
            ("x-temps-campaign".to_string(), "welcome".to_string()),
        ]);
        assert!(validate_custom_headers(Some(&accepted)).is_ok());

        let newline = std::collections::HashMap::from([(
            "X-Temps-Trace-Id".to_string(),
            "trace-123\r\nBcc: attacker@example.test".to_string(),
        )]);
        assert!(matches!(
            validate_custom_headers(Some(&newline)),
            Err(EmailError::Validation(_))
        ));
    }

    #[test]
    fn ambiguous_provider_outcome_is_never_retryable() {
        assert!(!delivery_status_is_retryable("delivery_unknown", None));
        assert!(delivery_status_is_retryable("queued", None));
        assert!(delivery_status_is_retryable(
            "captured",
            Some("Send failed: provider rejected request")
        ));
    }

    // ============================================
    // Integration Tests (Require Docker)
    // ============================================

    #[tokio::test]
    async fn test_list_emails_empty() {
        let Some((_db, email_service, _provider_service, _domain_service)) = setup_test_env().await
        else {
            return;
        };

        let options = ListEmailsOptions::default();
        let (emails, total) = email_service.list(options).await.unwrap();

        assert!(emails.is_empty());
        assert_eq!(total, 0);
    }

    #[tokio::test]
    async fn test_get_email_not_found() {
        let Some((_db, email_service, _provider_service, _domain_service)) = setup_test_env().await
        else {
            return;
        };

        let email_id = Uuid::new_v4();
        let result = email_service.get(email_id).await;

        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), EmailError::EmailNotFound(_)));
    }

    #[tokio::test]
    async fn test_count_by_status_empty() {
        let Some((_db, email_service, _provider_service, _domain_service)) = setup_test_env().await
        else {
            return;
        };

        let stats = email_service.count_by_status(None).await.unwrap();

        assert_eq!(stats.total, 0);
        assert_eq!(stats.sent, 0);
        assert_eq!(stats.failed, 0);
        assert_eq!(stats.queued, 0);
        assert_eq!(stats.captured, 0);
        assert_eq!(stats.sending, 0);
        assert_eq!(stats.delivery_unknown, 0);
    }

    #[tokio::test]
    async fn unfenced_delivery_ambiguity_is_persisted_as_terminal() {
        let Some((db, email_service, _provider_service, _domain_service)) = setup_test_env().await
        else {
            return;
        };

        let email = emails::ActiveModel {
            id: Set(Uuid::new_v4()),
            domain_id: Set(None),
            project_id: Set(None),
            from_address: Set("sender@example.test".to_string()),
            to_addresses: Set(serde_json::json!(["recipient@example.test"])),
            subject: Set("ambiguous unfenced delivery".to_string()),
            status: Set("queued".to_string()),
            track_opens: Set(false),
            track_clicks: Set(false),
            open_count: Set(0),
            click_count: Set(0),
            ..Default::default()
        }
        .insert(db.db.as_ref())
        .await
        .unwrap();

        let response = email_service
            .finalize_unfenced_delivery(
                email.clone(),
                ProviderDeliveryOutcome::Unknown(
                    "provider disconnected after request upload".to_string(),
                ),
                None,
                None,
            )
            .await
            .unwrap();

        assert_eq!(response.status, "delivery_unknown");
        let persisted = emails::Entity::find_by_id(email.id)
            .one(db.db.as_ref())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(persisted.status, "delivery_unknown");
        assert!(persisted
            .error_message
            .as_deref()
            .is_some_and(|reason| reason.contains("disconnected")));
    }

    #[tokio::test]
    async fn count_by_status_uses_one_consistent_grouped_snapshot_and_domain_filter() {
        let Some((db, email_service, provider_service, _domain_service)) = setup_test_env().await
        else {
            return;
        };
        let provider = create_test_provider(&provider_service).await;
        let first_domain = create_test_domain(
            &db.db,
            provider.id,
            &format!("stats-a-{}.example.test", Uuid::new_v4()),
        )
        .await;
        let second_domain = create_test_domain(
            &db.db,
            provider.id,
            &format!("stats-b-{}.example.test", Uuid::new_v4()),
        )
        .await;

        for status in [
            "sent",
            "failed",
            "queued",
            "captured",
            "sending",
            "delivery_unknown",
        ] {
            emails::ActiveModel {
                id: Set(Uuid::new_v4()),
                domain_id: Set(Some(first_domain.id)),
                project_id: Set(None),
                from_address: Set(format!("sender@{}", first_domain.domain)),
                to_addresses: Set(serde_json::json!(["recipient@example.test"])),
                subject: Set(format!("status {status}")),
                status: Set(status.to_string()),
                track_opens: Set(false),
                track_clicks: Set(false),
                open_count: Set(0),
                click_count: Set(0),
                ..Default::default()
            }
            .insert(db.db.as_ref())
            .await
            .unwrap();
        }
        emails::ActiveModel {
            id: Set(Uuid::new_v4()),
            domain_id: Set(Some(second_domain.id)),
            project_id: Set(None),
            from_address: Set(format!("sender@{}", second_domain.domain)),
            to_addresses: Set(serde_json::json!(["recipient@example.test"])),
            subject: Set("other domain".to_string()),
            status: Set("sent".to_string()),
            track_opens: Set(false),
            track_clicks: Set(false),
            open_count: Set(0),
            click_count: Set(0),
            ..Default::default()
        }
        .insert(db.db.as_ref())
        .await
        .unwrap();

        let scoped = email_service
            .count_by_status(Some(first_domain.id))
            .await
            .unwrap();
        assert_eq!(scoped.total, 6);
        assert_eq!(scoped.sent, 1);
        assert_eq!(scoped.failed, 1);
        assert_eq!(scoped.queued, 1);
        assert_eq!(scoped.captured, 1);
        assert_eq!(scoped.sending, 1);
        assert_eq!(scoped.delivery_unknown, 1);

        let all = email_service.count_by_status(None).await.unwrap();
        assert_eq!(all.total, 7);
        assert_eq!(all.sent, 2);
    }

    #[tokio::test]
    async fn test_send_email_domain_not_verified() {
        let Some((db, email_service, provider_service, _domain_service)) = setup_test_env().await
        else {
            return;
        };

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
    async fn deployment_send_requires_domain_grant_and_replays_without_a_second_email() {
        use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

        let Some((db, email_service, provider_service, domain_service)) = setup_test_env().await
        else {
            return;
        };
        let provider = create_test_provider(&provider_service).await;
        let domain = create_test_domain(&db.db, provider.id, "test-pending.example.com").await;
        let project_slug = format!("email-project-{}", Uuid::new_v4());
        let project_row = db
            .db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                format!(
                    "INSERT INTO projects \
                     (name,repo_name,repo_owner,directory,main_branch,preset,created_at,updated_at,slug) \
                     VALUES ('Email Test','email-test','tests','.','main','python',now(),now(),'{project_slug}') \
                     RETURNING id"
                ),
            ))
            .await
            .unwrap()
            .unwrap();
        let project_id: i32 = project_row.try_get("", "id").unwrap();

        let denied = email_service
            .send_for_project(test_send_request("first"), project_id, "delivery-1".into())
            .await;
        assert!(matches!(
            denied,
            Err(EmailError::DomainNotAuthorized { .. })
        ));

        domain_service
            .authorize_project(domain.id, project_id)
            .await
            .unwrap();
        let first = email_service
            .send_for_project(test_send_request("first"), project_id, "delivery-1".into())
            .await
            .unwrap();
        let replay = email_service
            .send_for_project(test_send_request("first"), project_id, "delivery-1".into())
            .await
            .unwrap();
        assert_eq!(first.id, replay.id);
        assert_eq!(first.status, "captured");

        // `captured` is retryable for deployment sends unless the recipient is
        // permanently suppressed. Once the short delivery lease expires, the
        // same request resumes the same email instead of being suppressed
        // forever by its idempotency claim.
        email_idempotency_keys::Entity::update_many()
            .col_expr(
                email_idempotency_keys::Column::LeaseExpiresAt,
                Expr::value(Utc::now() - chrono::Duration::seconds(1)),
            )
            .filter(email_idempotency_keys::Column::ProjectId.eq(project_id))
            .filter(email_idempotency_keys::Column::IdempotencyKey.eq("delivery-1"))
            .exec(db.db.as_ref())
            .await
            .unwrap();
        let resumed = email_service
            .send_for_project(test_send_request("first"), project_id, "delivery-1".into())
            .await
            .unwrap();
        assert_eq!(first.id, resumed.id);
        assert_eq!(resumed.status, "captured");

        let email_count = emails::Entity::find()
            .filter(emails::Column::ProjectId.eq(project_id))
            .count(db.db.as_ref())
            .await
            .unwrap();
        assert_eq!(email_count, 1, "an HTTP retry must not enqueue twice");

        let conflict = email_service
            .send_for_project(
                test_send_request("different"),
                project_id,
                "delivery-1".into(),
            )
            .await;
        assert!(matches!(
            conflict,
            Err(EmailError::IdempotencyConflict { .. })
        ));
    }

    #[tokio::test]
    async fn stale_delivery_lease_resumes_after_crash_without_creating_a_second_email() {
        use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

        let Some((db, email_service, provider_service, domain_service)) = setup_test_env().await
        else {
            return;
        };
        let provider = create_test_provider(&provider_service).await;
        let domain = create_test_domain(&db.db, provider.id, "test-pending.example.com").await;
        let project_slug = format!("email-resume-{}", Uuid::new_v4());
        let project_row = db
            .db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                format!(
                    "INSERT INTO projects \
                     (name,repo_name,repo_owner,directory,main_branch,preset,created_at,updated_at,slug) \
                     VALUES ('Email Resume','email-resume','tests','.','main','python',now(),now(),'{project_slug}') \
                     RETURNING id"
                ),
            ))
            .await
            .unwrap()
            .unwrap();
        let project_id: i32 = project_row.try_get("", "id").unwrap();
        domain_service
            .authorize_project(domain.id, project_id)
            .await
            .unwrap();

        let request = test_send_request("resume after crash");
        let email_id = Uuid::new_v4();
        emails::ActiveModel {
            id: Set(email_id),
            domain_id: Set(Some(domain.id)),
            project_id: Set(Some(project_id)),
            from_address: Set(request.from.clone()),
            from_name: Set(request.from_name.clone()),
            to_addresses: Set(serde_json::to_value(&request.to).unwrap()),
            subject: Set(request.subject.clone()),
            html_body: Set(request.html.clone()),
            text_body: Set(request.text.clone()),
            status: Set("sending".to_string()),
            track_opens: Set(false),
            track_clicks: Set(false),
            open_count: Set(0),
            click_count: Set(0),
            ..Default::default()
        }
        .insert(db.db.as_ref())
        .await
        .unwrap();
        email_idempotency_keys::ActiveModel {
            project_id: Set(project_id),
            idempotency_key: Set("crashed-delivery".to_string()),
            payload_hash: Set(email_payload_hash(&request)),
            email_id: Set(email_id),
            lease_expires_at: Set(Utc::now() - chrono::Duration::seconds(1)),
            ..Default::default()
        }
        .insert(db.db.as_ref())
        .await
        .unwrap();

        let resumed = email_service
            .send_for_project(request, project_id, "crashed-delivery".into())
            .await
            .unwrap();

        assert_eq!(resumed.id, email_id);
        assert_eq!(resumed.status, "captured");
        assert_eq!(
            emails::Entity::find()
                .filter(emails::Column::ProjectId.eq(project_id))
                .count(db.db.as_ref())
                .await
                .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn concurrent_project_delivery_is_fenced_to_one_provider_call() {
        use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

        let Some((db, email_service, _provider_service, _domain_service)) = setup_test_env().await
        else {
            return;
        };

        let project_slug = format!("email-fence-{}", Uuid::new_v4());
        let project_row = db
            .db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                format!(
                    "INSERT INTO projects \
                     (name,repo_name,repo_owner,directory,main_branch,preset,created_at,updated_at,slug) \
                     VALUES ('Email Fence','email-fence','tests','.','main','python',now(),now(),'{project_slug}') \
                     RETURNING id"
                ),
            ))
            .await
            .unwrap()
            .unwrap();
        let project_id: i32 = project_row.try_get("", "id").unwrap();
        let email_id = Uuid::new_v4();
        emails::ActiveModel {
            id: Set(email_id),
            domain_id: Set(None),
            project_id: Set(Some(project_id)),
            from_address: Set("sender@example.test".to_string()),
            to_addresses: Set(serde_json::json!(["recipient@example.test"])),
            subject: Set("fenced delivery".to_string()),
            status: Set("queued".to_string()),
            track_opens: Set(false),
            track_clicks: Set(false),
            open_count: Set(0),
            click_count: Set(0),
            ..Default::default()
        }
        .insert(db.db.as_ref())
        .await
        .unwrap();
        email_idempotency_keys::ActiveModel {
            project_id: Set(project_id),
            idempotency_key: Set("fenced-delivery".to_string()),
            payload_hash: Set("fixture-payload-hash".to_string()),
            email_id: Set(email_id),
            lease_expires_at: Set(Utc::now() - chrono::Duration::seconds(1)),
            ..Default::default()
        }
        .insert(db.db.as_ref())
        .await
        .unwrap();

        let provider =
            MockEmailProvider::new().with_send_delay(std::time::Duration::from_millis(200));
        let request = ProviderSendRequest {
            from: "sender@example.test".to_string(),
            from_name: None,
            to: vec!["recipient@example.test".to_string()],
            cc: None,
            bcc: None,
            reply_to: None,
            subject: "fenced delivery".to_string(),
            html: None,
            text: Some("body".to_string()),
            headers: None,
        };

        let first =
            email_service.send_project_email_with_fence(email_id, &provider, None, &request);
        let second = async {
            while provider.send_call_count() == 0 {
                tokio::task::yield_now().await;
            }
            email_service
                .send_project_email_with_fence(email_id, &provider, None, &request)
                .await
        };
        let (first, second) = tokio::join!(first, second);

        assert_eq!(first.unwrap().status, "sent");
        assert!(matches!(
            second.unwrap().status.as_str(),
            "sending" | "sent"
        ));
        assert_eq!(
            provider.send_call_count(),
            1,
            "a concurrent retry must not call the provider twice"
        );
    }

    #[tokio::test]
    async fn ambiguous_provider_timeout_is_terminal_to_prevent_duplicate_delivery() {
        use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

        let Some((db, email_service, _provider_service, _domain_service)) = setup_test_env().await
        else {
            return;
        };

        let project_slug = format!("email-timeout-{}", Uuid::new_v4());
        let project_row = db
            .db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                format!(
                    "INSERT INTO projects \
                     (name,repo_name,repo_owner,directory,main_branch,preset,created_at,updated_at,slug) \
                     VALUES ('Email Timeout','email-timeout','tests','.','main','python',now(),now(),'{project_slug}') \
                     RETURNING id"
                ),
            ))
            .await
            .unwrap()
            .unwrap();
        let project_id: i32 = project_row.try_get("", "id").unwrap();
        let email_id = Uuid::new_v4();
        emails::ActiveModel {
            id: Set(email_id),
            project_id: Set(Some(project_id)),
            from_address: Set("sender@example.test".to_string()),
            to_addresses: Set(serde_json::json!(["recipient@example.test"])),
            subject: Set("ambiguous provider timeout".to_string()),
            status: Set("queued".to_string()),
            track_opens: Set(false),
            track_clicks: Set(false),
            open_count: Set(0),
            click_count: Set(0),
            ..Default::default()
        }
        .insert(db.db.as_ref())
        .await
        .unwrap();
        email_idempotency_keys::ActiveModel {
            project_id: Set(project_id),
            idempotency_key: Set("ambiguous-timeout".to_string()),
            payload_hash: Set("fixture-payload-hash".to_string()),
            email_id: Set(email_id),
            lease_expires_at: Set(Utc::now() - chrono::Duration::seconds(1)),
            ..Default::default()
        }
        .insert(db.db.as_ref())
        .await
        .unwrap();

        let attempt_token = match email_service
            .claim_project_email_delivery(email_id)
            .await
            .unwrap()
        {
            DeliveryClaim::Acquired(token) => token,
            DeliveryClaim::Return(_) => panic!("new queued delivery must acquire its lease"),
        };
        let response = email_service
            .finalize_project_email_delivery(
                email_id,
                attempt_token,
                ProviderDeliveryOutcome::Unknown("test timeout".to_string()),
                None,
                None,
            )
            .await
            .unwrap();
        assert_eq!(response.status, "delivery_unknown");

        let stored = emails::Entity::find_by_id(email_id)
            .one(db.db.as_ref())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.status, "delivery_unknown");
        assert!(stored
            .error_message
            .as_deref()
            .is_some_and(|message| message.contains("avoid duplicate delivery")));
        assert!(!email_delivery_is_retryable(&stored));

        let replay = email_service
            .idempotency_decision(project_id, "ambiguous-timeout", "fixture-payload-hash")
            .await
            .unwrap();
        assert!(matches!(replay, IdempotencyDecision::Return(_)));
    }

    #[tokio::test]
    async fn test_send_email_no_domain_configured() {
        let Some((_db, email_service, _provider_service, _domain_service)) = setup_test_env().await
        else {
            return;
        };

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
        let Some((_db, email_service, _provider_service, _domain_service)) = setup_test_env().await
        else {
            return;
        };

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

    /// Emails addressed to a domain whose provider is inactive must be
    /// captured without any provider call.
    #[tokio::test]
    async fn inactive_provider_captures_without_calling_send() {
        use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

        let Some((db, email_service, provider_service, _domain_service)) = setup_test_env().await
        else {
            return;
        };

        // Create a provider and immediately deactivate it.
        let provider = create_test_provider(&provider_service).await;
        db.db
            .execute(Statement::from_string(
                DatabaseBackend::Postgres,
                format!(
                    "UPDATE email_providers SET is_active = false WHERE id = {}",
                    provider.id
                ),
            ))
            .await
            .unwrap();

        // Create a verified domain pointing at the inactive provider.
        let domain_name = format!("inactive-provider-{}.example.test", uuid::Uuid::new_v4());
        let domain = create_test_domain(&db.db, provider.id, &domain_name).await;
        db.db
            .execute(Statement::from_string(
                DatabaseBackend::Postgres,
                format!(
                    "UPDATE email_domains SET status = 'verified' WHERE id = {}",
                    domain.id
                ),
            ))
            .await
            .unwrap();

        let request = SendEmailRequest {
            from: format!("sender@{domain_name}"),
            from_name: None,
            to: vec!["recipient@example.test".to_string()],
            cc: None,
            bcc: None,
            reply_to: None,
            subject: "inactive provider test".to_string(),
            html: None,
            text: Some("body".to_string()),
            headers: None,
            tags: None,
            track_opens: false,
            track_clicks: false,
        };

        let response = email_service.send(request).await.unwrap();
        assert_eq!(
            response.status, "captured",
            "emails must be captured when the provider is inactive"
        );
    }

    /// A retryable error on the first attempt followed by success on the second
    /// must record `status = sent` and `attempt_count = 2`.
    #[tokio::test]
    async fn retryable_error_then_success_records_attempt_count_two() {
        use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

        let Some((db, email_service, provider_service, _domain_service)) = setup_test_env().await
        else {
            return;
        };

        // We test send_with_retry directly (unit style, no real provider I/O)
        // and then verify the persisted attempt_count via finalize_unfenced_delivery.
        let provider = create_test_provider(&provider_service).await;
        let domain_name = format!("retry-success-{}.example.test", uuid::Uuid::new_v4());
        let domain = create_test_domain(&db.db, provider.id, &domain_name).await;
        db.db
            .execute(Statement::from_string(
                DatabaseBackend::Postgres,
                format!(
                    "UPDATE email_domains SET status = 'verified' WHERE id = {}",
                    domain.id
                ),
            ))
            .await
            .unwrap();

        // Insert an email row directly so we can call finalize_unfenced_delivery.
        let email_id = uuid::Uuid::new_v4();
        emails::ActiveModel {
            id: Set(email_id),
            from_address: Set(format!("sender@{domain_name}")),
            to_addresses: Set(serde_json::json!(["recipient@example.test"])),
            subject: Set("retry attempt count test".to_string()),
            status: Set("queued".to_string()),
            track_opens: Set(false),
            track_clicks: Set(false),
            open_count: Set(0),
            click_count: Set(0),
            ..Default::default()
        }
        .insert(db.db.as_ref())
        .await
        .unwrap();

        let email_model = emails::Entity::find_by_id(email_id)
            .one(db.db.as_ref())
            .await
            .unwrap()
            .unwrap();

        // Use a scripted mock so we don't need a live provider.
        let mock = MockEmailProvider::new().with_scripted_responses([
            MockSendResult::Fail { retryable: true },
            MockSendResult::Succeed,
        ]);
        let (outcome, attempt_count) = send_with_retry(&mock, &test_provider_send_request()).await;
        assert!(matches!(outcome, ProviderDeliveryOutcome::Accepted(_)));
        assert_eq!(attempt_count, 2);

        email_service
            .finalize_unfenced_delivery(
                email_model,
                outcome,
                Some(provider.id),
                Some(attempt_count as i32),
            )
            .await
            .unwrap();

        let stored = emails::Entity::find_by_id(email_id)
            .one(db.db.as_ref())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.status, "sent");
        assert_eq!(stored.attempt_count, Some(2));
        assert_eq!(stored.provider_id, Some(provider.id));
    }

    /// A non-retryable rejection on the first attempt must record
    /// `status = captured` and `attempt_count = 1`.
    #[tokio::test]
    async fn non_retryable_rejection_records_attempt_count_one() {
        use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

        let Some((db, email_service, provider_service, _domain_service)) = setup_test_env().await
        else {
            return;
        };

        let provider = create_test_provider(&provider_service).await;
        let domain_name = format!("non-retry-reject-{}.example.test", uuid::Uuid::new_v4());
        let domain = create_test_domain(&db.db, provider.id, &domain_name).await;
        db.db
            .execute(Statement::from_string(
                DatabaseBackend::Postgres,
                format!(
                    "UPDATE email_domains SET status = 'verified' WHERE id = {}",
                    domain.id
                ),
            ))
            .await
            .unwrap();

        let email_id = uuid::Uuid::new_v4();
        emails::ActiveModel {
            id: Set(email_id),
            from_address: Set(format!("sender@{domain_name}")),
            to_addresses: Set(serde_json::json!(["recipient@example.test"])),
            subject: Set("non-retryable rejection test".to_string()),
            status: Set("queued".to_string()),
            track_opens: Set(false),
            track_clicks: Set(false),
            open_count: Set(0),
            click_count: Set(0),
            ..Default::default()
        }
        .insert(db.db.as_ref())
        .await
        .unwrap();

        let email_model = emails::Entity::find_by_id(email_id)
            .one(db.db.as_ref())
            .await
            .unwrap()
            .unwrap();

        let mock = MockEmailProvider::new()
            .with_scripted_responses([MockSendResult::Fail { retryable: false }]);
        let (outcome, attempt_count) = send_with_retry(&mock, &test_provider_send_request()).await;
        assert!(matches!(outcome, ProviderDeliveryOutcome::Rejected(_)));
        assert_eq!(attempt_count, 1);

        email_service
            .finalize_unfenced_delivery(
                email_model,
                outcome,
                Some(provider.id),
                Some(attempt_count as i32),
            )
            .await
            .unwrap();

        let stored = emails::Entity::find_by_id(email_id)
            .one(db.db.as_ref())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.status, "captured");
        assert_eq!(stored.attempt_count, Some(1));
        assert_eq!(stored.provider_id, Some(provider.id));
    }

    /// `ProviderDeliveryUnknown` must produce `delivery_unknown` and
    /// `attempt_count = 1`; the provider must not be called twice.
    #[tokio::test]
    async fn delivery_unknown_records_attempt_count_one_and_is_not_retried() {
        use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

        let Some((db, email_service, provider_service, _domain_service)) = setup_test_env().await
        else {
            return;
        };

        let provider = create_test_provider(&provider_service).await;
        let domain_name = format!("unknown-outcome-{}.example.test", uuid::Uuid::new_v4());
        let domain = create_test_domain(&db.db, provider.id, &domain_name).await;
        db.db
            .execute(Statement::from_string(
                DatabaseBackend::Postgres,
                format!(
                    "UPDATE email_domains SET status = 'verified' WHERE id = {}",
                    domain.id
                ),
            ))
            .await
            .unwrap();

        let email_id = uuid::Uuid::new_v4();
        emails::ActiveModel {
            id: Set(email_id),
            from_address: Set(format!("sender@{domain_name}")),
            to_addresses: Set(serde_json::json!(["recipient@example.test"])),
            subject: Set("delivery unknown test".to_string()),
            status: Set("queued".to_string()),
            track_opens: Set(false),
            track_clicks: Set(false),
            open_count: Set(0),
            click_count: Set(0),
            ..Default::default()
        }
        .insert(db.db.as_ref())
        .await
        .unwrap();

        let email_model = emails::Entity::find_by_id(email_id)
            .one(db.db.as_ref())
            .await
            .unwrap()
            .unwrap();

        let mock = MockEmailProvider::new().with_scripted_responses([MockSendResult::Unknown]);
        let (outcome, attempt_count) = send_with_retry(&mock, &test_provider_send_request()).await;
        assert!(matches!(outcome, ProviderDeliveryOutcome::Unknown(_)));
        assert_eq!(attempt_count, 1);
        assert_eq!(
            mock.send_call_count(),
            1,
            "unknown outcome must never retry"
        );

        email_service
            .finalize_unfenced_delivery(
                email_model,
                outcome,
                Some(provider.id),
                Some(attempt_count as i32),
            )
            .await
            .unwrap();

        let stored = emails::Entity::find_by_id(email_id)
            .one(db.db.as_ref())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.status, "delivery_unknown");
        assert_eq!(stored.attempt_count, Some(1));
    }
}
