// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Variable aliases on a project↔service link.
//!
//! A linked database injects its own names (`POSTGRES_URL`); an app usually
//! asks for another (`DATABASE_URL`). The link's `env_aliases` maps the app's
//! name onto the injected one, and the runtime variables then carry both.

use crate::services::*;
use std::collections::{BTreeMap, HashMap};
use tracing::warn;

/// Most aliases one link may carry.
pub const MAX_LINK_ENV_ALIASES: usize = 32;

fn valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && name.len() <= 128
}

/// Check aliases a client sent: valid variable names on both sides, never a
/// name onto itself, and a bounded count.
pub fn validate_env_aliases(
    service_id: i32,
    project_id: i32,
    aliases: &BTreeMap<String, String>,
) -> Result<(), ExternalServiceError> {
    let refuse = |reason: String| ExternalServiceError::InvalidDatabaseProvisioning {
        service_id,
        project_id,
        reason,
    };
    if aliases.len() > MAX_LINK_ENV_ALIASES {
        return Err(refuse(format!(
            "a link carries at most {MAX_LINK_ENV_ALIASES} variable aliases, not {}",
            aliases.len()
        )));
    }
    for (alias, injected) in aliases {
        if !valid_env_name(alias) {
            return Err(refuse(format!(
                "variable alias '{alias}' is not a valid variable name"
            )));
        }
        if !valid_env_name(injected) {
            return Err(refuse(format!(
                "variable alias '{alias}' points at '{injected}', which is not a valid variable name"
            )));
        }
        if alias == injected {
            return Err(refuse(format!("variable alias '{alias}' points at itself")));
        }
    }
    Ok(())
}

/// Read a stored `env_aliases` document. Anything that is not a map of
/// strings is ignored, never fatal for a deployment.
pub fn stored_env_aliases(value: &serde_json::Value) -> BTreeMap<String, String> {
    value
        .as_object()
        .map(|map| {
            map.iter()
                .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

/// Add each alias to `vars`, copying the injected variable's value. An alias
/// never overrides a variable the service injects itself, and one pointing
/// at a name the service did not inject is skipped.
pub fn apply_env_aliases(
    vars: &mut HashMap<String, String>,
    aliases: &BTreeMap<String, String>,
    service_id: i32,
    project_id: i32,
) {
    for (alias, injected) in aliases {
        if vars.contains_key(alias) {
            continue;
        }
        match vars.get(injected).cloned() {
            Some(value) => {
                vars.insert(alias.clone(), value);
            }
            None => warn!(
                service_id,
                project_id,
                alias = %alias,
                injected = %injected,
                "Skipping a variable alias: the service does not inject the variable it points at"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aliases(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    }

    #[test]
    fn database_url_gets_the_postgres_url_value() {
        let mut vars = HashMap::from([
            (
                "POSTGRES_URL".to_string(),
                "postgresql://app@db:5432/app".to_string(),
            ),
            ("POSTGRES_HOST".to_string(), "db".to_string()),
        ]);
        apply_env_aliases(
            &mut vars,
            &aliases(&[
                ("DATABASE_URL", "POSTGRES_URL"),
                ("PGHOST", "POSTGRES_HOST"),
            ]),
            3,
            9,
        );
        assert_eq!(vars["DATABASE_URL"], "postgresql://app@db:5432/app");
        assert_eq!(vars["PGHOST"], "db");
        assert_eq!(vars["POSTGRES_URL"], "postgresql://app@db:5432/app");
    }

    #[test]
    fn an_alias_never_overrides_an_injected_variable_or_invents_one() {
        let mut vars = HashMap::from([
            ("DATABASE_URL".to_string(), "mysql://x".to_string()),
            ("MYSQL_HOST".to_string(), "db".to_string()),
        ]);
        apply_env_aliases(
            &mut vars,
            &aliases(&[
                ("DATABASE_URL", "MYSQL_HOST"),
                ("REDIS_URL", "NOT_INJECTED"),
            ]),
            3,
            9,
        );
        assert_eq!(vars["DATABASE_URL"], "mysql://x");
        assert!(!vars.contains_key("REDIS_URL"));
    }

    #[test]
    fn stored_aliases_tolerate_bad_documents() {
        assert_eq!(
            stored_env_aliases(&serde_json::json!({"DATABASE_URL": "POSTGRES_URL", "X": 1})),
            aliases(&[("DATABASE_URL", "POSTGRES_URL")])
        );
        assert!(stored_env_aliases(&serde_json::json!([1, 2])).is_empty());
        assert!(stored_env_aliases(&serde_json::Value::Null).is_empty());
    }

    #[test]
    fn invalid_aliases_are_refused_with_the_name() {
        for (bad, needle) in [
            (aliases(&[("1BAD", "POSTGRES_URL")]), "1BAD"),
            (aliases(&[("DATABASE_URL", "has space")]), "has space"),
            (aliases(&[("SAME", "SAME")]), "itself"),
        ] {
            let error = validate_env_aliases(3, 9, &bad).unwrap_err();
            assert!(error.to_string().contains(needle), "{error}");
            assert!(error.to_string().contains("service 3"), "{error}");
        }
        let too_many: BTreeMap<String, String> = (0..=MAX_LINK_ENV_ALIASES)
            .map(|i| (format!("A{i}"), "B".to_string()))
            .collect();
        assert!(validate_env_aliases(3, 9, &too_many).is_err());
        assert!(validate_env_aliases(3, 9, &aliases(&[("DATABASE_URL", "POSTGRES_URL")])).is_ok());
    }
}
