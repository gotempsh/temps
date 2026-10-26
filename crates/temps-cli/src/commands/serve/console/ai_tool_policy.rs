// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Coverage policy for the assistant's API tools.
//!
//! Every operation in the published OpenAPI document must be in exactly one of
//! four places: the read allowlist, the read-only-POST list, the write
//! allowlist (all in the parent module), or [`EXCLUDED`] below with the reason
//! it is withheld from the assistant. A new endpoint that is none of those
//! fails `every_operation_is_classified`, so an endpoint can no longer be
//! invisible to the assistant by accident, which is how the model came to tell
//! a user that the platform had no setting for something it did have.
//!
//! This is test-only data: moving an operation out of [`EXCLUDED`] and into an
//! allowlist is the product and security decision, and it is made in review.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use super::{ai_read_allowlist, ai_read_safe_posts, ai_write_allowlist};

/// Operations withheld from the assistant, grouped by the reason they are.
pub(super) const EXCLUDED: &[(&str, &[&str])] = &[
    (
        "irreversible data loss or an availability risk; the console shows the user the plan first",
        &[
            "ExternalServiceEnablePgStatStatements",
            // *Make it reachable*: changes DNS records, tunnels and
            // certificates for a live address, and can take a Cloudflare token
            // again on resume. The console shows the plan and asks.
            "apply_environment_reachability",
            "decide_reachability_conflict",
            "resume_reachability_run",
            "undo_reachability_run",
            "blob_disable",
            "cancel_data_import",
            "cancel_pg_upgrade",
            "cancel_restore_run",
            "cleanup_expired_backups",
            "delete_backup",
            "delete_connection",
            "delete_delivery_profile",
            "delete_dns_provider",
            "delete_domain",
            "delete_email_domain",
            "delete_email_provider",
            "delete_environment",
            "delete_environment_ip_access",
            "delete_git_provider",
            "delete_ingress_connector",
            "delete_project",
            "delete_project_group",
            "delete_project_ip_access",
            "delete_provider_key",
            "delete_provider_safely",
            "delete_s3_source",
            "delete_static_bundle",
            "delete_status_page",
            "kv_disable",
            "plan_restore",
            "promote_cluster_member",
            "purge_project_logs",
            "put_environment_ip_access",
            "put_project_ip_access",
            "remove_cluster_member",
            "repointContinuousArchiveSource",
            "retry_pg_upgrade",
            "rollback_pg_upgrade",
            "rotate_status_page_url",
            "set_ingress_connector_nodes",
            "set_project_hosting",
            "start_data_import",
            "start_pg_upgrade",
            "start_restore",
            "teardown_deployment",
            "teardown_environment",
            "unpublish_status_page",
            "update_ingress_connector",
            "upgrade_project_service_template",
            "upgrade_service",
        ],
    ),
    (
        "removes monitoring silently, so a human must do it (same rule as delete_alert)",
        &[
            "ExternalServiceMetricsDeleteAlertRule",
            "NodeMetricsDeleteAlertRule",
            "delete_alert",
            "delete_alert_rule",
        ],
    ),
    (
        "cluster topology and node enrollment; operator only",
        &[
            "ClusterNetworkUpdate",
            "NodePairingCancel",
            "NodePairingCreate",
            "NodeSshEnrollmentCreate",
            "NodeSshEnrollmentGet",
            "NodeSshEnrollmentList",
            "NodeSshHostKey",
            "WireguardMeshEnable",
            "WireguardMeshHubSet",
            "admin_check_node_public_ingress",
            "admin_drain_node",
            "admin_remove_node",
            "admin_set_node_build_limits",
            "admin_set_node_public_ingress",
            "admin_undrain_node",
            "admin_update_node_public_ingress_settings",
            "list_peers",
        ],
    ),
    (
        "node-agent, mesh, streaming or websocket endpoint, not a tool call",
        &[
            "WireguardMeshHandshakesReport",
            "WireguardMeshRegister",
            "attach_application_workspace_terminal",
            "attach_global_workspace_terminal",
            "get_dns_changes",
            "mesh_enroll",
            "node_heartbeat",
            "plane_identity",
            "post_dns_ack",
            "register_node",
            "stream_container_metrics",
        ],
    ),
    (
        "configures the assistant's own model provider; it must not reconfigure itself",
        &[
            "activate_ai_provider",
            "cancel_ai_provider_login",
            "discover_provider_models",
            "get_ai_provider_harness_update",
            "get_ai_provider_login",
            "refresh_ai_provider_models",
            "refresh_ai_provider_status",
            "reset_project_limits",
            "run_ai_provider_preflight",
            "run_ai_provider_smoke",
            "start_ai_provider_login",
            "submit_ai_provider_login_code",
            "update_ai_provider",
            "update_ai_provider_harness",
            "update_ai_summary_preference",
            "update_instance_limits",
            "update_project_limits",
            "verify_saved_ai_provider_credential",
        ],
    ),
    (
        "identity, session or API-credential management; console only",
        &[
            "activate_api_key",
            "assign_role",
            "change_password_self",
            "change_required_password",
            "cli_device_approve",
            "cli_device_deny",
            "cli_device_lookup",
            "cli_device_poll",
            "cli_device_start",
            "cli_logout",
            "createProjectSecret",
            "create_api_key",
            "create_deployment_token",
            "create_oidc_provider",
            "create_oidc_role_mapping",
            "create_user",
            "deactivate_api_key",
            "deleteProjectSecret",
            "delete_api_key",
            "delete_deployment_token",
            "delete_oidc_provider",
            "delete_oidc_role_mapping",
            "delete_secret",
            "delete_user",
            "disable_mfa",
            "get_admin_gate",
            "list_users",
            "login",
            "logout",
            "oidc_callback",
            "patch_admin_gate",
            "remove_role",
            "request_password_reset",
            "reset_password",
            "reset_user_mfa",
            "reset_user_password",
            "restore_user",
            "rotate_api_key",
            "rotate_deployment_token",
            "setup_mfa",
            "start_managed_cloud_login",
            "start_oidc_login_by_slug",
            "test_oidc_provider",
            "updateProjectSecret",
            "update_api_key",
            "update_deployment_token",
            "update_oidc_provider",
            "update_self",
            "update_user",
            "upsert_secret",
            "verify_and_enable_mfa",
            "verify_email",
            "verify_mfa_challenge",
            "verify_step_up",
        ],
    ),
    (
        "raw session capture with unredacted end-user content",
        &[
            "add_events",
            "delete_session_replay",
            "get_project_session_replays",
            "get_session_replay",
            "get_session_replay_events",
            "get_visitor_sessions",
            "update_session_duration",
        ],
    ),
    (
        "machine endpoint called by SDKs, workers or email clients, not an operator action",
        &[
            "add_session_replay_events",
            "append_ci_build_logs",
            "assemble_artifact_bundle",
            "check_ci_build_lease",
            "chunk_upload_options",
            "claim_ci_build",
            "complete_ci_build",
            "create_analytics_ingest_key",
            "create_project_release",
            "create_release",
            "fail_ci_build",
            "finalize_project_release",
            "get_ci_build_context",
            "get_public_status_page_json",
            "ingest_logs",
            "ingest_logs_by_path",
            "ingest_metrics",
            "ingest_metrics_by_path",
            "ingest_nel_reports",
            "ingest_security_reports",
            "ingest_sentry_event",
            "ingest_traces",
            "ingest_traces_by_path",
            "init_session_replay",
            "list_analytics_ingest_keys",
            "record_event_metrics",
            "record_speed_metrics",
            "revoke_analytics_ingest_key",
            "rotate_analytics_ingest_key",
            "track_click",
            "track_open",
            "update_analytics_ingest_key",
            "update_speed_metrics",
            "upload_artifact_chunk",
            "upload_release_file",
        ],
    ),
    (
        "changes who can access the platform; a prompt injection could escalate privilege",
        &[
            "add_team_member",
            "create_ip_access_control",
            "create_team",
            "delete_ip_access_control",
            "delete_team",
            "grant_project_access",
            "remove_team_member",
            "revoke_project_access",
            "update_compose_security",
            "update_ip_access_control",
            "update_team",
            "update_team_member_role",
        ],
    ),
    (
        "reviewed and kept out: the response carries raw, unredacted content",
        &[
            "aggregate_api_traffic",
            "get_analytics_session_events",
            "get_api_callers",
            "get_api_routes",
            "get_api_summary",
            "get_api_traffic_proxy_log_access",
            "get_data_import",
            "get_data_import_availability",
            "get_event_entries",
            "get_ingest_errors",
            "get_log_context",
            "get_preview_gateway_logs",
            "get_property_breakdown",
            "get_property_timeline",
            "get_proxy_log_by_id",
            "get_proxy_log_by_request_id",
            "get_proxy_logs",
            "get_run_with_logs",
            "get_session_logs",
            "latest_run_for_source",
            "list_agent_runs",
            "list_all_runs",
            "list_data_imports",
            "observability_full_event",
            "observability_list_events",
            "revenue_global_events",
            "revenue_recent_events",
        ],
    ),
    (
        "multipart upload, file download or binary body; not expressible as a tool call",
        &[
            "append_attachment_chunk",
            "archive_conversation",
            "archive_user_conversation",
            "blob_put",
            "create_response",
            "create_response_stream",
            "deploy_from_uploaded_source",
            "download_application_workspace_file",
            "download_autofix_patch",
            "download_global_skill_archive",
            "download_global_workspace_file",
            "download_object",
            "download_skill_archive",
            "file_content",
            "get_file",
            "get_public_status_page",
            "get_user_conversation_attachment",
            "ingest_sentry_envelope",
            "ingest_tunneled_envelope",
            "inspect_drop_archive",
            "stream_events",
            "stream_run_events",
            "stream_structured_output",
            "upload_file",
            "upload_global_skill",
            "upload_skill",
            "upload_static_bundle",
            "upload_user_conversation_attachment",
        ],
    ),
    (
        "the assistant's own chat machinery; it must not drive or approve itself",
        &[
            "accept_blueprint_version",
            "authorize_workspace_preview",
            "bind_blueprint_component",
            "cancel_attachment_upload",
            "cancel_goal",
            "close_autofix_incident",
            "compact_conversation",
            "complete_attachment_upload",
            "confirm_pending_action",
            "confirm_user_pending_action",
            "connect_chat_sandbox_project",
            "connect_chat_sandbox_service",
            "create_assistant_conversation",
            "create_conversation",
            "create_global_conversation",
            "create_global_workspace_preview_link",
            "discard_secret",
            "disconnect_chat_sandbox_project",
            "disconnect_chat_sandbox_service",
            "getUserConversationDiagnostics",
            "getUserConversationUsage",
            "get_blueprint",
            "get_chat_sandbox",
            "get_chat_sandbox_changes",
            "get_chat_sandbox_diff",
            "get_chat_suggestion_mode",
            "get_chat_workspace",
            "get_global_ai_workspace",
            "get_global_workspace_changes",
            "get_global_workspace_diff",
            "get_global_workspace_directory",
            "get_global_workspace_file",
            "get_suggestion_default",
            "get_user_conversation",
            "get_user_pending_action",
            "get_workspace_activity",
            "get_workspace_file_limits",
            "leave_chat_workspace",
            "link_chat_sandbox",
            "list_blueprint_versions",
            "list_chat_resources",
            "list_chat_sandbox_files",
            "list_goals",
            "list_secret_requests",
            "list_user_pending_actions",
            "open_autofix_incident",
            "provide_secret",
            "read_chat_sandbox_file",
            "reject_blueprint_version",
            "reject_pending_action",
            "reject_user_pending_action",
            "rename_application",
            "rename_conversation",
            "rename_user_conversation",
            "resolve_permission",
            "resolve_user_permission",
            "restore_user_conversation",
            "retry_goal",
            "save_blueprint_version",
            "send_project_ai_message",
            "send_user_message",
            "set_autofix_paused",
            "set_chat_workspace",
            "start_attachment_upload",
            "stop_autofix_incident",
            "stop_turn",
            "stop_user_turn",
            "suggest_next_steps",
            "track_chat_sandbox_changes",
            "unbind_blueprint_component",
            "unlink_chat_sandbox",
            "update_autofix_settings",
            "update_chat_suggestion_mode",
            "update_permission_mode",
            "update_suggestion_default",
            "update_user_permission_mode",
            "upload_global_workspace_files",
            "wake_chat_sandbox",
        ],
    ),
    (
        "user data plane (KV and blob contents), not platform management",
        &[
            "blob_copy",
            "blob_delete",
            "blob_download",
            "kv_del",
            "kv_expire",
            "kv_get",
            "kv_incr",
            "kv_keys",
            "kv_set",
            "kv_ttl",
        ],
    ),
    (
        "payload carries a credential; it must be entered in the console until sealed secret input exists",
        &[
            "blob_enable",
            "createHttpCheck",
            "create_agent",
            "create_bitbucket_provider",
            "create_delivery_profile",
            "create_dns_provider",
            "create_email_provider",
            "create_generic_provider",
            "create_git_provider",
            "create_gitea_pat_provider",
            "create_github_pat_provider",
            "create_gitlab_pat_provider",
            "create_global_mcp",
            "create_mcp",
            "create_plan",
            "create_provider_key",
            "create_webhook",
            "discover_workloads",
            "execute_import",
            "import_external_service",
            "import_local_ai_provider_credential",
            "revenue_create_integration",
            "revenue_update_secret",
            "save_agent_token",
            "save_ai_provider_credential",
            "set_ci_builder",
            "test_provider_key_inline",
            "updateHttpCheck",
            "update_agent",
            "update_connection_token",
            "update_email_provider",
            "update_git_provider_credentials",
            "update_global_mcp",
            "update_mcp",
            "update_project_deployment_config",
            "update_provider",
            "update_provider_key",
            "update_service",
            "update_settings",
            "update_webhook",
        ],
    ),
    (
        "model-inference data plane, not management",
        &[
            "cancel_batch",
            "chat_completions",
            "create_batch",
            "create_response_json",
            "delete_file",
            "embeddings",
            "retrieve_batch",
            "retrieve_file",
        ],
    ),
    (
        "host-level operator policy or cluster edge routing, not a per-project setting",
        &[
            "blob_get_settings",
            "blob_update_settings",
            "cancel_bulk_activation_job",
            "create_bulk_activation_job",
            "create_route",
            "deauthorize_discovered_route_cert",
            "delete_route",
            "ensure_cloud_backup_schedule",
            "get_compose_status",
            "get_public_ingress_status",
            "import_traefik_acme_json",
            "kv_get_settings",
            "kv_update_settings",
            "patch_preview_gateway_settings",
            "reconcile_cloud_backup_source",
            "request_discovered_route_cert",
            "restart_preview_gateway",
            "set_traefik_discovered_route_enabled",
            "update_cloud_features",
            "update_route",
            "update_telemetry_settings",
            "upgrade_preview_gateway",
        ],
    ),
    (
        "the node platform's own platform lifecycle and trust root; operator only",
        &[
            "check_for_update",
            "disconnect_cloud",
            "enroll_cloud",
            "generate_join_token",
            "mint_enrollment_token",
            "refresh_route_table",
            "revoke_enrollment_token",
            "revoke_join_token",
            "rotate_cluster_ca",
            "start_update",
        ],
    ),
    (
        "served by the dedicated sandbox tools",
        &[
            "clear_preview_password",
            "cmd",
            "cmd_kill",
            "cmd_logs",
            "create_sandbox",
            "create_snapshot",
            "delete_snapshot",
            "destroy_sandbox",
            "domain",
            "evict_node_sandboxes",
            "exec",
            "exec_detached",
            "extend_timeout",
            "get_cmd",
            "get_sandbox_placement",
            "get_snapshot",
            "job_logs",
            "job_status",
            "kill_job",
            "list_dir",
            "list_jobs",
            "list_node_sandboxes",
            "list_snapshots",
            "mkdir",
            "pause_sandbox",
            "read_file",
            "resize_sandbox",
            "restart_sandbox",
            "resume_sandbox",
            "rootfs_gc",
            "rootfs_report",
            "sandbox_create_preview_link",
            "sandbox_issue_runtime_environment",
            "set_preview_password",
            "source_sandbox",
            "stat_path",
            "stop_sandbox",
            "storage_summary",
            "terminal",
            "update_sandbox_placement",
            "write_file",
            "write_files",
        ],
    ),
    (
        "response returns a plaintext secret or mints a credential",
        &[
            "create_dsn",
            "detectEnvCredential",
            "detectSecretCredential",
            "get_challenge_token",
            "get_container_environment_variable",
            "get_environment_variable_value",
            "get_or_create_dsn",
            "get_provider_key",
            "get_resolved_environment_variable_value",
            "get_s3_credentials",
            "get_service_environment_variable",
            "get_settings",
            "issue_runtime_credentials",
            "listSecretHistory",
            "listVariableHistory",
            "list_dsns",
            "regenerate_dsn",
            "reveal_global_mcp_config",
            "reveal_mcp_config",
            "reveal_notification_provider_config",
            "reveal_service_environment_variables",
            "reveal_service_parameter",
            "revenue_list_integrations",
            "revenue_rotate_token",
            "revoke_dsn",
        ],
    ),
    (
        "browser OAuth callback",
        &[
            "create_gitlab_oauth_provider",
            "handle_git_provider_oauth_callback",
            "start_git_provider_oauth",
        ],
    ),
    (
        "static bundles and image archives need an upload the assistant cannot perform",
        &[
            "deploy_from_image_upload",
            "deploy_from_static",
            "execute_deployment_operation",
            "upload_source_file",
            "upload_source_map",
        ],
    ),
    (
        "personal data written or ingested on behalf of end users, not an operator action",
        &[
            "enrich_visitor",
            "record_console_event",
            "record_flag_exposure",
            "revenue_import_invoices_csv",
            "revenue_import_subscriptions_csv",
        ],
    ),
    (
        "can send data to an arbitrary external destination",
        &[
            "push_external_image",
            "send_email",
            "send_failure_report",
            "trigger_weekly_digest",
        ],
    ),
    (
        "the console's POST form of read_entity_rows; the assistant reads rows through the GET form",
        &[
            "query_data",
        ],
    ),
    (
        "moves a domain between projects; the tool layer does not scope its source_project_id selector, so a human does it",
        &[
            "reassign_project_custom_domain",
        ],
    ),
    (
        "platform image and trust configuration; operator only",
        &[
            "get_ci_builder",
            "provision_ci_builder",
            "rebuild_sandbox_image",
        ],
    ),
    (
        "tail_logs streams; use get_container_logs",
        &[
            "tail_logs",
        ],
    ),
    (
        "inbound webhook or machine endpoint, not an operator action",
        &[
            "webhook_trigger",
        ],
    ),
    (
        "added on main while the assistant branch was open; withheld until it is reviewed for the assistant",
        &[
            "add_project_to_group",
            "add_status_page_domain",
            "clear_group_repository",
            "createCronMonitor",
            "create_group_env_var",
            "create_group_route",
            "create_project_group",
            "create_status_page",
            "deleteCronMonitor",
            "delete_group_env_var",
            "delete_group_route",
            "delete_project_feedback",
            "external_service_backup_preflight",
            "getCronMonitor",
            "get_analytics_ingest_key_endpoint",
            "get_autofix_capability",
            "get_autofix_incident",
            "get_environment_ip_access",
            "get_group_repository",
            "get_inherited_env_vars",
            "get_instantiation",
            "get_limits_overview",
            "get_project_compose_validation",
            "get_project_feedback",
            "get_project_group",
            "get_project_group_by_slug",
            "get_project_group_membership",
            "get_project_ip_access",
            "get_project_limits",
            "get_project_runtime_health",
            "get_runtime_health",
            "get_status_page",
            "get_usage_by_project",
            "import_project_services",
            "instantiate_template",
            "listCronMonitorRuns",
            "listCronMonitors",
            "list_autofix_incident_events",
            "list_autofix_incidents",
            "list_client_outcomes",
            "list_group_env_vars",
            "list_group_routes",
            "list_instantiations",
            "list_project_feedback",
            "list_project_groups",
            "list_status_page_domains",
            "list_status_pages",
            "list_telemetry_releases",
            "migrate_s3_service",
            "publish_status_page",
            "remove_project_from_group",
            "remove_status_page_domain",
            "renew_status_page_domain",
            "resend_email",
            "retry_instantiation",
            "run_deployment_compose_check",
            "set_group_repository",
            "set_incident_update_visibility",
            "set_repository_inheritance",
            "test_environment_ip_access",
            "test_project_ip_access",
            "unsilenceAlarm",
            "unsilenceSystemAlarm",
            "updateCronMonitor",
            "update_group_env_var",
            "update_project_feedback",
            "update_project_group",
            "update_runtime_health",
            "update_status_page",
            "verify_status_page_domain",
        ],
    ),
];

