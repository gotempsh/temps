// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Write-time normalization and validation (ADR-071 §1–§3). The messages match
//! the console's `validateRule` so a rule rejected by the API reads the same as
//! one the editor flags.

use std::collections::HashSet;
use std::sync::OnceLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::fields::{limits, spec, KeyKind, ValueKind};
use crate::ip::parse_cidr;
use crate::model::{Action, Condition, Expression, Field, Group, Operator};

/// One problem with a rule. `target` is `name`, `action`, or the `id` of the
/// condition or group it concerns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as = EdgeRuleIssue)]
pub struct RuleIssue {
    pub target: String,
    pub message: String,
}

fn issue(target: &str, message: impl Into<String>) -> RuleIssue {
    RuleIssue {
        target: target.to_string(),
        message: message.into(),
    }
}

/// Trims values and keys, splits list values on commas and newlines, and
/// gives every node a unique id. Run before [`validate_rule`] and before
/// storing.
pub fn prepare_expression(root: &mut Group) {
    let mut seen = HashSet::new();
    let mut counter = 0usize;
    prepare_group(root, &mut seen, &mut counter);
}

fn assign_id(id: &mut String, seen: &mut HashSet<String>, counter: &mut usize) {
    let trimmed = id.trim().to_string();
    let usable =
        !trimmed.is_empty() && trimmed.len() <= limits::NODE_ID_LENGTH && !seen.contains(&trimmed);
    if usable {
        *id = trimmed;
    } else {
        loop {
            *counter += 1;
            let candidate = format!("n{counter}");
            if !seen.contains(&candidate) {
                *id = candidate;
                break;
            }
        }
    }
    seen.insert(id.clone());
}

fn prepare_group(group: &mut Group, seen: &mut HashSet<String>, counter: &mut usize) {
    assign_id(&mut group.id, seen, counter);
    for child in &mut group.children {
        match child {
            Expression::Group(g) => prepare_group(g, seen, counter),
            Expression::Condition(c) => {
                assign_id(&mut c.id, seen, counter);
                prepare_condition(c);
            }
        }
    }
}

fn prepare_condition(c: &mut Condition) {
    let field = spec(c.field);
    c.key = match (field.keyed, c.key.take()) {
        (Some(_), Some(key)) => Some(key.trim().to_string()),
        (Some(_), None) => Some(String::new()),
        (None, _) => None,
    };
    if !field.case_flag {
        c.case_insensitive = false;
    }
    if c.op.is_presence() {
        c.values.clear();
        return;
    }
    if c.op.is_list() {
        c.values = c
            .values
            .iter()
            .flat_map(|v| v.split([',', '\n']))
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .collect();
    } else {
        c.values = c
            .values
            .iter()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .collect();
    }
}

fn header_name_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^[!#$%&'*+.^_`|~0-9A-Za-z-]+$").unwrap_or_else(|_| unreachable_regex())
    })
}

fn hostname_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)^(\*\.)?[a-z0-9]([a-z0-9-]*[a-z0-9])?(\.[a-z0-9]([a-z0-9-]*[a-z0-9])?)*$")
            .unwrap_or_else(|_| unreachable_regex())
    })
}

fn asn_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)^(AS)?\d{1,10}$").unwrap_or_else(|_| unreachable_regex()))
}

fn country_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)^[A-Z]{2}$").unwrap_or_else(|_| unreachable_regex()))
}

fn placeholder_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\{([^}]*)\}").unwrap_or_else(|_| unreachable_regex()))
}

fn absolute_target_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?is)^https?://([^/?#\s{}\\]+)(.*)$").unwrap_or_else(|_| unreachable_regex())
    })
}

/// A regex that never matches, used only if a constant pattern above fails to
/// compile, which the unit tests rule out.
fn unreachable_regex() -> Regex {
    // `[^\s\S]` matches nothing and always compiles.
    match Regex::new(r"[^\s\S]") {
        Ok(re) => re,
        Err(_) => std::process::abort(),
    }
}

