// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Saved searches against a real database (ADR 111, D1.8): names unique per
//! owner ignoring case, the per-user cap, environments of the project only,
//! and what happens when an environment or the owner is deleted.

use super::tests::create_project;
use super::*;
use temps_test_utils::{is_container_runtime_unavailable, TestDatabase};

macro_rules! test_db {
    ($what:literal) => {
        match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(error) if is_container_runtime_unavailable(&error.to_string()) => {
                eprintln!(concat!("Skipping ", $what, ": {}"), error);
                return;
            }
            Err(error) => panic!(concat!($what, " database setup failed: {}"), error),
        }
    };
}

async fn create_user(db: &DatabaseConnection, name: &str) -> i32 {
    db.query_one(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "INSERT INTO users (name, email, email_verified, must_change_password, mfa_enabled, \
                            created_at, updated_at) \
         VALUES ($1, $2, TRUE, FALSE, FALSE, NOW(), NOW()) RETURNING id",
        [
            name.into(),
            format!("{}-{}@example.test", name, uuid::Uuid::new_v4()).into(),
        ],
    ))
    .await
    .expect("insert user")
    .expect("user row")
    .try_get("", "id")
    .expect("user id")
}

async fn create_environment(db: &DatabaseConnection, project: i32) -> i32 {
    db.query_one(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "INSERT INTO environments (name, slug, subdomain, host, upstreams, created_at, updated_at, project_id) \
         VALUES ('staging', 'staging', 'staging-' || $1::text, 'staging.example.test', '[]', NOW(), NOW(), $1) \
         RETURNING id",
        [project.into()],
    ))
    .await
    .expect("insert environment")
    .expect("environment row")
    .try_get("", "id")
    .expect("environment id")
}

fn open_in(environment_id: Option<i32>) -> SavedSearchFilters {
    SavedSearchFilters {
        status: Some("unresolved".into()),
        environment_id,
        ..Default::default()
    }
}

#[tokio::test]
async fn names_are_unique_per_owner_ignoring_case() {
    let test_db = test_db!("saved search names");
    let db = test_db.connection_arc();
    let project = create_project(db.as_ref()).await;
    let owner = create_user(db.as_ref(), "mia").await;
    let other = create_user(db.as_ref(), "ned").await;
    let service = GroupWorkflowService::new(db.clone());

    let first = service
        .create_saved_search(project, owner, " Open ones ", false, open_in(None))
        .await
        .expect("create");
    assert_eq!(first.name, "Open ones");
    assert_eq!(
        first.filters.sort_order.as_deref(),
        Some("desc"),
        "the default order is stored"
    );
    let error = service
        .create_saved_search(project, owner, "OPEN ONES", false, open_in(None))
        .await
        .expect_err("same name, other case");
    assert!(
        matches!(&error, ErrorTrackingError::SavedSearchExists { project_id, name }
            if *project_id == project && name == "OPEN ONES"),
        "{error:?}"
    );
    service
        .create_saved_search(project, other, "Open ones", true, open_in(None))
        .await
        .expect("another owner may use the name");

    let second = service
        .create_saved_search(
            project,
            owner,
            "Resolved",
            false,
            SavedSearchFilters::default(),
        )
        .await
        .expect("second");
    let error = service
        .update_saved_search(
            project,
            second.id,
            owner,
            SavedSearchChange {
                name: Some("open ONES".into()),
                ..Default::default()
            },
        )
        .await
        .expect_err("rename onto an existing name");
    assert!(
        matches!(error, ErrorTrackingError::SavedSearchExists { .. }),
        "{error:?}"
    );

    let page = service
        .list_saved_searches(project, Some(owner), 1, 20)
        .await
        .expect("list");
    assert_eq!(page.total, 3, "two of mine and one shared");
    let page = service
        .list_saved_searches(project, None, 1, 20)
        .await
        .expect("list without a user");
    assert_eq!(page.total, 1, "only the shared one");
}

