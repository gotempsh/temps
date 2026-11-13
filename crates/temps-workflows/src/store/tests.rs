// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! [`PgWorkflowStore`] against a real, migrated database. Each test gets a
//! database of its own; every test returns early with a message when no
//! container runtime or external database is available.

use std::sync::Arc;

use chrono::{Duration, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection};
use temps_test_utils::TestDatabase;
use uuid::Uuid;

use super::*;
use crate::error::WorkflowError;
use crate::payload::StoredPayload;

struct Fixture {
    _db: TestDatabase,
    conn: Arc<DatabaseConnection>,
    store: PgWorkflowStore,
    project_id: i32,
    environment_id: i32,
}

async fn fixture(test: &str) -> Option<Fixture> {
    let env = crate::test_support::test_environment(test).await?;
    Some(Fixture {
        store: PgWorkflowStore::new(env.conn.clone()),
        conn: env.conn,
        _db: env.db,
        project_id: env.project_id,
        environment_id: env.environment_id,
    })
}

impl Fixture {
    fn new_run(&self, workflow: &str, key: Option<&str>) -> NewRun {
        NewRun {
            id: Uuid::new_v4(),
            project_id: self.project_id,
            environment_id: self.environment_id,
            workflow_name: workflow.into(),
            input: StoredPayload::default(),
            idempotency_key: key.map(str::to_string),
            deployment_id: None,
            starter: Starter::Machine,
            parent_run_id: None,
        }
    }

    async fn start(&self, workflow: &str) -> RunRow {
        match self
            .store
            .insert_run(self.new_run(workflow, None), 10_000)
            .await
            .expect("insert run")
        {
            StartOutcome::Created(run) => run,
            StartOutcome::Existing(run) => panic!("unexpected existing run {}", run.id),
        }
    }

    async fn claim_one(&self) -> ClaimedRun {
        let mut claimed = self.store.claim_due(1, &[]).await.expect("claim");
        assert_eq!(claimed.len(), 1, "expected one claimable run");
        claimed.remove(0)
    }

    async fn make_due(&self, run_id: Uuid) {
        crate::test_support::make_due(&self.conn, run_id).await;
    }

    async fn set_concurrency(&self, max_concurrent_runs: i32, stuck_after_failures: i32) {
        let settings = WorkflowSettings {
            max_concurrent_runs,
            stuck_after_failures,
            ..WorkflowSettings::default()
        };
        self.store
            .upsert_settings(self.project_id, self.environment_id, &settings)
            .await
            .expect("settings");
    }
}

fn step(run: &RunRow, sequence: i32, name: &str, status: StepStatus) -> StepRecord {
    StepRecord {
        run_id: run.id,
        lease: run.lease_token,
        sequence,
        name: name.into(),
        kind: StepKind::Step,
        status,
        attempts: 1,
        result: StoredPayload {
            encrypted: Some("sealed".into()),
            blob_key: None,
            bytes: 10,
        },
        error: StoredPayload::default(),
        wake_at: None,
        duration_ms: Some(5),
        trace_id: None,
        error_ref: None,
        event: None,
    }
}

const LIMITS: StepLimits = StepLimits {
    max_steps: 3,
    max_journal_bytes: 25,
    lease_secs: 120,
};

#[tokio::test]
async fn starting_honours_idempotency_keys_and_the_active_run_quota() {
    let Some(f) = fixture("idempotency test").await else {
        return;
    };
    assert_eq!(
        f.store.environment_project(f.environment_id).await.unwrap(),
        Some(f.project_id)
    );
    assert_eq!(f.store.environment_project(-1).await.unwrap(), None);

    let first = match f
        .store
        .insert_run(f.new_run("checkout", Some("order-1")), 10)
        .await
        .unwrap()
    {
        StartOutcome::Created(run) => run,
        other => panic!("expected a new run, got {other:?}"),
    };
    assert_eq!(first.status, "queued");
    match f
        .store
        .insert_run(f.new_run("checkout", Some("order-1")), 10)
        .await
        .unwrap()
    {
        StartOutcome::Existing(run) => assert_eq!(run.id, first.id),
        other => panic!("expected the existing run, got {other:?}"),
    }
    // The same key on another workflow is another run.
    assert!(matches!(
        f.store
            .insert_run(f.new_run("refund", Some("order-1")), 10)
            .await
            .unwrap(),
        StartOutcome::Created(_)
    ));
    // Two active runs now: a quota of 2 refuses a third, naming the quota.
    let error = f
        .store
        .insert_run(f.new_run("checkout", None), 2)
        .await
        .unwrap_err();
    assert!(
        matches!(
            error,
            WorkflowError::QuotaExceeded {
                quota: "max_active_runs",
                limit: 2,
                ..
            }
        ),
        "{error}"
    );
}

#[tokio::test]
async fn concurrent_starts_cannot_exceed_the_active_run_quota() {
    let Some(f) = fixture("admission race test").await else {
        return;
    };
    // Every start counts zero active runs unless the starts are serialized.
    let starts = (0..8).map(|i| {
        f.store
            .insert_run(f.new_run(&format!("burst-{i}"), None), 1)
    });
    let results = futures::future::join_all(starts).await;
    let created = results
        .iter()
        .filter(|result| matches!(result, Ok(StartOutcome::Created(_))))
        .count();
    let refused = results
        .iter()
        .filter(|result| matches!(result, Err(WorkflowError::QuotaExceeded { .. })))
        .count();
    assert_eq!((created, refused), (1, 7), "{results:?}");
}

