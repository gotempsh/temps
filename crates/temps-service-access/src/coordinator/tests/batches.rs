// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;

#[tokio::test]
async fn renewal_sweep_shares_policy_and_fences_revisions_and_failed_references() {
    if !temps_test_utils::external_database_configured()
        && !temps_test_utils::docker_available().await
    {
        eprintln!("Skipping worker renewal sweep: Docker unavailable");
        return;
    }
    let mut test = temps_test_utils::TestDatabase::with_migrations()
        .await
        .unwrap();
    let db = test.connection_arc();
    db.execute_unprepared("INSERT INTO users (id,name,email) VALUES (9801,'Test user','user@example.com'); INSERT INTO projects (id,name,repo_name,repo_owner,directory,main_branch,preset,slug,created_at,updated_at) VALUES (9801,'Dispatch','repo','owner','/','main','nodejs','dispatch',now(),now()); INSERT INTO sandboxes (id,public_id,name,user_id,project_id,status,backend) SELECT 9900+i,'sbx_batch_'||i,'batch_'||i,9801,9801,'running','docker' FROM generate_series(1,8) i").await.unwrap();
    let provider = Provider::new();
    let coordinator = coordinator(db.clone(), provider.clone());
    let service = coordinator
        .service
        .save_service(
            9801,
            None,
            SaveAccessService {
                vault_id: None,
                name: "API".into(),
                policy: SandboxAccessPolicy {
                    provider: None,
                    github_repository: None,
                    origin: "https://api.example.com".into(),
                    path_prefixes: vec!["/".into()],
                    methods: vec![AccessMethod::Get],
                    delivery: None,
                },
                max_ttl_seconds: 300,
                enabled: true,
                credential: Some(SecretString::new("fixture-credential")),
                clear_credential: false,
                expected_version: None,
            },
        )
        .await
        .unwrap();
    for i in 1..=8 {
        provider.add_sandbox(&format!("batch_{i}"));
        let mut request = consent(&service.id);
        request.sandbox_id = format!("sbx_batch_{i}");
        request.request.ttl_seconds = 300;
        coordinator.connect(request).await.unwrap();
    }
    for _ in 0..8 {
        assert_eq!(
            coordinator.dispatch_one().await.unwrap(),
            Some(DispatchOutcome::Applied)
        );
    }
    assert_eq!(provider.full_policies.load(Ordering::SeqCst), 1);
    assert_eq!(provider.policy_references.load(Ordering::SeqCst), 7);
    let boot = provider.worker.identity().boot_id;
    let authority = temps_entities::sandbox_access_authorities::Entity::find_by_id(boot)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        authority.snapshot_version, 1,
        "a sweep compiles and signs just once"
    );
    let receipts = runtimes::Entity::find().all(db.as_ref()).await.unwrap();
    assert_eq!(receipts.len(), 8);
    assert!(receipts
        .iter()
        .all(|r| r.desired_version == r.applied_version && r.closed_at.is_none()));
    assert!(
        receipts
            .iter()
            .all(|r| r.lease_expires_at == receipts[0].lease_expires_at),
        "reuse must keep the first signed deadline"
    );

    db.execute_unprepared("UPDATE sandbox_access_authorities SET next_reconcile_at=now()")
        .await
        .unwrap();
    assert_eq!(coordinator.maintain().await.unwrap().queued, 8);
    assert_eq!(
        coordinator.dispatch_one().await.unwrap(),
        Some(DispatchOutcome::Applied)
    );
    assert_eq!(provider.full_policies.load(Ordering::SeqCst), 2);
    // An authorization change invalidates cached scope before any reuse.
    db.execute_unprepared(
        "UPDATE sandbox_access_authorities SET desired_revision=desired_revision+1",
    )
    .await
    .unwrap();
    assert_eq!(
        coordinator.dispatch_one().await.unwrap(),
        Some(DispatchOutcome::Applied)
    );
    assert_eq!(provider.full_policies.load(Ordering::SeqCst), 3);
    // Another control plane reserving a snapshot invalidates the local batch.
    db.execute_unprepared(
        "UPDATE sandbox_access_authorities SET snapshot_version=snapshot_version+1",
    )
    .await
    .unwrap();
    assert_eq!(
        coordinator.dispatch_one().await.unwrap(),
        Some(DispatchOutcome::Applied)
    );
    assert_eq!(provider.full_policies.load(Ordering::SeqCst), 4);
    // A replaced/unsupported reference is recoverable, never grounds to revoke consent.
    provider.reject_reference.store(true, Ordering::SeqCst);
    assert!(coordinator.dispatch_one().await.is_err());
    assert!(runtimes::Entity::find()
        .all(db.as_ref())
        .await
        .unwrap()
        .iter()
        .all(|r| r.closed_at.is_none()));
    provider.reject_reference.store(false, Ordering::SeqCst);
    db.execute_unprepared(
        "UPDATE sandbox_access_delivery SET available_at=now() WHERE completed_at IS NULL",
    )
    .await
    .unwrap();
    for _ in 0..16 {
        if coordinator.dispatch_one().await.unwrap().is_none() {
            break;
        }
    }
    let receipts = runtimes::Entity::find().all(db.as_ref()).await.unwrap();
    assert!(receipts
        .iter()
        .all(|r| r.closed_at.is_none() && r.desired_version == r.applied_version));
    test.cleanup().await;
}
