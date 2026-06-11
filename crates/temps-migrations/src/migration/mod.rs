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
mod m20251210_000001_add_vulnerability_class_fields;
mod m20260103_000001_add_visitor_has_activity;
mod m20260103_000002_add_utm_fields_to_sessions;
mod m20260121_000001_add_remote_builds_support;
mod m20260122_000001_increase_checksum_length;
mod m20260213_000001_create_source_maps;
mod m20260214_000001_create_events_hourly_aggregate;
mod m20260214_000002_add_analytics_performance_indexes;
mod m20260217_000001_add_first_referrer_to_visitor;
mod m20260225_000001_add_proxy_logs_retention;
mod m20260225_000001_create_log_aggregator_tables;
mod m20260225_000001_create_otel_tables;
mod m20260226_000001_add_deployment_id_to_deployment_tokens;
mod m20260305_000001_create_nodes_table;
mod m20260305_000002_add_node_id_columns;
mod m20260305_000003_add_encrypted_flag_to_env_vars;
mod m20260308_000001_create_alarms_table;
mod m20260310_000001_create_ai_provider_keys;
mod m20260310_000002_create_ai_gateway_config;
mod m20260310_000003_create_ai_usage_logs;
mod m20260310_000004_add_is_byok_to_ai_usage_logs;
mod m20260310_000005_add_agent_tracking_to_ai_usage_logs;
mod m20260310_000006_add_environment_protection;
mod m20260311_000001_add_on_demand_environments;
mod m20260313_000001_add_service_members;
mod m20260313_000002_add_service_error_message;
mod m20260314_000001_update_environment_route_trigger;
mod m20260315_000001_add_last_activity_at_to_environments;
mod m20260315_000002_create_error_alert_rules;
mod m20260320_000001_add_email_tracking;
mod m20260323_000004_add_deployment_container_service_name;
mod m20260326_000001_create_asset_manifests;
mod m20260326_000002_create_static_asset_cache;
mod m20260326_000003_add_edge_public_key_to_nodes;
mod m20260327_000001_add_service_name_to_custom_domains;
mod m20260328_000001_create_email_events;
mod m20260328_000002_add_check_path_to_status_monitors;
mod m20260331_000001_create_autopilot_tables;
mod m20260401_000001_add_tracked_html_body_to_emails;
mod m20260401_000001_autopilot_to_agents;
mod m20260401_000002_add_autofixer_columns;
mod m20260401_000002_add_missing_email_events_columns;
// Squashes 34 migrations from m20260403 through m20260420 into one.
// Production (b8d6519) still has the original migrations in seaql_migrations;
// this replaces them on fresh setups. On local DBs already past b8d6519,
// insert this migration name into seaql_migrations manually to mark it done.
mod m20260421_000001_squash_apr_post_v006;
mod m20260422_000001_external_service_health;
mod m20260422_000002_add_git_connection_health;
mod m20260423_000001_create_oauth_states;
mod m20260423_000002_add_sync_progress_count;
mod m20260423_000003_fix_gitlab_nested_group_owner;
mod m20260424_000001_create_secrets;
mod m20260427_000001_add_compute_network;
mod m20260427_000002_add_dns_service_endpoints;
mod m20260427_000003_add_compute_ip_to_service_members;
mod m20260427_000004_add_provisioning_to_service_members;
mod m20260428_000001_unique_member_ordinal;
mod m20260428_000002_dns_owner_kind_deployment;
mod m20260428_000003_create_node_route_state;
mod m20260430_000001_add_deployment_container_exit_info;
mod m20260430_000002_add_deployment_container_runtime_info;
mod m20260501_000001_add_gitlab_webhook_to_projects;
mod m20260502_000001_add_observe_correlation;
mod m20260504_000001_widen_backup_size_and_heartbeat;
mod m20260505_000001_create_events_ch_outbox;
mod m20260507_000001_add_workspace_preview_password_encrypted;
mod m20260511_000001_create_cli_login_sessions;
mod m20260511_000002_add_is_secret_to_env_vars;
mod m20260514_000001_create_backup_jobs;
mod m20260515_000001_create_backup_alerts;
mod m20260515_000002_add_backup_jobs_max_runtime;
mod m20260515_000003_add_backup_schedules_max_runtime;
mod m20260516_000001_create_schedule_runs;
mod m20260517_000001_add_health_metadata_to_external_services;
mod m20260517_000002_drop_backup_jobs;
mod m20260518_000001_drop_backups_last_heartbeat_at;
mod m20260519_000001_create_backup_schedule_services;
mod m20260519_000002_add_target_all_services;
mod m20260519_000003_add_include_control_plane;
mod m20260522_000001_oidc_sso;
mod m20260522_000002_oidc_role_mappings;
mod m20260526_000001_add_preview_envs_on_demand;
mod m20260526_000002_add_trust_idp_email_to_oidc_providers;
mod m20260528_000001_add_proxy_logs_listing_indexes;
mod m20260529_000001_add_proxy_logs_filter_indexes;
mod m20260601_000001_create_service_metrics;
mod m20260601_000002_add_monitoring_settings;
mod m20260601_000003_add_monitoring_alert_rules;
mod m20260601_000004_add_monitoring_alert_rules_unique_idx;
mod m20260601_000005_add_service_id_to_api_keys;
mod m20260601_000006_update_metrics_retention_30d;
mod m20260601_000007_create_service_metrics_status;
mod m20260601_000008_alarms_nullable_env_deployment;
mod m20260601_000009_metrics_caggs_keep_labels;
mod m20260601_000010_add_service_id_to_alarms;
mod m20260603_000001_create_otel_trace_summaries;
mod m20260609_000001_create_deployment_container_logs;
mod m20260611_000001_change_log_deploy_id_to_integer;

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
            Box::new(m20251210_000001_add_vulnerability_class_fields::Migration),
            Box::new(m20260103_000001_add_visitor_has_activity::Migration),
            Box::new(m20260103_000002_add_utm_fields_to_sessions::Migration),
            Box::new(m20260121_000001_add_remote_builds_support::Migration),
            Box::new(m20260122_000001_increase_checksum_length::Migration),
            Box::new(m20260213_000001_create_source_maps::Migration),
            Box::new(m20260214_000001_create_events_hourly_aggregate::Migration),
            Box::new(m20260214_000002_add_analytics_performance_indexes::Migration),
            Box::new(m20260217_000001_add_first_referrer_to_visitor::Migration),
            Box::new(m20260225_000001_add_proxy_logs_retention::Migration),
            Box::new(m20260225_000001_create_otel_tables::Migration),
            Box::new(m20260226_000001_add_deployment_id_to_deployment_tokens::Migration),
            Box::new(m20260225_000001_create_log_aggregator_tables::Migration),
            Box::new(m20260305_000001_create_nodes_table::Migration),
            Box::new(m20260305_000002_add_node_id_columns::Migration),
            Box::new(m20260305_000003_add_encrypted_flag_to_env_vars::Migration),
            Box::new(m20260308_000001_create_alarms_table::Migration),
            Box::new(m20260310_000001_create_ai_provider_keys::Migration),
            Box::new(m20260310_000002_create_ai_gateway_config::Migration),
            Box::new(m20260310_000003_create_ai_usage_logs::Migration),
            Box::new(m20260310_000004_add_is_byok_to_ai_usage_logs::Migration),
            Box::new(m20260310_000005_add_agent_tracking_to_ai_usage_logs::Migration),
            Box::new(m20260310_000006_add_environment_protection::Migration),
            Box::new(m20260311_000001_add_on_demand_environments::Migration),
            Box::new(m20260313_000001_add_service_members::Migration),
            Box::new(m20260313_000002_add_service_error_message::Migration),
            Box::new(m20260314_000001_update_environment_route_trigger::Migration),
            Box::new(m20260315_000001_add_last_activity_at_to_environments::Migration),
            Box::new(m20260315_000002_create_error_alert_rules::Migration),
            Box::new(m20260320_000001_add_email_tracking::Migration),
            Box::new(m20260323_000004_add_deployment_container_service_name::Migration),
            Box::new(m20260326_000001_create_asset_manifests::Migration),
            Box::new(m20260326_000002_create_static_asset_cache::Migration),
            Box::new(m20260326_000003_add_edge_public_key_to_nodes::Migration),
            Box::new(m20260327_000001_add_service_name_to_custom_domains::Migration),
            Box::new(m20260328_000001_create_email_events::Migration),
            Box::new(m20260328_000002_add_check_path_to_status_monitors::Migration),
            Box::new(m20260331_000001_create_autopilot_tables::Migration),
            Box::new(m20260401_000001_add_tracked_html_body_to_emails::Migration),
            Box::new(m20260401_000001_autopilot_to_agents::Migration),
            Box::new(m20260401_000002_add_autofixer_columns::Migration),
            Box::new(m20260401_000002_add_missing_email_events_columns::Migration),
            Box::new(m20260421_000001_squash_apr_post_v006::Migration),
            Box::new(m20260422_000001_external_service_health::Migration),
            Box::new(m20260422_000002_add_git_connection_health::Migration),
            Box::new(m20260423_000001_create_oauth_states::Migration),
            Box::new(m20260423_000002_add_sync_progress_count::Migration),
            Box::new(m20260423_000003_fix_gitlab_nested_group_owner::Migration),
            Box::new(m20260424_000001_create_secrets::Migration),
            Box::new(m20260427_000001_add_compute_network::Migration),
            Box::new(m20260427_000002_add_dns_service_endpoints::Migration),
            Box::new(m20260427_000003_add_compute_ip_to_service_members::Migration),
            Box::new(m20260427_000004_add_provisioning_to_service_members::Migration),
            Box::new(m20260428_000001_unique_member_ordinal::Migration),
            Box::new(m20260428_000002_dns_owner_kind_deployment::Migration),
            Box::new(m20260428_000003_create_node_route_state::Migration),
            Box::new(m20260430_000001_add_deployment_container_exit_info::Migration),
            Box::new(m20260430_000002_add_deployment_container_runtime_info::Migration),
            Box::new(m20260501_000001_add_gitlab_webhook_to_projects::Migration),
            Box::new(m20260502_000001_add_observe_correlation::Migration),
            Box::new(m20260504_000001_widen_backup_size_and_heartbeat::Migration),
            Box::new(m20260505_000001_create_events_ch_outbox::Migration),
            Box::new(m20260507_000001_add_workspace_preview_password_encrypted::Migration),
            Box::new(m20260511_000001_create_cli_login_sessions::Migration),
            Box::new(m20260511_000002_add_is_secret_to_env_vars::Migration),
            Box::new(m20260514_000001_create_backup_jobs::Migration),
            Box::new(m20260515_000001_create_backup_alerts::Migration),
            Box::new(m20260515_000002_add_backup_jobs_max_runtime::Migration),
            Box::new(m20260515_000003_add_backup_schedules_max_runtime::Migration),
            Box::new(m20260516_000001_create_schedule_runs::Migration),
            Box::new(m20260517_000001_add_health_metadata_to_external_services::Migration),
            Box::new(m20260517_000002_drop_backup_jobs::Migration),
            Box::new(m20260518_000001_drop_backups_last_heartbeat_at::Migration),
            Box::new(m20260519_000001_create_backup_schedule_services::Migration),
            Box::new(m20260519_000002_add_target_all_services::Migration),
            Box::new(m20260519_000003_add_include_control_plane::Migration),
            Box::new(m20260522_000001_oidc_sso::Migration),
            Box::new(m20260522_000002_oidc_role_mappings::Migration),
            Box::new(m20260526_000001_add_preview_envs_on_demand::Migration),
            Box::new(m20260526_000002_add_trust_idp_email_to_oidc_providers::Migration),
            Box::new(m20260528_000001_add_proxy_logs_listing_indexes::Migration),
            Box::new(m20260529_000001_add_proxy_logs_filter_indexes::Migration),
            Box::new(m20260601_000001_create_service_metrics::Migration),
            Box::new(m20260601_000002_add_monitoring_settings::Migration),
            Box::new(m20260601_000003_add_monitoring_alert_rules::Migration),
            Box::new(m20260601_000004_add_monitoring_alert_rules_unique_idx::Migration),
            Box::new(m20260601_000005_add_service_id_to_api_keys::Migration),
            Box::new(m20260601_000006_update_metrics_retention_30d::Migration),
            Box::new(m20260601_000007_create_service_metrics_status::Migration),
            Box::new(m20260601_000008_alarms_nullable_env_deployment::Migration),
            Box::new(m20260601_000009_metrics_caggs_keep_labels::Migration),
            Box::new(m20260601_000010_add_service_id_to_alarms::Migration),
            Box::new(m20260603_000001_create_otel_trace_summaries::Migration),
            Box::new(m20260609_000001_create_deployment_container_logs::Migration),
            Box::new(m20260611_000001_change_log_deploy_id_to_integer::Migration),
        ]
    }
}
