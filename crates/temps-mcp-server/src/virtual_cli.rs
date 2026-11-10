// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! External protocol adapter over the shared chat API callers.
use crate::{
    audit::{AuditContext, McpCliWriteAudit},
    error::McpError,
    protocol::{McpTool, McpToolResult},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use temps_ai_api_tools::{
    redaction::{redact_api_proposal, redact_api_result, redact_json_string, redact_value},
    ApiCallScope, ProjectSelectorScope, WritePrepareOutcome,
};
use temps_authz::AuthContext;

pub(crate) struct VirtualCli {
    pub api_tools: std::sync::Arc<temps_ai_api_tools::ApiToolsHandle>,
    pub write_api_tools: std::sync::Arc<temps_ai_api_tools::WriteApiToolsHandle>,
    pub proposals: std::sync::Arc<crate::proposal::ProposalStore>,
    pub audit_service: std::sync::Arc<dyn temps_core::AuditLogger>,
}

/// Describe the read tool and, when enabled, the proposal/confirmation tools.
pub(crate) fn tools(write: bool) -> Vec<McpTool> {
    let mut tools = vec![McpTool {
        name: "temps".into(),
        description: "Read Temps APIs. Start with `temps --help`, then drill down with section and operation --help.".into(),
        input_schema: json!({"type":"object","properties":{"command":{"type":"string"}},"required":["command"],"additionalProperties":false}),
    }];
    if write {
        tools.push(McpTool {
            name: "temps_write".into(),
            description: "Propose a Temps write; show the proposal to the human before confirm_action. Start with command `--help` on this tool and drill down. Never executes directly.".into(),
            input_schema: tools[0].input_schema.clone(),
        });
        tools.push(McpTool {
            name: "confirm_action".into(),
            description: "Execute a proposed write only after human approval. Tokens expire after 5 minutes and are single-use.".into(),
            input_schema: json!({"type":"object","properties":{"token":{"type":"string","description":"Proposal token returned by temps_write"}},"required":["token"],"additionalProperties":false}),
        });
    }
    tools
}

/// Preserve the authenticated actor and request origin during REST replay.
fn scope(auth: &AuthContext, metadata: &temps_core::RequestMetadata) -> ApiCallScope {
    ApiCallScope {
        auth: auth.clone(),
        project_scope: ProjectSelectorScope::Unrestricted,
        origin: Some(temps_core::ai_tool_call::AiToolOrigin {
            ip_address: metadata.ip_address.clone(),
            user_agent: metadata.user_agent.clone(),
        }),
        secrets: None,
    }
}

/// Retain typed failures while masking untrusted upstream response details.
fn safe_error(error: impl Into<McpError>) -> McpError {
    match error.into() {
        McpError::Cli {
            source:
                temps_ai_api_tools::ApiToolError::Upstream {
                    status,
                    detail,
                    operation_id,
                },
        } => McpError::Cli {
            source: temps_ai_api_tools::ApiToolError::Upstream {
                status,
                detail: safe(&detail),
                operation_id,
            },
        },
        other => other,
    }
}

/// Redact structured credentials and cap the displayed tool text.
fn safe(text: &str) -> String {
    cap_output(redact_json_string(text))
}

/// Bound output size without splitting a UTF-8 code point.
fn cap_output(mut text: String) -> String {
    const LIMIT: usize = 16 * 1024;
    if text.len() > LIMIT {
        let mut end = LIMIT;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str("\n[truncated]");
    }
    text
}

// The operation comes from the shared caller's result envelope, not the
// upstream response body. Only successful API data receives its schema policy.
/// Apply operation-aware redaction to the trusted caller result envelope.
fn safe_read_result(text: &str) -> String {
    let Ok(mut result) = serde_json::from_str::<Value>(text) else {
        return safe(text);
    };
    let Some(operation) = result.get("operation").and_then(Value::as_str) else {
        return safe(text);
    };
    let data = redact_api_result(operation, &result["data"]);
    result = redact_value(&result);
    result["data"] = data;
    cap_output(result.to_string())
}

/// Only server-created data enters the proposal store. The owner is checked
/// before consuming the token; confirmation always uses freshly authenticated permissions.
#[derive(Serialize, Deserialize)]
struct PendingWrite {
    user_id: i32,
    key_id: i32,
    operation_id: String,
    method: String,
    path: String,
    params: Value,
    plugin_revision: Option<String>,
}

/// Require an API-key identity for owner-bound proposals.
fn key_id(auth: &AuthContext) -> Result<i32, McpError> {
    auth.api_key_info()
        .map(|(_, id)| id)
        .ok_or(McpError::ProposalOwnerMismatch)
}

/// Run a read or prepare a write proposal without performing a mutation.
pub(crate) async fn execute(
    state: &VirtualCli,
    auth: &AuthContext,
    metadata: &temps_core::RequestMetadata,
    name: &str,
    args: &Value,
) -> Result<McpToolResult, McpError> {
    let command =
        args.get("command")
            .and_then(Value::as_str)
            .ok_or_else(|| McpError::MissingArgument {
                arg: "command".into(),
                tool: name.into(),
            })?;
    if command.len() > 16 * 1024 {
        return Err(McpError::InvalidArgument {
            arg: "command".into(),
            reason: "command exceeds 16384 bytes; use temps --help".into(),
        });
    }
    if name == "temps" {
        let caller = state.api_tools.get().ok_or(McpError::CliUnavailable)?;
        return caller
            .run_cli_result(command, &scope(auth, metadata))
            .await
            .map(|text| McpToolResult::text(safe_read_result(&text)))
            .map_err(safe_error);
    }
    let caller = state
        .write_api_tools
        .get()
        .ok_or(McpError::CliUnavailable)?;
    match caller.prepare_write_cli_without_secret_input(command, &scope(auth, metadata)) {
        WritePrepareOutcome::Help(text) => Ok(McpToolResult::text(safe(&text))),
        WritePrepareOutcome::Invalid(detail) => Err(McpError::Cli {
            source: temps_ai_api_tools::ApiToolError::InvalidCommand {
                detail: safe(&detail),
            },
        }),
        WritePrepareOutcome::Prepared(prepared) => {
            let review = json!({"operation": prepared.operation_id, "description": safe(&prepared.summary),
                "parameters": redact_api_proposal(&prepared.operation_id, &prepared.params)});
            let pending = PendingWrite {
                user_id: auth.user_id(),
                key_id: key_id(auth)?,
                operation_id: prepared.operation_id,
                method: prepared.method,
                path: prepared.path,
                params: prepared.params,
                plugin_revision: prepared.plugin_revision,
            };
            let token = state
                .proposals
                .create("temps_write".into(), serde_json::to_value(pending)?);
            Ok(McpToolResult::text_with_proposal_token(format!("{}\nAsk the human to approve, then call confirm_action. Token expires in 5 minutes.", cap_output(review.to_string())), token))
        }
    }
}

/// Consume an owner-bound proposal, replay with current auth, and audit the outcome.
pub(crate) async fn confirm(
    state: &VirtualCli,
    auth: &AuthContext,
    metadata: &temps_core::RequestMetadata,
    token: &str,
    arguments: Value,
) -> Result<McpToolResult, McpError> {
    let pending: PendingWrite = serde_json::from_value(arguments)?;
    if pending.user_id != auth.user_id() || pending.key_id != key_id(auth)? {
        return Err(McpError::ProposalOwnerMismatch);
    }
    let caller = state
        .write_api_tools
        .get()
        .ok_or(McpError::CliUnavailable)?;
    state.proposals.take(token).map_err(safe_error)?;
    let result = caller
        .execute_approved_write(
            &pending.operation_id,
            pending.params,
            &scope(auth, metadata),
            pending.plugin_revision.as_deref(),
        )
        .await;
    let event = McpCliWriteAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        operation_id: pending.operation_id,
        method: pending.method,
        path: pending.path,
        succeeded: result.is_ok(),
    };
    if let Err(error) = state.audit_service.create_audit_log(&event).await {
        tracing::error!(operation_id = %event.operation_id, %error, "Failed to audit MCP CLI write");
    }
    result.map(|response| McpToolResult::text(cap_output(json!({"status":response.status,"data":redact_api_result(&event.operation_id, &response.body),"truncated":response.truncated}).to_string()))).map_err(safe_error)
}

