// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Plugin declarations are untrusted metadata, never authorization.

use super::*;

/// Access is derived from HTTP semantics; plugins can only escalate to write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginToolAccess {
    Read,
    Write,
}

/// Discoverable even when an administrator has not enabled execution.
#[derive(Debug, Clone, Serialize)]
pub struct PluginTool {
    pub plugin: String,
    pub operation: ApiOperation,
    pub access: PluginToolAccess,
    pub enabled: bool,
    pub setup_path: String,
}

/// Separate snapshot of plugin operations; curated core indexes are untouched.
#[derive(Debug, Clone, Default)]
pub struct PluginToolsIndex {
    pub(crate) revision: uuid::Uuid,
    tools: Vec<PluginTool>,
    secrets: HashMap<String, Vec<SecretPath>>,
    routes: HashMap<String, HashMap<String, matchit::Router<String>>>,
    folded_routes: HashMap<String, HashMap<String, matchit::Router<String>>>,
}

#[derive(Debug, thiserror::Error)]
pub enum PluginToolsError {
    #[error(
        "Cannot index plugin query parameter '{parameter}' in operation '{operation_id}': {reason}"
    )]
    QuerySerialization {
        operation_id: String,
        parameter: String,
        reason: String,
    },
    #[error("Cannot index AI tools for plugin '{plugin}': name overlaps the host plugin management namespace")]
    ReservedPluginName { plugin: String },
    #[error("Cannot index AI operation '{operation_id}' for plugin '{plugin}': parameter '{parameter}' occurs in multiple locations")]
    AmbiguousParameter {
        plugin: String,
        operation_id: String,
        parameter: String,
    },
    #[error("Cannot index AI tools for plugin '{plugin}': invalid plugin name")]
    InvalidPluginName { plugin: String },
    #[error(
        "Cannot index AI operation '{operation_id}' for plugin '{plugin}': duplicate operation ID"
    )]
    DuplicateOperation {
        plugin: String,
        operation_id: String,
    },
    #[error("Cannot index AI operation '{operation_id}' for plugin '{plugin}': invalid operation ID or path '{path}'")]
    InvalidOperation {
        plugin: String,
        operation_id: String,
        path: String,
    },
    #[error("Cannot index AI operation '{operation_id}' for plugin '{plugin}': request body must declare an application/json object schema with named properties, inline or through a component reference; root maps and catch-all properties are unsupported")]
    UnsupportedRequestBody {
        plugin: String,
        operation_id: String,
    },
    #[error("Cannot index route '{path}' for plugin '{plugin}': {source}")]
    InvalidRoute {
        plugin: String,
        path: String,
        source: matchit::InsertError,
    },
}

impl PluginToolsIndex {
    /// Remove one stopped plugin while retaining unrelated healthy declarations.
    pub fn without_plugin(&self, plugin: &str) -> Self {
        let mut index = self.clone();
        index.tools.retain(|tool| tool.plugin != plugin);
        index.routes.remove(plugin);
        index.folded_routes.remove(plugin);
        let prefix = format!("plugin.{plugin}.");
        index
            .secrets
            .retain(|operation, _| !operation.starts_with(&prefix));
        index
    }

    pub fn extend(&mut self, other: Self) {
        self.tools.extend(other.tools);
        self.secrets.extend(other.secrets);
        self.routes.extend(other.routes);
        self.folded_routes.extend(other.folded_routes);
    }

    pub(crate) fn append_to(&self, index: &mut ReadOnlyApiIndex, access: PluginToolAccess) {
        for tool in &self.tools {
            if tool.access != access && (tool.enabled || access == PluginToolAccess::Write) {
                continue;
            }
            let mut operation = tool.operation.clone();
            operation.plugin_enabled = Some(tool.enabled);
            if !tool.enabled {
                operation.summary = Some(format!(
                    "[disabled; enable at {}] {}",
                    tool.setup_path,
                    operation.summary.as_deref().unwrap_or("")
                ));
            }
            index.operations.push(operation);
        }
        index.secret_paths.extend(self.secrets.clone());
    }

