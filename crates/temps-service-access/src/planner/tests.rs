// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use sea_orm::{ConnectionTrait, DatabaseBackend, MockDatabase};
use std::{
    collections::BTreeSet,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

fn encryption() -> Arc<EncryptionService> {
    Arc::new(EncryptionService::new(&"ab".repeat(32)).unwrap())
}
fn planner(checker: Option<Arc<dyn ProjectAccessChecker>>) -> PolicyPlanner {
    PolicyPlanner::new(
        Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
        encryption(),
        checker,
    )
}
fn candidate(user: i32, project: i32) -> Candidate {
    Candidate {
        runtime_id: Uuid::new_v4(),
        project_id: project,
        approved_by: user,
        admin: false,
        grant_id: format!("grant_{}", Uuid::new_v4().simple()),
        service_id: format!("svc_{}", Uuid::new_v4().simple()),
        service_version: 1,
        policy: serde_json::json!({}),
        max_ttl_seconds: 60,
        expires_at: Utc::now() + chrono::Duration::seconds(60),
        credential_encrypted: "secret-ciphertext".into(),
    }
}
struct Checker {
    mode: u8,
    batches: AtomicUsize,
    deny: AtomicBool,
}
impl Checker {
    fn new(mode: u8) -> Arc<Self> {
        Arc::new(Self {
            mode,
            batches: AtomicUsize::new(0),
            deny: AtomicBool::new(false),
        })
    }
}
#[async_trait::async_trait]
impl ProjectAccessChecker for Checker {
    async fn user_can_access_project(
        &self,
        _user: i32,
        project: i32,
    ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        Ok(project == 1)
    }
    async fn effective_project_permissions_batch(
        &self,
        _user: i32,
        projects: &[i32],
    ) -> Result<BTreeMap<i32, Option<Vec<String>>>, Box<dyn std::error::Error + Send + Sync>> {
        self.batches.fetch_add(1, Ordering::SeqCst);
        match self.mode {
            1 => Ok(BTreeMap::new()),
            2 => Err(std::io::Error::other("private checker detail").into()),
            3 => Ok(projects.iter().map(|id| (*id, None)).collect()),
            4 => std::future::pending().await,
            _ => Ok(projects
                .iter()
                .map(|id| {
                    (
                        *id,
                        Some(if self.deny.load(Ordering::SeqCst) {
                            vec!["projects:write".into()]
                        } else {
                            vec!["projects:write".into(), "sandboxes:write".into()]
                        }),
                    )
                })
                .collect()),
        }
    }
}

#[tokio::test]
async fn permission_batches_deduplicate_and_require_both_scoped_permissions() {
    let checker = Checker::new(0);
    let planner = planner(Some(checker.clone()));
    let boot = Uuid::new_v4();
    let candidates = [
        candidate(1, 1),
        candidate(1, 1),
        candidate(1, 2),
        candidate(2, 1),
    ];
    assert_eq!(
        planner.recheck(boot, &candidates).await.unwrap(),
        BTreeSet::from([(1, 1), (1, 2), (2, 1)])
    );
    assert_eq!(checker.batches.load(Ordering::SeqCst), 2);
    checker.deny.store(true, Ordering::SeqCst);
    assert!(planner.recheck(boot, &candidates).await.unwrap().is_empty());
    assert_eq!(planner.recheck(boot, &[]).await.unwrap(), BTreeSet::new());
}

#[tokio::test]
async fn permission_fallback_and_admin_bypass_match_handler_contract() {
    let candidates = [candidate(1, 1), candidate(1, 2)];
    assert_eq!(
        planner(Some(Checker::new(3)))
            .recheck(Uuid::new_v4(), &candidates)
            .await
            .unwrap(),
        BTreeSet::from([(1, 1)])
    );
    assert_eq!(
        planner(None)
            .recheck(Uuid::new_v4(), &candidates)
            .await
            .unwrap(),
        BTreeSet::from([(1, 1), (1, 2)])
    );
    let mut admin = candidate(2, 3);
    admin.admin = true;
    let checker = Checker::new(2);
    assert!(planner(Some(checker.clone()))
        .recheck(Uuid::new_v4(), &[admin])
        .await
        .unwrap()
        .is_empty());
    assert_eq!(checker.batches.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn incomplete_or_failed_permission_batches_fail_closed_without_leaking_errors() {
    for mode in [1, 2] {
        let boot = Uuid::new_v4();
        let error = planner(Some(Checker::new(mode)))
            .recheck(boot, &[candidate(1, 1)])
            .await
            .unwrap_err();
        assert!(error.to_string().contains(&boot.to_string()));
        assert!(!error.to_string().contains("private checker detail"));
    }
}

#[tokio::test]
async fn invalid_missing_and_failed_authority_reads_have_context() {
    assert!(planner(None)
        .prepare(Uuid::nil())
        .await
        .unwrap_err()
        .to_string()
        .contains("non-nil"));
    let boot = Uuid::new_v4();
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([Vec::<temps_entities::sandbox_access_authorities::Model>::new()])
        .into_connection();
    let error = PolicyPlanner::new(Arc::new(db), encryption(), None)
        .prepare(boot)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("missing or closed"));
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_errors([sea_orm::DbErr::Custom("private database detail".into())])
        .into_connection();
    let error = PolicyPlanner::new(Arc::new(db), encryption(), None)
        .prepare(boot)
        .await
        .unwrap_err();
    assert!(error.to_string().contains(&boot.to_string()));
    assert!(error.to_string().contains("read authority"));
    assert!(!error.to_string().contains("private database detail"));
}

#[tokio::test]
async fn postgres_planner_rechecks_consent_and_preserves_other_services_on_revocation() {
    if !temps_test_utils::external_database_configured()
        && !temps_test_utils::docker_available().await
    {
        eprintln!("Skipping policy planner: Docker unavailable");
        return;
    }
    let mut test = temps_test_utils::TestDatabase::with_migrations()
        .await
        .unwrap();
    let db = test.connection_arc();
    db.execute_unprepared("INSERT INTO users (id,name,email) VALUES (9701,'Test user','user@example.com'); INSERT INTO projects (id,name,repo_name,repo_owner,directory,main_branch,preset,slug,created_at,updated_at) VALUES (9701,'Planner','repo','owner','/','main','nodejs','planner',now(),now()); INSERT INTO sandboxes (id,public_id,name,user_id,project_id,status,backend) VALUES (9701,'sbx_planner','planner',9701,9701,'running','docker')").await.unwrap();
    use crate::{
        authority::AuthorityStore,
        broker::UnavailableTokenlessBroker,
        service::{FreshConsent, HostObservation, ServiceAccessService},
        types::{AccessMethod, RequestServiceGrant, SaveAccessService},
    };
    use temps_service_access_runtime::{HostAudit, SessionDelivery, WorkerControl};
    let service = ServiceAccessService::new(
        db.clone(),
        encryption(),
        Arc::new(UnavailableTokenlessBroker),
    );
    let store = AuthorityStore::new(
        db.clone(),
        encryption(),
        Arc::new(temps_core::NoopAuditLogger),
    );
    let worker = WorkerControl::new(Arc::new(HostAudit));
    let identity = worker.identity();
    worker
        .configure(store.register(None, &identity).await.unwrap())
        .await
        .unwrap();
    let checker = Checker::new(0);
    let planner = PolicyPlanner::new(db.clone(), encryption(), Some(checker.clone()));
    let incarnation = Uuid::new_v4();
    let mut grants = Vec::new();
    let mut services = Vec::new();
    for origin in ["https://api.github.com", "https://api.example.com"] {
        let definition = service
            .save_service(
                9701,
                None,
                SaveAccessService {
                    name: "API".into(),
                    policy: SandboxAccessPolicy {
                        origin: origin.into(),
                        path_prefixes: vec!["/user".into()],
                        methods: vec![AccessMethod::Get],
                    },
                    max_ttl_seconds: 60,
                    enabled: true,
                    credential: Some(SecretString::new("upstream-fixture-token")),
                    clear_credential: false,
                    expected_version: None,
                },
            )
            .await
            .unwrap();
        let request = || RequestServiceGrant {
            service_id: definition.id.clone(),
            service_version: 1,
            ttl_seconds: 60,
        };
        // An old saved intent never appears in the worker's credential policy.
        service
            .request_grant(9701, "sbx_planner", 9701, false, request())
            .await
            .unwrap();
        let approved = service
            .authorize_grant(
                FreshConsent {
                    request_id: Uuid::new_v4(),
                    project_id: 9701,
                    sandbox_id: "sbx_planner".into(),
                    user_id: 9701,
                    admin: false,
                    request: request(),
                },
                HostObservation {
                    worker: identity.clone(),
                    incarnation,
                    node_id: None,
                    container_id: "immutable-planner-container".into(),
                    container_name: "temps-sandbox-planner".into(),
                    backend: "docker".into(),
                },
            )
            .await
            .unwrap();
        grants.push(approved);
        services.push(definition);
    }
    let plan = planner.prepare(identity.boot_id).await.unwrap();
    assert_eq!(plan.bindings.len(), 1);
    assert_eq!(plan.bindings[0].plan.grants().len(), 2);
    assert_eq!(plan.bindings[0].credentials.len(), 2);
    assert!(!format!("{plan:?}").contains("upstream-fixture-token"));
    for grant in &grants {
        let prepared = plan.bindings[0]
            .plan
            .grants()
            .iter()
            .find(|g| g.grant_id() == grant.grant_id)
            .unwrap();
        assert!(prepared.claims().exp <= grant.expires_at);
        assert!(prepared.claims().exp > grant.expires_at - chrono::Duration::seconds(2));
    }
    let revision = plan.expected_revision;
    let issued = store
        .issue(
            plan.node_id,
            &plan.worker,
            plan.expected_revision,
            plan.bindings,
        )
        .await
        .unwrap();
    let delivery = issued.delivery(incarnation).unwrap();
    let version = worker
        .install(identity.boot_id, &delivery.policy.into_snapshot().unwrap())
        .await
        .unwrap();
    let binding = worker
        .bind(
            identity.boot_id,
            SessionDelivery::binding_spec(
                incarnation.to_string(),
                delivery.session,
                delivery.version,
            ),
            version,
        )
        .await
        .unwrap();
    binding.ready(version).await.unwrap();
    let queue = crate::delivery::DeliveryQueue::new(db.clone());
    let claims = queue.claim(32).await.unwrap();
    assert_eq!(claims.len(), 2);
    for claim in &claims {
        if claim.command().desired_version == 1 {
            queue.supersede(claim).await.unwrap();
        } else {
            assert!(queue.supersede(claim).await.is_err());
            for deadline in [Utc::now(), Utc::now() + chrono::Duration::seconds(31)] {
                assert!(queue
                    .complete_applied(claim, revision, deadline)
                    .await
                    .is_err());
            }
            assert!(queue
                .complete_applied(claim, revision + 1, issued.lease_expires_at())
                .await
                .is_err());
            queue
                .complete_applied(claim, revision, issued.lease_expires_at())
                .await
                .unwrap();
        }
    }
    use sea_orm::EntityTrait;
    let applied = sandbox_access_runtimes::Entity::find_by_id(incarnation)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(applied.applied_version, 2);
    // PostgreSQL timestamps have microsecond precision; never extend the lease
    // based on how long the actual worker acknowledgement took to arrive.
    assert!(
        (applied.lease_expires_at.unwrap() - issued.lease_expires_at())
            .num_microseconds()
            .unwrap()
            .abs()
            <= 1
    );
    assert!(queue.claim(32).await.unwrap().is_empty());

    let stale = planner.prepare(identity.boot_id).await.unwrap();
    service
        .revoke_grant(9701, "sbx_planner", &grants[0].grant_id, 9701, false, 1)
        .await
        .unwrap();
    assert!(store
        .issue(
            stale.node_id,
            &stale.worker,
            stale.expected_revision,
            stale.bindings
        )
        .await
        .is_err());
    let remaining = planner.prepare(identity.boot_id).await.unwrap();
    assert_eq!(remaining.bindings[0].plan.grants().len(), 1);
    assert_eq!(
        remaining.bindings[0].plan.grants()[0].grant_id(),
        grants[1].grant_id
    );
    checker.deny.store(true, Ordering::SeqCst);
    let denied = planner.prepare(identity.boot_id).await.unwrap();
    assert!(denied.bindings.is_empty());
    assert_eq!(denied.retire[0].id, incarnation);
    checker.deny.store(false, Ordering::SeqCst);

    for (invalidate, restore) in [
        (
            "UPDATE projects SET is_deleted=true",
            "UPDATE projects SET is_deleted=false",
        ),
        (
            "UPDATE users SET deleted_at=now()",
            "UPDATE users SET deleted_at=NULL",
        ),
        (
            "UPDATE sandboxes SET status='stopped'",
            "UPDATE sandboxes SET status='running'",
        ),
        (
            "UPDATE sandboxes SET backend='firecracker'",
            "UPDATE sandboxes SET backend='docker'",
        ),
        (
            "UPDATE sandbox_access_services SET enabled=false",
            "UPDATE sandbox_access_services SET enabled=true",
        ),
    ] {
        db.execute_unprepared(invalidate).await.unwrap();
        let denied = planner.prepare(identity.boot_id).await.unwrap();
        assert!(denied.bindings.is_empty(), "{invalidate}");
        assert_eq!(denied.retire.len(), 1);
        db.execute_unprepared(restore).await.unwrap();
        assert_eq!(
            planner
                .prepare(identity.boot_id)
                .await
                .unwrap()
                .bindings
                .len(),
            1
        );
    }
    let slow = PolicyPlanner::new(db.clone(), encryption(), Some(Checker::new(4)));
    let error = tokio::time::timeout(Duration::from_secs(12), slow.prepare(identity.boot_id))
        .await
        .unwrap()
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("authorization deadline exceeded"));
    drop(slow);

    // Changing policy requires new consent, rather than upgrading an old grant.
    let stale = planner.prepare(identity.boot_id).await.unwrap();
    db.execute_unprepared("UPDATE sandbox_access_services SET version=version+1")
        .await
        .unwrap();
    assert!(store
        .issue(
            stale.node_id,
            &stale.worker,
            stale.expected_revision,
            stale.bindings
        )
        .await
        .is_err());
    assert!(planner
        .prepare(identity.boot_id)
        .await
        .unwrap()
        .bindings
        .is_empty());
    binding
        .revoke_and_wait(Duration::from_secs(5))
        .await
        .unwrap();
    assert!(binding.ready(version).await.is_err());
    drop(planner);
    drop(service);
    drop(store);
    drop(worker);
    drop(db);
    test.cleanup().await;
}
