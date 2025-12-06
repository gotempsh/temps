use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Step 1: Drop the foreign key constraint on domain_id
        manager
            .drop_foreign_key(
                ForeignKey::drop()
                    .name("fk_emails_domain")
                    .table(Emails::Table)
                    .to_owned(),
            )
            .await?;

        // Step 2: Make domain_id nullable
        manager
            .alter_table(
                Table::alter()
                    .table(Emails::Table)
                    .modify_column(ColumnDef::new(Emails::DomainId).integer().null())
                    .to_owned(),
            )
            .await?;

        // Step 3: Re-add the foreign key constraint with ON DELETE SET NULL
        manager
            .create_foreign_key(
                ForeignKey::create()
                    .name("fk_emails_domain")
                    .from(Emails::Table, Emails::DomainId)
                    .to(EmailDomains::Table, EmailDomains::Id)
                    .on_delete(ForeignKeyAction::SetNull)
                    .to_owned(),
            )
            .await?;

        // Step 4: Add unique index on email_domains.domain for efficient lookups
        // First check if the index already exists (it might from provider+domain unique constraint)
        // We want a simple unique index on just the domain name
        manager
            .create_index(
                Index::create()
                    .name("idx_email_domains_domain_unique")
                    .table(EmailDomains::Table)
                    .col(EmailDomains::Domain)
                    .unique()
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Step 1: Drop the unique index on domain
        manager
            .drop_index(
                Index::drop()
                    .name("idx_email_domains_domain_unique")
                    .table(EmailDomains::Table)
                    .to_owned(),
            )
            .await?;

        // Step 2: Drop the foreign key constraint
        manager
            .drop_foreign_key(
                ForeignKey::drop()
                    .name("fk_emails_domain")
                    .table(Emails::Table)
                    .to_owned(),
            )
            .await?;

        // Step 3: Make domain_id NOT NULL again (this will fail if there are NULL values)
        manager
            .alter_table(
                Table::alter()
                    .table(Emails::Table)
                    .modify_column(ColumnDef::new(Emails::DomainId).integer().not_null())
                    .to_owned(),
            )
            .await?;

        // Step 4: Re-add the foreign key constraint with ON DELETE CASCADE
        manager
            .create_foreign_key(
                ForeignKey::create()
                    .name("fk_emails_domain")
                    .from(Emails::Table, Emails::DomainId)
                    .to(EmailDomains::Table, EmailDomains::Id)
                    .on_delete(ForeignKeyAction::Cascade)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum Emails {
    Table,
    DomainId,
}

#[derive(DeriveIden)]
enum EmailDomains {
    Table,
    Id,
    Domain,
}
