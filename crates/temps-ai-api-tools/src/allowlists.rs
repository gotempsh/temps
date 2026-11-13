// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The operations the assistant may reach through the API tools.
//!
//! Three curated lists: GET operations the read tool may call, the narrow set
//! of side-effect-free `POST`s it may also call, and the mutating operations
//! the write tool may propose. The console hands them to
//! [`crate::InternalApiCaller`] once the router is assembled. Adding an entry
//! is a security decision; each list's doc comment says what to check.

/// ALLOWLIST for the AI `call_api` read tool — opt-in / secure by default.
/// The model may call ONLY these read-only GET operations; every other
/// endpoint (including any newly added one) is invisible to it. This is
/// deliberately an allowlist, not a denylist: GET endpoints across the
/// platform return decrypted secrets (service params → DB passwords,
/// env-var reveals, notification configs → Slack/SMTP/Cloudflare creds, S3
/// credentials, …), and a hand-maintained denylist can't be kept exhaustive.
/// Curated for the SRE/debugging use case: observability, runtime/deploy
/// status, errors, and MASKED metadata. Adding an entry is a security
/// decision — verify the operation's response contains NO decrypted
/// secret/token/key.
///
/// Analytics `custom_data`/`event_data` (freeform JSON on visitors/events)
/// is an ACCEPTED risk, not an oversight: it's developer-controlled data the
/// project operator already sends themselves, gated by the same
/// AnalyticsRead + project scope as every other analytics tool below — not
/// a platform secret. Raw HTTP request/response headers (session logs) and
/// session-replay DOM/keystroke capture are excluded on purpose; those can
/// carry Authorization/Cookie values or typed passwords and are a different
/// risk class entirely.
///
/// Extracted into its own function (rather than an inline literal at the
/// call site) so tests can assert every entry resolves to a real OpenAPI
/// operation_id without duplicating the list — see `ai_tool_allowlist_tests`
/// in the console (`temps-cli`), which checks it against the real API.
pub const AI_READ_ALLOWLIST: &[&str] = &[
    // Vault metadata is masked; grants never return upstream credentials.
    "list_access_services",
    "get_access_service",
    "list_sandbox_services",
    // ── OpenTelemetry: metrics / traces / logs / health ──
    "query_metrics",
    "list_metric_names",
    "list_metric_label_keys",
    "list_metric_label_values",
    "query_traces",
    "query_trace_summaries",
    "get_trace",
    "query_genai_traces",
    "get_genai_trace",
    "query_logs",
    "list_insights",
    "get_health",
    "get_quota",
    "get_pipeline_stats",
    // ── Container runtime: logs + metrics (no secrets) ──
    "get_container_metrics",
    "get_container_logs",
    "get_container_logs_by_id",
    "get_container_detail",
    "list_containers",
    // ── Projects: current-user-filtered metadata ──
    // The handler enforces ProjectsRead and derives hidden project ids
    // from the AuthContext forwarded by the current private chat turn.
    "get_projects",
    // ── Deployments: status / jobs / history ──
    "get_deployment",
    "get_last_deployment",
    "get_project_deployments",
    // Manual-deploy discovery: registered external images the
    // AI can deploy by id/ref (metadata only — no registry
    // credentials). Static bundles are frontend-only, so their
    // read ops are intentionally excluded here.
    "list_external_images",
    "get_external_image",
    "get_deployment_jobs",
    "get_deployment_operations",
    "get_deployment_operation_status",
    "get_activity_graph",
    "get_deployment_job_logs",
    "list_deployment_container_logs",
    "get_deployment_container_log_content",
    // ── Environments: metadata + MASKED env-var lists only ──
    // (the *_value reveal endpoints are intentionally excluded)
    "get_environments",
    "get_environment",
    "get_environment_domains",
    "get_environment_crons",
    // Where a Compose stack lives, drift and a pending move: node ids
    // and names only, project-scoped like the environment reads.
    "get_compose_placement",
    "get_cron_by_id",
    "get_cron_executions",
    "get_environment_variables",
    "get_resolved_environment_variables",
    // ── Error tracking ──
    "get_error_dashboard_stats",
    "get_error_event",
    "get_error_group",
    "get_error_stats",
    "get_error_time_series",
    "has_error_groups",
    "list_error_events",
    "list_error_groups",
    "list_alert_rules",
    "get_alert_rule",
    // ── Metric alert rules (OTel) — the rules themselves, so the AI can
    //    see what is already alerted on before proposing anything new.
    //    Without these it proposes duplicates of rules that exist.
    "list_alerts",
    "get_alert",
    // Notification provider responses use decrypt_provider_config(),
    // which masks secret fields before serialization. That lets the AI
    // discover and manage providers without receiving reusable secrets.
    "list_notification_providers",
    "get_notification_provider",
    "list_notification_routes",
    "get_notification_route",
    // ── Service inventory / status / health / types (NOT params/env) ──
    // `list_services` is filtered by the current user's
    // ExternalServicesRead permission, and deployment tokens are rejected
    // by the handler. Without it, the AI can see only services already
    // linked to the current project and cannot discover a newly created
    // database in order to link or inspect it.
    "list_services",
    "get_service_health_status",
    "list_service_health_statuses",
    "get_service_stats",
    "get_service_runtime",
    "list_project_services",
    "list_service_projects",
    "get_service_types",
    "get_service_type_parameters",
    "get_cluster_health",
    "get_cluster_member",
    // Service detail with every sensitive parameter masked
    // (`mask_sensitive_parameter_values`); `connection_info` is never
    // populated. Without it the assistant could not see why a service
    // failed (`error_message`) or how its members were laid out.
    // `reveal_service_parameter` / `reveal_service_environment_variables`
    // stay out: those return plaintext.
    "get_service",
    "get_service_by_slug",
    // Server TLS of a managed database: mode, whether each container or
    // data node serves it, and the certificate's public details. The CA
    // download is the public certificate clients verify against; no
    // private key is ever returned.
    "get_service_tls",
    "get_managed_ca_certificate",
    // ── Agentic follow-up reads: what a change did, and what to do next ──
    "list_container_history",
    "tail_deployment_job_logs",
    "get_restore_run",
    "get_external_service_backup_capability",
    "list_renewal_attempts",
    "get_compose_security",
    "get_project_service_template",
    // The public IPs and whether domains can be created: what a user needs
    // to point a DNS record at this platform.
    "get_access_info",
    // Repository files fetched through the stored Git connection, so a
    // private repo's `.env.example` / compose file can be read when
    // planning a deployment.
    "get_repository_env_example_live",
    "get_repository_compose_services_live",
    "get_public_compose_services",
    // ── Monitoring the user already configured ──
    "listHttpChecks",
    "getHttpChecksCapabilities",
    "listHttpCheckPresets",
    "listSystemAlarms",
    "getSystemAlarmsSummary",
    "list_global_error_groups",
    // ── Cross-project analytics and traces (same scope rules as the
    //    per-project reads above). API traffic summaries, routes and
    //    callers are deliberately absent: a test pins them out. ──
    "get_global_analytics",
    "get_analytics_projects",
    "query_global_traces",
    "query_global_trace_summaries",
    "getUnifiedTrace",
    "getPostgresWalHealth",
    "ExternalServiceMetricsGetLatest",
    "ExternalServiceMetricsGetRange",
    "ExternalServiceMetricsStatus",
    "ExternalServiceMetricsByDatabase",
    "ExternalServiceMetricsGetAlertRules",
    "DeploymentMetricsGetRange",
    "DeploymentMetricsGetLatest",
    "NodeMetricsGetRange",
    "NodeMetricsGetLatest",
    "NodeMetricsGetClusterRange",
    "NodeMetricsGetClusterLatest",
    "NodeMetricsGetReporting",
    "NodeMetricsGetDiskHealth",
    // A managed service's own log ingestion budget: two numbers.
    "get_service_log_ingestion",
    // ── Reachability: plans, runs and their journals (no tokens; the
    // journal `key` names a step, it is not a credential) ──
    "get_certificate_methods",
    "get_environment_reachability",
    "get_reachability_choices",
    "list_reachability_runs",
    "get_reachability_run",
    // ── Domains: metadata (no challenge tokens) ──
    "list_domains",
    "get_domain",
    "get_domain_by_name",
    "get_domain_dns_records",
    "list_custom_domains_for_project",
    "get_custom_domain",
    "check_domain_status",
    "list_managed_domains",
    "get_on_demand_cert_status",
    // ── Platform / monitor status ──
    "get_status_overview",
    "get_disk_status",
    "get_current_monitor_status",
    "get_projects_health",
    "get_projects_monitor_health",
    "get_project_statistics",
    // ── Backups: metadata only (NOT s3 credentials/source) ──
    "get_backup",
    "get_backup_schedule",
    "list_backup_schedules",
    "list_backups_for_schedule",
    "list_external_service_backups",
    "list_source_backups",
    "list_schedule_runs",
    "list_restore_runs_for_service",
    "get_restore_capabilities",
    // ── Audit trail ──
    "list_audit_logs",
    "get_audit_log",
    // ── Analytics (the user's own traffic data) ──
    "get_general_stats",
    "get_visitor_stats",
    "get_visitors",
    "get_today_stats",
    "get_recent_activity",
    "get_events_timeline",
    "get_events_count",
    "get_page_paths",
    "get_performance_metrics",
    // Aggregates / counts / booleans — no PII, no secrets
    "get_analytics_events_count",
    "get_event_detail",
    "get_visitor_facets",
    "check_analytics_has_events",
    "get_page_path_detail",
    "get_page_hourly_sessions",
    "get_page_paths_sparklines",
    "get_page_flow",
    "has_analytics_events",
    "get_event_type_breakdown",
    "get_active_visitors",
    "get_hourly_visits",
    "get_unique_counts",
    "get_aggregated_buckets",
    "get_dashboard_projects_analytics",
    "get_metrics_over_time",
    "get_grouped_page_metrics",
    "has_performance_metrics",
    "get_funnel_metrics",
    "list_funnels",
    "get_unique_events",
    // Traffic exclusion settings, the project's own rules (referrer
    // domains, IPs/CIDR ranges, ASNs, countries the user chose to
    // exclude) and per-reason counts. No secrets; IPs here are ones the
    // user entered, a lower risk class than get_visitor_details below.
    "get_project_analytics_settings",
    "list_traffic_filters",
    "get_filtered_traffic",
    // ── API traffic: privacy-safe investigation ──
    // The time series contains only bounded counts and latency/error
    // aggregates. Keep get_api_summary out because refresh=true can incur
    // paid provider work. Keep get_api_routes and get_api_callers out of
    // the general tool loop: paths are attacker-controlled and caller
    // addresses are personal data.
    "get_api_timeseries",
    // Per-visitor/session metadata — same risk class as
    // get_visitors/get_visitor_stats above (IP, geolocation,
    // user_agent, is_crawler, custom_data/event_data)
    "get_event_visitors",
    "get_visitor_details",
    "get_visitor_info",
    "get_analytics_visitor_sessions",
    "get_visitor_journey",
    "get_session_details",
    "get_session_events",
    "get_page_path_visitors",
    "get_analytics_active_visitors",
    "get_live_visitors_list",
    "get_visitor_by_id",
    "get_visitor_by_guid",
    // Excluded on purpose: get_property_breakdown /
    // get_property_timeline (developer custom-property VALUES
    // by name — held pending explicit review), get_session_logs
    // (raw request/response headers), and all
    // temps-analytics-session-replay reads (raw DOM/keystroke
    // capture) — see allowlist comment above.
    //
    // ── Auth / API keys / OIDC (masked keys/secrets only) ──
    "list_api_keys",
    "get_api_key",
    "list_public_providers",
    "list_oidc_providers",
    "list_oidc_provider_users",
    "list_oidc_role_mappings",
    "get_current_user",
    // ── Webhooks (no signing secrets — those are excluded) ──
    "list_webhooks",
    "get_webhook",
    "list_deliveries",
    "get_delivery",
    "list_event_types",
    // ── Data browser: schema navigation, plus opt-in row reads ──
    // Schema shape only (database/schema/bucket names, table and column
    // names, row counts, sizes). These carry no stored values, so they
    // are safe on the same footing as the rest of this list — and they
    // are what lets the agent resolve a question like "the users in the
    // landing production database" to a concrete container path.
    "check_explorer_support",
    "list_root_containers",
    "list_containers_at_path",
    // The data browser's container-info endpoint is now published as
    // `get_query_container_info` (see its `operation_id` in
    // temps-providers). It used to share `get_container_info` with the
    // Docker container endpoint above, and since utoipa keys the document
    // by operation_id one silently overwrote the other — the data browser
    // won, so this allowlist granted the agent that endpoint while the
    // comment here claimed the Docker one. Not listed: navigation is
    // already covered by `list_containers_at_path`, and it is not needed.
    // Entities and their rows. The assistant reads data with the user's
    // own permissions, exactly as the data browser does — the handlers
    // check `ExternalServicesRead` and the caller's project scope — and
    // every row read it makes is audited (`AiRowsReadAudit` in
    // temps-providers). Row contents reach the configured AI provider.
    "list_entities",
    "get_entity_info",
    "read_entity_rows",
    // ── KV / Blob: status only, no connection strings ──
    "kv_status",
    "blob_status",
    "blob_list",
    // ── Email: stats/tracking/DNS, no provider credentials ──
    "list_emails",
    "get_email",
    "get_email_stats",
    "list_email_domains",
    "get_global_event_stats",
    "get_global_events",
    "get_email_tracking",
    "get_email_events",
    "get_email_links",
    // ── Git: provider/connection/repo metadata, no OAuth tokens ──
    "list_git_providers",
    "get_git_provider",
    "list_connections",
    "list_repositories_by_connection",
    "list_repositories_by_provider",
    "list_synced_repositories",
    "get_repository_preset_by_name",
    "get_repository_by_name",
    "get_public_branches",
    "get_public_repository",
    // ── Agents / skills / MCP / sandbox — config + run status ──
    "list_agents",
    "get_agent",
    "list_skills",
    "get_skill",
    "list_mcps",
    "get_mcp",
    "list_global_skills",
    "get_global_skill",
    "list_global_mcps",
    "get_global_mcp",
    "get_run",
    "get_cli_status",
    "get_sandbox_status",
    "get_global_sandbox_status",
    // ── AI Gateway: masked keys, usage/cost stats, conversations ──
    "list_models",
    "get_usage_summary",
    "get_usage_by_provider",
    "get_usage_timeseries",
    "get_usage_top_models",
    "get_usage_recent",
    "get_conversations",
    "get_conversation_detail",
    "get_pricing",
    "list_provider_keys",
    // ── Status page: monitors/incidents/uptime ──
    "list_monitors",
    "get_monitor",
    "get_uptime_history",
    "get_bucketed_status",
    "list_incidents",
    "get_incident",
    "get_incident_updates",
    "get_bucketed_incidents",
    // ── Vulnerability scanning: results, no credentials ──
    "list_project_scans",
    "get_scan",
    "get_scan_vulnerabilities",
    "get_latest_scan",
    "get_latest_scans_per_environment",
    "get_scan_by_deployment",
    // ── Import ──
    "list_sources",
    "get_import_status",
    // ── Container metrics history ──
    // Same class as `get_container_metrics` (already allowlisted); returns
    // time-series data points with no secrets.
    "ContainerMetricsGetHistory",
    // ── Projects: single-project read (metadata, no secrets) ──
    // `get_project` / `get_project_by_slug` mirror `get_projects` but for a
    // specific ID/slug. Both enforce permission_guard + project_scope_guard +
    // project_access_guard — same three-layer check the list endpoint uses.
    "get_project",
    "get_project_by_slug",
    // ── External services: masked env vars only ──
    // `get_service` / `get_service_by_slug` are intentionally NOT included.
    // `get_service` calls `get_service_details`, which runs
    // `mask_sensitive_parameter_values` — a name-heuristic (suffixes like
    // `_key`/`_password`/`_token`/`_secret`, prefixes like `private_`), not
    // an unconditional guarantee. A parameter stored under a non-standard
    // name (e.g. a custom service plugin's `psk`, or a PEM blob under
    // `cert`) passes through unmasked, and the schema's per-parameter
    // `encrypted` flag (externalsvc/mod.rs's `ServiceParameter`) isn't
    // wired into that masking function to catch what the heuristic misses
    // — flagged in security review on PR #732 (Greptile + internal audit).
    // `get_service_by_slug` additionally has no `assert_service_owned_by_caller`
    // check (handlers.rs:2343), so it would let the AI resolve any service
    // on the instance by slug, not just ones linked to the caller's project.
    // The `get_service_environment_variables` bulk endpoint explicitly sets
    // `mask_sensitive: true`; `get_project_service_environment_variables` calls
    // `mask_environment_variable_values` before responding — both unconditional
    // (every value becomes "***", not name-heuristic). Preview env var
    // endpoints return names only or masked values by design.
    "get_service_environment_variables",
    "get_project_service_environment_variables",
    "get_service_preview_environment_variable_names",
    "get_service_preview_environment_variables_masked",
    // Provider type catalog and schema metadata — no stored values.
    "get_provider_metadata",
    "get_providers_metadata",
    // Available Docker images for external services (name, version tags).
    "list_available_containers",
    // Slow-query statistics from pg_stat_statements — parameterised query
    // text ($1/$2 placeholders), execution counts, timing. No data values.
    "get_slow_queries",
    // ── AI traffic analytics (proxy-log aggregates, no raw rows) ──
    // These endpoints return aggregated breakdowns and time-series counts
    // derived from proxy logs — agent names, page paths, HTTP status buckets,
    // request counts, latencies. No full request/response bodies are included.
    "get_ai_agent_breakdown",
    "get_ai_agent_pages",
    "get_ai_agent_timeline",
    "get_ai_page_breakdown",
    "get_ai_status_breakdown",
    // Time-bucketed request aggregates (counts, error rates, p50/p95 latency).
    // Same privacy class as `get_api_timeseries` (already allowlisted).
    "get_time_bucket_stats",
    // Proxy route table — hostname→environment mapping, no credentials.
    "list_routes",
    "get_route",
    // ── OpenTelemetry: cross-project traces, span stats ──
    // `getCrossProjectTraceSiblings` requires OtelRead + denies deployment
    // tokens; its response (`CrossProjectTraceResponse`) is genuinely
    // metadata-only — project id/name/slug/first_seen timestamp, no span
    // content. `getUnifiedTrace` is intentionally NOT included: it embeds
    // the full `SpanRecord` per span, whose `attributes` field is
    // documented as "raw key/value pairs exactly as reported by the
    // instrumenting library" plus raw `events` — same risk class as the
    // already-excluded observability span attributes, but fanned out
    // across up to 20 projects instead of one. Flagged in security review
    // on PR #732 (Greptile, 5th pass).
    "getCrossProjectTraceSiblings",
    // Span statistics ranked by latency/volume (aggregates, no span payloads).
    "query_span_stats",
    // ── Alarms ──
    // Project-scoped alarm list and summary counts; permission_guard DeploymentsRead.
    "listProjectAlarms",
    "getProjectAlarmsSummary",
    // ── Observability event store: excluded entirely ──
    // Neither `observability_list_events` nor `observability_full_event`
    // is included. `_full_event` returns the un-truncated row outright
    // (for errors, `FullError.data` — "the full JSONB blob... stack
    // trace, breadcrumbs, request context, everything", service.rs).
    // `_list_events`'s "truncated" preview rows are NOT safe either:
    // `truncate_stacktrace`/the attributes preview (types.rs) only cap
    // *count* (first 5 stack frames, first 20 span attribute keys) —
    // they don't redact *content*. So the list endpoint still returns,
    // verbatim: `RequestRow.query_string` (untruncated — can carry
    // `?token=`/`?api_key=`/PII), `SpanRow.attributes` (raw
    // developer-set tags — same risk class as the already-excluded
    // `get_property_breakdown`), `ErrorRow.stacktrace_preview` (raw
    // frames), and `ErrorRow.message`. Only `request_headers`/
    // `response_headers` are genuinely safe (allowlist-filtered by
    // `HEADER_WHITELIST`). Flagged in security review on PR #732
    // (Greptile, 3rd pass).
    // ── IP access control ──
    // Access-rule list and single-IP block check; no secrets.
    "check_ip_blocked",
    "get_ip_access_control",
    "list_ip_access_control",
    // ── Shield (ADR 066) ──
    // Verdict counts, lookups and operator entries; token rows carry
    // id, scope, environment_id, created_by, name, and timestamps,
    // never the token or its hash. The event list and learning's
    // proposals stay out: they carry visitors' addresses, paths and user
    // agents.
    "get_shield_summary",
    "lookup_shield_client",
    "list_shield_tokens",
    "list_project_shield_entries",
    "list_platform_shield_entries",
    "get_shield_settings",
    // Geolocation lookup for an IP — country/city, no PII beyond what the
    // caller already knows (the IP itself). Gated by AnalyticsRead.
    "get_ip_geolocation",
    // ── AI chat: conversations + pending actions ──
    // Conversations are the operator's own chat history. Pending actions are
    // proposed (not yet executed) write operations waiting for confirmation.
    // Both are scoped to the current user's projects via ProjectsRead.
    "get_conversation",
    "find_conversation",
    "list_conversations",
    "list_all_conversations",
    "get_pending_action",
    "list_pending_actions",
    // The deployments, backups and restores a chat started, filtered to the
    // ones the caller may read. Statuses and IDs only; no secrets.
    "list_chat_operations",
    // Instance AI-provider readiness; project access is enforced separately.
    "get_chat_readiness",
    // ── OTel dashboards ──
    // Dashboard config (queries, panel layout) — no secrets. Gated OtelRead.
    "get_dashboard",
    "list_dashboards",
    // ── Platform info + update status ──
    // OS type, architecture, and supported platform strings — no secrets.
    "get_platform_info",
    // Which subsystems this process actually provides (serve profile plus a
    // boolean per capability). No secrets, and it is what lets the assistant
    // answer "why can't I create a database here?" with the real reason
    // instead of guessing from a failed call.
    "get_platform_features",
    // Server's externally-reachable public IP as detected at startup.
    "get_public_ip",
    // Whether an in-place binary update is possible and what version is available.
    "get_update_capability",
    "get_update_status",
    // ── Email provider (masked credentials) + tracking ──
    // `get_email_provider` / `list_email_providers` call `get_masked_credentials`
    // before responding — the actual SMTP/SES credentials are replaced with "***".
    "get_email_provider",
    "list_email_providers",
    // Whether email sending is configured (boolean). Unauthenticated endpoint.
    "email_status",
    // Tracking pixel / click-through status booleans — not the raw events.
    "get_email_tracking_status",
    // ── DNS providers (masked credentials) + domain orders ──
    // `get_dns_provider` / `list_dns_providers` call `get_masked_credentials`
    // before responding — the actual provider API key is replaced with "***".
    "get_dns_provider",
    "list_dns_providers",
    // Cloudflare Access tokens (ADR 065 D9): the token is never returned,
    // `api_token` is always "***".
    "list_access_providers",
    // Domain lookups — metadata only, no signing secrets.
    "get_domain_by_host",
    "get_domain_by_id",
    "get_domain_order",
    "get_certificate_settings",
    "list_orders",
    "list_on_demand_certs",
    // DNS provider zone list (zone IDs and names for domain selection).
    "list_provider_zones",
    // Live DNS A-record lookup for a hostname — diagnostic read.
    "lookup_dns_a_records",
    // Flat vs. wildcard subdomain preview mode for DNS providers.
    "preview_hostname_mode",
    // ── Ingress tunnels (ADR 058) ──
    // Tunnels, where they run, which hostnames each publishes and why
    // one is not published. Connector secrets never leave the service;
    // the views carry ids, names, states and provider error text only.
    "list_ingress_providers",
    "list_ingress_connectors",
    "get_ingress_connector",
    "get_ingress_connector_links",
    "list_ingress_hostnames",
    "get_ingress_hostname",
    // ADR 068: whether the preview domain is reachable and why, and the
    // hosts a tunnel publishes for environments (names and states only).
    "get_preview_domain_status",
    "get_connector_environment_hosts",
    // ── User preferences + cluster metadata ──
    // Notification preferences for the current user — no credentials.
    "get_preferences",
    // Boolean: whether a cluster join token is set (not the token itself).
    "get_join_token_status",
    // Enrollment token metadata (expiry, use count, bound node). Token values
    // are never returned; the response only contains `EnrollmentTokenInfo`.
    "list_enrollment_tokens",
    // Static list of all available permission names — not sensitive.
    "get_api_key_permissions",
    // ── Teams + project access ──
    // Team and membership metadata scoped by UsersRead permission.
    "list_teams",
    "get_team",
    "list_team_members",
    "list_team_projects",
    "list_project_access",
    // ── Agent secrets + AI provider status ──
    // Agent secrets always return value masked as "***" in responses.
    "list_secrets",
    // Project secrets — metadata only (key names + environment scoping).
    // Values are never returned by this endpoint; see its doc comment.
    "listProjectSecrets",
    // AI provider list — reports installed/authenticated booleans and
    // available models; actual credentials are never included.
    "list_ai_providers",
    // Known AI agent identifiers (name catalogue, no credentials).
    "list_known_ai_agents",
    // ── Feature flags ──
    "get_flag",
    "get_flag_snapshot",
    "list_flags",
    // ── Project presets + templates ──
    // Built-in and community preset catalogue — no secrets.
    "list_presets",
    "get_project_template",
    "list_project_templates",
    "list_project_template_tags",
    // Detect applicable presets from a public repository URL.
    // Makes read-only calls to public git provider APIs (same as
    // `get_public_branches` / `get_public_repository` already allowlisted).
    "detect_public_presets",
    // ── Static files + source maps + releases (error tracking) ──
    // Error-tracking release metadata (version strings, file lists).
    // No stored values; same permission class as list_error_groups.
    "list_releases",
    "list_release_files",
    "list_static_bundles",
    "get_static_bundle",
    "list_source_maps",
    "list_source_files",
    // Remote external image metadata (registry ref + description). No credentials.
    "get_remote_external_image",
    "list_remote_external_images",
    // ── Deployment tokens (masked prefix only) ──
    // `DeploymentTokenResponse` exposes only `token_prefix` (first few chars),
    // not the full token — same masking class as `get_api_key` (already listed).
    "get_deployment_token",
    "list_deployment_tokens",
    // ── Backup metadata ──
    // Alert thresholds and child backup records — no S3 credentials (those
    // are in `get_s3_source` / `list_s3_sources`, which are excluded).
    "list_backup_alerts",
    "list_backup_children",
    "list_schedule_run_jobs",
    "list_service_schedules",
    "list_schedule_services",
    // ── PostgreSQL major-version upgrade status ──
    "get_pg_upgrade",
    "get_pg_upgrade_logs",
    "list_pg_upgrades",
    // ── Git: repository / branch / tag reads ──
    // Boolean safety check (can_delete + projects_in_use list).
    "check_provider_deletion_safety",
    // Boolean: whether a specific commit SHA exists in a repository.
    "check_commit_exists",
    // Repository and ref metadata; no OAuth tokens. Same class as
    // `list_repositories_by_connection` already allowlisted.
    "get_all_repositories_by_name",
    "get_repository_by_id",
    "get_branches_by_repository_id",
    "get_tags_by_repository_id",
    "list_commits_by_repository_id",
    "get_repository_branches",
    "get_repository_tags",
    "get_repository_preset_live",
    // Git connection metadata (connection IDs, names, provider type).
    "get_provider_connections",
    // ── Getting started: what a repository / template needs ──
    // Preset detection and `.env.example` discovery for a public
    // repository, and the project template catalogue. Read-only metadata.
    "detect_public_env_example",
    // ── Log aggregator: context window — excluded ──
    // `get_log_context` is intentionally NOT included. It returns raw
    // `message`/`fields` from application stdout/stderr (arbitrary
    // developer-controlled content), and its resolver
    // (temps-log-aggregator/src/services/search.rs `get_context`) has NO
    // project-ownership check on `chunk_id` at all — only a global
    // `LogsRead` permission gate, unlike `get_container_logs`'s
    // container-scoped equivalent. Flagged in security review on PR #732
    // (Greptile, 4th pass).
    // ── Preview gateway: settings + status (NOT logs) ──
    // Settings expose image tag, host port, auto-upgrade flag — no shared_secret
    // (that is masked as a boolean in the response struct).
    // `get_preview_gateway_logs` is intentionally NOT included: it
    // returns `LogsResponse { lines }` straight from `tail_logs` —
    // raw Docker container stdout/stderr, zero redaction. Flagged in
    // security review on PR #732 (Greptile, 5th pass).
    "get_preview_gateway_settings",
    "get_preview_gateway_status",
    // ── External plugins ──
    "list_external_plugins",
    // ── Analytics: event entry list — excluded ──
    // `get_event_entries` is intentionally NOT included. Per its own doc
    // comment: "raw occurrences of a specific event, including custom
    // JSON properties" — same risk class as the already-excluded
    // `get_property_breakdown`/`get_property_timeline`. Flagged in
    // security review on PR #732 (Greptile, 4th pass).
    // ── Agents + sandbox: run status, job metadata ──
    // `list_agent_runs`, `list_all_runs`, `get_run_with_logs`, and
    // `latest_run_for_source` are intentionally NOT included: they all
    // serialize `AgentRunResponse`, which carries `ai_output`,
    // `ai_reasoning`, `prompt_text`, `ephemeral_yaml`, `analysis`, and
    // `user_context` verbatim — raw agent execution content, not
    // metadata. `job_status` is also excluded: `JobStatusResponse`
    // returns raw `stdout`/`stderr` from a detached sandbox command.
    // `list_jobs`/`get_cmd` are ALSO now excluded (previously kept —
    // wrong call): their DTOs (`JobSummaryResponse.cmd`,
    // `CmdInner.args`) carry the full invoked command line, which can
    // itself embed a secret passed as a CLI argument (e.g. `mysql
    // -p'...'`, `curl -H "Authorization: Bearer ..."`). Flagged in
    // security review on PR #732 (Greptile, 4th + 5th pass).
    "get_sandbox",
    "list_sandboxes",
    // Sandbox event list — lifecycle events (created, started, stopped).
    "list_events",
    // ── Coverage audit: reads added after a DTO review (masked or no secrets) ──
    // Projects and deployments
    "get_cloudflare_project_capability",
    "get_latest_deployment_media",
    "list_managed_environment_variables",
    "get_failure_report_preview",
    "get_deployment_by_upload_request_id",
    "list_operations",
    // Backups
    "list_s3_sources",
    "get_s3_source",
    // Git
    "get_connection",
    "validate_connection",
    // AI applications
    "list_applications",
    "get_application",
    "list_application_conversations",
    "list_thread_artifacts",
    "listApplicationGitConnections",
    "get_application_workspace",
    "get_application_workspace_changes",
    "get_application_workspace_diff",
    "get_application_workspace_directory",
    "get_application_workspace_file",
    "get_ai_provider_status",
    // Revenue (aggregates only)
    "revenue_metrics_customers",
    "revenue_metrics_mrr",
    "revenue_metrics_summary",
    "revenue_metrics_global_mrr",
    "revenue_metrics_global_summary",
    "revenue_list_providers",
    // Observability
    "global_log_aggregate",
    "global_log_attribute_keys",
    "global_log_capabilities",
    "global_log_facets_attrs",
    "global_log_histogram",
    "log_ingest_budget",
    "list_facets",
    "NodeMetricsGetAlertRules",
    "get_cloud_backfill_status",
    "get_current_bulk_activation_job",
    "get_bulk_activation_job",
    "get_project_cloud_telemetry",
    "get_cloud_telemetry_status",
    "has_traces",
    "get_pipeline_history",
    "get_activity_status",
    // Domains, DNS and delivery
    "get_http_challenge_debug",
    "get_record_ownership",
    "get_visible_custom_domain_by_hostname",
    "get_delivery_capabilities",
    "list_delivery_profiles",
    "get_delivery_profile",
    "get_project_delivery_settings",
    "list_domain_delivery_bindings",
    "list_traefik_discovered_routes",
    "get_traefik_discovery_status",
    "list_discoverable_domains",
    "list_email_domain_projects",
    // Cluster and platform
    "cluster_dns_status",
    // Internal DNS observability (ADR-095), SettingsRead: each node's
    // resolver health, the records it serves, the query log and traffic.
    // Names, addresses and counts only, no credential.
    "list_dns_resolvers",
    "list_dns_records",
    "list_dns_queries",
    "dns_traffic",
    "NodeCapabilityGet",
    "NodeDockerDiskUsageGet",
    "admin_list_nodes",
    "admin_get_node",
    "admin_list_node_containers",
    "admin_drain_status",
    "admin_get_node_public_ingress_settings",
    "ClusterNetworkGet",
    "NodePairingList",
    "WireguardMeshStatusGet",
    "get_private_ip",
    "get_feature_maturity",
    "get_geo_database_status",
    "get_telemetry_settings",
    "get_cloud_ai_capability",
    "get_cloud_capability",
    "get_cloud_status",
    // Databases and notifications
    "get_query_container_info",
    "get_notification_delivery_coverage",
    // ── Workload plane (ADR-055): placement, volumes, node resources,
    //    workload modes and network policy, read only ──
    "admin_environment_pinning_recommendation",
    "admin_environment_volume_status",
    "admin_explain_environment_placement",
    "admin_get_environment_workload_isolation",
    "admin_get_environment_workload_mode",
    "admin_get_node_resources",
    "admin_list_environment_volumes",
    "get_effective_policy",
    // Which projects may call a project's internal addresses (ADR 101):
    // project and environment ids, names and a description, no credential.
    // ProjectsRead on the project, narrowed to it.
    "list_internal_access_grants",
    "get_ingress_settings",
    "get_rule",
    "get_workload_mode",
    "get_workload_pending_changes",
    "list_node_modes",
    "list_rules",
    "node_state_status",
    // Durable workflows (ADR-103): status, runs and journals with every
    // payload masked, settings, and whether a credential exists (its prefix,
    // never the token). Decrypting a payload is `reveal_workflow_payload`,
    // which is withheld.
    "get_workflows_overview",
    "list_workflow_runs",
    "get_workflow_run",
    "list_workflow_steps",
    "get_workflow_settings",
    "get_workflow_credential",
];

