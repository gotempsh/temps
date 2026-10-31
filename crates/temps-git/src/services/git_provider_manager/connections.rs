// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Provider connections: create, look up (scoped to the caller where needed),
//! list, (de)activate and delete them.

use super::*;

impl GitProviderManager {
    /// Create a connection to a git provider for a user
    #[allow(clippy::too_many_arguments)]
    pub async fn create_connection(
        &self,
        provider_id: i32,
        user_id: i32,
        account_name: String,
        account_type: String,
        access_token: Option<String>,
        refresh_token: Option<String>,
        installation_id: Option<String>,
        metadata: Option<serde_json::Value>,
        expires_at: Option<UtcDateTime>,
    ) -> Result<git_provider_connections::Model, GitProviderManagerError> {
        // Encrypt tokens if provided
        let encrypted_access = if let Some(token) = access_token {
            Some(self.encrypt_string(&token).await?)
        } else {
            None
        };

        let encrypted_refresh = if let Some(token) = refresh_token {
            Some(self.encrypt_string(&token).await?)
        } else {
            None
        };

        let new_connection = git_provider_connections::ActiveModel {
            provider_id: Set(provider_id),
            user_id: Set(Some(user_id)),
            account_name: Set(account_name),
            account_type: Set(account_type),
            access_token: Set(encrypted_access),
            refresh_token: Set(encrypted_refresh),
            installation_id: Set(installation_id),
            metadata: Set(metadata),
            is_active: Set(true),
            token_expires_at: Set(expires_at),
            ..Default::default()
        };

        let connection = new_connection.insert(self.db.as_ref()).await?;

        // Probe the upstream immediately so the connection's health_status
        // reflects reality without waiting for the scheduled sweep. We never
        // let a probe failure break connection creation — an unreachable
        // provider is recorded as unhealthy with a reason and the caller gets
        // their connection row back.
        if let Err(e) = self.probe_and_record_connection_health(connection.id).await {
            tracing::warn!(
                connection_id = connection.id,
                error = %e,
                "Initial connection health probe failed to record; row stays at default status"
            );
        }

        // Re-fetch so callers see the updated health_status/last_health_check_at
        // instead of the pre-probe defaults.
        let connection = self.get_connection(connection.id).await?;
        Ok(connection)
    }

    /// Get connections for a user
    pub async fn get_user_connections(
        &self,
    ) -> Result<Vec<git_provider_connections::Model>, GitProviderManagerError> {
        let connections = git_provider_connections::Entity::find()
            .filter(git_provider_connections::Column::IsActive.eq(true))
            .order_by_desc(git_provider_connections::Column::CreatedAt)
            .all(self.db.as_ref())
            .await?;

        Ok(connections)
    }

    /// Get all connections with pagination and sorting.
    ///
    /// Connections are visible to every authenticated user (single-tenant:
    /// "the team is the app" for now) rather than scoped to their creator,
    /// so teammates can share and reuse each other's git connections.
    pub async fn get_user_connections_paginated(
        &self,
        page: u64,
        per_page: u64,
        sort: &str,
        direction: &str,
    ) -> Result<(Vec<git_provider_connections::Model>, usize), GitProviderManagerError> {
        use sea_orm::QueryOrder;

        let mut query = git_provider_connections::Entity::find()
            .filter(git_provider_connections::Column::IsActive.eq(true));

        // Apply sorting - default to created_at desc
        query = match (sort, direction) {
            ("updated_at", "asc") => {
                query.order_by_asc(git_provider_connections::Column::UpdatedAt)
            }
            ("updated_at", "desc") => {
                query.order_by_desc(git_provider_connections::Column::UpdatedAt)
            }
            ("account_name", "asc") => {
                query.order_by_asc(git_provider_connections::Column::AccountName)
            }
            ("account_name", "desc") => {
                query.order_by_desc(git_provider_connections::Column::AccountName)
            }
            ("created_at", "asc") => {
                query.order_by_asc(git_provider_connections::Column::CreatedAt)
            }
            ("created_at", "desc") | (_, _) => {
                query.order_by_desc(git_provider_connections::Column::CreatedAt)
            }
        };

        // Get total count
        let total_count = query.clone().count(self.db.as_ref()).await?;

        // Apply pagination
        let offset = (page - 1) * per_page;
        let connections = query
            .offset(offset)
            .limit(per_page)
            .all(self.db.as_ref())
            .await?;

        Ok((connections, total_count as usize))
    }

    /// Get a specific connection
    pub async fn get_connection(
        &self,
        connection_id: i32,
    ) -> Result<git_provider_connections::Model, GitProviderManagerError> {
        let connection = git_provider_connections::Entity::find_by_id(connection_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                GitProviderManagerError::ConnectionNotFound(format!(
                    "no git connection with id {connection_id}"
                ))
            })?;

