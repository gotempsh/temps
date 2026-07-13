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
                UPDATE email_events SET event_type = 'opened' WHERE event_type = 'open';
                UPDATE email_events SET event_type = 'clicked' WHERE event_type = 'click';
                UPDATE email_events SET event_type = 'bounced' WHERE event_type = 'bounce';
                UPDATE email_events SET event_type = 'complained' WHERE event_type = 'complaint';
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
                UPDATE email_events SET event_type = 'open' WHERE event_type = 'opened';
                UPDATE email_events SET event_type = 'click' WHERE event_type = 'clicked';
                UPDATE email_events SET event_type = 'bounce' WHERE event_type = 'bounced';
                UPDATE email_events SET event_type = 'complaint' WHERE event_type = 'complained';
                "#,
            )
            .await?;
        Ok(())
    }
}
