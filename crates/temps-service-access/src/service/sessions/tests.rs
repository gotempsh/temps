// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use crate::{authority::AuthorityStore, broker::UnavailableTokenlessBroker, vault_types::*};
use temps_core::{secret_string::SecretString, NoopAuditLogger};
use temps_service_access_runtime::{HostAudit, WorkerControl};

fn request() -> CreateAccessSession {
    CreateAccessSession {
        request_id: Uuid::new_v4(),
        bundle_id: "bundle_test".into(),
        bundle_version: 1,
        consumer: AccessSessionConsumer::Sandbox {
            sandbox_id: "sbx_session".into(),
        },
        ttl_seconds: 60,
        services: vec![SessionServiceVersion {
            service_id: "svc_test".into(),
            version: 1,
        }],
    }
}
#[test]
fn session_requires_a_specific_consumer_bounded_lifetime_and_exact_versions() {
    let valid = request();
    assert!(validate(&valid).is_ok());
    for ttl in [0, 59, 86401] {
        let mut v = valid.clone();
        v.ttl_seconds = ttl;
        assert!(validate(&v).is_err());
    }
    let mut v = valid.clone();
    v.services.push(v.services[0].clone());
    assert!(validate(&v).is_err());
    let mut v = valid.clone();
    v.consumer = AccessSessionConsumer::Sandbox {
        sandbox_id: String::new(),
    };
    assert!(validate(&v).is_err());
    let mut v = valid.clone();
    v.request_id = Uuid::nil();
    assert!(validate(&v).is_err());
    let mut v = valid.clone();
    v.services.clear();
    assert!(validate(&v).is_err());
    let mut v = valid;
    v.services = (0..17)
        .map(|n| SessionServiceVersion {
            service_id: format!("s{n}"),
            version: 1,
        })
        .collect();
    assert!(validate(&v).is_err());
    assert!(serde_json::from_value::<AccessSessionConsumer>(
        serde_json::json!({"kind":"project","project_id":1})
    )
    .is_err());
}
#[tokio::test]
async fn session_database_errors_are_contextual_and_do_not_leak_driver_data() {
    let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
        .append_query_errors([sea_orm::DbErr::Custom("private driver detail".into())])
        .into_connection();
    let service = ServiceAccessService::new(
        Arc::new(db),
        super::super::tests::encryption(),
        Arc::new(UnavailableTokenlessBroker),
    );
    let error = service
        .list_sessions(42, 1, false, AccessSessionQuery::default())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("42"));
    assert!(error.to_string().contains("find project"));
    assert!(!error.to_string().contains("private driver"));
}
#[tokio::test]
async fn sessions_are_atomic_scoped_expiring_revocable_and_retry_safe() {
    if !temps_test_utils::external_database_configured()
        && !temps_test_utils::docker_available().await
    {
        eprintln!("Skipping Vault sessions: Docker unavailable");
        return;
    }
    let mut test = temps_test_utils::TestDatabase::with_migrations()
        .await
        .unwrap();
    let db = test.connection_arc();
    db.execute_unprepared("INSERT INTO users (id,name,email) VALUES (9701,'Session user','session@example.com'),(9702,'Other user','other@example.com'); INSERT INTO projects (id,name,repo_name,repo_owner,directory,main_branch,preset,slug,created_at,updated_at) VALUES (9701,'Session tests','repo','owner','/','main','nodejs','session-tests',now(),now()),(9702,'Other project','repo','owner','/','main','nodejs','other-session-tests',now(),now()); INSERT INTO sandboxes (id,public_id,name,user_id,project_id,status,backend) VALUES (9701,'sbx_session','session',9701,9701,'running','docker'),(9702,'sbx_other','other',9702,9702,'running','docker')").await.unwrap();
    let encryption = super::super::tests::encryption();
    let service = ServiceAccessService::new(
        db.clone(),
        encryption.clone(),
        Arc::new(UnavailableTokenlessBroker),
    );
    let vault = service
        .save_vault(
            9701,
            None,
            SaveAccessVault {
                name: "Work".into(),
                service_ids: vec![],
                expected_version: None,
            },
        )
        .await
        .unwrap();
    let mut ids = Vec::new();
    for index in 0..16 {
        let v = service
            .save_service(
                9701,
                None,
                SaveAccessService {
                    vault_id: Some(vault.id.clone()),
                    name: format!("API {index}"),
                    policy: SandboxAccessPolicy {
                        provider: None,
                        origin: "https://api.example.com".into(),
                        path_prefixes: vec!["/v1".into()],
                        methods: vec![AccessMethod::Get],
                        github_repository: None,
                    },
                    max_ttl_seconds: 900,
                    enabled: true,
                    credential: Some(SecretString::new("fixture-token")),
                    clear_credential: false,
                    expected_version: None,
                },
            )
            .await
            .unwrap();
        ids.push(v.id);
    }
    let bundle = service
        .save_bundle(
            9701,
            &vault.id,
            None,
            SaveAccessBundle {
                name: "Read APIs".into(),
                service_ids: ids.clone(),
                ttl_seconds: 300,
                expected_version: None,
            },
        )
        .await
        .unwrap();
    let worker = WorkerControl::new(Arc::new(HostAudit));
    AuthorityStore::new(db.clone(), encryption, Arc::new(NoopAuditLogger))
        .register(None, &worker.identity())
        .await
        .unwrap();
    let observed = HostObservation {
        worker: worker.identity(),
        incarnation: Uuid::new_v4(),
        node_id: None,
        container_id: "immutable-session-container".into(),
        container_name: "temps-sandbox-session".into(),
        backend: "docker".into(),
    };
    let mut input = request();
    input.bundle_id = bundle.id.clone();
    input.services = ids
        .iter()
        .map(|id| SessionServiceVersion {
            service_id: id.clone(),
            version: 1,
        })
        .collect();
    let issue = |input: CreateAccessSession, observed: HostObservation| {
        service.authorize_session(9701, &vault.id, 9701, false, input, observed)
    };
    let mut wrong = input.clone();
    wrong.ttl_seconds = 301;
    assert!(issue(wrong, observed.clone()).await.is_err());
    let mut wrong = input.clone();
    wrong.bundle_version = 2;
    assert!(issue(wrong, observed.clone()).await.is_err());
    let mut wrong = input.clone();
    wrong.services.pop();
    assert!(issue(wrong, observed.clone()).await.is_err());
    let mut wrong = input.clone();
    wrong.consumer = AccessSessionConsumer::Sandbox {
        sandbox_id: "sbx_other".into(),
    };
    assert!(issue(wrong, observed.clone()).await.is_err());
    assert!(service
        .authorize_session(
            9701,
            &vault.id,
            9702,
            false,
            input.clone(),
            observed.clone()
        )
        .await
        .is_err());
    // Every selected version must pass validation before any grant is persisted.
    let mut wrong = input.clone();
    wrong.services[1].version = 2;
    assert!(issue(wrong, observed.clone()).await.is_err());
    assert_eq!(grants::Entity::find().count(db.as_ref()).await.unwrap(), 0);
    assert_eq!(
        sessions::Entity::find().count(db.as_ref()).await.unwrap(),
        0
    );
    let issued = issue(input.clone(), observed.clone()).await.unwrap();
    assert_eq!(issued.status, AccessSessionStatus::Issued);
    assert_eq!((issued.expires_at - issued.created_at).num_seconds(), 60);
    let runtime = temps_entities::sandbox_access_runtimes::Entity::find_by_id(observed.incarnation)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        runtime.desired_version, 1,
        "the entire bundle queues one policy delivery"
    );
    let mut excess = input.clone();
    excess.request_id = Uuid::new_v4();
    assert!(issue(excess, observed.clone()).await.is_err());
    assert_eq!(
        sessions::Entity::find().count(db.as_ref()).await.unwrap(),
        1
    );
    assert_batched_session_query_budget(db.as_ref(), &input, &observed, &issued.id).await;
    let retry = issue(input.clone(), observed.clone()).await.unwrap();
    assert_eq!(issued.id, retry.id);
    assert_eq!(issued.expires_at, retry.expires_at);
    assert_eq!(grants::Entity::find().count(db.as_ref()).await.unwrap(), 16);
    let detail = service
        .get_session(9701, &issued.id, 9701, false)
        .await
        .unwrap();
    assert_eq!(detail.grants.len(), 16);
    assert!(detail
        .grants
        .iter()
        .all(|g| g.expires_at == issued.expires_at
            && !g.connected
            && g.enforcement == GrantEnforcement::Pending));
    assert!(service
        .get_session(9701, &issued.id, 9702, false)
        .await
        .is_err());
    assert!(service
        .get_session(9702, &issued.id, 9701, true)
        .await
        .is_err());
    assert_eq!(
        service
            .list_sessions(9701, 9702, false, AccessSessionQuery::default())
            .await
            .unwrap()
            .total,
        0
    );
    assert_eq!(
        service
            .list_sessions(
                9701,
                9701,
                false,
                AccessSessionQuery {
                    vault_id: Some(vault.id.clone()),
                    sandbox_id: Some("sbx_session".into()),
                    ..Default::default()
                }
            )
            .await
            .unwrap()
            .total,
        1
    );
    let mut changed_retry = input.clone();
    changed_retry.ttl_seconds = 61;
    assert!(issue(changed_retry, observed.clone()).await.is_err());
    assert!(service
        .revoke_session(9701, &issued.id, 9702, false)
        .await
        .is_err());
    let revoked = service
        .revoke_session(9701, &issued.id, 9701, false)
        .await
        .unwrap();
    assert_eq!(revoked.status, AccessSessionStatus::Revoked);
    assert_eq!(
        service
            .revoke_session(9701, &issued.id, 9701, false)
            .await
            .unwrap()
            .revoked_at,
        revoked.revoked_at
    );
    assert!(service
        .get_session(9701, &issued.id, 9701, false)
        .await
        .unwrap()
        .grants
        .iter()
        .all(|g| g.revoked_at.is_some() && !g.connected));
    assert_eq!(
        issue(input.clone(), observed.clone()).await.unwrap().status,
        AccessSessionStatus::Revoked
    );
    // Time projection changes state at the hard deadline without a scheduler tick.
    let mut row = sessions::Entity::find_by_id(&issued.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();
    row.revoked_at = None;
    assert_eq!(
        response(row.clone(), row.expires_at).status,
        AccessSessionStatus::Expired
    );
    test.cleanup().await;
}

