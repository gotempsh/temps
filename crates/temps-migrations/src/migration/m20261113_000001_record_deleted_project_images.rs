// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `deleted_project_images`: the projects whose rows were deleted and whose
//! built images may still be on a runtime, for the nightly sweep of images
//! of deleted projects.
//!
//! Deleting a project removes its images best-effort; whatever is left
//! (an unreachable node, a removal Docker refused, a build that finished
//! during the deletion) carries the project's ID in its
//! `sh.temps.image.project_id` label. The sweep used to find those by
//! listing every image the install labelled on each runtime, an answer
//! Docker cannot limit or page. With this table it asks each runtime for
//! one deleted project's images at a time instead, so a daemon's answer
//! never holds more than one project's leftovers, and the sweep's work
//! follows deletions rather than the number of images on the host.
//!
//! A trigger records the ID in the same transaction as the delete, so every
//! path that deletes a project row is covered, and a crash between the
//! delete and the image cleanup loses nothing. The sweep removes a row once
//! no runtime holds an image labelled with the project; until then it is
//! checked again on later nights, least recently checked first.
//!
//! No backfill: projects deleted before this table existed left images that
//! were never labelled with an owner, which the sweep never removes. `down`
//! drops the trigger, its function and the table; the images those rows
//! pointed at are then left alone, the safe direction.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            r#"
            CREATE TABLE IF NOT EXISTS deleted_project_images (
                project_id integer PRIMARY KEY,
                deleted_at timestamptz NOT NULL DEFAULT now(),
                checked_at timestamptz
            );
            CREATE INDEX IF NOT EXISTS idx_deleted_project_images_checked
                ON deleted_project_images (checked_at NULLS FIRST, project_id);
            "#,
        )
        .await?;
        db.execute_unprepared(
            r#"
            CREATE OR REPLACE FUNCTION record_deleted_project_images()
            RETURNS TRIGGER AS $$
            BEGIN
                INSERT INTO deleted_project_images (project_id)
                VALUES (OLD.id)
                ON CONFLICT (project_id)
                DO UPDATE SET deleted_at = now(), checked_at = NULL;
                RETURN OLD;
            END;
            $$ LANGUAGE plpgsql;
            "#,
        )
        .await?;
        db.execute_unprepared(
            r#"
            DROP TRIGGER IF EXISTS projects_record_deleted_images ON projects;
            CREATE TRIGGER projects_record_deleted_images
                AFTER DELETE ON projects
                FOR EACH ROW
                EXECUTE FUNCTION record_deleted_project_images();
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
                DROP TRIGGER IF EXISTS projects_record_deleted_images ON projects;
                DROP FUNCTION IF EXISTS record_deleted_project_images();
                DROP TABLE IF EXISTS deleted_project_images;
                "#,
            )
            .await?;
        Ok(())
    }
}
