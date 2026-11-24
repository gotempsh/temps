// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use sea_orm::{ConnectionTrait, DatabaseBackend, MockDatabase};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

mod credential_isolation;

#[tokio::test]
async fn linked_sessions_plan_projectless_and_direct_access_and_retire_after_unlink() {
    if !temps_test_utils::external_database_configured()
        && !temps_test_utils::docker_available().await
    {
        eprintln!("Skipping linked session planner: database unavailable");
        return;
    }
    use crate::{
        authority::AuthorityStore,
        broker::UnavailableTokenlessBroker,
        service::{HostObservation, ServiceAccessService},
        session_types::*,
        types::*,
        vault_types::*,
    };
    use temps_service_access_runtime::{HostAudit, WorkerControl};
    for mode in [
        "direct",
        "deleted_project",
        "deleted_before",
        "projectless",
        "legacy",
    ] {
        let mut test = temps_test_utils::TestDatabase::with_migrations()
            .await
            .unwrap();
        let db = test.connection_arc();
        db.execute_unprepared("INSERT INTO users(id,name,email) VALUES(9703,'Fixture','planner@example.com'); INSERT INTO projects(id,name,repo_name,repo_owner,directory,main_branch,preset,slug,created_at,updated_at) VALUES(9703,'Fixture','repo','owner','/','main','nodejs','linked-planner',now(),now()); INSERT INTO sandboxes(id,public_id,name,user_id,project_id,status,backend) VALUES(9703,'sbx_linked_planner','Fixture',9703,9703,'running','docker')").await.unwrap();
        if mode == "projectless" {
            db.execute_unprepared("UPDATE sandboxes SET project_id=NULL WHERE id=9703")
                .await
                .unwrap();
        }
        let service = ServiceAccessService::new(
            db.clone(),
            encryption(),
            Arc::new(UnavailableTokenlessBroker),
        );
        let vault = service
            .save_vault(
                None,
                None,
                SaveAccessVault {
                    name: "Fixture".into(),
                    service_ids: vec![],
                    expected_version: None,
                },
            )
            .await
            .unwrap();
        let definition = service
            .save_service(
                None,
                None,
                SaveAccessService {
                    vault_id: Some(vault.id.clone()),
                    name: "Fixture API".into(),
                    policy: SandboxAccessPolicy {
                        provider: None,
                        github_repository: None,
                        origin: "https://api.example.com".into(),
                        path_prefixes: vec!["/v1".into()],
                        methods: vec![AccessMethod::Get],
                        delivery: None,
                    },
                    max_ttl_seconds: 300,
                    enabled: true,
                    credential: Some(SecretString::new("fixture-token")),
                    clear_credential: false,
                    expected_version: None,
                },
            )
            .await
            .unwrap();
        let bundle = service
            .save_bundle(
                None,
                &vault.id,
                None,
                SaveAccessBundle {
                    name: "Fixture bundle".into(),
                    service_ids: vec![definition.id.clone()],
                    ttl_seconds: 120,
                    expected_version: None,
                },
            )
            .await
            .unwrap();
        let link = service
            .link_vault(
                &vault.id,
                if mode == "legacy" { Some(9703) } else { None },
                if mode == "legacy" { None } else { Some(9703) },
            )
            .await
            .unwrap();
        let link_id = if mode == "legacy" {
            let id = format!("link_legacy_{}", vault.id);
            db.execute(sea_orm::Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE access_vault_links SET id=$1 WHERE id=$2",
                [id.clone().into(), link.id.into()],
            ))
            .await
            .unwrap();
            id
        } else {
            link.id
        };
        let worker = WorkerControl::new(Arc::new(HostAudit));
        let identity = worker.identity();
        AuthorityStore::new(
            db.clone(),
            encryption(),
            Arc::new(temps_core::NoopAuditLogger),
        )
        .register(None, &identity)
        .await
        .unwrap();
        if mode == "deleted_before" {
            db.execute_unprepared("DELETE FROM projects WHERE id=9703")
                .await
                .unwrap();
        }
        let approved = service
            .authorize_session(
                None,
                &vault.id,
                9703,
                false,
                CreateAccessSession {
                    authorized_link_id: Some(link_id.clone()),
                    request_id: Uuid::new_v4(),
                    bundle_id: bundle.id,
                    bundle_version: bundle.version,
                    consumer: AccessSessionConsumer::Sandbox {
                        sandbox_id: "sbx_linked_planner".into(),
                    },
                    ttl_seconds: 120,
                    services: vec![SessionServiceVersion {
                        service_id: definition.id,
                        version: definition.version,
                    }],
                },
                HostObservation {
                    worker: identity.clone(),
                    incarnation: Uuid::new_v4(),
                    node_id: None,
                    container_id: "immutable-linked-container".into(),
                    container_name: "temps-sandbox-linked_planner".into(),
                    backend: "docker".into(),
                },
            )
            .await
            .unwrap();
        if mode == "legacy" {
            db.execute(sea_orm::Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE access_sessions SET link_id=NULL,legacy_project_link=true WHERE id=$1",
                [approved.id.clone().into()],
            ))
            .await
            .unwrap();
        }
        if mode == "deleted_project" {
            db.execute_unprepared("DELETE FROM projects WHERE id=9703")
                .await
                .unwrap();
        }
        if mode == "deleted_project" {
            // Also cover rows cleared by the preceding SET NULL migration.
            db.execute_unprepared("UPDATE sandbox_service_grants SET project_id=NULL; UPDATE sandbox_access_runtime_records SET project_id=NULL").await.unwrap();
            let detail = service
                .get_session(None, &approved.id, 9703, false)
                .await
                .unwrap();
            assert_eq!(detail.grants[0].status, ServiceGrantStatus::Approved);
        }
        let checker = Checker::new(0);
        checker.deny.store(mode != "legacy", Ordering::SeqCst);
        let planner = PolicyPlanner::new(
            db.clone(),
            encryption(),
            Some(checker.clone()),
            Arc::new(crate::github::NoGithub),
        );
        let plan = planner.prepare(identity.boot_id).await.unwrap();
        assert_eq!(
            plan.bindings.len(),
            1,
            "{mode} must reach the worker planner"
        );
        assert!(plan.retire.is_empty());
        if mode == "legacy" {
            checker.deny.store(true, Ordering::SeqCst);
            assert!(
                planner
                    .prepare(identity.boot_id)
                    .await
                    .unwrap()
                    .bindings
                    .is_empty(),
                "legacy project links still require current project permission"
            );
            checker.deny.store(false, Ordering::SeqCst);
        } else {
            assert_eq!(
                checker.batches.load(Ordering::SeqCst),
                0,
                "direct links do not require project membership"
            );
        }
        service.unlink_vault(&vault.id, &link_id).await.unwrap();
        let revoked = planner.prepare(identity.boot_id).await.unwrap();
        assert!(
            revoked.bindings.is_empty(),
            "{mode} cannot retain revoked credentials"
        );
        assert_eq!(revoked.retire.len(), 1);
        test.cleanup().await;
    }
}

