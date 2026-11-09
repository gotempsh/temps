// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use crate::{authority::AuthorityStore, broker::UnavailableTokenlessBroker};
use sea_orm::{DatabaseBackend, MockDatabase};
use temps_core::{secret_string::SecretString, NoopAuditLogger};
use temps_service_access_runtime::{HostAudit, WorkerControl};

fn consent(service_id: &str) -> FreshConsent {
    FreshConsent {
        request_id: Uuid::new_v4(),
        project_id: 9601,
        sandbox_id: "sbx_consent".into(),
        user_id: 9601,
        admin: false,
        request: RequestServiceGrant {
            service_id: service_id.into(),
            service_version: 1,
            ttl_seconds: 60,
        },
    }
}
fn observation(worker: &WorkerControl, incarnation: Uuid) -> HostObservation {
    HostObservation {
        worker: worker.identity(),
        incarnation,
        node_id: None,
        container_id: "immutable-container".into(),
        container_name: "temps-sandbox-consent".into(),
        backend: "docker".into(),
    }
}
fn encryption() -> Arc<EncryptionService> {
    super::super::tests::encryption()
}

#[tokio::test]
async fn fresh_consent_validates_actor_lifetime_and_host_identity_before_writes() {
    let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
    let service = ServiceAccessService::new(db, encryption(), Arc::new(UnavailableTokenlessBroker));
    let worker = WorkerControl::new(Arc::new(HostAudit));
    for ttl in [0, 59, 86401] {
        let mut request = consent("svc_test");
        request.request.ttl_seconds = ttl;
        assert!(service
            .authorize_grant(request, observation(&worker, Uuid::new_v4()))
            .await
            .is_err());
    }
    let mut request = consent("svc_test");
    request.user_id = 0;
    assert!(service
        .authorize_grant(request, observation(&worker, Uuid::new_v4()))
        .await
        .is_err());
    assert!(service
        .authorize_grant(consent("svc_test"), observation(&worker, Uuid::nil()))
        .await
        .is_err());
    let mut target = observation(&worker, Uuid::new_v4());
    target.backend = "local".into();
    assert!(service
        .authorize_grant(consent("svc_test"), target)
        .await
        .is_err());
}

#[tokio::test]
async fn fresh_consent_database_failure_retains_project_and_operation() {
    let service = ServiceAccessService::new(
        Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_errors([sea_orm::DbErr::Custom("driver detail".into())])
                .into_connection(),
        ),
        encryption(),
        Arc::new(UnavailableTokenlessBroker),
    );
    let worker = WorkerControl::new(Arc::new(HostAudit));
    let error = service
        .authorize_grant(consent("svc_test"), observation(&worker, Uuid::new_v4()))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("9601"));
    assert!(error.to_string().contains("find project"));
    assert!(!error.to_string().contains("driver detail"));
}