    /// Build from complete plugin-local documents, before the host prefixes paths.
    /// Invalid declarations fail closed; callers should publish an empty snapshot
    /// for that plugin on failure, never retain a formerly enabled version.
    pub fn from_openapi(
        plugin: &str,
        openapi: &utoipa::openapi::OpenApi,
        enabled: bool,
    ) -> Result<Self, PluginToolsError> {
        if plugin.eq_ignore_ascii_case("plugins") {
            return Err(PluginToolsError::ReservedPluginName {
                plugin: plugin.into(),
            });
        }
        if !valid_identifier(plugin) {
            return Err(PluginToolsError::InvalidPluginName {
                plugin: plugin.into(),
            });
        }
        let mut tools = Vec::new();
        let mut secrets = HashMap::new();
        let mut routes: HashMap<String, matchit::Router<String>> = HashMap::new();
        let mut folded_routes: HashMap<String, matchit::Router<String>> = HashMap::new();
        let schema_components = components_json(openapi);
        let mut ids = std::collections::HashSet::new();
        for (path, item) in &openapi.paths.paths {
            for (method, operation) in [
                ("GET", &item.get),
                ("POST", &item.post),
                ("PUT", &item.put),
                ("PATCH", &item.patch),
                ("DELETE", &item.delete),
                ("HEAD", &item.head),
                ("OPTIONS", &item.options),
                ("TRACE", &item.trace),
            ] {
                let Some(op) = operation else { continue };
                // Compare endpoints for the requested method, including
                // unmarked operations and GETs explicitly elevated to write.
                // A POST-only static route cannot override a GET template.
                let full_path = format!("/x/{plugin}{path}");
                routes
                    .entry(method.into())
                    .or_default()
                    .insert(full_path.clone(), full_path.clone())
                    .map_err(|source| PluginToolsError::InvalidRoute {
                        plugin: plugin.into(),
                        path: path.clone(),
                        source,
                    })?;
                // Conservatively protect frameworks with case-insensitive routing.
                folded_routes
                    .entry(method.into())
                    .or_default()
                    .insert(full_path.to_lowercase(), full_path)
                    .map_err(|source| PluginToolsError::InvalidRoute {
                        plugin: plugin.into(),
                        path: path.clone(),
                        source,
                    })?;
                let Some(mark) = op
                    .extensions
                    .as_ref()
                    .and_then(|e| e.get("x-temps-ai-tool"))
                else {
                    continue;
                };
                if !mark.is_object() {
                    continue;
                }
                let Some(id) = op.operation_id.as_deref() else {
                    continue;
                };
                if !valid_identifier(id) || !valid_path(path) {
                    return Err(PluginToolsError::InvalidOperation {
                        plugin: plugin.into(),
                        operation_id: id.into(),
                        path: path.clone(),
                    });
                }
                if !ids.insert(id) {
                    return Err(PluginToolsError::DuplicateOperation {
                        plugin: plugin.into(),
                        operation_id: id.into(),
                    });
                }
                validate_body_schema(plugin, id, op, openapi)?;
                let access = if method == "GET"
                    && mark
                        .get("access")
                        .is_none_or(|v| v.as_str() == Some("read"))
                {
                    PluginToolAccess::Read
                } else {
                    PluginToolAccess::Write
                };
                let description: String = mark
                    .get("description")
                    .and_then(|v| v.as_str())
                    .filter(|v| !v.trim().is_empty())
                    .or(op.summary.as_deref())
                    .unwrap_or(id)
                    .chars()
                    .take(500)
                    .collect();
                let mut effective = op.clone();
                let parameters = effective.parameters.get_or_insert_with(Vec::new);
                let components = openapi.components.as_ref().map(|c| &c.parameters);
                for inherited in item.parameters.iter().flatten() {
                    // Use the same resolution rules as schema construction. OpenAPI
                    // overrides are identified by (name, in), not reference spelling.
                    let Some(shared) =
                        resolve_component(inherited, components, "#/components/parameters/")
                    else {
                        continue;
                    };
                    let overridden = parameters.iter().any(|parameter| {
                        resolve_component(parameter, components, "#/components/parameters/")
                            .is_some_and(|local| {
                                local.name == shared.name
                                    && local.parameter_in == shared.parameter_in
                            })
                    });
                    if !overridden {
                        parameters.push(inherited.clone());
                    }
                }
                let mut operation = build_api_operation(
                    format!("plugin.{plugin}.{id}"),
                    format!("/x/{plugin}{path}"),
                    method,
                    &effective,
                    &resolve_body_params(&effective, openapi),
                    openapi,
                );
                let mut names = std::collections::HashSet::new();
                if let Some(parameter) = operation
                    .params
                    .iter()
                    .find(|p| !names.insert(p.name.as_str()))
                {
                    return Err(PluginToolsError::AmbiguousParameter {
                        plugin: plugin.into(),
                        operation_id: id.into(),
                        parameter: parameter.name.clone(),
                    });
                }
                preserve_query_input_types(&mut operation, &effective, openapi)?;
                operation.has_json_body = effective
                    .request_body
                    .as_ref()
                    .and_then(|body| {
                        resolve_component(
                            body,
                            openapi
                                .components
                                .as_ref()
                                .map(|components| &components.request_bodies),
                            "#/components/requestBodies/",
                        )
                    })
                    .is_some_and(|body| body.content.contains_key("application/json"));
                preserve_composite_body_inputs(&mut operation, &effective, openapi);
                operation.summary = Some(description.clone());
                operation.description = Some(description);
                // Do not let plugin-supplied tags impersonate core permission groups.
                operation.tags = vec![format!("plugin.{plugin}")];
                secrets.insert(
                    operation.operation_id.clone(),
                    secret_paths_of(op, &schema_components),
                );
                tools.push(PluginTool {
                    plugin: plugin.into(),
                    operation,
                    access,
                    enabled,
                    setup_path: format!("/plugins/{plugin}"),
                });
            }
        }
        Ok(Self {
            tools,
            secrets,
            routes: HashMap::from([(plugin.to_string(), routes)]),
            folded_routes: HashMap::from([(plugin.to_string(), folded_routes)]),
            revision: uuid::Uuid::nil(),
        })
    }

    pub fn tools(&self) -> &[PluginTool] {
        &self.tools
    }

    /// Execution lookup is deliberately separate from discovery.
    pub fn callable(&self, id: &str, access: PluginToolAccess) -> Option<&ApiOperation> {
        self.tools
            .iter()
            .find(|t| t.enabled && t.access == access && t.operation.operation_id == id)
            .map(|t| &t.operation)
    }

