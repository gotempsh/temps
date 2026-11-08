// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Secret references: how the assistant uses a credential it never sees.
//!
//! The model cannot be trusted with a credential (it would land in the provider's
//! logs and in the conversation history), so it never holds one. When an
//! operation needs a secret the user types it into a form the console renders;
//! the server stores it sealed and hands the model an opaque handle
//! (`secret_ref:v1:<id>`). The model passes the handle where the value belongs and
//! this module swaps the real value in just before the request is replayed
//! through the router, then scrubs the value from whatever comes back.
//!
//! # What makes this safe
//!
//! * A handle is only honoured in a request-body field the API itself declared
//!   to be a credential (`x-temps-secret`, set by `SecretString`). A handle in
//!   any other field, in a path or in a query string, is rejected rather than
//!   passed through, so a handle can never be steered into a field that echoes
//!   it back or into a place the handler logs.
//! * Resolution is delegated to a [`SecretResolver`] that the owner of the
//!   conversation constructs, already bound to the user and conversation. The
//!   resolver decides whether *this* operation and field may use *this* handle;
//!   this module only asks.
//! * The resolved value is scrubbed from the response and from error text before
//!   the model sees either.

use async_trait::async_trait;
use serde_json::Value;
use temps_core::secret_string::{SecretString, SECRET_MASK, SECRET_SCHEMA_EXTENSION};
use thiserror::Error;

use crate::error::ApiToolError;

/// Every handle starts with this. The version lets the format change without
/// old handles being misread.
pub const SECRET_REF_PREFIX: &str = "secret_ref:v1:";

/// How far a schema may nest before the walk gives up. Request bodies are
/// shallow; the bound only exists so a self-referential schema cannot loop.
const MAX_SCHEMA_DEPTH: usize = 10;

/// One step from the root of a request body to a credential field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretPathSegment {
    /// An object property.
    Key(String),
    /// Every element of an array.
    Items,
    /// Every value of a free-form map (`additionalProperties`).
    AnyKey,
}

/// Where in a request body a credential goes.
pub type SecretPath = Vec<SecretPathSegment>;

/// What a handle is being used for, so the resolver can refuse a mismatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretUse<'a> {
    pub operation_id: &'a str,
    /// JSON pointer of the body field, e.g. `/secret_key`.
    pub field: String,
}

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum SecretResolveError {
    #[error("secret handle '{handle}' does not exist")]
    Unknown { handle: String },

    #[error("secret handle '{handle}' has expired; ask the user to provide it again")]
    Expired { handle: String },

    #[error("secret handle '{handle}' was already used")]
    Consumed { handle: String },

    #[error("secret handle '{handle}' cannot be used for {operation_id} {field}: {reason}")]
    NotPermitted {
        handle: String,
        operation_id: String,
        field: String,
        reason: String,
    },

    #[error("secret handle '{handle}' could not be read: {reason}")]
    Storage { handle: String, reason: String },
}

/// Turns a handle into the value the user provided.
///
/// Implementations are bound to one user and conversation when constructed, and
/// must refuse a handle that belongs to anyone else.
#[async_trait]
pub trait SecretResolver: Send + Sync + std::fmt::Debug {
    async fn resolve(
        &self,
        handle: &str,
        usage: &SecretUse<'_>,
    ) -> Result<SecretString, SecretResolveError>;
}

/// Whether `value` is, in its entirety, a well-formed handle.
pub fn is_secret_ref(value: &str) -> bool {
    value.strip_prefix(SECRET_REF_PREFIX).is_some_and(|id| {
        !id.is_empty()
            && id.len() <= 64
            && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    })
}

// ---------------------------------------------------------------------------
// Finding the credential fields of an operation
// ---------------------------------------------------------------------------

/// The JSON paths of every credential field in a request-body schema.
///
/// `schema` is the (possibly `$ref`) schema of the body; `components` is the
/// document's `components` object, used to follow references.
pub fn collect_secret_paths(schema: &Value, components: &Value) -> Vec<SecretPath> {
    let mut found = Vec::new();
    let mut trail = Vec::new();
    walk_schema(schema, components, &mut Vec::new(), &mut trail, &mut found);
    found.sort_by_key(|path| format!("{path:?}"));
    found.dedup();
    found
}

