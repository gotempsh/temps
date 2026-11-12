// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Every request field that takes a credential must say so in the spec.
//!
//! A request field typed [`temps_core::secret_string::SecretString`] is
//! published as `format: password` with the `x-temps-secret` extension. That
//! flag is how the AI assistant's tool layer will know which fields take a
//! credential, and so which fields may receive a secret reference instead of
//! plaintext. A credential field the spec does not flag is one the assistant
//! can only handle by having the model see the value, so this test refuses new
//! ones.
//!
//! [`UNMARKED`] is the debt that existed when the rule was introduced. It can
//! only shrink: converting a field to `SecretString` makes its entry stale and
//! fails [`the_debt_list_holds_only_fields_that_are_still_unmarked`] until the
//! entry is deleted.

use std::collections::BTreeSet;

use serde_json::Value;
use temps_core::secret_string::SECRET_SCHEMA_EXTENSION;

use super::ai_tool_policy::PUBLISHED_SPEC;

/// Name fragments that make a request field look like it carries a credential.
const SECRET_NAME_FRAGMENTS: &[&str] = &[
    "secret",
    "password",
    "passwd",
    "token",
    "api_key",
    "apikey",
    "access_key",
    "private_key",
    "credential",
    "passphrase",
    "auth_header",
    "bearer",
    "webhook_url",
    "connection_string",
    "dsn",
    "smtp_pass",
];

/// Name fragments that make a secret-looking name harmless: a reference to a
/// credential, or a fact about one, rather than the credential itself.
const NOT_THE_VALUE_FRAGMENTS: &[&str] = &[
    "_id",
    "_ids",
    "has_",
    "is_",
    "_count",
    "_at",
    "_present",
    "_set",
    "_masked",
    "_configured",
];

const MAX_SCHEMA_DEPTH: usize = 4;

fn looks_like_a_credential(field: &str) -> bool {
    let field = field.to_ascii_lowercase();
    SECRET_NAME_FRAGMENTS.iter().any(|f| field.contains(f))
        && !NOT_THE_VALUE_FRAGMENTS.iter().any(|f| field.contains(f))
}

fn resolve<'a>(spec: &'a Value, schema: &'a Value) -> Option<(&'a str, &'a Value)> {
    let reference = schema.get("$ref")?.as_str()?;
    let name = reference.rsplit('/').next()?;
    Some((name, spec.pointer(&format!("/components/schemas/{name}"))?))
}

fn is_flagged(spec: &Value, schema: &Value, depth: usize) -> bool {
    if depth > MAX_SCHEMA_DEPTH {
        return false;
    }
    if schema.get(SECRET_SCHEMA_EXTENSION).and_then(Value::as_bool) == Some(true) {
        return true;
    }
    if let Some((_, target)) = resolve(spec, schema) {
        return is_flagged(spec, target, depth + 1);
    }
    ["allOf", "oneOf", "anyOf"].iter().any(|key| {
        schema[*key]
            .as_array()
            .is_some_and(|parts| parts.iter().any(|p| is_flagged(spec, p, depth + 1)))
    })
}

fn is_string_like(spec: &Value, schema: &Value, depth: usize) -> bool {
    if depth > MAX_SCHEMA_DEPTH {
        return false;
    }
    match schema.get("type") {
        Some(Value::String(t)) => return t == "string",
        Some(Value::Array(types)) => return types.iter().any(|t| t == "string"),
        _ => {}
    }
    if let Some((_, target)) = resolve(spec, schema) {
        return is_string_like(spec, target, depth + 1);
    }
    ["allOf", "oneOf", "anyOf"].iter().any(|key| {
        schema[*key]
            .as_array()
            .is_some_and(|parts| parts.iter().any(|p| is_string_like(spec, p, depth + 1)))
    })
}

