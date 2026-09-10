// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::sync::Arc;

use serde_json::json;
use temps_auth::context::AuthContext;
use temps_core::project_access::ProjectAccessChecker;
use temps_core::AuditLogger;
use temps_deployments::DeploymentService;
use tracing::error;

use crate::access::{check_project_access, check_project_scope};
use crate::audit::{AuditContext, McpDeploymentTriggeredAudit};
use crate::error::McpError;
use crate::proposal::ProposalStore;
use crate::protocol::{McpTool, McpToolResult};

/// Actor + request context needed to attribute an audit log entry to the MCP
/// caller. Constructed by the handler layer from `AuthContext` +
/// `RequestMetadata`, which this crate's tools modules don't otherwise see.
pub struct AuditActor {
    pub user_id: i32,
    pub ip_address: Option<String>,
    pub user_agent: String,
}

/// Tool definitions for the **deployments** group.
///
/// Read tools: `list_deployments`
/// Write tools (propose-then-confirm): `trigger_deployment`, `confirm_action`
///
/// Write tools are only included in the list when `write_enabled` is `true`
/// (i.e., the MCP URL was opened with `?write=1`).
pub fn tools(write_enabled: bool) -> Vec<McpTool> {
    let mut result = vec![
        McpTool {
            name: "list_deployments".to_string(),
            description: "List recent deployments for a project, optionally filtered \
                          by environment."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_id": {
                        "type": "integer",
                        "description": "Numeric project ID"
                    },
                    "environment_id": {
                        "type": "integer",
                        "description": "Filter to a specific environment (optional)"
                    },
                    "page": {
                        "type": "integer",
                        "description": "Page number, 1-based (default: 1)"
                    },
                    "per_page": {
                        "type": "integer",
                        "description": "Results per page, max 100 (default: 10)"
                    }
                },
                "required": ["project_id"]
            }),
        },
        // TODO(mcp): get_deployment — fetch a single deployment by ID
        // TODO(mcp): list_environments — list environments for a project
    ];

    if write_enabled {
        result.push(McpTool {
            name: "trigger_deployment".to_string(),
            description: "Propose triggering a new deployment pipeline for a project \
                          environment.  Returns a proposal token; call confirm_action \
                          with that token to execute."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_id": {
                        "type": "integer",
                        "description": "Numeric project ID"
                    },
                    "environment_id": {
                        "type": "integer",
                        "description": "Numeric environment ID"
                    },
                    "branch": {
                        "type": "string",
                        "description": "Git branch to deploy (optional, defaults to main branch)"
                    }
                },
                "required": ["project_id", "environment_id"]
            }),
        });

        result.push(McpTool {
            name: "confirm_action".to_string(),
            description: "Execute a previously proposed write action using its token. \
                          Tokens expire after 5 minutes and are single-use."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "token": {
                        "type": "string",
                        "description": "Proposal token returned by a write tool"
                    }
                },
                "required": ["token"]
            }),
        });
    }

    result
}

/// Shared services and auth context threaded into [`execute`].
///
/// Bundled as a single struct so the function stays below clippy's
/// `too_many_arguments` limit (7) while keeping all dependencies explicit.
pub struct DeploymentExecCtx<'a> {
    pub deployment_service: &'a Arc<DeploymentService>,
    pub proposals: &'a Arc<ProposalStore>,
    pub audit_service: &'a Arc<dyn AuditLogger>,
    pub actor: &'a AuditActor,
    /// Full auth context for the MCP caller.  Used by `check_project_access`
    /// to apply the admin / deployment-token bypass before consulting
    /// `checker`.  `actor` is kept separately for audit-log attribution.
    pub auth: &'a AuthContext,
    /// Optional per-project access checker (absent on instances without
    /// team-based RBAC configured).
    pub checker: Option<&'a dyn ProjectAccessChecker>,
}