#[tokio::test]
async fn a_user_holds_at_most_the_cap_per_project() {
    let test_db = test_db!("saved search cap");
    let db = test_db.connection_arc();
    let project = create_project(db.as_ref()).await;
    let other_project = create_project(db.as_ref()).await;
    let owner = create_user(db.as_ref(), "ola").await;
    let service = GroupWorkflowService::new(db.clone());

    for n in 0..MAX_SAVED_SEARCHES_PER_USER - 1 {
        service
            .create_saved_search(project, owner, &format!("search {n}"), false, open_in(None))
            .await
            .expect("under the cap");
    }
    // Two requests race for the last slot: exactly one gets it.
    let (a, b) = tokio::join!(
        service.create_saved_search(project, owner, "race a", false, open_in(None)),
        service.create_saved_search(project, owner, "race b", false, open_in(None)),
    );
    assert_eq!(
        usize::from(a.is_ok()) + usize::from(b.is_ok()),
        1,
        "one of {a:?} / {b:?}"
    );
    let error = service
        .create_saved_search(project, owner, "one too many", false, open_in(None))
        .await
        .expect_err("over the cap");
    assert!(
        matches!(error, ErrorTrackingError::SavedSearchLimit { project_id, user_id, limit }
            if project_id == project && user_id == owner && limit == MAX_SAVED_SEARCHES_PER_USER),
        "{error:?}"
    );
    service
        .create_saved_search(other_project, owner, "one too many", false, open_in(None))
        .await
        .expect("the cap is per project");
}

#[tokio::test]
async fn environments_must_be_the_projects_and_their_deletion_drops_the_filter() {
    let test_db = test_db!("saved search environments");
    let db = test_db.connection_arc();
    let project = create_project(db.as_ref()).await;
    let other_project = create_project(db.as_ref()).await;
    let mine = create_environment(db.as_ref(), project).await;
    let theirs = create_environment(db.as_ref(), other_project).await;
    let owner = create_user(db.as_ref(), "pia").await;
    let service = GroupWorkflowService::new(db.clone());

    let error = service
        .create_saved_search(project, owner, "Elsewhere", false, open_in(Some(theirs)))
        .await
        .expect_err("another project's environment");
    assert!(error.to_string().contains(&theirs.to_string()), "{error}");

    let search = service
        .create_saved_search(project, owner, "Staging", false, open_in(Some(mine)))
        .await
        .expect("create");
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "DELETE FROM environments WHERE id = $1",
        [mine.into()],
    ))
    .await
    .expect("delete environment");
    let search = service
        .get_saved_search(project, search.id, Some(owner))
        .await
        .expect("still there");
    assert_eq!(search.filters.environment_id, None);
    assert_eq!(
        search.filters.status.as_deref(),
        Some("unresolved"),
        "the rest stays"
    );
}

#[tokio::test]
async fn only_the_owner_changes_a_search_and_it_goes_with_them() {
    let test_db = test_db!("saved search ownership");
    let db = test_db.connection_arc();
    let project = create_project(db.as_ref()).await;
    let owner = create_user(db.as_ref(), "quin").await;
    let other = create_user(db.as_ref(), "rae").await;
    let service = GroupWorkflowService::new(db.clone());

    let private = service
        .create_saved_search(project, owner, "Private", false, open_in(None))
        .await
        .expect("private");
    let shared = service
        .create_saved_search(project, owner, "Shared", true, open_in(None))
        .await
        .expect("shared");

    let error = service
        .get_saved_search(project, private.id, Some(other))
        .await
        .expect_err("someone else's private search");
    assert!(
        matches!(error, ErrorTrackingError::SavedSearchNotFound { saved_search_id, .. }
            if saved_search_id == private.id),
        "{error:?}"
    );
    let error = service
        .delete_saved_search(project, private.id, Some(other), true)
        .await
        .expect_err("an administrator cannot see a private search either");
    assert!(
        matches!(error, ErrorTrackingError::SavedSearchNotFound { .. }),
        "{error:?}"
    );
    let error = service
        .delete_saved_search(project, shared.id, Some(other), false)
        .await
        .expect_err("not the owner");
    assert!(error.to_string().contains("or an administrator"), "{error}");
    let changed = service
        .update_saved_search(
            project,
            shared.id,
            owner,
            SavedSearchChange {
                shared: Some(false),
                filters: Some(SavedSearchFilters {
                    search: Some("timeout".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await
        .expect("owner changes it");
    assert!(!changed.shared);
    assert_eq!(
        changed.filters.status, None,
        "filters are replaced as a whole"
    );
    assert_eq!(changed.filters.search.as_deref(), Some("timeout"));

    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "DELETE FROM users WHERE id = $1",
        [owner.into()],
    ))
    .await
    .expect("delete owner");
    assert_eq!(
        service
            .list_saved_searches(project, Some(other), 1, 20)
            .await
            .expect("list")
            .total,
        0
    );
}
