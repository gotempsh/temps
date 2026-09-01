// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Service for managing OTel span attribute facets.
//!
//! A "facet" is an arbitrary OTel attribute key (e.g. `enduser.id`,
//! `galachain.contract`) that an admin has marked for fast filtering. Instead
//! of parsing the `attributes` JSON blob for every span on every query, the
//! value is written into a pre-allocated `facet_attr_N` slot column and
//! indexed (a ClickHouse bloom filter, or a Postgres B-tree index on
//! TimescaleDB — see `storage::timescaledb`'s module docs for the
//! compression caveat on that path).
//!
//! The mapping from attribute key → slot is stored in the Postgres
//! `otel_span_facets` table and cached in memory via an `ArcSwap` for
//! lock-free reads on the hot ingest/query paths.
//!
//! # Backfilling historical spans
//!
//! Registering a facet doesn't just affect spans ingested from that point
//! on — existing spans need their slot column backfilled too, and at
//! production data volumes that can take anywhere from seconds to hours.
//! Rather than blocking the create/delete request on that work (or spawning
//! a task from the HTTP handler, whose progress would be lost on a process
//! restart), all of it is driven by [`FacetService::advance_pending_facets`],
//! a periodic poller (see `plugin.rs`, mirrors the existing OTel
//! retention-cleanup loop) that advances every non-terminal facet by one
//! bounded unit of work per tick:
//!
//! - **ClickHouse backend**: issues the `ALTER TABLE ... UPDATE` mutation
//!   (returns immediately — `mutations_sync=0`), records its
//!   `system.mutations` id, and polls that id on later ticks for
//!   completion/failure.
//! - **TimescaleDB backend**: runs one keyset-paginated batch of a plain
//!   `UPDATE` per tick, persisting the resume cursor after each batch.
//!
//! Because all progress lives in the `otel_span_facets` row (`status`,
//! `ch_mutation_id`, `backfill_cursor`, `rows_backfilled`), a server restart
//! simply resumes from whatever was last persisted — nothing is lost.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use arc_swap::ArcSwap;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use clickhouse::Row;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseBackend, DatabaseConnection,
    EntityTrait, QueryFilter, QueryOrder, Statement,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{error, warn};
use utoipa::ToSchema;

use temps_entities::otel_span_facets::{ActiveModel, Column, Entity, Model};

// ── Error type ─────────────────────────────────────────────────────────────

#[derive(Error, Debug)]
pub enum FacetError {
    #[error("Attribute key '{attribute_key}' is already registered as a facet")]
    AlreadyFaceted { attribute_key: String },

    #[error("All {max_slots} facet slots are in use; delete a facet before adding a new one")]
    CapacityExceeded { max_slots: u8 },

    #[error("Facet for attribute key '{attribute_key}' not found")]
    NotFound { attribute_key: String },

    #[error(
        "Facet for attribute key '{attribute_key}' is not in a failed state (status={status}); \
         only a failed backfill can be retried"
    )]
    NotFailed {
        attribute_key: String,
        status: String,
    },

    #[error("Validation error: {message}")]
    Validation { message: String },

    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),

    #[error("ClickHouse storage error during facet operation: {message}")]
    Storage { message: String },
}

// ── Status / backend enums ───────────────────────────────────────────────────

/// Backfill/delete-clear lifecycle for a facet. See the module docs for how
/// the poller advances a facet through these states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FacetStatus {
    /// Registered, but the poller hasn't picked it up yet.
    Pending,
    /// Backfill (or delete-clear) is actively in progress.
    Running,
    /// Backfill finished successfully; the slot is fully populated for
    /// historical spans.
    Completed,
    /// Backfill or delete-clear failed — see `error_message`. The facet
    /// mapping itself remains active (new spans still populate the slot);
    /// only the historical backfill is affected. Retryable via
    /// [`FacetService::retry_backfill`].
    Failed,
    /// Unregistration was requested; the slot-clear mutation/loop hasn't
    /// confirmed done yet. The row (and its slot) stays reserved so a
    /// concurrent `create_facet` can't reuse the slot while stale data may
    /// still be in it.
    Deleting,
}

impl FacetStatus {
    fn as_db_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Deleting => "deleting",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "pending" => Self::Pending,
            "running" => Self::Running,
            "completed" => Self::Completed,
            "deleting" => Self::Deleting,
            "failed" => Self::Failed,
            other => {
                error!(
                    status = other,
                    "Unknown otel_span_facets.status value; treating as Failed"
                );
                Self::Failed
            }
        }
    }
}

/// Which storage engine a facet was created against. A deployment only ever
/// runs one backend at a time (see `plugin.rs`'s storage selection), but
/// stamping it on each row keeps the status fields unambiguous.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FacetBackendKind {
    Clickhouse,
    Timescaledb,
}

impl FacetBackendKind {
    fn as_db_str(self) -> &'static str {
        match self {
            Self::Clickhouse => "clickhouse",
            Self::Timescaledb => "timescaledb",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "timescaledb" => Self::Timescaledb,
            _ => Self::Clickhouse,
        }
    }
}

// ── DTO ─────────────────────────────────────────────────────────────────────

/// Public representation of a registered span attribute facet.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct FacetInfo {
    /// The OTel attribute key, e.g. `enduser.id` or `galachain.contract`.
    pub attribute_key: String,
    /// The slot column index 1..=20 (`facet_attr_N`).
    pub slot: u8,
    #[schema(example = "2025-10-12T12:15:47.609192Z")]
    pub created_at: String,
    pub status: FacetStatus,
    pub backend: FacetBackendKind,
    /// Rows backfilled so far. Only meaningful for the `timescaledb` backend
    /// — the `clickhouse` backend's mutation doesn't expose a row count
    /// until it's done, so this stays 0 there even while running.
    pub rows_backfilled: i64,
    /// Populated when `status = failed`, explaining why.
    pub error_message: Option<String>,
}

