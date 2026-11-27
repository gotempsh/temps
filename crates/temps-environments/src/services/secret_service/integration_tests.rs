// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use chrono::Utc;
use std::time::Duration;
use temps_entities::{preset::Preset, projects, upstream_config::UpstreamList};
use temps_test_utils::{is_container_runtime_unavailable, TestDatabase};

const ENCRYPTION_KEY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

async fn test_database() -> Option<TestDatabase> {
    match TestDatabase::with_migrations().await {
        Ok(database) => Some(database),
        Err(error) if is_container_runtime_unavailable(&error.to_string()) => {
            eprintln!("Docker unavailable, skipping secret scoping integration test: {error:#}");
            None
        }
        Err(error) => panic!("secret scoping test database setup failed: {error:#}"),
    }
}

fn secret_service(test_db: &TestDatabase) -> SecretService {
    let encryption =
        EncryptionService::new(ENCRYPTION_KEY).expect("the test encryption key should be valid");
    SecretService::new(test_db.connection_arc(), Arc::new(encryption))
}

async fn create_project(test_db: &TestDatabase, suffix: &str) -> projects::Model {
    projects::ActiveModel {
        name: Set(format!("Secret scoping {suffix}")),
        repo_name: Set(format!("repo-{suffix}")),
        repo_owner: Set("temps-tests".to_string()),
        directory: Set("/".to_string()),
        main_branch: Set("main".to_string()),
        slug: Set(format!("secret-scoping-{suffix}")),
        preset: Set(Preset::NextJs),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        ..Default::default()
    }
    .insert(test_db.connection())
    .await
    .expect("project fixture should insert")
}

async fn create_environment(
    test_db: &TestDatabase,
    project_id: i32,
    suffix: &str,
) -> environments::Model {
    environments::ActiveModel {
        project_id: Set(project_id),
        name: Set(format!("Environment {suffix}")),
        slug: Set(suffix.to_string()),
        host: Set(format!("{suffix}.example.test")),
        upstreams: Set(UpstreamList::default()),
        subdomain: Set(format!("secret-scoping-{suffix}.example.test")),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        ..Default::default()
    }
    .insert(test_db.connection())
    .await
    .expect("environment fixture should insert")
}

async fn create_secret(
    service: &SecretService,
    project_id: i32,
    environment_ids: Vec<i32>,
    key: &str,
    value: &str,
) -> Result<SecretWithEnvironments, SecretError> {
    service
        .create(
            project_id,
            environment_ids,
            key.to_string(),
            value.to_string(),
            false,
            Vec::new(),
        )
        .await
}

async fn hold_key_scope_lock(
    test_db: &TestDatabase,
    project_id: i32,
    key: &str,
) -> DatabaseTransaction {
    let txn = test_db
        .connection()
        .begin()
        .await
        .expect("lock transaction should begin");
    txn.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "SELECT pg_advisory_xact_lock($1, hashtext($2))",
        [project_id.into(), key.to_string().into()],
    ))
    .await
    .expect("advisory lock should be acquired");
    txn
}

async fn wait_for_key_scope_waiter(test_db: &TestDatabase, project_id: i32, key: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let row = test_db.connection().query_one(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "SELECT EXISTS (SELECT 1 FROM pg_locks WHERE locktype = 'advisory' AND classid = $1::oid AND objid = hashtext($2)::oid AND NOT granted) AS waiting",
                    [project_id.into(), key.to_string().into()],
                )).await.expect("waiter query should succeed").expect("waiter query should return a row");
                if row.try_get::<bool>("", "waiting").expect("waiting should be boolean") {
                    break;
                }
                tokio::task::yield_now().await;
            }
        }).await.expect("secret write should wait after locking environments");
}

async fn soft_delete_environment(db: Arc<temps_database::DbConnection>, environment_id: i32) {
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "UPDATE environments SET deleted_at = NOW() WHERE id = $1",
        [environment_id.into()],
    ))
    .await
    .expect("environment soft deletion should succeed");
}

