// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use temps_docker_util::image_removal::ImageRemovalStatus;

fn image(id: &str, project: Option<&str>, created: i64) -> OwnedImage {
    let mut labels = HashMap::from([(IMAGE_INSTANCE_LABEL.to_string(), "install-a".to_string())]);
    if let Some(project) = project {
        labels.insert(IMAGE_PROJECT_LABEL.to_string(), project.to_string());
    }
    labels.insert(
        image_ownership::IMAGE_TAG_LABEL.to_string(),
        format!("{id}:latest"),
    );
    OwnedImage {
        image_id: id.to_string(),
        tags: vec![format!("{id}:latest")],
        labels,
        created,
        in_use_by: None,
    }
}

/// A scripted daemon: lists the `images` labelled with the project asked
/// for, reports `held` as used by a container, records what it was asked
/// to remove, and answers `answer`.
struct ScriptedDaemon {
    images: Vec<OwnedImage>,
    held: Vec<&'static str>,
    answer: fn(&ImageRemovalRequest) -> ImageRemovalStatus,
    asked: std::sync::Mutex<Vec<String>>,
    /// The owner and project each listing asked for.
    listed_for: std::sync::Mutex<Vec<(String, i32)>>,
    /// The cursor each listing was given.
    listed_after: std::sync::Mutex<Vec<Option<ListCursor>>>,
    /// The cursor each listing answers with, in turn (then `None`).
    pages: std::sync::Mutex<Vec<Option<ListCursor>>>,
}

impl ScriptedDaemon {
    fn new(images: Vec<OwnedImage>) -> Self {
        Self {
            images,
            held: Vec::new(),
            answer: |request| ImageRemovalStatus::Removed {
                image_id: request.expected_image_id.clone().unwrap_or_default(),
            },
            asked: Default::default(),
            listed_for: Default::default(),
            listed_after: Default::default(),
            pages: Default::default(),
        }
    }

    fn asked(&self) -> Vec<String> {
        self.asked.lock().expect("asked mutex").clone()
    }

    fn listed_for(&self) -> Vec<(String, i32)> {
        self.listed_for.lock().expect("listed mutex").clone()
    }
}

#[async_trait::async_trait]
impl OwnedImageDaemon for ScriptedDaemon {
    async fn list_project_images(
        &self,
        owner: &str,
        project_id: i32,
        after: Option<&ListCursor>,
    ) -> Result<LabelledImages, OrphanSweepError> {
        self.listed_for
            .lock()
            .expect("listed mutex")
            .push((owner.to_string(), project_id));
        self.listed_after
            .lock()
            .expect("after mutex")
            .push(after.cloned());
        let mut pages = self.pages.lock().expect("pages mutex");
        let next = if pages.is_empty() {
            None
        } else {
            pages.remove(0)
        };
        let project = project_id.to_string();
        Ok(LabelledImages {
            images: self
                .images
                .iter()
                .filter(|image| image.labels.get(IMAGE_PROJECT_LABEL) == Some(&project))
                .cloned()
                .collect(),
            unlisted: usize::from(next.is_some()),
            next,
        })
    }

    async fn container_using_image(
        &self,
        image: &OwnedImage,
    ) -> Result<Option<String>, OrphanSweepError> {
        let image_id = image.image_id.as_str();
        Ok(self
            .held
            .contains(&image_id)
            .then(|| format!("holder-of-{image_id}")))
    }

    async fn remove_unused_images(
        &self,
        requests: &[ImageRemovalRequest],
    ) -> Result<Vec<ImageRemovalOutcome>, OrphanSweepError> {
        self.asked
            .lock()
            .expect("asked mutex")
            .extend(requests.iter().map(|r| r.reference.clone()));
        Ok(requests
            .iter()
            .map(|request| ImageRemovalOutcome {
                reference: request.reference.clone(),
                status: (self.answer)(request),
            })
            .collect())
    }
}

fn rows(
    column: &str,
    values: &[sea_orm::Value],
) -> Vec<std::collections::BTreeMap<String, sea_orm::Value>> {
    values
        .iter()
        .map(|value| std::collections::BTreeMap::from([(column.to_string(), value.clone())]))
        .collect()
}

/// The references other projects still hold, for one project's images.
fn references_db(named_elsewhere: &[&str]) -> sea_orm::DatabaseConnection {
    use sea_orm::{MockDatabase, Value};
    let named: Vec<Value> = named_elsewhere
        .iter()
        .map(|reference| Value::from(reference.to_string()))
        .collect();
    MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([rows("reference", &named)])
        .into_connection()
}

