// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Bounded workload history (ADR-055 §2).
//!
//! An event repeating the last one with the same reason and message
//! increments its `count` instead of adding a row. Each workload keeps its
//! newest [`MAX_EVENTS_PER_WORKLOAD`]; anything older than
//! [`EVENT_RETENTION`] is pruned by the reconciler's resync.

use chrono::{DateTime, Duration, Utc};
use sea_orm::{ConnectionTrait, DatabaseBackend, Statement, Value};
use temps_workload::WorkloadEvent;

use crate::error::PlaneError;

pub const MAX_EVENTS_PER_WORKLOAD: i64 = 100;
pub const EVENT_RETENTION: Duration = Duration::days(7);

fn stmt(sql: &str, values: Vec<Value>) -> Statement {
    Statement::from_sql_and_values(DatabaseBackend::Postgres, sql, values)
}

/// Record an event for `workload_id`.
pub async fn record<C: ConnectionTrait>(
    db: &C,
    workload_id: i64,
    instance_id: Option<i64>,
    reason: &str,
    message: &str,
    now: DateTime<Utc>,
) -> Result<(), PlaneError> {
    let updated = db
        .execute(stmt(
            "UPDATE workload_events SET count = count + 1, last_seen_at = $4, \
                 instance_id = COALESCE($5, instance_id) \
             WHERE id = (SELECT id FROM workload_events \
                         WHERE workload_id = $1 AND reason = $2 AND message = $3 \
                         ORDER BY last_seen_at DESC, id DESC LIMIT 1)",
            vec![
                workload_id.into(),
                reason.into(),
                message.into(),
                now.into(),
                instance_id.into(),
            ],
        ))
        .await
        .map_err(PlaneError::db("counting a repeated event", workload_id))?;
    if updated.rows_affected() > 0 {
        return Ok(());
    }
    db.execute(stmt(
        "INSERT INTO workload_events \
             (workload_id, instance_id, reason, message, count, first_seen_at, last_seen_at) \
         VALUES ($1, $2, $3, $4, 1, $5, $5)",
        vec![
            workload_id.into(),
            instance_id.into(),
            reason.into(),
            message.into(),
            now.into(),
        ],
    ))
    .await
    .map_err(PlaneError::db("recording an event", workload_id))?;
    db.execute(stmt(
        "DELETE FROM workload_events WHERE id IN ( \
             SELECT id FROM workload_events WHERE workload_id = $1 \
             ORDER BY last_seen_at DESC, id DESC OFFSET $2)",
        vec![workload_id.into(), MAX_EVENTS_PER_WORKLOAD.into()],
    ))
    .await
    .map_err(PlaneError::db("trimming events", workload_id))?;
    Ok(())
}

/// Delete every event last seen before `now - EVENT_RETENTION`. Returns how
/// many were removed.
pub async fn prune<C: ConnectionTrait>(db: &C, now: DateTime<Utc>) -> Result<u64, PlaneError> {
    let result = db
        .execute(stmt(
            "DELETE FROM workload_events WHERE last_seen_at < $1",
            vec![(now - EVENT_RETENTION).into()],
        ))
        .await
        .map_err(PlaneError::db("pruning events", "*"))?;
    Ok(result.rows_affected())
}

/// A workload's events, newest first.
pub async fn list<C: ConnectionTrait>(
    db: &C,
    workload_id: i64,
) -> Result<Vec<WorkloadEvent>, PlaneError> {
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};
    use temps_entities::workload_events;
    let rows = workload_events::Entity::find()
        .filter(workload_events::Column::WorkloadId.eq(workload_id))
        .order_by_desc(workload_events::Column::LastSeenAt)
        .order_by_desc(workload_events::Column::Id)
        .all(db)
        .await
        .map_err(PlaneError::db("listing events", workload_id))?;
    Ok(rows
        .into_iter()
        .map(|row| WorkloadEvent {
            reason: row.reason,
            message: row.message,
            count: u32::try_from(row.count).unwrap_or(0),
            first_seen_at: row.first_seen_at,
            last_seen_at: row.last_seen_at,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{insert_workload_row, test_db};

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000 + secs, 0).expect("valid time")
    }

    #[tokio::test]
    async fn repeats_increment_the_count_and_keep_the_first_time() {
        let Some(db) = test_db().await else { return };
        let id = insert_workload_row(&db, "events-dedup").await;
        record(&*db.db, id, None, "BackOff", "attempt 1 exited 1", at(0))
            .await
            .expect("recorded");
        record(&*db.db, id, None, "BackOff", "attempt 1 exited 1", at(5))
            .await
            .expect("recorded");
        record(&*db.db, id, None, "Started", "attempt 2", at(6))
            .await
            .expect("recorded");
        let events = list(&*db.db, id).await.expect("listed");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].reason, "Started");
        assert_eq!(events[1].reason, "BackOff");
        assert_eq!(events[1].count, 2);
        assert_eq!(events[1].first_seen_at, at(0));
        assert_eq!(events[1].last_seen_at, at(5));
    }

    #[tokio::test]
    async fn a_workload_keeps_its_newest_hundred_events() {
        let Some(db) = test_db().await else { return };
        let id = insert_workload_row(&db, "events-cap").await;
        for i in 0..(MAX_EVENTS_PER_WORKLOAD + 5) {
            record(&*db.db, id, None, "Tick", &format!("tick {i}"), at(i))
                .await
                .expect("recorded");
        }
        let events = list(&*db.db, id).await.expect("listed");
        assert_eq!(events.len() as i64, MAX_EVENTS_PER_WORKLOAD);
        assert_eq!(
            events[0].message,
            format!("tick {}", MAX_EVENTS_PER_WORKLOAD + 4)
        );
        assert_eq!(events.last().map(|e| e.message.as_str()), Some("tick 5"));
    }

    #[tokio::test]
    async fn events_older_than_seven_days_are_pruned() {
        let Some(db) = test_db().await else { return };
        let id = insert_workload_row(&db, "events-prune").await;
        let now = at(0);
        record(
            &*db.db,
            id,
            None,
            "Old",
            "eight days ago",
            now - Duration::days(8),
        )
        .await
        .expect("recorded");
        record(
            &*db.db,
            id,
            None,
            "Recent",
            "six days ago",
            now - Duration::days(6),
        )
        .await
        .expect("recorded");
        assert_eq!(prune(&*db.db, now).await.expect("pruned"), 1);
        let events = list(&*db.db, id).await.expect("listed");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].reason, "Recent");
    }
}