fn walk_schema(
    schema: &Value,
    components: &Value,
    path: &mut SecretPath,
    trail: &mut Vec<String>,
    found: &mut Vec<SecretPath>,
) {
    if path.len() > MAX_SCHEMA_DEPTH || trail.len() > MAX_SCHEMA_DEPTH * 4 {
        return;
    }

    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        // A schema that refers back to one already being expanded would loop.
        if trail.iter().any(|seen| seen == reference) {
            return;
        }
        if let Some(target) = lookup_reference(reference, components) {
            trail.push(reference.to_string());
            walk_schema(target, components, path, trail, found);
            trail.pop();
        }
        return;
    }

    if schema.get(SECRET_SCHEMA_EXTENSION).and_then(Value::as_bool) == Some(true) {
        found.push(path.clone());
        return;
    }

    for combinator in ["allOf", "oneOf", "anyOf"] {
        if let Some(members) = schema.get(combinator).and_then(Value::as_array) {
            for member in members {
                walk_schema(member, components, path, trail, found);
            }
        }
    }

    if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
        for (name, property) in properties {
            path.push(SecretPathSegment::Key(name.clone()));
            walk_schema(property, components, path, trail, found);
            path.pop();
        }
    }

    if let Some(items) = schema.get("items") {
        path.push(SecretPathSegment::Items);
        walk_schema(items, components, path, trail, found);
        path.pop();
    }

    if let Some(additional) = schema.get("additionalProperties") {
        if additional.is_object() {
            path.push(SecretPathSegment::AnyKey);
            walk_schema(additional, components, path, trail, found);
            path.pop();
        }
    }
}

fn lookup_reference<'a>(reference: &str, components: &'a Value) -> Option<&'a Value> {
    let rest = reference.strip_prefix("#/components/")?;
    let (section, name) = rest.split_once('/')?;
    components.get(section)?.get(name)
}

/// The credential paths for one operation's `requestBody`, as serialized in the
/// OpenAPI document. Empty when the operation has no body or no credentials.
pub fn secret_paths_for_request_body(request_body: &Value, components: &Value) -> Vec<SecretPath> {
    let body = match request_body.get("$ref") {
        Some(_) => {
            let mut resolved = None;
            if let Some(reference) = request_body.get("$ref").and_then(Value::as_str) {
                resolved = lookup_reference(reference, components);
            }
            match resolved {
                Some(body) => body,
                None => return Vec::new(),
            }
        }
        None => request_body,
    };

    let content = match body.get("content") {
        Some(content) => content,
        None => return Vec::new(),
    };
    let media = content
        .get("application/json")
        .or_else(|| content.as_object().and_then(|map| map.values().next()));
    match media.and_then(|media| media.get("schema")) {
        Some(schema) => collect_secret_paths(schema, components),
        None => Vec::new(),
    }
}

/// The credential paths as JSON pointers, with `*` standing for an array index
/// or map key. For telling the model which fields of an operation take a secret.
pub fn describe_credential_paths(paths: &[SecretPath]) -> Vec<String> {
    paths
        .iter()
        .map(|path| {
            path.iter()
                .map(|segment| match segment {
                    SecretPathSegment::Key(name) => format!("/{}", pointer_token(name)),
                    SecretPathSegment::Items | SecretPathSegment::AnyKey => "/*".to_string(),
                })
                .collect::<String>()
        })
        .collect()
}

/// Whether `pointer` (a JSON pointer such as `/extra/0/token`) names a
/// credential field under `paths`. Array indices and free-form map keys match
/// any value, mirroring how [`substitute`] walks a body.
pub fn pointer_is_credential(pointer: &str, paths: &[SecretPath]) -> bool {
    let tokens: Vec<String> = pointer
        .split('/')
        .skip(1)
        .map(|t| t.replace("~1", "/").replace("~0", "~"))
        .collect();
    if pointer.is_empty() || !pointer.starts_with('/') {
        return false;
    }
    paths.iter().any(|path| {
        path.len() == tokens.len()
            && path
                .iter()
                .zip(&tokens)
                .all(|(segment, token)| match segment {
                    SecretPathSegment::Key(name) => name == token,
                    SecretPathSegment::Items => token.parse::<usize>().is_ok(),
                    SecretPathSegment::AnyKey => true,
                })
    })
}

// ---------------------------------------------------------------------------
// Substituting handles into a request body
// ---------------------------------------------------------------------------

fn pointer_token(raw: &str) -> String {
    raw.replace('~', "~0").replace('/', "~1")
}