#[tokio::test]
async fn claims_respect_environment_and_workflow_concurrency() {
    let Some(f) = fixture("concurrency test").await else {
        return;
    };
    f.set_concurrency(2, 8).await;
    for _ in 0..3 {
        f.start("emails").await;
    }
    f.start("billing").await;

    // A per-workflow limit of 1 on `emails` leaves room for one email run and
    // the billing run, within the environment's 2.
    let limits = [WorkflowLimit {
        environment_id: f.environment_id,
        workflow_name: "emails".into(),
        limit: 1,
    }];
    let claimed = f.store.claim_due(10, &limits).await.unwrap();
    assert_eq!(claimed.len(), 2);
    let mut names: Vec<_> = claimed
        .iter()
        .map(|c| c.run.workflow_name.as_str())
        .collect();
    names.sort_unstable();
    assert_eq!(names, ["billing", "emails"]);
    for claim in &claimed {
        assert_eq!(claim.run.status, "running");
        assert_eq!(claim.run.attempt, 1);
        assert_eq!(claim.run.lease_token, 1);
        assert_eq!(claim.previous_status, RunStatus::Queued);
        assert!(claim.run.lease_expires_at.is_some());
    }
    // Full: nothing more is claimed, the remaining runs stay queued.
    assert!(f.store.claim_due(10, &limits).await.unwrap().is_empty());

    // Finishing one frees a slot.
    let done = &claimed[0].run;
    f.store
        .release(
            done.id,
            done.lease_token,
            Release::Completed {
                output: StoredPayload::default(),
            },
        )
        .await
        .unwrap();
    assert_eq!(f.store.claim_due(10, &[]).await.unwrap().len(), 1);
}

#[tokio::test]
async fn concurrent_claims_never_exceed_the_environment_limit() {
    let Some(f) = fixture("concurrent claim test").await else {
        return;
    };
    f.set_concurrency(3, 8).await;
    for _ in 0..12 {
        f.start("fanout").await;
    }
    let f = Arc::new(f);
    let mut tasks = Vec::new();
    for _ in 0..6 {
        let f = f.clone();
        tasks.push(tokio::spawn(async move {
            f.store.claim_due(2, &[]).await.unwrap().len()
        }));
    }
    let mut total = 0;
    for task in tasks {
        total += task.await.unwrap();
    }
    assert_eq!(
        total, 3,
        "claims from parallel callers over-filled the limit"
    );
}

#[tokio::test]
async fn journal_writes_are_fenced_ordered_and_bounded() {
    let Some(f) = fixture("journal test").await else {
        return;
    };
    f.start("orders").await;
    let run = f.claim_one().await.run;

    // A write with an older lease is rejected with the current one.
    let mut stale_write = step(&run, 0, "charge", StepStatus::Completed);
    stale_write.lease = run.lease_token - 1;
    let error = f.store.record_step(stale_write, LIMITS).await.unwrap_err();
    assert!(
        matches!(error, WorkflowError::StaleLease { current, .. } if current == run.lease_token),
        "{error}"
    );

    // Entries must be appended in order.
    let error = f
        .store
        .record_step(step(&run, 1, "charge", StepStatus::Completed), LIMITS)
        .await
        .unwrap_err();
    assert!(
        matches!(
            error,
            WorkflowError::Validation {
                field: "sequence",
                ..
            }
        ),
        "{error}"
    );

    let recorded = f
        .store
        .record_step(step(&run, 0, "charge", StepStatus::Completed), LIMITS)
        .await
        .unwrap();
    assert!(matches!(recorded, StepRecordOutcome::Recorded { .. }));

    // Writing the same entry again returns the recorded one.
    let mut again = step(&run, 0, "charge", StepStatus::Completed);
    again.result.encrypted = Some("different".into());
    match f.store.record_step(again, LIMITS).await.unwrap() {
        StepRecordOutcome::AlreadyRecorded { entry, .. } => {
            assert_eq!(entry.result_encrypted.as_deref(), Some("sealed"));
        }
        other => panic!("expected the recorded entry, got {other:?}"),
    }

    // A different step at a recorded position is an incompatible replay.
    let error = f
        .store
        .record_step(step(&run, 0, "refund", StepStatus::Completed), LIMITS)
        .await
        .unwrap_err();
    assert!(
        matches!(&error, WorkflowError::Incompatible { sequence: 0, expected, found, .. }
            if expected == "charge" && found == "refund"),
        "{error}"
    );

    // 10 + 10 bytes fit in 25; a third 10-byte entry does not.
    f.store
        .record_step(step(&run, 1, "ship", StepStatus::Completed), LIMITS)
        .await
        .unwrap();
    let error = f
        .store
        .record_step(step(&run, 2, "notify", StepStatus::Completed), LIMITS)
        .await
        .unwrap_err();
    assert!(
        matches!(
            error,
            WorkflowError::QuotaExceeded {
                quota: "max_journal_bytes_per_run",
                ..
            }
        ),
        "{error}"
    );

    let current = f.store.get_run(run.id).await.unwrap().unwrap();
    assert_eq!(current.step_count, 2);
    assert_eq!(current.journal_bytes, 20);
    let journal = f.store.journal(run.id, 0, 100).await.unwrap();
    assert_eq!(
        journal.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
        ["charge", "ship"]
    );
    assert_eq!(f.store.journal(run.id, 1, 100).await.unwrap().len(), 1);
}

