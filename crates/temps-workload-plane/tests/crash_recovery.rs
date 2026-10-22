// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! ADR-055 phase 2 proof: the plane dies mid-run, and the next plane's
//! first pass finishes or cleans up what it left behind.
//!
//! Against a real Docker daemon (skipped when none is reachable) and a real
//! PostgreSQL schema:
//!
//! 1. plane 1 starts two jobs: `quick` (exits 0 after 3 s) and `stuck`
//!    (sleeps 300 s, maximum run time 6 s);
//! 2. plane 1 is killed while both run, and a container carrying the plane's
//!    labels but tracked by no row is left on the daemon;
//! 3. while no plane runs, `quick` exits and `stuck` passes its deadline;
//! 4. plane 2 is built on the same database and daemon. Its first pass must
//!    record `quick`'s exit (and run its finalizer with the output), stop and
//!    remove `stuck` and fail it with `DeadlineExceeded`, and remove the
//!    untracked container.

use std::collections::{BTreeMap, HashMap};
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use bollard::models::ContainerCreateBody;
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, InspectContainerOptions, ListContainersOptions,
    RemoveContainerOptions, StartContainerOptions,
};
use bollard::Docker;
use futures::FutureExt;
use sea_orm::ConnectionTrait;
use temps_core::{AuditLogger, AuditOperation};
use temps_database::test_utils::{is_container_runtime_unavailable, TestDatabase};
use temps_workload::{
    Artifact, ConditionKind, ConditionStatus, Lifetime, Process, RuntimeDriver, WorkloadKind,
    WorkloadPhase, WorkloadRef, WorkloadSpec,
};
use temps_workload_docker::DockerDriver;
use temps_workload_plane::{
    labels, ApplyRequest, Finalizer, NoEnvSource, NodeDriver, Owner, PlaneConfig, PlaneError,
    WorkloadPlane, WorkloadView,
};

const IMAGE: &str = "busybox:latest";
const TEST_LABEL: (&str, &str) = ("sh.temps.test", "adr055-p2");
const FINALIZER: &str = "test.temps.sh/record-run";

struct NoAudit;

#[async_trait]
impl AuditLogger for NoAudit {
    async fn create_audit_log(&self, _: &dyn AuditOperation) -> temps_core::anyhow::Result<()> {
        Ok(())
    }
}

#[derive(Default)]
struct Recorder {
    seen: Mutex<Vec<WorkloadView>>,
}

#[async_trait]
impl Finalizer for Recorder {
    fn name(&self) -> &str {
        FINALIZER
    }

    async fn finalize(&self, workload: &WorkloadView) -> Result<(), String> {
        self.seen
            .lock()
            .map_err(|_| "poisoned".to_string())?
            .push(workload.clone());
        Ok(())
    }
}

fn job(owner_id: i64, script: &str, timeout_secs: u32) -> WorkloadSpec {
    WorkloadSpec {
        owner: WorkloadRef::new(WorkloadKind::Job, owner_id),
        artifact: Artifact::oci(IMAGE),
        isolation: Default::default(),
        process: Process {
            command: Some(vec!["sh".into(), "-c".into(), script.into()]),
            ..Default::default()
        },
        env: Default::default(),
        resources: Default::default(),
        volumes: vec![],
        ports: vec![],
        network: Default::default(),
        probes: Default::default(),
        placement: Default::default(),
        lifetime: Lifetime::RunToCompletion {
            timeout_secs,
            retry_limit: 0,
            ttl_secs_after_finished: Some(3_600),
        },
        security: None,
        labels: BTreeMap::from([(TEST_LABEL.0.to_string(), TEST_LABEL.1.to_string())]),
    }
}

fn request(owner_id: i64, name: &str, spec: WorkloadSpec) -> ApplyRequest {
    ApplyRequest {
        owner: Owner::new("cron", owner_id),
        name: name.to_string(),
        spec,
        project_slug: Some("shop".to_string()),
        finalizers: vec![FINALIZER.to_string()],
    }
}

