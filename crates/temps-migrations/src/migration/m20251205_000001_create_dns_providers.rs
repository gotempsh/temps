use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // ========================================
        // DNS_PROVIDERS TABLE
        // ========================================
        manager
            .create_table(
                Table::create()
                    .table(DnsProviders::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(DnsProviders::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(DnsProviders::Name)
                            .string_len(255)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(DnsProviders::ProviderType)
                            .string_len(50)
                            .not_null(),
                    )
                    .col(ColumnDef::new(DnsProviders::Credentials).text().not_null())
                    .col(
                        ColumnDef::new(DnsProviders::IsActive)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(ColumnDef::new(DnsProviders::Description).text().null())
                    .col(
                        ColumnDef::new(DnsProviders::LastUsedAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(ColumnDef::new(DnsProviders::LastError).text().null())
                    .col(
                        ColumnDef::new(DnsProviders::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(DnsProviders::UpdatedAt)
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
                    .name("idx_dns_providers_type")
                    .table(DnsProviders::Table)
                    .col(DnsProviders::ProviderType)
                    .to_owned(),
            )
            .await?;

        // Create index on is_active
        manager
            .create_index(
                Index::create()
                    .name("idx_dns_providers_active")
                    .table(DnsProviders::Table)
                    .col(DnsProviders::IsActive)
                    .to_owned(),
            )
            .await?;

        // ========================================
        // DNS_MANAGED_DOMAINS TABLE
        // ========================================
        manager
            .create_table(
                Table::create()
                    .table(DnsManagedDomains::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(DnsManagedDomains::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(DnsManagedDomains::ProviderId)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(DnsManagedDomains::Domain)
                            .string_len(255)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(DnsManagedDomains::ZoneId)
                            .string_len(255)
                            .null(),
                    )
                    .col(
                        ColumnDef::new(DnsManagedDomains::AutoManage)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(
                        ColumnDef::new(DnsManagedDomains::Verified)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(DnsManagedDomains::VerifiedAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(DnsManagedDomains::VerificationError)
                            .text()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(DnsManagedDomains::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(DnsManagedDomains::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_dns_managed_domains_provider")
                            .from(DnsManagedDomains::Table, DnsManagedDomains::ProviderId)
                            .to(DnsProviders::Table, DnsProviders::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // Create unique constraint on domain (each domain can only be managed by one provider)
        manager
            .create_index(
                Index::create()
                    .name("idx_dns_managed_domains_domain_unique")
                    .table(DnsManagedDomains::Table)
                    .col(DnsManagedDomains::Domain)
                    .unique()
                    .to_owned(),
            )
            .await?;

        // Create index on provider_id
        manager
            .create_index(
                Index::create()
                    .name("idx_dns_managed_domains_provider")
                    .table(DnsManagedDomains::Table)
                    .col(DnsManagedDomains::ProviderId)
                    .to_owned(),
            )
            .await?;

        // Create index on verified
        manager
            .create_index(
                Index::create()
                    .name("idx_dns_managed_domains_verified")
                    .table(DnsManagedDomains::Table)
                    .col(DnsManagedDomains::Verified)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Drop indexes for dns_managed_domains
        manager
            .drop_index(
                Index::drop()
                    .name("idx_dns_managed_domains_verified")
                    .table(DnsManagedDomains::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name("idx_dns_managed_domains_provider")
                    .table(DnsManagedDomains::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name("idx_dns_managed_domains_domain_unique")
                    .table(DnsManagedDomains::Table)
                    .to_owned(),
            )
            .await?;

        // Drop dns_managed_domains table
        manager
            .drop_table(Table::drop().table(DnsManagedDomains::Table).to_owned())
            .await?;

        // Drop indexes for dns_providers
        manager
            .drop_index(
                Index::drop()
                    .name("idx_dns_providers_active")
                    .table(DnsProviders::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name("idx_dns_providers_type")
                    .table(DnsProviders::Table)
                    .to_owned(),
            )
            .await?;

        // Drop dns_providers table
        manager
            .drop_table(Table::drop().table(DnsProviders::Table).to_owned())
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum DnsProviders {
    Table,
    Id,
    Name,
    ProviderType,
    Credentials,
    IsActive,
    Description,
    LastUsedAt,
    LastError,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum DnsManagedDomains {
    Table,
    Id,
    ProviderId,
    Domain,
    ZoneId,
    AutoManage,
    Verified,
    VerifiedAt,
    VerificationError,
    CreatedAt,
    UpdatedAt,
}
