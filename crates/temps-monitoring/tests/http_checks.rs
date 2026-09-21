// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
use temps_database::test_utils::{is_container_runtime_unavailable, TestDatabase};
use temps_migrations::{
    EnvCheckHistoryMigration, HttpChecksMigration, MigrationTrait, SchemaManager,
};

#[tokio::test]
async fn http_check_migration_claims_and_cascade_work_on_postgres() {
    let database = match TestDatabase::new().await {
        Ok(db) => db,
        Err(error) if is_container_runtime_unavailable(&error.to_string()) => {
            eprintln!("Skipping HTTP check integration test: Docker runtime unavailable");
            return;
        }
        Err(error) => panic!("Could not create isolated test database: {error}"),
    };
    let db = database.connection();
    db.execute_unprepared("CREATE TABLE projects(id INTEGER PRIMARY KEY); CREATE TABLE env_vars(id INTEGER PRIMARY KEY,project_id INTEGER DEFAULT 1,key TEXT DEFAULT 'GITHUB_TOKEN',value TEXT DEFAULT 'initial',is_encrypted BOOLEAN DEFAULT FALSE,is_secret BOOLEAN DEFAULT TRUE,include_in_preview BOOLEAN DEFAULT FALSE,environment_id INTEGER,created_at TIMESTAMPTZ DEFAULT NOW(),updated_at TIMESTAMPTZ DEFAULT NOW()); INSERT INTO projects VALUES(1); INSERT INTO env_vars(id) VALUES(1);").await.unwrap();
    let schema = SchemaManager::new(db);
    HttpChecksMigration.up(&schema).await.unwrap();
    EnvCheckHistoryMigration.up(&schema).await.unwrap();
    db.execute_unprepared("INSERT INTO http_checks(project_id,env_var_id,name,encrypted_spec) VALUES(1,1,'test','ciphertext')").await.unwrap();
    let claim = |token: &str| {
        Statement::from_sql_and_values(DatabaseBackend::Postgres,
        "UPDATE http_checks SET lease_until=NOW()+INTERVAL '60 seconds',lease_token=$1 WHERE id=(SELECT id FROM http_checks WHERE enabled AND next_check_at<=NOW() AND (lease_until IS NULL OR lease_until<NOW()) ORDER BY next_check_at,id LIMIT 1 FOR UPDATE SKIP LOCKED) RETURNING id", [token.into()])
    };
    let (first, second) =
        futures::join!(db.query_all(claim("first")), db.query_all(claim("second")));
    assert_eq!(
        first.unwrap().len() + second.unwrap().len(),
        1,
        "only one process may claim a due check"
    );
    db.execute_unprepared("UPDATE http_checks SET lease_until=NOW()-INTERVAL '1 second'")
        .await
        .unwrap();
    assert_eq!(
        db.query_all(claim("reclaimed")).await.unwrap().len(),
        1,
        "a dead worker's lease must expire"
    );
    db.execute_unprepared("UPDATE http_checks SET next_check_at=NOW()+INTERVAL '1 day',last_result='{}'; UPDATE env_vars SET value='rotated' WHERE id=1").await.unwrap();
    assert_eq!(
        db.query_all(claim("rotation")).await.unwrap().len(),
        1,
        "rotating an environment variable must schedule a fresh check and invalidate the old lease"
    );
    assert!(db.execute_unprepared("INSERT INTO http_checks(project_id,name,encrypted_spec) VALUES(999,'orphan','ciphertext')").await.is_err());
    assert!(db
        .execute_unprepared("UPDATE http_checks SET interval_seconds=1")
        .await
        .is_err());
    db.execute_unprepared(r#"UPDATE http_checks SET last_checked_at=NOW(),last_result='{"status":"error","findings":[{"code":"authentication_rejected","status":"error","message":"The endpoint rejected the credential."}],"checked_at":"2026-09-21T00:00:00Z"}'"#).await.unwrap();
    let history = db
        .query_all(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT kind,details FROM env_var_history WHERE env_var_id=1",
        ))
        .await
        .unwrap();
    assert!(history
        .iter()
        .any(|event| event.try_get::<String>("", "kind").unwrap() == "value_changed"));
    assert!(
        history.iter().all(|event| !event
            .try_get::<serde_json::Value>("", "details")
            .unwrap()
            .to_string()
            .contains("rotated")),
        "history must never record credentials"
    );
    assert!(history
        .iter()
        .any(|event| event.try_get::<String>("", "kind").unwrap() == "verification"));
    db.execute_unprepared("DELETE FROM env_vars WHERE id=1")
        .await
        .unwrap();
    let rows = db
        .query_all(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT id FROM http_checks",
        ))
        .await
        .unwrap();
    assert!(
        rows.is_empty(),
        "deleting a credential removes its scheduled checks"
    );
    EnvCheckHistoryMigration.down(&schema).await.unwrap();
    HttpChecksMigration.down(&schema).await.unwrap();
}

