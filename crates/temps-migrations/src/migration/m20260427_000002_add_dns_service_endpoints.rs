//! Adds the schema for the internal DNS layer (ADR-011): authoritative service
//! endpoint records + per-node resolver applied-state.
//!
//! 1. `service_endpoints` — every A/AAAA/SRV/CNAME record the cluster serves
//!    over its internal `*.temps.local` zone. Written by:
//!      - container lifecycle hooks (Tier 2: `<svc>-<ord>.<svc>.temps.local`)
//!      - per-cluster reconcilers (Tier 3: `primary.<svc>...`, `replica.<svc>...`)
//!      - static cluster setup (e.g. `<node>.nodes.temps.local`)
//!
//!    The unique index is on `(fqdn, record_type, target_ip)` so a name can
//!    have multiple A records (multi-A round-robin for VIPs / replica sets)
//!    but the same target IP can't be inserted twice for the same name+type.
//!
//!    `target_ip` is `TEXT`, not Postgres `inet`, to match the existing
//!    convention used by `nodes.private_address`, `nodes.compute_cidr`, and
//!    `nodes.underlay_address`. Parsing happens at the Rust boundary. Same
//!    column carries v4 and v6.
//!
//!    `generation` is a monotonic counter bumped on every mutation. Per-node
//!    agents long-poll for changes since their last applied generation, so
//!    the column needs to be cheap to filter on (hence the secondary index).
//!
//! 2. `node_dns_state` — keyed on `node_id`, tracks which generation each
//!    node's resolver has applied and when it last synced. Lets ops detect
//!    drift (`last_sync_at` stale, `applied_generation` lagging).
//!
//! Enum-shaped string columns (`record_type`, `owner_kind`, `health`) are
//! constrained with CHECK constraints rather than Postgres ENUM types,
//! matching the pattern in `m20260427_000001_add_compute_network` for
//! `network_config.transport`.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // 1. service_endpoints — authoritative records for the internal zone.
        manager
            .create_table(
                Table::create()
                    .table(ServiceEndpoints::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(ServiceEndpoints::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(ServiceEndpoints::Fqdn).text().not_null())
                    .col(
                        ColumnDef::new(ServiceEndpoints::RecordType)
                            .text()
                            .not_null(),
                    )
                    // Nullable for record types that don't carry an address
                    // (CNAME points at another fqdn — encoded in `target_ip`
                    // as the target name when `record_type = 'CNAME'`).
                    .col(ColumnDef::new(ServiceEndpoints::TargetIp).text().null())
                    // Required for SRV; null for plain A/AAAA when there's
                    // no port semantics (host alias only).
                    .col(
                        ColumnDef::new(ServiceEndpoints::TargetPort)
                            .integer()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(ServiceEndpoints::Ttl)
                            .integer()
                            .not_null()
                            .default(30),
                    )
                    .col(
                        ColumnDef::new(ServiceEndpoints::OwnerKind)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ServiceEndpoints::OwnerId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(ServiceEndpoints::NodeId).integer().null())
                    .col(
                        ColumnDef::new(ServiceEndpoints::Generation)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ServiceEndpoints::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(ServiceEndpoints::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_service_endpoints_node_id")
                            .from(ServiceEndpoints::Table, ServiceEndpoints::NodeId)
                            .to(Nodes::Table, Nodes::Id)
                            .on_delete(ForeignKeyAction::SetNull),
                    )
                    .to_owned(),
            )
            .await?;

        let db = manager.get_connection();

        // CHECK constraints — enum-shaped string columns. Mirrors
        // `network_config.transport` pattern (no Postgres ENUM types in this
        // codebase; constraints + Rust-side parsing instead).
        db.execute_unprepared(
            "ALTER TABLE service_endpoints \
             ADD CONSTRAINT service_endpoints_record_type_valid \
             CHECK (record_type IN ('A', 'AAAA', 'SRV', 'CNAME'))",
        )
        .await?;
        db.execute_unprepared(
            "ALTER TABLE service_endpoints \
             ADD CONSTRAINT service_endpoints_owner_kind_valid \
             CHECK (owner_kind IN ('service_member', 'service_role', 'node', 'static'))",
        )
        .await?;

        // Unique on (fqdn, record_type, target_ip): a name can have many A
        // records (multi-A for replicas), but the same IP must not be
        // inserted twice under the same name+type. NULL target_ip never
        // collides (PG NULLs aren't equal), which is fine — CNAMEs and
        // address-less aliases are rare enough that we don't need a
        // partial-unique index.
        db.execute_unprepared(
            "CREATE UNIQUE INDEX IF NOT EXISTS service_endpoints_uniq \
             ON service_endpoints (fqdn, record_type, target_ip)",
        )
        .await?;

        // Generation index — agents filter `WHERE generation > $applied`
        // on every long-poll. Heavy read pattern, light write pattern.
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS service_endpoints_generation_idx \
             ON service_endpoints (generation)",
        )
        .await?;

        // Owner index — needed by `delete_by_owner(owner_kind, owner_id)`
        // when a container/service member goes away (lifecycle hook).
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS service_endpoints_owner_idx \
             ON service_endpoints (owner_kind, owner_id)",
        )
        .await?;

        // 2. node_dns_state — applied-state per node, drift detection.
        manager
            .create_table(
                Table::create()
                    .table(NodeDnsState::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(NodeDnsState::NodeId)
                            .integer()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(NodeDnsState::AppliedGeneration)
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(NodeDnsState::LastSyncAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(NodeDnsState::Health)
                            .text()
                            .not_null()
                            .default("unknown"),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_node_dns_state_node_id")
                            .from(NodeDnsState::Table, NodeDnsState::NodeId)
                            .to(Nodes::Table, Nodes::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        db.execute_unprepared(
            "ALTER TABLE node_dns_state \
             ADD CONSTRAINT node_dns_state_health_valid \
             CHECK (health IN ('healthy', 'degraded', 'stale', 'unknown'))",
        )
        .await?;

        // 3. dns_generation — cluster-wide monotonic counter.
        //
        // Lives in its own table (not derived from MAX(service_endpoints.generation))
        // so that:
        //   - deleting all rows from service_endpoints does NOT reset the
        //     counter (which would break the long-poll "since=N" invariant
        //     for any agent that already saw a higher value);
        //   - no-op deletes can be detected without bumping;
        //   - the counter is well-defined on a fresh install (defaults to 0).
        //
        // Singleton-by-construction via CHECK (id = 1), same pattern as
        // network_config from m20260427_000001.
        manager
            .create_table(
                Table::create()
                    .table(DnsGeneration::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(DnsGeneration::Id)
                            .integer()
                            .not_null()
                            .primary_key()
                            .default(1),
                    )
                    .col(
                        ColumnDef::new(DnsGeneration::Current)
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(DnsGeneration::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await?;
        db.execute_unprepared(
            "ALTER TABLE dns_generation \
             ADD CONSTRAINT dns_generation_singleton CHECK (id = 1)",
        )
        .await?;
        db.execute_unprepared(
            "INSERT INTO dns_generation (id, current) VALUES (1, 0) \
             ON CONFLICT (id) DO NOTHING",
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(DnsGeneration::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(NodeDnsState::Table).to_owned())
            .await?;

        let db = manager.get_connection();
        db.execute_unprepared("DROP INDEX IF EXISTS service_endpoints_owner_idx")
            .await?;
        db.execute_unprepared("DROP INDEX IF EXISTS service_endpoints_generation_idx")
            .await?;
        db.execute_unprepared("DROP INDEX IF EXISTS service_endpoints_uniq")
            .await?;

        manager
            .drop_table(Table::drop().table(ServiceEndpoints::Table).to_owned())
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum ServiceEndpoints {
    Table,
    Id,
    Fqdn,
    RecordType,
    TargetIp,
    TargetPort,
    Ttl,
    OwnerKind,
    OwnerId,
    NodeId,
    Generation,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum NodeDnsState {
    Table,
    NodeId,
    AppliedGeneration,
    LastSyncAt,
    Health,
}

#[derive(DeriveIden)]
enum DnsGeneration {
    Table,
    Id,
    Current,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum Nodes {
    Table,
    Id,
}