#[tokio::test]
async fn a_sweep_removes_a_deleted_project_s_images_within_the_limit() {
    let mut daemon = ScriptedDaemon::new(vec![
        image("live", Some("1"), 10),
        image("gone-b", Some("2"), 30),
        image("gone-a", Some("2"), 20),
        image("gone-c", Some("2"), 40),
    ]);
    daemon.answer = |request| {
        if request.reference == "gone-b:latest" {
            ImageRemovalStatus::Refused {
                image_id: "gone-b".to_string(),
                reason: "conflict".to_string(),
            }
        } else {
            ImageRemovalStatus::Removed {
                image_id: request.reference.clone(),
            }
        }
    };
    let db = references_db(&[]);

    let sweep = sweep_deleted_project_images(&db, &daemon, "install-a", 2, None, 2)
        .await
        .expect("sweep");

    assert_eq!(
        daemon.listed_for(),
        [("install-a".to_string(), 2)],
        "only this install's images of the deleted project are listed"
    );
    assert_eq!(
        daemon.asked(),
        ["gone-a:latest", "gone-b:latest"],
        "oldest first, by their one tag, bounded by the limit"
    );
    assert_eq!(
        sweep,
        ProjectSweep {
            listed: 3,
            removed: 1,
            kept: 1,
            failed: 0,
            deferred: 1,
            attempted: 2,
            remaining: 2,
            next: None,
        }
    );
}

/// Images that must stay never take a slot in the batch: a backlog of
/// them cannot starve a newer image that could go.
#[tokio::test]
async fn held_shared_and_retagged_images_do_not_use_up_the_limit() {
    let mut retagged = image("gone-retagged", Some("2"), 25);
    retagged.tags.push("operator-backup:keep".to_string());
    // The operator retagged it and removed the build's tag.
    let mut replaced = image("gone-replaced", Some("2"), 26);
    replaced.tags = vec!["operator-backup:v1".to_string()];
    let mut unrecorded = image("gone-unrecorded", Some("2"), 27);
    unrecorded.labels.remove(image_ownership::IMAGE_TAG_LABEL);
    let mut daemon = ScriptedDaemon::new(vec![
        image("gone-held", Some("2"), 10),
        image("gone-shared", Some("2"), 20),
        retagged,
        replaced,
        unrecorded,
        image("gone-free", Some("2"), 30),
    ]);
    daemon.held = vec!["gone-held"];
    // Another project still deploys the shared image, naming it without
    // its implicit `:latest`.
    let db = references_db(&["gone-shared"]);

    let sweep = sweep_deleted_project_images(&db, &daemon, "install-a", 2, None, 1)
        .await
        .expect("sweep");

    assert_eq!(daemon.asked(), ["gone-free:latest"]);
    assert_eq!(
        sweep,
        ProjectSweep {
            listed: 6,
            removed: 1,
            kept: 5,
            attempted: 1,
            remaining: 5,
            ..Default::default()
        }
    );
}

/// A project with more images than one page: images the limit deferred
/// are retried on the same page when the run made progress; a page whose
/// batch is refused every time (dependent child images, a failing node)
/// moves on instead of pinning the listing there for good.
#[tokio::test]
async fn a_page_is_retried_only_while_its_removals_make_progress() {
    let page_start = ListCursor {
        created: 7,
        image_id: "sha256:07".to_string(),
    };
    let page_end = ListCursor {
        created: 99,
        image_id: "sha256:99".to_string(),
    };
    let pages = || {
        vec![
            image("gone-a", Some("2"), 10),
            image("gone-b", Some("2"), 20),
            image("gone-c", Some("2"), 30),
        ]
    };

    let progressing = ScriptedDaemon::new(pages());
    *progressing.pages.lock().expect("pages") = vec![Some(page_end.clone())];
    let sweep = sweep_deleted_project_images(
        &references_db(&[]),
        &progressing,
        "install-a",
        2,
        Some(&page_start),
        2,
    )
    .await
    .expect("sweep");
    assert_eq!((sweep.removed, sweep.deferred), (2, 1));
    assert_eq!(sweep.next, Some(page_start.clone()), "the same page again");
    assert_eq!(sweep.remaining, 2, "one deferred, one past the page");

    for answer in [
        (|request: &ImageRemovalRequest| ImageRemovalStatus::Refused {
            image_id: request.reference.clone(),
            reason: "image has dependent child images".to_string(),
        }) as fn(&ImageRemovalRequest) -> ImageRemovalStatus,
        |_| ImageRemovalStatus::Failed {
            reason: "agent unreachable".to_string(),
        },
    ] {
        let mut stuck = ScriptedDaemon::new(pages());
        stuck.answer = answer;
        *stuck.pages.lock().expect("pages") = vec![Some(page_end.clone())];
        let sweep = sweep_deleted_project_images(
            &references_db(&[]),
            &stuck,
            "install-a",
            2,
            Some(&page_start),
            2,
        )
        .await
        .expect("sweep");
        assert_eq!((sweep.removed, sweep.deferred), (0, 1));
        assert_eq!(
            sweep.next,
            Some(page_end.clone()),
            "no progress on this page: the listing moves on"
        );
    }
}