/// Compiles a user pattern with the engine's limits.
pub fn build_regex(source: &str, case_insensitive: bool) -> Result<Regex, String> {
    regex::RegexBuilder::new(source)
        .case_insensitive(case_insensitive)
        .size_limit(limits::REGEX_SIZE)
        .dfa_size_limit(limits::REGEX_SIZE)
        .build()
        .map_err(|e| {
            let text = e.to_string();
            let line = text
                .lines()
                .rev()
                .find(|l| l.trim_start().starts_with("error:"))
                .map(|l| l.trim().trim_start_matches("error:").trim().to_string())
                .unwrap_or_else(|| text.lines().next().unwrap_or("").to_string());
            format!("Invalid regular expression: {line}")
        })
}

fn validate_regex(source: &str) -> Option<String> {
    let unsupported = source.contains("(?=")
        || source.contains("(?!")
        || source.contains("(?<=")
        || source.contains("(?<!")
        || source.contains("\\k<")
        || source
            .as_bytes()
            .windows(2)
            .any(|w| w[0] == b'\\' && (b'1'..=b'9').contains(&w[1]));
    if unsupported {
        return Some("Look-ahead, look-behind and back-references are not supported.".into());
    }
    build_regex(source, false).err()
}

fn operator_label(op: Operator) -> &'static str {
    crate::fields::operator_label(op)
}

fn list_preview(bad: &[&String]) -> String {
    let shown: Vec<&str> = bad.iter().take(3).map(|s| s.as_str()).collect();
    format!(
        "{} {}",
        shown.join(", "),
        if bad.len() == 1 { "is" } else { "are" }
    )
}

fn validate_condition(c: &Condition) -> Vec<String> {
    let field = spec(c.field);
    let mut issues = Vec::new();
    if !field.operators.contains(&c.op) {
        issues.push(format!(
            "\"{}\" is not available for {}.",
            operator_label(c.op),
            field.label
        ));
        return issues;
    }
    if let Some(kind) = field.keyed {
        let key = c.key.as_deref().unwrap_or("").trim();
        if key.is_empty() {
            issues.push(format!("Enter the {} name.", kind.label()));
        } else if kind == KeyKind::Header && !header_name_re().is_match(key) {
            issues.push(format!("\"{key}\" is not a valid header name."));
        } else if key.chars().any(char::is_whitespace) {
            issues.push(format!("The {} name cannot contain spaces.", kind.label()));
        } else if key.len() > limits::VALUE_BYTES {
            issues.push(format!(
                "The {} name is limited to {} bytes.",
                kind.label(),
                limits::VALUE_BYTES
            ));
        }
    }
    if c.op.is_presence() {
        return issues;
    }
    let values = &c.values;
    if values.is_empty() {
        issues.push("Enter a value.".into());
        return issues;
    }
    if !c.op.is_list() && values.len() > 1 {
        issues.push(format!(
            "\"{}\" takes one value; use \"{}\" for a list.",
            operator_label(c.op),
            operator_label(Operator::In)
        ));
    }
    let max = if c.op == Operator::InCidr {
        limits::CIDR_VALUES
    } else {
        limits::LIST_VALUES
    };
    if values.len() > max {
        issues.push(format!("Use at most {max} values."));
    }
    if values.iter().any(|v| v.len() > limits::VALUE_BYTES) {
        issues.push(format!(
            "Values are limited to {} bytes.",
            limits::VALUE_BYTES
        ));
    }
    let mut invalid = |test: &dyn Fn(&str) -> bool, what: &str| {
        let bad: Vec<&String> = values.iter().filter(|v| !test(v)).collect();
        if !bad.is_empty() {
            issues.push(format!("{} not {what}.", list_preview(&bad)));
        }
    };
    match field.kind {
        ValueKind::Cidr => invalid(
            &|v| parse_cidr(v).is_some(),
            "a valid IP address or CIDR range",
        ),
        ValueKind::Number => invalid(&|v| asn_re().is_match(v), "an AS number"),
        ValueKind::Boolean => invalid(&|v| v == "true" || v == "false", "yes or no"),
        ValueKind::Options => {
            let what = format!("a valid {}", field.label.to_lowercase());
            invalid(
                &|v| field.options.iter().any(|(value, _)| *value == v),
                &what,
            )
        }
        ValueKind::Text => {
            if c.field == Field::Country {
                invalid(&|v| country_re().is_match(v), "a two-letter country code");
            }
            if c.field == Field::Host && matches!(c.op, Operator::Eq | Operator::In) {
                invalid(&|v| hostname_re().is_match(v), "a hostname");
            }
            if matches!(c.field, Field::Path | Field::RawPath)
                && matches!(c.op, Operator::Eq | Operator::In | Operator::StartsWith)
            {
                invalid(&|v| v.starts_with('/'), "a path starting with /");
            }
        }
    }
    if c.op == Operator::Glob {
        let stars: usize = values.iter().map(|v| v.matches('*').count()).sum();
        if stars > limits::GLOB_WILDCARDS {
            issues.push(format!(
                "Patterns may use at most {} * wildcards.",
                limits::GLOB_WILDCARDS
            ));
        }
    }
    if c.op == Operator::Regex {
        for v in values {
            if let Some(problem) = validate_regex(v) {
                issues.push(problem);
                break;
            }
        }
    }
    issues
}

