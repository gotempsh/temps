// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Assignees against a real database (ADR 111, D1.6): the upgrade keeps old
//! assignees, an assignee survives a status change, and "assigned to me"
//! includes the teams a user is in.

use super::tests::{create_group, create_project, GroupSpec};
use super::*;
use crate::services::error_crud_service::ErrorCRUDService;
use crate::services::types::GroupListRefinement;
use sea_orm::EntityTrait;
use temps_entities::error_groups;
use temps_migrations::{MigrationTrait, SchemaManager};
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

async fn email_of(db: &DatabaseConnection, user_id: i32) -> String {
    db.query_one(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "SELECT email FROM users WHERE id = $1",
        [user_id.into()],
    ))
    .await
    .expect("read user")
    .expect("user row")
    .try_get("", "email")
    .expect("email")
}

async fn create_team(db: &DatabaseConnection, owner: i32, members: &[i32]) -> i32 {
    let team: i32 = db
        .query_one(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "INSERT INTO teams (name, slug, created_by, created_at, updated_at) \
             VALUES ('Checkout', $1, $2, NOW(), NOW()) RETURNING id",
            [
                format!("checkout-{}", uuid::Uuid::new_v4()).into(),
                owner.into(),
            ],
        ))
        .await
        .expect("insert team")
        .expect("team row")
        .try_get("", "id")
        .expect("team id");
    for member in members {
        db.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "INSERT INTO team_members (team_id, user_id, role, added_by, created_at, updated_at) \
             VALUES ($1, $2, 'member', $3, NOW(), NOW())",
            [team.into(), (*member).into(), owner.into()],
        ))
        .await
        .expect("insert team member");
    }
    team
}

async fn group(db: &DatabaseConnection, id: i32) -> error_groups::Model {
    error_groups::Entity::find_by_id(id)
        .one(db)
        .await
        .expect("load group")
        .expect("group exists")
}

#[tokio::test]
async fn the_upgrade_keeps_assignees_and_reopens_the_assigned_status() {
    let test_db = test_db!("assignee upgrade");
    let db = test_db.connection_arc();
    let project = create_project(db.as_ref()).await;
    let user = create_user(db.as_ref(), "dana").await;
    let email = email_of(db.as_ref(), user).await;
    let matched = create_group(db.as_ref(), project, GroupSpec::default()).await;
    let unmatched = create_group(db.as_ref(), project, GroupSpec::default()).await;
    // Rows as an older release wrote them: a status of `assigned` and a
    // free-text assignee, typed in another case.
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "UPDATE error_groups SET status = 'assigned', \
           assigned_to = CASE WHEN id = $1 THEN upper($3) ELSE 'someone outside temps' END, \
           assignee_user_id = NULL WHERE id IN ($1, $2)",
        [matched.into(), unmatched.into(), email.clone().into()],
    ))
    .await
    .expect("write legacy rows");

    temps_migrations::ErrorGroupAssigneesMigration
        .up(&SchemaManager::new(db.as_ref()))
        .await
        .expect("re-run the assignee migration");

    let matched = group(db.as_ref(), matched).await;
    assert_eq!(
        matched.status, "unresolved",
        "assigned is now an open group"
    );
    assert_eq!(matched.assignee_user_id, Some(user), "matched by email");
    assert_eq!(
        matched.assigned_to, None,
        "the text is retired once matched"
    );
    let unmatched = group(db.as_ref(), unmatched).await;
    assert_eq!(unmatched.status, "unresolved");
    assert_eq!(unmatched.assignee_user_id, None);
    assert_eq!(
        unmatched.assigned_to.as_deref(),
        Some("someone outside temps"),
        "a value that is no user's is kept, read-only"
    );
}

