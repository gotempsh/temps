//! Normalizes `email_events.event_type` to a single, past-tense convention.
//!
//! Two code paths wrote to this shared table with different strings:
//! `temps-email-tracking` (pixel/click/SES-webhook) wrote `"opened"`,
//! `"clicked"`, `"bounced"`, `"complained"`, `"delivered"`; `temps-email`'s
//! own `TrackingService` wrote `"open"`/`"click"` for the same event types.
//! Any query or dashboard built against one convention silently missed rows
//! written under the other. The application code has been fixed to always
//! write the past-tense form (matching what the SES webhook already
//! produced and what the web UI's event icon/badge components already
//! expect); this migration backfills any rows a live deployment already
//! wrote under the old present-tense strings.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260711_000001_normalize_email_event_types"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                UPDATE email_events
                SET event_type = CASE event_type
                    WHEN 'open' THEN 'opened'
                    WHEN 'click' THEN 'clicked'
                    WHEN 'bounce' THEN 'bounced'
                    WHEN 'complaint' THEN 'complained'
                END
                WHERE event_type IN ('open', 'click', 'bounce', 'complaint');
                "#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Best-effort reverse mapping. Cannot distinguish rows that were
        // already past-tense before `up()` ran from ones this migration
        // rewrote, so this is a lossy revert of the naming convention, not
        // of any data — acceptable since these are just label strings.
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                UPDATE email_events
                SET event_type = CASE event_type
                    WHEN 'opened' THEN 'open'
                    WHEN 'clicked' THEN 'click'
                    WHEN 'bounced' THEN 'bounce'
                    WHEN 'complained' THEN 'complaint'
                END
                WHERE event_type IN ('opened', 'clicked', 'bounced', 'complained');
                "#,
            )
            .await?;
        Ok(())
    }
}
