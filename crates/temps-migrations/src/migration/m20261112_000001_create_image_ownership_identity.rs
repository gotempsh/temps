// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `image_ownership_identity`: the random, install-scoped ID Temps stamps on
//! every image it builds (`sh.temps.instance`), next to the project and
//! deployment that built it.
//!
//! Project and deployment IDs are local to one database, so on a Docker host
//! shared with another Temps install (a staging instance, a developer's
//! second instance) a label `sh.temps.project_id=7` does not say whose
//! project 7 it is. The install ID does: housekeeping only ever removes an
//! orphaned image labelled with *this* install's ID, and only once that
//! install's database no longer has the project.
//!
//! A single-row table, like `dns_instance_identity` (ADR-031), and kept
//! separate from it: that ID is published in DNS ownership markers and is
//! scoped to DNS. The row is created lazily on the first build, not here: a
//! migration must not generate per-install random state. Images built before
//! this table existed carry no install label and are never touched by the
//! orphan sweep. `down` drops the table; images labelled with the old ID are
//! then left alone by every later sweep, which is the safe direction.

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
                CREATE TABLE IF NOT EXISTS image_ownership_identity (
                    id integer PRIMARY KEY CHECK (id = 1),
                    instance_id text NOT NULL CHECK (instance_id <> ''),
                    created_at timestamptz NOT NULL DEFAULT now()
                );
                "#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS image_ownership_identity;")
            .await?;
        Ok(())
    }
}
