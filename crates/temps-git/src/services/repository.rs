use sea_orm::{prelude::*, JoinType, QueryFilter, QueryOrder, QuerySelect, RelationTrait};
use std::sync::Arc;
use temps_core::UtcDateTime;
use temps_entities::{git_provider_connections, repositories};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum RepositoryServiceError {
    #[error("Database error: {0}")]
    DatabaseError(#[from] sea_orm::DbErr),

    #[error("Git provider connection not found")]
    ConnectionNotFound,
}

#[derive(Debug, Clone)]
pub struct RepositoryModel {
    pub id: i32,
    pub git_provider_connection_id: i32,
    pub owner: String,
    pub name: String,
    pub full_name: String,
    pub description: Option<String>,
    pub private: bool,
    pub fork: bool,
    pub created_at: UtcDateTime,
    pub updated_at: UtcDateTime,
    pub pushed_at: UtcDateTime,
    pub size: i32,
    pub stargazers_count: i32,
    pub watchers_count: i32,
    pub language: Option<String>,
    pub default_branch: String,
    pub open_issues_count: i32,
    pub topics: String,
    pub clone_url: Option<String>,
    pub ssh_url: Option<String>,
    pub preset: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Default)]
pub struct RepositoryFilter {
    pub git_provider_connection_id: Option<i32>,
    pub provider_id: Option<i32>,
    /// Scopes `provider_id` lookups to a single caller's own
    /// `git_provider_connections` row(s) — `provider_id` identifies a
    /// shared, platform-level OAuth app/PAT config that many users can each
    /// have their own connection to, so a `provider_id`-only filter would
    /// return every user's repositories. Ignored when `provider_id` is
    /// unset (`git_provider_connection_id`-scoped lookups already resolve
    /// to a single connection).
    pub user_id: Option<i32>,
    pub search: Option<String>,
    pub owner: Option<String>,
    pub language: Option<String>,
    pub private: Option<bool>,
    pub sort: Option<String>,
    pub limit: Option<u64>,
    pub offset: Option<u64>,
}

pub struct RepositoryService {
    db: Arc<DatabaseConnection>,
}

