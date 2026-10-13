// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A virtual, read-only CLI over the API index.
//!
//! LLMs are far more fluent with `tool subcommand --flag value` and `--help`
//! than with a bespoke search/describe/call tool trio. This module turns the
//! [`ReadOnlyApiIndex`] into exactly that shape:
//!
//! ```text
//! <root> --help                       → list sections (grouped by OpenAPI tag)
//! <section> --help                    → list the operations in a section
//! <section> <operation_id> --help     → an operation's description + flags
//! <section> <operation_id> --flag v   → execute (via InternalApiCaller::call)
//! ```
//!
//! Operations keep their real `operation_id` as the command name; sections are
//! the operations' first OpenAPI tag. Discovery is `--help`-driven and the help
//! text is built from the same summaries/descriptions/params the index already
//! holds — so the disambiguating prose ("…replaces the old stages endpoint")
//! shows up exactly where a CLI user would look for it.
//!
//! This module is pure (parsing + help rendering); execution lives in
//! [`crate::InternalApiCaller::run_cli`], which feeds an [`CliAction::Execute`]
//! into the existing router-replay path (so auth scoping, `project_id`
//! auto-fill, pagination clamps, and the allowlist all still apply).

use serde_json::Value;

use crate::caller::is_project_scope_param;
use crate::index::{ApiOperation, ParamLocation, ParamSpec, ReadOnlyApiIndex};

/// Which tool a command line is parsed for. The two tools share one parser but
/// not one voice: the read tool's help must not claim the write tool is
/// read-only, and only the read tool should steer mutations to `temps_write`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CliMode {
    /// `temps`: GET (and vetted read-only POST) operations.
    ReadOnly,
    /// `temps_write`: propose-then-confirm state-changing operations.
    Write,
}

/// The outcome of parsing one CLI command line.
pub(crate) enum CliAction<'a> {
    /// Help or an error — already-rendered text to return to the model verbatim.
    Terminal(String),
    /// A resolved operation to execute with the given flat parameter object.
    Execute(&'a ApiOperation, Value),
}

/// A CLI section: a group of operations sharing an OpenAPI tag.
struct Section<'a> {
    slug: String,
    name: String,
    description: Option<String>,
    operations: Vec<&'a ApiOperation>,
}

/// Parse a command line and resolve it to a [`CliAction`]. Never executes.
///
/// `permit` filters which operations are *discoverable* (shown in `--help` /
/// section listings) — operations the caller can't read are hidden, and a
/// section with no discoverable operations disappears. It is advisory: direct
/// execution still goes through the router's `permission_guard!`, so a bare
/// `operation_id` is resolved for execution even if hidden from discovery.
pub(crate) fn resolve<'a>(
    index: &'a ReadOnlyApiIndex,
    command: &str,
    permit: &dyn Fn(&ApiOperation) -> bool,
    mode: CliMode,
) -> CliAction<'a> {
    let tokens = tokenize(command);
    // Tolerate a leading program name — the model may write `temps deploy …`.
    let tokens: &[String] = if tokens
        .first()
        .map(|t| t.eq_ignore_ascii_case("temps"))
        .unwrap_or(false)
    {
        &tokens[1..]
    } else {
        &tokens
    };

    // Root: no args or `--help`.
    if tokens.is_empty() || is_help(&tokens[0]) {
        return CliAction::Terminal(render_root_help(index, permit, mode));
    }

    let first = &tokens[0];
    let secs = sections(index, permit);

    // A section?
    if let Some(sec) = secs
        .iter()
        .find(|s| s.slug.eq_ignore_ascii_case(first) || s.name.eq_ignore_ascii_case(first))
    {
        // `<section>` or `<section> --help` → list operations.
        if tokens.len() == 1 || is_help(&tokens[1]) {
            return CliAction::Terminal(render_section_help(sec));
        }
        // `<section> <operation> …`
        let op_name = &tokens[1];
        return match sec
            .operations
            .iter()
            .find(|o| o.operation_id.eq_ignore_ascii_case(op_name))
        {
            Some(op) => resolve_operation(op, &tokens[2..]),
            None => CliAction::Terminal(format!(
                "Unknown operation '{op_name}' in section '{}'.{}\n\n{}",
                sec.slug,
                mutation_redirect_for(mode, op_name),
                render_section_help(sec),
            )),
        };
    }

    // Leniency: accept a bare `operation_id` (it's globally unique) — but apply
    // the same discovery filter as the section path. Without this, naming an
    // operation directly skipped `permit` entirely, so the "operations you
    // cannot read are not offered" property held only for callers who browsed
    // to them. The router's `permission_guard!` was still the real boundary, so
    // nothing was exploitable; this just stops a request being formed that
    // should never have been.
    if let Some(op) = index.get(first) {
        if permit(op) {
            return resolve_operation(op, &tokens[1..]);
        }
    }

    CliAction::Terminal(format!(
        "Unknown command '{first}'. Run `--help` to list sections.{}{}",
        mutation_redirect_for(mode, first),
        suggest_operations(&secs, first),
    ))
}

/// Fuzzy discovery aid: when the model names an unknown section or operation
/// (e.g. `environments`, because the env ops are tagged "Projects" and live under
/// the `projects` section), list the real operations whose id or path contains
/// that token so the model can find them without guessing the section. Returns a
/// trailing block (leading blank line) or "" when nothing matches.
fn suggest_operations(secs: &[Section], token: &str) -> String {
    let lower = token.to_ascii_lowercase();
    // Also match the singular ("environments" → "environment").
    let singular = lower.strip_suffix('s').unwrap_or(&lower);
    let needles: [&str; 2] = [lower.as_str(), singular];

    let mut hits: Vec<String> = Vec::new();
    for sec in secs {
        for op in &sec.operations {
            let id = op.operation_id.to_ascii_lowercase();
            let path = op.path.to_ascii_lowercase();
            if needles.iter().any(|n| id.contains(n) || path.contains(n)) {
                hits.push(format!("  {} {}", sec.slug, op.operation_id));
                if hits.len() >= 8 {
                    break;
                }
            }
        }
        if hits.len() >= 8 {
            break;
        }
    }

    if hits.is_empty() {
        return String::new();
    }
    format!(
        "\n\nNo '{token}' section — but these operations match (run `<section> \
         <operation> --help`):\n{}",
        hits.join("\n")
    )
}