fn encryption() -> Arc<EncryptionService> {
    Arc::new(EncryptionService::new(&"ab".repeat(32)).unwrap())
}
fn planner(checker: Option<Arc<dyn ProjectAccessChecker>>) -> PolicyPlanner {
    PolicyPlanner::new(
        Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
        encryption(),
        checker,
        Arc::new(crate::github::NoGithub),
    )
}
fn candidate(user: i32, project: i32) -> Candidate {
    Candidate {
        runtime_id: Uuid::new_v4(),
        project_id: Some(project),
        approved_by: user,
        admin: false,
        grant_id: format!("grant_{}", Uuid::new_v4().simple()),
        service_id: format!("svc_{}", Uuid::new_v4().simple()),
        service_version: 1,
        policy: serde_json::json!({}),
        max_ttl_seconds: 60,
        expires_at: Utc::now() + chrono::Duration::seconds(60),
        credential_encrypted: Some("secret-ciphertext".into()),
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
        BTreeMap::from([((1, 1), false), ((1, 2), false), ((2, 1), false)])
    );
    assert_eq!(checker.batches.load(Ordering::SeqCst), 2);
    checker.deny.store(true, Ordering::SeqCst);
    assert!(planner.recheck(boot, &candidates).await.unwrap().is_empty());
    assert_eq!(planner.recheck(boot, &[]).await.unwrap(), BTreeMap::new());
}

