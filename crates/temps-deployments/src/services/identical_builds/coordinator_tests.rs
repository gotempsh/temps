// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Coordination against a real PostgreSQL: leader and followers, take-over
//! after failure, cancellation and a stalled process, the bounded wait, the
//! line order, and the rules that keep an image from being reused where it
//! must not be.

use super::*;
use crate::services::identical_builds::BuildIdentity;
use sea_orm::{ActiveModelTrait, Set};
use std::sync::Mutex;
use temps_entities::{deployments, preset::Preset, projects};
use temps_test_utils::TestDatabase;

struct Recorder(Mutex<Vec<AdmissionEvent>>);

impl Recorder {
    fn new() -> Arc<Self> {
        Arc::new(Self(Mutex::new(Vec::new())))
    }
    fn events(&self) -> Vec<AdmissionEvent> {
        self.0.lock().unwrap().clone()
    }
}

#[async_trait]
impl AdmissionObserver for Recorder {
    async fn observe(&self, event: AdmissionEvent) {
        self.0.lock().unwrap().push(event);
    }
}

fn fast() -> CoordinationTiming {
    CoordinationTiming {
        poll_interval: Duration::from_millis(40),
        heartbeat_interval: Duration::from_millis(40),
        stale_after: Duration::from_millis(600),
        max_wait: Duration::from_secs(20),
        reuse_window: Duration::from_secs(60),
    }
}

struct Fixture {
    _db: TestDatabase,
    db: Arc<DbConnection>,
    coordinator: IdenticalBuildCoordinator,
}

async fn fixture() -> Option<Fixture> {
    let test_db = match TestDatabase::with_migrations().await {
        Ok(db) => db,
        Err(error) => {
            eprintln!("Skipping identical build coordination test: no PostgreSQL ({error})");
            return None;
        }
    };
    let db = test_db.connection_arc();
    Some(Fixture {
        coordinator: IdenticalBuildCoordinator::new(db.clone()).with_timing(fast()),
        db,
        _db: test_db,
    })
}

impl Fixture {
    async fn project(&self, slug: &str) -> i32 {
        projects::ActiveModel {
            name: Set(slug.to_string()),
            slug: Set(slug.to_string()),
            repo_owner: Set("acme".to_string()),
            repo_name: Set("shop".to_string()),
            main_branch: Set("main".to_string()),
            preset: Set(Preset::NextJs),
            directory: Set("/".to_string()),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            is_deleted: Set(false),
            ..Default::default()
        }
        .insert(self.db.as_ref())
        .await
        .unwrap()
        .id
    }

    async fn deployment(&self, project_id: i32) -> i32 {
        let environment = temps_entities::environments::ActiveModel {
            project_id: Set(project_id),
            name: Set(format!("env-{}", uuid::Uuid::new_v4())),
            slug: Set(format!("env-{}", uuid::Uuid::new_v4())),
            host: Set(format!("{}.example.test", uuid::Uuid::new_v4())),
            subdomain: Set(format!("{}.example.test", uuid::Uuid::new_v4())),
            upstreams: Set(Default::default()),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            ..Default::default()
        }
        .insert(self.db.as_ref())
        .await
        .unwrap();
        deployments::ActiveModel {
            project_id: Set(project_id),
            environment_id: Set(environment.id),
            slug: Set(format!("deployment-{}", uuid::Uuid::new_v4())),
            state: Set("running".to_string()),
            metadata: Set(Some(deployments::DeploymentMetadata::default())),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            ..Default::default()
        }
        .insert(self.db.as_ref())
        .await
        .unwrap()
        .id
    }

    async fn ticket(
        &self,
        project_id: i32,
        mode: IdenticalBuildMode,
        fingerprint: &str,
    ) -> BuildTicket {
        BuildTicket {
            deployment_id: self.deployment(project_id).await,
            project_id,
            job_id: "build_image".to_string(),
            mode,
            scope: IdenticalBuildScope::Fingerprint,
            identity: BuildIdentity {
                fingerprint: fingerprint.to_string(),
                source_key: "source-1".to_string(),
                considered_values: 0,
            },
            access_boundary: "unrestricted".to_string(),
            repository: "acme/shop".to_string(),
            commit_sha: "0123456789abcdef0123456789abcdef01234567".to_string(),
        }
    }

