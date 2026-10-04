// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
use temps_database::test_utils::{is_container_runtime_unavailable, TestDatabase};
use temps_migrations::{
    CredentialCatalogMigration, DetectionRetryMigration, EnvCheckHistoryMigration,
    HttpChecksMigration, MigrationTrait, SchemaManager, SecretChecksAndHistoryMigration,
};

/// Mirrors the columns of the real `secrets` table that checks and history rely on.
const SECRETS_TABLE: &str = "CREATE TABLE secrets(id INTEGER PRIMARY KEY,project_id INTEGER NOT NULL DEFAULT 1 REFERENCES projects(id) ON DELETE CASCADE,environment_id INTEGER,key TEXT NOT NULL DEFAULT 'TLS_CERT',value TEXT NOT NULL DEFAULT 'initial-ciphertext',include_in_preview BOOLEAN NOT NULL DEFAULT FALSE,created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW());";

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
    db.execute_unprepared("CREATE TABLE projects(id INTEGER PRIMARY KEY); CREATE TABLE env_vars(id INTEGER PRIMARY KEY,project_id INTEGER DEFAULT 1,key TEXT DEFAULT 'GITHUB_TOKEN',value TEXT DEFAULT 'initial',is_encrypted BOOLEAN DEFAULT FALSE,is_secret BOOLEAN DEFAULT TRUE,include_in_preview BOOLEAN DEFAULT FALSE,environment_id INTEGER,created_at TIMESTAMPTZ DEFAULT NOW(),updated_at TIMESTAMPTZ DEFAULT NOW()); INSERT INTO projects VALUES(1); INSERT INTO env_vars(id) VALUES(1),(2);").await.unwrap();
    db.execute_unprepared(SECRETS_TABLE).await.unwrap();
    let schema = SchemaManager::new(db);
    HttpChecksMigration.up(&schema).await.unwrap();
    EnvCheckHistoryMigration.up(&schema).await.unwrap();
    DetectionRetryMigration.up(&schema).await.unwrap();
    db.execute_unprepared("INSERT INTO env_check_detection(env_var_id,observed_updated_at) SELECT id,updated_at FROM env_vars; INSERT INTO http_checks(project_id,env_var_id,name,encrypted_spec,automatic_provider) VALUES(1,2,'GitHub verification','ciphertext','github'); DELETE FROM http_checks WHERE env_var_id=2").await.unwrap();
    CredentialCatalogMigration.up(&schema).await.unwrap();
    SecretChecksAndHistoryMigration.up(&schema).await.unwrap();
    let detection_rows = db
        .query_all(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT env_var_id FROM env_check_detection ORDER BY env_var_id",
        ))
        .await
        .unwrap();
    assert_eq!(detection_rows.len(), 1);
    assert_eq!(
        detection_rows[0].try_get::<i32>("", "env_var_id").unwrap(),
        2
    );
    let suppression = db
        .query_one(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT automatic_provider FROM env_check_suppressions WHERE env_var_id=2",
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        suppression
            .try_get::<String>("", "automatic_provider")
            .unwrap(),
        "*"
    );
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
    SecretChecksAndHistoryMigration.down(&schema).await.unwrap();
    CredentialCatalogMigration.down(&schema).await.unwrap();
    DetectionRetryMigration.down(&schema).await.unwrap();
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
    db.execute_unprepared(SECRETS_TABLE).await.unwrap();
    let schema = SchemaManager::new(db);
    HttpChecksMigration.up(&schema).await.unwrap();
    EnvCheckHistoryMigration.up(&schema).await.unwrap();
    DetectionRetryMigration.up(&schema).await.unwrap();
    db.execute_unprepared("INSERT INTO env_check_detection(env_var_id,observed_updated_at) SELECT id,updated_at FROM env_vars").await.unwrap();
    CredentialCatalogMigration.up(&schema).await.unwrap();
    SecretChecksAndHistoryMigration.up(&schema).await.unwrap();
    assert!(db
        .query_all(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT env_var_id FROM env_check_detection"
        ))
        .await
        .unwrap()
        .is_empty());
    let service = temps_monitoring::http_checks::HttpChecksService::new(
        database.connection_arc(),
        std::sync::Arc::new(temps_core::EncryptionService::new_from_password(
            "wrong-password",
        )),
        std::sync::Arc::new(Notifications),
    )
    .unwrap();
    db.execute_unprepared(
        "INSERT INTO env_vars(id,key,value,is_encrypted) VALUES(0,'OPENAI_API_KEY','invalid-ciphertext',TRUE),(1,'GITHUB_TOKEN','ghp_abcdefghijklmnopqrstuvwxyz0123456789',FALSE),(2,'GITHUB_TOKEN','unrelated-secret',FALSE)",
    )
    .await
    .unwrap();
    let repaired_key = temps_core::EncryptionService::new_from_password("test-password");
    let ciphertext = repaired_key
        .encrypt_string("ghp_abcdefghijklmnopqrstuvwxyz0123456789")
        .unwrap();
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "UPDATE env_vars SET value=$1 WHERE id=0",
        [ciphertext.into()],
    ))
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
    db.execute_unprepared(
        "UPDATE env_vars SET value='ghp_0123456789abcdefghijklmnopqrstuvwxyz' WHERE id=1",
    )
    .await
    .unwrap();
    service.reconcile_variables().await.unwrap();
    assert!(
        !service.list(1, 1, 20).await.unwrap().items[0].enabled,
        "rotation must preserve a user pause"
    );
    let synthetic_openai = format!(
        "sk-{}T3BlbkFJ{}",
        "abcdefghijklmnopqrst", "0123456789abcdefghij"
    );
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "UPDATE env_vars SET key='OPENAI_API_KEY',value=$1 WHERE id=1",
        [synthetic_openai.into()],
    ))
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
    // Recover after correcting the key, without changing the variable after failure.
    let service = temps_monitoring::http_checks::HttpChecksService::new(
        database.connection_arc(),
        std::sync::Arc::new(repaired_key),
        std::sync::Arc::new(Notifications),
    )
    .unwrap();
    db.execute_unprepared(
        "UPDATE env_check_detection SET retry_after=NOW()-INTERVAL '1 second' WHERE env_var_id=0",
    )
    .await
    .unwrap();
    service.reconcile_variables().await.unwrap();
    assert_eq!(service.list(1, 1, 20).await.unwrap().total, 1);
    // Upgrading the catalog rescans old variables, without unpausing existing checks.
    db.execute_unprepared("UPDATE http_checks SET enabled=FALSE")
        .await
        .unwrap();
    // Assemble an intentionally synthetic token so source scanners cannot
    // mistake a token-shaped literal for a live credential.
    let synthetic_digitalocean = format!("dop_v1_{}", "0123456789abcdef".repeat(4));
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "INSERT INTO env_vars(id,project_id,key,value) VALUES(3,1,'DIGITALOCEAN_TOKEN',$1)",
        [synthetic_digitalocean.into()],
    ))
    .await
    .unwrap();
    db.execute_unprepared("INSERT INTO env_check_detection(env_var_id,observed_updated_at) SELECT id,updated_at FROM env_vars WHERE id=3").await.unwrap();
    CredentialCatalogMigration.up(&schema).await.unwrap();
    service.reconcile_variables().await.unwrap();
    let checks = service.list(1, 1, 20).await.unwrap().items;
    assert!(checks.iter().any(|check| check.env_var_id == Some(3)
        && check.automatic_provider.as_deref() == Some("digitalocean")
        && check.enabled));
    assert!(checks
        .iter()
        .filter(|check| check.env_var_id != Some(3))
        .all(|check| !check.enabled));
    let automatic = checks
        .iter()
        .find(|check| check.env_var_id == Some(3))
        .unwrap();
    service.delete(1, automatic.id).await.unwrap();
    let rotated_digitalocean = format!("dop_v1_{}", "fedcba9876543210".repeat(4));
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "UPDATE env_vars SET include_in_preview=TRUE,value=$1 WHERE id=3",
        [rotated_digitalocean.into()],
    ))
    .await
    .unwrap();
    CredentialCatalogMigration.up(&schema).await.unwrap();
    service.reconcile_variables().await.unwrap();
    assert!(
        service
            .list(1, 1, 20)
            .await
            .unwrap()
            .items
            .iter()
            .all(|check| check.env_var_id != Some(3)),
        "a catalog rescan must preserve explicit deletion of an automatic check"
    );
    db.execute_unprepared("INSERT INTO http_checks(project_id,name,encrypted_spec) VALUES(1,'manual without variable','ciphertext')").await.unwrap();
    let manual_id = db
        .query_one(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT id FROM http_checks WHERE name='manual without variable'",
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get::<i32>("", "id")
        .unwrap();
    service.delete(1, manual_id).await.unwrap();
    let small = service.variable_history(1, 1, 1, 2).await.unwrap();
    let next = service.variable_history(1, 1, 2, 2).await.unwrap();
    assert_eq!(small.page_size, 2);
    assert_eq!(small.items.len(), 2);
    assert!(small.items[1].id > next.items[0].id);
    let history = service.variable_history(1, 1, 1, 15).await.unwrap();
    assert!(history.items.iter().any(|entry| entry.kind == "created"));
    assert!(history
        .items
        .iter()
        .any(|entry| entry.kind == "check_removed"));
    assert!(!serde_json::to_string(&history)
        .unwrap()
        .contains("synthetic"));
    assert!(matches!(
        service.variable_history(99, 1, 1, 15).await,
        Err(temps_monitoring::http_checks::HttpChecksError::NotFound { .. })
    ));
}

