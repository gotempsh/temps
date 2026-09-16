// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Queue trace-summary reconciliation after making summary writes atomic.
//!
//! Older versions treated a summary upsert as fail-soft, so a successfully
//! stored span could have no summary (or a stale one). They also stored blank
//! identity fields for live traces that had not received a root span. Upgrade
//! installations add the identity span ID and record durable pending state.
//! The post-migration maintenance path performs the long shadow-table rebuild
//! outside SeaORM's migration transaction. Fresh installations already get the
//! completed schema and do not enqueue a rebuild.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

fn trace_summary_rebuild_insert_sql(target: &str, predicate: &str, merge: bool) -> String {
    let conflict = if merge {
        format!(
            "ON CONFLICT (project_id, trace_id) DO UPDATE SET \
             span_count = {target}.span_count + EXCLUDED.span_count, \
             error_count = {target}.error_count + EXCLUDED.error_count, \
             start_time = LEAST({target}.start_time, EXCLUDED.start_time), \
             duration_ms = GREATEST({target}.duration_ms, EXCLUDED.duration_ms), \
             last_seen = now(), \
             has_root = {target}.has_root OR EXCLUDED.has_root, \
             identity_span_id = CASE WHEN NOT {target}.has_root AND \
                 (EXCLUDED.has_root OR EXCLUDED.duration_ms > {target}.duration_ms OR \
                  (EXCLUDED.duration_ms = {target}.duration_ms AND \
                   EXCLUDED.identity_span_id < {target}.identity_span_id)) \
                 THEN EXCLUDED.identity_span_id ELSE {target}.identity_span_id END, \
             root_span_name = CASE WHEN NOT {target}.has_root AND \
                 (EXCLUDED.has_root OR EXCLUDED.duration_ms > {target}.duration_ms OR \
                  (EXCLUDED.duration_ms = {target}.duration_ms AND \
                   EXCLUDED.identity_span_id < {target}.identity_span_id)) \
                 THEN EXCLUDED.root_span_name ELSE {target}.root_span_name END, \
             service_name = CASE WHEN NOT {target}.has_root AND \
                 (EXCLUDED.has_root OR EXCLUDED.duration_ms > {target}.duration_ms OR \
                  (EXCLUDED.duration_ms = {target}.duration_ms AND \
                   EXCLUDED.identity_span_id < {target}.identity_span_id)) \
                 THEN EXCLUDED.service_name ELSE {target}.service_name END, \
             kind = CASE WHEN NOT {target}.has_root AND \
                 (EXCLUDED.has_root OR EXCLUDED.duration_ms > {target}.duration_ms OR \
                  (EXCLUDED.duration_ms = {target}.duration_ms AND \
                   EXCLUDED.identity_span_id < {target}.identity_span_id)) \
                 THEN EXCLUDED.kind ELSE {target}.kind END, \
             deployment_environment = CASE WHEN NOT {target}.has_root AND \
                 (EXCLUDED.has_root OR EXCLUDED.duration_ms > {target}.duration_ms OR \
                  (EXCLUDED.duration_ms = {target}.duration_ms AND \
                   EXCLUDED.identity_span_id < {target}.identity_span_id)) \
                 THEN EXCLUDED.deployment_environment ELSE {target}.deployment_environment END, \
             deployment_id = CASE WHEN NOT {target}.has_root AND \
                 (EXCLUDED.has_root OR EXCLUDED.duration_ms > {target}.duration_ms OR \
                  (EXCLUDED.duration_ms = {target}.duration_ms AND \
                   EXCLUDED.identity_span_id < {target}.identity_span_id)) \
                 THEN EXCLUDED.deployment_id ELSE {target}.deployment_id END"
        )
    } else {
        String::new()
    };

    format!(
        "WITH aggregates AS ( \
             SELECT s.project_id, s.trace_id, MIN(s.start_time) AS start_time, \
                    MAX(s.duration_ms) AS duration_ms, COUNT(*)::BIGINT AS span_count, \
                    COUNT(*) FILTER (WHERE s.status_code = 'ERROR')::BIGINT AS error_count, \
                    bool_or(s.parent_span_id IS NULL) AS has_root \
             FROM otel_spans s WHERE {predicate} GROUP BY s.project_id, s.trace_id \
         ), identities AS ( \
             SELECT DISTINCT ON (s.project_id, s.trace_id) \
                    s.project_id, s.trace_id, s.span_id AS identity_span_id, \
                    s.name AS root_span_name, s.service_name, s.kind, \
                    s.deployment_environment, s.deployment_id \
             FROM otel_spans s WHERE {predicate} \
             ORDER BY s.project_id, s.trace_id, \
                      CASE WHEN s.parent_span_id IS NULL THEN 0 ELSE 1 END, \
                      s.duration_ms DESC, s.span_id ASC \
         ) \
         INSERT INTO {target} ( \
             project_id, trace_id, identity_span_id, root_span_name, service_name, kind, \
             deployment_environment, deployment_id, start_time, duration_ms, \
             span_count, error_count, has_root, last_seen \
         ) \
         SELECT a.project_id, a.trace_id, i.identity_span_id, i.root_span_name, \
                i.service_name, i.kind, i.deployment_environment, i.deployment_id, \
                a.start_time, a.duration_ms, a.span_count, a.error_count, a.has_root, now() \
         FROM aggregates a JOIN identities i USING (project_id, trace_id) {conflict}"
    )
}

pub fn trace_summary_rebuild_initial_sql() -> String {
    trace_summary_rebuild_insert_sql(
        "otel_trace_summaries_rebuild",
        "s.id <= (SELECT watermark FROM otel_trace_summary_rebuild_state WHERE NOT completed)",
        false,
    )
}

pub fn trace_summary_rebuild_delta_sql() -> String {
    trace_summary_rebuild_insert_sql(
        "otel_trace_summaries_rebuild",
        "s.id > (SELECT watermark FROM otel_trace_summary_rebuild_state WHERE NOT completed)",
        true,
    )
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            r#"
DO $$
DECLARE
    needs_rebuild BOOLEAN;
BEGIN
    SELECT NOT EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = current_schema()
          AND table_name = 'otel_trace_summaries'
          AND column_name = 'identity_span_id'
    ) INTO needs_rebuild;

    ALTER TABLE otel_trace_summaries
        ADD COLUMN IF NOT EXISTS identity_span_id TEXT NOT NULL DEFAULT '';

    CREATE TABLE IF NOT EXISTS otel_trace_summary_rebuild_state (
        singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton),
        watermark BIGINT,
        completed BOOLEAN NOT NULL DEFAULT FALSE,
        updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
    );

    IF needs_rebuild THEN
        INSERT INTO otel_trace_summary_rebuild_state
            (singleton, watermark, completed, updated_at)
        VALUES (TRUE, NULL, FALSE, now())
        ON CONFLICT (singleton) DO UPDATE SET
            watermark = NULL, completed = FALSE, updated_at = now();
    END IF;
END $$;
"#,
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS otel_trace_summary_rebuild_state; ALTER TABLE otel_trace_summaries DROP COLUMN IF EXISTS identity_span_id")
            .await?;
        Ok(())
    }
}
