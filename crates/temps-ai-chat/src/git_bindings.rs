// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Durable, credential-free Git repository bindings for application workspaces.

use std::sync::Arc;

use chrono::Utc;
use sea_orm::{
    ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder,
};
use temps_entities::{
    ai_application_git_bindings, ai_application_projects, ai_applications,
    git_provider_connections, git_providers, repositories,
};

#[derive(Debug, thiserror::Error)]
pub enum GitBindingError {
    #[error("application '{application_id}' was not found for user {user_id}")]
    ApplicationNotFound {
        application_id: String,
        user_id: i32,
    },
    #[error("project {project_id} is not linked to application '{application_id}'")]
    ProjectNotLinked {
        application_id: String,
        project_id: i32,
    },
    #[error("Git connection {connection_id} is not an active, non-expired connection owned by user {user_id}")]
    ConnectionUnavailable { connection_id: i32, user_id: i32 },
    #[error("repository {repository_id} does not belong to Git connection {connection_id}")]
    RepositoryUnavailable {
        repository_id: i32,
        connection_id: i32,
    },
    #[error("repository {repository_id} on Git connection {connection_id} has no safe canonical HTTPS clone URL")]
    RepositoryUrlUnavailable {
        repository_id: i32,
        connection_id: i32,
    },
    #[error(
        "remote name '{remote_name}' is invalid; start with an ASCII letter or number and use only letters, numbers, '_' or '-' (maximum 64 characters)"
    )]
    InvalidRemoteName { remote_name: String },
    #[error("remote '{remote_name}' is already bound for project {project_id} in application '{application_id}'")]
    RemoteAlreadyBound {
        application_id: String,
        project_id: i32,
        remote_name: String,
    },
    #[error("Git binding {binding_id} was not found in application '{application_id}'")]
    BindingNotFound {
        application_id: String,
        binding_id: i64,
    },
    #[error("failed to {operation} Git bindings for application '{application_id}': {source}")]
    Database {
        operation: &'static str,
        application_id: String,
        #[source]
        source: sea_orm::DbErr,
    },
}

#[derive(Debug, Clone)]
pub struct EligibleGitRepository {
    pub repository: repositories::Model,
    pub connection: git_provider_connections::Model,
    pub repository_url: String,
}

#[derive(Clone)]
pub struct GitBindingService {
    db: Arc<DatabaseConnection>,
}

