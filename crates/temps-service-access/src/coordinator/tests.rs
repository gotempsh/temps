// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use crate::{
    broker::UnavailableTokenlessBroker,
    types::{AccessMethod, RequestServiceGrant, SandboxAccessPolicy, SaveAccessService},
};
use sea_orm::{ConnectionTrait, DatabaseBackend, MockDatabase};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use temps_core::{secret_string::SecretString, NoopAuditLogger};
pub(crate) mod provider;
use provider::Provider;

fn coordinator(db: Arc<DatabaseConnection>, provider: Arc<Provider>) -> ServiceAccessCoordinator {
    let encryption = Arc::new(EncryptionService::new(&"ab".repeat(32)).unwrap());
    let service = Arc::new(ServiceAccessService::new(
        db.clone(),
        encryption.clone(),
        Arc::new(UnavailableTokenlessBroker),
    ));
    ServiceAccessCoordinator::new(
        db,
        encryption,
        Arc::new(NoopAuditLogger),
        service,
        Arc::new(WorkerTransport::new(provider)),
        None,
        Arc::new(crate::github::NoGithub),
    )
}
fn consent(id: &str) -> FreshConsent {
    FreshConsent {
        request_id: Uuid::new_v4(),
        project_id: 9801,
        sandbox_id: "sbx_dispatch".into(),
        user_id: 9801,
        admin: false,
        request: RequestServiceGrant {
            service_id: id.into(),
            service_version: 1,
            ttl_seconds: 60,
        },
    }
}