/// Pointers to every body value that sits at a credential path.
fn credential_pointers(body: &Value, paths: &[SecretPath]) -> Vec<String> {
    fn visit(value: &Value, rest: &[SecretPathSegment], pointer: String, out: &mut Vec<String>) {
        let Some((head, tail)) = rest.split_first() else {
            out.push(pointer);
            return;
        };
        match (head, value) {
            (SecretPathSegment::Key(name), Value::Object(map)) => {
                if let Some(child) = map.get(name) {
                    visit(
                        child,
                        tail,
                        format!("{pointer}/{}", pointer_token(name)),
                        out,
                    );
                }
            }
            (SecretPathSegment::Items, Value::Array(items)) => {
                for (index, child) in items.iter().enumerate() {
                    visit(child, tail, format!("{pointer}/{index}"), out);
                }
            }
            (SecretPathSegment::AnyKey, Value::Object(map)) => {
                for (name, child) in map {
                    visit(
                        child,
                        tail,
                        format!("{pointer}/{}", pointer_token(name)),
                        out,
                    );
                }
            }
            _ => {}
        }
    }

    let mut pointers = Vec::new();
    for path in paths {
        visit(body, path, String::new(), &mut pointers);
    }
    pointers
}

/// The pointer of the first string anywhere in `value` that mentions a handle
/// but is not in `allowed`.
fn first_misplaced_reference(value: &Value, pointer: &str, allowed: &[String]) -> Option<String> {
    match value {
        Value::String(text) => {
            if text.contains(SECRET_REF_PREFIX) && !allowed.iter().any(|a| a == pointer) {
                Some(pointer.to_string())
            } else {
                None
            }
        }
        Value::Array(items) => items.iter().enumerate().find_map(|(index, child)| {
            first_misplaced_reference(child, &format!("{pointer}/{index}"), allowed)
        }),
        Value::Object(map) => map.iter().find_map(|(name, child)| {
            if name.contains(SECRET_REF_PREFIX) {
                return Some(format!("{pointer}/{}", pointer_token(name)));
            }
            first_misplaced_reference(
                child,
                &format!("{pointer}/{}", pointer_token(name)),
                allowed,
            )
        }),
        _ => None,
    }
}

/// Reject a handle in a URL; handles belong in credential body fields only.
pub(crate) fn reject_references_in_url(
    operation_id: &str,
    path: &str,
    query: &str,
) -> Result<(), ApiToolError> {
    for (location, text) in [("path", path), ("query string", query)] {
        if text.contains(SECRET_REF_PREFIX) {
            return Err(ApiToolError::SecretReferenceMisplaced {
                operation_id: operation_id.to_string(),
                location: location.to_string(),
            });
        }
    }
    Ok(())
}

/// Replace every handle at a credential path with the user's value.
///
/// Returns the values that were substituted, so the caller can scrub them from
/// the response. A handle anywhere else is an error, and so is a handle when no
/// `resolver` is configured: passing the literal `secret_ref:…` string through
/// would store it as if it were the credential.
pub(crate) async fn substitute(
    operation_id: &str,
    body: &mut Value,
    paths: &[SecretPath],
    resolver: Option<&dyn SecretResolver>,
) -> Result<Vec<SecretString>, ApiToolError> {
    let credential_slots = credential_pointers(body, paths);

    if let Some(location) = first_misplaced_reference(body, "", &credential_slots) {
        return Err(ApiToolError::SecretReferenceMisplaced {
            operation_id: operation_id.to_string(),
            location: format!("body field '{location}' (not a credential field)"),
        });
    }

    let mut used = Vec::new();
    for pointer in credential_slots {
        let handle = match body.pointer(&pointer) {
            Some(Value::String(text)) if text.contains(SECRET_REF_PREFIX) => text.clone(),
            Some(Value::String(text)) if !text.is_empty() => {
                return Err(ApiToolError::SecretLiteralRejected {
                    operation_id: operation_id.to_string(),
                    field: pointer,
                });
            }
            _ => continue,
        };
        if !is_secret_ref(&handle) {
            return Err(ApiToolError::SecretReferenceMisplaced {
                operation_id: operation_id.to_string(),
                location: format!(
                    "body field '{pointer}' (a handle must be the whole value, nothing around it)"
                ),
            });
        }
        let Some(resolver) = resolver else {
            return Err(ApiToolError::SecretsUnavailable {
                operation_id: operation_id.to_string(),
                field: pointer,
            });
        };
        let usage = SecretUse {
            operation_id,
            field: pointer.clone(),
        };
        let secret = resolver.resolve(&handle, &usage).await.map_err(|e| {
            ApiToolError::SecretUnresolved {
                operation_id: operation_id.to_string(),
                field: pointer.clone(),
                reason: e.to_string(),
            }
        })?;
        if let Some(slot) = body.pointer_mut(&pointer) {
            *slot = Value::String(secret.expose().to_string());
        }
        used.push(secret);
    }
    Ok(used)
}

