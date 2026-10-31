// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! [`GitProviderManagerTrait`] for [`GitProviderManager`]: clone, sparse clone,
//! archive download, push-and-PR and scoped tokens for the deployment pipeline.

use super::*;

// Implement the trait for GitProviderManager
use crate::services::git_provider_manager_trait::{GitProviderManagerTrait, RepositoryInfo};

use async_trait::async_trait;

use std::path::Path;

#[async_trait]
impl GitProviderManagerTrait for GitProviderManager {
    async fn clone_repository(
        &self,
        connection_id: i32,
        repo_owner: &str,
        repo_name: &str,
        target_dir: &Path,
        branch_or_ref: Option<&str>,
    ) -> Result<(), crate::services::git_provider_manager_trait::GitProviderManagerError> {
        use crate::services::git_provider_manager_trait::GitProviderManagerError as TraitError;

        // Check if directory is empty
        if target_dir.exists() {
            let is_empty = std::fs::read_dir(target_dir)
                .map_err(|e| TraitError::CloneError(format!("Failed to read directory: {}", e)))?
                .next()
                .is_none();

            if !is_empty {
                return Err(TraitError::DirectoryNotEmpty(
                    target_dir.display().to_string(),
                ));
            }
        } else {
            std::fs::create_dir_all(target_dir).map_err(|e| {
                TraitError::CloneError(format!("Failed to create directory: {}", e))
            })?;
        }

        // Get connection and provider
        let connection = self
            .get_connection(connection_id)
            .await
            .map_err(|_| TraitError::ConnectionNotFound(connection_id))?;

        let provider_service = self
            .get_provider_service(connection.provider_id)
            .await
            .map_err(|_| TraitError::ProviderNotFound(connection.provider_id))?;

        let access_token = self
            .validate_and_refresh_connection_token(connection_id)
            .await
            .map_err(|e| TraitError::DecryptionError(e.to_string()))?;

        // Get repository info — also retry-on-auth, since the same stale
        // token would fail here before we ever reach the clone.
        let repo = match provider_service
            .get_repository(&access_token, repo_owner, repo_name)
            .await
        {
            Ok(r) => r,
            Err(e) if Self::is_auth_failure(&e.to_string()) => {
                tracing::warn!(
                    connection_id,
                    "get_repository hit auth failure ({}); force-refreshing token and retrying",
                    e
                );
                let refreshed = self
                    .force_refresh_connection_token(connection_id)
                    .await
                    .map_err(|err| TraitError::DecryptionError(err.to_string()))?;
                provider_service
                    .get_repository(&refreshed, repo_owner, repo_name)
                    .await
                    .map_err(|err| {
                        TraitError::CloneError(format!(
                            "Failed to get repository after token refresh: {}",
                            err
                        ))
                    })?
            }
            Err(e) => {
                return Err(TraitError::CloneError(format!(
                    "Failed to get repository: {}",
                    e
                )))
            }
        };

        // Clone the repository. On auth failure (libgit2 surfaces 401/403 as a
        // generic credential error), force-refresh the OAuth token and retry
        // once. Two motivations:
        //   1. Time-of-check / time-of-use gap: the token we validated above
        //      may have expired between then and libgit2's HTTPS auth attempt.
        //   2. Server-side revocation: GitLab can invalidate a token before
        //      its recorded expiry.
        // libgit2's credential callback only fires once, so we have to retry
        // the whole operation rather than refreshing inside the callback.
        target_dir.to_str().ok_or_else(|| {
            TraitError::CloneError("target directory contains invalid UTF-8".to_string())
        })?;
        // Branch/tag deployments only need the selected tip, not every
        // historical object. Keep full clones for pinned commit checkouts.
        let shallow_ref = branch_or_ref.filter(|reference| {
            !reference.eq_ignore_ascii_case("HEAD")
                && !(matches!(reference.len(), 7..=64)
                    && reference.bytes().all(|b| b.is_ascii_hexdigit()))
        });
        let username =
            Self::clone_username_for_provider_type(&provider_service.provider_type().to_string());
        let clone_with_token = |token: String| {
            let provider_service = provider_service.clone();
            let clone_url = repo.clone_url.clone();
            let target_dir = target_dir.to_path_buf();
            let shallow_ref = shallow_ref.map(str::to_string);
            async move {
                if let Some(reference) = shallow_ref {
                    // Dropping this future terminates Git and its transport
                    // process group before the checkout can be cleaned up.
                    tokio::time::timeout(
                        std::time::Duration::from_secs(300),
                        crate::services::git_ops::clone_named_ref_with_credentials(
                            &clone_url,
                            &target_dir,
                            username,
                            &token,
                            &reference,
                        ),
                    )
                    .await
                    .map_err(|_| {
                        TraitError::CloneError("Git shallow clone timed out after 300s".to_string())
                    })?
                    .map_err(|error| TraitError::CloneError(error.to_string()))
                } else {
                    provider_service
                        .clone_repository(&clone_url, &target_dir.to_string_lossy(), Some(&token))
                        .await
                        .map_err(|error| TraitError::CloneError(error.to_string()))
                }
            }
        };
        let clone_result = clone_with_token(access_token.clone()).await;
        if let Err(e) = clone_result {
            if Self::is_auth_failure(&e.to_string()) {
                tracing::warn!(
                    connection_id,
                    "clone_repository hit auth failure ({}); force-refreshing token and retrying once",
                    e
                );

                // Wipe the partially-cloned state, otherwise the retry trips
                // the "directory not empty" check or fails with a corrupted
                // git tree.
                if target_dir.exists() {
                    if let Err(rm) = std::fs::remove_dir_all(target_dir) {
                        return Err(TraitError::CloneError(format!(
                            "Auth retry: failed to clean partial clone at {}: {}",
                            target_dir.display(),
                            rm
                        )));
                    }
                    std::fs::create_dir_all(target_dir).map_err(|err| {
                        TraitError::CloneError(format!(
                            "Auth retry: failed to recreate target directory: {}",
                            err
                        ))
                    })?;
                }

                let refreshed = self
                    .force_refresh_connection_token(connection_id)
                    .await
                    .map_err(|err| TraitError::DecryptionError(err.to_string()))?;

                clone_with_token(refreshed).await.map_err(|err| {
                    TraitError::CloneError(format!("Failed to clone after refresh: {}", err))
                })?;
            } else {
                return Err(TraitError::CloneError(format!("Failed to clone: {}", e)));
            }
        }

        // Checkout specific ref if provided
        if let Some(ref_name) = branch_or_ref {
            if shallow_ref.is_none() && ref_name != repo.default_branch {
                let target_dir_owned = target_dir.to_path_buf();
                let ref_name_owned = ref_name.to_string();
                tokio::task::spawn_blocking(move || {
                    let repo = git2::Repository::open(&target_dir_owned).map_err(|e| {
                        TraitError::CloneError(format!("Failed to open repository: {}", e))
                    })?;
                    crate::services::git_ops::checkout_ref(&repo, &ref_name_owned).map_err(|e| {
                        TraitError::CloneError(format!(
                            "Failed to checkout ref {}: {}",
                            ref_name_owned, e
                        ))
                    })
                })
                .await
                .map_err(|e| {
                    TraitError::CloneError(format!("Git checkout task failed: {}", e))
                })??;
            }
        }

        Ok(())
    }

