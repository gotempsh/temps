// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Credential-safe presentation shared by chat and external MCP.
use std::sync::OnceLock;
/// Whether a field name indicates a credential or private configuration value.
pub fn is_sensitive_field_name(name: &str) -> bool {
    const SENSITIVE: &[&str] = &[
        "value",
        "secret",
        "password",
        "token",
        "key",
        "credential",
        "authorization",
        "cookie",
        "headers",
        "webhook",
    ];
    let lower = name.to_ascii_lowercase();
    SENSITIVE.iter().any(|needle| lower.contains(needle))
}

/// Recursively replace values under credential-like keys.
pub fn redact_value(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(object) => {
            // Generic webhook provider configs use the deliberately neutral
            // field name `url`. In that schema the sibling method/headers
            // fields identify it as a delivery endpoint whose path commonly
            // embeds a credential; ordinary public URLs remain visible.
            let webhook_config = object.contains_key("url")
                && object.contains_key("method")
                && object.contains_key("headers");
            serde_json::Value::Object(
                object
                    .iter()
                    .map(|(key, value)| {
                        let lower = key.to_ascii_lowercase();
                        let sensitive =
                            is_sensitive_field_name(key) || (webhook_config && lower == "url");
                        let value = if sensitive {
                            serde_json::Value::String("***".to_string())
                        } else {
                            redact_value(value)
                        };
                        (key.clone(), value)
                    })
                    .collect(),
            )
        }
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.iter().map(redact_value).collect())
        }
        serde_json::Value::String(value) => serde_json::Value::String(redact_text(value)),
        _ => value.clone(),
    }
}

/// Preserve known configuration and metric fields without weakening credential
/// masking for other operations or nested credentials in JSON flag values.
pub fn redact_api_result(operation_id: &str, value: &serde_json::Value) -> serde_json::Value {
    let mut redacted = redact_value(value);
    match operation_id {
        "get_environment_variables" | "get_resolved_environment_variables" => {
            if let (Some(original), Some(masked)) = (value.as_array(), redacted.as_array_mut()) {
                for (original, masked) in original.iter().zip(masked) {
                    if let (Some(name), Some(object)) = (
                        original.get("key").and_then(serde_json::Value::as_str),
                        masked.as_object_mut(),
                    ) {
                        object.insert("key".into(), serde_json::Value::String(redact_text(name)));
                    }
                }
            }
        }
        "query_metrics" => {
            if let (Some(original), Some(masked)) = (
                value.get("data").and_then(serde_json::Value::as_array),
                redacted
                    .get_mut("data")
                    .and_then(serde_json::Value::as_array_mut),
            ) {
                for (original, masked) in original.iter().zip(masked) {
                    restore_metric_bucket(original, masked);
                }
            }
        }
        "list_metric_label_keys" | "list_metric_label_values" => {
            let field = if operation_id == "list_metric_label_keys" {
                "keys"
            } else {
                "values"
            };
            if let (Some(values), Some(object)) = (
                value.get(field).and_then(serde_json::Value::as_array),
                redacted.as_object_mut(),
            ) {
                object.insert(
                    field.into(),
                    redact_value(&serde_json::Value::Array(values.clone())),
                );
            }
        }
        "list_flags" | "get_flag_snapshot" => {
            if let (Some(original), Some(masked)) = (
                value.get("flags").and_then(serde_json::Value::as_array),
                redacted
                    .get_mut("flags")
                    .and_then(serde_json::Value::as_array_mut),
            ) {
                for (original, masked) in original.iter().zip(masked) {
                    restore_flag_fields(original, masked);
                }
            }
        }
        "get_flag"
        | "create_flag"
        | "update_flag"
        | "restore_flag"
        | "archive_flag"
        | "set_flag_environment" => restore_flag_fields(value, &mut redacted),
        _ => {}
    }
    redacted
}

/// Write previews use an explicit operation policy, independently of response
/// schemas, so configuration values remain distinguishable before approval.
pub fn redact_api_proposal(operation_id: &str, params: &serde_json::Value) -> serde_json::Value {
    match operation_id {
        "create_flag"
        | "update_flag"
        | "restore_flag"
        | "archive_flag"
        | "set_flag_environment" => redact_api_result(operation_id, params),
        _ => redact_value(params),
    }
}

