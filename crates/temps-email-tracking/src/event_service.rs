//! Email event service — queries and stats for email tracking events

use sea_orm::{
    sea_query::LockType, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait,
    PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, TransactionTrait,
};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use uuid::Uuid;

use temps_entities::{email_domains, email_events, email_providers, emails};

use crate::errors::EmailTrackingError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnsProcessingOutcome {
    Processed,
    AlreadyProcessed,
    Unmatched,
}

/// Service for querying email tracking events
pub struct EmailEventService {
    db: Arc<DatabaseConnection>,
}

/// Query options for listing email events
#[derive(Debug, Clone, Default)]
pub struct ListEmailEventsOptions {
    pub email_id: Option<Uuid>,
    pub event_type: Option<String>,
    pub page: Option<u64>,
    pub page_size: Option<u64>,
}

/// Aggregate email event statistics
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct EmailEventStats {
    pub delivered: u64,
    pub opened: u64,
    pub clicked: u64,
    pub bounced: u64,
    pub complained: u64,
    pub open_rate: Option<f64>,
    pub click_rate: Option<f64>,
    pub bounce_rate: Option<f64>,
}

fn is_idempotency_violation(error: &sea_orm::DbErr) -> bool {
    error
        .to_string()
        .contains("idx_email_events_idempotency_key")
}

fn normalized_recipients(email: &emails::Model) -> HashSet<String> {
    let mut recipients = HashSet::new();
    for value in [
        Some(&email.to_addresses),
        email.cc_addresses.as_ref(),
        email.bcc_addresses.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        if let Ok(addresses) = serde_json::from_value::<Vec<String>>(value.clone()) {
            recipients.extend(
                addresses
                    .into_iter()
                    .map(|address| address.trim().to_lowercase()),
            );
        }
    }
    recipients
}

