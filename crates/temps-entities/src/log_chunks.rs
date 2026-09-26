// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "log_chunks")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub project_id: i32,
    /// Set when this chunk belongs to an imported/managed external service
    /// (Postgres, MariaDB, Redis, MongoDB, MinIO, …) rather than a deployment.
    /// External-service chunks store `project_id = 0` (sentinel) and key on
    /// this instead — a service isn't owned by a single project.
    pub external_service_id: Option<i32>,
    pub env: String,
    pub service: String,
    pub container_id: String,
    pub deploy_id: Option<i32>,
    /// Worker node this chunk's container ran on. `NULL` = control-plane-local
    /// container (collected via the local Docker daemon). `Some` = a remote
    /// worker node, collected by the remote log collector over mTLS.
    pub node_id: Option<i32>,
    /// Human-readable node name, denormalized at write time so history results
    /// can display the source node without a join.
    pub node_name: Option<String>,
    pub started_at: DBDateTime,
    pub ended_at: DBDateTime,
    pub storage_key: String,
    pub line_count: i32,
    pub compressed_size_bytes: i32,
    pub has_errors: bool,
    /// Byte offset of every 100th line (uncompressed) for partial retrieval
    pub line_offsets: Vec<i32>,
    /// Stable sequence number backing `line_id = seq << 20 | line_index`
    /// (ADR-046). Postgres identity column; never set on insert.
    pub seq: i64,
    /// `1` = legacy single-frame `.ndjson.zst`, `2` = block-structured
    /// object-storage format (ADR-046 §1). Existing rows default to `1`.
    pub format_version: i16,
    /// OR of every line's level bit; `31` (all bits) for v1 rows, whose
    /// per-line levels were never tracked at the chunk level, so they are
    /// never pruned by a level filter.
    pub level_mask: i16,
    /// Lines per level (Trace, Debug, Info, Warn, Error) for v2 chunks so
    /// level facets are exact; empty for v1 rows.
    pub level_counts: Vec<i32>,
    /// Byte offset of the v2 footer (labels + block index + bloom) from the
    /// start of the object. `None` for v1 chunks and chunks written without
    /// a footer.
    pub footer_offset: Option<i64>,
    /// Total footer length in bytes, so a reader fetches it with one
    /// range-GET.
    pub footer_len: Option<i32>,
    /// Length of the bloom section in bytes. `0` means "no bloom; scan,
    /// never prune" — written when bloom construction was shed under load.
    pub bloom_len: i32,
    /// Tombstone timestamp. `Some` means this chunk is retired (retention,
    /// compaction or operator purge) and pending hard deletion after the GC
    /// grace period (ADR-046 §8a.3); live queries must exclude it.
    pub deleted_at: Option<DBDateTime>,
    /// ADR-047: when the chunk's lines were accepted by the line index.
    pub indexed_at: Option<DBDateTime>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
