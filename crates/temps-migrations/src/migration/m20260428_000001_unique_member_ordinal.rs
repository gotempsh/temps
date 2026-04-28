//! Adds `UNIQUE(service_id, ordinal)` to `service_members`.
//!
//! ## Why
//!
//! `add_cluster_member` computes the next ordinal as
//! `MAX(ordinal) + 1` over the current rows, then inserts.
//! Two concurrent calls (two operators clicking "Add Replica" at the
//! same instant, or one operator + a retry) both observe the same
//! max, both insert the same `(service_id, ordinal)` pair, and both
//! background tasks then race to create `postgres-<svc>-N` containers.
//! Docker's `create_container` rejects the second by name, so we end
//! up with one orphan failed `service_members` row at the same
//! ordinal. The same race also produces a host-port collision
//! (`base_port + ordinal`).
//!
//! With this constraint, the second `INSERT` fails with a unique
//! violation; the calling code retries with the next free ordinal
//! and the cluster ends up with N consistent members.
//!
//! ## Migration safety
//!
//! Pre-existing rows should already be unique (the bug only triggers
//! under simultaneous create requests, which the dev cluster has not
//! seen) — we add the constraint optimistically. If a dirty
//! environment has duplicates, the migration will fail with a clear
//! error pointing at the offending pair, and the operator can clean
//! the bad rows manually before retrying.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_index(
                Index::create()
                    .name("uq_service_members_service_id_ordinal")
                    .table(ServiceMembers::Table)
                    .col(ServiceMembers::ServiceId)
                    .col(ServiceMembers::Ordinal)
                    .unique()
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                Index::drop()
                    .name("uq_service_members_service_id_ordinal")
                    .table(ServiceMembers::Table)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum ServiceMembers {
    Table,
    ServiceId,
    Ordinal,
}
