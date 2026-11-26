// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The words plans, runs and journals are stored and served with (ADR-117
//! D5), pinned so that moving providers behind the trait cannot rename one.
//!
//! `reachability_plans.plan` stores a [`ReachabilityPlan`], and
//! `reachability_runs` stores [`RunStep`]s, [`JournalEntry`]s and the
//! [`super::run::RunInput`] a resume re-reads; the console reads the same
//! types through the API. A changed name would strand a stored plan or run,
//! or change the generated clients. When a change here is intended, it comes
//! with a migration of the stored rows, and the golden files are replaced
//! with the new output the failing test wrote.

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde_json::Value;
use utoipa::ToSchema;

/// `T`'s schema and every schema it references, by name, without prose.
pub(crate) fn schema_shape<T: ToSchema>(into: &mut serde_json::Map<String, Value>) {
    let mut schemas = Vec::new();
    T::schemas(&mut schemas);
    schemas.push((T::name().into_owned(), T::schema()));
    for (name, schema) in schemas {
        let mut value = serde_json::to_value(schema).expect("a schema serializes");
        strip_prose(&mut value);
        into.insert(name, value);
    }
}

/// Descriptions, titles and examples change with doc comments, not with
/// what is stored.
fn strip_prose(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for key in ["description", "title", "example", "examples"] {
                map.remove(key);
            }
            map.values_mut().for_each(strip_prose);
        }
        Value::Array(items) => items.iter_mut().for_each(strip_prose),
        _ => {}
    }
}

/// Every path in `value` with its leaf's JSON type: `serve.node_id: string`.
/// Arrays read as `[]`, so the shape does not depend on how many items a
/// fixture has.
pub(crate) fn paths(value: &Value, into: &mut BTreeSet<String>) {
    fn walk(value: &Value, at: &str, into: &mut BTreeSet<String>) {
        let leaf = |kind: &str| format!("{at}: {kind}");
        match value {
            Value::Object(map) => {
                for (key, child) in map {
                    let next = if at.is_empty() {
                        key.clone()
                    } else {
                        format!("{at}.{key}")
                    };
                    walk(child, &next, into);
                }
            }
            Value::Array(items) => {
                let next = format!("{at}[]");
                if items.is_empty() {
                    into.insert(format!("{next}: empty"));
                }
                for item in items {
                    walk(item, &next, into);
                }
            }
            Value::Null => {
                into.insert(leaf("null"));
            }
            Value::Bool(_) => {
                into.insert(leaf("bool"));
            }
            Value::Number(_) => {
                into.insert(leaf("number"));
            }
            Value::String(_) => {
                into.insert(leaf("string"));
            }
        }
    }
    walk(value, "", into);
}

/// Fail when `actual` differs from the golden file `name`, writing the new
/// output next to the build so an intended change can replace it.
pub(crate) fn assert_golden(name: &str, golden: &str, actual: &str) {
    if golden == actual {
        return;
    }
    let out: PathBuf = std::env::temp_dir().join(name);
    let written = std::fs::write(&out, actual).is_ok();
    panic!(
        "The stored or served shape of reachability plans and runs changed ({name}). Stored \
         rows and the generated clients use these names (ADR-117 D5). If the change is \
         intended, migrate the stored rows and replace \
         crates/temps-ingress/src/reachability/wire_shape/{name} with {}{}.",
        out.display(),
        if written {
            ""
        } else {
            " (could not be written)"
        }
    );
}

#[cfg(test)]
mod tests {
    use super::super::model::{
        Intent, JournalEntry, PlanOptionKind, ReachabilityPlan, RunResource, RunStep, StepKind,
        UndoResult,
    };
    use super::*;

    #[test]
    fn plans_runs_and_journals_keep_their_stored_and_served_names() {
        let mut schemas = serde_json::Map::new();
        schema_shape::<ReachabilityPlan>(&mut schemas);
        schema_shape::<RunStep>(&mut schemas);
        schema_shape::<JournalEntry>(&mut schemas);
        schema_shape::<UndoResult>(&mut schemas);
        schema_shape::<PlanOptionKind>(&mut schemas);
        schema_shape::<StepKind>(&mut schemas);
        schema_shape::<Intent>(&mut schemas);
        schema_shape::<RunResource>(&mut schemas);
        let actual =
            serde_json::to_string_pretty(&Value::Object(schemas)).expect("serializes") + "\n";
        assert_golden(
            "schemas.json",
            include_str!("wire_shape/schemas.json"),
            &actual,
        );
    }

    #[test]
    fn option_kinds_parse_back_from_what_they_store() {
        // `reachability_runs.option` stores `as_str`; a resume parses it.
        let stored = [
            "existing_tunnel",
            "direct_managed_dns",
            "new_tunnel",
            "new_tunnel_with_token",
            "direct_manual_dns",
            "private_worker",
            "tailscale_serve",
            "tailscale_service",
            "tailscale_service_with_credential",
            "tunnel_environment_hosts",
        ];
        for word in stored {
            let kind =
                PlanOptionKind::parse(word).unwrap_or_else(|| panic!("{word} no longer parses"));
            assert_eq!(kind.as_str(), word);
            assert_eq!(
                serde_json::to_value(kind).expect("serializes"),
                Value::String(word.into())
            );
        }
    }

    #[test]
    fn paths_name_every_leaf_with_its_type() {
        let mut out = BTreeSet::new();
        paths(
            &serde_json::json!({"a": {"b": [1, 2]}, "c": null, "d": [], "e": "x"}),
            &mut out,
        );
        assert_eq!(
            out.into_iter().collect::<Vec<_>>(),
            vec!["a.b[]: number", "c: null", "d[]: empty", "e: string"]
        );
    }
}
