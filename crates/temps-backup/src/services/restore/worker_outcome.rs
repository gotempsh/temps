// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! How a run finishes from its worker restore's outcome, whichever engine
//! ran it (PostgreSQL WAL-G, MongoDB, Redis, MariaDB).
//!
//! A worker restore that began writing to its target runs on its node past
//! the request's deadline, until it has started the restored data or put
//! the original back. The control plane's request can time out, or its
//! connection drop, before then. The run must not be finished meanwhile:
//! that would release the service to another destructive restore while the
//! node still swaps its data, and skip the credential reconcile a later
//! success needs. So every worker restore is started under its run's
//! [`operation_id`]; when its request goes unanswered the run stays
//! `running` and its node is asked for the outcome until it reports one.
//! Only then is the run settled: a completed restore patches the target's
//! stored login to the restored one, as an answered one does.

use std::future::Future;
use std::time::Duration;

use sea_orm::DatabaseConnection;
use temps_providers::remote_service_client::{
    WorkerRestoreEngine, WorkerRestoreProgress, WorkerRestoreResult,
};
use temps_providers::services::ExternalServiceError;
use tracing::{info, warn};

use super::*;

/// How a restore whose request went unanswered is waited for: its node is
/// asked every `interval` where the restore is. The wait gives up when the
/// node stays unreachable for `unreachable_for`, or after `max_wait` (the
/// node keeps an outcome for a day).
#[derive(Debug, Clone, Copy)]
pub(super) struct OutcomeWait {
    pub(super) interval: Duration,
    pub(super) unreachable_for: Duration,
    pub(super) max_wait: Duration,
}

const OUTCOME_WAIT: OutcomeWait = OutcomeWait {
    interval: Duration::from_secs(15),
    unreachable_for: Duration::from_secs(2 * 3600),
    max_wait: Duration::from_secs(24 * 3600),
};

/// The name a restore run's worker restore is started under on its node, to
/// ask for its outcome by.
pub(super) fn operation_id(run_id: i32) -> String {
    format!("restore-run-{run_id}")
}

/// The run a worker restore belongs to, which its outcome finishes.
pub(super) struct WorkerRestoreRun<'a> {
    pub(super) db: &'a Arc<DatabaseConnection>,
    pub(super) run_id: i32,
    pub(super) node_id: i32,
    pub(super) target_id: i32,
    pub(super) engine: WorkerRestoreEngine,
    /// The login the restored data answers to, which the target's stored
    /// configuration (encrypted with the key given) is patched to once the
    /// restore completed; `None` when the restore keeps the target's.
    pub(super) reconcile: Option<(
        &'a Arc<temps_core::EncryptionService>,
        &'a RestoredServiceCredentials,
    )>,
}

