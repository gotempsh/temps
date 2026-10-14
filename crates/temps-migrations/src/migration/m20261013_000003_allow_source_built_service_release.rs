// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Let a source-built service of an app template keep its template release.
//!
//! `m20260903_000001_add_service_project_identity` ties `service_template` to
//! `project_type = 'service'`: a server or static project could never carry a
//! release. An app template (ADR-051) can have services that are built from
//! the template's repository instead of pulled as a pinned image. Those keep
//! the lifecycle their preset gives them -- `server`, or `static` for a
//! frontend -- so the normal build pipeline runs, yet they still need the
//! resolved release: it holds the service's managed-database bindings, which
//! the deployment environment resolver applies at every deploy.
//!
//! The constraint is therefore relaxed in exactly one way: a `server` or
//! `static` row may carry `service_template` when it also names the template
//! it came from (`template_slug` present and not blank). `project_type =
//! 'service'` keeps its stricter meaning (an image release that can be
//! upgraded) and nothing else about the invariant changes: a regular project
//! with a release and no provenance is still rejected.
//!
//! Down restores the original constraint. A server/static row holding a
//! release would violate it, so those snapshots are cleared first; the
//! services keep their `template_slug` and become ordinary projects.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "ALTER TABLE projects DROP CONSTRAINT IF EXISTS projects_service_template_identity",
        )
        .await?;
        db.execute_unprepared(
            "ALTER TABLE projects \
             ADD CONSTRAINT projects_service_template_identity \
             CHECK (\
                (project_type = 'service' AND service_template IS NOT NULL AND \
                    template_slug IS NOT NULL AND btrim(template_slug) <> '') OR \
                (project_type IN ('server', 'static') AND \
                    (service_template IS NULL OR \
                     (template_slug IS NOT NULL AND btrim(template_slug) <> '')))\
             )",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "UPDATE projects SET service_template = NULL \
             WHERE project_type IN ('server', 'static') AND service_template IS NOT NULL",
        )
        .await?;
        db.execute_unprepared(
            "ALTER TABLE projects DROP CONSTRAINT IF EXISTS projects_service_template_identity",
        )
        .await?;
        db.execute_unprepared(
            "ALTER TABLE projects \
             ADD CONSTRAINT projects_service_template_identity \
             CHECK (\
                (project_type = 'service' AND service_template IS NOT NULL AND \
                    template_slug IS NOT NULL AND btrim(template_slug) <> '') OR \
                (project_type IN ('server', 'static') AND service_template IS NULL)\
             )",
        )
        .await?;
        Ok(())
    }
}
