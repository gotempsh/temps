// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Ingestion service: verifies, persists, and projects webhook events.
//!
//! Flow for each inbound webhook:
//!   1. Look up integration by path token.
//!   2. Verify provider name in URL matches integration.provider.
//!   3. Decrypt signing secret, pass raw body + headers to provider
//!      adapter for signature verification + normalization.
//!   4. In one transaction: insert normalized event (idempotent on the
//!      unique index), upsert subscription + customer state, then flip
//!      integration to `active` on first event.

use std::sync::Arc;

use bytes::Bytes;
use chrono::Utc;
use http::HeaderMap;
use sea_orm::{
    sea_query::OnConflict, ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection,
    DatabaseTransaction, DbErr, EntityTrait, QueryFilter, TransactionTrait,
};
use temps_entities::{
    revenue_customers_state, revenue_events, revenue_integrations::Model as IntegrationModel,
    revenue_subscriptions_state,
};
use tracing::{debug, info, warn};

use crate::error::RevenueError;
use crate::providers::{
    MeteredMode, NormalizedEvent, NormalizedEventType, ProviderConfig, ProviderRegistry,
    SubscriptionStatus,
};
use crate::service::integration::RevenueIntegrationService;

/// Outcome of a single webhook call — used by the HTTP layer to pick
/// the right status code (200 vs 202).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestOutcome {
    /// Event(s) were new and persisted.
    Ingested(usize),
    /// Event(s) were all duplicates — already seen.
    Duplicate,
    /// No events matched any normalized type we track.
    Ignored,
}

pub struct RevenueIngestionService {
    db: Arc<DatabaseConnection>,
    integrations: Arc<RevenueIntegrationService>,
    providers: ProviderRegistry,
}

impl RevenueIngestionService {
    pub fn new(
        db: Arc<DatabaseConnection>,
        integrations: Arc<RevenueIntegrationService>,
        providers: ProviderRegistry,
    ) -> Self {
        Self {
            db,
            integrations,
            providers,
        }
    }

    pub async fn ingest(
        &self,
        provider_name: &str,
        path_token: &str,
        headers: HeaderMap,
        body: Bytes,
    ) -> Result<IngestOutcome, RevenueError> {
        let integration = self.integrations.get_by_path_token(path_token).await?;

        if integration.provider != provider_name {
            return Err(RevenueError::ProviderMismatch {
                integration_id: integration.id,
                url_provider: provider_name.to_string(),
                integration_provider: integration.provider,
            });
        }

        if integration.status == "disabled" {
            // Typed sentinel — handler maps to 410 Gone so Stripe stops
            // retrying a dead integration.
            return Err(RevenueError::IntegrationDisabled {
                integration_id: integration.id,
            });
        }

        let provider = self.providers.get(&integration.provider).ok_or_else(|| {
            RevenueError::UnknownProvider {
                provider: integration.provider.clone(),
            }
        })?;

        let signing_secret = self
            .integrations
            .decrypt_signing_secret(&integration)
            .await?;

        let events = provider
            .verify_and_parse(&headers, &body, &signing_secret)
            .map_err(|source| RevenueError::Provider {
                integration_id: integration.id,
                source,
            })?;

        if events.is_empty() {
            debug!(
                integration_id = integration.id,
                "webhook accepted but event type is not tracked"
            );
            return Ok(IngestOutcome::Ignored);
        }

        let config = ProviderConfig::from_value(integration.config.as_ref());
        let events = filter_events(&config, events);

        if events.is_empty() {
            debug!(
                integration_id = integration.id,
                "all parsed events rejected by integration config"
            );
            return Ok(IngestOutcome::Ignored);
        }

        let mut ingested = 0usize;
        let mut duplicates = 0usize;
        let mut latest_occurred_at = integration.last_event_at;

        let txn = self.db.begin().await?;
        for event in events {
            match persist_event(&txn, &integration, &event).await? {
                PersistResult::Inserted => ingested += 1,
                PersistResult::Duplicate => duplicates += 1,
            }
            if latest_occurred_at
                .map(|prev| event.occurred_at > prev)
                .unwrap_or(true)
            {
                latest_occurred_at = Some(event.occurred_at);
            }
        }
        txn.commit().await?;

        if ingested > 0 {
            if let Some(at) = latest_occurred_at {
                if integration.status != "active" || integration.last_event_at != Some(at) {
                    // Fire-and-forget on a best-effort basis — the event
                    // is already persisted, this is just the UI hint.
                    if let Err(e) = self.integrations.mark_active(integration.id, at).await {
                        warn!(
                            integration_id = integration.id,
                            error = %e,
                            "failed to mark integration active after first event"
                        );
                    }
                }
            }
            info!(
                integration_id = integration.id,
                ingested, duplicates, "revenue events ingested"
            );
            Ok(IngestOutcome::Ingested(ingested))
        } else {
            debug!(
                integration_id = integration.id,
                duplicates, "all events were duplicates"
            );
            Ok(IngestOutcome::Duplicate)
        }
    }
}

