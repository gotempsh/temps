// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Persist whether a deployment ever had the host Docker socket mounted into
//! one of its containers (ADR 045), so exec authorization can ask "was THIS
//! container root-equivalent" instead of "is the project's *current* slug
//! reserved".
//!
//! Renaming a project away from a granted slug is admin-only (ADR 045), but
//! it does not stop or recreate that project's already-running containers —
//! only future deployments stop being granted the socket. Before this
//! column existed, `verify_container_exec_access` re-derived socket status
//! from the project's current slug on every request, so a rename silently
//! downgraded exec authorization on a still-socket-mounted container from
//! "instance-admin only" to "anyone holding `ContainersExec`", even though
//! that container is exactly as root-equivalent as it was before the
//! rename. A security review found this reachable by a custom-permission
//! API key scoped to `ContainersExec` without instance-admin authority.
//!
//! Set once, from the executing host's own `DeployResult.docker_socket_mounted`
//! (`crates/temps-deployments/src/jobs/deploy_image.rs`), at the same point
//! the existing ADR-045 audit event is written — never cleared, since a
//! deployment that was ever root-equivalent stays sensitive for as long as
//! any of its containers exist.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "ALTER TABLE deployments \
             ADD COLUMN IF NOT EXISTS docker_socket_mounted BOOLEAN NOT NULL DEFAULT false",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "ALTER TABLE deployments DROP COLUMN IF EXISTS docker_socket_mounted",
        )
        .await?;
        Ok(())
    }
}