/// The redirect applies to the read-only tool only: the write tool is already
/// where mutations are proposed.
fn mutation_redirect_for(mode: CliMode, op_name: &str) -> String {
    match mode {
        CliMode::ReadOnly => mutation_redirect(op_name),
        CliMode::Write => String::new(),
    }
}

/// The read-only `temps` tool exposes only GET endpoints, so any request to
/// *change* state dead-ends here. When the operation the model reached for
/// looks like a mutation, steer it to the confirm-gated `temps_write` tool
/// instead of leaving it to loop on read-only `--help` probes. Returns a
/// trailing sentence (leading space) to append to the error, or "" otherwise.
fn mutation_redirect(op_name: &str) -> String {
    let lower = op_name.to_ascii_lowercase();
    // operationIds are `verb_noun`, so match only the LEADING verb — this
    // distinguishes the mutation `deploy`/`promote_deployment` from the read
    // noun in `get_deployment`/`list_deployments`. Read verbs (get/list/query/…)
    // are simply absent from this set.
    const MUTATION_VERBS: &[&str] = &[
        "create",
        "update",
        "delete",
        "remove",
        "add",
        "set",
        "promote",
        "rollback",
        "restart",
        "stop",
        "start",
        "pause",
        "resume",
        "cancel",
        "trigger",
        "deploy",
        "redeploy",
        "wake",
        "sleep",
        "scale",
        "rename",
        "enable",
        "disable",
        "rotate",
        "reset",
        "install",
        "uninstall",
    ];
    let leading_verb = lower.split('_').next().unwrap_or(&lower);
    let looks_mutating = MUTATION_VERBS.contains(&leading_verb);
    if looks_mutating {
        " This looks like a mutation — the read-only `temps` tool cannot change \
         state. Propose it with the `temps_write` tool instead (e.g. \
         `temps_write` with command `<operation> --help`), then let the user confirm."
            .to_string()
    } else {
        String::new()
    }
}

/// Resolve the tail of `… <operation_id> [tail]` — either help or execution.
fn resolve_operation<'a>(op: &'a ApiOperation, tail: &[String]) -> CliAction<'a> {
    if tail.first().map(|t| is_help(t)).unwrap_or(false) {
        return CliAction::Terminal(render_operation_help(op));
    }
    match parse_flags(tail, Some(op)) {
        Ok(params) => CliAction::Execute(op, params),
        Err(e) => CliAction::Terminal(e),
    }
}

// ---------------------------------------------------------------------------
// Help rendering
// ---------------------------------------------------------------------------

fn render_root_help(
    index: &ReadOnlyApiIndex,
    permit: &dyn Fn(&ApiOperation) -> bool,
    mode: CliMode,
) -> String {
    let secs = sections(index, permit);
    let title = match mode {
        CliMode::ReadOnly => "Temps read-only API — a CLI over the platform's GET endpoints.",
        CliMode::Write => {
            "Temps write API — a CLI over the platform's state-changing endpoints \
             (POST/PUT/PATCH/DELETE). A command is only proposed here: it runs after the \
             user confirms it."
        }
    };
    let mut out = format!(
        "{title}\n\
         Usage: <section> <operation> [--flag value ...]\n\
         Run `<section> --help` to list a section's operations, or \
         `<section> <operation> --help` for an operation's flags. `project_id` is auto-filled \
         for the current project.\n\nSections:\n",
    );
    let width = secs.iter().map(|s| s.slug.len()).max().unwrap_or(0);
    for s in &secs {
        let desc = s
            .description
            .as_deref()
            .map(first_line)
            .unwrap_or_else(|| format!("{} operation(s)", s.operations.len()));
        out.push_str(&format!("  {:<width$}  {desc}\n", s.slug, width = width));
    }
    out
}

fn render_section_help(sec: &Section) -> String {
    let mut out = format!("Section: {}", sec.slug);
    if let Some(d) = &sec.description {
        out.push_str(&format!(" — {}", first_line(d)));
    }
    out.push('\n');
    out.push_str(&format!(
        "Operations (run `{} <operation> --help` for flags):\n",
        sec.slug
    ));
    let width = sec
        .operations
        .iter()
        .map(|o| o.operation_id.len())
        .max()
        .unwrap_or(0);
    for op in &sec.operations {
        // Fall back to the description when there's no explicit summary — many
        // operations carry only a doc-comment (surfaced as `description`), and
        // an empty blurb is what leads a model to pick an operation by its name
        // alone (e.g. `promote_deployment` for a "redeploy" request).
        let summary = op
            .summary
            .as_deref()
            .or(op.description.as_deref())
            .map(first_line)
            .unwrap_or_default();
        out.push_str(&format!(
            "  {:<width$}  {summary}\n",
            op.operation_id,
            width = width
        ));
    }
    out
}

