// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Git source updates for a project.

use super::*;

/// Whether changing `repo_owner`/`repo_name` would leave `git_url` pointing at
/// a different repository.
///
/// Returns `Some((old, new))` — both as `owner/name` — only when the stored URL
/// demonstrably identifies the *current* repo and the requested change moves
/// away from it. A URL that doesn't carry a recognisable `owner/name` tail
/// (self-hosted layouts, ssh remotes with unusual paths) returns `None`: we
/// can't prove a desync, so we don't block the operator.
pub(super) fn would_desync_git_url(
    git_url: &Option<String>,
    current: (&str, &str),
    requested: (Option<&str>, Option<&str>),
) -> Option<(String, String)> {
    let (new_owner, new_name) = (
        requested.0.unwrap_or(current.0),
        requested.1.unwrap_or(current.1),
    );
    let old_pair = format!("{}/{}", current.0, current.1);
    let new_pair = format!("{}/{}", new_owner, new_name);
    if old_pair == new_pair {
        return None;
    }

    let url = git_url.as_deref()?;
    // Compare on the `owner/name` tail, ignoring a `.git` suffix and any
    // trailing slash, so https/ssh and with/without `.git` all match.
    let tail = url
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .rsplit(['/', ':'])
        .take(2)
        .collect::<Vec<_>>();
    if tail.len() < 2 {
        return None;
    }
    let url_pair = format!("{}/{}", tail[1], tail[0]);

    (url_pair.eq_ignore_ascii_case(&old_pair)).then_some((url_pair, new_pair))
}