#[tokio::test]
async fn a_failed_reference_lookup_removes_nothing() {
    let daemon = ScriptedDaemon::new(vec![image("gone", Some("2"), 1)]);
    let db = sea_orm::MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_errors([sea_orm::DbErr::Custom("connection reset".to_string())])
        .into_connection();
    let result = sweep_deleted_project_images(&db, &daemon, "install-a", 2, None, 10).await;
    assert!(
        matches!(
            result,
            Err(OrphanSweepError::ReferenceLookup { project_id: 2, .. })
        ),
        "{result:?}"
    );
    assert!(daemon.asked().is_empty());
}

/// Reachable nodes with their daemons, and the IDs of unreachable ones.
type ScriptedNodes = (Vec<(i32, std::sync::Arc<ScriptedDaemon>)>, Vec<i32>);

/// Worker nodes as the [`WorkerImageDaemons`] say: reachable ones, and
/// the IDs of those that cannot be asked.
struct ScriptedWorkers(Result<ScriptedNodes, &'static str>);

#[async_trait::async_trait]
impl WorkerImageDaemons for ScriptedWorkers {
    async fn worker_image_daemons(&self) -> Result<WorkerImageDaemonSet, OrphanSweepError> {
        match &self.0 {
            Ok((nodes, unreachable)) => Ok(WorkerImageDaemonSet {
                reachable: nodes
                    .iter()
                    .map(|(node_id, daemon)| WorkerImageDaemon {
                        node_id: *node_id,
                        node_name: format!("worker-{node_id}"),
                        daemon: daemon.clone(),
                    })
                    .collect(),
                unreachable: unreachable.clone(),
            }),
            Err(reason) => Err(OrphanSweepError::Workers {
                source: (*reason).into(),
            }),
        }
    }
}

/// A mock database for one run: the identity row, the recorded deleted
/// projects, those with a row again, then one reference lookup per
/// listing that found images, and an answer for each record settled.
fn run_db(
    deleted: &[i32],
    restored: &[i32],
    reference_lookups: usize,
) -> std::sync::Arc<sea_orm::DatabaseConnection> {
    use sea_orm::{MockDatabase, MockExecResult, Value};
    let deleted_at = chrono::Utc::now();
    let records: Vec<std::collections::BTreeMap<String, Value>> = deleted
        .iter()
        .map(|id| {
            std::collections::BTreeMap::from([
                ("project_id".to_string(), Value::from(*id)),
                ("deleted_at".to_string(), Value::from(deleted_at)),
            ])
        })
        .collect();
    let restored: Vec<Value> = restored.iter().map(|id| Value::from(*id)).collect();
    let mut db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([rows("instance_id", &[Value::from("install-a".to_string())])])
        .append_query_results([records])
        .append_query_results([rows("id", &restored)]);
    for _ in 0..reference_lookups {
        db = db.append_query_results([rows("reference", &[])]);
    }
    let settled = (0..deleted.len()).map(|_| MockExecResult {
        last_insert_id: 0,
        rows_affected: 1,
    });
    std::sync::Arc::new(db.append_exec_results(settled).into_connection())
}

/// The record writes a run sent, as `(statement kind, project IDs)`, from
/// the database the sweeper was given, once the sweeper is dropped.
fn record_writes(db: std::sync::Arc<sea_orm::DatabaseConnection>) -> Vec<(&'static str, Vec<i32>)> {
    let log = std::sync::Arc::try_unwrap(db)
        .expect("the sweeper released the connection")
        .into_transaction_log();
    log.iter()
        .flat_map(|transaction| transaction.statements())
        .filter_map(|statement| {
            let kind = if statement
                .sql
                .starts_with("DELETE FROM deleted_project_images")
            {
                "retire"
            } else if statement.sql.starts_with("UPDATE deleted_project_images") {
                "touch"
            } else {
                return None;
            };
            let ids = match statement.values.as_ref().map(|values| &values.0[0]) {
                Some(sea_orm::Value::Array(_, Some(ids))) => ids
                    .iter()
                    .filter_map(|id| match id {
                        sea_orm::Value::Int(Some(id)) => Some(*id),
                        _ => None,
                    })
                    .collect(),
                other => panic!("project IDs are bound as one array: {other:?}"),
            };
            Some((kind, ids))
        })
        .collect()
}

async fn marked_dir() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().expect("temp dir");
    let marker = image_ownership::install_marker(dir.path(), true)
        .await
        .expect("marker")
        .expect("created");
    (dir, marker)
}

