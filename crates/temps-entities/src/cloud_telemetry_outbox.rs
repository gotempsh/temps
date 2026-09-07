// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Shared durable outbox for all Cloud-bound telemetry entities (ADR-043 §2).
//!
//! Generalised from `cloud_span_outbox` (ADR-041 §3a): the table now carries
//! an `entity_type` discriminant so a single outbox worker and a single Postgres
//! table serve spans, metrics, analytics events and proxy logs without
//! duplicating the claim/deliver/dead-letter/byte-cap infrastructure.
//!
//! # What is in a row
//!
//! `payload` is a serialized entity-specific record (e.g.
//! `temps_cloud_protocol::SpanRecord` for `entity_type = 'span'`), already
//! projected through the consented fidelity level for the owning project. The
//! `entity_type` column is what tells the drain worker which type to
//! deserialize. Nothing beyond the consented projection is ever persisted here,
//! so the outbox can never leak more than the corresponding Cloud mirror already
//! would.
//!
//! # One generic accessor, not one per entity type
//!
//! Spans have their own accessor (`SpanOutbox`, `temps-cloud-client`), because
//! their payload is JSON stored in `payload` and predates this table's
//! generalization. Every entity type added since — metrics first — goes
//! through `TelemetryOutbox`, one concrete type parameterized by `entity_type`
//! at construction, operating on `(target_table, payload_row)` (ADR-043 §2b).
//! The partial index `idx_cloud_telemetry_outbox_pending (entity_type,
//! enqueued_at, id) WHERE state = 'pending'` makes each entity type's claim
//! scan independent: one type's backlog cannot slow another's worker.
//!
//! # Empty on an instance with no Cloud link
//!
//! Nothing writes here unless a project is explicitly set to Cloud-primary,
//! which requires a healthy link, `queryable` fidelity, and the telemetry
//! switch on. The large majority of installs (no Cloud link) get one empty
//! table and no behaviour change at all.

use sea_orm::entity::prelude::*;
use sea_orm::{DeriveActiveEnum, EnumIter};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;
use utoipa::ToSchema;

/// Which kind of telemetry entity this outbox row carries.
///
/// The discriminant the drain worker uses to choose the right deserializer and
/// Cloud ingest endpoint. New entity types are added here and to the `CHECK`
/// constraint in `m20260903_000001_generalize_cloud_telemetry_outbox`.
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
pub enum CloudTelemetryOutboxEntityType {
    /// An OTel span (`temps_cloud_protocol::SpanRecord`). The only entity type
    /// that existed before ADR-043; pre-migration rows default to this.
    #[default]
    #[sea_orm(string_value = "span")]
    Span,
    /// An OTel metric point or service metric sample.
    #[sea_orm(string_value = "metric")]
    Metric,
    /// An analytics event (`analytics_events`) or session record.
    /// Analytics sessions are bundled in the same entity type because they
    /// share the same consent surface, the same allowlist, and the same Cloud
    /// ingest endpoint format.
    #[sea_orm(string_value = "analytics_event")]
    AnalyticsEvent,
    /// A proxy log entry (Phase C3, gated on `ProxyLogStorage` trait
    /// prerequisite — see ADR-043 §3 Phase C3).
    #[sea_orm(string_value = "proxy_log")]
    ProxyLog,
}

impl CloudTelemetryOutboxEntityType {
    /// String suitable for use in SQL `entity_type = '...'` filters.
    pub fn as_sql_str(&self) -> &'static str {
        match self {
            CloudTelemetryOutboxEntityType::Span => "span",
            CloudTelemetryOutboxEntityType::Metric => "metric",
            CloudTelemetryOutboxEntityType::AnalyticsEvent => "analytics_event",
            CloudTelemetryOutboxEntityType::ProxyLog => "proxy_log",
        }
    }
}

impl std::fmt::Display for CloudTelemetryOutboxEntityType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_sql_str())
    }
}

/// Delivery state of one queued telemetry row.
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
pub enum CloudTelemetryOutboxState {
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
    /// Written back to the local store instead of Cloud, because the project
    /// stopped being Cloud-primary before the row could ship (disconnect, quota
    /// exhaustion, operator reversal). The row is kept until the sweep so the
    /// spill is countable, not inferred.
    #[sea_orm(string_value = "spilled_to_local")]
    SpilledToLocal,
}

