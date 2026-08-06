//! Database migrations for the Temps application
//!
//! This crate contains all database migration files that will be
//! moved from src/migration/

pub use sea_orm_migration::prelude::*;

// Module removed for initial build
mod migration;
// Re-export for convenience
// Re-export removed
pub use migration::m20260805_000001_index_normalized_managed_domains::Migration as NormalizedManagedDomainIndexMigration;
pub use migration::Migrator;