#[tokio::test]
async fn the_step_count_quota_is_enforced() {
    let Some(f) = fixture("step quota test").await else {
        return;
    };
    f.start("long").await;
    let run = f.claim_one().await.run;
    let limits = StepLimits {
        max_steps: 2,
        max_journal_bytes: 1_000,
        lease_secs: 120,
    };
    for sequence in 0..2 {
        f.store
            .record_step(step(&run, sequence, "s", StepStatus::Completed), limits)
            .await
            .unwrap();
    }
    let error = f
        .store
        .record_step(step(&run, 2, "s", StepStatus::Completed), limits)
        .await
        .unwrap_err();
    assert!(
        matches!(
            error,
            WorkflowError::QuotaExceeded {
                quota: "max_steps_per_run",
                limit: 2,
                ..
            }
        ),
        "{error}"
    );
}

#[tokio::test]
async fn failed_invocations_back_off_then_park_the_run_as_stuck() {
    let Some(f) = fixture("stuck test").await else {
        return;
    };
    f.set_concurrency(10, 2).await;
    let started = f.start("flaky").await;

    let first = f.claim_one().await.run;
    let status = f
        .store
        .fail_invocation(first.id, first.lease_token, "connection refused")
        .await
        .unwrap();
    assert_eq!(status, Some(RunStatus::Retrying));
    let retrying = f.store.get_run(started.id).await.unwrap().unwrap();
    assert!(retrying.due_at.unwrap() > Utc::now() + Duration::seconds(3));
    assert_eq!(
        retrying.blocked_reason.as_deref(),
        Some("connection refused")
    );
    // Not due yet.
    assert!(f.store.claim_due(10, &[]).await.unwrap().is_empty());

    f.make_due(started.id).await;
    let second = f.claim_one().await;
    assert_eq!(second.previous_status, RunStatus::Retrying);
    // A stale lease changes nothing.
    assert_eq!(
        f.store
            .fail_invocation(started.id, first.lease_token, "late")
            .await
            .unwrap(),
        None
    );
    let status = f
        .store
        .fail_invocation(started.id, second.run.lease_token, "connection refused")
        .await
        .unwrap();
    assert_eq!(status, Some(RunStatus::Stuck));
    let stuck = f.store.get_run(started.id).await.unwrap().unwrap();
    assert!(stuck.due_at.is_none());

    let stats = f.store.queue_stats().await.unwrap();
    assert_eq!(stats.len(), 1);
    assert_eq!(stats[0].stuck, 1);

    // Resume makes it due now and resets the failure count.
    let resumed = f.store.resume_run(started.id).await.unwrap().unwrap();
    assert_eq!(resumed.status, "queued");
    assert_eq!(resumed.consecutive_failures, 0);
    assert_eq!(f.claim_one().await.run.id, started.id);
    // A running run cannot be resumed.
    assert!(f.store.resume_run(started.id).await.unwrap().is_none());
}

#[tokio::test]
async fn blocked_claims_return_the_run_without_spending_an_attempt() {
    let Some(f) = fixture("blocked claim test").await else {
        return;
    };
    let run = f.start("nowhere").await;
    let claim = f.claim_one().await;
    let retry_at = Utc::now() + Duration::seconds(30);
    assert!(f
        .store
        .block_claimed(
            run.id,
            claim.run.lease_token,
            claim.previous_status,
            "No healthy replica",
            retry_at,
        )
        .await
        .unwrap());
    let blocked = f.store.get_run(run.id).await.unwrap().unwrap();
    assert_eq!(blocked.status, "queued");
    assert_eq!(blocked.attempt, 0);
    assert_eq!(
        blocked.blocked_reason.as_deref(),
        Some("No healthy replica")
    );
    assert!(blocked.lease_expires_at.is_none());
}

