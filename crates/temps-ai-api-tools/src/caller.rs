//! [`InternalApiCaller`] — executes API calls via Axum router replay.
//!
//! ## How it works
//!
//! 1. [`build_request_parts`] validates and routes the flat `params` object into
//!    a substituted path, a URL-encoded query string, and an optional JSON body
//!    (pure, no I/O).
//! 2. [`InternalApiCaller::call`] builds an `axum::http::Request`, injects the
//!    caller's [`AuthContext`] into `req.extensions_mut()`, and runs
//!    `tower::ServiceExt::oneshot(router.clone())`.  The router enforces authz;
//!    this crate only gates on project-id scoping before the call.
//!
//! ## Security model
//!
//! - Auth/project are **injected** from the resolved scope, never from the model.
//! - The router's `permission_guard!` is the real enforcement boundary.
//! - `build_request_parts` adds a *client-side* project-scope check so the LLM
//!   cannot probe projects it has no access to (advisory — not the security
//!   boundary).
//! - Response bodies are capped at `max_response_bytes` to prevent context
//!   flooding.
//! - The write allowlist enforces opt-in: a mutating operation is never callable
//!   unless it has been explicitly added to the allowlist.

use crate::{
    error::ApiToolError,
    index::{
        ApiOperation, ObjectShape, OperationSchema, OperationSummary, ParamLocation, ParamSpec,
        ReadOnlyApiIndex,
    },
};
use axum::body::Body;
use http::Request;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use temps_auth::context::AuthContext;
use temps_auth::permissions::Permission;
use tower::ServiceExt as _;
use tracing::{debug, warn};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// The authentication + project-scoping context for a single tool invocation.
/// This is supplied by the substrate adapter (OSS `ChatTool`, EE rig `Tool`),
/// never derived from anything the model says.
#[derive(Debug, Clone)]
pub struct ApiCallScope {
    /// Resolved auth context for the caller.  This is inserted into the Axum
    /// request extensions so the router's `permission_guard!` can evaluate it.
    pub auth: AuthContext,
    /// Accessible project IDs for this caller in this turn.  Used for the
    /// client-side project-scope guard in [`build_request_parts`].
    /// Empty means "no project constraint" (admin-level calls).
    pub project_ids: Vec<i32>,
}

/// The result of the pure param-routing step.
#[derive(Debug, Clone)]
pub struct BuiltRequest {
    /// Substituted path, e.g. `/projects/42/deployments`.
    pub path: String,
    /// URL-encoded query string (without leading `?`), e.g. `limit=20&status=running`.
    /// Empty string when there are no query params.
    pub query: String,
    /// JSON body for write operations.  `None` for GET operations or write
    /// operations with no body fields.  When `Some`, the caller must set
    /// `Content-Type: application/json`.
    pub body: Option<serde_json::Value>,
}

/// The response returned from [`InternalApiCaller::call`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiToolResponse {
    /// HTTP status code returned by the router.
    pub status: u16,
    /// Parsed JSON body, or a `serde_json::Value::String` containing raw text if the
    /// body is not valid JSON.
    pub body: Value,
    /// `true` when the response body was truncated because it exceeded
    /// `max_response_bytes`.
    pub truncated: bool,
}

/// A validated, structured write proposal produced by
/// [`InternalApiCaller::prepare_write_cli`].
///
/// This is NOT an execution — it is a snapshot of what would be executed if a
/// human confirms.  The `params` field contains the validated flat parameter
/// object the model supplied; the confirm endpoint passes it directly to
/// [`InternalApiCaller::execute_write`].
#[derive(Debug, Clone, Serialize)]
pub struct PreparedWrite {
    /// The `operation_id` of the operation to execute.
    pub operation_id: String,
    /// Uppercase HTTP method, e.g. `"POST"`.
    pub method: String,
    /// URL path template (un-substituted), e.g. `/projects/{id}/deployments`.
    pub path: String,
    /// Human-readable one-liner, e.g.
    /// `"POST /projects/{id}/deployments — redeploy_deployment"`.
    pub summary: String,
    /// The flat parameter object the model supplied (validated), persisted for
    /// later execution.
    pub params: serde_json::Value,
    /// The advisory write permission required to confirm this action, derived
    /// from the operation's OpenAPI tag + HTTP method at prepare time.
    /// Stored on the `ai_pending_actions` row so the confirm handler can
    /// pre-check it before claiming the row atomically.
    pub required_permission: Option<String>,
}

/// Outcome of [`InternalApiCaller::prepare_write_cli`].
pub enum WritePrepareOutcome {
    /// Discovery/help text to return to the model verbatim (e.g. `--help` was used).
    Help(String),
    /// A readable validation/error message for the model (e.g. bad enum, missing param).
    Invalid(String),
    /// The write was validated successfully and can be confirmed for execution.
    Prepared(PreparedWrite),
}

// ---------------------------------------------------------------------------
// build_request_parts — pure, no I/O
// ---------------------------------------------------------------------------

/// Is this parameter the project selector for the operation — i.e. should it be
/// auto-filled from the chat's project scope rather than supplied by the model?
///
/// Two shapes count:
/// - a parameter literally named `project_id` (anywhere), and
/// - a path parameter named `id` that is the leading `/projects/{id}` segment
///   (a very common route shape, e.g. `/projects/{id}/last-deployment`, where
///   `id` is the project's own id).
///
/// Restricted to id-named params so a string `/projects/{slug}` selector is NOT
/// filled with a numeric project id.
pub(crate) fn is_project_scope_param(op: &ApiOperation, param: &ParamSpec) -> bool {
    if param.name == "project_id" {
        return true;
    }
    matches!(param.location, ParamLocation::Path)
        && param.name == "id"
        && op.path.starts_with("/projects/{id}")
}

