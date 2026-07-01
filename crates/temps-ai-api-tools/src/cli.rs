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
use crate::index::{ApiOperation, ParamLocation, ReadOnlyApiIndex};

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
        return CliAction::Terminal(render_root_help(index, permit));
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
                "Unknown operation '{op_name}' in section '{}'. Run `{} --help` to list it.",
                sec.slug, sec.slug
            )),
        };
    }

    // Leniency: accept a bare `operation_id` (it's globally unique).
    if let Some(op) = index.get(first) {
        return resolve_operation(op, &tokens[1..]);
    }

    CliAction::Terminal(format!(
        "Unknown command '{first}'. Run `--help` to list sections."
    ))
}

/// Resolve the tail of `… <operation_id> [tail]` — either help or execution.
fn resolve_operation<'a>(op: &'a ApiOperation, tail: &[String]) -> CliAction<'a> {
    if tail.first().map(|t| is_help(t)).unwrap_or(false) {
        return CliAction::Terminal(render_operation_help(op));
    }
    match parse_flags(tail) {
        Ok(params) => CliAction::Execute(op, params),
        Err(e) => CliAction::Terminal(e),
    }
}

// ---------------------------------------------------------------------------
// Help rendering
// ---------------------------------------------------------------------------

fn render_root_help(index: &ReadOnlyApiIndex, permit: &dyn Fn(&ApiOperation) -> bool) -> String {
    let secs = sections(index, permit);
    let mut out = String::from(
        "Temps read-only API — a CLI over the platform's GET endpoints.\n\
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
        let summary = op.summary.as_deref().map(first_line).unwrap_or_default();
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

    // Hide the auto-filled project selector — the model must not supply it.
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
            // Path params are structurally required; query params are optional
            // filters (omit the ones you don't need).
            let req = if matches!(p.location, ParamLocation::Path) {
                " (required)"
            } else {
                ""
            };
            out.push_str(&format!("  --{} <{}>{}", p.name, p.ty, req));
            if !p.enum_values.is_empty() {
                out.push_str(&format!(" — one of: {}", p.enum_values.join(", ")));
            }
            if let Some(d) = &p.description {
                out.push_str(&format!(" — {d}"));
            }
            out.push('\n');
        }
    }
    out.push_str("(project_id is auto-filled for the current project.)\n");
    out
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
/// object. Values are coerced to int/bool when they look like one, else string.
fn parse_flags(tokens: &[String]) -> Result<Value, String> {
    let mut map = serde_json::Map::new();
    let mut i = 0;
    while i < tokens.len() {
        let t = &tokens[i];
        let Some(rest) = t.strip_prefix("--") else {
            return Err(format!(
                "Unexpected argument '{t}'. Flags must look like `--name value` or `--name=value`."
            ));
        };
        if let Some((k, v)) = rest.split_once('=') {
            map.insert(k.to_string(), coerce(v));
            i += 1;
        } else if i + 1 < tokens.len() && !tokens[i + 1].starts_with("--") {
            map.insert(rest.to_string(), coerce(&tokens[i + 1]));
            i += 2;
        } else {
            // A flag with no value → boolean true (e.g. `--only_errors`).
            map.insert(rest.to_string(), Value::Bool(true));
            i += 1;
        }
    }
    Ok(Value::Object(map))
}

fn coerce(s: &str) -> Value {
    if let Ok(n) = s.parse::<i64>() {
        return Value::from(n);
    }
    match s {
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        _ => Value::String(s.to_string()),
    }
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
        let v = parse_flags(&[
            "--limit".into(),
            "20".into(),
            "--name=foo".into(),
            "--flag".into(),
        ])
        .unwrap();
        assert_eq!(v["limit"], Value::from(20));
        assert_eq!(v["name"], Value::from("foo"));
        assert_eq!(v["flag"], Value::Bool(true));
    }

    #[test]
    fn parse_flags_rejects_bare_positional() {
        assert!(parse_flags(&["oops".into()]).is_err());
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
}
