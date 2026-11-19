// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Runs the shared conformance fixtures in `tests/conformance/*.json`. The
//! console's in-browser tester runs the same files
//! (`web/src/components/project/settings/edge-rules/conformance.test.ts`), so
//! the proxy and the tester cannot disagree about a request.

use std::fs;
use std::path::PathBuf;

use serde::Deserialize;
use temps_edge_rules::{
    auto_name, prepare_expression, validate_rule, Action, Decision, Group, Mode, ParsedSample,
    Program, RuleDefinition, RuleOutcome, SampleRequest,
};

#[derive(Deserialize)]
struct Fixture {
    description: String,
    rules: Vec<FixtureRule>,
    cases: Vec<FixtureCase>,
}

#[derive(Deserialize)]
struct FixtureRule {
    id: i32,
    #[serde(default = "yes")]
    enabled: bool,
    #[serde(default = "enforce")]
    mode: Mode,
    #[serde(default)]
    environment_id: Option<i32>,
    expression: Group,
    action: Action,
}

fn yes() -> bool {
    true
}

fn enforce() -> Mode {
    Mode::Enforce
}

#[derive(Deserialize)]
struct FixtureCase {
    name: String,
    request: SampleRequest,
    decision: ExpectedDecision,
    #[serde(default)]
    trace: Option<Vec<RuleOutcome>>,
}

#[derive(Deserialize, Debug)]
struct ExpectedDecision {
    kind: String,
    #[serde(default)]
    rule_id: Option<i32>,
    #[serde(default)]
    status: Option<u16>,
    #[serde(default)]
    location: Option<String>,
}

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/conformance")
}

fn summarize(d: &Decision) -> (String, Option<i32>, Option<u16>, Option<String>) {
    match d {
        Decision::Forward => ("forward".into(), None, None, None),
        Decision::Block {
            rule_id, status, ..
        } => ("block".into(), Some(*rule_id), Some(*status), None),
        Decision::Allow { rule_id } => ("allow".into(), Some(*rule_id), None, None),
        Decision::Challenge { rule_id } => ("challenge".into(), Some(*rule_id), None, None),
        Decision::Redirect {
            rule_id,
            status,
            location,
        } => (
            "redirect".into(),
            Some(*rule_id),
            Some(*status),
            Some(location.clone()),
        ),
    }
}

#[test]
fn conformance_fixtures() {
    let mut files: Vec<PathBuf> = fs::read_dir(fixtures_dir())
        .expect("conformance directory exists")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    assert!(
        files.len() >= 4,
        "expected the conformance fixtures, found {files:?}"
    );

    let mut failures = Vec::new();
    let mut cases = 0;
    for file in files {
        let text = fs::read_to_string(&file).expect("fixture is readable");
        let fixture: Fixture = serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("{} is not a valid fixture: {e}", file.display()));
        let mut rules = Vec::new();
        for r in fixture.rules {
            let mut expression = r.expression;
            prepare_expression(&mut expression);
            let name = auto_name(&expression, &r.action);
            let issues = validate_rule(&name, &expression, &r.action);
            assert!(
                issues.is_empty(),
                "{}: rule {} is invalid: {issues:?}",
                file.display(),
                r.id
            );
            rules.push(RuleDefinition {
                id: r.id,
                revision: 1,
                name,
                enabled: r.enabled,
                mode: r.mode,
                environment_id: r.environment_id,
                expression,
                action: r.action,
            });
        }
        let program = Program::compile(&rules).expect("fixture rules compile");
        for case in fixture.cases {
            cases += 1;
            let request = ParsedSample::new(case.request);
            let (evaluation, trace) = program.evaluate_traced(&request);
            let got = summarize(&evaluation.decision);
            let want = (
                case.decision.kind.clone(),
                case.decision.rule_id,
                case.decision.status,
                case.decision.location.clone(),
            );
            if got != want {
                failures.push(format!(
                    "{} / {}: expected {want:?}, got {got:?}",
                    fixture.description, case.name
                ));
            }
            if let Some(expected) = case.trace {
                let outcomes: Vec<RuleOutcome> = trace.iter().map(|t| t.outcome).collect();
                if outcomes != expected {
                    failures.push(format!(
                        "{} / {}: expected trace {expected:?}, got {outcomes:?}",
                        fixture.description, case.name
                    ));
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {cases} cases failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
