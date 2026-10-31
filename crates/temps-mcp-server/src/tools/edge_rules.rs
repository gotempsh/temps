// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Edge rule tools (ADR-071 §9.1), in the **networking** group.
//!
//! Read tools: `list_edge_rules`, `get_edge_rule`, `evaluate_edge_rules`.
//! Write tools (propose-then-confirm): `create_edge_rule`,
//! `update_edge_rule`, `delete_edge_rule`, `reorder_edge_rules`,
//! `set_edge_rule_mode`.
//!
//! Safety: a rule an agent creates always starts in `log_only` mode, so it
//! records what it would match without acting; turning it into an enforcing
//! rule is a separate `set_edge_rule_mode` proposal a human confirms. Every
//! write is validated when it is proposed, so the agent sees the same issues
//! the console shows before a human is asked to confirm anything.

use std::sync::Arc;

use serde::Serialize;
use serde_json::json;
use temps_auth::context::AuthContext;
use temps_auth::permissions::Permission;
use temps_core::project_access::ProjectAccessChecker;
use temps_core::{AuditLogger, AuditOperation};
use temps_edge_rules::Mode;
use temps_proxy::service::edge_rule_service::{
    CreateEdgeRuleRequest, EdgeRuleResponse, EdgeRuleService, EdgeRuleServiceError,
    EvaluateEdgeRulesRequest, ReorderEdgeRulesRequest, UpdateEdgeRuleRequest,
};
use tracing::error;

use crate::access::{check_project_permission, check_project_scope};
use crate::audit::AuditContext;
use crate::error::McpError;
use crate::proposal::ProposalStore;
use crate::protocol::{McpTool, McpToolResult};
use crate::tools::deployments::AuditActor;

/// Read tools, available without write mode.
pub const READ_TOOLS: &[&str] = &["list_edge_rules", "get_edge_rule", "evaluate_edge_rules"];

/// Write tools, which only propose; `confirm_action` executes them.
pub const WRITE_TOOLS: &[&str] = &[
    "create_edge_rule",
    "update_edge_rule",
    "delete_edge_rule",
    "reorder_edge_rules",
    "set_edge_rule_mode",
];

/// Whether `name` is one of this module's tools (or a proposal it made).
pub fn handles(name: &str) -> bool {
    READ_TOOLS.contains(&name) || WRITE_TOOLS.contains(&name)
}

const EXPRESSION_HELP: &str =
    "Match expression: a group {\"combinator\": \"all\"|\"any\"|\"none\", \
     \"children\": [...]} whose children are conditions {\"kind\": \"condition\", \"field\", \
     \"op\", \"values\": [...], \"key\"?, \"case_insensitive\"?} or nested groups {\"kind\": \
     \"group\", ...}. Fields: ip, country, continent, asn, is_hosting_provider, host, path, \
     raw_path, method, query, header, cookie, user_agent, ja4, bot_class. Operators: eq, in, \
     in_cidr, starts_with, ends_with, contains, glob, regex, exists, not_exists. query, header \
     and cookie need \"key\". Paths compare without case when case_insensitive is true.";

const ACTION_HELP: &str = "Action: {\"type\": \"block\", \"status\": 400-499, \"body\"?} | \
     {\"type\": \"allow\"} (skip later edge rules) | {\"type\": \"challenge\"} (proof-of-work \
     page) | {\"type\": \"redirect\", \"target\": \"/path or https://host/path\", \"status\": \
     301|302|307|308} ({path} and {query} insert the request's) | {\"type\": \"log\"}.";

fn project_id_schema() -> serde_json::Value {
    json!({"type": "integer", "description": "Numeric project ID"})
}

fn rule_id_schema() -> serde_json::Value {
    json!({"type": "integer", "description": "Numeric edge rule ID"})
}

