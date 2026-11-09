// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use sea_orm::{MockDatabase, QueryOrder};
use temps_entities::sandbox_access_runtimes as runtimes;

fn claimed_row() -> delivery::Model {
    let now = chrono::Utc::now();
    delivery::Model {
        id: 1,
        runtime_id: Uuid::new_v4(),
        node_id: None,
        worker_boot_id: None,
        provider_identity: None,
        provider_backend: None,
        provider_name: None,
        desired_version: 1,
        operation: DeliveryOperation::Reconcile,
        attempts: 1,
        available_at: now,
        claim_id: Some(Uuid::new_v4()),
        claimed_until: Some(now + chrono::Duration::seconds(30)),
        completed_at: None,
        created_at: now,
    }
}

#[tokio::test]
async fn receipt_and_retry_database_errors_include_the_failed_operation() {
    let row = claimed_row();
    let claim = DeliveryClaim {
        claim_id: row.claim_id.unwrap(),
        command: row,
    };
    let queue = DeliveryQueue::new(Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_errors([DbErr::Custom("unavailable".into())])
            .append_exec_errors([DbErr::Custom("unavailable".into())])
            .into_connection(),
    ));
    let error = queue.complete(&claim).await.unwrap_err();
    assert!(error.to_string().contains("lock receipt runtime"));
    assert!(error
        .to_string()
        .contains(&claim.command.runtime_id.to_string()));
    let error = queue.retry(&claim).await.unwrap_err();
    assert!(error.to_string().contains("reschedule command"));
    assert!(std::error::Error::source(&error).is_some());
}

#[tokio::test]
async fn retirement_cannot_be_superseded_or_treated_as_a_live_lease() {
    let mut row = claimed_row();
    row.operation = DeliveryOperation::Retire;
    let claim = DeliveryClaim {
        claim_id: row.claim_id.unwrap(),
        command: row,
    };
    let queue = DeliveryQueue::new(Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres).into_connection(),
    ));
    assert!(matches!(
        queue.supersede(&claim).await,
        Err(DeliveryError::Invalid { .. })
    ));
    assert!(matches!(
        queue
            .complete_applied(
                &claim,
                1,
                chrono::Utc::now() + chrono::Duration::seconds(10)
            )
            .await,
        Err(DeliveryError::Invalid { .. })
    ));
}

#[tokio::test]
async fn rejects_invalid_batches_and_preserves_database_failure_context() {
    let queue = DeliveryQueue::new(Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_errors([DbErr::Custom("unavailable".into())])
            .into_connection(),
    ));
    for limit in [0, 33, u32::MAX] {
        assert!(matches!(
            queue.claim(limit).await,
            Err(DeliveryError::Invalid { .. })
        ));
    }
    for limit in [0, 17, u32::MAX] {
        assert!(matches!(
            queue.schedule_due(limit).await,
            Err(DeliveryError::Invalid { .. })
        ));
    }
    let error = queue.claim(1).await.unwrap_err();
    assert!(error.to_string().contains("claim commands"));
    assert!(error.to_string().contains("pending queue"));
    assert!(std::error::Error::source(&error).is_some());
}

#[tokio::test]
async fn advance_rejects_bad_versions_missing_runtimes_and_database_errors() {
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([Vec::<runtimes::Model>::new()])
        .append_query_errors([DbErr::Custom("unavailable".into())])
        .into_connection();
    let txn = db.begin().await.unwrap();
    let id = Uuid::new_v4();
    for (id, version) in [(Uuid::nil(), 0), (id, -1), (id, i64::MAX)] {
        assert!(matches!(
            DeliveryQueue::advance(&txn, id, version, DeliveryOperation::Reconcile).await,
            Err(DeliveryError::Invalid { .. })
        ));
    }
    assert!(
        matches!(DeliveryQueue::advance(&txn,id,2,DeliveryOperation::Reconcile).await,
        Err(DeliveryError::RuntimeChanged { runtime_id, version:2 }) if runtime_id==id)
    );
    let error = DeliveryQueue::advance(&txn, id, 2, DeliveryOperation::Reconcile)
        .await
        .unwrap_err();
    assert!(error.to_string().contains(&id.to_string()));
    assert!(std::error::Error::source(&error).is_some());
}

