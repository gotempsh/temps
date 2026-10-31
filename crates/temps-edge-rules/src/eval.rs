// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Compiled rule programs and their evaluation (ADR-071 §4, §7).
//!
//! A [`Program`] is built once per change to a project's rules and shared
//! read-only between requests. Evaluation borrows everything from the request
//! through [`RequestView`], so a request that matches no rule performs no
//! allocation unless a rule needs a decoded query parameter or a normalized
//! path that differs from the raw one.

use std::borrow::Cow;
use std::cell::OnceCell;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};

use regex::Regex;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::error::EdgeRuleError;
use crate::fields::{always_case_insensitive, spec, ValueKind};
use crate::ip::{parse_cidr, parse_ip, CidrSet};
use crate::model::{
    Action, Combinator, Condition, Expression, Field, Group, Mode, Operator, RuleDefinition,
};
use crate::normalize::normalize_path;
use crate::validate::build_regex;

/// Result of visiting a multi-valued request attribute.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Visit {
    /// At least one value was present.
    pub found: bool,
    /// The visitor returned `true` for some value.
    pub matched: bool,
}

/// GeoIP facts about the client. Every field is `None` when the GeoIP
/// database is not installed, so geo conditions never match.
#[derive(Debug, Clone, Copy, Default)]
pub struct GeoFacts<'a> {
    /// Upper-case ISO code.
    pub country: Option<&'a str>,
    pub continent: Option<&'a str>,
    pub asn: Option<u32>,
    pub is_hosting_provider: Option<bool>,
}

/// What the engine needs from a request. The proxy implements it over the
/// Pingora session; [`SampleRequest`] implements it for dry runs.
///
/// Implementations may compute expensive facts (GeoIP, bot class, clearance)
/// lazily: the engine only asks when a rule needs them.
pub trait RequestView {
    /// Upper-case method.
    fn method(&self) -> &str;
    /// Lower-case host without the port; empty if unknown.
    fn host(&self) -> &str;
    /// Path exactly as received, without the query string.
    fn raw_path(&self) -> &str;
    /// Query string without the `?`; empty if none.
    fn raw_query(&self) -> &str;
    /// Calls `visit` with every value of the header `lower_name`, stopping at
    /// the first `true`.
    fn visit_header(&self, lower_name: &str, visit: &mut dyn FnMut(&str) -> bool) -> Visit;
    fn client_ip(&self) -> Option<IpAddr>;
    fn geo(&self) -> GeoFacts<'_>;
    fn ja4(&self) -> Option<&str>;
    fn bot_class(&self) -> Option<&str>;
    fn environment_id(&self) -> Option<i32>;
    /// The visitor holds valid challenge clearance for this project and
    /// environment.
    fn challenge_cleared(&self) -> bool;
}

/// How each rule fared, for the dry-run trace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as = EdgeRuleOutcome)]
#[serde(rename_all = "snake_case")]
pub enum RuleOutcome {
    Disabled,
    OtherEnvironment,
    NotReached,
    NoMatch,
    /// An `allow` rule that tests the path skipped an ambiguous path.
    AmbiguousPath,
    /// Matched in `log_only` mode or with the `log` action.
    Logged,
    /// A `challenge` rule matched a visitor who already holds clearance.
    ChallengePassed,
    /// Matched and decided the request.
    Decided,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as = EdgeRuleTrace)]
pub struct RuleTrace {
    pub rule_id: i32,
    pub outcome: RuleOutcome,
}

/// The final decision for a request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as = EdgeRuleDecision)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Decision {
    /// No rule decided: the request continues down the pipeline.
    Forward,
    Block {
        rule_id: i32,
        status: u16,
        body: String,
    },
    Allow {
        rule_id: i32,
    },
    Challenge {
        rule_id: i32,
    },
    Redirect {
        rule_id: i32,
        status: u16,
        location: String,
    },
}

/// Evaluation result. `first_match` is the rule recorded in the proxy log:
/// the deciding rule, or else the first rule that matched in log-only mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evaluation {
    pub decision: Decision,
    pub first_match: Option<MatchRef>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MatchRef {
    pub rule_id: i32,
    /// Whether the rule acted (`enforce`) or only logged.
    pub mode: Mode,
}

enum Matcher {
    Exists,
    NotExists,
    /// Exact, case-sensitive comparison against any of the values.
    Exact(Vec<Box<str>>),
    /// Exact comparison ignoring ASCII and Unicode simple case.
    ExactFolded(Vec<Box<str>>),
    StartsWith(Box<str>),
    EndsWith(Box<str>),
    Contains(Box<str>),
    /// Glob, regex, and every case-insensitive substring test.
    Pattern(Regex),
    Cidr(CidrSet),
    Asn(Vec<u32>),
}

struct CompiledCondition {
    field: Field,
    /// Lower-case for headers; as written for query parameters and cookies.
    key: Box<str>,
    matcher: Matcher,
}

enum Node {
    Condition(CompiledCondition),
    Group(Combinator, Vec<Node>),
}

struct CompiledRule {
    id: i32,
    enabled: bool,
    mode: Mode,
    environment_id: Option<i32>,
    action: Action,
    references_path: bool,
    root: Node,
    hits: AtomicU64,
}

/// A project's rules, compiled and ready to evaluate.
pub struct Program {
    rules: Vec<CompiledRule>,
    /// Some enabled rule tests the normalized path or is an `allow` rule
    /// that needs the canonical-path check.
    needs_path: bool,
}

fn fold(s: &str) -> String {
    s.to_lowercase()
}

fn escape_for(value: &str) -> String {
    regex::escape(value)
}