/// Allowlisted ids that are served by a plugin and so are not in the published
/// document. They are kept because the live router does serve them.
const SERVED_OUTSIDE_THE_PUBLISHED_SPEC: &[&str] = &["list_external_plugins"];

pub(super) const PUBLISHED_SPEC: &str =
    include_str!("../../../../../../apps/temps-cli/openapi.json");

/// `operation_id` -> upper-case HTTP method for every operation in the spec.
pub(super) fn published_operations() -> BTreeMap<String, String> {
    let doc: serde_json::Value =
        serde_json::from_str(PUBLISHED_SPEC).expect("committed openapi.json must parse");
    let mut operations = BTreeMap::new();
    let paths = doc["paths"].as_object().expect("spec has paths");
    for item in paths.values() {
        let Some(item) = item.as_object() else {
            continue;
        };
        for (method, operation) in item {
            if !matches!(method.as_str(), "get" | "post" | "put" | "patch" | "delete") {
                continue;
            }
            if let Some(id) = operation["operationId"].as_str() {
                operations.insert(id.to_string(), method.to_uppercase());
            }
        }
    }
    operations
}

fn excluded_ids() -> HashSet<&'static str> {
    EXCLUDED
        .iter()
        .flat_map(|(_, ids)| ids.iter().copied())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one rule this module exists for. The failure message says exactly
    /// what to do, because the person who hits it just added an endpoint.
    #[test]
    fn every_operation_is_classified() {
        let reads: HashSet<String> = ai_read_allowlist().into_iter().collect();
        let safe: HashSet<String> = ai_read_safe_posts().into_iter().collect();
        let writes: HashSet<String> = ai_write_allowlist().into_iter().collect();
        let excluded = excluded_ids();

        let mut unclassified = Vec::new();
        let mut ambiguous = Vec::new();
        for (id, method) in published_operations() {
            let homes = [
                reads.contains(&id),
                safe.contains(&id),
                writes.contains(&id),
                excluded.contains(id.as_str()),
            ]
            .iter()
            .filter(|present| **present)
            .count();
            match homes {
                0 => unclassified.push(format!("{method} {id}")),
                1 => {}
                _ => ambiguous.push(format!("{method} {id}")),
            }
        }

        assert!(
            unclassified.is_empty(),
            "{} operation(s) are not classified for the assistant: {unclassified:#?}\n\
             Add each to `ai_read_allowlist`, `ai_read_safe_posts` or `ai_write_allowlist` in \
             console.rs, or to `EXCLUDED` in ai_tool_policy.rs with the reason it is withheld. \
             Reads must not return a plaintext secret; writes must not take a credential.",
            unclassified.len()
        );
        assert!(
            ambiguous.is_empty(),
            "operation(s) are in more than one place, which hides which decision is real: \
             {ambiguous:#?}"
        );
    }

    /// An allowlisted or excluded id that is not in the spec is dead weight, and
    /// is how a typo silently drops an operation.
    #[test]
    fn no_entry_refers_to_an_operation_that_does_not_exist() {
        let operations = published_operations();
        let known: BTreeSet<&str> = SERVED_OUTSIDE_THE_PUBLISHED_SPEC.iter().copied().collect();
        let mut stale = Vec::new();
        let lists = [
            ("read", ai_read_allowlist()),
            ("safe_post", ai_read_safe_posts()),
            ("write", ai_write_allowlist()),
        ];
        for (name, ids) in &lists {
            for id in ids {
                if !operations.contains_key(id) && !known.contains(id.as_str()) {
                    stale.push(format!("{name}: {id}"));
                }
            }
        }
        for id in excluded_ids() {
            if !operations.contains_key(id) {
                stale.push(format!("excluded: {id}"));
            }
        }
        assert!(
            stale.is_empty(),
            "entries that match no operation in apps/temps-cli/openapi.json: {stale:#?}"
        );
    }

    /// The read tool is GET-only, the safe-POST list is POST-only and the write
    /// tool never takes a GET. A mismatch silently drops the operation.
    #[test]
    fn each_list_holds_the_right_kind_of_method() {
        let operations = published_operations();
        for id in ai_read_allowlist() {
            if let Some(method) = operations.get(&id) {
                assert_eq!(
                    method, "GET",
                    "`{id}` is in the read allowlist but is {method}"
                );
            }
        }
        for id in ai_read_safe_posts() {
            if let Some(method) = operations.get(&id) {
                assert_eq!(method, "POST", "`{id}` is a safe POST but is {method}");
            }
        }
        for id in ai_write_allowlist() {
            if let Some(method) = operations.get(&id) {
                assert_ne!(
                    method, "GET",
                    "`{id}` is in the write allowlist but is a GET"
                );
            }
        }
    }

    #[test]
    fn every_exclusion_has_a_reason_and_appears_once() {
        let mut seen = HashSet::new();
        for (reason, ids) in EXCLUDED {
            assert!(
                reason.trim().len() > 10,
                "an exclusion group needs a real reason, got `{reason}`"
            );
            for id in *ids {
                assert!(seen.insert(*id), "`{id}` is excluded more than once");
            }
        }
    }

    /// The controls that bound the assistant (approving or rejecting its
    /// proposals, answering its permission prompts, choosing its autonomy) must
    /// hold wherever the assistant can reach them. The replayed request carries
    /// the person's own credentials, so `permission_guard!` cannot tell the
    /// assistant from the person: only the handler-level `require_human_caller`
    /// check can. Pin that it is still present in every one of those handlers,
    /// so deleting it later fails here instead of opening a self-approval path.
    #[test]
    fn controls_that_bound_the_assistant_refuse_the_assistant() {
        const HANDLERS: &str = include_str!("../../../../../temps-ai-chat/src/handlers.rs");
        const HUMAN_ONLY: &[&str] = &[
            "confirm_pending_action",
            "reject_pending_action",
            "confirm_user_pending_action",
            "reject_user_pending_action",
            "update_permission_mode",
            "update_user_permission_mode",
            "resolve_permission",
            "resolve_user_permission",
        ];
        const SECRET_HANDLERS: &str =
            include_str!("../../../../../temps-ai-chat/src/secret_slot_handlers.rs");
        const SECRET_HUMAN_ONLY: &[&str] =
            &["list_secret_requests", "provide_secret", "discard_secret"];
        for operation in SECRET_HUMAN_ONLY {
            // The form is where the user types the key, so the assistant must
            // never be able to fill, list or discard a slot itself.
            let guard =
                format!("require_human_caller(ai_call.as_ref().map(|e|&e.0),\"{operation}\"");
            let signature = format!("pub async fn {operation}(");
            let start = SECRET_HANDLERS
                .find(&signature)
                .unwrap_or_else(|| panic!("handler {operation} not found in temps-ai-chat"));
            let end = SECRET_HANDLERS[start + signature.len()..]
                .find("pub async fn ")
                .map_or(SECRET_HANDLERS.len(), |n| start + signature.len() + n);
            assert!(
                SECRET_HANDLERS[start..end]
                    .split_whitespace()
                    .collect::<String>()
                    .contains(&guard),
                "{operation} no longer refuses assistant-issued calls: the assistant could \
                 read or fill the user's secret form"
            );
        }
        for operation in HUMAN_ONLY {
            // rustfmt may wrap the call (and add a trailing comma), so compare
            // without whitespace and stop before the closing parenthesis.
            let guard =
                format!("require_human_caller(ai_call.as_ref().map(|e|&e.0),\"{operation}\"");
            let signature = format!("pub async fn {operation}(");
            let start = HANDLERS
                .find(&signature)
                .unwrap_or_else(|| panic!("handler {operation} not found in temps-ai-chat"));
            // The guard must sit in this handler's own body, before the next one.
            let end = HANDLERS[start + signature.len()..]
                .find("pub async fn ")
                .map_or(HANDLERS.len(), |n| start + signature.len() + n);
            assert!(
                HANDLERS[start..end]
                    .split_whitespace()
                    .collect::<String>()
                    .contains(&guard),
                "{operation} no longer refuses assistant-issued calls: the assistant could \
                 approve its own proposals or widen its own autonomy"
            );
        }
    }

    /// Prints the coverage figure. Run with `--nocapture` to see it:
    /// `cargo test -p temps-cli ai_tool_policy -- --nocapture`.
    #[test]
    fn report_coverage() {
        let operations = published_operations();
        let reads: HashSet<String> = ai_read_allowlist().into_iter().collect();
        let safe: HashSet<String> = ai_read_safe_posts().into_iter().collect();
        let writes: HashSet<String> = ai_write_allowlist().into_iter().collect();
        let total = operations.len();
        let exposed = operations
            .keys()
            .filter(|id| reads.contains(*id) || safe.contains(*id) || writes.contains(*id))
            .count();
        let withheld = excluded_ids().len();
        println!(
            "assistant API coverage: {exposed}/{total} operations exposed ({:.1}%), \
             {withheld} withheld on purpose",
            100.0 * exposed as f64 / total as f64
        );
        assert!(
            exposed + withheld >= total,
            "some operations are unaccounted for"
        );
    }
}
