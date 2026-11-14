// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;
use sea_orm::{DbBackend, EntityTrait, Statement};

type ManagedRecords = std::collections::HashMap<(Uuid, String), String>;
#[derive(Default)]
struct ManagedFake {
    records: std::sync::Mutex<ManagedRecords>,
    crash: std::sync::atomic::AtomicBool,
}
#[async_trait::async_trait]
impl temps_core::workflow_journal::ManagedWorkflowJournal for ManagedFake {
    async fn check(
        &self,
        _: i32,
        _: i32,
    ) -> Result<(), temps_core::workflow_journal::WorkflowJournalError> {
        Ok(())
    }
    async fn put_first(
        &self,
        scope: temps_core::workflow_journal::WorkflowJournalScope,
        key: &str,
        _previous: Option<&str>,
        value: &str,
    ) -> Result<String, temps_core::workflow_journal::WorkflowJournalError> {
        let value = self
            .records
            .lock()
            .unwrap()
            .entry((scope.run_id, key.into()))
            .or_insert(value.into())
            .clone();
        if self.crash.swap(false, std::sync::atomic::Ordering::SeqCst) {
            return Err(temps_core::workflow_journal::WorkflowJournalError {
                service_id: scope.service_id,
                operation: "write entry",
                reason: "simulated crash after remote commit",
            });
        }
        Ok(value)
    }
    async fn read(
        &self,
        scope: temps_core::workflow_journal::WorkflowJournalScope,
        keys: &[String],
    ) -> Result<Vec<String>, temps_core::workflow_journal::WorkflowJournalError> {
        Ok(keys
            .iter()
            .map(|key| self.records.lock().unwrap()[&(scope.run_id, key.clone())].clone())
            .collect())
    }
    async fn delete_run(
        &self,
        scope: temps_core::workflow_journal::WorkflowJournalScope,
    ) -> Result<(), temps_core::workflow_journal::WorkflowJournalError> {
        self.records
            .lock()
            .unwrap()
            .retain(|(run, _), _| *run != scope.run_id);
        Ok(())
    }
}

