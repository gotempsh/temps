pub mod acme_accounts;
pub mod acme_orders;
pub mod ai_conversations;
pub mod ai_gateway_config;
pub mod ai_messages;
pub mod ai_pending_actions;
pub mod ai_provider_keys;
pub mod ai_usage_logs;
pub mod alarms;
pub mod cross_project_trace_refs;
// Agent entities (renamed from autopilot)
pub mod agent_run_logs;
pub mod agent_runs;
pub mod agent_secrets;
pub mod project_agents;
pub mod project_mcp_definitions;
pub mod project_secrets;
pub mod project_skill_definitions;
// Legacy autopilot entities (kept for migration compatibility)
pub mod api_keys;
pub mod audit_logs;
pub mod autopilot_configs;
pub mod autopilot_run_logs;
pub mod autopilot_runs;
pub mod backup_alerts;
pub mod backup_schedule_services;
pub mod backup_schedules;
pub mod backups;
pub mod challenge_sessions;
pub mod cli_login_sessions;
pub mod cron_executions;
pub mod crons;
pub mod custom_routes;
pub mod deployment_config;
pub mod deployment_container_logs;
pub mod deployment_containers;
pub mod deployment_domains;
pub mod deployment_jobs;
pub mod deployment_tokens;
pub mod deployments;
pub mod dns_managed_domains;
pub mod dns_providers;
pub mod domains;
pub mod email_domains;
pub mod email_events;
pub mod email_links;
pub mod email_providers;
pub mod emails;
pub mod env_var_environments;
pub mod env_vars;
pub mod environment_domains;
pub mod environments;
pub mod external_images;
pub mod external_service_backups;
pub mod external_service_health_checks;
pub mod external_services;
pub mod funnel_steps;
pub mod funnels;
pub mod git_provider_connections;
pub mod git_providers;
pub mod ip_access_control;
pub mod ip_geolocations;
pub mod network_config;
pub mod node_dns_state;
pub mod node_enrollment_tokens;
pub mod node_route_state;
pub mod nodes;
pub mod notification_preferences;
pub mod notification_providers;
pub mod notifications;
pub mod oauth_states;
pub mod oidc_login_states;
pub mod oidc_providers;
pub mod oidc_role_mappings;
pub mod on_demand_cert_attempts;
pub mod performance_metrics;
pub mod postgres_major_upgrades;
pub mod preset;
pub mod project_custom_domains;
pub mod project_services;
pub mod projects;
pub mod proxy_logs;
pub mod repositories;
pub mod request_sessions;
pub mod restore_runs;
pub mod roles;
pub mod s3_sources;
pub mod schedule_runs;
pub mod secret_environments;
pub mod secrets;
pub mod service_endpoints;
pub mod service_members;
pub mod sessions;
pub mod source_type;
pub mod static_asset_cache;
pub mod static_bundles;
pub mod suppressed_recipients;
pub mod tls_acme_certificates;
pub mod types;
pub mod upstream_config;
pub mod user_roles;
pub mod users;

// OpenTelemetry entities

pub mod events;
pub mod session_replay_events;
pub mod session_replay_sessions;
pub mod settings;
pub mod visitor;

// Error tracking entities
pub mod error_alert_fires;
pub mod error_alert_rules;
pub mod error_events;
pub mod error_groups;
pub mod project_dsns;
pub mod source_maps;
pub mod tokenizer;

// Status page entities
pub mod status_checks;
pub mod status_incident_updates;
pub mod status_incidents;
pub mod status_monitors;

// Metrics alert rules
pub mod monitoring_alert_rules;

// Metric dashboards (saved per-project dashboard layouts)
pub mod metric_dashboards;

// Metric alert rules (first-class metric-centric alerting)
pub mod metric_alert_rules;

// Webhook entities
pub mod webhook_deliveries;
pub mod webhooks;

// Revenue tracking entities
pub mod revenue_customers_state;
pub mod revenue_events;
pub mod revenue_integrations;
pub mod revenue_subscriptions_state;

// Vulnerability scanner entities
pub mod vulnerabilities;
pub mod vulnerability_scans;

// Log aggregator entities
pub mod log_chunks;
pub mod log_events;

// Standalone sandbox API (Vercel-compatible)
pub mod sandboxes;

// Workflow memory
pub mod workflow_memory;

pub mod prelude;