#[derive(Default)]
struct Notifications;
#[async_trait::async_trait]
impl temps_core::notifications::NotificationService for Notifications {
    async fn send_email(
        &self,
        _: temps_core::notifications::EmailMessage,
    ) -> Result<(), temps_core::notifications::NotificationError> {
        Ok(())
    }
    async fn send_notification(
        &self,
        _: temps_core::notifications::NotificationData,
    ) -> Result<(), temps_core::notifications::NotificationError> {
        Ok(())
    }
    async fn is_configured(&self) -> Result<bool, temps_core::notifications::NotificationError> {
        Ok(false)
    }
}
#[tokio::test]
async fn automatic_checks_follow_variable_creation_rotation_and_issuer_changes() {
    let database = match TestDatabase::new().await {
        Ok(db) => db,
        Err(error) if is_container_runtime_unavailable(&error.to_string()) => return,
        Err(error) => panic!("{error}"),
    };
    let db = database.connection();
    db.execute_unprepared("CREATE TABLE projects(id INTEGER PRIMARY KEY); CREATE TABLE env_vars(id INTEGER PRIMARY KEY,project_id INTEGER DEFAULT 1,key TEXT,value TEXT,is_encrypted BOOLEAN DEFAULT FALSE,is_secret BOOLEAN DEFAULT TRUE,include_in_preview BOOLEAN DEFAULT FALSE,environment_id INTEGER,created_at TIMESTAMPTZ DEFAULT NOW(),updated_at TIMESTAMPTZ DEFAULT NOW()); INSERT INTO projects VALUES(1);").await.unwrap();
    let schema = SchemaManager::new(db);
    HttpChecksMigration.up(&schema).await.unwrap();
    EnvCheckHistoryMigration.up(&schema).await.unwrap();
    let service = temps_monitoring::http_checks::HttpChecksService::new(
        database.connection_arc(),
        std::sync::Arc::new(temps_core::EncryptionService::new_from_password(
            "test-password",
        )),
        std::sync::Arc::new(Notifications),
    )
    .unwrap();
    db.execute_unprepared(
        "INSERT INTO env_vars(id,key,value,is_encrypted) VALUES(0,'OPENAI_API_KEY','invalid-ciphertext',TRUE),(1,'GITHUB_TOKEN','synthetic-not-real',FALSE)",
    )
    .await
    .unwrap();
    service.reconcile_variables().await.unwrap();
    service.reconcile_variables().await.unwrap();
    let checks = service.list(1, 1, 20).await.unwrap();
    assert_eq!(checks.total, 1);
    assert_eq!(
        checks.items[0].automatic_provider.as_deref(),
        Some("github")
    );
    let id = checks.items[0].id;
    service.set_enabled(1, id, false).await.unwrap();
    db.execute_unprepared("UPDATE env_vars SET value='rotated-synthetic' WHERE id=1")
        .await
        .unwrap();
    service.reconcile_variables().await.unwrap();
    assert!(
        !service.list(1, 1, 20).await.unwrap().items[0].enabled,
        "rotation must preserve a user pause"
    );
    db.execute_unprepared(
        "UPDATE env_vars SET key='OPENAI_API_KEY',value='another-synthetic' WHERE id=1",
    )
    .await
    .unwrap();
    service.reconcile_variables().await.unwrap();
    assert_eq!(
        service.list(1, 1, 20).await.unwrap().items[0]
            .automatic_provider
            .as_deref(),
        Some("openai")
    );
    db.execute_unprepared("UPDATE env_vars SET key='LOG_LEVEL',value='info' WHERE id=1")
        .await
        .unwrap();
    service.reconcile_variables().await.unwrap();
    assert_eq!(service.list(1, 1, 20).await.unwrap().total, 0);
    let history = service.variable_history(1, 1, 1).await.unwrap();
    assert!(history.items.iter().any(|entry| entry.kind == "created"));
    assert!(history
        .items
        .iter()
        .any(|entry| entry.kind == "check_removed"));
    assert!(!serde_json::to_string(&history)
        .unwrap()
        .contains("synthetic"));
    assert!(matches!(
        service.variable_history(99, 1, 1).await,
        Err(temps_monitoring::http_checks::HttpChecksError::NotFound { .. })
    ));
}
