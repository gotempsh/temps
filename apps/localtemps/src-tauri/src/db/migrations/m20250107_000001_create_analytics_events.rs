//! Migration: Create analytics_events table
//!
//! Stores all analytics events from @temps-sdk/react-analytics for inspection.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Create analytics_events table
        manager
            .create_table(
                Table::create()
                    .table(AnalyticsEvents::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(AnalyticsEvents::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(AnalyticsEvents::EventType)
                            .string()
                            .not_null(),
                    )
                    .col(ColumnDef::new(AnalyticsEvents::EventName).string())
                    .col(ColumnDef::new(AnalyticsEvents::RequestPath).string())
                    .col(ColumnDef::new(AnalyticsEvents::RequestQuery).string())
                    .col(ColumnDef::new(AnalyticsEvents::Domain).string())
                    .col(ColumnDef::new(AnalyticsEvents::SessionId).string())
                    .col(ColumnDef::new(AnalyticsEvents::RequestId).string())
                    .col(ColumnDef::new(AnalyticsEvents::Payload).text().not_null())
                    .col(
                        ColumnDef::new(AnalyticsEvents::ReceivedAt)
                            .string()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        // Create index on received_at for chronological sorting
        manager
            .create_index(
                Index::create()
                    .name("idx_analytics_received_at")
                    .table(AnalyticsEvents::Table)
                    .col(AnalyticsEvents::ReceivedAt)
                    .to_owned(),
            )
            .await?;

        // Create index on event_type for filtering
        manager
            .create_index(
                Index::create()
                    .name("idx_analytics_event_type")
                    .table(AnalyticsEvents::Table)
                    .col(AnalyticsEvents::EventType)
                    .to_owned(),
            )
            .await?;

        // Create index on event_name for filtering
        manager
            .create_index(
                Index::create()
                    .name("idx_analytics_event_name")
                    .table(AnalyticsEvents::Table)
                    .col(AnalyticsEvents::EventName)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(AnalyticsEvents::Table).to_owned())
            .await
    }
}

#[derive(Iden)]
pub enum AnalyticsEvents {
    Table,
    Id,
    EventType,
    EventName,
    RequestPath,
    RequestQuery,
    Domain,
    SessionId,
    RequestId,
    Payload,
    ReceivedAt,
}