#[tokio::test]
async fn test_create_same_key_in_distinct_environments_returns_own_deploy_values() {
    let Some(test_db) = test_database().await else {
        return;
    };
    let project = create_project(&test_db, "distinct").await;
    let production = create_environment(&test_db, project.id, "distinct-production").await;
    let staging = create_environment(&test_db, project.id, "distinct-staging").await;
    let service = secret_service(&test_db);

    create_secret(
        &service,
        project.id,
        vec![production.id],
        "DATABASE_URL",
        "postgres://production",
    )
    .await
    .expect("production-scoped secret should be created");
    create_secret(
        &service,
        project.id,
        vec![staging.id],
        "DATABASE_URL",
        "postgres://staging",
    )
    .await
    .expect("disjoint staging-scoped secret should be created");

    let production_values = service
        .get_for_deploy(project.id, Some(production.id))
        .await
        .expect("production secrets should resolve");
    let staging_values = service
        .get_for_deploy(project.id, Some(staging.id))
        .await
        .expect("staging secrets should resolve");

    assert_eq!(
        production_values.get("DATABASE_URL").map(String::as_str),
        Some("postgres://production")
    );
    assert_eq!(production_values.len(), 1);
    assert_eq!(
        staging_values.get("DATABASE_URL").map(String::as_str),
        Some("postgres://staging")
    );
    assert_eq!(staging_values.len(), 1);
}

#[tokio::test]
async fn test_create_overlapping_same_environment_and_global_scopes_rejects_collisions() {
    let Some(test_db) = test_database().await else {
        return;
    };
    let project = create_project(&test_db, "collisions").await;
    let production = create_environment(&test_db, project.id, "collisions-production").await;
    let staging = create_environment(&test_db, project.id, "collisions-staging").await;
    let service = secret_service(&test_db);

    create_secret(&service, project.id, vec![production.id], "TOKEN", "one")
        .await
        .expect("first environment-scoped secret should be created");
    let same_environment = create_secret(&service, project.id, vec![production.id], "TOKEN", "two")
        .await
        .expect_err("the same key and environment should overlap");
    let global_after_scoped = create_secret(&service, project.id, Vec::new(), "TOKEN", "global")
        .await
        .expect_err("a global secret should overlap an environment-scoped secret");

    create_secret(&service, project.id, Vec::new(), "GLOBAL_TOKEN", "global")
        .await
        .expect("first global secret should be created");
    let scoped_after_global = create_secret(
        &service,
        project.id,
        vec![staging.id],
        "GLOBAL_TOKEN",
        "staging",
    )
    .await
    .expect_err("an environment-scoped secret should overlap a global secret");

    for error in [same_environment, global_after_scoped, scoped_after_global] {
        assert!(matches!(
            error,
            SecretError::KeyAlreadyExists { project_id, .. } if project_id == project.id
        ));
    }
}

#[tokio::test]
async fn test_update_to_overlapping_environment_rejects_and_rolls_back_original() {
    let Some(test_db) = test_database().await else {
        return;
    };
    let project = create_project(&test_db, "update-rollback").await;
    let production = create_environment(&test_db, project.id, "update-production").await;
    let staging = create_environment(&test_db, project.id, "update-staging").await;
    let service = secret_service(&test_db);
    let original = create_secret(
        &service,
        project.id,
        vec![production.id],
        "SHARED_KEY",
        "original-production",
    )
    .await
    .expect("production secret should be created");
    create_secret(
        &service,
        project.id,
        vec![staging.id],
        "SHARED_KEY",
        "original-staging",
    )
    .await
    .expect("staging secret should be created");

    let error = service
        .update(
            project.id,
            original.id,
            Some("replacement".to_string()),
            Some(vec![staging.id]),
            Some(true),
            Some(vec!["web".to_string()]),
        )
        .await
        .expect_err("moving onto an occupied key scope should fail");

    assert!(matches!(error, SecretError::KeyAlreadyExists { .. }));
    let production_values = service
        .get_for_deploy(project.id, Some(production.id))
        .await
        .expect("the original production scope should remain readable");
    let staging_values = service
        .get_for_deploy(project.id, Some(staging.id))
        .await
        .expect("the original staging scope should remain readable");
    assert_eq!(
        production_values.get("SHARED_KEY").map(String::as_str),
        Some("original-production")
    );
    assert_eq!(
        staging_values.get("SHARED_KEY").map(String::as_str),
        Some("original-staging")
    );
    let (unchanged, _) = service
        .list_page(project.id, Some(production.id), PageQuery::default())
        .await
        .expect("the original metadata should remain readable");
    assert_eq!(unchanged.len(), 1);
    assert!(!unchanged[0].include_in_preview);
    assert!(unchanged[0].compose_services.is_empty());
}