impl GitBindingService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    async fn application(
        &self,
        user_id: i32,
        public_id: &str,
    ) -> Result<ai_applications::Model, GitBindingError> {
        ai_applications::Entity::find()
            .filter(ai_applications::Column::PublicId.eq(public_id))
            .filter(ai_applications::Column::CreatedBy.eq(user_id))
            .filter(ai_applications::Column::Status.eq("active"))
            .one(self.db.as_ref())
            .await
            .map_err(|source| GitBindingError::Database {
                operation: "load the owned application",
                application_id: public_id.to_string(),
                source,
            })?
            .ok_or_else(|| GitBindingError::ApplicationNotFound {
                application_id: public_id.to_string(),
                user_id,
            })
    }

    pub async fn list(
        &self,
        user_id: i32,
        application_id: &str,
    ) -> Result<Vec<ai_application_git_bindings::Model>, GitBindingError> {
        let application = self.application(user_id, application_id).await?;
        ai_application_git_bindings::Entity::find()
            .filter(ai_application_git_bindings::Column::ApplicationId.eq(application.id))
            .order_by_asc(ai_application_git_bindings::Column::ProjectId)
            .order_by_asc(ai_application_git_bindings::Column::RemoteName)
            .all(self.db.as_ref())
            .await
            .map_err(|source| GitBindingError::Database {
                operation: "list",
                application_id: application_id.to_string(),
                source,
            })
    }

    /// Repositories the caller can safely choose from. This is derived only from
    /// synchronized repository rows attached to currently usable connections.
    pub async fn eligible_repositories(
        &self,
        user_id: i32,
        application_id: &str,
    ) -> Result<Vec<EligibleGitRepository>, GitBindingError> {
        self.application(user_id, application_id).await?;
        let connections = git_provider_connections::Entity::find()
            .filter(git_provider_connections::Column::UserId.eq(user_id))
            .filter(git_provider_connections::Column::IsActive.eq(true))
            .filter(git_provider_connections::Column::IsExpired.eq(false))
            .all(self.db.as_ref())
            .await
            .map_err(|source| GitBindingError::Database {
                operation: "list eligible connections",
                application_id: application_id.to_string(),
                source,
            })?;
        let connections = connections
            .into_iter()
            .filter(connection_is_usable)
            .collect::<Vec<_>>();
        if connections.is_empty() {
            return Ok(Vec::new());
        }
        let ids = connections
            .iter()
            .map(|connection| connection.id)
            .collect::<Vec<_>>();
        let provider_ids = connections
            .iter()
            .map(|connection| connection.provider_id)
            .collect::<Vec<_>>();
        let providers = git_providers::Entity::find()
            .filter(git_providers::Column::Id.is_in(provider_ids))
            .filter(git_providers::Column::IsActive.eq(true))
            .all(self.db.as_ref())
            .await
            .map_err(|source| GitBindingError::Database {
                operation: "list active Git providers",
                application_id: application_id.to_string(),
                source,
            })?;
        let repos = repositories::Entity::find()
            .filter(repositories::Column::GitProviderConnectionId.is_in(ids))
            .order_by_asc(repositories::Column::FullName)
            .all(self.db.as_ref())
            .await
            .map_err(|source| GitBindingError::Database {
                operation: "list eligible repositories",
                application_id: application_id.to_string(),
                source,
            })?;
        let mut result = Vec::new();
        for repository in repos {
            if let Some(connection) = connections
                .iter()
                .find(|connection| connection.id == repository.git_provider_connection_id)
            {
                let Some(provider) = providers
                    .iter()
                    .find(|provider| provider.id == connection.provider_id)
                else {
                    continue;
                };
                let Some(repository_url) = repository
                    .clone_url
                    .as_deref()
                    .and_then(|value| canonical_https_url(value, provider))
                else {
                    continue;
                };
                result.push(EligibleGitRepository {
                    repository,
                    connection: connection.clone(),
                    repository_url,
                });
            }
        }
        Ok(result)
    }

    pub async fn bind(
        &self,
        user_id: i32,
        application_id: &str,
        project_id: i32,
        connection_id: i32,
        repository_id: i32,
        remote_name: &str,
    ) -> Result<ai_application_git_bindings::Model, GitBindingError> {
        validate_remote_name(remote_name)?;
        let application = self.application(user_id, application_id).await?;
        let linked = ai_application_projects::Entity::find()
            .filter(ai_application_projects::Column::ApplicationId.eq(application.id))
            .filter(ai_application_projects::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await
            .map_err(|source| GitBindingError::Database {
                operation: "validate the linked project",
                application_id: application_id.to_string(),
                source,
            })?;
        if linked.is_none() {
            return Err(GitBindingError::ProjectNotLinked {
                application_id: application_id.to_string(),
                project_id,
            });
        }
        let connection = git_provider_connections::Entity::find_by_id(connection_id)
            .filter(git_provider_connections::Column::UserId.eq(user_id))
            .filter(git_provider_connections::Column::IsActive.eq(true))
            .filter(git_provider_connections::Column::IsExpired.eq(false))
            .one(self.db.as_ref())
            .await
            .map_err(|source| GitBindingError::Database {
                operation: "validate the owned Git connection",
                application_id: application_id.to_string(),
                source,
            })?
            .filter(connection_is_usable)
            .ok_or(GitBindingError::ConnectionUnavailable {
                connection_id,
                user_id,
            })?;
        let provider = git_providers::Entity::find_by_id(connection.provider_id)
            .filter(git_providers::Column::IsActive.eq(true))
            .one(self.db.as_ref())
            .await
            .map_err(|source| GitBindingError::Database {
                operation: "validate the active Git provider",
                application_id: application_id.to_string(),
                source,
            })?
            .ok_or(GitBindingError::ConnectionUnavailable {
                connection_id,
                user_id,
            })?;
        let repository = repositories::Entity::find_by_id(repository_id)
            .filter(repositories::Column::GitProviderConnectionId.eq(connection.id))
            .one(self.db.as_ref())
            .await
            .map_err(|source| GitBindingError::Database {
                operation: "validate the synchronized repository",
                application_id: application_id.to_string(),
                source,
            })?
            .ok_or(GitBindingError::RepositoryUnavailable {
                repository_id,
                connection_id,
            })?;
        let repository_url = repository
            .clone_url
            .as_deref()
            .and_then(|value| canonical_https_url(value, &provider))
            .ok_or(GitBindingError::RepositoryUrlUnavailable {
                repository_id,
                connection_id,
            })?;
        let existing = ai_application_git_bindings::Entity::find()
            .filter(ai_application_git_bindings::Column::ApplicationId.eq(application.id))
            .filter(ai_application_git_bindings::Column::ProjectId.eq(project_id))
            .filter(ai_application_git_bindings::Column::RemoteName.eq(remote_name))
            .one(self.db.as_ref())
            .await
            .map_err(|source| GitBindingError::Database {
                operation: "check the remote name",
                application_id: application_id.to_string(),
                source,
            })?;
        if existing.is_some() {
            return Err(GitBindingError::RemoteAlreadyBound {
                application_id: application_id.to_string(),
                project_id,
                remote_name: remote_name.to_string(),
            });
        }
        let now = Utc::now();
        let inserted =
            ai_application_git_bindings::Entity::insert(ai_application_git_bindings::ActiveModel {
                application_id: Set(application.id),
                project_id: Set(project_id),
                connection_id: Set(connection_id),
                repository_id: Set(repository_id),
                repository_url: Set(repository_url.clone()),
                remote_name: Set(remote_name.to_string()),
                created_at: Set(now),
                updated_at: Set(now),
                ..Default::default()
            })
            .exec(self.db.as_ref())
            .await
            .map_err(|source| GitBindingError::Database {
                operation: "create",
                application_id: application_id.to_string(),
                source,
            })?;
        Ok(ai_application_git_bindings::Model {
            id: inserted.last_insert_id,
            application_id: application.id,
            project_id,
            connection_id,
            repository_id,
            repository_url,
            remote_name: remote_name.to_string(),
            created_at: now,
            updated_at: now,
        })
    }

    pub async fn disconnect(
        &self,
        user_id: i32,
        application_id: &str,
        binding_id: i64,
    ) -> Result<ai_application_git_bindings::Model, GitBindingError> {
        let application = self.application(user_id, application_id).await?;
        let binding = ai_application_git_bindings::Entity::find_by_id(binding_id)
            .filter(ai_application_git_bindings::Column::ApplicationId.eq(application.id))
            .one(self.db.as_ref())
            .await
            .map_err(|source| GitBindingError::Database {
                operation: "load the binding to disconnect",
                application_id: application_id.to_string(),
                source,
            })?
            .ok_or_else(|| GitBindingError::BindingNotFound {
                application_id: application_id.to_string(),
                binding_id,
            })?;
        ai_application_git_bindings::Entity::delete_by_id(binding_id)
            .exec(self.db.as_ref())
            .await
            .map_err(|source| GitBindingError::Database {
                operation: "disconnect",
                application_id: application_id.to_string(),
                source,
            })?;
        Ok(binding)
    }
}