impl From<Model> for FacetInfo {
    fn from(m: Model) -> Self {
        Self {
            attribute_key: m.attribute_key,
            slot: m.slot as u8,
            created_at: m.created_at.to_rfc3339(),
            status: FacetStatus::parse(&m.status),
            backend: FacetBackendKind::parse(&m.backend),
            rows_backfilled: m.rows_backfilled,
            error_message: m.error_message,
        }
    }
}

// ── The maximum pool of pre-allocated slots ──────────────────────────────────

pub const MAX_FACET_SLOTS: u8 = 20;

/// Rows scanned per poller tick for the TimescaleDB batched backfill/clear
/// loop. Deliberately bounded (not "however many match") so each tick does
/// O(batch), not O(table) — see CLAUDE.md's "Background loops must be
/// O(changes), not O(total)" rule. Scanned unconditionally by `id` range
/// (not by attribute match) so a long stretch of non-matching rows still
/// makes forward progress every tick instead of scanning indefinitely
/// without advancing the cursor.
const PG_BATCH_SIZE: u64 = 2000;

/// How long a `running`/`deleting` facet can go without a resolvable
/// ClickHouse mutation before the poller treats it as lost (process crashed
/// between issuing the mutation and this check, mutation GC'd from
/// `system.mutations`, etc.) rather than still-in-flight.
const STALE_MUTATION_THRESHOLD: ChronoDuration = ChronoDuration::minutes(30);

/// Max length stored/returned for `otel_span_facets.error_message`.
///
/// ClickHouse mutation failure reasons (`system.mutations.latest_fail_reason`)
/// can include internal detail — column/expression fragments, cluster
/// topology — that an `OtelWrite`-authorized-but-otherwise-unprivileged user
/// shouldn't necessarily see verbatim, especially on a shared multi-tenant
/// ClickHouse cluster. Truncating bounds the exposure without losing the
/// actionable prefix of the message.
const ERROR_MESSAGE_MAX_LEN: usize = 1024;

/// Truncate an error message to [`ERROR_MESSAGE_MAX_LEN`] bytes at a valid
/// UTF-8 char boundary, appending a marker so it's obvious the message was
/// cut rather than genuinely ending there.
fn truncate_error_message(message: String) -> String {
    if message.len() <= ERROR_MESSAGE_MAX_LEN {
        return message;
    }
    let mut end = ERROR_MESSAGE_MAX_LEN;
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    let mut truncated = message[..end].to_string();
    truncated.push_str("... [truncated]");
    truncated
}

/// Restrict `attribute_key` to the OTel-spec-like charset
/// `[a-zA-Z][a-zA-Z0-9_.:-]*` (mirrors `validate_label_key` in
/// `storage::timescaledb`).
///
/// `attribute_key` is always bound as a SQL/ClickHouse parameter, never
/// interpolated — so this is defense-in-depth, not an injection guard. It
/// exists to keep control characters (which would otherwise flow verbatim
/// into structured logs via `warn!`/`error!` on facet failures, a log-
/// injection vector) and other unexpected bytes out of a value that gets
/// echoed back through the API and CLI.
fn validate_attribute_key_charset(key: &str) -> Result<(), FacetError> {
    let mut chars = key.chars();
    let Some(first) = chars.next() else {
        return Err(FacetError::Validation {
            message: "attribute_key must not be empty".to_string(),
        });
    };
    if !first.is_ascii_alphabetic() {
        return Err(FacetError::Validation {
            message: format!(
                "attribute_key '{key}' must start with an ASCII letter \
                 (allowed characters: [a-zA-Z0-9_.:-])"
            ),
        });
    }
    for ch in chars {
        if !matches!(ch, 'a'..='z' | 'A'..='Z' | '0'..='9' | '_' | '.' | '-' | ':') {
            return Err(FacetError::Validation {
                message: format!(
                    "attribute_key '{key}' is outside the allowed character set \
                     [a-zA-Z0-9_.:-] (starting with a letter)"
                ),
            });
        }
    }
    Ok(())
}

// ── Service ─────────────────────────────────────────────────────────────────

/// Shared type alias for the facet attribute-key → slot cache.
///
/// Values are 1-indexed (1..=20). Lock-free reads via `ArcSwap::load()`.
pub type FacetCache = Arc<ArcSwap<HashMap<String, u8>>>;

/// Service managing OTel span attribute facets.
///
/// Holds a Postgres connection for config persistence AND, when the
/// `timescaledb` backend is active, the historical-backfill target itself
/// (`otel_spans` lives in the same primary database). `ch_client` is present
/// only when ClickHouse is configured.
pub struct FacetService {
    db: Arc<DatabaseConnection>,
    /// Optional: present when ClickHouse is configured. Absent means the
    /// `timescaledb` backend is active instead — `create_facet` and the
    /// poller branch on this to decide which backend's mutation/batch logic
    /// to run.
    ch_client: Option<::clickhouse::Client>,
    /// Shared with `ClickHouseOtelStorage`/`TimescaleDbStorage` so ingest and
    /// query paths on both backends see the same mapping without any extra
    /// coordination.
    pub facet_cache: FacetCache,
}

impl FacetService {
    /// Construct a new `FacetService`.
    ///
    /// Call [`refresh_cache`](Self::refresh_cache) after construction to load
    /// the initial facet map from Postgres.
    pub fn new(
        db: Arc<DatabaseConnection>,
        ch_client: Option<::clickhouse::Client>,
        facet_cache: FacetCache,
    ) -> Self {
        Self {
            db,
            ch_client,
            facet_cache,
        }
    }

