// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Status pages become platform-level: a page shows monitors from any number
//! of projects, optionally arranged in named groups.
//!
//! - `status_pages.project_id` is dropped. The projects a page draws from are
//!   derived from its components (`status_page_monitors` →
//!   `status_monitors.project_id`), and authorization is checked against all
//!   of them. Existing pages keep their components, so each becomes a page
//!   drawing from the one project it used to belong to.
//! - Deleting a project no longer deletes the pages that show its monitors:
//!   the monitors cascade away with the project's environments, so only that
//!   project's components leave the page.
//! - `status_page_monitors.group_name`: components with the same group name
//!   are shown together (e.g. "Temps Cloud" → API, Dashboard, Builds).

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
ALTER TABLE status_page_monitors
    ADD COLUMN IF NOT EXISTS group_name VARCHAR(80);

DROP INDEX IF EXISTS idx_status_pages_project_created;
ALTER TABLE status_pages DROP COLUMN IF EXISTS project_id;

CREATE INDEX IF NOT EXISTS idx_status_pages_created
    ON status_pages (created_at DESC, id DESC);
"#,
            )
            .await?;
        Ok(())
    }

    /// Restores single-project ownership. A page drawing from several
    /// projects goes back to the lowest project id among its components; a
    /// page with no components cannot be owned by anyone and is deleted.
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
DROP INDEX IF EXISTS idx_status_pages_created;
ALTER TABLE status_pages
    ADD COLUMN IF NOT EXISTS project_id INTEGER REFERENCES projects(id) ON DELETE CASCADE;

UPDATE status_pages sp
SET project_id = owner.project_id
FROM (
    SELECT spm.status_page_id, MIN(sm.project_id) AS project_id
    FROM status_page_monitors spm
    JOIN status_monitors sm ON sm.id = spm.monitor_id
    GROUP BY spm.status_page_id
) owner
WHERE owner.status_page_id = sp.id;

DELETE FROM status_page_monitors spm
USING status_pages sp, status_monitors sm
WHERE spm.status_page_id = sp.id
  AND sm.id = spm.monitor_id
  AND sm.project_id <> sp.project_id;

DELETE FROM status_pages WHERE project_id IS NULL;
ALTER TABLE status_pages ALTER COLUMN project_id SET NOT NULL;

CREATE INDEX IF NOT EXISTS idx_status_pages_project_created
    ON status_pages (project_id, created_at DESC, id DESC);

ALTER TABLE status_page_monitors DROP COLUMN IF EXISTS group_name;
"#,
            )
            .await?;
        Ok(())
    }
}
