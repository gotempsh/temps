use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Add session_id and visitor_id columns to proxy_logs table
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("proxy_logs"))
                    .add_column(ColumnDef::new(Alias::new("session_id")).integer().null())
                    .add_column(ColumnDef::new(Alias::new("visitor_id")).integer().null())
                    .to_owned(),
            )
            .await?;

        // Add foreign key constraint for session_id to request_sessions
        manager
            .create_foreign_key(
                ForeignKey::create()
                    .name("fk_proxy_logs_session")
                    .from(Alias::new("proxy_logs"), Alias::new("session_id"))
                    .to(Alias::new("request_sessions"), Alias::new("id"))
                    .on_delete(ForeignKeyAction::SetNull)
                    .to_owned(),
            )
            .await?;

        // Add foreign key constraint for visitor_id to visitor
        manager
            .create_foreign_key(
                ForeignKey::create()
                    .name("fk_proxy_logs_visitor")
                    .from(Alias::new("proxy_logs"), Alias::new("visitor_id"))
                    .to(Alias::new("visitor"), Alias::new("id"))
                    .on_delete(ForeignKeyAction::SetNull)
                    .to_owned(),
            )
            .await?;

        // Create index on session_id for better query performance
        manager
            .create_index(
                Index::create()
                    .name("idx_proxy_logs_session_id")
                    .table(Alias::new("proxy_logs"))
                    .col(Alias::new("session_id"))
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Drop index
        manager
            .drop_index(
                Index::drop()
                    .name("idx_proxy_logs_session_id")
                    .table(Alias::new("proxy_logs"))
                    .to_owned(),
            )
            .await?;

        // Drop foreign keys
        manager
            .drop_foreign_key(
                ForeignKey::drop()
                    .name("fk_proxy_logs_session")
                    .table(Alias::new("proxy_logs"))
                    .to_owned(),
            )
            .await?;

        manager
            .drop_foreign_key(
                ForeignKey::drop()
                    .name("fk_proxy_logs_visitor")
                    .table(Alias::new("proxy_logs"))
                    .to_owned(),
            )
            .await?;

        // Drop columns
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("proxy_logs"))
                    .drop_column(Alias::new("session_id"))
                    .drop_column(Alias::new("visitor_id"))
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}