#[tokio::test]
async fn test_create_with_foreign_project_environment_rejects_without_inserting_secret() {
    let Some(test_db) = test_database().await else {
        return;
    };
    let owner = create_project(&test_db, "foreign-owner").await;
    let foreign = create_project(&test_db, "foreign-project").await;
    let foreign_environment =
        create_environment(&test_db, foreign.id, "foreign-project-environment").await;
    let service = secret_service(&test_db);

    let error = create_secret(
        &service,
        owner.id,
        vec![foreign_environment.id],
        "FOREIGN_SCOPE",
        "must-not-persist",
    )
    .await
    .expect_err("an environment owned by another project should be rejected");

    assert!(matches!(
        error,
        SecretError::EnvironmentNotFound {
            environment_id,
            project_id,
        } if environment_id == foreign_environment.id && project_id == owner.id
    ));
    assert!(service
        .list_page(owner.id, None, PageQuery::default())
        .await
        .expect("owner secrets should list")
        .0
        .is_empty());
}

#[tokio::test]
async fn test_concurrent_create_with_overlapping_scope_allows_exactly_one() {
    let Some(test_db) = test_database().await else {
        return;
    };
    let project = create_project(&test_db, "concurrent").await;
    let environment = create_environment(&test_db, project.id, "concurrent-environment").await;
    let service = secret_service(&test_db);

    let first_service = service.clone();
    let second_service = service.clone();
    let first = tokio::spawn(async move {
        create_secret(
            &first_service,
            project.id,
            vec![environment.id],
            "RACING_KEY",
            "first",
        )
        .await
    });
    let second = tokio::spawn(async move {
        create_secret(
            &second_service,
            project.id,
            vec![environment.id],
            "RACING_KEY",
            "second",
        )
        .await
    });

    let first_result = first.await.expect("first create task should complete");
    let second_result = second.await.expect("second create task should complete");
    let results = [first_result, second_result];
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Err(SecretError::KeyAlreadyExists { .. })))
            .count(),
        1
    );

    let visible = service
        .get_for_deploy(project.id, Some(environment.id))
        .await
        .expect("the winning secret should resolve");
    assert_eq!(visible.len(), 1);
    assert!(matches!(
        visible.get("RACING_KEY").map(String::as_str),
        Some("first" | "second")
    ));
}

#[tokio::test]
async fn create_holds_environment_share_lock_until_binding_commits() {
    let Some(test_db) = test_database().await else {
        return;
    };
    let project = create_project(&test_db, "create-delete-race").await;
    let environment = create_environment(&test_db, project.id, "create-delete-race").await;
    let blocker = hold_key_scope_lock(&test_db, project.id, "LOCKED_CREATE").await;
    let service = secret_service(&test_db);
    let project_id = project.id;
    let environment_id = environment.id;
    let create = tokio::spawn(async move {
        create_secret(
            &service,
            project_id,
            vec![environment_id],
            "LOCKED_CREATE",
            "value",
        )
        .await
    });
    wait_for_key_scope_waiter(&test_db, project_id, "LOCKED_CREATE").await;
    let delete = tokio::spawn(soft_delete_environment(
        test_db.connection_arc(),
        environment_id,
    ));
    let mut delete = Box::pin(delete);
    assert!(
        tokio::time::timeout(Duration::from_millis(200), &mut delete)
            .await
            .is_err()
    );
    blocker
        .commit()
        .await
        .expect("advisory blocker should commit");
    create
        .await
        .expect("create task should complete")
        .expect("create should win");
    delete.await.expect("deletion task should complete");
}

