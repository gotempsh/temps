// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Writes to a user's repositories: create a repository, commit files, open a
//! pull request, and seed a new repository from a template.

use super::*;

impl GitProviderManager {
    /// Create a repository through a connection owned by `user_id`.
    ///
    /// Repository creation is a credentialed mutation. This method verifies
    /// ownership and active state before resolving the provider and refreshing
    /// its token. It deliberately does not download or push template content.
    pub async fn create_repository_for_user(
        &self,
        connection_id: i32,
        user_id: i32,
        repo_name: &str,
        repo_owner: Option<&str>,
        description: Option<&str>,
        private: bool,
    ) -> Result<crate::services::git_provider::Repository, GitProviderManagerError> {
        // Validate before any lookup or token refresh: a bad name is the
        // caller's mistake and must not cost a credentialed round trip.
        validate_new_repository(connection_id, repo_name, repo_owner, description)?;
        let (_, provider_service, access_token) = self
            .usable_connection_for_user(connection_id, user_id)
            .await?;
        provider_service
            .create_repository(&access_token, repo_name, repo_owner, description, private)
            .await
            .map_err(|source| GitProviderManagerError::RepositoryCreation {
                connection_id,
                repository_name: repo_name.to_string(),
                source,
            })
    }

    /// Resolve a connection the caller owns into everything a credentialed
    /// repository operation needs: the connection row, its provider service
    /// and a fresh access token.
    ///
    /// Another user's connection is reported as not found, and an inactive or
    /// expired connection or provider is refused before any token refresh.
    async fn usable_connection_for_user(
        &self,
        connection_id: i32,
        user_id: i32,
    ) -> Result<
        (
            git_provider_connections::Model,
            Arc<dyn GitProviderService>,
            String,
        ),
        GitProviderManagerError,
    > {
        let connection = self.get_connection_for_user(connection_id, user_id).await?;
        // MockDatabase does not apply query predicates, and this defense also
        // protects future alternative data-access implementations.
        if connection.user_id != Some(user_id) {
            return Err(foreign_or_missing_connection(connection_id));
        }
        if !connection.is_active || connection.is_expired {
            return Err(GitProviderManagerError::InvalidConfiguration(format!(
                "Git connection {connection_id} for user {user_id} is inactive or expired"
            )));
        }
        let provider = self.get_provider(connection.provider_id).await?;
        if !provider.is_active {
            return Err(GitProviderManagerError::InvalidConfiguration(format!(
                "Git provider {} for connection {connection_id} is inactive",
                provider.id
            )));
        }
        let provider_service = self.get_provider_service(provider.id).await?;
        let access_token = self
            .validate_and_refresh_connection_token(connection_id)
            .await?;
        Ok((connection, provider_service, access_token))
    }

    /// Look up `owner/repo` through a provider. A repository that does not
    /// exist, or that the token cannot see, comes back as the provider's own
    /// message in [`GitProviderManagerError::RepositoryOperation`].
    async fn existing_repository(
        provider_service: &Arc<dyn GitProviderService>,
        access_token: &str,
        connection_id: i32,
        owner: &str,
        repo: &str,
    ) -> Result<crate::services::git_provider::Repository, GitProviderManagerError> {
        provider_service
            .get_repository(access_token, owner, repo)
            .await
            .map_err(|source| GitProviderManagerError::RepositoryOperation {
                connection_id,
                repository: format!("{owner}/{repo}"),
                operation: "look up the repository",
                reason: redact_secret(&source.to_string(), access_token),
            })
    }

