// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use sea_orm::{ConnectionTrait, EntityTrait};

#[tokio::test]
async fn create_session_route_returns_accepted_session_and_audits_issuance() {
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
    let audit = Arc::new(Audit::default());
    let state = state_with_db(db.clone(), audit.clone());
    let vault = state
        .service
        .save_vault(
            9801,
            None,
            SaveAccessVault {
                name: "Development".into(),
                service_ids: vec![],
                expected_version: None,
            },
        )
        .await
        .unwrap();
    let mut selections = Vec::new();
    for name in ["API", "Other API"] {
        let service = state
            .service
            .save_service(
                9801,
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
            9801,
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
    let response = send(
        "POST",
        &format!("/projects/9801/access-vaults/{}/sessions", vault.id),
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
    assert_eq!(received.project_id, 9801);
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
        assert_eq!(sessions[0]["project_id"], 9801);
        assert_eq!(sessions[0]["resource_id"], received.id);
        assert_eq!(sessions[0]["context"]["user_id"], 9801);
        assert!(events
            .iter()
            .all(|event| !event.contains("fixture-credential")));
    }
    drop(state);
    drop(db);
    test.cleanup().await;
}
