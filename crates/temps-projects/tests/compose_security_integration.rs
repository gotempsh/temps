// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm::{ConnectionTrait, EntityTrait, PaginatorTrait};
use temps_database::test_utils::{is_container_runtime_unavailable, TestDatabase};
use temps_entities::{
    compose_security::{ComposeSecurityCheck, ComposeSecurityPolicy},
    compose_security_policies, compose_security_policy_changes,
};
use temps_migrations::{Migrator, MigratorTrait, SchemaManager};
use temps_projects::services::compose_security::{
    legacy_migration_pending, read, update, ComposeSecurityError,
};

#[tokio::test]
async fn compose_policy_migration_persistence_rollback_and_project_deletion() {
    let database = match TestDatabase::new().await {
        Ok(database) => database,
        Err(error) if is_container_runtime_unavailable(&error.to_string()) => {
            eprintln!(
                "Skipping Compose policy database test: container runtime unavailable: {error}"
            );
            return;
        }
        Err(error) => panic!("Cannot prepare isolated Compose policy database: {error}"),
    };
    let db = database.db.as_ref();
    // Keep the project identity, preset and legacy configuration columns so
    // the test exercises the real migration and service SQL without unrelated extensions.
    db.execute_unprepared(r#"CREATE TABLE projects (id INTEGER PRIMARY KEY, preset TEXT NOT NULL, preset_config JSONB); INSERT INTO projects VALUES (7, 'docker-compose', '{"unsandboxedServices":["proxy"]}')"#).await.unwrap();
    let migration = Migrator::migrations()
        .into_iter()
        .find(|migration| migration.name() == "m20260920_000001_compose_security_policies")
        .expect("Compose policy migration must be registered");
    let manager = SchemaManager::new(db);
    migration.up(&manager).await.unwrap();
    assert_eq!(read(db, 7).await.unwrap(), ComposeSecurityPolicy::default());
    assert!(legacy_migration_pending(db, 7).await.unwrap());
    db.execute_unprepared("UPDATE projects SET preset_config = '{}'::jsonb WHERE id = 7")
        .await
        .unwrap();
    assert!(
        legacy_migration_pending(db, 7).await.unwrap(),
        "unrelated project updates must preserve the notice"
    );
    let policy = ComposeSecurityPolicy {
        disabled_checks: [ComposeSecurityCheck::Extends].into(),
    };
    update(
        db,
        7,
        12,
        policy.clone(),
        true,
        ComposeSecurityPolicy::default(),
        false,
    )
    .await
    .unwrap();
    assert_eq!(read(db, 7).await.unwrap(), policy);
    let history = compose_security_policy_changes::Entity::find()
        .all(db)
        .await
        .unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].accepted_by, 12);
    assert_eq!(history[0].previous, ComposeSecurityPolicy::default());
    assert_eq!(history[0].policy, policy);

    // Two administrators read the same policy. The lock + compare admits exactly one writer.
    let privileged = ComposeSecurityPolicy {
        disabled_checks: [ComposeSecurityCheck::Privileged].into(),
    };
    let includes = ComposeSecurityPolicy {
        disabled_checks: [ComposeSecurityCheck::Include].into(),
    };
    let (a, b) = tokio::join!(
        update(db, 7, 12, privileged.clone(), true, policy.clone(), false),
        update(db, 7, 13, includes.clone(), true, policy.clone(), false),
    );
    assert_ne!(a.is_ok(), b.is_ok());
    assert!(matches!(
        a.as_ref().err().or(b.as_ref().err()),
        Some(ComposeSecurityError::Conflict { .. })
    ));
    let current = read(db, 7).await.unwrap();
    update(db, 7, 12, policy.clone(), true, current, false)
        .await
        .unwrap();
    assert!(legacy_migration_pending(db, 7).await.unwrap());

    db.execute_unprepared("CREATE FUNCTION fail_policy_history() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'history unavailable'; END $$; CREATE TRIGGER fail_history BEFORE INSERT ON compose_security_policy_changes FOR EACH ROW EXECUTE FUNCTION fail_policy_history()").await.unwrap();
    assert!(update(
        db,
        7,
        12,
        ComposeSecurityPolicy::default(),
        false,
        policy.clone(),
        true
    )
    .await
    .is_err());
    assert_eq!(
        read(db, 7).await.unwrap(),
        policy,
        "a failed audit insert must roll back the effective policy"
    );
    assert!(
        legacy_migration_pending(db, 7).await.unwrap(),
        "failed policy update must not acknowledge migration"
    );
    db.execute_unprepared("DROP TRIGGER fail_history ON compose_security_policy_changes")
        .await
        .unwrap();
    update(db, 7, 12, policy.clone(), false, policy.clone(), true)
        .await
        .unwrap();
    assert!(!legacy_migration_pending(db, 7).await.unwrap());
    db.execute_unprepared("DELETE FROM projects WHERE id = 7")
        .await
        .unwrap();
    assert_eq!(
        compose_security_policies::Entity::find()
            .count(db)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        compose_security_policy_changes::Entity::find()
            .count(db)
            .await
            .unwrap(),
        0
    );
    migration.down(&manager).await.unwrap();
    assert!(!manager
        .has_table("compose_security_legacy_migrations")
        .await
        .unwrap());
    assert!(!manager
        .has_table("compose_security_policies")
        .await
        .unwrap());
    assert!(!manager
        .has_table("compose_security_policy_changes")
        .await
        .unwrap());
}
