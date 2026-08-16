//! Compatibility identity for early builds of PR #413.
//!
//! The index itself is built outside the migration transaction with
//! `CREATE INDEX CONCURRENTLY`; retaining this no-op migration lets databases
//! that briefly applied the original transactional migration continue to boot.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}
