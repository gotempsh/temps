//! API operation index — GET (read-only) and vetted write operations.
//!
//! [`ReadOnlyApiIndex`] was originally built from only `GET` operations.  It
//! now also backs a **vetted write index** via
//! [`ReadOnlyApiIndex::from_openapi_write_allowlist`], which indexes
//! `POST`/`PUT`/`PATCH`/`DELETE` operations whose `operation_id` has been
//! explicitly allowlisted.  Body fields are surfaced as `ParamSpec` entries
//! with `location: ParamLocation::Body` so the same CLI/search/describe
//! machinery works unchanged over write operations.
//!
//! ## utoipa 5.4.0 type map used here
//!
//! | Purpose              | utoipa type                                               |
//! |----------------------|-----------------------------------------------------------|
//! | Root document        | `utoipa::openapi::OpenApi`                                |
//! | Path map             | `openapi.paths.paths: BTreeMap<String, PathItem>`         |
//! | GET operation        | `path_item.get: Option<Operation>`                        |
//! | POST operation       | `path_item.post: Option<Operation>`                       |
//! | PUT operation        | `path_item.put: Option<Operation>`                        |
//! | PATCH operation      | `path_item.patch: Option<Operation>`                      |
//! | DELETE operation     | `path_item.delete: Option<Operation>`                     |
//! | Operation tags       | `operation.tags: Option<Vec<String>>`                     |
//! | Operation params     | `operation.parameters: Option<Vec<Parameter>>`            |
//! | Request body         | `operation.request_body: Option<RequestBody>`             |
//! | Request body content | `request_body.content: BTreeMap<String, Content>`         |
//! | Content schema       | `content.schema: Option<RefOr<Schema>>`                   |
//! | Components           | `openapi.components: Option<Components>`                  |
//! | Component schemas    | `components.schemas: BTreeMap<String, RefOr<Schema>>`     |
//! | Parameter location   | `parameter.parameter_in: ParameterIn` (`Path`/`Query`/…) |
//! | Parameter required   | `parameter.required: Required` (`True`/`False`)           |
//! | Parameter schema     | `parameter.schema: Option<RefOr<Schema>>`                 |
//! | Schema type          | `Schema::Object(obj) => obj.schema_type: SchemaType`      |
//! | Schema enum values   | `obj.enum_values: Option<Vec<Value>>`                     |
//! | Object properties    | `obj.properties: BTreeMap<String, RefOr<Schema>>`         |
//! | Object required      | `obj.required: Vec<String>`                               |
//! | $ref location        | `Ref::ref_location: String`                               |

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use utoipa::openapi::{
    path::{Operation, ParameterIn},
    RefOr, Required, Schema,
};

/// Where this parameter is carried in the HTTP request.
///
/// Header and Cookie parameters are ignored by this crate (GET operations
/// rarely use them for business data, and they cannot be injected safely
/// through the flat params object).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParamLocation {
    Path,
    Query,
    /// Request-body field (JSON object property).  Only present on write
    /// operations indexed via [`ReadOnlyApiIndex::from_openapi_write_allowlist`].
    Body,
}

impl std::fmt::Display for ParamLocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParamLocation::Path => write!(f, "path"),
            ParamLocation::Query => write!(f, "query"),
            ParamLocation::Body => write!(f, "body"),
        }
    }
}

/// One field inside a `$ref`'d body object.
///
/// Carries the type and enum members, not just the name: knowing a key is
/// *required* only catches half the mistakes. A `threshold` supplied as the
/// string `"0.5"` and a `comparator` of `">"` (the schema wants `gt`) are both
/// present and both fatal, and both used to survive validation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectField {
    pub name: String,
    pub required: bool,
    /// Short JSON Schema type: `string`, `number`, `integer`, `boolean`,
    /// `array`, `object`, or `any` when it could not be determined.
    pub ty: String,
    /// Permitted values, when the field is an enum. Empty otherwise.
    pub enum_values: Vec<String>,
}

impl ObjectField {
    /// `"name": <type>` or `"name": one of a|b|c` — the form used in help text
    /// and in the validation error.
    fn describe(&self) -> String {
        if self.enum_values.is_empty() {
            format!("\"{}\": <{}>", self.name, self.ty)
        } else {
            format!("\"{}\": one of {}", self.name, self.enum_values.join("|"))
        }
    }
}

/// The inner shape of a body parameter whose type is a `$ref`'d object.
///
/// Without this, such a parameter reaches the model as a bare `object` with no
/// hint of what belongs inside it, and a wrong guess is not caught until the
/// request is actually executed — which, on the propose-then-confirm path,
/// means *after* a human has approved it. Capturing the fields lets the call be
/// rejected at validation time, while the model can still fix it.
///
/// Two shapes are modelled:
///
/// - a plain object → [`Self::fields`] describes it;
/// - a tagged union (`oneOf` where every variant pins one property to a single
///   enum value) → [`Self::discriminator`] names that property and
///   [`Self::variants`] maps each tag to that variant's fields.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ObjectShape {
    /// Property that selects the variant (e.g. `kind`), for a tagged union.
    pub discriminator: Option<String>,
    /// Variant tag → that variant's fields (discriminator included).
    /// Empty for a plain object.
    pub variants: BTreeMap<String, Vec<ObjectField>>,
    /// Fields of a plain (non-union) object.
    pub fields: Vec<ObjectField>,
}

impl ObjectShape {
    /// Is there anything worth validating or showing? A resolved-but-empty
    /// shape tells the model nothing.
    pub fn is_informative(&self) -> bool {
        !self.fields.is_empty() || !self.variants.is_empty()
    }

