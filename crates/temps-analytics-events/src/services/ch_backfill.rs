// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! One-shot ClickHouse backfill helper.
//!
//! Reuses the exact `ChEventRow` shape and `row_to_ch` mapper from the
//! live fan-out worker, but reads events directly from PostgreSQL by a
//! `[from, to]` timestamp window rather than draining the outbox. Intended
//! for the standalone `temps backfill clickhouse` subcommand:
//!
//! - Runs out-of-process from `temps serve`, so it never contends with
//!   live writes for outbox row locks.
//! - Does not enqueue into `events_ch_outbox`, so it does not double the
//!   PG write load on the primary while it works.
//! - Safe to re-run: ClickHouse's `ReplacingMergeTree(_version)` over
//!   `event_id` collapses duplicates. At-least-once is fine.
//!
//! See ADR-012 for why CH is bring-your-own and why this is a tooling
//! concern, not part of the live ingest path.
//!
//! Migration entry point: [`apply_clickhouse_schema`] re-exports the
//! `temps-analytics-backend` migrations runner so the CLI can do
//! `temps backfill clickhouse --apply-migrations` without taking another
//! dependency.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use sea_orm::{
    ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder,
    QuerySelect,
};
use tracing::debug;

use super::ch_fanout::{row_to_ch, ChEventRow};
use temps_core::DBDateTime;
use temps_entities::{events, ip_geolocations};

const WINDOW_LOG_TARGET: &str = "temps_analytics_events::services::ch_backfill::window";

