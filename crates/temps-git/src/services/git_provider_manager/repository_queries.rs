// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Repository lookups and read-only provider calls on a connected repository.

use super::*;

impl GitProviderManager {
    /// Get a repository API instance for convenient repository operations
    /// Returns a trait that provides repository-specific operations (get_commit_info, get_branches, etc.)
    /// without needing to pass owner/repo/connection repeatedly
    pub async fn get_repository_api(
        &self,
        connection_id: i32,
        owner: &str,
        repo: &str,
    ) -> Result<
        Arc<dyn crate::services::git_repository_api::GitRepositoryApi>,
        GitProviderManagerError,
    > {
        // Get the connection
        let connection = self.get_connection(connection_id).await?;

        // Get the provider service
        let provider_service = self.get_provider_service(connection.provider_id).await?;

        // Get access token (with automatic refresh if needed)
        let access_token = self
            .validate_and_refresh_connection_token(connection_id)
            .await?;

        // Create repository API instance
        let repo_api = crate::services::git_repository_api::GitRepositoryApiImpl::new(
            owner.to_string(),
            repo.to_string(),
            provider_service,
            access_token,
        );

        Ok(Arc::new(repo_api))
    }

    /// Fetch commit information from Git provider using connection_id
    /// Returns commit details (sha, message, author, date)
    /// @deprecated Use get_repository_api() instead for better encapsulation
    pub async fn get_commit_info(
        &self,
        connection_id: i32,
        owner: &str,
        repo: &str,
        commit_sha: &str,
    ) -> Result<crate::services::git_provider::Commit, GitProviderManagerError> {
        // Get the connection
        let connection = self.get_connection(connection_id).await?;

        // Get the provider service
        let provider_service = self.get_provider_service(connection.provider_id).await?;

        // Get access token (with automatic refresh if needed)
        let access_token = self
            .validate_and_refresh_connection_token(connection_id)
            .await?;

        // Fetch commit from provider - GitProviderService trait has get_commit method
        let commit = provider_service
            .get_commit(&access_token, owner, repo, commit_sha)
            .await?;

        Ok(commit)
    }

    /// Get the current commit info for a branch
    /// Returns commit details (sha, message, author, date) for the latest commit on the branch
    pub async fn get_branch_latest_commit(
        &self,
        connection_id: i32,
        owner: &str,
        repo: &str,
        branch: &str,
    ) -> Result<crate::services::git_provider::Commit, GitProviderManagerError> {
        // Get the connection
        let connection = self.get_connection(connection_id).await?;

        // Get the provider service
        let provider_service = self.get_provider_service(connection.provider_id).await?;

        // Get access token (with automatic refresh if needed)
        let access_token = self
            .validate_and_refresh_connection_token(connection_id)
            .await?;

        // Fetch latest commit from the branch
        let commit = provider_service
            .get_latest_commit(&access_token, owner, repo, branch)
            .await?;

        Ok(commit)
    }

    pub async fn get_repository_by_id(
        &self,
        id: i32,
    ) -> Result<repositories::Model, GitProviderManagerError> {
        let repository = repositories::Entity::find_by_id(id)
            .one(self.db.as_ref())
            .await?;
        repository.ok_or_else(|| {
            GitProviderManagerError::RepositoryNotFound(format!("Repository {} not found", id))
        })
    }