enum PersistResult {
    Inserted,
    Duplicate,
}

async fn persist_event(
    txn: &DatabaseTransaction,
    integration: &IntegrationModel,
    event: &NormalizedEvent,
) -> Result<PersistResult, RevenueError> {
    let event_row = revenue_events::ActiveModel {
        project_id: Set(integration.project_id),
        integration_id: Set(integration.id),
        provider: Set(integration.provider.clone()),
        provider_event_id: Set(event.provider_event_id.clone()),
        event_type: Set(event.event_type.as_str().to_string()),
        customer_ref: Set(event.customer_ref.clone()),
        subscription_ref: Set(event.subscription_ref.clone()),
        subscription_status: Set(event.subscription_status.map(|s| s.as_str().to_string())),
        mrr_minor: Set(event.mrr_minor),
        amount_minor: Set(event.amount_minor),
        currency: Set(event.currency.clone()),
        occurred_at: Set(event.occurred_at),
        payload: Set(event.raw.clone()),
        created_at: Set(Utc::now()),
        price_id: Set(event.price_id.clone()),
        product_id: Set(event.product_id.clone()),
        ..Default::default()
    };

    match event_row.insert(txn).await {
        Ok(_) => {}
        Err(DbErr::RecordNotInserted) => return Ok(PersistResult::Duplicate),
        // Duplicate key triggers a unique-violation DbErr::Exec. Treat
        // those as idempotent and continue. Other DB errors bubble up.
        Err(DbErr::Exec(runtime)) if runtime.to_string().contains("duplicate key") => {
            return Ok(PersistResult::Duplicate)
        }
        Err(DbErr::Query(runtime)) if runtime.to_string().contains("duplicate key") => {
            return Ok(PersistResult::Duplicate)
        }
        Err(other) => return Err(other.into()),
    }

    // --- Project into state tables ---
    if let Some(customer_ref) = &event.customer_ref {
        upsert_customer(txn, integration, customer_ref, event).await?;
    }

    if let Some(subscription_ref) = &event.subscription_ref {
        upsert_subscription(txn, integration, subscription_ref, event).await?;
    }

    Ok(PersistResult::Inserted)
}

async fn upsert_customer(
    txn: &DatabaseTransaction,
    integration: &IntegrationModel,
    customer_ref: &str,
    event: &NormalizedEvent,
) -> Result<(), RevenueError> {
    let now = Utc::now();
    let new_row = revenue_customers_state::ActiveModel {
        project_id: Set(integration.project_id),
        integration_id: Set(integration.id),
        provider: Set(integration.provider.clone()),
        provider_customer_ref: Set(customer_ref.to_string()),
        first_seen_at: Set(event.occurred_at.min(now)),
        churned_at: Set(None),
        updated_at: Set(now),
        ..Default::default()
    };

    // Insert or do nothing — the customer's `first_seen_at` must not
    // move forward on subsequent events. Churn is set separately below.
    let _ = revenue_customers_state::Entity::insert(new_row)
        .on_conflict(
            OnConflict::columns([
                revenue_customers_state::Column::IntegrationId,
                revenue_customers_state::Column::ProviderCustomerRef,
            ])
            .do_nothing()
            .to_owned(),
        )
        .exec_without_returning(txn)
        .await?;

    // If this event is a subscription cancellation and the customer has
    // no other active subs left, stamp churned_at. We compute this in a
    // second query rather than inline because SeaORM's insert-on-conflict
    // doesn't give us access to "do_update only when predicate holds".
    if matches!(event.event_type, NormalizedEventType::SubscriptionCanceled) {
        let has_active = revenue_subscriptions_state::Entity::find()
            .filter(revenue_subscriptions_state::Column::IntegrationId.eq(integration.id))
            .filter(revenue_subscriptions_state::Column::CustomerRef.eq(customer_ref))
            .filter(
                revenue_subscriptions_state::Column::Status
                    .is_in(vec!["active", "trialing", "past_due"]),
            )
            .one(txn)
            .await?
            .is_some();

        if !has_active {
            if let Some(existing) = revenue_customers_state::Entity::find()
                .filter(revenue_customers_state::Column::IntegrationId.eq(integration.id))
                .filter(revenue_customers_state::Column::ProviderCustomerRef.eq(customer_ref))
                .one(txn)
                .await?
            {
                let mut active: revenue_customers_state::ActiveModel = existing.into();
                active.churned_at = Set(Some(event.occurred_at));
                active.update(txn).await?;
            }
        }
    }

    Ok(())
}