#[tokio::test]
async fn permission_fallback_and_admin_bypass_match_handler_contract() {
    let candidates = [candidate(1, 1), candidate(1, 2)];
    assert_eq!(
        planner(Some(Checker::new(3)))
            .recheck(Uuid::new_v4(), &candidates)
            .await
            .unwrap(),
        BTreeMap::from([((1, 1), true)])
    );
    assert_eq!(
        planner(None)
            .recheck(Uuid::new_v4(), &candidates)
            .await
            .unwrap(),
        BTreeMap::from([((1, 1), true), ((1, 2), true)])
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
    let error = PolicyPlanner::new(
        Arc::new(db),
        encryption(),
        None,
        Arc::new(crate::github::NoGithub),
    )
    .prepare(boot)
    .await
    .unwrap_err();
    assert!(error.to_string().contains("missing or closed"));
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_errors([sea_orm::DbErr::Custom("private database detail".into())])
        .into_connection();
    let error = PolicyPlanner::new(
        Arc::new(db),
        encryption(),
        None,
        Arc::new(crate::github::NoGithub),
    )
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
    let planner = PolicyPlanner::new(
        db.clone(),
        encryption(),
        Some(checker.clone()),
        Arc::new(crate::github::NoGithub),
    );
    let incarnation = Uuid::new_v4();
    let mut grants = Vec::new();
    let mut services = Vec::new();
    for origin in ["https://api.github.com", "https://api.example.com"] {
        let definition = service
            .save_service(
                9701,
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
                        delivery: None,
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
    struct UnavailableGithub {
        calls: AtomicUsize,
        stall: bool,
    }
    #[async_trait::async_trait]
    impl crate::github::GithubCredentialResolver for UnavailableGithub {
        async fn resolve(
            &self,
            connection_id: i32,
        ) -> Result<SecretString, crate::github::GithubCredentialError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.stall {
                std::future::pending::<()>().await;
            }
            Err(crate::github::GithubCredentialError { connection_id })
        }
    }
    for stall in [false, true] {
        let resolver = Arc::new(UnavailableGithub {
            calls: AtomicUsize::new(0),
            stall,
        });
        let isolated = PolicyPlanner::new(
            db.clone(),
            encryption(),
            Some(checker.clone()),
            resolver.clone(),
        );
        let (authority, mut runtimes, mut candidates) =
            planner.read(identity.boot_id).await.unwrap();
        candidates[0].admin = true; // Credential lookup must actually be attempted.
        candidates[0].policy["github_repository"] = serde_json::json!({"connection_id": 1,"owner":"example","repository":"repo","protocol":"api"});
        // Put the healthy grant in another sandbox/project on the same worker.
        let mut other = runtimes[0].clone();
        other.id = Uuid::new_v4();
        other.project_id = other.project_id.map(|id| id + 1);
        other.sandbox_id += 1;
        candidates[1].runtime_id = other.id;
        candidates[1].project_id = other.project_id;
        let healthy_id = other.id;
        runtimes.push(other);
        let degraded = tokio::time::timeout(
            Duration::from_secs(5),
            isolated.prepare_rows(identity.boot_id, authority, runtimes, candidates),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(resolver.calls.load(Ordering::SeqCst), 1);
        assert!(
            degraded.retire.is_empty(),
            "credential outages must not revoke consent"
        );
        assert_eq!(degraded.bindings.len(), 1);
        assert_eq!(
            degraded.bindings[0].plan.target().incarnation_id,
            healthy_id
        );
        assert_eq!(
            degraded.bindings[0].plan.grants().len(),
            1,
            "healthy bearer grant survives GitHub outage"
        );
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
    let applied_ids: Vec<String> = plan.bindings[0]
        .plan
        .grants()
        .iter()
        .map(|g| g.grant_id().to_owned())
        .collect();
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
    for _ in 0..2 {
        let claims = queue.claim(32).await.unwrap();
        assert_eq!(claims.len(), 1);
        let claim = &claims[0];
        if claim.command().desired_version == 1 {
            queue.supersede(claim).await.unwrap();
        } else {
            assert!(queue.supersede(claim).await.is_err());
            for deadline in [Utc::now(), Utc::now() + chrono::Duration::seconds(31)] {
                assert!(queue
                    .complete_applied(claim, revision, deadline, &applied_ids)
                    .await
                    .is_err());
            }
            assert!(queue
                .complete_applied(claim, revision + 1, issued.lease_expires_at(), &applied_ids)
                .await
                .is_err());
            queue
                .complete_applied(claim, revision, issued.lease_expires_at(), &applied_ids)
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
    let slow = PolicyPlanner::new(
        db.clone(),
        encryption(),
        Some(Checker::new(4)),
        Arc::new(crate::github::NoGithub),
    );
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

#[tokio::test]
async fn deferred_project_permissions_fail_closed_then_follow_current_checker() {
    let binding = Arc::new(ProjectAccessBinding::default());
    let mut planner = planner(None);
    planner.bind_project_access(binding.clone());
    let boot = Uuid::new_v4();
    let grants = [candidate(1, 1)];
    assert!(planner
        .recheck(boot, &grants)
        .await
        .unwrap_err()
        .to_string()
        .contains("not been initialized"));
    let checker = Checker::new(0);
    binding.0.get_or_init(|| Some(checker.clone()));
    assert_eq!(planner.recheck(boot, &grants).await.unwrap().len(), 1);
    checker.deny.store(true, Ordering::SeqCst);
    assert!(planner.recheck(boot, &grants).await.unwrap().is_empty());
    assert_eq!(checker.batches.load(Ordering::SeqCst), 2);

    let oss = Arc::new(ProjectAccessBinding::default());
    planner.bind_project_access(oss.clone());
    oss.0.get_or_init(|| None);
    assert_eq!(planner.recheck(boot, &grants).await.unwrap().len(), 1);
}
