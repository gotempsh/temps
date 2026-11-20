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

/// How deep the free-form check follows a request body. Deeper than the name
/// check because reference hops and `oneOf` arms count as levels; cycles are
/// cut by name, and a body deeper than this fails the check instead of being
/// accepted half-inspected.
const FREE_FORM_MAX_DEPTH: usize = 16;

/// Collects `(path, schema)` for every property reachable from a schema,
/// including the properties of map values (`field.*.name`).
struct Walk<'a> {
    spec: &'a Value,
    limit: usize,
    seen: Vec<String>,
    out: Vec<(String, &'a Value)>,
    /// `(path[], schema)` of every array element schema, kept apart from
    /// `out` (the name check reads properties only).
    elements: Vec<(String, &'a Value)>,
    /// Paths where the depth limit stopped the walk, so what lies below them
    /// was not inspected.
    truncated: Vec<String>,
}

impl<'a> Walk<'a> {
    fn new(spec: &'a Value, limit: usize) -> Self {
        Self {
            spec,
            limit,
            seen: Vec::new(),
            out: Vec::new(),
            elements: Vec::new(),
            truncated: Vec::new(),
        }
    }

    fn walk(&mut self, schema: &'a Value, prefix: &str, depth: usize) {
        if depth > self.limit {
            self.truncated
                .push(prefix.trim_end_matches('.').to_string());
            return;
        }
        if let Some((name, target)) = resolve(self.spec, schema) {
            if self.seen.iter().any(|s| s == name) {
                return;
            }
            self.seen.push(name.to_string());
            self.walk(target, prefix, depth + 1);
            self.seen.pop();
            return;
        }
        for key in ["allOf", "oneOf", "anyOf"] {
            for part in schema[key].as_array().into_iter().flatten() {
                self.walk(part, prefix, depth + 1);
            }
        }
        for (name, property) in schema["properties"].as_object().into_iter().flatten() {
            let path = format!("{prefix}{name}");
            self.out.push((path.clone(), property));
            self.walk(property, &format!("{path}."), depth + 1);
        }
        // `items: false` (a closed tuple) is not a schema; tuple members
        // (`prefixItems`, e.g. `[key, value]` pairs) are element schemas too.
        let element = format!("{}[]", prefix.trim_end_matches('.'));
        let tuple = schema["prefixItems"].as_array().into_iter().flatten();
        for items in schema
            .get("items")
            .filter(|v| v.is_object())
            .into_iter()
            .chain(tuple)
        {
            self.elements.push((element.clone(), items));
            self.walk(items, &format!("{prefix}[]."), depth + 1);
        }
        // A map of described objects is not free-form itself, but the fields
        // of its values still are fields the caller sends.
        if let Some(values) = schema.get("additionalProperties").filter(|v| v.is_object()) {
            self.walk(values, &format!("{prefix}*."), depth + 1);
        }
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
            let mut walk = Walk::new(spec, MAX_SCHEMA_DEPTH);
            walk.walk(body, "", 0);
            for (path, schema) in walk.out {
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

/// Whether `schema` is a map or value whose contents the spec does not
/// describe: an object with no `properties` whose `additionalProperties` is not
/// a flagged credential, or a schema with no type at all. A credential inside
/// one is invisible to the secret-slot layer, which only knows the paths the
/// spec flags, so the model would have to type it.
fn is_free_form(spec: &Value, schema: &Value, depth: usize) -> bool {
    // A schema too deep to resolve cannot be shown to hold no credential.
    if depth > FREE_FORM_MAX_DEPTH {
        return true;
    }
    // A named schema can be a bare map too; moving an inline map into
    // `components` must not hide it from this check.
    if schema.get("$ref").is_some() {
        return resolve(spec, schema)
            .is_some_and(|(_, target)| is_free_form(spec, target, depth + 1));
    }
    if ["allOf", "oneOf", "anyOf"]
        .iter()
        .any(|key| schema.get(*key).is_some())
    {
        return ["allOf", "oneOf", "anyOf"].iter().any(|key| {
            schema[*key].as_array().is_some_and(|parts| {
                parts.iter().any(|p| {
                    p.get("type") != Some(&Value::from("null")) && is_free_form(spec, p, depth + 1)
                })
            })
        });
    }
    let is_object = match schema.get("type") {
        Some(Value::String(t)) => t == "object",
        Some(Value::Array(types)) => types.iter().any(|t| t == "object"),
        None => {
            // `{}` (any JSON value) is as opaque as an untyped object.
            return schema.get("properties").is_none()
                && schema.get("items").is_none()
                && schema.get("enum").is_none()
                && schema.get("const").is_none();
        }
        _ => false,
    };
    if !is_object || schema.get("properties").is_some() {
        return false;
    }
    match schema.get("additionalProperties") {
        Some(values) if values.is_object() => {
            !is_flagged(spec, values, depth + 1) && !has_described_shape(spec, values, depth + 1)
        }
        _ => true,
    }
}

/// A map whose values are themselves objects with named properties is walked
/// like any other schema; only opaque values count as free-form.
fn has_described_shape(spec: &Value, schema: &Value, depth: usize) -> bool {
    if depth > MAX_SCHEMA_DEPTH {
        return false;
    }
    if let Some((_, target)) = resolve(spec, schema) {
        return has_described_shape(spec, target, depth + 1);
    }
    schema.get("properties").is_some()
}

/// `operation_id:field.path` of every free-form request field of the given
/// operations.
fn free_form_fields(spec: &Value, operations: &BTreeSet<String>) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let Some(paths) = spec["paths"].as_object() else {
        return found;
    };
    for item in paths.values().filter_map(Value::as_object) {
        for operation in item.values() {
            let Some(id) = operation["operationId"].as_str() else {
                continue;
            };
            if !operations.contains(id) {
                continue;
            }
            let Some(body) = operation.pointer("/requestBody/content/application~1json/schema")
            else {
                continue;
            };
            // The body itself can be the map (`walk` only reports properties).
            if is_free_form(spec, body, 0) {
                found.insert(format!("{id}:(body)"));
            }
            let mut walk = Walk::new(spec, FREE_FORM_MAX_DEPTH);
            walk.walk(body, "", 0);
            // An array's element can be the opaque map (`[{...}, {...}]`).
            for (path, schema) in walk.out.into_iter().chain(walk.elements) {
                if is_free_form(spec, schema, 0) {
                    found.insert(format!("{id}:{path}"));
                }
            }
            for path in walk.truncated {
                found.insert(format!("{id}:{path} (too deep to inspect)"));
            }
        }
    }
    found
}

/// Free-form request fields of operations the assistant can call, each
/// reviewed: it holds no credential, or the handler refuses one the assistant
/// sends. Adding an entry is the same security review as adding the operation.
const REVIEWED_FREE_FORM: &[(&str, &str)] = &[
    (
        "create_ingress_connector:values",
        "setup answers; the handler refuses a `secret` field from the assistant, which passes it in the flagged `secrets` map",
    ),
    (
        "create_service:parameters",
        "service settings; the handler refuses a sensitive parameter from the assistant and Temps generates the credentials",
    ),
    ("create_thread_artifact:payload", "artifact content the model writes itself"),
    ("update_environment_settings:target_labels", "placement labels"),
    ("deploy_from_image:metadata", "free-form deployment labels"),
    ("register_external_image:metadata", "free-form image labels"),
    ("send_workflow_event:payload", "application event payload the user's workflow consumes"),
    ("start_workflow_run:input", "application workflow input"),
    ("create_alert_rule:trigger_config", "thresholds and windows"),
    ("update_alert_rule:trigger_config", "thresholds and windows"),
    ("create_flag:default_value", "feature flag value"),
    ("update_flag:default_value", "feature flag value"),
    ("set_flag_environment:value", "feature flag value"),
    ("create_flag:variants.[].value", "feature flag variant value"),
    ("update_flag:variants.[].value", "feature flag variant value"),
    ("set_flag_rules:rules.[].serve.value", "feature flag value a rule serves"),
    (
        "evaluate_flag:attributes",
        "targeting attributes for a dry-run evaluation; nothing is stored",
    ),
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

    fn assistant_write_operations() -> BTreeSet<String> {
        super::super::ai_write_allowlist().into_iter().collect()
    }

    fn assistant_operations() -> BTreeSet<String> {
        super::super::ai_write_allowlist()
            .into_iter()
            .chain(super::super::ai_read_safe_posts())
            .collect()
    }

    /// Every credential operation the assistant may propose, with the fields
    /// the user types into the private form (the pointers the
    /// `add-a-provider-credential` playbook names). Each must be a secret path
    /// in the published spec, so a literal there is refused and only a
    /// `secret_ref:` handle is accepted; the other fields must not be, or the
    /// model could not fill them in.
    const CREDENTIAL_OPERATIONS: &[(&str, &[&str], &[&str])] = &[
        (
            "create_dns_provider",
            &[
                "/credentials/api_token",
                "/credentials/api_key",
                "/credentials/secret_access_key",
                "/credentials/session_token",
                "/credentials/private_key",
                "/credentials/client_secret",
            ],
            &["/name", "/credentials/type", "/credentials/account_id"],
        ),
        (
            "update_provider",
            &["/credentials/api_token", "/credentials/secret_access_key"],
            &["/name", "/credentials/access_key_id"],
        ),
        ("check_cloudflare_token", &["/token"], &["/zone"]),
        ("check_access_provider", &["/token"], &["/account_id"]),
        (
            "create_access_provider",
            &["/token"],
            &["/account_id", "/name"],
        ),
        (
            "create_delivery_profile",
            &["/bunny_api_key"],
            &["/name", "/bunny_pull_zone_id"],
        ),
        (
            "create_email_provider",
            &[
                "/ses_credentials/secret_access_key",
                "/scaleway_credentials/api_key",
                "/smtp_credentials/password",
            ],
            &["/name", "/ses_credentials/access_key_id"],
        ),
        (
            "update_email_provider",
            &[
                "/ses_credentials/secret_access_key",
                "/scaleway_credentials/api_key",
                "/smtp_credentials/password",
            ],
            &["/name"],
        ),
        (
            "create_slack_provider",
            &["/config/webhook_url"],
            &["/name"],
        ),
        (
            "update_slack_provider",
            &["/config/webhook_url"],
            &["/name"],
        ),
        (
            "create_notification_email_provider",
            &["/config/password"],
            &["/name"],
        ),
        (
            "update_notification_email_provider",
            &["/config/password"],
            &["/name"],
        ),
        (
            "create_cloudflare_provider",
            &["/config/api_token"],
            &["/name"],
        ),
        (
            "update_cloudflare_provider",
            &["/config/api_token"],
            &["/name"],
        ),
        (
            "create_webhook_provider",
            &[
                "/config/headers/Authorization",
                "/config/headers/X-Signature",
            ],
            &["/name", "/config/url"],
        ),
        (
            "update_webhook_provider",
            &["/config/headers/Authorization"],
            &["/name", "/config/url"],
        ),
        ("create_webhook", &["/secret"], &["/url"]),
        ("update_webhook", &["/secret"], &["/url"]),
        ("create_github_pat_provider", &["/token"], &["/name"]),
        ("create_gitlab_pat_provider", &["/token"], &["/name"]),
        ("create_gitea_pat_provider", &["/token"], &["/name"]),
        ("create_generic_provider", &["/token"], &["/name"]),
        (
            "create_bitbucket_provider",
            &["/auth/token", "/auth/password"],
            &["/name"],
        ),
        ("update_git_provider_credentials", &["/token"], &[]),
        // S3 seals its access key id too (DNS and email treat it as an
        // identifier); either way the secret half is sealed.
        (
            "create_s3_source",
            &["/secret_key", "/access_key_id"],
            &["/name", "/bucket_name"],
        ),
        (
            "create_ingress_connector",
            &["/secrets/client_secret"],
            &["/values/client_secret", "/provider"],
        ),
    ];

    fn operation<'a>(spec: &'a Value, operation_id: &str) -> Option<&'a Value> {
        spec.get("paths")?
            .as_object()?
            .values()
            .filter_map(Value::as_object)
            .flat_map(|methods| methods.values())
            .find(|op| op.get("operationId").and_then(Value::as_str) == Some(operation_id))
    }

    #[test]
    fn every_provider_credential_the_assistant_can_send_is_a_secret_path() {
        let spec = spec();
        let components = spec.get("components").cloned().unwrap_or(Value::Null);
        let writes = assistant_write_operations();
        for (operation_id, credentials, plain) in CREDENTIAL_OPERATIONS {
            assert!(
                writes.contains(*operation_id),
                "`{operation_id}` must be in the assistant's write list: a secret handle only \
                 resolves for a confirmed write"
            );
            let op = operation(&spec, operation_id)
                .unwrap_or_else(|| panic!("`{operation_id}` is not in the published spec"));
            let body = op
                .get("requestBody")
                .unwrap_or_else(|| panic!("`{operation_id}` has no request body"));
            let paths = temps_ai_api_tools::secret_paths_for_request_body(body, &components);
            for pointer in *credentials {
                assert!(
                    temps_ai_api_tools::pointer_is_credential(pointer, &paths),
                    "`{operation_id}` {pointer} must be a secret path, or a literal key could be \
                     typed into the chat"
                );
            }
            for pointer in *plain {
                assert!(
                    !temps_ai_api_tools::pointer_is_credential(pointer, &paths),
                    "`{operation_id}` {pointer} is not a credential; a handle there is refused"
                );
            }
        }
    }

    /// The name check above cannot see into a map: a free-form `config`,
    /// `values` or `headers` object let the assistant type a credential into
    /// an operation it could call (the generic notification provider config,
    /// a tunnel's OAuth client secret, a webhook's Authorization header).
    /// Every free-form field the assistant can reach must be reviewed.
    #[test]
    fn every_free_form_field_the_assistant_can_send_is_reviewed() {
        let found = free_form_fields(&spec(), &assistant_operations());
        let reviewed: BTreeSet<String> = REVIEWED_FREE_FORM
            .iter()
            .map(|(field, _)| field.to_string())
            .collect();
        let new: Vec<_> = found.difference(&reviewed).collect();
        assert!(
            new.is_empty(),
            "the assistant can send these free-form request fields, and a credential inside one \
             would pass through the chat:\n  {}\nType the credential as a `SecretString` field, \
             refuse it in the handler for assistant calls, or keep the operation out of the \
             assistant's lists. Then list the field in REVIEWED_FREE_FORM with the reason.",
            new.iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join("\n  ")
        );
        let stale: Vec<_> = reviewed.difference(&found).collect();
        assert!(
            stale.is_empty(),
            "no longer free-form or no longer reachable; delete from REVIEWED_FREE_FORM: {stale:?}"
        );
    }

    #[test]
    fn a_map_behind_a_reference_is_still_free_form() {
        let spec = serde_json::json!({
            "paths": {
                "/things": { "post": {
                    "operationId": "create_thing",
                    "requestBody": { "content": { "application/json": {
                        "schema": { "$ref": "#/components/schemas/CreateThing" }
                    } } }
                } },
                "/labels": { "post": {
                    "operationId": "set_labels",
                    "requestBody": { "content": { "application/json": {
                        "schema": { "$ref": "#/components/schemas/Labels" }
                    } } }
                } }
            },
            "components": { "schemas": {
                "CreateThing": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" },
                        "settings": { "$ref": "#/components/schemas/Labels" },
                        "sealed": { "$ref": "#/components/schemas/SealedMap" }
                    }
                },
                "Labels": {
                    "type": "object",
                    "additionalProperties": { "type": "string" }
                },
                "SealedMap": {
                    "type": "object",
                    "additionalProperties": { "$ref": "#/components/schemas/SecretString" }
                },
                "SecretString": { "type": "string", "x-temps-secret": true }
            } }
        });
        let operations: BTreeSet<String> = ["create_thing", "set_labels"]
            .into_iter()
            .map(String::from)
            .collect();
        let found = free_form_fields(&spec, &operations);
        assert_eq!(
            found,
            ["create_thing:settings", "set_labels:(body)"]
                .into_iter()
                .map(String::from)
                .collect::<BTreeSet<_>>()
        );
    }

    #[test]
    fn an_opaque_map_inside_a_map_of_objects_is_free_form() {
        let spec = serde_json::json!({
            "paths": { "/things": { "post": {
                "operationId": "create_thing",
                "requestBody": { "content": { "application/json": { "schema": {
                    "type": "object",
                    "properties": { "settings": {
                        "type": "object",
                        "additionalProperties": {
                            "type": "object",
                            "properties": {
                                "url": { "type": "string" },
                                "headers": {
                                    "type": "object",
                                    "additionalProperties": { "type": "string" }
                                }
                            }
                        }
                    } }
                } } } }
            } } }
        });
        let operations: BTreeSet<String> = ["create_thing".to_string()].into_iter().collect();
        assert_eq!(
            free_form_fields(&spec, &operations),
            ["create_thing:settings.*.headers".to_string()]
                .into_iter()
                .collect::<BTreeSet<_>>()
        );
    }

    #[test]
    fn an_array_of_opaque_maps_is_free_form_and_an_array_of_strings_is_not() {
        let spec = serde_json::json!({
            "paths": { "/things": { "post": {
                "operationId": "create_thing",
                "requestBody": { "content": { "application/json": { "schema": {
                    "type": "object",
                    "properties": {
                        "rules": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "additionalProperties": { "type": "string" }
                            }
                        },
                        "tags": { "type": "array", "items": { "type": "string" } },
                        "pairs": {
                            "type": "array",
                            "items": {
                                "type": "array",
                                "prefixItems": [{ "type": "string" }, { "type": "string" }],
                                "items": false
                            }
                        }
                    }
                } } } }
            } } }
        });
        let operations: BTreeSet<String> = ["create_thing".to_string()].into_iter().collect();
        assert_eq!(
            free_form_fields(&spec, &operations),
            ["create_thing:rules[]".to_string()]
                .into_iter()
                .collect::<BTreeSet<_>>()
        );
    }

    #[test]
    fn a_body_too_deep_to_inspect_fails_the_check() {
        let mut schemas = serde_json::Map::new();
        let hops = FREE_FORM_MAX_DEPTH + 2;
        for hop in 0..hops {
            schemas.insert(
                format!("Hop{hop}"),
                serde_json::json!({ "$ref": format!("#/components/schemas/Hop{}", hop + 1) }),
            );
        }
        schemas.insert(
            format!("Hop{hops}"),
            serde_json::json!({ "type": "object", "additionalProperties": { "type": "string" } }),
        );
        let spec = serde_json::json!({
            "paths": { "/things": { "post": {
                "operationId": "create_thing",
                "requestBody": { "content": { "application/json": {
                    "schema": { "$ref": "#/components/schemas/Hop0" }
                } } }
            } } },
            "components": { "schemas": schemas }
        });
        let operations: BTreeSet<String> = ["create_thing".to_string()].into_iter().collect();
        assert!(
            !free_form_fields(&spec, &operations).is_empty(),
            "a reference chain past the limit must be reported, not accepted"
        );
    }

    #[test]
    fn a_map_of_plain_strings_is_free_form_and_a_map_of_secrets_is_not() {
        let mut document = doc(serde_json::json!({"type": "integer"}), Value::Null);
        document["components"]["schemas"]["Body"]["properties"]["headers"] = serde_json::json!({
            "type": "object", "additionalProperties": {"type": "string"}
        });
        document["components"]["schemas"]["Body"]["properties"]["sealed"] = serde_json::json!({
            "type": "object",
            "additionalProperties": {"$ref": "#/components/schemas/SecretString"}
        });
        document["components"]["schemas"]["Body"]["properties"]["config"] = serde_json::json!({});
        let operations = BTreeSet::from(["create_thing".to_string()]);
        let found = free_form_fields(&document, &operations);
        assert!(found.contains("create_thing:headers"), "{found:?}");
        assert!(found.contains("create_thing:config"), "{found:?}");
        assert!(!found.contains("create_thing:sealed"), "{found:?}");
        assert!(!found.contains("create_thing:name"), "{found:?}");
        assert!(free_form_fields(&document, &BTreeSet::new()).is_empty());
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