fn render_operation_help(op: &ApiOperation) -> String {
    let mut out = format!("{} — {} {}\n", op.operation_id, op.method, op.path);
    if let Some(s) = &op.summary {
        out.push_str(s);
        out.push('\n');
    }
    if let Some(d) = &op.description {
        if !d.trim().is_empty() && Some(d) != op.summary.as_ref() {
            out.push_str(d);
            out.push('\n');
        }
    }

    let project_selector = op
        .params
        .iter()
        .find(|param| is_project_scope_param(op, param));
    // Hide the context-supplied project selector — the model must not put it
    // inside the virtual command.
    let flags: Vec<&_> = op
        .params
        .iter()
        .filter(|p| !is_project_scope_param(op, p))
        .collect();
    if flags.is_empty() {
        out.push_str("Flags: none\n");
    } else {
        out.push_str("Flags:\n");
        for p in flags {
            // Path params are structurally required, and so are body fields the
            // schema marks required — a write call fails without them, so the
            // help has to say which ones they are. (Query params stay unmarked:
            // they're optional filters, and OpenAPI required-ness is frequently
            // wrong for them, so claiming "required" would send the model
            // inventing values for filters it should simply omit.)
            let req = match p.location {
                ParamLocation::Path => " (required)",
                ParamLocation::Body if p.required => " (required)",
                _ => "",
            };
            out.push_str(&format!("  --{} {}{}", p.name, flag_value_label(p), req));
            if !p.enum_values.is_empty() {
                out.push_str(&format!(" — one of: {}", p.enum_values.join(", ")));
            }
            // For an object-valued field, the accepted JSON is the single most
            // useful thing we can say — a bare "<object>" tells the caller
            // nothing, and guessing it wrong fails the whole call.
            if let Some(shape) = &p.object_shape {
                out.push_str(&format!(" — {}", shape.describe()));
            }
            if let Some(d) = &p.description {
                out.push_str(&format!(" — {d}"));
            }
            out.push('\n');
        }
    }
    if op
        .params
        .iter()
        .any(|p| p.ty == "array" && !is_project_scope_param(op, p))
    {
        out.push_str(
            "(List flags take comma-separated values, a JSON array, or the flag repeated: \
             `--ids 1,2`, `--ids '[1,2]'`, `--ids 1 --ids 2`.)\n",
        );
    }
    if let Some(selector) = project_selector {
        if matches!(selector.location, ParamLocation::Body) && !selector.required {
            out.push_str(
                "(project_id is optional: it is supplied by the selected tool context when one exists; otherwise the resource remains unlinked.)\n",
            );
        } else {
            out.push_str("(project_id is supplied by the selected tool context.)\n");
        }
    }
    out
}

/// The value placeholder shown after a flag in `--help`: `<string>` for a
/// scalar, `<int>[,<int>...]` for a list of integers, and so on.
fn flag_value_label(p: &ParamSpec) -> String {
    if p.ty != "array" {
        return format!("<{}>", p.ty);
    }
    let item = match p.item_ty.as_deref() {
        Some("integer") => "int",
        Some("number") => "number",
        Some("string") => "string",
        Some("boolean") => "bool",
        Some("object") => return "<json array of objects>".to_string(),
        Some("array") => return "<json array of arrays>".to_string(),
        _ => return "<json array>".to_string(),
    };
    format!("<{item}>[,<{item}>...]")
}

// ---------------------------------------------------------------------------
// Section grouping
// ---------------------------------------------------------------------------

/// Group operations into sections by their first OpenAPI tag, preserving a
/// stable (slug-sorted) order. Operations with no tag fall into "General".
fn sections<'a>(
    index: &'a ReadOnlyApiIndex,
    permit: &dyn Fn(&ApiOperation) -> bool,
) -> Vec<Section<'a>> {
    let mut secs: Vec<Section> = Vec::new();
    for op in index.operations() {
        if !permit(op) {
            continue;
        }
        let name = section_name(op);
        if let Some(s) = secs.iter_mut().find(|s| s.name == name) {
            s.operations.push(op);
        } else {
            secs.push(Section {
                slug: slugify(&name),
                description: index.tag_description(&name).map(str::to_string),
                name,
                operations: vec![op],
            });
        }
    }
    secs.sort_by(|a, b| a.slug.cmp(&b.slug));
    secs
}

/// The section an operation belongs to: its first OpenAPI tag, unless that tag
/// is missing or a generic catch-all (`default`/`crate`) — in which case derive
/// one from the path so untagged endpoints still land in a meaningful section
/// (e.g. `/projects/{id}/deployments/…` → "Deployments") rather than a dumping
/// ground. Explicit tags always win; this is only a safety net.
fn section_name(op: &ApiOperation) -> String {
    match op.tags.first() {
        Some(t)
            if !t.trim().is_empty()
                && !t.eq_ignore_ascii_case("default")
                && !t.eq_ignore_ascii_case("crate") =>
        {
            t.clone()
        }
        _ => path_section(&op.path),
    }
}

/// Derive a Title-cased section name from the first meaningful path segment
/// (skipping the leading `/projects` and any `{param}` placeholders).
fn path_section(path: &str) -> String {
    let seg = path
        .split('/')
        .find(|s| !s.is_empty() && !s.starts_with('{') && *s != "projects")
        .unwrap_or("general");
    let mut chars = seg.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => "General".to_string(),
    }
}

fn slugify(name: &str) -> String {
    name.to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-")
}

// ---------------------------------------------------------------------------
// Parsing helpers
// ---------------------------------------------------------------------------

fn is_help(token: &str) -> bool {
    token == "--help" || token == "-h" || token.eq_ignore_ascii_case("help")
}

/// Whitespace-split into tokens, honouring single/double quotes so a flag value
/// may contain spaces: `--name "two words"`.
fn tokenize(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut started = false;
    let mut quote: Option<char> = None;
    for c in s.chars() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                } else {
                    cur.push(c);
                }
            }
            None => {
                if c == '"' || c == '\'' {
                    quote = Some(c);
                    started = true;
                } else if c.is_whitespace() {
                    if started {
                        out.push(std::mem::take(&mut cur));
                        started = false;
                    }
                } else {
                    cur.push(c);
                    started = true;
                }
            }
        }
    }
    if started {
        out.push(cur);
    }
    out
}