    async fn clone_sparse_subdirectory(
        &self,
        connection_id: i32,
        repo_owner: &str,
        repo_name: &str,
        target_dir: &Path,
        subdirectory: &str,
        branch_or_ref: Option<&str>,
    ) -> Result<(), crate::services::git_provider_manager_trait::GitProviderManagerError> {
        use crate::services::git_provider_manager_trait::GitProviderManagerError as TraitError;

        let subdirectory = crate::services::git_ops::validate_sparse_subdirectory(subdirectory)
            .map_err(|e| TraitError::CloneError(e.to_string()))?;

        if target_dir.exists() {
            let is_empty = std::fs::read_dir(target_dir)
                .map_err(|e| TraitError::CloneError(format!("Failed to read directory: {}", e)))?
                .next()
                .is_none();

            if !is_empty {
                return Err(TraitError::DirectoryNotEmpty(
                    target_dir.display().to_string(),
                ));
            }
        } else {
            std::fs::create_dir_all(target_dir).map_err(|e| {
                TraitError::CloneError(format!("Failed to create directory: {}", e))
            })?;
        }

        let connection = self
            .get_connection(connection_id)
            .await
            .map_err(|_| TraitError::ConnectionNotFound(connection_id))?;

        let provider = self
            .get_provider(connection.provider_id)
            .await
            .map_err(|_| TraitError::ProviderNotFound(connection.provider_id))?;

        let provider_service = self
            .get_provider_service(connection.provider_id)
            .await
            .map_err(|_| TraitError::ProviderNotFound(connection.provider_id))?;

        let access_token = self
            .validate_and_refresh_connection_token(connection_id)
            .await
            .map_err(|e| TraitError::DecryptionError(e.to_string()))?;

        let repo = match provider_service
            .get_repository(&access_token, repo_owner, repo_name)
            .await
        {
            Ok(r) => r,
            Err(e) if Self::is_auth_failure(&e.to_string()) => {
                tracing::warn!(
                    connection_id,
                    "get_repository hit auth failure ({}); force-refreshing token and retrying",
                    e
                );
                let refreshed = self
                    .force_refresh_connection_token(connection_id)
                    .await
                    .map_err(|err| TraitError::DecryptionError(err.to_string()))?;
                provider_service
                    .get_repository(&refreshed, repo_owner, repo_name)
                    .await
                    .map_err(|err| {
                        TraitError::CloneError(format!(
                            "Failed to get repository after token refresh: {}",
                            err
                        ))
                    })?
            }
            Err(e) => {
                return Err(TraitError::CloneError(format!(
                    "Failed to get repository: {}",
                    e
                )))
            }
        };

        let username = Self::clone_username_for_provider_type(&provider.provider_type).to_string();
        let clone_url = repo.clone_url.clone();
        let target_dir_owned = target_dir.to_path_buf();
        let subdirectory_owned = subdirectory.clone();
        let checkout_ref = branch_or_ref.map(|value| value.to_string());

        const CLONE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

        let run_sparse_clone = |token: String| {
            let clone_url = clone_url.clone();
            let target_dir_owned = target_dir_owned.clone();
            let subdirectory_owned = subdirectory_owned.clone();
            let checkout_ref = checkout_ref.clone();
            let username = username.clone();
            async move {
                crate::services::git_ops::sparse_clone_repo(
                    &clone_url,
                    &target_dir_owned,
                    &subdirectory_owned,
                    checkout_ref.as_deref(),
                    Some((username.as_str(), token.as_str())),
                )
                .await
            }
        };

        let clone_result =
            match tokio::time::timeout(CLONE_TIMEOUT, run_sparse_clone(access_token.clone())).await
            {
                Ok(result) => result,
                Err(_) => {
                    return Err(TraitError::CloneError(format!(
                        "Git sparse clone timed out after {}s",
                        CLONE_TIMEOUT.as_secs()
                    )))
                }
            };

        if let Err(e) = clone_result {
            if Self::is_auth_failure(&e.to_string()) {
                tracing::warn!(
                    connection_id,
                    "clone_sparse_subdirectory hit auth failure ({}); force-refreshing token and retrying once",
                    e
                );

                if target_dir.exists() {
                    if let Err(rm) = std::fs::remove_dir_all(target_dir) {
                        return Err(TraitError::CloneError(format!(
                            "Auth retry: failed to clean partial clone at {}: {}",
                            target_dir.display(),
                            rm
                        )));
                    }
                    std::fs::create_dir_all(target_dir).map_err(|err| {
                        TraitError::CloneError(format!(
                            "Auth retry: failed to recreate target directory: {}",
                            err
                        ))
                    })?;
                }

                let refreshed = self
                    .force_refresh_connection_token(connection_id)
                    .await
                    .map_err(|err| TraitError::DecryptionError(err.to_string()))?;

                match tokio::time::timeout(CLONE_TIMEOUT, run_sparse_clone(refreshed)).await {
                    Ok(result) => {
                        result.map_err(|err| {
                            TraitError::CloneError(format!(
                                "Failed to sparse clone after refresh: {}",
                                err
                            ))
                        })?;
                    }
                    Err(_) => {
                        return Err(TraitError::CloneError(format!(
                            "Git sparse clone retry timed out after {}s",
                            CLONE_TIMEOUT.as_secs()
                        )))
                    }
                }
            } else {
                return Err(TraitError::CloneError(format!(
                    "Failed to sparse clone: {}",
                    e
                )));
            }
        }

        Ok(())
    }

