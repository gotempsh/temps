//! [`InternalApiCaller`] — executes read-only API calls via Axum router replay.
//!
//! ## How it works
//!
//! 1. [`build_request_parts`] validates and routes the flat `params` object into
//!    a substituted path and a URL-encoded query string (pure, no I/O).
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

use crate::{
    error::ApiToolError,
    index::{
        ApiOperation, OperationSchema, OperationSummary, ParamLocation, ParamSpec, ReadOnlyApiIndex,
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

/// Route a flat `params` JSON object into a substituted path + query string.
///
/// This function is pure (no I/O) and can be unit-tested without a running router.
///
/// ## Parameter handling
///
/// - **Path params** (`ParamLocation::Path`): substituted into `{name}` placeholders
///   in the path template.
/// - **Query params** (`ParamLocation::Query`): appended as `key=value` pairs
///   (URL-encoded).
/// - **Required check**: `ApiToolError::MissingParam` only if a required *path*
///   param is absent (the URL can't be built without it). Absent *query* params
///   are forwarded as-is regardless of their (often-unreliable) `required` flag;
///   the real router is the source of truth and returns a real 4xx if needed.
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

    let mut path = op.path.clone();
    let mut query_parts: Vec<String> = Vec::new();

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
        } else if param.name == "limit" {
            // Limit injection: default + clamp.
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
            // Normal param lookup. An absent param is simply omitted and the
            // request is replayed through the real router, which is the single
            // source of truth for what's actually required.
            //
            // We hard-fail ONLY on a missing PATH param, because the URL
            // literally cannot be built without it. Query params marked
            // `required` in the OpenAPI metadata are NOT enforced here: that
            // metadata is frequently wrong (optional filters declared required,
            // e.g. audit-log filters), and enforcing it forces the model to
            // invent junk values instead of letting the endpoint apply its real
            // defaults. If a query param is genuinely required, the router
            // returns a real 4xx the model can read and react to.
            match params_obj.and_then(|o| o.get(&param.name)) {
                Some(v) => v.clone(),
                None => {
                    if matches!(param.location, ParamLocation::Path) {
                        return Err(ApiToolError::MissingParam {
                            name: param.name.clone(),
                            operation_id: op.operation_id.clone(),
                        });
                    } else {
                        continue; // query param absent — let the router decide
                    }
                }
            }
        };

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

        let str_val = value_to_string(&value);

        match param.location {
            ParamLocation::Path => {
                let placeholder = format!("{{{}}}", param.name);
                path = path.replace(&placeholder, &urlencoding::encode(&str_val));
            }
            ParamLocation::Query => {
                query_parts.push(format!(
                    "{}={}",
                    urlencoding::encode(&param.name),
                    urlencoding::encode(&str_val),
                ));
            }
        }
    }

    Ok(BuiltRequest {
        path,
        query: query_parts.join("&"),
    })
}

// ---------------------------------------------------------------------------
// InternalApiCaller
// ---------------------------------------------------------------------------

/// Substrate-agnostic caller that turns read-only API operations into Axum
/// router replays.
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

    /// Construct a caller whose index contains ONLY the allowlisted operation
    /// IDs (opt-in / secure-by-default). This is the production constructor for
    /// the AI `call_api` tool — see the curated list in the serve wiring. A GET
    /// endpoint the model has never been vetted for is simply absent from the
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

    /// Search the read-only API index, hiding operations the caller can't read
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

    /// Run one virtual-CLI command line (see [`crate::cli`]). Parsing/help are
    /// pure; execution replays the resolved GET through the router with the
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

    /// Execute a read-only GET call by replaying a synthetic request through the
    /// Axum router with `scope.auth` injected into extensions.
    ///
    /// ## Steps
    ///
    /// 1. Look up the operation in the index — returns `UnknownOperation` if absent.
    /// 2. Call [`build_request_parts`] to validate params and build path+query.
    /// 3. Build an `axum::http::Request<Body>` and insert `scope.auth` into extensions.
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
            "InternalApiCaller: dispatching call"
        );

        // Step 2: validate params and build path + query.
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

        let mut req = Request::builder()
            .method(http::Method::GET)
            .uri(&uri)
            .body(Body::empty())
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
    use crate::index::{ApiOperation, ParamLocation, ParamSpec};

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

    fn path_param(name: &str) -> ParamSpec {
        ParamSpec {
            name: name.to_string(),
            location: ParamLocation::Path,
            required: true,
            ty: "integer".to_string(),
            enum_values: vec![],
            description: None,
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
}