impl WorkerRestoreRun<'_> {
    /// Finish the run from what its node answered to the restore request
    /// (started under [`operation_id`]). An unanswered request is followed
    /// by asking the node until it reports the restore's outcome.
    pub(super) async fn finish<T: Into<WorkerRestoreResult>>(
        &self,
        mgr: &Arc<ExternalServiceManager>,
        answered: Result<T, ExternalServiceError>,
    ) -> Result<Option<i32>, RestoreError> {
        let operation_id = operation_id(self.run_id);
        self.finish_with(&OUTCOME_WAIT, answered, || {
            mgr.worker_restore_outcome(self.target_id, self.engine, &operation_id)
        })
        .await
    }

    /// [`Self::finish`], asking the node with `ask` as `wait` says.
    pub(super) async fn finish_with<T, Ask, AskFut>(
        &self,
        wait: &OutcomeWait,
        answered: Result<T, ExternalServiceError>,
        ask: Ask,
    ) -> Result<Option<i32>, RestoreError>
    where
        T: Into<WorkerRestoreResult>,
        Ask: FnMut() -> AskFut,
        AskFut: Future<Output = Result<WorkerRestoreProgress, ExternalServiceError>>,
    {
        let (run_id, node_id) = (self.run_id, self.node_id);
        let outcome = match answered {
            Ok(outcome) => outcome.into(),
            // The request timed out or its connection dropped: the node may
            // still be restoring, past its deadline once it began writing.
            // The run stays active (and keeps other restores of the service
            // out) until the node says how it ended.
            Err(ExternalServiceError::AgentUnanswered { detail, .. }) => {
                warn!(
                    run_id,
                    service_id = self.target_id,
                    node_id,
                    engine = self.engine.label(),
                    %detail,
                    "the node did not answer a restore; waiting for its outcome there"
                );
                await_unanswered_restore(wait, self.engine, run_id, node_id, ask).await?
            }
            Err(e) => {
                return Err(RestoreError::ExternalService {
                    reason: format!("restore run {run_id} on node {node_id}: {e}"),
                })
            }
        };
        self.settle(outcome).await
    }

    /// What the run makes of its node's restore outcome: a completed restore
    /// patches the target's stored login to the restored one and enters
    /// verification; a refused or failed one fails the run.
    async fn settle(&self, outcome: WorkerRestoreResult) -> Result<Option<i32>, RestoreError> {
        let (run_id, node_id) = (self.run_id, self.node_id);
        match outcome {
            WorkerRestoreResult::Completed => {}
            WorkerRestoreResult::Refused { reason } => {
                return Err(RestoreError::Validation {
                    message: format!(
                        "restore run {run_id} on node {node_id} was refused: {reason}"
                    ),
                })
            }
            WorkerRestoreResult::Failed { reason } => {
                return Err(RestoreError::ExternalService {
                    reason: format!("restore run {run_id} on node {node_id}: {reason}"),
                })
            }
        }
        if let Some((enc, credentials)) = self.reconcile {
            reconcile_restored_credentials(self.db, enc, self.target_id, Some(credentials)).await;
        }
        if let Err(e) = update_phase(self.db.as_ref(), run_id, "verify").await {
            warn!("Restore run {run_id}: recording the verify phase failed: {e}");
        }
        Ok(None)
    }
}

