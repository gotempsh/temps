// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Compose-compatible variable interpolation, applied before policy validation.
//!
//! Docker Compose substitutes `${VAR}`, `${VAR:-default}`, `${VAR:?message}`
//! and friends when it loads a file. Validating the raw document therefore
//! checks text that is not what runs: `image: ${IMG:-ghcr.io/org/app:1}` is a
//! template, not an image. Temps resolves the document itself against the same
//! variables Compose would use (repository `.env`, overridden by the project
//! environment), validates the result, and hands Compose the resolved document
//! with every remaining literal `$` escaped as `$$`. Compose then has nothing
//! left to substitute, so the values that were validated are exactly the values
//! that are deployed — the guarantee does not depend on this module matching
//! Compose's interpolation byte for byte.

use serde_yaml::Value;
use std::collections::{BTreeSet, HashMap};
use thiserror::Error;

/// Longest location path copied into an error. Locations are built from
/// repository keys, never values, but a pathological key could be large.
const MAX_LOCATION_BYTES: usize = 256;

/// Deepest `${A:-${B:-...}}` nesting that is resolved before failing closed.
const MAX_NESTING_DEPTH: usize = 16;

/// Largest repository `.env` read for interpolation.
pub const MAX_DOTENV_BYTES: usize = 1024 * 1024;

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum InterpolationError {
    #[error(
        "Required Compose variable {variable} has no value (referenced at {location}). Set it in \
         the project environment or the repository .env file"
    )]
    RequiredVariableMissing { variable: String, location: String },

    #[error(
        "Invalid Compose interpolation at {location}: {reason}. Write $$ for a literal dollar sign"
    )]
    InvalidSyntax { location: String, reason: String },

    #[error("Failed to parse {document} as YAML before interpolation: {reason}")]
    Yaml { document: String, reason: String },
}

/// A document after interpolation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterpolatedDocument {
    /// The resolved YAML. Every literal `$` is escaped as `$$`, so Compose
    /// performs no further substitution on it.
    pub content: String,
    /// Whether any substitution took place. When false, `content` is the
    /// original text byte for byte.
    pub changed: bool,
    /// Variables referenced without a default that were not set. Compose
    /// substitutes an empty string for these and prints a warning; Temps
    /// reports them in the deployment log instead.
    pub unset_variables: BTreeSet<String>,
    /// Every variable name the document referenced, set or not.
    pub referenced_variables: BTreeSet<String>,
}

/// Resolve every interpolation in `content` against `variables`.
///
/// Keys are never interpolated (Compose does not either). Tags such as
/// `!reset` and `!override` are preserved around their resolved values.
pub fn interpolate_document(
    document: &str,
    content: &str,
    variables: &HashMap<String, String>,
) -> Result<InterpolatedDocument, InterpolationError> {
    if !content.contains('$') {
        return Ok(InterpolatedDocument {
            content: content.to_string(),
            changed: false,
            unset_variables: BTreeSet::new(),
            referenced_variables: BTreeSet::new(),
        });
    }
    let mut root: Value =
        serde_yaml::from_str(content).map_err(|error| InterpolationError::Yaml {
            document: document.to_string(),
            reason: error.to_string(),
        })?;
    let mut state = WalkState {
        variables,
        unset: BTreeSet::new(),
        referenced: BTreeSet::new(),
        changed: false,
    };
    let mut location = String::new();
    state.walk(&mut root, &mut location)?;
    if !state.changed {
        return Ok(InterpolatedDocument {
            content: content.to_string(),
            changed: false,
            unset_variables: state.unset,
            referenced_variables: state.referenced,
        });
    }
    let content = serde_yaml::to_string(&root).map_err(|error| InterpolationError::Yaml {
        document: document.to_string(),
        reason: format!("failed to serialize the resolved document: {error}"),
    })?;
    Ok(InterpolatedDocument {
        content,
        changed: true,
        unset_variables: state.unset,
        referenced_variables: state.referenced,
    })
}