/// [`AI_READ_ALLOWLIST`] as owned strings, the shape [`crate::InternalApiCaller`] takes.
pub fn ai_read_allowlist() -> Vec<String> {
    AI_READ_ALLOWLIST.iter().map(|s| s.to_string()).collect()
}

/// Mutating operations the AI may invoke through the native harness policy.
///
/// `temps_write` records an encrypted pending action, applies the conversation's
/// approval mode, and replays approved mutations through the same router
/// (`permission_guard!` + audit). Destructive operations always remain explicit.
///
/// Conservative by design: high-value, mostly-reversible lifecycle + config
/// operations. Adding an entry is a product + security decision — what may the
/// AI propose for a human to run.
///
/// Extracted into its own function (rather than an inline literal at the call
/// site) so tests can assert it stays disjoint from `ai_read_safe_posts()`,
/// which is the one rule holding up the read-only-POST mechanism.
pub const AI_WRITE_ALLOWLIST: &[&str] = &[
    // Same scoped user authorization as the console, through the existing
    // assistant write policy. This does not let the agent change that policy.
    "request_sandbox_service",
    "approve_sandbox_service",
    "revoke_sandbox_service",
    // ── Deployment lifecycle (reversible / safe) ──
    // Redeploy the project from its configured branch —
    // what a "redeploy main" request maps to
    // (promote/rollback are NOT redeploys).
    "trigger_project_pipeline",
    "rollback_to_deployment",
    "promote_deployment",
    "pause_deployment",
    "resume_deployment",
    "cancel_deployment",
    // ── Manual image deploy (no git build) ──
    // Deploy a prebuilt Docker image by `image_ref` (a
    // pullable registry ref) or a registered
    // `external_image_id`, to a specific environment_id.
    // Static-bundle deploys are intentionally NOT here: the
    // AI can't perform the multipart file upload, so the
    // whole static flow (upload + deploy) lives in the
    // frontend.
    "deploy_from_image",
    // ── Container runtime control (reversible) ──
    "restart_container",
    "stop_container",
    "start_container",
    // ── Environment wake/sleep (reversible) ──
    "wake_environment",
    "sleep_environment",
    // ── Environment settings (resource limits, replicas,
    //    branch) — what "raise memory to 512 MB" /
    //    "give it more CPU" / "scale to 2 replicas" map to.
    //    Values are microcores (1_000_000 = 1 core) and MB.
    //    Reversible: it's a config change, re-applicable.
    "update_environment_settings",
    // ── Automatic deployment + Git delivery repairs ──
    // These are the smallest reversible fixes for the common "pushes do
    // not deploy" workflow. `update_git_settings` and webhook reinstall
    // can contact the configured Git provider, but `temps_write` only
    // stages the exact request: the current user must still have the
    // operation's permission and the harness policy must authorize replay.
    "update_automatic_deploy",
    "update_git_settings",
    "reinstall_gitlab_webhook",
    "reinstall_github_webhook",
    // ── Environment variables (set / change) ──
    "create_environment_variable",
    "update_environment_variable",
    "delete_environment_variable",
    // Says a variable is not a credential, hiding the "store as secret?"
    // hint until its value changes. Takes and returns no value.
    "dismiss_environment_variable_secret_suggestion",
    // ── Domains (attach / detach at the environment level only;
    //    account-global domain create/delete excluded) ──
    "add_environment_domain",
    "delete_environment_domain",
    // ── Ingress tunnels (ADR 058 D9, D10) ──
    // Put one hostname through a tunnel (or none), re-sync a tunnel, or
    // create one for an already managed zone; the human approves each
    // like every write. Left out on purpose, because one call withdraws
    // the DNS of every hostname a tunnel carries: deleting it
    // (`delete_ingress_connector`), emptying its workers
    // (`set_ingress_connector_nodes`) and switching it to chosen names
    // only (`update_ingress_connector`). Those stay in the console.
    "set_ingress_hostname_choice",
    "sync_ingress_connector",
    "create_ingress_connector",
    // ── Managed external services (databases, caches, etc.) —
    //    provisioning a new container and linking an existing
    //    one to a project. Both reversible (a service can be
    //    unlinked / left running unused; nothing is deleted).
    "create_service",
    "link_service_to_project",
    // A cluster that failed while coming up is repaired in place; the
    // alternative is a second cluster beside the broken one. It carries no
    // credentials and only re-runs the creation the user asked for.
    "retry_cluster",
    // ── Getting started: create a project and ship it ──
    // `create_project` takes a Git URL (public) or a connected provider
    // repository and creates the project plus its first environment.
    // Reversible (a project can be deleted by a human) and it only
    // starts a build the user asked for. Credentials are never part of
    // this payload: a private repository goes through a Git connection
    // the user added in the console, referenced by id.
    "create_project",
    "create_project_from_template",
    "create_environment",
    // Refresh the repository list of an existing Git connection.
    "sync_repositories",
    // Create an empty repository through a Git connection, referenced by id.
    // The handler requires `git_repositories:create` and resolves the
    // connection by (id, caller), so another user's connection answers 404:
    // the assistant reaches exactly what its user could from the console,
    // with that connection's own token. The payload carries no credential.
    "create_repository",
    // Commit files to a repository, and open a pull request, through the same
    // kind of connection. Both require `git_repositories:write` and resolve
    // the connection by (id, caller). A commit only adds history on top of
    // the branch (it never force-pushes), is bounded in files and bytes, and
    // is shown to the user for approval first; the token never enters the
    // chat or a sandbox. The credential-bearing provider operations
    // (`update_connection_token`, `create_*_pat_provider`) stay out.
    "commit_repository_files",
    "open_repository_pull_request",
    // ── Getting started: backups ──
    // Schedules reference an S3 source by id. Registering or editing a
    // source takes the bucket secret key, so these three are proposable only
    // because `secret_key` is a `SecretString` flagged `x-temps-secret`: the
    // model passes a `secret_ref:` handle from `request_secret`, never the
    // key, and the caller substitutes it server-side at confirm time. Git
    // provider tokens (`create_*_pat_provider`, `create_generic_provider`)
    // are not converted yet and stay out.
    "create_s3_source",
    "update_s3_source",
    // Checks a candidate bucket before it is saved; the same secret_key rules.
    "test_s3_connection_preview",
    "create_backup_schedule",
    "update_backup_schedule",
    "enable_backup_schedule",
    "disable_backup_schedule",
    "attach_schedule_services",
    "run_schedule_now",
    // Connectivity probe against a stored source; reads nothing back but
    // an ok/error result.
    "test_s3_source_connection",
    // ── Operating a database service ──
    // Lifecycle and sizing of a service the user already has. None of
    // these carry a credential. `upgrade_service`, `promote_cluster_member`
    // (a failover), restores and Postgres upgrades are deliberately NOT
    // here: they put data or availability at risk and are done from the
    // console where the user sees the plan first.
    "start_service",
    "stop_service",
    "update_service_resources",
    "trigger_service_health_check",
    // Turning a managed database's server TLS on or off. Takes only the
    // mode, no credential; like a resize it restarts a standalone database.
    "update_service_tls",
    "add_cluster_member",
    "unlink_service_from_project",
    // Removing a service is the only way to clean up a cluster that failed
    // to come up. DELETE always waits for the user, in every mode.
    "delete_service",
    // ── Project configuration ──
    // `update_project_deployment_config` is NOT here: its payload can carry
    // a password-protection secret and arbitrary headers.
    "update_project",
    "update_project_settings",
    "update_environment_subdomain",
    // ── Backups: run and manage, never create the destination ──
    "run_external_service_backup",
    // Writes, reads back and deletes one probe object in the archive bucket
    // with the service's own WAL-G settings; it takes no credential and
    // changes no configuration, but it does touch the bucket, so it is a
    // confirmed write rather than a safe read.
    "test_wal_archive_target",
    "cancel_backup",
    "cancel_schedule_run",
    "detach_schedule_service",
    "delete_backup_schedule",
    // ── Domains and certificates ──
    // Registering a domain, ordering and finalizing its certificate and
    // pointing a project at it. DNS provider credentials are stored on
    // the provider and never travel in these payloads.
    "create_domain",
    "provision_domain",
    "renew_domain",
    "update_certificate_settings",
    "create_or_recreate_order",
    "finalize_order",
    "setup_dns_challenge",
    "cancel_domain_order",
    "create_custom_domain",
    "update_custom_domain",
    "delete_custom_domain",
    "link_custom_domain_to_certificate",
    // ── Alarms and incidents ──
    "acknowledgeAlarm",
    "resolveAlarm",
    "silenceAlarm",
    "acknowledgeSystemAlarm",
    "resolveSystemAlarm",
    "silenceSystemAlarm",
    // Checks are created in the console (their payload can hold request
    // headers); the assistant can switch one on or off and run it now.
    "setHttpCheckEnabled",
    "runHttpCheck",
    "create_monitor",
    "update_monitor",
    "create_incident",
    "update_incident_status",
    // ── Feature flags, error rules and scans ──
    "create_flag",
    "update_flag",
    "set_flag_environment",
    "restore_flag",
    "archive_flag",
    "create_alert_rule",
    "update_alert_rule",
    "update_error_group",
    "trigger_scan",
    // ── AI application topology ──
    // Composite create is the only safe way for the model to create a
    // Temps project and immediately link its generated id to the current
    // application. Each operation is still replayed with current user
    // authorization after native approval.
    "create_application_project",
    "deploy_application_workspace_project",
    "link_application_project",
    "unlink_application_project",
    "set_application_primary_project",
    "update_application_workspace",
    "control_application_workspace",
    // ── Metric alert rules (OTel) ──
    // Create/update an alert rule. Reversible (a rule
    // can be disabled or deleted) and non-destructive:
    // creating one changes no running workload, it only
    // starts evaluating a metric. This is what lets the
    // assistant turn "your p95 has no alert on it" into
    // a concrete rule the human confirms.
    //
    // `delete_alert` is deliberately excluded: deleting
    // an alert silently removes monitoring, which is the
    // kind of change that is only noticed when the
    // incident it would have caught happens.
    "create_alert",
    "update_alert",
    // ── Global notification control plane ──
    // Provider payloads may contain credentials. `temps_write` encrypts
    // executable parameters at rest and redacts both the approval card and
    // result returned to the model. DELETE operations remain explicit even
    // in Auto mode through `platform_request_is_destructive`.
    "create_notification_provider",
    "update_notification_provider",
    "delete_notification_provider",
    "create_slack_provider",
    "update_slack_provider",
    "create_notification_email_provider",
    "update_notification_email_provider",
    "create_webhook_provider",
    "update_webhook_provider",
    "create_cloudflare_provider",
    "update_cloudflare_provider",
    "test_notification_provider",
    "create_notification_route",
    "update_notification_route",
    "delete_notification_route",
    // ── Coverage audit: reversible configuration and lifecycle, proposed for approval; deletions always wait ──
    // Project source and deployment settings
    "set_alternate_sources",
    "change_project_source",
    "update_service_template_runtime",
    "preview_domain_delivery_binding",
    "apply_domain_delivery_binding",
    "delete_domain_delivery_binding",
    "update_project_delivery_settings",
    // Backups: set default and run now
    "set_default_s3_source",
    "run_backup_for_source",
    // Git connections and providers (toggle and health only)
    "activate_connection",
    "deactivate_connection",
    "activate_provider",
    "deactivate_provider",
    "run_connection_health_check",
    // Databases and services
    "ExternalServiceResetPgStatStatements",
    "ExternalServiceMetricsToggle",
    "DeploymentMetricsToggle",
    "ExternalServiceMetricsCreateAlertRule",
    "ExternalServiceMetricsUpdateAlertRule",
    "NodeMetricsCreateAlertRule",
    "NodeMetricsUpdateAlertRule",
    "update_service_log_ingestion",
    "kv_enable",
    "kv_update",
    "blob_update",
    // Domains, DNS and email
    "add_managed_domain",
    "update_managed_domain",
    "verify_managed_domain",
    "apply_hostname_mode",
    "test_provider_connection",
    "set_managed_record",
    "import_managed_record",
    "remove_managed_record",
    "remove_managed_domain",
    "create_email_domain",
    "import_email_domain",
    "setup_dns",
    "verify_domain",
    "authorize_email_domain_project",
    "revoke_email_domain_project",
    "test_provider",
    "setup_email_tracking",
    // Alerting, checks and notifications
    "bulkUpdateProjectAlarms",
    "bulkUpdateSystemAlarms",
    "deleteHttpCheck",
    "delete_monitor",
    "test_notification_route",
    "update_preferences",
    "delete_preferences",
    "retry_delivery",
    "delete_webhook",
    // Observability and analytics
    "create_dashboard",
    "update_dashboard",
    "delete_dashboard",
    "create_facet",
    "delete_facet",
    "retry_facet_backfill",
    "create_funnel",
    "update_funnel",
    "delete_funnel",
    "update_project_cloud_telemetry",
    // Which traffic analytics counts: reversible (a rule is deleted, a
    // switch flipped back) and takes no credential.
    "update_project_analytics_settings",
    "create_traffic_filter",
    "delete_traffic_filter",
    "save_activity_settings",
    "run_activity_report",
    "preview_activity_report",
    "suggest_activity_goals",
    "workflow_dry_run",
    "validate_email",
    // Error tracking and security
    "start_analysis",
    "add_context",
    "cancel",
    "start_fix",
    "re_analyze",
    "create_pr",
    "delete_source_map",
    "delete_release_source_maps",
    "delete_release_source_files",
    "delete_scan",
    // Registered external images
    "register_external_image",
    "delete_external_image",
    // AI applications
    "create_application",
    "archive_application",
    "create_application_conversation",
    "create_thread_artifact",
    "bindApplicationGitConnection",
    "disconnectApplicationGitConnection",
    "create_application_preview_link",
    "write_application_workspace_files",
    "import_application_workspace_git",
    "restore_application",
    "upload_application_workspace_files",
    // Agents, skills and MCP removal
    "trigger_agent",
    "cancel_run",
    "retry_run",
    "smoke_test_agent",
    "delete_agent",
    "create_skill",
    "update_skill",
    "delete_skill",
    "create_global_skill",
    "update_global_skill",
    "delete_global_skill",
    "delete_mcp",
    "delete_global_mcp",
    // AI gateway models (not keys)
    "add_provider_model",
    "refresh_provider_models",
    "update_provider_model",
    "delete_provider_model",
    "test_provider_key_by_id",
    // Revenue configuration
    "revenue_update_config",
    "revenue_delete_integration",
    // Durable workflow run lifecycle (ADR-103): stopping a run, or re-running
    // it from a step or out of `stuck`, through the console's guards and
    // audit. Starting a run or sending an event (which runs the app's code
    // with a caller-chosen payload) and deleting one stay console-only.
    "cancel_workflow_run",
    "retry_workflow_run",
    "resume_workflow_run",
];