/// Validates a redirect target so the visitor-controlled `{path}` and
/// `{query}` can never change the destination host.
fn validate_redirect(target: &str, status: u16) -> Vec<String> {
    let mut issues = Vec::new();
    if !matches!(status, 301 | 302 | 307 | 308) {
        issues.push("Redirect status must be 301, 302, 307 or 308.".into());
    }
    let target = target.trim();
    if target.is_empty() {
        issues.push("Enter where to redirect to.".into());
        return issues;
    }
    if target.len() > 2048 {
        issues.push("The redirect target is limited to 2048 bytes.".into());
    }
    if target.chars().any(|c| c.is_control()) {
        issues.push("The redirect target cannot contain control characters.".into());
    }
    for caps in placeholder_re().captures_iter(target) {
        let name = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        if name != "path" && name != "query" {
            issues.push(format!(
                "Unknown placeholder {{{name}}}. Use {{path}} or {{query}}."
            ));
        }
    }
    if let Some(caps) = absolute_target_re().captures(target) {
        let rest = caps.get(2).map(|m| m.as_str()).unwrap_or("");
        let ok = rest.is_empty()
            || rest.starts_with('/')
            || rest.starts_with('?')
            || rest.starts_with('#')
            || rest.starts_with("{path}");
        if !ok {
            issues.push(
                "The host must be followed by /, ?, # or {path}, so a visitor cannot change where the redirect goes."
                    .into(),
            );
        }
    } else if !target.starts_with('/') {
        issues.push(
            "Use a path starting with / or an absolute http(s) URL with a fixed host.".into(),
        );
    } else if target.starts_with("//") || target.starts_with("/\\") || target.starts_with("/{path}")
    {
        issues.push(
            "A path target cannot start with //, /\\ or /{path}: browsers treat those as a different host."
                .into(),
        );
    }
    if let Some(query_at) = target.find("{query}") {
        match target.find('?') {
            Some(q) if q < query_at => {}
            _ => {
                issues.push("Put {query} after a ? in the target, for example /new?{query}.".into())
            }
        }
    }
    issues
}

