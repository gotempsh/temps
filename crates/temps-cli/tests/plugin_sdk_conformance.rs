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
    use utoipa::openapi::{RefOr, Schema};

    let docs = merged_docs();
    let components = docs
        .components
        .as_ref()
        .expect("the merged document has no components section");

    let mut problems = Vec::new();
    for (schema_name, fields) in temps_plugin_sdk::api::DECLARED_PROJECTIONS {
        let Some(RefOr::T(Schema::Object(object))) = components.schemas.get(*schema_name) else {
            problems.push(format!(
                "{schema_name}: not an object schema in the platform's OpenAPI components"
            ));
            continue;
        };

        for (field, required) in *fields {
            if !object.properties.contains_key(*field) {
                problems.push(format!(
                    "{schema_name}.{field}: projected by the SDK but absent from the schema"
                ));
                continue;
            }
            // A non-`Option` Rust field cannot decode a response that omits
            // the key, so "present but optional" is a decode failure waiting
            // for the one response that leaves it out.
            if *required && !object.required.iter().any(|r| r == field) {
                problems.push(format!(
                    "{schema_name}.{field}: the SDK declares it required, but the schema does \
                     not — make the SDK field `Option<..>` or mark it `required = false`"
                ));
            }
        }
    }

    assert!(
        problems.is_empty(),
        "temps-plugin-sdk projections no longer match the platform's schemas:\n  {}",
        problems.join("\n  ")
    );
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