/// Route a flat `params` JSON object into a substituted path, query string, and
/// optional JSON body.
///
/// This function is pure (no I/O) and can be unit-tested without a running router.
///
/// ## Parameter handling
///
/// - **Path params** (`ParamLocation::Path`): substituted into `{name}` placeholders
///   in the path template.
/// - **Query params** (`ParamLocation::Query`): appended as `key=value` pairs
///   (URL-encoded).
/// - **Body params** (`ParamLocation::Body`): collected into a JSON object and
///   returned in `BuiltRequest::body`.
/// - **Required check**: `ApiToolError::MissingParam` if a required *path* or
///   *body* param is absent.  Absent *query* params are forwarded as-is
///   regardless of their (often-unreliable) `required` flag; the real router is
///   the source of truth and returns a real 4xx if needed.
/// - **Enum check**: `ApiToolError::BadEnum` if a value is not in `param.enum_values`.
/// - **Project scoping**: if a param is named `project_id` (exact match) and
///   `allowed_project_ids` is non-empty, the value is validated ∈
///   `allowed_project_ids`.  If the param is absent and `allowed_project_ids.len() == 1`,
///   it is auto-filled with the single accessible project.
/// - **Limit injection**: if a param named `limit` exists in the operation:
///   - absent → `default_limit` is injected.
///   - present → clamped to `[1, max_limit]`.
///
/// ## Parameters
///
/// - `op`: the operation whose params define the routing rules.
/// - `params`: a flat JSON object `{"param_name": value, …}`.
/// - `allowed_project_ids`: the caller's accessible project IDs (from
///   [`ApiCallScope::project_ids`]).  Empty slice = no constraint applied.
/// - `default_limit`: default page size to inject when `limit` is absent.
/// - `max_limit`: upper bound that `limit` is clamped to.
pub fn build_request_parts(
    op: &ApiOperation,
    params: &Value,
    allowed_project_ids: &[i32],
    default_limit: i64,
    max_limit: i64,
) -> Result<BuiltRequest, ApiToolError> {
    let params_obj = params.as_object();

    // Pre-pass: reject parameters this operation does not have.
    //
    // Silently ignoring them is the dangerous option. A near-miss on a filter
    // name (`--metric` for `--metric_name`) used to produce a 200 carrying the
    // *unfiltered* result — an average across every metric in the project,
    // which looks like a real number and is not. A wrong answer that cannot be
    // distinguished from a right one is worse than no answer, so this fails
    // loudly and says nothing was queried.
    if let Some(obj) = params_obj {
        let unknown: Vec<&str> = obj
            .keys()
            .map(String::as_str)
            .filter(|key| !op.params.iter().any(|p| p.name == *key))
            .collect();
        if !unknown.is_empty() {
            let valid: Vec<&str> = op.params.iter().map(|p| p.name.as_str()).collect();
            let described: Vec<String> = unknown
                .iter()
                .map(|key| match nearest_param(key, &valid) {
                    Some(hint) => format!("'{key}' (did you mean '{hint}'?)"),
                    None => format!("'{key}'"),
                })
                .collect();
            return Err(ApiToolError::UnknownParams {
                unknown: described.join(", "),
                valid: valid.join(", "),
                operation_id: op.operation_id.clone(),
            });
        }
    }

    // Pre-pass: report EVERY missing required parameter at once.
    //
    // The per-param loop below fails on the first one it meets, which reads fine
    // to a human running a CLI but is expensive for a model: each failure costs a
    // whole round, and the model only learns the next field's name by failing
    // again. An operation with many required fields therefore burns its way
    // through the tool loop's round cap discovering the signature one name at a
    // time, and never reaches a valid call. Collecting them means one round.
    //
    // The rules mirror the loop exactly — auto-filled project selectors and the
    // injected `limit` are not "missing", and absent query params are left to the
    // router (OpenAPI required-ness is frequently wrong for those).
    let missing: Vec<&str> = op
        .params
        .iter()
        .filter(|param| {
            if params_obj
                .and_then(|o| o.get(&param.name))
                .is_some_and(|v| !v.is_null())
            {
                return false;
            }
            if is_project_scope_param(op, param) && allowed_project_ids.len() == 1 {
                return false; // auto-filled
            }
            if param.name == "limit" && matches!(param.location, ParamLocation::Query) {
                return false; // injected with a default
            }
            match param.location {
                ParamLocation::Path => true,
                ParamLocation::Body => param.required,
                ParamLocation::Query => false,
            }
        })
        .map(|param| param.name.as_str())
        .collect();
    match missing.as_slice() {
        [] => {}
        [name] => {
            return Err(ApiToolError::MissingParam {
                name: (*name).to_string(),
                operation_id: op.operation_id.clone(),
            });
        }
        _ => {
            return Err(ApiToolError::MissingParams {
                names: missing.join(", "),
                operation_id: op.operation_id.clone(),
            });
        }
    }

    let mut path = op.path.clone();
    let mut query_parts: Vec<String> = Vec::new();
    let mut body_map = serde_json::Map::new();

    for param in &op.params {
        // Special handling: auto-fill the project selector when the caller has
        // exactly one project. This matches both `project_id` and the common
        // `/projects/{id}/…` shape where the project's own path param is named
        // `id` (see `is_project_scope_param`) — so the model never has to supply
        // (or be asked for) a project id the chat is already scoped to.
        let value = if is_project_scope_param(op, param) && !allowed_project_ids.is_empty() {
            let raw = params_obj.and_then(|o| o.get(&param.name));
            match raw {
                Some(v) => {
                    // Validate supplied value is in the allowed set.
                    let pid = value_as_i32(v).ok_or_else(|| ApiToolError::ProjectNotAllowed {
                        project_id: -1,
                        allowed: allowed_project_ids
                            .iter()
                            .map(|i| i.to_string())
                            .collect::<Vec<_>>()
                            .join(", "),
                        operation_id: op.operation_id.clone(),
                    })?;
                    if !allowed_project_ids.contains(&pid) {
                        return Err(ApiToolError::ProjectNotAllowed {
                            project_id: pid,
                            allowed: allowed_project_ids
                                .iter()
                                .map(|i| i.to_string())
                                .collect::<Vec<_>>()
                                .join(", "),
                            operation_id: op.operation_id.clone(),
                        });
                    }
                    v.clone()
                }
                None => {
                    if allowed_project_ids.len() == 1 {
                        // Auto-fill the single accessible project.
                        Value::Number(allowed_project_ids[0].into())
                    } else if param.required {
                        return Err(ApiToolError::MissingParam {
                            name: param.name.clone(),
                            operation_id: op.operation_id.clone(),
                        });
                    } else {
                        continue; // optional, absent, skip
                    }
                }
            }
        } else if param.name == "limit" && matches!(param.location, ParamLocation::Query) {
            // Limit injection: default + clamp (only for query params named "limit").
            let raw = params_obj.and_then(|o| o.get(&param.name));
            let limit_val = match raw {
                Some(v) => {
                    let n = v.as_i64().unwrap_or(default_limit);
                    n.clamp(1, max_limit)
                }
                None => default_limit,
            };
            Value::Number(limit_val.into())
        } else {
            // Normal param lookup.
            match params_obj.and_then(|o| o.get(&param.name)) {
                Some(v) => v.clone(),
                None => {
                    match param.location {
                        ParamLocation::Path => {
                            // Path param: can't build URL without it.
                            return Err(ApiToolError::MissingParam {
                                name: param.name.clone(),
                                operation_id: op.operation_id.clone(),
                            });
                        }
                        ParamLocation::Body => {
                            if param.required {
                                return Err(ApiToolError::MissingParam {
                                    name: param.name.clone(),
                                    operation_id: op.operation_id.clone(),
                                });
                            } else {
                                continue; // optional body param absent — skip
                            }
                        }
                        ParamLocation::Query => {
                            // Query param absent — let the router decide.
                            // OpenAPI metadata is frequently wrong about
                            // required-ness; we don't hard-fail here.
                            continue;
                        }
                    }
                }
            }
        };

        // Nested-object shape check. Only meaningful for body fields whose
        // schema resolved to a known object shape; everything else is skipped.
        if let Some(shape) = &param.object_shape {
            if let Some(problem) = check_object_shape(&value, shape) {
                return Err(ApiToolError::BadObjectShape {
                    name: param.name.clone(),
                    problem,
                    expected: shape.describe(),
                    operation_id: op.operation_id.clone(),
                });
            }
        }

        // Enum membership check.
        if !param.enum_values.is_empty() {
            let str_val = value_to_string(&value);
            if !param.enum_values.contains(&str_val) {
                return Err(ApiToolError::BadEnum {
                    name: param.name.clone(),
                    value: str_val,
                    allowed: param.enum_values.join(", "),
                    operation_id: op.operation_id.clone(),
                });
            }
        }

        match param.location {
            ParamLocation::Path => {
                let str_val = value_to_string(&value);
                let placeholder = format!("{{{}}}", param.name);
                path = path.replace(&placeholder, &urlencoding::encode(&str_val));
            }
            ParamLocation::Query => {
                let str_val = value_to_string(&value);
                query_parts.push(format!(
                    "{}={}",
                    urlencoding::encode(&param.name),
                    urlencoding::encode(&str_val),
                ));
            }
            ParamLocation::Body => {
                body_map.insert(param.name.clone(), value);
            }
        }
    }

    // When the operation declares a JSON request body (any Body-location param),
    // always send at least an empty object so the request carries
    // `Content-Type: application/json`. Handlers whose body fields are all
    // optional (e.g. DeployFromImageRequest) accept `{}`, and axum's `Json`
    // extractor 415s on a missing content type — so an empty body must still be
    // sent as `{}`, not omitted. GET/DELETE ops without a body stay body-less.
    let op_expects_body = op
        .params
        .iter()
        .any(|p| matches!(p.location, ParamLocation::Body));
    let body = if body_map.is_empty() {
        if op_expects_body {
            Some(Value::Object(serde_json::Map::new()))
        } else {
            None
        }
    } else {
        Some(Value::Object(body_map))
    };

    Ok(BuiltRequest {
        path,
        query: query_parts.join("&"),
        body,
    })
}

// ---------------------------------------------------------------------------
// InternalApiCaller
// ---------------------------------------------------------------------------

/// Substrate-agnostic caller that turns API operations (read-only GET or vetted
/// write) into Axum router replays.
///
/// ## Thread safety
///
/// `InternalApiCaller` is `Clone + Send + Sync` (assuming the wrapped `Router`
/// is, which Axum guarantees for `Router<()>`).
#[derive(Clone)]
pub struct InternalApiCaller {
    router: axum::Router,
    index: ReadOnlyApiIndex,
    default_limit: i64,
    max_limit: i64,
    max_response_bytes: usize,
}

impl InternalApiCaller {
    /// Construct a new caller.
    ///
    /// - `router`: the full (or filtered) Axum router to replay calls through.
    /// - `openapi`: the unified OpenAPI document to index.
    /// - `denylist`: operation IDs to exclude from the index (streaming GETs, etc.).
    pub fn new(
        router: axum::Router,
        openapi: &utoipa::openapi::OpenApi,
        denylist: Vec<String>,
    ) -> Self {
        let denylist_refs: Vec<&str> = denylist.iter().map(|s| s.as_str()).collect();
        let index = ReadOnlyApiIndex::from_openapi(openapi, &denylist_refs);
        Self {
            router,
            index,
            default_limit: 20,
            max_limit: 100,
            max_response_bytes: 128 * 1024, // 128 KiB
        }
    }

    /// Construct a caller whose index contains ONLY the allowlisted GET operation
    /// IDs (opt-in / secure-by-default). This is the production constructor for
    /// the AI `call_api` read tool — see the curated list in the serve wiring. A
    /// GET endpoint the model has never been vetted for is simply absent from the
    /// index and returns `UnknownOperation`, so new (possibly secret-bearing)
    /// endpoints can't leak by default.
    pub fn new_allowlisted(
        router: axum::Router,
        openapi: &utoipa::openapi::OpenApi,
        allowlist: Vec<String>,
    ) -> Self {
        let allowlist_refs: Vec<&str> = allowlist.iter().map(|s| s.as_str()).collect();
        let index = ReadOnlyApiIndex::from_openapi_allowlist(openapi, &allowlist_refs);
        Self {
            router,
            index,
            default_limit: 20,
            max_limit: 100,
            max_response_bytes: 128 * 1024, // 128 KiB
        }
    }

    /// Like [`Self::new_allowlisted`], but additionally indexes a separate,
    /// explicitly-vetted set of **read-only `POST`** operations (`safe_posts`).
    ///
    /// See [`ReadOnlyApiIndex::from_openapi_allowlist_with_safe_posts`] for the
    /// security rationale and the rule for what may be listed: an operation
    /// belongs in `safe_posts` only if it has no side effects. Anything that
    /// mutates must go through the propose-then-confirm write tool instead.
    pub fn new_allowlisted_with_safe_posts(
        router: axum::Router,
        openapi: &utoipa::openapi::OpenApi,
        allowlist: Vec<String>,
        safe_posts: Vec<String>,
    ) -> Self {
        let allowlist_refs: Vec<&str> = allowlist.iter().map(|s| s.as_str()).collect();
        let safe_post_refs: Vec<&str> = safe_posts.iter().map(|s| s.as_str()).collect();
        let index = ReadOnlyApiIndex::from_openapi_allowlist_with_safe_posts(
            openapi,
            &allowlist_refs,
            &safe_post_refs,
        );
        Self {
            router,
            index,
            default_limit: 20,
            max_limit: 100,
            max_response_bytes: 128 * 1024, // 128 KiB
        }
    }

    /// Construct a caller whose index contains ONLY the allowlisted write
    /// (`POST`/`PUT`/`PATCH`/`DELETE`) operation IDs.
    ///
    /// This is the production constructor for the AI vetted-write tool — a new
    /// mutating endpoint is never callable unless it has been explicitly reviewed
    /// and added to `allowlist`.
    pub fn new_write_allowlisted(
        router: axum::Router,
        openapi: &utoipa::openapi::OpenApi,
        allowlist: Vec<String>,
    ) -> Self {
        let allowlist_refs: Vec<&str> = allowlist.iter().map(|s| s.as_str()).collect();
        let index = ReadOnlyApiIndex::from_openapi_write_allowlist(openapi, &allowlist_refs);
        Self {
            router,
            index,
            default_limit: 20,
            max_limit: 100,
            max_response_bytes: 128 * 1024, // 128 KiB
        }
    }

    /// Override the default page size (default: 20).
    pub fn with_default_limit(mut self, limit: i64) -> Self {
        self.default_limit = limit;
        self
    }

    /// Override the maximum page size (default: 100).
    pub fn with_max_limit(mut self, limit: i64) -> Self {
        self.max_limit = limit;
        self
    }

    /// Override the response body cap in bytes (default: 128 KiB).
    pub fn with_max_response_bytes(mut self, bytes: usize) -> Self {
        self.max_response_bytes = bytes;
        self
    }

    /// Search the API index, hiding operations the caller can't access
    /// (advisory — execution is still guarded by the router; see [`Self::permitted`]).
    pub fn search(&self, query: &str, scope: &ApiCallScope) -> Vec<OperationSummary> {
        self.index
            .search(query)
            .into_iter()
            .filter(|op| self.permitted(op, &scope.auth))
            .map(OperationSummary::from)
            .collect()
    }

    /// Return the full schema for a single operation by `operation_id`.
    pub fn describe(&self, operation_id: &str) -> Option<OperationSchema> {
        self.index.get(operation_id).map(OperationSchema::from)
    }

