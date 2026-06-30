//! Per-context seeding for conversations.
//!
//! A `ConversationContextProvider` turns an entity reference (`context_type` +
//! `context_id`) into the AI framing for a new chat. Registered one per
//! `context_type`; the deployment provider (in `temps-deployments`) is the
//! first, seeding from a build/deploy failure diagnosis.

use async_trait::async_trait;

use temps_ai::ChatTool;
use temps_auth::context::AuthContext;

/// Shared behavioural guidance appended to every conversation's system framing,
/// right after each context's role preamble and before its specific facts.
///
/// Every context (project / deployment / alert) merges the same read-only tools
/// — distributed-trace inspection plus the generic API meta-tools (`search_api`,
/// `describe_api`, `call_api`) — so the rules for *using* them, and the
/// act-don't-ask / don't-fabricate behaviours, live here once instead of being
/// re-stated (and drifting) in each provider.
pub const TOOL_USAGE_GUIDANCE: &str = "\
## Grounding
Answer from tool results, not assumptions. If a claim can be checked with a tool, check it first. Cite concrete values you actually retrieved (ids, counts, timestamps, statuses), and say plainly when the data does not show something. Never invent facts, ids, or numbers.

## Querying the platform API (the `temps` CLI)
You have read-only access to the platform's REST API through a single `temps`
tool — a command line you drive like any CLI:
- `temps --help` → the list of sections (also shown below in this prompt).
- `temps <section> --help` → the operations in a section.
- `temps <section> <operation> --help` → an operation's description and flags.
- `temps <section> <operation> --flag value …` → run it and get the JSON back.
Use `--help` to navigate to the right operation; read an operation's `--help`
when two look similar (the description disambiguates them, e.g. which one is the
deployment \"stages\"/jobs) or when you're unsure of its flags. Prefer the most
specific operation (e.g. a get-last-deployment over a list).

## Running commands
Pass only flags you have a real, meaningful value for. OMIT every other flag — including filters — so the endpoint applies its real defaults (no filter = everything). NEVER fabricate placeholder values such as empty strings, \"all\", 0, or sentinel timestamps to satisfy a flag: that returns wrong or empty results. Authentication, `project_id`, and pagination are handled for you — never pass `project_id`. If a command errors, READ the message and adjust (or check its `--help`) — never re-run the same command unchanged.

## Acting, not asking
When the user asks for read-only data you can fetch, just fetch it with sensible defaults and report the result. Do not ask permission to call a read-only endpoint, and do not ask the user to confirm obvious defaults — act, then state the defaults you used. Ask a brief clarifying question only when the request is genuinely ambiguous and you truly cannot proceed.

## Output
Be concise and practical: lead with the answer, then supporting detail. Use compact lists or tables for multi-row data.";

/// The seed for a new conversation.
#[derive(Debug, Clone, Default)]
pub struct ConversationSeed {
    /// System framing: the situation + relevant facts (logs, status). Stored as
    /// the conversation's first `system` message and replayed every turn.
    pub system: String,
    /// Optional first assistant turn shown on open (e.g. the rendered diagnosis),
    /// so the chat starts already explaining the problem.
    pub first_assistant: Option<String>,
    /// Display title for the conversation.
    pub title: Option<String>,
    /// Provenance refs (log_ids, status) recorded on the conversation row.
    pub metadata: Option<serde_json::Value>,
}

/// Builds the AI context for one kind of entity.
#[async_trait]
pub trait ConversationContextProvider: Send + Sync {
    /// The `context_type` this provider handles, e.g. `"deployment"`.
    fn context_type(&self) -> &'static str;

    /// Finer-grained authorization for this context (the route already enforces
    /// project-level access). Default allow.
    async fn authorize(&self, _project_id: i32, _context_id: &str) -> bool {
        true
    }

    /// Build the seed for a new conversation. `None` if the entity can't be found
    /// or has no usable context (e.g. a deployment that didn't fail).
    async fn seed(&self, project_id: i32, context_id: &str) -> Option<ConversationSeed>;

    /// Optional extra text appended to the conversation's system framing on every
    /// turn, regardless of which provider seeded the chat. Used by the API-tools
    /// provider to inject the read-only endpoint catalogue (the "API map") so the
    /// model can pick an `operation_id` by path instead of guessing search
    /// keywords. Default: nothing.
    fn system_appendix(&self) -> Option<String> {
        None
    }

    /// Tools the model may call while debugging this context — e.g. read a file
    /// from the project's repository via the configured Git provider. Default:
    /// none. Context-aware so a provider offers a tool only when the underlying
    /// entity supports it (e.g. only git-backed deployments expose repo tools).
    /// When this returns empty, the chat uses plain streaming with no tool loop.
    async fn tools(&self, _project_id: i32, _context_id: &str) -> Vec<ChatTool> {
        Vec::new()
    }

    /// Execute a tool the model requested. `arguments` is the raw JSON string the
    /// model emitted. Returns a string fed back to the model — surface failures
    /// as readable text (e.g. "file not found"), never as an error, so the model
    /// can recover and try another path.
    async fn execute_tool(
        &self,
        _project_id: i32,
        _context_id: &str,
        name: &str,
        _arguments: &str,
    ) -> String {
        format!("Tool '{name}' is not available in this context.")
    }

    /// Like [`Self::execute_tool`], but with the calling user's [`AuthContext`]
    /// available. Tools that replay authenticated API calls (e.g. `call_api`)
    /// override this to scope the call to the user's own permissions — the
    /// router's `permission_guard!` then bounds the model to exactly what the
    /// user could read. The default ignores auth and delegates to
    /// [`Self::execute_tool`], so non-API providers need not implement it.
    async fn execute_tool_with_auth(
        &self,
        project_id: i32,
        context_id: &str,
        name: &str,
        arguments: &str,
        _auth: &AuthContext,
    ) -> String {
        self.execute_tool(project_id, context_id, name, arguments)
            .await
    }
}
