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

use std::collections::{BTreeSet, HashMap};
use thiserror::Error;
use yaml_serde::Value;

/// Longest location path copied into an error. Locations are built from
/// repository keys, never values, but a pathological key could be large.
const MAX_LOCATION_BYTES: usize = 256;

/// Deepest `${A:-${B:-...}}` nesting that is resolved before failing closed.
const MAX_NESTING_DEPTH: usize = 16;

/// Largest repository `.env` read for interpolation.
pub const MAX_DOTENV_BYTES: usize = 1024 * 1024;

/// Most bytes substitution may produce for one document or one `.env` file.
/// `A2=${A1}${A1}`, `A3=${A2}${A2}`, ... doubles with every line, so a file
/// of a few hundred bytes could otherwise allocate gigabytes; the input size
/// limit alone does not bound the output.
pub const MAX_INTERPOLATED_BYTES: usize = 4 * 1024 * 1024;

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

    #[error(
        "Compose interpolation at {location} expands to more than {limit} bytes; variables that \
         repeat other variables must not grow without bound"
    )]
    TooLarge { location: String, limit: usize },
}

/// Variables visible to a substitution: `primary` first, then `fallback`.
#[derive(Clone, Copy)]
struct Lookup<'a> {
    primary: &'a HashMap<String, String>,
    fallback: Option<&'a HashMap<String, String>>,
}

impl<'a> Lookup<'a> {
    fn get(&self, name: &str) -> Option<&'a String> {
        self.primary
            .get(name)
            .or_else(|| self.fallback.and_then(|fallback| fallback.get(name)))
    }
}

/// Bookkeeping shared by one substitution: names seen, names unset, and the
/// bytes still allowed.
struct Substitution<'a> {
    variables: Lookup<'a>,
    location: &'a str,
    unset: &'a mut BTreeSet<String>,
    referenced: &'a mut BTreeSet<String>,
    /// When present, unset required variables are recorded here and
    /// substituted as empty instead of failing.
    deferred: Option<&'a mut BTreeSet<String>>,
    /// Inside a service the deployment excludes: an unsatisfied required
    /// variable resolves to its current value (empty) instead of failing,
    /// because that service is removed before anything runs.
    ignore_required: bool,
    remaining: usize,
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
    /// Required variables (`${VAR:?}` / `${VAR?}`) that were unset and
    /// substituted as empty because the caller deferred them; see
    /// [`interpolate_document_deferring_required`].
    pub deferred_required: BTreeSet<String>,
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
    interpolate(document, content, variables, false, &[])
}

/// [`interpolate_document`] for checks that run without the deployment's
/// generated variables (linked services, secrets bindings, deployment
/// tokens). A required variable that is not set at all is recorded in
/// [`InterpolatedDocument::deferred_required`] and substituted as empty
/// instead of failing, because the deployment may supply it. A required
/// variable that is set but empty still fails: an explicit project value
/// overrides every generated one. Deployments never use this.
pub fn interpolate_document_deferring_required(
    document: &str,
    content: &str,
    variables: &HashMap<String, String>,
) -> Result<InterpolatedDocument, InterpolationError> {
    interpolate(document, content, variables, true, &[])
}

/// Interpolate a document whose `services` are project services, of which
/// `excluded_services` will be removed before anything is validated or run.
/// Their values are still substituted, so the resolved document lists every
/// service, but nothing in them can fail the resolution or be reported: an
/// unsatisfied required variable resolves to its current (empty) value, and
/// their variables are not counted as referenced, unset or deferred.
pub fn interpolate_project_document(
    document: &str,
    content: &str,
    variables: &HashMap<String, String>,
    defer_required: bool,
    excluded_services: &[String],
) -> Result<InterpolatedDocument, InterpolationError> {
    interpolate(
        document,
        content,
        variables,
        defer_required,
        excluded_services,
    )
}