    /// Render the compact endpoint catalogue (see [`ReadOnlyApiIndex::catalog`])
    /// for injection into the chat system prompt.
    pub fn catalog(&self) -> String {
        self.index.catalog()
    }

    /// The `operation_id`s actually resolved into this caller's index — i.e. the
    /// allowlist entries that matched a real operation in the OpenAPI document.
    /// Useful for startup diagnostics: an allowlist entry that is NOT in this list
    /// was silently dropped (typo, wrong method, or missing from the doc).
    pub fn indexed_operation_ids(&self) -> Vec<String> {
        self.index
            .operations()
            .iter()
            .map(|op| op.operation_id.clone())
            .collect()
    }

    /// Render the virtual-CLI root help (`<root> --help`) — the section list —
    /// for injection into the chat system prompt so the model starts oriented.
    /// Sections the `auth` caller can't read at all are omitted.
    pub fn cli_root_help(&self, auth: &AuthContext) -> String {
        let permit = |op: &ApiOperation| self.permitted(op, auth);
        match crate::cli::resolve(&self.index, "--help", &permit) {
            crate::cli::CliAction::Terminal(text) => text,
            // resolve("--help") is always Terminal; this arm is unreachable.
            crate::cli::CliAction::Execute(..) => String::new(),
        }
    }

    /// Render the virtual-CLI root help for write operations.
    ///
    /// Analogous to [`Self::cli_root_help`] but using the write permission filter,
    /// so only sections the caller has write access to are shown.
    pub fn cli_write_root_help(&self, auth: &AuthContext) -> String {
        let permit = |op: &ApiOperation| self.permitted_write(op, auth);
        match crate::cli::resolve(&self.index, "--help", &permit) {
            crate::cli::CliAction::Terminal(text) => text,
            crate::cli::CliAction::Execute(..) => String::new(),
        }
    }

    /// A flat catalogue of EVERY write operation the `auth` caller may run, one
    /// line each: `<operation_id> — <METHOD> <path> — <description>`. Unlike the
    /// section-grouped `--help` (which makes the model guess which section a verb
    /// lives in — e.g. "redeploy" is under `projects`, not `deployments`), this
    /// puts every option in front of the model at once so it can pick the right
    /// one directly. The write set is small and curated, so this is cheap.
    /// Sorted by operation_id for stable output.
    pub fn cli_write_catalog(&self, auth: &AuthContext) -> String {
        let mut ops: Vec<&ApiOperation> = self
            .index
            .operations()
            .iter()
            .filter(|op| self.permitted_write(op, auth))
            .collect();
        ops.sort_by(|a, b| a.operation_id.cmp(&b.operation_id));

        let mut out = String::with_capacity(ops.len() * 96);
        for op in ops {
            out.push_str("- ");
            out.push_str(&op.operation_id);
            out.push_str(" — ");
            out.push_str(&op.method);
            out.push(' ');
            out.push_str(&op.path);
            let blurb = op
                .summary
                .as_deref()
                .or(op.description.as_deref())
                .map(|s| {
                    s.lines()
                        .find(|l| !l.trim().is_empty())
                        .unwrap_or("")
                        .trim()
                })
                .filter(|s| !s.is_empty());
            if let Some(desc) = blurb {
                out.push_str(" — ");
                out.push_str(desc);
            }
            out.push('\n');
        }
        out
    }

    /// Run one virtual-CLI command line (see [`crate::cli`]). Parsing/help are
    /// pure; execution replays the resolved call through the router with the
    /// caller's scope (auth + `project_id` auto-fill + allowlist all apply).
    /// Always returns model-facing text — help, an error, or the response body.
    pub async fn run_cli(&self, command: &str, scope: &ApiCallScope) -> String {
        let permit = |op: &ApiOperation| self.permitted(op, &scope.auth);
        match crate::cli::resolve(&self.index, command, &permit) {
            crate::cli::CliAction::Terminal(text) => text,
            crate::cli::CliAction::Execute(op, params) => {
                let op_id = op.operation_id.clone();
                match self.call(&op_id, params, scope).await {
                    Ok(resp) => {
                        // Structured result: a clean `{operation, status, data}`
                        // object rather than a status-prefixed text blob. The model
                        // gets `status` as a number and `data` as real JSON (not a
                        // stringified body), and the UI can pretty-print it.
                        let mut obj = serde_json::Map::new();
                        obj.insert("operation".to_string(), Value::String(op_id.clone()));
                        obj.insert("status".to_string(), Value::from(resp.status));
                        obj.insert("data".to_string(), resp.body);
                        if resp.truncated {
                            obj.insert("truncated".to_string(), Value::Bool(true));
                        }
                        serde_json::to_string(&Value::Object(obj)).unwrap_or_else(|_| {
                            format!("{{\"operation\":\"{op_id}\",\"status\":{}}}", resp.status)
                        })
                    }
                    Err(e) => format!(
                        "`{op_id}` failed: {e}\nRun `{op_id} --help` to check its flags, or \
                         `--help` to list sections."
                    ),
                }
            }
        }
    }

    /// Parse and VALIDATE a write CLI command but do NOT execute it.
    ///
    /// Returns:
    /// - [`WritePrepareOutcome::Help`] when `--help` or a section/root help was
    ///   requested — the text can be returned verbatim to the model.
    /// - [`WritePrepareOutcome::Invalid`] when validation fails (missing required
    ///   param, bad enum value, project not allowed, etc.).
    /// - [`WritePrepareOutcome::Prepared`] when the command is valid — a snapshot
    ///   of what would be executed if a human confirms.
    ///
    /// This is the "propose" half of propose-then-confirm.  The confirm endpoint
    /// (in another crate) calls [`Self::execute_write`] with the `params` from
    /// the returned [`PreparedWrite`].
    pub fn prepare_write_cli(&self, command: &str, scope: &ApiCallScope) -> WritePrepareOutcome {
        let permit = |op: &ApiOperation| self.permitted_write(op, &scope.auth);
        match crate::cli::resolve(&self.index, command, &permit) {
            crate::cli::CliAction::Terminal(text) => WritePrepareOutcome::Help(text),
            crate::cli::CliAction::Execute(op, params) => {
                // Validate by running build_request_parts but discarding the
                // built request — we only care whether it succeeds or errors.
                match build_request_parts(
                    op,
                    &params,
                    &scope.project_ids,
                    self.default_limit,
                    self.max_limit,
                ) {
                    Err(e) => WritePrepareOutcome::Invalid(format!(
                        "{e}\nRun `{} --help` to check its flags.",
                        op.operation_id
                    )),
                    Ok(_) => {
                        // Lead the summary with the operation's human description
                        // (first line of the OpenAPI summary/doc-comment) so the
                        // person reviewing the confirm card sees WHAT the action
                        // does — e.g. "Promote a deployment to another
                        // environment" vs. "Trigger pipeline (redeploy)" — not
                        // just an opaque `method path`. The human gate is only
                        // meaningful if the human can tell what they're approving.
                        let human = op
                            .summary
                            .as_deref()
                            .or(op.description.as_deref())
                            .map(|s| {
                                s.lines()
                                    .find(|l| !l.trim().is_empty())
                                    .unwrap_or("")
                                    .trim()
                            })
                            .filter(|s| !s.is_empty());
                        let summary = match human {
                            Some(desc) => {
                                format!("{desc} ({} {})", op.method, op.path)
                            }
                            None => format!("{} {} — {}", op.method, op.path, op.operation_id),
                        };
                        let required_permission =
                            required_write_permission(op).map(|p| p.to_string());
                        WritePrepareOutcome::Prepared(PreparedWrite {
                            operation_id: op.operation_id.clone(),
                            method: op.method.clone(),
                            path: op.path.clone(),
                            summary,
                            params,
                            required_permission,
                        })
                    }
                }
            }
        }
    }

    /// Execute a previously-prepared write operation.
    ///
    /// This is what the confirm endpoint calls after a human has reviewed the
    /// [`PreparedWrite`] returned by [`Self::prepare_write_cli`].  It delegates
    /// directly to [`Self::call`], which is now method-aware.
    pub async fn execute_write(
        &self,
        operation_id: &str,
        params: serde_json::Value,
        scope: &ApiCallScope,
    ) -> Result<ApiToolResponse, ApiToolError> {
        self.call(operation_id, params, scope).await
    }