/// Collect `(path, schema)` for every property reachable from `schema`.
fn walk<'a>(
    spec: &'a Value,
    schema: &'a Value,
    prefix: &str,
    depth: usize,
    seen: &mut Vec<String>,
    out: &mut Vec<(String, &'a Value)>,
) {
    if depth > MAX_SCHEMA_DEPTH {
        return;
    }
    if let Some((name, target)) = resolve(spec, schema) {
        if seen.iter().any(|s| s == name) {
            return;
        }
        seen.push(name.to_string());
        walk(spec, target, prefix, depth + 1, seen, out);
        seen.pop();
        return;
    }
    for key in ["allOf", "oneOf", "anyOf"] {
        for part in schema[key].as_array().into_iter().flatten() {
            walk(spec, part, prefix, depth + 1, seen, out);
        }
    }
    for (name, property) in schema["properties"].as_object().into_iter().flatten() {
        let path = format!("{prefix}{name}");
        out.push((path.clone(), property));
        walk(spec, property, &format!("{path}."), depth + 1, seen, out);
    }
    if let Some(items) = schema.get("items") {
        walk(spec, items, &format!("{prefix}[]."), depth + 1, seen, out);
    }
}

/// `operation_id:field.path` for every JSON request field that looks like it
/// carries a credential but is not flagged as one.
fn unmarked_secret_fields(spec: &Value) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let Some(paths) = spec["paths"].as_object() else {
        return found;
    };
    for item in paths.values().filter_map(Value::as_object) {
        for (method, operation) in item {
            if !matches!(method.as_str(), "post" | "put" | "patch" | "delete") {
                continue;
            }
            let Some(id) = operation["operationId"].as_str() else {
                continue;
            };
            let Some(body) = operation.pointer("/requestBody/content/application~1json/schema")
            else {
                continue;
            };
            let mut fields = Vec::new();
            walk(spec, body, "", 0, &mut Vec::new(), &mut fields);
            for (path, schema) in fields {
                let leaf = path.rsplit('.').next().unwrap_or(&path);
                if looks_like_a_credential(leaf)
                    && !NOT_CREDENTIALS.contains(&format!("{id}:{path}").as_str())
                    && is_string_like(spec, schema, 0)
                    && !is_flagged(spec, schema, 0)
                {
                    found.insert(format!("{id}:{path}"));
                }
            }
        }
    }
    found
}

/// Credential fields that predate the rule and are not yet `SecretString`.
/// Delete an entry when its field is converted; never add one.
const UNMARKED: &[&str] = &[
    "NodeSshEnrollmentCreate:credentials.passphrase",
    "NodeSshEnrollmentCreate:credentials.password",
    "NodeSshEnrollmentCreate:credentials.private_key",
    "change_password_self:current_password",
    "change_password_self:new_password",
    "change_required_password:new_password",
    "createHttpCheck:credential",
    "create_agent:api_key",
    "create_bulk_activation_job:plan_token",
    "create_delivery_profile:bunny_api_key",
    "create_generic_provider:token_username",
    "create_plan:credentials.token",
    "create_user:password",
    "discover_workloads:credentials.token",
    "login:password",
    "register_node:join_token",
    "register_node:prior_token",
    "register_node:token",
    "reset_password:new_password",
    "reset_password:token",
    "save_agent_token:token",
    "setup_mfa:current_password",
    "source_sandbox:password",
    "updateHttpCheck:credential",
    "update_agent:api_key",
    "update_settings:agent_sandbox.api_key_encrypted",
    "update_settings:dns_provider.cloudflare_api_key",
    "update_settings:docker_registry.password",
    "update_settings:preview_gateway.shared_secret",
];