#[cfg(test)]
mod tests {
    use super::*;
    // Most tests use the same fixture origin; origin-specific tests pass their
    // own metadata directly through the production adapter.
    async fn execute(
        state: &VirtualCli,
        auth: &AuthContext,
        name: &str,
        args: &Value,
    ) -> Result<McpToolResult, McpError> {
        super::execute(state, auth, &metadata(), name, args).await
    }

    #[test]
    fn listing_keeps_virtual_tools_independent_of_legacy_groups() {
        for groups in [
            vec![],
            vec!["platform".into()],
            vec!["networking".into()],
            vec!["data".into()],
        ] {
            for write in [false, true] {
                let tools = crate::registry::collect_tools(&groups, write);
                assert!(tools.iter().any(|tool| tool.name == "temps"));
                assert_eq!(tools.iter().any(|tool| tool.name == "temps_write"), write);
                if write {
                    assert!(tools
                        .iter()
                        .find(|tool| tool.name == "temps_write")
                        .unwrap()
                        .description
                        .contains("command `--help` on this tool"));
                }
                assert_eq!(
                    tools
                        .iter()
                        .filter(|tool| tool.name == "confirm_action")
                        .count(),
                    usize::from(write)
                );
            }
        }
    }
    #[test]
    fn output_masks_credentials_and_caps_unicode() {
        assert!(!safe(r#"{"password":"private-fixture"}"#).contains("private-fixture"));
        assert!(safe(&"é".repeat(20000)).len() < 16500);
    }
    fn make_user(id: i32) -> temps_entities::users::Model {
        let now = chrono::Utc::now();
        temps_entities::users::Model {
            id,
            name: "Test User".to_string(),
            email: "test@example.com".to_string(),
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
        }
    }

    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    };
    use temps_authz::permissions::Permission;
    #[derive(Default)]
    struct Audit(Mutex<Vec<String>>);
    #[async_trait::async_trait]
    impl temps_core::AuditLogger for Audit {
        async fn create_audit_log(
            &self,
            op: &dyn temps_core::AuditOperation,
        ) -> temps_core::AuditResult<()> {
            self.0.lock().unwrap().push(op.serialize()?);
            Ok(())
        }
    }
    fn auth(key: i32, write: bool) -> AuthContext {
        let mut perms = vec![Permission::ProjectsRead];
        if write {
            perms.push(Permission::ProjectsWrite);
        }
        AuthContext::new_api_key(make_user(7), None, Some(perms), "fixture".into(), key)
    }
    fn metadata() -> temps_core::RequestMetadata {
        temps_core::RequestMetadata {
            ip_address: "127.0.0.1".into(),
            user_agent: "test".into(),
            headers: Default::default(),
            visitor_id_cookie: None,
            session_id_cookie: None,
            base_url: "http://localhost".into(),
            scheme: "http".into(),
            host: "localhost".into(),
            is_secure: false,
        }
    }
    fn fixture() -> (VirtualCli, Arc<AtomicUsize>, Arc<Audit>) {
        use axum::{routing::get, Json};
        let writes = Arc::new(AtomicUsize::new(0));
        let counter = writes.clone();
        let router = axum::Router::new().route("/projects", get(|temps_authz::RequireAuth(auth): temps_authz::RequireAuth| async move {
            temps_authz::permission_guard!(auth, ProjectsRead);
            Ok::<_, temps_core::problemdetails::Problem>(Json(json!({"user":auth.user_id(),"actor_id":auth.api_key_info().map(|(_,id)|id)})))
        }).post(move |temps_authz::RequireAuth(auth): temps_authz::RequireAuth| {
            let counter = counter.clone();
            async move {
                temps_authz::permission_guard!(auth, ProjectsWrite);
                counter.fetch_add(1, Ordering::SeqCst);
                Ok::<_, temps_core::problemdetails::Problem>(Json(json!({"done":true})))
            }
        }));
        let api: utoipa::openapi::OpenApi = serde_json::from_value(json!({"openapi":"3.1.0","info":{"title":"Fixture","version":"1"},"paths":{"/projects":{
            "get":{"operationId":"read_fixture","tags":["Projects"],"responses":{"200":{"description":"ok"}}},
            "post":{"operationId":"write_fixture","tags":["Projects"],"requestBody":{"content":{"application/json":{"schema":{"type":"object","properties":{"secret_key":{"type":"string","x-temps-secret":true}}}}}},"responses":{"200":{"description":"ok"}}}
        }}})).unwrap();
        let read = Arc::new(temps_ai_api_tools::ApiToolsHandle::new());
        read.set(temps_ai_api_tools::InternalApiCaller::new_allowlisted(
            router.clone(),
            &api,
            vec!["read_fixture".into()],
        ));
        let write = Arc::new(temps_ai_api_tools::WriteApiToolsHandle::new());
        write.set(
            temps_ai_api_tools::InternalApiCaller::new_write_allowlisted(
                router,
                &api,
                vec!["write_fixture".into()],
            ),
        );
        let audit = Arc::new(Audit::default());
        (
            VirtualCli {
                api_tools: read,
                write_api_tools: write,
                proposals: Arc::new(crate::proposal::ProposalStore::new()),
                audit_service: audit.clone(),
            },
            writes,
            audit,
        )
    }
    #[tokio::test]
    async fn flag_proposals_show_the_actual_configuration_before_confirmation() {
        for operation in ["set_flag_environment", "create_flag"] {
            let api = serde_json::from_value(json!({"openapi":"3.1.0","info":{"title":"Fixture","version":"1"},
                "paths":{"/flags":{"post":{"operationId":operation,"tags":["Flags"],"requestBody":{"content":{"application/json":{
                    "schema":{"type":"object","properties":{"key":{"type":"string"},"value":{"type":"boolean"},"value_type":{"type":"string"},"default_value":{"type":"boolean"},"token":{"type":"string"}}}
                }}},"responses":{"200":{"description":"ok"}}}}}})).unwrap();
            let (state, writes, _) = fixture();
            state.write_api_tools.set(
                temps_ai_api_tools::InternalApiCaller::new_write_allowlisted(
                    axum::Router::new(),
                    &api,
                    vec![operation.into()],
                ),
            );
            for value in ["true", "false"] {
                let command = if operation == "create_flag" {
                    format!("{operation} --key checkout.v2 --value_type boolean --default_value {value} --token fixture-private")
                } else {
                    format!("{operation} --key checkout.v2 --value {value} --token fixture-private")
                };
                let proposal = execute(
                    &state,
                    &auth(3, true),
                    "temps_write",
                    &json!({"command":command}),
                )
                .await
                .unwrap();
                let preview: Value =
                    serde_json::from_str(proposal.content[0].text.lines().next().unwrap()).unwrap();
                let field = if operation == "create_flag" {
                    "default_value"
                } else {
                    "value"
                };
                let expected: Value = serde_json::from_str(value).unwrap();
                assert_eq!(preview["parameters"]["key"], "checkout.v2");
                assert_eq!(preview["parameters"][field], expected);
                assert_eq!(preview["parameters"]["token"], "***");
                if operation == "create_flag" {
                    assert_eq!(preview["parameters"]["value_type"], "boolean");
                }
                let stored = state
                    .proposals
                    .peek(proposal.proposal_token.as_deref().unwrap())
                    .unwrap();
                assert_eq!(stored.arguments["params"][field], expected);
                assert!(!proposal.content[0].text.contains("fixture-private"));
                assert_eq!(writes.load(Ordering::SeqCst), 0);
            }
        }
    }

    #[tokio::test]
    async fn invalid_array_items_do_not_echo_input() {
        let api = serde_json::from_value(json!({"openapi":"3.1.0","info":{"title":"Fixture","version":"1"},
            "paths":{"/variables":{"post":{"operationId":"create_environment_variable","requestBody":{"content":{"application/json":{"schema":{"type":"object","properties":{"environment_ids":{"type":"array","items":{"type":"integer"}}}}}}},"responses":{"200":{"description":"ok"}}}}}})).unwrap();
        let (state, writes, _) = fixture();
        state.write_api_tools.set(
            temps_ai_api_tools::InternalApiCaller::new_write_allowlisted(
                axum::Router::new(),
                &api,
                vec!["create_environment_variable".into()],
            ),
        );
        for input in [
            "arbitrary-private-input",
            "postgres://fixture:private@example.test/db",
        ] {
            let error = execute(&state, &auth(3,true), "temps_write", &json!({"command":format!("create_environment_variable --environment_ids 1 {input}")})).await.unwrap_err().to_string();
            assert!(error.contains("environment_ids"));
            assert!(error.contains("item 2"));
            assert!(!error.contains(input));
            assert!(!error.contains("private"));
        }
        assert_eq!(writes.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn environment_names_remain_visible_through_mcp() {
        for operation in [
            "get_environment_variables",
            "get_resolved_environment_variables",
        ] {
            let router = axum::Router::new().route("/variables", axum::routing::get(|| async {
                axum::Json(json!([{"key":"DATABASE_URL","value":"fixture-private","value_preview":"fixture-private"}]))
            }));
            let api = serde_json::from_value(json!({"openapi":"3.1.0","info":{"title":"Fixture","version":"1"},
                "paths":{"/variables":{"get":{"operationId":operation,"responses":{"200":{"description":"ok"}}}}}})).unwrap();
            let (state, _, _) = fixture();
            state
                .api_tools
                .set(temps_ai_api_tools::InternalApiCaller::new_allowlisted(
                    router,
                    &api,
                    vec![operation.into()],
                ));
            let result = execute(
                &state,
                &auth(3, false),
                "temps",
                &json!({"command":operation}),
            )
            .await
            .unwrap();
            let result: Value = serde_json::from_str(&result.content[0].text).unwrap();
            assert_eq!(result["data"][0]["key"], "DATABASE_URL");
            assert_eq!(result["data"][0]["value"], "***");
            assert_eq!(result["data"][0]["value_preview"], "***");
        }
    }

    #[tokio::test]
    async fn concurrent_confirmations_execute_once() {
        let (state, writes, _) = fixture();
        let proposal = execute(
            &state,
            &auth(3, true),
            "temps_write",
            &json!({"command":"write_fixture"}),
        )
        .await
        .unwrap();
        let token = proposal.proposal_token.unwrap();
        let barrier = std::sync::Barrier::new(2);
        let run = || {
            let args = state.proposals.peek(&token).unwrap().arguments;
            barrier.wait();
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            match runtime.block_on(confirm(&state, &auth(3, true), &metadata(), &token, args)) {
                Ok(_) => 1,
                Err(McpError::ProposalNotFound) => 0,
                Err(error) => panic!("unexpected confirmation error: {error}"),
            }
        };
        let count = std::thread::scope(|scope| {
            let a = scope.spawn(run);
            let b = scope.spawn(run);
            a.join().unwrap() + b.join().unwrap()
        });
        assert_eq!(count, 1);
        assert_eq!(writes.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn metric_query_measurements_survive_mcp_router_replay() {
        let data = json!({"data":[
            {"bucket":"2026-01-01T00:00:00Z","avg_value":1.25,"min_value":-0.5,"max_value":2.5,"value":1.25,"count":2,"series_key":[["http.method","GET"]]},
            {"bucket":"2026-01-01T00:00:00Z","avg_value":0.0,"min_value":0.0,"max_value":0.0,"value":0.0,"count":1,"series_key":[["http.method","POST"]]}
        ],"count":2,"token":"fixture-private"});
        let payload = data.clone();
        let router = axum::Router::new().route(
            "/metrics",
            axum::routing::get(move || async move { axum::Json(payload) }),
        );
        let api = serde_json::from_value(json!({"openapi":"3.1.0","info":{"title":"Fixture","version":"1"},
            "paths":{"/metrics":{"get":{"operationId":"query_metrics","responses":{"200":{"description":"ok"}}}}}})).unwrap();
        let (state, _, _) = fixture();
        state
            .api_tools
            .set(temps_ai_api_tools::InternalApiCaller::new_allowlisted(
                router,
                &api,
                vec!["query_metrics".into()],
            ));
        let result = execute(
            &state,
            &auth(3, false),
            "temps",
            &json!({"command":"query_metrics"}),
        )
        .await
        .unwrap();
        let result: Value = serde_json::from_str(&result.content[0].text).unwrap();
        assert_eq!(result["data"]["data"], data["data"]);
        assert_eq!(result["data"]["count"], 2);
        assert_eq!(result["data"]["token"], "***");
    }

    #[tokio::test]
    async fn metric_label_discovery_survives_mcp_response_redaction() {
        for (operation, field, values) in [
            ("list_metric_label_keys", "keys", json!(["http.method"])),
            ("list_metric_label_values", "values", json!(["GET", "POST"])),
        ] {
            let payload = json!({field: values.clone(), "token": "fixture-private"});
            let router = axum::Router::new().route(
                "/labels",
                axum::routing::get(move || async move { axum::Json(payload) }),
            );
            let api = serde_json::from_value(json!({"openapi":"3.1.0","info":{"title":"Fixture","version":"1"},
                "paths":{"/labels":{"get":{"operationId":operation,"responses":{"200":{"description":"ok"}}}}}})).unwrap();
            let (state, _, _) = fixture();
            state
                .api_tools
                .set(temps_ai_api_tools::InternalApiCaller::new_allowlisted(
                    router,
                    &api,
                    vec![operation.into()],
                ));
            let result = execute(
                &state,
                &auth(3, false),
                "temps",
                &json!({"command":operation}),
            )
            .await
            .unwrap();
            let result: Value = serde_json::from_str(&result.content[0].text).unwrap();
            assert_eq!(result["data"][field], values);
            assert_eq!(result["data"]["token"], "***");
        }
    }

    #[tokio::test]
    async fn replay_preserves_read_origin_and_current_confirmation_origin() {
        async fn echo(
            axum::Extension(meta): axum::Extension<temps_core::RequestMetadata>,
        ) -> axum::Json<Value> {
            axum::Json(json!({"ip":meta.ip_address,"agent":meta.user_agent}))
        }
        let crypto = Arc::new(temps_core::CookieCrypto::from_bytes(&[7; 32]));
        let router = axum::Router::new()
            .route("/origin", axum::routing::get(echo).post(echo))
            .layer(axum::middleware::from_fn(move |req, next| {
                let crypto = crypto.clone();
                async move { temps_core::request_metadata_middleware(crypto, req, next).await }
            }));
        let api = serde_json::from_value(json!({"openapi":"3.1.0","info":{"title":"Fixture","version":"1"},
            "paths":{"/origin":{"get":{"operationId":"read_origin","responses":{"200":{"description":"ok"}}},
            "post":{"operationId":"write_origin","responses":{"200":{"description":"ok"}}}}}})).unwrap();
        let (state, _, audit) = fixture();
        state
            .api_tools
            .set(temps_ai_api_tools::InternalApiCaller::new_allowlisted(
                router.clone(),
                &api,
                vec!["read_origin".into()],
            ));
        state.write_api_tools.set(
            temps_ai_api_tools::InternalApiCaller::new_write_allowlisted(
                router,
                &api,
                vec!["write_origin".into()],
            ),
        );
        let mut origin = metadata();
        origin.ip_address = "192.0.2.10".into();
        origin.user_agent = "fixture-mcp-client".into();
        let result = super::execute(
            &state,
            &auth(3, true),
            &origin,
            "temps",
            &json!({"command":"read_origin"}),
        )
        .await
        .unwrap();
        assert!(result.content[0].text.contains("192.0.2.10"));
        assert!(result.content[0].text.contains("fixture-mcp-client"));
        let proposal = super::execute(
            &state,
            &auth(3, true),
            &origin,
            "temps_write",
            &json!({"command":"write_origin"}),
        )
        .await
        .unwrap();
        let token = proposal.proposal_token.unwrap();
        let args = state.proposals.peek(&token).unwrap().arguments;
        origin.ip_address = "192.0.2.20".into();
        origin.user_agent = "fixture-confirm-client".into();
        let result = confirm(&state, &auth(3, true), &origin, &token, args)
            .await
            .unwrap();
        assert!(result.content[0].text.contains("192.0.2.20"));
        assert!(result.content[0].text.contains("fixture-confirm-client"));
        assert!(result.content[0].text.contains("via Temps AI assistant"));
        assert!(!result.content[0].text.contains("192.0.2.10"));
        assert!(audit.0.lock().unwrap()[0].contains("192.0.2.20"));
    }

    #[tokio::test]
    async fn feature_flag_reads_preserve_keys_types_and_values_through_router_replay() {
        for operation in ["list_flags", "get_flag", "get_flag_snapshot"] {
            let flag = json!({"key":"checkout.v2","value_type":"boolean","default_value":false,
                "environment_value":true,"environments":[{"environment_id":1,"value":true}],
                "token":"fixture-secret"});
            let payload = if operation == "get_flag" {
                flag
            } else {
                json!({"flags":[flag]})
            };
            let router = axum::Router::new().route(
                "/flags",
                axum::routing::get(
                    move |temps_authz::RequireAuth(auth): temps_authz::RequireAuth| async move {
                        temps_authz::permission_guard!(auth, ProjectsRead);
                        Ok::<_, temps_core::problemdetails::Problem>(axum::Json(payload))
                    },
                ),
            );
            let api = serde_json::from_value(json!({"openapi":"3.1.0","info":{"title":"Fixture","version":"1"},
                "paths":{"/flags":{"get":{"operationId":operation,"tags":["Flags"],"responses":{"200":{"description":"ok"}}}}}})).unwrap();
            let (state, _, _) = fixture();
            state
                .api_tools
                .set(temps_ai_api_tools::InternalApiCaller::new_allowlisted(
                    router,
                    &api,
                    vec![operation.into()],
                ));
            let result = execute(
                &state,
                &auth(3, false),
                "temps",
                &json!({"command":operation}),
            )
            .await
            .unwrap();
            let result: Value = serde_json::from_str(&result.content[0].text).unwrap();
            let flag = if operation == "get_flag" {
                &result["data"]
            } else {
                &result["data"]["flags"][0]
            };
            assert_eq!(flag["key"], "checkout.v2");
            assert_eq!(flag["value_type"], "boolean");
            assert_eq!(flag["default_value"], false);
            assert_eq!(flag["environment_value"], true);
            assert_eq!(flag["environments"][0]["value"], true);
            assert_eq!(flag["token"], "***");
        }
        let large = json!({"operation":"get_flag","data":{"key":"fixture","default_value":"é".repeat(20000)}});
        assert!(safe_read_result(&large.to_string()).len() < 16500);
    }

    #[tokio::test]
    async fn upstream_server_failure_is_an_internal_rpc_error_after_replay() {
        for status in [500, 503] {
            let router = axum::Router::new().route(
                "/projects",
                axum::routing::get(move || async move {
                    (
                        axum::http::StatusCode::from_u16(status).unwrap(),
                        "fixture outage",
                    )
                }),
            );
            let api = serde_json::from_value(json!({"openapi":"3.1.0","info":{"title":"Fixture","version":"1"},
                "paths":{"/projects":{"get":{"operationId":"read_fixture","tags":["Projects"],"responses":{"200":{"description":"ok"}}}}}})).unwrap();
            let (state, _, _) = fixture();
            state
                .api_tools
                .set(temps_ai_api_tools::InternalApiCaller::new_allowlisted(
                    router,
                    &api,
                    vec!["read_fixture".into()],
                ));
            let error = execute(
                &state,
                &auth(3, false),
                "temps",
                &json!({"command":"read_fixture"}),
            )
            .await
            .unwrap_err();
            assert_eq!(error.rpc_code(), crate::protocol::INTERNAL_ERROR);
            assert!(error.to_string().contains("read_fixture"));
            assert!(error.to_string().contains(&status.to_string()));
        }
    }

    #[tokio::test]
    async fn help_read_and_invalid_commands_use_shared_caller() {
        let (state, _, _) = fixture();
        for command in ["temps --help", "projects --help"] {
            let result = execute(
                &state,
                &auth(3, false),
                "temps",
                &json!({"command":command}),
            )
            .await
            .unwrap();
            assert!(
                result.content[0].text.contains("projects")
                    || result.content[0].text.contains("read_fixture")
            );
        }
        let result = execute(
            &state,
            &auth(3, false),
            "temps",
            &json!({"command":"read_fixture"}),
        )
        .await
        .unwrap();
        assert!(result.content[0].text.contains("7"));
        assert!(result.content[0].text.contains("3") || result.content[0].text.contains("***"));
        let error = execute(
            &state,
            &auth(3, false),
            "temps",
            &json!({"command":"unknown"}),
        )
        .await
        .unwrap_err();
        assert_eq!(error.rpc_code(), crate::protocol::INVALID_PARAMS);
        assert!(error.to_string().contains("--help"));
    }
    #[tokio::test]
    async fn credential_input_is_rejected_before_proposing_without_a_secret_flow() {
        let (state, writes, audit) = fixture();
        for value in [
            "fixture-private-value",
            "secret_ref:v1:0123456789abcdef0123456789abcdef",
        ] {
            let error = execute(
                &state,
                &auth(3, true),
                "temps_write",
                &json!({"command":format!("write_fixture --secret_key {value}")}),
            )
            .await
            .unwrap_err();
            assert_eq!(error.rpc_code(), crate::protocol::INVALID_PARAMS);
            let message = error.to_string();
            assert!(message.contains("write_fixture"));
            assert!(message.contains("console or REST API"));
            assert!(!message.contains(value));
        }
        assert_eq!(writes.load(Ordering::SeqCst), 0);
        assert!(audit.0.lock().unwrap().is_empty());
        // An omitted optional credential still permits an ordinary proposal.
        let proposal = execute(
            &state,
            &auth(3, true),
            "temps_write",
            &json!({"command":"write_fixture"}),
        )
        .await
        .unwrap();
        assert!(proposal.proposal_token.is_some());
    }

    #[tokio::test]
    async fn proposal_requires_same_key_expires_and_executes_once_with_audit() {
        let (state, writes, audit) = fixture();
        let actor = auth(3, true);
        let proposal = execute(
            &state,
            &actor,
            "temps_write",
            &json!({"command":"write_fixture"}),
        )
        .await
        .unwrap();
        assert_eq!(writes.load(Ordering::SeqCst), 0);
        let token = proposal.proposal_token.unwrap();
        let args = state.proposals.peek(&token).unwrap().arguments;
        assert!(matches!(
            confirm(&state, &auth(4, true), &metadata(), &token, args.clone()).await,
            Err(McpError::ProposalOwnerMismatch)
        ));
        confirm(&state, &actor, &metadata(), &token, args.clone())
            .await
            .unwrap();
        assert_eq!(writes.load(Ordering::SeqCst), 1);
        assert!(matches!(
            confirm(&state, &actor, &metadata(), &token, args).await,
            Err(McpError::ProposalNotFound)
        ));
        {
            let audit = audit.0.lock().unwrap();
            assert_eq!(audit.len(), 1);
            assert!(audit[0].contains("write_fixture"));
            assert!(audit[0].contains("true"));
        }
        let proposal = execute(
            &state,
            &actor,
            "temps_write",
            &json!({"command":"write_fixture"}),
        )
        .await
        .unwrap();
        let token = proposal.proposal_token.unwrap();
        let args = state.proposals.peek(&token).unwrap().arguments;
        state.proposals.backdate(
            &token,
            crate::proposal::PROPOSAL_TTL + std::time::Duration::from_secs(1),
        );
        assert!(matches!(
            confirm(&state, &actor, &metadata(), &token, args).await,
            Err(McpError::ProposalExpired)
        ));
        assert_eq!(writes.load(Ordering::SeqCst), 1);
    }
    #[tokio::test]
    async fn confirmation_replays_current_key_permissions_without_elevation() {
        let (state, writes, audit) = fixture();
        let proposal = execute(
            &state,
            &auth(3, true),
            "temps_write",
            &json!({"command":"write_fixture"}),
        )
        .await
        .unwrap();
        let token = proposal.proposal_token.unwrap();
        let args = state.proposals.peek(&token).unwrap().arguments;
        let denied = confirm(&state, &auth(3, false), &metadata(), &token, args)
            .await
            .unwrap_err();
        assert_eq!(denied.rpc_code(), -32006);
        assert!(denied.to_string().contains("write_fixture"));
        assert!(denied.to_string().contains("projects:write"));
        assert_eq!(writes.load(Ordering::SeqCst), 0);
        assert!(audit.0.lock().unwrap()[0].contains("false"));
    }
    #[tokio::test]
    async fn api_key_project_access_is_enforced_by_replayed_rest_guard() {
        struct Checker;
        #[async_trait::async_trait]
        impl temps_core::project_access::ProjectAccessChecker for Checker {
            async fn user_can_access_project(
                &self,
                user_id: i32,
                project_id: i32,
            ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
                Ok(user_id == 7 && project_id == 11)
            }
        }
        let router = axum::Router::new().route(
            "/projects/{project_id}",
            axum::routing::get(
                |temps_authz::RequireAuth(auth): temps_authz::RequireAuth,
                 axum::extract::Path(project_id): axum::extract::Path<i32>| async move {
                    temps_authz::permission_guard!(auth, ProjectsRead);
                    let checker: Option<Arc<dyn temps_core::project_access::ProjectAccessChecker>> =
                        Some(Arc::new(Checker));
                    temps_authz::project_access_guard!(auth, project_id, checker);
                    Ok::<_, temps_core::problemdetails::Problem>(axum::Json(
                        json!({"project_id":project_id}),
                    ))
                },
            ),
        );
        let api = serde_json::from_value(json!({"openapi":"3.1.0","info":{"title":"Fixture","version":"1"},"paths":{"/projects/{project_id}":{"get":{"operationId":"get_fixture","tags":["Projects"],"parameters":[{"name":"project_id","in":"path","required":true,"schema":{"type":"integer"}}],"responses":{"200":{"description":"ok"}}}}}})).unwrap();
        let (state, _, _) = fixture();
        state
            .api_tools
            .set(temps_ai_api_tools::InternalApiCaller::new_allowlisted(
                router,
                &api,
                vec!["get_fixture".into()],
            ));
        let result = execute(
            &state,
            &auth(3, false),
            "temps",
            &json!({"command":"get_fixture --project_id 11"}),
        )
        .await
        .unwrap();
        assert!(result.content[0].text.contains("11"));
        let error = execute(
            &state,
            &auth(3, false),
            "temps",
            &json!({"command":"get_fixture --project_id 12"}),
        )
        .await
        .unwrap_err();
        assert_eq!(error.rpc_code(), -32006);
        assert!(error.to_string().contains("get_fixture"));
        assert!(error.to_string().contains("Project Access Denied"));
    }
}