#[tokio::test]
async fn postgres_delivery_fences_claims_versions_and_survives_runtime_deletion() {
    if !temps_test_utils::external_database_configured()
        && !temps_test_utils::docker_available().await
    {
        eprintln!("Skipping durable service-access delivery: Docker unavailable");
        return;
    }
    let mut test = temps_test_utils::TestDatabase::with_migrations()
        .await
        .unwrap();
    let db = test.connection_arc();
    db.execute_unprepared("INSERT INTO projects (id,name,repo_name,repo_owner,directory,main_branch,preset,slug,created_at,updated_at) VALUES (9401,'Delivery','repo','owner','/','main','nodejs','delivery',now(),now()); INSERT INTO sandboxes (id,public_id,name,project_id) VALUES (9401,'sbx_delivery','delivery',9401); INSERT INTO sandbox_access_runtimes (id,project_id,sandbox_id,provider_identity) VALUES ('00000000-0000-4000-8000-000000000201',9401,9401,'container:generation')").await.unwrap();
    let queue = DeliveryQueue::new(Arc::clone(&db));
    let runtime_id = Uuid::parse_str("00000000-0000-4000-8000-000000000201").unwrap();
    let txn = db.begin().await.unwrap();
    assert_eq!(
        DeliveryQueue::advance(&txn, runtime_id, 0, DeliveryOperation::Reconcile)
            .await
            .unwrap(),
        1
    );
    txn.rollback().await.unwrap();
    assert!(queue.claim(32).await.unwrap().is_empty());

    let advance = || async {
        let txn = db.begin().await.unwrap();
        let result =
            DeliveryQueue::advance(&txn, runtime_id, 0, DeliveryOperation::Reconcile).await;
        if result.is_ok() {
            txn.commit().await.unwrap();
        } else {
            txn.rollback().await.unwrap();
        }
        result
    };
    let (first, second) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        tokio::join!(advance(), advance())
    })
    .await
    .unwrap();
    assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
    assert!(matches!(
        first.err().or_else(|| second.err()),
        Some(DeliveryError::RuntimeChanged { version: 0, .. })
    ));
    // Competing dispatchers get disjoint claims, never two owners for a row.
    let (a, b) = tokio::join!(queue.claim(1), queue.claim(1));
    let mut claims = a.unwrap();
    claims.extend(b.unwrap());
    assert_eq!(claims.len(), 1);
    let first = claims.pop().unwrap();
    assert_eq!(first.command().attempts, 1);
    assert!(queue.claim(1).await.unwrap().is_empty());
    // A process lost its lease: reclaim without waiting on wall-clock sleeps.
    db.execute_unprepared(
        "UPDATE sandbox_access_delivery SET claimed_until=now()-interval '1 second'",
    )
    .await
    .unwrap();
    let reclaimed = queue.claim(1).await.unwrap().pop().unwrap();
    assert_eq!(reclaimed.command().attempts, 2);
    assert!(matches!(
        queue.complete(&first).await,
        Err(DeliveryError::StaleClaim { .. })
    ));
    assert!(matches!(
        queue.retry(&first).await,
        Err(DeliveryError::StaleClaim { .. })
    ));
    queue.retry(&reclaimed).await.unwrap();
    assert!(queue.claim(1).await.unwrap().is_empty());
    db.execute_unprepared(
        "UPDATE sandbox_access_delivery SET available_at=now()-interval '1 second'",
    )
    .await
    .unwrap();
    let retry = queue.claim(1).await.unwrap().pop().unwrap();
    assert_eq!(retry.command().attempts, 3);

    // Advancing desired state while an old command is in flight cannot make
    // that old receipt report the new policy as installed.
    let txn = db.begin().await.unwrap();
    assert_eq!(
        DeliveryQueue::advance(&txn, runtime_id, 1, DeliveryOperation::Reconcile)
            .await
            .unwrap(),
        2
    );
    txn.commit().await.unwrap();
    queue.complete(&retry).await.unwrap();
    assert_eq!(
        runtimes::Entity::find_by_id(runtime_id)
            .one(db.as_ref())
            .await
            .unwrap()
            .unwrap()
            .applied_version,
        0
    );
    let second = queue.claim(1).await.unwrap().pop().unwrap();
    queue.complete(&second).await.unwrap();
    assert_eq!(
        runtimes::Entity::find_by_id(runtime_id)
            .one(db.as_ref())
            .await
            .unwrap()
            .unwrap()
            .applied_version,
        2
    );
    assert!(matches!(
        queue.complete(&second).await,
        Err(DeliveryError::StaleClaim { .. })
    ));

    let txn = db.begin().await.unwrap();
    assert!(matches!(
        DeliveryQueue::advance(&txn, runtime_id, 1, DeliveryOperation::Reconcile).await,
        Err(DeliveryError::RuntimeChanged { version: 1, .. })
    ));
    assert_eq!(
        DeliveryQueue::advance(&txn, runtime_id, 2, DeliveryOperation::Retire)
            .await
            .unwrap(),
        3
    );
    txn.commit().await.unwrap();
    let cut = queue.claim(1).await.unwrap().pop().unwrap();
    assert_eq!(cut.command().operation, DeliveryOperation::Retire);
    let txn = db.begin().await.unwrap();
    assert!(matches!(
        DeliveryQueue::advance(&txn, runtime_id, 3, DeliveryOperation::Reconcile).await,
        Err(DeliveryError::RuntimeChanged { .. })
    ));
    txn.rollback().await.unwrap();
    // Deletion leaves the claimed retirement intact; no duplicate or FK loss.
    db.execute_unprepared("DELETE FROM sandbox_access_runtimes")
        .await
        .unwrap();
    queue.complete(&cut).await.unwrap();
    assert!(queue.claim(32).await.unwrap().is_empty());
    let commands = delivery::Entity::find()
        .order_by_asc(delivery::Column::Id)
        .all(db.as_ref())
        .await
        .unwrap();
    assert_eq!(commands.len(), 3);
    assert!(commands
        .iter()
        .all(|c| c.completed_at.is_some() && c.claim_id.is_none() && c.claimed_until.is_none()));
    db.execute_unprepared("INSERT INTO sandbox_access_delivery (runtime_id,desired_version,operation,completed_at) SELECT md5(i::text)::uuid,1,'reconcile',now()-interval '1 hour' FROM generate_series(1,260) i; INSERT INTO sandbox_access_delivery (runtime_id,desired_version,operation,created_at) VALUES ('00000000-0000-4000-8000-000000000001',1,'retire',now()-interval '1 hour')").await.unwrap();
    assert_eq!(queue.prune_completed().await.unwrap(), 256);
    assert_eq!(queue.prune_completed().await.unwrap(), 4);
    assert_eq!(queue.prune_completed().await.unwrap(), 0);
    let retained = delivery::Entity::find().all(db.as_ref()).await.unwrap();
    assert_eq!(retained.len(), 4);
    assert_eq!(
        retained
            .iter()
            .filter(|row| row.completed_at.is_none())
            .count(),
        1
    );
    test.cleanup().await;
}