/// Fields whose names look like credentials but which were reviewed and hold
/// none: the name of a header, the kind of a secret, a hash. Unlike [`UNMARKED`]
/// this list is permanent, and adding to it needs the same review.
const NOT_CREDENTIALS: &[&str] = &[
    "createHttpCheck:spec.credential_header",
    "createHttpCheck:spec.credential_prefix",
    // The S3 source's worker credential policy (ADR-102): the enum
    // `scoped` | `allow_full`, which kind of credential a worker node is
    // given, never a credential itself. `test_s3_connection_preview` takes the
    // create request.
    "create_s3_source:worker_credentials",
    "test_s3_connection_preview:worker_credentials",
    "update_s3_source:worker_credentials",
    "updateHttpCheck:spec.credential_header",
    "updateHttpCheck:spec.credential_prefix",
    "upsert_secret:secret_type",
    "update_settings:multi_node.join_token_hash",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> Value {
        serde_json::from_str(PUBLISHED_SPEC).expect("committed openapi.json must parse")
    }

    #[test]
    fn every_credential_field_in_a_request_is_flagged_or_a_known_gap() {
        let found = unmarked_secret_fields(&spec());
        let known: BTreeSet<String> = UNMARKED.iter().map(|s| s.to_string()).collect();
        let new: Vec<_> = found.difference(&known).collect();
        assert!(
            new.is_empty(),
            "these request fields look like credentials but are plain strings in the spec:\n  {}\n\
             Type each as `temps_core::secret_string::SecretString` so the AI assistant can take \
             the value through a secret slot instead of seeing it. If the name only looks \
             secret-like (it holds no credential), rename the field.",
            new.iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join("\n  ")
        );
    }

    #[test]
    fn the_debt_list_holds_only_fields_that_are_still_unmarked() {
        let found = unmarked_secret_fields(&spec());
        let stale: Vec<_> = UNMARKED.iter().filter(|e| !found.contains(**e)).collect();
        assert!(
            stale.is_empty(),
            "these entries are converted or gone; delete them from UNMARKED:\n  {}",
            stale.iter().map(|s| **s).collect::<Vec<_>>().join("\n  ")
        );
    }

    fn doc(request_property: Value, extra_schemas: Value) -> Value {
        let mut schemas = serde_json::json!({
            "SecretString": {
                "type": "string", "format": "password", SECRET_SCHEMA_EXTENSION: true
            },
            "Body": {
                "type": "object",
                "properties": { "secret_key": request_property, "name": {"type": "string"} }
            }
        });
        if let (Some(base), Some(extra)) = (schemas.as_object_mut(), extra_schemas.as_object()) {
            base.extend(extra.clone());
        }
        serde_json::json!({
            "paths": { "/things": { "post": {
                "operationId": "create_thing",
                "requestBody": { "content": { "application/json": {
                    "schema": { "$ref": "#/components/schemas/Body" }
                } } }
            } } },
            "components": { "schemas": schemas }
        })
    }

    #[test]
    fn a_plain_string_credential_is_reported_with_its_operation() {
        let found =
            unmarked_secret_fields(&doc(serde_json::json!({"type": "string"}), Value::Null));
        assert_eq!(
            found.into_iter().collect::<Vec<_>>(),
            vec!["create_thing:secret_key".to_string()]
        );
    }

    #[test]
    fn a_secret_string_reference_is_accepted() {
        let property = serde_json::json!({"$ref": "#/components/schemas/SecretString"});
        assert!(unmarked_secret_fields(&doc(property, Value::Null)).is_empty());
    }

    #[test]
    fn an_optional_secret_string_is_accepted() {
        let property = serde_json::json!({
            "oneOf": [{"type": "null"}, {"$ref": "#/components/schemas/SecretString"}]
        });
        assert!(unmarked_secret_fields(&doc(property, Value::Null)).is_empty());
    }

    #[test]
    fn names_that_only_describe_a_credential_are_not_reported() {
        for harmless in ["secret_id", "has_password", "token_count", "key_configured"] {
            assert!(
                !looks_like_a_credential(harmless),
                "{harmless} describes a credential, it does not carry one"
            );
        }
        assert!(looks_like_a_credential("secret_access_key"));
        assert!(looks_like_a_credential("Password"));
    }

    #[test]
    fn a_nested_credential_is_found_with_its_path() {
        let property = serde_json::json!({"type": "integer"});
        let nested = serde_json::json!({
            "Auth": {
                "type": "object",
                "properties": { "bearer_token": {"type": "string"} }
            }
        });
        let mut document = doc(property, nested);
        document["components"]["schemas"]["Body"]["properties"]["auth"] =
            serde_json::json!({"$ref": "#/components/schemas/Auth"});
        let found = unmarked_secret_fields(&document);
        assert!(
            found.contains("create_thing:auth.bearer_token"),
            "{found:?}"
        );
    }
}
