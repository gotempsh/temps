// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! ADR-043 §2: generalize `cloud_span_outbox` into `cloud_telemetry_outbox`.
//!
//! The span outbox created in `m20260901_000004_create_cloud_span_outbox` is
//! promoted to a shared queue for all Cloud-bound telemetry entities. The shape
//! is unchanged; a new `entity_type` discriminant is the only structural
//! addition.
//!
//! # What this migration does
//!
//! 1. `ALTER TABLE cloud_span_outbox RENAME TO cloud_telemetry_outbox` — a
//!    single DDL statement that is instant and does not rewrite any rows. The
//!    existing state constraint, settled-row retention index, and all foreign-key
//!    references (there are none — the table has no FKs by design) survive the
//!    rename unchanged.
//!
//! 2. Adds `entity_type TEXT NOT NULL DEFAULT 'span'` with a `CHECK` constraint
//!    matching `CloudTelemetryOutboxEntityType`'s string values. Existing rows
//!    correctly default to `'span'` — no data migration needed.
//!
//! 3. Drops the two existing per-type-unaware indexes:
//!    - `idx_cloud_span_outbox_pending (enqueued_at, id) WHERE state = 'pending'`
//!    - `idx_cloud_span_outbox_project_state (project_id, state, enqueued_at)`
//!
//!    Recreates them with `entity_type` as the leading column, so each entity
//!    type's claim scan is independent and one domain's backlog cannot slow
//!    another's worker.
//!
//! 4. Does NOT touch `idx_cloud_span_outbox_settled` (the retention sweep index
//!    on `settled_at`). The retention sweep is global — it deletes by age
//!    regardless of entity type — so adding `entity_type` to that index would
//!    only hurt, never help. ADR-043 §2 explicitly calls this out.
//!
//! # Index rebuild note
//!
//! Postgres supports `CREATE INDEX CONCURRENTLY` to build indexes without
//! locking writes on the table, but Sea-ORM migrations run inside a transaction
//! and `CONCURRENTLY` is not allowed inside a transaction block. The outbox
//! table on a self-hosted instance is small (pending rows only, delivered rows
//! are swept hourly), so a non-concurrent rebuild is safe. This is a known,
//! accepted trade-off per ADR-043's Migration plan section.

use sea_orm_migration::prelude::*;

/// Kept in sync with `temps_entities::cloud_telemetry_outbox::CloudTelemetryOutboxEntityType`.
const ENTITY_TYPE_CONSTRAINT: &str = "cloud_telemetry_outbox_entity_type_valid";

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Step 1: rename the table (instant, no row rewrite).
        manager
            .get_connection()
            .execute_unprepared("ALTER TABLE cloud_span_outbox RENAME TO cloud_telemetry_outbox")
            .await?;

        // Step 2: add the entity_type discriminant.
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE cloud_telemetry_outbox \
                 ADD COLUMN entity_type TEXT NOT NULL DEFAULT 'span'",
            )
            .await?;

        manager
            .get_connection()
            .execute_unprepared(&format!(
                "ALTER TABLE cloud_telemetry_outbox \
                 ADD CONSTRAINT {ENTITY_TYPE_CONSTRAINT} \
                 CHECK (entity_type IN ('span', 'metric', 'analytics_event', 'proxy_log'))"
            ))
            .await?;

        // Step 3a: replace the pending-row claim index with one keyed by
        // entity_type first, so each domain's worker claims only its own rows.
        manager
            .get_connection()
            .execute_unprepared("DROP INDEX IF EXISTS idx_cloud_span_outbox_pending")
            .await?;

        manager
            .get_connection()
            .execute_unprepared(
                "CREATE INDEX IF NOT EXISTS idx_cloud_telemetry_outbox_pending \
                 ON cloud_telemetry_outbox (entity_type, enqueued_at, id) \
                 WHERE state = 'pending'",
            )
            .await?;

        // Step 3b: replace the per-project index with entity_type as the
        // leading column for the same reason: per-project queue depth queries
        // are always scoped to one entity type.
        manager
            .get_connection()
            .execute_unprepared("DROP INDEX IF EXISTS idx_cloud_span_outbox_project_state")
            .await?;

        manager
            .get_connection()
            .execute_unprepared(
                "CREATE INDEX IF NOT EXISTS idx_cloud_telemetry_outbox_entity_project \
                 ON cloud_telemetry_outbox (entity_type, project_id, state, enqueued_at)",
            )
            .await?;

        // idx_cloud_span_outbox_settled is intentionally left untouched.
        // The retention sweep deletes by age regardless of entity type; adding
        // entity_type to that index would never speed it up and would widen the
        // index for no benefit (ADR-043 §2).

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Reverse Step 3b.
        manager
            .get_connection()
            .execute_unprepared("DROP INDEX IF EXISTS idx_cloud_telemetry_outbox_entity_project")
            .await?;

        manager
            .get_connection()
            .execute_unprepared(
                "CREATE INDEX IF NOT EXISTS idx_cloud_span_outbox_project_state \
                 ON cloud_telemetry_outbox (project_id, state, enqueued_at)",
            )
            .await?;

        // Reverse Step 3a.
        manager
            .get_connection()
            .execute_unprepared("DROP INDEX IF EXISTS idx_cloud_telemetry_outbox_pending")
            .await?;

        manager
            .get_connection()
            .execute_unprepared(
                "CREATE INDEX IF NOT EXISTS idx_cloud_span_outbox_pending \
                 ON cloud_telemetry_outbox (enqueued_at, id) \
                 WHERE state = 'pending'",
            )
            .await?;

        // Reverse Step 2: drop the CHECK constraint before dropping the column.
        manager
            .get_connection()
            .execute_unprepared(&format!(
                "ALTER TABLE cloud_telemetry_outbox \
                 DROP CONSTRAINT IF EXISTS {ENTITY_TYPE_CONSTRAINT}"
            ))
            .await?;

        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE cloud_telemetry_outbox DROP COLUMN IF EXISTS entity_type",
            )
            .await?;

        // Reverse Step 1: rename the table back.
        manager
            .get_connection()
            .execute_unprepared("ALTER TABLE cloud_telemetry_outbox RENAME TO cloud_span_outbox")
            .await?;

        Ok(())
    }
}
