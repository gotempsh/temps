pub use sea_orm_migration::prelude::*;

mod m20250101_000001_initial_schema;
mod m20250128_000001_add_config_to_external_services;
mod m20250128_000002_add_deployment_config_to_deployments;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20250101_000001_initial_schema::Migration),
            Box::new(m20250128_000001_add_config_to_external_services::Migration),
            Box::new(m20250128_000002_add_deployment_config_to_deployments::Migration),
        ]
    }
}
