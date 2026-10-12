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
//! the smallest id of its group. Existing `attempt_number` values are left
//! exactly as stored (a legacy retried pair keeps its odd 2 / 1 numbering
//! rather than being rewritten); only the new column is populated, and new
//! attempts continue after the group's highest attempt number.

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

CREATE INDEX IF NOT EXISTS idx_webhook_deliveries_webhook_delivery
    ON webhook_deliveries(webhook_id, delivery_id);
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
DROP INDEX IF EXISTS idx_webhook_deliveries_webhook_delivery;
ALTER TABLE webhook_deliveries DROP COLUMN IF EXISTS delivery_id;
"#,
            )
            .await?;
        Ok(())
    }
}