#[tokio::test]
async fn update_holds_environment_share_lock_until_binding_commits() {
    let Some(test_db) = test_database().await else {
        return;
    };
    let project = create_project(&test_db, "update-delete-race").await;
    let source = create_environment(&test_db, project.id, "update-delete-source").await;
    let target = create_environment(&test_db, project.id, "update-delete-target").await;
    let service = secret_service(&test_db);
    let secret = create_secret(
        &service,
        project.id,
        vec![source.id],
        "LOCKED_UPDATE",
        "old",
    )
    .await
    .expect("secret fixture should insert");
    let blocker = hold_key_scope_lock(&test_db, project.id, "LOCKED_UPDATE").await;
    let update_service = service.clone();
    let project_id = project.id;
    let target_id = target.id;
    let update = tokio::spawn(async move {
        update_service
            .update(
                project_id,
                secret.id,
                None,
                Some(vec![target_id]),
                Some(false),
                Some(Vec::new()),
            )
            .await
    });
    wait_for_key_scope_waiter(&test_db, project_id, "LOCKED_UPDATE").await;
    let delete = tokio::spawn(soft_delete_environment(test_db.connection_arc(), target_id));
    let mut delete = Box::pin(delete);
    assert!(
        tokio::time::timeout(Duration::from_millis(200), &mut delete)
            .await
            .is_err()
    );
    blocker
        .commit()
        .await
        .expect("advisory blocker should commit");
    update
        .await
        .expect("update task should complete")
        .expect("update should win");
    delete.await.expect("deletion task should complete");
}

#[tokio::test]
async fn deletion_committing_first_makes_create_reject_environment() {
    let Some(test_db) = test_database().await else {
        return;
    };
    let project = create_project(&test_db, "delete-create-race").await;
    let environment = create_environment(&test_db, project.id, "delete-create-race").await;
    let deletion = test_db
        .connection()
        .begin()
        .await
        .expect("deletion transaction should begin");
    deletion
        .execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE environments SET deleted_at = NOW() WHERE id = $1",
            [environment.id.into()],
        ))
        .await
        .expect("environment should be fenced");
    let service = secret_service(&test_db);
    let project_id = project.id;
    let environment_id = environment.id;
    let create = tokio::spawn(async move {
        create_secret(
            &service,
            project_id,
            vec![environment_id],
            "DELETE_FIRST",
            "value",
        )
        .await
    });
    tokio::task::yield_now().await;
    deletion.commit().await.expect("deletion should commit");
    assert!(matches!(create.await.expect("create task should complete"),
            Err(SecretError::EnvironmentNotFound { environment_id: id, .. }) if id == environment_id));
}

#[tokio::test]
async fn deletion_committing_first_makes_update_reject_environment() {
    let Some(test_db) = test_database().await else {
        return;
    };
    let project = create_project(&test_db, "delete-update-race").await;
    let source = create_environment(&test_db, project.id, "delete-update-source").await;
    let target = create_environment(&test_db, project.id, "delete-update-target").await;
    let service = secret_service(&test_db);
    let secret = create_secret(
        &service,
        project.id,
        vec![source.id],
        "DELETE_UPDATE",
        "value",
    )
    .await
    .expect("secret fixture should insert");
    let deletion = test_db
        .connection()
        .begin()
        .await
        .expect("deletion transaction should begin");
    deletion
        .execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE environments SET deleted_at = NOW() WHERE id = $1",
            [target.id.into()],
        ))
        .await
        .expect("environment should be fenced");
    let project_id = project.id;
    let target_id = target.id;
    let update = tokio::spawn(async move {
        service
            .update(
                project_id,
                secret.id,
                None,
                Some(vec![target_id]),
                Some(false),
                Some(Vec::new()),
            )
            .await
    });
    tokio::task::yield_now().await;
    deletion.commit().await.expect("deletion should commit");
    assert!(matches!(update.await.expect("update task should complete"),
            Err(SecretError::EnvironmentNotFound { environment_id: id, .. }) if id == target_id));
}

