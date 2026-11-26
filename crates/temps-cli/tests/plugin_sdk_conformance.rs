// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The plugin SDK's endpoint table must describe endpoints that exist.
//!
//! `temps-plugin-sdk` declares the platform endpoints it calls as a static
//! table (`DECLARED_ENDPOINTS`) rather than generating a client from the
//! OpenAPI document — generating one would be a cycle, since the document is
//! itself produced from the handlers by utoipa.
//!
//! That hand-written table is only safe if something proves it still matches
//! the API. This is that something. If a route is renamed, moved or changes
//! verb, this test names the endpoint and fails the build; without it the
//! first sign would be a 404 during a real deploy, on an operator's machine,
//! with nothing in the logs but a status code.
//!
//! It lives in `temps-cli` because that is the crate that can see every
//! domain's `ApiDoc`. The SDK cannot depend on them — it is compiled into
//! every plugin binary and must stay small.

use std::collections::BTreeSet;

use temps_plugin_sdk::api::DECLARED_ENDPOINTS;
use utoipa::OpenApi;

/// Every operation the platform exposes, as `(METHOD, path)`.
///
/// Assembled from the domain `ApiDoc`s that own the routes the SDK declares.
/// This is deliberately *not* the full unified document — that one is built
/// from a live `PluginManager` and needs a database. Missing a doc here would
/// show up as a false failure naming the exact endpoint, which is a good
/// failure mode: it points at the doc that needs adding.
fn merged_docs() -> utoipa::openapi::OpenApi {
    let mut docs = <temps_projects::handlers::ApiDoc as OpenApi>::openapi();
    docs.merge(<temps_environments::handlers::ApiDoc as OpenApi>::openapi());
    docs.merge(<temps_deployments::handlers::deployments::DeploymentsApiDoc as OpenApi>::openapi());
    docs.merge(
        <temps_deployments::handlers::remote_deployments::RemoteDeploymentsApiDoc as OpenApi>::openapi(),
    );
    docs.merge(<temps_providers::handlers::handlers::ExternalServiceApiDoc as OpenApi>::openapi());
    docs
}

fn platform_operations() -> BTreeSet<(String, String)> {
    let docs = merged_docs();

    // `PathItem` exposes one `Option<Operation>` field per verb rather than a
    // map, so the verbs the SDK can express are listed explicitly. A verb the
    // channel cannot carry is not worth collecting.
    let mut operations = BTreeSet::new();
    for (path, item) in docs.paths.paths.iter() {
        for (method, operation) in [
            ("GET", &item.get),
            ("POST", &item.post),
            ("PUT", &item.put),
            ("PATCH", &item.patch),
            ("DELETE", &item.delete),
        ] {
            if operation.is_some() {
                operations.insert((method.to_string(), path.clone()));
            }
        }
    }
    operations
}

/// Path parameter *names* are free to differ between the spec and the SDK —
/// `{id}` and `{project_id}` address the same segment. The structure is what
/// a router matches on, so that is what is compared.
fn normalize(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut in_param = false;
    for c in path.chars() {
        match c {
            '{' => {
                in_param = true;
                out.push_str("{}");
            }
            '}' => in_param = false,
            _ if in_param => {}
            _ => out.push(c),
        }
    }
    out
}

#[test]
fn every_declared_sdk_endpoint_exists_on_the_platform() {
    let operations = platform_operations();
    assert!(
        !operations.is_empty(),
        "No operations were collected from the domain ApiDocs — the merge above is broken, \
         so this test would pass vacuously"
    );

    let available: BTreeSet<(String, String)> = operations
        .iter()
        .map(|(method, path)| (method.clone(), normalize(path)))
        .collect();

    let mut missing = Vec::new();
    for (method, path) in DECLARED_ENDPOINTS {
        let key = (method.to_uppercase(), normalize(path));
        if !available.contains(&key) {
            missing.push(format!("{} {path}", method.to_uppercase()));
        }
    }

    assert!(
        missing.is_empty(),
        "temps-plugin-sdk declares endpoints the platform does not have:\n  {}\n\n\
         Either the route moved (update `DECLARED_ENDPOINTS` and the matching method in \
         `temps-plugin-sdk/src/api.rs`) or its `ApiDoc` is not merged in this test's \
         `platform_operations()`.\n\nThe platform currently exposes:\n  {}",
        missing.join("\n  "),
        operations
            .iter()
            .map(|(m, p)| format!("{m} {p}"))
            .collect::<Vec<_>>()
            .join("\n  ")
    );
}