fn compile_pattern(
    source: String,
    case_insensitive: bool,
    rule_id: i32,
) -> Result<Regex, EdgeRuleError> {
    build_regex(&source, case_insensitive)
        .map_err(|reason| EdgeRuleError::Compile { rule_id, reason })
}

fn condition_case_insensitive(c: &Condition) -> bool {
    always_case_insensitive(c.field) || (spec(c.field).case_flag && c.case_insensitive)
}

/// Whether [`compile_condition`] builds a regex automaton for this condition:
/// every `regex` and `glob`, and `starts_with`/`ends_with`/`contains` when
/// they ignore case. Everything else is a plain comparison. Budgeted per
/// project ([`crate::PatternUsage`]) because each automaton is held in every
/// proxy's memory.
pub fn compiles_to_pattern(c: &Condition) -> bool {
    if c.field == Field::Asn {
        return false;
    }
    match c.op {
        Operator::Exists | Operator::NotExists | Operator::InCidr | Operator::Eq | Operator::In => {
            false
        }
        Operator::StartsWith | Operator::EndsWith | Operator::Contains => {
            condition_case_insensitive(c)
        }
        Operator::Glob | Operator::Regex => true,
    }
}

fn compile_condition(c: &Condition, rule_id: i32) -> Result<CompiledCondition, EdgeRuleError> {
    let ci = condition_case_insensitive(c);
    let key: Box<str> = match c.field {
        Field::Header => c
            .key
            .as_deref()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase()
            .into(),
        Field::Query | Field::Cookie => c.key.as_deref().unwrap_or("").trim().into(),
        _ => "".into(),
    };
    let first = || c.values.first().map(|v| v.as_str()).unwrap_or("");
    let matcher = match c.op {
        Operator::Exists => Matcher::Exists,
        Operator::NotExists => Matcher::NotExists,
        Operator::InCidr => {
            let mut cidrs = Vec::with_capacity(c.values.len());
            for v in &c.values {
                let cidr = parse_cidr(v).ok_or_else(|| EdgeRuleError::Compile {
                    rule_id,
                    reason: format!("{v} is not a valid IP address or CIDR range"),
                })?;
                cidrs.push(cidr);
            }
            Matcher::Cidr(CidrSet::new(cidrs))
        }
        _ if c.field == Field::Asn => {
            let mut asns = Vec::with_capacity(c.values.len());
            for v in &c.values {
                asns.push(parse_asn(v).ok_or_else(|| EdgeRuleError::Compile {
                    rule_id,
                    reason: format!("{v} is not an AS number"),
                })?);
            }
            Matcher::Asn(asns)
        }
        Operator::Eq | Operator::In => {
            let values: Vec<&str> = if c.op == Operator::Eq {
                vec![first()]
            } else {
                c.values.iter().map(String::as_str).collect()
            };
            if ci {
                Matcher::ExactFolded(values.into_iter().map(|v| fold(v).into()).collect())
            } else {
                Matcher::Exact(values.into_iter().map(Into::into).collect())
            }
        }
        Operator::StartsWith if !ci => Matcher::StartsWith(first().into()),
        Operator::EndsWith if !ci => Matcher::EndsWith(first().into()),
        Operator::Contains if !ci => Matcher::Contains(first().into()),
        Operator::StartsWith => Matcher::Pattern(compile_pattern(
            format!("(?s)^{}", escape_for(first())),
            true,
            rule_id,
        )?),
        Operator::EndsWith => Matcher::Pattern(compile_pattern(
            format!("(?s){}$", escape_for(first())),
            true,
            rule_id,
        )?),
        Operator::Contains => {
            Matcher::Pattern(compile_pattern(escape_for(first()), true, rule_id)?)
        }
        Operator::Glob => {
            let body: Vec<String> = first().split('*').map(escape_for).collect();
            Matcher::Pattern(compile_pattern(
                format!("(?s)^{}$", body.join(".*")),
                ci,
                rule_id,
            )?)
        }
        Operator::Regex => Matcher::Pattern(compile_pattern(first().to_string(), ci, rule_id)?),
    };
    Ok(CompiledCondition {
        field: c.field,
        key,
        matcher,
    })
}

