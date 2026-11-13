// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::sync::Arc;

use serde_json::json;
use temps_core::NoopProjectMetricsSink;

use super::*;
use crate::engine::tests::{harness, Harness, Reply, Script};
use crate::protocol::Outcome;

fn completing() -> Script {
    Arc::new(|_, _, _| {
        Box::pin(async {
            Reply::Outcome(Outcome::Completed {
                output: Some(json!("done")),
            })
        })
    })
}

fn service(h: &Harness) -> WorkflowService {
    WorkflowService::new(
        h.store.clone(),
        h.engine.clone(),
        h.codec.clone(),
        Arc::new(WorkflowMetrics::new(Arc::new(NoopProjectMetricsSink))),
    )
}

fn start(workflow: &str) -> StartRun {
    StartRun {
        workflow: workflow.into(),
        input: Some(json!({ "order": 7 })),
        idempotency_key: None,
    }
}

async fn tick(h: &Harness) {
    for claim in h.engine.claim(10).await.unwrap() {
        h.engine.invoke(claim).await;
    }
}

#[test]
fn pages_default_to_20_and_cap_at_100() {
    assert_eq!(page_bounds(None, None), (1, 20));
    assert_eq!(page_bounds(Some(0), Some(500)), (1, 100));
    assert_eq!(page_bounds(Some(3), Some(0)), (3, 1));
}

#[tokio::test]
async fn every_operation_is_scoped_to_its_project_and_environment() {
    let Some(h) = harness("service scope test", &["orders"], completing(), true).await else {
        return;
    };
    let service = service(&h);
    let (p, e) = (h.env.project_id, h.env.environment_id);

    let error = service
        .start(p + 1, e, start("orders"), Starter::Machine)
        .await
        .unwrap_err();
    assert!(
        matches!(error, WorkflowError::EnvironmentNotFound { project_id, environment_id } if project_id == p + 1 && environment_id == e),
        "{error}"
    );
    let (run, created) = service
        .start(p, e, start("orders"), Starter::User(h.env.user_id))
        .await
        .unwrap();
    assert!(created);
    assert_eq!(run.started_by_user_id, Some(h.env.user_id));

    // A run of another environment is not found here, by its id.
    let missing = uuid::Uuid::new_v4();
    let error = service.get_run(p, e, missing).await.unwrap_err();
    assert!(
        matches!(error, WorkflowError::RunNotFound { run_id, .. } if run_id == missing),
        "{error}"
    );
    assert!(matches!(
        service.cancel(p, e + 1000, run.id).await,
        Err(WorkflowError::EnvironmentNotFound { .. })
    ));
}

#[tokio::test]
async fn starting_validates_and_honours_idempotency_keys() {
    let Some(h) = harness("service start test", &["orders"], completing(), true).await else {
        return;
    };
    let service = service(&h);
    let (p, e) = (h.env.project_id, h.env.environment_id);
    let bad = service
        .start(p, e, start("has spaces"), Starter::Machine)
        .await
        .unwrap_err();
    assert!(
        matches!(
            bad,
            WorkflowError::Validation {
                field: "workflow",
                ..
            }
        ),
        "{bad}"
    );

    let keyed = StartRun {
        idempotency_key: Some("order-7".into()),
        ..start("orders")
    };
    let (first, created) = service
        .start(p, e, keyed.clone(), Starter::Machine)
        .await
        .unwrap();
    assert!(created);
    let (again, created) = service.start(p, e, keyed, Starter::Machine).await.unwrap();
    assert!(!created);
    assert_eq!(again.id, first.id);

    // Input is stored encrypted and only revealed on request.
    assert!(first
        .input_encrypted
        .as_deref()
        .is_some_and(|c| !c.contains("order")));
    assert_eq!(
        service
            .reveal(p, e, first.id, RevealField::Input)
            .await
            .unwrap(),
        Some(json!({ "order": 7 }))
    );
    let error = service
        .reveal(p, e, first.id, RevealField::StepResult(3))
        .await
        .unwrap_err();
    assert!(
        matches!(error, WorkflowError::StepNotFound { sequence: 3, .. }),
        "{error}"
    );
}

#[tokio::test]
async fn lifecycle_operations_report_the_state_that_refused_them() {
    let Some(h) = harness("service lifecycle test", &["orders"], completing(), true).await else {
        return;
    };
    let service = service(&h);
    let (p, e) = (h.env.project_id, h.env.environment_id);
    let (run, _) = service
        .start(p, e, start("orders"), Starter::Machine)
        .await
        .unwrap();
    tick(&h).await;
    assert_eq!(
        service.get_run(p, e, run.id).await.unwrap().status,
        "completed"
    );
    assert_eq!(
        service
            .reveal(p, e, run.id, RevealField::Output)
            .await
            .unwrap(),
        Some(json!("done"))
    );

    let error = service.cancel(p, e, run.id).await.unwrap_err();
    assert!(
        matches!(&error, WorkflowError::InvalidState { status, operation: "cancel", .. } if status == "completed"),
        "{error}"
    );
    let error = service.resume(p, e, run.id).await.unwrap_err();
    assert!(
        matches!(error, WorkflowError::InvalidState { .. }),
        "{error}"
    );
    let error = service.retry_from(p, e, run.id, Some(5)).await.unwrap_err();
    assert!(
        matches!(
            error,
            WorkflowError::Validation {
                field: "from_sequence",
                ..
            }
        ),
        "{error}"
    );

    // Default retry point of a run with no journal: the start.
    let retried = service.retry_from(p, e, run.id, None).await.unwrap();
    assert_eq!(retried.status, "queued");
    tick(&h).await;
    let deleted = service.delete(p, e, run.id).await.unwrap();
    assert_eq!(deleted.id, run.id);
    assert!(matches!(
        service.get_run(p, e, run.id).await,
        Err(WorkflowError::RunNotFound { .. })
    ));
}

