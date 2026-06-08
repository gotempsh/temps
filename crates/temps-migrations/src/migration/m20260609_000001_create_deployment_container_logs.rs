//! Migration to create the `deployment_container_logs` table.
//!
//! Runtime container logs are normally only available live, streamed straight
//! from Docker. The moment a deployment is superseded its old containers are
//! stopped and removed (see `mark_deployment_complete::cancel_previous_deployments`),
//! and their logs vanish with them. That makes post-mortem debugging of "what
//! did the container that ran a few days ago actually print?" impossible.
//!
//! This table records, for each previous container we tear down, a pointer to a
//! captured plain-text log dump. The dump itself lives on disk under the data
//! dir (via `temps_logs::LogService`) — this row stores only the metadata and
//! the server-generated relative path to it, never user-controlled file paths.
//! Logs of replaced containers therefore survive teardown and can be viewed
//! later from the deployment detail page.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(DeploymentContainerLogs::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(DeploymentContainerLogs::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    // The deployment whose container this log belongs to. The row
                    // outlives the container (which is removed from Docker), so we
                    // key on the durable deployment id rather than the container id.
                    .col(
                        ColumnDef::new(DeploymentContainerLogs::DeploymentId)
                            .integer()
                            .not_null(),
                    )
                    // Denormalised for fast project-scoped authorization on read
                    // without a join back through deployments → environments.
                    .col(
                        ColumnDef::new(DeploymentContainerLogs::ProjectId)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(DeploymentContainerLogs::EnvironmentId)
                            .integer()
                            .not_null(),
                    )
                    // Docker container id at capture time. Kept for reference/debugging;
                    // the container itself no longer exists once this row is written.
                    .col(
                        ColumnDef::new(DeploymentContainerLogs::ContainerId)
                            .string()
                            .not_null(),
                    )
                    // Human-visible container name (e.g. "web-2"). This is
                    // what a user recognises when picking which past container's logs to read.
                    .col(
                        ColumnDef::new(DeploymentContainerLogs::ContainerName)
                            .string()
                            .not_null(),
                    )
                    // Compose service name (e.g. "web", "redis"). NULL for single-container
                    // deployments, mirroring `deployment_containers.service_name`.
                    .col(
                        ColumnDef::new(DeploymentContainerLogs::ServiceName)
                            .string_len(255)
                            .null(),
                    )
                    // Node the container ran on. NULL = local node (single-node mode),
                    // mirroring `deployment_containers.node_id`.
                    .col(
                        ColumnDef::new(DeploymentContainerLogs::NodeId)
                            .integer()
                            .null(),
                    )
                    // Server-generated relative path of the captured log file under the
                    // data dir (resolved by `LogService`). Never user-supplied, so it
                    // cannot be used for path traversal.
                    .col(
                        ColumnDef::new(DeploymentContainerLogs::LogPath)
                            .string()
                            .not_null(),
                    )
                    // Size of the captured dump in bytes. i64 to stay safe for large
                    // (multi-GB) log dumps, matching the backups size column convention.
                    .col(
                        ColumnDef::new(DeploymentContainerLogs::SizeBytes)
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    // True when the dump was truncated to the tail because the live log
                    // exceeded the capture cap. Surfaced in the UI so users know there's more.
                    .col(
                        ColumnDef::new(DeploymentContainerLogs::Truncated)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    // When the logs were captured (i.e. just before teardown).
                    .col(
                        ColumnDef::new(DeploymentContainerLogs::CapturedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(DeploymentContainerLogs::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_deployment_container_logs_deployment_id")
                            .from(
                                DeploymentContainerLogs::Table,
                                DeploymentContainerLogs::DeploymentId,
                            )
                            .to(Deployments::Table, Deployments::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // Hot read path: list all captured logs for one deployment, newest first.
        manager
            .create_index(
                Index::create()
                    .name("idx_deployment_container_logs_deployment_id")
                    .table(DeploymentContainerLogs::Table)
                    .col(DeploymentContainerLogs::DeploymentId)
                    .to_owned(),
            )
            .await?;

        // Project-scoped authorization checks on read.
        manager
            .create_index(
                Index::create()
                    .name("idx_deployment_container_logs_project_id")
                    .table(DeploymentContainerLogs::Table)
                    .col(DeploymentContainerLogs::ProjectId)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(DeploymentContainerLogs::Table)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum DeploymentContainerLogs {
    Table,
    Id,
    DeploymentId,
    ProjectId,
    EnvironmentId,
    ContainerId,
    ContainerName,
    ServiceName,
    NodeId,
    LogPath,
    SizeBytes,
    Truncated,
    CapturedAt,
    CreatedAt,
}

#[derive(DeriveIden)]
enum Deployments {
    Table,
    Id,
}
