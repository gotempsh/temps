// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! GitLab webhook lifecycle.

use super::*;

impl ProjectService {
    /// Resolve whether the given connection points to a GitLab provider.
    /// Returns `(base_url, access_token, auth_method)` for GitLab connections;
    /// `Err` for all others.
    async fn resolve_gitlab_connection(
        &self,
        connection_id: i32,
    ) -> Result<(String, String, String), String> {
        use temps_entities::{git_provider_connections, git_providers};

        let connection = git_provider_connections::Entity::find_by_id(connection_id)
            .one(self.db.as_ref())
            .await
            .map_err(|e| format!("DB error: {}", e))?
            .ok_or_else(|| format!("Connection {} not found", connection_id))?;

        let provider = git_providers::Entity::find_by_id(connection.provider_id)
            .one(self.db.as_ref())
            .await
            .map_err(|e| format!("DB error: {}", e))?
            .ok_or_else(|| format!("Provider {} not found", connection.provider_id))?;

        // Only handle GitLab providers.
        if provider.provider_type != "gitlab" {
            return Err(format!(
                "Provider {} is not a GitLab provider (type: {})",
                provider.id, provider.provider_type
            ));
        }

        let base_url = provider
            .base_url
            .unwrap_or_else(|| "https://gitlab.com".to_string());

        let auth_method = provider.auth_method.clone();

        let access_token = self
            .git_provider_manager
            .get_connection_token(connection_id)
            .await
            .map_err(|e| {
                format!(
                    "Failed to get access token for connection {}: {}",
                    connection_id, e
                )
            })?;

        Ok((base_url, access_token, auth_method))
    }

    /// Install a GitLab webhook for the given project/connection.
    /// Returns `(hook_id, encrypted_signing_token)` on success.
    pub(in crate::services::project) async fn install_gitlab_webhook_for_connection(
        &self,
        project_id: i32,
        connection_id: i32,
        owner: &str,
        repo: &str,
    ) -> Result<(i64, String), String> {
        use temps_git::services::gitlab_webhook::{
            generate_signing_token, GitLabWebhookClient, WebhookAuthMethod,
        };

        let (base_url, access_token, auth_method_str) =
            match self.resolve_gitlab_connection(connection_id).await {
                Ok(triple) => triple,
                // Not a GitLab provider — skip silently.
                Err(e) => return Err(e),
            };

        let client = GitLabWebhookClient::new(
            base_url,
            access_token,
            WebhookAuthMethod::from_str(&auth_method_str),
        );

        // Pre-flight: verify the user has >= Maintainer (40) access.
        let access_level = client
            .get_project_access_level(owner, repo)
            .await
            .map_err(|e| format!("Could not check permissions for {}/{}: {}", owner, repo, e))?;

        if access_level < 40 {
            return Err(format!(
                "Insufficient GitLab permissions for {}/{}: access_level={} (need >= 40 Maintainer)",
                owner, repo, access_level
            ));
        }

        // Resolve the webhook URL from config.
        let webhook_url = format!(
            "{}/api/webhook/git/gitlab/events",
            self.git_webhook_base_url().await
        );

        // Generate a random 32-byte signing token.
        let signing_token = generate_signing_token()
            .map_err(|error| format!("Failed to generate GitLab webhook token: {error}"))?;

        let hook_id = client
            .install_webhook(owner, repo, &webhook_url, &signing_token)
            .await
            .map_err(|e| {
                format!(
                    "Failed to install webhook for project {}: {}",
                    project_id, e
                )
            })?;

        // Encrypt the token before storing.
        let encrypted = self
            .encryption_service
            .encrypt_string(&signing_token)
            .map_err(|e| format!("Failed to encrypt signing token: {}", e))?;

        info!(
            "Installed GitLab webhook {} for project {} ({}/{})",
            hook_id, project_id, owner, repo
        );

        Ok((hook_id, encrypted))
    }

    /// Remove a GitLab webhook.  Best-effort; 404 is treated as success.
    pub(in crate::services::project) async fn delete_gitlab_webhook_for_connection(
        &self,
        connection_id: i32,
        owner: &str,
        repo: &str,
        hook_id: i32,
    ) -> Result<(), String> {
        use temps_git::services::gitlab_webhook::{GitLabWebhookClient, WebhookAuthMethod};

        let (base_url, access_token, auth_method_str) =
            match self.resolve_gitlab_connection(connection_id).await {
                Ok(triple) => triple,
                // Not a GitLab connection — nothing to remove.
                Err(_) => return Ok(()),
            };

        let client = GitLabWebhookClient::new(
            base_url,
            access_token,
            WebhookAuthMethod::from_str(&auth_method_str),
        );
        client
            .delete_webhook(owner, repo, hook_id as i64)
            .await
            .map_err(|e| format!("GitLab delete webhook error: {}", e))
    }

    /// Reinstall (or install for the first time) a GitLab webhook for a project.
    /// Called by `POST /projects/{id}/gitlab/reinstall-webhook`.
    pub async fn reinstall_gitlab_webhook(&self, project_id: i32) -> Result<i32, ProjectError> {
        let project = projects::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(ProjectError::NotFound(format!(
                "Project {} not found",
                project_id
            )))?;

        let connection_id = project.git_provider_connection_id.ok_or_else(|| {
            ProjectError::Other(format!(
                "Project {} has no git provider connection",
                project_id
            ))
        })?;

        let owner = project.repo_owner.clone();
        let repo = project.repo_name.clone();

        // Best-effort: remove the old webhook first.
        if let Some(old_hook_id) = project.gitlab_webhook_id {
            if let Err(e) = self
                .delete_gitlab_webhook_for_connection(connection_id, &owner, &repo, old_hook_id)
                .await
            {
                warn!(
                    "Failed to remove old GitLab webhook {} during reinstall for project {}: {}",
                    old_hook_id, project_id, e
                );
            }
        }

        let (hook_id, encrypted_token) = self
            .install_gitlab_webhook_for_connection(project_id, connection_id, &owner, &repo)
            .await
            .map_err(ProjectError::Other)?;

        // Persist the new hook id + token.
        let mut active_project: projects::ActiveModel = project.into();
        active_project.gitlab_webhook_id = Set(Some(hook_id as i32));
        active_project.gitlab_webhook_signing_token = Set(Some(encrypted_token));
        active_project.update(self.db.as_ref()).await?;

        info!(
            "Reinstalled GitLab webhook {} for project {}",
            hook_id, project_id
        );

        Ok(hook_id as i32)
    }
}
