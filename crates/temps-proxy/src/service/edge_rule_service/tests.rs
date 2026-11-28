// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use temps_test_utils::{is_container_runtime_unavailable, TestDatabase};

pub(crate) async fn database() -> Option<TestDatabase> {
    match TestDatabase::with_migrations().await {
        Ok(db) => Some(db),
        Err(error) if is_container_runtime_unavailable(&error.to_string()) => {
            eprintln!("Docker not available, skipping edge rule database test");
            None
        }
        Err(error) => panic!("test database: {error}"),
    }
}

/// A project with one environment; returns `(project_id, environment_id)`.
pub(crate) async fn project(db: &TestDatabase, slug: &str) -> (i32, i32) {
    let rows = db
        .query_sql(&format!(
            "WITH p AS (INSERT INTO projects (name, repo_name, repo_owner, directory, \
                 main_branch, preset, slug, created_at, updated_at) VALUES ('{slug}', '{slug}', \
                 'owner', '.', 'main', 'dockerfile', '{slug}', NOW(), NOW()) RETURNING id) \
                 INSERT INTO environments (name, slug, subdomain, host, upstreams, project_id, \
                 created_at, updated_at) SELECT 'production', 'production', '{slug}', \
                 '{slug}.example.test', '[]'::json, p.id, NOW(), NOW() FROM p \
                 RETURNING project_id, id"
        ))
        .await
        .expect("insert project and environment");
    let row = rows.first().expect("one environment row");
    (
        row.try_get("", "project_id").expect("project_id"),
        row.try_get("", "id").expect("environment id"),
    )
}

pub(crate) fn block_admin() -> CreateEdgeRuleRequest {
    serde_json::from_value(serde_json::json!({
        "mode": "enforce",
        "expression": {"combinator": "all", "children": [
            {"kind": "condition", "field": "path", "op": "starts_with",
             "values": ["/admin"], "case_insensitive": true}
        ]},
        "action": {"type": "block", "status": 403}
    }))
    .expect("valid request")
}

fn rule_on(path: &str, action: serde_json::Value) -> CreateEdgeRuleRequest {
    serde_json::from_value(serde_json::json!({
        "mode": "enforce",
        "expression": {"combinator": "all", "children": [
            {"kind": "condition", "field": "path", "op": "starts_with", "values": [path]}
        ]},
        "action": action
    }))
    .expect("valid request")
}

fn sample(url: &str) -> SampleRequest {
    SampleRequest {
        url: url.into(),
        ..Default::default()
    }
}

#[test]
fn invalid_rules_are_refused_with_every_issue() {
    let request: CreateEdgeRuleRequest = serde_json::from_value(serde_json::json!({
        "expression": {"combinator": "all", "children": [
            {"kind": "condition", "field": "ip", "op": "in_cidr", "values": ["not-an-ip"]},
            {"kind": "condition", "field": "path", "op": "regex", "values": ["("]}
        ]},
        "action": {"type": "block", "status": 200}
    }))
    .expect("parses");
    match EdgeRuleService::prepare(
        7,
        None,
        request.expression,
        request.action,
        EmptyExpression::Refuse,
    ) {
        Err(EdgeRuleServiceError::Invalid {
            project_id: 7,
            issues,
            ..
        }) => {
            assert!(issues.len() >= 3, "{issues:?}");
            assert!(issues.iter().any(|i| i.target == "action"));
        }
        Err(other) => panic!("expected Invalid, got {other}"),
        Ok(_) => panic!("expected Invalid, got a prepared rule"),
    }
}

#[test]
fn an_empty_name_is_generated_from_the_rule() {
    let request = block_admin();
    let prepared = EdgeRuleService::prepare(
        1,
        Some("  "),
        request.expression,
        request.action,
        EmptyExpression::Refuse,
    )
    .unwrap_or_else(|e| panic!("valid rule: {e}"));
    assert_eq!(
        prepared.name,
        "Block when path starts with /admin (ignoring case)"
    );
    // Node ids are assigned so issues and the editor can address them.
    assert!(!prepared.expression.id.is_empty());
}