    /// Execute a call by replaying a synthetic request through the Axum router
    /// with `scope.auth` injected into extensions.
    ///
    /// ## Steps
    ///
    /// 1. Look up the operation in the index — returns `UnknownOperation` if absent.
    /// 2. Call [`build_request_parts`] to validate params and build path + query +
    ///    optional body.
    /// 3. Build an `axum::http::Request<Body>` with the correct HTTP method and
    ///    insert `scope.auth` into extensions.
    /// 4. Run `router.clone().oneshot(req)` — the router enforces authz.
    /// 5. Collect the response body up to `max_response_bytes` (truncate if over).
    /// 6. Attempt to parse the body as JSON; fall back to a string value.
    pub async fn call(
        &self,
        operation_id: &str,
        params: Value,
        scope: &ApiCallScope,
    ) -> Result<ApiToolResponse, ApiToolError> {
        // Step 1: look up the operation.
        let op = self
            .index
            .get(operation_id)
            .ok_or_else(|| ApiToolError::UnknownOperation {
                operation_id: operation_id.to_string(),
            })?;

        debug!(
            operation_id = %op.operation_id,
            path = %op.path,
            method = %op.method,
            "InternalApiCaller: dispatching call"
        );

        // Step 2: validate params and build path + query + optional body.
        let built = build_request_parts(
            op,
            &params,
            &scope.project_ids,
            self.default_limit,
            self.max_limit,
        )?;

        // Step 3: build the Axum request.
        let uri = if built.query.is_empty() {
            built.path.clone()
        } else {
            format!("{}?{}", built.path, built.query)
        };

        // Parse the method; default to GET only for the literal "GET" string.
        let method = parse_http_method(&op.method);

        let (req_body, content_type) = if let Some(body_val) = built.body {
            let bytes = serde_json::to_vec(&body_val).map_err(|e| ApiToolError::RouterError {
                operation_id: operation_id.to_string(),
                reason: format!("Failed to serialize request body: {e}"),
            })?;
            (Body::from(bytes), Some("application/json"))
        } else {
            (Body::empty(), None)
        };

        let mut req_builder = Request::builder().method(method).uri(&uri);

        if let Some(ct) = content_type {
            req_builder = req_builder.header(http::header::CONTENT_TYPE, ct);
        }

        let mut req = req_builder
            .body(req_body)
            .map_err(|e| ApiToolError::RouterError {
                operation_id: operation_id.to_string(),
                reason: format!("Failed to build request for URI '{}': {}", uri, e),
            })?;

        // Inject the caller's AuthContext so permission_guard! can read it.
        req.extensions_mut().insert(scope.auth.clone());

        // Step 4: replay through the router.
        let response =
            self.router
                .clone()
                .oneshot(req)
                .await
                .map_err(|e| ApiToolError::RouterError {
                    operation_id: operation_id.to_string(),
                    reason: format!("Router oneshot error: {}", e),
                })?;

        let status = response.status().as_u16();

        // Step 5: collect body with size cap.
        let (body_value, truncated) =
            collect_body(response.into_body(), self.max_response_bytes, operation_id).await?;

        if truncated {
            warn!(
                operation_id = %operation_id,
                max_bytes = self.max_response_bytes,
                "InternalApiCaller: response body truncated at size cap"
            );
        }

        // Non-2xx responses are reported as Upstream errors so the model can
        // self-correct (e.g., supply a valid project_id, then retry).
        if !(200..300).contains(&(status as usize)) {
            let detail = match &body_value {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            return Err(ApiToolError::Upstream {
                status,
                detail,
                operation_id: operation_id.to_string(),
            });
        }

        Ok(ApiToolResponse {
            status,
            body: body_value,
            truncated,
        })
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    /// Advisory permission filter for discovery (search + `--help`).
    ///
    /// Returns whether the `scope` caller may *see* `op` during discovery: if the
    /// operation's domain maps to a read [`Permission`] the caller must hold it,
    /// otherwise (an unmapped tag) the op is shown — the router's
    /// `permission_guard!` is the real boundary, so failing *open* here only
    /// risks a confusing "not permitted" on execution, never an actual bypass.
    /// An admin (or any role holding the read permission) passes.
    pub(crate) fn permitted(&self, op: &ApiOperation, auth: &AuthContext) -> bool {
        match required_read_permission(op) {
            Some(perm) => auth.has_permission(&perm),
            None => true,
        }
    }

    /// Advisory write-permission filter for discovery (write `--help` + prepare).
    ///
    /// Analogous to [`Self::permitted`] but uses write permissions derived from
    /// the operation's HTTP method.  The router's `permission_guard!` remains the
    /// real enforcement boundary; this is advisory only.
    pub(crate) fn permitted_write(&self, op: &ApiOperation, auth: &AuthContext) -> bool {
        match required_write_permission(op) {
            Some(perm) => auth.has_permission(&perm),
            None => true,
        }
    }
}

/// Best guess at which valid parameter an unknown name was meant to be.
///
/// The near-misses that actually happen are prefixes and small typos
/// (an abbreviated `metric`, or a pair of transposed characters), so a
/// containment check plus a bounded edit distance covers them. Returns `None`
/// rather than a bad guess when nothing is close — a wrong suggestion sends the
/// caller further astray than none at all.
fn nearest_param<'a>(unknown: &str, valid: &[&'a str]) -> Option<&'a str> {
    let needle = unknown.to_ascii_lowercase();

    // A name that is a prefix/substring of exactly one valid parameter is almost
    // certainly it (the `--metric` / `--metric_name` case).
    let contained: Vec<&&str> = valid
        .iter()
        .filter(|v| {
            let v = v.to_ascii_lowercase();
            v.contains(&needle) || needle.contains(&v)
        })
        .collect();
    if let [only] = contained.as_slice() {
        return Some(**only);
    }

    // Otherwise the closest by edit distance, accepting at most a third of the
    // name being wrong so unrelated parameters are never suggested.
    let budget = (needle.chars().count() / 3).max(1);
    valid
        .iter()
        .map(|v| (edit_distance(&needle, &v.to_ascii_lowercase()), *v))
        .filter(|(d, _)| *d <= budget)
        .min_by_key(|(d, _)| *d)
        .map(|(_, v)| v)
}

/// Levenshtein distance, two-row variant (names here are short).
fn edit_distance(a: &str, b: &str) -> usize {
    let b_chars: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b_chars.len()).collect();
    let mut cur = vec![0; b_chars.len() + 1];

    for (i, ac) in a.chars().enumerate() {
        cur[0] = i + 1;
        for (j, bc) in b_chars.iter().enumerate() {
            let cost = usize::from(ac != *bc);
            cur[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b_chars.len()]
}

/// Check an object-valued parameter against its schema shape.
///
/// Returns `Some(problem)` describing what is wrong, or `None` when the value
/// is acceptable. This is a *shape* check, not full JSON Schema validation: it
/// verifies the discriminator and the required keys, which is what separates a
/// call that can possibly succeed from one that cannot. Type-checking
/// fields whose schema metadata is available here as well, so a confirmation
/// card never presents a request the router is guaranteed to reject.
fn check_object_shape(value: &Value, shape: &ObjectShape) -> Option<String> {
    let Some(obj) = value.as_object() else {
        return Some(format!("expected a JSON object, got {}", json_kind(value)));
    };

    let fields = match &shape.discriminator {
        Some(discriminator) => {
            // Tagged union: the discriminator decides which fields apply, so it
            // has to be present and recognised before anything else.
            let Some(tag) = obj.get(discriminator) else {
                return Some(format!("missing the '{discriminator}' discriminator"));
            };
            let Some(tag) = tag.as_str() else {
                return Some(format!("'{discriminator}' must be a string"));
            };
            let Some(fields) = shape.fields_for(Some(tag)) else {
                let mut known: Vec<&str> = shape.variants.keys().map(String::as_str).collect();
                known.sort_unstable();
                return Some(format!(
                    "'{discriminator}' is '{tag}', which is not one of: {}",
                    known.join(", ")
                ));
            };
            fields
        }
        None => shape.fields_for(None)?,
    };

    let missing: Vec<&str> = fields
        .iter()
        .filter(|f| f.required && obj.get(&f.name).is_none_or(serde_json::Value::is_null))
        .map(|f| f.name.as_str())
        .collect();
    if !missing.is_empty() {
        return Some(format!("missing key(s): {}", missing.join(", ")));
    }

    // Type and enum checks on what WAS supplied. Presence alone is not enough:
    // a quoted number (`"0.5"` for an f64) and a plausible-but-wrong enum
    // spelling (`">"` where the schema wants `gt`) are both present, both
    // fatal, and both indistinguishable from correct until the API rejects
    // them.
    for field in fields {
        let Some(supplied) = obj.get(&field.name) else {
            continue; // optional and absent
        };
        if supplied.is_null() {
            continue;
        }
        if !field.enum_values.is_empty() {
            let ok = supplied
                .as_str()
                .is_some_and(|v| field.enum_values.iter().any(|allowed| allowed == v));
            if !ok {
                return Some(format!(
                    "'{}' is {}, which is not one of: {}",
                    field.name,
                    render_value(supplied),
                    field.enum_values.join(", ")
                ));
            }
            continue;
        }
        if !json_matches_type(supplied, &field.ty) {
            return Some(format!(
                "'{}' is {}, expected {}",
                field.name,
                render_value(supplied),
                field.ty
            ));
        }
    }
    None
}

/// Does a JSON value satisfy a short JSON Schema type name?
///
/// Deliberately permissive in one direction only: an integer is accepted where
/// a number is wanted (`300` for an f64 is fine), but a *string* is never
/// accepted for a number even when it would parse. Quoting a number is the
/// exact mistake this catches, and silently accepting it would just move the
/// failure back to the API.
fn json_matches_type(value: &Value, ty: &str) -> bool {
    match ty {
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => value.is_i64() || value.is_u64(),
        "boolean" => value.is_boolean(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        // "any" or an unrecognised type — nothing to assert.
        _ => true,
    }
}

/// Render a supplied value compactly for an error message, so the caller can
/// see that it sent `"0.5"` rather than `0.5`.
fn render_value(value: &Value) -> String {
    match value {
        Value::String(s) => format!("the string \"{s}\""),
        other => other.to_string(),
    }
}

/// Name a JSON value's kind, for error messages.
fn json_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

/// Parse an uppercase HTTP method string into [`http::Method`].
///
/// Only the literal string `"GET"` defaults to `GET`; all other well-known
/// methods are mapped; unknown strings fall back to `GET` as a safe no-op.
fn parse_http_method(method: &str) -> http::Method {
    match method {
        "GET" => http::Method::GET,
        "POST" => http::Method::POST,
        "PUT" => http::Method::PUT,
        "PATCH" => http::Method::PATCH,
        "DELETE" => http::Method::DELETE,
        "HEAD" => http::Method::HEAD,
        "OPTIONS" => http::Method::OPTIONS,
        // Unknown method — fall back to GET (safe: the router will 404/405).
        _ => http::Method::GET,
    }
}

/// Best-effort mapping from an operation's OpenAPI tag to the read [`Permission`]
/// that gates it, used only to filter what the AI sees during discovery (never to
/// authorize execution — that's the router's job). Keyword-matched on the first
/// tag so related tags (e.g. the several OTel telemetry tags) collapse onto one
/// permission; `None` means "no known mapping → show it".
fn required_read_permission(op: &ApiOperation) -> Option<Permission> {
    let tag = op.tags.first()?.to_ascii_lowercase();
    // Order matters: more specific keywords first (e.g. telemetry before metrics).
    let perm = if tag.contains("telemetry")
        || tag.contains("trace")
        || tag.contains("span")
        || tag.contains("otel")
        || tag.contains("dashboard")
        || tag.contains("insight")
        || tag.contains("alert")
    {
        Permission::OtelRead
    } else if tag.contains("error") {
        Permission::ErrorTrackingRead
    } else if tag.contains("backup") {
        Permission::BackupsRead
    } else if tag.contains("deployment") {
        Permission::DeploymentsRead
    } else if tag.contains("environment") {
        Permission::EnvironmentsRead
    } else if tag.contains("domain") {
        Permission::DomainsRead
    } else if tag.contains("external") || tag.contains("static bundle") || tag.contains("image") {
        Permission::ExternalServicesRead
    } else if tag.contains("audit") {
        Permission::AuditRead
    } else if tag.contains("analytic") {
        Permission::AnalyticsRead
    } else if tag.contains("metric") {
        Permission::MetricsRead
    } else if tag.contains("log") {
        Permission::LogsRead
    } else if tag.contains("notification") {
        Permission::NotificationsRead
    } else if tag.contains("setting") {
        Permission::SettingsRead
    } else if tag.contains("project") {
        Permission::ProjectsRead
    } else {
        return None;
    };
    Some(perm)
}

/// Best-effort mapping from an operation's tag + HTTP method to the write
/// [`Permission`] that gates it.
///
/// Used only to filter what the AI sees during write discovery (advisory — the
/// router's `permission_guard!` is the real boundary).
///
/// Domain mapping: keyword-matched on the first tag (same keyword table as
/// `required_read_permission`).  Method mapping:
/// - `DELETE` → the domain's `*Delete` variant (fallback `*Write` when no
///   Delete variant exists for that domain).
/// - `POST` → the domain's `*Create` variant.
/// - `PUT` / `PATCH` → the domain's `*Write` variant.
///
/// Only the domains that have an explicit write variant are mapped; everything
/// else returns `None` (shown by default, router still guards).
pub(crate) fn required_write_permission(op: &ApiOperation) -> Option<Permission> {
    let tag = op.tags.first()?.to_ascii_lowercase();
    let method = op.method.as_str();

    let perm = if tag.contains("deployment") {
        match method {
            "POST" => Permission::DeploymentsCreate,
            "DELETE" => Permission::DeploymentsDelete,
            _ => Permission::DeploymentsWrite,
        }
    } else if tag.contains("environment") {
        match method {
            "POST" => Permission::EnvironmentsCreate,
            // No EnvironmentsDelete variant — fall back to Write.
            _ => Permission::EnvironmentsWrite,
        }
    } else if tag.contains("domain") {
        match method {
            "POST" => Permission::DomainsCreate,
            "DELETE" => Permission::DomainsDelete,
            _ => Permission::DomainsWrite,
        }
    } else if tag.contains("alert") {
        // Metric alert create/update/delete routes share the OTel write gate.
        Permission::OtelWrite
    } else {
        // Unknown / unsupported domain — show it (router still guards).
        return None;
    };
    Some(perm)
}

// ---------------------------------------------------------------------------
// Body collection helper
// ---------------------------------------------------------------------------

/// Collect an Axum response body up to `max_bytes`.
///
/// Returns `(value, truncated)` where `truncated` is true when the body was
/// cut short.  If the bytes are valid UTF-8 JSON they are parsed; otherwise
/// the raw string (or lossy-converted string) is returned as a JSON string value.
async fn collect_body(
    body: Body,
    max_bytes: usize,
    operation_id: &str,
) -> Result<(Value, bool), ApiToolError> {
    // We collect up to max_bytes + 1 so we can detect truncation.
    let cap = max_bytes + 1;
    let bytes = axum::body::to_bytes(body, cap)
        .await
        .map_err(|e| ApiToolError::BodyReadError {
            operation_id: operation_id.to_string(),
            reason: format!("Failed to read response body bytes: {}", e),
        })?;

    let truncated = bytes.len() > max_bytes;
    let slice = if truncated {
        &bytes[..max_bytes]
    } else {
        &bytes[..]
    };

    let text = String::from_utf8_lossy(slice);

    // Attempt JSON parse first.
    let value =
        serde_json::from_str::<Value>(&text).unwrap_or_else(|_| Value::String(text.into_owned()));

    Ok((value, truncated))
}

// ---------------------------------------------------------------------------
// Utility functions
// ---------------------------------------------------------------------------

/// Convert a `serde_json::Value` to its string representation for path/query
/// substitution and enum membership checks.
pub(crate) fn value_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => "null".to_string(),
        // Arrays and objects are serialised as JSON — unlikely for path/query params.
        other => other.to_string(),
    }
}