pub fn validate_action(action: &Action) -> Vec<String> {
    match action {
        Action::Block { status, body } => {
            let mut issues = Vec::new();
            if !(400..=499).contains(status) {
                issues.push("Block status must be between 400 and 499.".into());
            }
            if body.len() > limits::BLOCK_BODY_BYTES {
                issues.push(format!(
                    "The response body is limited to {} bytes.",
                    limits::BLOCK_BODY_BYTES
                ));
            }
            issues
        }
        Action::Redirect { target, status } => validate_redirect(target, *status),
        Action::Allow | Action::Challenge | Action::Log => Vec::new(),
    }
}

/// Validates a prepared rule. An empty list means the rule can be saved.
pub fn validate_rule(name: &str, expression: &Group, action: &Action) -> Vec<RuleIssue> {
    let mut issues = Vec::new();
    let name = name.trim();
    if name.is_empty() {
        issues.push(issue("name", "Give the rule a name."));
    } else if name.chars().count() > limits::NAME_LENGTH {
        issues.push(issue(
            "name",
            format!("Names are limited to {} characters.", limits::NAME_LENGTH),
        ));
    }
    let mut conditions = 0usize;
    walk_group(expression, 1, true, &mut issues, &mut conditions);
    if conditions > limits::CONDITIONS_PER_RULE {
        issues.push(issue(
            &expression.id,
            format!(
                "A rule can have at most {} conditions.",
                limits::CONDITIONS_PER_RULE
            ),
        ));
    }
    for message in validate_action(action) {
        issues.push(issue("action", message));
    }
    issues
}

