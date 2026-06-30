//! Node enrollment tokens (ADR-020 WS-1.1).
//!
//! Replaces the single shared, non-expiring join token with short-lived,
//! single-use (or bounded-use), optionally node-scoped enrollment tokens. Only
//! the SHA-256 hash of each token is stored; the plaintext is shown once at
//! mint time. The legacy shared token keeps working behind
//! `settings.multi_node.legacy_shared_token_enabled` during upgrade.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(NodeEnrollmentTokens::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(NodeEnrollmentTokens::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    // SHA-256 hex of the plaintext token — never store plaintext.
                    .col(
                        ColumnDef::new(NodeEnrollmentTokens::TokenHash)
                            .string_len(64)
                            .not_null()
                            .unique_key(),
                    )
                    .col(
                        ColumnDef::new(NodeEnrollmentTokens::MaxUses)
                            .integer()
                            .not_null()
                            .default(1),
                    )
                    .col(
                        ColumnDef::new(NodeEnrollmentTokens::UsedCount)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(NodeEnrollmentTokens::ExpiresAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    // Optional pin: token only valid for this node name.
                    .col(
                        ColumnDef::new(NodeEnrollmentTokens::BoundNodeName)
                            .string_len(100)
                            .null(),
                    )
                    // Optional pin: required scheduling labels for the joining node.
                    .col(
                        ColumnDef::new(NodeEnrollmentTokens::BoundLabels)
                            .json_binary()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(NodeEnrollmentTokens::CreatedByUserId)
                            .integer()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(NodeEnrollmentTokens::RevokedAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    // SHA-256 fingerprint of the cluster CA, so a joining node can
                    // verify the control plane out of band (ADR-020 WS-2.2).
                    .col(
                        ColumnDef::new(NodeEnrollmentTokens::CaFingerprint)
                            .string_len(64)
                            .null(),
                    )
                    .col(
                        ColumnDef::new(NodeEnrollmentTokens::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(NodeEnrollmentTokens::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await?;

        // Index for expiry-sweep / cleanup queries.
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_node_enrollment_tokens_expires_at")
                    .table(NodeEnrollmentTokens::Table)
                    .col(NodeEnrollmentTokens::ExpiresAt)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                Index::drop()
                    .name("idx_node_enrollment_tokens_expires_at")
                    .table(NodeEnrollmentTokens::Table)
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(Table::drop().table(NodeEnrollmentTokens::Table).to_owned())
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum NodeEnrollmentTokens {
    Table,
    Id,
    TokenHash,
    MaxUses,
    UsedCount,
    ExpiresAt,
    BoundNodeName,
    BoundLabels,
    CreatedByUserId,
    RevokedAt,
    CaFingerprint,
    CreatedAt,
    UpdatedAt,
}