pub fn tools(write_enabled: bool) -> Vec<McpTool> {
    let mut result = vec![
        McpTool {
            name: "list_edge_rules".to_string(),
            description: "List a project's edge rules in evaluation order (the first rule \
                          that decides wins), with each rule's one-line summary, mode and \
                          match count."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {"project_id": project_id_schema()},
                "required": ["project_id"]
            }),
        },
        McpTool {
            name: "get_edge_rule".to_string(),
            description: "Get one edge rule, including its full expression and action.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_id": project_id_schema(),
                    "rule_id": rule_id_schema()
                },
                "required": ["project_id", "rule_id"]
            }),
        },
        McpTool {
            name: "evaluate_edge_rules".to_string(),
            description: format!(
                "Dry run: what the proxy would do with a request, against the saved rules or \
                 the `rules` given. Returns the decision, the rule that matched and a \
                 per-rule trace. Nothing is stored. Use it to check a rule before proposing \
                 it. {EXPRESSION_HELP} {ACTION_HELP}"
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_id": project_id_schema(),
                    "request": {
                        "type": "object",
                        "description": "The request: {\"method\"?: \"GET\", \"url\": \"https://app.example.com/path?q\", \"headers\"?: [{\"name\", \"value\"}], \"user_agent\"?, \"ip\"?, \"country\"?, \"continent\"?, \"asn\"?, \"is_hosting_provider\"?, \"ja4\"?, \"bot_class\"?, \"environment_id\"?, \"challenge_cleared\"?}"
                    },
                    "rules": {
                        "type": "array",
                        "description": "Rules to evaluate instead of the saved ones: [{\"expression\", \"action\", \"mode\"?, \"enabled\"?, \"environment_id\"?, \"name\"?, \"id\"?}]",
                        "items": {"type": "object"}
                    }
                },
                "required": ["project_id", "request"]
            }),
        },
    ];

    if write_enabled {
        result.push(McpTool {
            name: "create_edge_rule".to_string(),
            description: format!(
                "Propose a new edge rule. It is created in log_only mode: it records what it \
                 would match without acting. Propose set_edge_rule_mode to enforce it once its \
                 matches look right. Returns a proposal token for confirm_action. \
                 {EXPRESSION_HELP} {ACTION_HELP}"
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_id": project_id_schema(),
                    "name": {"type": "string", "description": "Up to 80 characters; generated from the rule when omitted"},
                    "environment_id": {"type": "integer", "description": "Apply to one environment only (default: every environment)"},
                    "position": {"type": "integer", "description": "Insert at this 0-based position (default: last)"},
                    "expression": {"type": "object"},
                    "action": {"type": "object"}
                },
                "required": ["project_id", "expression", "action"]
            }),
        });
        result.push(McpTool {
            name: "update_edge_rule".to_string(),
            description: format!(
                "Propose changing an edge rule. Absent fields keep their value; send the \
                 `revision` you read so a concurrent change is not overwritten. Use \
                 set_edge_rule_mode to change the mode. {EXPRESSION_HELP} {ACTION_HELP}"
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_id": project_id_schema(),
                    "rule_id": rule_id_schema(),
                    "revision": {"type": "integer"},
                    "name": {"type": "string"},
                    "enabled": {"type": "boolean"},
                    "environment_id": {"type": ["integer", "null"]},
                    "expression": {"type": "object"},
                    "action": {"type": "object"}
                },
                "required": ["project_id", "rule_id"]
            }),
        });
        result.push(McpTool {
            name: "delete_edge_rule".to_string(),
            description: "Propose deleting an edge rule.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_id": project_id_schema(),
                    "rule_id": rule_id_schema()
                },
                "required": ["project_id", "rule_id"]
            }),
        });
        result.push(McpTool {
            name: "reorder_edge_rules".to_string(),
            description: "Propose a new evaluation order. `rule_ids` must list every rule of \
                          the project exactly once."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_id": project_id_schema(),
                    "rule_ids": {"type": "array", "items": {"type": "integer"}}
                },
                "required": ["project_id", "rule_ids"]
            }),
        });
        result.push(McpTool {
            name: "set_edge_rule_mode".to_string(),
            description: "Propose switching a rule between `log_only` (record matches) and \
                          `enforce` (act on them)."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_id": project_id_schema(),
                    "rule_id": rule_id_schema(),
                    "mode": {"type": "string", "enum": ["enforce", "log_only"]},
                    "revision": {"type": "integer"}
                },
                "required": ["project_id", "rule_id", "mode"]
            }),
        });
    }
    result
}

pub struct EdgeRuleExecCtx<'a> {
    pub service: &'a Arc<EdgeRuleService>,
    pub proposals: &'a Arc<ProposalStore>,
    pub audit_service: &'a Arc<dyn AuditLogger>,
    pub actor: &'a AuditActor,
    pub auth: &'a AuthContext,
    pub checker: Option<&'a dyn ProjectAccessChecker>,
}

impl From<EdgeRuleServiceError> for McpError {
    fn from(error: EdgeRuleServiceError) -> Self {
        match error {
            EdgeRuleServiceError::ProjectNotFound { project_id } => {
                McpError::ProjectNotFound { project_id }
            }
            EdgeRuleServiceError::RuleNotFound {
                project_id,
                rule_id,
            } => McpError::EdgeRuleNotFound {
                project_id,
                rule_id,
            },
            EdgeRuleServiceError::EnvironmentNotFound { .. }
            | EdgeRuleServiceError::Invalid { .. }
            | EdgeRuleServiceError::TooManyRules { .. }
            | EdgeRuleServiceError::RevisionConflict { .. }
            | EdgeRuleServiceError::InvalidOrder { .. } => McpError::EdgeRuleRejected {
                reason: error.to_string(),
            },
            EdgeRuleServiceError::Unreadable { .. } | EdgeRuleServiceError::Database { .. } => {
                McpError::EdgeRuleService(error.to_string())
            }
        }
    }
}

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