    pub(crate) fn resolves_to(&self, operation: &ApiOperation, path: &str) -> bool {
        let Some(plugin) = operation
            .operation_id
            .strip_prefix("plugin.")
            .and_then(|id| id.split_once('.').map(|pair| pair.0))
        else {
            return false;
        };
        let Some(router) = self
            .routes
            .get(plugin)
            .and_then(|routes| routes.get(&operation.method))
        else {
            return false;
        };
        let Some(folded) = self
            .folded_routes
            .get(plugin)
            .and_then(|routes| routes.get(&operation.method))
        else {
            return false;
        };
        let matches = |path: &str| {
            router
                .at(path)
                .is_ok_and(|route| route.value == &operation.path)
                && folded
                    .at(&path.to_lowercase())
                    .is_ok_and(|route| route.value == &operation.path)
        };
        // Check both URL representations, so decoding in a plugin framework
        // cannot change a template request into another declared endpoint.
        matches(path) && urlencoding::decode(path).is_ok_and(|decoded| matches(&decoded))
    }
}

fn validate_body_schema(
    plugin: &str,
    operation_id: &str,
    operation: &Operation,
    openapi: &utoipa::openapi::OpenApi,
) -> Result<(), PluginToolsError> {
    use utoipa::openapi::schema::{AdditionalProperties, SchemaType, Type};
    let Some(body) = operation.request_body.as_ref() else {
        return Ok(());
    };
    let schema = resolve_component(
        body,
        openapi.components.as_ref().map(|c| &c.request_bodies),
        "#/components/requestBodies/",
    )
    .and_then(|body| body.content.get("application/json"))
    .and_then(|content| match content {
        RefOr::T(content) => content.schema.as_ref(),
        RefOr::Ref(_) => None,
    })
    .and_then(|schema| deref_schema(schema, openapi));
    if matches!(schema, Some(Schema::Object(object))
        if matches!(object.schema_type, SchemaType::Type(Type::Object))
            && object.additional_properties.as_deref().is_none_or(|additional|
                matches!(additional, AdditionalProperties::FreeForm(false)))
            && object.min_properties.unwrap_or(0) <= object.properties.len())
    {
        return Ok(());
    }
    Err(PluginToolsError::UnsupportedRequestBody {
        plugin: plugin.into(),
        operation_id: operation_id.into(),
    })
}

fn preserve_query_input_types(
    operation: &mut ApiOperation,
    declaration: &Operation,
    openapi: &utoipa::openapi::OpenApi,
) -> Result<(), PluginToolsError> {
    for parameter in declaration.parameters.iter().flatten() {
        let Some(parameter) = resolve_component(
            parameter,
            openapi.components.as_ref().map(|c| &c.parameters),
            "#/components/parameters/",
        ) else {
            continue;
        };
        if parameter.parameter_in != ParameterIn::Query {
            continue;
        }
        use crate::QuerySerialization;
        use utoipa::openapi::path::ParameterStyle;
        let invalid = |reason: &str| PluginToolsError::QuerySerialization {
            operation_id: operation.operation_id.clone(),
            parameter: parameter.name.clone(),
            reason: reason.into(),
        };
        let json = !parameter.content.is_empty();
        let schema_ref = if json {
            if parameter.content.len() != 1
                || parameter.schema.is_some()
                || parameter.style.is_some()
                || parameter.explode.is_some()
            {
                return Err(invalid(
                    "JSON query content cannot be combined with schema/style/explode",
                ));
            }
            let Some(RefOr::T(content)) = parameter.content.get("application/json") else {
                return Err(invalid(
                    "Only inline application/json query content is supported",
                ));
            };
            content.schema.as_ref()
        } else {
            parameter.schema.as_ref()
        };
        let schema = schema_ref
            .and_then(|schema| deref_schema(schema, openapi))
            .ok_or_else(|| invalid("Query schema is missing or unresolved"))?;
        if parameter.allow_reserved == Some(true) {
            return Err(invalid("allowReserved query values are unsupported"));
        }
        let encoding = if json {
            QuerySerialization::Json
        } else {
            match parameter.style.as_ref().unwrap_or(&ParameterStyle::Form) {
                ParameterStyle::Form => QuerySerialization::Form {
                    explode: parameter.explode.unwrap_or(true),
                },
                ParameterStyle::DeepObject if parameter.explode != Some(false) => {
                    QuerySerialization::DeepObject
                }
                ParameterStyle::SpaceDelimited if parameter.explode != Some(true) => {
                    QuerySerialization::SpaceDelimited
                }
                ParameterStyle::PipeDelimited if parameter.explode != Some(true) => {
                    QuerySerialization::PipeDelimited
                }
                _ => return Err(invalid("Unsupported query style/explode combination")),
            }
        };
        let (ty, _) = extract_schema_type_and_enum(schema);
        if !json {
            let compatible = match &encoding {
                QuerySerialization::DeepObject => ty == "object",
                QuerySerialization::SpaceDelimited | QuerySerialization::PipeDelimited => {
                    ty == "array"
                }
                _ => ty != "object",
            };
            if !compatible || needs_json_input(schema, openapi, 0) {
                return Err(invalid("Use application/json content for composite values; objects require deepObject or JSON content"));
            }
        }
        if let Some(param) = operation.params.iter_mut().find(|param| {
            param.name == parameter.name && matches!(param.location, ParamLocation::Query)
        }) {
            let (ty, enum_values) = extract_schema_type_and_enum(schema);
            param.ty = ty;
            param.query_serialization = Some(encoding);
            param.enum_values = enum_values;
            if needs_json_input(schema, openapi, 0) {
                param.ty = "json".into();
                param.item_ty = None;
                param.enum_values.clear();
            } else if let Some(item_ty) = array_item_type(schema, openapi) {
                param.ty = "array".into();
                param.item_ty = Some(item_ty);
            }
        }
    }
    Ok(())
}

