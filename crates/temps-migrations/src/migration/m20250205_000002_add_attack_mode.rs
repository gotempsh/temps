use sea_orm_migration::{prelude::*, schema::*};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Add attack_mode column to projects table (project-level setting only)
        manager
            .alter_table(
                Table::alter()
                    .table(Projects::Table)
                    .add_column(
                        ColumnDef::new(Projects::AttackMode)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .to_owned(),
            )
            .await?;

        // Create challenge_sessions table to track CAPTCHA completions
        manager
            .create_table(
                Table::create()
                    .table(ChallengeSessions::Table)
                    .if_not_exists()
                    .col(pk_auto(ChallengeSessions::Id))
                    .col(integer(ChallengeSessions::EnvironmentId))
                    .col(string(ChallengeSessions::Identifier))
                    .col(string(ChallengeSessions::IdentifierType))
                    .col(string_null(ChallengeSessions::UserAgent))
                    .col(timestamp_with_time_zone(ChallengeSessions::CompletedAt))
                    .col(timestamp_with_time_zone(ChallengeSessions::ExpiresAt))
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_challenge_sessions_environment")
                            .from(ChallengeSessions::Table, ChallengeSessions::EnvironmentId)
                            .to(Environments::Table, Environments::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // Create indexes separately
        manager
            .create_index(
                Index::create()
                    .name("idx_challenge_sessions_identifier")
                    .table(ChallengeSessions::Table)
                    .col(ChallengeSessions::EnvironmentId)
                    .col(ChallengeSessions::Identifier)
                    .col(ChallengeSessions::IdentifierType)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_challenge_sessions_expires")
                    .table(ChallengeSessions::Table)
                    .col(ChallengeSessions::ExpiresAt)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Drop challenge_sessions table
        manager
            .drop_table(Table::drop().table(ChallengeSessions::Table).to_owned())
            .await?;

        // Remove attack_mode column from projects table
        manager
            .alter_table(
                Table::alter()
                    .table(Projects::Table)
                    .drop_column(Projects::AttackMode)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum Projects {
    Table,
    AttackMode,
}

#[derive(DeriveIden)]
enum Environments {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum ChallengeSessions {
    Table,
    Id,
    EnvironmentId,
    Identifier,     // IP address or JA4 fingerprint
    IdentifierType, // "ip" or "ja4"
    UserAgent,
    CompletedAt,
    ExpiresAt,
}