#[test]
fn update_distinguishes_a_null_environment_from_an_absent_one() {
    let absent: UpdateEdgeRuleRequest =
        serde_json::from_str(r#"{"enabled": false}"#).expect("parses");
    assert_eq!(absent.environment_id, None);
    let null: UpdateEdgeRuleRequest =
        serde_json::from_str(r#"{"environment_id": null}"#).expect("parses");
    assert_eq!(null.environment_id, Some(None));
    let set: UpdateEdgeRuleRequest =
        serde_json::from_str(r#"{"environment_id": 4}"#).expect("parses");
    assert_eq!(set.environment_id, Some(Some(4)));
}

/// The `project_route_change` notifications naming `project_id`'s
/// edge rules that arrive within a short window.
async fn route_announcements(listener: &mut sqlx::postgres::PgListener, project_id: i32) -> usize {
    let mut count = 0;
    while let Ok(received) =
        tokio::time::timeout(std::time::Duration::from_millis(400), listener.recv()).await
    {
        let notification = received.unwrap_or_else(|e| panic!("receive notification: {e}"));
        let payload: serde_json::Value = serde_json::from_str(notification.payload())
            .unwrap_or_else(|e| panic!("route change payload: {e}"));
        if payload["field"] == "edge_rules.enabled" && payload["project_id"] == project_id {
            count += 1;
        }
    }
    count
}

#[tokio::test]
async fn writes_that_change_enabled_rules_wake_the_worker_route_sync() {
    let Some(test_db) = database().await else {
        return;
    };
    let service = EdgeRuleService::new(test_db.connection_arc());
    let (project_id, _) = project(&test_db, "edge-notify").await;
    let mut listener = sqlx::postgres::PgListener::connect(&test_db.database_url)
        .await
        .unwrap_or_else(|e| panic!("connect listener: {e}"));
    listener
        .listen("project_route_change")
        .await
        .unwrap_or_else(|e| panic!("listen: {e}"));

    // A disabled rule does not change what workers may serve.
    let mut disabled = block_admin();
    disabled.enabled = false;
    let disabled = service
        .create(project_id, disabled, None)
        .await
        .unwrap_or_else(|e| panic!("create disabled: {e}"));
    assert_eq!(route_announcements(&mut listener, project_id).await, 0);
    let update = |patch: serde_json::Value| -> UpdateEdgeRuleRequest {
        serde_json::from_value(patch).unwrap_or_else(|e| panic!("update request: {e}"))
    };
    service
        .update(
            project_id,
            disabled.id,
            update(serde_json::json!({"name": "Renamed"})),
            None,
        )
        .await
        .unwrap_or_else(|e| panic!("rename: {e}"));
    assert_eq!(route_announcements(&mut listener, project_id).await, 0);

    // Enabling, creating enabled, and deleting an enabled rule all do.
    service
        .update(
            project_id,
            disabled.id,
            update(serde_json::json!({"enabled": true})),
            None,
        )
        .await
        .unwrap_or_else(|e| panic!("enable: {e}"));
    assert_eq!(route_announcements(&mut listener, project_id).await, 1);
    let enabled = service
        .create(
            project_id,
            rule_on("/api", serde_json::json!({"type": "log"})),
            None,
        )
        .await
        .unwrap_or_else(|e| panic!("create enabled: {e}"));
    assert_eq!(route_announcements(&mut listener, project_id).await, 1);
    service
        .delete(project_id, enabled.id, None)
        .await
        .unwrap_or_else(|e| panic!("delete enabled: {e}"));
    assert_eq!(route_announcements(&mut listener, project_id).await, 1);
    service
        .update(
            project_id,
            disabled.id,
            update(serde_json::json!({"enabled": false})),
            None,
        )
        .await
        .unwrap_or_else(|e| panic!("disable: {e}"));
    assert_eq!(route_announcements(&mut listener, project_id).await, 1);

    // A refused write announces nothing: the NOTIFY rolls back with it.
    let refused = service
        .update(
            project_id,
            disabled.id,
            update(serde_json::json!({"enabled": true, "revision": 1})),
            None,
        )
        .await;
    assert!(matches!(
        refused,
        Err(EdgeRuleServiceError::RevisionConflict { .. })
    ));
    service
        .delete(project_id, disabled.id, None)
        .await
        .unwrap_or_else(|e| panic!("delete disabled: {e}"));
    assert_eq!(route_announcements(&mut listener, project_id).await, 0);
}

#[tokio::test]
async fn rules_are_created_ordered_updated_and_deleted() {
    let Some(test_db) = database().await else {
        return;
    };
    let service = EdgeRuleService::new(test_db.connection_arc());
    let (project_id, environment_id) = project(&test_db, "edge-crud").await;

    let first = service
        .create(project_id, block_admin(), None)
        .await
        .unwrap_or_else(|e| panic!("create: {e}"));
    assert_eq!(first.position, 0);
    assert_eq!(first.revision, 1);
    assert_eq!(
        first.summary,
        "Block with 403 when path starts with /admin (ignoring case)"
    );

    let mut second = rule_on("/api", serde_json::json!({"type": "log"}));
    second.position = Some(0);
    second.environment_id = Some(environment_id);
    let second = service
        .create(project_id, second, None)
        .await
        .unwrap_or_else(|e| panic!("create at the top: {e}"));
    let listed = service
        .list(project_id)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    let order: Vec<i32> = listed.rules.iter().map(|r| r.id).collect();
    assert_eq!(order, vec![second.id, first.id]);
    assert_eq!(listed.max_rules, 100);

    // A stale revision is refused; the current one applies.
    let stale = service
        .update(
            project_id,
            first.id,
            UpdateEdgeRuleRequest {
                revision: Some(0),
                enabled: Some(false),
                ..Default::default()
            },
            None,
        )
        .await;
    assert!(matches!(
        stale,
        Err(EdgeRuleServiceError::RevisionConflict {
            expected: 0,
            current: 1,
            ..
        })
    ));
    let (before, after) = service
        .update(
            project_id,
            first.id,
            UpdateEdgeRuleRequest {
                revision: Some(1),
                mode: Some(Mode::LogOnly),
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap_or_else(|e| panic!("update: {e}"));
    assert_eq!(before.mode, Mode::Enforce);
    assert_eq!(after.mode, Mode::LogOnly);
    assert_eq!(after.revision, 2);
    assert_eq!(after.name, before.name);

    let reordered = service
        .reorder(
            project_id,
            ReorderEdgeRulesRequest {
                rule_ids: vec![first.id, second.id],
            },
        )
        .await
        .unwrap_or_else(|e| panic!("reorder: {e}"));
    let order: Vec<(i32, i32)> = reordered.rules.iter().map(|r| (r.id, r.position)).collect();
    assert_eq!(order, vec![(first.id, 0), (second.id, 1)]);

    let incomplete = service
        .reorder(
            project_id,
            ReorderEdgeRulesRequest {
                rule_ids: vec![first.id],
            },
        )
        .await;
    assert!(matches!(
        incomplete,
        Err(EdgeRuleServiceError::InvalidOrder { .. })
    ));

    // A deletion bound to a revision refuses a rule that changed since.
    let current = service
        .get(project_id, first.id)
        .await
        .unwrap_or_else(|e| panic!("get: {e}"));
    let stale = service
        .delete(project_id, first.id, Some(current.revision - 1))
        .await;
    assert!(matches!(
        stale,
        Err(EdgeRuleServiceError::RevisionConflict { .. })
    ));
    service
        .delete(project_id, first.id, Some(current.revision))
        .await
        .unwrap_or_else(|e| panic!("delete: {e}"));
    let listed = service
        .list(project_id)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(listed.rules.len(), 1);
    assert_eq!(listed.rules[0].position, 0, "the gap is closed");

    let missing = service.delete(project_id, first.id, None).await;
    assert!(matches!(
        missing,
        Err(EdgeRuleServiceError::RuleNotFound { rule_id, .. }) if rule_id == first.id
    ));
}

#[tokio::test]
async fn writes_are_scoped_to_the_project_and_its_environments() {
    let Some(test_db) = database().await else {
        return;
    };
    let service = EdgeRuleService::new(test_db.connection_arc());
    let (project_a, _) = project(&test_db, "edge-scope-a").await;
    let (project_b, environment_b) = project(&test_db, "edge-scope-b").await;

    let mut foreign_env = block_admin();
    foreign_env.environment_id = Some(environment_b);
    assert!(matches!(
        service.create(project_a, foreign_env, None).await,
        Err(EdgeRuleServiceError::EnvironmentNotFound { .. })
    ));

    let rule = service
        .create(project_b, block_admin(), None)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(matches!(
        service.get(project_a, rule.id).await,
        Err(EdgeRuleServiceError::RuleNotFound { .. })
    ));
    assert!(matches!(
        service.list(999_999).await,
        Err(EdgeRuleServiceError::ProjectNotFound {
            project_id: 999_999
        })
    ));
}

/// #2111: a rule whose environment was deleted can still be edited; only an
/// environment the update moves it to is checked.
#[tokio::test]
async fn a_rule_of_a_deleted_environment_can_still_be_edited() {
    let Some(test_db) = database().await else {
        return;
    };
    let service = EdgeRuleService::new(test_db.connection_arc());
    let (project_id, environment_id) = project(&test_db, "edge-deleted-env").await;
    let (_, foreign_environment) = project(&test_db, "edge-deleted-env-other").await;
    let mut scoped = block_admin();
    scoped.environment_id = Some(environment_id);
    let rule = service
        .create(project_id, scoped, None)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    test_db
        .query_sql(&format!(
            "UPDATE environments SET deleted_at = NOW() WHERE id = {environment_id} RETURNING id"
        ))
        .await
        .expect("soft-delete the environment");

    let update = |body: serde_json::Value| -> UpdateEdgeRuleRequest {
        serde_json::from_value(body).expect("valid update")
    };
    let (_, renamed) = service
        .update(
            project_id,
            rule.id,
            update(serde_json::json!({"name": "x"})),
            None,
        )
        .await
        .unwrap_or_else(|e| panic!("a rename keeps the binding: {e}"));
    assert_eq!(renamed.name, "x");
    assert_eq!(renamed.environment_id, Some(environment_id));
    // Sending the stored environment back unchanged is not a move.
    let (_, disabled) = service
        .update(
            project_id,
            rule.id,
            update(serde_json::json!({"enabled": false, "environment_id": environment_id})),
            None,
        )
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(!disabled.enabled);
    // Moving it still checks the destination.
    assert!(matches!(
        service
            .update(
                project_id,
                rule.id,
                update(serde_json::json!({"environment_id": foreign_environment})),
                None,
            )
            .await,
        Err(EdgeRuleServiceError::EnvironmentNotFound { .. })
    ));
    let (_, unscoped) = service
        .update(
            project_id,
            rule.id,
            update(serde_json::json!({"environment_id": null})),
            None,
        )
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(unscoped.environment_id, None);
}

fn no_conditions(combinator: &str) -> serde_json::Value {
    serde_json::json!({"combinator": combinator, "children": []})
}

fn is_no_conditions(error: &EdgeRuleServiceError) -> bool {
    matches!(
        error,
        EdgeRuleServiceError::Invalid { issues, .. }
            if issues.iter().any(|i| i.message == temps_edge_rules::NO_CONDITIONS)
    )
}

#[tokio::test]
async fn rules_without_conditions_are_refused_on_every_write_path() {
    let Some(test_db) = database().await else {
        return;
    };
    let service = EdgeRuleService::new(test_db.connection_arc());
    let (project_id, _) = project(&test_db, "edge-no-conditions").await;

    for combinator in ["all", "any", "none"] {
        let request: CreateEdgeRuleRequest = serde_json::from_value(serde_json::json!({
            "mode": "enforce",
            "expression": no_conditions(combinator),
            "action": {"type": "block", "status": 403}
        }))
        .expect("parses");
        let error = service
            .create(project_id, request, None)
            .await
            .expect_err("a rule without conditions must be refused");
        assert!(is_no_conditions(&error), "{combinator}: {error}");
        let problem = temps_core::problemdetails::Problem::from(error);
        assert_eq!(problem.status_code, axum::http::StatusCode::BAD_REQUEST);
    }
    let listed = service
        .list(project_id)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(listed.rules.is_empty(), "nothing is stored");

    // Validation and the dry run refuse the same draft.
    let expression: Group = serde_json::from_value(no_conditions("all")).expect("parses");
    let error = service
        .validate(project_id, None, expression.clone(), Action::Log)
        .expect_err("validation refuses it");
    assert!(is_no_conditions(&error), "{error}");
    let error = service
        .evaluate(
            project_id,
            EvaluateEdgeRulesRequest {
                request: sample("https://example.test/"),
                rules: Some(vec![EvaluateRuleInput {
                    id: None,
                    name: None,
                    enabled: true,
                    mode: Mode::Enforce,
                    environment_id: None,
                    expression: expression.clone(),
                    action: Action::Log,
                }]),
            },
        )
        .await
        .expect_err("the dry run refuses it");
    assert!(is_no_conditions(&error), "{error}");

    // Emptying the expression of a saved rule is refused, enabled or not.
    let saved = service
        .create(project_id, block_admin(), None)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    for enabled in [Some(true), Some(false)] {
        let error = service
            .update(
                project_id,
                saved.id,
                UpdateEdgeRuleRequest {
                    enabled,
                    expression: Some(expression.clone()),
                    ..Default::default()
                },
                None,
            )
            .await
            .expect_err("emptying the expression must be refused");
        assert!(is_no_conditions(&error), "{error}");
    }
    let kept = service
        .get(project_id, saved.id)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(kept.expression, saved.expression);
    assert_eq!(kept.revision, saved.revision);

    // Matching every request on purpose is one explicit condition.
    let every_request = rule_on("/", serde_json::json!({"type": "block", "status": 403}));
    let created = service
        .create(project_id, every_request, None)
        .await
        .unwrap_or_else(|e| panic!("an explicit condition is accepted: {e}"));
    assert_eq!(created.summary, "Block with 403 when path starts with /");
}

#[tokio::test]
async fn a_stored_rule_without_conditions_can_still_be_turned_off() {
    let Some(test_db) = database().await else {
        return;
    };
    let service = EdgeRuleService::new(test_db.connection_arc());
    let (project_id, _) = project(&test_db, "edge-legacy-empty").await;
    // A rule saved before such rules were refused.
    let rows = test_db
        .query_sql(&format!(
            "INSERT INTO project_edge_rules (project_id, position, name, enabled, mode, \
                 expression, action, revision, created_at, updated_at) VALUES ({project_id}, 0, \
                 'Block every request', true, 'enforce', \
                 '{{\"id\":\"root\",\"combinator\":\"all\",\"children\":[]}}'::jsonb, \
                 '{{\"type\":\"block\",\"status\":403}}'::jsonb, 1, NOW(), NOW()) RETURNING id"
        ))
        .await
        .expect("insert a stored rule");
    let rule_id: i32 = rows
        .first()
        .expect("one row")
        .try_get("", "id")
        .expect("id");

    // Renaming it while it stays enabled is refused...
    let error = service
        .update(
            project_id,
            rule_id,
            UpdateEdgeRuleRequest {
                name: Some("Renamed".into()),
                ..Default::default()
            },
            None,
        )
        .await
        .expect_err("an enabled rule without conditions cannot be saved again");
    assert!(is_no_conditions(&error), "{error}");

    // ...turning it off is not, and a disabled one can be renamed.
    let (_, off) = service
        .update(
            project_id,
            rule_id,
            UpdateEdgeRuleRequest {
                enabled: Some(false),
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap_or_else(|e| panic!("turning the rule off must work: {e}"));
    assert!(!off.enabled);
    let (_, renamed) = service
        .update(
            project_id,
            rule_id,
            UpdateEdgeRuleRequest {
                name: Some("Old catch-all".into()),
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap_or_else(|e| panic!("a disabled rule can be renamed: {e}"));
    assert_eq!(renamed.name, "Old catch-all");
    assert!(!renamed.enabled);

    // An editor that saves the whole rule sends its stored expression back:
    // that is not a new expression, so the disabled rule can still be renamed.
    let (_, resent) = service
        .update(
            project_id,
            rule_id,
            UpdateEdgeRuleRequest {
                name: Some("Old catch-all, kept".into()),
                expression: Some(renamed.expression.clone()),
                action: Some(renamed.action.clone()),
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap_or_else(|e| panic!("resending the stored expression is no change: {e}"));
    assert_eq!(resent.name, "Old catch-all, kept");
    assert_eq!(resent.expression, renamed.expression);

    // A different expression without conditions is a new one, and refused.
    let mut other = renamed.expression.clone();
    other.combinator = match other.combinator {
        temps_edge_rules::Combinator::All => temps_edge_rules::Combinator::Any,
        _ => temps_edge_rules::Combinator::All,
    };
    let error = service
        .update(
            project_id,
            rule_id,
            UpdateEdgeRuleRequest {
                expression: Some(other),
                ..Default::default()
            },
            None,
        )
        .await
        .expect_err("a new expression without conditions must be refused");
    assert!(is_no_conditions(&error), "{error}");

    // Turning it back on needs a condition.
    let error = service
        .update(
            project_id,
            rule_id,
            UpdateEdgeRuleRequest {
                enabled: Some(true),
                ..Default::default()
            },
            None,
        )
        .await
        .expect_err("re-enabling a rule without conditions must be refused");
    assert!(is_no_conditions(&error), "{error}");
    let stored = service
        .get(project_id, rule_id)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(!stored.enabled);

    service
        .delete(project_id, rule_id, None)
        .await
        .unwrap_or_else(|e| panic!("it can be deleted: {e}"));
}

#[tokio::test]
async fn positions_outside_the_rules_are_refused() {
    let Some(test_db) = database().await else {
        return;
    };
    let service = EdgeRuleService::new(test_db.connection_arc());
    let (project_id, _) = project(&test_db, "edge-positions").await;
    let first = service
        .create(project_id, block_admin(), None)
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    for position in [-1, 2, i32::MIN, i32::MAX] {
        let mut request = rule_on("/api", serde_json::json!({"type": "log"}));
        request.position = Some(position);
        let error = service
            .create(project_id, request, None)
            .await
            .expect_err("a position outside the rules must be refused");
        match &error {
            EdgeRuleServiceError::InvalidPosition {
                project_id: p,
                position: got,
                max,
            } => {
                assert_eq!((*p, *got, *max), (project_id, position, 1));
            }
            other => panic!("expected InvalidPosition, got {other}"),
        }
        let problem = temps_core::problemdetails::Problem::from(error);
        assert_eq!(problem.status_code, axum::http::StatusCode::BAD_REQUEST);
    }
    let listed = service
        .list(project_id)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(listed.rules.len(), 1, "nothing is stored");

    // The ends of the range are accepted, where they say.
    let mut last = rule_on("/last", serde_json::json!({"type": "log"}));
    last.position = Some(1);
    let last = service
        .create(project_id, last, None)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(last.position, 1);
    let mut top = rule_on("/top", serde_json::json!({"type": "log"}));
    top.position = Some(0);
    let top = service
        .create(project_id, top, None)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(top.position, 0);
    let order: Vec<i32> = service
        .list(project_id)
        .await
        .unwrap_or_else(|e| panic!("{e}"))
        .rules
        .iter()
        .map(|r| r.id)
        .collect();
    assert_eq!(order, vec![top.id, first.id, last.id]);
}

#[test]
fn an_invalid_rule_error_does_not_echo_a_long_name() {
    let request = block_admin();
    let name = "n".repeat(200_000);
    let error = EdgeRuleService::prepare(
        1,
        Some(&name),
        request.expression,
        request.action,
        EmptyExpression::Refuse,
    )
    .err()
    .expect("an over-long name is refused");
    let message = error.to_string();
    assert!(message.len() < 1_024, "{} bytes", message.len());
    assert!(
        message.contains("Names are limited to 80 characters."),
        "{message}"
    );
    assert!(
        message.contains(&format!("'{}…'", "n".repeat(80))),
        "{message}"
    );
    let problem = temps_core::problemdetails::Problem::from(error);
    let body = serde_json::to_string(&problem.body).expect("serializes");
    assert!(body.len() < 2_048, "{} bytes", body.len());
}

#[test]
fn a_short_rule_name_is_echoed_whole() {
    assert_eq!(echoed_name("Block admin"), "Block admin");
    assert_eq!(echoed_name(&"x".repeat(80)), "x".repeat(80));
}

#[tokio::test]
async fn the_rule_limit_is_enforced() {
    let Some(test_db) = database().await else {
        return;
    };
    let service = EdgeRuleService::new(test_db.connection_arc());
    let (project_id, _) = project(&test_db, "edge-limit").await;
    for _ in 0..limits::RULES_PER_PROJECT {
        service
            .create(project_id, block_admin(), None)
            .await
            .unwrap_or_else(|e| panic!("{e}"));
    }
    assert!(matches!(
        service.create(project_id, block_admin(), None).await,
        Err(EdgeRuleServiceError::TooManyRules { max: 100, .. })
    ));
}

/// Regex conditions cost memory in every proxy, so a project has a
/// budget across all of its rules, counted through nested groups.
#[tokio::test]
async fn regex_conditions_are_capped_per_project() {
    let Some(test_db) = database().await else {
        return;
    };
    let service = EdgeRuleService::new(test_db.connection_arc());
    let (project_id, _) = project(&test_db, "edge-regex").await;
    let regexes = |n: usize| -> CreateEdgeRuleRequest {
        let children: Vec<serde_json::Value> = (0..n)
                .map(|i| serde_json::json!({"kind": "condition", "field": "path", "op": "regex", "values": [format!("^/r{i}/")]}))
                .collect();
        serde_json::from_value(serde_json::json!({
            "expression": {"combinator": "any", "children": [
                {"kind": "group", "combinator": "any", "children": children}
            ]},
            "action": {"type": "log"}
        }))
        .expect("valid request")
    };
    let per_rule = limits::REGEX_CONDITIONS_PER_PROJECT / 2;
    let first = service
        .create(project_id, regexes(per_rule), None)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(first.mode, Mode::LogOnly, "new rules start log-only");
    service
        .create(project_id, regexes(per_rule), None)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    match service.create(project_id, regexes(1), None).await {
        Err(EdgeRuleServiceError::Invalid { issues, .. }) => {
            assert!(issues[0].message.contains("regex conditions"), "{issues:?}");
        }
        other => panic!("expected the regex budget to refuse it, got {other:?}"),
    }
    // Rules without a regex are unaffected, and so is a change that
    // keeps a rule's own regex count.
    service
        .create(project_id, block_admin(), None)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    let same = regexes(per_rule);
    service
        .update(
            project_id,
            first.id,
            UpdateEdgeRuleRequest {
                expression: Some(same.expression),
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap_or_else(|e| panic!("an update replaces the rule's own budget: {e}"));
}

fn conditions(field_op: (&str, &str), n: usize, offset: usize) -> serde_json::Value {
    let (field, op) = field_op;
    let children: Vec<serde_json::Value> = (0..n)
            .map(|i| serde_json::json!({"kind": "condition", "field": field, "op": op, "values": [format!("/p{}/*", i + offset)]}))
            .collect();
    serde_json::json!({"combinator": "any", "children": children})
}

#[tokio::test]
async fn globs_and_case_insensitive_matches_share_a_pattern_budget() {
    let Some(test_db) = database().await else {
        return;
    };
    let service = EdgeRuleService::new(test_db.connection_arc());
    let (project_id, _) = project(&test_db, "edge-patterns").await;
    let per_rule = limits::CONDITIONS_PER_RULE;
    let full_rules = limits::PATTERN_CONDITIONS_PER_PROJECT / per_rule;
    let rule = |n: usize, offset: usize| -> CreateEdgeRuleRequest {
        serde_json::from_value(serde_json::json!({
            "expression": conditions(("path", "glob"), n, offset),
            "action": {"type": "log"}
        }))
        .expect("valid request")
    };
    for r in 0..full_rules {
        service
            .create(project_id, rule(per_rule, r * per_rule), None)
            .await
            .unwrap_or_else(|e| panic!("rule {r} is within the budget: {e}"));
    }
    match service.create(project_id, rule(1, 10_000), None).await {
        Err(EdgeRuleServiceError::Invalid { issues, .. }) => {
            assert!(
                issues[0].message.contains("pattern conditions"),
                "{issues:?}"
            );
        }
        other => panic!("expected the pattern budget to refuse it, got {other:?}"),
    }
    // `in` compares strings without an automaton, so it is not budgeted.
    let exact: CreateEdgeRuleRequest = serde_json::from_value(serde_json::json!({
        "expression": {"combinator": "all", "children": [
            {"kind": "condition", "field": "path", "op": "in", "values": ["/a", "/b"]}
        ]},
        "action": {"type": "log"}
    }))
    .expect("valid request");
    service
        .create(project_id, exact, None)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
}

#[tokio::test]
async fn the_dry_run_refuses_drafts_over_the_pattern_budget() {
    let Some(test_db) = database().await else {
        return;
    };
    let service = EdgeRuleService::new(test_db.connection_arc());
    let (project_id, _) = project(&test_db, "edge-dry-run-budget").await;
    let half = limits::REGEX_CONDITIONS_PER_PROJECT / 2 + 1;
    let drafts: Vec<EvaluateRuleInput> = serde_json::from_value(serde_json::json!([
        {"expression": conditions(("path", "regex"), half, 0), "action": {"type": "log"}},
        {"expression": conditions(("path", "regex"), half, half), "action": {"type": "log"}}
    ]))
    .expect("drafts parse");
    match service
        .evaluate(
            project_id,
            EvaluateEdgeRulesRequest {
                request: sample("https://app.example.test/"),
                rules: Some(drafts),
            },
        )
        .await
    {
        Err(EdgeRuleServiceError::Invalid { issues, .. }) => {
            assert!(issues[0].message.contains("regex conditions"), "{issues:?}");
        }
        other => panic!("expected the dry run to refuse the drafts, got {other:?}"),
    }
}

#[tokio::test]
async fn the_dry_run_evaluates_saved_rules_and_drafts() {
    let Some(test_db) = database().await else {
        return;
    };
    let service = EdgeRuleService::new(test_db.connection_arc());
    let (project_id, _) = project(&test_db, "edge-dry-run").await;
    let saved = service
        .create(project_id, block_admin(), None)
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    // Paths ignore case by default for this rule.
    let result = service
        .evaluate(
            project_id,
            EvaluateEdgeRulesRequest {
                request: sample("https://app.example.test/ADMIN/users"),
                rules: None,
            },
        )
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        result.decision,
        Decision::Block {
            rule_id: saved.id,
            status: 403,
            body: String::new()
        }
    );
    assert_eq!(result.matched_rule_id, Some(saved.id));
    assert!(
        result.summary.starts_with("Blocked with 403"),
        "{}",
        result.summary
    );

    let drafts: Vec<EvaluateRuleInput> = serde_json::from_value(serde_json::json!([
        {"expression": {"combinator": "all", "children": [
            {"kind": "condition", "field": "path", "op": "eq", "values": ["/old"]}
        ]}, "action": {"type": "redirect", "target": "/new?{query}", "status": 308}}
    ]))
    .expect("drafts parse");
    let result = service
        .evaluate(
            project_id,
            EvaluateEdgeRulesRequest {
                request: sample("https://app.example.test/old?x=1"),
                rules: Some(drafts),
            },
        )
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        result.decision,
        Decision::Redirect {
            rule_id: -1,
            status: 308,
            location: "/new?x=1".into()
        }
    );
    assert_eq!(result.trace.len(), 1);
    assert_eq!(result.trace[0].outcome, RuleOutcome::Decided);
}
