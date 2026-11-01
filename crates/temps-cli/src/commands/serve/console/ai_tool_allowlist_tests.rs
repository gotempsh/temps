// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The assistant's allowlists checked against the real API documents: each
//! entry resolves to an operation, and the read and write callers built from
//! them expose what the console wiring expects.

use super::*;
use temps_ai_api_tools::ReadOnlyApiIndex;
use temps_providers::handlers::metrics_handlers::MetricsApiDoc;
use temps_providers::handlers::ExternalServiceApiDoc;
use utoipa::OpenApi;

/// A throwaway admin auth context for the prepare-path tests (no request is
/// executed, so it only has to satisfy the advisory permission filter).
fn admin_auth() -> temps_auth::AuthContext {
    let now = chrono::Utc::now();
    let user = temps_entities::users::Model {
        id: 1,
        name: "tester".to_string(),
        email: "tester@internal".to_string(),
        password_hash: None,
        email_verified: true,
        email_verification_token: None,
        email_verification_expires: None,
        password_reset_token: None,
        password_reset_expires: None,
        must_change_password: false,
        deleted_at: None,
        mfa_secret: None,
        mfa_enabled: false,
        mfa_recovery_codes: None,
        oidc_subject: None,
        oidc_provider_id: None,
        created_at: now,
        updated_at: now,
    };
    temps_auth::AuthContext::new_session(user, temps_auth::permissions::Role::Admin)
}

/// End-to-end against the REAL `OtelApiDoc`: a `create_alert` proposal with
/// a malformed `detection_config` must be rejected while the model can
/// still fix it, not after a human has approved it.
///
/// This is the exact payload a live model produced three runs running
/// (`{"type": "threshold"}` instead of `{"kind": "static", …}`). Before the
/// shape check it validated cleanly, was staged as a pending action, and
/// failed with a 422 only once the user clicked Confirm — the one person in
/// the loop who had no way to know it was wrong.
#[tokio::test]
async fn malformed_detection_config_is_rejected_before_a_human_sees_it() {
    use temps_ai_api_tools::{ApiCallScope, InternalApiCaller, WritePrepareOutcome};
    use utoipa::OpenApi;

    let openapi = temps_otel::plugin::OtelApiDoc::openapi();
    // No request is executed on the prepare path, so an empty router is fine.
    let caller = InternalApiCaller::new_write_allowlisted(
        axum::Router::new(),
        &openapi,
        vec!["create_alert".to_string()],
    );
    let scope = ApiCallScope {
        auth: admin_auth(),
        project_scope: temps_ai_api_tools::ProjectSelectorScope::Allowed(vec![1]),
        origin: None,
        secrets: None,
    };

    let base = "alerts create_alert --name p95 --metric_name http.server.duration \
                --aggregation avg --window_secs 300 --for_duration_secs 300 \
                --severity critical --enabled true";

    // 1. The model's actual mistake: no `kind` discriminator.
    let outcome = caller.prepare_write_cli(
        &format!("{base} --detection_config '{{\"type\": \"threshold\", \"threshold\": 500}}'"),
        &scope,
    );
    let WritePrepareOutcome::Invalid(msg) = outcome else {
        panic!("a payload the API rejects with 422 must not be staged for approval");
    };
    assert!(msg.contains("kind"), "must name the discriminator: {msg}");
    assert!(
        msg.contains("static"),
        "must show the accepted variants — the model does not run --help: {msg}"
    );

    // 2. A quoted number and a natural-but-wrong comparator: both present,
    //    both fatal, both used to reach the human before failing.
    let outcome = caller.prepare_write_cli(
        &format!(
            "{base} --detection_config \
             '{{\"kind\": \"static\", \"comparator\": \">\", \"threshold\": \"0.5\"}}'"
        ),
        &scope,
    );
    let WritePrepareOutcome::Invalid(msg) = outcome else {
        panic!("a payload the API rejects with 422 must not be staged for approval");
    };
    assert!(
        msg.contains("gt") && msg.contains("lte"),
        "the real Comparator enum must be listed: {msg}"
    );

    // 3. The correct payload still validates and is staged.
    let outcome = caller.prepare_write_cli(
        &format!(
            "{base} --detection_config \
             '{{\"kind\": \"static\", \"comparator\": \"gt\", \"threshold\": 500}}'"
        ),
        &scope,
    );
    match outcome {
        WritePrepareOutcome::Prepared(_) => {}
        WritePrepareOutcome::Invalid(message) => {
            panic!("a well-formed static detector must still be proposable: {message}")
        }
        WritePrepareOutcome::Help(message) => {
            panic!("a well-formed static detector unexpectedly returned help: {message}")
        }
    }
}

