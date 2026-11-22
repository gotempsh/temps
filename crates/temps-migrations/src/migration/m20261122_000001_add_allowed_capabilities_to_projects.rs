// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `projects.allowed_capabilities`: the Linux capabilities a project's
//! containers get back after `cap_drop: ALL`.
//!
//! Every project container is created fully hardened (`cap_drop: ALL`,
//! `no-new-privileges`, a PID limit, init). An image whose entrypoint was
//! marked with `setcap cap_net_bind_service=+ep` cannot even be executed
//! under that, and there was no per-project way to fix it.
//!
//! The column is an allow-list, empty by default, so every existing and new
//! project keeps exactly today's behaviour. The CHECK constraint makes the
//! database refuse anything but the values the application allows
//! (`temps_workload::ContainerCapability`), so neither a bug nor a direct
//! write can store `SYS_ADMIN`: widening the allow-list takes a migration.
//!
//! A constant default is a catalog-only change (no table rewrite); the
//! CHECK is validated against the `projects` table, which is small. The
//! previous binary does not read the column and its inserts take the
//! default, so a rollback keeps working with this schema in place.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE projects
                    ADD COLUMN IF NOT EXISTS allowed_capabilities text[] NOT NULL DEFAULT '{}';
                 ALTER TABLE projects
                    DROP CONSTRAINT IF EXISTS projects_allowed_capabilities_allow_list;
                 ALTER TABLE projects
                    ADD CONSTRAINT projects_allowed_capabilities_allow_list
                    CHECK (allowed_capabilities <@ ARRAY['NET_BIND_SERVICE']::text[]);",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Dropping the column removes the opt-in: every project goes back to
        // the fully hardened default on its next deploy, which is the safe
        // direction. Nothing else depends on it.
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE projects
                    DROP CONSTRAINT IF EXISTS projects_allowed_capabilities_allow_list;
                 ALTER TABLE projects DROP COLUMN IF EXISTS allowed_capabilities;",
            )
            .await?;
        Ok(())
    }
}
