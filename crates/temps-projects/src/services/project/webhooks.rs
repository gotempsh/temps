// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Git provider webhook lifecycle shared by every provider, plus the
//! generic webhook token.

use super::*;

mod bitbucket;
mod gitea;
mod github;
mod gitlab;

pub(super) use github::repository_target_changed;

/// Resolve the git webhook delivery base from the settings row:
/// `public_ingress_url` when set, `external_url` otherwise, and the local
/// development address when neither is configured.
fn git_webhook_base_url(settings: Option<&temps_core::AppSettings>) -> String {
    settings
        .and_then(temps_core::AppSettings::configured_public_ingress_url)
        .unwrap_or_else(|| "http://localhost:8080".to_string())
}

impl ProjectService {
    /// Base URL a git provider is told to deliver webhooks to, with no
    /// trailing slash.
    ///
    /// The provider calls from its own infrastructure, so this is the public
    /// ingress URL (ADR-051) and not necessarily where operators reach the
    /// console. An unreadable settings row falls back like an unconfigured
    /// one: a failed read must not abort a project's git setup.
    async fn git_webhook_base_url(&self) -> String {
        let settings = self.config_service.get_settings().await.ok();
        git_webhook_base_url(settings.as_ref())
    }

    // ── Generic webhook token install ────────────────────────────────────────────
    //
    // BLOCKER 2 fix: the Generic provider has no REST API to auto-register a
    // hook.  We generate a token and store it encrypted.  The webhook URL
    // (`{public ingress URL}/api/webhook/git/generic/events/{token}`) is surfaced to
    // the operator for manual configuration.
    //
    // Returns `Ok(Some(encrypted_token))` for Generic connections,
    // `Ok(None)` for non-Generic connections, `Err` on failure.

    /// Generate and store a generic webhook delivery token for `project_id`.
    ///
    /// Returns `Ok(Some(encrypted_token))` when `connection_id` is a Generic
    /// provider; `Ok(None)` when it's any other provider type (caller skips).
    pub(super) async fn install_generic_webhook_token(
        &self,
        project_id: i32,
        connection_id: i32,
    ) -> Result<Option<String>, String> {
        use temps_entities::{git_provider_connections, git_providers};
        use temps_git::services::generic_provider::generate_generic_webhook_token;

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

        if provider.provider_type != "generic" {
            // Not a Generic connection — nothing to do.
            return Ok(None);
        }

        let token = generate_generic_webhook_token()
            .map_err(|error| format!("Failed to generate Generic webhook token: {error}"))?;

        let encrypted_token = self
            .encryption_service
            .encrypt_string(&token)
            .map_err(|e| format!("Failed to encrypt Generic webhook token: {e}"))?;

        info!(
            "Generated and encrypted Generic webhook token for project {} (conn {})",
            project_id, connection_id
        );

        Ok(Some(encrypted_token))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ADR-051 D2: provider webhooks are delivered to the public ingress URL.
    #[test]
    fn git_webhooks_are_delivered_to_the_public_ingress_url_when_set() {
        let settings = temps_core::AppSettings {
            external_url: Some("https://console.example.com".to_string()),
            public_ingress_url: Some("https://hooks.example.com/".to_string()),
            ..Default::default()
        };
        assert_eq!(
            git_webhook_base_url(Some(&settings)),
            "https://hooks.example.com"
        );
    }

    #[test]
    fn git_webhooks_fall_back_to_the_external_url_then_the_local_default() {
        let settings = temps_core::AppSettings {
            external_url: Some("https://console.example.com".to_string()),
            ..Default::default()
        };
        assert_eq!(
            git_webhook_base_url(Some(&settings)),
            "https://console.example.com"
        );
        assert_eq!(
            git_webhook_base_url(Some(&temps_core::AppSettings::default())),
            "http://localhost:8080"
        );
        // An unreadable settings row behaves like an unconfigured one.
        assert_eq!(git_webhook_base_url(None), "http://localhost:8080");
    }
}