/// Restore declared metric measurements and sanitize ordered series labels.
/// Unexpected strings or objects in numeric fields remain masked.
fn restore_metric_bucket(original: &serde_json::Value, redacted: &mut serde_json::Value) {
    let Some(object) = redacted.as_object_mut() else {
        return;
    };
    for field in ["avg_value", "min_value", "max_value", "value"] {
        if let Some(value) = original.get(field).filter(|value| value.is_number()) {
            object.insert(field.into(), value.clone());
        }
    }
    let Some(series) = original.get("series_key") else {
        return;
    };
    // MetricBucket uses ordered label pairs, not an object. Apply the same
    // credential-name masking to each label's value before preserving the pairs.
    if let Ok(labels) = serde_json::from_value::<Option<Vec<(String, String)>>>(series.clone()) {
        let labels = labels
            .map(|labels| {
                serde_json::Value::Array(
                    labels
                        .into_iter()
                        .map(|(name, value)| {
                            let masked = redact_value(&serde_json::json!({name.as_str(): value}));
                            serde_json::json!([redact_text(&name), masked[&name]])
                        })
                        .collect(),
                )
            })
            .unwrap_or(serde_json::Value::Null);
        object.insert("series_key".into(), labels);
    }
}

/// Preserve known flag configuration while masking nested credential fields.
fn restore_flag_fields(original: &serde_json::Value, redacted: &mut serde_json::Value) {
    let Some(object) = redacted.as_object_mut() else {
        return;
    };
    for field in [
        "key",
        "value_type",
        "default_value",
        "environment_value",
        "value",
    ] {
        if let Some(value) = original.get(field) {
            object.insert(field.into(), redact_value(value));
        }
    }
    if let (Some(original), Some(masked)) = (
        original
            .get("environments")
            .and_then(serde_json::Value::as_array),
        object
            .get_mut("environments")
            .and_then(serde_json::Value::as_array_mut),
    ) {
        for (original, masked) in original.iter().zip(masked) {
            if let (Some(value), Some(object)) = (original.get("value"), masked.as_object_mut()) {
                object.insert("value".into(), redact_value(value));
            }
        }
    }
}

/// Redact a JSON-encoded tool argument/result when possible. Plain text is
/// preserved because it commonly contains useful logs rather than structured
/// credentials; providers must not persist raw structured secret values.
pub fn redact_json_string(value: &str) -> String {
    serde_json::from_str(value)
        .map(|value| redact_value(&value).to_string())
        .unwrap_or_else(|_| redact_text(value))
}

/// Scrub secrets embedded in CLI command strings or unstructured provider/API
/// errors. `temps_write` intentionally carries a command inside a neutral JSON
/// key, so recursive key redaction alone cannot protect `--value SECRET`.
pub fn redact_text(value: &str) -> String {
    static ASSIGNMENT_SECRET: OnceLock<Option<regex::Regex>> = OnceLock::new();
    static BEARER_SECRET: OnceLock<Option<regex::Regex>> = OnceLock::new();
    static URL_USERINFO: OnceLock<Option<regex::Regex>> = OnceLock::new();
    static PROVIDER_SECRET: OnceLock<Option<regex::Regex>> = OnceLock::new();

    let assignments = ASSIGNMENT_SECRET.get_or_init(|| {
        regex::Regex::new(
            r#"(?i)\b(password|secret|token|api[_-]?key|access[_-]?key)\b(["']?\s*[:=]\s*)(?:"[^"]*"|'[^']*'|[^\s,;&}]+)"#,
        )
        .ok()
    });
    let bearer = BEARER_SECRET.get_or_init(|| regex::Regex::new(r"(?i)\bBearer\s+[^\s,;]+").ok());
    let provider = PROVIDER_SECRET
        .get_or_init(|| regex::Regex::new(r"\b(?:sk|key|token)-[A-Za-z0-9._-]{6,}").ok());

    let userinfo = URL_USERINFO
        .get_or_init(|| regex::Regex::new(r"(?i)([a-z][a-z0-9+.-]*://)[^/\s?#]+@").ok());
    let scrubbed = userinfo.as_ref().map_or_else(
        || value.to_string(),
        |pattern| pattern.replace_all(value, "${1}***@").into_owned(),
    );
    let scrubbed = redact_cli_secret_flags(&scrubbed);
    let scrubbed = assignments.as_ref().map_or_else(
        || scrubbed.clone(),
        |pattern| pattern.replace_all(&scrubbed, "$1$2***").into_owned(),
    );
    let scrubbed = bearer.as_ref().map_or_else(
        || scrubbed.clone(),
        |pattern| pattern.replace_all(&scrubbed, "Bearer ***").into_owned(),
    );
    provider.as_ref().map_or(scrubbed.clone(), |pattern| {
        pattern.replace_all(&scrubbed, "[redacted]").into_owned()
    })
}

