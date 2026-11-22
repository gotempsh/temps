// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Which repository an agent's `config_repo` may name, for one project: its own
//! repository, or one on the administrator's allow-list
//! ([`temps_core::config_repo`] holds the rule itself).

use sea_orm::{DatabaseConnection, DbErr, EntityTrait};
use temps_core::config_repo::{check_config_repo, ConfigRepoRefusal, RepoRef};
use temps_entities::{projects, settings};
use thiserror::Error;

/// Why a project's config repository could not be approved.
#[derive(Error, Debug)]
pub enum ConfigRepoCheckError {
    #[error("project {project_id}: {refusal}")]
    Refused {
        project_id: i32,
        refusal: ConfigRepoRefusal,
    },

    #[error("project {project_id} was not found while checking its config repository")]
    ProjectNotFound { project_id: i32 },

    #[error("could not read project {project_id} or the settings to check its config repository: {source}")]
    Database {
        project_id: i32,
        #[source]
        source: DbErr,
    },
}

/// What decides which config repositories one project may use: its own
/// repository and the administrator's allow-list. Loaded once, then checked
/// against as many candidates as needed without further reads.
#[derive(Debug, Clone)]
pub struct ProjectConfigRepoPolicy {
    project_id: i32,
    repo_owner: String,
    repo_name: String,
    allowlist: Vec<String>,
}

impl ProjectConfigRepoPolicy {
    /// Read the project and the allow-list (two queries).
    pub async fn load(
        db: &DatabaseConnection,
        project_id: i32,
    ) -> Result<Self, ConfigRepoCheckError> {
        let database = |source| ConfigRepoCheckError::Database { project_id, source };
        let project = projects::Entity::find_by_id(project_id)
            .one(db)
            .await
            .map_err(database)?
            .ok_or(ConfigRepoCheckError::ProjectNotFound { project_id })?;
        let allowlist = settings::Entity::find_by_id(1)
            .one(db)
            .await
            .map_err(database)?
            .and_then(|row| row.data.get("ai_config").cloned())
            .and_then(|value| serde_json::from_value::<temps_config::AiConfigSettings>(value).ok())
            .map(|ai_config| ai_config.config_repo_allowlist)
            .unwrap_or_default();
        Ok(Self {
            project_id,
            repo_owner: project.repo_owner,
            repo_name: project.repo_name,
            allowlist,
        })
    }

    /// Approve `candidate`, returning the repository it names in canonical
    /// `owner/repo` form -- which is what must be cloned, since `candidate`
    /// may also be a `https://github.com/owner/repo(.git)` URL.
    pub fn check(&self, candidate: &str) -> Result<RepoRef, ConfigRepoCheckError> {
        check_config_repo(
            candidate,
            &self.repo_owner,
            &self.repo_name,
            &self.allowlist,
        )
        .map_err(|refusal| ConfigRepoCheckError::Refused {
            project_id: self.project_id,
            refusal,
        })
    }
}

/// Approve `candidate` as a config repository for `project_id`, or say why not.
pub async fn check_project_config_repo(
    db: &DatabaseConnection,
    project_id: i32,
    candidate: &str,
) -> Result<RepoRef, ConfigRepoCheckError> {
    ProjectConfigRepoPolicy::load(db, project_id)
        .await?
        .check(candidate)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};

    fn project(owner: &str, repo: &str) -> projects::Model {
        let mut model = crate::services::executor::tests::make_project(1);
        model.repo_owner = owner.to_string();
        model.repo_name = repo.to_string();
        model
    }

    fn settings_row(allowlist: &[&str]) -> settings::Model {
        settings::Model {
            id: 1,
            data: serde_json::json!({
                "ai_config": { "config_repo": "", "config_repo_branch": "main",
                               "config_repo_allowlist": allowlist }
            }),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    async fn check(allowlist: &[&str], candidate: &str) -> Result<RepoRef, ConfigRepoCheckError> {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![project("acme", "shop")]])
            .append_query_results([vec![settings_row(allowlist)]])
            .into_connection();
        check_project_config_repo(&db, 1, candidate).await
    }

    #[tokio::test]
    async fn the_projects_own_repository_is_approved() {
        assert!(check(&[], "acme/shop").await.is_ok());
    }

    #[tokio::test]
    async fn another_repository_is_refused_naming_the_project() {
        match check(&[], "other/private-config").await {
            Err(ConfigRepoCheckError::Refused {
                project_id,
                refusal,
            }) => {
                assert_eq!(project_id, 1);
                assert!(refusal.to_string().contains("other/private-config"));
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_listed_repository_is_approved() {
        assert!(check(&["other/shared-config"], "other/shared-config")
            .await
            .is_ok());
        assert!(check(&["other/*"], "Other/Anything").await.is_ok());
    }

    /// The policy accepts a GitHub URL for a repository; the clone needs the
    /// `owner/repo` it names, not the URL, or it splits `https:` off as the
    /// owner.
    #[tokio::test]
    async fn an_approved_url_is_returned_as_owner_and_repo() {
        let approved = check(&[], "https://github.com/Acme/Shop.git")
            .await
            .unwrap();
        assert_eq!(approved.to_string(), "acme/shop");
    }

    /// One load serves every candidate: checking several agents' repositories
    /// does not read the project and the settings again for each.
    #[tokio::test]
    async fn one_loaded_policy_checks_many_candidates() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![project("acme", "shop")]])
            .append_query_results([vec![settings_row(&["other/shared"])]])
            .into_connection();
        let policy = ProjectConfigRepoPolicy::load(&db, 1).await.unwrap();
        assert!(policy.check("acme/shop").is_ok());
        assert!(policy.check("other/shared").is_ok());
        assert!(policy.check("other/private").is_err());
        assert_eq!(db.into_transaction_log().len(), 2);
    }

    #[tokio::test]
    async fn a_missing_project_is_reported_with_its_id() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<projects::Model>::new()])
            .into_connection();
        assert!(matches!(
            check_project_config_repo(&db, 77, "acme/shop").await,
            Err(ConfigRepoCheckError::ProjectNotFound { project_id: 77 })
        ));
    }
}
