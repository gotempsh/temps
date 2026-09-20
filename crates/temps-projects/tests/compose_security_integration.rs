// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm::{ConnectionTrait, EntityTrait, PaginatorTrait};
use temps_database::test_utils::{is_container_runtime_unavailable, TestDatabase};
use temps_entities::{
    compose_security::{ComposeSecurityCheck, ComposeSecurityPolicy},
    compose_security_policies, compose_security_policy_changes,
};
use temps_migrations::{Migrator, MigratorTrait, SchemaManager};
use temps_projects::services::compose_security::{read, update};

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
    // This migration only depends on projects.id. Keep the schema minimal so
    // the test exercises the real migration and service SQL without unrelated extensions.
    db.execute_unprepared("CREATE TABLE projects (id INTEGER PRIMARY KEY, preset TEXT NOT NULL); INSERT INTO projects VALUES (7, 'docker-compose')").await.unwrap();
    let migration = Migrator::migrations()
        .into_iter()
        .find(|migration| migration.name() == "m20260920_000001_compose_security_policies")
        .expect("Compose policy migration must be registered");
    let manager = SchemaManager::new(db);
    migration.up(&manager).await.unwrap();
    assert_eq!(read(db, 7).await.unwrap(), ComposeSecurityPolicy::default());
    let policy = ComposeSecurityPolicy {
        disabled_checks: [ComposeSecurityCheck::Extends].into(),
    };
    update(db, 7, 12, policy.clone(), true).await.unwrap();
    assert_eq!(read(db, 7).await.unwrap(), policy);
    let history = compose_security_policy_changes::Entity::find()
        .all(db)
        .await
        .unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].accepted_by, 12);
    assert_eq!(history[0].previous, ComposeSecurityPolicy::default());
    assert_eq!(history[0].policy, policy);

    db.execute_unprepared("CREATE FUNCTION fail_policy_history() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'history unavailable'; END $$; CREATE TRIGGER fail_history BEFORE INSERT ON compose_security_policy_changes FOR EACH ROW EXECUTE FUNCTION fail_policy_history()").await.unwrap();
    assert!(update(db, 7, 12, ComposeSecurityPolicy::default(), false)
        .await
        .is_err());
    assert_eq!(
        read(db, 7).await.unwrap(),
        policy,
        "a failed audit insert must roll back the effective policy"
    );
    db.execute_unprepared("DROP TRIGGER fail_history ON compose_security_policy_changes; DELETE FROM projects WHERE id = 7").await.unwrap();
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
        .has_table("compose_security_policies")
        .await
        .unwrap());
    assert!(!manager
        .has_table("compose_security_policy_changes")
        .await
        .unwrap());
}
