//! Database migrations for LocalTemps
//!
//! Uses SeaORM migration system to manage schema changes.

mod m20250107_000001_create_analytics_events;

use sea_orm_migration::prelude::*;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(
            m20250107_000001_create_analytics_events::Migration,
        )]
    }
}