    async fn get_connection_access_token(
        &self,
        connection_id: i32,
    ) -> Result<
        (String, String),
        crate::services::git_provider_manager_trait::GitProviderManagerError,
    > {
        use crate::services::git_provider_manager_trait::GitProviderManagerError as TraitError;

        let connection = self
            .get_connection(connection_id)
            .await
            .map_err(|_| TraitError::ConnectionNotFound(connection_id))?;

        let provider = self
            .get_provider(connection.provider_id)
            .await
            .map_err(|_| TraitError::ProviderNotFound(connection.provider_id))?;

        let token = self
            .get_connection_token(connection_id)
            .await
            .map_err(|e| TraitError::DecryptionError(e.to_string()))?;

        Ok((token, provider.provider_type))
    }

    async fn get_repository_info(
        &self,
        connection_id: i32,
        repo_owner: &str,
        repo_name: &str,
    ) -> Result<RepositoryInfo, crate::services::git_provider_manager_trait::GitProviderManagerError>
    {
        use crate::services::git_provider_manager_trait::GitProviderManagerError as TraitError;

        let connection = self
            .get_connection(connection_id)
            .await
            .map_err(|_| TraitError::ConnectionNotFound(connection_id))?;

        let provider_service = self
            .get_provider_service(connection.provider_id)
            .await
            .map_err(|_| TraitError::ProviderNotFound(connection.provider_id))?;

        let access_token = self
            .validate_and_refresh_connection_token(connection_id)
            .await
            .map_err(|e| TraitError::DecryptionError(e.to_string()))?;

        let repo = provider_service
            .get_repository(&access_token, repo_owner, repo_name)
            .await
            .map_err(|e| TraitError::Other(format!("Failed to get repository: {}", e)))?;

        Ok(RepositoryInfo {
            clone_url: repo.clone_url,
            default_branch: repo.default_branch,
            owner: repo_owner.to_string(),
            name: repo_name.to_string(),
        })
    }