fn validate_remote_name(value: &str) -> Result<(), GitBindingError> {
    if value.is_empty()
        || value.len() > 64
        || !value.as_bytes()[0].is_ascii_alphanumeric()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(GitBindingError::InvalidRemoteName {
            remote_name: value.to_string(),
        });
    }
    Ok(())
}

fn connection_is_usable(connection: &git_provider_connections::Model) -> bool {
    if !connection.is_active || connection.is_expired {
        return false;
    }
    let now = Utc::now();
    connection
        .token_expires_at
        .is_none_or(|expires| expires > now)
        || connection.installation_id.is_some()
        || (connection.refresh_token.is_some()
            && connection
                .refresh_token_expires_at
                .is_none_or(|expires| expires > now))
}

fn canonical_https_url(value: &str, provider: &git_providers::Model) -> Option<String> {
    let value = value.trim();
    let lower = value.to_ascii_lowercase();
    if value.contains('\\')
        || lower.contains("%2e")
        || lower.contains("%2f")
        || lower.contains("%5c")
    {
        return None;
    }
    let parsed = url::Url::parse(value).ok()?;
    if parsed.scheme() != "https"
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return None;
    }
    let origin = if let Some(base) = provider.base_url.as_deref() {
        url::Url::parse(base).ok()?
    } else {
        let host = match provider.provider_type.as_str() {
            "github" => "github.com",
            "gitlab" => "gitlab.com",
            "bitbucket" => "bitbucket.org",
            _ => return None,
        };
        url::Url::parse(&format!("https://{host}")).ok()?
    };
    if origin.scheme() != "https"
        || parsed.host_str() != origin.host_str()
        || parsed.port_or_known_default() != origin.port_or_known_default()
        || parsed
            .path_segments()?
            .filter(|part| !part.is_empty())
            .count()
            < 2
    {
        return None;
    }
    Some(parsed.as_str().trim_end_matches('/').to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};

    fn application(owner: i32) -> ai_applications::Model {
        let now = Utc::now();
        ai_applications::Model {
            id: 11,
            public_id: "app_test".into(),
            name: "Test".into(),
            description: None,
            status: "active".into(),
            created_by: owner,
            created_at: now,
            updated_at: now,
        }
    }

    fn binding() -> ai_application_git_bindings::Model {
        let now = Utc::now();
        ai_application_git_bindings::Model {
            id: 41,
            application_id: 11,
            project_id: 7,
            connection_id: 17,
            repository_id: 23,
            repository_url: "https://github.com/example/service.git".into(),
            remote_name: "origin".into(),
            created_at: now,
            updated_at: now,
        }
    }

    fn provider() -> git_providers::Model {
        let now = Utc::now();
        git_providers::Model {
            id: 3,
            name: "GitHub".into(),
            provider_type: "github".into(),
            base_url: None,
            api_url: None,
            auth_method: "app".into(),
            auth_config: serde_json::json!({}),
            webhook_secret: None,
            is_active: true,
            is_default: true,
            created_at: now,
            updated_at: now,
        }
    }

    #[tokio::test]
    async fn cross_user_application_access_is_denied_before_bindings_are_read() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([Vec::<ai_applications::Model>::new()])
                .into_connection(),
        );
        let error = GitBindingService::new(db)
            .list(99, "app_test")
            .await
            .expect_err("another user's application must remain hidden");
        assert!(matches!(
            error,
            GitBindingError::ApplicationNotFound { user_id: 99, .. }
        ));
    }

    #[tokio::test]
    async fn list_returns_multiple_project_remotes_for_the_owned_application() {
        let mut second = binding();
        second.id = 42;
        second.project_id = 8;
        second.remote_name = "upstream".into();
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![application(7)]])
                .append_query_results([vec![binding(), second]])
                .into_connection(),
        );
        let result = GitBindingService::new(db)
            .list(7, "app_test")
            .await
            .expect("owned bindings");
        assert_eq!(result.len(), 2);
        assert_eq!(result[1].remote_name, "upstream");
    }

    #[tokio::test]
    async fn bind_rejects_a_project_outside_the_application() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![application(7)]])
                .append_query_results([Vec::<ai_application_projects::Model>::new()])
                .into_connection(),
        );
        let error = GitBindingService::new(db)
            .bind(7, "app_test", 99, 17, 23, "origin")
            .await
            .expect_err("unlinked project");
        assert!(matches!(
            error,
            GitBindingError::ProjectNotLinked { project_id: 99, .. }
        ));
    }

    #[tokio::test]
    async fn bind_rejects_a_connection_not_owned_by_the_application_owner() {
        let link = ai_application_projects::Model {
            id: 1,
            application_id: 11,
            project_id: 7,
            is_primary: true,
            created_at: Utc::now(),
        };
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![application(7)]])
                .append_query_results([vec![link]])
                .append_query_results([Vec::<git_provider_connections::Model>::new()])
                .into_connection(),
        );
        let error = GitBindingService::new(db)
            .bind(7, "app_test", 7, 17, 23, "origin")
            .await
            .expect_err("foreign connection");
        assert!(matches!(
            error,
            GitBindingError::ConnectionUnavailable {
                connection_id: 17,
                user_id: 7
            }
        ));
    }

    #[test]
    fn remote_names_and_canonical_urls_are_strict() {
        assert!(validate_remote_name("origin").is_ok());
        for invalid in ["-origin", "up.stream", "two words", "", "é"] {
            assert!(validate_remote_name(invalid).is_err(), "{invalid}");
        }
        let provider = provider();
        assert_eq!(
            canonical_https_url("https://github.com/example/service.git/", &provider).as_deref(),
            Some("https://github.com/example/service.git")
        );
        for invalid in [
            "http://github.com/example/service.git",
            "https://token@github.com/example/service.git",
            "https://evil.example/example/service.git",
            "https://github.com/example/%2e%2e/service.git",
            "https://github.com/example/service.git?token=x",
        ] {
            assert!(
                canonical_https_url(invalid, &provider).is_none(),
                "{invalid}"
            );
        }
    }

    #[tokio::test]
    async fn disconnect_requires_an_owned_application_and_scoped_binding() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![application(7)]])
                .append_query_results([Vec::<ai_application_git_bindings::Model>::new()])
                .into_connection(),
        );
        let error = GitBindingService::new(db)
            .disconnect(7, "app_test", 404)
            .await
            .expect_err("unknown binding");
        assert!(matches!(
            error,
            GitBindingError::BindingNotFound {
                binding_id: 404,
                ..
            }
        ));
    }
}
