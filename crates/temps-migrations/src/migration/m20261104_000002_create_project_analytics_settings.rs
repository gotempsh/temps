// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Create `project_analytics_settings`: per-project analytics traffic settings.
//!
//! `exclude_datacenter_traffic` flags events from hosting/cloud/VPS networks
//! as non-human at ingest so they stay out of every analytics report;
//! `exclude_spam_referrers` does the same for referrers on the bundled
//! referrer-spam list. The row is optional: a project without one
//! uses the defaults (exclusion on), so no backfill is needed and existing
//! projects get the protection on upgrade.
//!
//! A table of its own rather than a column on `projects` so traffic settings
//! have a home that is not the project row every other subsystem reads.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            r#"
            CREATE TABLE IF NOT EXISTS project_analytics_settings (
                project_id integer PRIMARY KEY
                    REFERENCES projects(id) ON DELETE CASCADE,
                exclude_datacenter_traffic boolean NOT NULL DEFAULT true,
                exclude_spam_referrers boolean NOT NULL DEFAULT true,
                created_at timestamptz NOT NULL DEFAULT now(),
                updated_at timestamptz NOT NULL DEFAULT now()
            );
            "#,
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared("DROP TABLE IF EXISTS project_analytics_settings;")
            .await?;

        Ok(())
    }
}
