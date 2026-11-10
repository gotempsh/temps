// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use sea_orm::{DatabaseBackend, MockDatabase};

pub(crate) fn policy(protocol: GithubAccessProtocol) -> SandboxAccessPolicy {
    let (origin, paths, methods) = match protocol {
        GithubAccessProtocol::Api => (
            "https://api.github.com",
            vec!["/repos/owner/repo".into()],
            vec![AccessMethod::Get, AccessMethod::Head],
        ),
        GithubAccessProtocol::Git => (
            "https://github.com",
            vec!["/owner/repo".into(), "/owner/repo.git".into()],
            vec![AccessMethod::Get, AccessMethod::Post],
        ),
    };
    SandboxAccessPolicy {
        provider: None,
        origin: origin.into(),
        path_prefixes: paths,
        methods,
        github_repository: Some(GithubRepositoryAccess {
            connection_id: 9901,
            owner: "owner".into(),
            repository: "repo".into(),
            protocol,
        }),
    }
}

#[test]
fn github_profiles_limit_credentials_to_one_repository_and_deny_push() {
    let (_, api) = crate::tokenless::compile_policy(&policy(GithubAccessProtocol::Api)).unwrap();
    assert!(api.git.is_none());
    let (_, git) = crate::tokenless::compile_policy(&policy(GithubAccessProtocol::Git)).unwrap();
    let git = git.git.unwrap();
    for repository in ["repo", "repo.git"] {
        assert!(git.allows(
            "GET",
            &format!("/owner/{repository}/info/refs"),
            Some("service=git-upload-pack")
        ));
        assert!(git.allows(
            "POST",
            &format!("/owner/{repository}/git-upload-pack"),
            None
        ));
        assert!(!git.allows(
            "GET",
            &format!("/owner/{repository}/info/refs"),
            Some("service=git-receive-pack")
        ));
        assert!(!git.allows(
            "POST",
            &format!("/owner/{repository}/git-receive-pack"),
            None
        ));
    }
    assert!(!git.allows(
        "GET",
        "/other/repo/info/refs",
        Some("service=git-upload-pack")
    ));
    assert!(!git.allows("POST", "/owner/repo.git/info/lfs/objects/batch", None));
    for protocol in [GithubAccessProtocol::Api, GithubAccessProtocol::Git] {
        for field in 0..5 {
            let mut p = policy(protocol);
            match field {
                0 => p.origin = "https://example.com".into(),
                1 => p.path_prefixes = vec!["/".into()],
                2 => p.methods.push(AccessMethod::Delete),
                3 => p.github_repository.as_mut().unwrap().connection_id = 0,
                _ => p.github_repository.as_mut().unwrap().repository = "../other".into(),
            }
            assert!(
                crate::tokenless::compile_policy(&p).is_err(),
                "case {field}"
            );
        }
    }
}

#[tokio::test]
async fn binding_errors_retain_project_operation_without_exposing_database_details() {
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_errors([sea_orm::DbErr::Custom("private driver value".into())])
        .into_connection();
    let error = validate_binding(
        &db,
        7,
        policy(GithubAccessProtocol::Api)
            .github_repository
            .as_ref()
            .unwrap(),
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("7"));
    assert!(error.to_string().contains("validate project GitHub"));
    assert!(!error.to_string().contains("private driver value"));
}

