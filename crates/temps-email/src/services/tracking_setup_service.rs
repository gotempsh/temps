//! One-click AWS-side setup for SES event tracking.
//!
//! Wires the three AWS resources needed for delivered/bounced/complained
//! events to reach the Temps webhook, using only the credentials the SES
//! provider already stores:
//!
//! 1. an SNS topic (`temps-email-events-{provider_id}`),
//! 2. an HTTP(S) subscription pointing at `{external_url}/api/t/webhook/ses`,
//! 3. an SESv2 event destination on the `temps-tracking` configuration set
//!    (which every send already passes through) publishing BOUNCE, COMPLAINT
//!    and DELIVERY events to that topic.
//!
//! Ordering matters: the topic ARN is persisted on the provider *before* the
//! subscription is requested, because the tracking webhook only auto-confirms
//! `SubscriptionConfirmation` messages for topics that are already authorized
//! on an active provider. Subscribing first would leave the subscription
//! stuck in "pending" with no error surfaced anywhere.
//!
//! Every step is idempotent, so a partially failed run can simply be retried.

use std::sync::Arc;

use aws_config::BehaviorVersion;
use aws_sdk_sesv2::config::{Credentials, Region};
use aws_sdk_sesv2::types::{EventDestinationDefinition, EventType, SnsDestination};
use sea_orm::DatabaseConnection;
use tracing::{debug, error};

use crate::errors::EmailError;
use crate::providers::{EmailProviderType, SesCredentials, DEFAULT_CONFIGURATION_SET};
use crate::services::provider_service::ProviderService;
use temps_entities::email_providers;

/// Name of the SESv2 event destination this service manages.
const EVENT_DESTINATION_NAME: &str = "temps-sns-events";

/// Outcome of a setup run. All steps are idempotent; a `true` flag means the
/// resource now exists in the desired state, not necessarily that this run
/// created it.
#[derive(Debug, Clone)]
pub struct TrackingSetupResult {
    /// ARN of the SNS topic now bound to the provider.
    pub topic_arn: String,
    /// The webhook endpoint the topic was subscribed to.
    pub webhook_url: String,
    /// Whether an HTTP(S) subscription request was issued. Confirmation
    /// happens asynchronously via the webhook.
    pub subscription_requested: bool,
    /// Whether the SESv2 event destination is attached to the
    /// `temps-tracking` configuration set.
    pub event_destination_attached: bool,
}

/// Provisions AWS-side SES event tracking for a provider.
pub struct TrackingSetupService {
    provider_service: Arc<ProviderService>,
    db: Arc<DatabaseConnection>,
}

impl TrackingSetupService {
    pub fn new(provider_service: Arc<ProviderService>, db: Arc<DatabaseConnection>) -> Self {
        Self {
            provider_service,
            db,
        }
    }