/// Deserializes `arguments` (minus `project_id`/`rule_id`) into `T`.
fn parse<T: serde::de::DeserializeOwned>(
    arguments: &serde_json::Value,
    what: &str,
) -> Result<T, McpError> {
    let mut object = arguments.clone();
    if let Some(map) = object.as_object_mut() {
        map.remove("project_id");
        map.remove("rule_id");
    }
    serde_json::from_value(object).map_err(|e| McpError::InvalidArgument {
        arg: what.to_string(),
        reason: e.to_string(),
    })
}

fn pretty<T: Serialize>(value: &T) -> Result<String, McpError> {
    serde_json::to_string_pretty(value).map_err(McpError::Serialization)
}

async fn authorize(
    ctx: &EdgeRuleExecCtx<'_>,
    project_id: i32,
    permission: Permission,
) -> Result<(), McpError> {
    if !ctx.auth.has_permission(&permission) {
        return Err(McpError::InsufficientPermission {
            permission: permission.to_string(),
        });
    }
    check_project_scope(ctx.auth, project_id)?;
    // The project role decides, not only the instance role: a team viewer
    // holding an instance-wide write permission must not write here.
    check_project_permission(ctx.checker, ctx.auth, project_id, &permission).await
}

/// `arguments` with `revision` set to the revision the proposal describes, so
/// confirming changes that rule and not one edited while the proposal waited
/// for a human. A caller-supplied revision that is already stale is refused
/// now rather than at confirmation.
fn bind_revision(
    arguments: &serde_json::Value,
    current: &EdgeRuleResponse,
) -> Result<serde_json::Value, McpError> {
    if let Some(expected) = arguments
        .get("revision")
        .and_then(serde_json::Value::as_i64)
    {
        if expected != current.revision {
            return Err(EdgeRuleServiceError::RevisionConflict {
                project_id: current.project_id,
                rule_id: current.id,
                expected,
                current: current.revision,
            }
            .into());
        }
    }
    let mut bound = arguments.clone();
    if let Some(map) = bound.as_object_mut() {
        map.insert("revision".to_string(), json!(current.revision));
    }
    Ok(bound)
}

/// The revision a stored proposal was bound to by [`bind_revision`].
fn bound_revision(arguments: &serde_json::Value) -> Result<i64, McpError> {
    arguments
        .get("revision")
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| McpError::MissingArgument {
            arg: "revision".to_string(),
            tool: "confirm_action".to_string(),
        })
}

fn proposal(
    ctx: &EdgeRuleExecCtx<'_>,
    tool: &str,
    arguments: &serde_json::Value,
    description: String,
) -> McpToolResult {
    let token = ctx.proposals.create(tool.to_string(), arguments.clone());
    McpToolResult::text_with_proposal_token(
        format!(
            "{description}\n\nCall confirm_action with token: {token}\nToken expires in 5 \
             minutes. The change is enforced by every proxy within seconds of confirming."
        ),
        token,
    )
}