#[tokio::test]
async fn expired_leases_are_reaped_and_heartbeats_extend_them() {
    let Some(f) = fixture("reaper test").await else {
        return;
    };
    let run = f.start("slow").await;
    let claim = f.claim_one().await.run;

    let extended = f.store.heartbeat(run.id, claim.lease_token).await.unwrap();
    assert!(extended > Utc::now() + Duration::seconds(60));
    let error = f
        .store
        .heartbeat(run.id, claim.lease_token + 1)
        .await
        .unwrap_err();
    assert!(matches!(error, WorkflowError::StaleLease { .. }), "{error}");

    assert!(f.store.reap_expired(10).await.unwrap().is_empty());
    f.conn
        .execute(sea_orm::Statement::from_sql_and_values(
            sea_orm::DbBackend::Postgres,
            "UPDATE workflow_runs SET lease_expires_at = now() - interval '1 second' WHERE id = $1",
            vec![run.id.into()],
        ))
        .await
        .unwrap();
    let reaped = f.store.reap_expired(10).await.unwrap();
    assert_eq!(reaped.len(), 1);
    assert_eq!(reaped[0].run_id, run.id);
    assert_eq!(reaped[0].status, RunStatus::Retrying);
    // The lost invocation can no longer report.
    let error = f
        .store
        .release(
            run.id,
            claim.lease_token,
            Release::Completed {
                output: StoredPayload::default(),
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(error, WorkflowError::StaleLease { .. }), "{error}");
}

#[tokio::test]
async fn events_wake_waiting_runs_exactly_once_and_buffer_early_arrivals() {
    let Some(f) = fixture("event test").await else {
        return;
    };
    let run = f.start("approval").await;
    let claim = f.claim_one().await.run;
    let mut wait = step(&claim, 0, "approved", StepStatus::Pending);
    wait.kind = StepKind::Wait;
    wait.result = StoredPayload::default();
    f.store.record_step(wait, LIMITS).await.unwrap();
    let released = f
        .store
        .release(
            run.id,
            claim.lease_token,
            Release::Wait {
                sequence: 0,
                event: "approved".into(),
                key: "order-7".into(),
                timeout_at: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(released.status, "waiting");
    assert!(released.due_at.is_none());
    assert!(f.store.claim_due(10, &[]).await.unwrap().is_empty());

    let waiting = f
        .store
        .waiting_runs(f.environment_id, "approved", "order-7", None, 10)
        .await
        .unwrap();
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0].sequence, 0);
    assert!(f
        .store
        .waiting_runs(f.environment_id, "approved", "order-8", None, 10)
        .await
        .unwrap()
        .is_empty());

    let event_id = f
        .store
        .insert_event(NewEvent {
            project_id: f.project_id,
            environment_id: f.environment_id,
            name: "approved".into(),
            key: "order-7".into(),
            payload: StoredPayload::default(),
            source: "api",
            source_run_id: None,
        })
        .await
        .unwrap();
    // A crash between storing the event and delivering it is reconciled.
    let undelivered = f.store.undelivered_waits(10).await.unwrap();
    assert_eq!(undelivered.len(), 1);
    assert!(f
        .store
        .buffered_event(run.id, "approved", "order-7")
        .await
        .unwrap()
        .is_some());

    let delivered = StoredPayload {
        encrypted: Some("sealed".into()),
        blob_key: None,
        bytes: 4,
    };
    assert_eq!(
        f.store
            .complete_wait(&waiting[0], Some(event_id), delivered.clone(), 1 << 20)
            .await
            .unwrap(),
        WaitCompletion::Completed
    );
    // Compare-and-set: the second delivery is a no-op.
    assert_eq!(
        f.store
            .complete_wait(&waiting[0], Some(event_id), delivered, 1 << 20)
            .await
            .unwrap(),
        WaitCompletion::NotWaiting
    );
    assert!(f.store.undelivered_waits(10).await.unwrap().is_empty());
    assert!(f
        .store
        .buffered_event(run.id, "approved", "order-7")
        .await
        .unwrap()
        .is_none());

    let woken = f.claim_one().await;
    assert_eq!(woken.run.id, run.id);
    // Delivery made it queued (due now), not still waiting.
    assert_eq!(woken.previous_status, RunStatus::Queued);
    assert!(woken.run.awaited_event_name.is_none());
    let entry = f.store.step(run.id, 0).await.unwrap().unwrap();
    assert_eq!(entry.status, "completed");
    assert_eq!(entry.event_id, Some(event_id));
}

/// Suspend `run` (claimed under `claim`) on a pending wait at `sequence`.
async fn suspend_on(f: &Fixture, claim: &RunRow, sequence: i32, key: &str) -> WaitingRun {
    let mut wait = step(claim, sequence, "approved", StepStatus::Pending);
    wait.kind = StepKind::Wait;
    wait.result = StoredPayload::default();
    f.store.record_step(wait, LIMITS).await.unwrap();
    f.store
        .release(
            claim.id,
            claim.lease_token,
            Release::Wait {
                sequence,
                event: "approved".into(),
                key: key.into(),
                timeout_at: None,
            },
        )
        .await
        .unwrap();
    let waiting = f
        .store
        .waiting_runs(f.environment_id, "approved", key, None, 10)
        .await
        .unwrap();
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0].lease, claim.lease_token);
    waiting[0].clone()
}

async fn approved_event(f: &Fixture, key: &str) -> i64 {
    f.store
        .insert_event(NewEvent {
            project_id: f.project_id,
            environment_id: f.environment_id,
            name: "approved".into(),
            key: key.into(),
            payload: StoredPayload::default(),
            source: "api",
            source_run_id: None,
        })
        .await
        .unwrap()
}

fn sealed(bytes: i32) -> StoredPayload {
    StoredPayload {
        encrypted: Some("sealed".into()),
        blob_key: None,
        bytes,
    }
}