/// An AS number, with or without the `AS` prefix: `AS64496` or `64496`.
/// `None` for anything else, including values that do not fit in 32 bits.
/// Slices only at ASCII boundaries, so no input can make it panic.
pub fn parse_asn(v: &str) -> Option<u32> {
    let t = v.trim();
    let digits = match t.get(..2) {
        Some(prefix) if prefix.eq_ignore_ascii_case("AS") => &t[2..],
        _ => t,
    };
    if digits.is_empty() || digits.len() > 10 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

fn compile_group(g: &Group, rule_id: i32) -> Result<Node, EdgeRuleError> {
    let mut children = Vec::with_capacity(g.children.len());
    for child in &g.children {
        children.push(match child {
            Expression::Condition(c) => Node::Condition(compile_condition(c, rule_id)?),
            Expression::Group(inner) => compile_group(inner, rule_id)?,
        });
    }
    Ok(Node::Group(g.combinator, children))
}

fn references(g: &Group, field: Field) -> bool {
    g.children.iter().any(|child| match child {
        Expression::Condition(c) => c.field == field,
        Expression::Group(inner) => references(inner, field),
    })
}

impl Program {
    /// Compiles rules in evaluation order. Rules must already be valid; a
    /// failure here means stored data bypassed validation.
    pub fn compile(rules: &[RuleDefinition]) -> Result<Program, EdgeRuleError> {
        let mut compiled = Vec::with_capacity(rules.len());
        let mut needs_path = false;
        for rule in rules {
            let references_path = references(&rule.expression, Field::Path);
            if rule.enabled
                && (references_path || rule.action.kind() == crate::model::ActionKind::Allow)
            {
                needs_path = true;
            }
            compiled.push(CompiledRule {
                id: rule.id,
                enabled: rule.enabled,
                mode: rule.mode,
                environment_id: rule.environment_id,
                action: rule.action.clone(),
                references_path,
                root: compile_group(&rule.expression, rule.id)?,
                hits: AtomicU64::new(0),
            });
        }
        Ok(Program {
            rules: compiled,
            needs_path,
        })
    }

    pub fn is_empty(&self) -> bool {
        self.rules.iter().all(|r| !r.enabled)
    }

    pub fn len(&self) -> usize {
        self.rules.len()
    }

    /// Hot-path evaluation: no trace.
    pub fn evaluate(&self, request: &dyn RequestView) -> Evaluation {
        self.run(request, None)
    }

    /// Dry-run evaluation with a per-rule trace. Does not count hits.
    pub fn evaluate_traced(&self, request: &dyn RequestView) -> (Evaluation, Vec<RuleTrace>) {
        let mut trace = Vec::with_capacity(self.rules.len());
        let evaluation = self.run(request, Some(&mut trace));
        (evaluation, trace)
    }

    /// Takes and resets the per-rule match counters: `(rule_id, hits)` for
    /// every rule with at least one hit since the last drain.
    pub fn drain_hits(&self) -> Vec<(i32, u64)> {
        self.rules
            .iter()
            .filter_map(|r| {
                let n = r.hits.swap(0, Ordering::Relaxed);
                (n > 0).then_some((r.id, n))
            })
            .collect()
    }

    fn run(&self, request: &dyn RequestView, mut trace: Option<&mut Vec<RuleTrace>>) -> Evaluation {
        let counting = trace.is_none();
        let ctx = EvalContext {
            request,
            path: OnceCell::new(),
        };
        let mut decision = Decision::Forward;
        let mut first_match: Option<MatchRef> = None;
        let mut decided = false;
        for rule in &self.rules {
            let outcome = self.rule_outcome(rule, &ctx, decided);
            if let Some(t) = trace.as_deref_mut() {
                t.push(RuleTrace {
                    rule_id: rule.id,
                    outcome,
                });
            } else if decided {
                break;
            }
            match outcome {
                RuleOutcome::Logged | RuleOutcome::ChallengePassed => {
                    if counting {
                        rule.hits.fetch_add(1, Ordering::Relaxed);
                    }
                    if first_match.is_none() {
                        first_match = Some(MatchRef {
                            rule_id: rule.id,
                            mode: Mode::LogOnly,
                        });
                    }
                }
                RuleOutcome::Decided => {
                    if counting {
                        rule.hits.fetch_add(1, Ordering::Relaxed);
                    }
                    decided = true;
                    first_match = Some(MatchRef {
                        rule_id: rule.id,
                        mode: Mode::Enforce,
                    });
                    decision = decide(rule, &ctx);
                }
                _ => {}
            }
        }
        Evaluation {
            decision,
            first_match,
        }
    }

    fn rule_outcome(
        &self,
        rule: &CompiledRule,
        ctx: &EvalContext<'_>,
        decided: bool,
    ) -> RuleOutcome {
        if !rule.enabled {
            return RuleOutcome::Disabled;
        }
        if let (Some(scope), Some(env)) = (rule.environment_id, ctx.request.environment_id()) {
            if scope != env {
                return RuleOutcome::OtherEnvironment;
            }
        }
        if decided {
            return RuleOutcome::NotReached;
        }
        // An allow rule that looks at the path must not match an ambiguous
        // path: the upstream may read it differently. The whole rule is
        // skipped so a `none` group cannot invert the check (ADR-071 §2).
        if matches!(rule.action, Action::Allow)
            && rule.references_path
            && self.needs_path
            && !ctx.path().1
        {
            return RuleOutcome::AmbiguousPath;
        }
        if !eval_node(&rule.root, ctx) {
            return RuleOutcome::NoMatch;
        }
        if rule.mode == Mode::LogOnly || matches!(rule.action, Action::Log) {
            return RuleOutcome::Logged;
        }
        if matches!(rule.action, Action::Challenge) && ctx.request.challenge_cleared() {
            return RuleOutcome::ChallengePassed;
        }
        RuleOutcome::Decided
    }
}

fn decide(rule: &CompiledRule, ctx: &EvalContext<'_>) -> Decision {
    match &rule.action {
        Action::Block { status, body } => Decision::Block {
            rule_id: rule.id,
            status: *status,
            body: body.clone(),
        },
        Action::Allow => Decision::Allow { rule_id: rule.id },
        Action::Challenge => Decision::Challenge { rule_id: rule.id },
        Action::Redirect { target, status } => Decision::Redirect {
            rule_id: rule.id,
            status: *status,
            location: redirect_location(target, &ctx.path().0, ctx.request.raw_query()),
        },
        // Log never decides; kept for exhaustiveness.
        Action::Log => Decision::Forward,
    }
}

/// `{path}` is the normalized path with its leading slash; `{query}` is the
/// raw query string without the `?`. Validation guarantees neither can change
/// the destination host.
pub fn redirect_location(target: &str, path: &str, raw_query: &str) -> String {
    target
        .trim()
        .replace("{path}", path)
        .replace("{query}", raw_query)
}

struct EvalContext<'r> {
    request: &'r dyn RequestView,
    path: OnceCell<(Cow<'r, str>, bool)>,
}

impl<'r> EvalContext<'r> {
    fn path(&self) -> &(Cow<'r, str>, bool) {
        self.path.get_or_init(|| {
            let n = normalize_path(self.request.raw_path());
            (n.path, n.canonical)
        })
    }
}

fn eval_node(node: &Node, ctx: &EvalContext<'_>) -> bool {
    match node {
        Node::Condition(c) => eval_condition(c, ctx),
        Node::Group(_, children) if children.is_empty() => true,
        Node::Group(Combinator::All, children) => children.iter().all(|n| eval_node(n, ctx)),
        Node::Group(Combinator::Any, children) => children.iter().any(|n| eval_node(n, ctx)),
        Node::Group(Combinator::None, children) => !children.iter().any(|n| eval_node(n, ctx)),
    }
}

fn test_value(matcher: &Matcher, candidate: &str) -> bool {
    match matcher {
        Matcher::Exists | Matcher::NotExists | Matcher::Cidr(_) => false,
        Matcher::Exact(values) => values.iter().any(|v| &**v == candidate),
        Matcher::ExactFolded(values) => {
            // ASCII fast path avoids allocating for the common case.
            if candidate.is_ascii() {
                values
                    .iter()
                    .any(|v| v.is_ascii() && v.eq_ignore_ascii_case(candidate))
            } else {
                let folded = fold(candidate);
                values.iter().any(|v| **v == *folded)
            }
        }
        Matcher::StartsWith(v) => candidate.starts_with(&**v),
        Matcher::EndsWith(v) => candidate.ends_with(&**v),
        Matcher::Contains(v) => candidate.contains(&**v),
        Matcher::Pattern(re) => re.is_match(candidate),
        Matcher::Asn(values) => parse_asn(candidate).is_some_and(|n| values.contains(&n)),
    }
}

/// Applies the matcher to every value of the field. A missing value never
/// satisfies a value test; only `not_exists` matches absence.
fn eval_condition(c: &CompiledCondition, ctx: &EvalContext<'_>) -> bool {
    let req = ctx.request;
    let mut visit = |candidate: &str| test_value(&c.matcher, candidate);
    let result: Visit = match c.field {
        Field::Path => single(Some(&ctx.path().0), &mut visit),
        Field::RawPath => single(Some(req.raw_path()), &mut visit),
        Field::Host => single(non_empty(req.host()), &mut visit),
        Field::Method => single(Some(req.method()), &mut visit),
        Field::Header => req.visit_header(&c.key, &mut visit),
        Field::UserAgent => req.visit_header("user-agent", &mut visit),
        Field::Query => visit_query(req.raw_query(), &c.key, &mut visit),
        Field::Cookie => {
            let key = &*c.key;
            let mut found = false;
            let mut inner = |header: &str| {
                for pair in header.split(';') {
                    if let Some((name, value)) = pair.split_once('=') {
                        if !name.trim().is_empty() && name.trim() == key {
                            found = true;
                            if visit(value.trim()) {
                                return true;
                            }
                        }
                    }
                }
                false
            };
            let v = req.visit_header("cookie", &mut inner);
            Visit {
                found,
                matched: v.matched,
            }
        }
        Field::Country => single(req.geo().country, &mut visit),
        Field::Continent => single(req.geo().continent, &mut visit),
        Field::Asn => match req.geo().asn {
            Some(n) => {
                let text = n.to_string();
                single(Some(&text), &mut visit)
            }
            None => Visit::default(),
        },
        Field::IsHostingProvider => match req.geo().is_hosting_provider {
            Some(true) => single(Some("true"), &mut visit),
            Some(false) => single(Some("false"), &mut visit),
            None => Visit::default(),
        },
        Field::Ja4 => single(req.ja4().and_then(non_empty), &mut visit),
        Field::BotClass => single(req.bot_class().and_then(non_empty), &mut visit),
        Field::Ip => {
            return match (&c.matcher, req.client_ip()) {
                (Matcher::Exists, ip) => ip.is_some(),
                (Matcher::NotExists, ip) => ip.is_none(),
                (Matcher::Cidr(set), Some(ip)) => set.contains(ip),
                _ => false,
            };
        }
    };
    match c.matcher {
        Matcher::Exists => result.found,
        Matcher::NotExists => !result.found,
        _ => result.found && result.matched,
    }
}

fn non_empty(s: &str) -> Option<&str> {
    (!s.is_empty()).then_some(s)
}

fn single(value: Option<&str>, visit: &mut dyn FnMut(&str) -> bool) -> Visit {
    match value {
        Some(v) => Visit {
            found: true,
            matched: visit(v),
        },
        None => Visit::default(),
    }
}

/// Decodes a query component like `decodeURIComponent` after `+` → space;
/// malformed input is returned as written.
fn decode_component(raw: &str) -> Cow<'_, str> {
    if !raw.contains(['%', '+']) {
        return Cow::Borrowed(raw);
    }
    let spaced = raw.replace('+', " ");
    match percent_encoding::percent_decode_str(&spaced).decode_utf8() {
        Ok(decoded) => Cow::Owned(decoded.into_owned()),
        Err(_) => Cow::Borrowed(raw),
    }
}