    async fn download_archive(
        &self,
        connection_id: i32,
        repo_owner: &str,
        repo_name: &str,
        branch_or_ref: &str,
        archive_path: &Path,
        progress: Option<&crate::services::git_provider::ArchiveProgressSender>,
    ) -> Result<(), crate::services::git_provider_manager_trait::GitProviderManagerError> {
        use crate::services::git_provider_manager_trait::GitProviderManagerError as TraitError;

        let connection = self
            .get_connection(connection_id)
            .await
            .map_err(|_| TraitError::ConnectionNotFound(connection_id))?;

        let provider_service = self
            .get_provider_service(connection.provider_id)
            .await
            .map_err(|_| TraitError::ProviderNotFound(connection.provider_id))?;

        let access_token = self
            .validate_and_refresh_connection_token(connection_id)
            .await
            .map_err(|e| TraitError::DecryptionError(e.to_string()))?;

        // Same retry-on-auth logic as clone_repository: the token we just
        // validated may have expired or been revoked between the check and
        // the HTTP request the provider issues to download the tarball.
        match provider_service
            .download_archive(
                &access_token,
                repo_owner,
                repo_name,
                branch_or_ref,
                archive_path,
                progress,
            )
            .await
        {
            Ok(()) => Ok(()),
            Err(e) if Self::is_auth_failure(&e.to_string()) => {
                tracing::warn!(
                    connection_id,
                    "download_archive hit auth failure ({}); force-refreshing token and retrying once",
                    e
                );
                // The provider usually creates the archive file as it streams
                // — if the auth failed mid-stream we'd be left with a partial
                // file. Remove it so the retry starts clean.
                if archive_path.exists() {
                    let _ = std::fs::remove_file(archive_path);
                }
                let refreshed = self
                    .force_refresh_connection_token(connection_id)
                    .await
                    .map_err(|err| TraitError::DecryptionError(err.to_string()))?;
                provider_service
                    .download_archive(
                        &refreshed,
                        repo_owner,
                        repo_name,
                        branch_or_ref,
                        archive_path,
                        progress,
                    )
                    .await
                    .map_err(|err| {
                        TraitError::Other(format!(
                            "Failed to download archive after token refresh: {}",
                            err
                        ))
                    })?;
                Ok(())
            }
            Err(e) => Err(TraitError::Other(format!(
                "Failed to download archive: {}",
                e
            ))),
        }
    }

