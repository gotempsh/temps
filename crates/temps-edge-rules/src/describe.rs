// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Plain-English descriptions of rules, matching the console's wording. Used
//! for automatic names, CLI output and MCP proposals, so a person confirming
//! a change reads what the rule will actually do.

use crate::fields::{limits, operator_label, spec, ValueKind};
use crate::model::{Action, Combinator, Condition, Expression, Group};

fn describe_values(c: &Condition) -> String {
    let field = spec(c.field);
    let label = |v: &str| -> String {
        field
            .options
            .iter()
            .find(|(value, _)| *value == v)
            .map(|(_, l)| l.to_string())
            .unwrap_or_else(|| v.to_string())
    };
    let shown: Vec<String> = c.values.iter().take(3).map(|v| label(v)).collect();
    let more = if c.values.len() > 3 {
        format!(" +{} more", c.values.len() - 3)
    } else {
        String::new()
    };
    format!("{}{}", shown.join(", "), more)
}

pub fn describe_condition(c: &Condition) -> String {
    let field = spec(c.field);
    if field.kind == ValueKind::Boolean {
        let label = field.label.to_lowercase();
        return if c.values.first().map(String::as_str) == Some("true") {
            format!("request comes from a {label}")
        } else {
            format!("request does not come from a {label}")
        };
    }
    let subject = match field.keyed {
        Some(kind) => {
            let key = c.key.as_deref().unwrap_or("").trim();
            format!(
                "{} {}",
                kind.label(),
                if key.is_empty() { "…" } else { key }
            )
        }
        None => field.label.to_lowercase(),
    };
    if c.op.is_presence() {
        return format!("{subject} {}", operator_label(c.op));
    }
    let mut value = describe_values(c);
    if value.is_empty() {
        value = "…".into();
    }
    let ci = if c.case_insensitive {
        " (ignoring case)"
    } else {
        ""
    };
    format!("{subject} {} {value}{ci}", operator_label(c.op))
}

fn describe_node(expr: &Expression, nested: bool) -> String {
    match expr {
        Expression::Condition(c) => describe_condition(c),
        Expression::Group(g) => describe_group(g, nested),
    }
}

fn describe_group(g: &Group, nested: bool) -> String {
    if g.children.is_empty() {
        return "every request".into();
    }
    let parts: Vec<String> = g.children.iter().map(|c| describe_node(c, true)).collect();
    match g.combinator {
        Combinator::None => format!("none of ({})", parts.join("; ")),
        Combinator::All | Combinator::Any => {
            let joiner = if g.combinator == Combinator::All {
                " and "
            } else {
                " or "
            };
            let text = parts.join(joiner);
            if nested && parts.len() > 1 {
                format!("({text})")
            } else {
                text
            }
        }
    }
}

pub fn describe_expression(g: &Group) -> String {
    describe_group(g, false)
}

fn action_label(action: &Action) -> &'static str {
    match action {
        Action::Block { .. } => "Block",
        Action::Allow => "Allow",
        Action::Challenge => "Challenge",
        Action::Redirect { .. } => "Redirect",
        Action::Log => "Log",
    }
}

pub fn describe_action(action: &Action) -> String {
    match action {
        Action::Block { status, .. } => format!("Block with {status}"),
        Action::Redirect { target, status } => format!(
            "Redirect ({status}) to {}",
            if target.is_empty() { "…" } else { target }
        ),
        other => action_label(other).to_string(),
    }
}

fn conditions(g: &Group, out: &mut Vec<Condition>) {
    for child in &g.children {
        match child {
            Expression::Condition(c) => out.push(c.clone()),
            Expression::Group(inner) => conditions(inner, out),
        }
    }
}

/// The name a rule gets when saved without one, e.g. "Block when path starts
/// with /admin and 1 more condition". Same rules as the console's `autoName`.
pub fn auto_name(expression: &Group, action: &Action) -> String {
    let mut all = Vec::new();
    conditions(expression, &mut all);
    let verb = action_label(action);
    let what = if all.is_empty() {
        "every request".to_string()
    } else if all.len() == 1 || expression.combinator == Combinator::None {
        describe_expression(expression)
    } else {
        let rest = all.len() - 1;
        format!(
            "{} and {rest} more {}",
            describe_condition(&all[0]),
            if rest == 1 { "condition" } else { "conditions" }
        )
    };
    let name = format!("{verb} when {what}");
    if name.chars().count() <= limits::NAME_LENGTH {
        name
    } else {
        let cut: String = name.chars().take(limits::NAME_LENGTH - 1).collect();
        format!("{cut}…")
    }
}

/// One line for a rule: "Block with 403 when path starts with /admin".
pub fn describe_rule(expression: &Group, action: &Action) -> String {
    format!(
        "{} when {}",
        describe_action(action),
        describe_expression(expression)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Field, Operator};

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

    fn block() -> Action {
        Action::Block {
            status: 403,
            body: String::new(),
        }
    }

    #[test]
    fn describes_nested_groups_like_the_console() {
        let g = Group {
            id: String::new(),
            combinator: Combinator::All,
            children: vec![
                Expression::Condition(cond(Field::Path, Operator::StartsWith, &["/wp-"])),
                Expression::Group(Group {
                    id: String::new(),
                    combinator: Combinator::None,
                    children: vec![Expression::Condition(cond(
                        Field::Country,
                        Operator::In,
                        &["ES", "PT"],
                    ))],
                }),
            ],
        };
        assert_eq!(
            describe_expression(&g),
            "path starts with /wp- and none of (country is any of ES, PT)"
        );
    }

    #[test]
    fn auto_names_match_the_console() {
        let one = Group {
            id: String::new(),
            combinator: Combinator::All,
            children: vec![Expression::Condition(cond(
                Field::Path,
                Operator::StartsWith,
                &["/admin"],
            ))],
        };
        assert_eq!(
            auto_name(&one, &block()),
            "Block when path starts with /admin"
        );
        let three = Group {
            children: vec![
                Expression::Condition(cond(Field::Path, Operator::StartsWith, &["/admin"])),
                Expression::Condition(cond(Field::Country, Operator::In, &["ES", "PT"])),
                Expression::Condition(cond(Field::Method, Operator::Eq, &["POST"])),
            ],
            ..one.clone()
        };
        assert_eq!(
            auto_name(&three, &block()),
            "Block when path starts with /admin and 2 more conditions"
        );
        let long = Group {
            children: vec![Expression::Condition(cond(
                Field::Path,
                Operator::StartsWith,
                &[&format!("/{}", "a".repeat(200))],
            ))],
            ..one
        };
        let name = auto_name(&long, &block());
        assert_eq!(name.chars().count(), limits::NAME_LENGTH);
        assert!(name.ends_with('…'));
    }

    #[test]
    fn describes_values_with_option_labels() {
        let c = cond(Field::Continent, Operator::In, &["EU", "NA", "AS", "OC"]);
        assert_eq!(
            describe_condition(&c),
            "continent is any of Europe, North America, Asia +1 more"
        );
        let hosting = cond(Field::IsHostingProvider, Operator::Eq, &["true"]);
        assert_eq!(
            describe_condition(&hosting),
            "request comes from a hosting provider network"
        );
    }
}