/// The sweep asks each runtime for each deleted project's images, with
/// this install's owner ID (the database ID joined with the data
/// directory's marker), and removes a project's record once no runtime
/// holds any. A process that runs no workloads sweeps its workers only.
#[tokio::test]
async fn the_sweeper_asks_every_worker_for_each_deleted_project() {
    let (dir, marker) = marked_dir().await;
    let worker = std::sync::Arc::new(ScriptedDaemon::new(vec![
        image("gone", Some("2"), 1),
        image("live", Some("1"), 1),
    ]));
    let db = run_db(&[2, 3], &[], 1);
    let sweeper = OrphanImageSweeper::new(db.clone())
        .with_local_daemon(None)
        .with_worker_daemons(std::sync::Arc::new(ScriptedWorkers(Ok((
            vec![(7, worker.clone())],
            Vec::new(),
        )))))
        .with_data_dir(dir.path().to_path_buf());

    let summary = sweeper.run().await;
    drop(sweeper);
    assert_eq!(
        record_writes(db),
        [("retire", vec![2, 3])],
        "both records go in one statement"
    );

    let owner = format!("install-a.{marker}");
    assert_eq!(worker.listed_for(), [(owner.clone(), 2), (owner, 3)]);
    assert_eq!(worker.asked(), ["gone:latest"]);
    assert_eq!(summary.swept.len(), 1);
    assert_eq!(summary.swept[0].0, Some(7));
    assert_eq!(
        summary.swept[0].1,
        OrphanSweepReport {
            projects: 2,
            listed: 1,
            removed: 1,
            ..Default::default()
        }
    );
    assert_eq!(summary.finished, [2, 3]);
    assert!(
        summary.pending.is_empty() && summary.failed.is_empty(),
        "{summary:?}"
    );
}

/// With no deleted project recorded, no daemon is asked anything: the
/// sweep's work follows deletions, not the images on the hosts.
#[tokio::test]
async fn with_no_deleted_projects_no_daemon_is_asked() {
    let (dir, _) = marked_dir().await;
    let local = std::sync::Arc::new(ScriptedDaemon::new(vec![image("gone", Some("2"), 1)]));
    let worker = std::sync::Arc::new(ScriptedDaemon::new(Vec::new()));
    let sweeper = OrphanImageSweeper::new(run_db(&[], &[], 0))
        .with_local_daemon(Some(local.clone()))
        .with_worker_daemons(std::sync::Arc::new(ScriptedWorkers(Ok((
            vec![(7, worker.clone())],
            Vec::new(),
        )))))
        .with_data_dir(dir.path().to_path_buf());

    let summary = sweeper.run().await;

    assert!(local.listed_for().is_empty() && worker.listed_for().is_empty());
    assert!(
        summary.swept.is_empty() && summary.failed.is_empty(),
        "{summary:?}"
    );
}

/// A record stays while the project's images cannot all be accounted
/// for: a node that cannot be asked, or an image that must stay. It is
/// checked again on a later run.
#[tokio::test]
async fn a_record_stays_while_a_node_is_unreachable_or_an_image_stays() {
    let (dir, _) = marked_dir().await;
    let local = std::sync::Arc::new(ScriptedDaemon::new(vec![image("gone", Some("2"), 1)]));
    let sweeper = OrphanImageSweeper::new(run_db(&[2], &[], 1))
        .with_local_daemon(Some(local.clone()))
        .with_worker_daemons(std::sync::Arc::new(ScriptedWorkers(Ok((
            Vec::new(),
            vec![9],
        )))))
        .with_data_dir(dir.path().to_path_buf());
    let summary = sweeper.run().await;
    assert_eq!(
        local.asked(),
        ["gone:latest"],
        "the reachable host is swept"
    );
    assert!(summary.finished.is_empty());
    assert_eq!(summary.pending, [2], "node 9 still has to be checked");

    let mut holding = ScriptedDaemon::new(vec![image("gone", Some("2"), 1)]);
    holding.held = vec!["gone"];
    let holding = std::sync::Arc::new(holding);
    let sweeper = OrphanImageSweeper::new(run_db(&[2], &[], 1))
        .with_local_daemon(Some(holding.clone()))
        .with_data_dir(dir.path().to_path_buf());
    let summary = sweeper.run().await;
    assert!(holding.asked().is_empty());
    assert_eq!(summary.pending, [2], "a container still uses its image");
}

