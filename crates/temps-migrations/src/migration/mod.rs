pub use sea_orm_migration::prelude::*;

mod m20250101_000001_initial_schema;
mod m20250127_000001_add_unique_email_constraint;
mod m20250129_000001_add_session_id_to_proxy_logs;
mod m20250205_000001_create_ip_access_control;
mod m20250205_000002_add_attack_mode;
mod m20250205_000003_add_projects_route_trigger;
mod m20251115_000001_add_preview_environments_support;
mod m20251121_000001_create_webhooks;
mod m20251203_000001_create_email_tables;
mod m20251204_000001_create_deployment_tokens;
mod m20251205_000001_create_dns_providers;
mod m20251206_000001_make_email_domain_id_optional;
mod m20251206_000002_add_encrypted_token_to_deployment_tokens;
mod m20251206_000003_alter_visitor_custom_data_to_jsonb;
mod m20251206_000004_add_route_type_to_custom_routes;
mod m20251208_000001_create_vulnerability_scans;
mod m20251208_000002_add_deployment_id_to_scans;
mod m20251209_000001_add_environments_route_trigger;

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
            Box::new(m20251115_000001_add_preview_environments_support::Migration),
            Box::new(m20251121_000001_create_webhooks::Migration),
            Box::new(m20251203_000001_create_email_tables::Migration),
            Box::new(m20251204_000001_create_deployment_tokens::Migration),
            Box::new(m20251205_000001_create_dns_providers::Migration),
            Box::new(m20251206_000001_make_email_domain_id_optional::Migration),
            Box::new(m20251206_000002_add_encrypted_token_to_deployment_tokens::Migration),
            Box::new(m20251206_000003_alter_visitor_custom_data_to_jsonb::Migration),
            Box::new(m20251206_000004_add_route_type_to_custom_routes::Migration),
            Box::new(m20251208_000001_create_vulnerability_scans::Migration),
            Box::new(m20251208_000002_add_deployment_id_to_scans::Migration),
            Box::new(m20251209_000001_add_environments_route_trigger::Migration),
        ]
    }
}