/// Try to interpret a `serde_json::Value` as an `i32`.
pub(crate) fn value_as_i32(v: &Value) -> Option<i32> {
    match v {
        Value::Number(n) => n.as_i64().and_then(|i| i32::try_from(i).ok()),
        Value::String(s) => s.parse::<i32>().ok(),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Tests (pure logic only — no router required)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{ApiOperation, ObjectField, ParamLocation, ParamSpec};

    // -----------------------------------------------------------------------
    // Test-only helpers
    // -----------------------------------------------------------------------

    /// Build an inline `Content` for a request body with the given properties.
    /// Each tuple is `(name, type_str, required, enum_values)`.
    fn build_inline_body_schema(props: &[(&str, &str, bool, &[&str])]) -> utoipa::openapi::Content {
        use utoipa::openapi::{
            schema::{ObjectBuilder, SchemaType, Type},
            Content, RefOr, Schema,
        };
        let mut builder = ObjectBuilder::new().schema_type(SchemaType::Type(Type::Object));
        for (name, ty, required, enum_vals) in props {
            let mut prop_builder = ObjectBuilder::new().schema_type(match *ty {
                "integer" => SchemaType::Type(Type::Integer),
                "boolean" => SchemaType::Type(Type::Boolean),
                _ => SchemaType::Type(Type::String),
            });
            if !enum_vals.is_empty() {
                let vals: Vec<serde_json::Value> = enum_vals
                    .iter()
                    .map(|s| serde_json::Value::String(s.to_string()))
                    .collect();
                prop_builder = prop_builder.enum_values(Some(vals));
            }
            builder = builder.property(*name, RefOr::T(Schema::Object(prop_builder.build())));
            if *required {
                builder = builder.required(*name);
            }
        }
        Content::new(Some(RefOr::T(Schema::Object(builder.build()))))
    }

    fn make_op(operation_id: &str, path: &str, params: Vec<ParamSpec>) -> ApiOperation {
        ApiOperation {
            operation_id: operation_id.to_string(),
            path: path.to_string(),
            method: "GET".to_string(),
            summary: None,
            description: None,
            tags: vec![],
            params,
        }
    }

    fn make_write_op(
        operation_id: &str,
        method: &str,
        path: &str,
        tag: &str,
        params: Vec<ParamSpec>,
    ) -> ApiOperation {
        ApiOperation {
            operation_id: operation_id.to_string(),
            path: path.to_string(),
            method: method.to_string(),
            summary: Some(format!("{method} {path}")),
            description: None,
            tags: if tag.is_empty() {
                vec![]
            } else {
                vec![tag.to_string()]
            },
            params,
        }
    }

    fn path_param(name: &str) -> ParamSpec {
        ParamSpec {
            name: name.to_string(),
            location: ParamLocation::Path,
            required: true,
            ty: "integer".to_string(),
            enum_values: vec![],
            description: None,
            object_shape: None,
        }
    }

    fn query_param(name: &str, required: bool) -> ParamSpec {
        ParamSpec {
            name: name.to_string(),
            location: ParamLocation::Query,
            required,
            ty: "string".to_string(),
            enum_values: vec![],
            description: None,
            object_shape: None,
        }
    }

    fn body_param(name: &str, required: bool) -> ParamSpec {
        ParamSpec {
            name: name.to_string(),
            location: ParamLocation::Body,
            required,
            ty: "string".to_string(),
            enum_values: vec![],
            description: None,
            object_shape: None,
        }
    }

    fn body_enum_param(name: &str, values: &[&str], required: bool) -> ParamSpec {
        ParamSpec {
            name: name.to_string(),
            location: ParamLocation::Body,
            required,
            ty: "string".to_string(),
            enum_values: values.iter().map(|s| s.to_string()).collect(),
            description: None,
            object_shape: None,
        }
    }

    fn op_tagged(tag: &str) -> ApiOperation {
        ApiOperation {
            tags: if tag.is_empty() {
                vec![]
            } else {
                vec![tag.to_string()]
            },
            ..make_op("x", "/x", vec![])
        }
    }

    fn op_tagged_with_method(tag: &str, method: &str) -> ApiOperation {
        ApiOperation {
            method: method.to_string(),
            tags: if tag.is_empty() {
                vec![]
            } else {
                vec![tag.to_string()]
            },
            ..make_op("x", "/x", vec![])
        }
    }

    #[test]
    fn required_read_permission_maps_tags_to_read_perms() {
        use temps_auth::permissions::Permission;
        assert_eq!(
            required_read_permission(&op_tagged("Deployments")),
            Some(Permission::DeploymentsRead)
        );
        // The several OTel telemetry tags collapse onto OtelRead.
        for t in [
            "Traces",
            "Telemetry Metrics",
            "Telemetry Logs",
            "Dashboards",
            "Alerts",
        ] {
            assert_eq!(
                required_read_permission(&op_tagged(t)),
                Some(Permission::OtelRead),
                "tag {t}"
            );
        }
        assert_eq!(
            required_read_permission(&op_tagged("Backups")),
            Some(Permission::BackupsRead)
        );
        assert_eq!(
            required_read_permission(&op_tagged("Error Tracking")),
            Some(Permission::ErrorTrackingRead)
        );
        // Unknown / missing tag → None (shown by default; router still guards).
        assert_eq!(required_read_permission(&op_tagged("Wibble")), None);
        assert_eq!(required_read_permission(&op_tagged("")), None);
    }

    fn enum_query_param(name: &str, values: &[&str]) -> ParamSpec {
        ParamSpec {
            name: name.to_string(),
            location: ParamLocation::Query,
            required: false,
            ty: "string".to_string(),
            enum_values: values.iter().map(|s| s.to_string()).collect(),
            description: None,
            object_shape: None,
        }
    }

    // -----------------------------------------------------------------------
    // Happy path
    // -----------------------------------------------------------------------

    #[test]
    fn happy_path_path_and_query_substitution() {
        let op = make_op(
            "get_deployment",
            "/projects/{project_id}/deployments/{id}",
            vec![
                path_param("project_id"),
                path_param("id"),
                query_param("include_logs", false),
            ],
        );

        let params = serde_json::json!({
            "project_id": 42,
            "id": 7,
            "include_logs": "true"
        });

        let result = build_request_parts(&op, &params, &[42], 20, 100).expect("should succeed");

        assert_eq!(result.path, "/projects/42/deployments/7");
        assert_eq!(result.query, "include_logs=true");
        assert!(result.body.is_none());
    }

    #[test]
    fn happy_path_no_query_params() {
        let op = make_op(
            "get_project",
            "/projects/{project_id}",
            vec![path_param("project_id")],
        );

        let params = serde_json::json!({ "project_id": 1 });
        let result = build_request_parts(&op, &params, &[1], 20, 100).expect("should succeed");

        assert_eq!(result.path, "/projects/1");
        assert!(result.query.is_empty());
        assert!(result.body.is_none());
    }

    // -----------------------------------------------------------------------
    // Body params
    // -----------------------------------------------------------------------

    #[test]
    fn body_params_build_json_body_object() {
        let op = make_write_op(
            "create_thing",
            "POST",
            "/things",
            "Things",
            vec![body_param("name", true), body_param("description", false)],
        );

        let params = serde_json::json!({ "name": "my-thing", "description": "a description" });
        let result = build_request_parts(&op, &params, &[], 20, 100).expect("should succeed");

        assert!(result.body.is_some(), "body should be Some");
        let body = result.body.unwrap();
        assert_eq!(body["name"], serde_json::json!("my-thing"));
        assert_eq!(body["description"], serde_json::json!("a description"));
    }

    #[test]
    fn optional_body_param_absent_produces_body_without_it() {
        let op = make_write_op(
            "create_thing",
            "POST",
            "/things",
            "Things",
            vec![body_param("name", true), body_param("description", false)],
        );

        let params = serde_json::json!({ "name": "required-only" });
        let result = build_request_parts(&op, &params, &[], 20, 100).expect("should succeed");

        let body = result.body.expect("body should be Some");
        assert!(body.get("name").is_some());
        assert!(
            body.get("description").is_none(),
            "absent optional body param must be omitted"
        );
    }

    #[test]
    fn all_optional_body_params_absent_still_sends_empty_json_object() {
        // A body operation whose fields are ALL optional (e.g. DeployFromImageRequest)
        // with none supplied must still produce an empty `{}` body so the replay
        // carries `Content-Type: application/json` — otherwise axum's `Json`
        // extractor returns 415, not a useful validation error.
        let op = make_write_op(
            "deploy_from_image",
            "POST",
            "/projects/{project_id}/environments/{environment_id}/deploy/image",
            "Deployments",
            vec![
                path_param("project_id"),
                path_param("environment_id"),
                body_param("image_ref", false),
                body_param("external_image_id", false),
            ],
        );

        let params = serde_json::json!({ "project_id": 1, "environment_id": 5 });
        let result = build_request_parts(&op, &params, &[1], 20, 100).expect("should succeed");

        let body = result
            .body
            .expect("op declares a JSON body → send `{}`, not None");
        assert_eq!(
            body,
            serde_json::json!({}),
            "empty body must be an empty object, not null/absent"
        );
    }

    #[test]
    fn required_absent_body_param_returns_missing_param_error() {
        let op = make_write_op(
            "create_thing",
            "POST",
            "/things",
            "Things",
            vec![body_param("name", true)],
        );

        let params = serde_json::json!({});
        let err = build_request_parts(&op, &params, &[], 20, 100)
            .expect_err("must fail on missing required body param");

        assert!(
            matches!(err, ApiToolError::MissingParam { ref name, .. } if name == "name"),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn required_null_body_param_returns_missing_param_error() {
        let op = make_write_op(
            "create_thing",
            "POST",
            "/things",
            "Things",
            vec![body_param("name", true)],
        );

        let err = build_request_parts(&op, &serde_json::json!({ "name": null }), &[], 20, 100)
            .expect_err("a required body field cannot be null");
        assert!(matches!(
            err,
            ApiToolError::MissingParam { ref name, .. } if name == "name"
        ));
    }

    /// Several missing required fields must be reported in ONE error.
    ///
    /// Discovered against a live model: `create_alert` has eight required body
    /// fields, and one-at-a-time reporting made the model spend a full tool
    /// round per field, exhausting the round cap before it could ever stage a
    /// valid proposal.
    #[test]
    fn all_missing_required_body_params_are_reported_together() {
        let op = make_write_op(
            "create_alert",
            "POST",
            "/otel/alerts",
            "Alerts",
            vec![
                body_param("name", true),
                body_param("metric_name", true),
                body_param("aggregation", true),
                body_param("severity", true),
                body_param("note", false),
            ],
        );

        let params = serde_json::json!({ "name": "p95 latency" });
        let err = build_request_parts(&op, &params, &[], 20, 100)
            .expect_err("must fail while required fields are absent");

        let ApiToolError::MissingParams { names, .. } = &err else {
            panic!("expected MissingParams, got {err:?}");
        };
        // Compare whole names — "metric_name" contains "name" as a substring.
        let listed: Vec<&str> = names.split(", ").collect();
        assert_eq!(
            listed,
            vec!["metric_name", "aggregation", "severity"],
            "expected exactly the absent required fields, got: {names}"
        );
    }

    /// Build the real-world shape: `detection_config` is an internally-tagged
    /// union, the exact field a live model kept getting wrong.
    fn detector_shape() -> ObjectShape {
        fn field(name: &str, ty: &str, enum_values: &[&str]) -> ObjectField {
            ObjectField {
                name: name.to_string(),
                required: true,
                ty: ty.to_string(),
                enum_values: enum_values.iter().map(|s| s.to_string()).collect(),
            }
        }

        let mut variants = std::collections::BTreeMap::new();
        variants.insert(
            "static".to_string(),
            vec![
                field("kind", "string", &[]),
                field("comparator", "string", &["gt", "gte", "lt", "lte"]),
                field("threshold", "number", &[]),
            ],
        );
        variants.insert(
            "anomaly".to_string(),
            vec![
                field("kind", "string", &[]),
                field("deviations", "number", &[]),
            ],
        );
        ObjectShape {
            discriminator: Some("kind".to_string()),
            variants,
            fields: Vec::new(),
        }
    }

    fn op_with_detector() -> ApiOperation {
        let mut param = body_param("detection_config", true);
        param.object_shape = Some(detector_shape());
        make_write_op(
            "create_alert",
            "POST",
            "/otel/alerts",
            "Alerts",
            vec![param],
        )
    }

    /// The exact failure seen against a live model: it proposed
    /// `{"type": "threshold"}`, which has no `kind`. Before this check the call
    /// validated fine, was staged for approval, and only failed with a 422
    /// AFTER a human clicked Confirm.
    #[test]
    fn object_body_param_missing_discriminator_is_rejected_at_validation() {
        let op = op_with_detector();
        let params = serde_json::json!({
            "detection_config": { "type": "threshold", "threshold": 500 }
        });

        let err = build_request_parts(&op, &params, &[], 20, 100)
            .expect_err("a payload the API cannot accept must not validate");

        let ApiToolError::BadObjectShape {
            problem, expected, ..
        } = &err
        else {
            panic!("expected BadObjectShape, got {err:?}");
        };
        assert!(
            problem.contains("kind"),
            "problem should name it: {problem}"
        );
        // The accepted shape must travel with the error — the model does not
        // reliably run `--help`, so this is the one place it is guaranteed to
        // see the discriminator's name and values.
        assert!(expected.contains("static"), "expected: {expected}");
        assert!(expected.contains("comparator"), "expected: {expected}");
    }

    #[test]
    fn object_body_param_with_unknown_variant_lists_the_known_ones() {
        let op = op_with_detector();
        let params = serde_json::json!({
            "detection_config": { "kind": "threshold", "threshold": 500 }
        });

        let err = build_request_parts(&op, &params, &[], 20, 100).expect_err("unknown variant");
        let ApiToolError::BadObjectShape { problem, .. } = &err else {
            panic!("expected BadObjectShape, got {err:?}");
        };
        assert!(
            problem.contains("anomaly") && problem.contains("static"),
            "{problem}"
        );
    }

    /// The variant's own required keys are enforced too — a `static` detector
    /// without a threshold is just as unusable as one without a `kind`.
    #[test]
    fn object_body_param_missing_variant_keys_is_rejected() {
        let op = op_with_detector();
        let params = serde_json::json!({ "detection_config": { "kind": "static" } });

        let err = build_request_parts(&op, &params, &[], 20, 100).expect_err("incomplete variant");
        let ApiToolError::BadObjectShape { problem, .. } = &err else {
            panic!("expected BadObjectShape, got {err:?}");
        };
        assert!(problem.contains("comparator"), "{problem}");
        assert!(problem.contains("threshold"), "{problem}");
    }

    #[test]
    fn object_body_param_required_null_key_is_rejected() {
        let op = op_with_detector();
        let params = serde_json::json!({
            "detection_config": {
                "kind": "static",
                "comparator": "gt",
                "threshold": null
            }
        });

        let err = build_request_parts(&op, &params, &[], 20, 100)
            .expect_err("a required nested field cannot be null");
        let ApiToolError::BadObjectShape { problem, .. } = &err else {
            panic!("expected BadObjectShape, got {err:?}");
        };
        assert!(problem.contains("threshold"), "{problem}");
    }

    #[test]
    fn well_formed_object_body_param_passes_through_untouched() {
        let op = op_with_detector();
        let detector = serde_json::json!({
            "kind": "static", "comparator": "gt", "threshold": 500
        });
        let params = serde_json::json!({ "detection_config": detector.clone() });

        let built = build_request_parts(&op, &params, &[], 20, 100).expect("valid payload");
        assert_eq!(
            built.body.expect("body")["detection_config"],
            detector,
            "a valid object must reach the API byte-for-byte"
        );
    }

    /// A plain (non-union) object is checked against its own required keys.
    #[test]
    fn plain_object_body_param_requires_its_keys() {
        let mut param = body_param("window", true);
        param.object_shape = Some(ObjectShape {
            discriminator: None,
            variants: std::collections::BTreeMap::new(),
            fields: ["from", "to"]
                .iter()
                .map(|n| ObjectField {
                    name: n.to_string(),
                    required: true,
                    ty: "string".to_string(),
                    enum_values: Vec::new(),
                })
                .collect(),
        });
        let op = make_write_op("create_thing", "POST", "/things", "Things", vec![param]);

        let params = serde_json::json!({ "window": { "from": "now-1h" } });
        let err = build_request_parts(&op, &params, &[], 20, 100).expect_err("missing 'to'");
        let ApiToolError::BadObjectShape { problem, .. } = &err else {
            panic!("expected BadObjectShape, got {err:?}");
        };
        assert!(problem.contains("to"), "{problem}");
    }

    /// A scalar where an object belongs must be named as such, not silently
    /// forwarded for the API to reject.
    #[test]
    fn scalar_where_object_expected_is_rejected() {
        let op = op_with_detector();
        let params = serde_json::json!({ "detection_config": "static" });

        let err = build_request_parts(&op, &params, &[], 20, 100).expect_err("scalar");
        let ApiToolError::BadObjectShape { problem, .. } = &err else {
            panic!("expected BadObjectShape, got {err:?}");
        };
        assert!(problem.contains("string"), "{problem}");
    }

    /// The bug this exists to prevent, in full.
    ///
    /// A live model called `query_metrics --metric http.server.duration`. The
    /// real flag is `--metric_name`, so the value was dropped, the query ran
    /// UNFILTERED, and the endpoint returned 200 with the average across every
    /// metric in the project (~62 million — a mean of latency-in-ms, error
    /// rates and memory-in-bytes mixed together). The model then reasoned
    /// confidently about thresholds from that number. Nothing downstream could
    /// have caught it: the response was well-formed and the status was 200.
    #[test]
    fn near_miss_filter_name_is_rejected_instead_of_silently_unfiltering() {
        let op = make_op(
            "query_metrics",
            "/otel/metrics",
            vec![
                query_param("project_id", false),
                query_param("metric_name", false),
                query_param("start_time", false),
            ],
        );

        let params = serde_json::json!({ "metric": "http.server.duration" });
        let err = build_request_parts(&op, &params, &[], 20, 100)
            .expect_err("an unfiltered query must never be passed off as a filtered one");

        let ApiToolError::UnknownParams { unknown, valid, .. } = &err else {
            panic!("expected UnknownParams, got {err:?}");
        };
        assert!(
            unknown.contains("metric_name"),
            "should suggest it: {unknown}"
        );
        assert!(
            valid.contains("start_time"),
            "should list the real flags: {valid}"
        );
        // Saying nothing ran is what stops the model treating the failure as an
        // empty result set and concluding the metric has no data.
        assert!(err.to_string().contains("Nothing was queried"), "{err}");
    }

    /// Optional filters must still be omittable — the whole point of the
    /// unfiltered query is that it is legitimate when you meant it.
    #[test]
    fn omitting_optional_params_is_still_fine() {
        let op = make_op(
            "query_metrics",
            "/otel/metrics",
            vec![
                query_param("metric_name", false),
                query_param("start_time", false),
            ],
        );
        let params = serde_json::json!({ "metric_name": "http.server.duration" });
        build_request_parts(&op, &params, &[], 20, 100).expect("partial filters are valid");
    }

    #[test]
    fn unknown_param_with_no_close_match_is_reported_without_a_guess() {
        let op = make_op(
            "query_metrics",
            "/otel/metrics",
            vec![query_param("metric_name", false)],
        );
        let params = serde_json::json!({ "wibble": 1 });
        let err = build_request_parts(&op, &params, &[], 20, 100).expect_err("unknown");
        let ApiToolError::UnknownParams { unknown, .. } = &err else {
            panic!("expected UnknownParams, got {err:?}");
        };
        assert!(
            !unknown.contains("did you mean"),
            "a bad guess is worse than none: {unknown}"
        );
    }

    #[test]
    fn nearest_param_matches_prefixes_and_typos_but_not_unrelated_names() {
        let valid = ["metric_name", "environment_id", "start_time"];
        assert_eq!(nearest_param("metric", &valid), Some("metric_name"));
        // The transposition is built at runtime on purpose: a misspelled
        // literal in this file gets "corrected" by the repo's spell-check hook,
        // which would quietly reduce this to an exact-match assertion proving
        // nothing.
        let mut chars: Vec<char> = "metric_name".chars().collect();
        chars.swap(8, 9);
        let transposed: String = chars.into_iter().collect();
        assert_eq!(nearest_param(&transposed, &valid), Some("metric_name"));
        assert_eq!(nearest_param("zzzzzzzzzz", &valid), None);
    }

    /// A quoted number is present, required, and fatal.
    ///
    /// Seen live: `{"kind":"static","threshold":"0.5","comparator":">"}` passed
    /// the presence check, was staged, the user clicked Confirm, and the API
    /// answered `invalid type: string "0.5", expected f64`. Checking that a key
    /// exists catches only half the mistakes.
    #[test]
    fn quoted_number_in_object_body_param_is_rejected() {
        let op = op_with_detector();
        let params = serde_json::json!({
            "detection_config": { "kind": "static", "comparator": "gt", "threshold": "0.5" }
        });

        let err = build_request_parts(&op, &params, &[], 20, 100)
            .expect_err("a string where the API wants f64 must not be staged");
        let ApiToolError::BadObjectShape { problem, .. } = &err else {
            panic!("expected BadObjectShape, got {err:?}");
        };
        assert!(problem.contains("threshold"), "{problem}");
        // Showing the quotes is the whole point — `0.5` and `"0.5"` are
        // indistinguishable in a message that just echoes the value.
        assert!(problem.contains("the string \"0.5\""), "{problem}");
        assert!(problem.contains("number"), "{problem}");
    }

    /// The other half of the same payload: `>` is a natural way to write a
    /// comparator and not what the schema accepts (`gt|gte|lt|lte`). The
    /// schema's own description even warns "NOT the SQL operators" — but that
    /// text never reaches the model, so the enum has to be enforced.
    #[test]
    fn wrong_enum_spelling_in_object_body_param_lists_the_allowed_values() {
        let op = op_with_detector();
        let params = serde_json::json!({
            "detection_config": { "kind": "static", "comparator": ">", "threshold": 0.5 }
        });

        let err = build_request_parts(&op, &params, &[], 20, 100).expect_err("bad comparator");
        let ApiToolError::BadObjectShape { problem, .. } = &err else {
            panic!("expected BadObjectShape, got {err:?}");
        };
        assert!(problem.contains("comparator"), "{problem}");
        for allowed in ["gt", "gte", "lt", "lte"] {
            assert!(
                problem.contains(allowed),
                "should list {allowed}: {problem}"
            );
        }
    }

    /// An integer is a valid f64 — `300` must not be rejected for a number
    /// field, or every whole-numbered threshold breaks.
    #[test]
    fn integer_is_accepted_where_a_number_is_expected() {
        let op = op_with_detector();
        let params = serde_json::json!({
            "detection_config": { "kind": "static", "comparator": "gt", "threshold": 300 }
        });
        build_request_parts(&op, &params, &[], 20, 100).expect("integers are valid numbers");
    }

    /// Optional fields that are absent must not trip the type check.
    #[test]
    fn absent_optional_object_fields_are_not_type_checked() {
        let mut param = body_param("window", true);
        param.object_shape = Some(ObjectShape {
            discriminator: None,
            variants: std::collections::BTreeMap::new(),
            fields: vec![
                ObjectField {
                    name: "from".to_string(),
                    required: true,
                    ty: "string".to_string(),
                    enum_values: Vec::new(),
                },
                ObjectField {
                    name: "step".to_string(),
                    required: false,
                    ty: "integer".to_string(),
                    enum_values: Vec::new(),
                },
            ],
        });
        let op = make_write_op("create_thing", "POST", "/things", "Things", vec![param]);

        let params = serde_json::json!({ "window": { "from": "now-1h" } });
        build_request_parts(&op, &params, &[], 20, 100).expect("absent optional field is fine");
    }

    #[test]
    fn no_body_params_produces_no_body() {
        let op = make_op("list", "/things", vec![query_param("filter", false)]);
        let params = serde_json::json!({ "filter": "active" });
        let result = build_request_parts(&op, &params, &[], 20, 100).expect("should succeed");
        assert!(result.body.is_none());
    }

    #[test]
    fn enum_check_on_body_param_rejects_bad_value() {
        let op = make_write_op(
            "create_thing",
            "POST",
            "/things",
            "Things",
            vec![body_enum_param("status", &["active", "draft"], true)],
        );

        let params = serde_json::json!({ "status": "deleted" });
        let err =
            build_request_parts(&op, &params, &[], 20, 100).expect_err("bad enum should fail");

        assert!(
            matches!(err, ApiToolError::BadEnum { ref name, ref value, .. }
                if name == "status" && value == "deleted"),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn enum_check_on_body_param_accepts_valid_value() {
        let op = make_write_op(
            "create_thing",
            "POST",
            "/things",
            "Things",
            vec![body_enum_param("status", &["active", "draft"], true)],
        );

        let params = serde_json::json!({ "status": "active" });
        let result = build_request_parts(&op, &params, &[], 20, 100).expect("should succeed");
        let body = result.body.expect("body should be Some");
        assert_eq!(body["status"], serde_json::json!("active"));
    }

    // -----------------------------------------------------------------------
    // Missing required param
    // -----------------------------------------------------------------------

    #[test]
    fn missing_required_param_returns_error() {
        let op = make_op(
            "get_deployment",
            "/projects/{project_id}/deployments/{id}",
            vec![path_param("project_id"), path_param("id")],
        );

        // `id` is missing.
        let params = serde_json::json!({ "project_id": 42 });
        let err = build_request_parts(&op, &params, &[42], 20, 100).expect_err("should fail");

        assert!(
            matches!(err, ApiToolError::MissingParam { ref name, .. } if name == "id"),
            "unexpected error: {:?}",
            err
        );
    }

    #[test]
    fn optional_param_absent_is_ok() {
        let op = make_op("list_things", "/things", vec![query_param("filter", false)]);

        let params = serde_json::json!({});
        let result = build_request_parts(&op, &params, &[], 20, 100).expect("should succeed");
        assert!(result.query.is_empty());
    }

    #[test]
    fn required_query_param_absent_is_forwarded_not_errored() {
        // OpenAPI metadata is frequently wrong about query-param required-ness
        // (e.g. audit-log filters declared required). An absent "required" query
        // param must NOT hard-fail here — it's omitted and the router decides.
        let op = make_op(
            "list_audit_logs",
            "/audit/logs",
            vec![
                query_param("operation_type", true),
                query_param("user_id", true),
            ],
        );

        let params = serde_json::json!({});
        let result = build_request_parts(&op, &params, &[], 20, 100)
            .expect("must not error on absent query");
        assert!(
            result.query.is_empty(),
            "absent required query params should be omitted, got: {}",
            result.query
        );
    }

    #[test]
    fn required_query_param_present_is_used() {
        let op = make_op(
            "list_audit_logs",
            "/audit/logs",
            vec![query_param("operation_type", true)],
        );

        let params = serde_json::json!({ "operation_type": "user.login" });
        let result = build_request_parts(&op, &params, &[], 20, 100).expect("should succeed");
        assert_eq!(result.query, "operation_type=user.login");
    }

    // -----------------------------------------------------------------------
    // Enum membership
    // -----------------------------------------------------------------------

    #[test]
    fn bad_enum_value_returns_error() {
        let op = make_op(
            "list_deployments",
            "/deployments",
            vec![enum_query_param("status", &["running", "stopped"])],
        );

        let params = serde_json::json!({ "status": "unknown_status" });
        let err = build_request_parts(&op, &params, &[], 20, 100).expect_err("should fail");

        assert!(
            matches!(err, ApiToolError::BadEnum { ref name, ref value, .. }
                if name == "status" && value == "unknown_status"),
            "unexpected error: {:?}",
            err
        );
    }

    #[test]
    fn valid_enum_value_is_accepted() {
        let op = make_op(
            "list_deployments",
            "/deployments",
            vec![enum_query_param("status", &["running", "stopped"])],
        );

        let params = serde_json::json!({ "status": "running" });
        let result = build_request_parts(&op, &params, &[], 20, 100).expect("should succeed");
        assert!(result.query.contains("status=running"));
    }

    // -----------------------------------------------------------------------
    // Project scoping
    // -----------------------------------------------------------------------

    #[test]
    fn project_not_allowed_returns_error() {
        let op = make_op(
            "get_project",
            "/projects/{project_id}",
            vec![path_param("project_id")],
        );

        // Caller only has access to project 1; they supply 99.
        let params = serde_json::json!({ "project_id": 99 });
        let err = build_request_parts(&op, &params, &[1, 2], 20, 100).expect_err("should fail");

        assert!(
            matches!(err, ApiToolError::ProjectNotAllowed { project_id: 99, .. }),
            "unexpected error: {:?}",
            err
        );
    }

    #[test]
    fn project_auto_fill_when_single_accessible() {
        let op = make_op(
            "get_project",
            "/projects/{project_id}",
            vec![path_param("project_id")],
        );

        // Caller has exactly one project and does not supply project_id.
        let params = serde_json::json!({});
        let result = build_request_parts(&op, &params, &[7], 20, 100).expect("should succeed");
        assert_eq!(result.path, "/projects/7");
    }

    #[test]
    fn project_path_param_named_id_auto_fills() {
        // The common `/projects/{id}/…` shape: the project's own path param is
        // named `id`, not `project_id`. It must still auto-fill from the chat's
        // project scope, so the model never has to supply (or ask for) it.
        let op = make_op(
            "get_last_deployment",
            "/projects/{id}/last-deployment",
            vec![path_param("id")],
        );

        let params = serde_json::json!({});
        let result = build_request_parts(&op, &params, &[42], 20, 100)
            .expect("project `id` should auto-fill from scope");
        assert_eq!(result.path, "/projects/42/last-deployment");
    }

    #[test]
    fn non_project_id_path_param_stays_required() {
        // An `id` that is NOT the leading `/projects/{id}` segment (here a
        // resource id) is a genuine required path param the model must supply —
        // it must not be silently filled with the project id.
        let op = make_op("get_thing", "/things/{id}", vec![path_param("id")]);

        let err = build_request_parts(&op, &serde_json::json!({}), &[42], 20, 100)
            .expect_err("non-project id must stay required");
        assert!(
            matches!(err, ApiToolError::MissingParam { ref name, .. } if name == "id"),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn supplied_project_in_allowed_list_is_accepted() {
        let op = make_op(
            "get_project",
            "/projects/{project_id}",
            vec![path_param("project_id")],
        );

        let params = serde_json::json!({ "project_id": 2 });
        let result =
            build_request_parts(&op, &params, &[1, 2, 3], 20, 100).expect("should succeed");
        assert_eq!(result.path, "/projects/2");
    }

    // -----------------------------------------------------------------------
    // Limit injection + clamping
    // -----------------------------------------------------------------------

    #[test]
    fn limit_defaults_when_absent() {
        let mut lim = query_param("limit", false);
        lim.ty = "integer".to_string();

        let op = make_op("list_things", "/things", vec![lim]);

        let params = serde_json::json!({});
        let result = build_request_parts(&op, &params, &[], 20, 100).expect("should succeed");
        assert!(
            result.query.contains("limit=20"),
            "query was: {}",
            result.query
        );
    }

    #[test]
    fn limit_clamped_to_max() {
        let mut lim = query_param("limit", false);
        lim.ty = "integer".to_string();

        let op = make_op("list_things", "/things", vec![lim]);

        let params = serde_json::json!({ "limit": 9999 });
        let result = build_request_parts(&op, &params, &[], 20, 100).expect("should succeed");
        assert!(
            result.query.contains("limit=100"),
            "query was: {}",
            result.query
        );
    }

    #[test]
    fn limit_within_bounds_is_preserved() {
        let mut lim = query_param("limit", false);
        lim.ty = "integer".to_string();

        let op = make_op("list_things", "/things", vec![lim]);

        let params = serde_json::json!({ "limit": 50 });
        let result = build_request_parts(&op, &params, &[], 20, 100).expect("should succeed");
        assert!(
            result.query.contains("limit=50"),
            "query was: {}",
            result.query
        );
    }

    // -----------------------------------------------------------------------
    // required_write_permission tests
    // -----------------------------------------------------------------------

    #[test]
    fn required_write_permission_deployments_post_is_create() {
        let op = op_tagged_with_method("Deployments", "POST");
        assert_eq!(
            required_write_permission(&op),
            Some(Permission::DeploymentsCreate)
        );
    }

    #[test]
    fn required_write_permission_deployments_delete_is_delete() {
        let op = op_tagged_with_method("Deployments", "DELETE");
        assert_eq!(
            required_write_permission(&op),
            Some(Permission::DeploymentsDelete)
        );
    }

    #[test]
    fn required_write_permission_deployments_patch_is_write() {
        let op = op_tagged_with_method("Deployments", "PATCH");
        assert_eq!(
            required_write_permission(&op),
            Some(Permission::DeploymentsWrite)
        );
    }

    #[test]
    fn required_write_permission_domains_post_is_create() {
        let op = op_tagged_with_method("Domains", "POST");
        assert_eq!(
            required_write_permission(&op),
            Some(Permission::DomainsCreate)
        );
    }

    #[test]
    fn required_write_permission_domains_delete_is_delete() {
        let op = op_tagged_with_method("Domains", "DELETE");
        assert_eq!(
            required_write_permission(&op),
            Some(Permission::DomainsDelete)
        );
    }

    #[test]
    fn required_write_permission_domains_patch_is_write() {
        let op = op_tagged_with_method("Domains", "PATCH");
        assert_eq!(
            required_write_permission(&op),
            Some(Permission::DomainsWrite)
        );
    }

    #[test]
    fn required_write_permission_environments_post_is_create() {
        let op = op_tagged_with_method("Environments", "POST");
        assert_eq!(
            required_write_permission(&op),
            Some(Permission::EnvironmentsCreate)
        );
    }

    #[test]
    fn required_write_permission_environments_delete_falls_back_to_write() {
        // No EnvironmentsDelete variant exists — DELETE falls back to Write.
        let op = op_tagged_with_method("Environments", "DELETE");
        assert_eq!(
            required_write_permission(&op),
            Some(Permission::EnvironmentsWrite)
        );
    }

    #[test]
    fn required_write_permission_alerts_uses_otel_write() {
        for method in ["POST", "PATCH", "DELETE"] {
            let op = op_tagged_with_method("Alerts", method);
            assert_eq!(
                required_write_permission(&op),
                Some(Permission::OtelWrite),
                "method {method}"
            );
        }
    }

    #[test]
    fn required_write_permission_unknown_tag_returns_none() {
        let op = op_tagged_with_method("Wibble", "POST");
        assert_eq!(required_write_permission(&op), None);
    }

    #[test]
    fn required_write_permission_no_tag_returns_none() {
        let op = op_tagged_with_method("", "POST");
        assert_eq!(required_write_permission(&op), None);
    }

    // -----------------------------------------------------------------------
    // prepare_write_cli tests (using InternalApiCaller with a minimal index)
    // -----------------------------------------------------------------------

    fn make_write_caller() -> InternalApiCaller {
        use utoipa::openapi::request_body::RequestBodyBuilder;
        use utoipa::openapi::{
            path::{OperationBuilder, PathItem, PathsBuilder},
            OpenApiBuilder,
        };

        // POST /deployments — requires `branch` (required body string)
        let create_deploy = OperationBuilder::new()
            .operation_id(Some("create_deployment"))
            .summary(Some("Create a deployment"))
            .tag("Deployments")
            .request_body(Some(
                RequestBodyBuilder::new()
                    .content(
                        "application/json",
                        build_inline_body_schema(&[("branch", "string", true, &[])]),
                    )
                    .build(),
            ))
            .build();

        let paths = PathsBuilder::new()
            .path("/deployments", {
                let mut item = PathItem::default();
                item.post = Some(create_deploy);
                item
            })
            .build();

        let openapi = OpenApiBuilder::new()
            .info(utoipa::openapi::Info::new("Test", "1.0.0"))
            .paths(paths)
            .build();

        let router = axum::Router::new();
        InternalApiCaller::new_write_allowlisted(
            router,
            &openapi,
            vec!["create_deployment".to_string()],
        )
    }

    #[test]
    fn prepare_write_cli_help_returns_help_outcome() {
        let caller = make_write_caller();
        use chrono::Utc;
        use temps_auth::{context::AuthContext, permissions::Role};
        use temps_entities::users;

        let now = Utc::now();
        let user = users::Model {
            id: 1,
            name: "Test".into(),
            email: "t@t.com".into(),
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
        let auth = AuthContext::new_session(user, Role::Admin);
        let scope = ApiCallScope {
            auth,
            project_ids: vec![],
        };

        let outcome = caller.prepare_write_cli("--help", &scope);
        assert!(
            matches!(outcome, WritePrepareOutcome::Help(_)),
            "expected Help outcome for --help"
        );
    }

    #[test]
    fn prepare_write_cli_valid_command_returns_prepared() {
        let caller = make_write_caller();
        use chrono::Utc;
        use temps_auth::{context::AuthContext, permissions::Role};
        use temps_entities::users;

        let now = Utc::now();
        let user = users::Model {
            id: 1,
            name: "Test".into(),
            email: "t@t.com".into(),
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
        let auth = AuthContext::new_session(user, Role::Admin);
        let scope = ApiCallScope {
            auth,
            project_ids: vec![],
        };

        // Supply the required `branch` param.
        let outcome = caller.prepare_write_cli("create_deployment --branch main", &scope);
        match outcome {
            WritePrepareOutcome::Prepared(pw) => {
                assert_eq!(pw.operation_id, "create_deployment");
                assert_eq!(pw.method, "POST");
                assert!(!pw.summary.is_empty());
                // FIX 2: required_permission must be populated at prepare time.
                // The operation is tagged "Deployments" with method POST → DeploymentsCreate.
                assert_eq!(
                    pw.required_permission.as_deref(),
                    Some("deployments:create"),
                    "required_permission must be set from the operation tag + method"
                );
            }
            WritePrepareOutcome::Help(h) => panic!("expected Prepared, got Help: {h}"),
            WritePrepareOutcome::Invalid(e) => panic!("expected Prepared, got Invalid: {e}"),
        }
    }

    #[test]
    fn prepare_write_cli_missing_required_body_param_returns_invalid() {
        let caller = make_write_caller();
        use chrono::Utc;
        use temps_auth::{context::AuthContext, permissions::Role};
        use temps_entities::users;

        let now = Utc::now();
        let user = users::Model {
            id: 1,
            name: "Test".into(),
            email: "t@t.com".into(),
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
        let auth = AuthContext::new_session(user, Role::Admin);
        let scope = ApiCallScope {
            auth,
            project_ids: vec![],
        };

        // Do NOT supply `branch` — should get Invalid.
        let outcome = caller.prepare_write_cli("create_deployment", &scope);
        assert!(
            matches!(outcome, WritePrepareOutcome::Invalid(_)),
            "expected Invalid outcome for missing required param"
        );
    }
}