fn interpolate(
    document: &str,
    content: &str,
    variables: &HashMap<String, String>,
    defer_required: bool,
    excluded_services: &[String],
) -> Result<InterpolatedDocument, InterpolationError> {
    if !content.contains('$') {
        return Ok(InterpolatedDocument {
            content: content.to_string(),
            changed: false,
            unset_variables: BTreeSet::new(),
            referenced_variables: BTreeSet::new(),
            deferred_required: BTreeSet::new(),
        });
    }
    let mut root: Value =
        yaml_serde::from_str(content).map_err(|error| InterpolationError::Yaml {
            document: document.to_string(),
            reason: error.to_string(),
        })?;
    let mut state = WalkState {
        variables,
        unset: BTreeSet::new(),
        referenced: BTreeSet::new(),
        changed: false,
        remaining: MAX_INTERPOLATED_BYTES,
        defer_required,
        deferred: BTreeSet::new(),
        excluded_services,
        in_excluded_service: false,
    };
    let mut location = String::new();
    state.walk(&mut root, &mut location)?;
    if !state.changed {
        return Ok(InterpolatedDocument {
            content: content.to_string(),
            changed: false,
            unset_variables: state.unset,
            referenced_variables: state.referenced,
            deferred_required: state.deferred,
        });
    }
    let content = yaml_serde::to_string(&root).map_err(|error| InterpolationError::Yaml {
        document: document.to_string(),
        reason: format!("failed to serialize the resolved document: {error}"),
    })?;
    Ok(InterpolatedDocument {
        content,
        changed: true,
        unset_variables: state.unset,
        referenced_variables: state.referenced,
        deferred_required: state.deferred,
    })
}

struct WalkState<'a> {
    variables: &'a HashMap<String, String>,
    unset: BTreeSet<String>,
    referenced: BTreeSet<String>,
    changed: bool,
    /// Bytes substitution may still produce in this document.
    remaining: usize,
    defer_required: bool,
    deferred: BTreeSet<String>,
    /// See [`interpolate_project_document`].
    excluded_services: &'a [String],
    in_excluded_service: bool,
}

impl WalkState<'_> {
    fn walk(&mut self, value: &mut Value, location: &mut String) -> Result<(), InterpolationError> {
        match value {
            Value::String(text) => {
                if !text.contains('$') {
                    return Ok(());
                }
                let mut substitution = Substitution {
                    variables: Lookup {
                        primary: self.variables,
                        fallback: None,
                    },
                    location,
                    unset: &mut self.unset,
                    referenced: &mut self.referenced,
                    deferred: self.defer_required.then_some(&mut self.deferred),
                    ignore_required: self.in_excluded_service,
                    remaining: self.remaining,
                };
                let resolved = substitute(text, &mut substitution, 0)?;
                self.remaining = substitution.remaining;
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
                    let excluded = !self.in_excluded_service
                        && location.len() == "services.".len() + key.len()
                        && location.starts_with("services.")
                        && self.excluded_services.contains(&key);
                    if excluded {
                        self.walk_excluded_service(item, location)?;
                    } else {
                        self.walk(item, location)?;
                    }
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

    /// Walk a service the deployment excludes, keeping what it records out
    /// of the document's results.
    fn walk_excluded_service(
        &mut self,
        value: &mut Value,
        location: &mut String,
    ) -> Result<(), InterpolationError> {
        let unset = std::mem::take(&mut self.unset);
        let referenced = std::mem::take(&mut self.referenced);
        let deferred = std::mem::take(&mut self.deferred);
        self.in_excluded_service = true;
        let result = self.walk(value, location);
        self.in_excluded_service = false;
        self.unset = unset;
        self.referenced = referenced;
        self.deferred = deferred;
        result
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
    state: &mut Substitution<'_>,
    depth: usize,
) -> Result<String, InterpolationError> {
    if depth > MAX_NESTING_DEPTH {
        return Err(InterpolationError::InvalidSyntax {
            location: location_label(state.location),
            reason: format!("variable defaults are nested more than {MAX_NESTING_DEPTH} levels"),
        });
    }
    let chars: Vec<char> = input.chars().collect();
    let mut output = String::with_capacity(input.len().min(state.remaining));
    let mut index = 0;
    while index < chars.len() {
        let ch = chars[index];
        if ch != '$' {
            push_bounded(&mut output, ch.encode_utf8(&mut [0; 4]), state)?;
            index += 1;
            continue;
        }
        match chars.get(index + 1).copied() {
            Some('$') => {
                push_bounded(&mut output, "$", state)?;
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
                        location: location_label(state.location),
                        reason: "'${' has no closing '}'".to_string(),
                    });
                }
                let body: String = chars[body_start..cursor].iter().collect();
                let resolved = resolve_braced(&body, state, depth)?;
                push_bounded(&mut output, &resolved, state)?;
                index = cursor + 1;
            }
            Some(next) if is_name_start(next) => {
                let mut cursor = index + 1;
                while cursor < chars.len() && is_name_char(chars[cursor]) {
                    cursor += 1;
                }
                let name: String = chars[index + 1..cursor].iter().collect();
                state.referenced.insert(name.clone());
                match state.variables.get(&name) {
                    Some(value) => push_bounded(&mut output, value, state)?,
                    None => {
                        state.unset.insert(name);
                    }
                }
                index = cursor;
            }
            _ => {
                push_bounded(&mut output, "$", state)?;
                index += 1;
            }
        }
    }
    Ok(output)
}

