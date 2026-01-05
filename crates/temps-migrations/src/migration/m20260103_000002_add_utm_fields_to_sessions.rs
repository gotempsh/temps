//! Migration to add UTM tracking fields and channel attribution to request_sessions
//!
//! This enables tracking marketing campaign attribution with UTM parameters
//! and computed channel classification (Organic Search, Paid Social, etc.)

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[derive(DeriveIden)]
enum RequestSessions {
    Table,
    UtmSource,
    UtmMedium,
    UtmCampaign,
    UtmContent,
    UtmTerm,
    Channel,
    ReferrerHostname,
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Add UTM fields to request_sessions using SeaORM API
        // Using TEXT type for unlimited length
        manager
            .alter_table(
                Table::alter()
                    .table(RequestSessions::Table)
                    .add_column_if_not_exists(
                        ColumnDef::new(RequestSessions::UtmSource).text().null(),
                    )
                    .add_column_if_not_exists(
                        ColumnDef::new(RequestSessions::UtmMedium).text().null(),
                    )
                    .add_column_if_not_exists(
                        ColumnDef::new(RequestSessions::UtmCampaign).text().null(),
                    )
                    .add_column_if_not_exists(
                        ColumnDef::new(RequestSessions::UtmContent).text().null(),
                    )
                    .add_column_if_not_exists(
                        ColumnDef::new(RequestSessions::UtmTerm).text().null(),
                    )
                    .add_column_if_not_exists(
                        ColumnDef::new(RequestSessions::Channel).text().null(),
                    )
                    .add_column_if_not_exists(
                        ColumnDef::new(RequestSessions::ReferrerHostname)
                            .text()
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;

        // Backfill referrer_hostname from existing referrer URLs
        // This needs raw SQL due to complex CASE expression
        let db = manager.get_connection();
        db.execute_unprepared(
            r#"
            UPDATE request_sessions
            SET referrer_hostname = CASE
                WHEN referrer IS NULL OR referrer = '' THEN NULL
                WHEN referrer LIKE 'http://%' THEN
                    SPLIT_PART(SUBSTRING(referrer FROM 8), '/', 1)
                WHEN referrer LIKE 'https://%' THEN
                    SPLIT_PART(SUBSTRING(referrer FROM 9), '/', 1)
                ELSE
                    SPLIT_PART(referrer, '/', 1)
            END
            WHERE referrer IS NOT NULL AND referrer != '' AND referrer_hostname IS NULL
            "#,
        )
        .await?;

        // Create indexes for efficient querying
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_request_sessions_utm_source")
                    .table(RequestSessions::Table)
                    .col(RequestSessions::UtmSource)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_request_sessions_utm_medium")
                    .table(RequestSessions::Table)
                    .col(RequestSessions::UtmMedium)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_request_sessions_utm_campaign")
                    .table(RequestSessions::Table)
                    .col(RequestSessions::UtmCampaign)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_request_sessions_channel")
                    .table(RequestSessions::Table)
                    .col(RequestSessions::Channel)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_request_sessions_referrer_hostname")
                    .table(RequestSessions::Table)
                    .col(RequestSessions::ReferrerHostname)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Drop indexes
        manager
            .drop_index(
                Index::drop()
                    .if_exists()
                    .name("idx_request_sessions_utm_source")
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .if_exists()
                    .name("idx_request_sessions_utm_medium")
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .if_exists()
                    .name("idx_request_sessions_utm_campaign")
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .if_exists()
                    .name("idx_request_sessions_channel")
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .if_exists()
                    .name("idx_request_sessions_referrer_hostname")
                    .to_owned(),
            )
            .await?;

        // Drop columns
        manager
            .alter_table(
                Table::alter()
                    .table(RequestSessions::Table)
                    .drop_column(RequestSessions::UtmSource)
                    .drop_column(RequestSessions::UtmMedium)
                    .drop_column(RequestSessions::UtmCampaign)
                    .drop_column(RequestSessions::UtmContent)
                    .drop_column(RequestSessions::UtmTerm)
                    .drop_column(RequestSessions::Channel)
                    .drop_column(RequestSessions::ReferrerHostname)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}