    /// Commit `files` to `owner/repo` through a connection owned by `user_id`.
    ///
    /// The commit is a normal fast-forward on top of `branch` (default: the
    /// repository's default branch), so existing history is never rewritten.
    /// A branch that does not exist yet is started from `base_branch`'s tip
    /// (default: the default branch's); a `base_branch` the remote lacks is
    /// refused rather than replaced by the default branch.
    ///
    /// `unchanged_since` is the revision the files' new content was written
    /// against. The files are compared between it and the tree the commit is
    /// built on, in the same fresh clone, and the commit is refused with
    /// [`GitProviderManagerError::FilesChangedSince`] when any differs, since
    /// replacing them whole would undo those changes. The push is a
    /// fast-forward from that tree, so the tree checked is the one the commit
    /// lands on. Returns the new commit's SHA and the branch it landed on.
    #[allow(clippy::too_many_arguments)]
    pub async fn commit_files_for_user(
        &self,
        connection_id: i32,
        user_id: i32,
        owner: &str,
        repo: &str,
        branch: Option<&str>,
        base_branch: Option<&str>,
        unchanged_since: Option<&str>,
        files: Vec<(String, Vec<u8>)>,
        commit_message: &str,
    ) -> Result<(String, String), GitProviderManagerError> {
        validate_commit_request(connection_id, owner, repo, branch, &files, commit_message)?;
        if let Some(base) = base_branch {
            validate_branch_name(connection_id, &format!("{owner}/{repo}"), base)?;
        }
        if let Some(revision) = unchanged_since {
            if !is_commit_id(revision) {
                return Err(GitProviderManagerError::InvalidConfiguration(format!(
                    "Commit to '{owner}/{repo}' (connection {connection_id}) must be checked against a full commit id (40 or 64 hex characters), got '{}'",
                    revision.escape_debug()
                )));
            }
        }
        let (connection, provider_service, access_token) = self
            .usable_connection_for_user(connection_id, user_id)
            .await?;
        let repository =
            Self::existing_repository(&provider_service, &access_token, connection_id, owner, repo)
                .await?;
        let branch = branch
            .map(str::to_string)
            .unwrap_or_else(|| repository.default_branch.clone());
        validate_branch_name(connection_id, &repository.full_name, &branch)?;

        let author_name = if connection.account_name.is_empty() {
            "Temps Bot".to_string()
        } else {
            connection.account_name.clone()
        };
        let clone_url = repository.clone_url.clone();
        let clone_username =
            Self::clone_username_for_provider_type(&provider_service.provider_type().to_string());
        let full_name = repository.full_name.clone();
        let token = access_token.clone();
        let target_branch = branch.clone();
        let base_branch = base_branch.map(str::to_string);
        let unchanged_since = unchanged_since.map(str::to_string);
        let message = commit_message.to_string();
        let file_count = files.len();
        let cancellation = Arc::new(CommitCancellation::default());
        let worker_cancellation = Arc::clone(&cancellation);

        // git2 is blocking: run it off the async workers, and bound how long
        // the caller waits for a stuck remote. On timeout the cancellation
        // aborts the clone and stops the push from starting, so a retry
        // cannot double-commit unless the push was already under way.
        let task = tokio::task::spawn_blocking(move || {
            let request = GitPushRequest {
                connection_id,
                repository: &full_name,
                clone_url: &clone_url,
                clone_username,
                access_token: &token,
                branch: &target_branch,
                base_branch: base_branch.as_deref(),
                unchanged_since: unchanged_since.as_deref(),
                commit_message: &message,
                author_name: &author_name,
                author_email: "bot@temps.sh",
            };
            Self::push_files_with_git(&request, files, &worker_cancellation)
        });
        let failure = |reason: String| GitProviderManagerError::RepositoryOperation {
            connection_id,
            repository: repository.full_name.clone(),
            operation: "commit files",
            reason: redact_secret(&reason, &access_token),
        };
        let commit_sha = match tokio::time::timeout(COMMIT_FILES_TIMEOUT, task).await {
            Ok(Ok(Ok(sha))) => sha,
            // Already typed and contextual; their status codes matter.
            Ok(Ok(Err(
                e @ (GitProviderManagerError::CommitPathRejected { .. }
                | GitProviderManagerError::BaseBranchNotFound { .. }
                | GitProviderManagerError::FilesChangedSince { .. }
                | GitProviderManagerError::RevisionNotFound { .. }
                | GitProviderManagerError::NothingToCommit { .. }
                | GitProviderManagerError::PushRejected { .. }
                | GitProviderManagerError::CommitTimedOut { .. }),
            ))) => return Err(e),
            Ok(Ok(Err(e))) => return Err(failure(e.to_string())),
            Ok(Err(join_error)) => return Err(failure(format!("push task failed: {join_error}"))),
            Err(_) => {
                let push_may_have_completed = cancellation.cancel();
                warn!(
                    connection_id,
                    user_id,
                    repository = %repository.full_name,
                    branch = %branch,
                    push_may_have_completed,
                    "Commit timed out after {}s",
                    COMMIT_FILES_TIMEOUT.as_secs()
                );
                return Err(GitProviderManagerError::CommitTimedOut {
                    connection_id,
                    repository: repository.full_name.clone(),
                    branch: branch.clone(),
                    timeout_secs: COMMIT_FILES_TIMEOUT.as_secs(),
                    push_may_have_completed,
                });
            }
        };
        info!(
            connection_id,
            user_id,
            repository = %repository.full_name,
            branch = %branch,
            file_count,
            commit_sha = %commit_sha,
            "Committed files to repository"
        );
        Ok((commit_sha, branch))
    }

