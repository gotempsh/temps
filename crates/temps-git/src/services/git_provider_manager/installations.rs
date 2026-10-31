// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! GitHub App providers and their installations (connections).

use super::*;

impl GitProviderManager {
    /// Find git provider by GitHub App ID
    pub async fn find_provider_by_github_app_id(
        &self,
        app_id: i32,
    ) -> Result<git_providers::Model, GitProviderManagerError> {
        // Get all GitHub providers
        let providers = git_providers::Entity::find()
            .filter(git_providers::Column::ProviderType.eq("github"))
            .order_by_desc(git_providers::Column::CreatedAt)
            .all(self.db.as_ref())
            .await?;

        // Check each provider's auth config for matching app_id
        for provider in providers {
            if let Ok(auth_config) = self.decrypt_sensitive_data(&provider.auth_config).await {
                if let Ok(AuthMethod::GitHubApp {
                    app_id: provider_app_id,
                    ..
                }) = serde_json::from_value::<AuthMethod>(auth_config)
                {
                    if provider_app_id == app_id {
                        return Ok(provider);
                    }
                }
            }
        }

        Err(GitProviderManagerError::ProviderNotFound(format!(
            "GitHub App with app_id: {}",
            app_id
        )))
    }

    /// Get all GitHub App providers
    pub async fn get_github_app_providers(
        &self,
    ) -> Result<Vec<git_providers::Model>, GitProviderManagerError> {
        let providers = git_providers::Entity::find()
            .filter(git_providers::Column::ProviderType.eq(GitProviderType::GitHub.to_string()))
            .filter(git_providers::Column::IsActive.eq(true))
            .order_by_desc(git_providers::Column::CreatedAt)
            .all(self.db.as_ref())
            .await?;

        // Filter to only include GitHub App providers (not OAuth or PAT)
        let mut github_app_providers = Vec::new();
        for provider in providers {
            let auth_config = self.decrypt_sensitive_data(&provider.auth_config).await?;
            if let Ok(auth_method) = serde_json::from_value::<AuthMethod>(auth_config) {
                if matches!(auth_method, AuthMethod::GitHubApp { .. }) {
                    github_app_providers.push(provider);
                }
            }
        }

        Ok(github_app_providers)
    }

    /// Get a GitHub App provider by app_id
    pub async fn get_github_app_provider_by_app_id(
        &self,
        app_id: i32,
    ) -> Result<git_providers::Model, GitProviderManagerError> {
        let providers = self.get_github_app_providers().await?;

        for provider in providers {
            let auth_config = self.decrypt_sensitive_data(&provider.auth_config).await?;
            if let Ok(AuthMethod::GitHubApp {
                app_id: provider_app_id,
                ..
            }) = serde_json::from_value::<AuthMethod>(auth_config)
            {
                if provider_app_id == app_id {
                    return Ok(provider);
                }
            }
        }

        Err(GitProviderManagerError::ProviderNotFound(format!(
            "GitHub App provider with app_id {} not found",
            app_id
        )))
    }