    /// The fields that apply to a value carrying the given discriminator tag,
    /// or the plain object's fields when this is not a union.
    pub fn fields_for(&self, tag: Option<&str>) -> Option<&[ObjectField]> {
        match tag {
            Some(tag) => self.variants.get(tag).map(Vec::as_slice),
            None => Some(&self.fields),
        }
    }

    /// One-line summary of the accepted JSON, for `--help` and error messages.
    ///
    /// The model does not reliably run `--help` before calling a write
    /// operation, so this same text is repeated in the validation error — that
    /// is the one place it is guaranteed to be read. Types and enum members are
    /// included because the mistakes seen in practice are a quoted number and a
    /// plausible-but-wrong enum spelling (`">"` for `gt`), neither of which a
    /// bare list of key names would have prevented.
    pub fn describe(&self) -> String {
        match &self.discriminator {
            Some(d) => {
                let variants: Vec<String> = self
                    .variants
                    .iter()
                    .map(|(tag, fields)| {
                        let rest: Vec<String> = fields
                            .iter()
                            .filter(|f| f.name != *d)
                            .map(ObjectField::describe)
                            .collect();
                        format!("{{\"{d}\": \"{tag}\", {}}}", rest.join(", "))
                    })
                    .collect();
                format!("object, one of: {}", variants.join(" | "))
            }
            None => format!(
                "object: {{{}}}",
                self.fields
                    .iter()
                    .map(ObjectField::describe)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
}

/// Compact description of a single parameter, extracted from the utoipa schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParamSpec {
    /// Parameter name as it appears in the path template, query string, or
    /// request body JSON object.
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
    /// For a body param that is a `$ref` to an object schema: the keys that
    /// object requires, so a malformed value is caught during validation rather
    /// than at execution. `None` for scalars and unresolvable schemas.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object_shape: Option<ObjectShape>,
}

/// A single operation kept in the index (GET *or* an allowlisted write method).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiOperation {
    /// Stable identifier for this operation, used as the tool call name.
    pub operation_id: String,
    /// URL path template, e.g. `/projects/{project_id}/deployments/{id}`.
    pub path: String,
    /// HTTP method in uppercase: `"GET"`, `"POST"`, `"PUT"`, `"PATCH"`, or
    /// `"DELETE"`.
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
    /// For write operations this includes body fields appended after any
    /// path/query params.
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

/// An in-memory index of API operations built from a `utoipa::openapi::OpenApi`
/// document.
///
/// The index supports two construction modes:
///
/// - **Read-only (GET-only)**: [`from_openapi`] / [`from_openapi_allowlist`] —
///   only `GET` operations are kept.  This is the existing production posture for
///   the AI `call_api` read tool.
/// - **Vetted write**: [`from_openapi_write_allowlist`] — `POST`, `PUT`, `PATCH`,
///   and `DELETE` operations whose `operation_id` is explicitly allowlisted.
///   Body fields are resolved from the operation's `requestBody` and appended to
///   `params` as `ParamSpec { location: Body, .. }`.
///
/// Construction is cheap (O(n) in the number of paths × operations) and the
/// result is `Clone`-able so it can be shared across threads via `Arc`.
///
/// [`from_openapi`]: ReadOnlyApiIndex::from_openapi
/// [`from_openapi_allowlist`]: ReadOnlyApiIndex::from_openapi_allowlist
/// [`from_openapi_write_allowlist`]: ReadOnlyApiIndex::from_openapi_write_allowlist
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
                    let api_op = build_api_operation(op_id.clone(), path.clone(), "GET", op, &[]);
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
                    let api_op = build_api_operation(op_id.clone(), path.clone(), "GET", op, &[]);
                    operations.push(api_op);
                }
            }
        }

        Self {
            operations,
            tag_descriptions: extract_tag_descriptions(openapi),
        }
    }

    /// Build the read index from an OpenAPI document: allowlisted `GET`
    /// operations PLUS a separate, explicitly-vetted set of **read-only
    /// `POST`** operations.
    ///
    /// ## Why this exists
    ///
    /// HTTP method is the crate's structural proxy for "does this mutate?", and
    /// it is right almost everywhere. A handful of genuinely read-only
    /// operations are nonetheless `POST` because their *input* is too large or
    /// too structured for a query string — the motivating case is the metric
    /// alert backtest (`preview_alert`), which takes a whole detector config and
    /// returns "would this rule have fired?" without writing anything.
    ///
    /// Routing those through the *write* tool would be wrong in the other
    /// direction: the write tool is propose-then-confirm, so a backtest would
    /// stage a pending action and wait for a human — turning the cheap
    /// "check before you propose" step into the very thing it is meant to
    /// inform.
    ///
    /// ## Security posture
    ///
    /// `safe_posts` is a SECOND allowlist, deliberately separate from the GET
    /// allowlist so that adding an entry is a conscious act with its own review:
    /// **an operation belongs here only if it has no side effects.** A `POST`
    /// that writes anything must go in the write allowlist instead. Membership
    /// in `allowlist` does NOT grant POST access and membership here does NOT
    /// grant GET access — the two lists are matched against disjoint sets of
    /// operations. The router's `permission_guard!` remains the real boundary;
    /// this is defence in depth for what the model can even name.
    ///
    /// Request-body fields are resolved exactly as for the write index, so the
    /// model can pass body params as CLI flags.
    pub fn from_openapi_allowlist_with_safe_posts(
        openapi: &utoipa::openapi::OpenApi,
        allowlist: &[&str],
        safe_posts: &[&str],
    ) -> Self {
        let mut index = Self::from_openapi_allowlist(openapi, allowlist);

        for (path, path_item) in &openapi.paths.paths {
            let Some(op) = path_item.post.as_ref() else {
                continue;
            };
            let Some(ref op_id) = op.operation_id else {
                continue;
            };
            if !safe_posts.contains(&op_id.as_str()) {
                continue;
            }
            let body_params = resolve_body_params(op, openapi);
            index.operations.push(build_api_operation(
                op_id.clone(),
                path.clone(),
                "POST",
                op,
                &body_params,
            ));
        }

        index
    }

    /// Build the write index from an OpenAPI document, including ONLY non-GET
    /// operations whose `operation_id` is in `allowlist` (opt-in /
    /// secure-by-default).
    ///
    /// This is the production posture for the AI write tool — a new mutating
    /// endpoint is never exposed to the AI unless it has been explicitly vetted
    /// and added to the write allowlist.
    ///
    /// Each operation is indexed with its real HTTP method (`"POST"`, `"PUT"`,
    /// `"PATCH"`, or `"DELETE"`).  Request-body fields (from the
    /// `application/json` content type, preferring it over any other) are
    /// appended to `params` as `ParamSpec { location: Body, .. }`.  One level
    /// of `$ref` is resolved against `openapi.components.schemas`; nested
    /// refs/objects that cannot be further resolved become `ty: "object"` (best
    /// effort — never panics).
    pub fn from_openapi_write_allowlist(
        openapi: &utoipa::openapi::OpenApi,
        allowlist: &[&str],
    ) -> Self {
        let mut operations = Vec::new();

        // Pre-build a slice of (method-string, Option<Operation>) pairs from
        // each path_item so we can iterate uniformly.
        for (path, path_item) in &openapi.paths.paths {
            let write_ops: [(&str, Option<&Operation>); 4] = [
                ("POST", path_item.post.as_ref()),
                ("PUT", path_item.put.as_ref()),
                ("PATCH", path_item.patch.as_ref()),
                ("DELETE", path_item.delete.as_ref()),
            ];

            for (method, maybe_op) in &write_ops {
                let Some(op) = maybe_op else { continue };
                let Some(ref op_id) = op.operation_id else {
                    continue;
                };
                if !allowlist.contains(&op_id.as_str()) {
                    continue;
                }

                // Resolve body fields from the operation's requestBody.
                let body_params = resolve_body_params(op, openapi);

                let api_op =
                    build_api_operation(op_id.clone(), path.clone(), method, op, &body_params);
                operations.push(api_op);
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
        scored.sort_by_key(|b| std::cmp::Reverse(b.0));
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
///
/// `method` must be an uppercase HTTP method string.
/// `extra_params` are appended after the path/query params already extracted
/// from `op.parameters` — used to supply body fields for write operations.
fn build_api_operation(
    operation_id: String,
    path: String,
    method: &str,
    op: &Operation,
    extra_params: &[ParamSpec],
) -> ApiOperation {
    let summary = op.summary.clone();
    let description = op.description.clone();
    let tags = op.tags.clone().unwrap_or_default();
    let mut params: Vec<ParamSpec> = op
        .parameters
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .filter_map(build_param_spec)
        .collect();

    params.extend_from_slice(extra_params);

    ApiOperation {
        operation_id,
        path,
        method: method.to_string(),
        summary,
        description,
        tags,
        params,
    }
}

/// Resolve the request-body fields of `op` into a list of [`ParamSpec`]s with
/// `location: Body`.
///
/// Steps:
/// 1. Read `op.request_body` → its `content` map.
/// 2. Prefer `"application/json"`; fall back to the first entry.
/// 3. Get `content.schema`.
/// 4. If the schema is a `$ref`, resolve one level against
///    `openapi.components.schemas`.
/// 5. If we land on a `Schema::Object`, emit one `ParamSpec` per top-level
///    property.
/// 6. If we can't resolve (missing components, nested refs, non-object schema),
///    return an empty vec — best effort, never panics.
fn resolve_body_params(op: &Operation, openapi: &utoipa::openapi::OpenApi) -> Vec<ParamSpec> {
    let request_body = match op.request_body.as_ref() {
        Some(rb) => rb,
        None => return vec![],
    };

    // Pick application/json first, else the first content entry.
    let content = if let Some(c) = request_body.content.get("application/json") {
        c
    } else if let Some(c) = request_body.content.values().next() {
        c
    } else {
        return vec![];
    };

    let schema_ref_or = match content.schema.as_ref() {
        Some(s) => s,
        None => return vec![],
    };

    // Resolve one level of $ref.
    let resolved: &Schema = match schema_ref_or {
        RefOr::T(s) => s,
        RefOr::Ref(r) => {
            // Extract the last segment of the $ref path, e.g.
            // "#/components/schemas/CreateDeploymentRequest" → "CreateDeploymentRequest"
            let schema_name = r.ref_location.split('/').next_back().unwrap_or("");
            match openapi
                .components
                .as_ref()
                .and_then(|c| c.schemas.get(schema_name))
            {
                Some(RefOr::T(s)) => s,
                // Nested ref or not found — best effort.
                _ => return vec![],
            }
        }
    };

    // We only handle Object schemas (the common case for request bodies).
    let obj = match resolved {
        Schema::Object(o) => o,
        _ => return vec![],
    };

    let mut params = Vec::new();
    for (prop_name, prop_schema) in &obj.properties {
        let required = obj.required.contains(prop_name);
        // Body fields are frequently `$ref`s to scalar enum components. Resolve
        // them before extracting their type; treating a string enum as an opaque
        // object makes the CLI tell the model to JSON-encode a value that the API
        // actually expects as a plain string.
        let (ty, enum_values) = match deref_schema(prop_schema, openapi) {
            Some(schema) => extract_schema_type_and_enum(schema),
            None => ("string".to_string(), vec![]),
        };
        let object_shape =
            resolve_object_shape(prop_schema, openapi).filter(ObjectShape::is_informative);

        // Description from the property schema.
        //
        // The `$ref` arm matters more than it looks. A property that points at a
        // component schema still carries its own sibling `description` — and for
        // a field whose type resolves to a bare "object" here (a discriminated
        // union, say), that prose is the ONLY place the required shape is
        // written down. Dropping it left the model guessing at the JSON, and
        // guessing wrong: it has no other way to learn the discriminator's name.
        let description = match prop_schema {
            RefOr::T(Schema::Object(prop_obj)) => prop_obj.description.clone(),
            RefOr::Ref(r) if !r.description.trim().is_empty() => Some(r.description.clone()),
            _ => None,
        };

        params.push(ParamSpec {
            name: prop_name.clone(),
            location: ParamLocation::Body,
            required,
            ty,
            enum_values,
            description,
            object_shape,
        });
    }
    params
}

/// Resolve a body property's schema into an [`ObjectShape`], if it names one.
///
/// Follows a `$ref` into `components.schemas` (one level) and understands the
/// two shapes utoipa emits for Rust types that matter here: a plain struct
/// (`Schema::Object` with `required`) and an internally-tagged enum
/// (`Schema::OneOf` of `Schema::AllOf`s, each combining the variant's payload
/// `$ref` with an inline object pinning the tag property to one enum value).
///
/// Best effort throughout: anything it cannot make sense of yields `None`, and
/// the parameter is simply treated as an opaque object exactly as before.
fn resolve_object_shape(
    prop_schema: &RefOr<Schema>,
    openapi: &utoipa::openapi::OpenApi,
) -> Option<ObjectShape> {
    let schema = deref_schema(prop_schema, openapi)?;
    match schema {
        Schema::Object(o) => Some(ObjectShape {
            discriminator: None,
            variants: BTreeMap::new(),
            fields: object_fields(o, openapi),
        }),
        Schema::OneOf(one_of) => {
            let mut discriminator: Option<String> = None;
            let mut variants: BTreeMap<String, Vec<ObjectField>> = BTreeMap::new();

            for item in &one_of.items {
                let (tag_prop, tag_value, fields) = variant_shape(item, openapi)?;
                // Every variant must agree on the discriminator, otherwise this
                // is not a tagged union and we should not pretend it is.
                match &discriminator {
                    Some(existing) if *existing != tag_prop => return None,
                    Some(_) => {}
                    None => discriminator = Some(tag_prop),
                }
                variants.insert(tag_value, fields);
            }

            discriminator.map(|d| ObjectShape {
                discriminator: Some(d),
                variants,
                fields: Vec::new(),
            })
        }
        _ => None,
    }
}

/// Describe every property of an object schema, resolving each property's own
/// `$ref` so an enum defined as its own component (utoipa's default for a Rust
/// enum) still yields its permitted values rather than an opaque "string".
fn object_fields(
    obj: &utoipa::openapi::schema::Object,
    openapi: &utoipa::openapi::OpenApi,
) -> Vec<ObjectField> {
    obj.properties
        .iter()
        .map(|(name, prop)| {
            let resolved = deref_schema(prop, openapi);
            let (ty, enum_values) = match resolved {
                Some(Schema::Object(o)) => (
                    schema_type_to_string(&o.schema_type),
                    o.enum_values
                        .as_deref()
                        .unwrap_or(&[])
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect(),
                ),
                _ => ("any".to_string(), Vec::new()),
            };
            ObjectField {
                name: name.clone(),
                required: obj.required.contains(name),
                ty,
                enum_values,
            }
        })
        .collect()
}

/// Follow one level of `$ref` into `components.schemas`.
fn deref_schema<'a>(
    schema: &'a RefOr<Schema>,
    openapi: &'a utoipa::openapi::OpenApi,
) -> Option<&'a Schema> {
    match schema {
        RefOr::T(s) => Some(s),
        RefOr::Ref(r) => {
            let name = r.ref_location.split('/').next_back()?;
            match openapi.components.as_ref()?.schemas.get(name)? {
                RefOr::T(s) => Some(s),
                RefOr::Ref(_) => None, // nested ref — not worth chasing
            }
        }
    }
}

/// Read one `oneOf` variant, returning `(tag property, tag value, required keys)`.
///
/// Expects the utoipa shape for an internally-tagged enum: an `allOf` whose
/// members are the payload (often a `$ref`) plus an inline object that pins the
/// tag property to a single-valued enum.
fn variant_shape(
    item: &RefOr<Schema>,
    openapi: &utoipa::openapi::OpenApi,
) -> Option<(String, String, Vec<ObjectField>)> {
    let Schema::AllOf(all_of) = deref_schema(item, openapi)? else {
        return None;
    };

    let mut fields: Vec<ObjectField> = Vec::new();
    let mut tag: Option<(String, String)> = None;

    for member in &all_of.items {
        let Some(Schema::Object(o)) = deref_schema(member, openapi) else {
            continue;
        };
        for field in object_fields(o, openapi) {
            if !fields.iter().any(|f| f.name == field.name) {
                fields.push(field);
            }
        }
        // The tag is a property whose enum admits exactly one value.
        for (name, prop) in &o.properties {
            let RefOr::T(Schema::Object(p)) = prop else {
                continue;
            };
            let Some(values) = p.enum_values.as_ref() else {
                continue;
            };
            if let [serde_json::Value::String(v)] = values.as_slice() {
                tag = Some((name.clone(), v.clone()));
            }
        }
    }

    let (tag_prop, tag_value) = tag?;
    Some((tag_prop, tag_value, fields))
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
        // Path/query params are scalars in this API; only body fields carry
        // nested object schemas worth resolving.
        object_shape: None,
    })
}

