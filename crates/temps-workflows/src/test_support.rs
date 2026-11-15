// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Shared fixtures for the database-backed tests of this crate.

use std::sync::Arc;

use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};
use temps_test_utils::TestDatabase;

/// A migrated database of its own with one project and one environment.
pub struct TestEnvironment {
    pub db: TestDatabase,
    pub conn: Arc<DatabaseConnection>,
    pub project_id: i32,
    pub environment_id: i32,
    pub user_id: i32,
}

/// `None`, with a message, when no database can be started here.
pub async fn test_environment(test: &str) -> Option<TestEnvironment> {
    let db = match TestDatabase::with_migrations().await {
        Ok(db) => db,
        Err(error) => {
            eprintln!("TestDatabase unavailable; skipping {test}: {error}");
            return None;
        }
    };
    let conn = db.connection_arc();
    conn.execute_unprepared(
        "INSERT INTO projects \
         (name, repo_name, repo_owner, directory, main_branch, preset, created_at, updated_at, slug) \
         VALUES ('Workflows', '', '', '.', 'main', 'dockerfile', now(), now(), 'workflows-test')",
    )
    .await
    .expect("insert project");
    conn.execute_unprepared(
        "INSERT INTO environments (name, slug, subdomain, host, upstreams, created_at, updated_at, project_id) \
         SELECT 'production', 'production', 'workflows-test-production', 'workflows.test', '[]', now(), now(), id \
         FROM projects WHERE slug = 'workflows-test'",
    )
    .await
    .expect("insert environment");
    let row = conn
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            "SELECT e.id AS eid, e.project_id AS pid FROM environments e \
             JOIN projects p ON p.id = e.project_id WHERE p.slug = 'workflows-test'",
        ))
        .await
        .expect("query environment")
        .expect("environment row");
    let user_id: i32 = conn
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            "INSERT INTO users (name, email, created_at, updated_at) \
             VALUES ('Workflow operator', 'workflows@example.test', now(), now()) RETURNING id",
        ))
        .await
        .expect("insert user")
        .expect("user row")
        .try_get("", "id")
        .expect("user id");
    Some(TestEnvironment {
        user_id,
        project_id: row.try_get("", "pid").expect("pid"),
        environment_id: row.try_get("", "eid").expect("eid"),
        conn,
        db,
    })
}

/// Make a run due now, as if its sleep or backoff had passed.
pub async fn make_due(conn: &DatabaseConnection, run_id: uuid::Uuid) {
    conn.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE workflow_runs SET due_at = now() - interval '1 second' WHERE id = $1",
        vec![run_id.into()],
    ))
    .await
    .expect("make due");
}
