// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A worker's scheduling labels, as an operator types them (ADR-091 D2).
//!
//! `temps join --labels` and `temps agent labels set` both parse and check
//! labels here, so one rule set decides what a node may carry. The control
//! plane stores whatever a heartbeat reports; these checks are where a typo
//! (`temps.sh/role=dedicatd`) is caught instead of silently doing nothing.

use std::collections::BTreeMap;

/// Label key that gives a node its scheduling role.
pub const NODE_ROLE_LABEL: &str = "temps.sh/role";
/// [`NODE_ROLE_LABEL`] value for a build-only node.
pub const BUILDER_NODE_ROLE: &str = "builder";
/// [`NODE_ROLE_LABEL`] value for a dedicated node.
pub const DEDICATED_NODE_ROLE: &str = "dedicated";
/// Every value [`NODE_ROLE_LABEL`] may take.
pub const NODE_ROLES: [&str; 2] = [BUILDER_NODE_ROLE, DEDICATED_NODE_ROLE];

/// Key prefix reserved for labels Temps itself interprets.
pub const RESERVED_PREFIX: &str = "temps.sh/";
/// Longest key name (the part after any `prefix/`).
pub const MAX_NAME_LEN: usize = 63;
/// Longest key prefix.
pub const MAX_PREFIX_LEN: usize = 253;
/// Longest part of a key prefix between dots, as in a DNS name.
pub const MAX_DNS_LABEL_LEN: usize = 63;
/// Longest value, in characters.
pub const MAX_VALUE_LEN: usize = 253;
/// Most labels one node may carry: they travel in every heartbeat.
pub const MAX_LABELS: usize = 64;

/// Why a label was refused. Each message names the label.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LabelError {
    #[error("label '{input}' has no '=': write labels as key=value")]
    MissingEquals { input: String },
    #[error("label key '{key}' is not valid: {reason}")]
    InvalidKey { key: String, reason: String },
    #[error("label '{key}' has an invalid value '{value}': {reason}")]
    InvalidValue {
        key: String,
        value: String,
        reason: String,
    },
    #[error(
        "label '{key}' is reserved: the only label under temps.sh/ is temps.sh/role \
         (builder or dedicated)"
    )]
    ReservedKey { key: String },
    #[error("temps.sh/role must be builder or dedicated, not '{value}'")]
    InvalidRole { value: String },
    #[error("label '{key}' is given twice, as '{first}' and '{second}'")]
    Duplicate {
        key: String,
        first: String,
        second: String,
    },
    #[error("a node carries at most {max} labels; this would give it {count}")]
    TooMany { count: usize, max: usize },
}

fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')
}

fn check_name(key: &str, name: &str) -> Result<(), LabelError> {
    let invalid = |reason: String| LabelError::InvalidKey {
        key: key.to_string(),
        reason,
    };
    if name.is_empty() {
        return Err(invalid("the name is empty".to_string()));
    }
    if name.len() > MAX_NAME_LEN {
        return Err(invalid(format!(
            "the name is {} characters; the most is {MAX_NAME_LEN}",
            name.len()
        )));
    }
    if let Some(bad) = name.chars().find(|c| !is_name_char(*c)) {
        return Err(invalid(format!(
            "'{}' is not allowed; use letters, digits, '-', '_' and '.'",
            bad.escape_default()
        )));
    }
    let edge_ok = |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphanumeric());
    if !edge_ok(name.chars().next()) || !edge_ok(name.chars().last()) {
        return Err(invalid(
            "the name must start and end with a letter or digit".to_string(),
        ));
    }
    Ok(())
}

fn check_prefix(key: &str, prefix: &str) -> Result<(), LabelError> {
    let invalid = |reason: String| LabelError::InvalidKey {
        key: key.to_string(),
        reason,
    };
    if prefix.is_empty() || prefix.len() > MAX_PREFIX_LEN {
        return Err(invalid(format!(
            "the prefix before '/' must be 1 to {MAX_PREFIX_LEN} characters"
        )));
    }
    let ok = prefix.split('.').all(|part| {
        !part.is_empty()
            && part.len() <= MAX_DNS_LABEL_LEN
            && part
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            && !part.starts_with('-')
            && !part.ends_with('-')
    });
    if !ok {
        return Err(invalid(format!(
            "the prefix '{prefix}' must be a lowercase DNS name such as example.com, each \
             part between dots 1 to {MAX_DNS_LABEL_LEN} characters"
        )));
    }
    Ok(())
}