#[tokio::test]
async fn coordinator_bounds_work_and_preserves_database_error_context() {
    let provider = Provider::new();
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_errors([sea_orm::DbErr::Custom("private database detail".into())])
        .into_connection();
    let coordinator = coordinator(Arc::new(db), provider.clone());
    let a = coordinator.admit("a").unwrap();
    let b = coordinator.admit("b").unwrap();
    assert!(coordinator
        .dispatch_one()
        .await
        .unwrap_err()
        .to_string()
        .contains("two operations"));
    drop(a);
    drop(b);
    let error = coordinator.connect(consent("svc_test")).await.unwrap_err();
    assert!(error.to_string().contains("sbx_dispatch"));
    assert!(!error.to_string().contains("private database detail"));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn plugin_registers_the_coordinator_starts_once_and_aborts_owned_tasks_on_drop() {
    use temps_core::plugin::{ServiceRegistrationContext, TempsPlugin};
    let context = ServiceRegistrationContext::new();
    context.register_service(Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_errors((0..8).map(|_| sea_orm::DbErr::Custom("offline".into())))
            .into_connection(),
    ));
    context.register_service(Arc::new(EncryptionService::new(&"ab".repeat(32)).unwrap()));
    let audit: Arc<dyn AuditLogger> = Arc::new(NoopAuditLogger);
    context.register_service(audit);
    let provider: Arc<dyn temps_sandbox_providers::SandboxProvider> = Provider::new();
    context.register_service(provider);
    let plugin = crate::ServiceAccessPlugin::new();
    plugin.register_with_credentials(&context, Arc::new(crate::github::NoGithub));
    let read_context = context.create_plugin_context();
    let coordinator = context.require_service::<ServiceAccessCoordinator>();
    let weak = Arc::downgrade(&coordinator);
    let before = Arc::strong_count(&coordinator);
    plugin
        .initialize_plugin_services(&read_context)
        .await
        .unwrap();
    plugin
        .initialize_plugin_services(&read_context)
        .await
        .unwrap();
    assert_eq!(Arc::strong_count(&coordinator), before + 4);
    tokio::task::yield_now().await;
    assert!(weak.upgrade().is_some());
    drop(read_context);
    drop(context);
    drop(coordinator);
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while weak.upgrade().is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn postgres_coordinator_delivers_approved_scope_and_cuts_unconfirmed_incarnations() {
    if !temps_test_utils::external_database_configured()
        && !temps_test_utils::docker_available().await
    {
        eprintln!("Skipping service-access coordinator: Docker unavailable");
        return;
    }
    let mut test = temps_test_utils::TestDatabase::with_migrations()
        .await
        .unwrap();
    let db = test.connection_arc();
    db.execute_unprepared("INSERT INTO users (id,name,email) VALUES (9801,'Test user','user@example.com'); INSERT INTO projects (id,name,repo_name,repo_owner,directory,main_branch,preset,slug,created_at,updated_at) VALUES (9801,'Dispatch','repo','owner','/','main','nodejs','dispatch',now(),now()); INSERT INTO sandboxes (id,public_id,name,user_id,project_id,status,backend) VALUES (9801,'sbx_dispatch','dispatch',9801,9801,'running','docker')").await.unwrap();
    let provider = Provider::new();
    let coordinator = coordinator(db.clone(), provider.clone());
    let mut services = Vec::new();
    for origin in ["https://api.github.com", "https://api.example.com"] {
        services.push(
            coordinator
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
                            origin: origin.into(),
                            path_prefixes: vec!["/user".into()],
                            methods: vec![AccessMethod::Get],
                        },
                        max_ttl_seconds: 60,
                        enabled: true,
                        credential: Some(SecretString::new("fixture-credential")),
                        clear_credential: false,
                        expected_version: None,
                    },
                )
                .await
                .unwrap(),
        );
    }
    let mut wrong = consent(&services[0].id);
    wrong.user_id = 9802;
    assert!(matches!(
        coordinator.connect(wrong).await,
        Err(CoordinatorError::Access(AccessError::NotFound {
            kind: "sandbox",
            ..
        }))
    ));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    let first = coordinator.connect(consent(&services[0].id)).await.unwrap();
    // Another dispatcher/process owns this worker until its delivery ACK.
    // A competing claim must retry without issuing policy or retiring consent.
    let lock = db.begin().await.unwrap();
    lock.query_one(sea_orm::Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "SELECT pg_advisory_xact_lock(hashtextextended($1, 739214))",
        [provider.worker.identity().boot_id.to_string().into()],
    ))
    .await
    .unwrap();
    let calls = provider.calls.load(Ordering::SeqCst);
    assert_eq!(coordinator.dispatch_one().await.unwrap(), None);
    assert_eq!(provider.calls.load(Ordering::SeqCst), calls);
    assert!(runtimes::Entity::find_by_id(first.runtime_id)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap()
        .closed_at
        .is_none());
    lock.rollback().await.unwrap();
    db.execute_unprepared(
        "UPDATE sandbox_access_delivery SET available_at=now() WHERE completed_at IS NULL",
    )
    .await
    .unwrap();
    assert_eq!(
        coordinator.dispatch_one().await.unwrap(),
        Some(DispatchOutcome::Applied)
    );
    let runtime = runtimes::Entity::find_by_id(first.runtime_id)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(runtime.applied_version, first.desired_version);
    assert_eq!(runtime.applied_grant_ids, vec![first.grant_id.clone()]);
    assert!(runtime.applied_revision.is_some());
    let page = || crate::types::AccessPageQuery {
        page: None,
        page_size: None,
    };
    let listed = coordinator
        .service
        .list_grants(9801, "sbx_dispatch", 9801, false, page())
        .await
        .unwrap();
    assert!(listed.items[0].connected);
    assert_eq!(
        listed.items[0].enforcement,
        crate::types::GrantEnforcement::Active
    );
    assert!(runtime.lease_expires_at.unwrap() > chrono::Utc::now());
    let second = coordinator.connect(consent(&services[1].id)).await.unwrap();
    assert_eq!(
        coordinator.dispatch_one().await.unwrap(),
        Some(DispatchOutcome::Applied)
    );
    assert_eq!(second.runtime_id, first.runtime_id);

    // Reconcile after revoking one service, keeping the other binding alive.
    coordinator
        .service
        .revoke_grant(9801, "sbx_dispatch", &first.grant_id, 9801, false, 1)
        .await
        .unwrap();
    let listed = coordinator
        .service
        .list_grants(9801, "sbx_dispatch", 9801, false, page())
        .await
        .unwrap();
    let revoked = listed
        .items
        .iter()
        .find(|g| g.id == first.grant_id)
        .unwrap();
    assert_eq!(
        revoked.enforcement,
        crate::types::GrantEnforcement::Stopping
    );
    assert!(!revoked.connected);
    let (a, b) = tokio::join!(coordinator.maintain(), coordinator.maintain());
    assert_eq!(a.unwrap() + b.unwrap(), 1);
    assert_eq!(
        coordinator.dispatch_one().await.unwrap(),
        Some(DispatchOutcome::Applied)
    );
    let active = coordinator
        .planner
        .prepare(provider.worker.identity().boot_id)
        .await
        .unwrap();
    assert_eq!(active.bindings[0].plan.grants().len(), 1);
    let listed = coordinator
        .service
        .list_grants(9801, "sbx_dispatch", 9801, false, page())
        .await
        .unwrap();
    let revoked = listed
        .items
        .iter()
        .find(|g| g.id == first.grant_id)
        .unwrap();
    assert_eq!(revoked.enforcement, crate::types::GrantEnforcement::Stopped);
    assert!(
        listed
            .items
            .iter()
            .find(|g| g.id == second.grant_id)
            .unwrap()
            .connected
    );
    assert_eq!(
        active.bindings[0].plan.grants()[0].grant_id(),
        second.grant_id
    );

    // The worker installs policy but supplies a mismatched ACK. Never mark it
    // connected: close consent and queue an exact, durable retirement.
    provider.wrong_ack.store(true, Ordering::SeqCst);
    let tx = db.begin().await.unwrap();
    DeliveryQueue::advance(&tx, first.runtime_id, 3, DeliveryOperation::Reconcile)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(matches!(
        coordinator.dispatch_one().await,
        Err(CoordinatorError::Transport(_))
    ));
    let row = runtimes::Entity::find_by_id(first.runtime_id)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert!(row.closed_at.is_some());
    assert_eq!(row.applied_version, 3);
    assert_eq!(row.desired_version, 5);
    db.execute_unprepared("UPDATE sandbox_access_delivery SET available_at=now()-interval '1 second' WHERE completed_at IS NULL").await.unwrap();
    let binding = provider.binding.lock().unwrap().as_ref().unwrap().0.clone();
    assert_eq!(
        coordinator.dispatch_one().await.unwrap(),
        Some(DispatchOutcome::Retired)
    );
    assert!(binding.ready(1).await.is_err());
    assert_eq!(
        coordinator.dispatch_one().await.unwrap(),
        Some(DispatchOutcome::Superseded)
    );
    assert_eq!(coordinator.dispatch_one().await.unwrap(), None);
    let row = runtimes::Entity::find_by_id(first.runtime_id)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert!(row.lease_expires_at.is_none());
    assert_eq!(row.applied_version, 5);
    assert!(coordinator
        .planner
        .prepare(provider.worker.identity().boot_id)
        .await
        .unwrap()
        .bindings
        .is_empty());
    drop(binding);
    drop(coordinator);
    drop(provider);
    drop(db);
    test.cleanup().await;
}