/// `preview_alert` must be reachable from the read CLI, and its help must
/// describe a backtest the model can actually run.
///
/// Allowlisted is not the same as usable. It was discoverable all along —
/// the reason it was never called is that it rejected every detector kind
/// except `anomaly`, while every rule the model proposes is a static
/// threshold. It also advertised its optional RFC 3339 timestamps as
/// `<array>`, because a nullable type union was being read as an array.
#[tokio::test]
async fn preview_alert_is_usable_from_the_read_cli() {
    use temps_ai_api_tools::{ApiCallScope, InternalApiCaller};
    use utoipa::OpenApi;

    let openapi = temps_otel::plugin::OtelApiDoc::openapi();
    let caller = InternalApiCaller::new_allowlisted_with_safe_posts(
        axum::Router::new(),
        &openapi,
        ai_read_allowlist(),
        ai_read_safe_posts(),
    );
    let auth = admin_auth();
    let scope = ApiCallScope {
        auth: auth.clone(),
        project_scope: temps_ai_api_tools::ProjectSelectorScope::Allowed(vec![1]),
        origin: None,
        secrets: None,
    };

    // Discoverable by browsing, which is how the model finds anything.
    let section = caller.run_cli("alerts --help", &scope).await;
    assert!(
        section.contains("preview_alert"),
        "must be listed in its section: {section}"
    );

    let help = caller.run_cli("alerts preview_alert --help", &scope).await;

    // The static variant has to be offered, or the backtest is impossible
    // for the rules this flow actually proposes.
    assert!(
        help.contains("\"kind\": \"static\""),
        "static detectors must be backtestable: {help}"
    );
    assert!(
        help.contains("comparator") && help.contains("gt|gte|lt|lte"),
        "the static variant's fields must be spelled out: {help}"
    );

    // Optional timestamps are strings, not lists.
    assert!(
        help.contains("--start_time <string>"),
        "an Option<String> must not advertise itself as an array: {help}"
    );
    assert!(
        !help.contains("--end_time <array>"),
        "nullable != array: {help}"
    );
}

#[test]
fn application_workspace_drop_is_proposable() {
    let openapi = temps_ai_chat::handlers::AiChatApiDoc::openapi();
    let caller = temps_ai_api_tools::InternalApiCaller::new_write_allowlisted(
        axum::Router::new(),
        &openapi,
        ai_write_allowlist(),
    );

    assert!(caller
        .indexed_operation_ids()
        .contains(&"deploy_application_workspace_project".to_string()));
}

#[test]
fn global_notification_operations_are_available_to_workspace_chats() {
    use utoipa::OpenApi;

    let openapi = temps_notifications::NotificationProvidersApiDoc::openapi();
    let writes = temps_ai_api_tools::InternalApiCaller::new_write_allowlisted(
        axum::Router::new(),
        &openapi,
        ai_write_allowlist(),
    )
    .indexed_operation_ids();
    for operation in [
        "create_notification_provider",
        "update_notification_provider",
        "delete_notification_provider",
        "create_notification_route",
        "update_notification_route",
        "delete_notification_route",
    ] {
        assert!(
            writes.contains(&operation.to_string()),
            "missing {operation}"
        );
    }

    let reads = temps_ai_api_tools::InternalApiCaller::new_allowlisted(
        axum::Router::new(),
        &openapi,
        ai_read_allowlist(),
    )
    .indexed_operation_ids();
    assert!(reads.contains(&"list_notification_routes".to_string()));
    assert!(reads.contains(&"get_notification_route".to_string()));
    assert!(reads.contains(&"list_notification_providers".to_string()));
    assert!(reads.contains(&"get_notification_provider".to_string()));
}

