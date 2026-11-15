// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Manual pipeline triggers.

use super::*;

/// Why a manual trigger could not resolve its ref in a public repository.
enum PublicTriggerRefError {
    /// The repository has no such branch, tag or commit; carries the
    /// user-facing explanation.
    Missing(String),
    /// The provider could not answer (rate limit, outage, not supported).
    Provider(PublicRepoError),
}

/// Resolve what a manual trigger deploys in a public repository to the commit
/// it points at.
///
/// An explicit `tag` is looked up as a tag only, so a branch of the same name
/// can never answer for it, and the deployment gets that tag's commit rather
/// than the branch tip. Without a tag, `reference` (the branch field) is
/// resolved as a branch, then a tag, then a full or abbreviated commit id,
/// because group repositories and templates pin a release tag or a commit in
/// the same field.
async fn resolve_public_trigger_ref(
    provider: &dyn temps_git::services::public_repo::PublicRepoProvider,
    owner: &str,
    repo: &str,
    reference: &str,
    tag: Option<&str>,
) -> Result<ResolvedPublicRef, PublicTriggerRefError> {
    let (lookup, missing) = match tag {
        Some(tag) => (
            format!("refs/tags/{tag}"),
            format!("Tag '{tag}' not found in public repo {owner}/{repo}"),
        ),
        None => (
            reference.to_string(),
            format!("'{reference}' is not a branch, tag or commit of public repo {owner}/{repo}"),
        ),
    };
    provider
        .resolve_ref(owner, repo, &lookup)
        .await
        .map_err(PublicTriggerRefError::Provider)?
        .ok_or(PublicTriggerRefError::Missing(missing))
}

impl ProjectService {
    /// Trigger a pipeline for a specific project and environment
    pub async fn trigger_pipeline(
        &self,
        project_id: i32,
        environment_id: i32,
        branch: Option<String>,
        tag: Option<String>,
        commit: Option<String>,
    ) -> Result<(i32, i32, Option<String>, Option<String>, Option<String>), ProjectError> {
        self.trigger_pipeline_as(
            project_id,
            environment_id,
            branch,
            tag,
            commit,
            DeployCaller::default(),
            None,
        )
        .await
    }

