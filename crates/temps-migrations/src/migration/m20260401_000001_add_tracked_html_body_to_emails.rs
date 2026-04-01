use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Emails::Table)
                    .add_column(ColumnDef::new(Emails::TrackedHtmlBody).text().null())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Emails::Table)
                    .drop_column(Emails::TrackedHtmlBody)
                    .to_owned(),
            )
            .await
    }
}

#[derive(Iden)]
enum Emails {
    Table,
    TrackedHtmlBody,
}
