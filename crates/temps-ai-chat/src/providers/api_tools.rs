//! The read-only `temps` virtual-CLI tool for the OSS debugging chat (ADR-024).
//!
//! Exposes a single [`temps_ai::ChatTool`] — `temps` — backed by
//! [`temps_ai_api_tools::InternalApiCaller`] via the shared
//! [`temps_ai_api_tools::ApiToolsHandle`]. The model drives it like a CLI:
//! `--help` lists sections (OpenAPI tags), `<section> --help` lists operations,
//! `<section> <operation> --flag value …` runs one. Discovery (`--help`) is a
//! pure index query; running an operation replays the underlying **GET** through
//! the real Axum router and returns the (capped) JSON body as
//! `{operation, status, data}`.
//!
//! ## Auth threading
//!
//! Running an operation replays a GET through the real router, so it needs the
//! caller's `AuthContext` to build the `ApiCallScope` injected into the request
//! (which `permission_guard!` then evaluates). The auth is threaded via
//! [`ConversationContextProvider::execute_tool_with_auth`] (a non-breaking trait
//! method that defaults to the auth-less `execute_tool`): the chat handler
//! forwards the user's `AuthContext` into `ConversationService::send_message`,
//! which passes it through the tool loop to this provider. The call is scoped to
//! the conversation's project, so the model is bounded by the user's own
//! permissions and cannot reach another tenant's data.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use temps_ai::ChatTool;
use temps_ai_api_tools::{ApiCallScope, ApiToolsHandle};
use temps_auth::context::AuthContext;

use crate::provider::{ConversationContextProvider, ConversationSeed};

/// A `ConversationContextProvider` that adds the single ADR-024 `temps`
/// virtual-CLI tool to the tool loop — a `--help`-driven command line over the
/// read-only API index (replacing the old `search_api`/`describe_api`/`call_api`
/// trio, which LLMs handled poorly).
///
/// This is a *supplement*, not a replacement: it is merged on top of whatever
/// tools the primary context provider supplies (deployment debug, project
/// assistant, etc.) by adding it to the provider list in the plugin.
pub struct ApiToolsProvider {
    handle: Arc<ApiToolsHandle>,
}

impl ApiToolsProvider {
    pub fn new(handle: Arc<ApiToolsHandle>) -> Self {
        Self { handle }
    }
}

/// JSON Schema for the `temps` virtual-CLI tool.
fn temps_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "required": ["command"],
        "properties": {
            "command": {
                "type": "string",
                "description": "A read-only Temps CLI command line. \
                                Discovery is `--help`-driven: `--help` lists sections; \
                                `<section> --help` lists that section's operations; \
                                `<section> <operation> --help` shows an operation's flags. \
                                Run an operation with `<section> <operation> --flag value …` \
                                (e.g. `deployments get_last_deployment`, or \
                                `audit-logs list_audit_logs --limit 20`). \
                                project_id is auto-filled for the current project — never pass it. \
                                Pass only flags you have a real value for; omit optional filters \
                                rather than inventing placeholders."
            }
        },
        "additionalProperties": false
    })
}

#[async_trait]
impl ConversationContextProvider for ApiToolsProvider {
    fn context_type(&self) -> &'static str {
        // This provider is special — it augments ALL contexts, not a specific
        // context_type.  We use a sentinel that is never stored in a conversation
        // row; the service merges this provider's tools with others at runtime.
        // The value is intentionally internal; no route produces this context_type.
        "__api_tools__"
    }

    async fn seed(&self, _project_id: i32, _context_id: &str) -> Option<ConversationSeed> {
        // This provider has no seed — it only contributes tools.
        None
    }

    /// Seed every chat's system framing with the `temps` CLI's root help — the
    /// section list — so the model starts oriented and can drill in with
    /// `<section> --help`. Returns `None` until the caller is wired (startup).
    fn system_appendix(&self) -> Option<String> {
        let caller = self.handle.get()?;
        let root_help = caller.cli_root_help();
        if root_help.trim().is_empty() {
            return None;
        }
        Some(format!(
            "## The `temps` read-only API CLI\n\
             You have a `temps` tool: a read-only command line over the platform API. Discover \
             with `--help` (`<section> --help` → operations; `<section> <operation> --help` → \
             flags), then run `<section> <operation> --flag value …`. Below is `temps --help` \
             (the sections). Drill into the relevant one rather than guessing.\n\n```\n{root_help}```"
        ))
    }

    async fn tools(&self, _project_id: i32, _context_id: &str) -> Vec<ChatTool> {
        vec![ChatTool {
            name: "temps".to_string(),
            description: "Read-only Temps CLI over the platform API. Use `--help` to discover \
                (sections → operations → flags), then run `<section> <operation> --flag value …`. \
                project_id is auto-filled — never pass it. Returns help text or the endpoint's \
                JSON body. If a call errors, read the message and adjust; don't repeat it unchanged."
                .to_string(),
            parameters: temps_schema(),
        }]
    }

    async fn execute_tool(
        &self,
        _project_id: i32,
        _context_id: &str,
        name: &str,
        _arguments: &str,
    ) -> String {
        // The `temps` tool needs the user's auth to replay GETs through the
        // router. The service always dispatches API tools via
        // `execute_tool_with_auth`, so this auth-less path is a defensive
        // fallback only. `--help` would work here, but execution would not, so
        // surface the wiring error rather than silently degrading.
        match name {
            "temps" => "The `temps` tool requires an authenticated context and was invoked \
                without one. This is an internal wiring error — retry the request."
                .to_string(),
            other => format!("Unknown tool '{other}'. The only API tool is `temps`."),
        }
    }

    /// Auth-aware dispatch. `temps` replays GETs through the router with the
    /// user's `AuthContext`, so the model is bounded by the user's own
    /// permissions; help/discovery needs no auth but flows through the same path.
    async fn execute_tool_with_auth(
        &self,
        project_id: i32,
        _context_id: &str,
        name: &str,
        arguments: &str,
        auth: &AuthContext,
    ) -> String {
        match name {
            "temps" => self.exec_cli(arguments, project_id, auth).await,
            other => format!("Unknown tool '{other}'. The only API tool is `temps`."),
        }
    }
}

impl ApiToolsProvider {
    /// Execute the `temps` virtual CLI: parse the command and either return help
    /// text or replay the resolved GET through the router with the user's auth.
    ///
    /// Security: execution is scoped to `project_ids: [project_id]` (the
    /// conversation's project, never a value the model supplied) and carries the
    /// user's own `AuthContext`, so `permission_guard!`/`project_scope_guard!`
    /// bound the model to exactly what the user could read — it cannot escalate
    /// or reach another tenant's data. Failures come back as readable text so the
    /// model can recover rather than loop.
    async fn exec_cli(&self, arguments: &str, project_id: i32, auth: &AuthContext) -> String {
        let caller = match self.handle.get() {
            Some(c) => c,
            None => {
                return "The Temps API CLI is not yet available (startup in progress).".to_string()
            }
        };

        let args: Value = match serde_json::from_str(arguments) {
            Ok(v) => v,
            Err(e) => return format!("Invalid `temps` arguments (not JSON): {e}"),
        };
        let command = match args.get("command").and_then(|v| v.as_str()) {
            Some(c) => c,
            None => {
                return "The `temps` tool requires a 'command' string. Try `--help` to list \
                        sections."
                    .to_string()
            }
        };

        let scope = ApiCallScope {
            auth: auth.clone(),
            project_ids: vec![project_id],
        };
        caller.run_cli(command, &scope).await
    }
}

// Note: Display for ParamLocation is defined in temps_ai_api_tools::index.