async fn query_strings(db: &sea_orm::DatabaseConnection, sql: &str, column: &str) -> Vec<String> {
    db.query_all(Statement::from_string(DatabaseBackend::Postgres, sql))
        .await
        .unwrap()
        .iter()
        .map(|row| row.try_get::<String>("", column).unwrap())
        .collect()
}

#[tokio::test]
async fn secret_history_and_check_constraints_follow_secret_changes() {
    let database = match TestDatabase::new().await {
        Ok(db) => db,
        Err(error) if is_container_runtime_unavailable(&error.to_string()) => {
            eprintln!("Skipping secret check integration test: Docker runtime unavailable");
            return;
        }
        Err(error) => panic!("Could not create isolated test database: {error}"),
    };
    let db = database.connection();
    db.execute_unprepared("CREATE TABLE projects(id INTEGER PRIMARY KEY); CREATE TABLE env_vars(id INTEGER PRIMARY KEY,project_id INTEGER DEFAULT 1,key TEXT DEFAULT 'API_TOKEN',value TEXT DEFAULT 'initial',is_encrypted BOOLEAN DEFAULT FALSE,is_secret BOOLEAN DEFAULT TRUE,include_in_preview BOOLEAN DEFAULT FALSE,environment_id INTEGER,created_at TIMESTAMPTZ DEFAULT NOW(),updated_at TIMESTAMPTZ DEFAULT NOW()); INSERT INTO projects VALUES(1); INSERT INTO env_vars(id) VALUES(1);").await.unwrap();
    db.execute_unprepared(SECRETS_TABLE).await.unwrap();
    db.execute_unprepared("INSERT INTO secrets(id,key) VALUES(1,'EXISTING_CERT')")
        .await
        .unwrap();
    let schema = SchemaManager::new(db);
    HttpChecksMigration.up(&schema).await.unwrap();
    EnvCheckHistoryMigration.up(&schema).await.unwrap();
    DetectionRetryMigration.up(&schema).await.unwrap();
    CredentialCatalogMigration.up(&schema).await.unwrap();
    db.execute_unprepared("INSERT INTO http_checks(project_id,env_var_id,name,encrypted_spec) VALUES(1,1,'pre-existing','ciphertext'); INSERT INTO env_check_detection(env_var_id,observed_updated_at) VALUES(1,NOW())").await.unwrap();
    SecretChecksAndHistoryMigration.up(&schema).await.unwrap();
    assert!(
        query_strings(
            db,
            "SELECT env_var_id::text AS id FROM env_check_detection",
            "id"
        )
        .await
        .is_empty(),
        "env vars scanned before local checks existed are re-scanned"
    );
    db.execute_unprepared(r#"UPDATE http_checks SET last_checked_at=NOW(),next_check_at=NOW()+INTERVAL '1 day',last_result='{"status":"warning","findings":[],"checked_at":"2026-10-02T00:00:00Z"}' WHERE env_var_id=1; UPDATE env_vars SET value='renewed' WHERE id=1"#).await.unwrap();
    assert_eq!(
        query_strings(
            db,
            "SELECT (last_result IS NULL AND next_check_at<=NOW())::text AS due FROM http_checks WHERE env_var_id=1",
            "due"
        )
        .await,
        vec!["true"],
        "a new env var value resets its checks, as a new secret value does"
    );

    assert_eq!(
        query_strings(db, "SELECT kind FROM http_checks", "kind").await,
        vec!["http"],
        "existing checks default to the HTTP kind"
    );
    assert_eq!(
        query_strings(
            db,
            "SELECT kind FROM secret_history WHERE secret_id=1",
            "kind"
        )
        .await,
        vec!["tracking_started"]
    );

    db.execute_unprepared("INSERT INTO secrets(id,key) VALUES(2,'TLS_CERT')")
        .await
        .unwrap();
    db.execute_unprepared("INSERT INTO secret_check_detection(secret_id) VALUES(2); INSERT INTO http_checks(project_id,secret_id,kind,name,encrypted_spec,automatic_provider,next_check_at,last_result) VALUES(1,2,'local','Credential expiry','ciphertext','local_expiry',NOW()+INTERVAL '1 day','{}')").await.unwrap();
    db.execute_unprepared("UPDATE secrets SET include_in_preview=TRUE,updated_at=NOW() WHERE id=2")
        .await
        .unwrap();
    assert_eq!(
        query_strings(
            db,
            "SELECT secret_id::text AS id FROM secret_check_detection",
            "id"
        )
        .await,
        vec!["2"],
        "preview and scope edits must not force a rescan"
    );
    db.execute_unprepared("UPDATE secrets SET value='rotated-ciphertext' WHERE id=2")
        .await
        .unwrap();
    assert!(
        query_strings(
            db,
            "SELECT secret_id::text AS id FROM secret_check_detection",
            "id"
        )
        .await
        .is_empty(),
        "a new value must be rescanned"
    );
    assert_eq!(
        query_strings(
            db,
            "SELECT (last_result IS NULL AND next_check_at<=NOW())::text AS due FROM http_checks WHERE secret_id=2",
            "due"
        )
        .await,
        vec!["true"],
        "rotating a secret reschedules its checks"
    );
    db.execute_unprepared(r#"UPDATE http_checks SET last_checked_at=NOW(),last_result='{"status":"warning","findings":[{"code":"expires_within_7_days","status":"warning","message":"Certificate svc.example.test expires within 7 days."}],"checked_at":"2026-10-02T00:00:00Z"}' WHERE secret_id=2"#).await.unwrap();
    db.execute_unprepared("UPDATE http_checks SET enabled=FALSE WHERE secret_id=2")
        .await
        .unwrap();
    assert_eq!(
        query_strings(
            db,
            "SELECT kind FROM secret_history WHERE secret_id=2 ORDER BY id",
            "kind"
        )
        .await,
        vec![
            "created",
            "check_added",
            "settings_changed",
            "value_changed",
            "verification",
            "check_paused"
        ]
    );
    let details = query_strings(
        db,
        "SELECT details::text AS details FROM secret_history WHERE secret_id=2",
        "details",
    )
    .await
    .join(" ");
    assert!(
        !details.contains("ciphertext"),
        "history must never record values"
    );
    assert!(details.contains("include_in_preview"));
    assert!(
        query_strings(db, "SELECT kind FROM env_var_history WHERE kind IN ('check_added','verification','check_paused') AND env_var_id<>1", "kind")
            .await
            .is_empty(),
        "secret check events must not leak into variable history"
    );

    // A check reads exactly one credential source, and kinds are a closed set.
    for invalid in [
        "INSERT INTO http_checks(project_id,env_var_id,secret_id,name,encrypted_spec) VALUES(1,1,2,'both','ciphertext')",
        "INSERT INTO http_checks(project_id,secret_id,encrypted_credential,name,encrypted_spec) VALUES(1,2,'ciphertext','both','ciphertext')",
        "INSERT INTO http_checks(project_id,kind,name,encrypted_spec) VALUES(1,'ping','unknown kind','ciphertext')",
        "INSERT INTO http_checks(project_id,secret_id,name,encrypted_spec) VALUES(1,999,'missing secret','ciphertext')",
        "INSERT INTO http_checks(project_id,secret_id,kind,name,encrypted_spec,automatic_provider) VALUES(1,2,'local','duplicate automatic','ciphertext','local_expiry')",
    ] {
        assert!(db.execute_unprepared(invalid).await.is_err(), "{invalid}");
    }

    // A manual check takes precedence, so it invalidates the detection marker.
    db.execute_unprepared("INSERT INTO secret_check_detection(secret_id) VALUES(1); INSERT INTO http_checks(project_id,secret_id,kind,name,encrypted_spec) VALUES(1,1,'local','manual','ciphertext')").await.unwrap();
    assert!(query_strings(
        db,
        "SELECT secret_id::text AS id FROM secret_check_detection WHERE secret_id=1",
        "id"
    )
    .await
    .is_empty());
    db.execute_unprepared("DELETE FROM http_checks WHERE secret_id=1 AND name='manual'")
        .await
        .unwrap();
    assert_eq!(
        query_strings(
            db,
            "SELECT kind FROM secret_history WHERE secret_id=1 ORDER BY id",
            "kind"
        )
        .await,
        vec!["tracking_started", "check_added", "check_removed"]
    );

    db.execute_unprepared("INSERT INTO secret_check_suppressions(secret_id,automatic_provider) VALUES(2,'local_expiry'); DELETE FROM secrets WHERE id=2").await.unwrap();
    for table in [
        "http_checks WHERE secret_id IS NOT NULL",
        "secret_history WHERE secret_id=2",
        "secret_check_suppressions",
    ] {
        assert!(
            query_strings(db, &format!("SELECT 'row' AS r FROM {table}"), "r")
                .await
                .is_empty(),
            "deleting a secret cascades to {table}"
        );
    }

    // Re-pointing a manual check re-runs detection for the credential it now reads
    // and for the one it left, which may need its automatic check back.
    db.execute_unprepared("INSERT INTO secret_check_detection(secret_id) VALUES(1); INSERT INTO env_check_detection(env_var_id,observed_updated_at) VALUES(1,NOW()) ON CONFLICT DO NOTHING; UPDATE http_checks SET env_var_id=NULL,secret_id=1 WHERE name='pre-existing'").await.unwrap();
    assert!(query_strings(
        db,
        "SELECT secret_id::text AS id FROM secret_check_detection WHERE secret_id=1",
        "id"
    )
    .await
    .is_empty());
    assert!(query_strings(
        db,
        "SELECT env_var_id::text AS id FROM env_check_detection WHERE env_var_id=1",
        "id"
    )
    .await
    .is_empty());
    // Both histories record the move: the variable lost the check, the secret gained it.
    assert_eq!(
        query_strings(
            db,
            "SELECT kind FROM env_var_history WHERE env_var_id=1 ORDER BY id DESC LIMIT 1",
            "kind"
        )
        .await,
        vec!["check_removed"]
    );
    assert_eq!(
        query_strings(
            db,
            "SELECT kind FROM secret_history WHERE secret_id=1 ORDER BY id DESC LIMIT 1",
            "kind"
        )
        .await,
        vec!["check_added"]
    );
    SecretChecksAndHistoryMigration.down(&schema).await.unwrap();
    db.execute_unprepared("INSERT INTO http_checks(project_id,env_var_id,name,encrypted_spec) VALUES(1,1,'after rollback','ciphertext')").await.unwrap();
    assert!(
        query_strings(
            db,
            "SELECT kind FROM env_var_history WHERE env_var_id=1",
            "kind"
        )
        .await
        .contains(&"check_added".to_string()),
        "the restored trigger keeps recording variable history"
    );
}

fn certificate_expiring_in(days: i64) -> String {
    use chrono::Datelike;
    let expiry = (chrono::Utc::now() + chrono::Duration::days(days)).date_naive();
    let mut params = rcgen::CertificateParams::new(vec!["svc.example.test".to_owned()]).unwrap();
    params.not_after =
        rcgen::date_time_ymd(expiry.year(), expiry.month() as u8, expiry.day() as u8);
    params
        .self_signed(&rcgen::KeyPair::generate().unwrap())
        .unwrap()
        .pem()
}

#[tokio::test]
async fn automatic_checks_follow_secret_values_and_certificates() {
    use temps_credential_checks::CheckKind;
    use temps_monitoring::http_checks::HttpCheckView;
    let database = match TestDatabase::new().await {
        Ok(db) => db,
        Err(error) if is_container_runtime_unavailable(&error.to_string()) => return,
        Err(error) => panic!("{error}"),
    };
    let db = database.connection();
    db.execute_unprepared("CREATE TABLE projects(id INTEGER PRIMARY KEY); CREATE TABLE env_vars(id INTEGER PRIMARY KEY,project_id INTEGER DEFAULT 1,key TEXT,value TEXT,is_encrypted BOOLEAN DEFAULT FALSE,is_secret BOOLEAN DEFAULT TRUE,include_in_preview BOOLEAN DEFAULT FALSE,environment_id INTEGER,created_at TIMESTAMPTZ DEFAULT NOW(),updated_at TIMESTAMPTZ DEFAULT NOW()); INSERT INTO projects VALUES(1);").await.unwrap();
    db.execute_unprepared(SECRETS_TABLE).await.unwrap();
    let schema = SchemaManager::new(db);
    HttpChecksMigration.up(&schema).await.unwrap();
    EnvCheckHistoryMigration.up(&schema).await.unwrap();
    DetectionRetryMigration.up(&schema).await.unwrap();
    CredentialCatalogMigration.up(&schema).await.unwrap();
    SecretChecksAndHistoryMigration.up(&schema).await.unwrap();
    let encryption = temps_core::EncryptionService::new_from_password("test-password");
    let encrypt = |value: &str| encryption.encrypt_string(value).unwrap();
    for (id, key, ciphertext) in [
        (1, "TLS_CERT", encrypt(&certificate_expiring_in(5))),
        (
            2,
            "DEPLOY_TOKEN",
            encrypt("ghp_abcdefghijklmnopqrstuvwxyz0123456789"),
        ),
        (3, "CORRUPT", "invalid-ciphertext".to_owned()),
        (4, "LARGE_BUNDLE", encrypt(&"A".repeat(70_000))),
    ] {
        db.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "INSERT INTO secrets(id,key,value) VALUES($1,$2,$3)",
            [id.into(), key.into(), ciphertext.into()],
        ))
        .await
        .unwrap();
    }
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "INSERT INTO env_vars(id,key,value) VALUES(1,'CA_BUNDLE',$1)",
        [certificate_expiring_in(200).into()],
    ))
    .await
    .unwrap();
    let service = temps_monitoring::http_checks::HttpChecksService::new(
        database.connection_arc(),
        std::sync::Arc::new(temps_core::EncryptionService::new_from_password(
            "test-password",
        )),
        std::sync::Arc::new(Notifications),
    )
    .unwrap();
    service.reconcile_secrets().await.unwrap();
    service.reconcile_variables().await.unwrap();
    let checks = service.list(1, 1, 20).await.unwrap().items;
    let for_secret = |checks: &[HttpCheckView], id: i32| -> Vec<(CheckKind, Option<String>)> {
        checks
            .iter()
            .filter(|check| check.secret_id == Some(id))
            .map(|check| (check.kind, check.automatic_provider.clone()))
            .collect()
    };
    assert_eq!(
        for_secret(&checks, 1),
        vec![(CheckKind::Local, Some("local_expiry".into()))]
    );
    assert_eq!(
        for_secret(&checks, 2),
        vec![(CheckKind::Http, Some("github".into()))]
    );
    assert!(for_secret(&checks, 3).is_empty());
    assert!(
        for_secret(&checks, 4).is_empty(),
        "values above the inspection bound are never decrypted"
    );
    assert!(checks.iter().any(|check| check.env_var_id == Some(1)
        && check.kind == CheckKind::Local
        && check.automatic_provider.as_deref() == Some("local_expiry")));
    assert_eq!(
        query_strings(
            db,
            "SELECT secret_id::text AS id FROM secret_check_detection WHERE retry_after IS NOT NULL",
            "id"
        )
        .await,
        vec!["3"],
        "an undecryptable secret is retried later"
    );
    assert_eq!(
        query_strings(
            db,
            "SELECT kind FROM secret_history WHERE secret_id=3 ORDER BY id",
            "kind"
        )
        .await,
        vec!["created", "detection_unavailable"]
    );
    db.execute_unprepared(
        "UPDATE secret_check_detection SET retry_after=NOW()-INTERVAL '1 minute' WHERE secret_id=3",
    )
    .await
    .unwrap();
    service.reconcile_secrets().await.unwrap();
    assert_eq!(
        query_strings(
            db,
            "SELECT kind FROM secret_history WHERE secret_id=3 ORDER BY id",
            "kind"
        )
        .await,
        vec!["created", "detection_unavailable"],
        "retries of a still-unreadable secret are not recorded again"
    );

    let certificate_check = checks
        .iter()
        .find(|check| check.secret_id == Some(1))
        .unwrap()
        .id;
    let ran = service.run_now(1, certificate_check).await.unwrap();
    let result = ran.result.unwrap();
    assert_eq!(result.fingerprint(), "expires_within_7_days");
    let history = service.secret_history(1, 1, 1, 15).await.unwrap();
    let kinds: Vec<_> = history.items.iter().map(|e| e.kind.as_str()).collect();
    assert_eq!(kinds, vec!["verification", "check_added", "created"]);
    assert!(!serde_json::to_string(&history).unwrap().contains("BEGIN"));
    let detected = service.detect_secret(1, 1).await.unwrap().local_artifacts;
    assert_eq!(detected.len(), 1);
    assert_eq!(detected[0].label, "Certificate 'rcgen self signed cert'");

    // Rotating a token into a certificate switches the automatic check's kind.
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "UPDATE secrets SET value=$1 WHERE id=2",
        [encrypt(&certificate_expiring_in(60)).into()],
    ))
    .await
    .unwrap();
    service.reconcile_secrets().await.unwrap();
    assert_eq!(
        for_secret(&service.list(1, 1, 20).await.unwrap().items, 2),
        vec![(CheckKind::Local, Some("local_expiry".into()))]
    );

    // A manual check replaces the automatic one.
    db.execute_unprepared("INSERT INTO http_checks(project_id,secret_id,kind,name,encrypted_spec) VALUES(1,2,'local','Manual certificate','ciphertext')").await.unwrap();
    service.reconcile_secrets().await.unwrap();
    assert_eq!(
        for_secret(&service.list(1, 1, 20).await.unwrap().items, 2),
        vec![(CheckKind::Local, None)]
    );

    // Deleting an automatic check is remembered across rotation.
    service.delete(1, certificate_check).await.unwrap();
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "UPDATE secrets SET value=$1 WHERE id=1",
        [encrypt(&certificate_expiring_in(30)).into()],
    ))
    .await
    .unwrap();
    service.reconcile_secrets().await.unwrap();
    assert!(for_secret(&service.list(1, 1, 20).await.unwrap().items, 1).is_empty());
    assert_eq!(
        query_strings(
            db,
            "SELECT automatic_provider FROM secret_check_suppressions WHERE secret_id=1",
            "automatic_provider"
        )
        .await,
        vec!["local_expiry"]
    );

    let first = service.secret_history(1, 1, 1, 2).await.unwrap();
    let second = service.secret_history(1, 1, 2, 2).await.unwrap();
    assert_eq!(first.items.len(), 2);
    assert!(first.items[1].id > second.items[0].id);
    assert!(matches!(
        service.secret_history(99, 1, 1, 15).await,
        Err(
            temps_monitoring::http_checks::HttpChecksError::SecretNotFound {
                project_id: 99,
                secret_id: 1
            }
        )
    ));
}