fn visit_query(raw_query: &str, key: &str, visit: &mut dyn FnMut(&str) -> bool) -> Visit {
    let mut result = Visit::default();
    for pair in raw_query.split('&').filter(|p| !p.is_empty()) {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        if decode_component(name) != key {
            continue;
        }
        result.found = true;
        if visit(&decode_component(value)) {
            result.matched = true;
            break;
        }
    }
    result
}

// ---------------------------------------------------------------------------
// Dry-run requests
// ---------------------------------------------------------------------------

/// A request described in JSON, for `POST /edge-rules/evaluate`, the CLI's
/// `test` command and the conformance fixtures.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as = EdgeRuleSampleRequest)]
pub struct SampleRequest {
    #[serde(default = "default_method")]
    pub method: String,
    /// Absolute URL (`https://app.example.com/path?q`) or origin-form path.
    pub url: String,
    #[serde(default)]
    pub headers: Vec<SampleHeader>,
    /// Shorthand for a `User-Agent` header; ignored if `headers` has one.
    #[serde(default)]
    pub user_agent: Option<String>,
    /// Client IP as the proxy would resolve it.
    #[serde(default)]
    pub ip: Option<String>,
    #[serde(default)]
    pub country: Option<String>,
    #[serde(default)]
    pub continent: Option<String>,
    #[serde(default)]
    pub asn: Option<u32>,
    #[serde(default)]
    pub is_hosting_provider: Option<bool>,
    #[serde(default)]
    pub ja4: Option<String>,
    #[serde(default)]
    pub bot_class: Option<String>,
    #[serde(default)]
    pub environment_id: Option<i32>,
    /// Pretend the visitor already holds challenge clearance.
    #[serde(default)]
    pub challenge_cleared: bool,
}