/// Redact complete CLI value tokens using the same quote-aware token boundary
/// rules as `temps-ai-api-tools::cli::tokenize`. This deliberately handles
/// mixed quoted/unquoted tokens and the parser's legacy acceptance of an
/// unmatched quote; both are impossible to model safely with a "quoted value"
/// regex without leaving a suffix visible.
fn redact_cli_secret_flags(input: &str) -> String {
    let spans = cli_token_spans(input);
    let mut replacements = Vec::new();
    let mut index = 0;
    while index < spans.len() {
        let (start, end) = spans[index];
        let raw_token = &input[start..end];
        let token = normalize_cli_token(raw_token);
        if let Some((flag, _)) = token.split_once('=') {
            if is_sensitive_cli_flag(flag) {
                if let Some(equals) = raw_token.find('=') {
                    replacements.push((start + equals + 1, end));
                }
            }
        } else if is_sensitive_cli_flag(&token) && index + 1 < spans.len() {
            let next = spans[index + 1];
            if !normalize_cli_token(&input[next.0..next.1]).starts_with("--") {
                replacements.push(next);
                index += 1;
            }
        }
        index += 1;
    }

    let mut output = input.to_string();
    for (start, end) in replacements.into_iter().rev() {
        output.replace_range(start..end, "***");
    }
    output
}

/// Normalize a CLI token for sensitive-flag detection without executing it.
fn normalize_cli_token(raw: &str) -> String {
    let mut normalized = String::new();
    let mut quote = None;
    for character in raw.chars() {
        match quote {
            Some(active) if character == active => quote = None,
            Some(_) => normalized.push(character),
            None if character == '\'' || character == '"' => quote = Some(character),
            None => normalized.push(character),
        }
    }
    normalized
}

/// Identify flags whose arguments must never be returned in tool output.
fn is_sensitive_cli_flag(token: &str) -> bool {
    let Some(flag) = token.strip_prefix("--") else {
        return false;
    };
    let flag = flag.to_ascii_lowercase();
    flag == "value"
        || flag == "config"
        || ["password", "secret", "token", "key"]
            .iter()
            .any(|needle| flag.contains(needle))
}

