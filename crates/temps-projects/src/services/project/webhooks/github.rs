// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! GitHub repository webhook lifecycle.

use super::*;

/// URL of the GitHub repository webhook registered for `project_id`.
///
/// GitHub refuses a second hook with the same URL and events on a repository,
/// so each project's hook needs a URL of its own for two projects to deploy
/// from one repository. The receiver ignores the query: it identifies the
/// project by the secret that signed the delivery.
fn github_repo_webhook_url(base_url: &str, project_id: i32) -> String {
    use temps_git::handlers::github::GITHUB_REPO_WEBHOOK_PATH;

    format!("{base_url}/api{GITHUB_REPO_WEBHOOK_PATH}?project_id={project_id}")
}

/// Whether a provider needs Temps to register a GitHub repository webhook for
/// its projects: a GitHub provider authenticated any way other than as a
/// GitHub App. An App delivers pushes through its own webhook and needs none.
fn github_connection_needs_repo_webhook(provider_type: &str, auth_method: &str) -> bool {
    provider_type == "github" && auth_method != "github_app"
}

/// Whether a project's Git connection or repository differs between `old`
/// and `new` (`(connection_id, repo_owner, repo_name)`), so a repository
/// webhook registered for `old` must be replaced.
pub(in crate::services::project) fn repository_target_changed(
    old: (Option<i32>, &str, &str),
    new: (Option<i32>, &str, &str),
) -> bool {
    old != new
}

impl ProjectService {
    // ── GitHub repository webhook install / delete ──────────────────────────────
    //
    // A GitHub App delivers every push through the App's own webhook. A GitHub
    // connection authenticated any other way (personal access token, OAuth)
    // has no App, so without a repository webhook nothing ever told Temps
    // about a push and auto-deploy silently did nothing (#1596). For those
    // connections we register a repository webhook signed with a fresh
    // per-project secret, stored encrypted together with the hook ID so the
    // hook can be deleted when the project's repository changes.
    //
    // Failures are non-fatal — same policy as GitLab/Gitea/Bitbucket.

    /// Resolve `connection_id` to `(provider_id, access_token)` when it is a
    /// GitHub connection that needs its own repository webhook, i.e. any
    /// GitHub connection that is not a GitHub App. `Ok(None)` for App
    /// connections and other providers (caller skips).
    async fn resolve_github_repo_webhook_connection(
        &self,
        connection_id: i32,
    ) -> Result<Option<(i32, String)>, String> {
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

        if !github_connection_needs_repo_webhook(&provider.provider_type, &provider.auth_method) {
            return Ok(None);
        }

        let access_token = self
            .git_provider_manager
            .get_connection_token(connection_id)
            .await
            .map_err(|e| {
                format!("Failed to get access token for GitHub connection {connection_id}: {e}")
            })?;

        Ok(Some((provider.id, access_token)))
    }

    /// Register a GitHub repository webhook for `project_id` when
    /// `connection_id` is a non-App GitHub connection.
    ///
    /// With `existing_hook_id`, that hook is given the new secret in place and
    /// keeps its ID; a new hook is registered only when it no longer exists on
    /// GitHub. GitHub refuses a second hook with the same URL and events, so
    /// replacing a hook by creating its successor first would always fail.
    ///
    /// Returns `Ok(Some((hook_id, encrypted_secret)))` on success, `Ok(None)`
    /// when the connection does not need one, `Err` on failure (non-fatal —
    /// the caller logs and continues).
    pub(in crate::services::project) async fn install_github_webhook_for_connection(
        &self,
        project_id: i32,
        connection_id: i32,
        owner: &str,
        repo: &str,
        existing_hook_id: Option<i64>,
    ) -> Result<Option<(i64, String)>, String> {
        use temps_git::services::git_provider::WebhookConfig;
        use temps_git::services::github_provider::generate_github_webhook_secret;

        let Some((provider_id, access_token)) = self
            .resolve_github_repo_webhook_connection(connection_id)
            .await?
        else {
            return Ok(None);
        };

        let secret = generate_github_webhook_secret()
            .map_err(|error| format!("Failed to generate GitHub webhook secret: {error}"))?;

        let config = WebhookConfig {
            url: github_repo_webhook_url(&self.git_webhook_base_url().await, project_id),
            secret: Some(secret.clone()),
            events: vec!["push".to_string()],
        };

        let provider_service = self
            .git_provider_manager
            .get_provider_service(provider_id)
            .await
            .map_err(|e| {
                format!("Failed to get provider service for GitHub connection {connection_id}: {e}")
            })?;

        let updated_hook_id = match existing_hook_id {
            Some(hook_id) => {
                let updated = provider_service
                    .update_webhook(
                        &access_token,
                        owner,
                        repo,
                        &hook_id.to_string(),
                        config.clone(),
                    )
                    .await
                    .map_err(|e| {
                        format!(
                            "Failed to update GitHub webhook {hook_id} for project {project_id} ({owner}/{repo}): {e}"
                        )
                    })?;
                if !updated {
                    info!(
                        "GitHub webhook {} for project {} no longer exists on {}/{}; registering a new one",
                        hook_id, project_id, owner, repo
                    );
                }
                updated.then_some(hook_id)
            }
            None => None,
        };

        let hook_id = match updated_hook_id {
            Some(hook_id) => hook_id,
            None => {
                let hook_id = provider_service
                    .create_webhook(&access_token, owner, repo, config)
                    .await
                    .map_err(|e| {
                        format!(
                            "Failed to register GitHub webhook for project {project_id} ({owner}/{repo}): {e}"
                        )
                    })?;
                hook_id.parse().map_err(|e| {
                    format!(
                        "GitHub returned a non-numeric webhook ID '{hook_id}' for project {project_id} ({owner}/{repo}): {e}"
                    )
                })?
            }
        };

        let encrypted_secret = self
            .encryption_service
            .encrypt_string(&secret)
            .map_err(|e| format!("Failed to encrypt GitHub webhook secret: {e}"))?;

        info!(
            "Installed GitHub repository webhook {} for project {} ({}/{})",
            hook_id, project_id, owner, repo
        );

        Ok(Some((hook_id, encrypted_secret)))
    }