/// Every safe-POST entry must resolve to a real operation, or it silently
/// does nothing — the same failure mode the read-allowlist test guards.
///
/// The safe POSTs come from several crates. Those from the OpenTelemetry
/// crate resolve against its own document; the rest are checked against
/// the published API, which is where `ai_tool_policy` pins every operation.
#[test]
fn safe_posts_resolve_against_the_real_openapi() {
    use temps_ai_api_tools::ReadOnlyApiIndex;
    use utoipa::OpenApi;

    let openapi = temps_otel::plugin::OtelApiDoc::openapi();
    let safe = ai_read_safe_posts();
    let safe_refs: Vec<&str> = safe.iter().map(String::as_str).collect();
    let index = ReadOnlyApiIndex::from_openapi_allowlist_with_safe_posts(&openapi, &[], &safe_refs);
    let published = super::ai_tool_policy::published_operations();

    for op in &safe {
        let in_otel = index.get(op).is_some();
        let published_post = published.get(op).is_some_and(|method| method == "POST");
        assert!(
            in_otel || published_post,
            "`{op}` is allowlisted as a read-only POST but does not resolve — \
             check for a typo or a renamed handler"
        );
    }
}

#[tokio::test]
async fn get_projects_resolves_and_is_discoverable_in_the_read_cli() {
    use temps_ai_api_tools::{ApiCallScope, InternalApiCaller};

    let openapi = temps_projects::handlers::ApiDoc::openapi();
    let caller =
        InternalApiCaller::new_allowlisted(axum::Router::new(), &openapi, ai_read_allowlist());
    assert!(caller
        .indexed_operation_ids()
        .contains(&"get_projects".to_string()));

    let scope = ApiCallScope {
        auth: admin_auth(),
        project_scope: temps_ai_api_tools::ProjectSelectorScope::Unrestricted,
        origin: None,
        secrets: None,
    };
    let catalog = caller.run_cli("projects --help", &scope).await;
    assert!(catalog.contains("get_projects"), "catalog: {catalog}");
}

/// Reproduces the page-country chat failure against the real analytics
/// OpenAPI document and the production AI read allowlist. This catches a
/// renamed/removed handler, missing allowlist entry, or parameter drift in
/// addition to the virtual CLI's unknown-operation recovery behavior.
#[tokio::test]
async fn page_country_analytics_recovers_through_real_api_contract() {
    use temps_ai_api_tools::{ApiCallScope, InternalApiCaller};

    let openapi = temps_analytics::handler::AnalyticsApiDoc::openapi();
    let caller =
        InternalApiCaller::new_allowlisted(axum::Router::new(), &openapi, ai_read_allowlist());
    let scope = ApiCallScope {
        auth: admin_auth(),
        project_scope: temps_ai_api_tools::ProjectSelectorScope::Allowed(vec![1]),
        origin: None,
        secrets: None,
    };

    let recovery = caller
        .run_cli("analytics get_analytics --path /managed", &scope)
        .await;
    assert!(
        recovery.contains("get_page_path_detail"),
        "page-level aggregate missing from recovery help: {recovery}"
    );

    let help = caller
        .run_cli("analytics get_page_path_detail --help", &scope)
        .await;
    for flag in ["--page_path", "--start_date", "--end_date"] {
        assert!(
            help.contains(flag),
            "real page-detail contract is missing `{flag}`: {help}"
        );
    }
}

#[test]
fn test_ai_read_allowlist_api_traffic_exposes_only_privacy_safe_operations() {
    let openapi = temps_analytics::handler::AnalyticsApiDoc::openapi();
    let allowlist = ai_read_allowlist();
    let allowlist_refs: Vec<&str> = allowlist.iter().map(String::as_str).collect();
    let index = ReadOnlyApiIndex::from_openapi_allowlist(&openapi, &allowlist_refs);

    assert!(
        index.get("get_api_timeseries").is_some(),
        "AI chat must discover the privacy-safe API traffic time series"
    );

    for operation in ["get_api_summary", "get_api_routes", "get_api_callers"] {
        assert!(
            !allowlist.iter().any(|entry| entry == operation),
            "paid or sensitive API traffic operation must not enter the AI read allowlist: `{operation}`"
        );
        assert!(
            index.get(operation).is_none(),
            "paid or sensitive API traffic operation must remain invisible to AI discovery: `{operation}`"
        );
    }
}

