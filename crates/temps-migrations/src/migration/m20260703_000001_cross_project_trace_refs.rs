//! Migration: cross-project trace discovery index (ADR-027 Phase 0).
//!
//! Creates `cross_project_trace_refs`, a lightweight append-only control table
//! that records which project_ids have seen a given trace_id. This lets the
//! cross-project discovery endpoint answer "which other projects hold spans for
//! this trace?" via a primary-key lookup without touching either the TimescaleDB
//! hypertable or the ClickHouse spans table.
//!
//! Also adds `projects.cross_project_trace_sharing` (ADR-027 Phase 3 opt-out
//! column, defaulting to TRUE to match the existing OSS global-observability
//! model where any OtelRead holder can query any project).
//!
//! Both DDL statements are idempotent (IF NOT EXISTS / IF NOT EXISTS).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            r#"
CREATE TABLE IF NOT EXISTS cross_project_trace_refs (
    trace_id    TEXT        NOT NULL,
    project_id  INTEGER     NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    first_seen  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT cross_project_trace_refs_pkey PRIMARY KEY (trace_id, project_id)
);

CREATE INDEX IF NOT EXISTS cross_project_trace_refs_by_trace
    ON cross_project_trace_refs (trace_id, first_seen DESC);

CREATE INDEX IF NOT EXISTS cross_project_trace_refs_by_age
    ON cross_project_trace_refs (first_seen);
"#,
        )
        .await
        .map_err(|e| {
            DbErr::Custom(format!(
                "Failed to create cross_project_trace_refs table/indexes: {e}"
            ))
        })?;

        // Phase 3 opt-out column: TRUE = this project's traces are visible to
        // cross-project discovery. Defaults TRUE for OSS global-observability
        // compatibility. Operators who want private-by-default can flip it.
        manager
            .alter_table(
                Table::alter()
                    .table(Projects::Table)
                    .add_column_if_not_exists(
                        ColumnDef::new(Projects::CrossProjectTraceSharing)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Projects::Table)
                    .drop_column(Projects::CrossProjectTraceSharing)
                    .to_owned(),
            )
            .await?;

        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS cross_project_trace_refs CASCADE;")
            .await
            .map_err(|e| {
                DbErr::Custom(format!(
                    "Failed to drop cross_project_trace_refs table: {e}"
                ))
            })?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum Projects {
    Table,
    CrossProjectTraceSharing,
}