/// Append `text`, failing once the substitution budget is spent. Nested
/// defaults are charged when they are produced and again when copied into
/// their parent, which only makes the bound stricter.
fn push_bounded(
    output: &mut String,
    text: &str,
    state: &mut Substitution<'_>,
) -> Result<(), InterpolationError> {
    if text.len() > state.remaining {
        return Err(InterpolationError::TooLarge {
            location: location_label(state.location),
            limit: MAX_INTERPOLATED_BYTES,
        });
    }
    state.remaining -= text.len();
    output.push_str(text);
    Ok(())
}

fn resolve_braced(
    body: &str,
    state: &mut Substitution<'_>,
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
            location: location_label(state.location),
            reason: "'${...}' must start with a variable name".to_string(),
        });
    }
    let (name, modifier) = body.split_at(name_len);
    state.referenced.insert(name.to_string());
    let value = state.variables.get(name);
    let set = value.is_some();
    let non_empty = value.is_some_and(|value| !value.is_empty());
    let current = value.cloned().unwrap_or_default();

    let required_missing = |location: &str| InterpolationError::RequiredVariableMissing {
        variable: name.to_string(),
        location: location_label(location),
    };

    if modifier.is_empty() {
        if !set {
            state.unset.insert(name.to_string());
        }
        return Ok(current);
    }
    if let Some(default) = modifier.strip_prefix(":-") {
        return if non_empty {
            Ok(current)
        } else {
            substitute(default, state, depth + 1)
        };
    }
    if let Some(default) = modifier.strip_prefix('-') {
        return if set {
            Ok(current)
        } else {
            substitute(default, state, depth + 1)
        };
    }
    if modifier.starts_with(":?") || modifier.starts_with('?') {
        let satisfied = if modifier.starts_with(':') {
            non_empty
        } else {
            set
        };
        if satisfied || state.ignore_required {
            return Ok(current);
        }
        if !set {
            if let Some(deferred) = state.deferred.as_deref_mut() {
                deferred.insert(name.to_string());
                return Ok(String::new());
            }
        }
        return Err(required_missing(state.location));
    }
    if let Some(replacement) = modifier.strip_prefix(":+") {
        return if non_empty {
            substitute(replacement, state, depth + 1)
        } else {
            Ok(String::new())
        };
    }
    if let Some(replacement) = modifier.strip_prefix('+') {
        return if set {
            substitute(replacement, state, depth + 1)
        } else {
            Ok(String::new())
        };
    }
    Err(InterpolationError::InvalidSyntax {
        location: location_label(state.location),
        reason: format!(
            "unsupported modifier after variable {name}; use ':-', '-', ':?', '?', ':+' or '+'"
        ),
    })
}