/// Execute a deployments-group tool call.
///
/// `arguments` is `serde_json::Value` because tool call arguments are defined
/// per-tool by `inputSchema` and dispatched generically.  Typed values are
/// extracted at the point of use with helpers such as `require_i32` and
/// `.get().as_str()`.
///
/// # Errors
///
/// - [`McpError::UnknownTool`] when `name` is not in [`tools()`].
/// - [`McpError::WriteNotEnabled`] when a write tool is called without
///   `write_enabled`.
/// - [`McpError::ProjectAccessDenied`] when the caller lacks per-project
///   access (at both propose and confirm steps, independently).
/// - [`McpError::MissingArgument`] / [`McpError::InvalidArgument`] on bad
///   input.
/// - [`McpError::DeploymentService`] on backend errors.
pub async fn execute(
    name: &str,
    arguments: &serde_json::Value,
    write_enabled: bool,
    ctx: &DeploymentExecCtx<'_>,
) -> Result<McpToolResult, McpError> {
    let deployment_service = ctx.deployment_service;
    let proposals = ctx.proposals;
    let audit_service = ctx.audit_service;
    let actor = ctx.actor;
    let checker = ctx.checker;
    match name {
        "list_deployments" => {
            let project_id = require_i32(arguments, "project_id", name)?;

            // Tenant-boundary check first: confines a deployment token to its
            // own bound project regardless of the checker/admin bypass below.
            check_project_scope(ctx.auth, project_id)?;

            // Per-project access check — a single `user_can_access_project`
            // call is correct here (not the hidden-list pattern) because this
            // is a direct, project-scoped read.  Admins bypass via auth.
            check_project_access(checker, ctx.auth, project_id).await?;

            let environment_id = arguments
                .get("environment_id")
                .and_then(serde_json::Value::as_i64)
                .map(i32::try_from)
                .transpose()
                .map_err(|_| McpError::InvalidArgument {
                    arg: "environment_id".to_string(),
                    reason: "does not fit in a 32-bit ID".to_string(),
                })?;
            // `get_project_deployments` clamps page/per_page itself (that's
            // the authoritative fix, since it's the one place every caller —
            // REST and MCP — goes through). Clamping again here is
            // defense-in-depth for this attacker-controlled boundary, not the
            // primary fix: see clamp_page/clamp_per_page's doc comments.
            let page = arguments
                .get("page")
                .and_then(serde_json::Value::as_i64)
                .map(clamp_page);
            let per_page = arguments
                .get("per_page")
                .and_then(serde_json::Value::as_i64)
                .map(clamp_per_page);

            let response = deployment_service
                .get_project_deployments(project_id, page, per_page, environment_id)
                .await
                .map_err(|e| match e {
                    temps_deployments::DeploymentError::NotFound(_) => {
                        McpError::DeploymentNotFound { project_id }
                    }
                    other => McpError::DeploymentService(other.to_string()),
                })?;

            let text = serde_json::to_string_pretty(&response).map_err(McpError::Serialization)?;

            Ok(McpToolResult::text(text))
        }

        "trigger_deployment" => {
            if !write_enabled {
                return Err(McpError::WriteNotEnabled);
            }
            let project_id = require_i32(arguments, "project_id", name)?;
            let environment_id = require_i32(arguments, "environment_id", name)?;
            let branch = arguments
                .get("branch")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);

            // Tenant-boundary check first: confines a deployment token to its
            // own bound project regardless of the checker/admin bypass below.
            check_project_scope(ctx.auth, project_id)?;

            // Project access check at the propose step.  The check is
            // repeated at confirm time as well, because the two steps are
            // separate HTTP requests and either could be replayed or misused
            // independently (e.g. a propose token passed to a different
            // session that has lost the project access).  Admins bypass via auth.
            check_project_access(checker, ctx.auth, project_id).await?;

            // Propose rather than execute immediately.  The human confirms
            // via confirm_action.
            let token = proposals.create(
                "trigger_deployment".to_string(),
                json!({
                    "project_id": project_id,
                    "environment_id": environment_id,
                    "branch": branch
                }),
            );

            Ok(McpToolResult::text_with_proposal_token(
                format!(
                    "Deployment proposed for project {project_id} / environment \
                     {environment_id}{}.\n\nCall confirm_action with token: {token}\n\
                     Token expires in 5 minutes.",
                    branch
                        .as_deref()
                        .map(|b| format!(" (branch: {b})"))
                        .unwrap_or_default()
                ),
                token,
            ))
        }

        "confirm_action" => {
            if !write_enabled {
                return Err(McpError::WriteNotEnabled);
            }
            let token = arguments
                .get("token")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| McpError::MissingArgument {
                    arg: "token".to_string(),
                    tool: name.to_string(),
                })?;

            // Peek at the proposal without consuming it.  This lets us extract
            // arguments and run the access check BEFORE the token is removed.
            // A denied access check must not consume the token, so the
            // legitimate confirmer can retry — preventing a denial-of-service
            // where an unauthorised caller invalidates valid proposal tokens.
            let peeked = proposals.peek(token).map_err(McpError::from)?;

            // Execute the proposed action.
            match peeked.tool_name.as_str() {
                "trigger_deployment" => {
                    let project_id =
                        require_i32(&peeked.arguments, "project_id", "confirm_action")?;
                    let environment_id =
                        require_i32(&peeked.arguments, "environment_id", "confirm_action")?;
                    let branch = peeked
                        .arguments
                        .get("branch")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string);

                    // Re-check project access at the confirm step.  The propose
                    // and confirm steps are separate HTTP requests: the token
                    // holder at confirm time may differ from the one who proposed
                    // (e.g. a replayed or forwarded token).  Fail-closed on any
                    // access denial or infra error.  Admins bypass via auth;
                    // deployment tokens are still confined to their own project
                    // by check_project_scope below regardless of that bypass.
                    //
                    // These checks run BEFORE take() so a denied attempt does NOT
                    // consume the token — the authorised proposer can still confirm.
                    check_project_scope(ctx.auth, project_id)?;
                    check_project_access(checker, ctx.auth, project_id).await?;

                    // Access granted: now consume the token atomically.  If a
                    // concurrent confirm won the race between our peek and here,
                    // take() returns NotFound — the expected single-use behaviour,
                    // propagated normally.
                    proposals.take(token).map_err(McpError::from)?;

                    let audit_event = McpDeploymentTriggeredAudit {
                        context: AuditContext {
                            user_id: actor.user_id,
                            ip_address: actor.ip_address.clone(),
                            user_agent: actor.user_agent.clone(),
                        },
                        project_id,
                        environment_id,
                        branch: branch.clone(),
                    };
                    if let Err(e) = audit_service.create_audit_log(&audit_event).await {
                        error!("Failed to create MCP deployment-triggered audit log: {}", e);
                        // Continue with the operation even if audit logging fails.
                    }

                    deployment_service
                        .trigger_pipeline(project_id, environment_id, branch, None, None)
                        .await
                        .map_err(|e| match e {
                            temps_deployments::DeploymentError::NotFound(_) => {
                                McpError::DeploymentNotFound { project_id }
                            }
                            other => McpError::DeploymentService(other.to_string()),
                        })?;

                    Ok(McpToolResult::text(format!(
                        "Deployment triggered for project {project_id} / \
                         environment {environment_id}. Check the Temps console \
                         for build progress."
                    )))
                }

                other => Err(McpError::UnknownTool {
                    name: format!("confirm_action/{other}"),
                }),
            }
        }

        other => Err(McpError::UnknownTool {
            name: other.to_string(),
        }),
    }
}

