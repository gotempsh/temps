pub use sea_orm_migration::prelude::*;

mod m20250101_000001_initial_schema;
mod m20250127_000001_add_unique_email_constraint;
mod m20250129_000001_add_session_id_to_proxy_logs;
mod m20250205_000001_create_ip_access_control;
mod m20250205_000002_add_attack_mode;
mod m20250205_000003_add_projects_route_trigger;
mod m20251110_000001_add_preview_environment_id;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20250101_000001_initial_schema::Migration),
            Box::new(m20250127_000001_add_unique_email_constraint::Migration),
            Box::new(m20250129_000001_add_session_id_to_proxy_logs::Migration),
            Box::new(m20250205_000001_create_ip_access_control::Migration),
            Box::new(m20250205_000002_add_attack_mode::Migration),
            Box::new(m20250205_000003_add_projects_route_trigger::Migration),
            Box::new(m20251110_000001_add_preview_environment_id::Migration),
        ]
    }
}
