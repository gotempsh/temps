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
}

#[derive(Debug, thiserror::Error)]
pub enum PluginToolsError {
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
}

impl PluginToolsIndex {
    /// Remove one stopped plugin while retaining unrelated healthy declarations.
    pub fn without_plugin(&self, plugin: &str) -> Self {
        let mut index = self.clone();
        index.tools.retain(|tool| tool.plugin != plugin);
        let prefix = format!("plugin.{plugin}.");
        index
            .secrets
            .retain(|operation, _| !operation.starts_with(&prefix));
        index
    }

    pub fn extend(&mut self, other: Self) {
        self.tools.extend(other.tools);
        self.secrets.extend(other.secrets);
    }

    pub(crate) fn append_to(&self, index: &mut ReadOnlyApiIndex, access: PluginToolAccess) {
        for tool in &self.tools {
            if tool.access != access && (tool.enabled || access == PluginToolAccess::Write) {
                continue;
            }
            let mut operation = tool.operation.clone();
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
        if !valid_identifier(plugin) {
            return Err(PluginToolsError::InvalidPluginName {
                plugin: plugin.into(),
            });
        }
        let mut tools = Vec::new();
        let mut secrets = HashMap::new();
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
                let access = if method == "GET"
                    && mark
                        .get("access")
                        .and_then(|v| v.as_str())
                        .is_none_or(|v| v == "read")
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
}
