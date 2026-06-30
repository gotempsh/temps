//! Read-only OpenAPI index.
//!
//! [`ReadOnlyApiIndex`] is built from a `utoipa::openapi::OpenApi` value; it
//! retains only `GET` operations and provides keyword-ranked search over them.
//!
//! ## utoipa 5.4.0 type map used here
//!
//! | Purpose             | utoipa type                                               |
//! |---------------------|-----------------------------------------------------------|
//! | Root document       | `utoipa::openapi::OpenApi`                                |
//! | Path map            | `openapi.paths.paths: BTreeMap<String, PathItem>`         |
//! | GET operation       | `path_item.get: Option<Operation>`                        |
//! | Operation tags      | `operation.tags: Option<Vec<String>>`                     |
//! | Operation params    | `operation.parameters: Option<Vec<Parameter>>`            |
//! | Parameter location  | `parameter.parameter_in: ParameterIn` (`Path`/`Query`/…) |
//! | Parameter required  | `parameter.required: Required` (`True`/`False`)           |
//! | Parameter schema    | `parameter.schema: Option<RefOr<Schema>>`                 |
//! | Schema type         | `Schema::Object(obj) => obj.schema_type: SchemaType`      |
//! | Schema enum values  | `obj.enum_values: Option<Vec<Value>>`                     |

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use utoipa::openapi::{
    path::{Operation, ParameterIn},
    RefOr, Required, Schema,
};

/// Whether this parameter lives in the URL path or the query string.
///
/// Header and Cookie parameters are ignored by this crate (GET operations
/// rarely use them for business data, and they cannot be injected safely
/// through the flat params object).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParamLocation {
    Path,
    Query,
}

impl std::fmt::Display for ParamLocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParamLocation::Path => write!(f, "path"),
            ParamLocation::Query => write!(f, "query"),
        }
    }
}

/// Compact description of a single parameter, extracted from the utoipa schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParamSpec {
    /// Parameter name as it appears in the path template or query string.
    pub name: String,
    /// Where the parameter is carried.
    pub location: ParamLocation,
    /// Whether the parameter must be supplied (path params are always required).
    pub required: bool,
    /// JSON Schema type as a short string ("string", "integer", "number",
    /// "boolean", "array", "object", or "any").  Derived from the utoipa
    /// `SchemaType`; defaults to "string" when the schema is absent or
    /// is a `$ref` (the full schema is not resolved here — Phase 2).
    pub ty: String,
    /// Enum variants if the parameter schema declares them.  Empty when the
    /// parameter accepts any value.
    pub enum_values: Vec<String>,
    /// Optional human-readable description from the OpenAPI document.
    pub description: Option<String>,
}

/// A single read-only operation kept in the index.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiOperation {
    /// Stable identifier for this operation, used as the tool call name.
    pub operation_id: String,
    /// URL path template, e.g. `/projects/{project_id}/deployments/{id}`.
    pub path: String,
    /// Always `"GET"` in the current implementation.
    pub method: String,
    /// One-line description from the OpenAPI `summary` field, if present.
    pub summary: Option<String>,
    /// Fuller prose from the OpenAPI `description` field (the body of the doc
    /// comment), if present. Often carries the disambiguating detail that the
    /// one-line summary omits (e.g. "this replaces the old stages endpoint"), so
    /// it's surfaced in `describe_api` to help the model choose the right one.
    pub description: Option<String>,
    /// Tags used for grouping, also used in keyword search.
    pub tags: Vec<String>,
    /// Typed parameter specifications derived from the OpenAPI document.
    pub params: Vec<ParamSpec>,
}

/// Compact view returned by [`ReadOnlyApiIndex::search`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationSummary {
    pub operation_id: String,
    pub method: String,
    pub path: String,
    pub summary: Option<String>,
    /// Compact parameter list (name, in, required, type, enum).
    pub params: Vec<ParamSpec>,
}

impl From<&ApiOperation> for OperationSummary {
    fn from(op: &ApiOperation) -> Self {
        Self {
            operation_id: op.operation_id.clone(),
            method: op.method.clone(),
            path: op.path.clone(),
            summary: op.summary.clone(),
            params: op.params.clone(),
        }
    }
}

/// Full schema view returned by [`crate::InternalApiCaller::describe`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationSchema {
    pub operation_id: String,
    pub method: String,
    pub path: String,
    pub summary: Option<String>,
    pub description: Option<String>,
    pub tags: Vec<String>,
    pub params: Vec<ParamSpec>,
}