// ---------------------------------------------------------------------------
// Scrubbing the response
// ---------------------------------------------------------------------------

/// Replace every occurrence of a substituted value in `value` with the mask.
pub(crate) fn scrub(value: &mut Value, secrets: &[SecretString]) {
    if secrets.iter().all(SecretString::is_empty) {
        return;
    }
    match value {
        Value::String(text) => {
            for secret in secrets.iter().filter(|s| !s.is_empty()) {
                if text.contains(secret.expose()) {
                    *text = text.replace(secret.expose(), SECRET_MASK);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|item| scrub(item, secrets)),
        Value::Object(map) => {
            let original = std::mem::take(map);
            for (key, mut item) in original {
                let mut masked = Value::String(key);
                scrub(&mut masked, secrets);
                let Value::String(mut key) = masked else {
                    continue;
                };
                scrub(&mut item, secrets);
                // Preserve every value when masking makes distinct keys equal.
                let base = key.clone();
                let mut suffix = 2;
                while map.contains_key(&key) {
                    key = format!("{base} ({suffix})");
                    suffix += 1;
                }
                map.insert(key, item);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;

    const VALUE: &str = "wJalr-example-not-a-real-secret";
    const HANDLE: &str = "secret_ref:v1:0123456789abcdef0123456789abcdef";

    fn components() -> Value {
        json!({
            "schemas": {
                "SecretString": {"type": "string", "format": "password", "x-temps-secret": true},
                "CreateSource": {
                    "type": "object",
                    "properties": {
                        "name": {"type": "string"},
                        "access_key_id": {"type": "string"},
                        "secret_key": {"$ref": "#/components/schemas/SecretString"},
                        "extra": {
                            "type": "array",
                            "items": {"$ref": "#/components/schemas/Extra"}
                        }
                    }
                },
                "Extra": {
                    "type": "object",
                    "properties": {
                        "token": {
                            "oneOf": [
                                {"type": "null"},
                                {"$ref": "#/components/schemas/SecretString"}
                            ]
                        }
                    }
                },
                "Loop": {
                    "type": "object",
                    "properties": {"again": {"$ref": "#/components/schemas/Loop"}}
                }
            },
            "requestBodies": {
                "Shared": {
                    "content": {"application/json": {"schema": {"$ref": "#/components/schemas/CreateSource"}}}
                }
            }
        })
    }

    #[derive(Debug, Default)]
    struct FakeResolver {
        values: HashMap<String, String>,
        refuse_field: Option<String>,
    }

    #[async_trait]
    impl SecretResolver for FakeResolver {
        async fn resolve(
            &self,
            handle: &str,
            usage: &SecretUse<'_>,
        ) -> Result<SecretString, SecretResolveError> {
            if self.refuse_field.as_deref() == Some(usage.field.as_str()) {
                return Err(SecretResolveError::NotPermitted {
                    handle: handle.to_string(),
                    operation_id: usage.operation_id.to_string(),
                    field: usage.field.clone(),
                    reason: "bound to another field".into(),
                });
            }
            self.values
                .get(handle)
                .map(|v| SecretString::new(v.clone()))
                .ok_or_else(|| SecretResolveError::Unknown {
                    handle: handle.to_string(),
                })
        }
    }

    fn resolver() -> FakeResolver {
        FakeResolver {
            values: HashMap::from([(HANDLE.to_string(), VALUE.to_string())]),
            refuse_field: None,
        }
    }

    fn source_paths() -> Vec<SecretPath> {
        collect_secret_paths(
            &json!({"$ref": "#/components/schemas/CreateSource"}),
            &components(),
        )
    }

    #[test]
    fn scrub_masks_object_keys_without_losing_colliding_values() {
        let mut value = json!({"credential-one":1,"credential-two":2,"***":3,"nested":{"credential-one":"credential-two"}});
        scrub(
            &mut value,
            &[
                SecretString::new("credential-one"),
                SecretString::new("credential-two"),
            ],
        );
        assert!(!value.to_string().contains("credential-"));
        let values: Vec<_> = value
            .as_object()
            .unwrap()
            .values()
            .filter_map(Value::as_i64)
            .collect();
        assert_eq!(values.len(), 3);
        assert!(values.contains(&1) && values.contains(&2) && values.contains(&3));
        assert_eq!(value["nested"]["***"], "***");
    }

    #[test]
    fn finds_flagged_fields_through_refs_arrays_and_nullable_unions() {
        let paths = source_paths();
        assert!(paths.contains(&vec![SecretPathSegment::Key("secret_key".into())]));
        assert!(paths.contains(&vec![
            SecretPathSegment::Key("extra".into()),
            SecretPathSegment::Items,
            SecretPathSegment::Key("token".into()),
        ]));
        assert_eq!(
            paths.len(),
            2,
            "unflagged fields are not credentials: {paths:?}"
        );
    }

    #[test]
    fn a_self_referential_schema_terminates() {
        let paths =
            collect_secret_paths(&json!({"$ref": "#/components/schemas/Loop"}), &components());
        assert!(paths.is_empty());
    }

    #[test]
    fn a_request_body_reference_is_followed() {
        let paths = secret_paths_for_request_body(
            &json!({"$ref": "#/components/requestBodies/Shared"}),
            &components(),
        );
        assert_eq!(paths.len(), 2);
    }

    #[test]
    fn an_operation_without_a_body_has_no_credential_fields() {
        assert!(secret_paths_for_request_body(&json!({}), &components()).is_empty());
    }

    #[tokio::test]
    async fn swaps_a_handle_for_the_value_at_a_credential_field() {
        let mut body = json!({"name": "n", "secret_key": HANDLE});
        let used = substitute(
            "create_s3_source",
            &mut body,
            &source_paths(),
            Some(&resolver()),
        )
        .await
        .expect("a handle at a credential field resolves");
        assert_eq!(body["secret_key"], VALUE);
        assert_eq!(body["name"], "n");
        assert_eq!(used.len(), 1);
        assert_eq!(used[0].expose(), VALUE);
    }

    #[tokio::test]
    async fn swaps_a_handle_nested_in_an_array() {
        let mut body = json!({"extra": [{"token": HANDLE}, {"token": null}]});
        substitute("op", &mut body, &source_paths(), Some(&resolver()))
            .await
            .expect("resolves");
        assert_eq!(body["extra"][0]["token"], VALUE);
        assert!(body["extra"][1]["token"].is_null());
    }

    #[tokio::test]
    async fn a_literal_in_a_credential_field_is_rejected_without_echoing_it() {
        let mut body = json!({"secret_key": "typed-directly"});
        let err = substitute("op", &mut body, &source_paths(), Some(&resolver()))
            .await
            .expect_err("a credential must arrive as a handle, never as text");
        assert!(
            matches!(err, ApiToolError::SecretLiteralRejected { ref field, .. } if field == "/secret_key"),
            "{err:?}"
        );
        assert!(!err.to_string().contains("typed-directly"));
        assert!(err.to_string().contains("request_secret"));
    }

    #[tokio::test]
    async fn a_literal_is_rejected_even_when_no_resolver_exists() {
        let mut body = json!({"secret_key": "typed-directly"});
        let err = substitute("op", &mut body, &source_paths(), None)
            .await
            .expect_err("no resolver is no excuse to pass a literal through");
        assert!(
            matches!(err, ApiToolError::SecretLiteralRejected { .. }),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn an_empty_or_absent_credential_field_is_not_a_literal() {
        let mut body = json!({"name": "bucket"});
        substitute("op", &mut body, &source_paths(), Some(&resolver()))
            .await
            .expect("absent field is fine");
        let mut body = json!({"secret_key": ""});
        substitute("op", &mut body, &source_paths(), Some(&resolver()))
            .await
            .expect("an empty value carries nothing to protect");
    }

    #[tokio::test]
    async fn a_handle_in_an_ordinary_field_is_rejected_not_passed_through() {
        let mut body = json!({"name": HANDLE, "secret_key": "x"});
        let err = substitute("op", &mut body, &source_paths(), Some(&resolver()))
            .await
            .expect_err("a handle in `name` could be echoed back");
        assert!(
            matches!(err, ApiToolError::SecretReferenceMisplaced { .. }),
            "{err:?}"
        );
        assert_eq!(body["name"], HANDLE, "nothing was substituted");
    }

    #[tokio::test]
    async fn a_handle_used_as_an_object_key_is_rejected() {
        let mut body = json!({ HANDLE: "x" });
        let err = substitute("op", &mut body, &source_paths(), Some(&resolver()))
            .await
            .expect_err("keys are not credential slots");
        assert!(
            matches!(err, ApiToolError::SecretReferenceMisplaced { .. }),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn a_handle_with_text_around_it_is_rejected() {
        let mut body = json!({"secret_key": format!("prefix-{HANDLE}")});
        let err = substitute("op", &mut body, &source_paths(), Some(&resolver()))
            .await
            .expect_err("only a whole-value handle is substituted");
        assert!(
            matches!(err, ApiToolError::SecretReferenceMisplaced { .. }),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn a_handle_without_a_resolver_is_refused_rather_than_stored_literally() {
        let mut body = json!({"secret_key": HANDLE});
        let err = substitute("op", &mut body, &source_paths(), None)
            .await
            .expect_err("no resolver, no substitution");
        assert!(
            matches!(err, ApiToolError::SecretsUnavailable { .. }),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn an_unknown_handle_and_a_refusal_both_fail_without_leaking() {
        let mut body = json!({"secret_key": "secret_ref:v1:nope"});
        let err = substitute("op", &mut body, &source_paths(), Some(&resolver()))
            .await
            .expect_err("unknown handle");
        assert!(
            matches!(err, ApiToolError::SecretUnresolved { .. }),
            "{err:?}"
        );

        let refusing = FakeResolver {
            refuse_field: Some("/secret_key".into()),
            ..resolver()
        };
        let mut body = json!({"secret_key": HANDLE});
        let err = substitute("op", &mut body, &source_paths(), Some(&refusing))
            .await
            .expect_err("resolver refuses");
        assert!(err.to_string().contains("bound to another field"), "{err}");
        assert!(!err.to_string().contains(VALUE));
    }

    #[test]
    fn handles_in_a_url_are_rejected() {
        assert!(reject_references_in_url("op", "/a/b", "x=1").is_ok());
        assert!(reject_references_in_url("op", &format!("/a/{HANDLE}"), "").is_err());
        assert!(reject_references_in_url("op", "/a", &format!("k={HANDLE}")).is_err());
    }

    #[test]
    fn a_pointer_is_a_credential_only_where_the_spec_says_so() {
        let paths = source_paths();
        assert!(pointer_is_credential("/secret_key", &paths));
        assert!(pointer_is_credential("/extra/3/token", &paths));
        assert!(!pointer_is_credential("/name", &paths));
        assert!(!pointer_is_credential("/extra/x/token", &paths));
        assert!(!pointer_is_credential("/secret_key/deeper", &paths));
        assert!(!pointer_is_credential("", &paths));
        assert!(!pointer_is_credential("secret_key", &paths));
    }

    #[test]
    fn credential_paths_render_as_pointers_for_the_model() {
        let mut rendered = describe_credential_paths(&source_paths());
        rendered.sort();
        assert_eq!(rendered, vec!["/extra/*/token", "/secret_key"]);
    }

    #[test]
    fn recognises_only_whole_well_formed_handles() {
        assert!(is_secret_ref(HANDLE));
        assert!(!is_secret_ref("secret_ref:v1:"));
        assert!(!is_secret_ref("secret_ref:v2:abc"));
        assert!(!is_secret_ref(&format!("{HANDLE} ")));
        assert!(!is_secret_ref(&format!("x{HANDLE}")));
    }

    #[test]
    fn scrub_masks_the_value_wherever_the_response_repeats_it() {
        let mut response = json!({
            "echo": VALUE,
            "nested": [{"msg": format!("bad credentials {VALUE} rejected")}],
            "n": 3
        });
        scrub(&mut response, &[SecretString::new(VALUE)]);
        let rendered = response.to_string();
        assert!(!rendered.contains(VALUE), "{rendered}");
        assert_eq!(response["echo"], SECRET_MASK);
        assert_eq!(response["n"], 3);
    }

    #[test]
    fn scrub_ignores_empty_secrets_instead_of_masking_everything() {
        let mut response = json!({"a": "keep"});
        scrub(&mut response, &[SecretString::new("")]);
        assert_eq!(response["a"], "keep");
    }
}
