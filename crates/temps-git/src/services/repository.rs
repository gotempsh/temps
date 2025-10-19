use std::sync::Arc;
use sea_orm::{prelude::*, QueryFilter, QueryOrder, QuerySelect, JoinType, RelationTrait};
use temps_core::UtcDateTime;
use temps_entities::{repositories, git_provider_connections};
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
    pub framework: Option<String>,
    pub framework_version: Option<String>,
    pub package_manager: Option<String>,
    pub clone_url: Option<String>,
    pub ssh_url: Option<String>,
    pub preset: Option<String>,
    pub git_provider_connection_id: Option<i32>,
}

#[derive(Debug, Clone, Default)]
pub struct RepositoryFilter {
    pub git_provider_connection_id: Option<i32>,
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

        if let Some(search) = &filter.search {
            query = query.filter(
                repositories::Column::Name.contains(search)
                    .or(repositories::Column::FullName.contains(search))
                    .or(repositories::Column::Description.contains(search))
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
            Some("stars_desc") => query = query.order_by_desc(repositories::Column::StargazersCount),
            Some("watchers") => query = query.order_by_asc(repositories::Column::WatchersCount),
            Some("watchers_desc") => query = query.order_by_desc(repositories::Column::WatchersCount),
            Some("size") => query = query.order_by_asc(repositories::Column::Size),
            Some("size_desc") => query = query.order_by_desc(repositories::Column::Size),
            Some("issues") => query = query.order_by_asc(repositories::Column::OpenIssuesCount),
            Some("issues_desc") => query = query.order_by_desc(repositories::Column::OpenIssuesCount),
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

        Ok(repositories.into_iter().map(|repo| RepositoryModel {
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
            framework: repo.framework,
            framework_version: repo.framework_version,
            package_manager: repo.package_manager,
            clone_url: repo.clone_url,
            ssh_url: repo.ssh_url,
            preset: repo.preset,
            git_provider_connection_id: repo.git_provider_connection_id,
        }).collect())
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
            framework: repo.framework,
            framework_version: repo.framework_version,
            package_manager: repo.package_manager,
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
            framework: repo.framework,
            framework_version: repo.framework_version,
            package_manager: repo.package_manager,
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

        Ok(repositories.into_iter().map(|repo| RepositoryModel {
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
            framework: repo.framework,
            framework_version: repo.framework_version,
            package_manager: repo.package_manager,
            clone_url: repo.clone_url,
            ssh_url: repo.ssh_url,
            preset: repo.preset,
            git_provider_connection_id: repo.git_provider_connection_id,
        }).collect())
    }

    /// List all repositories linked to a specific git provider
    pub async fn list_repositories_by_provider(
        &self,
        provider_id: i32,
    ) -> Result<Vec<RepositoryModel>, RepositoryServiceError> {
        let repositories = repositories::Entity::find()
            .join(
                JoinType::InnerJoin,
                repositories::Relation::GitProviderConnection.def()
            )
            .filter(git_provider_connections::Column::ProviderId.eq(provider_id))
            .all(self.db.as_ref())
            .await?;

        Ok(repositories.into_iter().map(|repo| RepositoryModel {
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
            framework: repo.framework,
            framework_version: repo.framework_version,
            package_manager: repo.package_manager,
            clone_url: repo.clone_url,
            ssh_url: repo.ssh_url,
            preset: repo.preset,
            git_provider_connection_id: repo.git_provider_connection_id,
        }).collect())
    }
}