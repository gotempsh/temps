use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // ========================================
        // EMAIL_PROVIDERS TABLE
        // ========================================
        manager
            .create_table(
                Table::create()
                    .table(EmailProviders::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(EmailProviders::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(EmailProviders::Name)
                            .string_len(255)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(EmailProviders::ProviderType)
                            .string_len(50)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(EmailProviders::Region)
                            .string_len(50)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(EmailProviders::Credentials)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(EmailProviders::IsActive)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(
                        ColumnDef::new(EmailProviders::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(EmailProviders::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await?;

        // Create index on provider_type
        manager
            .create_index(
                Index::create()
                    .name("idx_email_providers_type")
                    .table(EmailProviders::Table)
                    .col(EmailProviders::ProviderType)
                    .to_owned(),
            )
            .await?;

        // ========================================
        // EMAIL_DOMAINS TABLE
        // ========================================
        manager
            .create_table(
                Table::create()
                    .table(EmailDomains::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(EmailDomains::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(EmailDomains::ProviderId)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(EmailDomains::Domain)
                            .string_len(255)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(EmailDomains::Status)
                            .string_len(50)
                            .not_null()
                            .default("pending"),
                    )
                    .col(
                        ColumnDef::new(EmailDomains::SpfRecordName)
                            .string_len(255)
                            .null(),
                    )
                    .col(ColumnDef::new(EmailDomains::SpfRecordValue).text().null())
                    .col(
                        ColumnDef::new(EmailDomains::DkimSelector)
                            .string_len(100)
                            .null(),
                    )
                    .col(
                        ColumnDef::new(EmailDomains::DkimRecordName)
                            .string_len(255)
                            .null(),
                    )
                    .col(ColumnDef::new(EmailDomains::DkimRecordValue).text().null())
                    .col(
                        ColumnDef::new(EmailDomains::MxRecordName)
                            .string_len(255)
                            .null(),
                    )
                    .col(ColumnDef::new(EmailDomains::MxRecordValue).text().null())
                    .col(
                        ColumnDef::new(EmailDomains::MxRecordPriority)
                            .small_integer()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(EmailDomains::ProviderIdentityId)
                            .string_len(255)
                            .null(),
                    )
                    .col(
                        ColumnDef::new(EmailDomains::LastVerifiedAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(EmailDomains::VerificationError)
                            .text()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(EmailDomains::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(EmailDomains::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_email_domains_provider")
                            .from(EmailDomains::Table, EmailDomains::ProviderId)
                            .to(EmailProviders::Table, EmailProviders::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // Create unique constraint on provider_id + domain
        manager
            .create_index(
                Index::create()
                    .name("idx_email_domains_provider_domain")
                    .table(EmailDomains::Table)
                    .col(EmailDomains::ProviderId)
                    .col(EmailDomains::Domain)
                    .unique()
                    .to_owned(),
            )
            .await?;

        // Create index on status
        manager
            .create_index(
                Index::create()
                    .name("idx_email_domains_status")
                    .table(EmailDomains::Table)
                    .col(EmailDomains::Status)
                    .to_owned(),
            )
            .await?;

        // ========================================
        // EMAILS TABLE
        // ========================================
        manager
            .create_table(
                Table::create()
                    .table(Emails::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Emails::Id)
                            .uuid()
                            .not_null()
                            .primary_key()
                            .default(Expr::cust("gen_random_uuid()")),
                    )
                    .col(ColumnDef::new(Emails::DomainId).integer().not_null())
                    .col(ColumnDef::new(Emails::ProjectId).integer().null())
                    .col(
                        ColumnDef::new(Emails::FromAddress)
                            .string_len(255)
                            .not_null(),
                    )
                    .col(ColumnDef::new(Emails::FromName).string_len(255).null())
                    .col(ColumnDef::new(Emails::ToAddresses).json_binary().not_null())
                    .col(ColumnDef::new(Emails::CcAddresses).json_binary().null())
                    .col(ColumnDef::new(Emails::BccAddresses).json_binary().null())
                    .col(ColumnDef::new(Emails::ReplyTo).string_len(255).null())
                    .col(ColumnDef::new(Emails::Subject).text().not_null())
                    .col(ColumnDef::new(Emails::HtmlBody).text().null())
                    .col(ColumnDef::new(Emails::TextBody).text().null())
                    .col(ColumnDef::new(Emails::Headers).json_binary().null())
                    .col(ColumnDef::new(Emails::Tags).json_binary().null())
                    .col(
                        ColumnDef::new(Emails::Status)
                            .string_len(50)
                            .not_null()
                            .default("queued"),
                    )
                    .col(
                        ColumnDef::new(Emails::ProviderMessageId)
                            .string_len(255)
                            .null(),
                    )
                    .col(ColumnDef::new(Emails::ErrorMessage).text().null())
                    .col(
                        ColumnDef::new(Emails::SentAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Emails::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_emails_domain")
                            .from(Emails::Table, Emails::DomainId)
                            .to(EmailDomains::Table, EmailDomains::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_emails_project")
                            .from(Emails::Table, Emails::ProjectId)
                            .to(Projects::Table, Projects::Id)
                            .on_delete(ForeignKeyAction::SetNull),
                    )
                    .to_owned(),
            )
            .await?;

        // Create indexes for emails table
        manager
            .create_index(
                Index::create()
                    .name("idx_emails_domain_id")
                    .table(Emails::Table)
                    .col(Emails::DomainId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_emails_project_id")
                    .table(Emails::Table)
                    .col(Emails::ProjectId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_emails_status")
                    .table(Emails::Table)
                    .col(Emails::Status)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_emails_created_at")
                    .table(Emails::Table)
                    .col(Emails::CreatedAt)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_emails_from_address")
                    .table(Emails::Table)
                    .col(Emails::FromAddress)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Drop indexes for emails
        manager
            .drop_index(
                Index::drop()
                    .name("idx_emails_from_address")
                    .table(Emails::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name("idx_emails_created_at")
                    .table(Emails::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name("idx_emails_status")
                    .table(Emails::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name("idx_emails_project_id")
                    .table(Emails::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name("idx_emails_domain_id")
                    .table(Emails::Table)
                    .to_owned(),
            )
            .await?;

        // Drop emails table
        manager
            .drop_table(Table::drop().table(Emails::Table).to_owned())
            .await?;

        // Drop indexes for email_domains
        manager
            .drop_index(
                Index::drop()
                    .name("idx_email_domains_status")
                    .table(EmailDomains::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name("idx_email_domains_provider_domain")
                    .table(EmailDomains::Table)
                    .to_owned(),
            )
            .await?;

        // Drop email_domains table
        manager
            .drop_table(Table::drop().table(EmailDomains::Table).to_owned())
            .await?;

        // Drop index for email_providers
        manager
            .drop_index(
                Index::drop()
                    .name("idx_email_providers_type")
                    .table(EmailProviders::Table)
                    .to_owned(),
            )
            .await?;

        // Drop email_providers table
        manager
            .drop_table(Table::drop().table(EmailProviders::Table).to_owned())
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum EmailProviders {
    Table,
    Id,
    Name,
    ProviderType,
    Region,
    Credentials,
    IsActive,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum EmailDomains {
    Table,
    Id,
    ProviderId,
    Domain,
    Status,
    SpfRecordName,
    SpfRecordValue,
    DkimSelector,
    DkimRecordName,
    DkimRecordValue,
    MxRecordName,
    MxRecordValue,
    MxRecordPriority,
    ProviderIdentityId,
    LastVerifiedAt,
    VerificationError,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum Emails {
    Table,
    Id,
    DomainId,
    ProjectId,
    FromAddress,
    FromName,
    ToAddresses,
    CcAddresses,
    BccAddresses,
    ReplyTo,
    Subject,
    HtmlBody,
    TextBody,
    Headers,
    Tags,
    Status,
    ProviderMessageId,
    ErrorMessage,
    SentAt,
    CreatedAt,
}

#[derive(DeriveIden)]
enum Projects {
    Table,
    Id,
}