fn cli_token_spans(input: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut start = None;
    let mut quote = None;
    for (index, character) in input.char_indices() {
        match quote {
            Some(active) if character == active => quote = None,
            Some(_) => {}
            None if character == '\'' || character == '"' => {
                start.get_or_insert(index);
                quote = Some(character);
            }
            None if character.is_whitespace() => {
                if let Some(start) = start.take() {
                    spans.push((start, index));
                }
            }
            None => {
                start.get_or_insert(index);
            }
        }
    }
    if let Some(start) = start {
        spans.push((start, input.len()));
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_survive_but_environment_values_and_url_credentials_do_not() {
        for operation in [
            "get_environment_variables",
            "get_resolved_environment_variables",
        ] {
            let result = redact_api_result(
                operation,
                &serde_json::json!([{"key":"DATABASE_URL","value":"private","value_preview":"private"}]),
            );
            assert_eq!(result[0]["key"], "DATABASE_URL");
            assert_eq!(result[0]["value"], "***");
            assert_eq!(result[0]["value_preview"], "***");
        }
        for url in [
            "postgres://fixture:private@example.test/db",
            "https://fixture:p%40ss@example.test/path",
            "https://fixture:p@private@example.test/path",
            "redis://:private@[::1]:6379",
        ] {
            let result = redact_api_result(
                "query_metrics",
                &serde_json::json!({"data":[{"series_key":[["db.connection",url],["http.method","GET"]]}]}),
            );
            let value = result["data"][0]["series_key"][0][1].as_str().unwrap();
            assert!(value.contains("://***@"));
            assert!(!value.contains("private"));
            assert!(!value.contains("p%40ss"));
            assert_eq!(result["data"][0]["series_key"][1][1], "GET");
        }
    }

    #[test]
    fn metric_query_preserves_measurements_and_redacts_credentials() {
        let bucket = serde_json::json!({
            "bucket":"2026-01-01T00:00:00Z",
            "avg_value":0.125, "min_value":-2.5, "max_value":8.75, "value":0.0,
            "count":4, "quantiles":[[0.95,8.0]],
            "histogram_summary":{"count":4,"sum":0.5,"bounds":[0.0,10.0],"bucket_counts":[1,3,0]},
            "series_key":[["http.method","GET"],["authorization","fixture-private"],["service.name","Bearer fixture-private"]],
            "token":"fixture-private"
        });
        let response = serde_json::json!({"data":[bucket],"count":1,"api_key":"fixture-private"});
        let result = redact_api_result("query_metrics", &response);
        for field in [
            "bucket",
            "avg_value",
            "min_value",
            "max_value",
            "value",
            "count",
            "quantiles",
            "histogram_summary",
        ] {
            assert_eq!(
                result["data"][0][field], response["data"][0][field],
                "{field}"
            );
        }
        assert_eq!(
            result["data"][0]["series_key"],
            serde_json::json!([
                ["http.method", "GET"],
                ["authorization", "***"],
                ["service.name", "Bearer ***"]
            ])
        );
        assert_eq!(result["data"][0]["token"], "***");
        assert_eq!(result["api_key"], "***");
        assert_eq!(
            redact_api_result("other_operation", &response)["data"][0]["value"],
            "***"
        );

        let malformed = serde_json::json!({"data":[{"value":"fixture-private","avg_value":{"token":"fixture-private"},"series_key":{"password":"fixture-private"}}]});
        let masked = redact_api_result("query_metrics", &malformed);
        for field in ["value", "avg_value", "series_key"] {
            assert_eq!(masked["data"][0][field], "***");
        }
        for series in [serde_json::Value::Null, serde_json::json!([])] {
            let data = serde_json::json!({"data":[{"value":3.0,"series_key":series}]});
            assert_eq!(redact_api_result("query_metrics", &data), data);
        }
        let empty = serde_json::json!({"data":[],"count":0});
        assert_eq!(redact_api_result("query_metrics", &empty), empty);
    }

    #[test]
    fn metric_lists_and_flag_proposals_preserve_only_known_configuration_fields() {
        for (operation, field) in [
            ("list_metric_label_keys", "keys"),
            ("list_metric_label_values", "values"),
        ] {
            let data = serde_json::json!({field:["GET","Bearer fixture-private"],"token":"fixture-private"});
            let result = redact_api_result(operation, &data);
            assert_eq!(result[field][0], "GET");
            assert_eq!(result[field][1], "Bearer ***");
            assert_eq!(result["token"], "***");
            assert_eq!(redact_api_result("other_operation", &data)[field], "***");
        }
        let params = serde_json::json!({"key":"checkout.v2","value":false,"default_value":{"password":"fixture-private","enabled":true}});
        let result = redact_api_proposal("set_flag_environment", &params);
        assert_eq!(result["key"], "checkout.v2");
        assert_eq!(result["value"], false);
        assert_eq!(
            redact_api_proposal(
                "set_flag_environment",
                &serde_json::json!({"key":"checkout.v2","value":null})
            )["value"],
            serde_json::Value::Null
        );
        assert_eq!(result["default_value"]["password"], "***");
        assert_eq!(
            redact_api_proposal("create_secret", &params)["value"],
            "***"
        );
    }

    #[test]
    fn flag_result_policy_preserves_configuration_but_masks_nested_credentials() {
        let flag = serde_json::json!({"key":"checkout.v2","value_type":"json",
            "default_value":{"enabled":false,"password":"fixture-password"},
            "environment_value":true,"environments":[{"environment_id":1,"value":true}],
            "api_key":"fixture-key"});
        for operation in [
            "get_flag",
            "create_flag",
            "update_flag",
            "restore_flag",
            "archive_flag",
        ] {
            let result = redact_api_result(operation, &flag);
            assert_eq!(result["key"], "checkout.v2");
            assert_eq!(result["value_type"], "json");
            assert_eq!(result["default_value"]["enabled"], false);
            assert_eq!(result["default_value"]["password"], "***");
            assert_eq!(result["environment_value"], true);
            assert_eq!(result["environments"][0]["value"], true);
            assert_eq!(result["api_key"], "***");
        }
        for operation in ["list_flags", "get_flag_snapshot"] {
            let result = redact_api_result(operation, &serde_json::json!({"flags":[flag.clone()]}));
            assert_eq!(result["flags"][0]["key"], "checkout.v2");
        }
        let result = redact_api_result("get_environment_variables", &flag);
        assert_eq!(result["key"], "***");
        assert_eq!(result["default_value"], "***");
        assert_eq!(
            redact_api_result("set_flag_environment", &serde_json::json!({"value":false}))["value"],
            false
        );
    }

    #[test]
    fn notification_provider_credentials_are_redacted_from_review_and_results() {
        let provider = serde_json::json!({
            "provider_type": "webhook",
            "config": {
                "url": "https://hooks.example.test/services/private-path",
                "method": "POST",
                "headers": {"X-Signature": "private-signature"}
            }
        });
        let redacted = redact_value(&provider);
        assert_eq!(redacted["config"]["url"], "***");
        assert_eq!(redacted["config"]["headers"], "***");

        let command = serde_json::json!({
            "command": "notifications create_notification_provider --config '{\"url\":\"https://hooks.example.test/private\"}'"
        });
        let display = redact_json_string(&command.to_string());
        assert!(!display.contains("hooks.example.test"));
        assert!(display.contains("--config ***"));
    }

    #[test]
    fn write_command_arguments_are_safe_for_sse_and_history() {
        let arguments = serde_json::json!({
            "command": "projects create_environment_variable --name DATABASE_URL --value 'postgres://user:pass@db/app' --api-key sk-live-abcdef"
        })
        .to_string();
        let display = redact_json_string(&arguments);
        assert!(!display.contains("postgres://user:pass@db/app"));
        assert!(!display.contains("sk-live-abcdef"));
        assert!(display.contains("--value ***"));
        assert!(display.contains("--api-key ***"));
    }

    #[test]
    fn write_command_redaction_consumes_complete_punctuation_values() {
        let secret = "VERY_SECRET";
        for command in [
            format!(
                r#"projects create_environment_variable --name CONFIG --value {{"first":"public","second":"{secret}"}}"#
            ),
            format!("projects create_environment_variable --name CONFIG --value abc;{secret}"),
        ] {
            let arguments = serde_json::json!({"command": command}).to_string();
            let display = redact_json_string(&arguments);
            assert!(!display.contains(secret), "secret leaked from {arguments}");
            assert!(display.contains("--value ***"));
        }
    }

    #[test]
    fn write_command_redaction_matches_mixed_and_unterminated_quote_tokens() {
        let secret = "VERY_SECRET";
        for command in [
            format!("projects create_environment_variable --name X --value 'prefix'{secret}"),
            format!("projects create_environment_variable --name X --value 'prefix {secret}"),
        ] {
            let display = redact_json_string(&serde_json::json!({"command": command}).to_string());
            assert!(!display.contains(secret), "secret leaked from {command}");
            assert!(display.contains("--value ***"));
        }
    }

    #[test]
    fn bare_sensitive_boolean_does_not_hide_the_following_secret_flag() {
        for command in [
            "operation --token --value VERY_SECRET",
            "operation --value --token VERY_SECRET",
        ] {
            let display = redact_json_string(&serde_json::json!({"command": command}).to_string());
            assert!(
                !display.contains("VERY_SECRET"),
                "secret leaked from {command}"
            );
        }
    }

    #[test]
    fn quoted_or_mixed_sensitive_flag_names_are_classified_like_execution() {
        for command in [
            r#"operation "--value" VERY_SECRET"#,
            "operation --'value' VERY_SECRET",
            "operation --'api-key'=VERY_SECRET",
        ] {
            let display = redact_json_string(&serde_json::json!({"command": command}).to_string());
            assert!(
                !display.contains("VERY_SECRET"),
                "secret leaked from {command}"
            );
        }
    }

    #[test]
    fn unstructured_upstream_errors_are_scrubbed() {
        let error = r#"HTTP 400: {"password":"hunter2","detail":"Bearer token-abcdef rejected"}"#;
        let display = redact_text(error);
        assert!(!display.contains("hunter2"));
        assert!(!display.contains("token-abcdef"));
    }
}