/// Parse a dotenv file the way Compose reads `.env` for interpolation.
///
/// Supports comments, `export KEY=...`, single-quoted literals (with `\'` for
/// a quote), double-quoted values with escapes (both may span lines),
/// unquoted values with trailing ` # comments`, and `${VAR}` references to
/// keys defined earlier in the same file. A malformed line is skipped rather
/// than failing the deployment: Compose's own parser is the authority on the
/// file at runtime, and Temps only needs these values to resolve the Compose
/// document. Expansion is bounded by [`MAX_INTERPOLATED_BYTES`] for the whole
/// file.
pub fn parse_dotenv(content: &str) -> Result<HashMap<String, String>, InterpolationError> {
    parse_dotenv_with(content, &HashMap::new())
}

/// [`parse_dotenv`] where `${VAR}` references see `environment` first and
/// keys defined earlier in the file second, as Compose resolves an
/// `include` entry's `env_file` against the including project's variables.
pub fn parse_dotenv_with(
    content: &str,
    environment: &HashMap<String, String>,
) -> Result<HashMap<String, String>, InterpolationError> {
    parse_dotenv_layered(content, environment, HashMap::new())
}

/// [`parse_dotenv_with`] for the next file of an ordered `env_file` list.
/// `earlier` holds what the files before it loaded: `${VAR}` references see
/// `environment` first, then `earlier` and this file's own earlier keys, and
/// the result is `earlier` overridden by this file. This is how Compose
/// layers an `include` entry's `env_file` list, so a later file may build on
/// a value an earlier one set.
pub fn parse_dotenv_layered(
    content: &str,
    environment: &HashMap<String, String>,
    earlier: HashMap<String, String>,
) -> Result<HashMap<String, String>, InterpolationError> {
    let mut values = earlier;
    let mut remaining = MAX_INTERPOLATED_BYTES;
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
                let mut value = String::new();
                while index < chars.len() && chars[index] != '\'' {
                    // `\'` is a quote inside the value, as in Compose.
                    if chars[index] == '\\' && chars.get(index + 1) == Some(&'\'') {
                        value.push('\'');
                        index += 2;
                        continue;
                    }
                    value.push(chars[index]);
                    index += 1;
                }
                index += 1;
                skip_to_line_end(&chars, &mut index);
                charge_dotenv(&mut remaining, value.len())?;
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
                expand_dotenv_value(&raw, environment, &values, &mut remaining)?
            }
            _ => {
                let start = index;
                skip_to_line_end(&chars, &mut index);
                let line: String = chars[start..index].iter().collect();
                let without_comment = match line.find(" #").or_else(|| line.find("\t#")) {
                    Some(position) => &line[..position],
                    None => line.as_str(),
                };
                expand_dotenv_value(without_comment.trim(), environment, &values, &mut remaining)?
            }
        };
        values.insert(key, value);
    }
    Ok(values)
}

fn charge_dotenv(remaining: &mut usize, bytes: usize) -> Result<(), InterpolationError> {
    *remaining = remaining
        .checked_sub(bytes)
        .ok_or_else(|| InterpolationError::TooLarge {
            location: ".env".to_string(),
            limit: MAX_INTERPOLATED_BYTES,
        })?;
    Ok(())
}

fn skip_to_line_end(chars: &[char], index: &mut usize) {
    while *index < chars.len() && chars[*index] != '\n' {
        *index += 1;
    }
}