/// [`AI_WRITE_ALLOWLIST`] as owned strings, the shape [`crate::InternalApiCaller`] takes.
pub fn ai_write_allowlist() -> Vec<String> {
    AI_WRITE_ALLOWLIST.iter().map(|s| s.to_string()).collect()
}

/// Vetted **read-only `POST`** operations for the AI read tool.
///
/// HTTP method is this codebase's structural proxy for "does this mutate?", and
/// `ai_read_allowlist` above is GET-only for exactly that reason. This is the
/// narrow, separately-reviewed exception: operations that are genuinely
/// side-effect-free but are `POST` because their input is a structured document
/// rather than a handful of query params.
///
/// **The rule for adding an entry: the operation must write nothing.** Not a
/// row, not a file, not a queued job. If it mutates anything at all it belongs
/// in the propose-then-confirm write allowlist instead, never here. Keeping this
/// list separate from the GET allowlist is what makes each addition a conscious
/// decision rather than a line lost in a 200-entry list.
///
/// Deliberately NOT here: anything that creates, updates, deletes, triggers, or
/// enqueues — including the metric-alert CRUD operations that live next to
/// `preview_alert` in the same handler module.
pub const AI_READ_SAFE_POSTS: &[&str] = &[
    // Backtest a metric-alert detector over historical data: replays the
    // metric against the band the evaluator would use and returns which
    // points would have fired. Explicitly documented read-only, guarded by
    // OtelRead + project_access_guard!, and persists nothing. It is a POST
    // only because the request body is a whole detector config.
    //
    // This is what lets the assistant check "would this rule actually have
    // fired?" *before* proposing it, so a suggested alert arrives with
    // evidence attached instead of a guessed threshold.
    "preview_alert",
    // ── Coverage audit: POSTs that write nothing (search, preview, quote) ──
    // Read-only POST: log search and facets
    "search_logs",
    "search_global_logs",
    "facet_global_logs",
    // Read-only POST: previews and quotes
    "preview_funnel_metrics",
    "estimate_bulk_activation",
    "get_repository_compose_preview",
    "get_public_compose_preview",
    "generate_preset_dockerfile",
    // Read-only POST: *Make it reachable* plans change nothing; a plan is
    // only kept for `apply`, which stays with the operator.
    "plan_environment_reachability",
    // Read-only POST: which permissions a Cloudflare token has. Nothing is
    // created or stored; the token is a `SecretString`, so the assistant
    // passes it through a secret slot.
    "check_cloudflare_token",
    // Read-only POST: one test lookup of a *.temps.local name from one
    // node's resolver (ADR-095), SettingsRead. It stores nothing; the body
    // is a POST only because it names the node, the name and the type.
    "resolve_dns_name",
];