        Ok(connection)
    }

    /// Resolve a connection only when it belongs to the authenticated user.
    /// Returning not-found for both missing and foreign connections avoids an
    /// ownership oracle at callers that use stored provider credentials.
    pub async fn get_connection_for_user(
        &self,
        connection_id: i32,
        user_id: i32,
    ) -> Result<git_provider_connections::Model, GitProviderManagerError> {
        git_provider_connections::Entity::find()
            .filter(git_provider_connections::Column::Id.eq(connection_id))
            .filter(git_provider_connections::Column::UserId.eq(user_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| foreign_or_missing_connection(connection_id))
    }

    /// Get every connection for a provider, regardless of owner or active state.
    ///
    /// Deletion checks must use this rather than [`Self::get_provider_connections`]:
    /// a deactivated connection (or one owned by another user) still holds rows
    /// that block or get cascaded by a provider delete, and counting only the
    /// active ones produced errors referencing connections the caller could not
    /// see anywhere in the UI.
    pub async fn get_all_provider_connections(
        &self,
        provider_id: i32,
    ) -> Result<Vec<git_provider_connections::Model>, GitProviderManagerError> {
        let connections = git_provider_connections::Entity::find()
            .filter(git_provider_connections::Column::ProviderId.eq(provider_id))
            .order_by_desc(git_provider_connections::Column::CreatedAt)
            .all(self.db.as_ref())
            .await?;

        Ok(connections)
    }

    /// Get the active connections for a specific provider
    pub async fn get_provider_connections(
        &self,
        provider_id: i32,
    ) -> Result<Vec<git_provider_connections::Model>, GitProviderManagerError> {
        let connections = git_provider_connections::Entity::find()
            .filter(git_provider_connections::Column::ProviderId.eq(provider_id))
            .filter(git_provider_connections::Column::IsActive.eq(true))
            .order_by_desc(git_provider_connections::Column::CreatedAt)
            .all(self.db.as_ref())
            .await?;

        Ok(connections)
    }

    /// Deactivate a git provider connection
    pub async fn deactivate_connection(
        &self,
        connection_id: i32,
    ) -> Result<(), GitProviderManagerError> {
        // Check if connection exists
        let connection = self.get_connection(connection_id).await?;

        if !connection.is_active {
            return Ok(()); // Already deactivated
        }

        // Check if connection is in use by any projects
        let project_count = temps_entities::projects::Entity::find()
            .filter(
                temps_entities::projects::Column::GitProviderConnectionId.eq(Some(connection_id)),
            )
            .count(self.db.as_ref())
            .await?;

        if project_count > 0 {
            return Err(GitProviderManagerError::InvalidConfiguration(format!(
                "Cannot deactivate connection {} because it is used by {} project(s)",
                connection_id, project_count
            )));
        }

        // Deactivate the connection
        let mut active_model: git_provider_connections::ActiveModel = connection.into();
        active_model.is_active = Set(false);
        active_model.updated_at = Set(chrono::Utc::now());
        active_model.update(self.db.as_ref()).await?;

        Ok(())
    }

    /// Activate a git provider connection
    pub async fn activate_connection(
        &self,
        connection_id: i32,
    ) -> Result<(), GitProviderManagerError> {
        // Check if connection exists
        let connection = self.get_connection(connection_id).await?;

        if connection.is_active {
            return Ok(()); // Already activated
        }

        // Check if the provider is active
        let provider = self.get_provider(connection.provider_id).await?;
        if !provider.is_active {
            return Err(GitProviderManagerError::InvalidConfiguration(format!(
                "Cannot activate connection because provider {} is deactivated",
                provider.name
            )));
        }

        // Activate the connection
        let mut active_model: git_provider_connections::ActiveModel = connection.into();
        active_model.is_active = Set(true);
        active_model.updated_at = Set(chrono::Utc::now());
        active_model.update(self.db.as_ref()).await?;

        Ok(())
    }

    /// Permanently delete a git provider connection (hard delete)
    pub async fn delete_connection(
        &self,
        connection_id: i32,
    ) -> Result<(), GitProviderManagerError> {
        // Check if connection exists
        self.get_connection(connection_id).await?;

        // Check if connection is in use by any projects. Name them — "used by 2
        // project(s)" leaves the user hunting through every project to work out
        // which ones to disconnect first.
        // Select id + name only. Loading whole project models makes the check
        // fail with an opaque "Database Error: unexpected value for Preset
        // enum" if any blocking project carries a column value this build's
        // enums don't know — and the user loses the real reason they can't
        // delete, which is the whole point of this branch.
        let projects: Vec<(i32, String)> = temps_entities::projects::Entity::find()
            .select_only()
            .column(temps_entities::projects::Column::Id)
            .column(temps_entities::projects::Column::Name)
            .filter(
                temps_entities::projects::Column::GitProviderConnectionId.eq(Some(connection_id)),
            )
            .into_tuple()
            .all(self.db.as_ref())
            .await?;

        if !projects.is_empty() {
            let project_names: Vec<String> = projects
                .iter()
                .map(|(id, name)| format!("'{}' (ID: {})", name, id))
                .collect();
            return Err(GitProviderManagerError::InvalidConfiguration(format!(
                "Cannot delete connection {} because it is used by {} project(s): {}. Change the git source of those projects (or delete them) first.",
                connection_id,
                projects.len(),
                project_names.join(", ")
            )));
        }

        // Delete repositories associated with this connection
        repositories::Entity::delete_many()
            .filter(repositories::Column::GitProviderConnectionId.eq(Some(connection_id)))
            .exec(self.db.as_ref())
            .await?;

        // Delete the connection
        git_provider_connections::Entity::delete_by_id(connection_id)
            .exec(self.db.as_ref())
            .await?;

        Ok(())
    }
}