/// Runs a read tool or proposes a write.
pub async fn execute(
    name: &str,
    arguments: &serde_json::Value,
    write_enabled: bool,
    ctx: &EdgeRuleExecCtx<'_>,
) -> Result<McpToolResult, McpError> {
    let project_id = require_i32(arguments, "project_id", name)?;
    if WRITE_TOOLS.contains(&name) {
        if !write_enabled {
            return Err(McpError::WriteNotEnabled);
        }
        authorize(ctx, project_id, Permission::EdgeRulesWrite).await?;
    } else {
        authorize(ctx, project_id, Permission::EdgeRulesRead).await?;
    }

    match name {
        "list_edge_rules" => Ok(McpToolResult::text(pretty(
            &ctx.service.list(project_id).await?,
        )?)),
        "get_edge_rule" => {
            let rule_id = require_i32(arguments, "rule_id", name)?;
            Ok(McpToolResult::text(pretty(
                &ctx.service.get(project_id, rule_id).await?,
            )?))
        }
        "evaluate_edge_rules" => {
            let request: EvaluateEdgeRulesRequest = parse(arguments, "request")?;
            Ok(McpToolResult::text(pretty(
                &ctx.service.evaluate(project_id, request).await?,
            )?))
        }
        "create_edge_rule" => {
            let request: CreateEdgeRuleRequest = parse(arguments, "rule")?;
            let (rule_name, summary) = ctx.service.validate(
                project_id,
                request.name.as_deref(),
                request.expression,
                request.action,
            )?;
            Ok(proposal(
                ctx,
                name,
                arguments,
                format!(
                    "Proposed: create edge rule '{rule_name}' in project {project_id} in \
                     log_only mode.\n{summary}"
                ),
            ))
        }
        "update_edge_rule" => {
            let rule_id = require_i32(arguments, "rule_id", name)?;
            let mut request: UpdateEdgeRuleRequest = parse(arguments, "changes")?;
            // The mode only changes through set_edge_rule_mode.
            request.mode = None;
            let current = ctx.service.get(project_id, rule_id).await?;
            let bound = bind_revision(arguments, &current)?;
            let (current_revision, current_summary) = (current.revision, current.summary.clone());
            let (rule_name, summary) = ctx.service.validate(
                project_id,
                request.name.as_deref().or(Some(current.name.as_str())),
                request.expression.unwrap_or(current.expression),
                request.action.unwrap_or(current.action),
            )?;
            Ok(proposal(
                ctx,
                name,
                &bound,
                format!(
                    "Proposed: update edge rule {rule_id} ('{rule_name}') in project \
                     {project_id} at revision {current_revision}.\nAfter: {summary}\n\
                     Before: {current_summary}"
                ),
            ))
        }
        "delete_edge_rule" => {
            let rule_id = require_i32(arguments, "rule_id", name)?;
            let current = ctx.service.get(project_id, rule_id).await?;
            let bound = bind_revision(arguments, &current)?;
            Ok(proposal(
                ctx,
                name,
                &bound,
                format!(
                    "Proposed: delete edge rule {rule_id} ('{}') at revision {} from project \
                     {project_id}.\n{}",
                    current.name, current.revision, current.summary
                ),
            ))
        }
        "reorder_edge_rules" => {
            let request: ReorderEdgeRulesRequest = parse(arguments, "rule_ids")?;
            Ok(proposal(
                ctx,
                name,
                arguments,
                format!(
                    "Proposed: evaluate the edge rules of project {project_id} in the order {:?}.",
                    request.rule_ids
                ),
            ))
        }
        "set_edge_rule_mode" => {
            let rule_id = require_i32(arguments, "rule_id", name)?;
            let mode = arguments
                .get("mode")
                .and_then(serde_json::Value::as_str)
                .and_then(Mode::parse)
                .ok_or_else(|| McpError::InvalidArgument {
                    arg: "mode".to_string(),
                    reason: "must be 'enforce' or 'log_only'".to_string(),
                })?;
            let current = ctx.service.get(project_id, rule_id).await?;
            let effect = match mode {
                Mode::Enforce => "start acting on matches",
                Mode::LogOnly => "only record matches",
            };
            let bound = bind_revision(arguments, &current)?;
            Ok(proposal(
                ctx,
                name,
                &bound,
                format!(
                    "Proposed: switch edge rule {rule_id} ('{}', revision {}) in project \
                     {project_id} to {} so it will {effect}.\n{}",
                    current.name,
                    current.revision,
                    mode.as_str(),
                    current.summary
                ),
            ))
        }
        other => Err(McpError::UnknownTool {
            name: other.to_string(),
        }),
    }
}

#[derive(Debug, Clone, Serialize)]
struct McpEdgeRuleAudit {
    context: AuditContext,
    #[serde(skip)]
    operation: &'static str,
    source: &'static str,
    project_id: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    rule_id: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    before: Option<EdgeRuleResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    after: Option<EdgeRuleResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rule_ids: Option<Vec<i32>>,
}

impl AuditOperation for McpEdgeRuleAudit {
    fn operation_type(&self) -> String {
        self.operation.to_string()
    }

    fn user_id(&self) -> Option<i32> {
        Some(self.context.user_id)
    }

    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }

    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }

    fn serialize(&self) -> temps_core::anyhow::Result<String> {
        serde_json::to_string(self).map_err(|e| {
            temps_core::anyhow::anyhow!(
                "Failed to serialize audit operation {}: {}",
                self.operation,
                e
            )
        })
    }
}