    /// [`Self::trigger_pipeline`] for a caller whose authority is known.
    ///
    /// ADR 045: deploying a project that holds host Docker access runs the
    /// deployed image and command as host root, so it is admin-only — deploy
    /// permission on the project is not sufficient. Every other project is
    /// unaffected.
    #[allow(clippy::too_many_arguments)]
    pub async fn trigger_pipeline_as(
        &self,
        project_id: i32,
        environment_id: i32,
        branch: Option<String>,
        tag: Option<String>,
        commit: Option<String>,
        caller: DeployCaller,
        github_user_id: Option<i32>,
    ) -> Result<(i32, i32, Option<String>, Option<String>, Option<String>), ProjectError> {
        // Get the project to validate it exists and get repository information
        let project = temps_entities::projects::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await
            .map_err(|e| ProjectError::Other(e.to_string()))?
            .ok_or_else(|| ProjectError::NotFound("Project not found".to_string()))?;
        self.guard_granted_project_deploy(&project.slug, caller)?;

        // Validate environment belongs to this project and is not soft-deleted
        let environment = temps_entities::environments::Entity::find_by_id(environment_id)
            .filter(temps_entities::environments::Column::ProjectId.eq(project_id))
            .filter(temps_entities::environments::Column::DeletedAt.is_null())
            .one(self.db.as_ref())
            .await
            .map_err(|e| ProjectError::Other(e.to_string()))?
            .ok_or_else(|| {
                ProjectError::NotFound(
                    "Environment not found or doesn't belong to project".to_string(),
                )
            })?;

        // Validate project has repository information
        if project.repo_owner.is_empty() || project.repo_name.is_empty() {
            return Err(ProjectError::InvalidInput(
                "Project must have repository information to trigger pipeline".to_string(),
            ));
        }

        // Use provided branch/commit or fall back to project defaults
        let branch_to_use = branch.unwrap_or(project.main_branch.clone());

        // An empty tag is no tag. A tag names the commit to deploy: it is
        // resolved to that commit below, never ignored in favour of the
        // branch tip.
        let mut tag = tag.filter(|tag| !tag.trim().is_empty());

        // Fetch the latest commit from the branch if no commit was provided
        let commit_to_use = if let Some(commit) = commit {
            commit
        } else if let Some(connection_id) = project.git_provider_connection_id {
            // The ref to resolve: the tag when one was asked for, otherwise
            // the branch. Providers resolve a branch, tag or commit id here.
            let lookup_ref = tag.as_deref().unwrap_or(&branch_to_use);
            // Fetch latest commit from the ref using authenticated git provider
            match self
                .git_provider_manager
                .get_branch_latest_commit(
                    connection_id,
                    &project.repo_owner,
                    &project.repo_name,
                    lookup_ref,
                )
                .await
            {
                Ok(commit_info) => {
                    info!(
                        "Fetched latest commit from ref {}: {} ({})",
                        lookup_ref, commit_info.sha, commit_info.message
                    );
                    commit_info.sha
                }
                Err(e) => {
                    warn!(
                        "Failed to fetch latest commit from ref {}: {}, using placeholder",
                        lookup_ref, e
                    );
                    format!("manual-trigger-{}", chrono::Utc::now().timestamp())
                }
            }
        } else if project.is_public_repo {
            // For public repos without git provider connection, fetch from public API
            let provider_name = if let Some(ref git_url) = project.git_url {
                if git_url.contains("github.com") {
                    "github"
                } else if git_url.contains("gitlab.com") {
                    "gitlab"
                } else {
                    return Err(ProjectError::InvalidInput(format!(
                        "Unknown git provider for public repo URL: {}. Only GitHub and GitLab public repos are supported.",
                        git_url
                    )));
                }
            } else {
                // No git_url, try to infer from repo structure (assume GitHub for public repos)
                "github"
            };

            // Use only the requesting user's GitHub credential. A provider
            // configured by another user cannot be borrowed by this project.
            let token = if provider_name == "github" {
                match github_user_id {
                    Some(user_id) => {
                        self.git_provider_manager
                            .get_valid_github_token_for_user(user_id)
                            .await
                    }
                    None => None,
                }
            } else {
                None
            };
            let provider = PublicRepoProviderFactory::create_with_token(provider_name, token)
                .map_err(|e| {
                    ProjectError::Other(format!(
                        "Failed to create public repo provider for {}: {}",
                        provider_name, e
                    ))
                })?;

            // A shared credential may be able to see private repositories.
            // `is_public_repo` must never turn that credential into a private
            // repository oracle, so verify visibility before reading branches.
            provider
                .get_repository(&project.repo_owner, &project.repo_name)
                .await
                .map_err(|e| match e {
                    PublicRepoError::RateLimitExceeded => ProjectError::PublicRepoRateLimited {
                        project_id,
                        project_slug: project.slug.clone(),
                        provider: provider_name.to_string(),
                    },
                    e => ProjectError::Other(format!(
                        "Failed to verify that repository {}/{} is public: {}",
                        project.repo_owner, project.repo_name, e
                    )),
                })?;

            // The tag when one was asked for, otherwise the branch field,
            // which may hold a tag or a commit id (group repositories and
            // templates pin either). One lookup per kind by name: listing
            // every branch stops at a safety cap and reports existing
            // branches of large repositories as missing.
            let resolved = resolve_public_trigger_ref(
                provider.as_ref(),
                &project.repo_owner,
                &project.repo_name,
                &branch_to_use,
                tag.as_deref(),
            )
            .await
            .map_err(|e| match e {
                PublicTriggerRefError::Provider(PublicRepoError::RateLimitExceeded) => {
                    ProjectError::PublicRepoRateLimited {
                        project_id,
                        project_slug: project.slug.clone(),
                        provider: provider_name.to_string(),
                    }
                }
                PublicTriggerRefError::Provider(e) => ProjectError::Other(format!(
                    "Failed to resolve '{}' in public repo {}/{}: {}. The repository may not exist, be private, or the provider API may be unavailable.",
                    tag.as_deref().unwrap_or(&branch_to_use),
                    project.repo_owner,
                    project.repo_name,
                    e
                )),
                PublicTriggerRefError::Missing(message) => ProjectError::NotFound(message),
            })?;

            info!(
                "Resolved {} '{}' of public repo {}/{} to commit {}",
                resolved.kind.as_str(),
                resolved.name,
                project.repo_owner,
                project.repo_name,
                resolved.commit_sha
            );
            // Keep the tag on the deployment when the ref turned out to be one.
            if resolved.kind == PublicRefKind::Tag && tag.is_none() {
                tag = Some(resolved.name.clone());
            }
            resolved.commit_sha
        } else {
            warn!("No git provider connection found for project, using placeholder commit");
            format!("manual-trigger-{}", chrono::Utc::now().timestamp())
        };

        // Create GitPushEvent job to trigger pipeline
        let git_push_job = temps_core::GitPushEventJob {
            owner: project.repo_owner.clone(),
            repo: project.repo_name.clone(),
            branch: Some(branch_to_use.clone()),
            tag: tag.clone(),
            commit: commit_to_use.clone(),
            project_id, // Include project_id
            // `trigger_pipeline` on the projects service is hit by the
            // "Deploy" button and the CLI — both are user-initiated.
            manual_trigger: true,
            rollback_from_deployment_id: None,
            // The caller explicitly chose this environment — deploy there
            // directly rather than re-inferring the target from the branch
            // (which would fall through to a preview/named-preview env when the
            // environment doesn't have the branch configured).
            target_environment_id: Some(environment_id),
            recovery_of_deployment_id: None,
        };

        // Send the job to the queue
        self.queue_service
            .send(temps_core::Job::GitPushEvent(git_push_job))
            .await
            .map_err(|e| {
                ProjectError::Other(format!("Failed to queue pipeline trigger job: {}", e))
            })?;

        info!(
            "Triggered pipeline for project {} ({}), environment {} ({}), branch: {}",
            project_id, project.name, environment_id, environment.name, branch_to_use
        );

        // Return the details for the response
        Ok((
            project_id,
            environment_id,
            Some(branch_to_use),
            tag,
            Some(commit_to_use),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------------
    // Manual triggers resolve branches, tags and commits of public repos
    // ------------------------------------------------------------------

    mod public_trigger_ref {
        use super::*;
        use temps_core::async_trait::async_trait;
        use temps_git::services::git_provider::FileContent;
        use temps_git::services::public_repo::{PublicBranch, PublicRepoInfo, PublicRepoProvider};

        const BRANCH_TIP: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        const TAG_COMMIT: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        const OLD_COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

        struct FakePublicRepo {
            rate_limited: bool,
        }

        #[async_trait]
        impl PublicRepoProvider for FakePublicRepo {
            fn provider_name(&self) -> &'static str {
                "fake"
            }

            async fn get_repository(
                &self,
                _owner: &str,
                _repo: &str,
            ) -> Result<PublicRepoInfo, PublicRepoError> {
                Err(PublicRepoError::Internal("not used".into()))
            }

            async fn list_branches(
                &self,
                _owner: &str,
                _repo: &str,
            ) -> Result<Vec<PublicBranch>, PublicRepoError> {
                Ok(Vec::new())
            }

            async fn get_branch(
                &self,
                _owner: &str,
                _repo: &str,
                branch: &str,
            ) -> Result<Option<PublicBranch>, PublicRepoError> {
                if self.rate_limited {
                    return Err(PublicRepoError::RateLimitExceeded);
                }
                // `v2` is both a branch and a tag in this repository.
                Ok(["master", "v2"].contains(&branch).then(|| PublicBranch {
                    name: branch.to_string(),
                    commit_sha: BRANCH_TIP.to_string(),
                    protected: false,
                }))
            }

            async fn get_tag_commit(
                &self,
                _owner: &str,
                _repo: &str,
                tag: &str,
            ) -> Result<Option<String>, PublicRepoError> {
                Ok(["v1.2.3", "v2"]
                    .contains(&tag)
                    .then(|| TAG_COMMIT.to_string()))
            }

            async fn get_commit_sha(
                &self,
                _owner: &str,
                _repo: &str,
                sha: &str,
            ) -> Result<Option<String>, PublicRepoError> {
                Ok(OLD_COMMIT.starts_with(sha).then(|| OLD_COMMIT.to_string()))
            }

            async fn get_file_tree(
                &self,
                _owner: &str,
                _repo: &str,
                _reference: &str,
            ) -> Result<Vec<String>, PublicRepoError> {
                Err(PublicRepoError::Internal("not used".into()))
            }

            async fn get_file_content(
                &self,
                _owner: &str,
                _repo: &str,
                _path: &str,
                _reference: &str,
            ) -> Result<FileContent, PublicRepoError> {
                Err(PublicRepoError::Internal("not used".into()))
            }
        }

        async fn resolve(
            reference: &str,
            tag: Option<&str>,
        ) -> Result<ResolvedPublicRef, PublicTriggerRefError> {
            resolve_public_trigger_ref(
                &FakePublicRepo {
                    rate_limited: false,
                },
                "acme",
                "shop",
                reference,
                tag,
            )
            .await
        }

        fn expect_ok(
            result: Result<ResolvedPublicRef, PublicTriggerRefError>,
        ) -> ResolvedPublicRef {
            match result {
                Ok(resolved) => resolved,
                Err(PublicTriggerRefError::Missing(message)) => {
                    panic!("expected a resolved ref, got missing: {message}")
                }
                Err(PublicTriggerRefError::Provider(error)) => {
                    panic!("expected a resolved ref, got provider error: {error}")
                }
            }
        }

        #[tokio::test]
        async fn a_branch_resolves_to_its_tip() {
            let resolved = expect_ok(resolve("master", None).await);
            assert_eq!(resolved.kind, PublicRefKind::Branch);
            assert_eq!(resolved.commit_sha, BRANCH_TIP);
        }

        #[tokio::test]
        async fn a_tag_in_the_branch_field_resolves_to_the_tag_commit() {
            let resolved = expect_ok(resolve("v1.2.3", None).await);
            assert_eq!(resolved.kind, PublicRefKind::Tag);
            assert_eq!(resolved.name, "v1.2.3");
            assert_eq!(resolved.commit_sha, TAG_COMMIT);
        }

        #[tokio::test]
        async fn a_commit_in_the_branch_field_resolves_full_or_abbreviated() {
            for reference in [OLD_COMMIT, &OLD_COMMIT[..7], &OLD_COMMIT[..12]] {
                let resolved = expect_ok(resolve(reference, None).await);
                assert_eq!(resolved.kind, PublicRefKind::Commit, "{reference}");
                assert_eq!(resolved.commit_sha, OLD_COMMIT, "{reference}");
            }
        }

        #[tokio::test]
        async fn the_tag_parameter_deploys_the_tags_commit_not_the_branch_tip() {
            // `{"branch":"master","tag":"v1.2.3"}` used to deploy master's tip.
            let resolved = expect_ok(resolve("master", Some("v1.2.3")).await);
            assert_eq!(resolved.kind, PublicRefKind::Tag);
            assert_eq!(resolved.commit_sha, TAG_COMMIT);
            assert_ne!(resolved.commit_sha, BRANCH_TIP);
        }

        #[tokio::test]
        async fn the_tag_parameter_is_never_answered_by_a_branch_of_the_same_name() {
            // `v2` is a branch and a tag; the explicit tag must win.
            let resolved = expect_ok(resolve("master", Some("v2")).await);
            assert_eq!(resolved.kind, PublicRefKind::Tag);
            assert_eq!(resolved.commit_sha, TAG_COMMIT);
            // Without the tag parameter the branch field prefers the branch.
            let resolved = expect_ok(resolve("v2", None).await);
            assert_eq!(resolved.kind, PublicRefKind::Branch);
            assert_eq!(resolved.commit_sha, BRANCH_TIP);
        }

        #[tokio::test]
        async fn a_missing_ref_names_the_repository_and_the_ref() {
            let Err(PublicTriggerRefError::Missing(message)) = resolve("nope", None).await else {
                panic!("an unknown ref must be reported missing");
            };
            assert_eq!(
                message,
                "'nope' is not a branch, tag or commit of public repo acme/shop"
            );

            let Err(PublicTriggerRefError::Missing(message)) =
                resolve("master", Some("v9.9.9")).await
            else {
                panic!("an unknown tag must be reported missing");
            };
            assert_eq!(message, "Tag 'v9.9.9' not found in public repo acme/shop");

            // A branch name is not a tag.
            assert!(matches!(
                resolve("master", Some("master")).await,
                Err(PublicTriggerRefError::Missing(_))
            ));
        }

        #[tokio::test]
        async fn a_provider_failure_is_not_a_missing_ref() {
            let result = resolve_public_trigger_ref(
                &FakePublicRepo { rate_limited: true },
                "acme",
                "shop",
                "v1.2.3",
                None,
            )
            .await;
            assert!(matches!(
                result,
                Err(PublicTriggerRefError::Provider(
                    PublicRepoError::RateLimitExceeded
                ))
            ));
        }
    }
}
