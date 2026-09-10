// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Create `analytics_ingest_keys` and make analytics scope columns nullable.
//!
//! See ADR-040. Three things ship as one migration because either half alone
//! leaves the feature broken:
//!
//! 1. `analytics_ingest_keys` — a project-scoped, optionally
//!    environment-scoped, **non-secret** ingest credential (`pa_` + 64 hex).
//!    It is deliberately its own table rather than a generalization of
//!    `project_dsns`: sharing that table would make one credential value
//!    simultaneously a valid Sentry DSN key and a valid analytics key, and
//!    would conflate revocation and permission semantics across two products.
//!    There is no `deployment_id` column — a deployment-scoped key would have
//!    to be re-minted on every deploy, the opposite of "a stable value baked
//!    into someone else's build"; deployment attribution is derived from
//!    `environments.current_deployment_id` at resolution time instead.
//!    `environment_id`'s FK is `ON DELETE CASCADE`, not `SET NULL`, so
//!    deleting an environment kills its keys rather than silently widening
//!    them to whole-project scope (a privilege expansion on delete).
//!
//! 2. `events`, `performance_metrics` and `session_replay_sessions` lose
//!    `NOT NULL` on `environment_id`/`deployment_id`. All three are written
//!    from ingest paths that legitimately have neither — an app Temps does not
//!    deploy, or a Temps environment momentarily without a live deployment.
//!    Today those events are dropped with a 204 (performance), FK-violate on
//!    insert (session replay, via an `unwrap_or(0)` sentinel that points at a
//!    nonexistent `environments.id = 0`), or fail with a 23502 not-null
//!    violation (`events`, whose entity has modelled both columns as
//!    `Option<i32>` all along while the column stayed `NOT NULL`). The FKs
//!    themselves stay as-is: Postgres FKs already permit `NULL`, so no
//!    drop/recreate is needed.
//!
//! 3. A defensive normalization of any `0` sentinel already stored in
//!    `session_replay_sessions`. The FKs mean such rows cannot exist on an
//!    instance that never had a constraint dropped, but the statement
//!    documents intent and is correct for one that did.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(AnalyticsIngestKeys::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(AnalyticsIngestKeys::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(AnalyticsIngestKeys::ProjectId)
                            .integer()
                            .not_null(),
                    )
                    // Nullable: a project-scoped key is the fallback when the
                    // caller has no Temps environment at all.
                    .col(ColumnDef::new(AnalyticsIngestKeys::EnvironmentId).integer())
                    .col(
                        ColumnDef::new(AnalyticsIngestKeys::Name)
                            .string_len(128)
                            .not_null()
                            .default("Default ingest key"),
                    )
                    // `pa_` + 64 hex chars = 67; 80 leaves room for a longer
                    // prefix without a second migration.
                    .col(
                        ColumnDef::new(AnalyticsIngestKeys::PublicKey)
                            .string_len(80)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AnalyticsIngestKeys::IsActive)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(ColumnDef::new(AnalyticsIngestKeys::RevokedAt).timestamp_with_time_zone())
                    // NULL / <= 0 means unlimited, matching the existing
                    // ingest rate limiter's contract.
                    .col(
                        ColumnDef::new(AnalyticsIngestKeys::RateLimitPerMinute)
                            .integer()
                            .default(600),
                    )
                    // `["https://app.example.com"]`. NULL or [] means any
                    // origin is accepted.
                    .col(ColumnDef::new(AnalyticsIngestKeys::AllowedOrigins).json_binary())
                    .col(
                        ColumnDef::new(AnalyticsIngestKeys::EventCount)
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .col(ColumnDef::new(AnalyticsIngestKeys::LastUsedAt).timestamp_with_time_zone())
                    .col(ColumnDef::new(AnalyticsIngestKeys::CreatedByUserId).integer())
                    .col(
                        ColumnDef::new(AnalyticsIngestKeys::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(AnalyticsIngestKeys::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_analytics_ingest_keys_project_id")
                            .from(AnalyticsIngestKeys::Table, AnalyticsIngestKeys::ProjectId)
                            .to(Projects::Table, Projects::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_analytics_ingest_keys_environment_id")
                            .from(
                                AnalyticsIngestKeys::Table,
                                AnalyticsIngestKeys::EnvironmentId,
                            )
                            .to(Environments::Table, Environments::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_analytics_ingest_keys_created_by_user_id")
                            .from(
                                AnalyticsIngestKeys::Table,
                                AnalyticsIngestKeys::CreatedByUserId,
                            )
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::SetNull),
                    )
                    .to_owned(),
            )
            .await?;

        // The ingest hot path looks a key up by this column alone, and a
        // duplicate value would make "which project does this key belong to"
        // ambiguous. Unique, not merely indexed.
        manager
            .create_index(
                Index::create()
                    .name("idx_analytics_ingest_keys_public_key")
                    .table(AnalyticsIngestKeys::Table)
                    .col(AnalyticsIngestKeys::PublicKey)
                    .unique()
                    .to_owned(),
            )
            .await?;

        // Supports the admin list endpoint (all keys for a project) and the
        // active-key lookups it drives.
        manager
            .create_index(
                Index::create()
                    .name("idx_analytics_ingest_keys_project_active")
                    .table(AnalyticsIngestKeys::Table)
                    .col(AnalyticsIngestKeys::ProjectId)
                    .col(AnalyticsIngestKeys::IsActive)
                    .to_owned(),
            )
            .await?;

        let db = manager.get_connection();

        // `ALTER COLUMN ... DROP NOT NULL` is metadata-only on Postgres (no
        // table rewrite, verified empirically against a reproduced
        // production-shaped compressed hypertable), but *acquiring* the
        // `ACCESS EXCLUSIVE` lock it needs can still queue indefinitely
        // behind a long-running transaction on `events` — a high-traffic
        // ingest table. Bound the wait so a busy production instance fails
        // this migration loudly instead of the boot hanging. Mirrors the
        // pattern in `m20260817_000001_index_deployments_retention_scan`.
        db.execute_unprepared("SET LOCAL lock_timeout = '5s'")
            .await?;
        db.execute_unprepared("SET LOCAL statement_timeout = '30s'")
            .await?;

        // sea-orm's `modify_column` does not reliably express a nullability
        // change on Postgres, so these are raw. The existing FKs
        // (fk_performance_metrics_environment_id, ...) are left alone —
        // Postgres FKs already permit NULL.
        db.execute_unprepared(
            "ALTER TABLE performance_metrics ALTER COLUMN environment_id DROP NOT NULL",
        )
        .await?;
        db.execute_unprepared(
            "ALTER TABLE performance_metrics ALTER COLUMN deployment_id DROP NOT NULL",
        )
        .await?;
        db.execute_unprepared(
            "ALTER TABLE session_replay_sessions ALTER COLUMN environment_id DROP NOT NULL",
        )
        .await?;
        db.execute_unprepared(
            "ALTER TABLE session_replay_sessions ALTER COLUMN deployment_id DROP NOT NULL",
        )
        .await?;

        // `events` too. `temps_entities::events` has modelled these as
        // `Option<i32>` since forever, but the column has always been NOT NULL,
        // so a project-scoped ingest key — or an existing Host-resolved route
        // whose project has no environment — makes `record_event` fail with a
        // 23502 not-null violation instead of storing the event. The entity was
        // right and the schema was wrong; this makes them agree.
        db.execute_unprepared("ALTER TABLE events ALTER COLUMN environment_id DROP NOT NULL")
            .await?;
        db.execute_unprepared("ALTER TABLE events ALTER COLUMN deployment_id DROP NOT NULL")
            .await?;

        // Defensive: `initialize_session` wrote `unwrap_or(0)` for a missing
        // environment/deployment. The FKs should have rejected every such
        // insert, but an instance that ever ran without them would hold rows
        // pointing at an id that does not exist. NULL is the honest value.
        db.execute_unprepared(
            "UPDATE session_replay_sessions SET environment_id = NULL WHERE environment_id = 0",
        )
        .await?;
        db.execute_unprepared(
            "UPDATE session_replay_sessions SET deployment_id = NULL WHERE deployment_id = 0",
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Deliberately asymmetric: only the table is dropped. Restoring NOT
        // NULL would fail against rows that legitimately hold NULL once this
        // feature has been used, and deleting those rows to satisfy the
        // constraint would silently destroy a user's analytics history.
        manager
            .drop_table(
                Table::drop()
                    .table(AnalyticsIngestKeys::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum AnalyticsIngestKeys {
    Table,
    Id,
    ProjectId,
    EnvironmentId,
    Name,
    PublicKey,
    IsActive,
    RevokedAt,
    RateLimitPerMinute,
    AllowedOrigins,
    EventCount,
    LastUsedAt,
    CreatedByUserId,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum Projects {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum Environments {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum Users {
    Table,
    Id,
}