fn config(scope: &str) -> PlaneConfig {
    let mut config = PlaneConfig::new(scope);
    config.poll_interval = Duration::from_millis(500);
    config
}

fn plane(
    db: &TestDatabase,
    driver: &Arc<dyn RuntimeDriver>,
    scope: &str,
    recorder: Arc<Recorder>,
) -> Arc<WorkloadPlane> {
    Arc::new(
        WorkloadPlane::new(
            db.db.clone(),
            NodeDriver(Some(Arc::clone(driver))),
            Arc::new(NoEnvSource),
            Arc::new(NoAudit),
            config(scope),
        )
        .with_finalizer(recorder),
    )
}

async fn container_exists(docker: &Docker, name: &str) -> bool {
    docker
        .inspect_container(name, None::<InspectContainerOptions>)
        .await
        .is_ok()
}

/// Remove every container this test's plane scope created.
async fn cleanup(docker: &Docker, scope: &str) {
    let options = ListContainersOptions {
        all: true,
        filters: Some(HashMap::from([(
            "label".to_string(),
            vec![format!("{}={scope}", labels::PLANE)],
        )])),
        ..Default::default()
    };
    if let Ok(containers) = docker.list_containers(Some(options)).await {
        for container in containers {
            if let Some(id) = container.id {
                let _ = docker
                    .remove_container(
                        &id,
                        Some(RemoveContainerOptions {
                            force: true,
                            v: true,
                            ..Default::default()
                        }),
                    )
                    .await;
            }
        }
    }
}

