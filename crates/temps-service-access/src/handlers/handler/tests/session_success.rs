// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use sea_orm::{ConnectionTrait, EntityTrait};

#[tokio::test]
async fn create_session_route_returns_accepted_session_and_audits_issuance() {
    session_route(false, "unlink").await;
    for removal in ["unlink", "bundle", "vault", "service", "sandbox"] {
        session_route(true, removal).await;
    }
}

async fn session_route(projectless: bool, removal: &str) {
    if !temps_test_utils::external_database_configured()
        && !temps_test_utils::docker_available().await
    {
        eprintln!("Skipping session HTTP success test: database unavailable");
        return;
    }
    let mut test = temps_test_utils::TestDatabase::with_migrations()
        .await
        .unwrap();
    let db = test.connection_arc();
    db.execute_unprepared("INSERT INTO users (id,name,email) VALUES (9801,'Test user','user@example.com'); INSERT INTO projects (id,name,repo_name,repo_owner,directory,main_branch,preset,slug,created_at,updated_at) VALUES (9801,'Session','repo','owner','/','main','nodejs','session',now(),now()); INSERT INTO sandboxes (id,public_id,name,user_id,project_id,status,backend) VALUES (9801,'sbx_dispatch','session',9801,9801,'running','docker')").await.unwrap();
    if projectless {
        db.execute_unprepared("UPDATE sandboxes SET project_id=NULL WHERE id=9801")
            .await
            .unwrap();
    }
    let context = if projectless { None } else { Some(9801) };
    let audit = Arc::new(Audit::default());
    let state = state_with_db(db.clone(), audit.clone());
    let vault = state
        .service
        .save_vault(
            context,
            None,
            SaveAccessVault {
                name: "Development".into(),
                service_ids: vec![],
                expected_version: None,
            },
        )
        .await
        .unwrap();
    if projectless {
        let mut manager = test_user();
        manager.id = 9801;
        let response = send(
            "POST",
            &format!("/access-vaults/{}/links", vault.id),
            r#"{"sandbox_id":"sbx_dispatch"}"#,
            Some(AuthContext::new_session(manager, Role::Admin)),
            state.clone(),
        )
        .await;
        let status = response.status();
        let raw = to_bytes(response.into_body(), 8192).await.unwrap();
        assert_eq!(
            status,
            StatusCode::CREATED,
            "{}",
            String::from_utf8_lossy(&raw)
        );
        let linked: crate::link_types::VaultLinkResponse = serde_json::from_slice(&raw).unwrap();
        assert_eq!(linked.vault_id, vault.id);
        assert_eq!(linked.sandbox_id.as_deref(), Some("sbx_dispatch"));
        let listed = state
            .service
            .list_vault_links(
                &vault.id,
                crate::types::AccessPageQuery {
                    page: None,
                    page_size: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(listed.items[0].sandbox_id.as_deref(), Some("sbx_dispatch"));
        assert_eq!(linked.project_id, None);
        assert!(linked.revoked_at.is_none());
        let events = audit.0.lock().unwrap();
        assert!(events
            .iter()
            .map(|event| serde_json::from_str::<serde_json::Value>(event).unwrap())
            .any(|event| event["operation"] == "CREATE_ACCESS_VAULT_LINK"
                && event["resource_id"] == linked.id
                && event["vault_id"] == vault.id
                && event["context"]["user_id"] == 9801));
    }
    let mut selections = Vec::new();
    for name in ["API", "Other API"] {
        // A migrated project vault can contain newly created general services;
        // its legacy approval route must still issue the unchanged bundle.
        let service_context = if name == "Other API" { None } else { context };
        let service = state
            .service
            .save_service(
                service_context,
                None,
                SaveAccessService {
                    vault_id: Some(vault.id.clone()),
                    name: name.into(),
                    policy: SandboxAccessPolicy {
                        provider: None,
                        github_repository: None,
                        origin: "https://api.example.com".into(),
                        path_prefixes: vec!["/v1".into()],
                        methods: vec![AccessMethod::Get],
                    },
                    max_ttl_seconds: 900,
                    enabled: true,
                    credential: Some(temps_core::secret_string::SecretString::new(
                        "fixture-credential",
                    )),
                    clear_credential: false,
                    expected_version: None,
                },
            )
            .await
            .unwrap();
        selections.push(SessionServiceVersion {
            service_id: service.id,
            version: service.version,
        });
    }
    let bundle = state
        .service
        .save_bundle(
            context,
            &vault.id,
            None,
            SaveAccessBundle {
                name: "Read APIs".into(),
                service_ids: selections.iter().map(|s| s.service_id.clone()).collect(),
                ttl_seconds: 300,
                expected_version: None,
            },
        )
        .await
        .unwrap();
    let request = CreateAccessSession {
        authorized_link_id: None,
        request_id: uuid::Uuid::new_v4(),
        bundle_id: bundle.id.clone(),
        bundle_version: bundle.version,
        consumer: AccessSessionConsumer::Sandbox {
            sandbox_id: "sbx_dispatch".into(),
        },
        ttl_seconds: 120,
        services: selections,
    };
    let mut user = test_user();
    user.id = 9801;
    let auth = AuthContext::new_api_key(
        user,
        None,
        Some(vec![Permission::ProjectsWrite, Permission::SandboxesWrite]),
        "key".into(),
        1,
    );
    let path = if projectless {
        format!("/access-vaults/{}/sessions", vault.id)
    } else {
        format!("/projects/9801/access-vaults/{}/sessions", vault.id)
    };
    if projectless {
        let other = AuthContext::new_session(test_user(), Role::User);
        let denied = send(
            "POST",
            &path,
            &serde_json::to_string(&request).unwrap(),
            Some(other),
            state.clone(),
        )
        .await;
        assert_eq!(denied.status(), StatusCode::NOT_FOUND);
    }
    let response = send(
        "POST",
        &path,
        &serde_json::to_string(&request).unwrap(),
        Some(auth),
        state.clone(),
    )
    .await;
    let status = response.status();
    let raw = to_bytes(response.into_body(), 8192).await.unwrap();
    assert_eq!(
        status,
        StatusCode::ACCEPTED,
        "{}",
        String::from_utf8_lossy(&raw)
    );
    assert!(!String::from_utf8_lossy(&raw).contains("fixture-credential"));
    let received: AccessSessionResponse = serde_json::from_slice(&raw).unwrap();
    assert_eq!(
        received.id,
        format!("session_{}", request.request_id.simple())
    );
    assert_eq!(received.project_id, context);
    assert_eq!(received.vault_id, vault.id);
    assert_eq!(received.bundle_id, bundle.id);
    assert_eq!(received.bundle_name, "Read APIs");
    assert_eq!(received.bundle_version, bundle.version);
    assert_eq!(received.consumer.sandbox_id(), "sbx_dispatch");
    assert_eq!(received.created_by, Some(9801));
    assert_eq!(received.status, AccessSessionStatus::Issued);
    assert_eq!(received.service_count, 2);
    assert_eq!(
        (received.expires_at - received.created_at).num_seconds(),
        120
    );
    assert!(received.revoked_at.is_none());
    let stored = temps_entities::access_sessions::Entity::find_by_id(&received.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.grant_ids.len(), 2);
    assert_eq!(stored.expires_at, received.expires_at);
    // HTTP acceptance records consent; it is not a worker enforcement receipt.
    let runtime = temps_entities::sandbox_access_runtimes::Entity::find()
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();
    let detail = state
        .service
        .get_session(None, &received.id, 9801, false)
        .await
        .unwrap();
    assert!(detail
        .grants
        .iter()
        .all(|grant| grant.status == ServiceGrantStatus::Approved));
    assert_eq!(runtime.applied_version, 0);
    assert!(runtime.lease_expires_at.is_none());
    {
        let events = audit.0.lock().unwrap();
        let sessions: Vec<serde_json::Value> = events
            .iter()
            .map(|event| serde_json::from_str::<serde_json::Value>(event).unwrap())
            .filter(|event| event["operation"] == "CREATE_ACCESS_SESSION")
            .collect();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0]["project_id"], serde_json::json!(context));
        assert_eq!(sessions[0]["resource_id"], received.id);
        assert_eq!(sessions[0]["vault_id"], vault.id);
        assert_eq!(sessions[0]["context"]["user_id"], 9801);
        assert!(events
            .iter()
            .all(|event| !event.contains("fixture-credential")));
    }
    // Project permission changes apply to general reads, writes, and listings;
    // a direct sandbox link remains independent of project membership.
    struct DeniedProject;
    #[async_trait::async_trait]
    impl temps_core::ProjectAccessChecker for DeniedProject {
        async fn user_can_access_project(
            &self,
            _: i32,
            _: i32,
        ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
            Ok(false)
        }
        async fn effective_project_permissions(
            &self,
            _: i32,
            _: i32,
        ) -> Result<Option<Vec<String>>, Box<dyn std::error::Error + Send + Sync>> {
            Ok(Some(vec![]))
        }
    }
    let mut denied_state = state_with_db(db.clone(), audit.clone());
    Arc::get_mut(&mut denied_state)
        .unwrap()
        .project_access_checker = Some(Arc::new(DeniedProject));
    let mut owner = test_user();
    owner.id = 9801;
    for (method, route) in [
        ("GET", format!("/access-sessions/{}", received.id)),
        ("POST", format!("/access-sessions/{}/revoke", received.id)),
    ] {
        // Do not revoke the direct session yet: that path is tested by unlink below.
        if projectless && method == "POST" {
            continue;
        }
        let result = send(
            method,
            &route,
            "",
            Some(AuthContext::new_session(owner.clone(), Role::User)),
            denied_state.clone(),
        )
        .await;
        assert_eq!(
            result.status(),
            if projectless {
                StatusCode::OK
            } else {
                StatusCode::FORBIDDEN
            }
        );
    }
    let result = send(
        "GET",
        "/access-sessions?sandbox_id=sbx_dispatch",
        "",
        Some(AuthContext::new_session(owner, Role::User)),
        denied_state,
    )
    .await;
    assert_eq!(result.status(), StatusCode::OK);
    let listing: AccessSessionList =
        serde_json::from_slice(&to_bytes(result.into_body(), 8192).await.unwrap()).unwrap();
    assert_eq!(listing.total, if projectless { 1 } else { 0 });
    let authority_before = temps_entities::sandbox_access_authorities::Entity::find_by_id(
        runtime.authority_id.unwrap(),
    )
    .one(db.as_ref())
    .await
    .unwrap()
    .unwrap();
    match removal {
        "unlink" => {
            let link_id = stored.link_id.as_deref().unwrap();
            let mut manager = test_user();
            manager.id = 9801;
            let response = send(
                "DELETE",
                &format!("/access-vaults/{}/links/{link_id}", vault.id),
                "",
                Some(AuthContext::new_session(manager, Role::Admin)),
                state.clone(),
            )
            .await;
            assert_eq!(response.status(), StatusCode::NO_CONTENT);
            assert!(to_bytes(response.into_body(), 8192)
                .await
                .unwrap()
                .is_empty());
            let events = audit.0.lock().unwrap();
            assert!(events
                .iter()
                .map(|event| serde_json::from_str::<serde_json::Value>(event).unwrap())
                .any(|event| event["operation"] == "REVOKE_ACCESS_VAULT_LINK"
                    && event["resource_id"] == link_id
                    && event["vault_id"] == vault.id
                    && event["context"]["user_id"] == 9801));
        }
        "service" | "bundle" | "vault" => {
            let (path, operation, id) = match removal {
                "service" => {
                    let id = bundle.service_ids[0].clone();
                    (
                        format!("/access-services/{id}"),
                        "DELETE_ACCESS_SERVICE",
                        id,
                    )
                }
                "bundle" => (
                    format!("/access-vaults/{}/bundles/{}", vault.id, bundle.id),
                    "DELETE_ACCESS_BUNDLE_GENERAL",
                    bundle.id.clone(),
                ),
                "vault" => (
                    format!("/access-vaults/{}", vault.id),
                    "DELETE_ACCESS_VAULT_GENERAL",
                    vault.id.clone(),
                ),
                _ => unreachable!(),
            };
            let mut manager = test_user();
            manager.id = 9801;
            let response = send(
                "DELETE",
                &path,
                "",
                Some(AuthContext::new_session(manager.clone(), Role::Admin)),
                state.clone(),
            )
            .await;
            assert_eq!(response.status(), StatusCode::NO_CONTENT);
            assert!(to_bytes(response.into_body(), 8192)
                .await
                .unwrap()
                .is_empty());
            {
                let events = audit.0.lock().unwrap();
                assert!(events
                    .iter()
                    .map(|event| serde_json::from_str::<serde_json::Value>(event).unwrap())
                    .any(|event| event["operation"] == operation
                        && event["resource_id"] == id
                        && event["context"]["user_id"] == 9801));
            }
            if removal == "service" {
                let key = {
                    let events = audit.0.lock().unwrap();
                    let event = events
                        .iter()
                        .map(|event| serde_json::from_str::<serde_json::Value>(event).unwrap())
                        .find(|event| event["operation"] == "DELETE_ACCESS_SERVICE")
                        .unwrap();
                    assert_eq!(
                        event["service_scope_key"],
                        format!("{id}:{}", event["version"].as_i64().unwrap())
                    );
                    event["service_scope_key"].as_str().unwrap().to_owned()
                };
                assert!(
                    temps_entities::access_service_audit_scopes::Entity::find_by_id((
                        key,
                        vault.id.clone()
                    ))
                    .one(db.as_ref())
                    .await
                    .unwrap()
                    .is_some()
                );
            }
            let missing = send(
                "DELETE",
                &format!("{path}_missing"),
                "",
                Some(AuthContext::new_session(manager, Role::Admin)),
                state.clone(),
            )
            .await;
            assert_eq!(missing.status(), StatusCode::NOT_FOUND);
            assert_eq!(
                missing.headers()["content-type"],
                "application/problem+json"
            );
            let problem: serde_json::Value =
                serde_json::from_slice(&to_bytes(missing.into_body(), 8192).await.unwrap())
                    .unwrap();
            assert!(problem["detail"]
                .as_str()
                .unwrap()
                .contains(&format!("{id}_missing")));
        }
        "sandbox" => {
            db.execute_unprepared("DELETE FROM sandboxes WHERE id=9801")
                .await
                .unwrap();
        }
        _ => panic!("unknown test removal"),
    }
    let authority_after =
        temps_entities::sandbox_access_authorities::Entity::find_by_id(authority_before.id)
            .one(db.as_ref())
            .await
            .unwrap()
            .unwrap();
    assert!(authority_after.desired_revision > authority_before.desired_revision);
    assert!(
        authority_after.next_reconcile_at.is_some(),
        "unlink durably schedules worker reconciliation"
    );
    for id in stored.grant_ids {
        let grant = temps_entities::sandbox_service_grants::Entity::find_by_id(id)
            .one(db.as_ref())
            .await
            .unwrap();
        if removal == "sandbox" {
            assert!(grant.is_none());
        } else {
            assert!(grant.unwrap().revoked_at.is_some());
        }
    }
    drop(state);
    drop(db);
    test.cleanup().await;
}