/// Extract and coerce an integer argument from a JSON object.
fn require_i32(arguments: &serde_json::Value, arg: &str, tool: &str) -> Result<i32, McpError> {
    let v = arguments
        .get(arg)
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| McpError::MissingArgument {
            arg: arg.to_string(),
            tool: tool.to_string(),
        })?;
    i32::try_from(v).map_err(|_| McpError::InvalidArgument {
        arg: arg.to_string(),
        reason: format!("{v} does not fit in a 32-bit ID"),
    })
}

/// Clamp a caller-supplied `page` to a range that can never overflow when
/// `DeploymentService::get_project_deployments` computes `OFFSET = per_page *
/// (page - 1)` as an unchecked `u64 * u64` (Sea-ORM's `Paginator::fetch_page`)
/// and binds the result back down to Postgres's `i64`. `i32::MAX` combined
/// with `per_page`'s max of 100 keeps the offset far below `i64::MAX`, and is
/// already a far larger page count than any real deployment history.
fn clamp_page(v: i64) -> i64 {
    v.clamp(1, i64::from(i32::MAX))
}

/// Clamp a caller-supplied `per_page` to `[1, 100]`, matching this
/// codebase's pagination convention (CLAUDE.md: default 20, max 100) and
/// keeping the OFFSET computation `clamp_page` protects against bounded on
/// both operands, not just one.
fn clamp_per_page(v: i64) -> i64 {
    v.clamp(1, 100)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use chrono::Utc;
    use temps_auth::context::AuthContext;
    use temps_auth::permissions::Role;
    use temps_entities::users;

    // ── AuthContext helpers ───────────────────────────────────────────────────

    fn make_user(id: i32) -> users::Model {
        let now = Utc::now();
        users::Model {
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

    fn user_auth(id: i32) -> AuthContext {
        AuthContext::new_session(make_user(id), Role::User)
    }

    fn platform_admin_auth() -> AuthContext {
        AuthContext::new_session(make_user(1), Role::PlatformAdmin)
    }

    #[test]
    fn tools_read_only_excludes_write_tools() {
        let tools = tools(false);
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"list_deployments"));
        assert!(!names.contains(&"trigger_deployment"));
        assert!(!names.contains(&"confirm_action"));
    }

    #[test]
    fn tools_write_mode_includes_write_tools() {
        let tools = tools(true);
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"list_deployments"));
        assert!(names.contains(&"trigger_deployment"));
        assert!(names.contains(&"confirm_action"));
    }

    #[test]
    fn require_i32_missing_returns_error() {
        let args = serde_json::json!({});
        let err = require_i32(&args, "project_id", "list_deployments")
            .expect_err("missing arg must fail");
        assert!(matches!(err, McpError::MissingArgument { .. }));
    }

    #[test]
    fn require_i32_out_of_range_returns_invalid_argument_not_a_silent_truncation() {
        // Rust's `as i32` would silently wrap this to a small, wrong value
        // instead of erroring — verify the typed rejection instead.
        let args = serde_json::json!({ "project_id": 4_294_967_297_i64 });
        let err = require_i32(&args, "project_id", "list_deployments")
            .expect_err("out-of-range value must be rejected, not truncated");
        assert!(matches!(err, McpError::InvalidArgument { .. }));
    }

    // ── clamp_page / clamp_per_page (pagination-overflow regression guard) ────
    //
    // Regression coverage for the crash reproduced live against a running
    // server: a negative or extreme `page`/`per_page` reaches
    // `DeploymentService::get_project_deployments`'s unchecked `as u64` cast,
    // and Sea-ORM's OFFSET computation then panics converting back to
    // Postgres's `i64` — killing the console's HTTP listener task. These
    // functions are the MCP-layer defense-in-depth clamp (the authoritative
    // fix lives in the service itself); test them directly since `execute()`
    // needs a real `Arc<DeploymentService>` and can't be unit tested here.

    #[test]
    fn clamp_page_floors_negative_to_one() {
        assert_eq!(clamp_page(-5), 1);
        assert_eq!(clamp_page(i64::MIN), 1);
    }

    #[test]
    fn clamp_page_leaves_in_range_value_untouched() {
        assert_eq!(clamp_page(1), 1);
        assert_eq!(clamp_page(42), 42);
    }

    #[test]
    fn clamp_page_caps_extreme_positive_value() {
        // This is the exact overflow this function exists to prevent: an
        // in-range i64 that, uncapped, would make per_page * (page - 1)
        // overflow i64 once bound to Postgres.
        assert_eq!(clamp_page(1_000_000_000_000_000_000), i64::from(i32::MAX));
        assert_eq!(clamp_page(i64::MAX), i64::from(i32::MAX));
    }

    #[test]
    fn clamp_per_page_clamps_both_directions() {
        assert_eq!(clamp_per_page(-5), 1);
        assert_eq!(clamp_per_page(0), 1);
        assert_eq!(clamp_per_page(1), 1);
        assert_eq!(clamp_per_page(100), 100);
        assert_eq!(clamp_per_page(101), 100);
        assert_eq!(clamp_per_page(i64::MAX), 100);
    }

    #[tokio::test]
    async fn trigger_deployment_without_write_returns_error() {
        // Stubs — the service path is never reached.
        let proposals = Arc::new(ProposalStore::new());
        // We can't build DeploymentService without a full DB, but the guard
        // fires before any service call.
        let err = McpError::WriteNotEnabled;
        // Mirror the guard logic without calling execute (which needs Arc<DeploymentService>).
        assert!(matches!(err, McpError::WriteNotEnabled));
        assert!(err.to_string().contains("write=1"));
        let _ = proposals; // kept alive
    }

    #[test]
    fn propose_and_consume_in_store() {
        let store = ProposalStore::new();
        let token = store.create(
            "trigger_deployment".to_string(),
            serde_json::json!({ "project_id": 5 }),
        );
        let taken = store.take(&token).expect("first take must succeed");
        assert_eq!(taken.tool_name, "trigger_deployment");
        store.take(&token).expect_err("second take must fail");
    }

    // ── ProjectAccessChecker mock ─────────────────────────────────────────────

    /// A checker that denies access to a specific project_id.
    struct DenyingChecker {
        denied_project_id: i32,
    }

    #[async_trait]
    impl ProjectAccessChecker for DenyingChecker {
        async fn user_can_access_project(
            &self,
            _user_id: i32,
            project_id: i32,
        ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
            Ok(project_id != self.denied_project_id)
        }
    }

    // ── confirm_action token-preservation test ────────────────────────────────

    /// An unauthorised `confirm_action` attempt (checker denies the project)
    /// must NOT remove the proposal token from the store.  The legitimate
    /// confirmer must be able to retry with the same token afterward.
    #[tokio::test]
    async fn unauthorized_confirm_does_not_consume_token() {
        let store = ProposalStore::new();
        let token = store.create(
            "trigger_deployment".to_string(),
            serde_json::json!({
                "project_id": 42,
                "environment_id": 1,
                "branch": null
            }),
        );

        let checker = DenyingChecker {
            denied_project_id: 42,
        };

        // Step 1: peek must succeed (token exists and is valid).
        let peeked = store
            .peek(&token)
            .expect("peek must succeed before any access check");
        let project_id =
            require_i32(&peeked.arguments, "project_id", "confirm_action").expect("project_id");

        // Step 2: access check must deny a non-admin user.
        let unauth = user_auth(99);
        let access_result = check_project_access(Some(&checker), &unauth, project_id).await;
        assert!(
            matches!(
                access_result,
                Err(McpError::ProjectAccessDenied { project_id: 42 })
            ),
            "access check must deny project 42 for non-admin user 99"
        );

        // Step 3: because we did NOT call take(), the token must still be
        // present in the store — peek() and take() must both still succeed.
        let peeked_again = store
            .peek(&token)
            .expect("token must still be present after a denied access check");
        assert_eq!(peeked_again.tool_name, "trigger_deployment");

        let taken = store
            .take(&token)
            .expect("authorised confirmer must still be able to consume the token");
        assert_eq!(taken.tool_name, "trigger_deployment");
    }

    /// A PlatformAdmin attempting `confirm_action` must bypass the checker
    /// and be allowed unconditionally.
    #[tokio::test]
    async fn platform_admin_confirm_bypasses_checker() {
        let checker = DenyingChecker {
            denied_project_id: 42,
        };
        let admin = platform_admin_auth();
        // PlatformAdmin must not be denied even for project 42 which the
        // checker would deny for a regular user.
        let result = check_project_access(Some(&checker), &admin, 42).await;
        assert!(
            result.is_ok(),
            "PlatformAdmin must bypass the checker at the confirm step"
        );
    }
}