/// Wait for the outcome of an `engine` restore whose request went
/// unanswered, asking its node with `ask` as `wait` says. Nothing is written
/// meanwhile: the run stays `running`, so no other destructive restore of the
/// service starts while the node may still be swapping data.
///
/// A restore that ended before it changed the service fails cleanly; one
/// that ended without an outcome after it began writing, that the node no
/// longer knows, or whose node stays unreachable, fails saying the service
/// must be checked before it is restored again.
async fn await_unanswered_restore<Ask, AskFut>(
    wait: &OutcomeWait,
    engine: WorkerRestoreEngine,
    run_id: i32,
    node_id: i32,
    mut ask: Ask,
) -> Result<WorkerRestoreResult, RestoreError>
where
    Ask: FnMut() -> AskFut,
    AskFut: Future<Output = Result<WorkerRestoreProgress, ExternalServiceError>>,
{
    let label = engine.label();
    let started = tokio::time::Instant::now();
    let mut unreachable_since: Option<tokio::time::Instant> = None;
    let mut last_error = String::new();
    loop {
        match ask().await {
            Ok(WorkerRestoreProgress::Finished { outcome }) => {
                info!(
                    run_id,
                    node_id,
                    engine = label,
                    ?outcome,
                    "the node reported the outcome of an unanswered restore"
                );
                return Ok(outcome);
            }
            Ok(WorkerRestoreProgress::Ended {
                writes_began: false,
                reason,
            }) => {
                return Err(RestoreError::ExternalService {
                    reason: format!(
                        "restore run {run_id} on node {node_id} ({label}) stopped before it \
                         changed the database, which is unchanged: {reason}"
                    ),
                })
            }
            Ok(WorkerRestoreProgress::Ended {
                writes_began: true,
                reason,
            }) => {
                return Err(RestoreError::ExternalService {
                    reason: format!(
                        "restore run {run_id} on node {node_id} ({label}) stopped after it began \
                         writing to the database, without an outcome: {reason}. The database \
                         may be partially restored; check it before restoring again"
                    ),
                })
            }
            Ok(WorkerRestoreProgress::Unknown) => {
                return Err(RestoreError::ExternalService {
                    reason: format!(
                        "restore run {run_id}: node {node_id} did not answer the {label} and has \
                         no record of it (its agent may have restarted). The database may have \
                         been changed; check it before restoring again"
                    ),
                })
            }
            Ok(WorkerRestoreProgress::Running { writes_began }) => {
                unreachable_since = None;
                tracing::debug!(
                    run_id,
                    node_id,
                    engine = label,
                    writes_began,
                    "an unanswered restore is still running"
                );
            }
            Err(error) => {
                let since = *unreachable_since.get_or_insert_with(tokio::time::Instant::now);
                last_error = error.to_string();
                warn!(
                    run_id,
                    node_id,
                    engine = label,
                    error = %last_error,
                    "could not ask node for the outcome of an unanswered restore; retrying"
                );
                if since.elapsed() >= wait.unreachable_for {
                    return Err(RestoreError::ExternalService {
                        reason: format!(
                            "restore run {run_id}: node {node_id} did not answer the {label}, and \
                             could not be asked for its outcome for {}s ({last_error}). The \
                             restore may still be running there and the database may be \
                             partially restored; check it before restoring again",
                            wait.unreachable_for.as_secs()
                        ),
                    });
                }
            }
        }
        if started.elapsed() >= wait.max_wait {
            return Err(RestoreError::ExternalService {
                reason: format!(
                    "restore run {run_id}: node {node_id} reported no outcome for the {label} \
                     within {}s{}. The restore may still be running there; check the database \
                     before restoring again",
                    wait.max_wait.as_secs(),
                    if last_error.is_empty() {
                        String::new()
                    } else {
                        format!(" (last error: {last_error})")
                    }
                ),
            });
        }
        tokio::time::sleep(wait.interval).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::EntityTrait;

    const PG: WorkerRestoreEngine = WorkerRestoreEngine::PostgresWalg;

    const FAST: OutcomeWait = OutcomeWait {
        interval: Duration::from_millis(5),
        unreachable_for: Duration::from_millis(60),
        max_wait: Duration::from_secs(30),
    };

    /// The node's answers to the outcome asks, in order; the last repeats.
    fn answers(
        answers: Vec<Result<WorkerRestoreProgress, ExternalServiceError>>,
    ) -> impl FnMut() -> std::future::Ready<Result<WorkerRestoreProgress, ExternalServiceError>>
    {
        let answers = std::sync::Mutex::new(std::collections::VecDeque::from(answers));
        move || {
            let mut answers = answers.lock().unwrap();
            let answer = if answers.len() > 1 {
                answers.pop_front().unwrap()
            } else {
                match answers.front().unwrap() {
                    Ok(progress) => Ok(progress.clone()),
                    Err(_) => Err(unreachable()),
                }
            };
            std::future::ready(answer)
        }
    }

    fn unreachable() -> ExternalServiceError {
        ExternalServiceError::AgentUnanswered {
            node: "worker-2".to_string(),
            route: "/agent/services/restore/outcome".to_string(),
            detail: "operation timed out".to_string(),
        }
    }

    /// The restore's own request went unanswered before it began writing:
    /// the node's agent stopped it at its deadline, and the run fails at
    /// once, saying the database is unchanged.
    #[tokio::test]
    async fn an_unanswered_restore_that_never_wrote_fails_cleanly() {
        let error = await_unanswered_restore(
            &FAST,
            PG,
            41,
            2,
            answers(vec![Ok(WorkerRestoreProgress::Ended {
                writes_began: false,
                reason: "WAL-G restore of 'orders' passed its deadline".to_string(),
            })]),
        )
        .await
        .expect_err("ended without an outcome");
        let message = error.to_string();
        assert!(message.contains("restore run 41 on node 2"), "{message}");
        assert!(message.contains("which is unchanged"), "{message}");
        assert!(message.contains("passed its deadline"), "{message}");
    }

    /// Running answers are waited through, an unreachable node in between
    /// is retried, and the node's final outcome is what the run gets.
    #[tokio::test]
    async fn an_unanswered_restore_is_waited_for_until_the_node_reports_it() {
        let outcome = await_unanswered_restore(
            &FAST,
            PG,
            41,
            2,
            answers(vec![
                Ok(WorkerRestoreProgress::Running { writes_began: true }),
                Err(unreachable()),
                Ok(WorkerRestoreProgress::Running { writes_began: true }),
                Ok(WorkerRestoreProgress::Finished {
                    outcome: WorkerRestoreResult::Failed {
                        reason: "the original database was put back unchanged".to_string(),
                    },
                }),
            ]),
        )
        .await
        .unwrap();
        assert_eq!(
            outcome,
            WorkerRestoreResult::Failed {
                reason: "the original database was put back unchanged".to_string()
            }
        );
    }

    /// Ended after writing, unknown to the node, or a node unreachable past
    /// the limit: each fails, saying the database must be checked.
    #[tokio::test]
    async fn an_unanswered_restore_without_a_known_outcome_says_to_check_the_database() {
        let cases = [
            (
                vec![Ok(WorkerRestoreProgress::Ended {
                    writes_began: true,
                    reason: "the engine stopped".to_string(),
                })],
                "may be partially restored",
            ),
            (
                vec![Ok(WorkerRestoreProgress::Unknown)],
                "has no record of it",
            ),
            (
                vec![
                    Ok(WorkerRestoreProgress::Running { writes_began: true }),
                    Err(unreachable()),
                ],
                "could not be asked for its outcome",
            ),
        ];
        for (answered, expected) in cases {
            let message = await_unanswered_restore(&FAST, PG, 41, 2, answers(answered))
                .await
                .expect_err("no known outcome")
                .to_string();
            assert!(message.contains(expected), "{expected}: {message}");
            assert!(message.contains("before restoring again"), "{message}");
        }
    }

    /// A node that keeps reporting the restore as running is given up on
    /// after the wait's limit, not waited for forever.
    #[tokio::test]
    async fn an_unanswered_restore_is_not_waited_for_past_the_limit() {
        let wait = OutcomeWait {
            max_wait: Duration::from_millis(40),
            ..FAST
        };
        let message = await_unanswered_restore(
            &wait,
            PG,
            41,
            2,
            answers(vec![Ok(WorkerRestoreProgress::Running {
                writes_began: true,
            })]),
        )
        .await
        .expect_err("past the limit")
        .to_string();
        assert!(message.contains("reported no outcome"), "{message}");
    }

    struct RunFixture {
        _test_db: temps_test_utils::TestDatabase,
        db: Arc<DatabaseConnection>,
        enc: Arc<temps_core::EncryptionService>,
        user_id: i32,
        service_id: i32,
        backup_id: i32,
    }

    /// A `service_type` service with its stored login, a backup of another
    /// service and the user who restores it, in a real database (settling a
    /// run reads neither the node nor the backup's data). `None` without one.
    async fn run_fixture(service_type: &str) -> Option<RunFixture> {
        use sea_orm::{ActiveModelTrait, ActiveValue::Set, NotSet};
        if bollard::Docker::connect_with_local_defaults().is_err() {
            println!("Docker not available, skipping");
            return None;
        }
        let test_db = match temps_test_utils::TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                println!("TestDatabase unavailable, skipping: {e}");
                return None;
            }
        };
        let db = test_db.db.clone();
        let enc = Arc::new(temps_core::EncryptionService::new_from_password(
            "synthetic-restore-test-key",
        ));
        let user = temps_entities::users::ActiveModel {
            name: Set("Restore Tester".to_string()),
            email: Set("restore-tester@example.com".to_string()),
            password_hash: Set(Some("hash".to_string())),
            email_verified: Set(true),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("insert user");
        let source = temps_entities::s3_sources::ActiveModel {
            name: Set("restore-source".to_string()),
            bucket_name: Set("backups".to_string()),
            bucket_path: Set("/".to_string()),
            access_key_id: Set(String::new()),
            secret_key: Set(String::new()),
            region: Set("us-east-1".to_string()),
            force_path_style: Set(Some(true)),
            is_default: Set(true),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("insert S3 source");
        let config = serde_json::json!({
            "host": "localhost",
            "port": "5432",
            "username": "orders_owner",
            "password": "target-password",
        })
        .to_string();
        let service = temps_entities::external_services::ActiveModel {
            name: Set("orders".to_string()),
            service_type: Set(service_type.to_string()),
            status: Set("running".to_string()),
            topology: Set("standalone".to_string()),
            config: Set(Some(enc.encrypt_string(&config).unwrap())),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("insert service");
        let backup = temps_entities::backups::ActiveModel {
            id: NotSet,
            name: Set("nightly".to_string()),
            backup_id: Set(uuid::Uuid::new_v4().to_string()),
            schedule_id: Set(None),
            schedule_run_id: Set(None),
            backup_type: Set("full".to_string()),
            state: Set("completed".to_string()),
            started_at: Set(Utc::now()),
            finished_at: Set(Some(Utc::now())),
            s3_source_id: Set(source.id),
            s3_location: Set(format!(
                "s3://backups/external_services/{service_type}/billing/walg"
            )),
            compression_type: Set("none".to_string()),
            created_by: Set(user.id),
            tags: Set("[]".to_string()),
            size_bytes: Set(None),
            file_count: Set(None),
            error_message: Set(None),
            expires_at: Set(None),
            checksum: Set(None),
            metadata: Set("{}".to_string()),
        }
        .insert(db.as_ref())
        .await
        .expect("insert backup");
        Some(RunFixture {
            _test_db: test_db,
            db,
            enc,
            user_id: user.id,
            service_id: service.id,
            backup_id: backup.id,
        })
    }

    impl RunFixture {
        async fn start_in_place(
            &self,
        ) -> Result<temps_entities::restore_runs::Model, RestoreError> {
            use sea_orm::{ActiveValue::Set, NotSet};
            insert_restore_run(
                self.db.as_ref(),
                temps_entities::restore_runs::ActiveModel {
                    id: NotSet,
                    source_backup_id: Set(self.backup_id),
                    source_service_id: Set(self.service_id),
                    target_service_id: Set(None),
                    target_service_name: Set(None),
                    mode: Set("in_place".to_string()),
                    status: Set("running".to_string()),
                    phase: Set("restore".to_string()),
                    recovery_target: Set(None),
                    parameter_overrides: Set(serde_json::json!({})),
                    resume_token: Set(None),
                    log_id: Set(uuid::Uuid::new_v4().to_string()),
                    error_message: Set(None),
                    attempt: Set(1),
                    started_at: Set(Some(Utc::now())),
                    finished_at: Set(None),
                    created_by: Set(self.user_id),
                    created_at: NotSet,
                    updated_at: NotSet,
                    cancel_requested_at: Set(None),
                    cancel_requested_by: Set(None),
                },
                Some(self.backup_id),
                self.service_id,
                true,
            )
            .await
        }

        async fn run_status(&self, run_id: i32) -> String {
            temps_entities::restore_runs::Entity::find_by_id(run_id)
                .one(self.db.as_ref())
                .await
                .unwrap()
                .unwrap()
                .status
        }

        async fn stored_login(&self) -> serde_json::Value {
            let service = temps_entities::external_services::Entity::find_by_id(self.service_id)
                .one(self.db.as_ref())
                .await
                .unwrap()
                .unwrap();
            let config = self.enc.decrypt_string(&service.config.unwrap()).unwrap();
            let config: serde_json::Value = serde_json::from_str(&config).unwrap();
            serde_json::json!({
                "username": config["username"],
                "password": config["password"],
            })
        }

        /// Finish `run_id` as an `engine` restore whose request went
        /// unanswered, from the node's answers, in its own task like the
        /// restore's. `origin` is the login of the backup's service, which
        /// the restored data answers to (`None`: the engine keeps the
        /// target's).
        fn finish_unanswered(
            &self,
            engine: WorkerRestoreEngine,
            run_id: i32,
            origin: Option<RestoredServiceCredentials>,
            progress: tokio::sync::watch::Receiver<WorkerRestoreProgress>,
        ) -> tokio::task::JoinHandle<Result<Option<i32>, RestoreError>> {
            let db = Arc::clone(&self.db);
            let enc = Arc::clone(&self.enc);
            let target_id = self.service_id;
            tokio::spawn(async move {
                let run = WorkerRestoreRun {
                    db: &db,
                    run_id,
                    node_id: 2,
                    target_id,
                    engine,
                    reconcile: origin.as_ref().map(|origin| (&enc, origin)),
                };
                run.finish_with(
                    &FAST,
                    Err::<WorkerRestoreResult, _>(ExternalServiceError::AgentUnanswered {
                        node: "worker-2".to_string(),
                        route: "/agent/services/mongodb/restore".to_string(),
                        detail: "operation timed out".to_string(),
                    }),
                    || std::future::ready(Ok(progress.borrow().clone())),
                )
                .await
            })
        }
    }

    /// The backup's service's login, as the restored data of `service_type`
    /// carries it.
    fn origin(service_type: &str) -> Option<RestoredServiceCredentials> {
        RestoredServiceCredentials::from_origin(
            service_type,
            &serde_json::json!({"username": "billing_owner", "password": "origin-password"}),
        )
    }

    /// For every worker engine: while the node still restores (past its
    /// write boundary) after the request went unanswered, the run stays
    /// `running`, a second destructive restore of the service is refused,
    /// and the stored login is untouched. The node's later success
    /// completes the run and runs the credential reconcile (the stored login
    /// becomes the restored one where the engine restores it); the service
    /// is free again after.
    #[tokio::test]
    async fn an_unanswered_restore_that_wrote_holds_the_service_until_it_succeeds() {
        let target_login =
            serde_json::json!({"username": "orders_owner", "password": "target-password"});
        let origin_login =
            serde_json::json!({"username": "billing_owner", "password": "origin-password"});
        // MariaDB's restored catalog carries the password, not the login name.
        let mariadb_login =
            serde_json::json!({"username": "orders_owner", "password": "origin-password"});
        let cases = [
            (PG, "postgres", origin("postgres"), origin_login.clone()),
            (
                WorkerRestoreEngine::Mongodb,
                "mongodb",
                origin("mongodb"),
                origin_login,
            ),
            (
                WorkerRestoreEngine::Mariadb,
                "mariadb",
                origin("mariadb"),
                mariadb_login,
            ),
            // Redis data has no login to reconcile.
            (
                WorkerRestoreEngine::Redis,
                "redis",
                None,
                target_login.clone(),
            ),
        ];
        for (engine, service_type, origin, restored_login) in cases {
            let Some(fixture) = run_fixture(service_type).await else {
                return;
            };
            let run = fixture.start_in_place().await.expect("first restore");
            let (report, progress) =
                tokio::sync::watch::channel(WorkerRestoreProgress::Running { writes_began: true });
            let finishing = fixture.finish_unanswered(engine, run.id, origin, progress);

            tokio::time::sleep(Duration::from_millis(100)).await;
            assert!(
                !finishing.is_finished(),
                "{engine:?}: waits for the node's outcome"
            );
            assert_eq!(fixture.run_status(run.id).await, "running", "{engine:?}");
            match fixture.start_in_place().await {
                Err(RestoreError::RestoreAlreadyActive {
                    service_id,
                    restore_run_id,
                }) => {
                    assert_eq!(service_id, fixture.service_id, "{engine:?}");
                    assert_eq!(restore_run_id, run.id, "{engine:?}");
                }
                other => panic!("{engine:?}: expected RestoreAlreadyActive, got {other:?}"),
            }
            assert_eq!(
                fixture.stored_login().await,
                target_login,
                "{engine:?}: the login is not changed while the outcome is unknown"
            );

            report
                .send(WorkerRestoreProgress::Finished {
                    outcome: WorkerRestoreResult::Completed,
                })
                .unwrap();
            let result = finishing.await.unwrap();
            assert!(result.is_ok(), "{engine:?}: {result:?}");
            persist_terminal_state(fixture.db.as_ref(), run.id, &result, TERMINAL_WRITE_RETRY)
                .await
                .unwrap();
            assert_eq!(fixture.run_status(run.id).await, "completed", "{engine:?}");
            assert_eq!(fixture.stored_login().await, restored_login, "{engine:?}");
            fixture
                .start_in_place()
                .await
                .expect("the service is free once the run completed");
        }
    }

    /// The node's later failure (the original put back) fails the run and
    /// leaves the stored login as it was.
    #[tokio::test]
    async fn an_unanswered_restore_that_wrote_fails_when_the_node_reports_failure() {
        let Some(fixture) = run_fixture("mongodb").await else {
            return;
        };
        let run = fixture.start_in_place().await.expect("first restore");
        let (report, progress) =
            tokio::sync::watch::channel(WorkerRestoreProgress::Running { writes_began: true });
        let finishing = fixture.finish_unanswered(
            WorkerRestoreEngine::Mongodb,
            run.id,
            origin("mongodb"),
            progress,
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!finishing.is_finished(), "waits for the node's outcome");
        assert!(matches!(
            fixture.start_in_place().await,
            Err(RestoreError::RestoreAlreadyActive { .. })
        ));

        report
            .send(WorkerRestoreProgress::Finished {
                outcome: WorkerRestoreResult::Failed {
                    reason: "mongorestore failed; the original data was put back".to_string(),
                },
            })
            .unwrap();
        let result = finishing.await.unwrap();
        let message = result.as_ref().expect_err("the node failed").to_string();
        assert!(
            message.contains("the original data was put back"),
            "{message}"
        );
        persist_terminal_state(fixture.db.as_ref(), run.id, &result, TERMINAL_WRITE_RETRY)
            .await
            .unwrap();
        assert_eq!(fixture.run_status(run.id).await, "failed");
        assert_eq!(
            fixture.stored_login().await,
            serde_json::json!({"username": "orders_owner", "password": "target-password"})
        );
    }

    /// Unanswered before it began writing: the node stopped it at its
    /// deadline, the run fails at once with the service unchanged, and the
    /// service is free for another restore.
    #[tokio::test]
    async fn an_unanswered_restore_before_writes_fails_and_frees_the_service() {
        let Some(fixture) = run_fixture("mariadb").await else {
            return;
        };
        let run = fixture.start_in_place().await.expect("first restore");
        let (_report, progress) = tokio::sync::watch::channel(WorkerRestoreProgress::Ended {
            writes_began: false,
            reason: "MariaDB restore of 'orders' passed its deadline".to_string(),
        });
        let result = fixture
            .finish_unanswered(
                WorkerRestoreEngine::Mariadb,
                run.id,
                origin("mariadb"),
                progress,
            )
            .await
            .unwrap();
        let message = result.as_ref().expect_err("ended early").to_string();
        assert!(message.contains("which is unchanged"), "{message}");
        persist_terminal_state(fixture.db.as_ref(), run.id, &result, TERMINAL_WRITE_RETRY)
            .await
            .unwrap();
        assert_eq!(fixture.run_status(run.id).await, "failed");
        assert_eq!(
            fixture.stored_login().await,
            serde_json::json!({"username": "orders_owner", "password": "target-password"})
        );
        fixture
            .start_in_place()
            .await
            .expect("the service is free once the run failed");
    }
}
