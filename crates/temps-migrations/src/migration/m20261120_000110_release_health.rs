// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Release health counters (ADR 111, D3).
//!
//! `release_health_hourly` holds one row per project, SDK environment,
//! release and hour in which sessions started: how many started, crashed
//! and ended abnormally, and the errored sessions reported as exact counts
//! by session aggregates. Individual sessions are never stored. Distinct
//! users, errored sessions and crashed users are HyperLogLog sketches
//! (`bytea`, one byte per register) so they can be merged across hours and
//! flushes; `temps_hll_merge` keeps the larger register of each position
//! when a flush adds to an existing row.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        // Sketches of a different length (another precision) are never
        // mixed: the existing value wins, so a merge cannot corrupt a row.
        db.execute_unprepared(
            "CREATE OR REPLACE FUNCTION temps_hll_merge(a bytea, b bytea) RETURNS bytea \
             LANGUAGE plpgsql IMMUTABLE PARALLEL SAFE AS $$ \
             DECLARE r bytea; i integer; \
             BEGIN \
                 IF a IS NULL THEN RETURN b; END IF; \
                 IF b IS NULL OR length(a) <> length(b) THEN RETURN a; END IF; \
                 r := a; \
                 FOR i IN 0 .. length(a) - 1 LOOP \
                     IF get_byte(b, i) > get_byte(r, i) THEN \
                         r := set_byte(r, i, get_byte(b, i)); \
                     END IF; \
                 END LOOP; \
                 RETURN r; \
             END $$",
        )
        .await?;
        db.execute_unprepared(
            "CREATE TABLE IF NOT EXISTS release_health_hourly ( \
                 project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE, \
                 environment TEXT NOT NULL DEFAULT '', \
                 release TEXT NOT NULL, \
                 bucket_start TIMESTAMPTZ NOT NULL, \
                 started BIGINT NOT NULL DEFAULT 0, \
                 crashed BIGINT NOT NULL DEFAULT 0, \
                 abnormal BIGINT NOT NULL DEFAULT 0, \
                 errored_exact BIGINT NOT NULL DEFAULT 0, \
                 errored_sessions BYTEA, \
                 users BYTEA, \
                 crashed_users BYTEA, \
                 updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(), \
                 PRIMARY KEY (project_id, environment, release, bucket_start))",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_release_health_hourly_bucket \
             ON release_health_hourly (project_id, bucket_start)",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_release_health_hourly_recent \
             ON release_health_hourly (bucket_start)",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared("DROP TABLE IF EXISTS release_health_hourly")
            .await?;
        db.execute_unprepared("DROP FUNCTION IF EXISTS temps_hll_merge(bytea, bytea)")
            .await?;
        Ok(())
    }
}