#[tokio::test]
async fn postgres_fresh_consent_is_scoped_atomic_bounded_and_preserves_retirement_history() {
    if !temps_test_utils::external_database_configured()
        && !temps_test_utils::docker_available().await
    {
        eprintln!("Skipping fresh sandbox consent: Docker unavailable");
        return;
    }
    let mut test = temps_test_utils::TestDatabase::with_migrations()
        .await
        .unwrap();
    let db = test.connection_arc();
    db.execute_unprepared("INSERT INTO users (id,name,email) VALUES (9601,'Test user','user@example.com'); INSERT INTO projects (id,name,repo_name,repo_owner,directory,main_branch,preset,slug,created_at,updated_at) VALUES (9601,'Consent','repo','owner','/','main','nodejs','consent',now(),now()); INSERT INTO sandboxes (id,public_id,name,user_id,project_id,status,backend) VALUES (9601,'sbx_consent','consent',9601,9601,'running','docker')").await.unwrap();
    let service = ServiceAccessService::new(
        Arc::clone(&db),
        encryption(),
        Arc::new(UnavailableTokenlessBroker),
    );
    let definition = service
        .save_service(
            9601,
            None,
            SaveAccessService {
                vault_id: None,
                name: "API".into(),
                policy: SandboxAccessPolicy {
                    provider: None,
                    github_repository: None,
                    origin: "https://api.github.com".into(),
                    path_prefixes: vec!["/user".into()],
                    methods: vec![AccessMethod::Get],
                },
                max_ttl_seconds: 60,
                enabled: true,
                credential: Some(SecretString::new("fixture-token")),
                clear_credential: false,
                expected_version: None,
            },
        )
        .await
        .unwrap();
    let worker = WorkerControl::new(Arc::new(HostAudit));
    let store = AuthorityStore::new(Arc::clone(&db), encryption(), Arc::new(NoopAuditLogger));
    store.register(None, &worker.identity()).await.unwrap();
    let incarnation = Uuid::new_v4();
    let legacy = service
        .request_grant(
            9601,
            "sbx_consent",
            9601,
            false,
            consent(&definition.id).request,
        )
        .await
        .unwrap();
    assert!(authorizations::Entity::find_by_id(&legacy.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .is_none());
    let mut wrong_owner = consent(&definition.id);
    wrong_owner.user_id = 9602;
    assert!(matches!(
        service
            .authorize_grant(wrong_owner, observation(&worker, incarnation))
            .await,
        Err(AccessError::NotFound {
            kind: "sandbox",
            ..
        })
    ));
    let mut stale = consent(&definition.id);
    stale.request.service_version = 2;
    assert!(matches!(
        service
            .authorize_grant(stale, observation(&worker, incarnation))
            .await,
        Err(AccessError::Conflict { .. })
    ));
    let mut wrong_node = observation(&worker, incarnation);
    wrong_node.node_id = Some(99);
    assert!(service
        .authorize_grant(consent(&definition.id), wrong_node)
        .await
        .is_err());
    assert!(runtimes::Entity::find()
        .all(db.as_ref())
        .await
        .unwrap()
        .is_empty());
    let request_id = Uuid::new_v4();
    let mut first_request = consent(&definition.id);
    first_request.request_id = request_id;
    let mut retry_request = consent(&definition.id);
    retry_request.request_id = request_id;
    let (first, retried) = tokio::join!(
        service.authorize_grant(first_request, observation(&worker, incarnation)),
        service.authorize_grant(retry_request, observation(&worker, incarnation)),
    );
    let first = first.unwrap();
    let retried = retried.unwrap();
    assert_eq!(retried.grant_id, first.grant_id);
    assert_eq!(retried.expires_at, first.expires_at);
    assert_eq!(retried.desired_version, first.desired_version);
    let mut changed_retry = consent(&definition.id);
    changed_retry.request_id = request_id;
    assert!(service
        .authorize_grant(changed_retry, observation(&worker, Uuid::new_v4()))
        .await
        .is_err());
    assert_eq!(first.runtime_id, incarnation);
    assert_eq!(first.desired_version, 1);
    let saved = grants::Entity::find_by_id(&first.grant_id)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert_eq!((saved.expires_at - saved.created_at).num_seconds(), 60);
    let row = runtimes::Entity::find_by_id(incarnation)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.applied_version, 0);
    assert!(row.lease_expires_at.is_none());
    let queue = crate::delivery::DeliveryQueue::new(Arc::clone(&db));
    let claim = queue.claim(1).await.unwrap().pop().unwrap();
    assert_eq!(
        claim.command().worker_boot_id,
        Some(worker.identity().boot_id)
    );
    assert_eq!(
        claim.command().provider_identity.as_deref(),
        Some("immutable-container")
    );
    // Filling to the admission limit does not count the legacy intention.
    for _ in 1..16 {
        service
            .authorize_grant(consent(&definition.id), observation(&worker, incarnation))
            .await
            .unwrap();
    }
    let before = grants::Entity::find().count(db.as_ref()).await.unwrap();
    assert!(service
        .authorize_grant(consent(&definition.id), observation(&worker, incarnation))
        .await
        .unwrap_err()
        .to_string()
        .contains("16 active grants"));
    assert_eq!(
        grants::Entity::find().count(db.as_ref()).await.unwrap(),
        before
    );
    let replacement = Uuid::new_v4();
    let next_request_id = Uuid::new_v4();
    let mut next_request = consent(&definition.id);
    next_request.request_id = next_request_id;
    let next = service
        .authorize_grant(next_request, observation(&worker, replacement))
        .await
        .unwrap();
    let old = runtimes::Entity::find_by_id(incarnation)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert!(old.closed_at.is_some());
    assert!(authorizations::Entity::find_by_id(first.grant_id)
        .one(db.as_ref())
        .await
        .unwrap()
        .is_some());
    let retirement = temps_entities::sandbox_access_delivery::Entity::find()
        .filter(temps_entities::sandbox_access_delivery::Column::RuntimeId.eq(incarnation))
        .filter(
            temps_entities::sandbox_access_delivery::Column::Operation
                .eq(temps_entities::sandbox_access_delivery::DeliveryOperation::Retire),
        )
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        retirement.provider_identity.as_deref(),
        Some("immutable-container")
    );
    service
        .revoke_grant(9601, "sbx_consent", &next.grant_id, 9601, false, 1)
        .await
        .unwrap();
    let mut revoked_retry = consent(&definition.id);
    revoked_retry.request_id = next_request_id;
    assert!(service
        .authorize_grant(revoked_retry, observation(&worker, replacement))
        .await
        .unwrap_err()
        .to_string()
        .contains("original approval ended"));
    // A removed approver must not leave an authorization eligible for renewal.
    db.execute_unprepared(
        "UPDATE sandbox_access_authorizations SET approved_by=NULL WHERE approved_by=9601",
    )
    .await
    .unwrap();
    assert!(authorizations::Entity::find()
        .all(db.as_ref())
        .await
        .unwrap()
        .iter()
        .all(|row| row.approved_by.is_none()));
    drop(service);
    drop(store);
    drop(queue);
    drop(worker);
    drop(db);
    test.cleanup().await;
}