    /// Run the full setup for `provider_id`, subscribing `webhook_url`.
    pub async fn setup_ses_event_tracking(
        &self,
        provider_id: i32,
        webhook_url: &str,
    ) -> Result<TrackingSetupResult, EmailError> {
        let provider = self.provider_service.get(provider_id).await?;
        if EmailProviderType::from_str(&provider.provider_type)? != EmailProviderType::Ses {
            return Err(EmailError::Validation(
                "Event tracking setup is only available for SES providers".to_string(),
            ));
        }
        if !provider.is_active {
            return Err(EmailError::Validation(
                "Provider is deactivated — activate it before setting up event tracking"
                    .to_string(),
            ));
        }
        let protocol = webhook_protocol(webhook_url)?;

        let credentials = self.provider_service.ses_credentials(&provider)?;

        // Step 1: ensure the SNS topic exists. CreateTopic is idempotent and
        // returns the existing ARN when the topic is already there.
        let sns = sns_client(&credentials, &provider.region).await;
        let topic_name = format!("temps-email-events-{}", provider.id);
        let topic_arn = sns
            .create_topic()
            .name(&topic_name)
            .send()
            .await
            .map_err(|e| step_error("create the SNS topic", &e))?
            .topic_arn()
            .ok_or_else(|| {
                EmailError::ProviderError("SNS CreateTopic returned no topic ARN".to_string())
            })?
            .to_string();
        debug!("Event tracking topic for provider {provider_id}: {topic_arn}");

        // Step 2: persist the ARN on the provider *before* subscribing, so
        // the incoming SubscriptionConfirmation is authorized and the
        // webhook auto-confirms it (see module docs for why this order).
        self.provider_service
            .update_with_sns_topic(
                provider_id,
                Default::default(),
                Some(Some(topic_arn.clone())),
            )
            .await?;

        // Step 3: subscribe the Temps webhook. Idempotent — SNS returns the
        // existing subscription (or "pending confirmation") for a repeated
        // endpoint+protocol pair.
        sns.subscribe()
            .topic_arn(&topic_arn)
            .protocol(protocol)
            .endpoint(webhook_url)
            .send()
            .await
            .map_err(|e| step_error("subscribe the webhook endpoint to the SNS topic", &e))?;

        // Step 4: attach the SESv2 event destination to the configuration
        // set every send goes through.
        let ses = sesv2_client(&credentials, &provider.region).await;
        ensure_configuration_set(&ses).await?;
        ensure_event_destination(&ses, &topic_arn).await?;

        Ok(TrackingSetupResult {
            topic_arn,
            webhook_url: webhook_url.to_string(),
            subscription_requested: true,
            event_destination_attached: true,
        })
    }

    /// The most recent provider-feedback event (delivered/bounced/complained)
    /// recorded for any email sent through this provider's domains. Opens and
    /// clicks are excluded — they come from the tracking pixel, not the SNS
    /// pipeline, so they say nothing about whether this setup works.
    pub async fn last_provider_event_at(
        &self,
        provider_id: i32,
    ) -> Result<Option<temps_core::DBDateTime>, EmailError> {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect};
        use temps_entities::{email_domains, email_events, emails};

        let db = self.db.as_ref();
        let domain_ids: Vec<i32> = email_domains::Entity::find()
            .filter(email_domains::Column::ProviderId.eq(provider_id))
            .all(db)
            .await?
            .into_iter()
            .map(|d| d.id)
            .collect();
        if domain_ids.is_empty() {
            return Ok(None);
        }

        let event = email_events::Entity::find()
            .inner_join(emails::Entity)
            .filter(emails::Column::DomainId.is_in(domain_ids))
            .filter(email_events::Column::EventType.is_in(["delivered", "bounced", "complained"]))
            .order_by_desc(email_events::Column::CreatedAt)
            .limit(1)
            .one(db)
            .await?;

        Ok(event.map(|e| e.created_at))
    }

    /// Load the provider row (for status reporting).
    pub async fn provider(&self, provider_id: i32) -> Result<email_providers::Model, EmailError> {
        self.provider_service.get(provider_id).await
    }
}

/// SNS requires the subscription protocol to match the endpoint scheme.
fn webhook_protocol(webhook_url: &str) -> Result<&'static str, EmailError> {
    if webhook_url.starts_with("https://") {
        Ok("https")
    } else if webhook_url.starts_with("http://") {
        // Allowed for local development; SNS itself refuses plain HTTP for
        // some regions/partitions, in which case the Subscribe step will
        // surface AWS's error verbatim.
        Ok("http")
    } else {
        Err(EmailError::Configuration(format!(
            "External URL '{webhook_url}' is not an http(s) URL — set a public external URL before configuring event tracking"
        )))
    }
}

async fn aws_config(credentials: &SesCredentials, region: &str) -> aws_config::SdkConfig {
    let creds = Credentials::new(
        &credentials.access_key_id,
        &credentials.secret_access_key,
        None,
        None,
        "temps-email-tracking-setup",
    );
    let mut builder = aws_config::defaults(BehaviorVersion::latest())
        .region(Region::new(region.to_string()))
        .credentials_provider(creds);
    if let Some(ref endpoint_url) = credentials.endpoint_url {
        builder = builder.endpoint_url(endpoint_url);
    }
    builder.load().await
}

