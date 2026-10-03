// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! DNS record ownership foundation (ADR-031).
//!
//! Two pieces:
//!
//! - `dns_instance_identity`: single-row table holding the random install ID
//!   stamped into `_temps-owned.*` ownership TXT markers, so two temps
//!   installs managing the same zone refuse to touch each other's records.
//!   The row is created lazily on first managed-DNS write, not here — a
//!   migration must not generate per-install random state.
//!
//! - `dns_managed_domains.proxied_by_default`: per-domain default for
//!   Cloudflare-style proxied (orange-cloud) records. Defaults to false so
//!   existing managed domains keep today's unproxied behaviour.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            r#"
            CREATE TABLE IF NOT EXISTS dns_instance_identity (
                id integer PRIMARY KEY CHECK (id = 1),
                instance_id text NOT NULL,
                created_at timestamptz NOT NULL DEFAULT now()
            );

            CREATE TABLE IF NOT EXISTS dns_managed_record_states (
                id serial PRIMARY KEY,
                provider_id integer NOT NULL REFERENCES dns_providers(id) ON DELETE CASCADE,
                zone text NOT NULL,
                name text NOT NULL,
                fqdn text NOT NULL,
                record_type text NOT NULL,
                controller text NOT NULL,
                proxied boolean NOT NULL,
                updated_at timestamptz NOT NULL DEFAULT now(),
                CONSTRAINT uq_dns_managed_record_state
                    UNIQUE (provider_id, zone, name, record_type, controller)
            );
            CREATE INDEX IF NOT EXISTS idx_dns_managed_record_states_tls
                ON dns_managed_record_states (fqdn)
                WHERE proxied = true;

            CREATE TABLE IF NOT EXISTS dns_reconciliation_runs (
                id serial PRIMARY KEY,
                provider_id integer NOT NULL REFERENCES dns_providers(id) ON DELETE CASCADE,
                zone text NOT NULL,
                actor_user_id integer NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
                controller text NOT NULL,
                status text NOT NULL,
                planned_changes jsonb NOT NULL,
                error text,
                created_at timestamptz NOT NULL DEFAULT now(),
                updated_at timestamptz NOT NULL DEFAULT now()
            );
            CREATE INDEX IF NOT EXISTS idx_dns_reconciliation_runs_zone_created
                ON dns_reconciliation_runs (zone, created_at DESC);

            ALTER TABLE dns_managed_domains
                ADD COLUMN IF NOT EXISTS proxied_by_default boolean NOT NULL DEFAULT false;
            "#,
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            r#"
            ALTER TABLE dns_managed_domains
                DROP COLUMN IF EXISTS proxied_by_default;

            DROP TABLE IF EXISTS dns_reconciliation_runs;
            DROP TABLE IF EXISTS dns_managed_record_states;
            DROP TABLE IF EXISTS dns_instance_identity;
            "#,
        )
        .await?;

        Ok(())
    }
}