    // ── Public API ─────────────────────────────────────────────────────────

    /// List all registered facets (newest first), including ones still
    /// backfilling or being torn down.
    pub async fn list_facets(&self) -> Result<Vec<FacetInfo>, FacetError> {
        let models = Entity::find()
            .order_by_desc(Column::CreatedAt)
            .all(self.db.as_ref())
            .await?;

        Ok(models.into_iter().map(FacetInfo::from).collect())
    }

    /// Register `attribute_key` as a facet.
    ///
    /// Validates the key, assigns the lowest free slot (1..=20), and inserts
    /// the Postgres row with `status = pending`. Returns immediately — the
    /// historical backfill runs entirely in the background (see the module
    /// docs), driven by the periodic poller rather than this call. New spans
    /// start getting the attribute written into the slot column right away,
    /// since [`refresh_cache`](Self::refresh_cache) runs before this returns.
    pub async fn create_facet(
        &self,
        attribute_key: String,
        created_by: Option<i32>,
    ) -> Result<FacetInfo, FacetError> {
        // 1. Validate
        let key = attribute_key.trim().to_string();
        if key.is_empty() {
            return Err(FacetError::Validation {
                message: "attribute_key must not be empty".to_string(),
            });
        }
        if key.len() > 200 {
            return Err(FacetError::Validation {
                message: format!(
                    "attribute_key must be at most 200 characters (got {})",
                    key.len()
                ),
            });
        }
        validate_attribute_key_charset(&key)?;

        // 2. Check for duplicate. A facet mid-deletion still counts as
        // occupying its key/slot until the row is hard-deleted, so a
        // recreate attempt during that window is correctly rejected here
        // rather than racing the teardown.
        let existing: Vec<Model> = Entity::find().all(self.db.as_ref()).await?;

        if existing.iter().any(|m| m.attribute_key == key) {
            return Err(FacetError::AlreadyFaceted { attribute_key: key });
        }

        // 3. Find the lowest free slot (1..=MAX_FACET_SLOTS)
        let used_slots: HashSet<i16> = existing.iter().map(|m| m.slot).collect();
        let slot = (1..=(MAX_FACET_SLOTS as i16))
            .find(|s| !used_slots.contains(s))
            .ok_or(FacetError::CapacityExceeded {
                max_slots: MAX_FACET_SLOTS,
            })?;

        // 4. Insert the Postgres row.
        let backend = if self.ch_client.is_some() {
            FacetBackendKind::Clickhouse
        } else {
            FacetBackendKind::Timescaledb
        };
        let now = Utc::now();
        let active = ActiveModel {
            attribute_key: Set(key.clone()),
            slot: Set(slot),
            created_by: Set(created_by),
            created_at: Set(now),
            status: Set(FacetStatus::Pending.as_db_str().to_string()),
            backend: Set(backend.as_db_str().to_string()),
            updated_at: Set(now),
            ..Default::default()
        };
        let model = active.insert(self.db.as_ref()).await?;

        // 5. New spans start getting this attribute written into its slot
        // column immediately. Historical backfill is picked up by the next
        // poller tick (see `advance_pending_facets`).
        self.refresh_cache().await?;

        Ok(FacetInfo::from(model))
    }

    /// Remove a facet by `attribute_key`.
    ///
    /// Marks the row `deleting` and removes the key from the active ingest
    /// cache immediately (so no new spans populate a slot being torn down),
    /// but does NOT delete the Postgres row yet — the slot-clear
    /// mutation/loop must confirm done first (driven by the poller), so a
    /// concurrent `create_facet` can't reuse the slot while stale data might
    /// still be in it. The row is hard-deleted once that clear completes.
    pub async fn delete_facet(&self, attribute_key: &str) -> Result<(), FacetError> {
        let model = Entity::find()
            .filter(Column::AttributeKey.eq(attribute_key))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| FacetError::NotFound {
                attribute_key: attribute_key.to_string(),
            })?;

        if model.status == FacetStatus::Deleting.as_db_str() {
            // Already tearing down; idempotent from the caller's perspective.
            return Ok(());
        }

        let mut active: ActiveModel = model.into();
        active.status = Set(FacetStatus::Deleting.as_db_str().to_string());
        active.error_message = Set(None);
        active.ch_mutation_id = Set(None);
        active.backfill_cursor = Set(None);
        active.rows_backfilled = Set(0);
        active.updated_at = Set(Utc::now());
        active.update(self.db.as_ref()).await?;

        self.refresh_cache().await?;