struct WalkState<'a> {
    variables: &'a HashMap<String, String>,
    unset: BTreeSet<String>,
    referenced: BTreeSet<String>,
    changed: bool,
}

impl WalkState<'_> {
    fn walk(&mut self, value: &mut Value, location: &mut String) -> Result<(), InterpolationError> {
        match value {
            Value::String(text) => {
                if !text.contains('$') {
                    return Ok(());
                }
                let resolved = substitute(
                    text,
                    self.variables,
                    location,
                    &mut self.unset,
                    &mut self.referenced,
                    0,
                )?;
                let escaped = resolved.replace('$', "$$");
                if escaped != *text {
                    self.changed = true;
                    *text = escaped;
                }
                Ok(())
            }
            Value::Sequence(items) => {
                for (index, item) in items.iter_mut().enumerate() {
                    let previous = location.len();
                    push_location(location, &format!("[{index}]"));
                    self.walk(item, location)?;
                    location.truncate(previous);
                }
                Ok(())
            }
            Value::Mapping(map) => {
                for (key, item) in map.iter_mut() {
                    let previous = location.len();
                    let free_form_parent = is_free_form_map(location);
                    let key = match key {
                        Value::String(key) => key.clone(),
                        Value::Number(number) => number.to_string(),
                        Value::Bool(flag) => flag.to_string(),
                        _ => "<key>".to_string(),
                    };
                    if !location.is_empty() {
                        push_location(location, ".");
                    }
                    push_location(location, &key);
                    let had_template = matches!(item, Value::String(text) if text.contains('$'));
                    self.walk(item, location)?;
                    if had_template && !free_form_parent {
                        cast_resolved_scalar(&key, item);
                    }
                    location.truncate(previous);
                }
                Ok(())
            }
            Value::Tagged(tagged) => self.walk(&mut tagged.value, location),
            Value::Null | Value::Bool(_) | Value::Number(_) => Ok(()),
        }
    }
}

/// Fields Compose reads as booleans. A templated value resolved to a boolean
/// word becomes a real boolean, as Compose's own type casting does, so the
/// policy validator judges the value Compose will apply. Anything else stays
/// a string, which the validator rejects as a non-literal boolean.
const BOOLEAN_FIELDS: &[&str] = &[
    "privileged",
    "use_api_socket",
    "oom_kill_disable",
    "read_only",
    "init",
    "tty",
    "stdin_open",
    "attach",
    "external",
    "required",
    "disable",
];

/// Fields Compose reads as integers.
const INTEGER_FIELDS: &[&str] = &["replicas", "pids_limit", "published", "target"];

/// Maps whose keys are user data (label names, variable names), where a key
/// such as `privileged` is not the Compose field of that name.
fn is_free_form_map(location: &str) -> bool {
    let last = location.rsplit('.').next().unwrap_or_default();
    matches!(
        last,
        "labels"
            | "environment"
            | "args"
            | "annotations"
            | "driver_opts"
            | "sysctls"
            | "extra_hosts"
    ) || last.starts_with("x-")
}

fn cast_resolved_scalar(key: &str, value: &mut Value) {
    let Value::String(text) = value else {
        return;
    };
    if BOOLEAN_FIELDS.contains(&key) {
        let cast = match text.to_ascii_lowercase().as_str() {
            "true" | "yes" | "y" | "on" => Some(true),
            "false" | "no" | "n" | "off" => Some(false),
            _ => None,
        };
        if let Some(flag) = cast {
            *value = Value::Bool(flag);
        }
    } else if INTEGER_FIELDS.contains(&key) {
        if let Ok(number) = text.trim().parse::<u64>() {
            *value = Value::Number(number.into());
        }
    }
}

fn push_location(location: &mut String, segment: &str) {
    if location.len() >= MAX_LOCATION_BYTES {
        return;
    }
    for ch in segment.chars() {
        if location.len() + ch.len_utf8() > MAX_LOCATION_BYTES {
            location.push('…');
            return;
        }
        location.push(ch);
    }
}

fn location_label(location: &str) -> String {
    if location.is_empty() {
        "<document root>".to_string()
    } else {
        location.to_string()
    }
}