#[tokio::test]
async fn a_late_delivery_cannot_complete_a_later_wait_or_a_cancelled_run() {
    let Some(f) = fixture("late delivery test").await else {
        return;
    };
    let run = f.start("approval").await;
    let first = f.claim_one().await.run;
    // A delivery reads the waiting run, then lags.
    let stale = suspend_on(&f, &first, 0, "order-7").await;
    let event = approved_event(&f, "order-7").await;
    assert_eq!(
        f.store
            .complete_wait(&stale, Some(event), sealed(4), 1 << 20)
            .await
            .unwrap(),
        WaitCompletion::Completed
    );

    // The run resumes and waits again for the same name and key.
    let second = f.claim_one().await.run;
    let current = suspend_on(&f, &second, 1, "order-7").await;
    // The lagging delivery of the same event changes nothing...
    assert_eq!(
        f.store
            .complete_wait(&stale, Some(event), sealed(4), 1 << 20)
            .await
            .unwrap(),
        WaitCompletion::NotWaiting
    );
    // ...and the event the run already consumed cannot satisfy the new wait.
    assert_eq!(
        f.store
            .complete_wait(&current, Some(event), sealed(4), 1 << 20)
            .await
            .unwrap(),
        WaitCompletion::NotWaiting
    );
    assert_eq!(
        f.store.step(run.id, 1).await.unwrap().unwrap().status,
        "pending"
    );

    // Cancelled: a delivery read before the cancel changes nothing either.
    let another = approved_event(&f, "order-7").await;
    f.store.cancel_run(run.id).await.unwrap().unwrap();
    assert_eq!(
        f.store
            .complete_wait(&current, Some(another), sealed(4), 1 << 20)
            .await
            .unwrap(),
        WaitCompletion::NotWaiting
    );
    assert_eq!(
        f.store.step(run.id, 1).await.unwrap().unwrap().status,
        "pending"
    );
    assert_eq!(
        f.store.get_run(run.id).await.unwrap().unwrap().status,
        "cancelled"
    );

    // Retried from the wait: the new suspension has a new lease, so the
    // delivery read before the retry still cannot complete it.
    f.store.retry_from(run.id, 1).await.unwrap().unwrap();
    let third = f.claim_one().await.run;
    let replaced = suspend_on(&f, &third, 1, "order-7").await;
    assert_ne!(replaced.lease, current.lease);
    assert_eq!(
        f.store
            .complete_wait(&current, Some(another), sealed(4), 1 << 20)
            .await
            .unwrap(),
        WaitCompletion::NotWaiting
    );
    assert_eq!(
        f.store
            .complete_wait(&replaced, Some(another), sealed(4), 1 << 20)
            .await
            .unwrap(),
        WaitCompletion::Completed
    );
}

#[tokio::test]
async fn a_delivered_wait_honours_the_journal_quota_and_marks_blob_payloads() {
    let Some(f) = fixture("wait quota test").await else {
        return;
    };
    let run = f.start("approval").await;
    let claim = f.claim_one().await.run;
    let waiting = suspend_on(&f, &claim, 0, "order-9").await;
    let event = approved_event(&f, "order-9").await;
    assert_eq!(
        f.store
            .complete_wait(&waiting, Some(event), sealed(2048), 1024)
            .await
            .unwrap(),
        WaitCompletion::OverQuota {
            journal_bytes: 0,
            result_bytes: 2048
        }
    );
    let after = f.store.get_run(run.id).await.unwrap().unwrap();
    assert_eq!(after.journal_bytes, 0);
    assert_eq!(after.status, "waiting");
    assert_eq!(
        f.store.step(run.id, 0).await.unwrap().unwrap().status,
        "pending"
    );

    // Within the quota, with a result spilled to blob storage: the run is
    // marked so deleting it removes the blob.
    assert!(!after.has_blob_payloads);
    let spilled = StoredPayload {
        encrypted: None,
        blob_key: Some(format!("workflows/{}/{}/wait", f.environment_id, run.id)),
        bytes: 512,
    };
    assert_eq!(
        f.store
            .complete_wait(&waiting, Some(event), spilled, 1024)
            .await
            .unwrap(),
        WaitCompletion::Completed
    );
    let delivered = f.store.get_run(run.id).await.unwrap().unwrap();
    assert_eq!(delivered.journal_bytes, 512);
    assert!(delivered.has_blob_payloads);
}

#[tokio::test]
async fn an_over_quota_wait_fails_only_the_run_still_suspended_on_it() {
    let Some(f) = fixture("fail wait test").await else {
        return;
    };
    let run = f.start("approval").await;
    let claim = f.claim_one().await.run;
    let waiting = suspend_on(&f, &claim, 0, "order-10").await;
    let stale = WaitingRun {
        lease: waiting.lease - 1,
        ..waiting.clone()
    };
    assert_eq!(
        f.store
            .fail_wait(&stale, sealed(8), "quota_exceeded")
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        f.store
            .fail_wait(&waiting, sealed(8), "quota_exceeded")
            .await
            .unwrap()
            .as_deref(),
        Some("approval")
    );
    let failed = f.store.get_run(run.id).await.unwrap().unwrap();
    assert_eq!(failed.status, "failed");
    assert_eq!(failed.error_kind.as_deref(), Some("quota_exceeded"));
    assert!(failed.awaited_event_name.is_none());
}

