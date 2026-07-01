<!--
SCOPE: AI tooling — expose the REST API to the LLM as read-only tools.
-->

# ADR-024: REST API as read-only LLM tools (generic invocation, permission-gated)

**Status:** Proposed
**Date:** 2026-06-29
**Author:** David Viejo

## Context

The AI assistant can only do what its tools let it do. Today every tool is
hand-written:

- **EE** (`temps-ee-sre`) uses **rig-core 0.38**; the agent registers 6 bespoke
  `Tool` impls (`query_traces`, `get_trace`, `list_errors`, `get_error_detail`,
  `search_logs`, `fetch_incident_context`) in `build_agent()`
  (`crates/temps-ee-sre/src/agent.rs`, `tools.rs`).
- **OSS** (`temps-ai-chat`) uses its own substrate — `temps_ai::ChatTool`
  (`crates/temps-ai/src/streaming.rs`) surfaced per context via a provider
  (`ContextProvider::tools()` / `execute_tool()` in `provider.rs`), e.g. the
  trace tools.

This does not scale. Temps already exposes ~**757** REST operations (~**399**
read-only `GET`s) across 55+ crates, each with a typed request/response. Wiring
every capability as a bespoke tool is:

1. **Expensive in context** — every tool's JSON schema is sent to the model on
   every turn. A few hundred tools would dominate the prompt budget.
2. **Expensive in code** — each tool is a struct + args + schema + executor,
   hand-maintained against an API that already exists and already has typed
   handlers, permissions, and audit.

The data is already reachable via the REST API, with authn/authz, project
scoping, secret masking, and audit already enforced at the handler layer. We
want the LLM to use that surface directly, **read-only for now**, **gated by the
caller's permissions**, and **identical across OSS and EE**.

### Rejected alternatives

- **Generate one tool per operation (~399 tools).** Solves "hand-written" but
  makes the context-cost problem worse, not better. Rejected.
- **Re-implement authz in the AI layer.** Duplicates `permission_guard!` /
  scoping / masking and will drift from the real handlers — a security
  liability. Rejected: the existing handlers must remain the single source of
  truth.
- **Unify the OSS and EE tool loops onto one substrate.** A large, risky
  refactor of two shipping systems, out of scope for this feature. Rejected in
  favour of a shared *core* with thin per-substrate adapters (below).

## Decision

**Expose the REST API to the LLM through a small set of generic meta-tools
backed by one shared `InternalApiCaller`, which executes each call by replaying
a synthetic request through the real Axum router with the caller's
`AuthContext` injected.** The router — not new code — enforces security.

### 1. The generic tool surface (what the model sees)

Not the 399 endpoints. Just:

- `search_api(query)` → ranked read-only operations the caller may use:
  `{operation_id, method, path, summary, params[]}` (params compact: name, in,
  required, type, enum).
- `describe_api(operation_id)` → the full parameter + response JSON schema for
  one operation, fetched only when needed.
- `call_api(operation_id, parameters)` → executes the `GET` and returns the
  (capped) JSON body.

The model loops *search → (describe) → call → reason*. Per-endpoint token cost is
zero until an endpoint is actually searched/called.

### 2. `InternalApiCaller` — the shared core (new, OSS)

A single substrate-agnostic component (new crate `temps-ai-api-tools`, or a
module in `temps-ai`) holding:

- the in-process `axum::Router` (from `PluginManager::build_split_application()`),
- the read-only **OpenAPI view** (filter `get_unified_openapi()` to `GET`
  operations, minus a denylist of streaming/heavy GETs),
- the **discovery permission filter** + **project-scope** policy.

API:

```rust
pub struct ApiCallScope {
    pub auth: AuthContext,           // the caller — injected, never from the model
    pub project_ids: Vec<i32>,       // accessible projects resolved for this turn
}

impl InternalApiCaller {
    fn search(&self, query: &str, scope: &ApiCallScope) -> Vec<OperationSummary>;
    fn describe(&self, operation_id: &str) -> Option<OperationSchema>;
    async fn call(&self, operation_id: &str, params: serde_json::Value,
                  scope: &ApiCallScope) -> Result<ApiToolResponse, ApiToolError>;
}
```

`call()`:
1. Look up the operation (must be a known, read-only, non-denylisted `GET`).
2. **Validate + route params** against the OpenAPI `Operation.parameters`:
   split the flat `parameters` object into path vs query by each param's `in`;
   check required/type/enum; on mismatch return a structured error so the model
   self-corrects in one round-trip.
3. **Inject server-controlled context:** any `project_id` param is validated ∈
   `scope.project_ids` (or auto-filled when the scope is a single project);
   `limit`/pagination defaulted and clamped; time-window defaulted.
4. Build the `axum::Request` (method, substituted path, query string), insert
   `scope.auth` into `req.extensions_mut()`, and run it via
   `tower::ServiceExt::oneshot(router)`.