    /// Open a pull request on `owner/repo` through a connection owned by
    /// `user_id`. `base_branch` defaults to the repository's default branch.
    #[allow(clippy::too_many_arguments)]
    pub async fn open_pull_request_for_user(
        &self,
        connection_id: i32,
        user_id: i32,
        owner: &str,
        repo: &str,
        title: &str,
        body: &str,
        head_branch: &str,
        base_branch: Option<&str>,
    ) -> Result<crate::services::git_provider::PullRequest, GitProviderManagerError> {
        validate_repository_ref(connection_id, owner, repo)?;
        let repository_ref = format!("{owner}/{repo}");
        validate_branch_name(connection_id, &repository_ref, head_branch)?;
        if let Some(base) = base_branch {
            validate_branch_name(connection_id, &repository_ref, base)?;
        }
        if title.trim().is_empty()
            || title.len() > MAX_PULL_REQUEST_TITLE_LEN
            || title.chars().any(char::is_control)
            || body.len() > MAX_PULL_REQUEST_BODY_LEN
            || body.contains('\0')
        {
            return Err(GitProviderManagerError::InvalidConfiguration(format!(
                "Pull request for '{repository_ref}' (connection {connection_id}) needs a title of 1-{MAX_PULL_REQUEST_TITLE_LEN} characters with no control characters and a body of at most {MAX_PULL_REQUEST_BODY_LEN} bytes"
            )));
        }
        let (_, provider_service, access_token) = self
            .usable_connection_for_user(connection_id, user_id)
            .await?;
        let repository =
            Self::existing_repository(&provider_service, &access_token, connection_id, owner, repo)
                .await?;
        let base = base_branch
            .map(str::to_string)
            .unwrap_or_else(|| repository.default_branch.clone());
        if base == head_branch {
            return Err(GitProviderManagerError::InvalidConfiguration(format!(
                "Pull request for '{repository_ref}' (connection {connection_id}) must come from a branch other than its base '{base}'"
            )));
        }
        provider_service
            .create_pull_request(&access_token, owner, repo, title, body, head_branch, &base)
            .await
            .map_err(|source| GitProviderManagerError::RepositoryOperation {
                connection_id,
                repository: repository_ref,
                operation: "open a pull request",
                reason: redact_secret(&source.to_string(), &access_token),
            })
    }

    /// Create a repository on the git provider and push template files
    ///
    /// This method:
    /// 1. Creates a new repository on the git provider
    /// 2. Downloads the template repository archive
    /// 3. Extracts the files (optionally from a subfolder)
    /// 4. Pushes the template files to the new repository using native git operations
    ///
    /// # Arguments
    /// * `connection_id` - The git provider connection ID
    /// * `repo_name` - Name for the new repository
    /// * `repo_owner` - Optional owner/organization (defaults to authenticated user)
    /// * `description` - Optional description for the repository
    /// * `private` - Whether the repository should be private
    /// * `template_url` - URL of the template repository
    /// * `template_ref` - Branch/tag/commit reference in the template repository
    /// * `template_subfolder` - Optional subfolder within the template repository
    ///
    /// # Returns
    /// * `Ok(Repository)` - The created repository with the pushed template code
    /// * `Err(GitProviderManagerError)` - If any step fails
    #[allow(clippy::too_many_arguments)]
    pub async fn create_repository_and_push_template(
        &self,
        connection_id: i32,
        user_id: i32,
        repo_name: &str,
        repo_owner: Option<&str>,
        description: Option<&str>,
        private: bool,
        template_url: &str,
        template_ref: &str,
        template_subfolder: Option<&str>,
    ) -> Result<crate::services::git_provider::Repository, GitProviderManagerError> {
        info!(
            "Creating repository {} from template {} (ref: {}, subfolder: {:?})",
            repo_name, template_url, template_ref, template_subfolder
        );

        // Repository creation is a credentialed mutation. Do not let a caller
        // select another user's stored Git connection as a confused deputy.
        let connection = self.get_connection_for_user(connection_id, user_id).await?;
        let provider_service = self.get_provider_service(connection.provider_id).await?;
        let access_token = self
            .validate_and_refresh_connection_token(connection_id)
            .await?;

        // Step 1: Create the new repository
        info!("Step 1: Creating repository {} on git provider", repo_name);
        let new_repo = provider_service
            .create_repository(&access_token, repo_name, repo_owner, description, private)
            .await?;

        info!(
            "Created repository: {} (clone_url: {})",
            new_repo.full_name, new_repo.clone_url
        );

        // Step 2: Download the template archive
        let temp_dir = tempfile::tempdir().map_err(|e| {
            GitProviderManagerError::InvalidConfiguration(format!(
                "Failed to create temp dir: {}",
                e
            ))
        })?;
        let archive_path = temp_dir.path().join("template.tar.gz");

        // Parse template URL to get owner/repo
        let (template_owner, template_repo) = parse_git_url(template_url).ok_or_else(|| {
            GitProviderManagerError::InvalidConfiguration(format!(
                "Invalid template URL: {}",
                template_url
            ))
        })?;

        info!(
            "Step 2: Downloading template archive from {}/{} (ref: {})",
            template_owner, template_repo, template_ref
        );

        // Use the provider service to download the archive (no progress sink:
        // template downloads are not surfaced in a deployment log stream).
        provider_service
            .download_archive(
                &access_token,
                &template_owner,
                &template_repo,
                template_ref,
                &archive_path,
                None,
            )
            .await?;

        // Step 3: Extract the archive and read files
        info!("Step 3: Extracting template archive");
        let files = extract_template_files(&archive_path, template_subfolder).map_err(|e| {
            GitProviderManagerError::InvalidConfiguration(format!(
                "Failed to extract template: {}",
                e
            ))
        })?;

        if files.is_empty() {
            error!("No files found in template archive");
            return Err(GitProviderManagerError::InvalidConfiguration(
                "Template archive contains no files".to_string(),
            ));
        }

        info!("Extracted {} files from template", files.len());

        // Step 4: Push template files to the new repository using native git
        info!("Step 4: Pushing template files to new repository using native git");
        let commit_message = format!("Initial commit from template: {}", template_url);

        // Get author info from connection or use defaults
        let author_name = if connection.account_name.is_empty() {
            "Temps Bot"
        } else {
            &connection.account_name
        };
        let author_email = "bot@temps.sh";

        // Use native git operations instead of API-based blob uploads
        // This is much faster: single clone + commit + push vs N individual API calls
        let clone_username =
            Self::clone_username_for_provider_type(&provider_service.provider_type().to_string());
        let push_request = GitPushRequest {
            connection_id,
            repository: &new_repo.full_name,
            clone_url: &new_repo.clone_url,
            clone_username,
            access_token: &access_token,
            branch: &new_repo.default_branch,
            base_branch: None,
            unchanged_since: None,
            commit_message: &commit_message,
            author_name,
            author_email,
        };
        // Template pushes have no deadline, so nothing ever cancels them.
        match Self::push_files_with_git(&push_request, files, &CommitCancellation::default()) {
            Ok(commit_sha) => {
                info!(
                    "Successfully pushed template files with commit: {}",
                    commit_sha
                );
            }
            Err(e) => {
                error!("Failed to push template files: {}", e);
                // Note: Repository was already created, so we return partial success
                // The user can still push files manually
                return Err(e);
            }
        }

        info!(
            "Successfully created repository {} from template",
            new_repo.full_name
        );

        // Record the new repository in the `repositories` table so that
        // `set_repository` / `validate_connected` can find it by name without
        // waiting for the next full sync. An existing row (e.g. from a
        // partially completed retry) is left as-is. This is a hard error: if
        // recording fails the caller cannot locate the repository on resume,
        // so the fork step must not succeed silently. The instantiation engine
        // stores the outcome before advancing; the user can safely retry and
        // the engine will detect the already-created repository on resume.
        self.record_repository(connection_id, &new_repo)
            .await
            .map_err(|e| {
                tracing::error!(
                    connection_id,
                    repo = %new_repo.full_name,
                    error = %e,
                    "Created repository but failed to record it; the fork step will fail so \
                     the instantiation can be retried safely"
                );
                e
            })?;

        Ok(new_repo)
    }