impl From<&ApiOperation> for OperationSchema {
    fn from(op: &ApiOperation) -> Self {
        Self {
            operation_id: op.operation_id.clone(),
            method: op.method.clone(),
            path: op.path.clone(),
            summary: op.summary.clone(),
            description: op.description.clone(),
            tags: op.tags.clone(),
            params: op.params.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// ReadOnlyApiIndex
// ---------------------------------------------------------------------------

/// An in-memory index of read-only (GET) API operations built from a
/// `utoipa::openapi::OpenApi` document.
///
/// Construction is cheap (O(n) in the number of paths × operations) and the
/// result is `Clone`-able so it can be shared across threads via `Arc`.
#[derive(Debug, Clone)]
pub struct ReadOnlyApiIndex {
    operations: Vec<ApiOperation>,
    /// Tag name → human description, from the OpenAPI document's top-level
    /// `tags`. Used to describe CLI sections in `--help`. Empty when the
    /// document declares no tag descriptions.
    tag_descriptions: BTreeMap<String, String>,
}

/// Extract `tag name → description` from the OpenAPI document's top-level tags.
fn extract_tag_descriptions(openapi: &utoipa::openapi::OpenApi) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    if let Some(tags) = &openapi.tags {
        for tag in tags {
            if let Some(desc) = &tag.description {
                if !desc.trim().is_empty() {
                    map.insert(tag.name.clone(), desc.clone());
                }
            }
        }
    }
    map
}

impl ReadOnlyApiIndex {
    /// Build the index from an OpenAPI document, excluding operations whose
    /// `operation_id` is contained in `denylist`.
    ///
    /// Only `GET` operations are kept; all other methods are silently ignored.
    /// Operations whose `operation_id` is `None` are also skipped (they cannot
    /// be called by name).
    pub fn from_openapi(openapi: &utoipa::openapi::OpenApi, denylist: &[&str]) -> Self {
        let mut operations = Vec::new();

        for (path, path_item) in &openapi.paths.paths {
            if let Some(op) = &path_item.get {
                if let Some(ref op_id) = op.operation_id {
                    if denylist.contains(&op_id.as_str()) {
                        continue;
                    }
                    let api_op = build_api_operation(op_id.clone(), path.clone(), op);
                    operations.push(api_op);
                }
            }
        }

        Self {
            operations,
            tag_descriptions: extract_tag_descriptions(openapi),
        }
    }

    /// Build the index from an OpenAPI document, including ONLY operations whose
    /// `operation_id` is in `allowlist` (opt-in / secure-by-default).
    ///
    /// This is the production posture for the AI `call_api` tool: a new endpoint
    /// is never exposed to the model unless it is explicitly vetted and added to
    /// the allowlist, so a future credential-bearing GET can't leak by default.
    /// Only `GET` operations are kept; operations with no `operation_id` cannot
    /// be referenced and are skipped.
    pub fn from_openapi_allowlist(openapi: &utoipa::openapi::OpenApi, allowlist: &[&str]) -> Self {
        let mut operations = Vec::new();

        for (path, path_item) in &openapi.paths.paths {
            if let Some(op) = &path_item.get {
                if let Some(ref op_id) = op.operation_id {
                    if !allowlist.contains(&op_id.as_str()) {
                        continue;
                    }
                    let api_op = build_api_operation(op_id.clone(), path.clone(), op);
                    operations.push(api_op);
                }
            }
        }

        Self {
            operations,
            tag_descriptions: extract_tag_descriptions(openapi),
        }
    }

    /// All indexed operations, in document order.
    pub fn operations(&self) -> &[ApiOperation] {
        &self.operations
    }

    /// Human description for a tag/section, from the OpenAPI top-level tags.
    pub fn tag_description(&self, tag: &str) -> Option<&str> {
        self.tag_descriptions.get(tag).map(String::as_str)
    }

    /// Return a reference to the operation with the given `operation_id`,
    /// or `None` if it is not in the index.
    pub fn get(&self, operation_id: &str) -> Option<&ApiOperation> {
        self.operations
            .iter()
            .find(|op| op.operation_id == operation_id)
    }

    /// Keyword search over the index.
    ///
    /// Scoring: count of query tokens that appear (case-insensitive) in the
    /// concatenation of `operation_id`, `summary`, `tags`, and `path`.
    /// Results with score > 0 are returned, sorted descending by score,
    /// capped at 15 entries.  This is deliberately simple; Phase 2 will
    /// add embedding-based ranking.
    pub fn search(&self, query: &str) -> Vec<&ApiOperation> {
        let tokens: Vec<String> = query.split_whitespace().map(|t| t.to_lowercase()).collect();

        if tokens.is_empty() {
            return self.operations.iter().take(15).collect();
        }

        let mut scored: Vec<(usize, &ApiOperation)> = self
            .operations
            .iter()
            .filter_map(|op| {
                let haystack = build_search_haystack(op);
                let score = tokens
                    .iter()
                    .filter(|tok| haystack.contains(tok.as_str()))
                    .count();
                if score > 0 {
                    Some((score, op))
                } else {
                    None
                }
            })
            .collect();

        // Stable descending sort: higher score first, ties preserve insertion order.
        scored.sort_by(|a, b| b.0.cmp(&a.0));
        scored.into_iter().take(15).map(|(_, op)| op).collect()
    }

    /// Total number of indexed operations (useful for diagnostics).
    pub fn len(&self) -> usize {
        self.operations.len()
    }

    /// Whether the index is empty.
    pub fn is_empty(&self) -> bool {
        self.operations.is_empty()
    }

    /// Render a compact, model-facing catalogue of every indexed operation: one
    /// line per endpoint with its `operation_id`, method, path, query-param
    /// names, and summary. Injected into the chat system prompt so the model can
    /// pick an `operation_id` by scanning paths directly, instead of guessing
    /// keywords for `search_api` (which it tends to do poorly). Sorted by path
    /// for stable, scannable output. Path params are visible in the `{...}` of
    /// the path; only query params are listed explicitly.
    pub fn catalog(&self) -> String {
        let mut ops: Vec<&ApiOperation> = self.operations.iter().collect();
        ops.sort_by(|a, b| {
            a.path
                .cmp(&b.path)
                .then_with(|| a.operation_id.cmp(&b.operation_id))
        });

        let mut out = String::with_capacity(ops.len() * 96);
        for op in ops {
            out.push_str("- ");
            out.push_str(&op.operation_id);
            out.push_str(" — ");
            out.push_str(&op.method);
            out.push(' ');
            out.push_str(&op.path);

            let query: Vec<&str> = op
                .params
                .iter()
                .filter(|p| matches!(p.location, ParamLocation::Query))
                .map(|p| p.name.as_str())
                .collect();
            if !query.is_empty() {
                out.push_str(" [q: ");
                out.push_str(&query.join(", "));
                out.push(']');
            }

            if let Some(summary) = op.summary.as_deref().filter(|s| !s.is_empty()) {
                out.push_str(" — ");
                out.push_str(summary);
            }
            out.push('\n');
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

/// Construct an [`ApiOperation`] from an utoipa [`Operation`] and its path.
fn build_api_operation(operation_id: String, path: String, op: &Operation) -> ApiOperation {
    let summary = op.summary.clone();
    let description = op.description.clone();
    let tags = op.tags.clone().unwrap_or_default();
    let params = op
        .parameters
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .filter_map(build_param_spec)
        .collect();

    ApiOperation {
        operation_id,
        path,
        method: "GET".to_string(),
        summary,
        description,
        tags,
        params,
    }
}

/// Convert a single utoipa [`Parameter`] into a [`ParamSpec`].
///
/// Returns `None` for Header and Cookie parameters (not modelled by this crate).
fn build_param_spec(param: &utoipa::openapi::path::Parameter) -> Option<ParamSpec> {
    let location = match param.parameter_in {
        ParameterIn::Path => ParamLocation::Path,
        ParameterIn::Query => ParamLocation::Query,
        // Header and Cookie are not supported; skip them.
        ParameterIn::Header | ParameterIn::Cookie => return None,
    };

    let required = matches!(param.required, Required::True);

    let (ty, enum_values) = extract_type_and_enum(param.schema.as_ref());

    Some(ParamSpec {
        name: param.name.clone(),
        location,
        required,
        ty,
        enum_values,
        description: param.description.clone(),
    })
}

/// Extract a short type string and optional enum values from an optional
/// `RefOr<Schema>`.
///
/// `$ref` schemas are not resolved here (would need the full components map);
/// they return `("string", [])` as a safe default.
fn extract_type_and_enum(schema: Option<&RefOr<Schema>>) -> (String, Vec<String>) {
    let Some(ref_or) = schema else {
        return ("string".to_string(), vec![]);
    };

    let Schema::Object(obj) = ref_or.as_schema() else {
        // RefOr::Ref or a non-Object schema — cannot introspect safely.
        return ("string".to_string(), vec![]);
    };

    let ty = schema_type_to_string(&obj.schema_type);

    let enum_values = obj
        .enum_values
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .filter_map(|v| v.as_str().map(|s| s.to_string()))
        .collect();

    (ty, enum_values)
}

/// Convert a [`utoipa::openapi::schema::SchemaType`] to a short lowercase string.
fn schema_type_to_string(st: &utoipa::openapi::schema::SchemaType) -> String {
    use utoipa::openapi::schema::{SchemaType, Type};
    match st {
        SchemaType::Type(Type::String) => "string",
        SchemaType::Type(Type::Integer) => "integer",
        SchemaType::Type(Type::Number) => "number",
        SchemaType::Type(Type::Boolean) => "boolean",
        SchemaType::Type(Type::Array) => "array",
        SchemaType::Type(Type::Object) => "object",
        SchemaType::Type(Type::Null) => "null",
        SchemaType::Array(_) => "array",
        SchemaType::AnyValue => "any",
    }
    .to_string()
}

/// Build a lowercase string to search over for a given operation.
fn build_search_haystack(op: &ApiOperation) -> String {
    let mut parts = Vec::new();
    parts.push(op.operation_id.to_lowercase());
    parts.push(op.path.to_lowercase());
    if let Some(ref s) = op.summary {
        parts.push(s.to_lowercase());
    }
    for tag in &op.tags {
        parts.push(tag.to_lowercase());
    }
    parts.join(" ")
}

// ---------------------------------------------------------------------------
// Extension trait helper — unwrap RefOr<Schema> to &Schema if not a $ref.
// ---------------------------------------------------------------------------

trait RefOrExt {
    fn as_schema(&self) -> &Schema;
}

impl RefOrExt for RefOr<Schema> {
    fn as_schema(&self) -> &Schema {
        match self {
            RefOr::T(s) => s,
            RefOr::Ref(_) => {
                // We return a static default; callers check for Object variant.
                static EMPTY: std::sync::OnceLock<Schema> = std::sync::OnceLock::new();
                EMPTY.get_or_init(Schema::default)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use utoipa::openapi::{
        path::{OperationBuilder, ParameterBuilder, ParameterIn, PathItem, PathsBuilder},
        schema::{ObjectBuilder, SchemaType, Type},
        OpenApiBuilder, RefOr, Required, Schema,
    };

    /// Build a minimal GET-only OpenApi document for testing.
    fn test_openapi() -> utoipa::openapi::OpenApi {
        // Operation with two params: a path param `project_id` and a query param `limit`.
        let list_deployments = OperationBuilder::new()
            .operation_id(Some("list_deployments"))
            .summary(Some("List all deployments for a project"))
            .tag("Deployments")
            .parameter(
                ParameterBuilder::new()
                    .name("project_id")
                    .parameter_in(ParameterIn::Path)
                    .required(Required::True)
                    .schema(Some(RefOr::T(Schema::Object(
                        ObjectBuilder::new()
                            .schema_type(SchemaType::Type(Type::Integer))
                            .build(),
                    ))))
                    .build(),
            )
            .parameter(
                ParameterBuilder::new()
                    .name("limit")
                    .parameter_in(ParameterIn::Query)
                    .required(Required::False)
                    .schema(Some(RefOr::T(Schema::Object(
                        ObjectBuilder::new()
                            .schema_type(SchemaType::Type(Type::Integer))
                            .build(),
                    ))))
                    .build(),
            )
            .build();

        // Operation with an enum param.
        let list_services = OperationBuilder::new()
            .operation_id(Some("list_services"))
            .summary(Some("List services by status"))
            .tag("Services")
            .parameter(
                ParameterBuilder::new()
                    .name("status")
                    .parameter_in(ParameterIn::Query)
                    .required(Required::False)
                    .schema(Some(RefOr::T(Schema::Object(
                        ObjectBuilder::new()
                            .schema_type(SchemaType::Type(Type::String))
                            .enum_values(Some(["running", "stopped", "error"]))
                            .build(),
                    ))))
                    .build(),
            )
            .build();

        // POST operation — should be excluded from the index.
        let create_deployment = OperationBuilder::new()
            .operation_id(Some("create_deployment"))
            .summary(Some("Create a new deployment"))
            .build();

        let paths = PathsBuilder::new()
            .path(
                "/projects/{project_id}/deployments",
                PathItem::new(utoipa::openapi::path::HttpMethod::Get, list_deployments),
            )
            .path(
                "/services",
                PathItem::new(utoipa::openapi::path::HttpMethod::Get, list_services),
            )
            .path("/deployments", {
                // POST is on this path; make a PathItem directly.
                let mut item = PathItem::default();
                item.post = Some(create_deployment);
                item
            })
            .build();

        OpenApiBuilder::new()
            .info(utoipa::openapi::Info::new("Test API", "1.0.0"))
            .paths(paths)
            .build()
    }

    #[test]
    fn index_keeps_only_get_operations() {
        let api = test_openapi();
        let index = ReadOnlyApiIndex::from_openapi(&api, &[]);

        // Should contain list_deployments and list_services but NOT create_deployment.
        assert_eq!(index.len(), 2);
        assert!(index.get("list_deployments").is_some());
        assert!(index.get("list_services").is_some());
        assert!(index.get("create_deployment").is_none());
    }

    #[test]
    fn catalog_lists_each_endpoint_with_path_query_and_summary() {
        let api = test_openapi();
        let index = ReadOnlyApiIndex::from_openapi(&api, &[]);
        let catalog = index.catalog();

        // One line per GET op, sorted by path (deployments path < /services).
        let lines: Vec<&str> = catalog.lines().collect();
        assert_eq!(lines.len(), 2, "catalog: {catalog}");
        assert_eq!(
            lines[0],
            "- list_deployments — GET /projects/{project_id}/deployments [q: limit] — List all deployments for a project"
        );
        assert_eq!(
            lines[1],
            "- list_services — GET /services [q: status] — List services by status"
        );
        // POST op is never indexed, so never catalogued.
        assert!(!catalog.contains("create_deployment"));
    }

    #[test]
    fn index_respects_denylist() {
        let api = test_openapi();
        let index = ReadOnlyApiIndex::from_openapi(&api, &["list_deployments"]);

        assert_eq!(index.len(), 1);
        assert!(index.get("list_deployments").is_none());
        assert!(index.get("list_services").is_some());
    }

    #[test]
    fn allowlist_includes_only_listed_ops() {
        let api = test_openapi();
        // Opt-in: only the named GET is indexed; everything else is invisible.
        let index = ReadOnlyApiIndex::from_openapi_allowlist(&api, &["list_services"]);

        assert_eq!(index.len(), 1);
        assert!(index.get("list_services").is_some());
        assert!(index.get("list_deployments").is_none());
    }

    #[test]
    fn empty_allowlist_indexes_nothing() {
        // Secure default: an empty allowlist exposes no operations at all.
        let api = test_openapi();
        let index = ReadOnlyApiIndex::from_openapi_allowlist(&api, &[]);
        assert_eq!(index.len(), 0);
    }

    #[test]
    fn search_ranks_by_keyword_overlap() {
        let api = test_openapi();
        let index = ReadOnlyApiIndex::from_openapi(&api, &[]);

        let results = index.search("deployments");
        assert!(!results.is_empty());
        // "list_deployments" should appear first (matches both operation_id and path)
        assert_eq!(results[0].operation_id, "list_deployments");
    }

    #[test]
    fn search_returns_empty_for_no_matches() {
        let api = test_openapi();
        let index = ReadOnlyApiIndex::from_openapi(&api, &[]);

        let results = index.search("xxxxxxxxxxxxxxnotfound");
        assert!(results.is_empty());
    }

    #[test]
    fn search_returns_all_on_empty_query_capped_at_15() {
        let api = test_openapi();
        let index = ReadOnlyApiIndex::from_openapi(&api, &[]);

        let results = index.search("");
        // We have 2 ops; empty query returns all (capped at 15).
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn param_spec_extracts_type_and_enum_values() {
        let api = test_openapi();
        let index = ReadOnlyApiIndex::from_openapi(&api, &[]);

        let svc = index
            .get("list_services")
            .expect("list_services must be in index");
        let status_param = svc
            .params
            .iter()
            .find(|p| p.name == "status")
            .expect("status param must exist");

        assert_eq!(status_param.ty, "string");
        assert_eq!(
            status_param.enum_values,
            vec!["running", "stopped", "error"]
        );
        assert!(!status_param.required);
        assert_eq!(status_param.location, ParamLocation::Query);
    }

    #[test]
    fn param_spec_path_param_is_always_required() {
        let api = test_openapi();
        let index = ReadOnlyApiIndex::from_openapi(&api, &[]);

        let dep = index
            .get("list_deployments")
            .expect("list_deployments must be in index");
        let pid = dep
            .params
            .iter()
            .find(|p| p.name == "project_id")
            .expect("project_id must exist");

        assert!(pid.required);
        assert_eq!(pid.location, ParamLocation::Path);
        assert_eq!(pid.ty, "integer");
    }

    #[test]
    fn get_returns_none_for_unknown_id() {
        let api = test_openapi();
        let index = ReadOnlyApiIndex::from_openapi(&api, &[]);

        assert!(index.get("nonexistent_operation").is_none());
    }
}
