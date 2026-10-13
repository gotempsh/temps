// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Give every webhook delivery a stable identity shared by all its attempts.
//!
//! `webhook_deliveries` holds one row per attempt. Before this migration a
//! manual retry inserted a row that looked like a brand-new delivery
//! (`attempt_number = 1`, new row id) and bumped the failed row to
//! `attempt_number = 2`, so a receiver saw a different `X-Webhook-Delivery`
//! on every attempt. `delivery_id` is the id of a delivery's first attempt
//! and is carried unchanged by its retries.
//!
//! Backfill: rows are grouped by `(webhook_id, event_id)` -- an event id is a
//! fresh UUID per event and a webhook receives each event at most once, so a
//! group can only be an original send plus its retries -- and every row gets
//! the smallest id of its group. Attempts are then renumbered 1, 2, 3... in
//! row-id (that is, chronological) order, which repairs the old retried pair
//! that read "failed = 2, retry = 1" and makes `(webhook_id, delivery_id,
//! attempt_number)` unique, so two concurrent retries of one delivery cannot
//! both claim the same attempt number.

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
ALTER TABLE webhook_deliveries ADD COLUMN IF NOT EXISTS delivery_id INTEGER;

UPDATE webhook_deliveries AS d
SET delivery_id = g.first_id
FROM (
    SELECT webhook_id, event_id, MIN(id) AS first_id
    FROM webhook_deliveries
    GROUP BY webhook_id, event_id
) AS g
WHERE d.delivery_id IS NULL
  AND d.webhook_id = g.webhook_id
  AND d.event_id = g.event_id;

UPDATE webhook_deliveries AS d
SET attempt_number = r.attempt
FROM (
    SELECT id,
           ROW_NUMBER() OVER (
               PARTITION BY webhook_id, delivery_id
               ORDER BY id
           )::INTEGER AS attempt
    FROM webhook_deliveries
    WHERE delivery_id IS NOT NULL
) AS r
WHERE d.id = r.id
  AND d.attempt_number IS DISTINCT FROM r.attempt;

CREATE UNIQUE INDEX IF NOT EXISTS idx_webhook_deliveries_attempt
    ON webhook_deliveries(webhook_id, delivery_id, attempt_number)
    WHERE delivery_id IS NOT NULL;
"#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
DROP INDEX IF EXISTS idx_webhook_deliveries_attempt;
DROP INDEX IF EXISTS idx_webhook_deliveries_webhook_delivery;
ALTER TABLE webhook_deliveries DROP COLUMN IF EXISTS delivery_id;
"#,
            )
            .await?;
        Ok(())
    }
}