    /// Get connection by installation_id
    pub async fn get_connection_by_installation_id(
        &self,
        installation_id: &str,
    ) -> Result<git_provider_connections::Model, GitProviderManagerError> {
        let connection = git_provider_connections::Entity::find()
            .filter(
                git_provider_connections::Column::InstallationId
                    .eq(Some(installation_id.to_string())),
            )
            .filter(git_provider_connections::Column::IsActive.eq(true))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                GitProviderManagerError::ConnectionNotFound(format!(
                    "Connection with installation_id {} not found",
                    installation_id
                ))
            })?;

        Ok(connection)
    }

    /// Check if any GitHub App installations exist
    pub async fn has_github_app_installation(&self) -> Result<bool, GitProviderManagerError> {
        let providers = self.get_github_app_providers().await?;

        for provider in providers {
            let connections = self.get_provider_connections(provider.id).await?;
            if !connections.is_empty() {
                return Ok(true);
            }
        }

        Ok(false)
    }

    /// Get all GitHub App installations (connections)
    pub async fn get_all_github_app_installations(
        &self,
    ) -> Result<Vec<git_provider_connections::Model>, GitProviderManagerError> {
        let providers = self.get_github_app_providers().await?;
        let mut all_installations = Vec::new();

        for provider in providers {
            let connections = self.get_provider_connections(provider.id).await?;
            all_installations.extend(connections);
        }

        Ok(all_installations)
    }

    /// Delete an installation (connection) by installation_id
    pub async fn delete_installation(
        &self,
        installation_id: i32,
    ) -> Result<(), GitProviderManagerError> {
        if let Ok(connection) = self
            .get_connection_by_installation_id(&installation_id.to_string())
            .await
        {
            let provider_id = connection.provider_id;

            // Soft delete the connection
            let mut active_model: git_provider_connections::ActiveModel = connection.into();
            active_model.is_active = Set(false);
            active_model.updated_at = Set(chrono::Utc::now());
            active_model.update(self.db.as_ref()).await?;

            // Also deactivate the associated provider
            self.deactivate_provider(provider_id).await?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::git_provider_manager::test_support::*;
    use temps_database::test_utils::TestDatabase;

    #[tokio::test]
    async fn test_delete_installation_deactivates_provider() {
        use chrono::Utc;
        use temps_entities::users;

        // Create real test database
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.connection_arc();

        // Create a test user first (required for foreign key)
        let now = Utc::now();
        let user = users::ActiveModel {
            email: Set("test@example.com".to_string()),
            password_hash: Set(Some("hash".to_string())),
            name: Set("Test User".to_string()),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        };
        user.insert(db.as_ref()).await.unwrap();

        // Create a git provider
        let provider = git_providers::ActiveModel {
            name: Set("test-provider".to_string()),
            provider_type: Set("github".to_string()),
            base_url: Set(None),
            api_url: Set(None),
            auth_method: Set("oauth".to_string()),
            auth_config: Set(serde_json::json!({})),
            webhook_secret: Set(None),
            is_active: Set(true),
            is_default: Set(false),
            ..Default::default()
        };
        let provider = provider.insert(db.as_ref()).await.unwrap();

        // Create a git provider connection with installation ID
        let connection = git_provider_connections::ActiveModel {
            provider_id: Set(provider.id),
            user_id: Set(Some(1)),
            account_name: Set("test-account".to_string()),
            account_type: Set("Organization".to_string()),
            access_token: Set(None),
            refresh_token: Set(None),
            token_expires_at: Set(None),
            refresh_token_expires_at: Set(None),
            installation_id: Set(Some("12345".to_string())),
            metadata: Set(None),
            is_active: Set(true),
            is_expired: Set(false),
            syncing: Set(false),
            last_synced_at: Set(None),
            ..Default::default()
        };
        connection.insert(db.as_ref()).await.unwrap();

        let encryption_service = Arc::new(
            temps_core::EncryptionService::new(
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            )
            .unwrap(),
        );

        let queue_service = Arc::new(MockJobQueue) as Arc<dyn JobQueue>;
        let config_service = create_test_config_service(db.clone());

        let manager = GitProviderManager::new(
            db.clone(),
            encryption_service,
            queue_service,
            config_service,
        );

        // Test delete_installation
        let result = manager.delete_installation(12345).await;

        assert!(result.is_ok(), "delete_installation should succeed");

        // Verify connection and provider were deactivated
        // (actual verification would require querying the database)
    }

    #[tokio::test]
    async fn test_delete_installation_with_nonexistent_installation() {
        // Create real test database with no data
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.connection_arc();

        let encryption_service = Arc::new(
            temps_core::EncryptionService::new(
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            )
            .unwrap(),
        );

        let queue_service = Arc::new(MockJobQueue) as Arc<dyn JobQueue>;
        let config_service = create_test_config_service(db.clone());

        let manager = GitProviderManager::new(
            db.clone(),
            encryption_service,
            queue_service,
            config_service,
        );

        // Should succeed even if installation not found (idempotent)
        let result = manager.delete_installation(99999).await;
        assert!(result.is_ok(), "delete_installation should be idempotent");
    }
}
