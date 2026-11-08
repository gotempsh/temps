// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::{ApiOperation, ApiToolError, ParamSpec};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Supported OpenAPI query representations, independent of CLI input coercion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum QuerySerialization {
    Json,
    Form { explode: bool },
    SpaceDelimited,
    PipeDelimited,
    DeepObject,
}

pub(crate) fn serialize(
    op: &ApiOperation,
    param: &ParamSpec,
    value: &Value,
) -> Result<Vec<String>, ApiToolError> {
    let Some(encoding) = &param.query_serialization else {
        // Curated core operations retain their existing query representation.
        let text = if param.ty == "json" {
            value.to_string()
        } else {
            crate::caller::value_to_string(value)
        };
        return Ok(vec![format!(
            "{}={}",
            urlencoding::encode(&param.name),
            urlencoding::encode(&text),
        )]);
    };
    let invalid = || ApiToolError::BadParamType {
        operation_id: op.operation_id.clone(),
        name: param.name.clone(),
        expected: format!("a value supported by {encoding:?} query serialization"),
        problem: "Nested values or an incompatible query shape require application/json content"
            .into(),
    };
    let scalar = |value: &Value| match value {
        Value::String(s) => Ok(urlencoding::encode(s).into_owned()),
        Value::Number(_) | Value::Bool(_) => Ok(value.to_string()),
        _ => Err(invalid()),
    };
    let name = urlencoding::encode(&param.name);
    if matches!(encoding, QuerySerialization::Json) {
        return Ok(vec![format!(
            "{name}={}",
            urlencoding::encode(&value.to_string())
        )]);
    }
    if matches!(encoding, QuerySerialization::DeepObject) {
        return value
            .as_object()
            .ok_or_else(invalid)?
            .iter()
            .map(|(key, value)| {
                Ok(format!(
                    "{}={}",
                    urlencoding::encode(&format!("{}[{key}]", param.name)),
                    scalar(value)?
                ))
            })
            .collect();
    }
    if let Value::Array(items) = value {
        let items = items.iter().map(scalar).collect::<Result<Vec<_>, _>>()?;
        if matches!(encoding, QuerySerialization::Form { explode: true }) {
            return Ok(items
                .into_iter()
                .map(|value| format!("{name}={value}"))
                .collect());
        }
        let separator = match encoding {
            QuerySerialization::SpaceDelimited => "%20",
            QuerySerialization::PipeDelimited => "|",
            _ => ",",
        };
        return Ok(vec![format!("{name}={}", items.join(separator))]);
    }
    if !matches!(encoding, QuerySerialization::Form { .. }) {
        return Err(invalid());
    }
    Ok(vec![format!("{name}={}", scalar(value)?)])
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, extract::Request, routing::get, Json, Router};
    use serde_json::json;
    use tower::ServiceExt;

    fn index(parameter: Value) -> Result<crate::PluginToolsIndex, crate::PluginToolsError> {
        let spec = serde_json::from_value(
            json!({"openapi":"3.1.0","info":{"title":"Reports","version":"1"},
            "paths":{"/list":{"get":{"operationId":"list","x-temps-ai-tool":{},
                "parameters":[parameter],"responses":{"200":{"description":"OK"}}}}}}),
        )
        .unwrap();
        crate::PluginToolsIndex::from_openapi("reports", &spec, true)
    }

    #[tokio::test]
    async fn declared_query_formats_reach_the_endpoint_without_changing_values() {
        let array = json!({"type":"array","items":{"type":"string"}});
        let object = json!({"type":"object","properties":{"region":{"type":"string"},"count":{"type":"integer"}}});
        for (declaration, value, expected) in [
            (
                json!({"schema":array}),
                json!(["a&b", "123"]),
                json!([["value", "a&b"], ["value", "123"]]),
            ),
            (
                json!({"schema":array,"style":"form","explode":true}),
                json!(["a", "b"]),
                json!([["value", "a"], ["value", "b"]]),
            ),
            (
                json!({"schema":array,"style":"form","explode":false}),
                json!(["a", "b"]),
                json!([["value", "a,b"]]),
            ),
            (
                json!({"schema":array,"style":"spaceDelimited","explode":false}),
                json!(["a", "b"]),
                json!([["value", "a b"]]),
            ),
            (
                json!({"schema":array,"style":"pipeDelimited"}),
                json!(["a", "b"]),
                json!([["value", "a|b"]]),
            ),
            (
                json!({"schema":object,"style":"deepObject","explode":true}),
                json!({"region":"west","count":2}),
                json!([["value[count]", "2"], ["value[region]", "west"]]),
            ),
            (
                json!({"content":{"application/json":{"schema":array}}}),
                json!(["a", "b"]),
                json!([["value", "[\"a\",\"b\"]"]]),
            ),
            (
                json!({"content":{"application/json":{"schema":{"anyOf":[{"type":"string"},{"type":"number"}]}}}}),
                json!("123"),
                json!([["value", "\"123\""]]),
            ),
        ] {
            let mut parameter = declaration;
            parameter["name"] = json!("value");
            parameter["in"] = json!("query");
            parameter["required"] = json!(true);
            let index = index(parameter).unwrap();
            let operation = &index.tools()[0].operation;
            let built =
                crate::build_request_parts(operation, &json!({"value":value}), &[], 20, 100)
                    .unwrap();
            let router = Router::new().route(
                "/x/reports/list",
                get(|request: Request| async move {
                    let pairs: Vec<_> = request
                        .uri()
                        .query()
                        .unwrap_or("")
                        .split('&')
                        .map(|pair| {
                            let (name, value) = pair.split_once('=').unwrap();
                            [
                                urlencoding::decode(name).unwrap().into_owned(),
                                urlencoding::decode(value).unwrap().into_owned(),
                            ]
                        })
                        .collect();
                    Json(pairs)
                }),
            );
            let response = router
                .oneshot(
                    Request::builder()
                        .uri(format!("{}?{}", built.path, built.query))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), 200);
            let received: Value = serde_json::from_slice(
                &axum::body::to_bytes(response.into_body(), 8192)
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(received, expected);
        }
    }

    #[test]
    fn unsupported_declarations_are_rejected_before_discovery() {
        for declaration in [
            json!({"schema":{"type":"object"}}),
            json!({"schema":{"type":"string"},"style":"simple"}),
            json!({"schema":{"type":"array","items":{"type":"string"}},"style":"pipeDelimited","explode":true}),
            json!({"schema":{"type":"string"},"allowReserved":true}),
            json!({"content":{"text/plain":{"schema":{"type":"string"}}}}),
            json!({"schema":{"type":"string"},"content":{"application/json":{"schema":{"type":"string"}}}}),
        ] {
            let mut parameter = declaration;
            parameter["name"] = json!("value");
            parameter["in"] = json!("query");
            parameter["required"] = json!(true);
            assert!(matches!(
                index(parameter),
                Err(crate::PluginToolsError::QuerySerialization { .. })
            ));
        }
    }
}