fn default_method() -> String {
    "GET".into()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as = EdgeRuleSampleHeader)]
pub struct SampleHeader {
    pub name: String,
    pub value: String,
}

/// A [`SampleRequest`] split into the parts the engine reads.
pub struct ParsedSample {
    sample: SampleRequest,
    method: String,
    host: String,
    raw_path: String,
    raw_query: String,
    headers: Vec<(String, String)>,
    ip: Option<IpAddr>,
    country: Option<String>,
}

impl ParsedSample {
    pub fn new(sample: SampleRequest) -> ParsedSample {
        let url = sample.url.trim();
        let (mut host, raw_path, raw_query) = split_url(url);
        if let Some(at) = host.rfind('@') {
            host = host[at + 1..].to_string();
        }
        host = host.to_lowercase();
        host = strip_port(&host);
        let mut headers: Vec<(String, String)> = sample
            .headers
            .iter()
            .map(|h| {
                (
                    h.name.trim().to_ascii_lowercase(),
                    h.value.trim().to_string(),
                )
            })
            .filter(|(n, _)| !n.is_empty())
            .collect();
        if let Some(ua) = sample
            .user_agent
            .as_deref()
            .map(str::trim)
            .filter(|u| !u.is_empty())
        {
            if !headers.iter().any(|(n, _)| n == "user-agent") {
                headers.push(("user-agent".into(), ua.to_string()));
            }
        }
        if !host.is_empty() && !headers.iter().any(|(n, _)| n == "host") {
            headers.push(("host".into(), host.clone()));
        }
        let ip = sample.ip.as_deref().and_then(parse_ip);
        let country = sample
            .country
            .as_deref()
            .map(|c| c.trim().to_uppercase())
            .filter(|c| !c.is_empty());
        ParsedSample {
            method: sample.method.trim().to_uppercase(),
            host,
            raw_path,
            raw_query,
            headers,
            ip,
            country,
            sample,
        }
    }

    pub fn host(&self) -> &str {
        &self.host
    }
}

fn split_url(url: &str) -> (String, String, String) {
    // scheme://authority/path?query#fragment
    let scheme_end = url.find("://").filter(|&i| {
        i > 0
            && url[..i].starts_with(|c: char| c.is_ascii_alphabetic())
            && url[..i]
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
    });
    let (host, rest) = match scheme_end {
        Some(i) => {
            let after = &url[i + 3..];
            let end = after.find(['/', '?', '#']).unwrap_or(after.len());
            (after[..end].to_string(), &after[end..])
        }
        None => (String::new(), url),
    };
    let rest = rest.split('#').next().unwrap_or("");
    let (path, query) = match rest.find('?') {
        Some(q) => (&rest[..q], &rest[q + 1..]),
        None => (rest, ""),
    };
    let path = if path.is_empty() { "/" } else { path };
    (host, path.to_string(), query.to_string())
}