/// Builders for structured credentials, generated in-test with generic names.
mod structured {
    use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
    use base64::Engine;

    pub fn expiry(days: i64) -> i64 {
        (chrono::Utc::now() + chrono::Duration::days(days)).timestamp()
    }
    pub fn jwt(days: i64) -> String {
        let encode = |json: String| URL_SAFE_NO_PAD.encode(json);
        format!(
            "{}.{}.c2lnbmF0dXJl",
            encode(r#"{"alg":"HS256"}"#.into()),
            encode(format!(r#"{{"sub":"svc","exp":{}}}"#, expiry(days)))
        )
    }
    pub fn kubeconfig(days: i64) -> String {
        let client = STANDARD.encode(super::certificate_expiring_in(days));
        format!("apiVersion: v1\nkind: Config\nusers:\n- name: ci\n  user:\n    client-certificate-data: {client}\n")
    }
    pub fn ssh_certificate(days: i64) -> String {
        let cert_type = "ssh-ed25519-cert-v01@openssh.com";
        let mut blob = Vec::new();
        let string = |out: &mut Vec<u8>, bytes: &[u8]| {
            out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
            out.extend_from_slice(bytes);
        };
        string(&mut blob, cert_type.as_bytes());
        string(&mut blob, &[7; 32]);
        string(&mut blob, &[9; 32]);
        blob.extend_from_slice(&1u64.to_be_bytes());
        blob.extend_from_slice(&1u32.to_be_bytes());
        string(&mut blob, b"deploy");
        string(&mut blob, b"");
        blob.extend_from_slice(&0u64.to_be_bytes());
        blob.extend_from_slice(&(expiry(days) as u64).to_be_bytes());
        format!("{cert_type} {}", STANDARD.encode(blob))
    }
}

#[tokio::test]
async fn env_vars_and_secrets_get_the_same_checks_and_results() {
    use temps_credential_checks::CheckKind;
    let database = match TestDatabase::new().await {
        Ok(db) => db,
        Err(error) if is_container_runtime_unavailable(&error.to_string()) => return,
        Err(error) => panic!("{error}"),
    };
    let db = database.connection();
    db.execute_unprepared("CREATE TABLE projects(id INTEGER PRIMARY KEY); CREATE TABLE env_vars(id INTEGER PRIMARY KEY,project_id INTEGER DEFAULT 1,key TEXT,value TEXT,is_encrypted BOOLEAN DEFAULT FALSE,is_secret BOOLEAN DEFAULT TRUE,include_in_preview BOOLEAN DEFAULT FALSE,environment_id INTEGER,created_at TIMESTAMPTZ DEFAULT NOW(),updated_at TIMESTAMPTZ DEFAULT NOW()); INSERT INTO projects VALUES(1);").await.unwrap();
    db.execute_unprepared(SECRETS_TABLE).await.unwrap();
    let schema = SchemaManager::new(db);
    HttpChecksMigration.up(&schema).await.unwrap();
    EnvCheckHistoryMigration.up(&schema).await.unwrap();
    DetectionRetryMigration.up(&schema).await.unwrap();
    CredentialCatalogMigration.up(&schema).await.unwrap();
    SecretChecksAndHistoryMigration.up(&schema).await.unwrap();
    let encryption = temps_core::EncryptionService::new_from_password("test-password");
    let values = [
        ("TLS_CERT", certificate_expiring_in(5)),
        ("KUBECONFIG", structured::kubeconfig(5)),
        ("DEPLOY_CERT", structured::ssh_certificate(5)),
        ("SERVICE_JWT", structured::jwt(5)),
        (
            "GITHUB_TOKEN",
            "ghp_abcdefghijklmnopqrstuvwxyz0123456789".into(),
        ),
        ("DATABASE_PASSWORD", "correct-horse-battery-staple".into()),
    ];
    for (id, (key, value)) in (1..).zip(&values) {
        db.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "INSERT INTO env_vars(id,key,value) VALUES($1,$2,$3)",
            [id.into(), (*key).into(), value.clone().into()],
        ))
        .await
        .unwrap();
        db.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "INSERT INTO secrets(id,key,value) VALUES($1,$2,$3)",
            [
                id.into(),
                (*key).into(),
                encryption.encrypt_string(value).unwrap().into(),
            ],
        ))
        .await
        .unwrap();
    }
    let service = temps_monitoring::http_checks::HttpChecksService::new(
        database.connection_arc(),
        std::sync::Arc::new(temps_core::EncryptionService::new_from_password(
            "test-password",
        )),
        std::sync::Arc::new(Notifications),
    )
    .unwrap();
    service.reconcile_variables().await.unwrap();
    service.reconcile_secrets().await.unwrap();
    let checks = service.list(1, 1, 100).await.unwrap().items;
    for (id, (key, _)) in (1..).zip(&values) {
        let variable: Vec<_> = checks.iter().filter(|c| c.env_var_id == Some(id)).collect();
        let secret: Vec<_> = checks.iter().filter(|c| c.secret_id == Some(id)).collect();
        let shape = |checks: &[&temps_monitoring::http_checks::HttpCheckView]| {
            checks
                .iter()
                .map(|c| (c.kind, c.automatic_provider.clone(), c.name.clone()))
                .collect::<Vec<_>>()
        };
        assert_eq!(shape(&variable), shape(&secret), "{key}");
        let expected = match *key {
            "GITHUB_TOKEN" => vec![(
                CheckKind::Http,
                Some("github".into()),
                "GitHub personal token verification".into(),
            )],
            "DATABASE_PASSWORD" => vec![],
            _ => vec![(
                CheckKind::Local,
                Some("local_expiry".into()),
                "Credential expiry".into(),
            )],
        };
        assert_eq!(shape(&variable), expected, "{key}");
        if let ([variable], [secret]) = (variable.as_slice(), secret.as_slice()) {
            if variable.kind == CheckKind::Local {
                let from_variable = service
                    .run_now(1, variable.id)
                    .await
                    .unwrap()
                    .result
                    .unwrap();
                let from_secret = service.run_now(1, secret.id).await.unwrap().result.unwrap();
                assert_eq!(from_variable.status, from_secret.status, "{key}");
                assert_eq!(
                    from_variable.fingerprint(),
                    "expires_within_7_days",
                    "{key}"
                );
                assert_eq!(
                    from_variable.fingerprint(),
                    from_secret.fingerprint(),
                    "{key}"
                );
                let messages = |r: &temps_credential_checks::VerificationResult| {
                    r.findings
                        .iter()
                        .map(|f| f.message.clone())
                        .collect::<Vec<_>>()
                };
                assert_eq!(messages(&from_variable), messages(&from_secret), "{key}");
            }
        }
    }

    // Renewing the certificate resets both checks; neither keeps the old warning.
    let renewed = certificate_expiring_in(200);
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "UPDATE env_vars SET value=$1 WHERE id=1",
        [renewed.clone().into()],
    ))
    .await
    .unwrap();
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "UPDATE secrets SET value=$1 WHERE id=1",
        [encryption.encrypt_string(&renewed).unwrap().into()],
    ))
    .await
    .unwrap();
    let after: Vec<_> = service
        .list(1, 1, 100)
        .await
        .unwrap()
        .items
        .into_iter()
        .filter(|c| c.env_var_id == Some(1) || c.secret_id == Some(1))
        .collect();
    assert_eq!(after.len(), 2);
    assert!(after.iter().all(|c| c.result.is_none()), "{after:?}");
    for check in &after {
        let result = service.run_now(1, check.id).await.unwrap().result.unwrap();
        assert_eq!(result.fingerprint(), "", "{check:?}");
    }
}
