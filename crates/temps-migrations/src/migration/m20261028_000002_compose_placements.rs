// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm_migration::prelude::*;

/// The node each Compose environment's stack lives on.
///
/// A stack's named volumes and persistent bind directories stay on the host
/// that ran it, so once a stack has started its environment is pinned there
/// and every later deployment goes back to the same node; moving it is an
/// explicit operator action that leaves the old node's data behind.
///
/// - `node_id` NULL is the control plane, the same convention as
///   `deployment_containers.node_id`. `ON DELETE RESTRICT`: a node that holds
///   a stack's data cannot be removed until the stack is moved off it.
/// - `vacate_pending` / `vacate_node_id`: a move was requested; the next
///   deployment runs `docker compose down` (never removing volumes) on the old
///   host before starting the stack on the new one. No foreign key on
///   `vacate_node_id`: if that node is removed first there is nothing left to
///   stop, which the deployment reports instead of treating NULL as the
///   control plane.
///
/// Every environment of a Compose project that has ever run a container is
/// pinned to the control plane, because until this migration Compose stacks
/// only ever ran there.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let connection = manager.get_connection();
        connection
            .execute_unprepared(
                "CREATE TABLE compose_placements (
                    environment_id INTEGER PRIMARY KEY REFERENCES environments(id) ON DELETE CASCADE,
                    node_id INTEGER REFERENCES nodes(id) ON DELETE RESTRICT,
                    pinned_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
                    vacate_pending BOOLEAN NOT NULL DEFAULT FALSE,
                    vacate_node_id INTEGER,
                    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
                )",
            )
            .await?;
        connection
            .execute_unprepared(
                "CREATE INDEX idx_compose_placements_node_id ON compose_placements (node_id)",
            )
            .await?;
        connection
            .execute_unprepared(
                "INSERT INTO compose_placements (environment_id, node_id)
                 SELECT DISTINCT d.environment_id, NULL::INTEGER
                 FROM deployment_containers dc
                 JOIN deployments d ON d.id = dc.deployment_id
                 JOIN projects p ON p.id = d.project_id
                 JOIN environments e ON e.id = d.environment_id
                 WHERE p.preset = 'docker-compose'
                 ON CONFLICT (environment_id) DO NOTHING",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE compose_placements")
            .await?;
        Ok(())
    }
}