async fn sns_client(credentials: &SesCredentials, region: &str) -> aws_sdk_sns::Client {
    aws_sdk_sns::Client::new(&aws_config(credentials, region).await)
}

async fn sesv2_client(credentials: &SesCredentials, region: &str) -> aws_sdk_sesv2::Client {
    aws_sdk_sesv2::Client::new(&aws_config(credentials, region).await)
}

async fn ensure_configuration_set(ses: &aws_sdk_sesv2::Client) -> Result<(), EmailError> {
    if let Err(e) = ses
        .create_configuration_set()
        .configuration_set_name(DEFAULT_CONFIGURATION_SET)
        .send()
        .await
    {
        let err_str = format!("{}", aws_sdk_sesv2::error::DisplayErrorContext(&e));
        if !err_str.contains("AlreadyExists") && !err_str.contains("already exists") {
            error!("Failed to create SES configuration set: {err_str}");
            return Err(EmailError::AwsSes(format!(
                "Failed to create the '{DEFAULT_CONFIGURATION_SET}' configuration set: {err_str}"
            )));
        }
    }
    Ok(())
}

async fn ensure_event_destination(
    ses: &aws_sdk_sesv2::Client,
    topic_arn: &str,
) -> Result<(), EmailError> {
    let definition = EventDestinationDefinition::builder()
        .enabled(true)
        .matching_event_types(EventType::Bounce)
        .matching_event_types(EventType::Complaint)
        .matching_event_types(EventType::Delivery)
        .sns_destination(
            SnsDestination::builder()
                .topic_arn(topic_arn)
                .build()
                .map_err(|e| EmailError::AwsSes(format!("Invalid SNS destination: {e}")))?,
        )
        .build();

    let create_result = ses
        .create_configuration_set_event_destination()
        .configuration_set_name(DEFAULT_CONFIGURATION_SET)
        .event_destination_name(EVENT_DESTINATION_NAME)
        .event_destination(definition.clone())
        .send()
        .await;

    if let Err(e) = create_result {
        let err_str = format!("{}", aws_sdk_sesv2::error::DisplayErrorContext(&e));
        if err_str.contains("AlreadyExists") || err_str.contains("already exists") {
            // Re-runs (e.g. after a topic rotation) update the existing
            // destination in place so it points at the current topic.
            ses.update_configuration_set_event_destination()
                .configuration_set_name(DEFAULT_CONFIGURATION_SET)
                .event_destination_name(EVENT_DESTINATION_NAME)
                .event_destination(definition)
                .send()
                .await
                .map_err(|e| step_error("update the SES event destination", &e))?;
        } else {
            error!("Failed to create SES event destination: {err_str}");
            return Err(EmailError::AwsSes(format!(
                "Failed to attach the SES event destination: {err_str}"
            )));
        }
    }
    Ok(())
}

/// Wrap an AWS SDK error with the step that failed, keeping AWS's own
/// message intact — the operator needs the real reason (missing IAM
/// permission, bad region, ...) to act on it.
fn step_error<E>(step: &str, error: &aws_sdk_sns::error::SdkError<E>) -> EmailError
where
    E: std::error::Error + Send + Sync + 'static,
{
    let detail = format!("{}", aws_sdk_sns::error::DisplayErrorContext(error));
    error!("Event tracking setup failed to {step}: {detail}");
    EmailError::ProviderError(format!("Failed to {step}: {detail}"))
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    /// Expose the internal SNS client construction so integration tests can
    /// inspect LocalStack state with the same endpoint handling.
    pub(crate) async fn sns_client_for_tests(
        credentials: &SesCredentials,
        region: &str,
    ) -> aws_sdk_sns::Client {
        sns_client(credentials, region).await
    }
}