/// Parse `--name value` / `--name=value` / bare `--flag` tokens into a flat JSON
/// object. Values are coerced to JSON scalars and structured values when they
/// look like one, else string.
///
/// A flag whose schema type is an array takes every following bare token as a
/// value and may be repeated, so `--ids 1,2`, `--ids 1 2`, `--ids '[1,2]'` and
/// `--ids 1 --ids 2` all produce `[1,2]`.
fn parse_flags(tokens: &[String], operation: Option<&ApiOperation>) -> Result<Value, String> {
    let mut map = serde_json::Map::new();
    let mut i = 0;
    while i < tokens.len() {
        let t = &tokens[i];
        let Some(rest) = t.strip_prefix("--") else {
            return Err(format!(
                "Unexpected argument '{t}'. Flags must look like `--name value` or `--name=value`."
            ));
        };
        let (name, inline) = match rest.split_once('=') {
            Some((k, v)) => (k, Some(v)),
            None => (rest, None),
        };
        let spec = parameter_spec(operation, name);

        if spec.is_some_and(|p| p.ty == "array") {
            let mut raw: Vec<&str> = Vec::new();
            i += 1;
            match inline {
                Some(v) => raw.push(v),
                None => {
                    while i < tokens.len() && !tokens[i].starts_with("--") {
                        raw.push(&tokens[i]);
                        i += 1;
                    }
                }
            }
            if raw.is_empty() {
                return Err(format!(
                    "`--{name}` is a list and needs at least one value, for example \
                     `--{name} 1,2` or `--{name} '[1,2]'`."
                ));
            }
            let items = coerce_array(name, &raw, spec.and_then(|p| p.item_ty.as_deref()))?;
            match map.get_mut(name) {
                Some(Value::Array(existing)) => existing.extend(items),
                _ => {
                    map.insert(name.to_string(), Value::Array(items));
                }
            }
            continue;
        }

        if let Some(v) = inline {
            map.insert(
                name.to_string(),
                coerce(name, v, spec.map(|p| p.ty.as_str()))?,
            );
            i += 1;
        } else if i + 1 < tokens.len() && !tokens[i + 1].starts_with("--") {
            map.insert(
                name.to_string(),
                coerce(name, &tokens[i + 1], spec.map(|p| p.ty.as_str()))?,
            );
            i += 2;
        } else {
            // A flag with no value → boolean true (e.g. `--only_errors`).
            map.insert(name.to_string(), Value::Bool(true));
            i += 1;
        }
    }
    Ok(Value::Object(map))
}

fn parameter_spec<'a>(operation: Option<&'a ApiOperation>, flag: &str) -> Option<&'a ParamSpec> {
    operation.and_then(|operation| operation.params.iter().find(|param| param.name == flag))
}

/// Turn the raw tokens of a list flag into JSON items of the schema's item type.
///
/// Accepts a single value (`15`), comma-separated values (`15,16`), a JSON array
/// (`[15,16]`, even when the shell-level tokenizer split it at spaces), or
/// several tokens (`15 16`). Every item is coerced to `item_ty`; an item that
/// cannot be is an error naming the flag, the position and the offending value,
/// so the model corrects the call instead of sending it to be rejected by the
/// API after a human approved it.
fn coerce_array(flag: &str, raw: &[&str], item_ty: Option<&str>) -> Result<Vec<Value>, String> {
    // A JSON array containing spaces arrives as several tokens: `[15, 16]`.
    let joined;
    let chunks: Vec<&str> = if raw.len() > 1 && raw[0].trim_start().starts_with('[') {
        joined = raw.join(" ");
        vec![joined.as_str()]
    } else {
        raw.to_vec()
    };

    let structured_items = matches!(item_ty, Some("object") | Some("array"));
    let several_tokens = chunks.len() > 1;
    let mut items: Vec<Value> = Vec::new();
    for chunk in chunks {
        let mut chunk = chunk.trim();
        if several_tokens && !chunk.starts_with('[') {
            // `15, 16` is two tokens: `15,` and `16`. The comma that joins
            // neighbouring tokens belongs to neither of them.
            chunk = chunk.trim_matches(',').trim();
            if chunk.is_empty() {
                continue;
            }
        }
        if chunk.starts_with('[') {
            let parsed = serde_json::from_str::<Vec<Value>>(chunk).map_err(|error| {
                format!(
                    "Invalid JSON array for `--{flag}`: {error}. Pass a list as \
                     `--{flag} 1,2` or `--{flag} '[1,2]'`."
                )
            })?;
            for value in parsed {
                let position = items.len() + 1;
                items.push(coerce_array_item(flag, position, value, item_ty)?);
            }
        } else if structured_items {
            // Object/array items are JSON; a bare token cannot be one.
            let value = parse_structured_value(flag, chunk)?;
            let position = items.len() + 1;
            items.push(coerce_array_item(flag, position, value, item_ty)?);
        } else {
            for piece in chunk.split(',') {
                let piece = piece.trim();
                let position = items.len() + 1;
                if piece.is_empty() {
                    return Err(format!(
                        "Empty item {position} in `--{flag}`: '{chunk}'. Separate values with a \
                         single comma, for example `--{flag} 1,2`."
                    ));
                }
                let item = match item_ty {
                    Some("integer") | Some("number") | Some("string") | Some("boolean") => {
                        coerce_array_item(
                            flag,
                            position,
                            Value::String(piece.to_string()),
                            item_ty,
                        )?
                    }
                    // Unknown item type: same permissive guess as an untyped flag.
                    _ => coerce(flag, piece, None)?,
                };
                items.push(item);
            }
        }
    }
    Ok(items)
}

/// Coerce one list item to the schema's item type (see [`coerce_array`]).
fn coerce_array_item(
    flag: &str,
    position: usize,
    value: Value,
    item_ty: Option<&str>,
) -> Result<Value, String> {
    let mismatch = |expected: &str, value: &Value| {
        let shown = match value {
            Value::String(s) => format!("the string \"{s}\""),
            other => other.to_string(),
        };
        format!(
            "Invalid item {position} for `--{flag}`: {shown} is not {expected}. Pass a list as \
             `--{flag} a,b`, a JSON array `--{flag} '[a,b]'`, or repeat the flag."
        )
    };
    match item_ty {
        Some("integer") => match &value {
            Value::Number(n) if n.is_i64() || n.is_u64() => Ok(value),
            Value::String(s) => s
                .trim()
                .parse::<i64>()
                .map(Value::from)
                .map_err(|_| mismatch("an integer", &value)),
            _ => Err(mismatch("an integer", &value)),
        },
        Some("number") => match &value {
            Value::Number(_) => Ok(value),
            Value::String(s) => s
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|n| n.is_finite())
                .and_then(serde_json::Number::from_f64)
                .map(Value::Number)
                .ok_or_else(|| mismatch("a number", &value)),
            _ => Err(mismatch("a number", &value)),
        },
        Some("string") => match &value {
            Value::String(_) => Ok(value),
            // `[1,2]` for a list of strings: the schema says string, so 1 -> "1".
            Value::Number(n) => Ok(Value::String(n.to_string())),
            Value::Bool(b) => Ok(Value::String(b.to_string())),
            _ => Err(mismatch("a string", &value)),
        },
        Some("boolean") => match &value {
            Value::Bool(_) => Ok(value),
            Value::String(s) if s.trim() == "true" => Ok(Value::Bool(true)),
            Value::String(s) if s.trim() == "false" => Ok(Value::Bool(false)),
            _ => Err(mismatch("a boolean (true or false)", &value)),
        },
        Some("object") if value.is_object() => Ok(value),
        Some("object") => Err(mismatch("a JSON object", &value)),
        Some("array") if value.is_array() => Ok(value),
        Some("array") => Err(mismatch("a JSON array", &value)),
        _ => Ok(value),
    }
}