async fn upsert_subscription(
    txn: &DatabaseTransaction,
    integration: &IntegrationModel,
    subscription_ref: &str,
    event: &NormalizedEvent,
) -> Result<(), RevenueError> {
    let status = event
        .subscription_status
        .unwrap_or(SubscriptionStatus::Active)
        .as_str()
        .to_string();
    let canceled_at = if matches!(event.event_type, NormalizedEventType::SubscriptionCanceled) {
        Some(event.occurred_at)
    } else {
        None
    };
    let started_at = if matches!(event.event_type, NormalizedEventType::SubscriptionCreated) {
        Some(event.occurred_at)
    } else {
        None
    };

    let existing = revenue_subscriptions_state::Entity::find()
        .filter(revenue_subscriptions_state::Column::IntegrationId.eq(integration.id))
        .filter(revenue_subscriptions_state::Column::ProviderSubscriptionId.eq(subscription_ref))
        .one(txn)
        .await?;

    match existing {
        Some(row) => {
            let mut active: revenue_subscriptions_state::ActiveModel = row.into();
            active.status = Set(status);
            if let Some(m) = event.mrr_minor {
                active.mrr_minor = Set(m);
            }
            if let Some(c) = &event.currency {
                active.currency = Set(Some(c.clone()));
            }
            if canceled_at.is_some() {
                active.canceled_at = Set(canceled_at);
            }
            active.update(txn).await?;
        }
        None => {
            let row = revenue_subscriptions_state::ActiveModel {
                project_id: Set(integration.project_id),
                integration_id: Set(integration.id),
                provider: Set(integration.provider.clone()),
                provider_subscription_id: Set(subscription_ref.to_string()),
                customer_ref: Set(event.customer_ref.clone()),
                status: Set(status),
                mrr_minor: Set(event.mrr_minor.unwrap_or(0)),
                currency: Set(event.currency.clone()),
                started_at: Set(started_at.or(Some(event.occurred_at))),
                canceled_at: Set(canceled_at),
                updated_at: Set(Utc::now()),
                ..Default::default()
            };
            row.insert(txn).await?;
        }
    }

    Ok(())
}

/// Apply the integration's per-provider config to a freshly-parsed event
/// batch.
///
/// * Drops events rejected by the price/product allowlist.
/// * Applies the metered-billing mode to subscription events (zeroing
///   `mrr_minor` or dropping the event entirely as configured). Invoice-
///   based `mrr.realized` events always flow through.
fn filter_events(
    config: &Option<ProviderConfig>,
    events: Vec<NormalizedEvent>,
) -> Vec<NormalizedEvent> {
    let Some(cfg) = config.as_ref() else {
        return events;
    };

    let metered = cfg.metered_mode();
    events
        .into_iter()
        .filter(|e| cfg.accepts(e))
        .filter_map(|mut e| match (metered, e.event_type) {
            (MeteredMode::UseSubscription, _) => Some(e),
            (MeteredMode::DeriveFromInvoices, _) => Some(e),
            (
                MeteredMode::Ignore,
                NormalizedEventType::SubscriptionCreated | NormalizedEventType::SubscriptionUpdated,
            ) if is_metered_subscription(&e) => {
                // Subscription has no fixed MRR — drop the row so the
                // state projection doesn't create a zero-MRR
                // placeholder that hides the real usage-billed amount.
                None
            }
            (MeteredMode::Ignore, NormalizedEventType::MrrRealized) => None,
            (_, _) => {
                if matches!(metered, MeteredMode::DeriveFromInvoices)
                    && matches!(
                        e.event_type,
                        NormalizedEventType::SubscriptionCreated
                            | NormalizedEventType::SubscriptionUpdated
                    )
                    && is_metered_subscription(&e)
                {
                    // Zero out subscription MRR for metered/tiered subs so
                    // the state table doesn't double-count with the
                    // MrrRealized events that ride on invoices.
                    e.mrr_minor = Some(0);
                }
                Some(e)
            }
        })
        .collect()
}