/// Executes a confirmed edge rule proposal. Access is re-checked before the
/// token is consumed, so a denied attempt leaves it for the right confirmer.
pub async fn confirm(
    token: &str,
    tool: &str,
    arguments: &serde_json::Value,
    ctx: &EdgeRuleExecCtx<'_>,
) -> Result<McpToolResult, McpError> {
    let project_id = require_i32(arguments, "project_id", "confirm_action")?;
    authorize(ctx, project_id, Permission::EdgeRulesWrite).await?;
    ctx.proposals.take(token).map_err(McpError::from)?;

    let user_id = Some(ctx.actor.user_id);
    let audit = |operation: &'static str| McpEdgeRuleAudit {
        context: AuditContext {
            user_id: ctx.actor.user_id,
            ip_address: ctx.actor.ip_address.clone(),
            user_agent: ctx.actor.user_agent.clone(),
        },
        operation,
        source: "mcp",
        project_id,
        rule_id: None,
        before: None,
        after: None,
        rule_ids: None,
    };

    let (record, message) = match tool {
        "create_edge_rule" => {
            let mut request: CreateEdgeRuleRequest = parse(arguments, "rule")?;
            request.mode = Mode::LogOnly;
            let rule = ctx.service.create(project_id, request, user_id).await?;
            let message = format!(
                "Created edge rule {} ('{}') in log_only mode.\n{}",
                rule.id, rule.name, rule.summary
            );
            let mut record = audit("EDGE_RULE_CREATED");
            record.rule_id = Some(rule.id);
            record.after = Some(rule);
            (record, message)
        }
        "update_edge_rule" | "set_edge_rule_mode" => {
            let rule_id = require_i32(arguments, "rule_id", "confirm_action")?;
            let mut request: UpdateEdgeRuleRequest = parse(arguments, "changes")?;
            // Applied only to the revision the human reviewed.
            let revision = Some(bound_revision(arguments)?);
            if tool == "update_edge_rule" {
                request.mode = None;
                request.revision = revision;
            } else {
                request = UpdateEdgeRuleRequest {
                    revision,
                    mode: request.mode,
                    ..Default::default()
                };
            }
            let (before, after) = ctx
                .service
                .update(project_id, rule_id, request, user_id)
                .await?;
            let message = format!(
                "Updated edge rule {rule_id} ('{}'), now {} at revision {}.\n{}",
                after.name,
                after.mode.as_str(),
                after.revision,
                after.summary
            );
            let mut record = audit("EDGE_RULE_UPDATED");
            record.rule_id = Some(rule_id);
            record.before = Some(before);
            record.after = Some(after);
            (record, message)
        }
        "delete_edge_rule" => {
            let rule_id = require_i32(arguments, "rule_id", "confirm_action")?;
            let revision = bound_revision(arguments)?;
            let rule = ctx
                .service
                .delete(project_id, rule_id, Some(revision))
                .await?;
            let message = format!("Deleted edge rule {rule_id} ('{}').", rule.name);
            let mut record = audit("EDGE_RULE_DELETED");
            record.rule_id = Some(rule_id);
            record.before = Some(rule);
            (record, message)
        }
        "reorder_edge_rules" => {
            let request: ReorderEdgeRulesRequest = parse(arguments, "rule_ids")?;
            let rule_ids = request.rule_ids.clone();
            let list = ctx.service.reorder(project_id, request).await?;
            let message = format!(
                "Reordered the edge rules of project {project_id}: {}.",
                list.rules
                    .iter()
                    .map(|r| format!("{} '{}'", r.id, r.name))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            let mut record = audit("EDGE_RULES_REORDERED");
            record.rule_ids = Some(rule_ids);
            (record, message)
        }
        other => {
            return Err(McpError::UnknownTool {
                name: format!("confirm_action/{other}"),
            })
        }
    };
    if let Err(e) = ctx.audit_service.create_audit_log(&record).await {
        error!("Failed to create MCP edge rule audit log: {}", e);
    }
    Ok(McpToolResult::text(message))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_tools_are_listed_only_in_write_mode() {
        let read: Vec<String> = tools(false).into_iter().map(|t| t.name).collect();
        assert_eq!(read, READ_TOOLS);
        let all: Vec<String> = tools(true).into_iter().map(|t| t.name).collect();
        for name in WRITE_TOOLS {
            assert!(all.iter().any(|n| n == name), "{name} missing");
        }
    }

    #[test]
    fn every_tool_has_an_object_schema_requiring_the_project() {
        for tool in tools(true) {
            assert_eq!(tool.input_schema["type"], "object", "{}", tool.name);
            let required = tool.input_schema["required"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            assert!(
                required.iter().any(|r| r == "project_id"),
                "{} must require project_id",
                tool.name
            );
        }
    }

    #[test]
    fn arguments_parse_without_the_ids() {
        let args = json!({
            "project_id": 3,
            "rule_id": 4,
            "revision": 2,
            "enabled": false
        });
        let update: UpdateEdgeRuleRequest = parse(&args, "changes").expect("parses");
        assert_eq!(update.revision, Some(2));
        assert_eq!(update.enabled, Some(false));

        let bad = json!({"project_id": 3, "expression": {"combinator": "sometimes"}, "action": {"type": "log"}});
        assert!(matches!(
            parse::<CreateEdgeRuleRequest>(&bad, "rule"),
            Err(McpError::InvalidArgument { .. })
        ));
    }

    mod database {
        use super::*;
        use chrono::Utc;
        use std::sync::Mutex;
        use temps_auth::permissions::Role;
        use temps_database::test_utils::{is_container_runtime_unavailable, TestDatabase};

        #[derive(Default)]
        struct RecordingAudit(Mutex<Vec<String>>);

        #[async_trait::async_trait]
        impl AuditLogger for RecordingAudit {
            async fn create_audit_log(
                &self,
                operation: &dyn AuditOperation,
            ) -> temps_core::anyhow::Result<()> {
                let mut ops = match self.0.lock() {
                    Ok(ops) => ops,
                    Err(poisoned) => poisoned.into_inner(),
                };
                ops.push(operation.operation_type());
                Ok(())
            }
        }

        fn session(role: Role) -> AuthContext {
            let now = Utc::now();
            AuthContext::new_session(
                temps_entities::users::Model {
                    id: 1,
                    name: "Test User".to_string(),
                    email: "user1@example.test".to_string(),
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
                },
                role,
            )
        }

        async fn seeded() -> Option<(TestDatabase, i32)> {
            let db = match TestDatabase::with_migrations().await {
                Ok(db) => db,
                Err(error) if is_container_runtime_unavailable(&error.to_string()) => {
                    eprintln!("Docker not available, skipping MCP edge rule test");
                    return None;
                }
                Err(error) => panic!("test database: {error}"),
            };
            let rows = db
                .query_sql(
                    "WITH u AS (INSERT INTO users (id, name, email, created_at, updated_at) \
                     VALUES (1, 'Test User', 'user1@example.test', now(), now()) RETURNING id) \
                     INSERT INTO projects (name, repo_name, repo_owner, directory, main_branch, \
                     preset, slug, created_at, updated_at) SELECT 'mcp-edge', 'mcp-edge', \
                     'owner', '.', 'main', 'dockerfile', 'mcp-edge', now(), now() FROM u \
                     RETURNING id",
                )
                .await
                .expect("seed user and project");
            let project_id: i32 = rows
                .first()
                .expect("one project")
                .try_get("", "id")
                .expect("project id");
            Some((db, project_id))
        }

        #[tokio::test]
        async fn proposals_execute_only_on_confirm_and_creates_start_log_only() {
            let Some((db, project_id)) = seeded().await else {
                return;
            };
            let service = Arc::new(EdgeRuleService::new(db.connection_arc()));
            let proposals = Arc::new(ProposalStore::new());
            let recorder = Arc::new(RecordingAudit::default());
            let audit: Arc<dyn AuditLogger> = recorder.clone();
            let actor = AuditActor {
                user_id: 1,
                ip_address: Some("127.0.0.1".into()),
                user_agent: "test".into(),
            };
            let auth = session(Role::User);
            let ctx = EdgeRuleExecCtx {
                service: &service,
                proposals: &proposals,
                audit_service: &audit,
                actor: &actor,
                auth: &auth,
                checker: None,
            };

            let args = json!({
                "project_id": project_id,
                "mode": "enforce",
                "expression": {"combinator": "all", "children": [
                    {"kind": "condition", "field": "path", "op": "starts_with",
                     "values": ["/admin"], "case_insensitive": true}
                ]},
                "action": {"type": "block", "status": 403}
            });
            assert!(matches!(
                execute("create_edge_rule", &args, false, &ctx).await,
                Err(McpError::WriteNotEnabled)
            ));
            let proposed = execute("create_edge_rule", &args, true, &ctx)
                .await
                .expect("proposes");
            let token = proposed.proposal_token.clone().expect("a token");
            assert!(
                service
                    .list(project_id)
                    .await
                    .expect("list")
                    .rules
                    .is_empty(),
                "nothing is written until confirmed"
            );

            confirm(&token, "create_edge_rule", &args, &ctx)
                .await
                .expect("confirms");
            let rules = service.list(project_id).await.expect("list").rules;
            assert_eq!(rules.len(), 1);
            assert_eq!(
                rules[0].mode,
                Mode::LogOnly,
                "an agent's rule never enforces without a second confirmation"
            );
            assert!(
                confirm(&token, "create_edge_rule", &args, &ctx)
                    .await
                    .is_err(),
                "tokens are single-use"
            );

            // An invalid rule is refused at proposal time, with no token.
            let bad = json!({
                "project_id": project_id,
                "expression": {"combinator": "all", "children": [
                    {"kind": "condition", "field": "ip", "op": "in_cidr", "values": ["nope"]}
                ]},
                "action": {"type": "block", "status": 403}
            });
            assert!(matches!(
                execute("create_edge_rule", &bad, true, &ctx).await,
                Err(McpError::EdgeRuleRejected { .. })
            ));

            // Enforcing is its own proposal.
            let mode_args =
                json!({"project_id": project_id, "rule_id": rules[0].id, "mode": "enforce"});
            let proposed = execute("set_edge_rule_mode", &mode_args, true, &ctx)
                .await
                .expect("proposes");
            let token = proposed.proposal_token.clone().expect("a token");
            // The handler confirms with the stored proposal, which carries the
            // revision it was made against.
            let stored = proposals.peek(&token).expect("stored").arguments;
            confirm(&token, "set_edge_rule_mode", &stored, &ctx)
                .await
                .expect("confirms");
            let rule = service.get(project_id, rules[0].id).await.expect("get");
            assert_eq!(rule.mode, Mode::Enforce);

            let ops = match recorder.0.lock() {
                Ok(ops) => ops.clone(),
                Err(poisoned) => poisoned.into_inner().clone(),
            };
            assert_eq!(ops, vec!["EDGE_RULE_CREATED", "EDGE_RULE_UPDATED"]);
        }

        fn log_admin() -> CreateEdgeRuleRequest {
            serde_json::from_value(json!({
                "name": "Watch admin",
                "expression": {"combinator": "all", "children": [
                    {"kind": "condition", "field": "path", "op": "starts_with",
                     "values": ["/admin"]}
                ]},
                "action": {"type": "block", "status": 403}
            }))
            .expect("valid rule")
        }

        #[tokio::test]
        async fn confirming_applies_only_to_the_reviewed_revision() {
            let Some((db, project_id)) = seeded().await else {
                return;
            };
            let service = Arc::new(EdgeRuleService::new(db.connection_arc()));
            let proposals = Arc::new(ProposalStore::new());
            let audit: Arc<dyn AuditLogger> = Arc::new(RecordingAudit::default());
            let actor = AuditActor {
                user_id: 1,
                ip_address: None,
                user_agent: "test".into(),
            };
            let auth = session(Role::User);
            let ctx = EdgeRuleExecCtx {
                service: &service,
                proposals: &proposals,
                audit_service: &audit,
                actor: &actor,
                auth: &auth,
                checker: None,
            };
            let rule = service
                .create(project_id, log_admin(), None)
                .await
                .expect("create");
            let stored = |token: &str| {
                proposals
                    .peek(token)
                    .map(|p| p.arguments)
                    .expect("stored proposal")
            };

            // Enforcing: the proposal is bound to revision 1, someone edits
            // the rule, and confirming refuses to enforce the edited rule.
            let mode_args =
                json!({"project_id": project_id, "rule_id": rule.id, "mode": "enforce"});
            let token = execute("set_edge_rule_mode", &mode_args, true, &ctx)
                .await
                .expect("proposes")
                .proposal_token
                .expect("a token");
            let bound = stored(&token);
            assert_eq!(bound["revision"], 1);
            service
                .update(
                    project_id,
                    rule.id,
                    serde_json::from_value(json!({"name": "Edited elsewhere"})).expect("update"),
                    None,
                )
                .await
                .expect("edit");
            let refused = confirm(&token, "set_edge_rule_mode", &bound, &ctx).await;
            assert!(
                matches!(&refused, Err(McpError::EdgeRuleRejected { reason }) if reason.contains("revision 1")),
                "{refused:?}"
            );
            let after = service.get(project_id, rule.id).await.expect("get");
            assert_eq!(after.mode, Mode::LogOnly, "the edited rule is not enforced");

            // Deleting: same, and the rule survives.
            let delete_args = json!({"project_id": project_id, "rule_id": rule.id});
            let token = execute("delete_edge_rule", &delete_args, true, &ctx)
                .await
                .expect("proposes")
                .proposal_token
                .expect("a token");
            let bound = stored(&token);
            assert_eq!(bound["revision"], after.revision);
            service
                .update(
                    project_id,
                    rule.id,
                    serde_json::from_value(json!({"enabled": false})).expect("update"),
                    None,
                )
                .await
                .expect("edit");
            assert!(matches!(
                confirm(&token, "delete_edge_rule", &bound, &ctx).await,
                Err(McpError::EdgeRuleRejected { .. })
            ));
            service.get(project_id, rule.id).await.expect("still there");

            // A revision the caller already knows is stale is refused up front.
            let stale = json!({"project_id": project_id, "rule_id": rule.id, "revision": 1, "enabled": true});
            assert!(matches!(
                execute("update_edge_rule", &stale, true, &ctx).await,
                Err(McpError::EdgeRuleRejected { .. })
            ));
        }

        /// A team-gated project where the caller is a viewer.
        struct ViewerChecker;

        #[async_trait::async_trait]
        impl ProjectAccessChecker for ViewerChecker {
            async fn user_can_access_project(
                &self,
                _user_id: i32,
                _project_id: i32,
            ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
                Ok(true)
            }

            async fn effective_project_permissions(
                &self,
                _user_id: i32,
                _project_id: i32,
            ) -> Result<Option<Vec<String>>, Box<dyn std::error::Error + Send + Sync>> {
                Ok(Some(vec![
                    "projects:read".to_string(),
                    "edge_rules:read".to_string(),
                ]))
            }
        }

        #[tokio::test]
        async fn a_project_viewer_cannot_propose_or_confirm_writes() {
            let Some((db, project_id)) = seeded().await else {
                return;
            };
            let service = Arc::new(EdgeRuleService::new(db.connection_arc()));
            let proposals = Arc::new(ProposalStore::new());
            let audit: Arc<dyn AuditLogger> = Arc::new(RecordingAudit::default());
            let actor = AuditActor {
                user_id: 1,
                ip_address: None,
                user_agent: "test".into(),
            };
            let rule = service
                .create(project_id, log_admin(), None)
                .await
                .expect("create");
            let args = json!({"project_id": project_id, "rule_id": rule.id});

            // An instance admin proposes; the token is then offered to a
            // viewer, whose instance role carries edge_rules:write.
            let admin = session(Role::Admin);
            let viewer = session(Role::User);
            assert!(viewer.has_permission(&Permission::EdgeRulesWrite));
            let checker = ViewerChecker;
            let ctx = |auth| EdgeRuleExecCtx {
                service: &service,
                proposals: &proposals,
                audit_service: &audit,
                actor: &actor,
                auth,
                checker: Some(&checker as &dyn ProjectAccessChecker),
            };
            let token = execute("delete_edge_rule", &args, true, &ctx(&admin))
                .await
                .expect("an admin proposes")
                .proposal_token
                .expect("a token");

            execute("list_edge_rules", &args, true, &ctx(&viewer))
                .await
                .expect("viewers read");
            assert!(matches!(
                execute("delete_edge_rule", &args, true, &ctx(&viewer)).await,
                Err(McpError::ProjectPermissionDenied { .. })
            ));
            let bound = proposals.peek(&token).expect("stored").arguments;
            assert!(matches!(
                confirm(&token, "delete_edge_rule", &bound, &ctx(&viewer)).await,
                Err(McpError::ProjectPermissionDenied { .. })
            ));
            service.get(project_id, rule.id).await.expect("not deleted");
            assert!(
                proposals.peek(&token).is_ok(),
                "a denied confirmation leaves the token for the right confirmer"
            );
        }

        #[tokio::test]
        async fn readers_can_list_but_not_propose() {
            let Some((db, project_id)) = seeded().await else {
                return;
            };
            let service = Arc::new(EdgeRuleService::new(db.connection_arc()));
            let proposals = Arc::new(ProposalStore::new());
            let audit: Arc<dyn AuditLogger> = Arc::new(RecordingAudit::default());
            let actor = AuditActor {
                user_id: 1,
                ip_address: None,
                user_agent: "test".into(),
            };
            let auth = session(Role::Reader);
            let ctx = EdgeRuleExecCtx {
                service: &service,
                proposals: &proposals,
                audit_service: &audit,
                actor: &actor,
                auth: &auth,
                checker: None,
            };
            let args = json!({"project_id": project_id});
            execute("list_edge_rules", &args, true, &ctx)
                .await
                .expect("readers list");
            let err = execute(
                "delete_edge_rule",
                &json!({"project_id": project_id, "rule_id": 1}),
                true,
                &ctx,
            )
            .await
            .expect_err("readers cannot write");
            assert!(matches!(err, McpError::InsufficientPermission { .. }));
        }
    }

    #[test]
    fn service_errors_map_to_specific_mcp_errors() {
        assert!(matches!(
            McpError::from(EdgeRuleServiceError::RuleNotFound {
                project_id: 1,
                rule_id: 2
            }),
            McpError::EdgeRuleNotFound { rule_id: 2, .. }
        ));
        assert!(matches!(
            McpError::from(EdgeRuleServiceError::TooManyRules {
                project_id: 1,
                max: 100
            }),
            McpError::EdgeRuleRejected { .. }
        ));
    }
}