#[tokio::test]
async fn settings_are_validated_per_field() {
    let Some(h) = harness("service settings test", &["orders"], completing(), true).await else {
        return;
    };
    let service = service(&h);
    let (p, e) = (h.env.project_id, h.env.environment_id);
    let bad = WorkflowSettings {
        lease_secs: 5,
        ..WorkflowSettings::default()
    };
    let error = service.update_settings(p, e, bad).await.unwrap_err();
    assert!(
        matches!(
            error,
            WorkflowError::Validation {
                field: "lease_secs",
                ..
            }
        ),
        "{error}"
    );
    let good = WorkflowSettings {
        retention_days: 3,
        ..WorkflowSettings::default()
    };
    assert_eq!(
        service.update_settings(p, e, good.clone()).await.unwrap(),
        good
    );
    assert_eq!(service.settings(p, e).await.unwrap(), good);
}

#[tokio::test]
async fn credentials_are_shown_once_stored_hashed_and_revocable() {
    let Some(h) = harness("service credential test", &["orders"], completing(), true).await else {
        return;
    };
    let service = service(&h);
    let (p, e) = (h.env.project_id, h.env.environment_id);
    assert!(service.credential(p, e).await.unwrap().is_none());
    let issued = service
        .rotate_credential(p, e, Some(h.env.user_id))
        .await
        .unwrap();
    assert!(issued.token.starts_with("twf_"));
    assert_ne!(issued.credential.token_hash, issued.token);
    assert!(!format!("{issued:?}").contains(&issued.token));
    assert_eq!(
        service
            .authenticate_credential(&issued.token)
            .await
            .unwrap()
            .id,
        issued.credential.id
    );

    let rotated = service
        .rotate_credential(p, e, Some(h.env.user_id))
        .await
        .unwrap();
    assert!(matches!(
        service.authenticate_credential(&issued.token).await,
        Err(WorkflowError::InvalidCredential { .. })
    ));
    assert!(service.revoke_credential(p, e).await.unwrap());
    assert!(matches!(
        service.authenticate_credential(&rotated.token).await,
        Err(WorkflowError::InvalidCredential { .. })
    ));
    assert!(matches!(
        service.authenticate_credential("tk_not_ours").await,
        Err(WorkflowError::InvalidCredential {
            reason: "not a workflow credential"
        })
    ));
}

#[tokio::test]
async fn the_overview_says_why_the_environment_cannot_run_workflows() {
    let Some(h) = harness("service overview test", &["orders"], completing(), false).await else {
        return;
    };
    let service = service(&h);
    let overview = service
        .overview(h.env.project_id, h.env.environment_id)
        .await
        .unwrap();
    assert!(!overview.executor.configured);
    assert_eq!(
        overview.executor.reason.as_deref(),
        Some("No deployment is live in this environment yet")
    );
    assert_eq!(
        overview.executor.setup_path.as_deref(),
        Some(EXECUTOR_SETUP_PATH)
    );
    assert!(overview.workflows.is_empty());
}

#[tokio::test]
async fn the_overview_lists_defined_workflows_and_their_runs() {
    let Some(h) = harness(
        "service overview live test",
        &["orders", "billing"],
        completing(),
        true,
    )
    .await
    else {
        return;
    };
    let service = service(&h);
    let (p, e) = (h.env.project_id, h.env.environment_id);
    service
        .start(p, e, start("orders"), Starter::Machine)
        .await
        .unwrap();
    // A run of a workflow the deployment no longer defines.
    service
        .start(p, e, start("legacy"), Starter::Machine)
        .await
        .unwrap();
    let overview = service.overview(p, e).await.unwrap();
    assert!(
        overview.executor.configured,
        "{:?}",
        overview.executor.reason
    );
    assert_eq!(overview.executor.replicas, 1);
    assert_eq!(overview.executor.sdk.as_deref(), Some("fake"));
    let names: Vec<_> = overview
        .workflows
        .iter()
        .map(|w| (w.name.as_str(), w.defined, w.active_runs))
        .collect();
    assert_eq!(
        names,
        [
            ("orders", true, 1),
            ("billing", true, 0),
            ("legacy", false, 1)
        ]
    );
}

#[tokio::test]
async fn sending_an_event_reports_how_many_runs_it_woke() {
    let Some(h) = harness("service event test", &["orders"], completing(), true).await else {
        return;
    };
    let service = service(&h);
    let (p, e) = (h.env.project_id, h.env.environment_id);
    let (id, delivery) = service
        .send_event(
            p,
            e,
            "paid",
            Some("order-7"),
            Some(&json!({"amount": 1})),
            "api",
        )
        .await
        .unwrap();
    assert!(id > 0);
    assert_eq!(delivery.delivered, 0);
    let error = service
        .send_event(p, e, "bad name", None, None, "api")
        .await
        .unwrap_err();
    assert!(
        matches!(
            error,
            WorkflowError::Validation {
                field: "event name",
                ..
            }
        ),
        "{error}"
    );
}
