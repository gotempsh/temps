// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Runtime AI model catalog and coding-agent update settings (ADR 112).
//!
//! A Temps release bundles a price table and model lists that age: providers
//! publish new models and change prices between releases. These tables let a
//! running instance pick them up without an upgrade.
//!
//! * `ai_model_catalog_settings`: one row (`id = 1`) holding where the
//!   catalog is fetched from, whether and how often it is refreshed, whether
//!   provider keys' model lists are rediscovered on the same interval, and
//!   the outcome of the last attempt. No row means the defaults: the built-in
//!   source, automatic refresh every 24 hours, periodic discovery on. A
//!   singleton table rather than more keys in the platform settings blob,
//!   because the status columns are written by a background task and must
//!   not race the administrator's edits to unrelated settings.
//! * `ai_model_catalog_entries`: the last catalog that fetched and validated,
//!   so a restart without network access still prices with it. Replaced as a
//!   whole in one transaction on each successful refresh; the bundled table
//!   stays the fallback for every model it does not list.
//! * `ai_agent_cli_settings`: one row (`id = 1`) for coding-agent CLIs.
//!   `auto_update` defaults to `false`, so no workspace is changed until an
//!   administrator turns it on.
//!
//! All three are new tables, so the previous binary is unaffected. `down`
//! drops them; the fetched catalog is a cache and is fetched again.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            r#"
            CREATE TABLE IF NOT EXISTS ai_model_catalog_settings (
                id SMALLINT PRIMARY KEY DEFAULT 1 CHECK (id = 1),
                source_url TEXT NULL,
                auto_refresh BOOLEAN NOT NULL DEFAULT TRUE,
                refresh_interval_hours INTEGER NOT NULL DEFAULT 24
                    CHECK (refresh_interval_hours BETWEEN 1 AND 168),
                discover_provider_models BOOLEAN NOT NULL DEFAULT TRUE,
                last_attempt_at TIMESTAMPTZ NULL,
                last_refreshed_at TIMESTAMPTZ NULL,
                last_source_url TEXT NULL,
                last_error TEXT NULL,
                entry_count INTEGER NOT NULL DEFAULT 0,
                skipped_count INTEGER NOT NULL DEFAULT 0,
                last_discovery_at TIMESTAMPTZ NULL,
                created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
                updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
            )
            "#,
        )
        .await?;
        db.execute_unprepared(
            r#"
            CREATE TABLE IF NOT EXISTS ai_model_catalog_entries (
                id BIGSERIAL PRIMARY KEY,
                provider TEXT NOT NULL,
                model_id TEXT NOT NULL,
                display_name TEXT NOT NULL,
                input_per_million DOUBLE PRECISION NULL,
                output_per_million DOUBLE PRECISION NULL,
                cache_read_per_million DOUBLE PRECISION NULL,
                cache_write_per_million DOUBLE PRECISION NULL,
                context_window BIGINT NULL,
                max_output_tokens BIGINT NULL,
                supports_tools BOOLEAN NOT NULL DEFAULT FALSE,
                supports_vision BOOLEAN NOT NULL DEFAULT FALSE,
                supports_reasoning BOOLEAN NOT NULL DEFAULT FALSE,
                deprecated BOOLEAN NOT NULL DEFAULT FALSE,
                release_date TEXT NULL,
                source_url TEXT NOT NULL,
                fetched_at TIMESTAMPTZ NOT NULL,
                CONSTRAINT ai_model_catalog_entries_provider_model_key UNIQUE (provider, model_id)
            )
            "#,
        )
        .await?;
        db.execute_unprepared(
            r#"
            CREATE TABLE IF NOT EXISTS ai_agent_cli_settings (
                id SMALLINT PRIMARY KEY DEFAULT 1 CHECK (id = 1),
                auto_update BOOLEAN NOT NULL DEFAULT FALSE,
                created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
                updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
            )
            "#,
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared("DROP TABLE IF EXISTS ai_agent_cli_settings")
            .await?;
        db.execute_unprepared("DROP TABLE IF EXISTS ai_model_catalog_entries")
            .await?;
        db.execute_unprepared("DROP TABLE IF EXISTS ai_model_catalog_settings")
            .await?;
        Ok(())
    }
}