#[tokio::test]
async fn an_assignee_survives_resolving_and_ignoring_the_group() {
    let test_db = test_db!("assignee and status");
    let db = test_db.connection_arc();
    let project = create_project(db.as_ref()).await;
    let user = create_user(db.as_ref(), "eli").await;
    let id = create_group(db.as_ref(), project, GroupSpec::default()).await;
    let service = GroupWorkflowService::new(db.clone());

    let outcome = service
        .set_assignee(project, id, Some(Assignee::User(user)))
        .await
        .expect("assign");
    assert_eq!(outcome.previous, None);
    assert_eq!(outcome.current.as_ref().map(|a| a.id), Some(user));
    assert_eq!(outcome.recipients, vec![email_of(db.as_ref(), user).await]);

    for action in [BulkGroupAction::Resolve, BulkGroupAction::Ignore] {
        service
            .bulk_update_status(
                project,
                BulkGroupSelector::Ids(vec![id]),
                action,
                None,
                false,
            )
            .await
            .expect("status change");
        assert_eq!(
            group(db.as_ref(), id).await.assignee_user_id,
            Some(user),
            "{action:?} keeps the assignee"
        );
    }

    let again = service
        .set_assignee(project, id, Some(Assignee::User(user)))
        .await
        .expect("assign again");
    assert!(again.recipients.is_empty(), "no change, nobody to tell");

    let cleared = service
        .set_assignee(project, id, None)
        .await
        .expect("unassign");
    assert_eq!(cleared.previous, Some(Assignee::User(user)));
    assert_eq!(group(db.as_ref(), id).await.assignee_user_id, None);
}

#[tokio::test]
async fn assigning_refuses_unknown_assignees_and_other_projects() {
    let test_db = test_db!("assignee validation");
    let db = test_db.connection_arc();
    let project = create_project(db.as_ref()).await;
    let other = create_project(db.as_ref()).await;
    let id = create_group(db.as_ref(), project, GroupSpec::default()).await;
    let service = GroupWorkflowService::new(db.clone());

    for unknown in [Assignee::User(987_654), Assignee::Team(987_654)] {
        let error = service
            .set_assignee(project, id, Some(unknown))
            .await
            .expect_err("unknown assignee");
        assert!(
            matches!(error, ErrorTrackingError::Validation(_)),
            "{error:?}"
        );
    }
    let error = service
        .set_assignee(other, id, None)
        .await
        .expect_err("group of another project");
    assert!(
        matches!(error, ErrorTrackingError::GroupNotInProject { project_id, group_id }
            if project_id == other && group_id == id),
        "{error:?}"
    );
    let error = service
        .user_by_email("nobody@example.test")
        .await
        .expect_err("no such email");
    assert!(error.to_string().contains("nobody@example.test"), "{error}");
}

#[tokio::test]
async fn assigned_to_me_includes_my_teams_and_a_team_assignment_mails_its_members() {
    let test_db = test_db!("assigned to me");
    let db = test_db.connection_arc();
    let project = create_project(db.as_ref()).await;
    let me = create_user(db.as_ref(), "fran").await;
    let teammate = create_user(db.as_ref(), "gil").await;
    let stranger = create_user(db.as_ref(), "hal").await;
    let team = create_team(db.as_ref(), me, &[me, teammate]).await;
    let mine = create_group(db.as_ref(), project, GroupSpec::default()).await;
    let my_team = create_group(db.as_ref(), project, GroupSpec::default()).await;
    let theirs = create_group(db.as_ref(), project, GroupSpec::default()).await;
    let nobody = create_group(db.as_ref(), project, GroupSpec::default()).await;
    let service = GroupWorkflowService::new(db.clone());
    service
        .set_assignee(project, mine, Some(Assignee::User(me)))
        .await
        .expect("assign me");
    let team_outcome = service
        .set_assignee(project, my_team, Some(Assignee::Team(team)))
        .await
        .expect("assign team");
    assert_eq!(team_outcome.recipients.len(), 2, "both members are told");
    assert_eq!(
        team_outcome.current.map(|a| a.kind),
        Some("team".to_string())
    );
    service
        .set_assignee(project, theirs, Some(Assignee::User(stranger)))
        .await
        .expect("assign stranger");

    let crud = ErrorCRUDService::new(db.clone());
    let ids_for = |filter: AssigneeFilter| {
        let crud = &crud;
        async move {
            let (groups, _) = crud
                .list_error_groups(
                    project,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    &GroupListRefinement {
                        assignee: Some(filter),
                        search: None,
                        issue_type: None,
                    },
                )
                .await
                .expect("list");
            let mut ids: Vec<i32> = groups.iter().map(|g| g.id).collect();
            ids.sort_unstable();
            (ids, groups)
        }
    };
    let (ids, groups) = ids_for(AssigneeFilter::Me(me)).await;
    let mut expected = vec![mine, my_team];
    expected.sort_unstable();
    assert_eq!(ids, expected);
    assert!(
        groups.iter().all(|g| g.assignee.is_some()),
        "the list names each assignee"
    );
    assert_eq!(ids_for(AssigneeFilter::Unassigned).await.0, vec![nobody]);
    assert_eq!(ids_for(AssigneeFilter::Team(team)).await.0, vec![my_team]);
}