impl ProjectService {
    #[allow(clippy::too_many_arguments)]
    pub async fn update_git_settings(
        &self,
        project_id: i32,
        git_provider_connection_id: Option<i32>,
        main_branch: String,
        repo_owner: String,
        repo_name: String,
        preset: Option<String>,
        directory: String,
        preset_config: Option<serde_json::Value>,
        git_url: Option<String>,
        is_public_repo: Option<bool>,
        pull_only_root_directory: Option<bool>,
        caller: DeployCaller,
    ) -> Result<Project, ProjectError> {
        // Get the current project (includes the old gitlab_webhook_id / signing_token)
        let project = projects::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(ProjectError::NotFound(format!(
                "Project {} not found",
                project_id
            )))?;
        // ADR 045: the other half of the source-definition gate (see
        // `update_project_settings_as`). This endpoint owns `git_url` — the
        // URL the deployment actually clones — and always rewrites the owner,
        // name, branch and directory, so on a declared project every call
        // decides what the next git push builds and runs as host root.
        self.guard_granted_project_write(&project.slug, "the source repository", caller)?;
        if project.project_type == ProjectType::Service {
            return Err(ProjectError::InvalidInput(
                "A service project cannot be converted to a Git source; update its applied template or runtime instead"
                    .to_string(),
            ));
        }

        // Snapshot fields we need to reason about the old/new repo transition.
        let old_connection_id = project.git_provider_connection_id;
        let old_repo_owner = project.repo_owner.clone();
        let old_repo_name = project.repo_name.clone();
        let old_gitlab_webhook_id = project.gitlab_webhook_id;
        let old_bitbucket_hook_id = project.bitbucket_webhook_hook_id.clone();
        let old_github_webhook_id = project.github_webhook_id;
        // Gitea: no separate hook_id column — we clear gitea_webhook_signing_token
        // on repo change / disconnect. The orphaned remote hook on Gitea is
        // acceptable for v1 (no stored ID to call DELETE with).
        // Generic: no remote API at all — just regenerate the token.

        // Verify git provider connection if provided.
        //
        // Connections are scoped to the installation (workspace-wide), not to
        // the user who created them: a GitHub App installation is shared by
        // design, and PAT connections are intended to be usable by anyone
        // with write access to the project, not just their creator. Access
        // control for this endpoint is enforced by `permission_guard!` and
        // `project_scope_guard!` in the handler, not by connection ownership.
        if let Some(connection_id) = git_provider_connection_id {
            if connection_id > 0 {
                use temps_entities::git_provider_connections;
                let connection = git_provider_connections::Entity::find_by_id(connection_id)
                    .one(self.db.as_ref())
                    .await?
                    .ok_or(ProjectError::GitProviderConnectionNotFound { connection_id })?;

                if !connection.is_active {
                    return Err(ProjectError::Other(format!(
                        "Git provider connection {} is not active",
                        connection_id
                    )));
                }

                // Verify that the branch exists in the repository
                match self
                    .git_provider_manager
                    .get_branch_latest_commit(connection_id, &repo_owner, &repo_name, &main_branch)
                    .await
                {
                    Ok(_) => {
                        // Branch exists and we got its latest commit
                    }
                    Err(e) => {
                        return Err(ProjectError::GitHubError(format!(
                            "Branch '{}' does not exist in repository {}/{} or failed to verify: {}",
                            main_branch, repo_owner, repo_name, e
                        )));
                    }
                }
            }
        }

        // The branch exists; now the root directory on it (issue #1350).
        // This endpoint always makes the project a Git source, so the check
        // applies whatever the project was before.
        {
            let normalized_directory = normalize_project_directory(&directory)?;
            let target = SourceDirectoryTarget {
                connection_id: match git_provider_connection_id {
                    Some(id) if id > 0 => Some(id),
                    Some(_) => None,
                    None => old_connection_id,
                },
                is_public_repo: is_public_repo.unwrap_or(project.is_public_repo),
                git_url: git_url.as_deref().or(project.git_url.as_deref()),
                repo_owner: &repo_owner,
                repo_name: &repo_name,
                branch: &main_branch,
                directory: &normalized_directory,
            };
            if target.differs_from(&project)
                || project.source_type != temps_entities::source_type::SourceType::Git
            {
                self.verify_root_directory_in_repository(Some(project_id), target)
                    .await?;
            }
        }

        // Capture the current preset/config before converting to ActiveModel
        let project_preset = project.preset;
        let existing_preset_config = project.preset_config.clone();
        let existing_pull_only_root_directory = project.pull_only_root_directory;

        // Update the project
        let mut active_project: projects::ActiveModel = project.into();
        active_project.main_branch = Set(main_branch.clone());
        active_project.repo_owner = Set(repo_owner.clone());
        active_project.repo_name = Set(repo_name.clone());
        let normalized_directory = normalize_project_directory(&directory)?;
        active_project.directory = Set(normalized_directory.clone());
        active_project.pull_only_root_directory = Set(pull_only_root_directory_value(
            &normalized_directory,
            pull_only_root_directory,
            existing_pull_only_root_directory,
        ));
        // Configuring a Git repository makes this a Git-source project — this is
        // how a docker_image / static_files project is converted to Git (the
        // reverse conversion goes through `set_source_type`).
        active_project.source_type = Set(temps_entities::source_type::SourceType::Git);

        // Reject invalid settings before any provider-side webhook changes.
        // Re-resolve under the row lock below; this snapshot can become stale.
        resolve_preset_patch(
            project_preset,
            existing_preset_config.as_ref(),
            preset.as_deref(),
            preset_config.as_ref(),
        )?;

        // Determine the effective new connection id and whether we need to handle
        // webhook lifecycle.  Three cases:
        //   1. connection_id provided and > 0 → connecting / changing repo
        //   2. connection_id provided and == 0 → disconnecting
        //   3. connection_id not provided → no change
        let new_connection_id: Option<i32> = match git_provider_connection_id {
            Some(cid) if cid > 0 => {
                active_project.git_provider_connection_id = Set(Some(cid));
                Some(cid)
            }
            Some(_) => {
                // Explicit disconnect (connection_id == 0)
                active_project.git_provider_connection_id = Set(None);
                None
            }
            None => {
                // No change requested — carry existing connection forward.
                old_connection_id
            }
        };

        if let Some(ref url) = git_url {
            // SSRF guard: validate before persisting (Fix #12).
            validate_git_url(url).map_err(|e| ProjectError::InvalidGitUrl {
                url: redact_url_password(url),
                reason: e.to_string(),
            })?;
            active_project.git_url = Set(Some(url.clone()));
        }

        if let Some(is_public) = is_public_repo {
            active_project.is_public_repo = Set(is_public);
        }

        // ── GitLab webhook lifecycle ──────────────────────────────────────────
        //
        // We detect a "repo change" when either the connection or the repo path
        // differs from what was previously stored.  A change triggers:
        //   • Delete the old webhook from GitLab (best-effort, idempotent on 404).
        //   • Install a new webhook on the new repo (GitLab connections only).
        //
        // Failures here are non-fatal: we log warnings and continue so that the
        // project save always succeeds.

        let repo_changed = git_provider_connection_id.is_some()
            || repo_owner != old_repo_owner
            || repo_name != old_repo_name;

        if repo_changed {
            // Step 1: Remove old webhook if the old connection was GitLab.
            if let (Some(old_hook_id), Some(old_conn_id)) =
                (old_gitlab_webhook_id, old_connection_id)
            {
                if let Err(e) = self
                    .delete_gitlab_webhook_for_connection(
                        old_conn_id,
                        &old_repo_owner,
                        &old_repo_name,
                        old_hook_id,
                    )
                    .await
                {
                    warn!(
                        "Failed to remove old GitLab webhook {} for project {}: {}",
                        old_hook_id, project_id, e
                    );
                }
                // Clear stale hook fields unconditionally — even if delete failed
                // (it may already be gone on GitLab's side).
                active_project.gitlab_webhook_id = Set(None);
                active_project.gitlab_webhook_signing_token = Set(None);
            }

            // Step 2: Install a new webhook if the new connection is GitLab.
            if let Some(conn_id) = new_connection_id {
                match self
                    .install_gitlab_webhook_for_connection(
                        project_id,
                        conn_id,
                        &repo_owner,
                        &repo_name,
                    )
                    .await
                {
                    Ok((hook_id, encrypted_token)) => {
                        active_project.gitlab_webhook_id = Set(Some(hook_id as i32));
                        active_project.gitlab_webhook_signing_token = Set(Some(encrypted_token));
                    }
                    Err(e) => {
                        // Non-fatal: the project connects without the webhook.
                        warn!(
                            "Failed to install GitLab webhook for project {}: {}",
                            project_id, e
                        );
                        active_project.gitlab_webhook_id = Set(None);
                        active_project.gitlab_webhook_signing_token = Set(None);
                    }
                }
            }
        } else if git_provider_connection_id == Some(0) {
            // Explicit disconnect: clear webhook state.
            active_project.gitlab_webhook_id = Set(None);
            active_project.gitlab_webhook_signing_token = Set(None);
        }

        // ── Bitbucket Cloud webhook lifecycle ────────────────────────────────
        //
        // Mirrors the GitLab block above: delete the old hook when the repo
        // changes and install a new one on the new repo (best-effort, non-fatal).

        if repo_changed {
            // Step 1: Remove old webhook if the old connection was Bitbucket.
            if let (Some(old_hook_uuid), Some(old_conn_id)) =
                (old_bitbucket_hook_id.as_deref(), old_connection_id)
            {
                if let Err(e) = self
                    .delete_bitbucket_webhook_for_connection(
                        old_conn_id,
                        &old_repo_owner,
                        &old_repo_name,
                        old_hook_uuid,
                    )
                    .await
                {
                    warn!(
                        "Failed to remove old Bitbucket webhook {} for project {}: {}",
                        old_hook_uuid, project_id, e
                    );
                }
                // Clear stale hook fields unconditionally.
                active_project.bitbucket_webhook_hook_id = Set(None);
                active_project.bitbucket_webhook_token = Set(None);
            }

            // Step 2: Install a new webhook if the new connection is Bitbucket.
            if let Some(conn_id) = new_connection_id {
                match self
                    .install_bitbucket_webhook_for_connection(
                        project_id,
                        conn_id,
                        &repo_owner,
                        &repo_name,
                    )
                    .await
                {
                    Ok((hook_uuid, encrypted_token)) => {
                        active_project.bitbucket_webhook_hook_id = Set(Some(hook_uuid));
                        active_project.bitbucket_webhook_token = Set(Some(encrypted_token));
                    }
                    Err(e) => {
                        // Non-fatal: project connects without the webhook.
                        warn!(
                            "Failed to install Bitbucket webhook for project {}: {}",
                            project_id, e
                        );
                        active_project.bitbucket_webhook_hook_id = Set(None);
                        active_project.bitbucket_webhook_token = Set(None);
                    }
                }
            }
        } else if git_provider_connection_id == Some(0) {
            // Explicit disconnect: clear Bitbucket webhook state.
            active_project.bitbucket_webhook_hook_id = Set(None);
            active_project.bitbucket_webhook_token = Set(None);
        }

        // ── GitHub repository webhook lifecycle ─────────────────────────────
        //
        // Mirrors the GitLab block above for non-App GitHub connections:
        // delete the old repository hook when the repo changes and register
        // one on the new repo (best-effort, non-fatal).
        //
        // Unlike `repo_changed`, this compares against the stored values: the
        // console sends `git_provider_connection_id` with every Git settings
        // save (branch, preset, directory), and replacing a working hook on
        // each of those would rotate its secret and, if the new registration
        // failed, silently stop push deploys over an unrelated edit.
        let github_repo_changed = repository_target_changed(
            (old_connection_id, &old_repo_owner, &old_repo_name),
            (new_connection_id, &repo_owner, &repo_name),
        );

        if github_repo_changed {
            if let (Some(old_hook_id), Some(old_conn_id)) =
                (old_github_webhook_id, old_connection_id)
            {
                if let Err(e) = self
                    .delete_github_webhook_for_connection(
                        old_conn_id,
                        &old_repo_owner,
                        &old_repo_name,
                        old_hook_id,
                    )
                    .await
                {
                    warn!(
                        "Failed to remove old GitHub webhook {} for project {}: {}",
                        old_hook_id, project_id, e
                    );
                }
            }
            // Clear stale hook fields unconditionally — even if delete failed
            // (it may already be gone on GitHub's side).
            active_project.github_webhook_id = Set(None);
            active_project.github_webhook_signing_token = Set(None);

            if let Some(conn_id) = new_connection_id {
                match self
                    .install_github_webhook_for_connection(
                        project_id,
                        conn_id,
                        &repo_owner,
                        &repo_name,
                        None,
                    )
                    .await
                {
                    Ok(Some((hook_id, encrypted_secret))) => {
                        active_project.github_webhook_id = Set(Some(hook_id));
                        active_project.github_webhook_signing_token = Set(Some(encrypted_secret));
                    }
                    Ok(None) => {
                        // Not a non-App GitHub connection — nothing to do.
                    }
                    Err(e) => {
                        warn!(
                            "Failed to install GitHub webhook for project {}: {}",
                            project_id, e
                        );
                    }
                }
            }
        } else if git_provider_connection_id == Some(0) {
            // Explicit disconnect: clear GitHub webhook state.
            active_project.github_webhook_id = Set(None);
            active_project.github_webhook_signing_token = Set(None);
        }

        // ── Gitea webhook lifecycle ──────────────────────────────────────────
        //
        // BLOCKER 1 fix: gitea_webhook_signing_token is now written here.
        // There is no stored hook_id for remote deletion in v1 — on repo
        // change we clear the token (orphaning the remote hook) and re-install.
        // On explicit disconnect we clear the token.

        if repo_changed {
            // Clear the old Gitea signing token unconditionally.
            active_project.gitea_webhook_signing_token = Set(None);

            // Install a new webhook if the new connection is Gitea.
            if let Some(conn_id) = new_connection_id {
                match self
                    .install_gitea_webhook_for_connection(
                        project_id,
                        conn_id,
                        &repo_owner,
                        &repo_name,
                    )
                    .await
                {
                    Ok(encrypted_token) => {
                        active_project.gitea_webhook_signing_token = Set(Some(encrypted_token));
                    }
                    Err(e) => {
                        warn!(
                            "Failed to install Gitea webhook for project {}: {}",
                            project_id, e
                        );
                        active_project.gitea_webhook_signing_token = Set(None);
                    }
                }
            }
        } else if git_provider_connection_id == Some(0) {
            // Explicit disconnect: clear Gitea webhook state.
            active_project.gitea_webhook_signing_token = Set(None);
        }

        // ── Generic webhook token lifecycle ─────────────────────────────────
        //
        // BLOCKER 2 fix: generic_webhook_token is now written here.
        // Generic has no remote API to deregister; we simply regenerate.

        if repo_changed {
            // Clear the old token.
            active_project.generic_webhook_token = Set(None);

            if let Some(conn_id) = new_connection_id {
                match self
                    .install_generic_webhook_token(project_id, conn_id)
                    .await
                {
                    Ok(Some(encrypted_token)) => {
                        active_project.generic_webhook_token = Set(Some(encrypted_token));
                    }
                    Ok(None) => {
                        // Not a Generic connection — nothing to do.
                    }
                    Err(e) => {
                        warn!(
                            "Failed to install Generic webhook token for project {}: {}",
                            project_id, e
                        );
                        active_project.generic_webhook_token = Set(None);
                    }
                }
            }
        } else if git_provider_connection_id == Some(0) {
            // Explicit disconnect: clear Generic webhook state.
            active_project.generic_webhook_token = Set(None);
        }

        // Same rollback-together reasoning as `update_project_settings`: publish
        // the reload on the transaction that carries the write, so a failure to
        // signal leaves nothing persisted.
        let txn = self.db.begin().await?;
        let locked_project = projects::Entity::find_by_id(project_id)
            .lock_exclusive()
            .one(&txn)
            .await?
            .ok_or_else(|| ProjectError::NotFound(format!("Project {project_id} not found")))?;
        self.guard_granted_project_write(&locked_project.slug, "the source repository", caller)?;
        let previous_public_ports = compose_public_ports(locked_project.preset_config.as_ref());
        if locked_project.project_type == ProjectType::Service {
            return Err(ProjectError::InvalidInput(
                "A service project cannot be converted to a Git source; update its applied template or runtime instead"
                    .to_string(),
            ));
        }
        if let Some(resolved) = resolve_preset_patch(
            locked_project.preset,
            locked_project.preset_config.as_ref(),
            preset.as_deref(),
            preset_config.as_ref(),
        )? {
            apply_resolved_preset(&mut active_project, resolved)?;
        }
        let enable_hosting =
            locked_project.source_type == temps_entities::source_type::SourceType::External;
        let updated_project = active_project.update(&txn).await?;
        if enable_hosting {
            self.activate_hosting_domains(&txn, project_id).await?;
            // Telemetry environments start without a repository. Attach the
            // production environment to the selected branch when hosting is added.
            environments::Entity::update_many()
                .col_expr(
                    environments::Column::Branch,
                    sea_orm::sea_query::Expr::value(Some(main_branch)),
                )
                .filter(environments::Column::ProjectId.eq(project_id))
                .filter(environments::Column::Name.eq("production"))
                .filter(environments::Column::DeletedAt.is_null())
                .exec(&txn)
                .await?;
        }

        let ports_changed =
            previous_public_ports != compose_public_ports(updated_project.preset_config.as_ref());
        if ports_changed {
            // Every *database* write in this method rides `txn`. The provider-side
            // webhook install/removal above does not, and nothing here undoes
            // it — so do not claim a total rollback.
            self.publish_route_reload(
                &txn,
                project_id,
                "No database changes were saved, but any git-provider webhook this request \
                 installed or removed has already been applied at the provider.",
            )
            .await?;
        }
        txn.commit().await?;
        if enable_hosting {
            self.notify_hosting_enabled(project_id).await;
        }
        if ports_changed {
            self.enqueue_route_reload(project_id).await;
        }

        Ok(self.map_written_project(updated_project).await)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── git_url / repo identity consistency ─────────────────────────────

    /// The failure this guards: the repo identity was changed through
    /// `/settings`, the clone URL was left behind, and the next deploy
    /// resolved a commit from the new repo while cloning the old one.
    #[test]
    fn test_repo_change_that_strands_git_url_is_detected() {
        let url = Some("https://github.com/acme/old-repo.git".to_string());
        let desync = would_desync_git_url(&url, ("acme", "old-repo"), (None, Some("new-repo")));

        let (old, new) = desync.expect("changing the name strands the URL");
        assert_eq!(old, "acme/old-repo");
        assert_eq!(new, "acme/new-repo");
    }

    /// Changing the owner counts too.
    #[test]
    fn test_owner_change_is_detected() {
        let url = Some("https://github.com/acme/app.git".to_string());
        assert!(would_desync_git_url(&url, ("acme", "app"), (Some("other"), None)).is_some());
    }

    /// No repo change means nothing to desync, whatever the URL looks like.
    #[test]
    fn test_no_repo_change_is_allowed() {
        let url = Some("https://github.com/acme/app.git".to_string());
        assert!(would_desync_git_url(&url, ("acme", "app"), (None, None)).is_none());
        assert!(
            would_desync_git_url(&url, ("acme", "app"), (Some("acme"), Some("app"))).is_none(),
            "restating the same values is not a change"
        );
    }

    /// A URL that doesn't identify the current repo can't be proven stale, so
    /// the operator isn't blocked — self-hosted layouts and unusual remotes
    /// must keep working.
    #[test]
    fn test_unrelated_or_unparsable_url_does_not_block() {
        // Points somewhere that isn't the current repo: not our call to make.
        let other = Some("https://git.internal/mirrors/vendored.git".to_string());
        assert!(would_desync_git_url(&other, ("acme", "app"), (None, Some("app2"))).is_none());

        // No URL at all.
        assert!(would_desync_git_url(&None, ("acme", "app"), (None, Some("app2"))).is_none());
    }

    /// ssh remotes and missing `.git` must match the same way https does,
    /// or the guard would fire on projects it shouldn't.
    #[test]
    fn test_matching_is_scheme_and_suffix_insensitive() {
        for url in [
            "git@github.com:acme/app.git",
            "https://github.com/acme/app",
            "https://github.com/acme/app/",
            "https://github.com/ACME/App.git",
        ] {
            assert!(
                would_desync_git_url(
                    &Some(url.to_string()),
                    ("acme", "app"),
                    (None, Some("app2"))
                )
                .is_some(),
                "should recognise {url} as the current repo"
            );
        }
    }
}
