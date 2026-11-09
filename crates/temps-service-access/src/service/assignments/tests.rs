// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;
#[test]
fn requests_cannot_contain_approval_authority_or_extend_lifetime() {
    for extra in ["approved_by", "expires_at", "approved", "sandbox_id"] {
        let mut input = serde_json::json!({"service_id":"svc_example","service_version":1});
        input[extra] = serde_json::json!(1);
        assert!(serde_json::from_value::<RequestSandboxService>(input).is_err());
    }
    let approval: ApproveSandboxService =
        serde_json::from_value(serde_json::json!({"expected_version":1,"service_version":2}))
            .unwrap();
    assert!(approval.expires_at.is_none());
}
#[tokio::test]
async fn durable_approvals_renew_only_after_explicit_consent_and_revocation_wins() {
    if !temps_test_utils::external_database_configured()
        && !temps_test_utils::docker_available().await
    {
        eprintln!("Skipping durable approvals: database unavailable");
        return;
    }
    let mut test = temps_test_utils::TestDatabase::with_migrations()
        .await
        .unwrap();
    let db = test.connection_arc();
    db.execute_unprepared("INSERT INTO users(id,name,email) VALUES(9751,'Access user','access@example.com'); INSERT INTO projects(id,name,repo_name,repo_owner,directory,main_branch,preset,slug,created_at,updated_at) VALUES(9751,'Access test','repo','owner','/','main','nodejs','access-test',now(),now()); INSERT INTO sandboxes(id,public_id,name,user_id,project_id,status,backend) VALUES(9751,'sbx_durable','durable',9751,9751,'running','docker')").await.unwrap();
    let encryption = crate::service::tests::encryption();
    let service = ServiceAccessService::new(
        db.clone(),
        encryption.clone(),
        Arc::new(crate::broker::UnavailableTokenlessBroker),
    );
    let svc = service
        .save_service(
            9751,
            None,
            SaveAccessService {
                vault_id: None,
                name: "Example API".into(),
                policy: SandboxAccessPolicy {
                    provider: None,
                    origin: "https://api.example.com".into(),
                    path_prefixes: vec!["/v1".into()],
                    methods: vec![AccessMethod::Get],
                    github_repository: None,
                },
                max_ttl_seconds: 60,
                enabled: true,
                credential: Some(temps_core::secret_string::SecretString::new(
                    "test-only-token",
                )),
                clear_credential: false,
                expected_version: None,
            },
        )
        .await
        .unwrap();
    let request = || RequestSandboxService {
        service_id: svc.id.clone(),
        service_version: svc.version,
    };
    let pending = service
        .request_sandbox_service(9751, "sbx_durable", 9751, false, request())
        .await
        .unwrap();
    assert_eq!(pending.status, SandboxServiceStatus::PendingApproval);
    assert!(service
        .due_assignments()
        .await
        .unwrap()
        .assignments
        .is_empty());
    assert_eq!(
        service
            .request_sandbox_service(9751, "sbx_durable", 9751, false, request())
            .await
            .unwrap()
            .id,
        pending.id
    );
    assert!(service
        .approve_sandbox_service(
            9751,
            "sbx_durable",
            &pending.id,
            9752,
            false,
            ApproveSandboxService {
                expected_version: 1,
                service_version: 1,
                expires_at: None
            }
        )
        .await
        .is_err());
    let approved = service
        .approve_sandbox_service(
            9751,
            "sbx_durable",
            &pending.id,
            9751,
            false,
            ApproveSandboxService {
                expected_version: 1,
                service_version: 1,
                expires_at: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(approved.status, SandboxServiceStatus::Approved);
    assert!(!approved.connected);
    assert!(approved.expires_at.is_none());
    let worker = temps_service_access_runtime::WorkerControl::new(Arc::new(
        temps_service_access_runtime::HostAudit,
    ));
    crate::authority::AuthorityStore::new(
        db.clone(),
        encryption,
        Arc::new(temps_core::NoopAuditLogger),
    )
    .register(None, &worker.identity())
    .await
    .unwrap();
    let observed = HostObservation {
        worker: worker.identity(),
        incarnation: uuid::Uuid::new_v4(),
        node_id: None,
        container_id: "durable-container".into(),
        container_name: "temps-sandbox-durable".into(),
        backend: "docker".into(),
    };
    let grant = service
        .renew_assignment(&approved.id, 9751, 9751, observed.clone())
        .await
        .unwrap()
        .unwrap();
    db.execute_unprepared(&format!("UPDATE sandbox_service_grants SET expires_at=now()+interval '20 seconds' WHERE id='{grant}'")).await.unwrap();
    service
        .renew_assignment(&approved.id, 9751, 9751, observed.clone())
        .await
        .unwrap();
    let renewed = grants::Entity::find_by_id(&grant)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert!(renewed.expires_at > Utc::now() + Duration::seconds(45));
    // Rotation must happen before the old grant expires, even when its
    // immutable creation time is about to reach the 24-hour database cap.
    db.execute_unprepared(&format!("UPDATE sandbox_service_grants SET created_at=now()-interval '24 hours'+interval '20 seconds', expires_at=now()+interval '20 seconds' WHERE id='{grant}'")).await.unwrap();
    let rotated_id = service
        .renew_assignment(&approved.id, 9751, 9751, observed.clone())
        .await
        .unwrap()
        .expect("a live capped grant must rotate");
    assert_ne!(rotated_id, grant);
    let rotated = grants::Entity::find_by_id(&rotated_id)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert!(rotated.expires_at > Utc::now() + Duration::seconds(45));
    assert!(rotated.expires_at <= rotated.created_at + Duration::hours(24));
    assert!(grants::Entity::find_by_id(&grant)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap()
        .revoked_at
        .is_some());
    assert!(service
        .renew_assignment(&approved.id, 9751, 9751, observed.clone())
        .await
        .unwrap()
        .is_none());
    // A paused sandbox cannot mint another incarnation. Resuming binds the
    // existing approval to a freshly observed host identity without consent UI.
    db.execute_unprepared("UPDATE sandboxes SET status='stopped' WHERE id=9751")
        .await
        .unwrap();
    assert!(service
        .renew_assignment(&approved.id, 9751, 9751, observed.clone())
        .await
        .unwrap()
        .is_none());
    db.execute_unprepared("UPDATE sandboxes SET status='running' WHERE id=9751")
        .await
        .unwrap();
    let mut resumed = observed.clone();
    resumed.incarnation = uuid::Uuid::new_v4();
    resumed.container_id = "resumed-container".into();
    let resumed_grant = service
        .renew_assignment(&approved.id, 9751, 9751, resumed.clone())
        .await
        .unwrap()
        .unwrap();
    assert_ne!(resumed_grant, grant);
    assert!(grants::Entity::find_by_id(&grant)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap()
        .revoked_at
        .is_some());
    let revoked = service
        .revoke_sandbox_service(
            9751,
            "sbx_durable",
            &approved.id,
            9751,
            false,
            approved.version,
        )
        .await
        .unwrap();
    assert!(service
        .renew_assignment(&approved.id, 9751, 9751, observed)
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        service
            .request_sandbox_service(9751, "sbx_durable", 9751, false, request())
            .await
            .unwrap()
            .status,
        SandboxServiceStatus::Revoked
    );
    assert!(grants::Entity::find_by_id(&grant)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap()
        .revoked_at
        .is_some());
    assert!(grants::Entity::find_by_id(&resumed_grant)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap()
        .revoked_at
        .is_some());
    // Reapproval is versioned; stale callers cannot undo a revocation.
    assert!(service
        .approve_sandbox_service(
            9751,
            "sbx_durable",
            &approved.id,
            9751,
            false,
            ApproveSandboxService {
                expected_version: approved.version,
                service_version: 1,
                expires_at: None
            }
        )
        .await
        .is_err());
    let fixed = service
        .approve_sandbox_service(
            9751,
            "sbx_durable",
            &approved.id,
            9751,
            false,
            ApproveSandboxService {
                expected_version: revoked.version,
                service_version: 1,
                expires_at: Some(Utc::now() + Duration::minutes(2)),
            },
        )
        .await
        .unwrap();
    let fixed_grant = service
        .renew_assignment(&fixed.id, 9751, 9751, resumed.clone())
        .await
        .unwrap()
        .unwrap();
    db.execute_unprepared(&format!("UPDATE sandbox_service_assignments SET expires_at=now()+interval '25 seconds' WHERE id='{}'; UPDATE sandbox_service_grants SET expires_at=now()+interval '20 seconds' WHERE id='{fixed_grant}'",fixed.id)).await.unwrap();
    service
        .renew_assignment(&fixed.id, 9751, 9751, resumed.clone())
        .await
        .unwrap();
    let capped = grants::Entity::find_by_id(&fixed_grant)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();
    let approval = assignments::Entity::find_by_id(&fixed.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(Some(capped.expires_at), approval.expires_at);
    // Changing a service requires fresh approval; renewal cannot expand scope.
    db.execute_unprepared(&format!(
        "UPDATE sandbox_access_services SET version=version+1 WHERE id='{}'",
        svc.id
    ))
    .await
    .unwrap();
    assert!(service
        .renew_assignment(&fixed.id, 9751, 9751, resumed)
        .await
        .unwrap()
        .is_none());
    db.execute_unprepared(&format!("UPDATE sandbox_service_assignments SET expires_at=now()-interval '1 second',next_reconcile_at=now()-interval '1 minute' WHERE id='{}'",fixed.id)).await.unwrap();
    assert!(
        service
            .due_assignments()
            .await
            .unwrap()
            .assignments
            .is_empty(),
        "expired approvals must never enter provider renewal work"
    );
    let terminal = assignments::Entity::find_by_id(&fixed.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert!(terminal.next_reconcile_at.is_none());
    assert!(terminal.approved_at.is_some(), "retain approval history");
    assert!(
        terminal.revoked_at.is_none(),
        "expiry is not operator revocation"
    );
    assert_eq!(service.due_assignments().await.unwrap().processed, 0);
    // Reapproval explicitly wakes a terminal declaration again.
    let policy_version = service.get_service(9751, &svc.id).await.unwrap().version;
    service
        .approve_sandbox_service(
            9751,
            "sbx_durable",
            &fixed.id,
            9751,
            false,
            ApproveSandboxService {
                expected_version: terminal.version,
                service_version: policy_version,
                expires_at: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        service.due_assignments().await.unwrap().assignments.len(),
        1
    );
    // Both hard-deleted (NULL) and soft-deleted approvers must leave due work.
    db.execute_unprepared(&format!("UPDATE sandbox_service_assignments SET approved_by=NULL,next_reconcile_at=now() WHERE id='{}'", fixed.id)).await.unwrap();
    let batch = service.due_assignments().await.unwrap();
    assert_eq!(batch.processed, 1);
    assert!(batch.assignments.is_empty());
    assert_eq!(service.due_assignments().await.unwrap().processed, 0);
    db.execute_unprepared(&format!("UPDATE sandbox_service_assignments SET approved_by=9751,next_reconcile_at=now() WHERE id='{}'; UPDATE users SET deleted_at=now() WHERE id=9751", fixed.id)).await.unwrap();
    assert!(service
        .due_assignments()
        .await
        .unwrap()
        .assignments
        .is_empty());
    assert_eq!(service.due_assignments().await.unwrap().processed, 0);
    // A backlog of terminal rows is consumed once in bounded batches, without
    // sleeping after batches that contain cleanup work but no live renewals.
    db.execute_unprepared("UPDATE users SET deleted_at=NULL WHERE id=9751; INSERT INTO sandbox_access_services(id,project_id,name,policy,max_ttl_seconds) SELECT 'svc_history_'||i,9751,'History','{}',60 FROM generate_series(1,25) i; INSERT INTO sandbox_service_assignments(id,project_id,sandbox_id,service_id,service_version,approved_by,approved_at,expires_at,next_reconcile_at) SELECT 'access_history_'||i,9751,9751,'svc_history_'||i,1,9751,now()-interval '1 day',CASE WHEN i=25 THEN NULL ELSE now()-interval '1 hour' END,now()-interval '1 minute' FROM generate_series(1,25) i").await.unwrap();
    let mut processed = 0;
    let mut live = 0;
    for _ in 0..4 {
        let batch = service.due_assignments().await.unwrap();
        assert!((1..=8).contains(&batch.processed));
        processed += batch.processed;
        live += batch.assignments.len();
    }
    assert_eq!(processed, 25);
    assert_eq!(live, 1);
    assert_eq!(service.due_assignments().await.unwrap().processed, 0);
    test.cleanup().await;
}

#[tokio::test]
async fn replacement_waits_for_runtime_before_touching_authority() {
    if !temps_test_utils::external_database_configured()
        && !temps_test_utils::docker_available().await
    {
        eprintln!("Skipping durable approvals: database unavailable");
        return;
    }
    let mut test = temps_test_utils::TestDatabase::with_migrations()
        .await
        .unwrap();
    let db = test.connection_arc();
    db.execute_unprepared("INSERT INTO users(id,name,email) VALUES(9751,'Access user','access@example.com'); INSERT INTO projects(id,name,repo_name,repo_owner,directory,main_branch,preset,slug,created_at,updated_at) VALUES(9751,'Access test','repo','owner','/','main','nodejs','access-test',now(),now()); INSERT INTO sandboxes(id,public_id,name,user_id,project_id,status,backend) VALUES(9751,'sbx_durable','durable',9751,9751,'running','docker')").await.unwrap();
    let encryption = crate::service::tests::encryption();
    let service = ServiceAccessService::new(
        db.clone(),
        encryption.clone(),
        Arc::new(crate::broker::UnavailableTokenlessBroker),
    );
    let svc = service
        .save_service(
            9751,
            None,
            SaveAccessService {
                vault_id: None,
                name: "Example API".into(),
                policy: SandboxAccessPolicy {
                    provider: None,
                    origin: "https://api.example.com".into(),
                    path_prefixes: vec!["/v1".into()],
                    methods: vec![AccessMethod::Get],
                    github_repository: None,
                },
                max_ttl_seconds: 60,
                enabled: true,
                credential: Some(temps_core::secret_string::SecretString::new(
                    "test-only-token",
                )),
                clear_credential: false,
                expected_version: None,
            },
        )
        .await
        .unwrap();
    let request = || RequestSandboxService {
        service_id: svc.id.clone(),
        service_version: svc.version,
    };
    let pending = service
        .request_sandbox_service(9751, "sbx_durable", 9751, false, request())
        .await
        .unwrap();
    assert_eq!(pending.status, SandboxServiceStatus::PendingApproval);
    assert!(service
        .due_assignments()
        .await
        .unwrap()
        .assignments
        .is_empty());
    assert_eq!(
        service
            .request_sandbox_service(9751, "sbx_durable", 9751, false, request())
            .await
            .unwrap()
            .id,
        pending.id
    );
    assert!(service
        .approve_sandbox_service(
            9751,
            "sbx_durable",
            &pending.id,
            9752,
            false,
            ApproveSandboxService {
                expected_version: 1,
                service_version: 1,
                expires_at: None
            }
        )
        .await
        .is_err());
    let approved = service
        .approve_sandbox_service(
            9751,
            "sbx_durable",
            &pending.id,
            9751,
            false,
            ApproveSandboxService {
                expected_version: 1,
                service_version: 1,
                expires_at: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(approved.status, SandboxServiceStatus::Approved);
    assert!(!approved.connected);
    assert!(approved.expires_at.is_none());
    let worker = temps_service_access_runtime::WorkerControl::new(Arc::new(
        temps_service_access_runtime::HostAudit,
    ));
    crate::authority::AuthorityStore::new(
        db.clone(),
        encryption,
        Arc::new(temps_core::NoopAuditLogger),
    )
    .register(None, &worker.identity())
    .await
    .unwrap();
    let observed = HostObservation {
        worker: worker.identity(),
        incarnation: uuid::Uuid::new_v4(),
        node_id: None,
        container_id: "durable-container".into(),
        container_name: "temps-sandbox-durable".into(),
        backend: "docker".into(),
    };
    let grant = service
        .renew_assignment(&approved.id, 9751, 9751, observed.clone())
        .await
        .unwrap()
        .unwrap();

    db.execute_unprepared(&format!(
        "UPDATE sandbox_service_grants SET created_at=now()-interval '2 minutes',expires_at=now()-interval '1 second' WHERE id='{grant}'"
    ))
    .await
    .unwrap();
    let lock = db.begin().await.unwrap();
    lock.execute_unprepared(&format!(
        "SELECT id FROM sandbox_access_runtimes WHERE id='{}' FOR UPDATE",
        observed.incarnation
    ))
    .await
    .unwrap();
    let renewal = service.renew_assignment(&approved.id, 9751, 9751, observed.clone());
    tokio::pin!(renewal);
    tokio::select! {
        result = &mut renewal => panic!("renewal should wait for runtime: {result:?}"),
        _ = tokio::time::sleep(std::time::Duration::from_millis(200)) => {}
    }
    // The renewal must not hold authority while waiting for this runtime.
    lock.execute_unprepared(&format!(
        "SELECT id FROM sandbox_access_authorities WHERE id='{}' FOR UPDATE NOWAIT",
        observed.worker.boot_id
    ))
    .await
    .unwrap();
    lock.rollback().await.unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(5), &mut renewal)
            .await
            .unwrap()
            .unwrap()
            .is_some()
    );
    test.cleanup().await;
}

#[tokio::test]
async fn historical_declarations_release_capacity_but_reapproval_cannot_overfill() {
    if !temps_test_utils::external_database_configured()
        && !temps_test_utils::docker_available().await
    {
        eprintln!("Skipping durable approvals: database unavailable");
        return;
    }
    let mut test = temps_test_utils::TestDatabase::with_migrations()
        .await
        .unwrap();
    let db = test.connection_arc();
    db.execute_unprepared("INSERT INTO users(id,name,email) VALUES(9751,'Access user','access@example.com'); INSERT INTO projects(id,name,repo_name,repo_owner,directory,main_branch,preset,slug,created_at,updated_at) VALUES(9751,'Access test','repo','owner','/','main','nodejs','access-test',now(),now()); INSERT INTO sandboxes(id,public_id,name,user_id,project_id,status,backend) VALUES(9751,'sbx_durable','durable',9751,9751,'running','docker')").await.unwrap();
    let encryption = crate::service::tests::encryption();
    let service = ServiceAccessService::new(
        db.clone(),
        encryption.clone(),
        Arc::new(crate::broker::UnavailableTokenlessBroker),
    );
    let svc = service
        .save_service(
            9751,
            None,
            SaveAccessService {
                vault_id: None,
                name: "Example API".into(),
                policy: SandboxAccessPolicy {
                    provider: None,
                    origin: "https://api.example.com".into(),
                    path_prefixes: vec!["/v1".into()],
                    methods: vec![AccessMethod::Get],
                    github_repository: None,
                },
                max_ttl_seconds: 60,
                enabled: true,
                credential: Some(temps_core::secret_string::SecretString::new(
                    "test-only-token",
                )),
                clear_credential: false,
                expected_version: None,
            },
        )
        .await
        .unwrap();

    // Sixteen retained audit/history rows, half revoked and half expired.
    for i in 0..16 {
        db.execute_unprepared(&format!("INSERT INTO sandbox_access_services (id,project_id,name,policy,max_ttl_seconds,enabled,version,created_at,updated_at,credential_encrypted) SELECT 'svc_history_{i}',project_id,name,policy,max_ttl_seconds,enabled,version,created_at,updated_at,credential_encrypted FROM sandbox_access_services WHERE id='{}'; INSERT INTO sandbox_service_assignments (id,project_id,sandbox_id,service_id,service_version,requested_by,approved_by,approved_at,expires_at,revoked_at) VALUES ('access_history_{i}',9751,9751,'svc_history_{i}',1,9751,9751,now()-interval '2 minutes',CASE WHEN {i}%2=0 THEN NULL ELSE now()-interval '1 second' END,CASE WHEN {i}%2=0 THEN now() ELSE NULL END)",svc.id)).await.unwrap();
    }
    let request = service
        .request_sandbox_service(
            9751,
            "sbx_durable",
            9751,
            false,
            RequestSandboxService {
                service_id: svc.id.clone(),
                service_version: 1,
            },
        )
        .await
        .unwrap();
    assert_eq!(request.status, SandboxServiceStatus::PendingApproval);
    // Other active declarations fill capacity. Revived history must not bypass it.
    db.execute_unprepared("UPDATE sandbox_service_assignments SET revoked_at=NULL,expires_at=NULL WHERE id LIKE 'access_history_%' AND id<>'access_history_0'").await.unwrap();
    let error = service
        .approve_sandbox_service(
            9751,
            "sbx_durable",
            "access_history_0",
            9751,
            false,
            ApproveSandboxService {
                expected_version: 1,
                service_version: 1,
                expires_at: None,
            },
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("16 active services"));
    assert_eq!(
        assignments::Entity::find()
            .count(db.as_ref())
            .await
            .unwrap(),
        17
    );
    test.cleanup().await;
}
