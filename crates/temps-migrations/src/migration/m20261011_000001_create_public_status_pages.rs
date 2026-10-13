// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Publishable status pages (#1412).
//!
//! - `status_pages`: one row per page a project admin creates. `slug` is the
//!   unguessable capability in the public URL; it is generated server-side and
//!   is the only way the public endpoint finds a page. A page is invisible to
//!   the public until `is_published` is true.
//! - `status_page_monitors`: the opt-in monitor selection, each with the
//!   admin-chosen public display name. Rows cascade away with either the page
//!   or the monitor, so a deleted monitor can never linger on a public page.
//! - `status_incident_updates.is_public`: incident updates are private unless
//!   explicitly marked public. Existing rows stay private.
//! - `status_incident_updates.is_system`: updates Temps generated itself, which
//!   embed internal text and can never be made public.

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
CREATE TABLE IF NOT EXISTS status_pages (
    id SERIAL PRIMARY KEY,
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    slug VARCHAR(64) NOT NULL,
    title VARCHAR(120) NOT NULL,
    description TEXT,
    is_published BOOLEAN NOT NULL DEFAULT FALSE,
    published_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_status_pages_slug ON status_pages (slug);

CREATE INDEX IF NOT EXISTS idx_status_pages_project_created
    ON status_pages (project_id, created_at DESC, id DESC);

CREATE INDEX IF NOT EXISTS idx_status_pages_published
    ON status_pages (id) WHERE is_published;

CREATE TABLE IF NOT EXISTS status_page_monitors (
    id SERIAL PRIMARY KEY,
    status_page_id INTEGER NOT NULL REFERENCES status_pages(id) ON DELETE CASCADE,
    monitor_id INTEGER NOT NULL REFERENCES status_monitors(id) ON DELETE CASCADE,
    display_name VARCHAR(80) NOT NULL,
    position INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_status_page_monitors_page_monitor
    ON status_page_monitors (status_page_id, monitor_id);

CREATE INDEX IF NOT EXISTS idx_status_page_monitors_monitor
    ON status_page_monitors (monitor_id);

ALTER TABLE status_incident_updates
    ADD COLUMN IF NOT EXISTS is_public BOOLEAN NOT NULL DEFAULT FALSE;

-- Updates Temps writes itself (the "Incident created: <title>" entry) carry
-- internal text such as the incident title and can never be made public.
ALTER TABLE status_incident_updates
    ADD COLUMN IF NOT EXISTS is_system BOOLEAN NOT NULL DEFAULT FALSE;

UPDATE status_incident_updates u
SET is_system = TRUE
FROM status_incidents i
WHERE u.incident_id = i.id
  AND u.message = 'Incident created: ' || i.title;
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
ALTER TABLE status_incident_updates DROP COLUMN IF EXISTS is_system;
ALTER TABLE status_incident_updates DROP COLUMN IF EXISTS is_public;
DROP TABLE IF EXISTS status_page_monitors;
DROP TABLE IF EXISTS status_pages;
"#,
            )
            .await?;
        Ok(())
    }
}
