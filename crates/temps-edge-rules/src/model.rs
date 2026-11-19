// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The edge rule data model (ADR-089 §1–§3), exactly as stored in
//! `project_edge_rules.expression` / `.action` and exchanged over the API.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// A request attribute a condition tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[schema(as = EdgeRuleField)]
#[serde(rename_all = "snake_case")]
pub enum Field {
    /// Client IP, resolved through trusted proxies.
    Ip,
    /// Two-letter ISO country code (GeoIP).
    Country,
    /// Continent code (GeoIP).
    Continent,
    /// Autonomous system number (GeoIP).
    Asn,
    /// Whether the client IP belongs to a hosting/cloud network (GeoIP).
    IsHostingProvider,
    /// Lowercase host without the port.
    Host,
    /// Normalized path without the query string.
    Path,
    /// Path exactly as received.
    RawPath,
    /// HTTP method.
    Method,
    /// A query parameter, named by `key`.
    Query,
    /// A request header, named by `key` (case-insensitive).
    Header,
    /// A cookie, named by `key`.
    Cookie,
    /// The `User-Agent` header.
    UserAgent,
    /// JA4 TLS fingerprint (HTTPS only).
    Ja4,
    /// Crawler / AI agent classification.
    BotClass,
}

/// How a condition compares the request value with its `values`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[schema(as = EdgeRuleOperator)]
#[serde(rename_all = "snake_case")]
pub enum Operator {
    Eq,
    In,
    InCidr,
    StartsWith,
    EndsWith,
    Contains,
    /// `*` matches any run of characters, including `/`.
    Glob,
    /// Rust `regex` syntax: linear time, no look-around or back-references.
    Regex,
    Exists,
    NotExists,
}

impl Operator {
    /// Operators whose `values` are a list.
    pub fn is_list(self) -> bool {
        matches!(self, Operator::In | Operator::InCidr)
    }

    /// Operators that take no value.
    pub fn is_presence(self) -> bool {
        matches!(self, Operator::Exists | Operator::NotExists)
    }
}

/// How a group combines its children. `none` is `not(any(...))`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[schema(as = EdgeRuleCombinator)]
#[serde(rename_all = "snake_case")]
pub enum Combinator {
    All,
    Any,
    None,
}

/// A single test against one request attribute.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as = EdgeRuleCondition)]
pub struct Condition {
    /// Node identifier, unique within the rule. Assigned by the server when
    /// empty; used to attach validation messages to the right condition.
    #[serde(default)]
    pub id: String,
    pub field: Field,
    /// Header, query parameter or cookie name. Required for those fields.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    pub op: Operator,
    /// One value for single-value operators, a list for `in`/`in_cidr`, none
    /// for `exists`/`not_exists`.
    #[serde(default)]
    pub values: Vec<String>,
    /// Compare without case. Hosts, methods, countries and enumerated fields
    /// always compare without case.
    #[serde(default)]
    pub case_insensitive: bool,
}

/// A group of conditions and nested groups.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as = EdgeRuleGroup)]
pub struct Group {
    #[serde(default)]
    pub id: String,
    pub combinator: Combinator,
    #[serde(default)]
    #[schema(no_recursion)]
    pub children: Vec<Expression>,
}

/// A node of a rule's match expression.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as = EdgeRuleExpression)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Expression {
    Condition(Condition),
    Group(Group),
}

impl Expression {
    pub fn id(&self) -> &str {
        match self {
            Expression::Condition(c) => &c.id,
            Expression::Group(g) => &g.id,
        }
    }
}

/// What a matching rule does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as = EdgeRuleAction)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    /// Return an error response. Status 400–499.
    Block {
        status: u16,
        /// Plain-text body, up to 1024 bytes. Empty for a short default.
        #[serde(default)]
        body: String,
    },
    /// Skip the remaining edge rules. Rate limits, attack mode and password
    /// protection still apply.
    Allow,
    /// Serve the proof-of-work challenge unless the visitor holds clearance.
    Challenge,
    /// Redirect to a path or an absolute URL with a fixed host. `{path}` and
    /// `{query}` insert the visitor's normalized path and raw query string.
    Redirect { target: String, status: u16 },
    /// Record the match and keep evaluating.
    Log,
}

