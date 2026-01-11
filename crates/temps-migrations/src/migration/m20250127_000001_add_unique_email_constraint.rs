use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Add unique constraint on users.email
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_users_email_unique")
                    .table(Alias::new("users"))
                    .col(Alias::new("email"))
                    .unique()
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Drop the unique constraint
        manager
            .drop_index(
                Index::drop()
                    .name("idx_users_email_unique")
                    .table(Alias::new("users"))
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}