/// Check a key: `name` or `prefix/name`, and not a reserved key.
pub fn validate_key(key: &str) -> Result<(), LabelError> {
    match key.split_once('/') {
        Some((prefix, name)) => {
            check_prefix(key, prefix)?;
            if name.contains('/') {
                return Err(LabelError::InvalidKey {
                    key: key.to_string(),
                    reason: "a key has at most one '/'".to_string(),
                });
            }
            check_name(key, name)?;
            if key.starts_with(RESERVED_PREFIX) && key != NODE_ROLE_LABEL {
                return Err(LabelError::ReservedKey {
                    key: key.to_string(),
                });
            }
            Ok(())
        }
        None => check_name(key, key),
    }
}

/// Check a value for `key` (already checked).
pub fn validate_value(key: &str, value: &str) -> Result<(), LabelError> {
    let invalid = |reason: &str| LabelError::InvalidValue {
        key: key.to_string(),
        value: value.escape_default().to_string(),
        reason: reason.to_string(),
    };
    if value.chars().count() > MAX_VALUE_LEN {
        return Err(invalid("values are at most 253 characters"));
    }
    if value.chars().any(char::is_control) {
        return Err(invalid("control characters are not allowed"));
    }
    if value.contains(',') {
        return Err(invalid(
            "commas separate labels, so a value cannot hold one",
        ));
    }
    if value.trim() != value {
        return Err(invalid("remove the spaces around it"));
    }
    if key == NODE_ROLE_LABEL && !NODE_ROLES.contains(&value) {
        return Err(LabelError::InvalidRole {
            value: value.escape_default().to_string(),
        });
    }
    Ok(())
}

/// Parse one `key=value` and check both halves. Whitespace around the key
/// and value is ignored, as `temps agent --labels` always did.
pub fn parse_label(input: &str) -> Result<(String, String), LabelError> {
    let (key, value) = input
        .split_once('=')
        .ok_or_else(|| LabelError::MissingEquals {
            input: input.escape_default().to_string(),
        })?;
    let (key, value) = (key.trim(), value.trim());
    validate_key(key)?;
    validate_value(key, value)?;
    Ok((key.to_string(), value.to_string()))
}

/// Parse labels as `--labels` gives them (each `key=value`; empty entries,
/// from a trailing comma, are skipped). A key given twice with different
/// values is refused rather than guessed at.
pub fn parse_labels<S: AsRef<str>>(inputs: &[S]) -> Result<BTreeMap<String, String>, LabelError> {
    let mut labels: BTreeMap<String, String> = BTreeMap::new();
    for input in inputs {
        let input = input.as_ref();
        if input.trim().is_empty() {
            continue;
        }
        let (key, value) = parse_label(input)?;
        if let Some(first) = labels.get(&key) {
            if *first != value {
                return Err(LabelError::Duplicate {
                    key,
                    first: first.clone(),
                    second: value,
                });
            }
        }
        labels.insert(key, value);
    }
    check_count(labels.len())?;
    Ok(labels)
}

/// Refuse a label set larger than [`MAX_LABELS`].
pub fn check_count(count: usize) -> Result<(), LabelError> {
    if count > MAX_LABELS {
        return Err(LabelError::TooMany {
            count,
            max: MAX_LABELS,
        });
    }
    Ok(())
}

