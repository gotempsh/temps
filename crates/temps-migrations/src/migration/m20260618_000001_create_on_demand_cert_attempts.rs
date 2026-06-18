//! Migration to create the `on_demand_cert_attempts` table (ADR-018 §5).
//!
//! This is the append-only audit log for on-demand TLS issuance. Every
//! attempt — issued, failed, or skipped — produces exactly one row carrying the
//! full error chain and category, so a failed handshake is never an opaque TLS
//! error: the operator can query the reason from the console "Certificates" UI
//! or `temps domain cert-status`.
//!
//! Additive-only and backward-compatible with the N-1 proxy binary (ADR §9):
//! the new table is invisible to a pre-migration binary, and the post-migration
//! binary only writes to it when `on_demand_tls_enabled` is set.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(OnDemandCertAttempts::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(OnDemandCertAttempts::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    // SNI hostname that triggered the attempt.
                    .col(
                        ColumnDef::new(OnDemandCertAttempts::Hostname)
                            .text()
                            .not_null(),
                    )
                    // What triggered it. Always "tls_callback" in this feature.
                    .col(
                        ColumnDef::new(OnDemandCertAttempts::Trigger)
                            .text()
                            .not_null(),
                    )
                    // Did the proxy serve the ACME HTTP-01 challenge request?
                    // NULL when the attempt was skipped before any challenge.
                    .col(
                        ColumnDef::new(OnDemandCertAttempts::ChallengeServed)
                            .boolean()
                            .null(),
                    )
                    // Did we reach the Let's Encrypt API? NULL when skipped.
                    .col(
                        ColumnDef::new(OnDemandCertAttempts::AcmeRequestSent)
                            .boolean()
                            .null(),
                    )
                    // HTTP status or ACME error type from Let's Encrypt.
                    .col(
                        ColumnDef::new(OnDemandCertAttempts::AcmeResponseStatus)
                            .text()
                            .null(),
                    )
                    // "issued" | "failed" | "skipped_duplicate" | "skipped_gate"
                    // | "skipped_rate_limit" | "skipped_no_route".
                    .col(
                        ColumnDef::new(OnDemandCertAttempts::Outcome)
                            .text()
                            .not_null(),
                    )
                    // Full Display chain of the error (all source() levels).
                    .col(
                        ColumnDef::new(OnDemandCertAttempts::ErrorChain)
                            .text()
                            .null(),
                    )
                    // Coarse category for UI labelling: "rate_limited",
                    // "dns_failure", "acme_order_expired", "challenge_mismatch",
                    // "timeout", "internal", or NULL.
                    .col(
                        ColumnDef::new(OnDemandCertAttempts::ErrorCategory)
                            .text()
                            .null(),
                    )
                    // End-to-end issuance duration in ms (0/NULL for skipped).
                    .col(
                        ColumnDef::new(OnDemandCertAttempts::DurationMs)
                            .integer()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(OnDemandCertAttempts::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await?;

        // Hot read path: look up the latest attempts for one hostname.
        manager
            .create_index(
                Index::create()
                    .name("idx_on_demand_cert_attempts_hostname")
                    .table(OnDemandCertAttempts::Table)
                    .col(OnDemandCertAttempts::Hostname)
                    .to_owned(),
            )
            .await?;

        // Time-ordered listing for the console "Certificates" surface and for
        // the retain-last-90-days cleanup job (ADR §Consequences).
        manager
            .create_index(
                Index::create()
                    .name("idx_on_demand_cert_attempts_created_at")
                    .table(OnDemandCertAttempts::Table)
                    .col(OnDemandCertAttempts::CreatedAt)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(OnDemandCertAttempts::Table).to_owned())
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum OnDemandCertAttempts {
    Table,
    Id,
    Hostname,
    Trigger,
    ChallengeServed,
    AcmeRequestSent,
    AcmeResponseStatus,
    Outcome,
    ErrorChain,
    ErrorCategory,
    DurationMs,
    CreatedAt,
}
