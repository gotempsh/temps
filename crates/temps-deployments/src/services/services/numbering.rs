// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Deployment numbering: the `<n>` in a deployment's `<project>-<n>` slug.

use super::*;

impl DeploymentService {
    /// Allocate a unique slug, `<prefix>-<n>`, for a new deployment of
    /// `project_id`; `prefix` is normally the project slug.
    ///
    /// The slug is also the deployment's container name and hostname, so it
    /// must be unique even when several deployments of the project start at
    /// the same instant: the number comes from the project's atomic counter,
    /// never from counting its deployments.
    pub async fn allocate_deployment_slug(
        &self,
        project_id: i32,
        prefix: &str,
    ) -> Result<String, DeploymentError> {
        temps_entities::project_deployment_counters::allocate_deployment_slug(
            self.db.as_ref(),
            project_id,
            prefix,
        )
        .await
        .map_err(|source| DeploymentError::DeploymentNumberAllocation { project_id, source })
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{create_deployment_service_for_test, setup_test_data};
    use super::*;
    use sea_orm::ActiveModelTrait;
    use std::collections::BTreeSet;
    use temps_database::test_utils::TestDatabase;

    /// Deployments of one project started at the same instant (one per
    /// environment, say) must get distinct slugs: the slug is the container
    /// name, and two deployments sharing one replace each other's container.
    /// Counting the project's deployments, as numbering used to, hands every
    /// concurrent caller the same number.
    #[tokio::test]
    async fn concurrent_allocations_for_one_project_get_distinct_slugs() {
        let test_db = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(error)
                if temps_database::test_utils::is_container_runtime_unavailable(
                    &error.to_string(),
                ) =>
            {
                println!("Docker unavailable; skipping: {error}");
                return;
            }
            Err(error) => panic!("test database: {error}"),
        };
        let db = test_db.connection_arc();
        // The fixture already holds one deployment, slugged `...-123`.
        let (project, environment, _) = setup_test_data(&db).await.expect("seed project");
        deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set(format!("{}-7", project.slug)),
            state: Set("completed".to_string()),
            metadata: Set(Some(deployments::DeploymentMetadata::default())),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("seed a numbered deployment");

        let service = Arc::new(create_deployment_service_for_test(db.clone()));
        const CONCURRENT: usize = 16;
        let barrier = Arc::new(tokio::sync::Barrier::new(CONCURRENT));
        let tasks: Vec<_> = (0..CONCURRENT)
            .map(|_| {
                let service = service.clone();
                let barrier = barrier.clone();
                let prefix = project.slug.clone();
                tokio::spawn(async move {
                    barrier.wait().await;
                    service.allocate_deployment_slug(project.id, &prefix).await
                })
            })
            .collect();
        let mut slugs = Vec::with_capacity(CONCURRENT);
        for task in tasks {
            slugs.push(task.await.expect("join").expect("allocate slug"));
        }

        let distinct: BTreeSet<&String> = slugs.iter().collect();
        assert_eq!(distinct.len(), CONCURRENT, "duplicate slugs: {slugs:?}");

        // Numbering continues above every number already in a slug (123),
        // so no existing slug is handed out again, and has no gaps.
        let numbers: BTreeSet<i64> = slugs
            .iter()
            .map(|slug| {
                slug.strip_prefix(&format!("{}-", project.slug))
                    .and_then(|n| n.parse().ok())
                    .unwrap_or_else(|| panic!("unexpected slug {slug}"))
            })
            .collect();
        let expected: BTreeSet<i64> = (124..124 + CONCURRENT as i64).collect();
        assert_eq!(numbers, expected);

        // Another project's numbering is independent and starts at 1.
        let other = temps_entities::projects::ActiveModel {
            name: Set("Other Project".to_string()),
            slug: Set("other-project".to_string()),
            repo_owner: Set("owner".to_string()),
            repo_name: Set("repo".to_string()),
            main_branch: Set("main".to_string()),
            preset: Set(temps_entities::preset::Preset::NextJs),
            directory: Set("/".to_string()),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            is_deleted: Set(false),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("seed a second project");
        assert_eq!(
            service
                .allocate_deployment_slug(other.id, "other-project")
                .await
                .expect("allocate for the second project"),
            "other-project-1"
        );
    }

    #[tokio::test]
    async fn allocation_for_a_missing_project_names_it_in_the_error() {
        let test_db = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(error)
                if temps_database::test_utils::is_container_runtime_unavailable(
                    &error.to_string(),
                ) =>
            {
                println!("Docker unavailable; skipping: {error}");
                return;
            }
            Err(error) => panic!("test database: {error}"),
        };
        let service = create_deployment_service_for_test(test_db.connection_arc());

        let error = service
            .allocate_deployment_slug(987_654, "ghost")
            .await
            .expect_err("a missing project has no counter to bump");
        assert!(
            matches!(
                error,
                DeploymentError::DeploymentNumberAllocation {
                    project_id: 987_654,
                    ..
                }
            ),
            "{error:?}"
        );
        assert!(error.to_string().contains("987654"), "{error}");
    }
}