#[tokio::test]
async fn secret_writes_and_running_checks_never_deadlock() {
    let Some(test_db) = test_database().await else {
        return;
    };
    let project = create_project(&test_db, "lock-order").await;
    for operation in ["update", "delete"] {
        let service = secret_service(&test_db);
        let secret = create_secret(
            &service,
            project.id,
            vec![],
            &format!("LOCK_ORDER_{}", operation.to_uppercase()),
            "first",
        )
        .await
        .expect("secret fixture should insert");
        test_db
                .connection()
                .execute(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "INSERT INTO http_checks(project_id,secret_id,kind,name,encrypted_spec) VALUES($1,$2,'local','Credential expiry','ciphertext')",
                    [project.id.into(), secret.id.into()],
                ))
                .await
                .expect("check fixture should insert");
        // A running check holds its row before it records a result.
        let check = test_db
            .connection()
            .begin()
            .await
            .expect("check transaction should begin");
        check
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT id FROM http_checks WHERE secret_id = $1 FOR UPDATE",
                [secret.id.into()],
            ))
            .await
            .expect("check row should lock");
        let (project_id, secret_id) = (project.id, secret.id);
        let write = tokio::spawn(async move {
            if operation == "update" {
                service
                    .update(
                        project_id,
                        secret_id,
                        Some("rotated".into()),
                        Some(vec![]),
                        Some(false),
                        Some(vec![]),
                    )
                    .await
                    .map(|_| ())
            } else {
                service.delete(project_id, secret_id).await.map(|_| ())
            }
        });
        // Let the write lock the secret and queue behind the check row.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        // Recording the result inserts secret history, which key-share locks the secret.
        check
                .execute(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    r#"UPDATE http_checks SET last_checked_at=NOW(),last_result='{"status":"healthy","findings":[],"checked_at":"2026-10-03T00:00:00Z"}' WHERE secret_id = $1"#,
                    [secret.id.into()],
                ))
                .await
                .unwrap_or_else(|e| panic!("recording a result during a secret {operation} failed: {e}"));
        check.commit().await.expect("check should commit");
        write
            .await
            .expect("write task should complete")
            .unwrap_or_else(|e| panic!("secret {operation} after a running check failed: {e}"));
    }
}

async fn secret_history_kinds(test_db: &TestDatabase, secret_id: i32) -> Vec<String> {
    secret_history::Entity::find()
        .filter(secret_history::Column::SecretId.eq(secret_id))
        .order_by_asc(secret_history::Column::Id)
        .all(test_db.connection())
        .await
        .expect("secret history should load")
        .into_iter()
        .map(|entry| entry.kind)
        .collect()
}

async fn bound_environment_ids(test_db: &TestDatabase, secret_id: i32) -> Vec<i32> {
    secret_environments::Entity::find()
        .filter(secret_environments::Column::SecretId.eq(secret_id))
        .order_by_asc(secret_environments::Column::EnvironmentId)
        .all(test_db.connection())
        .await
        .expect("bindings should load")
        .into_iter()
        .map(|binding| binding.environment_id)
        .collect()
}

#[tokio::test]
async fn update_of_secret_bound_to_deleted_environment_keeps_the_binding() {
    let Some(test_db) = test_database().await else {
        return;
    };
    let service = secret_service(&test_db);
    let project = create_project(&test_db, "deleted-binding").await;
    let live = create_environment(&test_db, project.id, "deleted-binding-live").await;
    let removed = create_environment(&test_db, project.id, "deleted-binding-removed").await;
    let secret = create_secret(&service, project.id, vec![removed.id], "SCOPED", "value")
        .await
        .expect("secret fixture should insert");
    soft_delete_environment(test_db.connection_arc(), removed.id).await;

    // Echoing the stale id back (as a client that re-sends the current
    // scope does) is accepted, keeps the binding and is not a scope change.
    let updated = service
        .update(
            project.id,
            secret.id,
            Some("rotated".into()),
            Some(vec![removed.id]),
            Some(false),
            Some(Vec::new()),
        )
        .await
        .expect("a stale id for a bound deleted environment must not block the save");
    assert_eq!(
        updated
            .environments
            .iter()
            .map(|e| e.id)
            .collect::<Vec<_>>(),
        vec![removed.id]
    );
    assert_eq!(
        bound_environment_ids(&test_db, secret.id).await,
        vec![removed.id]
    );
    assert_eq!(
        secret_history_kinds(&test_db, secret.id).await,
        vec!["created", "value_changed"]
    );

    // Clearing the selection must not drop the deleted binding: with no
    // bindings left the secret would be mounted in every environment.
    service
        .update(
            project.id,
            secret.id,
            None,
            Some(vec![]),
            Some(false),
            Some(Vec::new()),
        )
        .await
        .expect("clearing the live selection should succeed");
    assert_eq!(
        bound_environment_ids(&test_db, secret.id).await,
        vec![removed.id]
    );

    // A live environment is added next to it, not instead of it.
    service
        .update(
            project.id,
            secret.id,
            None,
            Some(vec![live.id]),
            Some(false),
            Some(Vec::new()),
        )
        .await
        .expect("adding a live environment should succeed");
    let mut expected = vec![live.id, removed.id];
    expected.sort();
    assert_eq!(bound_environment_ids(&test_db, secret.id).await, expected);
}