impl Action {
    pub fn kind(&self) -> ActionKind {
        match self {
            Action::Block { .. } => ActionKind::Block,
            Action::Allow => ActionKind::Allow,
            Action::Challenge => ActionKind::Challenge,
            Action::Redirect { .. } => ActionKind::Redirect,
            Action::Log => ActionKind::Log,
        }
    }
}

/// The action type without its parameters. Used for metric labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[schema(as = EdgeRuleActionKind)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    Block,
    Allow,
    Challenge,
    Redirect,
    Log,
}

impl ActionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ActionKind::Block => "block",
            ActionKind::Allow => "allow",
            ActionKind::Challenge => "challenge",
            ActionKind::Redirect => "redirect",
            ActionKind::Log => "log",
        }
    }
}

/// `log_only` evaluates the rule and records matches but never acts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[schema(as = EdgeRuleMode)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Enforce,
    LogOnly,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Enforce => "enforce",
            Mode::LogOnly => "log_only",
        }
    }

    pub fn parse(value: &str) -> Option<Mode> {
        match value {
            "enforce" => Some(Mode::Enforce),
            "log_only" => Some(Mode::LogOnly),
            _ => None,
        }
    }
}

/// A rule as the engine sees it: everything needed to compile and evaluate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleDefinition {
    pub id: i32,
    pub revision: i64,
    pub name: String,
    pub enabled: bool,
    pub mode: Mode,
    /// `None` applies the rule to every environment of the project.
    pub environment_id: Option<i32>,
    pub expression: Group,
    pub action: Action,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expression_round_trips_with_kind_tags() {
        let json = serde_json::json!({
            "id": "root",
            "combinator": "all",
            "children": [
                {"kind": "condition", "id": "c1", "field": "path", "op": "starts_with",
                 "values": ["/admin"], "case_insensitive": true},
                {"kind": "group", "id": "g1", "combinator": "none", "children": [
                    {"kind": "condition", "field": "ip", "op": "in_cidr", "values": ["203.0.113.0/24"]}
                ]}
            ]
        });
        let group: Group = serde_json::from_value(json.clone()).expect("valid expression");
        assert_eq!(group.children.len(), 2);
        match &group.children[1] {
            Expression::Group(g) => {
                assert_eq!(g.combinator, Combinator::None);
                match &g.children[0] {
                    Expression::Condition(c) => {
                        assert_eq!(c.id, "");
                        assert!(!c.case_insensitive);
                    }
                    other => panic!("expected condition, got {other:?}"),
                }
            }
            other => panic!("expected group, got {other:?}"),
        }
        let back = serde_json::to_value(&group).expect("serializes");
        let again: Group = serde_json::from_value(back).expect("round trip");
        assert_eq!(again, group);
    }

    #[test]
    fn actions_are_tagged_by_type() {
        let block: Action =
            serde_json::from_str(r#"{"type":"block","status":404}"#).expect("block");
        assert_eq!(
            block,
            Action::Block {
                status: 404,
                body: String::new()
            }
        );
        let allow: Action = serde_json::from_str(r#"{"type":"allow"}"#).expect("allow");
        assert_eq!(allow.kind(), ActionKind::Allow);
        assert!(serde_json::from_str::<Action>(r#"{"type":"teleport"}"#).is_err());
    }

    #[test]
    fn unknown_fields_and_operators_are_rejected() {
        let bad = r#"{"kind":"condition","field":"planet","op":"eq","values":["x"]}"#;
        assert!(serde_json::from_str::<Expression>(bad).is_err());
        let bad_op = r#"{"kind":"condition","field":"path","op":"neq","values":["x"]}"#;
        assert!(serde_json::from_str::<Expression>(bad_op).is_err());
    }
}
