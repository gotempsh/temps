// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Migration to add `hosting` to the `projects` table.
//!
//! `hosted` (the default, and every existing project) means Temps builds and
//! runs the app. `external` means the app runs elsewhere (Vercel, Netlify, a
//! VPS) and Temps only receives its analytics, errors and traces: such a
//! project is never deployed, and the console labels it External.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        // Add the column without the inline CHECK so we can use NOT VALID +
        // VALIDATE CONSTRAINT to avoid an ACCESS EXCLUSIVE lock while scanning
        // the table. This is safe because the column has a DEFAULT of 'hosted'
        // and only two callers write it (set_hosting and the migration itself).
        db.execute_unprepared(
            "ALTER TABLE projects \
             ADD COLUMN IF NOT EXISTS hosting TEXT NOT NULL DEFAULT 'hosted'",
        )
        .await?;
        // Add the constraint in a non-blocking way: NOT VALID skips the table
        // scan; the subsequent VALIDATE acquires only SHARE UPDATE EXCLUSIVE.
        // Idempotent: IF NOT EXISTS is not available for CHECK constraints, so
        // we use a DO block instead.
        db.execute_unprepared(
            "DO $$ BEGIN \
               IF NOT EXISTS ( \
                 SELECT 1 FROM pg_constraint \
                  WHERE conname = 'projects_hosting_check' \
                    AND conrelid = 'projects'::regclass \
               ) THEN \
                 ALTER TABLE projects \
                   ADD CONSTRAINT projects_hosting_check \
                   CHECK (hosting IN ('hosted', 'external')) NOT VALID; \
               END IF; \
             END $$",
        )
        .await?;
        db.execute_unprepared("ALTER TABLE projects VALIDATE CONSTRAINT projects_hosting_check")
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("ALTER TABLE projects DROP COLUMN IF EXISTS hosting")
            .await?;
        Ok(())
    }
}