/// Expand one value, charging what it produces to the file's budget. A
/// malformed template keeps its raw text (Compose decides at runtime); an
/// expansion over budget fails the whole file.
fn expand_dotenv_value(
    raw: &str,
    environment: &HashMap<String, String>,
    defined: &HashMap<String, String>,
    remaining: &mut usize,
) -> Result<String, InterpolationError> {
    if !raw.contains('$') {
        charge_dotenv(remaining, raw.len())?;
        return Ok(raw.to_string());
    }
    let mut unset = BTreeSet::new();
    let mut referenced = BTreeSet::new();
    let mut state = Substitution {
        variables: Lookup {
            primary: environment,
            fallback: Some(defined),
        },
        location: ".env",
        unset: &mut unset,
        referenced: &mut referenced,
        deferred: None,
        ignore_required: false,
        remaining: *remaining,
    };
    match substitute(raw, &mut state, 0) {
        Ok(value) => {
            *remaining = state.remaining;
            Ok(value)
        }
        Err(error @ InterpolationError::TooLarge { .. }) => Err(error),
        Err(_) => {
            charge_dotenv(remaining, raw.len())?;
            Ok(raw.to_string())
        }
    }
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
        let mut state = Substitution {
            variables: Lookup {
                primary: variables,
                fallback: None,
            },
            location: "services.app.image",
            unset: &mut unset,
            referenced: &mut referenced,
            deferred: None,
            ignore_required: false,
            remaining: MAX_INTERPOLATED_BYTES,
        };
        substitute(input, &mut state, 0)
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
        let value: Value = yaml_serde::from_str(&resolved.content).unwrap();
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
        let value: Value = yaml_serde::from_str(&resolved.content).unwrap();
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
        )
        .unwrap();
        assert_eq!(parsed["A"], "1");
        assert_eq!(parsed["B"], "two");
        assert_eq!(parsed["C"], "lit $A");
        assert_eq!(parsed["D"], "x\ny 1");
        assert_eq!(parsed["E"], "1-e");
        assert_eq!(parsed["F"], "multi\nline");
        assert_eq!(parsed.len(), 6);
    }

    #[test]
    fn single_quoted_dotenv_values_keep_escaped_quotes() {
        let parsed = parse_dotenv("PASSWORD='Let\\'sGo!'\nNEXT=after\nPLAIN='a\\b'\n").unwrap();
        assert_eq!(parsed["PASSWORD"], "Let'sGo!");
        assert_eq!(parsed["NEXT"], "after");
        // Any other backslash stays literal in a single-quoted value.
        assert_eq!(parsed["PLAIN"], "a\\b");
    }

    #[test]
    fn dotenv_expansion_that_doubles_per_line_is_bounded() {
        let mut content = String::from("A0=xxxxxxxxxxxxxxxx\n");
        for line in 1..40 {
            content.push_str(&format!("A{line}=${{A{}}}${{A{}}}\n", line - 1, line - 1));
        }
        assert!(content.len() < 1024);
        let error = parse_dotenv(&content).unwrap_err();
        assert!(
            matches!(error, InterpolationError::TooLarge { .. }),
            "{error}"
        );
    }

    #[test]
    fn document_values_repeating_a_large_variable_are_bounded() {
        let variables = vars(&[("BIG", &"x".repeat(1024 * 1024))]);
        let content =
            "services:\n  app:\n    image: nginx\n    command: ${BIG}${BIG}${BIG}${BIG}${BIG}\n";
        let error = interpolate_document("compose file", content, &variables).unwrap_err();
        assert!(
            matches!(error, InterpolationError::TooLarge { ref location, .. } if location == "services.app.command")
        );
    }

    #[test]
    fn later_env_files_build_on_values_earlier_files_set() {
        let first = parse_dotenv_layered(
            "BASE=from-first\nKEEP=first\n",
            &HashMap::new(),
            HashMap::new(),
        )
        .expect("first file parses");
        let layered = parse_dotenv_layered(
            "RESULT=${BASE}-second\nKEEP=second\nPARENT=${OWNER}\n",
            &vars(&[("OWNER", "parent"), ("BASE", "from-parent")]),
            first,
        )
        .expect("second file parses");
        // The including environment is consulted first, as in Compose.
        assert_eq!(layered["RESULT"], "from-parent-second");
        let layered = parse_dotenv_layered(
            "RESULT=${BASE}-second\nKEEP=second\n",
            &vars(&[("OWNER", "parent")]),
            parse_dotenv("BASE=from-first\nKEEP=first\n").expect("first file parses"),
        )
        .expect("second file parses");
        assert_eq!(layered["RESULT"], "from-first-second");
        assert_eq!(layered["BASE"], "from-first");
        // Later files override earlier ones.
        assert_eq!(layered["KEEP"], "second");
    }

    #[test]
    fn include_env_files_see_the_including_environment_first() {
        let parent = vars(&[("REGION", "eu")]);
        let parsed = parse_dotenv_with("REGION=us\nHOST=api.${REGION}\n", &parent).unwrap();
        assert_eq!(parsed["HOST"], "api.eu");
    }

    #[test]
    fn deferred_checks_record_unset_required_variables_only() {
        let content = "services:\n  app:\n    image: nginx\n    environment:\n      DB: ${POSTGRES_URL:?database required}\n";
        let strict = interpolate_document("compose file", content, &HashMap::new()).unwrap_err();
        assert!(matches!(
            strict,
            InterpolationError::RequiredVariableMissing { .. }
        ));
        let deferred =
            interpolate_document_deferring_required("compose file", content, &HashMap::new())
                .unwrap();
        assert!(deferred.deferred_required.contains("POSTGRES_URL"));
        // An explicit empty value overrides any generated one, so it still fails.
        let empty = vars(&[("POSTGRES_URL", "")]);
        assert!(matches!(
            interpolate_document_deferring_required("compose file", content, &empty),
            Err(InterpolationError::RequiredVariableMissing { .. })
        ));
    }

    #[test]
    fn excluded_services_never_fail_or_report_their_variables() {
        let content = "services:\n  app:\n    image: nginx:${TAG}\n  db:\n    image: postgres:${DB_TAG}\n    environment:\n      POSTGRES_PASSWORD: ${DB_PASSWORD:?required}\n      EMPTY: ${DB_EMPTY:?required}\nx-db:\n  password: ${OTHER:?required}\n";
        let variables = vars(&[("TAG", "1"), ("DB_EMPTY", "")]);
        let excluded = vec!["db".to_string()];
        // A top-level extension is not a service: it still fails.
        assert!(matches!(
            interpolate_project_document("compose file", content, &variables, false, &excluded),
            Err(InterpolationError::RequiredVariableMissing { variable, .. }) if variable == "OTHER"
        ));
        let content = content.split("x-db:").next().unwrap_or_default();
        for defer in [false, true] {
            let resolved =
                interpolate_project_document("compose file", content, &variables, defer, &excluded)
                    .unwrap();
            assert!(resolved.content.contains("nginx:1"));
            // The excluded service is still listed for discovery.
            assert!(resolved.content.contains("postgres:"));
            assert_eq!(
                resolved.referenced_variables,
                vars(&[("TAG", "")]).into_keys().collect()
            );
            assert!(resolved.unset_variables.is_empty());
            assert!(resolved.deferred_required.is_empty());
        }
        // Without the exclusion the same service fails deployment.
        assert!(matches!(
            interpolate_project_document("compose file", content, &variables, false, &[]),
            Err(InterpolationError::RequiredVariableMissing { .. })
        ));
        // A service merely named like an excluded one elsewhere is not exempt.
        let nested = "services:\n  app:\n    image: nginx\n    labels:\n      db: ${DB_PASSWORD:?required}\n";
        assert!(
            interpolate_project_document("compose file", nested, &variables, false, &excluded)
                .is_err()
        );
    }
}
