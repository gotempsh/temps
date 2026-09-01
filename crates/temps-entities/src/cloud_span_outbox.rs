// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Durable outbox for spans a Cloud-primary project produced (ADR-041 §3a).
//!
//! Modelled directly on `events_ch_outbox` (`temps-analytics-events`), which
//! already solves this exact shape in this codebase: a claim/deliver worker
//! with `attempts`, dead-lettering, a retention sweep and a `Notify` wake-up.
//!
//! # Why this table exists at all
//!
//! The in-memory [`Spool`](temps_cloud_client) buffers 10,000 spans / 8 MiB and
//! survives nothing. That is correct for a best-effort mirror where local
//! storage stays authoritative. For a Cloud-primary project there *is* no local
//! copy, so an upgrade, a deploy, an OOM kill or a crash would lose every
//! buffered span. A restart is an ordinary event; losing a customer's telemetry
//! on each one is not a primary write path.
//!
//! # What is in a row
//!
//! `payload` is a serialized [`temps_cloud_protocol::SpanRecord`] — the record
//! that is about to leave the instance, already projected through
//! `cloud_span()` at the owning project's consented fidelity. It is **not** the
//! local `SpanRecord`: nothing beyond the consented projection is ever
//! persisted here, so the outbox can never leak more than the mirror already
//! would.
//!
//! At `Queryable` fidelity that payload is real span data at rest. It lives in
//! the deployment's existing database and therefore inherits the deployment's
//! existing posture — the explicit reason ADR-041 rejected a bespoke encrypted
//! file queue (Option E) rather than a reason to invent a second encryption
//! scheme here.
//!
//! # Why it is bounded in bytes, not only rows
//!
//! The reference deployment is 3 vCPU / 4 GB. `payload_bytes` is stored per row
//! so the cap can be enforced and reported in bytes without re-serializing the
//! backlog, exactly like `Spool::estimated_bytes` does in memory.

use sea_orm::entity::prelude::*;
use sea_orm::{DeriveActiveEnum, EnumIter};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;
use utoipa::ToSchema;

/// Delivery state of one queued span.
///
/// Kept as an explicit column rather than derived from `delivered_at IS NULL`
/// so a dead-lettered row is distinguishable from a merely slow one *in an
/// index*: the worker's claim query must not have to scan the dead letters to
/// find live work, and an operator asking "what is stuck" must not have to know
/// the retry ceiling to ask the question.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    ToSchema,
    DeriveActiveEnum,
    EnumIter,
    Default,
)]
#[sea_orm(rs_type = "String", db_type = "Text")]
#[serde(rename_all = "snake_case")]
pub enum CloudSpanOutboxState {
    /// Waiting for a shipping attempt, or waiting to be retried after one.
    #[default]
    #[sea_orm(string_value = "pending")]
    Pending,
    /// Handed to Cloud and acknowledged. Rows in this state are deleted by the
    /// retention sweep rather than retained — the outbox is a queue, not a
    /// second copy of the telemetry.
    #[sea_orm(string_value = "delivered")]
    Delivered,
    /// Attempts exhausted. Never retried automatically and never deleted by the
    /// sweep: an operator has to see it and decide. This is the state that
    /// makes "telemetry silently stopped" impossible to reach quietly.
    #[sea_orm(string_value = "dead_letter")]
    DeadLetter,
    /// Written back to the local span store instead of Cloud, because the
    /// project stopped being Cloud-primary before the row could ship
    /// (disconnect, quota exhaustion, operator reversal). The row is kept until
    /// the sweep so the spill is countable, not inferred.
    #[sea_orm(string_value = "spilled_to_local")]
    SpilledToLocal,
}

impl CloudSpanOutboxState {
    /// Whether a row in this state still owes the customer a delivery attempt.
    pub fn is_outstanding(&self) -> bool {
        matches!(self, CloudSpanOutboxState::Pending)
    }
}

impl std::fmt::Display for CloudSpanOutboxState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CloudSpanOutboxState::Pending => write!(f, "pending"),
            CloudSpanOutboxState::Delivered => write!(f, "delivered"),
            CloudSpanOutboxState::DeadLetter => write!(f, "dead_letter"),
            CloudSpanOutboxState::SpilledToLocal => write!(f, "spilled_to_local"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "cloud_span_outbox")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// Owning project. Not a foreign key, for the same reason
    /// `events_ch_outbox.event_id` is not one: a project deleted mid-outage
    /// must not block the queue, and an orphaned row is simply dropped by the
    /// worker rather than wedging delivery.
    pub project_id: i32,
    /// A serialized `temps_cloud_protocol::SpanRecord` — the consented
    /// projection, never the local span.
    ///
    /// `None` only on a dead-lettered row past
    /// `DEAD_LETTER_PAYLOAD_RETENTION`: the row is kept as the evidence that a
    /// delivery failed, but the span content it carried is not kept forever.
    /// Anything the worker can still act on — every `pending` row — always has
    /// a payload.
    pub payload: Option<String>,
    /// Size of `payload` in bytes, stored so the byte cap and the operator's
    /// queue-size display never have to re-read or re-serialize the backlog.
    /// Zeroed together with a redacted payload so the accounting keeps matching
    /// what is actually stored.
    pub payload_bytes: i32,
    /// When the ingest path accepted this span. Drives FIFO delivery order and
    /// the "oldest unshipped span age" the operator sees.
    pub enqueued_at: DBDateTime,
    pub attempts: i32,
    pub state: CloudSpanOutboxState,
    /// Set when `state` leaves `pending`. Also what the retention sweep keys
    /// on, so a row is never deleted before its terminal state is durable.
    pub settled_at: Option<DBDateTime>,
    /// Verbatim reason for the last failed attempt. Bounded by the writer —
    /// an upstream error body can be arbitrarily large.
    pub last_error: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_pending_rows_are_outstanding() {
        // The queue-depth number an operator reads must not silently include
        // rows nothing will ever attempt again.
        assert!(CloudSpanOutboxState::Pending.is_outstanding());
        assert!(!CloudSpanOutboxState::Delivered.is_outstanding());
        assert!(!CloudSpanOutboxState::DeadLetter.is_outstanding());
        assert!(!CloudSpanOutboxState::SpilledToLocal.is_outstanding());
    }

    #[test]
    fn the_default_state_is_pending() {
        assert_eq!(
            CloudSpanOutboxState::default(),
            CloudSpanOutboxState::Pending
        );
    }

    #[test]
    fn display_matches_the_stored_column_values() {
        assert_eq!(CloudSpanOutboxState::Pending.to_string(), "pending");
        assert_eq!(CloudSpanOutboxState::Delivered.to_string(), "delivered");
        assert_eq!(CloudSpanOutboxState::DeadLetter.to_string(), "dead_letter");
        assert_eq!(
            CloudSpanOutboxState::SpilledToLocal.to_string(),
            "spilled_to_local"
        );
    }
}