    /// Delete a previously registered GitHub repository webhook. Best-effort.
    pub(in crate::services::project) async fn delete_github_webhook_for_connection(
        &self,
        connection_id: i32,
        owner: &str,
        repo: &str,
        hook_id: i64,
    ) -> Result<(), String> {
        let Some((provider_id, access_token)) = self
            .resolve_github_repo_webhook_connection(connection_id)
            .await?
        else {
            // No longer a non-App GitHub connection — nothing we can delete with.
            return Ok(());
        };

        let provider_service = self
            .git_provider_manager
            .get_provider_service(provider_id)
            .await
            .map_err(|e| {
                format!("Failed to get provider service for GitHub connection {connection_id}: {e}")
            })?;

        provider_service
            .delete_webhook(&access_token, owner, repo, &hook_id.to_string())
            .await
            .map_err(|e| format!("GitHub delete webhook {hook_id} error for {owner}/{repo}: {e}"))
    }

    /// Install (or replace) the GitHub repository webhook for a project on a
    /// non-App GitHub connection. Called by
    /// `POST /projects/{id}/github/reinstall-webhook`, for when automatic
    /// registration failed (e.g. the token lacked webhook permission) or the
    /// hook was deleted on GitHub.
    pub async fn reinstall_github_webhook(&self, project_id: i32) -> Result<i64, ProjectError> {
        let project = projects::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(ProjectError::NotFound(format!(
                "Project {} not found",
                project_id
            )))?;

        let connection_id = project.git_provider_connection_id.ok_or_else(|| {
            ProjectError::InvalidInput(format!(
                "Project {} has no git provider connection, so there is no repository to install a GitHub webhook on",
                project_id
            ))
        })?;

        let owner = project.repo_owner.clone();
        let repo = project.repo_name.clone();
        if owner.is_empty() || repo.is_empty() {
            return Err(ProjectError::InvalidInput(format!(
                "Project {} has no repository configured, so there is nothing to install a GitHub webhook on",
                project_id
            )));
        }

        // The stored hook is updated in place (GitHub refuses a second hook with
        // the same URL); a new one is registered only if it was deleted on
        // GitHub. Nothing is persisted unless GitHub accepted the change, so a
        // failure leaves the project with the hook and secret it had.
        let (hook_id, encrypted_secret) = self
            .install_github_webhook_for_connection(
                project_id,
                connection_id,
                &owner,
                &repo,
                project.github_webhook_id,
            )
            .await
            .map_err(ProjectError::GitHubError)?
            .ok_or_else(|| {
                ProjectError::InvalidInput(format!(
                    "Project {} is not connected through a GitHub personal access token or OAuth connection (connection {}). GitHub App connections receive pushes through the App webhook and need no repository webhook",
                    project_id, connection_id
                ))
            })?;

        let mut active_project: projects::ActiveModel = project.into();
        active_project.github_webhook_id = Set(Some(hook_id));
        active_project.github_webhook_signing_token = Set(Some(encrypted_secret));
        active_project.updated_at = Set(chrono::Utc::now());
        active_project.update(self.db.as_ref()).await?;

        info!(
            "Reinstalled GitHub repository webhook {} for project {}",
            hook_id, project_id
        );

        Ok(hook_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // #1596: a GitHub PAT/OAuth connection has no App webhook, so its
    // projects need a repository webhook; an App connection must not get one
    // (the push would arrive twice), and other providers have their own flow.
    #[test]
    fn only_non_app_github_connections_get_a_repository_webhook() {
        assert!(github_connection_needs_repo_webhook("github", "pat"));
        assert!(github_connection_needs_repo_webhook("github", "oauth"));
        assert!(!github_connection_needs_repo_webhook(
            "github",
            "github_app"
        ));
        assert!(!github_connection_needs_repo_webhook("gitlab", "pat"));
        assert!(!github_connection_needs_repo_webhook("gitea", "pat"));
        assert!(!github_connection_needs_repo_webhook("bitbucket", "pat"));
    }

    #[test]
    fn repository_webhook_is_replaced_only_when_the_repository_changes() {
        let current = (Some(7), "acme", "web");
        // A branch, preset or directory save resends the same connection.
        assert!(!repository_target_changed(
            current,
            (Some(7), "acme", "web")
        ));
        assert!(repository_target_changed(current, (Some(8), "acme", "web")));
        assert!(repository_target_changed(
            current,
            (Some(7), "other", "web")
        ));
        assert!(repository_target_changed(current, (Some(7), "acme", "api")));
        // Disconnecting removes the hook.
        assert!(repository_target_changed(current, (None, "acme", "web")));
    }

    /// GitHub refuses a second hook with the same URL and events, so two
    /// projects on one repository each need a hook URL of their own.
    #[test]
    fn each_project_gets_its_own_github_webhook_url() {
        let first = github_repo_webhook_url("https://temps.example.com", 7);
        let second = github_repo_webhook_url("https://temps.example.com", 8);

        assert_eq!(
            first,
            "https://temps.example.com/api/webhook/git/github/repo-events?project_id=7"
        );
        assert_ne!(first, second);
    }
}