/// A deleted project's ID that has a row again (a restored backup) is a
/// live project: its record is dropped and its images are not touched.
#[tokio::test]
async fn a_restored_project_s_record_is_dropped_without_listing() {
    let (dir, _) = marked_dir().await;
    let local = std::sync::Arc::new(ScriptedDaemon::new(vec![image("back", Some("2"), 1)]));
    let sweeper = OrphanImageSweeper::new(run_db(&[2], &[2], 0))
        .with_local_daemon(Some(local.clone()))
        .with_data_dir(dir.path().to_path_buf());
    let summary = sweeper.run().await;
    assert!(local.listed_for().is_empty());
    assert!(summary.finished.is_empty() && summary.pending.is_empty());
}

/// The per-run limit spans the deleted projects on a runtime: once it is
/// used up, the remaining projects wait for a later run.
#[tokio::test]
async fn the_limit_defers_the_remaining_projects() {
    let (dir, _) = marked_dir().await;
    let local = std::sync::Arc::new(ScriptedDaemon::new(vec![
        image("gone-2", Some("2"), 1),
        image("gone-3", Some("3"), 1),
    ]));
    let db = run_db(&[2, 3], &[], 1);
    let sweeper = OrphanImageSweeper::new(db.clone())
        .with_local_daemon(Some(local.clone()))
        .with_data_dir(dir.path().to_path_buf())
        .with_max_per_run(1);
    let summary = sweeper.run().await;
    drop(sweeper);
    assert_eq!(local.asked(), ["gone-2:latest"]);
    assert_eq!(local.listed_for().len(), 1, "project 3 is not listed");
    assert_eq!(summary.swept[0].1.deferred, 1);
    assert_eq!((summary.finished, summary.pending), (vec![2], vec![3]));
    assert_eq!(
        record_writes(db),
        [("retire", vec![2])],
        "project 3 was never got to: it keeps its place, first in line next run"
    );
}

/// A project that keeps using up the limit goes to the back of the line:
/// the projects behind it are looked at first next run instead of being
/// passed over every night.
#[tokio::test]
async fn a_project_that_uses_up_the_limit_goes_to_the_back_of_the_line() {
    let (dir, _) = marked_dir().await;
    let local = std::sync::Arc::new(ScriptedDaemon::new(vec![
        image("gone-2a", Some("2"), 1),
        image("gone-2b", Some("2"), 2),
        image("gone-3", Some("3"), 1),
    ]));
    let db = run_db(&[2, 3], &[], 1);
    let sweeper = OrphanImageSweeper::new(db.clone())
        .with_local_daemon(Some(local.clone()))
        .with_data_dir(dir.path().to_path_buf())
        .with_max_per_run(1);
    let summary = sweeper.run().await;
    drop(sweeper);
    assert_eq!(local.asked(), ["gone-2a:latest"]);
    assert_eq!(summary.pending, [2, 3]);
    assert_eq!(
        record_writes(db),
        [("touch", vec![2])],
        "only the project looked at is marked checked"
    );
}

/// Nothing left on a runtime retires a record only when the listing
/// started at the oldest image: a listing that started past a cursor did
/// not see the images before it, which may still be there.
#[tokio::test]
async fn a_record_goes_only_after_a_listing_from_the_oldest_image_finds_nothing() {
    let (dir, _) = marked_dir().await;
    let page_end = ListCursor {
        created: 42,
        image_id: "sha256:42".to_string(),
    };
    // The first listing reports one image past its (empty) page; the next
    // one, after that cursor, finds nothing; the third starts over.
    let local = ScriptedDaemon::new(Vec::new());
    *local.pages.lock().expect("pages") = vec![Some(page_end.clone()), None, None];
    let local = std::sync::Arc::new(local);
    let db = {
        use sea_orm::{MockDatabase, MockExecResult, Value};
        let mut db = MockDatabase::new(DatabaseBackend::Postgres);
        for _ in 0..3 {
            db = db
                .append_query_results([rows(
                    "instance_id",
                    &[Value::from("install-a".to_string())],
                )])
                .append_query_results([vec![std::collections::BTreeMap::from([
                    ("project_id".to_string(), Value::from(2)),
                    ("deleted_at".to_string(), Value::from(chrono::Utc::now())),
                ])]])
                .append_query_results([rows("id", &[])])
                .append_exec_results([MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 1,
                }]);
        }
        std::sync::Arc::new(db.into_connection())
    };
    let sweeper = OrphanImageSweeper::new(db)
        .with_local_daemon(Some(local.clone()))
        .with_data_dir(dir.path().to_path_buf());
    let mut summaries = Vec::new();
    for _ in 0..3 {
        summaries.push(sweeper.run().await);
    }
    assert_eq!(
        local.listed_after.lock().expect("after").as_slice(),
        [None, Some(page_end), None]
    );
    assert_eq!(summaries[0].pending, [2], "an image is past the page");
    assert_eq!(
        summaries[1].pending,
        [2],
        "nothing after the cursor is not nothing at all"
    );
    assert_eq!(
        summaries[2].finished,
        [2],
        "from the oldest, nothing is left"
    );
}