fn coerce(flag: &str, s: &str, expected_type: Option<&str>) -> Result<Value, String> {
    // The operation's OpenAPI schema is authoritative when it is available.
    // A CLI token has no intrinsic scalar type: `18` can be either an integer
    // threshold or the Postgres version string. Guessing from spelling alone
    // turned `--version 18` into JSON number 18 and made a human-approved
    // create-service proposal fail only after confirmation.
    match expected_type {
        Some("string") => {
            let trimmed = s.trim();
            if trimmed.starts_with('"') && trimmed.ends_with('"') {
                return serde_json::from_str::<String>(trimmed)
                    .map(Value::String)
                    .map_err(|error| format!("Invalid JSON string for `--{flag}`: {error}."));
            }
            return Ok(Value::String(s.to_string()));
        }
        Some("integer") => {
            return s
                .parse::<i64>()
                .map(Value::from)
                .map_err(|error| format!("Invalid integer for `--{flag}`: '{s}' ({error})."));
        }
        Some("number") => {
            return s
                .parse::<f64>()
                .ok()
                .filter(|number| number.is_finite())
                .and_then(serde_json::Number::from_f64)
                .map(Value::Number)
                .ok_or_else(|| format!("Invalid number for `--{flag}`: '{s}'."));
        }
        Some("boolean") => {
            return match s {
                "true" => Ok(Value::Bool(true)),
                "false" => Ok(Value::Bool(false)),
                _ => Err(format!(
                    "Invalid boolean for `--{flag}`: '{s}'. Use `true` or `false`."
                )),
            };
        }
        Some("object") | Some("array") => return parse_structured_value(flag, s),
        _ => {}
    }

    // Operations without schema information retain the permissive legacy
    // coercion used by parser-only tests and unknown/`any` parameters.
    if let Ok(n) = s.parse::<i64>() {
        return Ok(Value::from(n));
    }
    // Floats too, or every fractional value becomes a string and the API
    // rejects it with `invalid type: string "0.5", expected f64`. Thresholds
    // are the obvious case: `--threshold 0.5` has to arrive as a number.
    // Guarded against `f64::from_str` accepting "inf"/"NaN", which are not
    // representable in JSON and would serialize as `null`.
    if let Ok(n) = s.parse::<f64>() {
        if n.is_finite() {
            if let Some(v) = serde_json::Number::from_f64(n) {
                return Ok(Value::Number(v));
            }
        }
    }
    match s {
        "true" => Ok(Value::Bool(true)),
        "false" => Ok(Value::Bool(false)),
        _ => {
            // Object/array-shaped flag values (e.g. `--parameters {"database":"app"}`)
            // are real JSON, not opaque strings — a body param typed as a map/array
            // must actually deserialize as one downstream, not as a JSON string
            // containing braces. Only object/array literals are attempted here;
            // a bare `"true"`/number-looking string inside quotes still falls
            // through to the String arm below like any other scalar.
            let trimmed = s.trim();
            if (trimmed.starts_with('{') && trimmed.ends_with('}'))
                || (trimmed.starts_with('[') && trimmed.ends_with(']'))
                || (trimmed.starts_with('"') && trimmed.ends_with('"'))
            {
                return parse_structured_value(flag, s);
            }
            Ok(Value::String(s.to_string()))
        }
    }
}

fn parse_structured_value(flag: &str, s: &str) -> Result<Value, String> {
    serde_json::from_str::<Value>(s.trim()).map_err(|error| {
        format!(
            "Invalid JSON for `--{flag}`: {error}. Object and array values must be \
             valid JSON with double-quoted keys and string values, for example \
             `--{flag} '{{\"database\":\"postgres\",\"username\":\"postgres\"}}'`."
        )
    })
}

