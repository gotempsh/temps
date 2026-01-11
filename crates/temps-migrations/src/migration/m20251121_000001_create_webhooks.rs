use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // ========================================
        // WEBHOOKS TABLE
        // ========================================
        manager
            .create_table(
                Table::create()
                    .table(Webhooks::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Webhooks::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Webhooks::ProjectId).integer().not_null())
                    .col(ColumnDef::new(Webhooks::Url).string().not_null())
                    .col(ColumnDef::new(Webhooks::Secret).string().null())
                    .col(ColumnDef::new(Webhooks::Events).string().not_null())
                    .col(
                        ColumnDef::new(Webhooks::Enabled)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(
                        ColumnDef::new(Webhooks::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(Webhooks::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_webhooks_project")
                            .from(Webhooks::Table, Webhooks::ProjectId)
                            .to(Projects::Table, Projects::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // Create indexes for webhooks table
        manager
            .create_index(
                Index::create()
                    .name("idx_webhooks_project_id")
                    .table(Webhooks::Table)
                    .col(Webhooks::ProjectId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_webhooks_enabled")
                    .table(Webhooks::Table)
                    .col(Webhooks::Enabled)
                    .to_owned(),
            )
            .await?;

        // ========================================
        // WEBHOOK_DELIVERIES TABLE
        // ========================================
        manager
            .create_table(
                Table::create()
                    .table(WebhookDeliveries::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(WebhookDeliveries::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(WebhookDeliveries::WebhookId)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(WebhookDeliveries::EventType)
                            .string()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(WebhookDeliveries::EventId)
                            .string()
                            .not_null(),
                    )
                    .col(ColumnDef::new(WebhookDeliveries::Payload).text().not_null())
                    .col(
                        ColumnDef::new(WebhookDeliveries::Success)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(WebhookDeliveries::StatusCode)
                            .integer()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(WebhookDeliveries::ResponseBody)
                            .text()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(WebhookDeliveries::ErrorMessage)
                            .string()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(WebhookDeliveries::AttemptNumber)
                            .integer()
                            .not_null()
                            .default(1),
                    )
                    .col(
                        ColumnDef::new(WebhookDeliveries::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(WebhookDeliveries::DeliveredAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_webhook_deliveries_webhook")
                            .from(WebhookDeliveries::Table, WebhookDeliveries::WebhookId)
                            .to(Webhooks::Table, Webhooks::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // Create indexes for webhook_deliveries table
        manager
            .create_index(
                Index::create()
                    .name("idx_webhook_deliveries_webhook_id")
                    .table(WebhookDeliveries::Table)
                    .col(WebhookDeliveries::WebhookId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_webhook_deliveries_event_type")
                    .table(WebhookDeliveries::Table)
                    .col(WebhookDeliveries::EventType)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_webhook_deliveries_success")
                    .table(WebhookDeliveries::Table)
                    .col(WebhookDeliveries::Success)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_webhook_deliveries_created_at")
                    .table(WebhookDeliveries::Table)
                    .col(WebhookDeliveries::CreatedAt)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Drop indexes for webhook_deliveries
        manager
            .drop_index(
                Index::drop()
                    .name("idx_webhook_deliveries_created_at")
                    .table(WebhookDeliveries::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name("idx_webhook_deliveries_success")
                    .table(WebhookDeliveries::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name("idx_webhook_deliveries_event_type")
                    .table(WebhookDeliveries::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name("idx_webhook_deliveries_webhook_id")
                    .table(WebhookDeliveries::Table)
                    .to_owned(),
            )
            .await?;

        // Drop webhook_deliveries table
        manager
            .drop_table(Table::drop().table(WebhookDeliveries::Table).to_owned())
            .await?;

        // Drop indexes for webhooks
        manager
            .drop_index(
                Index::drop()
                    .name("idx_webhooks_enabled")
                    .table(Webhooks::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name("idx_webhooks_project_id")
                    .table(Webhooks::Table)
                    .to_owned(),
            )
            .await?;

        // Drop webhooks table
        manager
            .drop_table(Table::drop().table(Webhooks::Table).to_owned())
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum Projects {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum Webhooks {
    Table,
    Id,
    ProjectId,
    Url,
    Secret,
    Events,
    Enabled,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum WebhookDeliveries {
    Table,
    Id,
    WebhookId,
    EventType,
    EventId,
    Payload,
    Success,
    StatusCode,
    ResponseBody,
    ErrorMessage,
    AttemptNumber,
    CreatedAt,
    DeliveredAt,
}