/// The check above must have teeth.
///
/// A comparison built on normalization can go wrong in a way that makes
/// everything match — a bug in `normalize`, an over-eager `contains` — and
/// then the conformance test passes forever while the SDK rots. This proves a
/// route that does not exist is actually detected.
#[test]
fn a_route_the_platform_does_not_have_is_detected() {
    let available: BTreeSet<(String, String)> = platform_operations()
        .iter()
        .map(|(method, path)| (method.clone(), normalize(path)))
        .collect();

    for bogus in [
        ("GET", "/projects/{id}/does-not-exist"),
        ("DELETE", "/projects"),
        // Right path, wrong verb.
        ("PATCH", "/projects/{id}"),
        // Right shape, one segment too few: the deployment ID is required.
        (
            "GET",
            "/projects/{project_id}/deployments/{deployment_id}/{extra}",
        ),
    ] {
        let key = (bogus.0.to_string(), normalize(bogus.1));
        assert!(
            !available.contains(&key),
            "{} {} is not a real platform route but the conformance check accepts it",
            bogus.0,
            bogus.1
        );
    }

    // ...and a route that *is* real must still be accepted, or the check
    // would be vacuously strict instead of vacuously loose.
    assert!(available.contains(&("POST".to_string(), normalize("/projects"))));
}

/// Every field the SDK reads must exist on the schema it claims to project.
///
/// Path conformance alone is not enough, and this test exists because of a
/// concrete near-miss: the deploy endpoints return `RemoteDeploymentResponse`
/// (`state`, `slug`) while polling one returns `DeploymentResponse`
/// (`status`, `url`). Both live under `/projects/...`, so a path check passed
/// while the SDK would have failed to decode the deploy response — on a real
/// deploy, in a background task, as "Failed to deserialize platform response".
#[test]
fn every_projected_field_exists_on_the_schema_it_projects() {
    let docs = merged_docs();
    let components = docs
        .components
        .as_ref()
        .expect("the merged document has no components section");

    let problems = projection_problems(components, temps_plugin_sdk::api::DECLARED_PROJECTIONS);
    assert!(
        problems.is_empty(),
        "temps-plugin-sdk projections no longer match the platform's schemas:\n  {}",
        problems.join("\n  ")
    );
}

/// A projection as `DECLARED_PROJECTIONS` lists it: the schema name and its
/// `(field, required, rust type)` triples.
type Projection<'a> = (&'a str, &'a [(&'a str, bool, &'a str)]);