/// Heuristic: a subscription event is "metered" when our parser returned
/// `mrr_minor = Some(0)` while the status is non-canceled. The parser
/// skips tiered/metered lines — if that was the only line, total is 0.
fn is_metered_subscription(event: &NormalizedEvent) -> bool {
    event.mrr_minor == Some(0)
        && !matches!(
            event.subscription_status,
            Some(SubscriptionStatus::Canceled) | Some(SubscriptionStatus::Incomplete)
        )
}

#[cfg(test)]
mod tests {
    //! Ingestion tests focus on the decision paths that run *before* the
    //! database transaction. The transaction itself is covered by the
    //! Docker-backed integration test in `tests/ingestion_integration.rs`
    //! (skipped gracefully when Docker is unavailable).

    use super::*;
    use http::HeaderMap;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use temps_core::EncryptionService;
    use temps_entities::revenue_integrations;

    fn make_encryption() -> Arc<EncryptionService> {
        Arc::new(
            EncryptionService::new(
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            )
            .unwrap(),
        )
    }

    fn sample_integration(provider: &str, status: &str) -> revenue_integrations::Model {
        revenue_integrations::Model {
            id: 1,
            project_id: 42,
            provider: provider.to_string(),
            webhook_path_token: "token123".to_string(),
            webhook_signing_secret_encrypted: "ciphertext".to_string(),
            status: status.to_string(),
            last_event_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            config: None,
        }
    }

    fn build_service(integration: revenue_integrations::Model) -> RevenueIngestionService {
        // Mock DB returns the supplied integration for the first lookup
        // (get_by_path_token). No subsequent queries will run since the
        // pre-DB checks short-circuit in the scenarios under test.
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![integration]])
                .into_connection(),
        );
        let integrations = Arc::new(RevenueIntegrationService::new(
            db.clone(),
            make_encryption(),
            ProviderRegistry::default_registry(),
        ));
        RevenueIngestionService::new(db, integrations, ProviderRegistry::default_registry())
    }

    #[tokio::test]
    async fn provider_mismatch_between_url_and_integration() {
        let svc = build_service(sample_integration("paddle", "active"));
        let err = svc
            .ingest("stripe", "token123", HeaderMap::new(), bytes::Bytes::new())
            .await
            .unwrap_err();
        assert!(
            matches!(err, RevenueError::ProviderMismatch { .. }),
            "expected ProviderMismatch, got {:?}",
            err
        );
    }

    #[tokio::test]
    async fn disabled_integration_returns_typed_error() {
        let svc = build_service(sample_integration("stripe", "disabled"));
        let err = svc
            .ingest("stripe", "token123", HeaderMap::new(), bytes::Bytes::new())
            .await
            .unwrap_err();
        assert!(
            matches!(err, RevenueError::IntegrationDisabled { integration_id: 1 }),
            "expected typed IntegrationDisabled, got {:?}",
            err
        );
    }

    #[tokio::test]
    async fn unknown_token_surfaces_not_found() {
        // Empty query result means the token lookup fails.
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![Vec::<revenue_integrations::Model>::new()])
                .into_connection(),
        );
        let integrations = Arc::new(RevenueIntegrationService::new(
            db.clone(),
            make_encryption(),
            ProviderRegistry::default_registry(),
        ));
        let svc =
            RevenueIngestionService::new(db, integrations, ProviderRegistry::default_registry());
        let err = svc
            .ingest("stripe", "nope", HeaderMap::new(), bytes::Bytes::new())
            .await
            .unwrap_err();
        assert!(matches!(err, RevenueError::IntegrationNotFoundByToken));
    }
}