#[tokio::test]
async fn update_rejects_environment_from_another_project() {
    let Some(test_db) = test_database().await else {
        return;
    };
    let service = secret_service(&test_db);
    let project = create_project(&test_db, "foreign-update-owner").await;
    let other = create_project(&test_db, "foreign-update-other").await;
    let live = create_environment(&test_db, project.id, "foreign-update-live").await;
    let foreign = create_environment(&test_db, other.id, "foreign-update-env").await;
    let secret = create_secret(&service, project.id, vec![live.id], "FOREIGN", "value")
        .await
        .expect("secret fixture should insert");

    let error = service
        .update(
            project.id,
            secret.id,
            None,
            Some(vec![live.id, foreign.id]),
            Some(false),
            Some(Vec::new()),
        )
        .await
        .expect_err("another project's environment must be rejected");
    assert!(
        matches!(
            error,
            SecretError::EnvironmentNotFound { environment_id, project_id }
                if environment_id == foreign.id && project_id == project.id
        ),
        "unexpected error: {error:?}"
    );
    assert_eq!(
        bound_environment_ids(&test_db, secret.id).await,
        vec![live.id]
    );
}

#[tokio::test]
async fn updates_record_value_preview_and_scope_history_without_values() {
    let Some(test_db) = test_database().await else {
        return;
    };
    let service = secret_service(&test_db);
    let project = create_project(&test_db, "history").await;
    let staging = create_environment(&test_db, project.id, "history-staging").await;
    let production = create_environment(&test_db, project.id, "history-production").await;
    let secret = create_secret(&service, project.id, vec![staging.id], "TLS_CERT", "first")
        .await
        .expect("secret should be created");

    // Re-saving the same scope and preview flag records nothing.
    service
        .update(
            project.id,
            secret.id,
            None,
            Some(vec![staging.id]),
            Some(false),
            Some(vec![]),
        )
        .await
        .expect("no-op update should succeed");
    assert_eq!(
        secret_history_kinds(&test_db, secret.id).await,
        vec!["created"]
    );

    service
        .update(
            project.id,
            secret.id,
            Some("rotated-plaintext".into()),
            Some(vec![production.id]),
            Some(true),
            Some(vec![]),
        )
        .await
        .expect("update should succeed");
    assert_eq!(
        secret_history_kinds(&test_db, secret.id).await,
        vec![
            "created",
            "value_changed",
            "settings_changed",
            "scope_changed"
        ]
    );
    let scope = secret_history::Entity::find()
        .filter(secret_history::Column::SecretId.eq(secret.id))
        .filter(secret_history::Column::Kind.eq("scope_changed"))
        .one(test_db.connection())
        .await
        .expect("scope entry should load")
        .expect("scope entry should exist");
    assert_eq!(scope.project_id, project.id);
    assert_eq!(
        scope.details,
        serde_json::json!({"environment_ids": [production.id], "compose_services": []})
    );
    let all_details = secret_history::Entity::find()
        .filter(secret_history::Column::SecretId.eq(secret.id))
        .all(test_db.connection())
        .await
        .expect("history should load")
        .into_iter()
        .map(|entry| entry.details.to_string())
        .collect::<String>();
    assert!(!all_details.contains("rotated-plaintext"));
    assert!(!all_details.contains("first"));

    let key = service
        .delete(project.id, secret.id)
        .await
        .expect("delete should succeed");
    assert_eq!(key, "TLS_CERT");
    assert!(secret_history_kinds(&test_db, secret.id).await.is_empty());
}