#[tokio::test]
async fn managed_journal_recovers_a_remote_commit_without_changing_its_result() {
    let Some(mut f) = fixture("managed journal crash recovery").await else {
        return;
    };
    let row=f.conn.query_one(Statement::from_string(DbBackend::Postgres,"INSERT INTO external_services(name,service_type,status,created_at,updated_at) VALUES('journal-test','postgres','running',now(),now()) RETURNING id".to_string())).await.unwrap().unwrap();
    let service_id: i32 = row.try_get("", "id").unwrap();
    let remote = Arc::new(ManagedFake::default());
    f.store = PgWorkflowStore::new(f.conn.clone()).with_managed_journal(remote.clone());
    f.store
        .upsert_settings(
            f.project_id,
            f.environment_id,
            &WorkflowSettings {
                managed_store_service_id: Some(service_id),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let run = f.start("managed").await;
    assert!(run.has_blob_payloads);
    let claimed = f.claim_one().await.run;
    let mut first = step(&claimed, 0, "first", StepStatus::Completed);
    first.result.encrypted = Some("first ciphertext".into());
    remote
        .crash
        .store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(f.store.record_step(first.clone(), LIMITS).await.is_err());
    assert_eq!(
        f.store.get_run(run.id).await.unwrap().unwrap().step_count,
        0
    );
    assert!(f.store.journal(run.id, 0, 100).await.unwrap().is_empty());
    assert_eq!(remote.records.lock().unwrap().len(), 1);
    // Changing code after the uncertain commit cannot change its step identity.
    let mut renamed = first.clone();
    renamed.name = "renamed".into();
    assert!(matches!(
        f.store.record_step(renamed, LIMITS).await,
        Err(WorkflowError::Incompatible { .. })
    ));
    assert_eq!(
        f.store.get_run(run.id).await.unwrap().unwrap().step_count,
        0
    );
    // A different result on retry must recover the first durable write.
    first.result.encrypted = Some("second ciphertext".into());
    first.result.bytes = 11;
    let result = f.store.record_step(first, LIMITS).await.unwrap();
    let StepRecordOutcome::Recorded { entry, .. } = result else {
        panic!("recovered write")
    };
    assert_eq!(entry.result_encrypted.as_deref(), Some("first ciphertext"));
    assert_eq!(entry.result_bytes, 10);
    let central = temps_entities::workflow_steps::Entity::find_by_id((run.id, 0))
        .one(f.conn.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(central.result_encrypted, None);
    assert!(central.managed_revision.is_some());
    assert_eq!(
        f.store
            .step(run.id, 0)
            .await
            .unwrap()
            .unwrap()
            .result_encrypted,
        entry.result_encrypted
    );
    assert_eq!(
        f.store
            .get_run(run.id)
            .await
            .unwrap()
            .unwrap()
            .journal_bytes,
        10
    );
    let saved = remote.records.lock().unwrap().clone();
    for value in remote.records.lock().unwrap().values_mut() {
        *value = "invalid".into();
    }
    assert!(matches!(
        f.store.step(run.id, 0).await,
        Err(WorkflowError::ManagedJournal(_))
    ));
    *remote.records.lock().unwrap() = saved;
    // New settings cannot strand runs already pinned to this managed store.
    f.store
        .upsert_settings(f.project_id, f.environment_id, &WorkflowSettings::default())
        .await
        .unwrap();
    assert_eq!(
        f.store.step(run.id, 0).await.unwrap().unwrap().result_bytes,
        10
    );
    use sea_orm_migration::MigratorTrait;
    let migration = temps_migrations::Migrator::migrations()
        .into_iter()
        .find(|m| m.name() == "m20261113_000007_workflow_managed_journal")
        .unwrap();
    let manager = sea_orm_migration::SchemaManager::new(f.conn.as_ref());
    assert!(migration.down(&manager).await.is_err());
    f.store.cancel_run(run.id).await.unwrap();
    f.store.delete_run(run.id).await.unwrap();
    assert_eq!(remote.records.lock().unwrap().len(), 1);
    assert!(migration.down(&manager).await.is_err());
    f.store.delete_expired_runs(20).await.unwrap();
    assert!(remote.records.lock().unwrap().is_empty());
    migration.down(&manager).await.unwrap();
    migration.up(&manager).await.unwrap();
}

#[tokio::test]
async fn managed_wait_completion_recovers_after_remote_commit() {
    let Some(mut f) = fixture("managed journal crash recovery").await else {
        return;
    };
    let row=f.conn.query_one(Statement::from_string(DbBackend::Postgres,"INSERT INTO external_services(name,service_type,status,created_at,updated_at) VALUES('journal-test','postgres','running',now(),now()) RETURNING id".to_string())).await.unwrap().unwrap();
    let service_id: i32 = row.try_get("", "id").unwrap();
    let remote = Arc::new(ManagedFake::default());
    f.store = PgWorkflowStore::new(f.conn.clone()).with_managed_journal(remote.clone());
    f.store
        .upsert_settings(
            f.project_id,
            f.environment_id,
            &WorkflowSettings {
                managed_store_service_id: Some(service_id),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let run = f.start("managed-wait").await;
    let claimed = f.claim_one().await.run;
    let mut wait = step(&claimed, 0, "approval", StepStatus::Pending);
    wait.kind = StepKind::Wait;
    wait.result = StoredPayload::default();
    f.store.record_step(wait, LIMITS).await.unwrap();
    f.store
        .release(
            run.id,
            claimed.lease_token,
            Release::Wait {
                sequence: 0,
                event: "approval".into(),
                key: "one".into(),
                timeout_at: None,
            },
        )
        .await
        .unwrap();
    let waiting = f
        .store
        .waiting_runs(f.environment_id, "approval", "one", None, 10)
        .await
        .unwrap()
        .remove(0);
    let first = StoredPayload {
        encrypted: Some("first wait ciphertext".into()),
        bytes: 5,
        blob_key: Some("run/first-object".into()),
    };
    remote
        .crash
        .store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(f
        .store
        .complete_wait(&waiting, None, first.clone(), 25)
        .await
        .is_err());
    assert_eq!(
        f.store.get_run(run.id).await.unwrap().unwrap().status,
        "waiting"
    );
    let second = StoredPayload {
        encrypted: Some("second wait ciphertext".into()),
        bytes: 9,
        blob_key: None,
    };
    assert_eq!(
        f.store
            .complete_wait(&waiting, None, second, 25)
            .await
            .unwrap(),
        WaitCompletion::Completed
    );
    let entry = f.store.step(run.id, 0).await.unwrap().unwrap();
    assert_eq!(entry.result_encrypted, first.encrypted);
    assert_eq!(entry.result_blob_key, first.blob_key);
    let run = f.store.get_run(run.id).await.unwrap().unwrap();
    assert_eq!(run.journal_bytes, 5);
    assert!(run.has_blob_payloads);
    assert_eq!(run.status, "queued");
}

#[tokio::test]
async fn managed_sleep_resume_keeps_the_central_wake_time() {
    let Some(mut f) = fixture("managed sleep resume").await else {
        return;
    };
    let row=f.conn.query_one(Statement::from_string(DbBackend::Postgres,"INSERT INTO external_services(name,service_type,status,created_at,updated_at) VALUES('journal-test','postgres','running',now(),now()) RETURNING id".to_string())).await.unwrap().unwrap();
    let service_id: i32 = row.try_get("", "id").unwrap();
    let remote = Arc::new(ManagedFake::default());
    f.store = PgWorkflowStore::new(f.conn.clone()).with_managed_journal(remote.clone());
    f.store
        .upsert_settings(
            f.project_id,
            f.environment_id,
            &WorkflowSettings {
                managed_store_service_id: Some(service_id),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let run = f.start("managed").await;
    let claimed = f.claim_one().await.run;
    let later = Utc::now() + Duration::hours(1);
    let mut record = step(&claimed, 0, "nap", StepStatus::Pending);
    record.kind = StepKind::Sleep;
    record.wake_at = Some(later);
    f.store.record_step(record, LIMITS).await.unwrap();
    f.store
        .release(run.id, claimed.lease_token, Release::Sleep { until: later })
        .await
        .unwrap();
    f.store.resume_run(run.id).await.unwrap();
    let entry = f.store.step(run.id, 0).await.unwrap().unwrap();
    assert!(entry.wake_at.unwrap() <= Utc::now());
    let page = f.store.journal(run.id, 0, 20).await.unwrap();
    let hydrated = f.store.hydrate_journal(run.id, page).await.unwrap();
    assert_eq!(hydrated[0].wake_at, entry.wake_at);
}

#[tokio::test]
async fn managed_child_creation_recovers_the_same_child_after_remote_commit() {
    let Some(mut f) = fixture("managed child recovery").await else {
        return;
    };
    let row=f.conn.query_one(Statement::from_string(DbBackend::Postgres,"INSERT INTO external_services(name,service_type,status,created_at,updated_at) VALUES('journal-test','postgres','running',now(),now()) RETURNING id".to_string())).await.unwrap().unwrap();
    let service_id: i32 = row.try_get("", "id").unwrap();
    let remote = Arc::new(ManagedFake::default());
    f.store = PgWorkflowStore::new(f.conn.clone()).with_managed_journal(remote.clone());
    f.store
        .upsert_settings(
            f.project_id,
            f.environment_id,
            &WorkflowSettings {
                managed_store_service_id: Some(service_id),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    f.start("parent").await;
    let parent = f.claim_one().await.run;
    let mut child = f.new_run("child", Some("child-recovery"));
    child.parent_run_id = Some(parent.id);
    let mut record = step(&parent, 0, "child", StepStatus::Pending);
    record.kind = StepKind::Child;
    record.child = Some((child.clone(), 100));
    f.store.track_child_input(parent.id, &child).await.unwrap();
    remote
        .crash
        .store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(f.store.record_step(record.clone(), LIMITS).await.is_err());
    assert!(f.store.get_run(child.id).await.unwrap().is_none());
    // An uncertain remote write holds the input until replay or parent deletion.
    assert!(f.store.pending_blob_cleanup(100).await.unwrap().is_empty());
    let mut other = child.clone();
    other.id = Uuid::new_v4();
    record.child = Some((other.clone(), 100));
    f.store.record_step(record, LIMITS).await.unwrap();
    assert!(f.store.get_run(child.id).await.unwrap().is_some());
    assert!(f.store.get_run(other.id).await.unwrap().is_none());
    assert!(
        temps_entities::workflow_prepared_child_inputs::Entity::find_by_id(child.id)
            .one(f.conn.as_ref())
            .await
            .unwrap()
            .is_none(),
        "committed children no longer need preparation tracking"
    );
}

#[tokio::test]
async fn prepared_child_cleanup_survives_restart_and_waits_for_owners() {
    let Some(f) = fixture("prepared child cleanup ownership").await else {
        return;
    };
    let parent = f.start("parent").await;
    let mut child = f.new_run("child", None);
    child.parent_run_id = Some(parent.id);
    child.input.blob_key = Some("test-input".into());
    f.store.track_child_input(parent.id, &child).await.unwrap();
    let restarted = PgWorkflowStore::new(f.conn.clone());
    assert!(restarted
        .pending_blob_cleanup(100)
        .await
        .unwrap()
        .is_empty());
    restarted.insert_run(child.clone(), 100).await.unwrap();
    restarted.delete_run(parent.id).await.unwrap();
    assert!(
        restarted
            .pending_blob_cleanup(100)
            .await
            .unwrap()
            .is_empty(),
        "a live child still owns its input"
    );
    restarted.delete_run(child.id).await.unwrap();
    assert_eq!(
        restarted.pending_blob_cleanup(100).await.unwrap()[0].run_id,
        child.id
    );
    restarted.acknowledge_blob_cleanup(child.id).await.unwrap();

    let parent = f.start("another-parent").await;
    let orphan = f.new_run("orphan", None);
    restarted
        .track_child_input(parent.id, &orphan)
        .await
        .unwrap();
    restarted.delete_run(parent.id).await.unwrap();
    let mut pending = restarted.pending_blob_cleanup(100).await.unwrap();
    if pending.is_empty() {
        // A new random ID may sort behind the persisted cursor; wrap once.
        pending = restarted.pending_blob_cleanup(100).await.unwrap();
    }
    assert_eq!(pending[0].run_id, orphan.id);
}

#[tokio::test]
async fn prepared_cleanup_scans_bounded_windows_and_resumes_after_restart() {
    let Some(f) = fixture("bounded prepared cleanup").await else {
        return;
    };
    let parent = f.start("parent").await;
    for id in 1..=4 {
        let mut child = f.new_run("child", None);
        child.id = Uuid::from_u128(id);
        f.store.track_child_input(parent.id, &child).await.unwrap();
    }
    f.store
        .release_child_input(Uuid::from_u128(4))
        .await
        .unwrap();
    assert!(f.store.pending_blob_cleanup(0).await.unwrap().is_empty());
    for expected in 1..=3 {
        let restarted = PgWorkflowStore::new(f.conn.clone());
        assert!(restarted.pending_blob_cleanup(1).await.unwrap().is_empty());
        let cursor = temps_entities::workflow_child_cleanup_cursor::Entity::find_by_id(true)
            .one(f.conn.as_ref())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(cursor.last_run_id, Uuid::from_u128(expected));
    }
    let restarted = PgWorkflowStore::new(f.conn.clone());
    assert_eq!(
        restarted.pending_blob_cleanup(1).await.unwrap()[0].run_id,
        Uuid::from_u128(4)
    );
    restarted
        .acknowledge_blob_cleanup(Uuid::from_u128(4))
        .await
        .unwrap();
    assert!(restarted.pending_blob_cleanup(1).await.unwrap().is_empty()); // wrap
    restarted.delete_run(parent.id).await.unwrap();
    for expected in 1..=3 {
        let pending = restarted.pending_blob_cleanup(1).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].run_id, Uuid::from_u128(expected));
        restarted
            .acknowledge_blob_cleanup(pending[0].run_id)
            .await
            .unwrap();
    }
}