/// Composite plugin inputs require an explicit JSON value, not scalar guessing.
/// The endpoint retains validation of alternatives and their constraints.
fn preserve_composite_body_inputs(
    operation: &mut ApiOperation,
    declaration: &Operation,
    openapi: &utoipa::openapi::OpenApi,
) {
    let Some(body) = declaration.request_body.as_ref().and_then(|body| {
        resolve_component(
            body,
            openapi.components.as_ref().map(|c| &c.request_bodies),
            "#/components/requestBodies/",
        )
    }) else {
        return;
    };
    let Some(RefOr::T(content)) = body.content.get("application/json") else {
        return;
    };
    let Some(Schema::Object(schema)) = content
        .schema
        .as_ref()
        .and_then(|s| deref_schema(s, openapi))
    else {
        return;
    };
    for param in &mut operation.params {
        if matches!(param.location, ParamLocation::Body)
            && schema
                .properties
                .get(&param.name)
                .and_then(|s| deref_schema(s, openapi))
                .is_some_and(|s| needs_json_input(s, openapi, 0))
        {
            param.ty = "json".into();
            param.item_ty = None;
            param.enum_values.clear();
            param.object_shape = None;
        }
    }
}

fn needs_json_input(schema: &Schema, openapi: &utoipa::openapi::OpenApi, depth: usize) -> bool {
    use utoipa::openapi::schema::{ArrayItems, SchemaType, Type};
    if depth >= 16 {
        return true;
    }
    match schema {
        Schema::AnyOf(_) | Schema::OneOf(_) | Schema::AllOf(_) => true,
        Schema::Object(object) => {
            matches!(
                object.schema_type,
                SchemaType::Array(_) | SchemaType::Type(Type::Null)
            ) || object.properties.values().any(|property| {
                deref_schema(property, openapi)
                    .is_some_and(|s| needs_json_input(s, openapi, depth + 1))
            })
        }
        Schema::Array(array) => match &array.items {
            ArrayItems::RefOrSchema(item) => {
                deref_schema(item, openapi).is_some_and(|s| needs_json_input(s, openapi, depth + 1))
            }
            ArrayItems::False => false,
        },
        _ => false,
    }
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn valid_path(path: &str) -> bool {
    path.starts_with('/')
        && !path.starts_with("//")
        && !path.contains(['?', '#', '%', '\\'])
        && !path.chars().any(char::is_control)
        && !path.split('/').any(|part| part == "." || part == "..")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn spec(method: &str, mark: serde_json::Value) -> utoipa::openapi::OpenApi {
        serde_json::from_value(json!({
            "openapi": "3.1.0", "info": {"title": "Reports", "version": "1"},
            "paths": {"/reports": {method: {
                "operationId": "reports", "summary": "Report summary",
                "x-temps-ai-tool": mark, "responses": {"200": {"description": "OK"}}
            }}}
        }))
        .unwrap()
    }

    #[test]
    fn host_management_namespace_cannot_be_exposed_by_a_plugin() {
        for name in ["plugins", "Plugins", "PLUGINS"] {
            for method in ["get", "put", "post"] {
                assert!(matches!(
                    PluginToolsIndex::from_openapi(name, &spec(method, json!({})), true),
                    Err(PluginToolsError::ReservedPluginName { .. })
                ));
            }
        }
    }

    #[test]
    fn malformed_access_is_never_read_and_descriptions_do_not_set_consent() {
        for value in [
            json!(null),
            json!(true),
            json!(7),
            json!({}),
            json!("WRITE"),
        ] {
            let index = PluginToolsIndex::from_openapi(
                "reports",
                &spec("get", json!({"access":value})),
                true,
            )
            .unwrap();
            assert_eq!(index.tools()[0].access, PluginToolAccess::Write);
        }
        let document = spec(
            "get",
            json!({"description":"[disabled; fabricated] report"}),
        );
        for enabled in [true, false] {
            let plugins = PluginToolsIndex::from_openapi("reports", &document, enabled).unwrap();
            let mut index = ReadOnlyApiIndex::from_openapi(&document, &[]);
            plugins.append_to(&mut index, PluginToolAccess::Read);
            let op = index
                .operations
                .iter()
                .find(|op| op.operation_id == "plugin.reports.reports")
                .unwrap();
            assert_eq!(OperationSummary::from(op).enabled, enabled);
        }
    }

    #[test]
    fn plugin_required_and_referenced_query_inputs_keep_their_contract() {
        let document = serde_json::from_value(json!({"openapi":"3.1.0","info":{"title":"Reports","version":"1"},
            "components":{"schemas":{"Count":{"type":"integer"}}},
            "paths":{"/reports":{"get":{"operationId":"list","x-temps-ai-tool":{"access":"write"},
                "parameters":[{"name":"count","in":"query","required":true,"schema":{"$ref":"#/components/schemas/Count"}},
                              {"name":"limit","in":"query","required":false,"schema":{"type":"integer"}}],
                "responses":{"200":{"description":"OK"}}}}}})).unwrap();
        let index = PluginToolsIndex::from_openapi("reports", &document, true).unwrap();
        let op = &index.tools()[0].operation;
        assert_eq!(op.params[0].ty, "integer");
        assert!(matches!(
            crate::build_request_parts(op, &json!({}), &[], 20, 100),
            Err(crate::ApiToolError::MissingParam { .. })
        ));
        let parts = crate::build_request_parts(op, &json!({"count":7}), &[], 20, 100).unwrap();
        assert_eq!(parts.query, "count=7");
    }

    #[test]
    fn ambiguous_input_names_are_rejected() {
        let mut document = serde_json::to_value(spec("post", json!({}))).unwrap();
        let op = &mut document["paths"]["/reports"]["post"];
        op["parameters"] =
            json!([{"name":"name","in":"query","required":true,"schema":{"type":"string"}}]);
        op["requestBody"] = json!({"content":{"application/json":{"schema":{"type":"object","properties":{"name":{"type":"string"}}}}}});
        assert!(matches!(
            PluginToolsIndex::from_openapi(
                "reports",
                &serde_json::from_value(document).unwrap(),
                true
            ),
            Err(PluginToolsError::AmbiguousParameter { .. })
        ));
    }

    #[test]
    fn case_insensitive_static_routes_cannot_be_selected_by_read_templates() {
        let document = serde_json::from_value(json!({"openapi":"3.1.0","info":{"title":"Reports","version":"1"},"paths":{
            "/reports/{id}":{"get":{"operationId":"read","x-temps-ai-tool":{},"responses":{"200":{"description":"OK"}}}},
            "/reports/reset":{"get":{"operationId":"reset","x-temps-ai-tool":{"access":"write"},"responses":{"200":{"description":"OK"}}}}
        }})).unwrap();
        let index = PluginToolsIndex::from_openapi("reports", &document, true).unwrap();
        let op = index
            .callable("plugin.reports.read", PluginToolAccess::Read)
            .unwrap();
        assert!(index.resolves_to(op, "/x/reports/reports/normal"));
        for path in [
            "/x/reports/reports/RESET",
            "/x/reports/reports/Reset",
            "/x/reports/reports/%52ESET",
        ] {
            assert!(!index.resolves_to(op, path));
        }
    }

    #[test]
    fn method_is_authoritative_and_get_can_escalate() {
        for method in [
            "get", "post", "put", "patch", "delete", "head", "options", "trace",
        ] {
            let index = PluginToolsIndex::from_openapi(
                "reports",
                &spec(method, json!({"access":"read"})),
                true,
            )
            .unwrap();
            let access = if method == "get" {
                PluginToolAccess::Read
            } else {
                PluginToolAccess::Write
            };
            assert_eq!(index.tools()[0].access, access);
            assert!(index.callable("plugin.reports.reports", access).is_some());
            assert_eq!(index.tools()[0].operation.path, "/x/reports/reports");
        }
        let index = PluginToolsIndex::from_openapi(
            "reports",
            &spec("get", json!({"access":"write"})),
            true,
        )
        .unwrap();
        assert!(index
            .callable("plugin.reports.reports", PluginToolAccess::Read)
            .is_none());
        assert_eq!(index.tools()[0].access, PluginToolAccess::Write);
    }

    #[test]
    fn disabled_tools_remain_discoverable_but_never_callable() {
        let index =
            PluginToolsIndex::from_openapi("reports", &spec("get", json!({})), false).unwrap();
        assert_eq!(index.tools().len(), 1);
        assert!(!index.tools()[0].enabled);
        assert!(index
            .callable("plugin.reports.reports", PluginToolAccess::Read)
            .is_none());
        assert_eq!(
            index.tools()[0].operation.summary.as_deref(),
            Some("Report summary")
        );
    }

    #[test]
    fn description_cap_counts_unicode_characters() {
        let index = PluginToolsIndex::from_openapi(
            "reports",
            &spec("get", json!({"description":"界".repeat(501)})),
            true,
        )
        .unwrap();
        assert_eq!(
            index.tools()[0]
                .operation
                .description
                .as_ref()
                .unwrap()
                .chars()
                .count(),
            500
        );
    }

    #[test]
    fn unmarked_or_malformed_markers_are_not_tools() {
        for mark in [json!(null), json!(true), json!("read")] {
            assert!(
                PluginToolsIndex::from_openapi("reports", &spec("get", mark), true)
                    .unwrap()
                    .tools()
                    .is_empty()
            );
        }
        let mut api = spec("get", json!({}));
        api.paths
            .paths
            .get_mut("/reports")
            .unwrap()
            .get
            .as_mut()
            .unwrap()
            .extensions = None;
        assert!(PluginToolsIndex::from_openapi("reports", &api, true)
            .unwrap()
            .tools()
            .is_empty());
    }

    #[test]
    fn inherited_parameters_are_preserved_and_operation_parameters_override() {
        let api = serde_json::from_value(json!({
            "openapi": "3.1.0", "info": {"title":"Reports", "version":"1"},
            "paths": {"/reports/{report_id}": {
                "parameters": [
                    {"name":"report_id", "in":"path", "required":true, "schema":{"type":"integer"}},
                    {"name":"limit", "in":"query", "required":false, "schema":{"type":"string"}}
                ],
                "get": {"operationId":"report", "x-temps-ai-tool":{},
                    "parameters":[{"name":"limit", "in":"query", "required":false, "schema":{"type":"integer"}}],
                    "responses":{"200":{"description":"OK"}}}
            }}
        }))
        .unwrap();
        let index = PluginToolsIndex::from_openapi("reports", &api, true).unwrap();
        let op = &index.tools()[0].operation;
        assert_eq!(op.params.len(), 2);
        assert_eq!(
            op.params.iter().find(|p| p.name == "limit").unwrap().ty,
            "integer"
        );
        let built = crate::build_request_parts(op, &json!({"report_id":7}), &[], 20, 100).unwrap();
        assert_eq!(built.path, "/x/reports/reports/7");
    }

    #[test]
    fn referenced_parameters_override_by_resolved_name_and_location() {
        let shared = json!({"name":"mode", "in":"query", "required":false,
            "schema":{"type":"string", "enum":["summary"]}});
        let local = json!({"name":"mode", "in":"query", "required":false,
            "schema":{"type":"string", "enum":["detail"]}});
        for shared_ref in [false, true] {
            for local_ref in [false, true] {
                let inherited = if shared_ref {
                    json!({"$ref":"#/components/parameters/SharedMode"})
                } else {
                    shared.clone()
                };
                let override_param = if local_ref {
                    json!({"$ref":"#/components/parameters/LocalMode"})
                } else {
                    local.clone()
                };
                let api = serde_json::from_value(json!({
                    "openapi":"3.1.0", "info":{"title":"Reports", "version":"1"},
                    "paths":{"/reports":{
                        "parameters":[inherited],
                        "get":{"operationId":"reports", "x-temps-ai-tool":{},
                            "parameters":[override_param], "responses":{"200":{"description":"OK"}}}
                    }},
                    "components":{"parameters":{"SharedMode":shared, "LocalMode":local}}
                }))
                .unwrap();
                let index = PluginToolsIndex::from_openapi("reports", &api, true).unwrap();
                let op = index
                    .callable("plugin.reports.reports", PluginToolAccess::Read)
                    .unwrap();
                assert_eq!(
                    op.params.len(),
                    1,
                    "shared_ref={shared_ref}, local_ref={local_ref}"
                );
                let request =
                    crate::build_request_parts(op, &json!({"mode":"detail"}), &[], 20, 100)
                        .unwrap();
                assert_eq!(request.query, "mode=detail");
                assert!(
                    crate::build_request_parts(op, &json!({"mode":"summary"}), &[], 20, 100)
                        .is_err()
                );
            }
        }
    }

    #[test]
    fn reject_ambiguous_ids_and_paths_that_escape_namespace() {
        let mut api = spec("get", json!({}));
        let item = api.paths.paths.get("/reports").unwrap().clone();
        api.paths.paths.insert("/duplicate".into(), item.clone());
        assert!(matches!(
            PluginToolsIndex::from_openapi("reports", &api, true),
            Err(PluginToolsError::DuplicateOperation { .. })
        ));
        for path in ["/../core", "/%2e%2e/core", "//core", "/reports?admin=true"] {
            api.paths.paths.clear();
            api.paths.paths.insert(path.into(), item.clone());
            assert!(matches!(
                PluginToolsIndex::from_openapi("reports", &api, true),
                Err(PluginToolsError::InvalidOperation { .. })
            ));
        }
        assert!(PluginToolsIndex::from_openapi("../core", &spec("get", json!({})), true).is_err());
    }
    #[test]
    fn plugin_project_path_id_preserves_scope_and_autofill() {
        let spec = serde_json::from_value(json!({"openapi":"3.1.0","info":{"title":"Reports","version":"1"},"paths":{"/projects/{id}/reports":{"parameters":[{"name":"id","in":"path","required":true,"schema":{"type":"integer"}}],"get":{"operationId":"list","x-temps-ai-tool":{},"responses":{"200":{"description":"ok"}}}}}})).unwrap();
        let index = PluginToolsIndex::from_openapi("reports", &spec, true).unwrap();
        let op = index
            .callable("plugin.reports.list", PluginToolAccess::Read)
            .unwrap();
        let scope = [42];
        let built = crate::caller::build_request_parts(op, &json!({}), &scope, 20, 100).unwrap();
        assert_eq!(built.path, "/x/reports/projects/42/reports");
        assert!(
            crate::caller::build_request_parts(op, &json!({"id":43}), &scope, 20, 100).is_err()
        );
    }
    #[test]
    fn plugin_path_values_cannot_escape_the_declared_read_endpoint() {
        let spec = serde_json::from_value(serde_json::json!({"openapi":"3.1.0","info":{"title":"reports","version":"1"},"paths":{"/reports/{id}/reset":{"get":{"operationId":"read","x-temps-ai-tool":{},"parameters":[{"name":"id","in":"path","required":true,"schema":{"type":"string"}}],"responses":{"200":{"description":"ok"}}}},"/reset":{"get":{"operationId":"reset","x-temps-ai-tool":{"access":"write"},"responses":{"200":{"description":"ok"}}}}}})).unwrap();
        let index = PluginToolsIndex::from_openapi("reports", &spec, true).unwrap();
        let op = index
            .callable("plugin.reports.read", PluginToolAccess::Read)
            .unwrap();
        for id in [".", "..", "", "../reset", "a/b", "a\\b", "%2e%2e", "%2f"] {
            assert!(
                matches!(
                    crate::build_request_parts(op, &serde_json::json!({"id":id}), &[], 20, 100),
                    Err(crate::ApiToolError::BadParamType { .. })
                ),
                "{id}"
            );
        }
        let request =
            crate::build_request_parts(op, &serde_json::json!({"id":"report-1"}), &[], 20, 100)
                .unwrap();
        assert_eq!(request.path, "/x/reports/reports/report-1/reset");
        assert!(index
            .callable("plugin.reports.reset", PluginToolAccess::Read)
            .is_none());
    }
    #[test]
    fn empty_json_request_bodies_are_preserved_including_component_references() {
        for body in [
            serde_json::json!({"required":true,"content":{"application/json":{"schema":{"type":"object","properties":{}}}}}),
            serde_json::json!({"$ref":"#/components/requestBodies/Empty"}),
        ] {
            let spec = serde_json::from_value(serde_json::json!({"openapi":"3.1.0","info":{"title":"Reports","version":"1"},"components":{"requestBodies":{"Empty":{"required":true,"content":{"application/json":{"schema":{"type":"object","properties":{}}}}}}},"paths":{"/create":{"post":{"operationId":"create","x-temps-ai-tool":{},"requestBody":body,"responses":{"200":{"description":"ok"}}}}}})).unwrap();
            let index = PluginToolsIndex::from_openapi("reports", &spec, true).unwrap();
            let op = index
                .callable("plugin.reports.create", PluginToolAccess::Write)
                .unwrap();
            assert!(op.params.is_empty());
            let request =
                crate::build_request_parts(op, &serde_json::json!({}), &[], 20, 100).unwrap();
            assert_eq!(request.body, Some(serde_json::json!({})));
        }
    }

    #[test]
    fn composite_plugin_body_inputs_keep_json_types_through_cli_and_request_building() {
        use crate::cli::{resolve, CliAction, CliMode};
        for (schema, values) in [
            (
                json!({"anyOf":[{"type":"number"},{"type":"null"}]}),
                vec![json!(7), json!(null)],
            ),
            (
                json!({"oneOf":[{"type":"number"},{"type":"boolean"}]}),
                vec![json!(7), json!(true)],
            ),
            (
                json!({"type":["string","null"]}),
                vec![json!("7"), json!(null)],
            ),
            (
                json!({"type":"array","items":{"anyOf":[{"type":"number"},{"type":"null"}]}}),
                vec![json!([7, null])],
            ),
            (
                json!({"$ref":"#/components/schemas/Count"}),
                vec![json!(7), json!(null)],
            ),
            (
                json!({"type":"object","required":["count"],"properties":{"count":{"anyOf":[{"type":"number"},{"type":"null"}]}}}),
                vec![json!({"count":7}), json!({"count":null})],
            ),
        ] {
            let spec = serde_json::from_value(json!({"openapi":"3.1.0","info":{"title":"Reports","version":"1"},"components":{"schemas":{"Count":{"anyOf":[{"type":"number"},{"type":"null"}]}}},"paths":{"/create":{"post":{"operationId":"create","x-temps-ai-tool":{},"requestBody":{"required":true,"content":{"application/json":{"schema":{"type":"object","required":["value"],"properties":{"value":schema}}}}},"responses":{"200":{"description":"ok"}}}}}})).unwrap();
            let plugin = PluginToolsIndex::from_openapi("reports", &spec, true).unwrap();
            let mut index = ReadOnlyApiIndex::from_openapi_allowlist(&spec, &[]);
            plugin.append_to(&mut index, PluginToolAccess::Write);
            for value in values {
                let command = format!("plugin.reports.create --value '{value}'");
                let (op, params) = match resolve(&index, &command, &|_| true, CliMode::Write) {
                    CliAction::Execute(op, params) => (op, params),
                    CliAction::Terminal(error) => panic!("{error}"),
                };
                let request = crate::build_request_parts(op, &params, &[], 20, 100).unwrap();
                assert_eq!(request.body, Some(json!({"value": value})), "{command}");
            }
            assert!(matches!(
                resolve(
                    &index,
                    "plugin.reports.create --value invalid-json",
                    &|_| true,
                    CliMode::Write
                ),
                CliAction::Terminal(_)
            ));
            let op = plugin
                .callable("plugin.reports.create", PluginToolAccess::Write)
                .unwrap();
            assert!(matches!(
                crate::build_request_parts(op, &json!({}), &[], 20, 100),
                Err(crate::ApiToolError::MissingParam { .. })
            ));
        }
    }

    #[test]
    fn unsupported_root_bodies_are_rejected_and_object_components_are_preserved() {
        for schema in [
            json!({"allOf":[{"type":"object","properties":{"report_name":{"type":"string"}},"required":["report_name"]}]}),
            json!({"oneOf":[{"type":"object"},{"type":"array","items":{"type":"string"}}]}),
            json!({"type":"array","items":{"type":"string"}}),
            json!({"type":"string"}),
            json!({"type":"object","additionalProperties":{"type":"string"},"minProperties":1}),
            json!({"type":"object","additionalProperties":true}),
            json!({"type":"object","minProperties":1}),
            json!({"$ref":"#/components/schemas/Missing"}),
        ] {
            let spec = serde_json::from_value(json!({"openapi":"3.1.0","info":{"title":"Reports","version":"1"},"paths":{"/create":{"post":{"operationId":"create","x-temps-ai-tool":{},"requestBody":{"content":{"application/json":{"schema":schema}}},"responses":{"200":{"description":"ok"}}}}}})).unwrap();
            let error = PluginToolsIndex::from_openapi("reports", &spec, true).unwrap_err();
            assert!(
                matches!(error, PluginToolsError::UnsupportedRequestBody { ref plugin, ref operation_id } if plugin == "reports" && operation_id == "create")
            );
            assert!(error.to_string().contains("application/json object schema"));
        }
        let spec = serde_json::from_value(json!({"openapi":"3.1.0","info":{"title":"Reports","version":"1"},"components":{"schemas":{"Input":{"type":"object","properties":{"report_name":{"type":"string"}},"required":["report_name"]}}},"paths":{"/create":{"post":{"operationId":"create","x-temps-ai-tool":{},"requestBody":{"content":{"application/json":{"schema":{"$ref":"#/components/schemas/Input"}}}},"responses":{"200":{"description":"ok"}}}}}})).unwrap();
        let index = PluginToolsIndex::from_openapi("reports", &spec, true).unwrap();
        let operation = index
            .callable("plugin.reports.create", PluginToolAccess::Write)
            .unwrap();
        assert_eq!(operation.params.len(), 1);
        assert_eq!(operation.params[0].name, "report_name");
        assert!(operation.params[0].required);
    }

    #[test]
    fn query_string_array_items_survive_cli_coercion_including_schema_references() {
        use crate::cli::{resolve, CliAction, CliMode};
        for schema in [
            json!({"type":"array","items":{"type":"string"}}),
            json!({"$ref":"#/components/schemas/Ids"}),
        ] {
            let spec = serde_json::from_value(json!({"openapi":"3.1.0","info":{"title":"Reports","version":"1"},"components":{"schemas":{"Ids":{"type":"array","items":{"$ref":"#/components/schemas/Id"}},"Id":{"type":"string"}}},"paths":{"/list":{"get":{"operationId":"list","x-temps-ai-tool":{},"parameters":[{"in":"query","name":"ids","required":true,"schema":schema}],"responses":{"200":{"description":"ok"}}}}}})).unwrap();
            let plugin = PluginToolsIndex::from_openapi("reports", &spec, true).unwrap();
            let mut index = ReadOnlyApiIndex::from_openapi_allowlist(&spec, &[]);
            plugin.append_to(&mut index, PluginToolAccess::Read);
            let (op, params) = match resolve(
                &index,
                "plugin.reports.list --ids 123,true",
                &|_| true,
                CliMode::ReadOnly,
            ) {
                CliAction::Execute(op, params) => (op, params),
                CliAction::Terminal(error) => panic!("{error}"),
            };
            assert_eq!(params, json!({"ids":["123","true"]}));
            let request = crate::build_request_parts(op, &params, &[], 20, 100).unwrap();
            assert_eq!(
                urlencoding::decode(&request.query).unwrap(),
                "ids=123&ids=true"
            );
        }
    }

    #[test]
    fn composite_query_values_preserve_json_types_through_cli_and_url_encoding() {
        use crate::cli::{resolve, CliAction, CliMode};
        for schema in [
            json!({"anyOf":[{"type":"string"},{"type":"number"},{"type":"null"}]}),
            json!({"$ref":"#/components/schemas/Choice"}),
        ] {
            let spec = serde_json::from_value(json!({"openapi":"3.1.0","info":{"title":"Reports","version":"1"},"components":{"schemas":{"Choice":{"type":["string","number","null"]}}},"paths":{"/filter":{"get":{"operationId":"filter","x-temps-ai-tool":{},"parameters":[{"in":"query","name":"choice","required":true,"content":{"application/json":{"schema":schema}}}],"responses":{"200":{"description":"ok"}}}}}})).unwrap();
            let plugin = PluginToolsIndex::from_openapi("reports", &spec, true).unwrap();
            let mut index = ReadOnlyApiIndex::from_openapi_allowlist(&spec, &[]);
            plugin.append_to(&mut index, PluginToolAccess::Read);
            for value in [
                json!(2),
                json!("2"),
                json!("all"),
                json!(null),
                json!("null"),
            ] {
                let command = format!("plugin.reports.filter --choice '{value}'");
                let (op, params) = match resolve(&index, &command, &|_| true, CliMode::ReadOnly) {
                    CliAction::Execute(op, params) => (op, params),
                    CliAction::Terminal(error) => panic!("{error}"),
                };
                let request = crate::build_request_parts(op, &params, &[], 20, 100).unwrap();
                assert_eq!(
                    urlencoding::decode(&request.query).unwrap(),
                    format!("choice={value}")
                );
            }
        }
    }

    #[test]
    fn route_resolution_only_compares_endpoints_for_the_requested_method() {
        let spec = serde_json::from_value(json!({"openapi":"3.1.0","info":{"title":"Reports","version":"1"},"paths":{
            "/reports/{name}":{"get":{"operationId":"read","x-temps-ai-tool":{},"parameters":[{"name":"name","in":"path","required":true,"schema":{"type":"string"}}],"responses":{"200":{"description":"ok"}}}},
            "/reports/export":{"post":{"operationId":"export","x-temps-ai-tool":{},"responses":{"200":{"description":"ok"}}}},
            "/reports/reset":{"get":{"operationId":"reset","x-temps-ai-tool":{"access":"write"},"responses":{"200":{"description":"ok"}}}},
            "/reports/private":{"get":{"responses":{"200":{"description":"ok"}}}}
        }})).unwrap();
        let index = PluginToolsIndex::from_openapi("reports", &spec, true).unwrap();
        let read = index
            .callable("plugin.reports.read", PluginToolAccess::Read)
            .unwrap();
        assert!(index.resolves_to(read, "/x/reports/reports/export"));
        assert!(!index.resolves_to(read, "/x/reports/reports/reset"));
        assert!(!index.resolves_to(read, "/x/reports/reports/private"));
        let write = index
            .callable("plugin.reports.export", PluginToolAccess::Write)
            .unwrap();
        assert!(index.resolves_to(write, "/x/reports/reports/export"));
    }
}