    /// Inserts `repo` into the `repositories` table for `connection_id`, so
    /// `find_repository` / `validate_connected` can locate it immediately
    /// without waiting for the next full sync. If a row for this connection
    /// and `full_name` already exists (e.g. from a prior partially-completed
    /// attempt), the existing row is left unchanged.
    pub async fn record_repository(
        &self,
        connection_id: i32,
        repo: &crate::services::git_provider::Repository,
    ) -> Result<(), GitProviderManagerError> {
        use sea_orm::{ActiveModelTrait, ColumnTrait, QueryFilter, Set};
        // Skip the insert when a row is already there (resume-after-crash).
        let already_exists = repositories::Entity::find()
            .filter(repositories::Column::GitProviderConnectionId.eq(connection_id))
            .filter(repositories::Column::FullName.eq(&repo.full_name))
            .one(self.db.as_ref())
            .await
            .map_err(GitProviderManagerError::DatabaseError)?
            .is_some();
        if already_exists {
            return Ok(());
        }
        let now = chrono::Utc::now();
        let new_row = repositories::ActiveModel {
            git_provider_connection_id: Set(connection_id),
            owner: Set(repo.owner.clone()),
            name: Set(repo.name.clone()),
            full_name: Set(repo.full_name.clone()),
            description: Set(repo.description.clone()),
            private: Set(repo.private),
            fork: Set(false),
            size: Set(0),
            stargazers_count: Set(0),
            watchers_count: Set(0),
            language: Set(None),
            default_branch: Set(repo.default_branch.clone()),
            open_issues_count: Set(0),
            topics: Set(String::new()),
            repo_object: Set(String::new()),
            created_at: Set(repo.created_at),
            updated_at: Set(repo.updated_at),
            pushed_at: Set(repo.pushed_at.unwrap_or(now)),
            clone_url: Set(Some(repo.clone_url.clone())),
            ssh_url: Set(Some(repo.ssh_url.clone())),
            installation_id: Set(None),
            preset: Set(None),
            ..Default::default()
        };
        new_row
            .insert(self.db.as_ref())
            .await
            .map(|_| ())
            .map_err(GitProviderManagerError::DatabaseError)
    }
}