/// First non-empty line of a (possibly multi-line) string, trimmed.
fn first_line(s: &str) -> String {
    s.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use utoipa::openapi::{
        path::{OperationBuilder, PathItem, PathsBuilder},
        OpenApiBuilder,
    };

    fn analytics_discovery_index() -> ReadOnlyApiIndex {
        let visitor_sessions = OperationBuilder::new()
            .operation_id(Some("get_analytics_visitor_sessions"))
            .summary(Some("Get all sessions for a specific visitor"))
            .tag("Analytics")
            .build();
        let page_detail = OperationBuilder::new()
            .operation_id(Some("get_page_path_detail"))
            .summary(Some("Get detailed analytics for a specific page path"))
            .tag("Analytics")
            .build();
        let paths = PathsBuilder::new()
            .path(
                "/analytics/visitors/{visitor_id}/sessions",
                PathItem::new(utoipa::openapi::path::HttpMethod::Get, visitor_sessions),
            )
            .path(
                "/analytics/page-path-detail",
                PathItem::new(utoipa::openapi::path::HttpMethod::Get, page_detail),
            )
            .build();
        let openapi = OpenApiBuilder::new()
            .info(utoipa::openapi::Info::new("Test API", "1.0.0"))
            .paths(paths)
            .build();

        ReadOnlyApiIndex::from_openapi_allowlist(
            &openapi,
            &["get_analytics_visitor_sessions", "get_page_path_detail"],
        )
    }

    use crate::index::ParamSpec;

    #[test]
    fn operation_help_explains_optional_project_linkage() {
        let operation = ApiOperation {
            operation_id: "create_service".to_string(),
            path: "/external-services".to_string(),
            method: "POST".to_string(),
            summary: Some("Create an external service".to_string()),
            description: None,
            tags: vec!["External Services".to_string()],
            params: vec![ParamSpec {
                name: "project_id".to_string(),
                location: ParamLocation::Body,
                required: false,
                ty: "integer".to_string(),
                item_ty: None,
                enum_values: Vec::new(),
                description: Some("Optionally link the service".to_string()),
                object_shape: None,
            }],
        };

        let help = render_operation_help(&operation);

        assert!(help.contains("project_id is optional"));
        assert!(help.contains("resource remains unlinked"));
        assert!(!help.contains("auto-filled for the current project"));
    }

    #[test]
    fn tokenize_respects_quotes() {
        assert_eq!(
            tokenize("deploy get --name foo"),
            ["deploy", "get", "--name", "foo"]
        );
        assert_eq!(
            tokenize(r#"x --name "two words" --b=1"#),
            ["x", "--name", "two words", "--b=1"]
        );
        assert!(tokenize("   ").is_empty());
    }

    #[test]
    fn parse_flags_coerces_and_supports_eq() {
        let v = parse_flags(
            &[
                "--limit".into(),
                "20".into(),
                "--name=foo".into(),
                "--flag".into(),
            ],
            None,
        )
        .unwrap();
        assert_eq!(v["limit"], Value::from(20));
        assert_eq!(v["name"], Value::from("foo"));
        assert_eq!(v["flag"], Value::Bool(true));
    }

    /// `--threshold 0.5` must arrive as a JSON number. It used to fall through
    /// to a string (only `i64` was attempted), and the API answered
    /// `invalid type: string "0.5", expected f64`.
    #[test]
    fn parse_flags_coerces_floats_to_numbers() {
        let v = parse_flags(
            &[
                "--threshold".into(),
                "0.5".into(),
                "--ratio".into(),
                "-1.25".into(),
            ],
            None,
        )
        .unwrap();
        assert_eq!(v["threshold"], serde_json::json!(0.5));
        assert_eq!(v["ratio"], serde_json::json!(-1.25));
        assert!(v["threshold"].is_number() && !v["threshold"].is_string());
    }

    /// Integers must keep parsing as integers, not become floats.
    #[test]
    fn parse_flags_keeps_integers_integral() {
        let v = parse_flags(&["--window_secs".into(), "300".into()], None).unwrap();
        assert_eq!(v["window_secs"], Value::from(300));
        assert!(v["window_secs"].is_i64());
    }

    /// `f64::from_str` accepts these; JSON cannot represent them, and
    /// `Number::from_f64` would yield `null`. They stay strings.
    #[test]
    fn parse_flags_leaves_non_finite_floats_as_strings() {
        for token in ["inf", "-inf", "NaN"] {
            let v = parse_flags(&["--x".into(), token.into()], None).unwrap();
            assert!(v["x"].is_string(), "{token} became {:?}", v["x"]);
        }
    }

    #[test]
    fn parse_flags_rejects_bare_positional() {
        assert!(parse_flags(&["oops".into()], None).is_err());
    }

    #[test]
    fn parse_flags_coerces_object_and_array_flag_values() {
        let v = parse_flags(
            &[
                "--parameters".into(),
                r#"{"database":"app","username":"app"}"#.into(),
                "--members".into(),
                r#"[{"role":"primary"}]"#.into(),
            ],
            None,
        )
        .unwrap();
        assert_eq!(
            v["parameters"],
            serde_json::json!({"database": "app", "username": "app"})
        );
        assert_eq!(v["members"], serde_json::json!([{"role": "primary"}]));
    }

    #[test]
    fn parse_flags_rejects_malformed_structured_values() {
        let error = parse_flags(
            &[
                "--parameters".into(),
                "{database:postgres,username:postgres}".into(),
            ],
            None,
        )
        .unwrap_err();

        assert!(error.contains("Invalid JSON for `--parameters`"), "{error}");
        assert!(error.contains("double-quoted keys"), "{error}");
        assert!(error.contains(r#"{"database":"postgres""#), "{error}");
    }

    #[test]
    fn escaped_json_quotes_are_not_silently_stripped_into_a_string() {
        let tokens = tokenize(
            r#"create_service --name postgres-test --service_type postgres --parameters {\"database\":\"postgres\",\"username\":\"postgres\"}"#,
        );
        let error = parse_flags(&tokens[1..], None).unwrap_err();

        assert!(error.contains("Invalid JSON for `--parameters`"), "{error}");
    }

    #[test]
    fn parse_flags_decodes_a_json_quoted_string() {
        let parsed = parse_flags(&["--service_type".into(), r#""postgres""#.into()], None).unwrap();

        assert_eq!(parsed["service_type"], Value::String("postgres".into()));
    }

    #[test]
    fn operation_schema_preserves_number_looking_string_flags() {
        let operation = ApiOperation {
            operation_id: "create_service".to_string(),
            path: "/external-services".to_string(),
            method: "POST".to_string(),
            summary: None,
            description: None,
            tags: vec!["External Services".to_string()],
            params: vec![crate::index::ParamSpec {
                name: "version".to_string(),
                location: ParamLocation::Body,
                required: true,
                ty: "string".to_string(),
                item_ty: None,
                enum_values: vec![],
                description: None,
                object_shape: None,
            }],
        };

        let parsed = parse_flags(&["--version".into(), "18".into()], Some(&operation))
            .expect("OpenAPI string parameters must not be guessed as numbers");

        assert_eq!(parsed["version"], Value::String("18".to_string()));
    }

    #[test]
    fn mutation_redirect_steers_write_ops_to_temps_write() {
        // Mutation-shaped names get the redirect...
        for op in [
            "promote_deployment",
            "rollback_to_deployment",
            "trigger_project_pipeline",
            "update_environment_settings",
            "restart_container",
            "create_environment_variable",
            "delete",
        ] {
            let hint = mutation_redirect(op);
            assert!(
                hint.contains("temps_write"),
                "expected `{op}` to redirect to temps_write, got: {hint:?}"
            );
        }
        // ...read/query verbs do not.
        for op in [
            "get_deployment",
            "list_containers",
            "query_metrics",
            "get_environments",
        ] {
            assert!(
                mutation_redirect(op).is_empty(),
                "expected `{op}` to produce no redirect"
            );
        }
    }

    #[test]
    fn slugify_lowercases_and_hyphenates() {
        assert_eq!(slugify("Audit Logs"), "audit-logs");
        assert_eq!(slugify("Deployments"), "deployments");
    }

    #[test]
    fn first_line_skips_blanks() {
        assert_eq!(first_line("\n\n  Hello\nworld"), "Hello");
    }

    #[test]
    fn path_section_skips_projects_and_params() {
        assert_eq!(
            path_section("/projects/{project_id}/deployments/{deployment_id}/jobs"),
            "Deployments"
        );
        assert_eq!(path_section("/deployments/activity-graph"), "Deployments");
        assert_eq!(
            path_section("/projects/{id}/static-bundles"),
            "Static-bundles"
        );
        assert_eq!(path_section("/"), "General");
    }

    #[test]
    fn unknown_analytics_operation_surfaces_page_level_country_operation() {
        let index = analytics_discovery_index();

        let result = resolve(
            &index,
            "analytics get_analytics --path /managed",
            &|_| true,
            CliMode::ReadOnly,
        );
        let CliAction::Terminal(message) = result else {
            panic!("unknown analytics operation unexpectedly resolved");
        };

        assert!(message.contains("Unknown operation 'get_analytics'"));
        assert!(
            message.contains("get_page_path_detail"),
            "page-level analytics operation missing from recovery help: {message}"
        );
        assert!(
            message.contains("Get detailed analytics for a specific page path"),
            "page-detail summary missing from recovery help: {message}"
        );
    }

    #[test]
    fn unknown_operation_recovery_only_lists_permitted_operations() {
        let index = analytics_discovery_index();

        let result = resolve(
            &index,
            "analytics get_analytics --path /managed",
            &|op| op.operation_id != "get_analytics_visitor_sessions",
            CliMode::ReadOnly,
        );
        let CliAction::Terminal(message) = result else {
            panic!("unknown analytics operation unexpectedly resolved");
        };

        assert!(message.contains("get_page_path_detail"));
        assert!(
            !message.contains("get_analytics_visitor_sessions"),
            "recovery help exposed an operation rejected by the permission filter: {message}"
        );
    }

    #[test]
    fn unknown_mutation_preserves_write_tool_redirect_and_section_help() {
        let index = analytics_discovery_index();

        let result = resolve(
            &index,
            "analytics delete_analytics",
            &|_| true,
            CliMode::ReadOnly,
        );
        let CliAction::Terminal(message) = result else {
            panic!("unknown analytics mutation unexpectedly resolved");
        };

        assert!(
            message.contains("temps_write"),
            "redirect missing: {message}"
        );
        assert!(
            message.contains("get_page_path_detail"),
            "section recovery help missing: {message}"
        );
    }

    fn array_param(name: &str, item_ty: Option<&str>) -> ParamSpec {
        ParamSpec {
            name: name.to_string(),
            location: ParamLocation::Body,
            required: true,
            ty: "array".to_string(),
            item_ty: item_ty.map(str::to_string),
            enum_values: Vec::new(),
            description: None,
            object_shape: None,
        }
    }

    fn env_var_operation() -> ApiOperation {
        let scalar = |name: &str, ty: &str| ParamSpec {
            name: name.to_string(),
            location: ParamLocation::Body,
            required: true,
            ty: ty.to_string(),
            item_ty: None,
            enum_values: Vec::new(),
            description: None,
            object_shape: None,
        };
        ApiOperation {
            operation_id: "create_environment_variable".to_string(),
            path: "/projects/{project_id}/env-vars".to_string(),
            method: "POST".to_string(),
            summary: Some("Create an environment variable".to_string()),
            description: None,
            tags: vec!["Projects".to_string()],
            params: vec![
                scalar("key", "string"),
                scalar("value", "string"),
                array_param("environment_ids", Some("integer")),
                array_param("labels", Some("string")),
                array_param("flags", Some("boolean")),
                array_param("weights", Some("number")),
                array_param("rules", Some("object")),
                array_param("anything", None),
            ],
        }
    }

    /// Parse a command tail against `env_var_operation` and return the value of `flag`.
    fn parsed(tail: &str, flag: &str) -> Result<Value, String> {
        let op = env_var_operation();
        let tokens = tokenize(tail);
        parse_flags(&tokens, Some(&op)).map(|object| object[flag].clone())
    }

    #[test]
    fn array_flag_accepts_every_documented_spelling() {
        let expected = serde_json::json!([15, 16]);
        for tail in [
            "--environment_ids 15,16",
            "--environment_ids 15, 16",
            "--environment_ids 15 16",
            "--environment_ids [15,16]",
            "--environment_ids [15, 16]",
            "--environment_ids '[15,16]'",
            "--environment_ids=15,16",
            "--environment_ids=[15,16]",
            "--environment_ids 15 --environment_ids 16",
            "--environment_ids 15 --environment_ids=16",
            "--environment_ids \"15,16\"",
            "--environment_ids [\"15\",\"16\"]",
            "--environment_ids '[\"15\", \"16\"]'",
        ] {
            assert_eq!(parsed(tail, "environment_ids").unwrap(), expected, "{tail}");
        }
    }

    #[test]
    fn single_value_is_wrapped_into_a_one_item_array() {
        // The reported bug: `--environment_ids 15` was sent as the scalar 15.
        assert_eq!(
            parsed("--environment_ids 15", "environment_ids").unwrap(),
            serde_json::json!([15])
        );
        assert_eq!(
            parsed("--environment_ids [15]", "environment_ids").unwrap(),
            serde_json::json!([15])
        );
        assert_eq!(
            parsed("--environment_ids []", "environment_ids").unwrap(),
            serde_json::json!([])
        );
    }

    #[test]
    fn array_items_are_coerced_to_the_schema_item_type() {
        // Number-looking strings stay strings when the schema says string.
        assert_eq!(
            parsed("--labels 18,19", "labels").unwrap(),
            serde_json::json!(["18", "19"])
        );
        assert_eq!(
            parsed("--labels [18,19]", "labels").unwrap(),
            serde_json::json!(["18", "19"])
        );
        assert_eq!(
            parsed("--labels web,api", "labels").unwrap(),
            serde_json::json!(["web", "api"])
        );
        assert_eq!(
            parsed("--flags true,false", "flags").unwrap(),
            serde_json::json!([true, false])
        );
        assert_eq!(
            parsed("--weights 1,0.5,-2.25", "weights").unwrap(),
            serde_json::json!([1.0, 0.5, -2.25])
        );
        assert_eq!(
            parsed(r#"--rules '[{"a":1},{"a":2}]'"#, "rules").unwrap(),
            serde_json::json!([{"a": 1}, {"a": 2}])
        );
        // Unknown item type keeps the permissive legacy guess per item.
        assert_eq!(
            parsed("--anything 1,x,true", "anything").unwrap(),
            serde_json::json!([1, "x", true])
        );
    }

    #[test]
    fn array_item_mismatches_are_typed_errors_naming_flag_position_and_value() {
        let error = parsed("--environment_ids 15,abc", "environment_ids").unwrap_err();
        assert!(error.contains("`--environment_ids`"), "{error}");
        assert!(error.contains("item 2"), "{error}");
        assert!(error.contains("\"abc\""), "{error}");
        assert!(error.contains("an integer"), "{error}");

        let error = parsed("--environment_ids [1.5]", "environment_ids").unwrap_err();
        assert!(error.contains("item 1"), "{error}");

        let error = parsed(r#"--environment_ids '[1,{"x":1}]'"#, "environment_ids").unwrap_err();
        assert!(error.contains("item 2"), "{error}");

        let error = parsed("--flags true,maybe", "flags").unwrap_err();
        assert!(error.contains("boolean"), "{error}");

        let error = parsed("--weights 1,inf", "weights").unwrap_err();
        assert!(error.contains("a number"), "{error}");

        let error = parsed("--rules 1,2", "rules").unwrap_err();
        assert!(error.contains("`--rules`"), "{error}");

        let error = parsed("--rules [1]", "rules").unwrap_err();
        assert!(error.contains("a JSON object"), "{error}");
    }

    #[test]
    fn malformed_or_empty_array_values_are_rejected_clearly() {
        let error = parsed("--environment_ids [15,", "environment_ids").unwrap_err();
        assert!(
            error.contains("Invalid JSON array for `--environment_ids`"),
            "{error}"
        );

        let error = parsed("--environment_ids 15,,16", "environment_ids").unwrap_err();
        assert!(error.contains("Empty item 2"), "{error}");

        let error = parsed("--environment_ids --key x", "environment_ids").unwrap_err();
        assert!(error.contains("needs at least one value"), "{error}");
    }

    #[test]
    fn array_flags_do_not_swallow_following_flags() {
        let op = env_var_operation();
        let tokens = tokenize("--key K --environment_ids 15 16 --value V");
        let object = parse_flags(&tokens, Some(&op)).unwrap();
        assert_eq!(object["key"], Value::String("K".into()));
        assert_eq!(object["value"], Value::String("V".into()));
        assert_eq!(object["environment_ids"], serde_json::json!([15, 16]));
    }

    #[test]
    fn full_create_environment_variable_command_resolves_with_an_array_body() {
        let op = env_var_operation();
        let tokens = tokenize("--key K --value V --environment_ids 15");
        let object = parse_flags(&tokens, Some(&op)).unwrap();
        assert_eq!(
            object,
            serde_json::json!({"key": "K", "value": "V", "environment_ids": [15]})
        );
    }

    #[test]
    fn help_documents_array_flags_with_their_item_type() {
        let help = render_operation_help(&env_var_operation());
        assert!(
            help.contains("--environment_ids <int>[,<int>...] (required)"),
            "{help}"
        );
        assert!(help.contains("--labels <string>[,<string>...]"), "{help}");
        assert!(help.contains("--flags <bool>[,<bool>...]"), "{help}");
        assert!(help.contains("--weights <number>[,<number>...]"), "{help}");
        assert!(help.contains("--rules <json array of objects>"), "{help}");
        assert!(help.contains("--anything <json array>"), "{help}");
        assert!(
            help.contains("List flags take comma-separated values"),
            "{help}"
        );
        assert!(!help.contains("--environment_ids <string>"), "{help}");
        // Scalars keep their plain placeholder.
        assert!(help.contains("--key <string> (required)"), "{help}");
    }

    #[test]
    fn help_without_list_flags_has_no_list_hint() {
        let help = render_operation_help(&ApiOperation {
            params: vec![ParamSpec {
                name: "name".to_string(),
                location: ParamLocation::Body,
                required: true,
                ty: "string".to_string(),
                item_ty: None,
                enum_values: Vec::new(),
                description: None,
                object_shape: None,
            }],
            ..env_var_operation()
        });
        assert!(!help.contains("List flags"), "{help}");
    }

    #[test]
    fn write_root_help_describes_the_write_tool_not_the_read_only_one() {
        let index = analytics_discovery_index();

        let CliAction::Terminal(write) = resolve(&index, "--help", &|_| true, CliMode::Write)
        else {
            panic!("root help must be terminal");
        };
        assert!(write.contains("Temps write API"), "{write}");
        assert!(write.contains("after the user confirms"), "{write}");
        assert!(!write.contains("read-only"), "{write}");
        assert!(!write.contains("GET endpoints"), "{write}");

        let CliAction::Terminal(read) = resolve(&index, "--help", &|_| true, CliMode::ReadOnly)
        else {
            panic!("root help must be terminal");
        };
        assert!(
            read.starts_with("Temps read-only API — a CLI over the platform's GET endpoints."),
            "{read}"
        );
    }

    #[test]
    fn write_mode_does_not_redirect_unknown_mutations_to_itself() {
        let index = analytics_discovery_index();

        let CliAction::Terminal(message) = resolve(
            &index,
            "analytics delete_analytics",
            &|_| true,
            CliMode::Write,
        ) else {
            panic!("unknown operation must be terminal");
        };
        assert!(
            message.contains("Unknown operation 'delete_analytics'"),
            "{message}"
        );
        assert!(!message.contains("read-only"), "{message}");
        assert!(!message.contains("Propose it with"), "{message}");
    }
}