#[tokio::test]
async fn postgres_github_binding_requires_selected_repository_and_active_provider() {
    if !temps_test_utils::external_database_configured()
        && !temps_test_utils::docker_available().await
    {
        eprintln!("Skipping GitHub binding test: Docker unavailable");
        return;
    }
    let mut test = temps_test_utils::TestDatabase::with_migrations()
        .await
        .unwrap();
    let db = test.connection_arc();
    db.execute_unprepared("INSERT INTO git_providers (id,name,provider_type,auth_method,auth_config,is_active,is_default,created_at,updated_at) VALUES (9901,'GitHub','github','pat','{}',true,false,now(),now()); INSERT INTO git_provider_connections (id,provider_id,account_name,account_type,is_active,is_expired,created_at,updated_at) VALUES (9901,9901,'test-account','User',true,false,now(),now()); INSERT INTO projects (id,name,repo_name,repo_owner,git_provider_connection_id,directory,main_branch,preset,slug,created_at,updated_at) VALUES (9901,'Source','repo','owner',9901,'/','main','nodejs','source',now(),now())").await.unwrap();

    db.execute_unprepared("INSERT INTO repositories (git_provider_connection_id,owner,name,full_name,private,fork,created_at,updated_at,pushed_at,size,stargazers_count,watchers_count,default_branch,open_issues_count,topics,repo_object) VALUES (9901,'owner','repo','owner/repo',true,false,now(),now(),now(),0,0,0,'main',0,'[]','{}')").await.unwrap();
    let source = policy(GithubAccessProtocol::Api).github_repository.unwrap();
    db.execute_unprepared(
        "UPDATE projects SET repo_name='different', git_provider_connection_id=NULL WHERE id=9901",
    )
    .await
    .unwrap();
    assert!(validate_binding(db.as_ref(), 9901, &source).await.is_ok());
    assert!(validate_binding(db.as_ref(), 9902, &source).await.is_err());
    let selected = vec![source.clone(); 16];
    assert!(validate_bindings(db.as_ref(), 9901, &selected)
        .await
        .is_ok());
    let mut missing = selected.clone();
    missing[15].repository = "missing".into();
    assert!(validate_bindings(db.as_ref(), 9901, &missing)
        .await
        .is_err());
    assert!(validate_bindings(db.as_ref(), 9902, &selected)
        .await
        .is_err());
    for (change, reset) in [
        (
            "UPDATE repositories SET name='other' WHERE git_provider_connection_id=9901",
            "UPDATE repositories SET name='repo' WHERE git_provider_connection_id=9901",
        ),
        (
            "UPDATE projects SET is_deleted=true WHERE id=9901",
            "UPDATE projects SET is_deleted=false WHERE id=9901",
        ),
        (
            "UPDATE git_provider_connections SET is_active=false WHERE id=9901",
            "UPDATE git_provider_connections SET is_active=true WHERE id=9901",
        ),
        (
            "UPDATE git_provider_connections SET is_expired=true WHERE id=9901",
            "UPDATE git_provider_connections SET is_expired=false WHERE id=9901",
        ),
        (
            "UPDATE git_providers SET api_url='https://other.example.test' WHERE id=9901",
            "UPDATE git_providers SET api_url=NULL WHERE id=9901",
        ),
        (
            "UPDATE git_providers SET is_active=false WHERE id=9901",
            "UPDATE git_providers SET is_active=true WHERE id=9901",
        ),
    ] {
        db.execute_unprepared(change).await.unwrap();
        assert!(validate_binding(db.as_ref(), 9901, &source).await.is_err());
        assert!(validate_bindings(db.as_ref(), 9901, &selected)
            .await
            .is_err());
        db.execute_unprepared(reset).await.unwrap();
        assert!(validate_binding(db.as_ref(), 9901, &source).await.is_ok());
    }
    drop(db);
    test.cleanup().await;
}

struct FixtureGithub;
#[async_trait::async_trait]
impl GithubCredentialResolver for FixtureGithub {
    async fn resolve(&self, connection_id: i32) -> Result<SecretString, GithubCredentialError> {
        assert_eq!(connection_id, 9901);
        Ok(SecretString::new("fixture-provider-token"))
    }
}