fn strip_port(host: &str) -> String {
    if host.starts_with('[') {
        if let Some(end) = host.find(']') {
            return host[..=end].to_string();
        }
        return host.to_string();
    }
    match host.rsplit_once(':') {
        Some((h, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => {
            h.to_string()
        }
        _ => host.to_string(),
    }
}

impl RequestView for ParsedSample {
    fn method(&self) -> &str {
        &self.method
    }
    fn host(&self) -> &str {
        &self.host
    }
    fn raw_path(&self) -> &str {
        &self.raw_path
    }
    fn raw_query(&self) -> &str {
        &self.raw_query
    }
    fn visit_header(&self, lower_name: &str, visit: &mut dyn FnMut(&str) -> bool) -> Visit {
        let mut result = Visit::default();
        for (name, value) in &self.headers {
            if name == lower_name {
                result.found = true;
                if visit(value) {
                    result.matched = true;
                    break;
                }
            }
        }
        result
    }
    fn client_ip(&self) -> Option<IpAddr> {
        self.ip
    }
    fn geo(&self) -> GeoFacts<'_> {
        GeoFacts {
            country: self.country.as_deref(),
            continent: self.sample.continent.as_deref().filter(|s| !s.is_empty()),
            asn: self.sample.asn,
            is_hosting_provider: self.sample.is_hosting_provider,
        }
    }
    fn ja4(&self) -> Option<&str> {
        self.sample.ja4.as_deref()
    }
    fn bot_class(&self) -> Option<&str> {
        self.sample.bot_class.as_deref()
    }
    fn environment_id(&self) -> Option<i32> {
        self.sample.environment_id
    }
    fn challenge_cleared(&self) -> bool {
        self.sample.challenge_cleared
    }
}

/// Whether any enabled rule needs GeoIP facts, so the proxy can skip the
/// lookup otherwise.
pub fn needs_geo(rules: &[RuleDefinition]) -> bool {
    fn any_geo(g: &Group) -> bool {
        g.children.iter().any(|child| match child {
            Expression::Condition(c) => spec(c.field).geo,
            Expression::Group(inner) => any_geo(inner),
        })
    }
    rules.iter().any(|r| r.enabled && any_geo(&r.expression))
}

/// Whether the value kind of a field is enumerated (used by describe).
pub fn is_enumerated(field: Field) -> bool {
    spec(field).kind == ValueKind::Options
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fields::limits;
    use crate::model::Combinator;

    const ALL_FIELDS: [Field; 15] = [
        Field::Ip,
        Field::Country,
        Field::Continent,
        Field::Asn,
        Field::IsHostingProvider,
        Field::Host,
        Field::Path,
        Field::RawPath,
        Field::Method,
        Field::Query,
        Field::Header,
        Field::Cookie,
        Field::UserAgent,
        Field::Ja4,
        Field::BotClass,
    ];

    /// The budget counts what the compiler builds; if the two disagree, a
    /// project can hold more automatons than its budget says.
    #[test]
    fn compiles_to_pattern_agrees_with_the_compiler() {
        for field in ALL_FIELDS {
            for &op in spec(field).operators {
                for case_insensitive in [false, true] {
                    let value = match (field, op) {
                        (_, Operator::InCidr) => "10.0.0.0/8",
                        (Field::Asn, _) => "64496",
                        _ => "x",
                    };
                    let c = Condition {
                        id: String::new(),
                        field,
                        key: Some("k".into()),
                        op,
                        values: vec![value.into()],
                        case_insensitive,
                    };
                    let compiled = match compile_condition(&c, 1) {
                        Ok(compiled) => compiled,
                        Err(e) => panic!("{field:?} {op:?} did not compile: {e}"),
                    };
                    assert_eq!(
                        compiles_to_pattern(&c),
                        matches!(compiled.matcher, Matcher::Pattern(_)),
                        "{field:?} {op:?} case_insensitive={case_insensitive}"
                    );
                }
            }
        }
    }

    /// The memory estimate in `limits::PATTERN_CONDITIONS_PER_PROJECT`: the
    /// largest pattern a non-regex condition can produce (a full-length value
    /// of the letters with the most case variants, every wildcard used, case
    /// ignored) fits in 64 KiB. Measured at about 61 KiB.
    #[test]
    fn literal_patterns_stay_within_their_memory_estimate() {
        let mut glob = "ks".repeat((limits::VALUE_BYTES - limits::GLOB_WILDCARDS) / 2);
        for i in 0..limits::GLOB_WILDCARDS {
            glob.insert(1 + i * 2, '*');
        }
        assert!(glob.len() <= limits::VALUE_BYTES);
        let literal = "k".repeat(limits::VALUE_BYTES);
        let sources = [
            format!(
                "(?s)^{}$",
                glob.split('*')
                    .map(escape_for)
                    .collect::<Vec<_>>()
                    .join(".*")
            ),
            format!("(?s)^{}", escape_for(&literal)),
            escape_for(&"\u{3b8}".repeat(limits::VALUE_BYTES / 2)),
        ];
        for source in sources {
            let built = regex::RegexBuilder::new(&source)
                .case_insensitive(true)
                .size_limit(64 * 1024)
                .build();
            assert!(
                built.is_ok(),
                "{} bytes of pattern exceeded 64 KiB",
                source.len()
            );
        }
    }

    fn cond(field: Field, op: Operator, values: &[&str]) -> Expression {
        Expression::Condition(Condition {
            id: String::new(),
            field,
            key: None,
            op,
            values: values.iter().map(|v| v.to_string()).collect(),
            case_insensitive: false,
        })
    }

    fn keyed(field: Field, key: &str, op: Operator, values: &[&str]) -> Expression {
        match cond(field, op, values) {
            Expression::Condition(mut c) => {
                c.key = Some(key.into());
                Expression::Condition(c)
            }
            other => other,
        }
    }

    fn group(combinator: Combinator, children: Vec<Expression>) -> Group {
        Group {
            id: String::new(),
            combinator,
            children,
        }
    }

    fn rule(id: i32, expression: Group, action: Action) -> RuleDefinition {
        RuleDefinition {
            id,
            revision: 1,
            name: format!("rule {id}"),
            enabled: true,
            mode: Mode::Enforce,
            environment_id: None,
            expression,
            action,
        }
    }

    fn block(status: u16) -> Action {
        Action::Block {
            status,
            body: String::new(),
        }
    }

    fn sample(url: &str) -> ParsedSample {
        ParsedSample::new(SampleRequest {
            method: "GET".into(),
            url: url.into(),
            ip: Some("198.51.100.20".into()),
            ..Default::default()
        })
    }

    fn decide_for(rules: &[RuleDefinition], req: &ParsedSample) -> Decision {
        match Program::compile(rules) {
            Ok(p) => p.evaluate(req).decision,
            Err(e) => panic!("compile failed: {e}"),
        }
    }

    #[test]
    fn first_terminal_match_wins() {
        let rules = vec![
            rule(
                1,
                group(
                    Combinator::All,
                    vec![cond(Field::Path, Operator::StartsWith, &["/api"])],
                ),
                block(401),
            ),
            rule(
                2,
                group(
                    Combinator::All,
                    vec![cond(Field::Path, Operator::StartsWith, &["/"])],
                ),
                block(403),
            ),
        ];
        assert_eq!(
            decide_for(&rules, &sample("https://app.example.com/api/x")),
            Decision::Block {
                rule_id: 1,
                status: 401,
                body: String::new()
            }
        );
        assert_eq!(
            decide_for(&rules, &sample("https://app.example.com/other")),
            Decision::Block {
                rule_id: 2,
                status: 403,
                body: String::new()
            }
        );
    }

    #[test]
    fn normalized_path_catches_encoded_and_doubled_slashes() {
        let rules = vec![rule(
            1,
            group(
                Combinator::All,
                vec![cond(Field::Path, Operator::Glob, &["/admin*"])],
            ),
            block(403),
        )];
        for url in ["/admin", "//admin", "/%61dmin/x", "/x/../admin"] {
            assert!(
                matches!(decide_for(&rules, &sample(url)), Decision::Block { .. }),
                "{url} should be blocked"
            );
        }
        assert_eq!(decide_for(&rules, &sample("/public")), Decision::Forward);
    }

    #[test]
    fn ambiguous_paths_skip_allow_rules_even_inside_none() {
        // "Allow everything except /admin*" must not allow /%61dmin.
        let rules = vec![
            rule(
                1,
                group(
                    Combinator::None,
                    vec![cond(Field::Path, Operator::Glob, &["/admin*"])],
                ),
                Action::Allow,
            ),
            rule(2, group(Combinator::All, vec![]), block(403)),
        ];
        let program = Program::compile(&rules).unwrap_or_else(|e| panic!("{e}"));
        let (eval, trace) = program.evaluate_traced(&sample("/%61dmin"));
        assert_eq!(
            eval.decision,
            Decision::Block {
                rule_id: 2,
                status: 403,
                body: String::new()
            }
        );
        assert_eq!(trace[0].outcome, RuleOutcome::AmbiguousPath);
        assert_eq!(
            decide_for(&rules, &sample("/public")),
            Decision::Allow { rule_id: 1 }
        );
    }

    #[test]
    fn log_only_records_without_deciding() {
        let mut logged = rule(1, group(Combinator::All, vec![]), block(403));
        logged.mode = Mode::LogOnly;
        let program = Program::compile(&[logged]).unwrap_or_else(|e| panic!("{e}"));
        let eval = program.evaluate(&sample("/x"));
        assert_eq!(eval.decision, Decision::Forward);
        assert_eq!(
            eval.first_match,
            Some(MatchRef {
                rule_id: 1,
                mode: Mode::LogOnly
            })
        );
        assert_eq!(program.drain_hits(), vec![(1, 1)]);
        assert!(program.drain_hits().is_empty());
    }

    #[test]
    fn dry_runs_do_not_count_hits() {
        let program = Program::compile(&[rule(1, group(Combinator::All, vec![]), block(403))])
            .unwrap_or_else(|e| panic!("{e}"));
        let _ = program.evaluate_traced(&sample("/x"));
        assert!(program.drain_hits().is_empty());
    }

    #[test]
    fn missing_headers_never_match_value_tests_but_satisfy_none() {
        let key_check = keyed(Field::Header, "X-Api-Key", Operator::Eq, &["s3cret"]);
        let rules = vec![rule(
            1,
            group(Combinator::None, vec![key_check]),
            block(401),
        )];
        assert!(matches!(
            decide_for(&rules, &sample("/x")),
            Decision::Block { .. }
        ));
        let with_key = ParsedSample::new(SampleRequest {
            url: "/x".into(),
            headers: vec![SampleHeader {
                name: "x-api-key".into(),
                value: "s3cret".into(),
            }],
            ..Default::default()
        });
        assert_eq!(decide_for(&rules, &with_key), Decision::Forward);
    }

    #[test]
    fn repeated_headers_match_any_value() {
        let rules = vec![rule(
            1,
            group(
                Combinator::All,
                vec![keyed(Field::Header, "x-tier", Operator::Eq, &["gold"])],
            ),
            block(402),
        )];
        let req = ParsedSample::new(SampleRequest {
            url: "/".into(),
            headers: vec![
                SampleHeader {
                    name: "X-Tier".into(),
                    value: "silver".into(),
                },
                SampleHeader {
                    name: "x-tier".into(),
                    value: "gold".into(),
                },
            ],
            ..Default::default()
        });
        assert!(matches!(decide_for(&rules, &req), Decision::Block { .. }));
    }

    #[test]
    fn query_and_cookie_values_are_decoded_and_split() {
        let rules = vec![
            rule(
                1,
                group(
                    Combinator::All,
                    vec![keyed(Field::Query, "q", Operator::Eq, &["a b"])],
                ),
                block(400),
            ),
            rule(
                2,
                group(
                    Combinator::All,
                    vec![keyed(Field::Cookie, "session", Operator::Exists, &[])],
                ),
                block(401),
            ),
        ];
        assert!(matches!(
            decide_for(&rules, &sample("/s?x=1&q=a+b")),
            Decision::Block { rule_id: 1, .. }
        ));
        assert!(matches!(
            decide_for(&rules, &sample("/s?q=a%20b")),
            Decision::Block { rule_id: 1, .. }
        ));
        let with_cookie = ParsedSample::new(SampleRequest {
            url: "/".into(),
            headers: vec![SampleHeader {
                name: "Cookie".into(),
                value: "theme=dark; session=abc".into(),
            }],
            ..Default::default()
        });
        assert!(matches!(
            decide_for(&rules, &with_cookie),
            Decision::Block { rule_id: 2, .. }
        ));
    }

    #[test]
    fn case_insensitive_flag_and_forced_fields() {
        let mut ci = cond(Field::Path, Operator::StartsWith, &["/admin"]);
        if let Expression::Condition(c) = &mut ci {
            c.case_insensitive = true;
        }
        let rules = vec![
            rule(1, group(Combinator::All, vec![ci]), block(403)),
            rule(
                2,
                group(
                    Combinator::All,
                    vec![cond(Field::Method, Operator::In, &["post"])],
                ),
                block(405),
            ),
        ];
        assert!(matches!(
            decide_for(&rules, &sample("/ADMIN/x")),
            Decision::Block { rule_id: 1, .. }
        ));
        let post = ParsedSample::new(SampleRequest {
            method: "post".into(),
            url: "/x".into(),
            ..Default::default()
        });
        assert!(matches!(
            decide_for(&rules, &post),
            Decision::Block { rule_id: 2, .. }
        ));
        let sensitive = vec![rule(
            1,
            group(
                Combinator::All,
                vec![cond(Field::Path, Operator::StartsWith, &["/admin"])],
            ),
            block(403),
        )];
        assert_eq!(decide_for(&sensitive, &sample("/ADMIN")), Decision::Forward);
    }

    #[test]
    fn cidr_geo_and_hosting_conditions() {
        let rules = vec![rule(
            1,
            group(
                Combinator::All,
                vec![cond(Field::Ip, Operator::InCidr, &["198.51.100.0/24"])],
            ),
            block(403),
        )];
        assert!(matches!(
            decide_for(&rules, &sample("/")),
            Decision::Block { .. }
        ));
        let geo_rules = vec![rule(
            1,
            group(
                Combinator::All,
                vec![cond(Field::Country, Operator::In, &["es", "PT"])],
            ),
            block(451),
        )];
        let es = ParsedSample::new(SampleRequest {
            url: "/".into(),
            country: Some("es".into()),
            ..Default::default()
        });
        assert!(matches!(
            decide_for(&geo_rules, &es),
            Decision::Block { .. }
        ));
        // No GeoIP data: geo conditions never match.
        assert_eq!(decide_for(&geo_rules, &sample("/")), Decision::Forward);
        let hosting = vec![rule(
            1,
            group(
                Combinator::All,
                vec![cond(Field::IsHostingProvider, Operator::Eq, &["true"])],
            ),
            Action::Challenge,
        )];
        let cloud = ParsedSample::new(SampleRequest {
            url: "/".into(),
            is_hosting_provider: Some(true),
            ..Default::default()
        });
        assert_eq!(
            decide_for(&hosting, &cloud),
            Decision::Challenge { rule_id: 1 }
        );
        let cleared = ParsedSample::new(SampleRequest {
            url: "/".into(),
            is_hosting_provider: Some(true),
            challenge_cleared: true,
            ..Default::default()
        });
        assert_eq!(decide_for(&hosting, &cleared), Decision::Forward);
    }

    #[test]
    fn asn_accepts_as_prefix() {
        let rules = vec![rule(
            1,
            group(
                Combinator::All,
                vec![cond(Field::Asn, Operator::In, &["AS64496", "64497"])],
            ),
            block(403),
        )];
        let req = ParsedSample::new(SampleRequest {
            url: "/".into(),
            asn: Some(64496),
            ..Default::default()
        });
        assert!(matches!(decide_for(&rules, &req), Decision::Block { .. }));
    }

    #[test]
    fn redirects_substitute_path_and_query() {
        let rules = vec![rule(
            1,
            group(
                Combinator::All,
                vec![cond(Field::Path, Operator::StartsWith, &["/docs"])],
            ),
            Action::Redirect {
                target: "https://docs.example.com{path}?{query}".into(),
                status: 308,
            },
        )];
        assert_eq!(
            decide_for(&rules, &sample("https://app.example.com//docs/a?x=1")),
            Decision::Redirect {
                rule_id: 1,
                status: 308,
                location: "https://docs.example.com/docs/a?x=1".into()
            }
        );
    }

    #[test]
    fn environment_scope_and_disabled_rules_are_skipped() {
        let mut scoped = rule(1, group(Combinator::All, vec![]), block(403));
        scoped.environment_id = Some(7);
        let mut off = rule(2, group(Combinator::All, vec![]), block(404));
        off.enabled = false;
        let program = Program::compile(&[scoped, off]).unwrap_or_else(|e| panic!("{e}"));
        let other_env = ParsedSample::new(SampleRequest {
            url: "/".into(),
            environment_id: Some(8),
            ..Default::default()
        });
        let (eval, trace) = program.evaluate_traced(&other_env);
        assert_eq!(eval.decision, Decision::Forward);
        assert_eq!(trace[0].outcome, RuleOutcome::OtherEnvironment);
        assert_eq!(trace[1].outcome, RuleOutcome::Disabled);
        let same_env = ParsedSample::new(SampleRequest {
            url: "/".into(),
            environment_id: Some(7),
            ..Default::default()
        });
        assert!(matches!(
            program.evaluate(&same_env).decision,
            Decision::Block { rule_id: 1, .. }
        ));
    }

    #[test]
    fn later_rules_are_not_reached_after_a_decision() {
        let rules = vec![
            rule(1, group(Combinator::All, vec![]), block(403)),
            rule(2, group(Combinator::All, vec![]), block(404)),
        ];
        let program = Program::compile(&rules).unwrap_or_else(|e| panic!("{e}"));
        let (_, trace) = program.evaluate_traced(&sample("/"));
        assert_eq!(trace[1].outcome, RuleOutcome::NotReached);
        let _ = program.evaluate(&sample("/"));
        assert_eq!(program.drain_hits(), vec![(1, 1)]);
    }

    #[test]
    fn sample_url_parsing_matches_the_console() {
        let s = sample("HTTPS://User@App.Example.com:8443//a/b?x=1#frag");
        assert_eq!(s.host(), "app.example.com");
        assert_eq!(s.raw_path(), "//a/b");
        assert_eq!(s.raw_query(), "x=1");
        let origin = sample("/only/path?y");
        assert_eq!(origin.host(), "");
        assert_eq!(origin.raw_path(), "/only/path");
        assert_eq!(origin.raw_query(), "y");
        // `//admin` is a path, not a host.
        assert_eq!(sample("//admin").raw_path(), "//admin");
        assert_eq!(
            sample("https://[2001:db8::1]:443/x").host(),
            "[2001:db8::1]"
        );
    }
}