// Exercise the public session path with a query budget, using the integration
// fixture's real rows. Increasing bundle size must not add database round trips.
async fn assert_batched_session_query_budget(
    db: &DatabaseConnection,
    input: &CreateAccessSession,
    observed: &HostObservation,
    session_id: &str,
) {
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
    use std::collections::BTreeMap;
    use temps_entities::{access_vaults, sandbox_access_authorities, sandbox_access_runtimes};
    let session = sessions::Entity::find_by_id(session_id)
        .one(db)
        .await
        .unwrap()
        .unwrap();
    let vault = access_vaults::Entity::find_by_id(&session.vault_id)
        .one(db)
        .await
        .unwrap()
        .unwrap();
    let bundle = bundles::Entity::find_by_id(&input.bundle_id)
        .one(db)
        .await
        .unwrap()
        .unwrap();
    let sandbox = sandboxes::Entity::find_by_id(session.sandbox_id)
        .one(db)
        .await
        .unwrap()
        .unwrap();
    let authority = sandbox_access_authorities::Entity::find_by_id(observed.worker.boot_id)
        .one(db)
        .await
        .unwrap()
        .unwrap();
    let runtime = sandbox_access_runtimes::Entity::find_by_id(observed.incarnation)
        .one(db)
        .await
        .unwrap()
        .unwrap();
    let services = services::Entity::find()
        .filter(services::Column::ProjectId.eq(session.project_id))
        .order_by_asc(services::Column::Id)
        .all(db)
        .await
        .unwrap();
    let scalar = |name: &str, value: sea_orm::Value| BTreeMap::from([(name.to_owned(), value)]);
    let mut counts = Vec::new();
    for size in [2, 16] {
        let selected = services[..size].to_vec();
        let mut request = input.clone();
        request.request_id = Uuid::new_v4();
        request.services = selected
            .iter()
            .map(|s| SessionServiceVersion {
                service_id: s.id.clone(),
                version: s.version,
            })
            .collect();
        let mut bundle = bundle.clone();
        bundle.service_ids = selected.iter().map(|s| s.id.clone()).collect();
        let mock = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![scalar("id", session.project_id.into())]])
            .append_query_results([vec![vault.clone()]])
            .append_query_results([vec![sandbox.clone()]])
            .append_query_results([Vec::<sessions::Model>::new()])
            .append_query_results([vec![bundle]])
            .append_query_results([selected])
            .append_query_results([vec![sandbox.clone()]])
            .append_query_results([vec![authority.clone()]])
            .append_query_results([vec![runtime.clone()]])
            .append_query_results([vec![scalar("count", 0_i64.into())]])
            .append_query_results([vec![scalar("desired_version", 2_i64.into())]])
            .append_query_results([vec![session.clone()]])
            .append_exec_results([
                MockExecResult {
                    last_insert_id: 0,
                    rows_affected: size as u64,
                },
                MockExecResult {
                    last_insert_id: 0,
                    rows_affected: size as u64,
                },
            ])
            .into_connection();
        let service = ServiceAccessService::new(
            Arc::new(mock),
            super::super::tests::encryption(),
            Arc::new(UnavailableTokenlessBroker),
        );
        service
            .authorize_session(
                session.project_id,
                &vault.id,
                session.created_by.unwrap(),
                false,
                request,
                observed.clone(),
            )
            .await
            .unwrap();
        let log = Arc::try_unwrap(service.db).unwrap().into_transaction_log();
        let statements: Vec<_> = log.iter().flat_map(|tx| tx.statements()).collect();
        let selects = statements
            .iter()
            .filter(|s| s.sql.starts_with("SELECT"))
            .count();
        assert!(selects <= 10, "batch {size} used {selects} reads");
        counts.push(statements.len());
    }
    assert_eq!(
        counts[0], counts[1],
        "queries must not grow with service count"
    );
}
