// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Email service for sending and managing emails

use chrono::{SubsecRound, Utc};
use sea_orm::{
    sea_query::Expr, ActiveModelTrait, ActiveValue::Set, ColumnTrait, Condition,
    DatabaseConnection, EntityTrait, FromQueryResult, PaginatorTrait, QueryFilter, QueryOrder,
    QuerySelect, TransactionTrait,
};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::Arc;
use temps_core::retry::RetryConfig;
use temps_entities::{email_idempotency_keys, emails};
use tracing::{debug, error, info, warn};
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
#[derive(Clone)]
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
/// Attempts at saving a provider result before reporting it as unrecorded.
const OUTCOME_WRITE_ATTEMPTS: u32 = 3;

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
        self.send_with_context(request, None, None).await
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
        self.send_with_context(request, Some((project_id, idempotency_key)), None)
            .await
    }

    /// Deliver a captured email again, in place.
    ///
    /// Reuses the stored email (same ID, so tracking, history and stats stay
    /// attached) and re-runs the normal pipeline, so the domain, project
    /// authorization, suppression and provider checks all apply again. Only
    /// `captured` emails qualify — never `delivery_unknown`, whose provider
    /// may already have accepted it, and never a capture caused by suppressed
    /// recipients.
    ///
    /// Project emails go through their idempotency claim and the fenced
    /// delivery path, so a console resend cannot race the application's own
    /// retry with the same Idempotency-Key. Other emails are claimed with an
    /// atomic `captured -> queued` transition, so concurrent resends deliver
    /// at most once (see `resend_without_project`).
    pub async fn resend(&self, email_id: Uuid) -> Result<SendEmailResponse, EmailError> {
        let email = self.get(email_id).await?;
        ensure_resendable(&email, Utc::now())?;
        let request = stored_email_request(&email)?;
        // An email recorded before custom headers were restricted can carry
        // one the send pipeline now rejects; every resend of it would fail
        // the same way, so refuse it before claiming the email.
        validate_custom_headers(request.headers.as_ref()).map_err(|error| {
            EmailError::NotResendable {
                email_id,
                status: email.status.clone(),
                reason: format!("its stored headers can no longer be sent: {error}"),
            }
        })?;

        // A capture taken before the sender domain existed in Temps has no
        // domain; attach it now so the delivery counts towards that domain.
        if email.domain_id.is_none() {
            if let Some((_, from_domain)) = email.from_address.rsplit_once('@') {
                if let Some(domain) = self.domain_service.find_by_domain_name(from_domain).await? {
                    emails::Entity::update_many()
                        .col_expr(emails::Column::DomainId, Expr::value(domain.id))
                        .filter(emails::Column::Id.eq(email_id))
                        .filter(emails::Column::DomainId.is_null())
                        .exec(self.db.as_ref())
                        .await?;
                }
            }
        }

        let Some(project_id) = email.project_id else {
            return self.resend_without_project(email, request).await;
        };

        let claim = email_idempotency_keys::Entity::find()
            .filter(email_idempotency_keys::Column::EmailId.eq(email_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| EmailError::NotResendable {
                email_id,
                status: email.status.clone(),
                reason: format!("project {project_id} has no delivery claim for it"),
            })?;
        if claim.lease_expires_at > Utc::now() {
            return Err(EmailError::NotResendable {
                email_id,
                status: email.status.clone(),
                reason: "its last delivery attempt is less than a minute old; try again shortly"
                    .to_string(),
            });
        }
        info!(email_id = %email_id, project_id, "Resending captured project email");
        self.send_with_context(request, Some((project_id, claim.idempotency_key)), None)
            .await
    }

    /// Claim and deliver an email that has no project, so no idempotency
    /// claim to fence it.
    ///
    /// The claim is an atomic `captured -> queued` transition that stamps
    /// `sent_at`, and that pair is the attempt's fencing token (see
    /// `ResendClaim`): every later write of the attempt is conditional on it.
    /// Right before the provider is called the claim moves to `sending` under
    /// a fresh token (`begin_resend_delivery`).
    ///
    /// A claim left `queued` by a crash, restart or stall can be taken over
    /// once it is older than the delivery lease (see `ensure_resendable`).
    /// That is safe because a `queued` claim has never reached the provider,
    /// and an attempt that lost its claim can neither call the provider nor
    /// overwrite what the newer attempt recorded. A claim left `sending` is
    /// never taken over: the provider may already have accepted the email.
    ///
    /// Delivery runs on a detached task, so a client that disconnects
    /// mid-request does not cancel it halfway through, and an attempt that
    /// fails before the provider was contacted, or whose rejection by the
    /// provider could not be saved, puts the email back to `captured`
    /// instead of leaving it stuck (see `release_failed_resend`).
    async fn resend_without_project(
        &self,
        email: emails::Model,
        request: SendEmailRequest,
    ) -> Result<SendEmailResponse, EmailError> {
        let email_id = email.id;
        let now = Utc::now();
        let claim = ResendClaim {
            status: "queued",
            token: fence_token(now),
        };
        let claimed = emails::Entity::update_many()
            .col_expr(emails::Column::Status, Expr::value(claim.status))
            .col_expr(
                emails::Column::ErrorMessage,
                Expr::value(Option::<String>::None),
            )
            .col_expr(emails::Column::SentAt, Expr::value(claim.token))
            .filter(emails::Column::Id.eq(email_id))
            .filter(emails::Column::ProjectId.is_null())
            .filter(
                Condition::any()
                    .add(
                        Condition::all()
                            .add(emails::Column::Status.eq("captured"))
                            .add(
                                Condition::any()
                                    .add(emails::Column::ErrorMessage.is_null())
                                    .add(
                                        emails::Column::ErrorMessage
                                            .not_like(format!("{SUPPRESSED_RECIPIENTS_ERROR}%")),
                                    ),
                            ),
                    )
                    .add(
                        Condition::all()
                            .add(emails::Column::Status.eq("queued"))
                            .add(emails::Column::SentAt.lt(stale_resend_cutoff(now))),
                    ),
            )
            .exec(self.db.as_ref())
            .await?
            .rows_affected
            == 1;
        if !claimed {
            return Err(self.resend_claim_lost(email_id).await);
        }
        info!(email_id = %email_id, "Resending captured email");

        let service = self.clone();
        let original_error = email.error_message;
        let delivery = tokio::spawn(async move {
            let attempt = ResendAttempt::new(claim);
            let result = match service.get(email_id).await {
                Ok(queued) => {
                    service
                        .send_with_context(request, None, Some((queued, &attempt)))
                        .await
                }
                Err(error) => Err(error),
            };
            if let Err(error) = &result {
                service
                    .release_failed_resend(email_id, &attempt, original_error, error)
                    .await;
            }
            result
        });
        delivery
            .await
            .map_err(|join_error| EmailError::ResendInterrupted {
                email_id,
                reason: join_error.to_string(),
            })?
    }

    /// Return a resend that failed without recording an outcome to
    /// `captured`, so it can be resent again. There are two such failures:
    ///
    /// - It never reached the provider. The email goes back under the reason
    ///   it was captured with, and only the attempt's own `queued` claim is
    ///   touched.
    /// - The provider definitively rejected it, and saving that failed. The
    ///   email goes back under the rejection, through the `sending` claim the
    ///   attempt took before calling the provider.
    ///
    /// A recorded outcome and a claim another resend took over are never
    /// overwritten. Neither is a resend the provider accepted, or may have:
    /// that is not an undelivered capture, and making it resendable would
    /// invite a second delivery.
    async fn release_failed_resend(
        &self,
        email_id: Uuid,
        attempt: &ResendAttempt,
        original_error: Option<String>,
        error: &EmailError,
    ) {
        let (claim, error_message) = match error {
            EmailError::DeliveryOutcomeNotRecorded { .. } => {
                error!(
                    email_id = %email_id,
                    error = %error,
                    "Resend reached the provider but its result was not recorded; keeping the \
                     claim instead of returning the email to captured"
                );
                return;
            }
            EmailError::DeliveryRejectionNotRecorded { rejection, .. } => {
                let Some(sending) = attempt.sending.get() else {
                    return;
                };
                (*sending, format!("Send failed: {rejection}"))
            }
            _ => (
                attempt.queued,
                original_error.unwrap_or_else(|| format!("Resend failed: {error}")),
            ),
        };
        warn!(email_id = %email_id, error = %error, "Resend failed; returning email to captured");
        let release = RetryConfig::new(OUTCOME_WRITE_ATTEMPTS)
            .with_base_delay(std::time::Duration::from_millis(200))
            .with_max_delay(std::time::Duration::from_secs(1))
            .retry(|| {
                emails::Entity::update_many()
                    .col_expr(emails::Column::Status, Expr::value("captured"))
                    .col_expr(
                        emails::Column::ErrorMessage,
                        Expr::value(error_message.clone()),
                    )
                    .filter(emails::Column::Id.eq(email_id))
                    .filter(emails::Column::Status.eq(claim.status))
                    .filter(emails::Column::SentAt.eq(claim.token))
                    .exec(self.db.as_ref())
            })
            .await;
        if let Err(release_error) = release {
            error!(
                email_id = %email_id,
                claim_status = claim.status,
                error = %release_error,
                "Failed to return email to captured after a failed resend"
            );
        }
    }

    /// Move a resend's claim to `sending` under a fresh token, immediately
    /// before the provider is called. An attempt whose claim lapsed and was
    /// taken over fails here, so it never reaches the provider, and the fresh
    /// token dates the provider call rather than the work that preceded it.
    async fn begin_resend_delivery(
        &self,
        email_id: Uuid,
        claim: ResendClaim,
    ) -> Result<ResendClaim, EmailError> {
        let sending = ResendClaim {
            status: "sending",
            token: fence_token(Utc::now()),
        };
        let owned = emails::Entity::update_many()
            .col_expr(emails::Column::Status, Expr::value(sending.status))
            .col_expr(emails::Column::SentAt, Expr::value(sending.token))
            .filter(emails::Column::Id.eq(email_id))
            .filter(emails::Column::Status.eq(claim.status))
            .filter(emails::Column::SentAt.eq(claim.token))
            .exec(self.db.as_ref())
            .await?
            .rows_affected
            == 1;
        if !owned {
            return Err(self.resend_claim_lost(email_id).await);
        }
        Ok(sending)
    }

    /// The refusal for a resend that does not, or no longer, own the email.
    async fn resend_claim_lost(&self, email_id: Uuid) -> EmailError {
        match self.get(email_id).await {
            Ok(current) => EmailError::NotResendable {
                email_id,
                status: current.status,
                reason: "another delivery of this email started first".to_string(),
            },
            Err(error) => error,
        }
    }

    /// Write a state change of an email being delivered. A resend only
    /// writes while it still owns its claim; `Ok(false)` means it does not,
    /// and nothing was written.
    async fn try_save_delivery_state(
        &self,
        active_model: emails::ActiveModel,
        resend_claim: Option<ResendClaim>,
    ) -> Result<bool, sea_orm::DbErr> {
        let Some(claim) = resend_claim else {
            active_model.update(self.db.as_ref()).await?;
            return Ok(true);
        };
        match emails::Entity::update(active_model)
            .filter(emails::Column::Status.eq(claim.status))
            .filter(emails::Column::SentAt.eq(claim.token))
            .exec(self.db.as_ref())
            .await
        {
            Ok(_) => Ok(true),
            Err(sea_orm::DbErr::RecordNotUpdated) => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// Record why an email was captured instead of handed to the provider.
    async fn save_capture(
        &self,
        email_id: Uuid,
        active_model: emails::ActiveModel,
        resend_claim: Option<ResendClaim>,
    ) -> Result<(), EmailError> {
        if self
            .try_save_delivery_state(active_model, resend_claim)
            .await?
        {
            Ok(())
        } else {
            Err(self.resend_claim_lost(email_id).await)
        }
    }

    /// `resend_of` resumes an existing, already-claimed email (see `resend`)
    /// instead of recording a new one; it is only used without a project
    /// context, since project emails resume through their idempotency claim.
    async fn send_with_context(
        &self,
        request: SendEmailRequest,
        project_context: Option<(i32, String)>,
        resend_of: Option<(emails::Model, &ResendAttempt)>,
    ) -> Result<SendEmailResponse, EmailError> {
        let (resend_of, resend_attempt) = match resend_of {
            Some((email, attempt)) => (Some(email), Some(attempt)),
            None => (None, None),
        };
        let resend_claim = resend_attempt.map(|attempt| attempt.queued);
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
        let resume_email = if let Some(email) = resend_of {
            Some(email)
        } else if let (Some((project_id, idempotency_key)), Some(payload_hash)) =
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
            active_model.error_message = Set(Some(format!(
                "{SUPPRESSED_RECIPIENTS_ERROR} (previous hard bounce or complaint)"
            )));
            active_model.sent_at = Set(Some(Utc::now()));

            self.save_capture(email_id, active_model, resend_claim)
                .await?;

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

                self.save_capture(email_id, active_model, resend_claim)
                    .await?;

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

            self.save_capture(email_id, active_model, resend_claim)
                .await?;

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

            self.save_capture(email_id, active_model, resend_claim)
                .await?;

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
            self.save_capture(email_id, active_model, resend_claim)
                .await?;

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
                self.save_capture(email_id, active_model, resend_claim)
                    .await?;

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

        let resend_claim = match resend_attempt {
            Some(attempt) => {
                let sending = self.begin_resend_delivery(email_id, attempt.queued).await?;
                // Set once per attempt; a failed release must know it.
                let _ = attempt.sending.set(sending);
                Some(sending)
            }
            None => None,
        };

        let (outcome, attempt_count) =
            send_with_retry(provider_instance.as_ref(), &provider_request).await;
        self.finalize_unfenced_delivery(
            email_model,
            outcome,
            Some(provider.id),
            Some(attempt_count as i32),
            resend_claim,
        )
        .await
    }

    async fn finalize_unfenced_delivery(
        &self,
        email_model: emails::Model,
        outcome: ProviderDeliveryOutcome,
        provider_id: Option<i32>,
        attempt_count: Option<i32>,
        resend_claim: Option<ResendClaim>,
    ) -> Result<SendEmailResponse, EmailError> {
        let email_id = email_model.id;
        let recorded = match outcome {
            ProviderDeliveryOutcome::Accepted(response) => {
                // Update email with success status
                let mut active_model: emails::ActiveModel = email_model.clone().into();
                active_model.status = Set("sent".to_string());
                // A resent email may carry the error of its earlier capture.
                active_model.error_message = Set(None);
                active_model.provider_message_id = Set(Some(response.message_id.clone()));
                active_model.sent_at = Set(Some(Utc::now()));
                active_model.provider_id = Set(provider_id);
                active_model.attempt_count = Set(attempt_count);

                let saved = self
                    .record_delivery_outcome(active_model, resend_claim)
                    .await
                    .map_err(|db_error| {
                        error!(
                            email_id = %email_id,
                            provider_message_id = %response.message_id,
                            error = %db_error,
                            "Provider accepted the email but the result could not be recorded"
                        );
                        EmailError::DeliveryOutcomeNotRecorded {
                            email_id,
                            outcome: "sent".to_string(),
                            reason: db_error.to_string(),
                        }
                    })?;

                if saved {
                    info!(
                        "Email sent successfully, id: {}, provider_message_id: {}",
                        email_id, response.message_id
                    );
                }

                saved.then(|| SendEmailResponse {
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

                let saved = self
                    .record_delivery_outcome(active_model, resend_claim)
                    .await
                    .map_err(|db_error| EmailError::DeliveryRejectionNotRecorded {
                        email_id,
                        rejection: e.to_string(),
                        reason: db_error.to_string(),
                    })?;

                saved.then(|| SendEmailResponse {
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
                let saved = self
                    .record_delivery_outcome(active_model, resend_claim)
                    .await
                    .map_err(|db_error| EmailError::DeliveryOutcomeNotRecorded {
                        email_id,
                        outcome: "delivery_unknown".to_string(),
                        reason: db_error.to_string(),
                    })?;

                saved.then(|| SendEmailResponse {
                    id: email_id,
                    status: "delivery_unknown".to_string(),
                    provider_message_id: None,
                })
            }
        };
        if let Some(response) = recorded {
            return Ok(response);
        }

        // The resend no longer owned the email, so nothing was written:
        // report what the row says now instead of this attempt's result.
        warn!(
            email_id = %email_id,
            "Resend result discarded; the email is no longer owned by this attempt"
        );
        let current = self.get(email_id).await?;
        Ok(SendEmailResponse {
            id: current.id,
            status: current.status,
            provider_message_id: current.provider_message_id,
        })
    }

    /// Save the result of a provider call that has already happened. This
    /// write is the only record of that call, so a transient database failure
    /// is retried before giving up. `Ok(false)` means a resend no longer
    /// owned the email, and nothing was written.
    async fn record_delivery_outcome(
        &self,
        active_model: emails::ActiveModel,
        resend_claim: Option<ResendClaim>,
    ) -> Result<bool, sea_orm::DbErr> {
        RetryConfig::new(OUTCOME_WRITE_ATTEMPTS)
            .with_base_delay(std::time::Duration::from_millis(200))
            .with_max_delay(std::time::Duration::from_secs(1))
            .retry(|| self.try_save_delivery_state(active_model.clone(), resend_claim))
            .await
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
                // A retried or resent email may carry the error of its
                // earlier capture.
                active.error_message = Set(None);
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
            && !error_message
                .is_some_and(|message| message.starts_with(SUPPRESSED_RECIPIENTS_ERROR)))
}

/// Prefix of the error recorded when every recipient was suppressed.
const SUPPRESSED_RECIPIENTS_ERROR: &str = "Recipient(s) suppressed";

/// A non-project resend claim (`queued`, stamped with `sent_at`) older than
/// this was abandoned by a crash, restart or stall and may be taken over.
fn stale_resend_cutoff(now: chrono::DateTime<Utc>) -> chrono::DateTime<Utc> {
    now - chrono::Duration::seconds(DELIVERY_LEASE_SECONDS)
}

/// Ownership of an email being resent without a project. `token` is the
/// `sent_at` stamped together with `status`, and the pair is a fencing token:
/// every write of the resend is conditional on both, so an attempt whose
/// claim was taken over can neither deliver nor overwrite the newer attempt.
#[derive(Debug, Clone, Copy)]
struct ResendClaim {
    status: &'static str,
    token: chrono::DateTime<Utc>,
}

/// The claims one resend attempt holds: the `queued` one it starts with and,
/// once it is about to call the provider, the `sending` one that replaces it.
struct ResendAttempt {
    queued: ResendClaim,
    sending: std::sync::OnceLock<ResendClaim>,
}

impl ResendAttempt {
    fn new(queued: ResendClaim) -> Self {
        Self {
            queued,
            sending: std::sync::OnceLock::new(),
        }
    }
}

/// PostgreSQL stores TIMESTAMPTZ at microsecond precision; a token must
/// compare equal to its stored value.
fn fence_token(now: chrono::DateTime<Utc>) -> chrono::DateTime<Utc> {
    now.trunc_subsecs(6)
}

fn ensure_resendable(email: &emails::Model, now: chrono::DateTime<Utc>) -> Result<(), EmailError> {
    let reason = match email.status.as_str() {
        // A first send left `queued` has no `sent_at`; only an abandoned
        // resend claim of an email without a project carries one.
        "queued"
            if email.project_id.is_none()
                && email
                    .sent_at
                    .is_some_and(|started| started < stale_resend_cutoff(now)) =>
        {
            return Ok(())
        }
        "captured"
            if email
                .error_message
                .as_deref()
                .is_some_and(|message| message.starts_with(SUPPRESSED_RECIPIENTS_ERROR)) =>
        {
            "its recipients are suppressed after a hard bounce or complaint, and mailing them \
             again would hurt the sending domain's reputation"
        }
        "captured" => return Ok(()),
        "sent" => "it was already delivered",
        "delivery_unknown" => {
            "the provider may already have accepted it, so resending could deliver it twice"
        }
        // A resend that reached the provider and never recorded a result.
        "sending"
            if email.project_id.is_none()
                && email
                    .sent_at
                    .is_some_and(|started| started < stale_resend_cutoff(now)) =>
        {
            "its last delivery was interrupted after the provider was contacted, so the \
             provider may already have accepted it and resending could deliver it twice"
        }
        "queued" | "sending" => "it is still being delivered",
        _ => "only captured emails can be resent",
    };
    Err(EmailError::NotResendable {
        email_id: email.id,
        status: email.status.clone(),
        reason: reason.to_string(),
    })
}

/// Rebuild the send request an email was recorded from.
fn stored_email_request(email: &emails::Model) -> Result<SendEmailRequest, EmailError> {
    let addresses =
        |value: &serde_json::Value| serde_json::from_value::<Vec<String>>(value.clone());
    Ok(SendEmailRequest {
        from: email.from_address.clone(),
        from_name: email.from_name.clone(),
        to: addresses(&email.to_addresses)?,
        cc: email.cc_addresses.as_ref().map(addresses).transpose()?,
        bcc: email.bcc_addresses.as_ref().map(addresses).transpose()?,
        reply_to: email.reply_to.clone(),
        subject: email.subject.clone(),
        html: email.html_body.clone(),
        text: email.text_body.clone(),
        headers: email
            .headers
            .as_ref()
            .map(|value| serde_json::from_value(value.clone()))
            .transpose()?,
        tags: email.tags.as_ref().map(addresses).transpose()?,
        track_opens: email.track_opens,
        track_clicks: email.track_clicks,
    })
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
            console_ingress_address: None,
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
                None,
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
                None,
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
                None,
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

    // ============================================
    // Resend
    // ============================================

    async fn insert_email(
        db: &Arc<sea_orm::DatabaseConnection>,
        status: &str,
        error_message: Option<&str>,
    ) -> Uuid {
        let email_id = Uuid::new_v4();
        emails::ActiveModel {
            id: Set(email_id),
            from_address: Set("sender@test-pending.example.com".to_string()),
            to_addresses: Set(serde_json::json!(["recipient@test.com"])),
            subject: Set(format!("resend {status}")),
            text_body: Set(Some("body".to_string())),
            status: Set(status.to_string()),
            error_message: Set(error_message.map(str::to_string)),
            track_opens: Set(false),
            track_clicks: Set(false),
            open_count: Set(0),
            click_count: Set(0),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();
        email_id
    }

    #[tokio::test]
    async fn resend_refuses_emails_that_must_not_be_delivered_again() {
        let Some((db, email_service, _provider_service, _domain_service)) = setup_test_env().await
        else {
            return;
        };

        let suppressed =
            format!("{SUPPRESSED_RECIPIENTS_ERROR} (previous hard bounce or complaint)");
        for (status, error_message) in [
            ("sent", None),
            ("delivery_unknown", Some("Provider outcome unknown")),
            ("sending", None),
            ("queued", None),
            ("captured", Some(suppressed.as_str())),
        ] {
            let email_id = insert_email(&db.db, status, error_message).await;

            let result = email_service.resend(email_id).await;

            assert!(
                matches!(
                    &result,
                    Err(EmailError::NotResendable { email_id: id, status: s, .. })
                        if *id == email_id && s == status
                ),
                "{status} must not be resendable, got {result:?}"
            );
            let stored = email_service.get(email_id).await.unwrap();
            assert_eq!(
                stored.status, status,
                "a refused resend must not touch the email"
            );
        }

        let missing = email_service.resend(Uuid::new_v4()).await;
        assert!(matches!(missing, Err(EmailError::EmailNotFound(_))));
    }

    #[tokio::test]
    async fn resend_recaptures_in_place_while_the_domain_is_still_pending() {
        let Some((db, email_service, provider_service, _domain_service)) = setup_test_env().await
        else {
            return;
        };
        let provider = create_test_provider(&provider_service).await;
        let domain = create_test_domain(&db.db, provider.id, "test-pending.example.com").await;

        let first = email_service
            .send(test_send_request("captured first"))
            .await
            .unwrap();
        assert_eq!(first.status, "captured");

        let resent = email_service.resend(first.id).await.unwrap();

        assert_eq!(resent.id, first.id, "a resend must reuse the same email");
        assert_eq!(resent.status, "captured");
        let stored = email_service.get(first.id).await.unwrap();
        assert_eq!(stored.domain_id, Some(domain.id));
        assert!(stored
            .error_message
            .as_deref()
            .is_some_and(|message| message.contains("not verified")));
        let count = emails::Entity::find().count(db.db.as_ref()).await.unwrap();
        assert_eq!(count, 1, "a resend must not record a second email");
    }

    #[tokio::test]
    async fn resend_refuses_an_email_whose_stored_headers_can_no_longer_be_sent() {
        let Some((db, email_service, _provider_service, _domain_service)) = setup_test_env().await
        else {
            return;
        };
        let email_id = insert_email(&db.db, "captured", Some("Domain not verified")).await;
        // Recorded before custom headers were restricted to X-Temps-*.
        emails::Entity::update_many()
            .col_expr(
                emails::Column::Headers,
                Expr::value(serde_json::json!({ "List-Unsubscribe": "<https://example.com/u>" })),
            )
            .filter(emails::Column::Id.eq(email_id))
            .exec(db.db.as_ref())
            .await
            .unwrap();

        let result = email_service.resend(email_id).await;

        assert!(
            matches!(
                &result,
                Err(EmailError::NotResendable { email_id: id, reason, .. })
                    if *id == email_id && reason.contains("headers")
            ),
            "got {result:?}"
        );
        let stored = email_service.get(email_id).await.unwrap();
        assert_eq!(
            stored.status, "captured",
            "a refused resend must not claim the email"
        );
        assert_eq!(stored.error_message.as_deref(), Some("Domain not verified"));
    }

    /// Insert an email holding a live resend claim, exactly as
    /// `resend_without_project` leaves it.
    async fn insert_claimed_email(db: &Arc<sea_orm::DatabaseConnection>) -> (Uuid, ResendClaim) {
        let email_id = insert_email(db, "queued", None).await;
        let claim = ResendClaim {
            status: "queued",
            token: fence_token(Utc::now()),
        };
        emails::Entity::update_many()
            .col_expr(emails::Column::SentAt, Expr::value(claim.token))
            .filter(emails::Column::Id.eq(email_id))
            .exec(db.as_ref())
            .await
            .unwrap();
        (email_id, claim)
    }

    #[tokio::test]
    async fn a_failed_resend_returns_its_claim_to_captured_with_the_original_reason() {
        let Some((db, email_service, _provider_service, _domain_service)) = setup_test_env().await
        else {
            return;
        };
        let failure = EmailError::Validation("simulated delivery failure".to_string());

        let (claimed, claim) = insert_claimed_email(&db.db).await;
        email_service
            .release_failed_resend(
                claimed,
                &ResendAttempt::new(claim),
                Some("Domain not verified".to_string()),
                &failure,
            )
            .await;
        let stored = email_service.get(claimed).await.unwrap();
        assert_eq!(
            stored.status, "captured",
            "a failed resend must not strand the email in queued"
        );
        assert_eq!(
            stored.error_message.as_deref(),
            Some("Domain not verified"),
            "the original capture reason must survive a failed resend"
        );
        assert!(
            ensure_resendable(&stored, Utc::now()).is_ok(),
            "the email must stay resendable"
        );

        // Without an original reason, the failure itself is recorded.
        let (unexplained, claim) = insert_claimed_email(&db.db).await;
        email_service
            .release_failed_resend(unexplained, &ResendAttempt::new(claim), None, &failure)
            .await;
        let stored = email_service.get(unexplained).await.unwrap();
        assert_eq!(stored.status, "captured");
        assert!(stored
            .error_message
            .as_deref()
            .is_some_and(|message| message.contains("simulated delivery failure")));

        // An outcome that was already recorded is never overwritten.
        let (sent, claim) = insert_claimed_email(&db.db).await;
        emails::Entity::update_many()
            .col_expr(emails::Column::Status, Expr::value("sent"))
            .filter(emails::Column::Id.eq(sent))
            .exec(db.db.as_ref())
            .await
            .unwrap();
        email_service
            .release_failed_resend(
                sent,
                &ResendAttempt::new(claim),
                Some("Domain not verified".to_string()),
                &failure,
            )
            .await;
        let stored = email_service.get(sent).await.unwrap();
        assert_eq!(stored.status, "sent");
        assert_eq!(stored.error_message, None);
    }

    #[tokio::test]
    async fn a_resend_whose_claim_was_taken_over_neither_delivers_nor_overwrites() {
        let Some((db, email_service, _provider_service, _domain_service)) = setup_test_env().await
        else {
            return;
        };
        let failure = EmailError::Validation("simulated delivery failure".to_string());
        let accepted = || {
            ProviderDeliveryOutcome::Accepted(crate::providers::SendEmailResponse {
                message_id: "provider-message-1".to_string(),
            })
        };

        // A first resend stalls past its lease, and a second one takes over.
        let (email_id, stalled) = insert_claimed_email(&db.db).await;
        let owner = ResendClaim {
            status: "queued",
            token: stalled.token + chrono::Duration::seconds(DELIVERY_LEASE_SECONDS + 1),
        };
        emails::Entity::update_many()
            .col_expr(emails::Column::SentAt, Expr::value(owner.token))
            .filter(emails::Column::Id.eq(email_id))
            .exec(db.db.as_ref())
            .await
            .unwrap();

        // The stalled attempt can no longer reach the provider...
        let refused = email_service.begin_resend_delivery(email_id, stalled).await;
        assert!(
            matches!(refused, Err(EmailError::NotResendable { .. })),
            "a resend that lost its claim must not start delivery, got {refused:?}"
        );
        // ...record a capture...
        let mut capture: emails::ActiveModel = email_service.get(email_id).await.unwrap().into();
        capture.status = Set("captured".to_string());
        capture.error_message = Set(Some("Domain not verified".to_string()));
        let refused = email_service
            .save_capture(email_id, capture, Some(stalled))
            .await;
        assert!(
            matches!(refused, Err(EmailError::NotResendable { .. })),
            "a resend that lost its claim must not record a capture, got {refused:?}"
        );
        // ...or release the claim it no longer owns.
        email_service
            .release_failed_resend(email_id, &ResendAttempt::new(stalled), None, &failure)
            .await;
        let stored = email_service.get(email_id).await.unwrap();
        assert_eq!(stored.status, "queued");
        assert_eq!(stored.sent_at, Some(owner.token));
        assert_eq!(stored.error_message, None);

        // The owner moves on to the provider under a fresh token.
        let sending = email_service
            .begin_resend_delivery(email_id, owner)
            .await
            .unwrap();
        let model = email_service.get(email_id).await.unwrap();
        assert_eq!(model.status, "sending");
        assert_eq!(model.sent_at, Some(sending.token));

        // A result arriving under any other token is discarded.
        let outdated = ResendClaim {
            status: "sending",
            token: stalled.token,
        };
        let response = email_service
            .finalize_unfenced_delivery(model.clone(), accepted(), None, Some(1), Some(outdated))
            .await
            .unwrap();
        assert_eq!(response.status, "sending");
        let stored = email_service.get(email_id).await.unwrap();
        assert_eq!(stored.status, "sending");
        assert_eq!(stored.provider_message_id, None);

        // The owner's own result is recorded.
        let response = email_service
            .finalize_unfenced_delivery(model, accepted(), None, Some(1), Some(sending))
            .await
            .unwrap();
        assert_eq!(response.status, "sent");
        let stored = email_service.get(email_id).await.unwrap();
        assert_eq!(stored.status, "sent");
        assert_eq!(
            stored.provider_message_id.as_deref(),
            Some("provider-message-1")
        );
    }

    #[tokio::test]
    async fn a_rejected_resend_whose_result_could_not_be_saved_returns_to_captured() {
        let Some((db, email_service, _provider_service, _domain_service)) = setup_test_env().await
        else {
            return;
        };
        let rejected = |email_id| EmailError::DeliveryRejectionNotRecorded {
            email_id,
            rejection: "mailbox unavailable".to_string(),
            reason: "connection reset".to_string(),
        };

        // The provider rejected the email after the claim moved to `sending`.
        let (email_id, claim) = insert_claimed_email(&db.db).await;
        let attempt = ResendAttempt::new(claim);
        let sending = email_service
            .begin_resend_delivery(email_id, claim)
            .await
            .unwrap();
        attempt.sending.set(sending).unwrap();
        email_service
            .release_failed_resend(
                email_id,
                &attempt,
                Some("Domain not verified".to_string()),
                &rejected(email_id),
            )
            .await;
        let stored = email_service.get(email_id).await.unwrap();
        assert_eq!(
            stored.status, "captured",
            "an email the provider rejected must not stay stuck in sending"
        );
        assert_eq!(
            stored.error_message.as_deref(),
            Some("Send failed: mailbox unavailable")
        );
        assert!(
            ensure_resendable(&stored, Utc::now()).is_ok(),
            "an undelivered email must stay resendable"
        );

        // Any other failure after the provider was contacted leaves a
        // `sending` claim alone: only a definite rejection is known to be
        // undelivered.
        let (email_id, claim) = insert_claimed_email(&db.db).await;
        let attempt = ResendAttempt::new(claim);
        let sending = email_service
            .begin_resend_delivery(email_id, claim)
            .await
            .unwrap();
        attempt.sending.set(sending).unwrap();
        for failure in [
            EmailError::Validation("simulated delivery failure".to_string()),
            EmailError::DeliveryOutcomeNotRecorded {
                email_id,
                outcome: "sent".to_string(),
                reason: "connection reset".to_string(),
            },
        ] {
            email_service
                .release_failed_resend(email_id, &attempt, None, &failure)
                .await;
            let stored = email_service.get(email_id).await.unwrap();
            assert_eq!(stored.status, "sending", "released by {failure:?}");
            assert_eq!(stored.error_message, None);
        }

        // The rejection is reported as such when it cannot be saved.
        let vanished = insert_email(&db.db, "queued", None).await;
        let model = email_service.get(vanished).await.unwrap();
        emails::Entity::delete_by_id(vanished)
            .exec(db.db.as_ref())
            .await
            .unwrap();
        let error = email_service
            .finalize_unfenced_delivery(
                model,
                ProviderDeliveryOutcome::Rejected(EmailError::SendFailed {
                    provider: "mock".to_string(),
                    retryable: false,
                    message: "mailbox unavailable".to_string(),
                }),
                None,
                Some(1),
                None,
            )
            .await
            .unwrap_err();
        assert!(
            matches!(
                &error,
                EmailError::DeliveryRejectionNotRecorded { email_id, rejection, .. }
                    if *email_id == vanished && rejection.contains("mailbox unavailable")
            ),
            "an unsaved rejection must be reported as such, got {error:?}"
        );
    }

    #[tokio::test]
    async fn a_resend_interrupted_after_reaching_the_provider_is_never_taken_over() {
        let Some((db, email_service, _provider_service, _domain_service)) = setup_test_env().await
        else {
            return;
        };

        // The provider was being called when the process died, long ago.
        let (email_id, claim) = insert_claimed_email(&db.db).await;
        email_service
            .begin_resend_delivery(email_id, claim)
            .await
            .unwrap();
        emails::Entity::update_many()
            .col_expr(
                emails::Column::SentAt,
                Expr::value(Utc::now() - chrono::Duration::seconds(DELIVERY_LEASE_SECONDS + 1)),
            )
            .filter(emails::Column::Id.eq(email_id))
            .exec(db.db.as_ref())
            .await
            .unwrap();

        let refused = email_service.resend(email_id).await;
        assert!(
            matches!(
                &refused,
                Err(EmailError::NotResendable { status, reason, .. })
                    if status == "sending" && reason.contains("interrupted after the provider")
            ),
            "an email the provider may hold must not be resent, got {refused:?}"
        );
        assert_eq!(email_service.get(email_id).await.unwrap().status, "sending");
    }

    #[tokio::test]
    async fn a_resend_the_provider_may_have_accepted_is_never_returned_to_captured() {
        let Some((db, email_service, _provider_service, _domain_service)) = setup_test_env().await
        else {
            return;
        };

        // The provider answered, but the result cannot be saved: the row is
        // gone, so every attempt at the write fails.
        for (outcome, expected) in [
            (
                ProviderDeliveryOutcome::Accepted(crate::providers::SendEmailResponse {
                    message_id: "provider-message-1".to_string(),
                }),
                "sent",
            ),
            (
                ProviderDeliveryOutcome::Unknown("provider timed out".to_string()),
                "delivery_unknown",
            ),
        ] {
            let vanished = insert_email(&db.db, "queued", None).await;
            let model = email_service.get(vanished).await.unwrap();
            emails::Entity::delete_by_id(vanished)
                .exec(db.db.as_ref())
                .await
                .unwrap();
            let error = email_service
                .finalize_unfenced_delivery(model, outcome, None, Some(1), None)
                .await
                .unwrap_err();
            assert!(
                matches!(
                    &error,
                    EmailError::DeliveryOutcomeNotRecorded { email_id, outcome, .. }
                        if *email_id == vanished && outcome == expected
                ),
                "an unsaved '{expected}' result must be reported as such, got {error:?}"
            );
        }

        // A live resend claim whose delivery ended that way keeps its claim.
        let (claimed, claim) = insert_claimed_email(&db.db).await;
        let unrecorded = EmailError::DeliveryOutcomeNotRecorded {
            email_id: claimed,
            outcome: "sent".to_string(),
            reason: "connection reset".to_string(),
        };
        email_service
            .release_failed_resend(
                claimed,
                &ResendAttempt::new(claim),
                Some("Domain not verified".to_string()),
                &unrecorded,
            )
            .await;
        let stored = email_service.get(claimed).await.unwrap();
        assert_eq!(
            stored.status, "queued",
            "an email the provider accepted must not reappear as an undelivered capture"
        );
        assert_eq!(stored.error_message, None);
        assert!(
            matches!(
                email_service.resend(claimed).await,
                Err(EmailError::NotResendable { .. })
            ),
            "it must not be resendable while its claim is live"
        );
    }

    #[tokio::test]
    async fn resend_recovers_a_claim_abandoned_by_a_crash_once_its_lease_lapses() {
        let Some((db, email_service, _provider_service, _domain_service)) = setup_test_env().await
        else {
            return;
        };
        let set_started = |email_id: Uuid, started: chrono::DateTime<Utc>| {
            let db = db.db.clone();
            async move {
                emails::Entity::update_many()
                    .col_expr(emails::Column::SentAt, Expr::value(started))
                    .filter(emails::Column::Id.eq(email_id))
                    .exec(db.as_ref())
                    .await
                    .unwrap();
            }
        };

        let in_flight = insert_email(&db.db, "queued", None).await;
        set_started(in_flight, Utc::now()).await;
        let refused = email_service.resend(in_flight).await;
        assert!(
            matches!(refused, Err(EmailError::NotResendable { .. })),
            "a resend still inside its lease must not be stolen, got {refused:?}"
        );

        let abandoned = insert_email(&db.db, "queued", None).await;
        set_started(
            abandoned,
            Utc::now() - chrono::Duration::seconds(DELIVERY_LEASE_SECONDS + 1),
        )
        .await;
        let resent = email_service.resend(abandoned).await.unwrap();
        assert_eq!(resent.id, abandoned);
        assert_eq!(resent.status, "captured");
        assert_eq!(
            email_service.get(abandoned).await.unwrap().status,
            "captured"
        );
    }

    #[tokio::test]
    async fn resend_attaches_the_domain_to_a_capture_taken_before_it_existed() {
        let Some((db, email_service, provider_service, _domain_service)) = setup_test_env().await
        else {
            return;
        };
        let first = email_service
            .send(test_send_request("before the domain"))
            .await
            .unwrap();
        assert_eq!(first.status, "captured");
        assert_eq!(email_service.get(first.id).await.unwrap().domain_id, None);

        let provider = create_test_provider(&provider_service).await;
        let domain = create_test_domain(&db.db, provider.id, "test-pending.example.com").await;
        email_service.resend(first.id).await.unwrap();

        let stored = email_service.get(first.id).await.unwrap();
        assert_eq!(stored.domain_id, Some(domain.id));
    }

    #[tokio::test]
    async fn resend_of_a_project_email_reuses_its_claim_and_respects_the_live_lease() {
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
        domain_service
            .authorize_project(domain.id, project_id)
            .await
            .unwrap();

        // Every optional field set, so rebuilding the request from the stored
        // email must reproduce the exact idempotency payload hash.
        let mut request = test_send_request("project resend");
        request.cc = Some(vec!["cc@test.com".to_string()]);
        request.reply_to = Some("reply@test.com".to_string());
        request.headers = Some(
            [
                ("X-Temps-B".to_string(), "2".to_string()),
                ("X-Temps-A".to_string(), "1".to_string()),
            ]
            .into_iter()
            .collect(),
        );
        request.tags = Some(vec!["welcome".to_string()]);
        let first = email_service
            .send_for_project(request, project_id, "resend-1".into())
            .await
            .unwrap();
        assert_eq!(first.status, "captured");

        let too_soon = email_service.resend(first.id).await;
        assert!(
            matches!(too_soon, Err(EmailError::NotResendable { ref reason, .. }) if reason.contains("try again")),
            "a live delivery lease must not be stolen, got {too_soon:?}"
        );

        email_idempotency_keys::Entity::update_many()
            .col_expr(
                email_idempotency_keys::Column::LeaseExpiresAt,
                Expr::value(Utc::now() - chrono::Duration::seconds(1)),
            )
            .filter(email_idempotency_keys::Column::ProjectId.eq(project_id))
            .exec(db.db.as_ref())
            .await
            .unwrap();
        let resent = email_service.resend(first.id).await.unwrap();

        assert_eq!(resent.id, first.id);
        assert_eq!(resent.status, "captured");
        let count = emails::Entity::find()
            .filter(emails::Column::ProjectId.eq(project_id))
            .count(db.db.as_ref())
            .await
            .unwrap();
        assert_eq!(count, 1, "a resend must not record a second email");
    }

    #[tokio::test]
    async fn successful_delivery_clears_the_error_of_an_earlier_capture() {
        let Some((db, email_service, _provider_service, _domain_service)) = setup_test_env().await
        else {
            return;
        };
        let email_id = insert_email(&db.db, "queued", Some("Domain 'x' not verified")).await;
        let email_model = email_service.get(email_id).await.unwrap();

        let mock = MockEmailProvider::new().with_scripted_responses([MockSendResult::Succeed]);
        let (outcome, attempts) = send_with_retry(&mock, &test_provider_send_request()).await;
        email_service
            .finalize_unfenced_delivery(email_model, outcome, None, Some(attempts as i32), None)
            .await
            .unwrap();

        let stored = email_service.get(email_id).await.unwrap();
        assert_eq!(stored.status, "sent");
        assert_eq!(stored.error_message, None);
    }
}