    async fn state(&self, deployment_id: i32) -> (String, Option<i32>, Option<String>) {
        let row = self
            .db
            .query_one(statement(
                "SELECT state, source_build_id, detail FROM identical_builds WHERE deployment_id = $1",
                vec![deployment_id.into()],
            ))
            .await
            .unwrap()
            .expect("row");
        (
            row.try_get("", "state").unwrap(),
            row.try_get("", "source_build_id").unwrap(),
            row.try_get("", "detail").unwrap(),
        )
    }

    async fn build(&self, ticket: &BuildTicket) -> BuildLease {
        match self
            .coordinator
            .admit(ticket, Recorder::new().as_ref())
            .await
        {
            Ok(Admission::Build(lease)) => lease,
            other => panic!("expected to build, got {other:?}"),
        }
    }

    /// Start admitting `ticket` in the background and wait until it waits.
    async fn wait_in_line(
        &self,
        ticket: &BuildTicket,
    ) -> (
        tokio::task::JoinHandle<Result<Admission, IdenticalBuildError>>,
        Arc<Recorder>,
    ) {
        let recorder = Recorder::new();
        let coordinator = self.coordinator.clone();
        let task_ticket = ticket.clone();
        let task_recorder = recorder.clone();
        let handle = tokio::spawn(async move {
            coordinator
                .admit(&task_ticket, task_recorder.as_ref())
                .await
        });
        for _ in 0..200 {
            if !recorder.events().is_empty() {
                return (handle, recorder);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("deployment {} never started waiting", ticket.deployment_id);
    }
}

fn image(tag: &str) -> SharedImage {
    SharedImage {
        image_tag: tag.to_string(),
        image_id: "sha256:abc".to_string(),
        size_bytes: 1024,
        builder_node_id: Some(3),
        image_tags_by_platform: HashMap::new(),
        builder_node_ids_by_platform: HashMap::new(),
        image_ids_by_platform: HashMap::new(),
        image_config: None,
        dockerfile_preset: Some("nextjs".to_string()),
    }
}

async fn finished(
    handle: tokio::task::JoinHandle<Result<Admission, IdenticalBuildError>>,
) -> Result<Admission, IdenticalBuildError> {
    tokio::time::timeout(Duration::from_secs(10), handle)
        .await
        .expect("admission finished in time")
        .expect("admission task")
}

#[tokio::test(flavor = "multi_thread")]
async fn followers_wait_for_the_leader_and_reuse_its_image() {
    let Some(f) = fixture().await else { return };
    let (a, b, c) = (
        f.project("a").await,
        f.project("b").await,
        f.project("c").await,
    );
    let leader = f.ticket(a, IdenticalBuildMode::Share, "fp-1").await;
    let follower_b = f.ticket(b, IdenticalBuildMode::Share, "fp-1").await;
    let follower_c = f.ticket(c, IdenticalBuildMode::Share, "fp-1").await;

    let lease = f.build(&leader).await;
    let (b_task, b_events) = f.wait_in_line(&follower_b).await;
    let (c_task, _) = f.wait_in_line(&follower_c).await;
    assert_eq!(
        b_events.events()[0],
        AdmissionEvent::Waiting {
            leader: Some(LeaderView {
                build_id: lease.build_id(),
                deployment_id: Some(leader.deployment_id),
                same_project: false,
                will_share: true,
            }),
            ahead: 0,
        }
    );

    lease.succeed(Some(&image("a-build:latest"))).await.unwrap();
    for (task, ticket) in [(b_task, &follower_b), (c_task, &follower_c)] {
        match finished(task).await.unwrap() {
            Admission::Reuse(reused) => {
                assert_eq!(reused.deployment_id, leader.deployment_id);
                assert_eq!(reused.image, image("a-build:latest"));
                assert!(!reused.same_project);
                assert_eq!(reused.repository, "acme/shop");
            }
            other => panic!(
                "deployment {} should reuse, got {other:?}",
                ticket.deployment_id
            ),
        }
        let (state, source, _) = f.state(ticket.deployment_id).await;
        assert_eq!(state, "reused");
        assert!(source.is_some());
    }
    assert_eq!(f.state(leader.deployment_id).await.0, "succeeded");

    // A build starting later, within the reuse window, reuses without waiting.
    let late = f.ticket(b, IdenticalBuildMode::Share, "fp-1").await;
    assert!(matches!(
        f.coordinator
            .admit(&late, Recorder::new().as_ref())
            .await
            .unwrap(),
        Admission::Reuse(_)
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn queue_mode_serialises_without_reusing() {
    let Some(f) = fixture().await else { return };
    let (a, b) = (f.project("a").await, f.project("b").await);
    let first = f.ticket(a, IdenticalBuildMode::Queue, "fp-1").await;
    let second = f.ticket(b, IdenticalBuildMode::Queue, "fp-1").await;

    let lease = f.build(&first).await;
    let (task, events) = f.wait_in_line(&second).await;
    assert!(matches!(
        &events.events()[0],
        AdmissionEvent::Waiting {
            leader: Some(LeaderView {
                will_share: false,
                ..
            }),
            ..
        }
    ));
    lease.succeed(None).await.unwrap();
    assert!(matches!(finished(task).await.unwrap(), Admission::Build(_)));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_image_is_only_reused_between_projects_that_both_opted_in() {
    let Some(f) = fixture().await else { return };
    let (a, b) = (f.project("a").await, f.project("b").await);
    // The leader only queues: it never offers its image.
    let queue_leader = f.ticket(a, IdenticalBuildMode::Queue, "fp-1").await;
    let lease = f.build(&queue_leader).await;
    lease.succeed(Some(&image("a:latest"))).await.unwrap();
    let sharer = f.ticket(b, IdenticalBuildMode::Share, "fp-1").await;
    let admission = f
        .coordinator
        .admit(&sharer, Recorder::new().as_ref())
        .await
        .unwrap();
    assert!(matches!(admission, Admission::Build(_)), "{admission:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_different_fingerprint_queues_in_the_same_source_group_but_builds_its_own_image() {
    let Some(f) = fixture().await else { return };
    let (a, b) = (f.project("a").await, f.project("b").await);
    let mut first = f.ticket(a, IdenticalBuildMode::Share, "fp-1").await;
    let mut second = f.ticket(b, IdenticalBuildMode::Share, "fp-2").await;
    first.scope = IdenticalBuildScope::Source;
    second.scope = IdenticalBuildScope::Source;

    let lease = f.build(&first).await;
    let (task, events) = f.wait_in_line(&second).await;
    assert!(matches!(
        &events.events()[0],
        AdmissionEvent::Waiting {
            leader: Some(LeaderView {
                will_share: false,
                ..
            }),
            ..
        }
    ));
    lease.succeed(Some(&image("a:latest"))).await.unwrap();
    assert!(matches!(finished(task).await.unwrap(), Admission::Build(_)));
}

#[tokio::test(flavor = "multi_thread")]
async fn another_access_boundary_never_reuses_and_never_sees_the_leader() {
    let Some(f) = fixture().await else { return };
    let (a, b) = (f.project("a").await, f.project("b").await);
    let leader = f.ticket(a, IdenticalBuildMode::Share, "fp-1").await;
    let mut gated = f.ticket(b, IdenticalBuildMode::Share, "fp-1").await;
    gated.access_boundary = "teams:4".to_string();

    let lease = f.build(&leader).await;
    let (task, events) = f.wait_in_line(&gated).await;
    assert_eq!(
        events.events()[0],
        AdmissionEvent::Waiting {
            leader: Some(LeaderView {
                build_id: lease.build_id(),
                deployment_id: None,
                same_project: false,
                will_share: false,
            }),
            ahead: 0,
        }
    );
    lease.succeed(Some(&image("a:latest"))).await.unwrap();
    assert!(matches!(finished(task).await.unwrap(), Admission::Build(_)));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_leader_hands_the_build_to_the_next_in_line() {
    let Some(f) = fixture().await else { return };
    let (a, b) = (f.project("a").await, f.project("b").await);
    let leader = f.ticket(a, IdenticalBuildMode::Share, "fp-1").await;
    let follower = f.ticket(b, IdenticalBuildMode::Share, "fp-1").await;
    let lease = f.build(&leader).await;
    let (task, events) = f.wait_in_line(&follower).await;

    lease.fail("exit code 1").await.unwrap();
    assert!(matches!(finished(task).await.unwrap(), Admission::Build(_)));
    let (state, _, detail) = f.state(leader.deployment_id).await;
    assert_eq!(state, "failed");
    assert_eq!(detail.as_deref(), Some("exit code 1"));
    assert!(events.events().iter().any(|event| matches!(
        event,
        AdmissionEvent::LeaderEnded {
            state: IdenticalBuildState::Failed,
            ..
        }
    )));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cancelled_leader_does_not_strand_its_followers() {
    let Some(f) = fixture().await else { return };
    let (a, b) = (f.project("a").await, f.project("b").await);
    let leader = f.ticket(a, IdenticalBuildMode::Share, "fp-1").await;
    let follower = f.ticket(b, IdenticalBuildMode::Share, "fp-1").await;
    let lease = f.build(&leader).await;
    let (task, events) = f.wait_in_line(&follower).await;

    // The leader's deployment is cancelled (a newer commit arrived): its
    // build future, lease included, is dropped.
    drop(lease);
    assert!(matches!(finished(task).await.unwrap(), Admission::Build(_)));
    assert_eq!(f.state(leader.deployment_id).await.0, "cancelled");
    assert!(events.events().iter().any(|event| matches!(
        event,
        AdmissionEvent::LeaderEnded {
            state: IdenticalBuildState::Cancelled,
            ..
        }
    )));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cancelled_follower_gives_up_its_place_in_line() {
    let Some(f) = fixture().await else { return };
    let (a, b, c) = (
        f.project("a").await,
        f.project("b").await,
        f.project("c").await,
    );
    let leader = f.ticket(a, IdenticalBuildMode::Queue, "fp-1").await;
    let cancelled = f.ticket(b, IdenticalBuildMode::Queue, "fp-1").await;
    let next = f.ticket(c, IdenticalBuildMode::Queue, "fp-1").await;
    let lease = f.build(&leader).await;
    let (cancelled_task, _) = f.wait_in_line(&cancelled).await;
    let (next_task, _) = f.wait_in_line(&next).await;

    cancelled_task.abort();
    let _ = cancelled_task.await;
    for _ in 0..100 {
        if f.state(cancelled.deployment_id).await.0 == "cancelled" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(f.state(cancelled.deployment_id).await.0, "cancelled");

    lease.succeed(None).await.unwrap();
    assert!(matches!(
        finished(next_task).await.unwrap(),
        Admission::Build(_)
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_leader_whose_process_stopped_is_taken_over() {
    let Some(f) = fixture().await else { return };
    let (a, b) = (f.project("a").await, f.project("b").await);
    let crashed = f.ticket(a, IdenticalBuildMode::Share, "fp-1").await;
    let follower = f.ticket(b, IdenticalBuildMode::Share, "fp-1").await;
    // A process that crashed mid-build leaves a building row whose heartbeat
    // no longer moves.
    f.db.execute(statement(
        "INSERT INTO identical_builds (deployment_id, project_id, job_id, mode, queue_key, \
            fingerprint, access_boundary, state, repository, commit_sha, heartbeat_at) \
         VALUES ($1, $2, 'build_image', 'share', 'fingerprint:fp-1', 'fp-1', 'unrestricted', \
            'building', 'acme/shop', $3, now() - interval '1 hour')",
        vec![
            crashed.deployment_id.into(),
            a.into(),
            crashed.commit_sha.clone().into(),
        ],
    ))
    .await
    .unwrap();

    let admission = f
        .coordinator
        .admit(&follower, Recorder::new().as_ref())
        .await
        .unwrap();
    assert!(matches!(admission, Admission::Build(_)));
    let (state, _, detail) = f.state(crashed.deployment_id).await;
    assert_eq!(state, "abandoned");
    assert!(detail.unwrap().contains("took over"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_live_leader_keeps_its_group_through_its_heartbeat() {
    let Some(f) = fixture().await else { return };
    let (a, b) = (f.project("a").await, f.project("b").await);
    let leader = f.ticket(a, IdenticalBuildMode::Queue, "fp-1").await;
    let follower = f.ticket(b, IdenticalBuildMode::Queue, "fp-1").await;
    let lease = f.build(&leader).await;
    let (task, _) = f.wait_in_line(&follower).await;
    // Well past `stale_after`: the heartbeat keeps the leader alive.
    tokio::time::sleep(fast().stale_after * 3).await;
    assert!(!task.is_finished(), "the follower must still be waiting");
    assert_eq!(f.state(leader.deployment_id).await.0, "building");
    lease.succeed(None).await.unwrap();
    assert!(matches!(finished(task).await.unwrap(), Admission::Build(_)));
}

#[tokio::test(flavor = "multi_thread")]
async fn waiting_is_bounded() {
    let Some(f) = fixture().await else { return };
    let (a, b) = (f.project("a").await, f.project("b").await);
    let coordinator = f.coordinator.clone().with_timing(CoordinationTiming {
        max_wait: Duration::from_millis(300),
        ..fast()
    });
    let leader = f.ticket(a, IdenticalBuildMode::Queue, "fp-1").await;
    let follower = f.ticket(b, IdenticalBuildMode::Queue, "fp-1").await;
    let _lease = f.build(&leader).await;
    let error = coordinator
        .admit(&follower, Recorder::new().as_ref())
        .await
        .unwrap_err();
    assert!(
        matches!(error, IdenticalBuildError::WaitTimedOut { deployment_id, .. } if deployment_id == follower.deployment_id),
        "{error:?}"
    );
    assert!(error.to_string().contains("parallel"), "{error}");
    assert_eq!(f.state(follower.deployment_id).await.0, "timed_out");
}

#[tokio::test(flavor = "multi_thread")]
async fn waiting_builds_take_turns_oldest_first() {
    let Some(f) = fixture().await else { return };
    let (a, b, c) = (
        f.project("a").await,
        f.project("b").await,
        f.project("c").await,
    );
    let leader = f.ticket(a, IdenticalBuildMode::Queue, "fp-1").await;
    let second = f.ticket(b, IdenticalBuildMode::Queue, "fp-1").await;
    let third = f.ticket(c, IdenticalBuildMode::Queue, "fp-1").await;
    let lease = f.build(&leader).await;
    let (second_task, _) = f.wait_in_line(&second).await;
    let (third_task, third_events) = f.wait_in_line(&third).await;
    assert!(matches!(
        third_events.events()[0],
        AdmissionEvent::Waiting { ahead: 1, .. }
    ));

    lease.succeed(None).await.unwrap();
    let second_lease = match finished(second_task).await.unwrap() {
        Admission::Build(lease) => lease,
        other => panic!("the oldest waiting build goes next, got {other:?}"),
    };
    tokio::time::sleep(fast().poll_interval * 5).await;
    assert!(
        !third_task.is_finished(),
        "the third build waits for the second"
    );
    second_lease.succeed(None).await.unwrap();
    assert!(matches!(
        finished(third_task).await.unwrap(),
        Admission::Build(_)
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_image_older_than_the_reuse_window_is_built_again() {
    let Some(f) = fixture().await else { return };
    let (a, b) = (f.project("a").await, f.project("b").await);
    let leader = f.ticket(a, IdenticalBuildMode::Share, "fp-1").await;
    f.build(&leader)
        .await
        .succeed(Some(&image("a:latest")))
        .await
        .unwrap();
    f.db.execute(statement(
        "UPDATE identical_builds SET finished_at = now() - interval '2 hours' WHERE deployment_id = $1",
        vec![leader.deployment_id.into()],
    ))
    .await
    .unwrap();
    let later = f.ticket(b, IdenticalBuildMode::Share, "fp-1").await;
    assert!(matches!(
        f.coordinator
            .admit(&later, Recorder::new().as_ref())
            .await
            .unwrap(),
        Admission::Build(_)
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_retried_build_job_takes_over_its_own_row() {
    let Some(f) = fixture().await else { return };
    let a = f.project("a").await;
    let ticket = f.ticket(a, IdenticalBuildMode::Queue, "fp-1").await;
    let lease = f.build(&ticket).await;
    let first_id = lease.build_id();
    // The process restarts mid-build and the job runs again: the old lease
    // is gone without a word, and the retry must not wait for itself.
    std::mem::forget(lease);
    let retry = f.build(&ticket).await;
    assert_eq!(retry.build_id(), first_id);
    retry.succeed(None).await.unwrap();
    assert_eq!(f.state(ticket.deployment_id).await.0, "succeeded");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_access_boundary_lists_the_project_teams() {
    let Some(f) = fixture().await else { return };
    let a = f.project("a").await;
    assert_eq!(
        access_boundary(f.db.as_ref(), a).await.unwrap(),
        "unrestricted"
    );
    assert_eq!(boundary_label(&[2, 9]), "teams:2,9");
}