        Ok(())
    }

    /// Retry a failed backfill. Only valid for facets whose historical
    /// backfill failed (`status = failed`) — resets progress and lets the
    /// next poller tick re-issue the mutation / restart the batch loop from
    /// the beginning.
    pub async fn retry_backfill(&self, attribute_key: &str) -> Result<FacetInfo, FacetError> {
        let model = Entity::find()
            .filter(Column::AttributeKey.eq(attribute_key))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| FacetError::NotFound {
                attribute_key: attribute_key.to_string(),
            })?;

        if model.status != FacetStatus::Failed.as_db_str() {
            return Err(FacetError::NotFailed {
                attribute_key: attribute_key.to_string(),
                status: model.status.clone(),
            });
        }

        let mut active: ActiveModel = model.into();
        active.status = Set(FacetStatus::Pending.as_db_str().to_string());
        active.error_message = Set(None);
        active.ch_mutation_id = Set(None);
        // Restart from the top: we don't know how much of a partial
        // TimescaleDB batch run was valid before the failure, and re-scanning
        // is cheap relative to correctness risk.
        active.backfill_cursor = Set(None);
        active.rows_backfilled = Set(0);
        active.updated_at = Set(Utc::now());
        let updated = active.update(self.db.as_ref()).await?;

        Ok(FacetInfo::from(updated))
    }

    /// Reload all *active* facets from Postgres and swap the in-memory
    /// cache. "Active" excludes `deleting` rows — everything else (including
    /// `pending`/`running`/`failed`) stays live for ingest, since the
    /// key→slot mapping itself is valid even while a historical backfill is
    /// still running or has failed; only `deleting` means new writes should
    /// stop.
    ///
    /// Called after every create/delete/retry and once at construction time.
    pub async fn refresh_cache(&self) -> Result<(), FacetError> {
        let rows: Vec<Model> = Entity::find()
            .filter(Column::Status.ne(FacetStatus::Deleting.as_db_str()))
            .all(self.db.as_ref())
            .await?;

        let map: HashMap<String, u8> = rows
            .into_iter()
            .map(|m| (m.attribute_key, m.slot as u8))
            .collect();

        self.facet_cache.store(Arc::new(map));
        Ok(())
    }

    // ── Poller entry point ────────────────────────────────────────────────

    /// Advance every non-terminal facet (`pending`, `running`, `deleting`) by
    /// one bounded unit of work. Called periodically by the background loop
    /// registered in `plugin.rs`. Never panics or propagates errors — a
    /// failure advancing one facet is logged and the rest still get their
    /// turn; the failed one gets picked up again next tick.
    pub async fn advance_pending_facets(&self) {
        let facets = match Entity::find()
            .filter(Column::Status.is_in([
                FacetStatus::Pending.as_db_str(),
                FacetStatus::Running.as_db_str(),
                FacetStatus::Deleting.as_db_str(),
            ]))
            .all(self.db.as_ref())
            .await
        {
            Ok(f) => f,
            Err(e) => {
                error!(error = %e, "Failed to load in-flight OTel facets for poller tick");
                return;
            }
        };

        for facet in facets {
            let attribute_key = facet.attribute_key.clone();
            if let Err(e) = self.advance_one(facet).await {
                warn!(
                    attribute_key = %attribute_key,
                    error = %e,
                    "OTel facet backfill/clear tick failed (will retry next tick)"
                );
            }
        }
    }

    async fn advance_one(&self, facet: Model) -> Result<(), FacetError> {
        let status = FacetStatus::parse(&facet.status);
        let backend = FacetBackendKind::parse(&facet.backend);

        match (status, backend) {
            (FacetStatus::Pending, FacetBackendKind::Clickhouse) => {
                self.ch_issue_backfill(facet).await
            }
            (FacetStatus::Running, FacetBackendKind::Clickhouse) => {
                self.ch_check_mutation(facet, false).await
            }
            (FacetStatus::Deleting, FacetBackendKind::Clickhouse) => {
                if facet.ch_mutation_id.is_some() {
                    self.ch_check_mutation(facet, true).await
                } else {
                    self.ch_issue_clear(facet).await
                }
            }
            (FacetStatus::Pending | FacetStatus::Running, FacetBackendKind::Timescaledb) => {
                self.pg_backfill_batch(facet).await
            }
            (FacetStatus::Deleting, FacetBackendKind::Timescaledb) => {
                self.pg_clear_batch(facet).await
            }
            (FacetStatus::Completed | FacetStatus::Failed, _) => Ok(()),
        }
    }

    // ── ClickHouse backend ───────────────────────────────────────────────

    /// Issue the backfill mutation for a `pending` ClickHouse facet and
    /// record its `system.mutations` id. Leaves the row `pending` (retried
    /// next tick) on any failure to issue or resolve the id — no partial
    /// "running with no mutation_id" state is ever persisted.
    async fn ch_issue_backfill(&self, facet: Model) -> Result<(), FacetError> {
        let Some(ch) = self.ch_client.as_ref() else {
            error!(
                attribute_key = %facet.attribute_key,
                "Facet backend=clickhouse but no ClickHouse client is configured \
                 (config changed since this facet was created?); stuck until resolved"
            );
            return Ok(());
        };

        let column = facet_column_name(facet.slot as u8);
        let sql = format!(
            "ALTER TABLE spans UPDATE {column} = JSONExtractString(attributes, ?) \
             WHERE JSONHas(attributes, ?)"
        );
        if let Err(e) = ch
            .query(&sql)
            .bind(facet.attribute_key.as_str())
            .bind(facet.attribute_key.as_str())
            .execute()
            .await
        {
            warn!(
                attribute_key = %facet.attribute_key,
                error = %e,
                "Failed to issue OTel facet backfill mutation; will retry next tick"
            );
            return Ok(());
        }

        match self.find_latest_mutation_id(&column).await {
            Ok(Some(mutation_id)) => self.set_running(facet.id, mutation_id).await,
            Ok(None) => {
                warn!(
                    attribute_key = %facet.attribute_key,
                    "Backfill mutation issued but not found in system.mutations yet; will retry next tick"
                );
                Ok(())
            }
            Err(e) => {
                warn!(
                    attribute_key = %facet.attribute_key,
                    error = %e,
                    "Failed to resolve backfill mutation_id; will retry next tick"
                );
                Ok(())
            }
        }
    }

    /// Issue the slot-clear mutation for a `deleting` ClickHouse facet.
    /// Mirrors `ch_issue_backfill` but for `= NULL` instead of populate, and
    /// leaves the row `deleting` with no `ch_mutation_id` (retried next
    /// tick) on failure — the row is never hard-deleted without a confirmed
    /// clear, so a slot can't be reused with stale data still in it.
    async fn ch_issue_clear(&self, facet: Model) -> Result<(), FacetError> {
        let Some(ch) = self.ch_client.as_ref() else {
            // No ClickHouse configured means there's nothing to clear —
            // finish the delete outright.
            return self.finalize_delete(facet.id).await;
        };

        let column = facet_column_name(facet.slot as u8);
        let sql = format!("ALTER TABLE spans UPDATE {column} = NULL WHERE {column} IS NOT NULL");
        if let Err(e) = ch.query(&sql).execute().await {
            warn!(
                attribute_key = %facet.attribute_key,
                error = %e,
                "Failed to issue OTel facet clear mutation; will retry next tick"
            );
            return Ok(());
        }

        match self.find_latest_mutation_id(&column).await {
            Ok(Some(mutation_id)) => {
                let mut active = ActiveModel {
                    id: Set(facet.id),
                    ..Default::default()
                };
                active.ch_mutation_id = Set(Some(mutation_id));
                active.updated_at = Set(Utc::now());
                active.update(self.db.as_ref()).await?;
                Ok(())
            }
            Ok(None) | Err(_) => {
                // Leave status=deleting with no mutation_id; next tick retries
                // issuing it from scratch.
                Ok(())
            }
        }
    }

    /// Poll `system.mutations` for a `running`/`deleting` ClickHouse facet's
    /// mutation. `is_delete` selects whether completion finalizes a backfill
    /// (-> `completed`) or a slot clear (-> hard-delete the row).
    async fn ch_check_mutation(&self, facet: Model, is_delete: bool) -> Result<(), FacetError> {
        let Some(ch) = self.ch_client.as_ref() else {
            return Ok(());
        };
        let Some(mutation_id) = facet.ch_mutation_id.clone() else {
            return Ok(());
        };

        #[derive(Row, Deserialize)]
        struct MutationRow {
            is_done: u8,
            latest_fail_reason: String,
        }

        let result = ch
            .query("SELECT is_done, latest_fail_reason FROM system.mutations WHERE table = ? AND mutation_id = ? LIMIT 1")
            .bind("spans")
            .bind(mutation_id.as_str())
            .fetch_optional::<MutationRow>()
            .await;

        match result {
            Ok(Some(row)) if row.is_done == 1 && row.latest_fail_reason.is_empty() => {
                if is_delete {
                    self.finalize_delete(facet.id).await
                } else {
                    self.set_completed(facet.id).await
                }
            }
            Ok(Some(row)) if row.is_done == 1 => {
                if is_delete {
                    warn!(
                        attribute_key = %facet.attribute_key,
                        reason = %row.latest_fail_reason,
                        "OTel facet clear mutation failed; re-issuing"
                    );
                    self.ch_issue_clear(facet).await
                } else {
                    self.set_failed(facet.id, row.latest_fail_reason).await
                }
            }
            Ok(Some(_)) => Ok(()), // still running; check again next tick
            Ok(None) => self.handle_missing_mutation(facet, is_delete).await,
            Err(e) => {
                warn!(
                    attribute_key = %facet.attribute_key,
                    error = %e,
                    "Failed to poll system.mutations; will retry next tick"
                );
                Ok(())
            }
        }
    }

    /// The mutation this row was tracking is no longer visible in
    /// `system.mutations`. Usually transient (visibility lag right after
    /// issuing); only treated as genuinely lost once `updated_at` is older
    /// than [`STALE_MUTATION_THRESHOLD`], to avoid false positives.
    async fn handle_missing_mutation(
        &self,
        facet: Model,
        is_delete: bool,
    ) -> Result<(), FacetError> {
        if !is_stale(facet.updated_at) {
            return Ok(());
        }
        if is_delete {
            warn!(
                attribute_key = %facet.attribute_key,
                "OTel facet clear mutation lost (not found after {} minutes); re-issuing",
                STALE_MUTATION_THRESHOLD.num_minutes()
            );
            self.ch_issue_clear(facet).await
        } else {
            self.set_failed(
                facet.id,
                format!(
                    "Backfill mutation not found in system.mutations after {} minutes \
                     (process may have restarted or the mutation was garbage-collected); retry to re-attempt.",
                    STALE_MUTATION_THRESHOLD.num_minutes()
                ),
            )
            .await
        }
    }

    async fn find_latest_mutation_id(&self, column: &str) -> Result<Option<String>, FacetError> {
        let ch = self.ch_client.as_ref().ok_or_else(|| FacetError::Storage {
            message: "ClickHouse client not configured".to_string(),
        })?;

        #[derive(Row, Deserialize)]
        struct MutationIdRow {
            mutation_id: String,
        }

        let like_pattern = format!("%{column} =%");
        let result = ch
            .query(
                "SELECT mutation_id FROM system.mutations \
                 WHERE table = ? AND command LIKE ? ORDER BY create_time DESC LIMIT 1",
            )
            .bind("spans")
            .bind(like_pattern.as_str())
            .fetch_optional::<MutationIdRow>()
            .await
            .map_err(|e| FacetError::Storage {
                message: format!("failed to query system.mutations: {e}"),
            })?;

        Ok(result.map(|r| r.mutation_id))
    }

    // ── TimescaleDB backend ──────────────────────────────────────────────

    /// Run one batch of the backfill loop for a `pending`/`running`
    /// TimescaleDB facet.
    ///
    /// Scans the next `PG_BATCH_SIZE` rows by `id` (unconditionally, not
    /// filtered by attribute match) so the cursor always advances by a full
    /// batch per tick — a long stretch of spans that don't carry this
    /// attribute at all still makes forward progress instead of scanning
    /// indefinitely. Rows whose `attributes` contain the key get the slot
    /// column populated in a single follow-up `UPDATE`.
    async fn pg_backfill_batch(&self, facet: Model) -> Result<(), FacetError> {
        let column = facet_column_name(facet.slot as u8);
        let cursor = facet.backfill_cursor.unwrap_or(0);

        let candidates = self
            .db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT id, attributes FROM otel_spans WHERE id > $1 ORDER BY id LIMIT $2",
                vec![cursor.into(), (PG_BATCH_SIZE as i64).into()],
            ))
            .await?;

        if candidates.is_empty() {
            return self.set_completed(facet.id).await;
        }

        let mut max_id = cursor;
        let mut matching_ids: Vec<i64> = Vec::new();
        for row in &candidates {
            let id: i64 = row.try_get("", "id")?;
            max_id = max_id.max(id);
            let attrs: serde_json::Value = row.try_get("", "attributes").unwrap_or_default();
            if attrs.get(&facet.attribute_key).is_some() {
                matching_ids.push(id);
            }
        }

        let updated_count = if matching_ids.is_empty() {
            0i64
        } else {
            let mut sql = format!("UPDATE otel_spans SET {column} = attributes->>$1 WHERE id IN (");
            let mut values: Vec<sea_orm::Value> = vec![facet.attribute_key.clone().into()];
            for (i, (idx, id)) in (2u32..).zip(matching_ids.iter()).enumerate() {
                if i > 0 {
                    sql.push(',');
                }
                sql.push_str(&format!("${idx}"));
                values.push((*id).into());
            }
            sql.push(')');

            let result = self
                .db
                .execute(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    &sql,
                    values,
                ))
                .await?;
            result.rows_affected() as i64
        };

        self.bump_backfill_progress(facet.id, max_id, facet.rows_backfilled + updated_count)
            .await
    }

    /// Run one batch of the slot-clear loop for a `deleting` TimescaleDB
    /// facet. Same scan-by-id-range shape as the backfill batch, but clears
    /// unconditionally (setting an already-NULL column to NULL is a cheap
    /// no-op) so it can use a single `UPDATE ... RETURNING` instead of the
    /// two-step fetch-then-filter the backfill needs.
    async fn pg_clear_batch(&self, facet: Model) -> Result<(), FacetError> {
        let column = facet_column_name(facet.slot as u8);
        let cursor = facet.backfill_cursor.unwrap_or(0);

        let sql = format!(
            "UPDATE otel_spans SET {column} = NULL \
             WHERE id IN (SELECT id FROM otel_spans WHERE id > $1 ORDER BY id LIMIT $2) \
             RETURNING id"
        );
        let rows = self
            .db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                &sql,
                vec![cursor.into(), (PG_BATCH_SIZE as i64).into()],
            ))
            .await?;

        if rows.is_empty() {
            return self.finalize_delete(facet.id).await;
        }

        let max_id = rows
            .iter()
            .filter_map(|r| r.try_get::<i64>("", "id").ok())
            .max()
            .unwrap_or(cursor);

        self.bump_backfill_progress(facet.id, max_id, facet.rows_backfilled + rows.len() as i64)
            .await
    }

    // ── Shared status-persistence helpers ───────────────────────────────

    async fn set_running(&self, id: i32, mutation_id: String) -> Result<(), FacetError> {
        let mut active = ActiveModel {
            id: Set(id),
            ..Default::default()
        };
        active.status = Set(FacetStatus::Running.as_db_str().to_string());
        active.ch_mutation_id = Set(Some(mutation_id));
        active.updated_at = Set(Utc::now());
        active.update(self.db.as_ref()).await?;
        Ok(())
    }

    async fn set_completed(&self, id: i32) -> Result<(), FacetError> {
        let mut active = ActiveModel {
            id: Set(id),
            ..Default::default()
        };
        active.status = Set(FacetStatus::Completed.as_db_str().to_string());
        active.error_message = Set(None);
        active.updated_at = Set(Utc::now());
        active.update(self.db.as_ref()).await?;
        Ok(())
    }

    async fn set_failed(&self, id: i32, message: String) -> Result<(), FacetError> {
        error!(facet_id = id, error = %message, "OTel facet backfill failed");
        let mut active = ActiveModel {
            id: Set(id),
            ..Default::default()
        };
        active.status = Set(FacetStatus::Failed.as_db_str().to_string());
        active.error_message = Set(Some(truncate_error_message(message)));
        active.updated_at = Set(Utc::now());
        active.update(self.db.as_ref()).await?;
        Ok(())
    }

    async fn bump_backfill_progress(
        &self,
        id: i32,
        cursor: i64,
        rows_backfilled: i64,
    ) -> Result<(), FacetError> {
        let mut active = ActiveModel {
            id: Set(id),
            ..Default::default()
        };
        active.status = Set(FacetStatus::Running.as_db_str().to_string());
        active.backfill_cursor = Set(Some(cursor));
        active.rows_backfilled = Set(rows_backfilled);
        active.updated_at = Set(Utc::now());
        active.update(self.db.as_ref()).await?;
        Ok(())
    }

    /// The slot-clear finished: hard-delete the row (freeing the slot for
    /// reuse) and refresh the cache.
    async fn finalize_delete(&self, id: i32) -> Result<(), FacetError> {
        Entity::delete_by_id(id).exec(self.db.as_ref()).await?;
        self.refresh_cache().await?;
        Ok(())
    }
}

