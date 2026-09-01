// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm_migration::prelude::*;

/// Additional composite indexes on `proxy_logs` for the project-scoped
/// listing endpoint (`GET /proxy-logs?project_id=…`).
///
/// The project Request Logs page exposes five filters in addition to
/// `project_id` + time range: HTTP method, status code, environment, bot
/// traffic toggle, and (via deep links) deployment id. The previous index
/// pair (`m20260528_000001_add_proxy_logs_listing_indexes`) covers the
/// unfiltered and project-scoped sort paths but leaves the planner with no
/// usable index when one of these dimensions is added. On a 30-day window
/// that meant a per-page seq-scan over millions of rows for both the
/// `COUNT(*)` and the top-N fetch.
///
/// Each new index leads with `project_id` and trails with `timestamp DESC`
/// so it doubles as the sort key (no separate sort step required). They
/// mirror the hypertable's compression layout (`compress_segmentby =
/// 'project_id'`, `compress_orderby = 'timestamp DESC'`), which keeps
/// scans cheap as chunks compress.
///
/// Skipped on purpose: `browser`, `operating_system`, `device_type`,
/// `cache_status`, `request_source`, `is_system_request`. They are almost
/// always combined with at least one of the indexed columns, so the
/// existing `(project_id, timestamp DESC)` plus chunk exclusion handles
/// them at acceptable cost, and extra indexes would slow the hot
/// insert path.
///
/// `bot_name` is also intentionally NOT indexed: the handler filters it
/// with `ILIKE %x%`, which a plain B-tree cannot serve. The same data is
/// reachable via `user_agent` (also `ILIKE %x%`) plus the `is_bot`
/// boolean, which IS covered here.
///
/// **Safely re-runnable**: every statement uses `IF NOT EXISTS`. See
/// `m20260502_000001_add_observe_correlation` for the operational note on
/// orphan chunks from raw `pg_dump`/`pg_restore` migrations.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            r#"
DO $$
BEGIN
    -- Status-code filter (e.g. 200/404/500 dropdown). Most selective of
    -- the four; pays for itself even with the project_id prefix.
    CREATE INDEX IF NOT EXISTS idx_proxy_logs_project_status_timestamp
        ON proxy_logs (project_id, status_code, timestamp DESC);

    -- HTTP method filter (GET/POST/...). Low cardinality on its own but
    -- frequently combined with project_id; the trailing timestamp keeps
    -- the sort free.
    CREATE INDEX IF NOT EXISTS idx_proxy_logs_project_method_timestamp
        ON proxy_logs (project_id, method, timestamp DESC);

    -- Environment dropdown on the project Request Logs page.
    CREATE INDEX IF NOT EXISTS idx_proxy_logs_project_environment_timestamp
        ON proxy_logs (project_id, environment_id, timestamp DESC)
        WHERE environment_id IS NOT NULL;

    -- Bot traffic toggle (Hide Bots / All / Only Bots). Partial on the
    -- common "hide bots" path; lookups for "only bots" still get the
    -- (project_id, timestamp DESC) index.
    CREATE INDEX IF NOT EXISTS idx_proxy_logs_project_is_bot_timestamp
        ON proxy_logs (project_id, is_bot, timestamp DESC);

    -- Deployment drill-down (used by deployment detail → request logs).
    -- Partial because most rows have no deployment_id.
    CREATE INDEX IF NOT EXISTS idx_proxy_logs_project_deployment_timestamp
        ON proxy_logs (project_id, deployment_id, timestamp DESC)
        WHERE deployment_id IS NOT NULL;
END
$$;
"#,
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            r#"
DO $$
BEGIN
    DROP INDEX IF EXISTS idx_proxy_logs_project_deployment_timestamp;
    DROP INDEX IF EXISTS idx_proxy_logs_project_is_bot_timestamp;
    DROP INDEX IF EXISTS idx_proxy_logs_project_environment_timestamp;
    DROP INDEX IF EXISTS idx_proxy_logs_project_method_timestamp;
    DROP INDEX IF EXISTS idx_proxy_logs_project_status_timestamp;
END
$$;
"#,
        )
        .await?;

        Ok(())
    }
}