impl EmailEventService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    /// Correlate, validate and persist an SNS envelope before returning 2xx.
    /// Event rows and any hard-bounce/complaint suppression share one DB
    /// transaction, and a dedicated idempotency key makes retries safe.
    #[allow(clippy::too_many_arguments)]
    pub async fn process_sns_event(
        &self,
        suppression_service: &temps_email::SuppressionService,
        topic_arn: &str,
        sns_message_id: &str,
        provider_message_id: &str,
        event_type: &str,
        recipients: &[String],
        metadata: Option<serde_json::Value>,
        suppression_reason: Option<temps_email::SuppressionReason>,
    ) -> Result<SnsProcessingOutcome, EmailTrackingError> {
        let transaction = self.db.begin().await?;
        let matches = emails::Entity::find()
            .filter(emails::Column::ProviderMessageId.eq(provider_message_id))
            .limit(2)
            .all(&transaction)
            .await?;

        let email = match matches.as_slice() {
            [] => {
                transaction.rollback().await?;
                return Ok(SnsProcessingOutcome::Unmatched);
            }
            [email] => email,
            _ => {
                transaction.rollback().await?;
                return Err(EmailTrackingError::AmbiguousProviderMessage {
                    provider_message_id: provider_message_id.to_string(),
                });
            }
        };

        let domain_id = email.domain_id.ok_or_else(|| {
            EmailTrackingError::Configuration(format!(
                "Correlated email {} has no sending domain",
                email.id
            ))
        })?;
        let domain = email_domains::Entity::find_by_id(domain_id)
            .one(&transaction)
            .await?
            .ok_or_else(|| {
                EmailTrackingError::Configuration(format!(
                    "Correlated email {} references missing domain {domain_id}",
                    email.id
                ))
            })?;
        let provider = email_providers::Entity::find_by_id(domain.provider_id)
            // Hold the provider configuration stable until the event and any
            // suppression are committed. Rotation/deactivation takes an
            // exclusive row lock and therefore cannot race this check.
            .lock(LockType::Share)
            .one(&transaction)
            .await?
            .ok_or_else(|| {
                EmailTrackingError::Configuration(format!(
                    "Sending domain {domain_id} references missing provider {}",
                    domain.provider_id
                ))
            })?;
        if !provider.is_active
            || provider.provider_type != "ses"
            || provider.sns_topic_arn.as_deref() != Some(topic_arn)
        {
            transaction.rollback().await?;
            return Err(EmailTrackingError::TopicMismatch {
                email_id: email.id.to_string(),
                topic_arn: topic_arn.to_string(),
            });
        }

        let allowed_recipients = normalized_recipients(email);
        let mut unique_recipients = HashMap::new();
        for recipient in recipients {
            let normalized = recipient.trim().to_lowercase();
            if !allowed_recipients.contains(&normalized) {
                transaction.rollback().await?;
                return Err(EmailTrackingError::RecipientMismatch {
                    email_id: email.id.to_string(),
                    recipient: recipient.clone(),
                });
            }
            unique_recipients
                .entry(normalized)
                .or_insert_with(|| recipient.clone());
        }

        for (normalized, recipient) in unique_recipients {
            let idempotency_key = hex::encode(Sha256::digest(
                format!("{topic_arn}\n{sns_message_id}\n{normalized}").as_bytes(),
            ));
            let event = email_events::ActiveModel {
                email_id: Set(email.id),
                event_type: Set(event_type.to_string()),
                provider_message_id: Set(Some(provider_message_id.to_string())),
                idempotency_key: Set(Some(idempotency_key)),
                recipient: Set(Some(recipient.clone())),
                metadata: Set(metadata.clone()),
                ip_address: Set(None),
                user_agent: Set(None),
                ..Default::default()
            };

            if let Err(error) = email_events::Entity::insert(event).exec(&transaction).await {
                transaction.rollback().await?;
                if is_idempotency_violation(&error) {
                    return Ok(SnsProcessingOutcome::AlreadyProcessed);
                }
                return Err(error.into());
            }

            if let Some(reason) = suppression_reason {
                if let Err(error) = suppression_service
                    .suppress_with(
                        &transaction,
                        &recipient,
                        reason,
                        domain_id,
                        Some(format!(
                            "SES {event_type} for message {provider_message_id}"
                        )),
                    )
                    .await
                {
                    transaction.rollback().await?;
                    return Err(error.into());
                }
            }
        }

        transaction.commit().await?;
        Ok(SnsProcessingOutcome::Processed)
    }

    /// Record a tracking event (open, click, bounce, complaint, delivery)
    #[allow(clippy::too_many_arguments)]
    pub async fn record_event(
        &self,
        email_id: Uuid,
        event_type: &str,
        provider_message_id: Option<String>,
        recipient: Option<String>,
        metadata: Option<serde_json::Value>,
        ip_address: Option<String>,
        user_agent: Option<String>,
    ) -> Result<(), EmailTrackingError> {
        use sea_orm::ActiveValue::Set;

        let event = email_events::ActiveModel {
            email_id: Set(email_id),
            event_type: Set(event_type.to_string()),
            provider_message_id: Set(provider_message_id),
            recipient: Set(recipient),
            metadata: Set(metadata),
            ip_address: Set(ip_address),
            user_agent: Set(user_agent),
            ..Default::default()
        };

        email_events::Entity::insert(event)
            .exec(self.db.as_ref())
            .await?;
        Ok(())
    }

    /// Look up which email a provider notification (SES bounce/complaint/delivery)
    /// is about, by the message ID the provider returned when we sent it
    /// (`emails.provider_message_id`). Returns `None` when there's no match —
    /// e.g. the email was sent outside Temps' own send() path, or the row has
    /// since been deleted — so the caller can decide how to record an
    /// unmatched event instead of incorrectly attaching it to a random email.
    pub async fn find_email_id_by_provider_message_id(
        &self,
        provider_message_id: &str,
    ) -> Result<Option<Uuid>, EmailTrackingError> {
        let email = emails::Entity::find()
            .filter(emails::Column::ProviderMessageId.eq(provider_message_id))
            .one(self.db.as_ref())
            .await?;
        Ok(email.map(|e| e.id))
    }

    /// List events for an email with optional filtering
    pub async fn list_events(
        &self,
        options: ListEmailEventsOptions,
    ) -> Result<(Vec<email_events::Model>, u64), EmailTrackingError> {
        let page = options.page.unwrap_or(1);
        let page_size = std::cmp::min(options.page_size.unwrap_or(20), 100);

        let mut query = email_events::Entity::find().order_by_desc(email_events::Column::CreatedAt);

        if let Some(email_id) = options.email_id {
            query = query.filter(email_events::Column::EmailId.eq(email_id));
        }

        if let Some(event_type) = options.event_type {
            query = query.filter(email_events::Column::EventType.eq(event_type));
        }

        let paginator = query.paginate(self.db.as_ref(), page_size);
        let total = paginator.num_items().await?;
        let items = paginator.fetch_page(page - 1).await?;

        Ok((items, total))
    }

    /// Get aggregate email event statistics
    pub async fn get_stats(
        &self,
        email_id: Option<Uuid>,
    ) -> Result<EmailEventStats, EmailTrackingError> {
        let mut base_query = email_events::Entity::find();

        if let Some(email_id) = email_id {
            base_query = base_query.filter(email_events::Column::EmailId.eq(email_id));
        }

        let delivered = base_query
            .clone()
            .filter(email_events::Column::EventType.eq("delivered"))
            .count(self.db.as_ref())
            .await?;

        let opened = base_query
            .clone()
            .filter(email_events::Column::EventType.eq("opened"))
            .count(self.db.as_ref())
            .await?;

        let clicked = base_query
            .clone()
            .filter(email_events::Column::EventType.eq("clicked"))
            .count(self.db.as_ref())
            .await?;

        let bounced = base_query
            .clone()
            .filter(email_events::Column::EventType.eq("bounced"))
            .count(self.db.as_ref())
            .await?;

        let complained = base_query
            .filter(email_events::Column::EventType.eq("complained"))
            .count(self.db.as_ref())
            .await?;

        let open_rate = if delivered > 0 {
            Some(opened as f64 / delivered as f64)
        } else {
            None
        };

        let click_rate = if delivered > 0 {
            Some(clicked as f64 / delivered as f64)
        } else {
            None
        };

        let bounce_rate = if delivered > 0 {
            Some(bounced as f64 / delivered as f64)
        } else {
            None
        };

        Ok(EmailEventStats {
            delivered,
            opened,
            clicked,
            bounced,
            complained,
            open_rate,
            click_rate,
            bounce_rate,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ActiveModelTrait, ActiveValue::Set, ConnectionTrait};
    use temps_database::test_utils::TestDatabase;
    use temps_entities::{email_domains, email_providers, suppressed_recipients};

    const TEST_TOPIC_ARN: &str = "arn:aws:sns:us-east-1:123456789012:temps-ses";

    fn docker_is_unavailable(error: &impl std::fmt::Display) -> bool {
        let message = error.to_string().to_lowercase();
        message.contains("docker")
            || message.contains("testcontainers")
            || message.contains("container runtime")
            || message.contains("/var/run/docker.sock")
            || message.contains("failed to create a container")
            || message.contains("hyper legacy client")
    }

    async fn setup_database() -> Option<TestDatabase> {
        match TestDatabase::with_migrations().await {
            Ok(db) => Some(db),
            Err(error) if docker_is_unavailable(&error) => {
                eprintln!("Docker unavailable, skipping email-event integration test: {error:#}");
                None
            }
            Err(error) => panic!("email-event test database setup failed: {error:#}"),
        }
    }

    async fn create_domain(db: &DatabaseConnection, suffix: &str) -> i32 {
        let provider = email_providers::ActiveModel {
            name: Set(format!("Event test provider {suffix}")),
            provider_type: Set("ses".to_string()),
            region: Set("us-east-1".to_string()),
            credentials: Set("test-credentials".to_string()),
            sns_topic_arn: Set(Some(TEST_TOPIC_ARN.to_string())),
            is_active: Set(true),
            ..Default::default()
        }
        .insert(db)
        .await
        .unwrap();
        email_domains::ActiveModel {
            provider_id: Set(provider.id),
            domain: Set(format!("{suffix}.example.com")),
            status: Set("verified".to_string()),
            ..Default::default()
        }
        .insert(db)
        .await
        .unwrap()
        .id
    }

    async fn create_email_with_recipients(
        db: &DatabaseConnection,
        provider_message_id: &str,
        domain_id: Option<i32>,
        recipients: &[&str],
    ) -> Uuid {
        let email_id = Uuid::new_v4();
        emails::ActiveModel {
            id: Set(email_id),
            domain_id: Set(domain_id),
            from_address: Set("sender@example.com".to_string()),
            to_addresses: Set(serde_json::json!(recipients)),
            subject: Set("Provider correlation regression".to_string()),
            status: Set("sent".to_string()),
            provider_message_id: Set(Some(provider_message_id.to_string())),
            track_opens: Set(false),
            track_clicks: Set(false),
            open_count: Set(0),
            click_count: Set(0),
            ..Default::default()
        }
        .insert(db)
        .await
        .unwrap();
        email_id
    }

    async fn create_email(db: &DatabaseConnection, provider_message_id: &str) -> Uuid {
        create_email_with_recipients(db, provider_message_id, None, &["recipient@example.com"])
            .await
    }

    #[test]
    fn test_list_options_default() {
        let options = ListEmailEventsOptions::default();
        assert!(options.email_id.is_none());
        assert!(options.event_type.is_none());
        assert!(options.page.is_none());
        assert!(options.page_size.is_none());
    }

    #[test]
    fn test_email_event_stats_rates() {
        let stats = EmailEventStats {
            delivered: 100,
            opened: 40,
            clicked: 10,
            bounced: 5,
            complained: 1,
            open_rate: Some(0.4),
            click_rate: Some(0.1),
            bounce_rate: Some(0.05),
        };

        assert_eq!(stats.delivered, 100);
        assert_eq!(stats.open_rate, Some(0.4));
        assert_eq!(stats.click_rate, Some(0.1));
        assert_eq!(stats.bounce_rate, Some(0.05));
    }

    #[test]
    fn test_email_event_stats_zero_delivered() {
        let stats = EmailEventStats {
            delivered: 0,
            opened: 0,
            clicked: 0,
            bounced: 0,
            complained: 0,
            open_rate: None,
            click_rate: None,
            bounce_rate: None,
        };

        assert!(stats.open_rate.is_none());
        assert!(stats.click_rate.is_none());
        assert!(stats.bounce_rate.is_none());
    }

    #[tokio::test]
    async fn provider_message_id_correlation_returns_exact_email_or_none() {
        let Some(db) = setup_database().await else {
            return;
        };
        let service = EmailEventService::new(db.db.clone());
        let email_id = create_email(db.db.as_ref(), "ses-message-123").await;

        assert_eq!(
            service
                .find_email_id_by_provider_message_id("ses-message-123")
                .await
                .unwrap(),
            Some(email_id)
        );
        assert_eq!(
            service
                .find_email_id_by_provider_message_id("unknown-message")
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn unrelated_foreign_key_failure_is_not_swallowed_as_replay() {
        let Some(db) = setup_database().await else {
            return;
        };
        let service = EmailEventService::new(db.db.clone());

        let result = service
            .record_event(
                Uuid::new_v4(),
                "bounced",
                Some("sns-notification-orphan:recipient@example.com".to_string()),
                Some("recipient@example.com".to_string()),
                None,
                None,
                None,
            )
            .await;

        assert!(
            matches!(result, Err(EmailTrackingError::Database(_))),
            "an orphan event must surface its FK violation, not masquerade as dedup"
        );
        assert_eq!(
            email_events::Entity::find()
                .count(db.db.as_ref())
                .await
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn sns_processing_requires_provider_and_recipient_correlation() {
        let Some(db) = setup_database().await else {
            return;
        };
        let event_service = EmailEventService::new(db.db.clone());
        let suppression_service = temps_email::SuppressionService::new(db.db.clone());
        let domain_id = create_domain(db.db.as_ref(), "correlation").await;
        create_email_with_recipients(
            db.db.as_ref(),
            "known-provider-message",
            Some(domain_id),
            &["intended@example.com"],
        )
        .await;

        let unmatched = event_service
            .process_sns_event(
                &suppression_service,
                "arn:aws:sns:us-east-1:123456789012:temps-ses",
                "sns-unmatched",
                "unknown-provider-message",
                "complained",
                &["intended@example.com".to_string()],
                None,
                Some(temps_email::SuppressionReason::Complained),
            )
            .await
            .unwrap();
        assert_eq!(unmatched, SnsProcessingOutcome::Unmatched);

        let mismatch = event_service
            .process_sns_event(
                &suppression_service,
                "arn:aws:sns:us-east-1:123456789012:temps-ses",
                "sns-mismatch",
                "known-provider-message",
                "complained",
                &["attacker-selected@example.com".to_string()],
                None,
                Some(temps_email::SuppressionReason::Complained),
            )
            .await;
        assert!(matches!(
            mismatch,
            Err(EmailTrackingError::RecipientMismatch { .. })
        ));
        let wrong_configured_topic = event_service
            .process_sns_event(
                &suppression_service,
                "arn:aws:sns:us-east-1:123456789012:other-configured-provider",
                "sns-wrong-provider-topic",
                "known-provider-message",
                "complained",
                &["intended@example.com".to_string()],
                None,
                Some(temps_email::SuppressionReason::Complained),
            )
            .await;
        assert!(matches!(
            wrong_configured_topic,
            Err(EmailTrackingError::TopicMismatch { .. })
        ));
        assert_eq!(
            email_events::Entity::find()
                .count(db.db.as_ref())
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            suppressed_recipients::Entity::find()
                .count(db.db.as_ref())
                .await
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn sns_processing_is_transactional_and_replay_safe() {
        let Some(db) = setup_database().await else {
            return;
        };
        let event_service = EmailEventService::new(db.db.clone());
        let suppression_service = temps_email::SuppressionService::new(db.db.clone());
        let domain_id = create_domain(db.db.as_ref(), "replay").await;
        let email_id = create_email_with_recipients(
            db.db.as_ref(),
            "provider-message-replay",
            Some(domain_id),
            &["recipient@example.com"],
        )
        .await;
        let args_recipients = [
            " Recipient@Example.COM ".to_string(),
            "recipient@example.com".to_string(),
        ];

        let first = event_service
            .process_sns_event(
                &suppression_service,
                "arn:aws:sns:us-east-1:123456789012:temps-ses",
                "sns-message-replay-safe",
                "provider-message-replay",
                "complained",
                &args_recipients,
                None,
                Some(temps_email::SuppressionReason::Complained),
            )
            .await
            .unwrap();
        let replay = event_service
            .process_sns_event(
                &suppression_service,
                "arn:aws:sns:us-east-1:123456789012:temps-ses",
                "sns-message-replay-safe",
                "provider-message-replay",
                "complained",
                &args_recipients,
                None,
                Some(temps_email::SuppressionReason::Complained),
            )
            .await
            .unwrap();

        assert_eq!(first, SnsProcessingOutcome::Processed);
        assert_eq!(replay, SnsProcessingOutcome::AlreadyProcessed);
        let events = email_events::Entity::find()
            .all(db.db.as_ref())
            .await
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].email_id, email_id);
        let suppressions = suppressed_recipients::Entity::find()
            .all(db.db.as_ref())
            .await
            .unwrap();
        assert_eq!(suppressions.len(), 1);
        assert_eq!(suppressions[0].domain_id, domain_id);
        assert_eq!(suppressions[0].email, "recipient@example.com");
    }

    #[tokio::test]
    async fn one_sns_notification_can_persist_multiple_distinct_recipients() {
        let Some(db) = setup_database().await else {
            return;
        };
        let event_service = EmailEventService::new(db.db.clone());
        let suppression_service = temps_email::SuppressionService::new(db.db.clone());
        let domain_id = create_domain(db.db.as_ref(), "multiple-recipients").await;
        create_email_with_recipients(
            db.db.as_ref(),
            "provider-message-multiple",
            Some(domain_id),
            &["first@example.com", "second@example.com"],
        )
        .await;

        let result = event_service
            .process_sns_event(
                &suppression_service,
                "arn:aws:sns:us-east-1:123456789012:temps-ses",
                "sns-message-multiple",
                "provider-message-multiple",
                "bounced",
                &[
                    "first@example.com".to_string(),
                    "second@example.com".to_string(),
                ],
                None,
                Some(temps_email::SuppressionReason::Bounced),
            )
            .await
            .unwrap();

        assert_eq!(result, SnsProcessingOutcome::Processed);
        assert_eq!(
            email_events::Entity::find()
                .count(db.db.as_ref())
                .await
                .unwrap(),
            2
        );
        assert_eq!(
            suppressed_recipients::Entity::find()
                .count(db.db.as_ref())
                .await
                .unwrap(),
            2
        );
    }

    #[tokio::test]
    async fn sns_processing_holds_provider_lock_until_event_commit() {
        let Some(db) = setup_database().await else {
            return;
        };
        let domain_id = create_domain(db.db.as_ref(), "provider-lock").await;
        let domain = email_domains::Entity::find_by_id(domain_id)
            .one(db.db.as_ref())
            .await
            .unwrap()
            .unwrap();
        create_email_with_recipients(
            db.db.as_ref(),
            "provider-message-lock",
            Some(domain_id),
            &["recipient@example.com"],
        )
        .await;

        db.db
            .execute_unprepared(
                r#"
                CREATE FUNCTION delay_sns_event_insert() RETURNS trigger AS $$
                BEGIN
                    PERFORM pg_sleep(0.75);
                    RETURN NEW;
                END;
                $$ LANGUAGE plpgsql;
                CREATE TRIGGER delay_sns_event_insert
                    BEFORE INSERT ON email_events
                    FOR EACH ROW EXECUTE FUNCTION delay_sns_event_insert();
                "#,
            )
            .await
            .unwrap();

        let event_service = EmailEventService::new(db.db.clone());
        let suppression_service = temps_email::SuppressionService::new(db.db.clone());
        let processing = tokio::spawn(async move {
            event_service
                .process_sns_event(
                    &suppression_service,
                    TEST_TOPIC_ARN,
                    "sns-message-lock",
                    "provider-message-lock",
                    "delivered",
                    &["recipient@example.com".to_string()],
                    None,
                    None,
                )
                .await
        });

        // Wait until the insert trigger is sleeping. The provider FOR SHARE
        // lock has already been acquired at this point.
        let mut trigger_is_sleeping = false;
        for _ in 0..50 {
            let row = db
                .db
                .query_one(sea_orm::Statement::from_string(
                    sea_orm::DatabaseBackend::Postgres,
                    "SELECT EXISTS (SELECT 1 FROM pg_stat_activity \
                     WHERE pid <> pg_backend_pid() AND wait_event = 'PgSleep') AS sleeping"
                        .to_string(),
                ))
                .await
                .unwrap()
                .unwrap();
            trigger_is_sleeping = row.try_get("", "sleeping").unwrap();
            if trigger_is_sleeping {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(
            trigger_is_sleeping,
            "SNS insert trigger never entered pg_sleep"
        );

        let started = std::time::Instant::now();
        email_providers::Entity::update_many()
            .col_expr(email_providers::Column::IsActive, false.into())
            .filter(email_providers::Column::Id.eq(domain.provider_id))
            .exec(db.db.as_ref())
            .await
            .unwrap();
        assert!(
            started.elapsed() >= std::time::Duration::from_millis(300),
            "provider update was not blocked by SNS authorization lock"
        );
        assert_eq!(
            processing.await.unwrap().unwrap(),
            SnsProcessingOutcome::Processed
        );
    }
}