#[tokio::test]
async fn rotating_only_the_value_keeps_the_secret_scope() {
    let Some(test_db) = test_database().await else {
        return;
    };
    let service = secret_service(&test_db);
    let project = create_project(&test_db, "rotate-only").await;
    let production = create_environment(&test_db, project.id, "rotate-production").await;
    let staging = create_environment(&test_db, project.id, "rotate-staging").await;
    let secret = service
        .create(
            project.id,
            vec![production.id],
            "API_TOKEN".to_string(),
            "first".to_string(),
            true,
            vec!["web".to_string()],
        )
        .await
        .expect("secret should be created");

    let rotated = service
        .update(
            project.id,
            secret.id,
            Some("second".to_string()),
            None,
            None,
            None,
        )
        .await
        .expect("a value-only update should succeed");

    assert_eq!(
        rotated
            .environments
            .iter()
            .map(|env| env.id)
            .collect::<Vec<_>>(),
        vec![production.id],
        "the response reports the kept environments"
    );
    assert!(rotated.include_in_preview, "the preview flag is kept");
    assert_eq!(rotated.compose_services, vec!["web".to_string()]);
    assert_eq!(
        bound_environment_ids(&test_db, secret.id).await,
        vec![production.id],
        "the stored bindings are untouched"
    );

    let production_secrets = service
        .get_for_deploy(project.id, Some(production.id))
        .await
        .expect("production secrets should load");
    assert_eq!(
        production_secrets.get("API_TOKEN").map(String::as_str),
        Some("second")
    );
    let staging_secrets = service
        .get_for_deploy(project.id, Some(staging.id))
        .await
        .expect("staging secrets should load");
    assert!(
        !staging_secrets.contains_key("API_TOKEN"),
        "a production-only secret must not reach staging after a rotation"
    );

    let stored = service
        .get(project.id, secret.id)
        .await
        .expect("secret should load");
    assert!(stored.include_in_preview);
    assert_eq!(stored.compose_services, vec!["web".to_string()]);
    assert!(
        !secret_history_kinds(&test_db, secret.id)
            .await
            .contains(&"scope_changed".to_string()),
        "a value rotation is not a scope change"
    );
}

#[tokio::test]
async fn each_scope_field_is_replaced_only_when_given() {
    let Some(test_db) = test_database().await else {
        return;
    };
    let service = secret_service(&test_db);
    let project = create_project(&test_db, "one-field").await;
    let production = create_environment(&test_db, project.id, "one-field-production").await;
    let staging = create_environment(&test_db, project.id, "one-field-staging").await;
    let secret = service
        .create(
            project.id,
            vec![production.id],
            "SIGNING_KEY".to_string(),
            "value".to_string(),
            true,
            vec!["web".to_string(), "worker".to_string()],
        )
        .await
        .expect("secret should be created");

    // Only the preview flag.
    let updated = service
        .update(project.id, secret.id, None, None, Some(false), None)
        .await
        .expect("preview-only update should succeed");
    assert!(!updated.include_in_preview);
    assert_eq!(
        bound_environment_ids(&test_db, secret.id).await,
        vec![production.id]
    );
    assert_eq!(
        updated.compose_services,
        vec!["web".to_string(), "worker".to_string()]
    );

    // Only the compose services.
    let updated = service
        .update(
            project.id,
            secret.id,
            None,
            None,
            None,
            Some(vec!["worker".to_string()]),
        )
        .await
        .expect("services-only update should succeed");
    assert_eq!(updated.compose_services, vec!["worker".to_string()]);
    assert!(!updated.include_in_preview);
    assert_eq!(
        bound_environment_ids(&test_db, secret.id).await,
        vec![production.id]
    );

    // Only the environments.
    let updated = service
        .update(
            project.id,
            secret.id,
            None,
            Some(vec![staging.id]),
            None,
            None,
        )
        .await
        .expect("environments-only update should succeed");
    assert_eq!(
        bound_environment_ids(&test_db, secret.id).await,
        vec![staging.id]
    );
    assert_eq!(updated.compose_services, vec!["worker".to_string()]);
    assert!(!updated.include_in_preview);

    // An explicit empty list still means every environment, as on create.
    let updated = service
        .update(project.id, secret.id, None, Some(vec![]), None, None)
        .await
        .expect("an explicit project-wide scope should be accepted");
    assert!(updated.environments.is_empty());
    assert!(bound_environment_ids(&test_db, secret.id).await.is_empty());
}

#[tokio::test]
async fn value_only_update_of_a_missing_secret_names_it() {
    let Some(test_db) = test_database().await else {
        return;
    };
    let service = secret_service(&test_db);
    let project = create_project(&test_db, "missing").await;

    let error = service
        .update(project.id, 999_999, Some("x".to_string()), None, None, None)
        .await
        .expect_err("a missing secret must be reported");
    assert!(
        matches!(
            error,
            SecretError::NotFound { secret_id: 999_999, project_id } if project_id == project.id
        ),
        "{error:?}"
    );
}