/// Errors surfaced by the backfill helper. Mirrors `ChFanoutError` but is a
/// distinct type because the surface is much narrower (no orphans, no
/// outbox, no dead-letters).
#[derive(Debug, thiserror::Error)]
pub enum ChBackfillError {
    #[error("Database error during backfill: {0}")]
    Database(#[from] sea_orm::DbErr),

    #[error("ClickHouse insert failed for batch starting at event_id {first_event_id}: {reason}")]
    ClickHouseInsert { first_event_id: i64, reason: String },
}

/// Cursor for keyset pagination. Iterate by `(timestamp, id)` so we
/// keep a stable order even when many events share the exact same
/// timestamp (common at high QPS). The fan-out worker doesn't need this
/// because it dequeues by outbox row id; we read from `events` directly,
/// so we need our own deterministic cursor.
#[derive(Debug, Clone, Copy, Default)]
pub struct BackfillCursor {
    pub last_timestamp: Option<DBDateTime>,
    pub last_id: Option<i64>,
}

/// Result of a single window backfill.
#[derive(Debug, Clone, Copy, Default)]
pub struct BackfillReport {
    pub events_pushed: u64,
    pub batches: u64,
    pub final_cursor: BackfillCursor,
}

/// Backfill every event in `[from, to]` (timestamp inclusive on both ends)
/// for the given `project_id` filter (or all projects when `None`) into
/// ClickHouse.
///
/// Reads in batches of `batch_size` ordered by `(timestamp, id)` ascending,
/// resolves `ip_geolocations` once per batch (same as the fan-out worker),
/// and pushes via the typed `clickhouse::Client::insert::<ChEventRow>("events")`
/// path so the row shape stays in lockstep with the live ingest.
///
/// `start_cursor` lets the caller resume from a previous run — pass
/// `BackfillCursor::default()` for a fresh start. Each completed batch
/// updates the cursor; the caller is responsible for persisting it
/// between process restarts if `--resume` is desired.
///
/// `rate_limit` (optional) sleeps `Duration` between batches so the
/// backfill doesn't saturate PG read IO on a live server.
///
/// Returns the total number of events pushed and the final cursor.
#[allow(clippy::too_many_arguments)]
pub async fn backfill_events_window(
    db: Arc<DatabaseConnection>,
    ch: Arc<::clickhouse::Client>,
    from: DBDateTime,
    to: DBDateTime,
    project_id: Option<i32>,
    batch_size: u64,
    start_cursor: BackfillCursor,
    rate_limit: Option<Duration>,
) -> Result<BackfillReport, ChBackfillError> {
    // This helper runs once per CLI chunk while indicatif owns the terminal.
    // Keep routine window lifecycle events below INFO so they do not force the
    // interactive progress bar onto a new line for every chunk.
    debug!(
        target: WINDOW_LOG_TARGET,
        from = %from,
        to = %to,
        project_id = ?project_id,
        batch_size,
        "ch_backfill starting window"
    );

    let mut cursor = start_cursor;
    let mut total: u64 = 0;
    let mut batches: u64 = 0;

    loop {
        let batch = fetch_next_batch(db.as_ref(), from, to, project_id, cursor, batch_size).await?;

        if batch.is_empty() {
            break;
        }

        let first_id = batch.first().map(|r| r.id).unwrap_or(0);
        let last = batch.last().expect("non-empty checked above");
        let next_cursor = BackfillCursor {
            last_timestamp: Some(last.timestamp),
            last_id: Some(last.id),
        };

        let geo_map = resolve_geo(db.as_ref(), &batch).await?;
        push_batch(ch.as_ref(), &batch, &geo_map, first_id).await?;

        let count = batch.len() as u64;
        total += count;
        batches += 1;
        cursor = next_cursor;

        debug!(
            count,
            total,
            batches,
            last_timestamp = %last.timestamp,
            last_id = last.id,
            "ch_backfill pushed batch"
        );

        if let Some(d) = rate_limit {
            tokio::time::sleep(d).await;
        }

        if count < batch_size {
            break;
        }
    }

    debug!(
        target: WINDOW_LOG_TARGET,
        events_pushed = total,
        batches, "ch_backfill window complete"
    );

    Ok(BackfillReport {
        events_pushed: total,
        batches,
        final_cursor: cursor,
    })
}

/// Count the rows the backfill *would* push for the given window. Used by
/// the CLI for the progress bar denominator and by `--dry-run`.
pub async fn count_events_window(
    db: &DatabaseConnection,
    from: DBDateTime,
    to: DBDateTime,
    project_id: Option<i32>,
) -> Result<u64, ChBackfillError> {
    let mut q = events::Entity::find()
        .filter(events::Column::Timestamp.gte(from))
        .filter(events::Column::Timestamp.lte(to));
    if let Some(pid) = project_id {
        q = q.filter(events::Column::ProjectId.eq(pid));
    }
    let n = q.count(db).await?;
    Ok(n)
}

/// Apply the embedded ClickHouse schema migrations (`events`,
/// `events_5m_mv`, `sessions`). Idempotent; safe to invoke on every
/// backfill run. Surfaced here so the CLI doesn't have to take a direct
/// dependency on `temps-analytics-backend`.
pub async fn apply_clickhouse_schema(
    client: &::clickhouse::Client,
) -> Result<temps_analytics_backend::migrations::MigrationReport, anyhow::Error> {
    temps_analytics_backend::migrations::apply_migrations(client)
        .await
        .map_err(|e| anyhow::anyhow!("ClickHouse migrations failed: {}", e))
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

async fn fetch_next_batch(
    db: &DatabaseConnection,
    from: DBDateTime,
    to: DBDateTime,
    project_id: Option<i32>,
    cursor: BackfillCursor,
    batch_size: u64,
) -> Result<Vec<events::Model>, ChBackfillError> {
    use sea_orm::Condition;

    let mut q = events::Entity::find()
        .filter(events::Column::Timestamp.gte(from))
        .filter(events::Column::Timestamp.lte(to));

    if let Some(pid) = project_id {
        q = q.filter(events::Column::ProjectId.eq(pid));
    }

    // Keyset condition: (timestamp, id) > (last_ts, last_id).
    // Expressed as `(ts > last_ts) OR (ts = last_ts AND id > last_id)`
    // so it can use the `(timestamp, id)` index.
    if let (Some(ts), Some(id)) = (cursor.last_timestamp, cursor.last_id) {
        let cond = Condition::any().add(events::Column::Timestamp.gt(ts)).add(
            Condition::all()
                .add(events::Column::Timestamp.eq(ts))
                .add(events::Column::Id.gt(id)),
        );
        q = q.filter(cond);
    }

    let rows = q
        .order_by_asc(events::Column::Timestamp)
        .order_by_asc(events::Column::Id)
        .limit(batch_size)
        .all(db)
        .await?;

    Ok(rows)
}

async fn resolve_geo(
    db: &DatabaseConnection,
    rows: &[events::Model],
) -> Result<HashMap<i32, ip_geolocations::Model>, ChBackfillError> {
    let geo_ids: HashSet<i32> = rows.iter().filter_map(|r| r.ip_geolocation_id).collect();
    if geo_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let geo_ids: Vec<i32> = geo_ids.into_iter().collect();
    let map = ip_geolocations::Entity::find()
        .filter(ip_geolocations::Column::Id.is_in(geo_ids))
        .all(db)
        .await?
        .into_iter()
        .map(|m| (m.id, m))
        .collect();
    Ok(map)
}

async fn push_batch(
    ch: &::clickhouse::Client,
    rows: &[events::Model],
    geo_map: &HashMap<i32, ip_geolocations::Model>,
    first_id: i64,
) -> Result<(), ChBackfillError> {
    let mut inserter =
        ch.insert::<ChEventRow>("events")
            .await
            .map_err(|e| ChBackfillError::ClickHouseInsert {
                first_event_id: first_id,
                reason: format!("inserter setup failed: {e}"),
            })?;

    for r in rows {
        let geo = r.ip_geolocation_id.and_then(|id| geo_map.get(&id));
        inserter.write(&row_to_ch(r, geo)).await.map_err(|e| {
            ChBackfillError::ClickHouseInsert {
                first_event_id: first_id,
                reason: format!("write failed: {e}"),
            }
        })?;
    }
    inserter
        .end()
        .await
        .map_err(|e| ChBackfillError::ClickHouseInsert {
            first_event_id: first_id,
            reason: format!("end failed: {e}"),
        })?;

    Ok(())
}

// Re-exports for the CLI so it can pretty-print and build the CH client
// without taking a direct dependency on `temps-analytics-backend`.
pub use temps_analytics_backend::clickhouse::{ClickHouseBackend, ClickHouseConfig};
pub use temps_analytics_backend::migrations::MigrationReport;

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use sea_orm::{DatabaseBackend, MockDatabase};
    use tracing::{instrument::WithSubscriber, Level};
    use tracing_subscriber::{layer::SubscriberExt, Layer};

    use super::*;

    #[derive(Clone, Default)]
    struct CapturedEvents(Arc<Mutex<Vec<(String, Level)>>>);

    impl<S> Layer<S> for CapturedEvents
    where
        S: tracing::Subscriber,
    {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            self.0
                .lock()
                .expect("captured-level mutex should not be poisoned")
                .push((
                    event.metadata().target().to_string(),
                    *event.metadata().level(),
                ));
        }
    }

    #[tokio::test]
    async fn test_backfill_events_window_empty_window_emits_only_debug_lifecycle_events() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results::<events::Model, _, _>(vec![vec![]])
                .into_connection(),
        );
        let captured = CapturedEvents::default();
        let subscriber = tracing_subscriber::registry().with(captured.clone());
        let now = chrono::Utc::now();

        let report = async {
            backfill_events_window(
                db,
                Arc::new(::clickhouse::Client::default()),
                now,
                now,
                None,
                10_000,
                BackfillCursor::default(),
                None,
            )
            .await
        }
        .with_subscriber(subscriber)
        .await
        .expect("empty backfill window should succeed");

        assert_eq!(report.events_pushed, 0);
        assert_eq!(report.batches, 0);
        let events = captured
            .0
            .lock()
            .expect("captured-level mutex should not be poisoned");
        let lifecycle_levels: Vec<Level> = events
            .iter()
            .filter(|(target, _)| target == WINDOW_LOG_TARGET)
            .map(|(_, level)| *level)
            .collect();
        assert_eq!(
            lifecycle_levels,
            vec![Level::DEBUG, Level::DEBUG],
            "routine per-window events must stay below INFO so they cannot corrupt the CLI progress bar"
        );
    }
}
