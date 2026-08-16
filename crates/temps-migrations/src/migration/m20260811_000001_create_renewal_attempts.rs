//! Migration to create the `renewal_attempts` table.
//!
//! Append-only audit log for the standard (non-on-demand) certificate
//! issuance/renewal flow driven by `DomainService::request_challenge` and
//! `DomainService::complete_challenge` — the path used by regular managed
//! domains (scheduled 3 AM renewal cron, and the manual "Renew certificate"
//! action), as opposed to `on_demand_cert_attempts` which covers the proxy's
//! lazy on-demand TLS issuance for stable hostnames.
//!
//! Before this table existed, `domains.last_error` / `last_error_type` held
//! only the MOST RECENT failure, overwritten on every attempt, and a failure
//! in `request_challenge` (order creation) was not persisted anywhere at all
//! — only `complete_challenge` (order finalization) failures were. This table
//! gives every attempt, at either stage, a permanent queryable row so the
//! Certificates UI can show a renewal history instead of a single fact that
//! silently disappears on the next attempt.
//!
//! Additive-only: invisible to a pre-migration binary, matching the same
//! backward-compatibility pattern as `on_demand_cert_attempts`.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(RenewalAttempts::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(RenewalAttempts::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(RenewalAttempts::DomainId)
                            .integer()
                            .not_null(),
                    )
                    // "request_challenge" | "complete_challenge".
                    .col(ColumnDef::new(RenewalAttempts::Stage).text().not_null())
                    // "http-01" | "dns-01".
                    .col(
                        ColumnDef::new(RenewalAttempts::VerificationMethod)
                            .text()
                            .not_null(),
                    )
                    // "success" | "failed".
                    .col(ColumnDef::new(RenewalAttempts::Outcome).text().not_null())
                    .col(ColumnDef::new(RenewalAttempts::Error).text().null())
                    .col(ColumnDef::new(RenewalAttempts::ErrorType).text().null())
                    .col(
                        ColumnDef::new(RenewalAttempts::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_renewal_attempts_domain_id")
                            .from(RenewalAttempts::Table, RenewalAttempts::DomainId)
                            .to(Domains::Table, Domains::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // Hot read path: timeline for one domain, most recent first.
        manager
            .create_index(
                Index::create()
                    .name("idx_renewal_attempts_domain_id_created_at")
                    .table(RenewalAttempts::Table)
                    .col(RenewalAttempts::DomainId)
                    .col(RenewalAttempts::CreatedAt)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(RenewalAttempts::Table).to_owned())
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum RenewalAttempts {
    Table,
    Id,
    DomainId,
    Stage,
    VerificationMethod,
    Outcome,
    Error,
    ErrorType,
    CreatedAt,
}

#[derive(DeriveIden)]
enum Domains {
    Table,
    Id,
}
