//! Adds short-lived step-up authorization to authenticated sessions.
//!
//! The nullable timestamp is intentionally session-scoped: verifying one
//! browser must not elevate the user's other devices or API credentials.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("sessions"))
                    .add_column_if_not_exists(
                        ColumnDef::new(Alias::new("step_up_expires_at"))
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("sessions"))
                    .drop_column(Alias::new("step_up_expires_at"))
                    .to_owned(),
            )
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_name_is_stable() {
        assert_eq!(
            Migration.name(),
            "m20260803_000002_add_step_up_expires_at_to_sessions"
        );
    }
}