#[tokio::test]
async fn list_services_resolves_and_is_discoverable_in_the_read_cli() {
    use temps_ai_api_tools::{ApiCallScope, InternalApiCaller};

    let openapi = ExternalServiceApiDoc::openapi();
    let caller =
        InternalApiCaller::new_allowlisted(axum::Router::new(), &openapi, ai_read_allowlist());
    assert!(caller
        .indexed_operation_ids()
        .contains(&"list_services".to_string()));

    let scope = ApiCallScope {
        auth: admin_auth(),
        project_scope: temps_ai_api_tools::ProjectSelectorScope::Unrestricted,
        origin: None,
        secrets: None,
    };
    let catalog = caller.run_cli("external-services --help", &scope).await;
    assert!(catalog.contains("list_services"), "catalog: {catalog}");
}

/// PR #265 added `DeploymentMetricsGetRange`/`DeploymentMetricsGetLatest`/
/// `NodeMetricsGetRange` to the allowlist as bare strings — nothing
/// type-checks them against the real `operation_id`s declared via
/// `#[utoipa::path]` in `metrics_handlers.rs`. This proves each one
/// actually resolves through the same
/// `ReadOnlyApiIndex::from_openapi_allowlist` the production
/// `InternalApiCaller` uses, so a future typo or renamed handler fails a
/// test instead of silently vanishing from the AI's tool catalogue — the
/// exact failure mode PR #265 itself was fixing (see the allowlist doc
/// comment on `ai_read_allowlist`).
#[test]
fn deployment_and_node_metrics_tools_resolve_against_real_openapi() {
    let openapi = MetricsApiDoc::openapi();
    let allowlist = ai_read_allowlist();
    let allowlist_refs: Vec<&str> = allowlist.iter().map(String::as_str).collect();
    let index = ReadOnlyApiIndex::from_openapi_allowlist(&openapi, &allowlist_refs);

    for tool in [
        "DeploymentMetricsGetRange",
        "DeploymentMetricsGetLatest",
        "NodeMetricsGetRange",
        "NodeMetricsGetLatest",
    ] {
        assert!(
            index.get(tool).is_some(),
            "AI read allowlist claims to expose `{tool}` but it does not resolve \
             against the real MetricsApiDoc operation_id — check for a typo or a \
             renamed handler"
        );
    }
}

/// The companion write operation `DeploymentMetricsToggle` (a PATCH) must
/// never become callable through the read-only index, even though it is
/// declared in the same `MetricsApiDoc` — `from_openapi_allowlist` only
/// ever indexes `GET` operations, so this is a structural guarantee, not
/// just an allowlist-membership check. Matches the PR #265 description's
/// own stated intent: "DeploymentMetricsToggle ... is intentionally left
/// out, consistent with the existing pattern of excluding all write
/// operations from the read allowlist."
#[test]
fn deployment_metrics_toggle_write_op_is_never_read_callable() {
    let openapi = MetricsApiDoc::openapi();
    let allowlist = ai_read_allowlist();
    let allowlist_refs: Vec<&str> = allowlist.iter().map(String::as_str).collect();
    let index = ReadOnlyApiIndex::from_openapi_allowlist(&openapi, &allowlist_refs);

    assert!(
        index.get("DeploymentMetricsToggle").is_none(),
        "DeploymentMetricsToggle is a write (PATCH) operation and must never be \
         resolvable via the read-only AI tool allowlist"
    );
}

/// `describe_api` only ever surfaces an operation's `summary`/`description`
/// to the model — never response-body field docs — so a span's
/// `duration_ms` vs. unlabeled `attributes` units can only be explained via
/// the operation description itself. This proves the unit-guidance text
/// added to the trace/GenAI-trace handler doc comments actually survives
/// into the real compiled `OtelApiDoc` and would reach the model through
/// `describe_api`, rather than just existing as a comment nobody reads.
#[test]
fn trace_tool_descriptions_warn_about_unlabeled_attribute_units() {
    let openapi = temps_otel::plugin::OtelApiDoc::openapi();
    let index = ReadOnlyApiIndex::from_openapi(&openapi, &[]);

    for operation_id in [
        "get_trace",
        "query_traces",
        "query_genai_traces",
        "get_genai_trace",
    ] {
        let op = index
            .get(operation_id)
            .unwrap_or_else(|| panic!("{operation_id} missing from OtelApiDoc"));
        let description = op.description.as_deref().unwrap_or_default();
        assert!(
            description.contains("duration_ms") && description.contains("milliseconds"),
            "{operation_id}'s OpenAPI description must warn the model that only \
             `duration_ms` is guaranteed to be milliseconds and other numeric \
             fields carry unlabeled/different units — got: {description:?}"
        );
    }
}
