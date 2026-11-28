// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Inbound filter rules against a real database: writes are validated and
//! capped, take effect in the registry before they return, and dropped
//! events are counted per rule and flushed.

use super::*;
use crate::services::types::CreateErrorEventData;
use temps_entities::projects;
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

async fn create_project(db: &DatabaseConnection) -> i32 {
    let now = Utc::now();
    projects::ActiveModel {
        name: Set("Filter Project".to_string()),
        repo_name: Set("repo".to_string()),
        repo_owner: Set("owner".to_string()),
        directory: Set("/".to_string()),
        main_branch: Set("main".to_string()),
        slug: Set(format!("filters-{}", uuid::Uuid::new_v4())),
        preset: Set(temps_entities::preset::Preset::NextJs),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("insert project")
    .id
}

fn noisy(project_id: i32) -> CreateErrorEventData {
    CreateErrorEventData {
        project_id,
        exception_type: Some("Error".into()),
        exception_value: Some("ResizeObserver loop limit exceeded".into()),
        ..Default::default()
    }
}

#[tokio::test]
async fn a_new_rule_drops_matching_events_at_once_and_counts_them() {
    let test_db = test_db!("inbound filter drops");
    let db = test_db.connection_arc();
    let project = create_project(db.as_ref()).await;
    let registry = Arc::new(InboundFilterRegistry::new());
    let service = InboundFilterService::new(db.clone(), registry.clone());

    assert_eq!(registry.evaluate(&noisy(project)), None, "no rule yet");
    let rule = service
        .create(
            project,
            InboundFilterKind::ErrorMessage,
            Some("*ResizeObserver*".into()),
            true,
        )
        .await
        .expect("create the rule");
    assert_eq!(registry.evaluate(&noisy(project)), Some(rule.id));
    assert_eq!(registry.evaluate(&noisy(project)), Some(rule.id));

    let listed = service.list(project).await.expect("list");
    assert_eq!(listed[0].dropped_count, 2, "unflushed drops are shown");
    assert_eq!(registry.flush(db.as_ref()).await.expect("flush"), 2);
    assert_eq!(
        registry.flush(db.as_ref()).await.expect("flush"),
        0,
        "nothing twice"
    );
    let listed = service.list(project).await.expect("list");
    assert_eq!(listed[0].dropped_count, 2);
    assert!(listed[0].last_dropped_at.is_some());

    // Disabling the rule stores the next event again.
    service
        .update(project, rule.id, None, Some(false))
        .await
        .expect("disable");
    assert_eq!(registry.evaluate(&noisy(project)), None);

    service.delete(project, rule.id).await.expect("delete");
    assert!(service.list(project).await.expect("list").is_empty());
}

#[tokio::test]
async fn rules_are_validated_scoped_and_capped() {
    let test_db = test_db!("inbound filter validation");
    let db = test_db.connection_arc();
    let project = create_project(db.as_ref()).await;
    let other = create_project(db.as_ref()).await;
    let service = InboundFilterService::new(db.clone(), Arc::new(InboundFilterRegistry::new()));

    for (kind, pattern) in [
        (InboundFilterKind::Release, None),
        (InboundFilterKind::ErrorMessage, Some("   ".to_string())),
        (InboundFilterKind::Localhost, Some("*".to_string())),
        (
            InboundFilterKind::Release,
            Some("x".repeat(MAX_PATTERN_CHARS + 1)),
        ),
    ] {
        let error = service
            .create(project, kind, pattern.clone(), true)
            .await
            .expect_err("invalid rule");
        assert!(
            matches!(error, ErrorTrackingError::Validation(_)),
            "{kind:?} {pattern:?}: {error}"
        );
    }

    service
        .create(project, InboundFilterKind::WebCrawlers, None, true)
        .await
        .expect("first crawler rule");
    let duplicate = service
        .create(project, InboundFilterKind::WebCrawlers, None, true)
        .await
        .expect_err("a built-in kind exists once per project");
    assert!(matches!(
        duplicate,
        ErrorTrackingError::InboundFilterExists { .. }
    ));

    let rule = service
        .create(
            project,
            InboundFilterKind::Release,
            Some("0.*".into()),
            true,
        )
        .await
        .expect("release rule");
    let not_found = service
        .update(other, rule.id, None, Some(false))
        .await
        .expect_err("another project's rule is not found");
    assert!(matches!(
        not_found,
        ErrorTrackingError::InboundFilterNotFound { project_id, filter_id }
            if project_id == other && filter_id == rule.id
    ));

    for n in 2..MAX_FILTERS_PER_PROJECT {
        service
            .create(
                project,
                InboundFilterKind::Release,
                Some(format!("{n}.*")),
                true,
            )
            .await
            .expect("within the cap");
    }
    let over = service
        .create(
            project,
            InboundFilterKind::Release,
            Some("99.*".into()),
            true,
        )
        .await
        .expect_err("over the cap");
    assert!(matches!(
        over,
        ErrorTrackingError::InboundFilterLimit { .. }
    ));
}

#[tokio::test]
async fn a_reload_from_the_database_picks_up_rules_written_elsewhere() {
    let test_db = test_db!("inbound filter reload");
    let db = test_db.connection_arc();
    let project = create_project(db.as_ref()).await;
    // Another process writes the rule; this one only reloads.
    InboundFilterService::new(db.clone(), Arc::new(InboundFilterRegistry::new()))
        .create(
            project,
            InboundFilterKind::Release,
            Some("1.*".into()),
            true,
        )
        .await
        .expect("create");
    let registry = InboundFilterRegistry::new();
    let event = CreateErrorEventData {
        project_id: project,
        release_version: Some("1.4.0".into()),
        ..Default::default()
    };
    assert_eq!(registry.evaluate(&event), None);
    registry.reload(db.as_ref()).await.expect("reload");
    assert!(registry.evaluate(&event).is_some());
}

#[tokio::test]
async fn a_missing_project_is_project_not_found_for_list_and_create() {
    let test_db = test_db!("inbound filter missing project");
    let db = test_db.connection_arc();
    let service = InboundFilterService::new(db.clone(), Arc::new(InboundFilterRegistry::new()));

    let listed = service.list(999_999).await;
    assert!(
        matches!(
            listed,
            Err(ErrorTrackingError::ProjectNotFound {
                project_id: 999_999
            })
        ),
        "{listed:?}"
    );

    // Before this check the insert hit the project foreign key and the SQL
    // error came back as a 500.
    let created = service
        .create(
            999_999,
            InboundFilterKind::ErrorMessage,
            Some("TypeError: *".into()),
            true,
        )
        .await;
    assert!(
        matches!(
            created,
            Err(ErrorTrackingError::ProjectNotFound {
                project_id: 999_999
            })
        ),
        "{created:?}"
    );
}

/// #2112: concurrent creates cannot overshoot the per-project limit; the
/// check runs under a lock in the inserting transaction.
#[tokio::test]
async fn concurrent_creates_stop_at_the_limit() {
    let test_db = test_db!("inbound filter limit race");
    let db = test_db.connection_arc();
    let project = create_project(db.as_ref()).await;
    let service = Arc::new(InboundFilterService::new(
        db.clone(),
        Arc::new(InboundFilterRegistry::new()),
    ));
    let attempts = MAX_FILTERS_PER_PROJECT + 20;
    let mut tasks = tokio::task::JoinSet::new();
    for n in 0..attempts {
        let service = service.clone();
        tasks.spawn(async move {
            service
                .create(
                    project,
                    InboundFilterKind::Release,
                    Some(format!("rel-{n}")),
                    true,
                )
                .await
        });
    }
    let (mut created, mut refused) = (0u64, 0u64);
    while let Some(result) = tasks.join_next().await {
        match result.expect("task") {
            Ok(_) => created += 1,
            Err(ErrorTrackingError::InboundFilterLimit { .. }) => refused += 1,
            Err(other) => panic!("unexpected error: {other}"),
        }
    }
    assert_eq!(created, MAX_FILTERS_PER_PROJECT);
    assert_eq!(refused, attempts - MAX_FILTERS_PER_PROJECT);
    assert_eq!(
        service.list(project).await.expect("list").len() as u64,
        MAX_FILTERS_PER_PROJECT
    );
}

/// #2112: a pattern rule exists once per kind, ignoring case as matching
/// does, on create and on update; a pattern of only `*` is refused.
#[tokio::test]
async fn pattern_rules_are_unique_ignoring_case_and_match_all_is_refused() {
    let test_db = test_db!("inbound filter duplicate patterns");
    let db = test_db.connection_arc();
    let project = create_project(db.as_ref()).await;
    let other = create_project(db.as_ref()).await;
    let service = InboundFilterService::new(db.clone(), Arc::new(InboundFilterRegistry::new()));

    let first = service
        .create(
            project,
            InboundFilterKind::ErrorMessage,
            Some("TypeError: *".into()),
            true,
        )
        .await
        .expect("first rule");
    for duplicate in ["TypeError: *", "typeerror: *", "  TYPEERROR: *  "] {
        let error = service
            .create(
                project,
                InboundFilterKind::ErrorMessage,
                Some(duplicate.into()),
                true,
            )
            .await
            .expect_err("same rule");
        assert!(
            matches!(
                &error,
                ErrorTrackingError::InboundPatternFilterExists { filter_id, .. }
                    if *filter_id == first.id
            ),
            "{duplicate}: {error}"
        );
    }
    // Another kind, or another project, is a different rule.
    service
        .create(
            project,
            InboundFilterKind::Release,
            Some("typeerror: *".into()),
            true,
        )
        .await
        .expect("another kind");
    service
        .create(
            other,
            InboundFilterKind::ErrorMessage,
            Some("TypeError: *".into()),
            true,
        )
        .await
        .expect("another project");

    let second = service
        .create(
            project,
            InboundFilterKind::ErrorMessage,
            Some("ResizeObserver*".into()),
            true,
        )
        .await
        .expect("second rule");
    let error = service
        .update(project, second.id, Some("TYPEERROR: *".into()), None)
        .await
        .expect_err("an edit cannot duplicate another rule");
    assert!(matches!(
        error,
        ErrorTrackingError::InboundPatternFilterExists { .. }
    ));
    // Re-saving a rule's own pattern in another case is not a duplicate.
    service
        .update(project, first.id, Some("TYPEERROR: *".into()), None)
        .await
        .expect("its own pattern");

    for match_all in ["*", "***", " * "] {
        let error = service
            .create(
                project,
                InboundFilterKind::Release,
                Some(match_all.into()),
                true,
            )
            .await
            .expect_err("matches everything");
        assert!(
            matches!(&error, ErrorTrackingError::Validation(m) if m.contains("matches every event")),
            "{match_all}: {error}"
        );
    }
    assert!(service
        .update(project, second.id, Some("*".into()), None)
        .await
        .is_err());
}