#[tokio::test]
async fn postgres_github_consent_is_host_resolved_and_unlinking_stops_renewal() {
    if !temps_test_utils::external_database_configured()
        && !temps_test_utils::docker_available().await
    {
        eprintln!("Skipping GitHub consent test: Docker unavailable");
        return;
    }
    use crate::{
        authority::AuthorityStore,
        broker::UnavailableTokenlessBroker,
        planner::PolicyPlanner,
        service::{FreshConsent, HostObservation, ServiceAccessService},
        types::{RequestServiceGrant, SaveAccessService},
    };
    use temps_core::{EncryptionService, NoopAuditLogger};
    use temps_service_access_runtime::{HostAudit, WorkerControl};
    let mut test = temps_test_utils::TestDatabase::with_migrations()
        .await
        .unwrap();
    let db = test.connection_arc();
    db.execute_unprepared("INSERT INTO users (id,name,email) VALUES (9901,'Test user','source@example.test'); INSERT INTO git_providers (id,name,provider_type,auth_method,auth_config,is_active,is_default,created_at,updated_at) VALUES (9901,'GitHub','github','pat','{}',true,false,now(),now()); INSERT INTO git_provider_connections (id,provider_id,account_name,account_type,is_active,is_expired,created_at,updated_at) VALUES (9901,9901,'test-account','User',true,false,now(),now()); INSERT INTO projects (id,name,repo_name,repo_owner,git_provider_connection_id,directory,main_branch,preset,slug,created_at,updated_at) VALUES (9901,'Source','repo','owner',9901,'/','main','nodejs','source',now(),now()); INSERT INTO sandboxes (id,public_id,name,user_id,project_id,status,backend) VALUES (9901,'sbx_source','source',9901,9901,'running','docker')").await.unwrap();

    db.execute_unprepared("INSERT INTO repositories (git_provider_connection_id,owner,name,full_name,private,fork,created_at,updated_at,pushed_at,size,stargazers_count,watchers_count,default_branch,open_issues_count,topics,repo_object) VALUES (9901,'owner','repo','owner/repo',true,false,now(),now(),now(),0,0,0,'main',0,'[]','{}')").await.unwrap();
    let encryption = Arc::new(EncryptionService::new(&"ab".repeat(32)).unwrap());
    let service = ServiceAccessService::new(
        db.clone(),
        encryption.clone(),
        Arc::new(UnavailableTokenlessBroker),
    );
    let definition = service
        .save_service(
            9901,
            None,
            SaveAccessService {
                vault_id: None,
                name: "GitHub clone/fetch".into(),
                policy: policy(GithubAccessProtocol::Git),
                max_ttl_seconds: 900,
                enabled: true,
                credential: None,
                clear_credential: false,
                expected_version: None,
            },
        )
        .await
        .unwrap();
    assert!(definition.has_credential);
    let worker = WorkerControl::new(Arc::new(HostAudit));
    let identity = worker.identity();
    let authority = AuthorityStore::new(db.clone(), encryption.clone(), Arc::new(NoopAuditLogger));
    authority.register(None, &identity).await.unwrap();
    let incarnation = uuid::Uuid::new_v4();
    service
        .authorize_grant(
            FreshConsent {
                request_id: uuid::Uuid::new_v4(),
                project_id: 9901,
                sandbox_id: "sbx_source".into(),
                user_id: 9901,
                admin: false,
                request: RequestServiceGrant {
                    service_id: definition.id,
                    service_version: 1,
                    ttl_seconds: 60,
                },
            },
            HostObservation {
                worker: identity.clone(),
                incarnation,
                node_id: None,
                container_id: "immutable-source-container".into(),
                container_name: "temps-sandbox-source".into(),
                backend: "docker".into(),
            },
        )
        .await
        .unwrap();
    let planner = PolicyPlanner::new(db.clone(), encryption, None, Arc::new(FixtureGithub));
    let plan = planner.prepare(identity.boot_id).await.unwrap();
    assert_eq!(plan.bindings.len(), 1);
    assert!(!format!("{plan:?}").contains("fixture-provider-token"));
    assert!(plan.bindings[0].plan.grants()[0].rule().git.is_some());
    let issued = authority
        .issue(None, &identity, plan.expected_revision, plan.bindings)
        .await
        .unwrap();
    let delivered = issued.delivery(incarnation).unwrap();
    assert!(!delivered
        .policy
        .into_snapshot()
        .unwrap()
        .body
        .contains("fixture-provider-token"));
    // Selected repository, connection state and provider state are all
    // rechecked by the actual joined planner read, not just on service save.
    for (change, reset) in [
        (
            "UPDATE projects SET is_deleted=true WHERE id=9901",
            "UPDATE projects SET is_deleted=false WHERE id=9901",
        ),
        (
            "UPDATE git_provider_connections SET is_active=false WHERE id=9901",
            "UPDATE git_provider_connections SET is_active=true WHERE id=9901",
        ),
        (
            "UPDATE git_providers SET provider_type='gitlab' WHERE id=9901",
            "UPDATE git_providers SET provider_type='github' WHERE id=9901",
        ),
    ] {
        db.execute_unprepared(change).await.unwrap();
        let plan = planner.prepare(identity.boot_id).await.unwrap();
        assert!(plan.bindings.is_empty());
        assert_eq!(plan.retire.len(), 1);
        db.execute_unprepared(reset).await.unwrap();
    }
    drop(planner);
    drop(authority);
    drop(service);
    drop(db);
    test.cleanup().await;
}