#[tokio::test]
async fn direct_github_session_checks_source_project_permissions_before_issuance() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct SourcePermissions(Vec<String>, Arc<AtomicUsize>);
    #[async_trait::async_trait]
    impl temps_core::ProjectAccessChecker for SourcePermissions {
        async fn user_can_access_project(
            &self,
            user_id: i32,
            project_id: i32,
        ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
            assert_eq!((user_id, project_id), (1, 7));
            Ok(true)
        }
        async fn effective_project_permissions(
            &self,
            user_id: i32,
            project_id: i32,
        ) -> Result<Option<Vec<String>>, Box<dyn std::error::Error + Send + Sync>> {
            assert_eq!((user_id, project_id), (1, 7));
            self.1.fetch_add(1, Ordering::SeqCst);
            Ok(Some(self.0.clone()))
        }
    }
    for missing in [
        Some(Permission::ProjectsWrite),
        Some(Permission::SandboxesWrite),
        Some(Permission::GitRepositoriesRead),
        None,
    ] {
        let now = Utc::now();
        let mut service = row();
        service.policy =
            serde_json::to_value(crate::github::tests::policy(GithubAccessProtocol::Git)).unwrap();
        let mut second_service = service.clone();
        second_service.id = "svc_second".into();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![sandbox()]])
            .append_query_results([vec![temps_entities::access_vault_links::Model {
                id: "link_direct".into(),
                vault_id: "vault_test".into(),
                project_id: None,
                sandbox_id: Some(42),
                revoked_at: None,
                created_at: now,
            }]])
            .append_query_results([vec![temps_entities::access_vaults::Model {
                id: "vault_test".into(),
                project_id: None,
                name: "Git APIs".into(),
                service_ids: vec!["svc_test".into(), "svc_second".into()],
                version: 1,
                legacy_approval_before: None,
                legacy_service_ids: None,
                legacy_removed_service_ids: vec![],
                retired_at: None,
                created_at: now,
                updated_at: now,
            }]])
            .append_query_results([vec![service, second_service]])
            // The fully authorized case reaches issuance; no consent is written
            // because the coordinator's first storage operation fails here.
            .append_query_errors([sea_orm::DbErr::Custom(
                "issuance storage unavailable".into(),
            )]);
        let audit = Arc::new(Audit::default());
        let mut state = state(db, audit.clone());
        let permission_reads = Arc::new(AtomicUsize::new(0));
        Arc::get_mut(&mut state).unwrap().project_access_checker =
            Some(Arc::new(SourcePermissions(
                [
                    Permission::ProjectsWrite,
                    Permission::SandboxesWrite,
                    Permission::GitRepositoriesRead,
                ]
                .into_iter()
                .filter(|p| Some(*p) != missing)
                .map(|p| p.to_string())
                .collect(),
                permission_reads.clone(),
            )));
        let response = send("POST", "/access-vaults/vault_test/sessions",
            &serde_json::json!({"request_id":uuid::Uuid::new_v4(),"bundle_id":"bundle_test","bundle_version":1,"consumer":{"kind":"sandbox","sandbox_id":"sbx_test"},"ttl_seconds":60,"services":[{"service_id":"svc_test","version":1},{"service_id":"svc_second","version":1}]}).to_string(),
            Some(AuthContext::new_session(test_user(), Role::User)), state).await;
        assert_eq!(
            response.status(),
            if missing.is_some() {
                StatusCode::FORBIDDEN
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        );
        if let Some(permission) = missing {
            assert_eq!(
                response.headers()["content-type"],
                "application/problem+json"
            );
            let problem: serde_json::Value =
                serde_json::from_slice(&to_bytes(response.into_body(), 8192).await.unwrap())
                    .unwrap();
            assert_eq!(problem["title"], "Project Permission Denied");
            assert!(problem["detail"]
                .as_str()
                .unwrap()
                .contains(&permission.to_string()));
        }
        if missing.is_none() {
            assert_eq!(
                permission_reads.load(Ordering::SeqCst),
                3,
                "permissions are checked once per source project, not per service"
            );
        }
        assert!(audit.0.lock().unwrap().is_empty());
    }
}