#[tokio::test]
async fn postgres_last_revoked_grant_requires_a_cut_ack_before_retirement_completes() {
    if !temps_test_utils::external_database_configured()
        && !temps_test_utils::docker_available().await
    {
        eprintln!("Skipping service-access retirement: Docker unavailable");
        return;
    }
    let mut test = temps_test_utils::TestDatabase::with_migrations()
        .await
        .unwrap();
    let db = test.connection_arc();
    db.execute_unprepared("INSERT INTO users (id,name,email) VALUES (9801,'Test user','user@example.com'); INSERT INTO projects (id,name,repo_name,repo_owner,directory,main_branch,preset,slug,created_at,updated_at) VALUES (9801,'Dispatch','repo','owner','/','main','nodejs','dispatch',now(),now()); INSERT INTO sandboxes (id,public_id,name,user_id,project_id,status,backend) VALUES (9801,'sbx_dispatch','dispatch',9801,9801,'running','docker')").await.unwrap();
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
                    origin: "https://api.github.com".into(),
                    path_prefixes: vec!["/user".into()],
                    methods: vec![AccessMethod::Get],
                },
                max_ttl_seconds: 60,
                enabled: true,
                credential: Some(SecretString::new("fixture-credential")),
                clear_credential: false,
                expected_version: None,
            },
        )
        .await
        .unwrap();
    let grant = coordinator.connect(consent(&service.id)).await.unwrap();
    assert_eq!(coordinator.maintain().await.unwrap(), 0);
    assert_eq!(
        coordinator.dispatch_one().await.unwrap(),
        Some(DispatchOutcome::Applied)
    );
    let binding = provider.binding.lock().unwrap().as_ref().unwrap().0.clone();
    assert_eq!(coordinator.maintain().await.unwrap(), 0);
    db.execute_unprepared(
        "UPDATE sandbox_access_authorities SET next_reconcile_at=now()-interval '1 second'",
    )
    .await
    .unwrap();
    assert_eq!(coordinator.maintain().await.unwrap(), 1);
    assert_eq!(coordinator.maintain().await.unwrap(), 0);
    assert_eq!(
        coordinator.dispatch_one().await.unwrap(),
        Some(DispatchOutcome::Applied)
    );
    let renewed = temps_entities::sandbox_service_grants::Entity::find_by_id(&grant.grant_id)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(renewed.expires_at, grant.expires_at);
    coordinator
        .service
        .revoke_grant(9801, "sbx_dispatch", &grant.grant_id, 9801, false, 1)
        .await
        .unwrap();
    assert_eq!(coordinator.maintain().await.unwrap(), 1);
    assert_eq!(
        coordinator.dispatch_one().await.unwrap(),
        Some(DispatchOutcome::Superseded)
    );
    let pending = runtimes::Entity::find_by_id(grant.runtime_id)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert!(pending.closed_at.is_some());
    assert_eq!(pending.applied_version, 2);
    assert_eq!(pending.desired_version, 4);
    assert_eq!(
        coordinator.dispatch_one().await.unwrap(),
        Some(DispatchOutcome::Retired)
    );
    assert!(binding.ready(1).await.is_err());
    let retired = runtimes::Entity::find_by_id(grant.runtime_id)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(retired.applied_version, retired.desired_version);
    assert!(retired.lease_expires_at.is_none());
    assert_eq!(coordinator.dispatch_one().await.unwrap(), None);
    drop(binding);
    drop(coordinator);
    drop(provider);
    drop(db);
    test.cleanup().await;
}

