// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `projects.allowed_capabilities` against a real PostgreSQL: the migration,
//! the service SQL, the CHECK constraint that refuses anything outside the
//! allow-list even for a direct write, and the down migration.

use std::sync::Arc;

use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};
use temps_migrations::{Migrator, MigratorTrait, SchemaManager};
use temps_projects::services::container_security::{
    ContainerSecurityError, ContainerSecurityService,
};
use temps_test_utils::{is_container_runtime_unavailable, TestDatabase};
use temps_workload::ContainerCapability;

const MIGRATION: &str = "m20261122_000001_add_allowed_capabilities_to_projects";

async fn column_exists(db: &DatabaseConnection) -> Result<bool, sea_orm::DbErr> {
    Ok(db
        .query_one(Statement::from_string(
            db.get_database_backend(),
            "SELECT 1 FROM information_schema.columns WHERE table_name = 'projects' \
         AND column_name = 'allowed_capabilities' AND table_schema = current_schema()",
        ))
        .await?
        .is_some())
}

#[tokio::test]
async fn allowed_capabilities_migration_service_constraint_and_rollback() {
    let database = match TestDatabase::new().await {
        Ok(database) => database,
        Err(error) if is_container_runtime_unavailable(&error.to_string()) => {
            eprintln!(
                "Skipping container security database test: container runtime unavailable: {error}"
            );
            return;
        }
        Err(error) => panic!("Cannot prepare isolated container security database: {error}"),
    };
    let db = database.db.as_ref();
    // Only the columns the migration and the service touch.
    db.execute_unprepared(
        "CREATE TABLE projects (id INTEGER PRIMARY KEY, is_deleted BOOLEAN NOT NULL DEFAULT false, \
         updated_at TIMESTAMPTZ NOT NULL DEFAULT now()); \
         INSERT INTO projects (id) VALUES (7); \
         INSERT INTO projects (id, is_deleted) VALUES (8, true)",
    )
    .await
    .expect("projects table");
    let migration = Migrator::migrations()
        .into_iter()
        .find(|migration| migration.name() == MIGRATION)
        .expect("the allowed capabilities migration is registered");
    let manager = SchemaManager::new(db);
    migration.up(&manager).await.expect("migration up");
    // Idempotent: a re-run after a partial apply does not fail.
    migration.up(&manager).await.expect("migration up again");

    let service = ContainerSecurityService::new(database.db.clone() as Arc<DatabaseConnection>);
    assert!(
        service
            .allowed_capabilities(7)
            .await
            .expect("read")
            .is_empty(),
        "existing projects default to fully hardened"
    );

    let nbs = vec!["NET_BIND_SERVICE".to_string()];
    let change = service
        .update_allowed_capabilities(7, &nbs, &[], true)
        .await
        .expect("allow NET_BIND_SERVICE");
    assert!(change.previous.is_empty());
    assert_eq!(
        service.allowed_capabilities(7).await.expect("read back"),
        vec![ContainerCapability::NetBindService]
    );

    // A stale editor cannot undo it.
    assert!(matches!(
        service
            .update_allowed_capabilities(7, &[], &[], false)
            .await,
        Err(ContainerSecurityError::Conflict { project_id: 7, .. })
    ));
    // A deleted project is not found.
    assert!(matches!(
        service.allowed_capabilities(8).await,
        Err(ContainerSecurityError::NotFound { project_id: 8 })
    ));

    // The CHECK constraint refuses anything else, whoever writes it.
    for forbidden in ["SYS_ADMIN", "ALL", "NET_RAW"] {
        let result = db
            .execute_unprepared(&format!(
                "UPDATE projects SET allowed_capabilities = ARRAY['{forbidden}'] WHERE id = 7"
            ))
            .await;
        assert!(result.is_err(), "{forbidden} must violate the constraint");
    }
    assert_eq!(
        service.allowed_capabilities(7).await.expect("unchanged"),
        vec![ContainerCapability::NetBindService]
    );

    // Removing it needs no acknowledgement.
    service
        .update_allowed_capabilities(7, &[], &nbs, false)
        .await
        .expect("back to fully hardened");
    assert!(service
        .allowed_capabilities(7)
        .await
        .expect("read")
        .is_empty());

    migration.down(&manager).await.expect("migration down");
    assert!(
        !column_exists(db)
            .await
            .expect("information_schema is readable"),
        "down drops the column"
    );
}