/// Extract a short type string and optional enum values from an optional
/// `RefOr<Schema>`.
///
/// `$ref` schemas are not resolved here (would need the full components map);
/// they return `("string", [])` as a safe default.
pub(crate) fn extract_type_and_enum(schema: Option<&RefOr<Schema>>) -> (String, Vec<String>) {
    let Some(ref_or) = schema else {
        return ("string".to_string(), vec![]);
    };

    extract_schema_type_and_enum(ref_or.as_schema())
}

fn extract_schema_type_and_enum(schema: &Schema) -> (String, Vec<String>) {
    let Schema::Object(obj) = schema else {
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
        // NOT an array — this is a *type union*, which utoipa emits as
        // `type: ["string", "null"]` for an `Option<String>`. Reporting it as
        // "array" told the model that every optional field wanted a list, so
        // `--start_time <array>` for what is an RFC 3339 string. Report the one
        // real type instead and ignore the null.
        SchemaType::Array(types) => {
            let mut concrete = types.iter().filter(|t| !matches!(t, Type::Null));
            return match (concrete.next(), concrete.next()) {
                // The overwhelmingly common case: `T | null`.
                (Some(single), None) => schema_type_to_string(&SchemaType::Type(single.clone())),
                // Genuinely polymorphic, or null-only — neither is a type the
                // caller can act on, so don't pretend otherwise.
                (Some(_), Some(_)) => "any".to_string(),
                (None, _) => "null".to_string(),
            };
        }
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

        // POST operation — should be excluded from the GET index.
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

    /// Build an OpenApi document that contains POST + PATCH + DELETE write ops,
    /// and includes a component schema for request-body resolution via `$ref`.
    fn write_test_openapi() -> utoipa::openapi::OpenApi {
        use utoipa::openapi::{
            request_body::RequestBodyBuilder, schema::ComponentsBuilder, Content, ContentBuilder,
        };

        // Component schema: CreateProjectRequest { name: string (required), description: string }
        let create_req_schema = Schema::Object(
            ObjectBuilder::new()
                .schema_type(SchemaType::Type(Type::Object))
                .property(
                    "name",
                    RefOr::T(Schema::Object(
                        ObjectBuilder::new()
                            .schema_type(SchemaType::Type(Type::String))
                            .description(Some("Project name"))
                            .build(),
                    )),
                )
                .property(
                    "description",
                    RefOr::T(Schema::Object(
                        ObjectBuilder::new()
                            .schema_type(SchemaType::Type(Type::String))
                            .build(),
                    )),
                )
                .property(
                    "service_type",
                    RefOr::Ref(utoipa::openapi::schema::Ref::from_schema_name(
                        "ServiceType",
                    )),
                )
                .required("name")
                .required("service_type")
                .build(),
        );

        let service_type_schema = Schema::Object(
            ObjectBuilder::new()
                .schema_type(SchemaType::Type(Type::String))
                .enum_values(Some(["postgres", "redis"]))
                .build(),
        );

        let components = ComponentsBuilder::new()
            .schema("CreateProjectRequest", RefOr::T(create_req_schema))
            .schema("ServiceType", RefOr::T(service_type_schema))
            .build();

        // POST /projects — body is a $ref to CreateProjectRequest
        let create_project = OperationBuilder::new()
            .operation_id(Some("create_project"))
            .summary(Some("Create a new project"))
            .tag("Projects")
            .request_body(Some(
                RequestBodyBuilder::new()
                    .content(
                        "application/json",
                        ContentBuilder::new()
                            .schema(Some(RefOr::Ref(
                                utoipa::openapi::schema::Ref::from_schema_name(
                                    "CreateProjectRequest",
                                ),
                            )))
                            .build(),
                    )
                    .build(),
            ))
            .build();

        // PATCH /projects/{id} — body is an inline object with an enum field
        let update_project = OperationBuilder::new()
            .operation_id(Some("update_project"))
            .summary(Some("Update a project"))
            .tag("Projects")
            .parameter(
                ParameterBuilder::new()
                    .name("id")
                    .parameter_in(ParameterIn::Path)
                    .required(Required::True)
                    .schema(Some(RefOr::T(Schema::Object(
                        ObjectBuilder::new()
                            .schema_type(SchemaType::Type(Type::Integer))
                            .build(),
                    ))))
                    .build(),
            )
            .request_body(Some(
                RequestBodyBuilder::new()
                    .content(
                        "application/json",
                        Content::new(Some(RefOr::T(Schema::Object(
                            ObjectBuilder::new()
                                .schema_type(SchemaType::Type(Type::Object))
                                .property(
                                    "status",
                                    RefOr::T(Schema::Object(
                                        ObjectBuilder::new()
                                            .schema_type(SchemaType::Type(Type::String))
                                            .enum_values(Some(["active", "archived"]))
                                            .build(),
                                    )),
                                )
                                .required("status")
                                .build(),
                        )))),
                    )
                    .build(),
            ))
            .build();

        // DELETE /projects/{id} — no body
        let delete_project = OperationBuilder::new()
            .operation_id(Some("delete_project"))
            .summary(Some("Delete a project"))
            .tag("Projects")
            .parameter(
                ParameterBuilder::new()
                    .name("id")
                    .parameter_in(ParameterIn::Path)
                    .required(Required::True)
                    .schema(Some(RefOr::T(Schema::Object(
                        ObjectBuilder::new()
                            .schema_type(SchemaType::Type(Type::Integer))
                            .build(),
                    ))))
                    .build(),
            )
            .build();

        // GET /projects — should NOT appear in write allowlist index
        let list_projects = OperationBuilder::new()
            .operation_id(Some("list_projects"))
            .summary(Some("List projects"))
            .tag("Projects")
            .build();

        let paths = PathsBuilder::new()
            .path("/projects", {
                let mut item = PathItem::default();
                item.get = Some(list_projects);
                item.post = Some(create_project);
                item
            })
            .path("/projects/{id}", {
                let mut item = PathItem::default();
                item.patch = Some(update_project);
                item.delete = Some(delete_project);
                item
            })
            .build();

        OpenApiBuilder::new()
            .info(utoipa::openapi::Info::new("Write Test API", "1.0.0"))
            .paths(paths)
            .components(Some(components))
            .build()
    }

    // -----------------------------------------------------------------------
    // Existing GET-builder tests — must stay green
    // -----------------------------------------------------------------------

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

    // -----------------------------------------------------------------------
    // New write-allowlist tests
    // -----------------------------------------------------------------------

    #[test]
    fn write_allowlist_keeps_only_allowlisted_non_get_ops() {
        let api = write_test_openapi();
        let index = ReadOnlyApiIndex::from_openapi_write_allowlist(
            &api,
            &["create_project", "delete_project"],
        );

        assert_eq!(
            index.len(),
            2,
            "should have exactly 2 allowlisted write ops"
        );
        assert!(index.get("create_project").is_some());
        assert!(index.get("delete_project").is_some());
        // GET op is never included in the write index.
        assert!(index.get("list_projects").is_none());
        // Non-allowlisted write op is excluded.
        assert!(index.get("update_project").is_none());
    }

    #[test]
    fn write_allowlist_sets_correct_methods() {
        let api = write_test_openapi();
        let index = ReadOnlyApiIndex::from_openapi_write_allowlist(
            &api,
            &["create_project", "update_project", "delete_project"],
        );

        let create = index.get("create_project").expect("create_project");
        assert_eq!(create.method, "POST");

        let update = index.get("update_project").expect("update_project");
        assert_eq!(update.method, "PATCH");

        let delete = index.get("delete_project").expect("delete_project");
        assert_eq!(delete.method, "DELETE");
    }

    #[test]
    fn write_allowlist_resolves_ref_body_fields() {
        // create_project body is a $ref to CreateProjectRequest which has
        // `name` (required, string) and `description` (optional, string).
        let api = write_test_openapi();
        let index = ReadOnlyApiIndex::from_openapi_write_allowlist(&api, &["create_project"]);

        let op = index.get("create_project").expect("create_project");
        let body_params: Vec<&ParamSpec> = op
            .params
            .iter()
            .filter(|p| matches!(p.location, ParamLocation::Body))
            .collect();

        assert!(
            !body_params.is_empty(),
            "should have body params from $ref resolution"
        );

        let name_param = body_params
            .iter()
            .find(|p| p.name == "name")
            .expect("name param");
        assert!(name_param.required, "name should be required");
        assert_eq!(name_param.ty, "string");

        let desc_param = body_params
            .iter()
            .find(|p| p.name == "description")
            .expect("description param");
        assert!(!desc_param.required, "description should be optional");
        assert_eq!(desc_param.ty, "string");

        let service_type = body_params
            .iter()
            .find(|p| p.name == "service_type")
            .expect("service_type param");
        assert!(service_type.required);
        assert_eq!(service_type.ty, "string");
        assert_eq!(service_type.enum_values, vec!["postgres", "redis"]);
    }

    #[test]
    fn write_allowlist_resolves_inline_body_with_enum() {
        // update_project body is an inline object with `status` (required, enum).
        let api = write_test_openapi();
        let index = ReadOnlyApiIndex::from_openapi_write_allowlist(&api, &["update_project"]);

        let op = index.get("update_project").expect("update_project");

        // Should have path param `id` + body param `status`.
        let id_param = op.params.iter().find(|p| p.name == "id").expect("id param");
        assert_eq!(id_param.location, ParamLocation::Path);

        let status_param = op
            .params
            .iter()
            .find(|p| p.name == "status")
            .expect("status param");
        assert_eq!(status_param.location, ParamLocation::Body);
        assert!(status_param.required, "status should be required");
        assert_eq!(status_param.enum_values, vec!["active", "archived"]);
    }

    #[test]
    fn write_allowlist_empty_returns_no_ops() {
        let api = write_test_openapi();
        let index = ReadOnlyApiIndex::from_openapi_write_allowlist(&api, &[]);
        assert_eq!(index.len(), 0);
    }

    #[test]
    fn get_index_unchanged_by_write_builder() {
        // The original GET builders must still work exactly as before —
        // write ops are NOT included when using from_openapi or from_openapi_allowlist.
        let api = write_test_openapi();

        let read_index = ReadOnlyApiIndex::from_openapi(&api, &[]);
        assert_eq!(read_index.len(), 1, "only list_projects GET");
        assert!(read_index.get("list_projects").is_some());
        assert!(read_index.get("create_project").is_none());
        assert!(read_index.get("update_project").is_none());
        assert!(read_index.get("delete_project").is_none());

        let allowlist_index = ReadOnlyApiIndex::from_openapi_allowlist(&api, &["list_projects"]);
        assert_eq!(allowlist_index.len(), 1);
        assert!(allowlist_index.get("list_projects").is_some());
        assert!(allowlist_index.get("create_project").is_none());
    }

    /// A `$ref` body property's own description must survive into the param
    /// spec.
    ///
    /// Found against a live model: `create_alert`'s `detection_config` is a
    /// discriminated union, so it resolves to a bare "object" here. Its sibling
    /// description is the only place the required shape (the `kind`
    /// discriminator) is written down, and dropping it left the model to guess
    /// the JSON — it guessed `{"type": ...}` instead of `{"kind": ...}` and the
    /// confirmed write failed with a 422.
    #[test]
    fn ref_body_property_keeps_its_own_description() {
        use utoipa::openapi::{
            request_body::RequestBodyBuilder, schema::ComponentsBuilder, ContentBuilder,
        };

        const SHAPE: &str = "Discriminated union keyed by `kind`, e.g. \
                             `{ \"kind\": \"static\", \"threshold\": 500 }`.";

        let components = ComponentsBuilder::new()
            .schema(
                "Detector",
                RefOr::T(Schema::Object(
                    ObjectBuilder::new()
                        .schema_type(SchemaType::Type(Type::Object))
                        .build(),
                )),
            )
            .schema(
                "CreateAlertRequest",
                RefOr::T(Schema::Object(
                    ObjectBuilder::new()
                        .schema_type(SchemaType::Type(Type::Object))
                        .property(
                            "detection_config",
                            RefOr::Ref(
                                utoipa::openapi::schema::Ref::builder()
                                    .ref_location_from_schema_name("Detector")
                                    .description(Some(SHAPE))
                                    .build(),
                            ),
                        )
                        .required("detection_config")
                        .build(),
                )),
            )
            .build();

        let create = OperationBuilder::new()
            .operation_id(Some("create_alert"))
            .tag("Alerts")
            .request_body(Some(
                RequestBodyBuilder::new()
                    .content(
                        "application/json",
                        ContentBuilder::new()
                            .schema(Some(RefOr::Ref(
                                utoipa::openapi::schema::Ref::from_schema_name(
                                    "CreateAlertRequest",
                                ),
                            )))
                            .build(),
                    )
                    .build(),
            ))
            .build();

        let paths = PathsBuilder::new()
            .path("/otel/alerts", {
                let mut item = PathItem::default();
                item.post = Some(create);
                item
            })
            .build();

        let api = OpenApiBuilder::new()
            .info(utoipa::openapi::Info::new("Test API", "1.0.0"))
            .paths(paths)
            .components(Some(components))
            .build();

        let index = ReadOnlyApiIndex::from_openapi_write_allowlist(&api, &["create_alert"]);
        let op = index.get("create_alert").expect("indexed");
        let param = op
            .params
            .iter()
            .find(|p| p.name == "detection_config")
            .expect("body param resolved");

        assert_eq!(
            param.description.as_deref(),
            Some(SHAPE),
            "the shape guidance must reach the model — it is the only description of the JSON"
        );
    }

    /// The resolver must recognise utoipa's internally-tagged-enum encoding —
    /// `oneOf` of `allOf`s, each pairing a payload `$ref` with an inline object
    /// pinning the tag. This is exactly how `DetectionConfig` is emitted, and
    /// getting it wrong is what left the model guessing.
    #[test]
    fn tagged_union_body_param_resolves_to_its_variants() {
        use utoipa::openapi::{
            request_body::RequestBodyBuilder,
            schema::{AllOfBuilder, ComponentsBuilder, OneOfBuilder},
            ContentBuilder,
        };

        fn tag(name: &str, value: &str) -> RefOr<Schema> {
            RefOr::T(Schema::Object(
                ObjectBuilder::new()
                    .schema_type(SchemaType::Type(Type::Object))
                    .property(
                        name,
                        RefOr::T(Schema::Object(
                            ObjectBuilder::new()
                                .schema_type(SchemaType::Type(Type::String))
                                .enum_values(Some([value]))
                                .build(),
                        )),
                    )
                    .required(name)
                    .build(),
            ))
        }

        let static_params = Schema::Object(
            ObjectBuilder::new()
                .schema_type(SchemaType::Type(Type::Object))
                .property(
                    "comparator",
                    RefOr::T(Schema::Object(ObjectBuilder::new().build())),
                )
                .property(
                    "threshold",
                    RefOr::T(Schema::Object(ObjectBuilder::new().build())),
                )
                .required("comparator")
                .required("threshold")
                .build(),
        );
        let anomaly_params = Schema::Object(
            ObjectBuilder::new()
                .schema_type(SchemaType::Type(Type::Object))
                .property(
                    "deviations",
                    RefOr::T(Schema::Object(ObjectBuilder::new().build())),
                )
                .required("deviations")
                .build(),
        );

        let detection_config = Schema::OneOf(
            OneOfBuilder::new()
                .item(RefOr::T(Schema::AllOf(
                    AllOfBuilder::new()
                        .item(RefOr::Ref(utoipa::openapi::schema::Ref::from_schema_name(
                            "StaticParams",
                        )))
                        .item(tag("kind", "static"))
                        .build(),
                )))
                .item(RefOr::T(Schema::AllOf(
                    AllOfBuilder::new()
                        .item(RefOr::Ref(utoipa::openapi::schema::Ref::from_schema_name(
                            "AnomalyParams",
                        )))
                        .item(tag("kind", "anomaly"))
                        .build(),
                )))
                .build(),
        );

        let components = ComponentsBuilder::new()
            .schema("StaticParams", RefOr::T(static_params))
            .schema("AnomalyParams", RefOr::T(anomaly_params))
            .schema("DetectionConfig", RefOr::T(detection_config))
            .schema(
                "CreateAlertRequest",
                RefOr::T(Schema::Object(
                    ObjectBuilder::new()
                        .schema_type(SchemaType::Type(Type::Object))
                        .property(
                            "detection_config",
                            RefOr::Ref(utoipa::openapi::schema::Ref::from_schema_name(
                                "DetectionConfig",
                            )),
                        )
                        .required("detection_config")
                        .build(),
                )),
            )
            .build();

        let create = OperationBuilder::new()
            .operation_id(Some("create_alert"))
            .tag("Alerts")
            .request_body(Some(
                RequestBodyBuilder::new()
                    .content(
                        "application/json",
                        ContentBuilder::new()
                            .schema(Some(RefOr::Ref(
                                utoipa::openapi::schema::Ref::from_schema_name(
                                    "CreateAlertRequest",
                                ),
                            )))
                            .build(),
                    )
                    .build(),
            ))
            .build();

        let paths = PathsBuilder::new()
            .path("/otel/alerts", {
                let mut item = PathItem::default();
                item.post = Some(create);
                item
            })
            .build();

        let api = OpenApiBuilder::new()
            .info(utoipa::openapi::Info::new("Test API", "1.0.0"))
            .paths(paths)
            .components(Some(components))
            .build();

        let index = ReadOnlyApiIndex::from_openapi_write_allowlist(&api, &["create_alert"]);
        let param = index
            .get("create_alert")
            .expect("indexed")
            .params
            .iter()
            .find(|p| p.name == "detection_config")
            .expect("body param")
            .clone();

        let shape = param.object_shape.expect("union shape resolved");
        assert_eq!(shape.discriminator.as_deref(), Some("kind"));

        let static_keys = shape.variants.get("static").expect("static variant");
        for key in ["kind", "comparator", "threshold"] {
            assert!(
                static_keys.iter().any(|f| f.name == key),
                "missing {key} in {static_keys:?}"
            );
        }
        let anomaly_keys = shape.variants.get("anomaly").expect("anomaly variant");
        assert!(anomaly_keys.iter().any(|f| f.name == "deviations"));

        // And the rendered form names the discriminator, which is the detail
        // the model needs and cannot otherwise discover.
        assert!(
            shape.describe().contains("\"kind\": \"static\""),
            "{}",
            shape.describe()
        );
    }

    /// `Option<T>` must report `T`, not "array".
    ///
    /// utoipa encodes a nullable field as `type: ["string", "null"]`, which is
    /// a type UNION. Mapping that to "array" told the model every optional
    /// field wanted a list — `--start_time <array>` for an RFC 3339 string.
    #[test]
    fn nullable_type_union_reports_the_concrete_type() {
        use utoipa::openapi::schema::{SchemaType, Type};

        let nullable_string = SchemaType::Array(vec![Type::String, Type::Null]);
        assert_eq!(schema_type_to_string(&nullable_string), "string");

        let nullable_number = SchemaType::Array(vec![Type::Null, Type::Number]);
        assert_eq!(schema_type_to_string(&nullable_number), "number");

        // A real array is still an array.
        assert_eq!(
            schema_type_to_string(&SchemaType::Type(Type::Array)),
            "array"
        );

        // Genuinely polymorphic, or null-only: neither is actionable, so don't
        // claim a type the caller could act on.
        assert_eq!(
            schema_type_to_string(&SchemaType::Array(vec![Type::String, Type::Number])),
            "any"
        );
        assert_eq!(
            schema_type_to_string(&SchemaType::Array(vec![Type::Null])),
            "null"
        );
    }

    #[test]
    fn safe_posts_are_indexed_with_post_method_and_body_params() {
        let api = write_test_openapi();
        let index = ReadOnlyApiIndex::from_openapi_allowlist_with_safe_posts(
            &api,
            &["list_projects"],
            &["create_project"],
        );

        assert_eq!(index.len(), 2, "the GET plus the vetted read-only POST");

        let post = index.get("create_project").expect("safe POST indexed");
        assert_eq!(post.method, "POST", "must dispatch as a real POST");
        let name = post
            .params
            .iter()
            .find(|p| p.name == "name")
            .expect("body params resolved through the $ref");
        assert_eq!(name.location, ParamLocation::Body);
        assert!(name.required);
    }

    /// The two allowlists are disjoint: naming an operation in one does not
    /// grant it through the other. This is the property that keeps "vetted
    /// read-only POST" from quietly widening into "any POST".
    #[test]
    fn safe_post_and_get_allowlists_do_not_leak_into_each_other() {
        let api = write_test_openapi();

        // A POST named only in the GET allowlist stays out.
        let index = ReadOnlyApiIndex::from_openapi_allowlist_with_safe_posts(
            &api,
            &["create_project"],
            &[],
        );
        assert!(
            index.get("create_project").is_none(),
            "a POST must never be reachable via the GET allowlist"
        );

        // A GET named only in the safe-POST list stays out.
        let index =
            ReadOnlyApiIndex::from_openapi_allowlist_with_safe_posts(&api, &[], &["list_projects"]);
        assert!(index.get("list_projects").is_none());
        assert_eq!(index.len(), 0);
    }

    /// PATCH/PUT/DELETE can never enter the read index, even if someone
    /// mistakenly adds one to `safe_posts` — only `path_item.post` is consulted.
    #[test]
    fn safe_posts_never_admit_non_post_write_methods() {
        let api = write_test_openapi();
        let index = ReadOnlyApiIndex::from_openapi_allowlist_with_safe_posts(
            &api,
            &[],
            &["update_project", "delete_project"],
        );
        assert_eq!(
            index.len(),
            0,
            "PATCH/DELETE must be structurally unreachable from the read index"
        );
    }
}