#[tokio::test]
async fn a_retried_step_is_charged_only_for_its_latest_error() {
    let Some(f) = fixture("retry error bytes test").await else {
        return;
    };
    let run = f.start("flaky").await;
    let claim = f.claim_one().await.run;
    let limits = StepLimits {
        max_steps: 3,
        max_journal_bytes: 1024,
        lease_secs: 120,
    };
    for attempt in 1..=3 {
        let mut failing = step(&claim, 0, "charge", StepStatus::Pending);
        failing.attempts = attempt;
        failing.result = StoredPayload::default();
        failing.error = sealed(600);
        f.store.record_step(failing, limits).await.unwrap();
        let charged = f
            .store
            .get_run(run.id)
            .await
            .unwrap()
            .unwrap()
            .journal_bytes;
        assert_eq!(
            charged, 600,
            "attempt {attempt} kept an earlier error charged"
        );
    }
    let done = step(&claim, 0, "charge", StepStatus::Completed);
    f.store.record_step(done, limits).await.unwrap();
    let after = f.store.get_run(run.id).await.unwrap().unwrap();
    assert_eq!(after.journal_bytes, 10);
}

#[tokio::test]
async fn retrying_from_the_end_of_the_journal_keeps_a_pending_error_charged() {
    let Some(f) = fixture("retry keeps error bytes test").await else {
        return;
    };
    let run = f.start("flaky").await;
    let claim = f.claim_one().await.run;
    let limits = StepLimits {
        max_steps: 3,
        max_journal_bytes: 1024,
        lease_secs: 120,
    };
    let mut failing = step(&claim, 0, "charge", StepStatus::Pending);
    failing.result = StoredPayload::default();
    failing.error = sealed(600);
    f.store.record_step(failing, limits).await.unwrap();
    f.store.cancel_run(run.id).await.unwrap().unwrap();
    // From the end of the journal: the pending entry, and its error, stay.
    let retried = f.store.retry_from(run.id, 1).await.unwrap().unwrap();
    assert_eq!(retried.step_count, 1);
    assert_eq!(retried.journal_bytes, 600);

    let again = f.claim_one().await.run;
    f.store
        .record_step(step(&again, 0, "charge", StepStatus::Completed), limits)
        .await
        .unwrap();
    assert_eq!(
        f.store
            .get_run(run.id)
            .await
            .unwrap()
            .unwrap()
            .journal_bytes,
        10
    );
}

#[tokio::test]
async fn an_event_name_as_long_as_the_api_allows_is_stored() {
    let Some(f) = fixture("event name length test").await else {
        return;
    };
    let name = "a".repeat(crate::types::MAX_EVENT_NAME);
    crate::types::validate_name("event name", &name, crate::types::MAX_EVENT_NAME).unwrap();
    let id = f
        .store
        .insert_event(NewEvent {
            project_id: f.project_id,
            environment_id: f.environment_id,
            name: name.clone(),
            key: String::new(),
            payload: StoredPayload::default(),
            source: "api",
            source_run_id: None,
        })
        .await
        .unwrap();
    assert_eq!(f.store.event(id).await.unwrap().unwrap().name, name);
}

