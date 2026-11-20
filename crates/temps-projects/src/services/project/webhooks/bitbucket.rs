// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Bitbucket Cloud webhook lifecycle.

use super::*;

impl ProjectService {
    // ── Bitbucket Cloud webhook lifecycle ────────────────────────────────────
    //
    // Mirrors the GitLab pattern.  The delivery URL embeds a secret token in
    // the path (`…/bitbucket/events/{token}`); there is no HMAC body signing.
    // The token is generated once per project, stored encrypted, and persisted
    // together with the Bitbucket hook UUID so we can `DELETE` it on disconnect.
    //
    // Failures are always non-fatal: the project connects without the webhook
    // and the operator can re-try via the Temps UI or re-connect the repo.

    /// Resolve whether `connection_id` points to a Bitbucket provider.
    ///
    /// Returns `(access_token, auth_method_str)` for Bitbucket connections;
    /// `Err(String)` for all others (caller silently skips).
    async fn resolve_bitbucket_connection(
        &self,
        connection_id: i32,
    ) -> Result<(String, String), String> {
        use temps_entities::{git_provider_connections, git_providers};

        let connection = git_provider_connections::Entity::find_by_id(connection_id)
            .one(self.db.as_ref())
            .await
            .map_err(|e| format!("DB error fetching connection {connection_id}: {e}"))?
            .ok_or_else(|| format!("Connection {connection_id} not found"))?;

        let provider = git_providers::Entity::find_by_id(connection.provider_id)
            .one(self.db.as_ref())
            .await
            .map_err(|e| format!("DB error fetching provider {}: {e}", connection.provider_id))?
            .ok_or_else(|| format!("Provider {} not found", connection.provider_id))?;

        if provider.provider_type != "bitbucket" {
            return Err(format!(
                "Provider {} is not a Bitbucket provider (type: {})",
                provider.id, provider.provider_type
            ));
        }

        let auth_method = provider.auth_method.clone();

        let access_token = self
            .git_provider_manager
            .get_connection_token(connection_id)
            .await
            .map_err(|e| {
                format!("Failed to get access token for Bitbucket connection {connection_id}: {e}")
            })?;

        Ok((access_token, auth_method))
    }

    /// Install a Bitbucket Cloud webhook for `project_id` / `connection_id`.
    ///
    /// Steps:
    /// 1. Generate a fresh `bitbucket_webhook_token` via `OsRng`.
    /// 2. Build the delivery URL: `{public ingress URL}/api/webhook/git/bitbucket/events/{token}`.
    /// 3. Call `POST /2.0/repositories/{workspace}/{slug}/hooks`.
    /// 4. Return `(hook_uuid, encrypted_token)` to the caller for persistence.
    pub(in crate::services::project) async fn install_bitbucket_webhook_for_connection(
        &self,
        project_id: i32,
        connection_id: i32,
        owner: &str,
        repo: &str,
    ) -> Result<(String, String), String> {
        use temps_git::services::bitbucket_provider::generate_bitbucket_webhook_token;
        use temps_git::services::git_provider::WebhookConfig;

        let (access_token, _auth_method_str) =
            match self.resolve_bitbucket_connection(connection_id).await {
                Ok(pair) => pair,
                Err(e) => return Err(e),
            };

        let webhook_base_url = self.git_webhook_base_url().await;

        // Generate a fresh secret-in-path delivery token.
        let delivery_token = generate_bitbucket_webhook_token()
            .map_err(|error| format!("Failed to generate Bitbucket webhook token: {error}"))?;

        let webhook_url = format!(
            "{}/api/webhook/git/bitbucket/events/{}",
            webhook_base_url, delivery_token
        );

        // Resolve the connection to get provider_id, then get provider service.
        let connection = {
            use temps_entities::git_provider_connections;
            git_provider_connections::Entity::find_by_id(connection_id)
                .one(self.db.as_ref())
                .await
                .map_err(|e| format!("DB error fetching connection {connection_id}: {e}"))?
                .ok_or_else(|| format!("Connection {connection_id} not found"))?
        };

        let provider_service = self
            .git_provider_manager
            .get_provider_service(connection.provider_id)
            .await
            .map_err(|e| {
                format!(
                    "Failed to get provider service for Bitbucket connection {connection_id}: {e}"
                )
            })?;

        let hook_uuid = provider_service
            .create_webhook(
                &access_token,
                owner,
                repo,
                WebhookConfig {
                    url: webhook_url,
                    secret: None, // Bitbucket uses secret-in-path; no HMAC secret
                    events: vec![
                        "repo:push".to_string(),
                        "pullrequest:created".to_string(),
                        "pullrequest:updated".to_string(),
                        "pullrequest:fulfilled".to_string(),
                        "pullrequest:rejected".to_string(),
                    ],
                },
            )
            .await
            .map_err(|e| {
                format!(
                    "Failed to register Bitbucket webhook for project {project_id} ({owner}/{repo}): {e}"
                )
            })?;

        // Encrypt the delivery token before storing.
        let encrypted_token = self
            .encryption_service
            .encrypt_string(&delivery_token)
            .map_err(|e| format!("Failed to encrypt Bitbucket webhook token: {e}"))?;

        info!(
            "Registered Bitbucket webhook {} for project {} ({}/{})",
            hook_uuid, project_id, owner, repo
        );

        Ok((hook_uuid, encrypted_token))
    }

    /// Delete a previously auto-registered Bitbucket webhook.  Best-effort;
    /// 404 is treated as success (already gone).
    pub(in crate::services::project) async fn delete_bitbucket_webhook_for_connection(
        &self,
        connection_id: i32,
        owner: &str,
        repo: &str,
        hook_uuid: &str,
    ) -> Result<(), String> {
        let (access_token, _) = match self.resolve_bitbucket_connection(connection_id).await {
            Ok(pair) => pair,
            // Not a Bitbucket connection — nothing to do.
            Err(_) => return Ok(()),
        };

        let connection = {
            use temps_entities::git_provider_connections;
            git_provider_connections::Entity::find_by_id(connection_id)
                .one(self.db.as_ref())
                .await
                .map_err(|e| format!("DB error: {e}"))?
                .ok_or_else(|| format!("Connection {connection_id} not found"))?
        };

        let provider_service = self
            .git_provider_manager
            .get_provider_service(connection.provider_id)
            .await
            .map_err(|e| {
                format!(
                    "Failed to get provider service for Bitbucket connection {connection_id}: {e}"
                )
            })?;

        provider_service
            .delete_webhook(&access_token, owner, repo, hook_uuid)
            .await
            .map_err(|e| format!("Bitbucket delete webhook error for {owner}/{repo}: {e}"))
    }
}
