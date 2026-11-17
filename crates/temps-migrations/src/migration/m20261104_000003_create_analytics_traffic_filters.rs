// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Create `analytics_traffic_filters`: per-project rules that keep traffic
//! out of a project's analytics.
//!
//! Each row bans one referrer domain, IP address or CIDR range, network (ASN)
//! or country. Events matching a rule are flagged as non-human at ingest
//! (`events.is_crawler`, with `crawler_name = 'rule:<kind>'`), so every
//! analytics report on both backends excludes them with no query changes.
//!
//! `value` is stored normalised by the service (lowercase host, canonical
//! IP/CIDR, ASN digits, uppercase ISO country code), which is what makes the
//! unique constraint meaningful.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            r#"
            CREATE TABLE IF NOT EXISTS analytics_traffic_filters (
                id serial PRIMARY KEY,
                project_id integer NOT NULL
                    REFERENCES projects(id) ON DELETE CASCADE,
                kind text NOT NULL
                    CHECK (kind IN ('referrer', 'ip', 'asn', 'country')),
                value text NOT NULL,
                note text NULL,
                created_by integer NULL
                    REFERENCES users(id) ON DELETE SET NULL,
                created_at timestamptz NOT NULL DEFAULT now(),
                CONSTRAINT analytics_traffic_filters_project_kind_value_key
                    UNIQUE (project_id, kind, value)
            );
            "#,
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared("DROP TABLE IF EXISTS analytics_traffic_filters;")
            .await?;

        Ok(())
    }
}
