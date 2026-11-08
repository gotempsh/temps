// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Internal DNS observability (ADR-095): the last report of each node's
//! resolver, and a short, bounded log of the queries they answered.
//!
//! - `dns_resolver_reports`: one row per node, written on every pull. The
//!   control plane's own resolver is node 0, which has no `nodes` row, so
//!   there is no foreign key; a row for a removed node is deleted with the
//!   node by the service, and is harmless until then. `query_cursor` and
//!   `instance_id` are the read position in the resolver's query ring, so a
//!   console restart does not ingest the same entries twice.
//! - `dns_query_log`: internal-zone queries (and failed forwarded ones,
//!   without their name), attributed to the project, environment or
//!   database whose container asked when that is known. Kept for 24 hours
//!   and at most 50,000 rows by a periodic prune. The ids are not foreign
//!   keys: a deleted project leaves rows that age out within a day, and a
//!   delete never has to scan this table.
//!
//! New tables only; nothing to backfill.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            r#"
            CREATE TABLE IF NOT EXISTS dns_resolver_reports (
                node_id integer PRIMARY KEY,
                report_state varchar(32) NOT NULL,
                reported_at timestamptz NOT NULL,
                last_ok_at timestamptz NULL,
                instance_id varchar(64) NULL,
                build_version varchar(64) NULL,
                running boolean NOT NULL DEFAULT false,
                tasks_alive boolean NOT NULL DEFAULT false,
                listen_address text NULL,
                zone_generation bigint NULL,
                record_count bigint NULL,
                last_sync_age_secs bigint NULL,
                consecutive_sync_failures integer NOT NULL DEFAULT 0,
                last_error text NULL,
                query_cursor bigint NULL,
                dropped_log_entries bigint NOT NULL DEFAULT 0,
                created_at timestamptz NOT NULL DEFAULT now(),
                updated_at timestamptz NOT NULL DEFAULT now()
            )
            "#,
        )
        .await?;
        db.execute_unprepared(
            r#"
            CREATE TABLE IF NOT EXISTS dns_query_log (
                id bigserial PRIMARY KEY,
                node_id integer NOT NULL,
                observed_at timestamptz NOT NULL,
                qname varchar(253) NULL,
                qtype varchar(16) NOT NULL,
                zone varchar(16) NOT NULL,
                outcome varchar(16) NOT NULL,
                latency_us bigint NOT NULL,
                answer_count integer NOT NULL,
                source_ip varchar(45) NOT NULL,
                project_id integer NULL,
                environment_id integer NULL,
                service_id integer NULL,
                created_at timestamptz NOT NULL DEFAULT now()
            )
            "#,
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_dns_query_log_observed_at \
             ON dns_query_log (observed_at DESC, id DESC)",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_dns_query_log_node_observed_at \
             ON dns_query_log (node_id, observed_at DESC)",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared("DROP TABLE IF EXISTS dns_query_log")
            .await?;
        db.execute_unprepared("DROP TABLE IF EXISTS dns_resolver_reports")
            .await?;
        Ok(())
    }
}