fn walk_group(
    group: &Group,
    depth: usize,
    root: bool,
    issues: &mut Vec<RuleIssue>,
    conditions: &mut usize,
) {
    if depth > limits::DEPTH {
        issues.push(issue(
            &group.id,
            format!(
                "Groups can be nested at most {} levels deep.",
                limits::DEPTH
            ),
        ));
    }
    if group.children.is_empty() && !root {
        issues.push(issue(
            &group.id,
            "Add a condition to this group or remove it.",
        ));
    }
    for child in &group.children {
        match child {
            Expression::Group(g) => walk_group(g, depth + 1, false, issues, conditions),
            Expression::Condition(c) => {
                *conditions += 1;
                for message in validate_condition(c) {
                    issues.push(issue(&c.id, message));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Combinator;

    fn cond(field: Field, op: Operator, values: &[&str]) -> Condition {
        Condition {
            id: String::new(),
            field,
            key: None,
            op,
            values: values.iter().map(|v| v.to_string()).collect(),
            case_insensitive: false,
        }
    }

    fn rule(children: Vec<Expression>) -> Group {
        let mut g = Group {
            id: String::new(),
            combinator: Combinator::All,
            children,
        };
        prepare_expression(&mut g);
        g
    }

    fn block() -> Action {
        Action::Block {
            status: 403,
            body: String::new(),
        }
    }

    fn messages(name: &str, g: &Group, a: &Action) -> Vec<String> {
        validate_rule(name, g, a)
            .into_iter()
            .map(|i| i.message)
            .collect()
    }

    #[test]
    fn constant_patterns_compile() {
        for re in [
            header_name_re(),
            hostname_re(),
            asn_re(),
            country_re(),
            placeholder_re(),
            absolute_target_re(),
        ] {
            assert_ne!(re.as_str(), r"[^\s\S]");
        }
    }

    #[test]
    fn prepare_assigns_unique_ids_and_splits_lists() {
        let mut dup = cond(Field::Country, Operator::In, &["ES, PT", "FR\nDE", " "]);
        dup.id = "x".into();
        let mut other = cond(Field::Path, Operator::StartsWith, &["  /admin "]);
        other.id = "x".into();
        let g = rule(vec![
            Expression::Condition(dup),
            Expression::Condition(other),
        ]);
        let ids: Vec<&str> = g.children.iter().map(|c| c.id()).collect();
        assert_eq!(ids[0], "x");
        assert_ne!(ids[1], "x");
        assert!(!g.id.is_empty());
        match &g.children[0] {
            Expression::Condition(c) => assert_eq!(c.values, vec!["ES", "PT", "FR", "DE"]),
            other => panic!("unexpected {other:?}"),
        }
        match &g.children[1] {
            Expression::Condition(c) => assert_eq!(c.values, vec!["/admin"]),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn a_valid_rule_has_no_issues() {
        let g = rule(vec![Expression::Condition(cond(
            Field::Path,
            Operator::StartsWith,
            &["/admin"],
        ))]);
        assert!(validate_rule("Block admin", &g, &block()).is_empty());
    }

    #[test]
    fn name_is_required_and_bounded() {
        let g = rule(vec![]);
        assert_eq!(messages(" ", &g, &block()), vec!["Give the rule a name."]);
        let long = "x".repeat(81);
        assert_eq!(
            messages(&long, &g, &block()),
            vec!["Names are limited to 80 characters."]
        );
    }

    #[test]
    fn rejects_wrong_operator_missing_values_and_bad_values() {
        let g = rule(vec![
            Expression::Condition(cond(Field::Ip, Operator::Eq, &["1.2.3.4"])),
            Expression::Condition(cond(Field::Path, Operator::Eq, &[])),
            Expression::Condition(cond(Field::Path, Operator::Eq, &["admin"])),
            Expression::Condition(cond(Field::Ip, Operator::InCidr, &["10.0.0.0/8", "nope"])),
            Expression::Condition(cond(Field::Country, Operator::In, &["ES", "Spain"])),
            Expression::Condition(cond(Field::Method, Operator::Eq, &["FETCH"])),
            Expression::Condition(cond(Field::Asn, Operator::Eq, &["AS64496"])),
        ]);
        let issues = validate_rule("r", &g, &block());
        let text: Vec<String> = issues.iter().map(|i| i.message.clone()).collect();
        assert!(text.contains(&"\"is\" is not available for Client IP.".to_string()));
        assert!(text.contains(&"Enter a value.".to_string()));
        assert!(text.contains(&"admin is not a path starting with /.".to_string()));
        assert!(text.contains(&"nope is not a valid IP address or CIDR range.".to_string()));
        assert!(text.contains(&"Spain is not a two-letter country code.".to_string()));
        assert!(text.contains(&"FETCH is not a valid method.".to_string()));
        assert_eq!(issues.len(), 6, "{text:?}");
        // Issues point at the condition that caused them.
        assert!(issues
            .iter()
            .all(|i| i.target != "name" && i.target != "action"));
    }

    #[test]
    fn keyed_fields_need_a_valid_key() {
        let mut header = cond(Field::Header, Operator::Exists, &[]);
        header.key = Some("Bad Header".into());
        let mut query = cond(Field::Query, Operator::Eq, &["1"]);
        query.key = None;
        let g = rule(vec![
            Expression::Condition(header),
            Expression::Condition(query),
        ]);
        let text = messages("r", &g, &block());
        assert!(text.contains(&"\"Bad Header\" is not a valid header name.".to_string()));
        assert!(text.contains(&"Enter the query parameter name.".to_string()));
    }

    #[test]
    fn single_value_operators_take_one_value() {
        let g = rule(vec![Expression::Condition(cond(
            Field::Path,
            Operator::Eq,
            &["/a", "/b"],
        ))]);
        assert_eq!(
            messages("r", &g, &block()),
            vec!["\"is\" takes one value; use \"is any of\" for a list."]
        );
    }

    #[test]
    fn regex_rejects_lookaround_and_invalid_syntax() {
        for (pattern, expected) in [
            ("^/(?!public)", "Look-ahead"),
            ("(a)\\1", "back-references"),
            ("(", "Invalid regular expression"),
        ] {
            let g = rule(vec![Expression::Condition(cond(
                Field::Path,
                Operator::Regex,
                &[pattern],
            ))]);
            let text = messages("r", &g, &block()).join(" ");
            assert!(text.contains(expected), "{pattern}: {text}");
        }
    }

    #[test]
    fn limits_are_enforced() {
        let many: Vec<String> = (0..65).map(|i| format!("/p{i}")).collect();
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();
        let g = rule(vec![Expression::Condition(cond(
            Field::Path,
            Operator::In,
            &refs,
        ))]);
        assert_eq!(messages("r", &g, &block()), vec!["Use at most 64 values."]);

        let stars = "/*".repeat(9);
        let g = rule(vec![Expression::Condition(cond(
            Field::Path,
            Operator::Glob,
            &[&stars],
        ))]);
        assert_eq!(
            messages("r", &g, &block()),
            vec!["Patterns may use at most 8 * wildcards."]
        );

        let conditions: Vec<Expression> = (0..33)
            .map(|_| Expression::Condition(cond(Field::Path, Operator::StartsWith, &["/a"])))
            .collect();
        let g = rule(conditions);
        assert_eq!(
            messages("r", &g, &block()),
            vec!["A rule can have at most 32 conditions."]
        );

        let mut deep = Group {
            id: String::new(),
            combinator: Combinator::Any,
            children: vec![Expression::Condition(cond(
                Field::Path,
                Operator::StartsWith,
                &["/a"],
            ))],
        };
        for _ in 0..4 {
            deep = Group {
                id: String::new(),
                combinator: Combinator::Any,
                children: vec![Expression::Group(deep)],
            };
        }
        prepare_expression(&mut deep);
        assert!(messages("r", &deep, &block())
            .contains(&"Groups can be nested at most 4 levels deep.".to_string()));
    }

    #[test]
    fn empty_nested_groups_are_rejected_but_an_empty_root_is_not() {
        let g = rule(vec![Expression::Group(Group {
            id: String::new(),
            combinator: Combinator::Any,
            children: vec![],
        })]);
        assert_eq!(
            messages("r", &g, &block()),
            vec!["Add a condition to this group or remove it."]
        );
        assert!(messages("r", &rule(vec![]), &block()).is_empty());
    }

    #[test]
    fn block_status_and_body_are_bounded() {
        let g = rule(vec![]);
        let bad = Action::Block {
            status: 500,
            body: "x".repeat(1025),
        };
        assert_eq!(
            messages("r", &g, &bad),
            vec![
                "Block status must be between 400 and 499.",
                "The response body is limited to 1024 bytes."
            ]
        );
    }

    #[test]
    fn redirect_targets_cannot_become_open_redirects() {
        let g = rule(vec![]);
        let check = |target: &str| {
            messages(
                "r",
                &g,
                &Action::Redirect {
                    target: target.into(),
                    status: 308,
                },
            )
        };
        for ok in [
            "/new",
            "/new?{query}",
            "https://docs.example.com{path}",
            "https://docs.example.com{path}?{query}",
            "https://docs.example.com/",
            "https://docs.example.com",
        ] {
            assert!(check(ok).is_empty(), "{ok}: {:?}", check(ok));
        }
        for (bad, expected) in [
            ("https://example.com{query}", "The host must be followed by"),
            (
                "https://example.com{query}x",
                "The host must be followed by",
            ),
            ("//evil.example", "cannot start with //"),
            ("/\\evil.example", "cannot start with //"),
            ("/{path}", "cannot start with //"),
            ("evil.example/x", "Use a path starting with /"),
            ("javascript:alert(1)", "Use a path starting with /"),
            ("/x{host}", "Unknown placeholder {host}"),
            ("/x{query}", "Put {query} after a ?"),
            ("", "Enter where to redirect to."),
        ] {
            let text = check(bad).join(" ");
            assert!(text.contains(expected), "{bad}: {text}");
        }
        let wrong_status = messages(
            "r",
            &g,
            &Action::Redirect {
                target: "/x".into(),
                status: 303,
            },
        );
        assert_eq!(
            wrong_status,
            vec!["Redirect status must be 301, 302, 307 or 308."]
        );
    }
}
