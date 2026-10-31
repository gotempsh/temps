// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Gitea webhook lifecycle.

use super::*;

impl ProjectService {
    // ── Gitea webhook install / resolve ─────────────────────────────────────────
    //
    // BLOCKER 1 fix: Gitea webhooks use HMAC-SHA256 (unlike Bitbucket's
    // secret-in-path).  We generate a signing token with OsRng, register it
    // with Gitea's API via `create_webhook` (passing the secret in the config),
    // then store it encrypted in `projects.gitea_webhook_signing_token`.
    //
    // Failures are always non-fatal — same policy as Bitbucket/GitLab.

    /// Resolve whether `connection_id` points to a Gitea provider.
    ///
    /// Returns `(access_token, base_url)` for Gitea connections;
    /// `Err(String)` for all others (caller silently skips).
    async fn resolve_gitea_connection(
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

        if provider.provider_type != "gitea" {
            return Err(format!(
                "Provider {} is not a Gitea provider (type: {})",
                provider.id, provider.provider_type
            ));
        }

        let base_url = provider
            .base_url
            .ok_or_else(|| format!("Gitea provider {} has no base_url configured", provider.id))?;

        let access_token = self
            .git_provider_manager
            .get_connection_token(connection_id)
            .await
            .map_err(|e| {
                format!("Failed to get access token for Gitea connection {connection_id}: {e}")
            })?;

        Ok((access_token, base_url))
    }

    /// Install a Gitea webhook for `project_id` / `connection_id`.
    ///
    /// Steps:
    /// 1. Resolve the connection to confirm it's a Gitea provider.
    /// 2. Generate a fresh 32-byte hex signing token via `generate_gitea_signing_token`.
    /// 3. Build the delivery URL: `{public ingress URL}/api/webhook/git/gitea/events`.
    /// 4. Call `create_webhook` with the HMAC secret (Gitea HMAC signing).
    /// 5. Encrypt the token and return it for storage.
    ///
    /// Returns `Ok(encrypted_token)` on success; `Err(String)` on any failure
    /// (non-fatal — caller logs and continues).
    pub(in crate::services::project) async fn install_gitea_webhook_for_connection(
        &self,
        project_id: i32,
        connection_id: i32,
        owner: &str,
        repo: &str,
    ) -> Result<String, String> {
        use temps_git::services::git_provider::WebhookConfig;
        use temps_git::services::gitea_provider::generate_gitea_signing_token;

        let (access_token, _base_url) = match self.resolve_gitea_connection(connection_id).await {
            Ok(pair) => pair,
            Err(e) => return Err(e),
        };

        // Generate a fresh HMAC secret (used as the Gitea webhook secret).
        let signing_token = generate_gitea_signing_token()
            .map_err(|error| format!("Failed to generate Gitea webhook token: {error}"))?;

        let webhook_url = format!(
            "{}/api/webhook/git/gitea/events",
            self.git_webhook_base_url().await
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
                format!("Failed to get provider service for Gitea connection {connection_id}: {e}")
            })?;

        // Gitea webhooks are HMAC-SHA256; pass the secret in WebhookConfig.secret.
        provider_service
            .create_webhook(
                &access_token,
                owner,
                repo,
                WebhookConfig {
                    url: webhook_url,
                    secret: Some(signing_token.clone()), // HMAC secret
                    events: vec!["push".to_string()],
                },
            )
            .await
            .map_err(|e| {
                format!(
                    "Failed to register Gitea webhook for project {project_id} ({owner}/{repo}): {e}"
                )
            })?;

        // Encrypt the signing token before storing.
        let encrypted_token = self
            .encryption_service
            .encrypt_string(&signing_token)
            .map_err(|e| format!("Failed to encrypt Gitea webhook signing token: {e}"))?;

        info!(
            "Registered Gitea webhook for project {} ({}/{})",
            project_id, owner, repo
        );

        Ok(encrypted_token)
    }
}