/// [`AI_READ_SAFE_POSTS`] as owned strings, the shape [`crate::InternalApiCaller`] takes.
pub fn ai_read_safe_posts() -> Vec<String> {
    AI_READ_SAFE_POSTS.iter().map(|s| s.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn certificate_policy_reads_and_writes_have_separate_permissions() {
        let reads = ai_read_allowlist();
        let writes = ai_write_allowlist();
        assert!(reads.iter().any(|id| id == "get_certificate_settings"));
        assert!(!reads.iter().any(|id| id == "update_certificate_settings"));
        assert!(writes.iter().any(|id| id == "update_certificate_settings"));
        assert!(!writes.iter().any(|id| id == "get_certificate_settings"));
    }

    /// The read-only-POST list and the write allowlist must stay disjoint.
    ///
    /// This is the one rule holding up the safe-POST mechanism, and until now it
    /// existed only as prose in a doc comment. `create_alert` and
    /// `preview_alert` are neighbours in the same handler module under the same
    /// OpenAPI tag, so a future `preview_and_save_alert` added by pattern-match
    /// would execute unconfirmed writes with the chat user's auth and no confirm
    /// card. Turn the rule into a build failure.
    #[test]
    fn safe_post_and_write_allowlists_are_disjoint() {
        let safe: std::collections::HashSet<String> = ai_read_safe_posts().into_iter().collect();
        let writes: std::collections::HashSet<String> = ai_write_allowlist().into_iter().collect();

        let both: Vec<&String> = safe.intersection(&writes).collect();
        assert!(
            both.is_empty(),
            "an operation cannot be both a side-effect-free read and a vetted write: {both:?}"
        );
    }

    #[test]
    fn automatic_deployment_repairs_are_proposable() {
        let writes: std::collections::HashSet<String> = ai_write_allowlist().into_iter().collect();

        for operation in [
            "update_automatic_deploy",
            "update_environment_settings",
            "update_git_settings",
            "reinstall_gitlab_webhook",
            "reinstall_github_webhook",
        ] {
            assert!(
                writes.contains(operation),
                "automatic-deployment diagnosis cannot stage `{operation}`"
            );
        }
    }

    /// Creating a repository is allowed through the user's own connection and
    /// stays a proposal; pushing, which would need a token in the request, is not
    /// something the assistant can stage.
    #[test]
    fn git_repository_operations_are_proposable_but_credential_ones_are_not() {
        let writes: std::collections::HashSet<String> = ai_write_allowlist().into_iter().collect();
        let safe: std::collections::HashSet<String> = ai_read_safe_posts().into_iter().collect();
        for operation in [
            "create_repository",
            "commit_repository_files",
            "open_repository_pull_request",
        ] {
            assert!(
                writes.contains(operation),
                "the assistant cannot stage `{operation}`"
            );
            assert!(
                !safe.contains(operation),
                "`{operation}` is a write and needs the confirm card"
            );
        }
        for operation in [
            "update_connection_token",
            "create_github_pat_provider",
            "create_gitlab_pat_provider",
            "create_git_provider",
            "delete_connection",
        ] {
            assert!(
                !writes.contains(operation),
                "`{operation}` must stay out of the assistant's write tool"
            );
        }
    }

    /// The assistant must be able to take a user from nothing to a deployed
    /// project and scheduled backups. An operation whose payload carries a stored
    /// credential is proposable only once that field is a flagged `SecretString`
    /// (the S3 source operations); the rest still go through the console forms.
    #[test]
    fn getting_started_operations_are_proposable_but_unconverted_credential_forms_are_not() {
        let writes: std::collections::HashSet<String> = ai_write_allowlist().into_iter().collect();
        for operation in [
            "create_project",
            "create_project_from_template",
            "create_environment",
            "create_backup_schedule",
            "update_backup_schedule",
            "run_schedule_now",
            "test_s3_source_connection",
            "create_s3_source",
            "update_s3_source",
            "test_s3_connection_preview",
        ] {
            assert!(
                writes.contains(operation),
                "getting-started flow cannot stage `{operation}`"
            );
        }
        for operation in [
            "create_github_pat_provider",
            "create_gitlab_pat_provider",
            "create_gitea_pat_provider",
            "create_generic_provider",
            "create_bitbucket_provider",
            "update_git_provider_credentials",
        ] {
            assert!(
                !writes.contains(operation),
                "`{operation}` carries a secret and must stay out of the assistant's write tool"
            );
        }
        // Both return `S3SourceResponse`, which masks the access key id and the
        // secret key as `***` (`backup_handler.rs`, pinned by
        // `s3_source_response_masks_both_keys` in temps-backup). They have to be
        // readable: the backup playbook starts from "is there a source already?".
        let reads: std::collections::HashSet<String> = ai_read_allowlist().into_iter().collect();
        for operation in ["list_s3_sources", "get_s3_source"] {
            assert!(
                reads.contains(operation),
                "`{operation}` returns only masked credentials and must be readable"
            );
        }
    }

    /// The assistant has to be able to run the platform it set up: see why a
    /// service failed, repair or remove it, size it, put a domain on a project.
    /// And the operations whose payload or response carries a secret, or that
    /// can lose data, stay with the console. Pinning both keeps a later
    /// addition from quietly crossing the line in either direction.
    #[test]
    fn operating_operations_are_proposable_and_secret_or_data_risking_ones_are_not() {
        let writes: std::collections::HashSet<String> = ai_write_allowlist().into_iter().collect();
        let reads: std::collections::HashSet<String> = ai_read_allowlist().into_iter().collect();
        for operation in [
            "start_service",
            "stop_service",
            "update_service_resources",
            "update_service_tls",
            "add_cluster_member",
            "retry_cluster",
            "delete_service",
            "update_project",
            "run_external_service_backup",
            "create_custom_domain",
            "provision_domain",
            "acknowledgeAlarm",
        ] {
            assert!(
                writes.contains(operation),
                "operating flow cannot stage `{operation}`"
            );
        }
        for operation in [
            "get_service",
            "get_service_by_slug",
            "get_access_info",
            "get_service_tls",
            "get_managed_ca_certificate",
        ] {
            assert!(
                reads.contains(operation),
                "operating flow cannot read `{operation}`"
            );
        }
        // Payload carries a secret, or the operation can lose data or fail over.
        for operation in [
            "update_project_deployment_config",
            "createHttpCheck",
            "updateHttpCheck",
            "create_webhook",
            "create_api_key",
            "create_deployment_token",
            "createProjectSecret",
            "upsert_secret",
            "create_dsn",
            "upgrade_service",
            "promote_cluster_member",
            "start_restore",
            "start_data_import",
            "start_pg_upgrade",
            "delete_project",
            "delete_backup",
        ] {
            assert!(
                !writes.contains(operation),
                "`{operation}` must stay out of the assistant's write tool"
            );
        }
        // Responses that return plaintext secrets.
        for operation in [
            "reveal_service_parameter",
            "reveal_service_environment_variables",
            "get_environment_variable_value",
            "get_resolved_environment_variable_value",
            "get_container_environment_variable",
            "get_service_environment_variable",
            "get_settings",
        ] {
            assert!(
                !reads.contains(operation),
                "`{operation}` returns plaintext secrets and must stay out of the read tool"
            );
        }
    }

    /// The AI read allowlist must never contain duplicate entries — a repeat
    /// is dead weight in the model's tool catalogue and a signal something
    /// was pasted twice while merging.
    #[test]
    fn ai_read_allowlist_has_no_duplicate_entries() {
        let allowlist = ai_read_allowlist();
        let mut seen = std::collections::HashSet::new();
        for entry in &allowlist {
            assert!(
                seen.insert(entry.as_str()),
                "duplicate allowlist entry: {entry}"
            );
        }
    }
}

#[cfg(test)]
mod sandbox_service_tests {
    use super::*;
    #[test]
    fn chat_can_discover_and_manage_scoped_sandbox_access() {
        for operation in [
            "list_access_services",
            "get_access_service",
            "list_sandbox_services",
        ] {
            assert!(AI_READ_ALLOWLIST.contains(&operation));
        }
        for operation in [
            "request_sandbox_service",
            "approve_sandbox_service",
            "revoke_sandbox_service",
        ] {
            assert!(AI_WRITE_ALLOWLIST.contains(&operation));
            assert!(!AI_READ_ALLOWLIST.contains(&operation));
        }
        assert!(!AI_WRITE_ALLOWLIST.contains(&"confirm_pending_action"));
    }
}
