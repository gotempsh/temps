use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

/// A registered OTel span attribute facet.
///
/// When an attribute key is registered as a facet, new spans that carry that
/// attribute will have its value written into the corresponding
/// `facet_attr_N` slot column, and existing spans are back-filled in the
/// background (see `status`). This makes filtering by that attribute value
/// use an index (ClickHouse bloom-filter skip index, or a Postgres B-tree
/// index on TimescaleDB) rather than a full JSON/text parse on every row.
///
/// The `slot` is a number 1..=20 mapping to the pre-allocated DDL column
/// `facet_attr_N`. Slots are global across the platform (one spans table)
/// and cannot be reused until the previous assignment is deleted and the
/// column cleared.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "otel_span_facets")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    /// The OTel attribute key (e.g. `enduser.id`, `galachain.contract`).
    /// Must be unique across all facets.
    pub attribute_key: String,
    /// The slot column index 1..=20, mapping to `facet_attr_N`.
    pub slot: i16,
    /// The user who created this facet mapping, if known.
    pub created_by: Option<i32>,
    pub created_at: DBDateTime,
    /// Backfill/delete-clear lifecycle: `pending` -> `running` ->
    /// `completed` | `failed`. `deleting` means unregistration was
    /// requested but the slot-clear mutation hasn't confirmed done yet —
    /// the row (and its slot) stays reserved until it has.
    pub status: String,
    /// Which storage engine this facet was created against
    /// (`clickhouse` | `timescaledb`) — a deployment only ever runs one,
    /// but this keeps the status fields below unambiguous.
    pub backend: String,
    /// ClickHouse `system.mutations.mutation_id` for the in-flight backfill
    /// or clear mutation. `None` for the `timescaledb` backend, which drives
    /// its own batched loop instead of an externally tracked mutation.
    pub ch_mutation_id: Option<String>,
    /// Resume point for the TimescaleDB batched backfill/clear loop: the
    /// last-processed `otel_spans.id`. `None` means that backend's loop
    /// hasn't started yet. Always `None` for the `clickhouse` backend.
    pub backfill_cursor: Option<i64>,
    /// Rows updated so far by the TimescaleDB batched backfill/clear loop.
    /// Always 0 for the `clickhouse` backend (progress there is tracked via
    /// `ch_mutation_id` + `system.mutations`, which doesn't expose a row
    /// count until done).
    pub rows_backfilled: i64,
    /// Populated when `status = 'failed'` with the reason, so an operator
    /// can decide whether to retry.
    pub error_message: Option<String>,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
