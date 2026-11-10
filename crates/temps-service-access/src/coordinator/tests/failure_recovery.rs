// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;

#[tokio::test]
async fn failed_delivery_cannot_retire_a_newer_approval() {
    if !temps_test_utils::external_database_configured()
        && !temps_test_utils::docker_available().await
    {
        eprintln!("Skipping concurrent delivery recovery: Docker unavailable");
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
                    origin: "https://api.example.com".into(),
                    path_prefixes: vec!["/".into()],
                    methods: vec![AccessMethod::Get],
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
    let original = coordinator.connect(consent(&service.id)).await.unwrap();
    provider.wrong_ack.store(true, Ordering::SeqCst);
    provider.hold_ack.store(true, Ordering::SeqCst);
    let (failed, newer) = tokio::join!(coordinator.dispatch_one(), async {
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            provider.ack_ready.notified(),
        )
        .await
        .unwrap();
        let newer = coordinator.connect(consent(&service.id)).await.unwrap();
        provider.hold_ack.store(false, Ordering::SeqCst);
        provider.release_ack.notify_one();
        newer
    });
    assert!(failed.is_err());
    assert_eq!(newer.runtime_id, original.runtime_id);
    let row = runtimes::Entity::find_by_id(original.runtime_id)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert!(
        row.closed_at.is_none(),
        "delivery failure cannot revoke later consent"
    );
    assert_eq!(row.desired_version, newer.desired_version);
    assert_eq!(
        row.applied_version, 0,
        "an unconfirmed ACK is not a receipt"
    );
    provider.wrong_ack.store(false, Ordering::SeqCst);
    db.execute_unprepared(
        "UPDATE sandbox_access_delivery SET available_at=now() WHERE completed_at IS NULL",
    )
    .await
    .unwrap();
    for _ in 0..4 {
        if coordinator.dispatch_one().await.unwrap().is_none() {
            break;
        }
    }
    let row = runtimes::Entity::find_by_id(original.runtime_id)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert!(row.closed_at.is_none());
    assert_eq!(row.applied_version, newer.desired_version);
    assert!(row.applied_grant_ids.contains(&newer.grant_id));
    assert!(row.applied_grant_ids.contains(&original.grant_id));
    test.cleanup().await;
}