/// Parse a git URL to extract owner and repository name
fn parse_git_url(url: &str) -> Option<(String, String)> {
    // Handle HTTPS URLs: https://github.com/owner/repo.git
    // Handle SSH URLs: git@github.com:owner/repo.git
    let url = url.trim_end_matches(".git");

    if url.contains("://") {
        // HTTPS URL
        let parts: Vec<&str> = url.split('/').collect();
        if parts.len() >= 2 {
            let repo = parts.last()?;
            let owner = parts.get(parts.len() - 2)?;
            return Some((owner.to_string(), repo.to_string()));
        }
    } else if url.contains('@') && url.contains(':') {
        // SSH URL: git@github.com:owner/repo
        let after_colon = url.split(':').nth(1)?;
        let parts: Vec<&str> = after_colon.split('/').collect();
        if parts.len() >= 2 {
            let owner = parts[0];
            let repo = parts[1];
            return Some((owner.to_string(), repo.to_string()));
        }
    }

    None
}

/// Extract files from a template archive
fn extract_template_files(
    archive_path: &std::path::Path,
    subfolder: Option<&str>,
) -> Result<Vec<(String, Vec<u8>)>, std::io::Error> {
    use flate2::read::GzDecoder;
    use std::io::Read;
    use tar::Archive;

    let file = std::fs::File::open(archive_path)?;
    let decoder = GzDecoder::new(file);
    let mut archive = Archive::new(decoder);

    let mut files = Vec::new();
    let entries = archive.entries()?;

    // Determine the prefix to strip (archive usually has a root folder like "repo-main/")
    let mut prefix_to_strip: Option<String> = None;

    for entry in entries {
        let mut entry = entry?;
        let path = entry.path()?.to_path_buf();
        let path_str = path.to_string_lossy().to_string();

        // Skip pax_global_header files (tar archive artifacts)
        if path_str == "pax_global_header" || path_str.ends_with("/pax_global_header") {
            continue;
        }

        // Skip directories but detect the root prefix
        if entry.header().entry_type().is_dir() {
            // Detect the root folder prefix from the first directory entry
            if prefix_to_strip.is_none() {
                let root = path_str
                    .trim_end_matches('/')
                    .split('/')
                    .next()
                    .unwrap_or("");
                if !root.is_empty() {
                    prefix_to_strip = Some(format!("{}/", root));
                }
            }
            continue;
        }

        // For files, also try to detect the prefix if not already detected
        if prefix_to_strip.is_none() && path_str.contains('/') {
            let root = path_str.split('/').next().unwrap_or("");
            if !root.is_empty() {
                prefix_to_strip = Some(format!("{}/", root));
            }
        }

        // Get the relative path after stripping the archive root folder
        let relative_path = if let Some(ref prefix) = prefix_to_strip {
            if path_str.starts_with(prefix) {
                path_str[prefix.len()..].to_string()
            } else {
                path_str.clone()
            }
        } else {
            path_str.clone()
        };

        // Skip any remaining pax files after path processing
        if relative_path == "pax_global_header" || relative_path.contains("pax_global_header") {
            continue;
        }

        // If a subfolder is specified, only include files from that subfolder
        if let Some(subfolder) = subfolder {
            let subfolder_prefix = if subfolder.ends_with('/') {
                subfolder.to_string()
            } else {
                format!("{}/", subfolder)
            };

            if !relative_path.starts_with(&subfolder_prefix) {
                continue;
            }

            // Strip the subfolder prefix to get the final path
            let final_path = relative_path[subfolder_prefix.len()..].to_string();
            if !final_path.is_empty() {
                let mut content = Vec::new();
                entry.read_to_end(&mut content)?;
                files.push((final_path, content));
            }
        } else if !relative_path.is_empty() {
            let mut content = Vec::new();
            entry.read_to_end(&mut content)?;
            files.push((relative_path, content));
        }
    }

    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::git_provider_manager::test_support::*;

    #[tokio::test]
    async fn commit_and_pull_request_reject_bad_input_before_any_connection_lookup() {
        use sea_orm::{DatabaseBackend, MockDatabase};

        // No query results are queued: reaching the database would fail with a
        // different (database) error, so an InvalidConfiguration proves the
        // input was refused first.
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let manager = GitProviderManager::new(
            db.clone(),
            Arc::new(
                temps_core::EncryptionService::new(
                    "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                )
                .expect("test encryption key should be valid"),
            ),
            Arc::new(MockJobQueue) as Arc<dyn JobQueue>,
            create_test_config_service(db),
        );

        let commit = manager
            .commit_files_for_user(9, 5, "acme", "app", None, None, None, one_file("../x"), "m")
            .await
            .expect_err("path traversal must be refused");
        assert!(matches!(
            commit,
            GitProviderManagerError::InvalidConfiguration(_)
        ));

        let same_branch = manager
            .open_pull_request_for_user(9, 5, "acme", "app", "Title", "", "bad branch", None)
            .await
            .expect_err("bad head branch must be refused");
        assert!(matches!(
            same_branch,
            GitProviderManagerError::InvalidConfiguration(_)
        ));

        let no_title = manager
            .open_pull_request_for_user(9, 5, "acme", "app", "  ", "", "feature", None)
            .await
            .expect_err("blank title must be refused");
        assert!(matches!(
            no_title,
            GitProviderManagerError::InvalidConfiguration(_)
        ));
    }

    #[tokio::test]
    async fn commit_files_for_user_treats_another_users_connection_as_missing() {
        use sea_orm::{DatabaseBackend, MockDatabase};

        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([[connection_fixture(11, Some(6))]])
                .into_connection(),
        );
        let manager = GitProviderManager::new(
            db.clone(),
            Arc::new(
                temps_core::EncryptionService::new(
                    "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                )
                .expect("test encryption key should be valid"),
            ),
            Arc::new(MockJobQueue) as Arc<dyn JobQueue>,
            create_test_config_service(db),
        );

        let error = manager
            .commit_files_for_user(
                11,
                5,
                "acme",
                "app",
                None,
                None,
                None,
                one_file("a.txt"),
                "Add file",
            )
            .await
            .expect_err("a connection owned by someone else must not be usable");
        assert!(matches!(
            error,
            GitProviderManagerError::ConnectionNotFound(ref message)
                if message.starts_with("Git connection 11 ")
        ));
    }

    #[tokio::test]
    async fn create_repository_for_user_rejects_foreign_and_inactive_connections() {
        use sea_orm::{DatabaseBackend, MockDatabase};

        for (connection, expected_inactive) in [
            (connection_fixture(11, Some(6)), false),
            (
                {
                    let mut connection = connection_fixture(11, Some(5));
                    connection.is_active = false;
                    connection
                },
                true,
            ),
        ] {
            let db = Arc::new(
                MockDatabase::new(DatabaseBackend::Postgres)
                    .append_query_results([[connection]])
                    .into_connection(),
            );
            let manager = GitProviderManager::new(
                db.clone(),
                Arc::new(
                    temps_core::EncryptionService::new(
                        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                    )
                    .expect("test encryption key should be valid"),
                ),
                Arc::new(MockJobQueue) as Arc<dyn JobQueue>,
                create_test_config_service(db),
            );

            let error = manager
                .create_repository_for_user(11, 5, "new-repo", None, None, true)
                .await
                .expect_err("unusable connection must be rejected before provider access");
            if expected_inactive {
                assert!(
                    matches!(error, GitProviderManagerError::InvalidConfiguration(reason)
                    if reason.contains("connection 11") && reason.contains("user 5") && reason.contains("inactive or expired"))
                );
            } else {
                assert!(matches!(
                    error,
                    GitProviderManagerError::ConnectionNotFound(message) if message.contains("connection 11")
                ));
            }
        }
    }

    #[tokio::test]
    async fn create_repository_for_user_uses_owned_connection_and_refreshed_token_path() {
        use crate::services::git_provider::{AuthMethod, GitProviderService};
        use crate::services::github_provider::GitHubProvider;
        use sea_orm::{DatabaseBackend, MockDatabase};

        let mut server = mockito::Server::new_async().await;
        let create = server
            .mock("POST", "/user/repos")
            .match_header("authorization", "Bearer owned-create-token")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "name": "new-repo",
                "private": true,
                "auto_init": true
            })))
            .with_status(201)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "id": 99,
                    "name": "new-repo",
                    "full_name": "owner/new-repo",
                    "owner": {"login": "owner"},
                    "description": "created by Temps",
                    "private": true,
                    "default_branch": "main",
                    "clone_url": "https://example.test/owner/new-repo.git",
                    "ssh_url": "git@example.test:owner/new-repo.git",
                    "html_url": "https://example.test/owner/new-repo",
                    "language": null,
                    "size": 0,
                    "stargazers_count": 0,
                    "forks_count": 0,
                    "created_at": "2026-09-11T00:00:00Z",
                    "updated_at": "2026-09-11T00:00:00Z",
                    "pushed_at": null
                })
                .to_string(),
            )
            .create_async()
            .await;
        let encryption_service = Arc::new(
            temps_core::EncryptionService::new(
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            )
            .expect("test encryption key should be valid"),
        );
        let mut connection = connection_fixture(11, Some(5));
        connection.access_token = Some(
            encryption_service
                .encrypt_string("owned-create-token")
                .expect("test token should encrypt"),
        );
        let mut provider_model = github_provider_fixture();
        provider_model.auth_config = serde_json::to_value(AuthMethod::PersonalAccessToken {
            token: "unused-provider-token".to_string(),
        })
        .unwrap();
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([[connection.clone()]])
                .append_query_results([[provider_model.clone()]])
                .append_query_results([[connection]])
                .append_query_results([[provider_model]])
                .into_connection(),
        );
        let manager = GitProviderManager::new(
            db.clone(),
            encryption_service,
            Arc::new(MockJobQueue) as Arc<dyn JobQueue>,
            create_test_config_service(db),
        );
        manager.providers_cache.write().await.insert(
            7,
            Arc::new(GitHubProvider::new(
                Some(server.url()),
                AuthMethod::PersonalAccessToken {
                    token: "unused-provider-token".to_string(),
                },
            )) as Arc<dyn GitProviderService>,
        );

        let repository = manager
            .create_repository_for_user(11, 5, "new-repo", None, Some("created by Temps"), true)
            .await
            .expect("owned active connection creates repository");

        assert_eq!(repository.full_name, "owner/new-repo");
        assert_eq!(repository.default_branch, "main");
        create.assert_async().await;
    }

    #[test]
    fn test_parse_git_url_https_with_git_extension() {
        let url = "https://github.com/owner/repo.git";
        let result = parse_git_url(url);
        assert!(result.is_some());
        let (owner, repo) = result.unwrap();
        assert_eq!(owner, "owner");
        assert_eq!(repo, "repo");
    }

    #[test]
    fn test_parse_git_url_https_without_git_extension() {
        let url = "https://github.com/owner/repo";
        let result = parse_git_url(url);
        assert!(result.is_some());
        let (owner, repo) = result.unwrap();
        assert_eq!(owner, "owner");
        assert_eq!(repo, "repo");
    }

    #[test]
    fn test_parse_git_url_ssh_format() {
        let url = "git@github.com:owner/repo.git";
        let result = parse_git_url(url);
        assert!(result.is_some());
        let (owner, repo) = result.unwrap();
        assert_eq!(owner, "owner");
        assert_eq!(repo, "repo");
    }

    #[test]
    fn test_parse_git_url_ssh_without_git_extension() {
        let url = "git@gitlab.com:myorg/myrepo";
        let result = parse_git_url(url);
        assert!(result.is_some());
        let (owner, repo) = result.unwrap();
        assert_eq!(owner, "myorg");
        assert_eq!(repo, "myrepo");
    }

    #[test]
    fn test_parse_git_url_gitlab_https() {
        let url = "https://gitlab.com/group/project.git";
        let result = parse_git_url(url);
        assert!(result.is_some());
        let (owner, repo) = result.unwrap();
        assert_eq!(owner, "group");
        assert_eq!(repo, "project");
    }

    #[test]
    fn test_parse_git_url_with_nested_groups() {
        // For nested groups, we want the last two components
        let url = "https://gitlab.com/group/subgroup/project.git";
        let result = parse_git_url(url);
        assert!(result.is_some());
        let (owner, repo) = result.unwrap();
        assert_eq!(owner, "subgroup");
        assert_eq!(repo, "project");
    }

    #[test]
    fn test_parse_git_url_invalid_url() {
        let url = "not-a-valid-url";
        let result = parse_git_url(url);
        assert!(result.is_none());
    }

    #[test]
    fn test_extract_template_files_creates_files_from_archive() {
        use flate2::write::GzEncoder;
        use flate2::Compression;

        // Create a temporary directory and archive
        let temp_dir = tempfile::tempdir().unwrap();
        let archive_path = temp_dir.path().join("test.tar.gz");

        // Create a tar.gz archive with test files
        {
            let file = std::fs::File::create(&archive_path).unwrap();
            let encoder = GzEncoder::new(file, Compression::default());
            let mut builder = tar::Builder::new(encoder);

            // Add root directory entry first (like git archive does)
            let mut dir_header = tar::Header::new_gnu();
            dir_header.set_path("repo-main/").unwrap();
            dir_header.set_size(0);
            dir_header.set_mode(0o755);
            dir_header.set_entry_type(tar::EntryType::Directory);
            dir_header.set_cksum();
            builder.append(&dir_header, &[] as &[u8]).unwrap();

            // Add a file inside a root folder (mimicking git archive format)
            let content = b"Hello, World!";
            let mut header = tar::Header::new_gnu();
            header.set_path("repo-main/test.txt").unwrap();
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_entry_type(tar::EntryType::Regular);
            header.set_cksum();
            builder.append(&header, &content[..]).unwrap();

            // Add src directory
            let mut src_dir_header = tar::Header::new_gnu();
            src_dir_header.set_path("repo-main/src/").unwrap();
            src_dir_header.set_size(0);
            src_dir_header.set_mode(0o755);
            src_dir_header.set_entry_type(tar::EntryType::Directory);
            src_dir_header.set_cksum();
            builder.append(&src_dir_header, &[] as &[u8]).unwrap();

            // Add another file
            let content2 = b"console.log('test');";
            let mut header2 = tar::Header::new_gnu();
            header2.set_path("repo-main/src/index.js").unwrap();
            header2.set_size(content2.len() as u64);
            header2.set_mode(0o644);
            header2.set_entry_type(tar::EntryType::Regular);
            header2.set_cksum();
            builder.append(&header2, &content2[..]).unwrap();

            builder.finish().unwrap();
        }

        // Extract the files
        let files = extract_template_files(&archive_path, None).unwrap();

        assert_eq!(files.len(), 2);

        // Check that the root folder prefix was stripped
        let file_names: Vec<&str> = files.iter().map(|(name, _)| name.as_str()).collect();
        assert!(file_names.contains(&"test.txt"));
        assert!(file_names.contains(&"src/index.js"));

        // Verify content
        let test_file = files.iter().find(|(name, _)| name == "test.txt").unwrap();
        assert_eq!(test_file.1, b"Hello, World!");
    }

    #[test]
    fn test_extract_template_files_with_subfolder() {
        use flate2::write::GzEncoder;
        use flate2::Compression;

        let temp_dir = tempfile::tempdir().unwrap();
        let archive_path = temp_dir.path().join("test.tar.gz");

        // Create archive with multiple folders
        {
            let file = std::fs::File::create(&archive_path).unwrap();
            let encoder = GzEncoder::new(file, Compression::default());
            let mut builder = tar::Builder::new(encoder);

            // Add root directory entry first
            let mut root_dir = tar::Header::new_gnu();
            root_dir.set_path("repo-main/").unwrap();
            root_dir.set_size(0);
            root_dir.set_mode(0o755);
            root_dir.set_entry_type(tar::EntryType::Directory);
            root_dir.set_cksum();
            builder.append(&root_dir, &[] as &[u8]).unwrap();

            // Add files in different subfolders
            let content1 = b"Root file";
            let mut header1 = tar::Header::new_gnu();
            header1.set_path("repo-main/README.md").unwrap();
            header1.set_size(content1.len() as u64);
            header1.set_mode(0o644);
            header1.set_entry_type(tar::EntryType::Regular);
            header1.set_cksum();
            builder.append(&header1, &content1[..]).unwrap();

            // Add templates directory
            let mut templates_dir = tar::Header::new_gnu();
            templates_dir.set_path("repo-main/templates/").unwrap();
            templates_dir.set_size(0);
            templates_dir.set_mode(0o755);
            templates_dir.set_entry_type(tar::EntryType::Directory);
            templates_dir.set_cksum();
            builder.append(&templates_dir, &[] as &[u8]).unwrap();

            // Add template1 directory
            let mut template1_dir = tar::Header::new_gnu();
            template1_dir
                .set_path("repo-main/templates/template1/")
                .unwrap();
            template1_dir.set_size(0);
            template1_dir.set_mode(0o755);
            template1_dir.set_entry_type(tar::EntryType::Directory);
            template1_dir.set_cksum();
            builder.append(&template1_dir, &[] as &[u8]).unwrap();

            let content2 = b"Template 1 content";
            let mut header2 = tar::Header::new_gnu();
            header2
                .set_path("repo-main/templates/template1/index.js")
                .unwrap();
            header2.set_size(content2.len() as u64);
            header2.set_mode(0o644);
            header2.set_entry_type(tar::EntryType::Regular);
            header2.set_cksum();
            builder.append(&header2, &content2[..]).unwrap();

            let content3 = b"Template 1 config";
            let mut header3 = tar::Header::new_gnu();
            header3
                .set_path("repo-main/templates/template1/config.json")
                .unwrap();
            header3.set_size(content3.len() as u64);
            header3.set_mode(0o644);
            header3.set_entry_type(tar::EntryType::Regular);
            header3.set_cksum();
            builder.append(&header3, &content3[..]).unwrap();

            // Add template2 directory
            let mut template2_dir = tar::Header::new_gnu();
            template2_dir
                .set_path("repo-main/templates/template2/")
                .unwrap();
            template2_dir.set_size(0);
            template2_dir.set_mode(0o755);
            template2_dir.set_entry_type(tar::EntryType::Directory);
            template2_dir.set_cksum();
            builder.append(&template2_dir, &[] as &[u8]).unwrap();

            let content4 = b"Template 2 content";
            let mut header4 = tar::Header::new_gnu();
            header4
                .set_path("repo-main/templates/template2/main.py")
                .unwrap();
            header4.set_size(content4.len() as u64);
            header4.set_mode(0o644);
            header4.set_entry_type(tar::EntryType::Regular);
            header4.set_cksum();
            builder.append(&header4, &content4[..]).unwrap();

            builder.finish().unwrap();
        }

        // Extract only files from templates/template1 subfolder
        let files = extract_template_files(&archive_path, Some("templates/template1")).unwrap();

        assert_eq!(files.len(), 2);

        let file_names: Vec<&str> = files.iter().map(|(name, _)| name.as_str()).collect();
        assert!(file_names.contains(&"index.js"));
        assert!(file_names.contains(&"config.json"));

        // Verify the other template's files were not included
        assert!(!file_names.contains(&"main.py"));
        assert!(!file_names.contains(&"README.md"));
    }
}