/// A project with more images on a runtime than one page is paged
/// through: the next run starts where the last one stopped, and the
/// end of its list starts over.
#[tokio::test]
async fn each_run_resumes_a_project_s_listing_where_the_last_one_stopped() {
    let (dir, _) = marked_dir().await;
    let page_end = ListCursor {
        created: 42,
        image_id: "sha256:42".to_string(),
    };
    let mut local = ScriptedDaemon::new(vec![image("gone", Some("2"), 1)]);
    // Each listing's image is held, so the project stays recorded.
    local.held = vec!["gone"];
    *local.pages.lock().expect("pages") = vec![Some(page_end.clone()), None];
    let local = std::sync::Arc::new(local);
    let db = {
        use sea_orm::{MockDatabase, MockExecResult, Value};
        let mut db = MockDatabase::new(DatabaseBackend::Postgres);
        for _ in 0..3 {
            db = db
                .append_query_results([rows(
                    "instance_id",
                    &[Value::from("install-a".to_string())],
                )])
                .append_query_results([vec![std::collections::BTreeMap::from([
                    ("project_id".to_string(), Value::from(2)),
                    ("deleted_at".to_string(), Value::from(chrono::Utc::now())),
                ])]])
                .append_query_results([rows("id", &[])])
                .append_query_results([rows("reference", &[])])
                .append_exec_results([MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 1,
                }]);
        }
        std::sync::Arc::new(db.into_connection())
    };
    let sweeper = OrphanImageSweeper::new(db)
        .with_local_daemon(Some(local.clone()))
        .with_data_dir(dir.path().to_path_buf());

    for _ in 0..3 {
        sweeper.run().await;
    }

    assert_eq!(
        local.listed_after.lock().expect("after").as_slice(),
        [None, Some(page_end), None],
        "first page, then after it, then over from the oldest"
    );
}

/// If the worker nodes cannot be read, this host is still swept, the
/// failure is reported rather than hidden, and the record stays.
#[tokio::test]
async fn an_unreadable_worker_list_does_not_stop_this_host_s_sweep() {
    let (dir, _) = marked_dir().await;
    let local = std::sync::Arc::new(ScriptedDaemon::new(vec![image("gone", Some("2"), 1)]));
    let sweeper = OrphanImageSweeper::new(run_db(&[2], &[], 1))
        .with_local_daemon(Some(local.clone()))
        .with_worker_daemons(std::sync::Arc::new(ScriptedWorkers(Err(
            "nodes table unreadable",
        ))))
        .with_data_dir(dir.path().to_path_buf());

    let summary = sweeper.run().await;

    assert_eq!(local.asked(), ["gone:latest"]);
    assert_eq!(summary.failed.len(), 1);
    assert!(
        summary.failed[0].1.contains("nodes table unreadable"),
        "{summary:?}"
    );
    assert_eq!(summary.pending, [2]);
}

/// Without the data directory's marker, no image can carry this
/// install's owner ID: nothing is listed, let alone removed. A copied
/// database in another install lands here until it builds.
#[tokio::test]
async fn without_the_install_marker_nothing_is_listed_or_removed() {
    let dir = tempfile::tempdir().expect("temp dir");
    let local = std::sync::Arc::new(ScriptedDaemon::new(vec![image("gone", Some("2"), 1)]));
    let identity_only = std::sync::Arc::new(
        sea_orm::MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([rows(
                "instance_id",
                &[sea_orm::Value::from("install-a".to_string())],
            )])
            .into_connection(),
    );
    let sweeper = OrphanImageSweeper::new(identity_only)
        .with_local_daemon(Some(local.clone()))
        .with_data_dir(dir.path().to_path_buf());
    let summary = sweeper.run().await;
    assert!(
        summary.swept.is_empty() && summary.failed.is_empty(),
        "{summary:?}"
    );
    assert!(local.listed_for().is_empty());

    // Without a data directory at all, not even the database is read.
    let unconfigured = OrphanImageSweeper::new(std::sync::Arc::new(
        sea_orm::MockDatabase::new(DatabaseBackend::Postgres).into_connection(),
    ))
    .with_local_daemon(Some(local.clone()));
    let summary = unconfigured.run().await;
    assert!(summary.swept.is_empty() && summary.failed.is_empty());
    assert!(local.listed_for().is_empty());
}