/// Everything wrong with `projections` against the platform's schemas: a
/// field that is absent, required by the SDK but optional on the wire, or
/// decoded into a Rust type its wire format cannot fill.
fn projection_problems(
    components: &utoipa::openapi::Components,
    projections: &[Projection<'_>],
) -> Vec<String> {
    use utoipa::openapi::{RefOr, Schema};

    let mut problems = Vec::new();
    for (schema_name, fields) in projections {
        let Some(RefOr::T(Schema::Object(object))) = components.schemas.get(*schema_name) else {
            problems.push(format!(
                "{schema_name}: not an object schema in the platform's OpenAPI components"
            ));
            continue;
        };

        for (field, required, rust_type) in *fields {
            let Some(property) = object.properties.get(*field) else {
                problems.push(format!(
                    "{schema_name}.{field}: projected by the SDK but absent from the schema"
                ));
                continue;
            };
            // A non-`Option` Rust field cannot decode a response that omits
            // the key, so "present but optional" is a decode failure waiting
            // for the one response that leaves it out.
            if *required && !object.required.iter().any(|r| r == field) {
                problems.push(format!(
                    "{schema_name}.{field}: the SDK declares it required, but the schema does \
                     not — make the SDK field `Option<..>` or mark it `required = false`"
                ));
            }
            if let Some(problem) = wire_type_problem(components, property, rust_type) {
                problems.push(format!("{schema_name}.{field}: {problem}"));
            }
        }
    }
    problems
}

/// Why a field the SDK decodes into `rust_type` cannot take what the schema
/// `property` says the platform sends, or `None` when it can. The ISO 8601
/// timestamp migration is the case this exists for: a `created_at: i64`
/// exists on the schema and is required, yet fails on every response.
fn wire_type_problem(
    components: &utoipa::openapi::Components,
    property: &utoipa::openapi::RefOr<utoipa::openapi::Schema>,
    rust_type: &str,
) -> Option<String> {
    let rust_type: String = rust_type.chars().filter(|c| !c.is_whitespace()).collect();
    let inner = rust_type
        .strip_prefix("Option<")
        .and_then(|rest| rest.strip_suffix('>'))
        .unwrap_or(&rust_type);
    let (want_type, want_format) = match inner {
        "DateTime<Utc>" | "chrono::DateTime<chrono::Utc>" => ("string", Some("date-time")),
        "String" => ("string", None),
        "i32" | "i64" | "u32" | "u64" => ("integer", None),
        "f32" | "f64" => ("number", None),
        "bool" => ("boolean", None),
        other => {
            return Some(format!(
                "the SDK decodes it as `{other}`, which this test has no wire mapping for; \
                 add one to `wire_type_problem`"
            ))
        }
    };

    let mut types = BTreeSet::new();
    let mut formats = BTreeSet::new();
    let value = match serde_json::to_value(property) {
        Ok(value) => value,
        Err(error) => return Some(format!("its schema does not serialize to JSON: {error}")),
    };
    collect_wire_types(components, &value, 0, &mut types, &mut formats);

    if !types.contains(want_type) {
        return Some(format!(
            "the SDK decodes it as `{rust_type}`, but the platform sends {types:?}"
        ));
    }
    if let Some(format) = want_format {
        if !formats.contains(format) {
            return Some(format!(
                "the SDK decodes it as `{rust_type}`, which needs a `{format}` string, but the \
                 schema's formats are {formats:?}"
            ));
        }
    }
    None
}

/// Gathers the JSON types and formats a schema allows, through nullable
/// type arrays, `oneOf`/`anyOf`/`allOf` and component `$ref`s.
fn collect_wire_types(
    components: &utoipa::openapi::Components,
    schema: &serde_json::Value,
    depth: usize,
    types: &mut BTreeSet<String>,
    formats: &mut BTreeSet<String>,
) {
    if depth > 8 {
        return;
    }
    match schema.get("type") {
        Some(serde_json::Value::String(t)) => {
            types.insert(t.clone());
        }
        Some(serde_json::Value::Array(ts)) => {
            types.extend(ts.iter().filter_map(|t| t.as_str()).map(str::to_string));
        }
        _ => {}
    }
    if let Some(format) = schema.get("format").and_then(|f| f.as_str()) {
        formats.insert(format.to_string());
    }
    for key in ["oneOf", "anyOf", "allOf"] {
        for variant in schema
            .get(key)
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
        {
            collect_wire_types(components, variant, depth + 1, types, formats);
        }
    }
    if let Some(name) = schema
        .get("$ref")
        .and_then(|r| r.as_str())
        .and_then(|r| r.strip_prefix("#/components/schemas/"))
    {
        // A target that cannot be read adds no types, so the field is
        // reported as mismatched rather than passing unchecked.
        if let Some(Ok(target)) = components.schemas.get(name).map(serde_json::to_value) {
            collect_wire_types(components, &target, depth + 1, types, formats);
        }
    }
}

/// The wire-type check must catch the regression it was added for: decoding
/// `ProjectResponse.created_at`, now an ISO 8601 string, into an `i64`.
#[test]
fn the_projection_check_rejects_a_timestamp_decoded_as_a_number() {
    let docs = merged_docs();
    let components = docs.components.as_ref().expect("no components section");

    let wrong: &[Projection<'_>] = &[(
        "ProjectResponse",
        &[
            ("created_at", true, "i64"),
            ("last_deployment", false, "Option<i64>"),
        ],
    )];
    let problems = projection_problems(components, wrong);
    assert_eq!(
        problems.len(),
        2,
        "expected both timestamps flagged: {problems:#?}"
    );
    assert!(
        problems[0].starts_with("ProjectResponse.created_at:"),
        "{problems:#?}"
    );

    // ...and the right types pass, so the check is not vacuously strict.
    let right: &[Projection<'_>] = &[(
        "ProjectResponse",
        &[
            ("created_at", true, "DateTime<Utc>"),
            ("last_deployment", false, "Option<DateTime<Utc>>"),
            ("slug", true, "String"),
            ("id", true, "i32"),
        ],
    )];
    assert_eq!(projection_problems(components, right), Vec::<String>::new());
}

/// The projection check must have teeth: if schema lookup silently returned
/// nothing, the loop above would pass while checking nothing at all.
#[test]
fn the_projection_check_reads_real_schemas() {
    use utoipa::openapi::{RefOr, Schema};

    let docs = merged_docs();
    let components = docs.components.as_ref().expect("no components section");
    let Some(RefOr::T(Schema::Object(project))) = components.schemas.get("ProjectResponse") else {
        panic!("ProjectResponse is not an object schema — the check above would be vacuous");
    };
    assert!(
        project.properties.contains_key("slug"),
        "ProjectResponse has no properties the test can see"
    );
    assert!(
        !project.properties.contains_key("definitely_not_a_field"),
        "the schema claims to contain a field that does not exist"
    );
}

/// Parameter renaming is allowed, but the *number* of path parameters is not:
/// a segment gained or lost means the SDK is building a URL the router will
/// never match, which `normalize` alone would not catch if both sides changed.
#[test]
fn declared_paths_have_the_same_segment_count_as_the_platform() {
    let operations = platform_operations();
    for (method, path) in DECLARED_ENDPOINTS {
        let normalized = normalize(path);
        let matched = operations
            .iter()
            .find(|(m, p)| m == &method.to_uppercase() && normalize(p) == normalized);
        let Some((_, spec_path)) = matched else {
            continue; // reported by the test above
        };
        assert_eq!(
            path.matches('{').count(),
            spec_path.matches('{').count(),
            "{method} {path} has a different parameter count than the platform's {spec_path}"
        );
    }
}
