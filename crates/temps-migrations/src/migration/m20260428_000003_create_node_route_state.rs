//! Per-node applied-state for the worker-side internal route store.
//!
//! Mirror of `node_dns_state`. Each row tracks the highest in-memory
//! route-table generation a worker's `route_sync_client` has applied
//! and ACKed back to the CP. `mark_deployment_complete` waits on
//! `MIN(applied_generation)` across healthy workers to reach the
//! generation the CP observed after its own route reload before
//! marking the deployment "completed". Without this, "completed"
//! would fire before workers actually know about the new deployment,
//! producing transient 502s on the propagation window (~100-500ms).
//!
//! Same shape decisions as `node_dns_state`:
//!   - `node_id` is PK (one row per node)
//!   - `applied_generation` defaults to 0 (no applies yet)
//!   - `last_sync_at` is NULL until the first ACK
//!   - `health` constrained to a small enum-ish set, with `'unknown'`
//!     as the post-migration default
//!   - FK is `ON DELETE CASCADE` so removing a node sweeps its state.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[derive(DeriveIden)]
enum NodeRouteState {
    Table,
    NodeId,
    AppliedGeneration,
    LastSyncAt,
    Health,
}

#[derive(DeriveIden)]
enum Nodes {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum RouteGeneration {
    Table,
    Id,
    Current,
    UpdatedAt,
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(NodeRouteState::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(NodeRouteState::NodeId)
                            .integer()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(NodeRouteState::AppliedGeneration)
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(NodeRouteState::LastSyncAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(NodeRouteState::Health)
                            .text()
                            .not_null()
                            .default("unknown"),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_node_route_state_node_id")
                            .from(NodeRouteState::Table, NodeRouteState::NodeId)
                            .to(Nodes::Table, Nodes::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE node_route_state \
                 ADD CONSTRAINT node_route_state_health_valid \
                 CHECK (health IN ('healthy', 'degraded', 'stale', 'unknown'))",
            )
            .await?;

        // Singleton table tracking the CP's current in-memory
        // route-table generation. CP bumps this on every successful
        // `load_routes()`; mark_deployment_complete reads it as the
        // anchor that workers must catch up to before the deploy is
        // declared "completed". Mirror of the existing dns_generation
        // singleton — same one-row-with-CHECK pattern.
        manager
            .create_table(
                Table::create()
                    .table(RouteGeneration::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(RouteGeneration::Id)
                            .integer()
                            .not_null()
                            .default(1)
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(RouteGeneration::Current)
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(RouteGeneration::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .check(Expr::col(RouteGeneration::Id).eq(1))
                    .to_owned(),
            )
            .await?;

        manager
            .get_connection()
            .execute_unprepared(
                "INSERT INTO route_generation (id, current) VALUES (1, 0) \
                 ON CONFLICT (id) DO NOTHING",
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(RouteGeneration::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(NodeRouteState::Table).to_owned())
            .await
    }
}
