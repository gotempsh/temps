//! Integration tests for the SES event-tracking setup pipeline.
//!
//! Requires Docker: PostgreSQL (via `TestDatabase`) for persistence tests
//! and LocalStack for the AWS-side provisioning test. Every test skips
//! gracefully when Docker is unavailable.
//!
//! LocalStack community edition fully implements SNS but not SESv2, so the
//! provisioning test always asserts the SNS side (topic created, ARN
//! persisted *before* subscribing, webhook subscription requested) and
//! treats the SESv2 event-destination step as best-effort: verified when
//! LocalStack supports it, skipped with a note when it does not.

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use sea_orm::{ActiveModelTrait, ActiveValue::Set, DatabaseConnection};
    use temps_core::EncryptionService;
    use temps_database::test_utils::TestDatabase;
    use temps_entities::{email_domains, email_events, email_providers, emails};
    use uuid::Uuid;

    use crate::providers::{EmailProviderType, SesCredentials};
    use crate::services::{
        CreateProviderRequest, ProviderCredentials, ProviderService, TrackingSetupService,
    };

    const WEBHOOK_URL: &str = "http://host.docker.internal:1/api/t/webhook/ses";

    fn encryption_service() -> Arc<EncryptionService> {
        let key = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        Arc::new(EncryptionService::new(key).unwrap())
    }

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
                eprintln!("Docker unavailable, skipping tracking-setup test: {error:#}");
                None
            }
            Err(error) => panic!("tracking-setup test database setup failed: {error:#}"),
        }
    }

    async fn create_ses_provider(
        provider_service: &ProviderService,
        endpoint_url: Option<String>,
    ) -> email_providers::Model {
        provider_service
            .create(CreateProviderRequest {
                name: "Tracking setup test".to_string(),
                provider_type: EmailProviderType::Ses,
                region: "us-east-1".to_string(),
                credentials: ProviderCredentials::Ses(SesCredentials {
                    access_key_id: "test".to_string(),
                    secret_access_key: "test".to_string(),
                    endpoint_url,
                }),
            })
            .await
            .unwrap()
    }

    // =====================================================================
    // PostgreSQL-side behavior
    // =====================================================================

    #[tokio::test]
    async fn subscription_confirmation_is_recorded_and_cleared_on_topic_rotation() {
        let Some(db) = setup_database().await else {
            return;
        };
        let provider_service = ProviderService::new(db.db.clone(), encryption_service());

        let provider = create_ses_provider(&provider_service, None).await;
        let topic_arn = "arn:aws:sns:us-east-1:123456789012:temps-events";
        provider_service
            .update_with_sns_topic(
                provider.id,
                Default::default(),
                Some(Some(topic_arn.to_string())),
            )
            .await
            .unwrap();

        // Freshly bound topic: not confirmed yet.
        let row = provider_service.get(provider.id).await.unwrap();
        assert_eq!(row.sns_topic_arn.as_deref(), Some(topic_arn));
        assert!(row.sns_subscription_confirmed_at.is_none());

        // A SubscriptionConfirmation for the bound topic marks it confirmed.
        provider_service
            .mark_sns_subscription_confirmed(topic_arn)
            .await
            .unwrap();
        let row = provider_service.get(provider.id).await.unwrap();
        assert!(row.sns_subscription_confirmed_at.is_some());

        // Confirmations for other topics don't touch this provider.
        provider_service
            .mark_sns_subscription_confirmed("arn:aws:sns:us-east-1:123456789012:other")
            .await
            .unwrap();
        let row = provider_service.get(provider.id).await.unwrap();
        assert!(row.sns_subscription_confirmed_at.is_some());

        // Rotating the topic invalidates the recorded confirmation.
        provider_service
            .update_with_sns_topic(
                provider.id,
                Default::default(),
                Some(Some(
                    "arn:aws:sns:us-east-1:123456789012:temps-events-2".to_string(),
                )),
            )
            .await
            .unwrap();
        let row = provider_service.get(provider.id).await.unwrap();
        assert!(row.sns_subscription_confirmed_at.is_none());
    }

    async fn insert_event(
        db: &DatabaseConnection,
        email_id: Uuid,
        event_type: &str,
        created_at: temps_core::DBDateTime,
    ) {
        email_events::ActiveModel {
            email_id: Set(email_id),
            event_type: Set(event_type.to_string()),
            created_at: Set(created_at),
            ..Default::default()
        }
        .insert(db)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn last_provider_event_at_reports_provider_feedback_only() {
        let Some(db) = setup_database().await else {
            return;
        };
        let provider_service = Arc::new(ProviderService::new(db.db.clone(), encryption_service()));
        let setup_service = TrackingSetupService::new(provider_service.clone(), db.db.clone());

        let provider = create_ses_provider(&provider_service, None).await;

        // No domains yet → no events.
        assert!(setup_service
            .last_provider_event_at(provider.id)
            .await
            .unwrap()
            .is_none());

        let domain = email_domains::ActiveModel {
            provider_id: Set(provider.id),
            domain: Set("tracking-setup.example.com".to_string()),
            status: Set("verified".to_string()),
            ..Default::default()
        }
        .insert(db.db.as_ref())
        .await
        .unwrap();

        let email_id = Uuid::new_v4();
        emails::ActiveModel {
            id: Set(email_id),
            domain_id: Set(Some(domain.id)),
            from_address: Set("sender@tracking-setup.example.com".to_string()),
            to_addresses: Set(serde_json::json!(["r@example.com"])),
            subject: Set("Tracking setup test".to_string()),
            status: Set("sent".to_string()),
            provider_message_id: Set(Some("ses-message-1".to_string())),
            track_opens: Set(false),
            track_clicks: Set(false),
            open_count: Set(0),
            click_count: Set(0),
            ..Default::default()
        }
        .insert(db.db.as_ref())
        .await
        .unwrap();

        let base = chrono::Utc::now();

        // Pixel-driven events must not count as provider feedback.
        insert_event(db.db.as_ref(), email_id, "opened", base).await;
        assert!(setup_service
            .last_provider_event_at(provider.id)
            .await
            .unwrap()
            .is_none());

        let delivered_at = base - chrono::Duration::minutes(10);
        let bounced_at = base - chrono::Duration::minutes(5);
        insert_event(db.db.as_ref(), email_id, "delivered", delivered_at).await;
        insert_event(db.db.as_ref(), email_id, "bounced", bounced_at).await;

        let last = setup_service
            .last_provider_event_at(provider.id)
            .await
            .unwrap()
            .expect("provider feedback events should be found");
        assert_eq!(last.timestamp(), bounced_at.timestamp());
    }

    // =====================================================================
    // AWS-side provisioning against LocalStack
    // =====================================================================

    async fn start_localstack() -> Option<(
        testcontainers::ContainerAsync<testcontainers::GenericImage>,
        String,
    )> {
        use testcontainers::{runners::AsyncRunner, GenericImage, ImageExt};

        // Pinned to 3.0: newer LocalStack images require a Pro license key.
        let container = match GenericImage::new("localstack/localstack", "3.0")
            .with_env_var("SERVICES", "sns,ses,sesv2")
            .start()
            .await
        {
            Ok(c) => c,
            Err(e) => {
                eprintln!("Failed to start LocalStack ({e}), skipping AWS provisioning test");
                return None;
            }
        };
        let port = match container.get_host_port_ipv4(4566).await {
            Ok(p) => p,
            Err(e) => {
                eprintln!("Failed to resolve LocalStack port ({e}), skipping");
                return None;
            }
        };
        let endpoint = format!("http://127.0.0.1:{port}");

        // Wait for readiness — LocalStack answers /_localstack/health once up.
        let client = reqwest::Client::new();
        for _ in 0..60 {
            if let Ok(resp) = client
                .get(format!("{endpoint}/_localstack/health"))
                .send()
                .await
            {
                if resp.status().is_success() {
                    return Some((container, endpoint));
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
        eprintln!("LocalStack did not become healthy in 60s, skipping");
        None
    }

    fn is_missing_sesv2_support(message: &str) -> bool {
        let m = message.to_lowercase();
        m.contains("not yet implemented")
            || m.contains("not implemented")
            || m.contains("internalfailure")
            || m.contains("501")
            || m.contains("api action")
    }

    #[tokio::test]
    async fn localstack_setup_provisions_sns_and_persists_topic_before_subscribing() {
        let Some(db) = setup_database().await else {
            return;
        };
        let Some((_container, endpoint)) = start_localstack().await else {
            return;
        };

        let provider_service = Arc::new(ProviderService::new(db.db.clone(), encryption_service()));
        let setup_service = TrackingSetupService::new(provider_service.clone(), db.db.clone());
        let provider = create_ses_provider(&provider_service, Some(endpoint.clone())).await;

        let outcome = setup_service
            .setup_ses_event_tracking(provider.id, WEBHOOK_URL)
            .await;

        // The SNS half must have completed regardless of SESv2 support:
        // topic created, ARN persisted on the provider row (before the
        // subscription — the ordering the auto-confirm flow depends on),
        // webhook subscription requested.
        let row = provider_service.get(provider.id).await.unwrap();
        let topic_arn = row
            .sns_topic_arn
            .clone()
            .expect("topic ARN must be persisted even if the SESv2 step is unsupported");
        assert!(
            topic_arn.contains(&format!("temps-email-events-{}", provider.id)),
            "unexpected topic ARN: {topic_arn}"
        );
        // A fresh topic binding is never marked confirmed.
        assert!(row.sns_subscription_confirmed_at.is_none());

        let creds = SesCredentials {
            access_key_id: "test".to_string(),
            secret_access_key: "test".to_string(),
            endpoint_url: Some(endpoint.clone()),
        };
        let sns = super::super::tracking_setup_service::test_support::sns_client_for_tests(
            &creds,
            "us-east-1",
        )
        .await;
        let subs = sns
            .list_subscriptions_by_topic()
            .topic_arn(&topic_arn)
            .send()
            .await
            .unwrap();
        assert!(
            subs.subscriptions()
                .iter()
                .any(|s| s.endpoint() == Some(WEBHOOK_URL)),
            "webhook subscription missing from topic"
        );

        match outcome {
            Ok(result) => {
                assert_eq!(result.topic_arn, topic_arn);
                assert!(result.subscription_requested);
                assert!(result.event_destination_attached);
            }
            Err(error) if is_missing_sesv2_support(&error.to_string()) => {
                eprintln!(
                    "LocalStack lacks SESv2 support — SNS side verified, \
                     skipping event-destination assertion: {error}"
                );
                return;
            }
            Err(error) => panic!("setup failed for a non-SESv2 reason: {error}"),
        }

        // Idempotency: a second run succeeds and keeps the same topic.
        let second = setup_service
            .setup_ses_event_tracking(provider.id, WEBHOOK_URL)
            .await
            .expect("re-running setup must be idempotent");
        assert_eq!(second.topic_arn, topic_arn);
        let row = provider_service.get(provider.id).await.unwrap();
        assert_eq!(row.sns_topic_arn.as_deref(), Some(topic_arn.as_str()));
    }
}