#[tokio::test]
async fn a_send_event_step_stores_its_event_in_the_same_write() {
    let Some(f) = fixture("send event test").await else {
        return;
    };
    let run = f.start("producer").await;
    let claim = f.claim_one().await.run;
    let mut send = step(&claim, 0, "notify", StepStatus::Completed);
    send.kind = StepKind::SendEvent;
    send.event = Some(NewEvent {
        project_id: f.project_id,
        environment_id: f.environment_id,
        name: "ready".into(),
        key: String::new(),
        payload: StoredPayload::default(),
        source: "workflow",
        source_run_id: Some(run.id),
    });
    let StepRecordOutcome::Recorded {
        event_id: Some(event_id),
        ..
    } = f.store.record_step(send.clone(), LIMITS).await.unwrap()
    else {
        panic!("expected the event to be stored");
    };
    let event = f.store.event(event_id).await.unwrap().unwrap();
    assert_eq!(event.source, "workflow");
    assert_eq!(event.source_run_id, Some(run.id));
    // A replayed write stores no second event.
    assert!(matches!(
        f.store.record_step(send, LIMITS).await.unwrap(),
        StepRecordOutcome::AlreadyRecorded { .. }
    ));
    let count = f
        .conn
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DbBackend::Postgres,
            "SELECT count(*)::BIGINT AS n FROM workflow_events",
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get::<i64>("", "n")
        .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn cancel_retry_from_and_delete_follow_the_run_lifecycle() {
    let Some(f) = fixture("lifecycle test").await else {
        return;
    };
    let run = f.start("pipeline").await;
    let claim = f.claim_one().await.run;
    for (sequence, name) in ["fetch", "transform", "load"].iter().enumerate() {
        let limits = StepLimits {
            max_steps: 10,
            max_journal_bytes: 1_000,
            lease_secs: 120,
        };
        f.store
            .record_step(
                step(&claim, sequence as i32, name, StepStatus::Completed),
                limits,
            )
            .await
            .unwrap();
    }
    // A running run cannot be deleted or retried.
    assert!(f.store.delete_run(run.id).await.unwrap().is_none());
    assert!(f.store.retry_from(run.id, 1).await.unwrap().is_none());

    let cancelled = f.store.cancel_run(run.id).await.unwrap().unwrap();
    assert_eq!(cancelled.status, "cancelled");
    assert!(cancelled.lease_token > claim.lease_token);
    assert!(cancelled.finished_at.is_some());
    // Cancelling twice is a no-op.
    assert!(f.store.cancel_run(run.id).await.unwrap().is_none());
    // The cancelled invocation's writes are rejected.
    assert!(matches!(
        f.store
            .record_step(step(&claim, 3, "extra", StepStatus::Completed), LIMITS)
            .await,
        Err(WorkflowError::StaleLease { .. })
    ));

    let retried = f.store.retry_from(run.id, 1).await.unwrap().unwrap();
    assert_eq!(retried.status, "queued");
    assert_eq!(retried.step_count, 1);
    assert_eq!(retried.journal_bytes, 10);
    assert!(retried.finished_at.is_none());
    assert_eq!(f.store.journal(run.id, 0, 10).await.unwrap().len(), 1);

    let again = f.claim_one().await.run;
    f.store
        .release(
            run.id,
            again.lease_token,
            Release::Failed {
                error: StoredPayload::default(),
                kind: "step_failed",
            },
        )
        .await
        .unwrap();
    let deleted = f.store.delete_run(run.id).await.unwrap().unwrap();
    assert_eq!(deleted.id, run.id);
    assert!(f.store.get_run(run.id).await.unwrap().is_none());
    assert!(f.store.journal(run.id, 0, 10).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_sleep_release_in_the_past_is_due_immediately() {
    let Some(f) = fixture("sleep test").await else {
        return;
    };
    let run = f.start("nap").await;
    let claim = f.claim_one().await.run;
    let later = Utc::now() + Duration::hours(1);
    let slept = f
        .store
        .release(run.id, claim.lease_token, Release::Sleep { until: later })
        .await
        .unwrap();
    assert_eq!(slept.status, "sleeping");
    assert!(f.store.claim_due(10, &[]).await.unwrap().is_empty());
    f.make_due(run.id).await;
    let woken = f.claim_one().await;
    assert_eq!(woken.previous_status, RunStatus::Sleeping);
    assert_eq!(woken.run.attempt, 2);
}

#[tokio::test]
async fn credentials_rotate_atomically_and_authenticate_by_hash() {
    let Some(f) = fixture("credential test").await else {
        return;
    };
    assert!(f
        .store
        .active_credential(f.environment_id)
        .await
        .unwrap()
        .is_none());
    let first = f
        .store
        .rotate_credential(
            f.project_id,
            f.environment_id,
            "hash-1",
            "twf_aaaaaaaa",
            None,
        )
        .await
        .unwrap();
    let second = f
        .store
        .rotate_credential(
            f.project_id,
            f.environment_id,
            "hash-2",
            "twf_bbbbbbbb",
            None,
        )
        .await
        .unwrap();
    assert_ne!(first.id, second.id);
    assert!(f
        .store
        .credential_by_hash("hash-1")
        .await
        .unwrap()
        .is_none());
    let found = f.store.credential_by_hash("hash-2").await.unwrap().unwrap();
    assert_eq!(found.id, second.id);
    assert!(found.last_used_at.is_some());
    assert_eq!(
        f.store
            .active_credential(f.environment_id)
            .await
            .unwrap()
            .unwrap()
            .id,
        second.id
    );
    assert!(f.store.revoke_credential(f.environment_id).await.unwrap());
    assert!(!f.store.revoke_credential(f.environment_id).await.unwrap());
    assert!(f
        .store
        .credential_by_hash("hash-2")
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn listing_filters_paginates_and_summarizes() {
    let Some(f) = fixture("listing test").await else {
        return;
    };
    for _ in 0..3 {
        f.start("a").await;
    }
    f.start("b").await;
    let (page, total) = f
        .store
        .list_runs(f.environment_id, &RunFilter::default(), 1, 2)
        .await
        .unwrap();
    assert_eq!(total, 4);
    assert_eq!(page.len(), 2);
    assert!(page[0].created_at >= page[1].created_at);
    let filter = RunFilter {
        status: Some(RunStatus::Queued),
        workflow_name: Some("b".into()),
    };
    let (page, total) = f
        .store
        .list_runs(f.environment_id, &filter, 1, 20)
        .await
        .unwrap();
    assert_eq!((page.len(), total), (1, 1));
    let summaries = f.store.workflow_summaries(f.environment_id).await.unwrap();
    assert_eq!(summaries["a"].active, 3);
    assert_eq!(summaries["b"].active, 1);
}

#[tokio::test]
async fn retention_deletes_only_finished_runs_past_their_window() {
    let Some(f) = fixture("retention test").await else {
        return;
    };
    let old = f.start("old").await;
    let claim = f.claim_one().await.run;
    f.store
        .release(
            old.id,
            claim.lease_token,
            Release::Completed {
                output: StoredPayload::default(),
            },
        )
        .await
        .unwrap();
    let active = f.start("active").await;
    assert!(f.store.delete_expired_runs(10).await.unwrap().is_empty());
    f.conn
        .execute_unprepared(
            "UPDATE workflow_runs SET finished_at = now() - interval '31 days' \
             WHERE finished_at IS NOT NULL",
        )
        .await
        .unwrap();
    let expired = f.store.delete_expired_runs(10).await.unwrap();
    assert_eq!(
        expired.iter().map(|e| e.run_id).collect::<Vec<_>>(),
        [old.id]
    );
    assert!(f.store.get_run(old.id).await.unwrap().is_none());
    assert!(f.store.get_run(active.id).await.unwrap().is_some());
    assert!(f.store.delete_expired_runs(10).await.unwrap().is_empty());

    // A run past retention that an operator retries is live again: retention
    // leaves it, and its payloads, alone.
    f.conn
        .execute_unprepared("DELETE FROM workflow_runs")
        .await
        .unwrap();
    let retried = f.start("retried").await;
    let claim = f.claim_one().await.run;
    f.store
        .release(
            retried.id,
            claim.lease_token,
            Release::Completed {
                output: StoredPayload::default(),
            },
        )
        .await
        .unwrap();
    f.conn
        .execute_unprepared(
            "UPDATE workflow_runs SET finished_at = now() - interval '31 days' \
             WHERE finished_at IS NOT NULL",
        )
        .await
        .unwrap();
    assert!(f.store.retry_from(retried.id, 0).await.unwrap().is_some());
    assert!(f.store.delete_expired_runs(10).await.unwrap().is_empty());
    assert_eq!(
        f.store.get_run(retried.id).await.unwrap().unwrap().status,
        "queued"
    );

    f.store
        .insert_event(NewEvent {
            project_id: f.project_id,
            environment_id: f.environment_id,
            name: "e".into(),
            key: String::new(),
            payload: StoredPayload::default(),
            source: "api",
            source_run_id: None,
        })
        .await
        .unwrap();
    assert_eq!(f.store.delete_expired_events(10).await.unwrap(), 0);
    f.conn
        .execute_unprepared("UPDATE workflow_events SET created_at = now() - interval '31 days'")
        .await
        .unwrap();
    assert_eq!(f.store.delete_expired_events(10).await.unwrap(), 1);
}

#[tokio::test]
async fn settings_round_trip_and_default_when_absent() {
    let Some(f) = fixture("settings test").await else {
        return;
    };
    assert_eq!(
        f.store.settings(f.environment_id).await.unwrap(),
        WorkflowSettings::default()
    );
    let custom = WorkflowSettings {
        host_service: Some("worker".into()),
        retention_days: 7,
        ..WorkflowSettings::default()
    };
    let saved = f
        .store
        .upsert_settings(f.project_id, f.environment_id, &custom)
        .await
        .unwrap();
    assert_eq!(saved, custom);
    assert_eq!(f.store.settings(f.environment_id).await.unwrap(), custom);
}

#[tokio::test]
async fn deploy_drain_pauses_claims_until_release_and_recovers_after_a_crash() {
    use sea_orm::{DbBackend, Statement};
    let Some(f) = fixture("deployment drain").await else {
        return;
    };
    let row=f.conn.query_one(Statement::from_sql_and_values(DbBackend::Postgres,"INSERT INTO deployments(project_id,environment_id,slug,state,created_at,updated_at,metadata) VALUES($1,$2,'drain-test','running',now(),now(),'{}') RETURNING id",vec![f.project_id.into(),f.environment_id.into()])).await.unwrap().unwrap();
    let deployment: i32 = row.try_get("", "id").unwrap();
    f.start("orders").await;
    let running = f.claim_one().await;
    let token = f
        .store
        .begin_drain(f.environment_id, deployment)
        .await
        .unwrap();
    assert!(f
        .store
        .begin_drain(f.environment_id, deployment)
        .await
        .is_err());
    f.start("queued").await;
    assert!(f.store.claim_due(10, &[]).await.unwrap().is_empty());
    assert_eq!(
        f.store.renew_drain(f.environment_id, token).await.unwrap(),
        1
    );
    f.store.cancel_run(running.run.id).await.unwrap();
    assert_eq!(
        f.store.renew_drain(f.environment_id, token).await.unwrap(),
        0
    );
    f.store
        .end_drain(f.environment_id, Uuid::new_v4())
        .await
        .unwrap();
    assert!(f.store.claim_due(10, &[]).await.unwrap().is_empty());
    f.store.end_drain(f.environment_id, token).await.unwrap();
    assert_eq!(f.store.claim_due(10, &[]).await.unwrap().len(), 1);
    let expired = f
        .store
        .begin_drain(f.environment_id, deployment)
        .await
        .unwrap();
    f.conn
        .execute_unprepared(
            "UPDATE workflow_deployment_drains SET expires_at=now()-interval '1 second'",
        )
        .await
        .unwrap();
    assert!(f
        .store
        .renew_drain(f.environment_id, expired)
        .await
        .is_err());
    f.start("after-crash").await;
    assert_eq!(f.store.claim_due(10, &[]).await.unwrap().len(), 1);
    let replacement = f
        .store
        .begin_drain(f.environment_id, deployment)
        .await
        .unwrap();
    f.store.end_drain(f.environment_id, expired).await.unwrap();
    assert!(f
        .store
        .renew_drain(f.environment_id, replacement)
        .await
        .is_ok());
    f.store
        .end_drain(f.environment_id, replacement)
        .await
        .unwrap();
}

mod managed;
