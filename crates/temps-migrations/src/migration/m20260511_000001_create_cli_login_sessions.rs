//! Migration to create the `cli_login_sessions` table.
//!
//! Tracks an OAuth-2.0-style device authorization flow used by the Temps CLI
//! when running interactively (especially inside workspace sandboxes where
//! prompting for a password in the terminal is undesirable):
//!
//!   1. CLI POSTs to `/auth/cli/device/start` (anon) and receives a
//!      `device_code` (opaque, polled by the CLI) and `user_code` (short,
//!      human-readable, entered in a browser).
//!   2. CLI prints/opens `verification_uri_complete` so the user can approve
//!      in the regular web app login UI.
//!   3. CLI polls `/auth/cli/device/poll` (anon) with `device_code` until the
//!      user approves or the session expires.
//!   4. Web app calls `/auth/cli/device/lookup` and `/auth/cli/device/approve`
//!      (both authenticated) to inspect and approve the request, minting an
//!      API key tied to the session.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(CliLoginSessions::Table)
                    .col(
                        ColumnDef::new(CliLoginSessions::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    // Opaque high-entropy token polled by the CLI. Never shown to a human.
                    .col(
                        ColumnDef::new(CliLoginSessions::DeviceCode)
                            .string()
                            .not_null(),
                    )
                    // Short human-readable code (e.g. "ABCD-1234") shown in the CLI
                    // and entered in the browser. Letters chosen to avoid 0/O, 1/I.
                    .col(
                        ColumnDef::new(CliLoginSessions::UserCode)
                            .string()
                            .not_null(),
                    )
                    // `pending` | `approved` | `denied` | `expired`. Status drives the
                    // poll endpoint's response: pending -> 202, approved -> 200, etc.
                    .col(
                        ColumnDef::new(CliLoginSessions::Status)
                            .string()
                            .not_null()
                            .default("pending"),
                    )
                    // Populated only after approval. Nullable so pending rows have no user.
                    .col(ColumnDef::new(CliLoginSessions::UserId).integer().null())
                    // API key minted during approval. Nullable for the same reason.
                    .col(ColumnDef::new(CliLoginSessions::ApiKeyId).integer().null())
                    // The plaintext API key, returned to the CLI exactly once (on the
                    // first poll that observes `approved`) and then cleared. We store
                    // it briefly so the poll endpoint stays unauthenticated -- the CLI
                    // exchanges `device_code` for the key without ever holding a session.
                    .col(
                        ColumnDef::new(CliLoginSessions::ApiKeyPlaintext)
                            .text()
                            .null(),
                    )
                    // Friendly client name from the CLI (hostname). Surfaced in the
                    // browser approval screen so the user can verify what they're approving.
                    .col(ColumnDef::new(CliLoginSessions::ClientName).string().null())
                    // IP the device_code request came from. Surfaced in the approval UI.
                    .col(
                        ColumnDef::new(CliLoginSessions::RequestedIp)
                            .string()
                            .null(),
                    )
                    // Hard expiry. After this point the row is "expired" regardless of status.
                    .col(
                        ColumnDef::new(CliLoginSessions::ExpiresAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    // Rate-limit signal for `slow_down` responses on the poll endpoint.
                    .col(
                        ColumnDef::new(CliLoginSessions::LastPolledAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(CliLoginSessions::ApprovedAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(CliLoginSessions::DeniedAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(CliLoginSessions::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(CliLoginSessions::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_cli_login_sessions_user_id")
                            .from(CliLoginSessions::Table, CliLoginSessions::UserId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_cli_login_sessions_api_key_id")
                            .from(CliLoginSessions::Table, CliLoginSessions::ApiKeyId)
                            .to(ApiKeys::Table, ApiKeys::Id)
                            .on_delete(ForeignKeyAction::SetNull),
                    )
                    .to_owned(),
            )
            .await?;

        // Device code is the bearer secret the CLI polls with; must be unique
        // and indexed for the hot poll path.
        manager
            .create_index(
                Index::create()
                    .name("idx_cli_login_sessions_device_code")
                    .table(CliLoginSessions::Table)
                    .col(CliLoginSessions::DeviceCode)
                    .unique()
                    .to_owned(),
            )
            .await?;

        // User code lookups from the browser approval screen.
        manager
            .create_index(
                Index::create()
                    .name("idx_cli_login_sessions_user_code")
                    .table(CliLoginSessions::Table)
                    .col(CliLoginSessions::UserCode)
                    .unique()
                    .to_owned(),
            )
            .await?;

        // Cleanup query support: prune sessions older than retention window.
        manager
            .create_index(
                Index::create()
                    .name("idx_cli_login_sessions_expires_at")
                    .table(CliLoginSessions::Table)
                    .col(CliLoginSessions::ExpiresAt)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(CliLoginSessions::Table).to_owned())
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum CliLoginSessions {
    Table,
    Id,
    DeviceCode,
    UserCode,
    Status,
    UserId,
    ApiKeyId,
    ApiKeyPlaintext,
    ClientName,
    RequestedIp,
    ExpiresAt,
    LastPolledAt,
    ApprovedAt,
    DeniedAt,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum Users {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum ApiKeys {
    Table,
    Id,
}