5. Cap the response body size, run a defensive secret-redaction pass, return.

### 3. Parameter passing

The per-operation schema is **derived from utoipa**, never hand-maintained.
`search_api` returns a compact param list; `describe_api` returns the full
schema on demand. The model passes one flat `parameters` object
(`{name: value}`); the caller routes path-vs-query, validates, and injects the
context the model must not supply (auth, project scope, pagination, time
window). The model therefore carries almost no per-endpoint knowledge.

### 4. Security model — layered, enforcement = the router

1. **Enforcement (non-bypassable):** execution through the real router runs
   `permission_guard!`, `project_scope_guard!`, secret-masking DTOs, and audit
   unchanged. A tool can never exceed what the caller could do via the API.
2. **Discovery filter (advisory, UX):** `search_api` only surfaces operations
   whose required permission ∈ the caller's set
   (`AuthContext::has_permission` over `Role::permissions()` / custom perms,
   filtered to `:read`). MVP derives the required permission from the operation
   **tag** (coarse heuristic); Phase 2 adds a generated `operation_id →
   Permission` map. The map is *never* the security boundary — only what to show.
3. **Project scoping (stricter than the bare API):** the current API does not
   auto-scope a *user* to their projects. The AI layer is deliberately stricter
   — every `project_id` parameter is constrained to the caller's accessible
   projects (reusing the EE companion's `accessible_projects`; an equivalent
   helper in core for OSS).
4. **No identity from the model:** auth/project are injected from the caller's
   resolved scope, same trust boundary as today's tools.
5. **Read-only:** `GET` only this phase.

### 5. Per-substrate adapters (this is what "both the same" means)

The core is shared and identical; only thin registration glue differs:

- **OSS** (`temps-ai-chat`): a provider exposes the three tools as
  `temps_ai::ChatTool` and routes `execute_tool(name, args)` into
  `InternalApiCaller`.
- **EE** (`temps-ee-sre`): three rig `Tool` impls wrap the same
  `InternalApiCaller` and register in `build_agent()` (alongside or replacing
  the bespoke tools).

Both call the identical core → identical behaviour and security; ~a few lines
of glue each. No loop unification required.

### 6. Guardrails

Read-only; per-turn call budget + wall-clock timeout; response size cap +
default/clamped pagination (keeps list endpoints from flooding context);
defensive redaction; AI-initiated calls tagged in audit; denylist for
streaming/heavy GETs.

## Consequences

### Positive
- Whole read-only API reachable with ~3 tools — flat, tiny context cost.
- Almost no per-endpoint code; new endpoints are usable automatically.
- Security is the existing handler stack, not duplicated logic.
- One core, identical in OSS and EE.

### Negative / risks
- **Required permission isn't in the OpenAPI** → discovery filter is heuristic
  until the generated map lands (mitigated: runtime guard is the real boundary).
- **Response size** can balloon context if caps/pagination are weak — must be
  enforced hard.
- **Search quality** (keyword/tag MVP) may surface imperfect matches; embeddings
  over summaries in Phase 2.
- **operation_id** stability/collisions — audit for duplicates.
- Router replay couples the tool to in-process app assembly (acceptable; same
  process).

### Neutral
- Writes (POST/PUT/PATCH/DELETE) are a deliberate later phase with extra
  guardrails (human confirmation, idempotency, louder audit).

## Phased plan

1. **Phase 1 (MVP):** `InternalApiCaller` core (router replay + read-only
   OpenAPI filter + param routing/validation + project-scope guard + caps) +
   the two/three meta-tools + the OSS `ChatTool` adapter and the EE rig adapter.
   Discovery filter via tag heuristic. Unit + integration tests (a known GET
   end-to-end; a forbidden op returns 403 via the real guard).
2. **Phase 2:** generated `operation_id → Permission` map (parse handlers, test
   gate); embeddings-backed search; denylist tuning; eval on real questions.
3. **Phase 3 (writes, opt-in):** mutating ops gated by write permissions + a
   human-confirmation step + idempotency + audit.

## Open questions

- Home: new `temps-ai-api-tools` crate vs a module in `temps-ai`. (Leaning new
  crate to avoid a heavy dep — router/openapi — on `temps-ai`.)
- How the `InternalApiCaller` obtains the `Router` handle at agent-build time
  (pass from `AppState`/plugin context).
- OSS accessible-projects helper location (lift the EE `accessible_projects`
  logic into core).

## References

- ADR-022 (AI foundation, structured output), ADR-023 (AI debugging
  conversations).
- `crates/temps-core/src/plugin.rs` — `get_unified_openapi()`,
  `build_split_application()`.
- `crates/temps-auth/src/{permissions.rs,context.rs,permission_guard.rs}`.
- `crates/temps-ai/src/streaming.rs` — `ChatTool`; `temps-ee-sre/src/{agent,tools,scope}.rs`.