    async fn push_files_and_create_pr(
        &self,
        connection_id: i32,
        owner: &str,
        repo: &str,
        branch: &str,
        base_branch: &str,
        files: Vec<(String, Vec<u8>)>,
        commit_message: &str,
        pr_title: &str,
        pr_body: &str,
    ) -> Result<
        crate::services::git_provider_manager_trait::PullRequest,
        crate::services::git_provider_manager_trait::GitProviderManagerError,
    > {
        use crate::services::git_provider_manager_trait::GitProviderManagerError as TraitError;

        let connection = self
            .get_connection(connection_id)
            .await
            .map_err(|_| TraitError::ConnectionNotFound(connection_id))?;

        let provider_service = self
            .get_provider_service(connection.provider_id)
            .await
            .map_err(|_| TraitError::ProviderNotFound(connection.provider_id))?;

        let access_token = self
            .validate_and_refresh_connection_token(connection_id)
            .await
            .map_err(|e| TraitError::DecryptionError(e.to_string()))?;

        // Push files to the branch (creates the branch + commit)
        provider_service
            .push_files_to_repository(&access_token, owner, repo, branch, files, commit_message)
            .await
            .map_err(|e| {
                TraitError::Other(format!(
                    "Failed to push files to {}/{} branch {}: {}",
                    owner, repo, branch, e
                ))
            })?;

        // Create the pull request
        let pr = provider_service
            .create_pull_request(
                &access_token,
                owner,
                repo,
                pr_title,
                pr_body,
                branch,
                base_branch,
            )
            .await
            .map_err(|e| {
                TraitError::Other(format!(
                    "Failed to create pull request in {}/{} ({}->{}): {}",
                    owner, repo, branch, base_branch, e
                ))
            })?;

        Ok(pr)
    }

    async fn mint_scoped_repo_token(
        &self,
        connection_id: i32,
        owner: &str,
        repo: &str,
        operation: crate::services::git_provider_manager_trait::ScopedTokenOp,
    ) -> Result<
        crate::services::git_provider_manager_trait::ScopedTokenGrant,
        crate::services::git_provider_manager_trait::GitProviderManagerError,
    > {
        use crate::services::git_provider_manager_trait::GitProviderManagerError as TraitError;

        match self
            .mint_scoped_repo_token_for_connection(connection_id, owner, repo, operation)
            .await
        {
            Ok(grant) => Ok(grant),
            Err(GitProviderManagerError::ProviderError(
                crate::services::git_provider::GitProviderError::NotImplemented,
            )) => Err(TraitError::ScopedTokensUnsupported {
                connection_id,
                reason: "provider does not implement scoped per-op tokens (PAT/OAuth)".into(),
            }),
            Err(GitProviderManagerError::ProviderError(
                crate::services::git_provider::GitProviderError::InvalidConfiguration(reason),
            ))
            | Err(GitProviderManagerError::InvalidConfiguration(reason)) => {
                Err(TraitError::ScopedTokensUnsupported {
                    connection_id,
                    reason,
                })
            }
            Err(GitProviderManagerError::ConnectionNotFound(_)) => {
                Err(TraitError::ConnectionNotFound(connection_id))
            }
            Err(GitProviderManagerError::ProviderNotFound(_)) => {
                Err(TraitError::ProviderNotFound(connection_id))
            }
            Err(e) => Err(TraitError::Other(format!(
                "Failed to mint scoped token for connection {} on {}/{}: {}",
                connection_id, owner, repo, e
            ))),
        }
    }
}
