// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Order revenue subscription state by event time.
//!
//! `revenue_subscriptions_state` was projected from every webhook event in
//! arrival order, so a provider's out-of-order delivery (or a replay by a
//! signing-secret holder) of an older "active" event revived a subscription
//! a newer event had canceled. `last_event_at` records the `occurred_at` of
//! the newest event applied to the row, and ingestion refuses to project an
//! older one.
//!
//! Existing rows are backfilled from the event log with the newest
//! `occurred_at` of a *lifecycle* event (one that carries a
//! `subscription_status`) per subscription, which is the value ingestion
//! itself would have recorded: invoice and realized-MRR events have no status
//! and never move the fence, so seeding it from them would make a delayed
//! older cancellation look stale. A row with no logged lifecycle event (CSV
//! import, or invoices only) stays NULL, which means "no fence yet": the next
//! event is applied and sets it, exactly as before this migration. The
//! backfill is one grouped pass over `revenue_events`, not a per-row subquery.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                ALTER TABLE revenue_subscriptions_state
                    ADD COLUMN IF NOT EXISTS last_event_at TIMESTAMPTZ NULL;

                UPDATE revenue_subscriptions_state AS s
                SET last_event_at = e.newest
                FROM (
                    SELECT integration_id, subscription_ref, MAX(occurred_at) AS newest
                    FROM revenue_events
                    WHERE subscription_ref IS NOT NULL
                      AND subscription_status IS NOT NULL
                    GROUP BY integration_id, subscription_ref
                ) AS e
                WHERE e.integration_id = s.integration_id
                  AND e.subscription_ref = s.provider_subscription_id
                  AND s.last_event_at IS NULL;
                "#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE revenue_subscriptions_state DROP COLUMN IF EXISTS last_event_at",
            )
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{Database, DatabaseBackend, Statement};
    use testcontainers::{core::WaitFor, runners::AsyncRunner, GenericImage, ImageExt};

    #[tokio::test]
    async fn backfill_fences_on_lifecycle_events_only() -> Result<(), Box<dyn std::error::Error>> {
        let Ok(docker) = bollard::Docker::connect_with_local_defaults() else {
            eprintln!("Docker unavailable; skipping revenue fence backfill integration");
            return Ok(());
        };
        if docker.ping().await.is_err() {
            eprintln!("Docker unavailable; skipping revenue fence backfill integration");
            return Ok(());
        }
        let container = GenericImage::new("postgres", "17-alpine")
            .with_wait_for(WaitFor::message_on_stderr(
                "database system is ready to accept connections",
            ))
            .with_env_var("POSTGRES_PASSWORD", "revenue-fence-test")
            .start()
            .await?;
        let port = container.get_host_port_ipv4(5432).await?;
        let db = Database::connect(format!(
            "postgres://postgres:revenue-fence-test@127.0.0.1:{port}/postgres"
        ))
        .await?;
        // Only the columns the backfill reads. `sub_mixed` has a lifecycle
        // event at 10:00 and a later status-less invoice at 10:05;
        // `sub_invoice_only` has nothing but invoices; `sub_csv` has no events.
        db.execute_unprepared(
            "CREATE TABLE revenue_subscriptions_state(integration_id INTEGER, provider_subscription_id TEXT); \
             CREATE TABLE revenue_events(integration_id INTEGER, subscription_ref TEXT, subscription_status TEXT, occurred_at TIMESTAMPTZ); \
             INSERT INTO revenue_subscriptions_state VALUES (1,'sub_mixed'),(1,'sub_invoice_only'),(1,'sub_csv'); \
             INSERT INTO revenue_events VALUES \
               (1,'sub_mixed','active','2026-01-01T09:00:00Z'), \
               (1,'sub_mixed','active','2026-01-01T10:00:00Z'), \
               (1,'sub_mixed',NULL,'2026-01-01T10:05:00Z'), \
               (1,'sub_invoice_only',NULL,'2026-01-01T11:00:00Z'), \
               (2,'sub_mixed','canceled','2026-01-01T12:00:00Z');",
        )
        .await?;

        Migration.up(&SchemaManager::new(&db)).await?;

        let fence = |sub: &'static str| {
            let db = &db;
            async move {
                let row = db
                    .query_one(Statement::from_sql_and_values(
                        DatabaseBackend::Postgres,
                        "SELECT to_char(last_event_at AT TIME ZONE 'UTC', 'HH24:MI') AS fence \
                         FROM revenue_subscriptions_state \
                         WHERE integration_id = 1 AND provider_subscription_id = $1",
                        [sub.into()],
                    ))
                    .await?
                    .ok_or_else(|| DbErr::RecordNotFound(format!("subscription {sub}")))?;
                row.try_get::<Option<String>>("", "fence")
            }
        };
        // The invoice at 10:05 and the other integration's event at 12:00
        // must not move the fence past the newest lifecycle event.
        assert_eq!(fence("sub_mixed").await?.as_deref(), Some("10:00"));
        assert_eq!(fence("sub_invoice_only").await?, None);
        assert_eq!(fence("sub_csv").await?, None);
        Ok(())
    }
}
