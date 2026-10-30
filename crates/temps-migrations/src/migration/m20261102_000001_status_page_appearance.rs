// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Customizable public status pages and incidents written for the public.
//!
//! - `status_pages.theme`, `accent_color`, `logo_url`, `website_url`,
//!   `show_uptime_percentage`, `show_response_times`: per-page appearance,
//!   changed by a project admin at runtime. Defaults reproduce the page as it
//!   rendered before, plus response times.
//! - `status_incidents.public_title`: a heading the admin explicitly wrote for
//!   the public. `title` stays internal (automatic incidents embed the
//!   monitor's internal name in it); a page shows `public_title` when set and
//!   a severity-derived heading otherwise.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
ALTER TABLE status_pages
    ADD COLUMN IF NOT EXISTS theme VARCHAR(8) NOT NULL DEFAULT 'auto',
    ADD COLUMN IF NOT EXISTS accent_color VARCHAR(7),
    ADD COLUMN IF NOT EXISTS logo_url TEXT,
    ADD COLUMN IF NOT EXISTS website_url TEXT,
    ADD COLUMN IF NOT EXISTS show_uptime_percentage BOOLEAN NOT NULL DEFAULT TRUE,
    ADD COLUMN IF NOT EXISTS show_response_times BOOLEAN NOT NULL DEFAULT TRUE;

ALTER TABLE status_pages DROP CONSTRAINT IF EXISTS ck_status_pages_theme;
ALTER TABLE status_pages
    ADD CONSTRAINT ck_status_pages_theme CHECK (theme IN ('auto', 'light', 'dark'));

ALTER TABLE status_pages DROP CONSTRAINT IF EXISTS ck_status_pages_accent_color;
ALTER TABLE status_pages
    ADD CONSTRAINT ck_status_pages_accent_color
    CHECK (accent_color IS NULL OR accent_color ~ '^#[0-9a-f]{6}$');

ALTER TABLE status_incidents
    ADD COLUMN IF NOT EXISTS public_title VARCHAR(200);
"#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
ALTER TABLE status_incidents DROP COLUMN IF EXISTS public_title;
ALTER TABLE status_pages
    DROP CONSTRAINT IF EXISTS ck_status_pages_accent_color,
    DROP CONSTRAINT IF EXISTS ck_status_pages_theme,
    DROP COLUMN IF EXISTS show_response_times,
    DROP COLUMN IF EXISTS show_uptime_percentage,
    DROP COLUMN IF EXISTS website_url,
    DROP COLUMN IF EXISTS logo_url,
    DROP COLUMN IF EXISTS accent_color,
    DROP COLUMN IF EXISTS theme;
"#,
            )
            .await?;
        Ok(())
    }
}