fn is_stale(updated_at: DateTime<Utc>) -> bool {
    Utc::now() - updated_at > STALE_MUTATION_THRESHOLD
}

// ── Column name helper ───────────────────────────────────────────────────────

/// Map a 1-indexed slot number (1..=20) to the storage column name.
///
/// Returns `"facet_attr_N"` — must match the DDL in `0008_facet_slots.sql`
/// (ClickHouse) and `m20260815_000001_add_facet_attr_columns_to_otel_spans`
/// (TimescaleDB) exactly. The slot is validated to be in range before this
/// function is called.
pub fn facet_column_name(slot: u8) -> String {
    format!("facet_attr_{slot}")
}

/// Invert a `key -> slot` facet cache snapshot into a `slot -> key` lookup,
/// indexed by `slot - 1`.
///
/// Both storage backends' hot-path span ingest need, per span, "what
/// attribute key (if any) is assigned to slot N" for N in 1..=20. Doing that
/// via `snapshot.iter().find(|(_, &s)| s == slot)` per slot per span is
/// O(MAX_FACET_SLOTS) per lookup — O(MAX_FACET_SLOTS^2) per span. Inverting
/// once per batch (this function) and then indexing the array is O(1) per
/// slot per span. Shared by `TimescaleDbStorage::batch_insert_spans` and
/// `ChSpanRow::from_span_with_facets` so the two backends don't duplicate the
/// inversion logic.
pub fn invert_facet_slots(
    snapshot: &HashMap<String, u8>,
) -> [Option<&str>; MAX_FACET_SLOTS as usize] {
    let mut slots: [Option<&str>; MAX_FACET_SLOTS as usize] = [None; MAX_FACET_SLOTS as usize];
    for (key, &slot) in snapshot.iter() {
        if (1..=MAX_FACET_SLOTS).contains(&slot) {
            slots[(slot - 1) as usize] = Some(key.as_str());
        }
    }
    slots
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn facet_column_name_correct() {
        assert_eq!(facet_column_name(1), "facet_attr_1");
        assert_eq!(facet_column_name(10), "facet_attr_10");
        assert_eq!(facet_column_name(20), "facet_attr_20");
    }

    #[test]
    fn facet_error_display_already_faceted() {
        let err = FacetError::AlreadyFaceted {
            attribute_key: "enduser.id".to_string(),
        };
        assert!(err.to_string().contains("enduser.id"));
        assert!(err.to_string().contains("already registered"));
    }

    #[test]
    fn facet_error_display_capacity_exceeded() {
        let err = FacetError::CapacityExceeded { max_slots: 20 };
        assert!(err.to_string().contains("20"));
        assert!(err.to_string().contains("slots are in use"));
    }

    #[test]
    fn facet_error_display_not_found() {
        let err = FacetError::NotFound {
            attribute_key: "galachain.contract".to_string(),
        };
        assert!(err.to_string().contains("galachain.contract"));
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn facet_error_display_not_failed() {
        let err = FacetError::NotFailed {
            attribute_key: "enduser.id".to_string(),
            status: "running".to_string(),
        };
        assert!(err.to_string().contains("enduser.id"));
        assert!(err.to_string().contains("running"));
        assert!(err
            .to_string()
            .contains("only a failed backfill can be retried"));
    }

    #[test]
    fn facet_error_display_validation() {
        let err = FacetError::Validation {
            message: "attribute_key must not be empty".to_string(),
        };
        assert!(err.to_string().contains("Validation error"));
        assert!(err.to_string().contains("empty"));
    }

    #[test]
    fn facet_status_round_trips_through_db_str() {
        for s in [
            FacetStatus::Pending,
            FacetStatus::Running,
            FacetStatus::Completed,
            FacetStatus::Failed,
            FacetStatus::Deleting,
        ] {
            assert_eq!(FacetStatus::parse(s.as_db_str()), s);
        }
    }

    #[test]
    fn facet_backend_round_trips_through_db_str() {
        for b in [FacetBackendKind::Clickhouse, FacetBackendKind::Timescaledb] {
            assert_eq!(FacetBackendKind::parse(b.as_db_str()), b);
        }
    }

    #[test]
    fn unknown_status_parses_as_failed_defensively() {
        assert_eq!(FacetStatus::parse("bogus"), FacetStatus::Failed);
    }

    #[test]
    fn is_stale_true_past_threshold() {
        let old = Utc::now() - ChronoDuration::minutes(31);
        assert!(is_stale(old));
    }

    #[test]
    fn is_stale_false_within_threshold() {
        let recent = Utc::now() - ChronoDuration::minutes(5);
        assert!(!is_stale(recent));
    }

    #[test]
    fn facet_info_from_model() {
        let model = Model {
            id: 1,
            attribute_key: "enduser.id".to_string(),
            slot: 3,
            created_by: Some(42),
            created_at: chrono::Utc::now(),
            status: "completed".to_string(),
            backend: "clickhouse".to_string(),
            ch_mutation_id: None,
            backfill_cursor: None,
            rows_backfilled: 0,
            error_message: None,
            updated_at: chrono::Utc::now(),
        };
        let info = FacetInfo::from(model);
        assert_eq!(info.attribute_key, "enduser.id");
        assert_eq!(info.slot, 3);
        assert_eq!(info.status, FacetStatus::Completed);
        assert_eq!(info.backend, FacetBackendKind::Clickhouse);
        assert!(info.created_at.ends_with('Z') || info.created_at.contains('+'));
    }

    #[test]
    fn truncate_error_message_leaves_short_messages_untouched() {
        let msg = "short failure reason".to_string();
        assert_eq!(truncate_error_message(msg.clone()), msg);
    }

    #[test]
    fn truncate_error_message_caps_long_messages() {
        let long = "x".repeat(ERROR_MESSAGE_MAX_LEN * 2);
        let truncated = truncate_error_message(long);
        assert!(truncated.len() <= ERROR_MESSAGE_MAX_LEN + "... [truncated]".len());
        assert!(truncated.ends_with("... [truncated]"));
    }

    #[test]
    fn truncate_error_message_respects_utf8_boundary() {
        // A multi-byte char straddling the cutoff must not panic and must
        // not split the char.
        let long = "é".repeat(ERROR_MESSAGE_MAX_LEN); // 2 bytes each
        let truncated = truncate_error_message(long);
        assert!(truncated.is_char_boundary(truncated.len() - "... [truncated]".len()));
    }

    #[test]
    fn validate_attribute_key_charset_accepts_otel_style_keys() {
        for key in [
            "enduser.id",
            "gen_ai.system",
            "galachain.contract",
            "http.request.method",
        ] {
            assert!(
                validate_attribute_key_charset(key).is_ok(),
                "expected {key} to be valid"
            );
        }
    }

    #[test]
    fn validate_attribute_key_charset_rejects_leading_digit() {
        let err = validate_attribute_key_charset("1abc").unwrap_err();
        assert!(matches!(err, FacetError::Validation { .. }));
    }

    #[test]
    fn validate_attribute_key_charset_rejects_control_characters() {
        let err = validate_attribute_key_charset("abc\ninjected").unwrap_err();
        assert!(matches!(err, FacetError::Validation { .. }));
    }

    #[test]
    fn validate_attribute_key_charset_rejects_empty() {
        let err = validate_attribute_key_charset("").unwrap_err();
        assert!(matches!(err, FacetError::Validation { .. }));
    }

    // ── retry_backfill / advance_one ─────────────────────────────────────────

    use sea_orm::{DatabaseBackend as MockBackend, MockDatabase, MockExecResult};

    fn facet_model(id: i32, status: &str, backend: &str, ch_mutation_id: Option<&str>) -> Model {
        Model {
            id,
            attribute_key: "enduser.id".to_string(),
            slot: 1,
            created_by: Some(1),
            created_at: Utc::now(),
            status: status.to_string(),
            backend: backend.to_string(),
            ch_mutation_id: ch_mutation_id.map(|s| s.to_string()),
            backfill_cursor: None,
            rows_backfilled: 0,
            error_message: None,
            updated_at: Utc::now(),
        }
    }

    fn empty_cache() -> FacetCache {
        Arc::new(ArcSwap::from_pointee(HashMap::new()))
    }

    #[tokio::test]
    async fn retry_backfill_success_resets_failed_facet() {
        let failed = facet_model(1, "failed", "timescaledb", None);
        let db = MockDatabase::new(MockBackend::Postgres)
            // find-by-attribute_key
            .append_query_results(vec![vec![failed.clone()]])
            // update returns the row again
            .append_query_results(vec![vec![facet_model(1, "pending", "timescaledb", None)]])
            .into_connection();
        let service = FacetService::new(Arc::new(db), None, empty_cache());

        let info = service.retry_backfill("enduser.id").await.unwrap();
        assert_eq!(info.status, FacetStatus::Pending);
        assert_eq!(info.rows_backfilled, 0);
    }

    #[tokio::test]
    async fn retry_backfill_rejects_non_failed_status() {
        for input_status in ["pending", "running", "completed", "deleting"] {
            let model = facet_model(1, input_status, "timescaledb", None);
            let db = MockDatabase::new(MockBackend::Postgres)
                .append_query_results(vec![vec![model]])
                .into_connection();
            let service = FacetService::new(Arc::new(db), None, empty_cache());

            let err = service.retry_backfill("enduser.id").await.unwrap_err();
            match err {
                FacetError::NotFailed { status: got, .. } => {
                    assert_eq!(
                        got, input_status,
                        "status echoed in error should match input"
                    )
                }
                other => panic!("expected NotFailed for status={input_status}, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn retry_backfill_not_found() {
        let db = MockDatabase::new(MockBackend::Postgres)
            .append_query_results(vec![Vec::<Model>::new()])
            .into_connection();
        let service = FacetService::new(Arc::new(db), None, empty_cache());

        let err = service.retry_backfill("missing.key").await.unwrap_err();
        assert!(matches!(err, FacetError::NotFound { .. }));
    }

    #[tokio::test]
    async fn advance_one_is_noop_for_terminal_statuses() {
        for status in ["completed", "failed"] {
            for backend in ["clickhouse", "timescaledb"] {
                let model = facet_model(1, status, backend, None);
                // No query results appended: a terminal-status dispatch must
                // not touch the database at all.
                let db = MockDatabase::new(MockBackend::Postgres).into_connection();
                let service = FacetService::new(Arc::new(db), None, empty_cache());

                let result = service.advance_one(model).await;
                assert!(
                    result.is_ok(),
                    "expected no-op Ok for status={status} backend={backend}, got {result:?}"
                );
            }
        }
    }

    #[tokio::test]
    async fn advance_one_pending_clickhouse_without_client_is_noop() {
        // backend=clickhouse but the service has no ch_client configured
        // (e.g. TEMPS_CLICKHOUSE_* unset since this facet was created) —
        // ch_issue_backfill must log and return Ok without any DB access.
        let model = facet_model(1, "pending", "clickhouse", None);
        let db = MockDatabase::new(MockBackend::Postgres).into_connection();
        let service = FacetService::new(Arc::new(db), None, empty_cache());

        let result = service.advance_one(model).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn advance_one_deleting_clickhouse_without_client_finalizes_delete() {
        // backend=clickhouse, deleting, no mutation_id yet, no ch_client ->
        // ch_issue_clear short-circuits straight to finalize_delete (nothing
        // to clear on a backend that was never configured).
        let model = facet_model(1, "deleting", "clickhouse", None);
        let db = MockDatabase::new(MockBackend::Postgres)
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            // refresh_cache() reload after the delete
            .append_query_results(vec![Vec::<Model>::new()])
            .into_connection();
        let service = FacetService::new(Arc::new(db), None, empty_cache());

        let result = service.advance_one(model).await;
        assert!(result.is_ok());
    }
}