    /// Resolve a repository only when its provider connection belongs to the
    /// requesting user. Returning `RepositoryNotFound` for both missing and
    /// foreign repositories avoids turning numeric repository IDs into a
    /// cross-tenant existence oracle.
    /// Look up a repository for the "live" fetch endpoints (preset,
    /// env-example, compose services/preview).
    ///
    /// Repositories are reached through their connection, which is shared
    /// across all authenticated users (single-tenant: "the team is the app"
    /// for now, see #656), so this is intentionally not scoped to a caller's
    /// user_id -- any user with GitRepositoriesRead may fetch live data for
    /// any repository behind any connection.
    pub(super) async fn get_repository_for_user(
        &self,
        repository_id: i32,
    ) -> Result<repositories::Model, GitProviderManagerError> {
        repositories::Entity::find_by_id(repository_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                GitProviderManagerError::RepositoryNotFound(format!(
                    "Repository {} not found",
                    repository_id
                ))
            })
    }

    pub async fn get_repository_by_owner_and_name_in_connection(
        &self,
        owner: &str,
        name: &str,
        connection_id: i32,
    ) -> Result<repositories::Model, GitProviderManagerError> {
        let repository = repositories::Entity::find()
            .filter(repositories::Column::Owner.eq(owner))
            .filter(repositories::Column::Name.eq(name))
            .filter(repositories::Column::GitProviderConnectionId.eq(connection_id))
            .one(self.db.as_ref())
            .await?;

        repository.ok_or_else(|| {
            GitProviderManagerError::RepositoryNotFound(format!(
                "Repository {}/{} not found in connection {}",
                owner, name, connection_id
            ))
        })
    }

    /// List repositories for a connection with pagination and filtering
    pub async fn list_repositories_by_connection(
        &self,
        connection_id: i32,
        params: crate::services::git_provider::RepositoryListParams,
    ) -> Result<Vec<crate::services::git_provider::Repository>, GitProviderManagerError> {
        // Get the connection details
        let connection = self.get_connection(connection_id).await?;

        // Get the provider service instance
        let provider_service = self.get_provider_service(connection.provider_id).await?;

        // Execute the API call with automatic token refresh
        let organization = params.organization.clone();
        let repositories = self
            .execute_with_refresh(connection_id, |access_token| {
                let org = organization.clone();
                let svc = provider_service.clone();
                async move { svc.list_repositories(&access_token, org.as_deref()).await }
            })
            .await?;

        // Apply client-side filtering for search term if provided
        let filtered_repositories = if let Some(search_term) = params.search_term {
            repositories
                .into_iter()
                .filter(|repo| {
                    repo.name
                        .to_lowercase()
                        .contains(&search_term.to_lowercase())
                        || repo
                            .full_name
                            .to_lowercase()
                            .contains(&search_term.to_lowercase())
                        || repo.description.as_ref().is_some_and(|desc| {
                            desc.to_lowercase().contains(&search_term.to_lowercase())
                        })
                })
                .collect()
        } else {
            repositories
        };

        // Apply client-side pagination if provided
        let paginated_repositories =
            if let (Some(page), Some(per_page)) = (params.page, params.per_page) {
                let start = ((page.saturating_sub(1)) * per_page) as usize;
                let end = (start + per_page as usize).min(filtered_repositories.len());

                if start < filtered_repositories.len() {
                    filtered_repositories[start..end].to_vec()
                } else {
                    Vec::new()
                }
            } else {
                filtered_repositories
            };

        Ok(paginated_repositories)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::git_provider_manager::test_support::*;

    fn mock_manager_with_repository(connection_owner_user_id: Option<i32>) -> GitProviderManager {
        use sea_orm::{DatabaseBackend, MockDatabase};

        let connection = connection_fixture(11, connection_owner_user_id);
        let repository = repository_fixture(connection.id);
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([[repository]])
                .into_connection(),
        );
        GitProviderManager::new(
            db.clone(),
            Arc::new(
                temps_core::EncryptionService::new(
                    "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                )
                .expect("test encryption key should be valid"),
            ),
            Arc::new(MockJobQueue) as Arc<dyn JobQueue>,
            create_test_config_service(db),
        )
    }

    // Connections (and the repositories behind them) are shared across all
    // authenticated users (single-tenant: "the team is the app" for now, see
    // #656) -- any user should resolve a repository regardless of which
    // teammate's connection it's attached to, or whether that connection has
    // an owner recorded at all.
    #[tokio::test]
    async fn connected_repository_lookup_is_shared_across_users() {
        let manager = mock_manager_with_repository(Some(5));

        let repository = manager
            .get_repository_for_user(42)
            .await
            .expect("any authenticated user should resolve a shared repository");

        assert_eq!(repository.id, 42);
    }

    #[tokio::test]
    async fn connected_repository_lookup_allows_ownerless_connections() {
        let manager = mock_manager_with_repository(None);

        let repository = manager
            .get_repository_for_user(42)
            .await
            .expect("an ownerless connection's repository should still resolve");

        assert_eq!(repository.id, 42);
    }
}