#[tokio::test]
async fn the_next_plane_finishes_or_cleans_up_a_run_its_predecessor_left_mid_run() {
    let docker = match Docker::connect_with_defaults() {
        Ok(docker) => docker,
        Err(error) => {
            println!("Docker not available, skipping: {error}");
            return;
        }
    };
    if let Err(error) = docker.ping().await {
        println!("Docker not available, skipping: {error}");
        return;
    }
    let db = match TestDatabase::new().await {
        Ok(db) => db,
        Err(error) => {
            let text = format!("{error:#}");
            if is_container_runtime_unavailable(&text) {
                println!("Docker not available for the test database, skipping: {text}");
                return;
            }
            panic!("test database could not be created: {text}");
        }
    };
    for statement in temps_migrations::WORKLOAD_TABLES_SQL {
        db.db
            .execute_unprepared(statement)
            .await
            .unwrap_or_else(|e| panic!("creating the plane's tables failed: {e}"));
    }
    let scope = format!("p2crash{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);

    let outcome = AssertUnwindSafe(scenario(&docker, &db, &scope))
        .catch_unwind()
        .await;
    cleanup(&docker, &scope).await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

async fn scenario(docker: &Docker, db: &TestDatabase, scope: &str) {
    let driver: Arc<dyn RuntimeDriver> = Arc::new(DockerDriver::new(docker.clone()));

    // 1. Plane 1 starts both jobs and sees them running.
    let first_recorder = Arc::new(Recorder::default());
    let first = plane(db, &driver, scope, first_recorder.clone());
    let first_reconciler = first.start().expect("plane 1 reconciler");
    let quick = first
        .apply_and_wait(
            request(
                101,
                "quick",
                job(101, "echo started; sleep 3; echo all-done; exit 0", 120),
            ),
            ConditionKind::Running,
            Duration::from_secs(60),
        )
        .await
        .expect("quick is running");
    let stuck = first
        .apply_and_wait(
            request(102, "stuck", job(102, "echo started; sleep 300", 6)),
            ConditionKind::Running,
            Duration::from_secs(60),
        )
        .await
        .expect("stuck is running");
    let quick_container = format!("{scope}-job-{}-a1", quick.id);
    let stuck_container = format!("{scope}-job-{}-a1", stuck.id);
    assert!(container_exists(docker, &quick_container).await);
    assert!(container_exists(docker, &stuck_container).await);

    // 2. The plane dies mid-run. A container with its labels that no row
    //    tracks is left behind as well.
    first_reconciler.abort();
    drop(first);
    let orphan = format!("{scope}-job-999999-a1");
    docker
        .create_container(
            Some(CreateContainerOptionsBuilder::new().name(&orphan).build()),
            ContainerCreateBody {
                image: Some(IMAGE.to_string()),
                cmd: Some(vec!["sleep".into(), "300".into()]),
                labels: Some(HashMap::from([
                    (labels::PLANE.to_string(), scope.to_string()),
                    (
                        labels::MANAGED_BY.to_string(),
                        labels::MANAGED_BY_VALUE.to_string(),
                    ),
                    (
                        temps_workload::labels::WORKLOAD_ID.to_string(),
                        "999999".to_string(),
                    ),
                    (labels::ATTEMPT.to_string(), "1".to_string()),
                    (TEST_LABEL.0.to_string(), TEST_LABEL.1.to_string()),
                ])),
                ..Default::default()
            },
        )
        .await
        .expect("orphan created");
    docker
        .start_container(&orphan, None::<StartContainerOptions>)
        .await
        .expect("orphan started");

    // 3. While no plane runs, `quick` exits and `stuck` passes its deadline.
    tokio::time::sleep(Duration::from_secs(8)).await;
    let quick_state = docker
        .inspect_container(&quick_container, None::<InspectContainerOptions>)
        .await
        .expect("quick still exists")
        .state
        .unwrap_or_default();
    assert_eq!(quick_state.running, Some(false), "quick exited meanwhile");
    assert!(container_exists(docker, &stuck_container).await);
    assert!(first_recorder.seen.lock().expect("lock").is_empty());

    // 4. A fresh plane on the same database and daemon.
    let second_recorder = Arc::new(Recorder::default());
    let second = plane(db, &driver, scope, second_recorder.clone());
    let second_reconciler = second.start().expect("plane 2 reconciler");

    // quick: finished, its exit recorded.
    let finished = second
        .wait_for(quick.id, ConditionKind::Completed, Duration::from_secs(30))
        .await
        .expect("quick completed");
    assert_eq!(finished.status.phase, WorkloadPhase::Succeeded);
    assert_eq!(finished.status.exit_code, Some(0));

    // stuck: cleaned up, failed with a reason, no container left.
    let error = second
        .wait_for(stuck.id, ConditionKind::Completed, Duration::from_secs(30))
        .await
        .expect_err("stuck failed");
    match error {
        PlaneError::ConditionFailed {
            status,
            reason,
            message,
            ..
        } => {
            assert_eq!(status, ConditionStatus::False);
            assert_eq!(reason, "DeadlineExceeded");
            assert!(message.contains("6s"), "{message}");
        }
        other => panic!("expected ConditionFailed for stuck, got {other}"),
    }
    assert!(
        !container_exists(docker, &stuck_container).await,
        "the run past its deadline left no container"
    );

    // The finalizers ran once each, with the result.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let seen = second_recorder.seen.lock().expect("lock").clone();
        if seen.len() >= 2 {
            let quick_seen = seen
                .iter()
                .find(|v| v.id == quick.id)
                .expect("quick finalized");
            assert_eq!(quick_seen.status.exit_code, Some(0));
            let tail = quick_seen.output_tail.clone().unwrap_or_default();
            assert!(tail.contains("all-done"), "quick's output tail: {tail:?}");
            let stuck_seen = seen
                .iter()
                .find(|v| v.id == stuck.id)
                .expect("stuck finalized");
            assert_eq!(stuck_seen.status.phase, WorkloadPhase::Failed);
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "finalizers did not run: {seen:?}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(second
        .get(quick.id)
        .await
        .expect("quick")
        .finalizers
        .is_empty());

    // The untracked container is removed by the startup pass.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while container_exists(docker, &orphan).await {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the orphan was not removed"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    // quick's container is kept for its time-to-live.
    assert!(container_exists(docker, &quick_container).await);

    second_reconciler.abort();
}
