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
    tools: Vec<PluginTool>,
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
                for inherited in item.parameters.iter().flatten() {
                    let overridden =
                        parameters
                            .iter()
                            .any(|parameter| match (parameter, inherited) {
                                (RefOr::T(local), RefOr::T(shared)) => {
                                    local.name == shared.name
                                        && local.parameter_in == shared.parameter_in
                                }
                                (RefOr::Ref(local), RefOr::Ref(shared)) => {
                                    local.ref_location == shared.ref_location
                                }
                                _ => false,
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
                tools.push(PluginTool {
                    plugin: plugin.into(),
                    operation,
                    access,
                    enabled,
                    setup_path: format!("/settings/plugins/{plugin}"),
                });
            }
        }
        Ok(Self { tools })
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
}
