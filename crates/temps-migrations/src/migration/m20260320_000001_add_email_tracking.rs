// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Add tracking columns to emails table
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("emails"))
                    .add_column(
                        ColumnDef::new(Alias::new("track_opens"))
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .add_column(
                        ColumnDef::new(Alias::new("track_clicks"))
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .add_column(
                        ColumnDef::new(Alias::new("open_count"))
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .add_column(
                        ColumnDef::new(Alias::new("click_count"))
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .add_column(
                        ColumnDef::new(Alias::new("first_opened_at"))
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .add_column(
                        ColumnDef::new(Alias::new("first_clicked_at"))
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;

        // Create email_events table for detailed tracking
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("email_events"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Alias::new("email_id")).uuid().not_null())
                    .col(
                        ColumnDef::new(Alias::new("event_type"))
                            .string_len(32)
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("link_url")).text().null())
                    .col(ColumnDef::new(Alias::new("link_index")).integer().null())
                    .col(
                        ColumnDef::new(Alias::new("ip_address"))
                            .string_len(45)
                            .null(),
                    )
                    .col(ColumnDef::new(Alias::new("user_agent")).text().null())
                    .col(
                        ColumnDef::new(Alias::new("created_at"))
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await?;

        // Create email_links table to map link_index -> original URL
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("email_links"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Alias::new("email_id")).uuid().not_null())
                    .col(
                        ColumnDef::new(Alias::new("link_index"))
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("original_url")).text().not_null())
                    .col(
                        ColumnDef::new(Alias::new("click_count"))
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .to_owned(),
            )
            .await?;

        // Index on email_events(email_id) for fast lookups
        manager
            .create_index(
                Index::create()
                    .name("idx_email_events_email_id")
                    .table(Alias::new("email_events"))
                    .col(Alias::new("email_id"))
                    .to_owned(),
            )
            .await?;

        // Index on email_events(event_type) for filtering
        manager
            .create_index(
                Index::create()
                    .name("idx_email_events_event_type")
                    .table(Alias::new("email_events"))
                    .col(Alias::new("event_type"))
                    .to_owned(),
            )
            .await?;

        // Index on email_links(email_id, link_index) for click tracking lookups
        manager
            .create_index(
                Index::create()
                    .name("idx_email_links_email_id_link_index")
                    .table(Alias::new("email_links"))
                    .col(Alias::new("email_id"))
                    .col(Alias::new("link_index"))
                    .unique()
                    .to_owned(),
            )
            .await?;

        // Foreign key on email_events.email_id -> emails.id
        manager
            .create_foreign_key(
                ForeignKey::create()
                    .name("fk_email_events_email_id")
                    .from(Alias::new("email_events"), Alias::new("email_id"))
                    .to(Alias::new("emails"), Alias::new("id"))
                    .on_delete(ForeignKeyAction::Cascade)
                    .to_owned(),
            )
            .await?;

        // Foreign key on email_links.email_id -> emails.id
        manager
            .create_foreign_key(
                ForeignKey::create()
                    .name("fk_email_links_email_id")
                    .from(Alias::new("email_links"), Alias::new("email_id"))
                    .to(Alias::new("emails"), Alias::new("id"))
                    .on_delete(ForeignKeyAction::Cascade)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Alias::new("email_events")).to_owned())
            .await?;

        manager
            .drop_table(Table::drop().table(Alias::new("email_links")).to_owned())
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("emails"))
                    .drop_column(Alias::new("track_opens"))
                    .drop_column(Alias::new("track_clicks"))
                    .drop_column(Alias::new("open_count"))
                    .drop_column(Alias::new("click_count"))
                    .drop_column(Alias::new("first_opened_at"))
                    .drop_column(Alias::new("first_clicked_at"))
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}
