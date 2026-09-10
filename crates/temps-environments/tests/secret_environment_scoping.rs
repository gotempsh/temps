// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::sync::Arc;

use chrono::Utc;
use sea_orm::{ActiveModelTrait, Set};
use temps_core::EncryptionService;
use temps_database::test_utils::{is_container_runtime_unavailable, TestDatabase};
use temps_entities::{environments, preset::Preset, projects, upstream_config::UpstreamList};
use temps_environments::{SecretError, SecretService};

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
) -> Result<temps_environments::SecretWithEnvironments, SecretError> {
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
            vec![staging.id],
            true,
            vec!["web".to_string()],
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
    let unchanged = service
        .list(project.id, Some(production.id))
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
        .list(owner.id, None)
        .await
        .expect("owner secrets should list")
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