#[tokio::test]
async fn a_failed_record_read_asks_no_daemon() {
    let (dir, _) = marked_dir().await;
    let local = std::sync::Arc::new(ScriptedDaemon::new(vec![image("gone", Some("2"), 1)]));
    let db = std::sync::Arc::new(
        sea_orm::MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([rows(
                "instance_id",
                &[sea_orm::Value::from("install-a".to_string())],
            )])
            .append_query_errors([sea_orm::DbErr::Custom("connection reset".to_string())])
            .into_connection(),
    );
    let sweeper = OrphanImageSweeper::new(db)
        .with_local_daemon(Some(local.clone()))
        .with_data_dir(dir.path().to_path_buf());
    let summary = sweeper.run().await;
    assert!(local.listed_for().is_empty());
    assert_eq!(summary.failed.len(), 1, "{summary:?}");
    assert!(
        summary.failed[0].1.contains("deleted projects"),
        "{summary:?}"
    );
}

async fn build_labelled_image(
    docker: &bollard::Docker,
    tag: &str,
    labels: &HashMap<String, String>,
) -> Result<(), Box<dyn std::error::Error>> {
    use futures_util::StreamExt as _;
    let dockerfile = format!(
        "FROM busybox:1.36\nENV TEMPS_TEST_MARKER={}\n",
        uuid::Uuid::new_v4()
    );
    let mut header = tar::Header::new_gnu();
    header.set_size(dockerfile.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    let mut archive = tar::Builder::new(Vec::new());
    archive.append_data(&mut header, "Dockerfile", dockerfile.as_bytes())?;
    let context = archive.into_inner()?;
    let mut build = docker.build_image(
        bollard::query_parameters::BuildImageOptionsBuilder::default()
            .t(tag)
            .labels(labels)
            .rm(true)
            .build(),
        None,
        Some(bollard::body_full(bytes::Bytes::from(context))),
    );
    while let Some(step) = build.next().await {
        step?;
    }
    Ok(())
}

async fn insert_project(
    db: &sea_orm::DatabaseConnection,
    slug: &str,
) -> Result<temps_entities::projects::Model, sea_orm::DbErr> {
    use sea_orm::{ActiveModelTrait, ActiveValue::Set};
    let now = chrono::Utc::now();
    temps_entities::projects::ActiveModel {
        name: Set(format!("Project {slug}")),
        slug: Set(slug.to_string()),
        repo_owner: Set("test-owner".to_string()),
        repo_name: Set("test-repo".to_string()),
        preset: Set(temps_entities::preset::Preset::Dockerfile),
        directory: Set("/".to_string()),
        main_branch: Set("main".to_string()),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(db)
    .await
}

/// Against a real daemon and database: deleting a project's row records
/// it, and the sweep then removes its unused labelled image, while an
/// image of that project a container was created from, an image of a
/// live project, an image another install labelled, and an unlabelled
/// image all survive. The record stays while the held image does, and
/// goes once it is gone. Skips when Docker or Postgres is unavailable.
#[tokio::test]
async fn the_sweep_removes_only_unused_images_of_deleted_projects_on_a_real_daemon(
) -> Result<(), Box<dyn std::error::Error>> {
    use bollard::query_parameters::{
        CreateContainerOptionsBuilder, CreateImageOptionsBuilder, RemoveContainerOptions,
        RemoveImageOptions,
    };
    use futures_util::StreamExt as _;
    use sea_orm::EntityTrait;

    let Ok(docker) = bollard::Docker::connect_with_unix_defaults() else {
        println!("Docker not available, skipping");
        return Ok(());
    };
    if docker.ping().await.is_err() {
        println!("Docker not available, skipping");
        return Ok(());
    }
    let test_db = match temps_test_utils::TestDatabase::with_migrations().await {
        Ok(db) => db,
        Err(error) => {
            println!("Database unavailable, skipping: {error}");
            return Ok(());
        }
    };
    let db = test_db.connection_arc();
    let mut pull = docker.create_image(
        Some(
            CreateImageOptionsBuilder::default()
                .from_image("busybox")
                .tag("1.36")
                .build(),
        ),
        None,
        None,
    );
    while let Some(step) = pull.next().await {
        if let Err(error) = step {
            println!("Could not pull busybox, skipping: {error}");
            return Ok(());
        }
    }

    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let live = insert_project(db.as_ref(), &format!("live-{nonce}")).await?;
    let doomed = insert_project(db.as_ref(), &format!("doomed-{nonce}")).await?;
    let data_dir = tempfile::tempdir()?;
    let instance = image_ownership::ensure_image_owner_id(db.as_ref(), data_dir.path()).await?;
    let tag = |name: &str| format!("temps-orphan-test-{name}-{nonce}:latest");
    let orphan = tag("orphan");
    let held = tag("held");
    let live_image = tag("live");
    let foreign = tag("foreign");
    let unlabelled = tag("unlabelled");
    let holder = format!("temps-orphan-test-holder-{nonce}");
    let images = [
        (
            &orphan,
            image_ownership::labels_for_tag(
                &image_ownership::image_ownership_labels(&instance, doomed.id, 1),
                &orphan,
            ),
        ),
        (
            &held,
            image_ownership::labels_for_tag(
                &image_ownership::image_ownership_labels(&instance, doomed.id, 2),
                &held,
            ),
        ),
        (
            &live_image,
            image_ownership::labels_for_tag(
                &image_ownership::image_ownership_labels(&instance, live.id, 3),
                &live_image,
            ),
        ),
        (
            &foreign,
            image_ownership::image_ownership_labels("another-install", doomed.id, 4),
        ),
        (&unlabelled, HashMap::new()),
    ];

    let mut prepared = Ok(());
    for (image_tag, labels) in &images {
        if let Err(error) = build_labelled_image(&docker, image_tag, labels).await {
            prepared = Err(format!("build {image_tag}: {error}"));
            break;
        }
    }
    if prepared.is_ok() {
        prepared = docker
            .create_container(
                Some(CreateContainerOptionsBuilder::new().name(&holder).build()),
                bollard::models::ContainerCreateBody {
                    image: Some(held.clone()),
                    cmd: Some(vec!["true".to_string()]),
                    ..Default::default()
                },
            )
            .await
            .map(|_| ())
            .map_err(|error| format!("create holder: {error}"));
    }
    let sweeper = OrphanImageSweeper::new(db.clone())
        .with_data_dir(data_dir.path().to_path_buf())
        .with_max_per_run(10);
    // Every step's error is kept, not returned, so the Docker resources
    // below are removed whatever fails.
    let mut first = None;
    let mut second = None;
    if prepared.is_ok() {
        match temps_entities::projects::Entity::delete_by_id(doomed.id)
            .exec(db.as_ref())
            .await
        {
            Ok(_) => first = Some(sweeper.run().await),
            Err(error) => prepared = Err(format!("delete project {}: {error}", doomed.id)),
        }
    }
    let present = |image_tag: String| {
        let docker = docker.clone();
        async move { docker.inspect_image(&image_tag).await.is_ok() }
    };
    let orphan_left = present(orphan.clone()).await;
    let held_left = present(held.clone()).await;
    let live_left = present(live_image.clone()).await;
    let foreign_left = present(foreign.clone()).await;
    let unlabelled_left = present(unlabelled.clone()).await;

    // Once the container is gone, the next run removes the held image
    // and the record with it.
    let _ = docker
        .remove_container(
            &holder,
            Some(RemoveContainerOptions {
                force: true,
                ..Default::default()
            }),
        )
        .await;
    if prepared.is_ok() {
        second = Some(sweeper.run().await);
    }
    let held_left_after = present(held.clone()).await;

    // Remove only what this test created, before asserting.
    for (image_tag, _) in &images {
        let _ = docker
            .remove_image(
                image_tag,
                Some(RemoveImageOptions {
                    force: true,
                    ..Default::default()
                }),
                None,
            )
            .await;
    }

    // Docker and Postgres answered and busybox was pulled: a failure from
    // here on is the test's, not a missing service, so it fails.
    if let Err(reason) = prepared {
        return Err(format!("preparing the sweep test failed: {reason}").into());
    }
    let first = first.expect("the first run");
    assert!(first.failed.is_empty(), "{first:?}");
    let report = &first.swept[0].1;
    assert_eq!(
        report.listed, 2,
        "only this install's images of the deleted project are listed: {report:?}"
    );
    assert_eq!((report.removed, report.kept), (1, 1), "{report:?}");
    assert_eq!(
        first.pending,
        [doomed.id],
        "the held image keeps the record"
    );
    assert!(
        !orphan_left,
        "the unused image of the deleted project must be gone"
    );
    assert!(held_left, "an image a container was created from must stay");
    assert!(live_left, "an image of a live project must stay");
    assert!(foreign_left, "another install's image must stay");
    assert!(unlabelled_left, "an image without Temps labels must stay");

    let second = second.expect("the second run");
    assert!(!held_left_after, "{second:?}");
    assert_eq!(second.finished, [doomed.id], "{second:?}");
    let remaining = temps_entities::deleted_project_images::Entity::find_by_id(doomed.id)
        .one(db.as_ref())
        .await?;
    assert!(
        remaining.is_none(),
        "the record is gone once nothing is left"
    );
    Ok(())
}