#[tokio::test]
async fn applied_delivery_with_lost_receipt_retries_with_a_new_binding_version() {
    if !temps_test_utils::external_database_configured()
        && !temps_test_utils::docker_available().await
    {
        eprintln!("Skipping service-access coordinator: Docker unavailable");
        return;
    }
    let mut test = temps_test_utils::TestDatabase::with_migrations()
        .await
        .unwrap();
    let db = test.connection_arc();
    db.execute_unprepared("INSERT INTO users (id,name,email) VALUES (9801,'Test user','user@example.com'); INSERT INTO projects (id,name,repo_name,repo_owner,directory,main_branch,preset,slug,created_at,updated_at) VALUES (9801,'Dispatch','repo','owner','/','main','nodejs','dispatch',now(),now()); INSERT INTO sandboxes (id,public_id,name,user_id,project_id,status,backend) VALUES (9801,'sbx_dispatch','dispatch',9801,9801,'running','docker')").await.unwrap();
    let provider = Provider::new();
    let coordinator = coordinator(db.clone(), provider.clone());
    let mut services = Vec::new();
    for origin in ["https://api.github.com", "https://api.example.com"] {
        services.push(
            coordinator
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
                            origin: origin.into(),
                            path_prefixes: vec!["/user".into()],
                            methods: vec![AccessMethod::Get],
                        },
                        max_ttl_seconds: 60,
                        enabled: true,
                        credential: Some(SecretString::new("fixture-credential")),
                        clear_credential: false,
                        expected_version: None,
                    },
                )
                .await
                .unwrap(),
        );
    }

    let first = coordinator.connect(consent(&services[0].id)).await.unwrap();
    assert_eq!(
        coordinator.dispatch_one().await.unwrap(),
        Some(DispatchOutcome::Applied)
    );
    // Simulate the worker's successful install surviving a lost receipt commit.
    db.execute_unprepared("UPDATE sandbox_access_runtimes SET applied_version=0,lease_expires_at=NULL; UPDATE sandbox_access_delivery SET completed_at=NULL,available_at=now() WHERE operation='reconcile'").await.unwrap();
    assert_eq!(
        coordinator.dispatch_one().await.unwrap(),
        Some(DispatchOutcome::Superseded)
    );
    assert_eq!(
        coordinator.dispatch_one().await.unwrap(),
        Some(DispatchOutcome::Applied)
    );
    let runtime = runtimes::Entity::find_by_id(first.runtime_id)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert!(runtime.closed_at.is_none());
    assert_eq!(runtime.applied_version, 2);
    assert_eq!(runtime.desired_version, 2);
    drop(coordinator);
    drop(provider);
    drop(db);
    test.cleanup().await;
}