impl RepositoryService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    pub async fn list_repositories(
        &self,
        filter: RepositoryFilter,
    ) -> Result<Vec<RepositoryModel>, RepositoryServiceError> {
        let mut query = repositories::Entity::find();

        // Apply filters
        if let Some(connection_id) = filter.git_provider_connection_id {
            query = query.filter(repositories::Column::GitProviderConnectionId.eq(connection_id));
        }

        if let Some(provider_id) = filter.provider_id {
            query = query
                .join(
                    JoinType::InnerJoin,
                    repositories::Relation::GitProviderConnection.def(),
                )
                .filter(git_provider_connections::Column::ProviderId.eq(provider_id));

            if let Some(user_id) = filter.user_id {
                query = query.filter(git_provider_connections::Column::UserId.eq(user_id));
            }
        }

        if let Some(search) = &filter.search {
            query = query.filter(
                repositories::Column::Name
                    .contains(search)
                    .or(repositories::Column::FullName.contains(search))
                    .or(repositories::Column::Description.contains(search)),
            );
        }

        if let Some(owner) = &filter.owner {
            query = query.filter(repositories::Column::Owner.eq(owner));
        }

        if let Some(language) = &filter.language {
            query = query.filter(repositories::Column::Language.eq(language));
        }

        if let Some(private) = filter.private {
            query = query.filter(repositories::Column::Private.eq(private));
        }

        // Apply sorting
        match filter.sort.as_deref() {
            Some("name") => query = query.order_by_asc(repositories::Column::Name),
            Some("name_desc") => query = query.order_by_desc(repositories::Column::Name),
            Some("created") => query = query.order_by_asc(repositories::Column::CreatedAt),
            Some("created_desc") => query = query.order_by_desc(repositories::Column::CreatedAt),
            Some("updated") => query = query.order_by_asc(repositories::Column::UpdatedAt),
            Some("updated_desc") => query = query.order_by_desc(repositories::Column::UpdatedAt),
            Some("pushed") => query = query.order_by_asc(repositories::Column::PushedAt),
            Some("pushed_desc") => query = query.order_by_desc(repositories::Column::PushedAt),
            Some("pushed_at") => query = query.order_by_asc(repositories::Column::PushedAt),
            Some("pushed_at_desc") => query = query.order_by_desc(repositories::Column::PushedAt),
            Some("stars") => query = query.order_by_asc(repositories::Column::StargazersCount),
            Some("stars_desc") => {
                query = query.order_by_desc(repositories::Column::StargazersCount)
            }
            Some("watchers") => query = query.order_by_asc(repositories::Column::WatchersCount),
            Some("watchers_desc") => {
                query = query.order_by_desc(repositories::Column::WatchersCount)
            }
            Some("size") => query = query.order_by_asc(repositories::Column::Size),
            Some("size_desc") => query = query.order_by_desc(repositories::Column::Size),
            Some("issues") => query = query.order_by_asc(repositories::Column::OpenIssuesCount),
            Some("issues_desc") => {
                query = query.order_by_desc(repositories::Column::OpenIssuesCount)
            }
            _ => query = query.order_by_desc(repositories::Column::PushedAt),
        }

        // Apply pagination
        if let Some(limit) = filter.limit {
            query = query.limit(limit);
        }

        if let Some(offset) = filter.offset {
            query = query.offset(offset);
        }

        let repositories = query.all(self.db.as_ref()).await?;

        Ok(repositories
            .into_iter()
            .map(|repo| RepositoryModel {
                id: repo.id,
                owner: repo.owner,
                name: repo.name,
                full_name: repo.full_name,
                description: repo.description,
                private: repo.private,
                fork: repo.fork,
                created_at: repo.created_at,
                updated_at: repo.updated_at,
                pushed_at: repo.pushed_at,
                size: repo.size,
                stargazers_count: repo.stargazers_count,
                watchers_count: repo.watchers_count,
                language: repo.language,
                default_branch: repo.default_branch,
                open_issues_count: repo.open_issues_count,
                topics: repo.topics,
                clone_url: repo.clone_url,
                ssh_url: repo.ssh_url,
                preset: repo.preset,
                git_provider_connection_id: repo.git_provider_connection_id,
            })
            .collect())
    }

    pub async fn verify_git_provider_connection_exists(
        &self,
        connection_id: i32,
    ) -> Result<bool, RepositoryServiceError> {
        let connection = git_provider_connections::Entity::find_by_id(connection_id)
            .one(self.db.as_ref())
            .await?;

        Ok(connection.is_some())
    }

    /// Find a repository by owner and name across all connections
    pub async fn find_by_owner_and_name(
        &self,
        owner: &str,
        name: &str,
    ) -> Result<Option<RepositoryModel>, RepositoryServiceError> {
        let repository = repositories::Entity::find()
            .filter(repositories::Column::Owner.eq(owner))
            .filter(repositories::Column::Name.eq(name))
            .one(self.db.as_ref())
            .await?;

        Ok(repository.map(|repo| RepositoryModel {
            id: repo.id,
            owner: repo.owner,
            name: repo.name,
            full_name: repo.full_name,
            description: repo.description,
            private: repo.private,
            fork: repo.fork,
            created_at: repo.created_at,
            updated_at: repo.updated_at,
            pushed_at: repo.pushed_at,
            size: repo.size,
            stargazers_count: repo.stargazers_count,
            watchers_count: repo.watchers_count,
            language: repo.language,
            default_branch: repo.default_branch,
            open_issues_count: repo.open_issues_count,
            topics: repo.topics,
            clone_url: repo.clone_url,
            ssh_url: repo.ssh_url,
            preset: repo.preset,
            git_provider_connection_id: repo.git_provider_connection_id,
        }))
    }

    /// Find a repository by owner and name within a specific connection
    pub async fn find_by_owner_and_name_in_connection(
        &self,
        owner: &str,
        name: &str,
        connection_id: i32,
    ) -> Result<Option<RepositoryModel>, RepositoryServiceError> {
        let repository = repositories::Entity::find()
            .filter(repositories::Column::Owner.eq(owner))
            .filter(repositories::Column::Name.eq(name))
            .filter(repositories::Column::GitProviderConnectionId.eq(connection_id))
            .one(self.db.as_ref())
            .await?;

        Ok(repository.map(|repo| RepositoryModel {
            id: repo.id,
            owner: repo.owner,
            name: repo.name,
            full_name: repo.full_name,
            description: repo.description,
            private: repo.private,
            fork: repo.fork,
            created_at: repo.created_at,
            updated_at: repo.updated_at,
            pushed_at: repo.pushed_at,
            size: repo.size,
            stargazers_count: repo.stargazers_count,
            watchers_count: repo.watchers_count,
            language: repo.language,
            default_branch: repo.default_branch,
            open_issues_count: repo.open_issues_count,
            topics: repo.topics,
            clone_url: repo.clone_url,
            ssh_url: repo.ssh_url,
            preset: repo.preset,
            git_provider_connection_id: repo.git_provider_connection_id,
        }))
    }

    /// Find repositories by owner and name pattern across multiple connections
    /// Returns all matching repositories with their connection information
    pub async fn find_all_by_owner_and_name(
        &self,
        owner: &str,
        name: &str,
    ) -> Result<Vec<RepositoryModel>, RepositoryServiceError> {
        let repositories = repositories::Entity::find()
            .filter(repositories::Column::Owner.eq(owner))
            .filter(repositories::Column::Name.eq(name))
            .all(self.db.as_ref())
            .await?;

        Ok(repositories
            .into_iter()
            .map(|repo| RepositoryModel {
                id: repo.id,
                owner: repo.owner,
                name: repo.name,
                full_name: repo.full_name,
                description: repo.description,
                private: repo.private,
                fork: repo.fork,
                created_at: repo.created_at,
                updated_at: repo.updated_at,
                pushed_at: repo.pushed_at,
                size: repo.size,
                stargazers_count: repo.stargazers_count,
                watchers_count: repo.watchers_count,
                language: repo.language,
                default_branch: repo.default_branch,
                open_issues_count: repo.open_issues_count,
                topics: repo.topics,
                clone_url: repo.clone_url,
                ssh_url: repo.ssh_url,
                preset: repo.preset,
                git_provider_connection_id: repo.git_provider_connection_id,
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use sea_orm::{ActiveModelTrait, Set};
    use temps_database::test_utils::TestDatabase;
    use temps_entities::{git_providers, repositories, users};

    /// `provider_id` is a shared, platform-level resource — many users can
    /// each hold their own `git_provider_connections` row against the same
    /// provider. Regression test for the IDOR this method used to have:
    /// querying by `provider_id` alone returned every user's repositories,
    /// not just the caller's.
    #[tokio::test]
    async fn list_repositories_by_provider_is_scoped_to_caller() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.connection_arc();
        let now = Utc::now();

        let make_user = |email: &str| users::ActiveModel {
            email: Set(email.to_string()),
            password_hash: Set(Some("hash".to_string())),
            name: Set("Test User".to_string()),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        };
        let user_a = make_user("a@example.com")
            .insert(db.as_ref())
            .await
            .unwrap();
        let user_b = make_user("b@example.com")
            .insert(db.as_ref())
            .await
            .unwrap();

        let provider = git_providers::ActiveModel {
            name: Set("shared-provider".to_string()),
            provider_type: Set("github".to_string()),
            base_url: Set(None),
            api_url: Set(None),
            auth_method: Set("oauth".to_string()),
            auth_config: Set(serde_json::json!({})),
            webhook_secret: Set(None),
            is_active: Set(true),
            is_default: Set(false),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();

        let make_connection = |user_id: i32, account: &str| git_provider_connections::ActiveModel {
            provider_id: Set(provider.id),
            user_id: Set(Some(user_id)),
            account_name: Set(account.to_string()),
            account_type: Set("User".to_string()),
            access_token: Set(None),
            refresh_token: Set(None),
            token_expires_at: Set(None),
            refresh_token_expires_at: Set(None),
            installation_id: Set(None),
            metadata: Set(None),
            is_active: Set(true),
            is_expired: Set(false),
            syncing: Set(false),
            last_synced_at: Set(None),
            ..Default::default()
        };
        let connection_a = make_connection(user_a.id, "user-a-account")
            .insert(db.as_ref())
            .await
            .unwrap();
        let connection_b = make_connection(user_b.id, "user-b-account")
            .insert(db.as_ref())
            .await
            .unwrap();

        let make_repo = |connection_id: i32, name: &str| repositories::ActiveModel {
            git_provider_connection_id: Set(connection_id),
            owner: Set("owner".to_string()),
            name: Set(name.to_string()),
            full_name: Set(format!("owner/{name}")),
            description: Set(None),
            private: Set(true),
            fork: Set(false),
            created_at: Set(now),
            updated_at: Set(now),
            pushed_at: Set(now),
            size: Set(0),
            stargazers_count: Set(0),
            watchers_count: Set(0),
            language: Set(None),
            default_branch: Set("main".to_string()),
            open_issues_count: Set(0),
            topics: Set("[]".to_string()),
            repo_object: Set("{}".to_string()),
            installation_id: Set(None),
            clone_url: Set(None),
            ssh_url: Set(Some(format!("git@example.com:owner/{name}.git"))),
            preset: Set(None),
            ..Default::default()
        };
        make_repo(connection_a.id, "user-a-private-repo")
            .insert(db.as_ref())
            .await
            .unwrap();
        make_repo(connection_b.id, "user-b-private-repo")
            .insert(db.as_ref())
            .await
            .unwrap();

        let service = RepositoryService::new(db.clone());

        let filter_for = |user_id: i32| RepositoryFilter {
            provider_id: Some(provider.id),
            user_id: Some(user_id),
            ..Default::default()
        };

        let repos_for_a = service
            .list_repositories(filter_for(user_a.id))
            .await
            .unwrap();
        assert_eq!(
            repos_for_a.len(),
            1,
            "caller must only see their own repositories for this provider"
        );
        assert_eq!(repos_for_a[0].name, "user-a-private-repo");

        let repos_for_b = service
            .list_repositories(filter_for(user_b.id))
            .await
            .unwrap();
        assert_eq!(repos_for_b.len(), 1);
        assert_eq!(repos_for_b[0].name, "user-b-private-repo");
    }
}