#[tokio::test]
async fn worker_admission_rejects_overflow_and_legacy_rows_still_reconcile() {
    if !temps_test_utils::external_database_configured()
        && !temps_test_utils::docker_available().await
    {
        eprintln!("Skipping fresh sandbox consent: Docker unavailable");
        return;
    }
    let mut test = temps_test_utils::TestDatabase::with_migrations()
        .await
        .unwrap();
    let db = test.connection_arc();
    db.execute_unprepared("INSERT INTO users (id,name,email) VALUES (9601,'Test user','user@example.com'); INSERT INTO projects (id,name,repo_name,repo_owner,directory,main_branch,preset,slug,created_at,updated_at) VALUES (9601,'Consent','repo','owner','/','main','nodejs','consent',now(),now()); INSERT INTO sandboxes (id,public_id,name,user_id,project_id,status,backend) VALUES (9601,'sbx_consent','consent',9601,9601,'running','docker')").await.unwrap();
    let service = ServiceAccessService::new(
        Arc::clone(&db),
        encryption(),
        Arc::new(UnavailableTokenlessBroker),
    );
    let definition = service
        .save_service(
            9601,
            None,
            SaveAccessService {
                vault_id: None,
                name: "API".into(),
                policy: SandboxAccessPolicy {
                    provider: None,
                    github_repository: None,
                    origin: "https://api.github.com".into(),
                    path_prefixes: vec!["/user".into()],
                    methods: vec![AccessMethod::Get],
                },
                max_ttl_seconds: 60,
                enabled: true,
                credential: Some(SecretString::new("fixture-token")),
                clear_credential: false,
                expected_version: None,
            },
        )
        .await
        .unwrap();
    let worker = WorkerControl::new(Arc::new(HostAudit));
    let store = AuthorityStore::new(Arc::clone(&db), encryption(), Arc::new(NoopAuditLogger));
    store.register(None, &worker.identity()).await.unwrap();

    use crate::{delivery::DeliveryQueue, planner::PolicyPlanner};
    use temps_entities::sandbox_access_delivery::DeliveryOperation;
    let healthy = service
        .authorize_grant(
            consent(&definition.id),
            observation(&worker, Uuid::new_v4()),
        )
        .await
        .unwrap();
    // Seed historical open runtimes left behind by destroyed sandboxes.
    db.execute_unprepared("INSERT INTO sandboxes(id,public_id,name,user_id,project_id,status,backend) SELECT n,'sbx_capacity_'||n,'capacity',9601,9601,'destroyed','docker' FROM generate_series(10001,10128) n").await.unwrap();
    let boot = worker.identity().boot_id;
    db.execute_unprepared(&format!("INSERT INTO sandbox_access_runtimes(id,project_id,sandbox_id,authority_id,provider_identity,provider_name,provider_backend) SELECT gen_random_uuid(),9601,id,'{boot}','container-'||id,'temps-sandbox-capacity_'||id,'docker' FROM sandboxes WHERE id BETWEEN 10001 AND 10127")).await.unwrap();
    db.execute_unprepared("UPDATE sandboxes SET status='running' WHERE id=10128")
        .await
        .unwrap();
    let mut extra = consent(&definition.id);
    extra.sandbox_id = "sbx_capacity_10128".into();
    let mut target = observation(&worker, Uuid::new_v4());
    target.container_name = "temps-sandbox-capacity_10128".into();
    let error = service.authorize_grant(extra, target).await.unwrap_err();
    assert!(error.to_string().contains("128 open bindings"));
    assert_eq!(
        runtimes::Entity::find()
            .filter(runtimes::Column::ClosedAt.is_null())
            .count(db.as_ref())
            .await
            .unwrap(),
        128
    );
    // Existing authorization remains renewable at capacity.
    service
        .authorize_grant(
            consent(&definition.id),
            observation(&worker, healthy.runtime_id),
        )
        .await
        .unwrap();
    // Simulate an excess row accepted by an older version. Neither scheduling nor
    // planning may reject the entire worker before retiring these stale rows.
    db.execute_unprepared(&format!("INSERT INTO sandbox_access_runtimes(id,project_id,sandbox_id,authority_id,provider_identity,provider_name,provider_backend) VALUES(gen_random_uuid(),9601,10128,'{boot}','container-extra','temps-sandbox-capacity_10128','docker')")).await.unwrap();
    let queue = DeliveryQueue::new(db.clone());
    assert!(queue.schedule_due(16).await.unwrap().queued > 0);
    let planner = PolicyPlanner::new(
        db.clone(),
        encryption(),
        None,
        Arc::new(crate::github::NoGithub),
    );
    let plan = planner.prepare(boot).await.unwrap();
    assert_eq!(plan.bindings.len(), 1);
    assert_eq!(plan.retire.len(), 128);
    // Even a legacy overflow of fully eligible bindings must be bounded at
    // issuance, rather than poisoning every policy renewal on the worker.
    db.execute_unprepared("UPDATE sandboxes SET status='running' WHERE id BETWEEN 10001 AND 10128")
        .await
        .unwrap();
    db.execute_unprepared(&format!(r#"
INSERT INTO sandbox_service_grants(id,project_id,service_id,sandbox_id,service_version,policy_snapshot,created_by,expires_at)
SELECT 'grant_'||replace(gen_random_uuid()::text,'-',''),9601,g.service_id,r.sandbox_id,g.service_version,g.policy_snapshot,9601,now()+interval '60 seconds'
FROM sandbox_access_runtimes r CROSS JOIN sandbox_service_grants g
WHERE r.sandbox_id BETWEEN 10001 AND 10128 AND g.id='{}';
INSERT INTO sandbox_access_authorizations(grant_id,request_id,requested_ttl,runtime_id,project_id,sandbox_id,approved_by)
SELECT g.id,gen_random_uuid(),60,r.id,9601,r.sandbox_id,9601
FROM sandbox_access_runtimes r JOIN sandbox_service_grants g ON g.sandbox_id=r.sandbox_id WHERE r.sandbox_id BETWEEN 10001 AND 10128;
"#, healthy.grant_id)).await.unwrap();
    let overflow = planner.prepare(boot).await.unwrap();
    assert_eq!(overflow.bindings.len(), 128);
    assert_eq!(overflow.retire.len(), 1);
    assert!(overflow
        .bindings
        .iter()
        .any(|binding| binding.plan.target().incarnation_id == healthy.runtime_id));
    for stale in plan.retire {
        let tx = db.begin().await.unwrap();
        DeliveryQueue::advance(
            &tx,
            stale.id,
            stale.desired_version,
            DeliveryOperation::Retire,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }
    let recovered = planner.prepare(boot).await.unwrap();
    assert_eq!(recovered.bindings.len(), 1);
    assert!(recovered.retire.is_empty());
    test.cleanup().await;
}