/// Labels as the JSON object `agent.json` and the heartbeat carry.
pub fn to_json(labels: &BTreeMap<String, String>) -> serde_json::Value {
    serde_json::Value::Object(
        labels
            .iter()
            .map(|(key, value)| (key.clone(), serde_json::Value::String(value.clone())))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_the_labels_temps_documents() {
        for input in [
            "region=eu-west",
            "gpu=true",
            "zone=a",
            "temps.sh/role=dedicated",
            "temps.sh/role=builder",
            "example.com/team=data",
            "tier=",
            "api_address=10.0.0.5:8080",
            " region = us-east ",
        ] {
            parse_label(input).unwrap_or_else(|e| panic!("{input}: {e}"));
        }
        assert_eq!(
            parse_label(" region = us-east ").unwrap(),
            ("region".to_string(), "us-east".to_string())
        );
    }

    #[test]
    fn a_role_typo_is_refused_instead_of_silently_ignored() {
        assert_eq!(
            parse_label("temps.sh/role=dedicatd"),
            Err(LabelError::InvalidRole {
                value: "dedicatd".to_string()
            })
        );
        assert!(parse_label("temps.sh/role=worker").is_err());
        assert!(parse_label("temps.sh/role=").is_err());
        let error = parse_label("temps.sh/role=Dedicated")
            .unwrap_err()
            .to_string();
        assert!(error.contains("builder or dedicated"), "{error}");
    }

    #[test]
    fn other_temps_keys_are_reserved() {
        assert_eq!(
            parse_label("temps.sh/dedicated=true"),
            Err(LabelError::ReservedKey {
                key: "temps.sh/dedicated".to_string()
            })
        );
    }

    #[test]
    fn malformed_keys_are_refused_with_the_reason() {
        let cases = [
            ("=x", "empty"),
            ("-gpu=x", "start and end"),
            ("gpu-=x", "start and end"),
            ("gp u=x", "not allowed"),
            ("Example.com/x=1", "lowercase DNS name"),
            ("a//b=1", "at most one '/'"),
            ("/b=1", "1 to 253"),
            ("a/b/c=1", "at most one '/'"),
            ("a.b/=1", "empty"),
        ];
        for (input, reason) in cases {
            let error = parse_label(input).unwrap_err().to_string();
            assert!(error.contains(reason), "{input}: {error}");
        }
        let long = format!("{}=1", "a".repeat(MAX_NAME_LEN + 1));
        assert!(parse_label(&long)
            .unwrap_err()
            .to_string()
            .contains("most is 63"));
        assert!(parse_label(&format!("{}=1", "a".repeat(MAX_NAME_LEN))).is_ok());
        // Each part of a prefix is a DNS label: at most 63 characters.
        let part = "p".repeat(MAX_DNS_LABEL_LEN);
        assert!(parse_label(&format!("{part}.example.com/x=1")).is_ok());
        let error = parse_label(&format!("{part}p.example.com/x=1"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("1 to 63"), "{error}");
    }

    #[test]
    fn malformed_values_are_refused_with_the_reason() {
        assert!(matches!(
            parse_label("gpu"),
            Err(LabelError::MissingEquals { .. })
        ));
        for (value, reason) in [
            ("a\nb", "control"),
            ("a\u{7}", "control"),
            ("a,b", "commas"),
        ] {
            let error = validate_value("k", value).unwrap_err().to_string();
            assert!(error.contains(reason), "{value:?}: {error}");
        }
        assert!(validate_value("k", " a").is_err());
        assert!(validate_value("k", &"v".repeat(MAX_VALUE_LEN)).is_ok());
        assert!(validate_value("k", &"v".repeat(MAX_VALUE_LEN + 1)).is_err());
        // The limit counts characters, not bytes.
        assert!(validate_value("k", &"é".repeat(MAX_VALUE_LEN)).is_ok());
        assert!(validate_value("k", &"é".repeat(MAX_VALUE_LEN + 1)).is_err());
        // A value may hold '=' after the first one.
        assert_eq!(parse_label("k=a=b").unwrap().1, "a=b");
    }

    #[test]
    fn a_list_skips_empty_entries_and_refuses_conflicts_and_excess() {
        let labels = parse_labels(&["gpu=true", "", "gpu=true", "zone=a"]).unwrap();
        assert_eq!(labels.len(), 2);
        assert_eq!(
            to_json(&labels),
            serde_json::json!({"gpu": "true", "zone": "a"})
        );
        assert!(matches!(
            parse_labels(&["gpu=true", "gpu=false"]),
            Err(LabelError::Duplicate { .. })
        ));
        let many: Vec<String> = (0..=MAX_LABELS).map(|i| format!("k{i}=v")).collect();
        assert_eq!(
            parse_labels(&many),
            Err(LabelError::TooMany {
                count: MAX_LABELS + 1,
                max: MAX_LABELS
            })
        );
        assert!(parse_labels(&many[..MAX_LABELS]).is_ok());
    }
}
