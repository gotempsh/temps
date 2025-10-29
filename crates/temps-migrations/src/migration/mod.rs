pub use sea_orm_migration::prelude::*;

mod m20250101_000001_initial_schema;
mod m20250127_000001_add_unique_email_constraint;
mod m20250129_000001_add_session_id_to_proxy_logs;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20250101_000001_initial_schema::Migration),
            Box::new(m20250127_000001_add_unique_email_constraint::Migration),
            Box::new(m20250129_000001_add_session_id_to_proxy_logs::Migration),
        ]
    }
}
