// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::super::{tests::migrated_test_db, tests::register_req, NodeError, NodeService};
use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement};
use std::sync::Arc;

async fn fixture(db: &Arc<DatabaseConnection>) -> i32 {
    let service = NodeService::new(db.clone());
    let node = service
        .register(register_req(
            "access-worker",
            "test-hash",
            "https://192.0.2.10:3100",
        ))
        .await
        .unwrap();
    db.execute_unprepared(r#"
INSERT INTO projects (id,name,repo_name,repo_owner,directory,main_branch,preset,slug,created_at,updated_at)
VALUES (9901,'Access','repo','owner','/','main','nodejs','access',now(),now());
INSERT INTO sandboxes (id,public_id,name,project_id,status)
VALUES (9901,'sbx_removal','removal',9901,'running'),(9902,'sbx_local','local',9901,'running');
INSERT INTO sandbox_access_authorities (id,sdk_node_id,seal_public_key,snapshot_key_encrypted,session_key_encrypted,ca_certificate,ca_key_encrypted)
SELECT ('00000000-0000-4000-8000-' || lpad(i::text,12,'0'))::uuid,
       ('00000000-0000-4000-9000-' || lpad(i::text,12,'0'))::uuid,
       repeat('a',44),'encrypted-snapshot','encrypted-session','public-ca','encrypted-ca'
FROM generate_series(1,3) i;
INSERT INTO sandbox_access_runtime_records (id,project_id,sandbox_id,provider_identity,authority_id,provider_backend,provider_name)
VALUES ('00000000-0000-4000-8000-000000000101',9901,9901,'remote-container','00000000-0000-4000-8000-000000000001','docker','remote'),
       ('00000000-0000-4000-8000-000000000102',9901,9902,'local-container','00000000-0000-4000-8000-000000000003','docker','local');
UPDATE sandbox_access_authorities SET closed_at=now() WHERE id='00000000-0000-4000-8000-000000000002';
INSERT INTO sandbox_access_delivery (runtime_id,desired_version,operation)
SELECT id,desired_version,'reconcile' FROM sandbox_access_runtime_records;
"#).await.unwrap();
    for sql in [
        "UPDATE sandboxes SET node_id=$1 WHERE id=9901",
        "UPDATE sandbox_access_authorities SET node_id=$1 WHERE id IN ('00000000-0000-4000-8000-000000000001','00000000-0000-4000-8000-000000000002')",
        "UPDATE sandbox_access_runtime_records SET node_id=$1 WHERE sandbox_id=9901",
        "UPDATE sandbox_access_delivery SET node_id=$1,claim_id='00000000-0000-4000-8000-000000000201',claimed_until=now()+interval '25 seconds' WHERE runtime_id='00000000-0000-4000-8000-000000000101'",
    ] {
        db.execute(Statement::from_sql_and_values(DatabaseBackend::Postgres,sql,[node.id.into()])).await.unwrap();
    }
    node.id
}

async fn count(db: &DatabaseConnection, sql: &str) -> i64 {
    db.query_one(Statement::from_string(DatabaseBackend::Postgres, sql))
        .await
        .unwrap()
        .unwrap()
        .try_get("", "n")
        .unwrap()
}

#[tokio::test]
async fn node_removal_releases_worker_authorities_and_destroyed_runtimes() {
    let Some(test_db) = migrated_test_db().await else {
        return;
    };
    let db = test_db.connection_arc();
    let node_id = fixture(&db).await;
    let service = NodeService::new(db.clone());
    assert!(matches!(
        service.remove(node_id).await,
        Err(NodeError::HasLiveSandboxes { .. })
    ));
    assert_eq!(
        count(&db, "SELECT count(*) AS n FROM sandbox_access_authorities").await,
        3
    );
    assert_eq!(
        count(
            &db,
            "SELECT count(*) AS n FROM sandbox_access_runtime_records"
        )
        .await,
        2
    );

    db.execute_unprepared("UPDATE sandboxes SET status='destroyed' WHERE id=9901")
        .await
        .unwrap();
    service.remove(node_id).await.unwrap();

    assert_eq!(count(&db, "SELECT count(*) AS n FROM nodes").await, 0);
    assert_eq!(
        count(
            &db,
            "SELECT count(*) AS n FROM sandbox_access_authorities WHERE node_id IS NOT NULL"
        )
        .await,
        0
    );
    assert_eq!(
        count(&db, "SELECT count(*) AS n FROM sandbox_access_authorities").await,
        1
    );
    assert_eq!(
        count(
            &db,
            "SELECT count(*) AS n FROM sandbox_access_runtime_records"
        )
        .await,
        1
    );
    // Deletion creates a retirement tombstone. Both it and the previously
    // claimed reconcile must be terminal without losing their remote target.
    assert_eq!(count(&db, "SELECT count(*) AS n FROM sandbox_access_delivery WHERE node_id IS NOT NULL AND completed_at IS NOT NULL AND claim_id IS NULL AND claimed_until IS NULL").await, 2);
    assert_eq!(count(&db, "SELECT count(*) AS n FROM sandbox_access_delivery WHERE node_id IS NULL AND completed_at IS NULL").await, 1);
    assert_eq!(
        count(
            &db,
            "SELECT count(*) AS n FROM sandboxes WHERE id=9901 AND status='destroyed'"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn node_removal_releases_authorities_after_project_deletion() {
    let Some(test_db) = migrated_test_db().await else {
        return;
    };
    let db = test_db.connection_arc();
    let node_id = fixture(&db).await;
    db.execute_unprepared(
        "UPDATE sandboxes SET status='destroyed' WHERE id=9901; DELETE FROM projects WHERE id=9901",
    )
    .await
    .unwrap();
    assert_eq!(
        count(
            &db,
            "SELECT count(*) AS n FROM sandbox_access_runtime_records"
        )
        .await,
        0
    );
    assert_eq!(
        count(&db, "SELECT count(*) AS n FROM sandbox_access_authorities").await,
        3
    );
    NodeService::new(db.clone()).remove(node_id).await.unwrap();
    assert_eq!(
        count(&db, "SELECT count(*) AS n FROM sandbox_access_authorities").await,
        1
    );
    assert_eq!(count(&db, "SELECT count(*) AS n FROM sandbox_access_delivery WHERE node_id IS NOT NULL AND completed_at IS NULL").await, 0);
}

#[tokio::test]
async fn node_removal_preserves_another_workers_live_access() {
    let Some(test_db) = migrated_test_db().await else {
        return;
    };
    let db = test_db.connection_arc();
    let node_id = fixture(&db).await;
    let service = NodeService::new(db.clone());
    let mut request = register_req("other-worker", "other-test-hash", "https://192.0.2.11:3100");
    request.private_address = "10.100.0.3".into();
    let other = service.register(request).await.unwrap();
    for sql in [
        "UPDATE sandboxes SET node_id=$1 WHERE id=9902",
        "UPDATE sandbox_access_authorities SET node_id=$1 WHERE id='00000000-0000-4000-8000-000000000003'",
        "UPDATE sandbox_access_runtime_records SET node_id=$1 WHERE sandbox_id=9902",
        "UPDATE sandbox_access_delivery SET node_id=$1 WHERE runtime_id='00000000-0000-4000-8000-000000000102'",
    ] {
        db.execute(Statement::from_sql_and_values(DatabaseBackend::Postgres, sql, [other.id.into()])).await.unwrap();
    }
    db.execute_unprepared("UPDATE sandboxes SET status='destroyed' WHERE id=9901")
        .await
        .unwrap();
    service.remove(node_id).await.unwrap();
    assert_eq!(
        count(
            &db,
            "SELECT count(*) AS n FROM sandbox_access_authorities WHERE closed_at IS NULL"
        )
        .await,
        1
    );
    assert_eq!(count(&db, "SELECT count(*) AS n FROM sandbox_access_runtime_records WHERE closed_at IS NULL AND sandbox_id=9902").await, 1);
    assert_eq!(count(&db, "SELECT count(*) AS n FROM sandbox_access_delivery WHERE completed_at IS NULL AND runtime_id='00000000-0000-4000-8000-000000000102'").await, 1);
    assert!(matches!(
        service.remove(other.id).await,
        Err(NodeError::HasLiveSandboxes { .. })
    ));
}

#[tokio::test]
async fn failed_node_delete_rolls_back_service_access_cleanup() {
    let Some(test_db) = migrated_test_db().await else {
        return;
    };
    let db = test_db.connection_arc();
    let node_id = fixture(&db).await;
    db.execute_unprepared(r#"
UPDATE sandboxes SET status='destroyed' WHERE id=9901;
CREATE FUNCTION reject_node_delete() RETURNS trigger AS $$ BEGIN
RAISE EXCEPTION 'test node delete failure'; END; $$ LANGUAGE plpgsql;
CREATE TRIGGER reject_node_delete BEFORE DELETE ON nodes FOR EACH ROW EXECUTE FUNCTION reject_node_delete();
"#).await.unwrap();
    let error = NodeService::new(db.clone())
        .remove(node_id)
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("test node delete failure"),
        "{error}"
    );
    assert_eq!(
        count(&db, "SELECT count(*) AS n FROM sandbox_access_authorities").await,
        3
    );
    assert_eq!(
        count(
            &db,
            "SELECT count(*) AS n FROM sandbox_access_runtime_records"
        )
        .await,
        2
    );
    assert_eq!(
        count(
            &db,
            "SELECT count(*) AS n FROM sandbox_access_delivery WHERE completed_at IS NULL"
        )
        .await,
        2
    );
    assert_eq!(
        count(
            &db,
            "SELECT count(*) AS n FROM sandbox_access_delivery WHERE claim_id IS NOT NULL"
        )
        .await,
        1
    );
}