impl std::fmt::Display for CloudTelemetryOutboxState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CloudTelemetryOutboxState::Pending => write!(f, "pending"),
            CloudTelemetryOutboxState::Delivered => write!(f, "delivered"),
            CloudTelemetryOutboxState::DeadLetter => write!(f, "dead_letter"),
            CloudTelemetryOutboxState::SpilledToLocal => write!(f, "spilled_to_local"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "cloud_telemetry_outbox")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// Owning project. Not a foreign key — a project deleted mid-outage must
    /// not block the queue, and an orphaned row is simply dropped by the worker
    /// rather than wedging delivery.
    pub project_id: i32,
    /// Which kind of entity this row carries. Drives deserializer choice and
    /// Cloud ingest endpoint selection in the drain worker.
    pub entity_type: CloudTelemetryOutboxEntityType,
    /// A serialized entity-specific record (e.g. `SpanRecord` for spans),
    /// projected at the owning project's consented fidelity. `None` for every
    /// non-span row (which uses [`Self::payload_row`] instead) and on a
    /// dead-lettered row past `DEAD_LETTER_PAYLOAD_RETENTION`: the row is kept
    /// as evidence but the payload is not kept forever.
    pub payload: Option<String>,
    /// The binary ClickHouse row payload for every entity type added after
    /// spans (ADR-043 §2c.2) — the same bytes the entity's own
    /// `#[derive(clickhouse::Row)]` struct serializes for its local insert.
    /// `None` for `span` rows, which keep using [`Self::payload`].
    pub payload_row: Option<Vec<u8>>,
    /// The Cloud ClickHouse table this row is destined for (e.g.
    /// `otel_metrics`, `events`, `proxy_logs`). `None` for `span` rows, whose
    /// destination (`telemetry_spans`) is implied by `entity_type` rather than
    /// stored.
    pub target_table: Option<String>,
    /// Size of whichever payload column this row actually uses, in bytes,
    /// stored so the byte cap and queue-size display never have to re-read or
    /// re-serialize the backlog. Zeroed with a redacted payload so the
    /// accounting always matches what is stored.
    pub payload_bytes: i32,
    /// When the ingest path accepted this entity. Drives FIFO delivery order
    /// and the "oldest unshipped item age" the operator sees.
    pub enqueued_at: DBDateTime,
    pub attempts: i32,
    pub state: CloudTelemetryOutboxState,
    /// Set when `state` leaves `pending`. Also what the retention sweep keys
    /// on, so a row is never deleted before its terminal state is durable.
    pub settled_at: Option<DBDateTime>,
    /// Verbatim reason for the last failed attempt. Bounded by the writer — an
    /// upstream error body can be arbitrarily large.
    pub last_error: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_state_is_pending() {
        assert_eq!(
            CloudTelemetryOutboxState::default(),
            CloudTelemetryOutboxState::Pending
        );
    }

    #[test]
    fn the_default_entity_type_is_span() {
        // Pre-migration rows default to 'span'; the default must match.
        assert_eq!(
            CloudTelemetryOutboxEntityType::default(),
            CloudTelemetryOutboxEntityType::Span
        );
    }

    #[test]
    fn state_display_matches_stored_column_values() {
        assert_eq!(CloudTelemetryOutboxState::Pending.to_string(), "pending");
        assert_eq!(
            CloudTelemetryOutboxState::Delivered.to_string(),
            "delivered"
        );
        assert_eq!(
            CloudTelemetryOutboxState::DeadLetter.to_string(),
            "dead_letter"
        );
        assert_eq!(
            CloudTelemetryOutboxState::SpilledToLocal.to_string(),
            "spilled_to_local"
        );
    }

    #[test]
    fn entity_type_display_matches_stored_column_values() {
        assert_eq!(CloudTelemetryOutboxEntityType::Span.to_string(), "span");
        assert_eq!(CloudTelemetryOutboxEntityType::Metric.to_string(), "metric");
        assert_eq!(
            CloudTelemetryOutboxEntityType::AnalyticsEvent.to_string(),
            "analytics_event"
        );
        assert_eq!(
            CloudTelemetryOutboxEntityType::ProxyLog.to_string(),
            "proxy_log"
        );
    }

    #[test]
    fn as_sql_str_matches_display() {
        for variant in [
            CloudTelemetryOutboxEntityType::Span,
            CloudTelemetryOutboxEntityType::Metric,
            CloudTelemetryOutboxEntityType::AnalyticsEvent,
            CloudTelemetryOutboxEntityType::ProxyLog,
        ] {
            assert_eq!(variant.as_sql_str(), variant.to_string().as_str());
        }
    }
}