fn is_name_start(ch: char) -> bool {
    ch.is_ascii_alphabetic() || ch == '_'
}

fn is_name_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

/// Substitute one string value, Compose-style.
///
/// * `$$` is a literal `$`.
/// * `$NAME` and `${NAME}` take the variable's value, or an empty string when
///   it is unset (recorded in `unset`).
/// * `${NAME:-d}` / `${NAME-d}`: `d` when unset-or-empty / unset.
/// * `${NAME:?m}` / `${NAME?m}`: fail when unset-or-empty / unset.
/// * `${NAME:+r}` / `${NAME+r}`: `r` when set-and-non-empty / set, else empty.
/// * Defaults and replacements are themselves substituted, so nesting works.
/// * A `$` followed by anything else is a literal `$`, as in Compose.
fn substitute(
    input: &str,
    variables: &HashMap<String, String>,
    location: &str,
    unset: &mut BTreeSet<String>,
    referenced: &mut BTreeSet<String>,
    depth: usize,
) -> Result<String, InterpolationError> {
    if depth > MAX_NESTING_DEPTH {
        return Err(InterpolationError::InvalidSyntax {
            location: location_label(location),
            reason: format!("variable defaults are nested more than {MAX_NESTING_DEPTH} levels"),
        });
    }
    let chars: Vec<char> = input.chars().collect();
    let mut output = String::with_capacity(input.len());
    let mut index = 0;
    while index < chars.len() {
        let ch = chars[index];
        if ch != '$' {
            output.push(ch);
            index += 1;
            continue;
        }
        match chars.get(index + 1).copied() {
            Some('$') => {
                output.push('$');
                index += 2;
            }
            Some('{') => {
                let body_start = index + 2;
                let mut nesting = 1_usize;
                let mut cursor = body_start;
                while cursor < chars.len() {
                    match chars[cursor] {
                        '{' => nesting += 1,
                        '}' => {
                            nesting -= 1;
                            if nesting == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    cursor += 1;
                }
                if nesting != 0 {
                    return Err(InterpolationError::InvalidSyntax {
                        location: location_label(location),
                        reason: "'${' has no closing '}'".to_string(),
                    });
                }
                let body: String = chars[body_start..cursor].iter().collect();
                output.push_str(&resolve_braced(
                    &body, variables, location, unset, referenced, depth,
                )?);
                index = cursor + 1;
            }
            Some(next) if is_name_start(next) => {
                let mut cursor = index + 1;
                while cursor < chars.len() && is_name_char(chars[cursor]) {
                    cursor += 1;
                }
                let name: String = chars[index + 1..cursor].iter().collect();
                referenced.insert(name.clone());
                match variables.get(&name) {
                    Some(value) => output.push_str(value),
                    None => {
                        unset.insert(name);
                    }
                }
                index = cursor;
            }
            _ => {
                output.push('$');
                index += 1;
            }
        }
    }
    Ok(output)
}

fn resolve_braced(
    body: &str,
    variables: &HashMap<String, String>,
    location: &str,
    unset: &mut BTreeSet<String>,
    referenced: &mut BTreeSet<String>,
    depth: usize,
) -> Result<String, InterpolationError> {
    let name_len = body
        .char_indices()
        .take_while(|(position, ch)| {
            if *position == 0 {
                is_name_start(*ch)
            } else {
                is_name_char(*ch)
            }
        })
        .map(|(position, ch)| position + ch.len_utf8())
        .last()
        .unwrap_or(0);
    if name_len == 0 {
        return Err(InterpolationError::InvalidSyntax {
            location: location_label(location),
            reason: "'${...}' must start with a variable name".to_string(),
        });
    }
    let (name, modifier) = body.split_at(name_len);
    referenced.insert(name.to_string());
    let value = variables.get(name);
    let set = value.is_some();
    let non_empty = value.is_some_and(|value| !value.is_empty());
    let current = value.cloned().unwrap_or_default();

    let nested = |text: &str,
                  unset: &mut BTreeSet<String>,
                  referenced: &mut BTreeSet<String>|
     -> Result<String, InterpolationError> {
        substitute(text, variables, location, unset, referenced, depth + 1)
    };

    let required_missing = || InterpolationError::RequiredVariableMissing {
        variable: name.to_string(),
        location: location_label(location),
    };

    if modifier.is_empty() {
        if !set {
            unset.insert(name.to_string());
        }
        return Ok(current);
    }
    if let Some(default) = modifier.strip_prefix(":-") {
        return if non_empty {
            Ok(current)
        } else {
            nested(default, unset, referenced)
        };
    }
    if let Some(default) = modifier.strip_prefix('-') {
        return if set {
            Ok(current)
        } else {
            nested(default, unset, referenced)
        };
    }
    if modifier.starts_with(":?") {
        return if non_empty {
            Ok(current)
        } else {
            Err(required_missing())
        };
    }
    if modifier.starts_with('?') {
        return if set {
            Ok(current)
        } else {
            Err(required_missing())
        };
    }
    if let Some(replacement) = modifier.strip_prefix(":+") {
        return if non_empty {
            nested(replacement, unset, referenced)
        } else {
            Ok(String::new())
        };
    }
    if let Some(replacement) = modifier.strip_prefix('+') {
        return if set {
            nested(replacement, unset, referenced)
        } else {
            Ok(String::new())
        };
    }
    Err(InterpolationError::InvalidSyntax {
        location: location_label(location),
        reason: format!(
            "unsupported modifier after variable {name}; use ':-', '-', ':?', '?', ':+' or '+'"
        ),
    })
}

/// Parse a dotenv file the way Compose reads `.env` for interpolation.
///
/// Supports comments, `export KEY=...`, single-quoted literals, double-quoted
/// values with escapes (both may span lines), unquoted values with trailing
/// ` # comments`, and `${VAR}` references to keys defined earlier in the same
/// file. A malformed line is skipped rather than failing the deployment:
/// Compose's own parser is the authority on the file at runtime, and Temps
/// only needs these values to resolve the Compose document.
pub fn parse_dotenv(content: &str) -> HashMap<String, String> {
    let mut values: HashMap<String, String> = HashMap::new();
    let chars: Vec<char> = content.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        // Skip leading whitespace and blank lines.
        while index < chars.len() && chars[index].is_whitespace() {
            index += 1;
        }
        if index >= chars.len() {
            break;
        }
        if chars[index] == '#' {
            skip_to_line_end(&chars, &mut index);
            continue;
        }
        let line_start = index;
        // Optional `export ` prefix.
        let rest: String = chars[index..chars.len().min(index + 7)].iter().collect();
        if rest == "export " {
            index += 7;
            while index < chars.len() && (chars[index] == ' ' || chars[index] == '\t') {
                index += 1;
            }
        }
        let key_start = index;
        while index < chars.len() && (is_name_char(chars[index]) || chars[index] == '.') {
            index += 1;
        }
        let key: String = chars[key_start..index].iter().collect();
        while index < chars.len() && (chars[index] == ' ' || chars[index] == '\t') {
            index += 1;
        }
        let valid_key = key.chars().next().is_some_and(is_name_start);
        if !valid_key || chars.get(index) != Some(&'=') {
            // Not an assignment: skip the line.
            index = line_start;
            skip_to_line_end(&chars, &mut index);
            continue;
        }
        index += 1;
        while index < chars.len() && (chars[index] == ' ' || chars[index] == '\t') {
            index += 1;
        }
        let value = match chars.get(index).copied() {
            Some('\'') => {
                index += 1;
                let start = index;
                while index < chars.len() && chars[index] != '\'' {
                    index += 1;
                }
                let value: String = chars[start..index.min(chars.len())].iter().collect();
                index += 1;
                skip_to_line_end(&chars, &mut index);
                value
            }
            Some('"') => {
                index += 1;
                let mut raw = String::new();
                while index < chars.len() && chars[index] != '"' {
                    if chars[index] == '\\' {
                        if let Some(next) = chars.get(index + 1).copied() {
                            match next {
                                'n' => raw.push('\n'),
                                'r' => raw.push('\r'),
                                't' => raw.push('\t'),
                                '"' => raw.push('"'),
                                '\\' => raw.push('\\'),
                                '$' => raw.push_str("$$"),
                                other => {
                                    raw.push('\\');
                                    raw.push(other);
                                }
                            }
                            index += 2;
                            continue;
                        }
                    }
                    raw.push(chars[index]);
                    index += 1;
                }
                index += 1;
                skip_to_line_end(&chars, &mut index);
                expand_dotenv_value(&raw, &values)
            }
            _ => {
                let start = index;
                skip_to_line_end(&chars, &mut index);
                let line: String = chars[start..index].iter().collect();
                let without_comment = match line.find(" #").or_else(|| line.find("\t#")) {
                    Some(position) => &line[..position],
                    None => line.as_str(),
                };
                expand_dotenv_value(without_comment.trim(), &values)
            }
        };
        values.insert(key, value);
    }
    values
}

fn skip_to_line_end(chars: &[char], index: &mut usize) {
    while *index < chars.len() && chars[*index] != '\n' {
        *index += 1;
    }
}

fn expand_dotenv_value(raw: &str, defined: &HashMap<String, String>) -> String {
    if !raw.contains('$') {
        return raw.to_string();
    }
    let mut unset = BTreeSet::new();
    let mut referenced = BTreeSet::new();
    substitute(raw, defined, ".env", &mut unset, &mut referenced, 0)
        .unwrap_or_else(|_| raw.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect()
    }

    fn resolve(
        input: &str,
        variables: &HashMap<String, String>,
    ) -> Result<String, InterpolationError> {
        let mut unset = BTreeSet::new();
        let mut referenced = BTreeSet::new();
        substitute(
            input,
            variables,
            "services.app.image",
            &mut unset,
            &mut referenced,
            0,
        )
    }

    #[test]
    fn substitutes_every_compose_modifier() {
        let variables = vars(&[("SET", "value"), ("EMPTY", "")]);
        let cases = [
            ("${SET}", "value"),
            ("$SET", "value"),
            ("${UNSET}", ""),
            ("${SET:-d}", "value"),
            ("${EMPTY:-d}", "d"),
            ("${UNSET:-d}", "d"),
            ("${EMPTY-d}", ""),
            ("${UNSET-d}", "d"),
            ("${SET:+r}", "r"),
            ("${EMPTY:+r}", ""),
            ("${EMPTY+r}", "r"),
            ("${UNSET+r}", ""),
            ("$$SET", "$SET"),
            ("cost: 5$", "cost: 5$"),
            ("$(cmd)", "$(cmd)"),
            ("${UNSET:-${SET}}", "value"),
            ("${UNSET:-${ALSO_UNSET:-deep}}", "deep"),
            ("pre-${SET}-post", "pre-value-post"),
        ];
        for (input, expected) in cases {
            assert_eq!(
                resolve(input, &variables).unwrap(),
                expected,
                "input {input}"
            );
        }
    }

    #[test]
    fn required_variables_fail_with_the_name_and_location_only() {
        let variables = vars(&[("EMPTY", "")]);
        for input in ["${MISSING:?set it}", "${EMPTY:?set it}", "${MISSING?x}"] {
            let error = resolve(input, &variables).unwrap_err();
            assert!(matches!(
                error,
                InterpolationError::RequiredVariableMissing { ref location, .. }
                    if location == "services.app.image"
            ));
            assert!(!error.to_string().contains("set it"));
        }
        // `?` without `:` accepts an empty value.
        assert_eq!(resolve("${EMPTY?x}", &variables).unwrap(), "");
    }

    #[test]
    fn malformed_templates_are_rejected() {
        let variables = vars(&[]);
        for input in ["${", "${1BAD}", "${NAME!x}", "${}"] {
            assert!(matches!(
                resolve(input, &variables),
                Err(InterpolationError::InvalidSyntax { .. })
            ));
        }
    }

    #[test]
    fn resolved_document_escapes_literal_dollars_and_keeps_keys() {
        let content = "services:\n  app:\n    image: ${IMG:-ghcr.io/o/a:1}\n    environment:\n      PRICE: ${PRICE}\n      ${KEY}: literal\n    command: echo $$HOME\n";
        let variables = vars(&[("PRICE", "5$USD")]);
        let resolved = interpolate_document("compose file", content, &variables).unwrap();
        assert!(resolved.changed);
        let value: Value = serde_yaml::from_str(&resolved.content).unwrap();
        let app = &value["services"]["app"];
        assert_eq!(app["image"].as_str(), Some("ghcr.io/o/a:1"));
        // The resolved literal `$` is escaped so Compose cannot expand `$USD`.
        assert_eq!(app["environment"]["PRICE"].as_str(), Some("5$$USD"));
        // Keys are not interpolated.
        assert!(app["environment"].get("${KEY}").is_some());
        assert_eq!(app["command"].as_str(), Some("echo $$HOME"));
        // A second pass is a no-op on values.
        let again = interpolate_document("compose file", &resolved.content, &variables).unwrap();
        assert!(!again.changed);
        assert!(resolved.referenced_variables.contains("IMG"));
    }

    #[test]
    fn templated_booleans_and_integers_are_cast_like_compose() {
        let content = "services:\n  app:\n    image: nginx\n    privileged: ${P:-false}\n    read_only: '${RO}'\n    deploy:\n      replicas: ${N:-2}\n    labels:\n      privileged: ${P:-false}\n    environment:\n      FLAG: ${P:-false}\n";
        let variables = vars(&[("RO", "maybe")]);
        let resolved = interpolate_document("compose file", content, &variables).unwrap();
        let value: Value = serde_yaml::from_str(&resolved.content).unwrap();
        let app = &value["services"]["app"];
        assert_eq!(app["privileged"], Value::Bool(false));
        // Not a boolean word: left as a string for the validator to reject.
        assert_eq!(app["read_only"].as_str(), Some("maybe"));
        assert_eq!(app["deploy"]["replicas"].as_u64(), Some(2));
        // Free-form maps keep strings, even under a field-like key.
        assert_eq!(app["environment"]["FLAG"].as_str(), Some("false"));
        assert_eq!(app["labels"]["privileged"].as_str(), Some("false"));
    }

    #[test]
    fn documents_without_dollars_are_returned_untouched() {
        let content = "# comment kept\nservices:\n  app:\n    image: nginx\n";
        let resolved = interpolate_document("compose file", content, &HashMap::new()).unwrap();
        assert!(!resolved.changed);
        assert_eq!(resolved.content, content);
    }

    #[test]
    fn unset_variables_are_reported_but_not_fatal() {
        let content = "services:\n  app:\n    image: nginx\n    environment:\n      - A=${A}\n";
        let resolved = interpolate_document("compose file", content, &HashMap::new()).unwrap();
        assert!(resolved.unset_variables.contains("A"));
    }

    #[test]
    fn tags_survive_interpolation() {
        let content = "services:\n  app:\n    image: nginx\n    ports: !override\n      - \"${PORT:-8080}:80\"\n";
        let resolved = interpolate_document("compose file", content, &HashMap::new()).unwrap();
        assert!(resolved.content.contains("!override"));
        assert!(resolved.content.contains("8080:80"));
    }

    #[test]
    fn dotenv_parsing_matches_compose_forms() {
        let parsed = parse_dotenv(
            "# comment\nexport A=1\nB = two # trailing\nC='lit $A'\nD=\"x\\ny $A\"\nE=${A}-e\nnot a line\nF=\"multi\nline\"\n",
        );
        assert_eq!(parsed["A"], "1");
        assert_eq!(parsed["B"], "two");
        assert_eq!(parsed["C"], "lit $A");
        assert_eq!(parsed["D"], "x\ny 1");
        assert_eq!(parsed["E"], "1-e");
        assert_eq!(parsed["F"], "multi\nline");
        assert_eq!(parsed.len(), 6);
    }
}
