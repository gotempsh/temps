// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use git2::{
    BranchType, Cred, CredentialType, ErrorClass, ErrorCode, PushOptions, RemoteCallbacks,
    Repository, Signature,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::task;
use tracing::debug;

/// Why a local git operation failed. `reason` is libgit2's message with any URL
/// credentials removed: a remote configured as `https://token@host/...` is
/// quoted by the messages of the operations that talk to it.
#[derive(Debug, thiserror::Error)]
pub enum GitServiceError {
    #[error("Could not {operation} in repository '{}': {reason}", .repo_path.display())]
    NotFound {
        operation: &'static str,
        repo_path: PathBuf,
        reason: String,
    },

    #[error("The remote refused the credentials to {operation} in repository '{}': {reason}", .repo_path.display())]
    AuthFailed {
        operation: &'static str,
        repo_path: PathBuf,
        reason: String,
    },

    #[error("The network failed while trying to {operation} in repository '{}': {reason}", .repo_path.display())]
    Network {
        operation: &'static str,
        repo_path: PathBuf,
        reason: String,
    },

    #[error("Failed to {operation} in repository '{}': {reason}", .repo_path.display())]
    Git {
        operation: &'static str,
        repo_path: PathBuf,
        reason: String,
    },

    #[error("Failed to delete file '{file}' in repository '{}': {source}", .repo_path.display())]
    FileRemoval {
        repo_path: PathBuf,
        file: String,
        #[source]
        source: std::io::Error,
    },

    #[error(
        "Pulling branch '{branch}' into repository '{}' needs a normal merge, which is not implemented",
        .repo_path.display()
    )]
    MergeRequired { repo_path: PathBuf, branch: String },

    #[error("Reference '{reference}' in repository '{}' has no target", .repo_path.display())]
    MissingTarget {
        repo_path: PathBuf,
        reference: String,
    },

    #[error("New branch in repository '{}' has an invalid reference name: {reason}", .repo_path.display())]
    InvalidBranchReference { repo_path: PathBuf, reason: String },

    #[error("The task to {operation} in repository '{}' did not finish: {reason}", .repo_path.display())]
    TaskFailed {
        operation: &'static str,
        repo_path: PathBuf,
        reason: String,
    },
}

impl GitServiceError {
    /// Whether the same operation may succeed if run again later.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Network { .. } => true,
            Self::NotFound { .. }
            | Self::AuthFailed { .. }
            | Self::Git { .. }
            | Self::FileRemoval { .. }
            | Self::MergeRequired { .. }
            | Self::MissingTarget { .. }
            | Self::InvalidBranchReference { .. }
            | Self::TaskFailed { .. } => false,
        }
    }

    fn from_git2(operation: &'static str, repo_path: PathBuf, error: git2::Error) -> Self {
        let reason = scrub_url_credentials(error.message());
        match (error.code(), error.class()) {
            (ErrorCode::Auth, _) => Self::AuthFailed {
                operation,
                repo_path,
                reason,
            },
            (ErrorCode::NotFound, _) => Self::NotFound {
                operation,
                repo_path,
                reason,
            },
            (_, ErrorClass::Net | ErrorClass::Http | ErrorClass::Ssl | ErrorClass::Ssh) => {
                Self::Network {
                    operation,
                    repo_path,
                    reason,
                }
            }
            _ => Self::Git {
                operation,
                repo_path,
                reason,
            },
        }
    }
}

type Result<T, E = GitServiceError> = std::result::Result<T, E>;

/// A failure inside a blocking git task, before the operation and repository
/// that [`run_blocking`] knows are attached.
enum GitTaskError {
    Git(git2::Error),
    FileRemoval {
        file: String,
        source: std::io::Error,
    },
    MergeRequired {
        branch: String,
    },
    MissingTarget {
        reference: String,
    },
    InvalidBranchReference {
        reason: String,
    },
}

impl From<git2::Error> for GitTaskError {
    fn from(error: git2::Error) -> Self {
        Self::Git(error)
    }
}

/// Run blocking git work off the async runtime and name the operation and
/// repository in whatever it returns.
async fn run_blocking<T, F>(operation: &'static str, repo_path: &Path, work: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce(PathBuf) -> std::result::Result<T, GitTaskError> + Send + 'static,
{
    let repo_path = repo_path.to_path_buf();
    let task_path = repo_path.clone();
    let outcome = task::spawn_blocking(move || work(task_path)).await;
    match outcome {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(GitTaskError::Git(error))) => {
            Err(GitServiceError::from_git2(operation, repo_path, error))
        }
        Ok(Err(GitTaskError::FileRemoval { file, source })) => Err(GitServiceError::FileRemoval {
            repo_path,
            file,
            source,
        }),
        Ok(Err(GitTaskError::MergeRequired { branch })) => {
            Err(GitServiceError::MergeRequired { repo_path, branch })
        }
        Ok(Err(GitTaskError::MissingTarget { reference })) => Err(GitServiceError::MissingTarget {
            repo_path,
            reference,
        }),
        Ok(Err(GitTaskError::InvalidBranchReference { reason })) => {
            Err(GitServiceError::InvalidBranchReference { repo_path, reason })
        }
        Err(error) => Err(GitServiceError::TaskFailed {
            operation,
            repo_path,
            reason: error.to_string(),
        }),
    }
}

/// Remove the userinfo of every URL quoted in `message`.
fn scrub_url_credentials(message: &str) -> String {
    let mut scrubbed = String::with_capacity(message.len());
    let mut rest = message;
    while let Some(scheme_end) = rest.find("://") {
        let (head, tail) = rest.split_at(scheme_end + 3);
        scrubbed.push_str(head);
        let authority_end = tail
            .find(|c: char| c.is_whitespace() || matches!(c, '/' | '?' | '#' | '\'' | '"'))
            .unwrap_or(tail.len());
        let (authority, remainder) = tail.split_at(authority_end);
        match authority.rsplit_once('@') {
            Some((_, host)) => {
                scrubbed.push_str("***@");
                scrubbed.push_str(host);
            }
            None => scrubbed.push_str(authority),
        }
        rest = remainder;
    }
    scrubbed.push_str(rest);
    scrubbed
}

#[derive(Debug, Clone)]
pub struct GitService {
    // No need to store database connection here since we'll pass credentials per operation
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GitCredentials {
    pub username: String,
    pub personal_access_token: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GitFileStatus {
    pub path: String,
    pub status: String, // "A" = added, "M" = modified, "D" = deleted, "??" = untracked
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GitStatus {
    pub branch: String,
    pub ahead: u32,
    pub behind: u32,
    pub staged: Vec<GitFileStatus>,
    pub modified: Vec<GitFileStatus>,
    pub untracked: Vec<GitFileStatus>,
    pub has_uncommitted_changes: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BranchInfo {
    pub name: String,
    pub is_current: bool,
    pub is_remote: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CommitInfo {
    pub hash: String,
    pub abbreviated_hash: String,
    pub message: String,
    pub author_name: String,
    pub author_email: String,
    pub date: String,
}

impl Default for GitService {
    fn default() -> Self {
        Self::new()
    }
}

impl GitService {
    pub fn new() -> Self {
        Self {}
    }

    /// Get git status for a repository
    pub async fn get_status(&self, repo_path: &Path) -> Result<GitStatus> {
        run_blocking("read the status", repo_path, move |repo_path| {
            let repo = Repository::open(&repo_path)?;

            // Get current branch
            let head = repo.head()?;
            let branch = head.shorthand().unwrap_or("HEAD").to_string();

            // Get repository status
            let statuses = repo.statuses(None)?;

            let mut staged = Vec::new();
            let mut modified = Vec::new();
            let mut untracked = Vec::new();

            for status in statuses.iter() {
                let path = status.path().unwrap_or("").to_string();
                let flags = status.status();

                if flags.is_index_new() || flags.is_index_modified() || flags.is_index_deleted() {
                    staged.push(GitFileStatus {
                        path: path.clone(),
                        status: if flags.is_index_new() {
                            "A".to_string()
                        } else if flags.is_index_modified() {
                            "M".to_string()
                        } else if flags.is_index_deleted() {
                            "D".to_string()
                        } else {
                            "?".to_string()
                        },
                    });
                }

                if flags.is_wt_modified() || flags.is_wt_deleted() {
                    modified.push(GitFileStatus {
                        path: path.clone(),
                        status: if flags.is_wt_modified() {
                            "M".to_string()
                        } else {
                            "D".to_string()
                        },
                    });
                }

                if flags.is_wt_new() {
                    untracked.push(GitFileStatus {
                        path,
                        status: "??".to_string(),
                    });
                }
            }

            // Get ahead/behind count
            let (ahead, behind) = Self::get_ahead_behind_count(&repo, &branch)?;

            let has_uncommitted_changes =
                !staged.is_empty() || !modified.is_empty() || !untracked.is_empty();

            Ok(GitStatus {
                branch,
                ahead,
                behind,
                staged,
                modified,
                untracked,
                has_uncommitted_changes,
            })
        })
        .await
    }

    /// Add files to staging area
    pub async fn add_files(&self, repo_path: &Path, files: Option<Vec<String>>) -> Result<String> {
        run_blocking("stage files", repo_path, move |repo_path| {
            let repo = Repository::open(&repo_path)?;
            let mut index = repo.index()?;

            if let Some(file_list) = files {
                if file_list.is_empty() {
                    // Add all files
                    index.add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)?;
                } else {
                    // Add specific files
                    for file in file_list {
                        index.add_path(Path::new(&file))?;
                    }
                }
            } else {
                // Add all files
                index.add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)?;
            }

            index.write()?;
            Ok("Files added to staging area successfully".to_string())
        })
        .await
    }

    /// Remove files from staging area (unstage)
    pub async fn unstage_files(
        &self,
        repo_path: &Path,
        files: Option<Vec<String>>,
    ) -> Result<String> {
        run_blocking("unstage files", repo_path, move |repo_path| {
            let repo = Repository::open(&repo_path)?;

            // Get HEAD commit
            let head_commit = repo.head()?.peel_to_commit()?;
            let _head_tree = head_commit.tree()?;

            if let Some(file_list) = files {
                if file_list.is_empty() {
                    // Reset all files
                    repo.reset_default(Some(head_commit.as_object()), ["*"].iter())?;
                } else {
                    // Reset specific files
                    repo.reset_default(
                        Some(head_commit.as_object()),
                        file_list.iter().map(|s| s.as_str()),
                    )?;
                }
            } else {
                // Reset all files
                repo.reset_default(Some(head_commit.as_object()), ["*"].iter())?;
            }

            Ok("Files removed from staging area successfully".to_string())
        })
        .await
    }

    /// Remove files from repository
    pub async fn remove_files(
        &self,
        repo_path: &Path,
        files: Vec<String>,
        delete_from_disk: bool,
    ) -> Result<String> {
        run_blocking("remove files", repo_path, move |repo_path| {
            let repo = Repository::open(&repo_path)?;
            let mut index = repo.index()?;

            for file in &files {
                let file_path = Path::new(file);

                // Remove from index
                index.remove_path(file_path)?;

                // Remove from disk if requested
                if delete_from_disk {
                    let full_path = repo_path.join(file_path);
                    if full_path.exists() {
                        std::fs::remove_file(&full_path).map_err(|source| {
                            GitTaskError::FileRemoval {
                                file: file.clone(),
                                source,
                            }
                        })?;
                    }
                }
            }

            index.write()?;

            let action = if delete_from_disk {
                "removed from repository and disk"
            } else {
                "removed from repository (kept on disk)"
            };

            Ok(format!("Files {} successfully", action))
        })
        .await
    }

    /// Commit changes
    pub async fn commit(
        &self,
        repo_path: &Path,
        message: &str,
        author_name: &str,
        author_email: &str,
    ) -> Result<String> {
        let message = message.to_string();
        let author_name = author_name.to_string();
        let author_email = author_email.to_string();

        run_blocking("commit", repo_path, move |repo_path| {
            let repo = Repository::open(&repo_path)?;
            let signature = Signature::now(&author_name, &author_email)?;

            // Get the index and write the tree
            let mut index = repo.index()?;
            let tree_id = index.write_tree()?;
            let tree = repo.find_tree(tree_id)?;

            // Get parent commit
            let parent_commit = match repo.head() {
                Ok(head) => Some(head.peel_to_commit()?),
                Err(_) => None, // Initial commit
            };

            // Create commit
            let parents = if let Some(ref parent) = parent_commit {
                vec![parent]
            } else {
                vec![]
            };

            let commit_id = repo.commit(
                Some("HEAD"),
                &signature,
                &signature,
                &message,
                &tree,
                &parents,
            )?;

            Ok(format!("Commit created: {}", commit_id))
        })
        .await
    }

    /// Pull latest changes from remote
    pub async fn pull(
        &self,
        repo_path: &Path,
        branch: &str,
        credentials: GitCredentials,
    ) -> Result<String> {
        let branch = branch.to_string();

        run_blocking("pull", repo_path, move |repo_path| {
            let repo = Repository::open(&repo_path)?;

            // Set up credentials
            let mut cb = RemoteCallbacks::new();
            cb.credentials(|_url, _username_from_url, allowed_types| {
                if allowed_types.contains(CredentialType::USER_PASS_PLAINTEXT) {
                    Cred::userpass_plaintext(
                        &credentials.username,
                        &credentials.personal_access_token,
                    )
                } else {
                    Cred::default()
                }
            });

            // Get remote
            let mut remote = repo.find_remote("origin")?;

            // Fetch
            let mut fetch_options = git2::FetchOptions::new();
            fetch_options.remote_callbacks(cb);
            remote.fetch(&[&branch], Some(&mut fetch_options), None)?;

            // Get the fetch head and target commit
            repo.fetchhead_foreach(|ref_name, remote_url, oid, _is_merge| {
                debug!(
                    "Fetched {}: {} from {}",
                    ref_name,
                    oid,
                    scrub_url_credentials(&String::from_utf8_lossy(remote_url))
                );
                true
            })?;

            // Merge the fetched changes
            let fetch_commit =
                repo.reference_to_annotated_commit(&repo.find_reference("FETCH_HEAD")?)?;
            let analysis = repo.merge_analysis(&[&fetch_commit])?;

            if analysis.0.is_fast_forward() {
                let refname = format!("refs/heads/{}", branch);
                let mut reference = repo.find_reference(&refname)?;
                reference.set_target(fetch_commit.id(), "Fast-forward")?;
                repo.set_head(&refname)?;
                repo.checkout_head(Some(git2::build::CheckoutBuilder::default().force()))?;
                Ok("Fast-forward merge completed".to_string())
            } else if analysis.0.is_normal() {
                // Normal merge would require more complex logic
                Err(GitTaskError::MergeRequired { branch })
            } else {
                Ok("Already up to date".to_string())
            }
        })
        .await
    }

    /// Push changes to remote
    pub async fn push(
        &self,
        repo_path: &Path,
        branch: &str,
        credentials: GitCredentials,
    ) -> Result<String> {
        let branch = branch.to_string();

        run_blocking("push", repo_path, move |repo_path| {
            let repo = Repository::open(&repo_path)?;

            // Set up credentials
            let mut cb = RemoteCallbacks::new();
            cb.credentials(|_url, _username_from_url, allowed_types| {
                if allowed_types.contains(CredentialType::USER_PASS_PLAINTEXT) {
                    Cred::userpass_plaintext(
                        &credentials.username,
                        &credentials.personal_access_token,
                    )
                } else {
                    Cred::default()
                }
            });

            // Get remote
            let mut remote = repo.find_remote("origin")?;

            // Push
            let refspec = format!("refs/heads/{}:refs/heads/{}", branch, branch);
            let mut push_options = PushOptions::new();
            push_options.remote_callbacks(cb);
            remote.push(&[&refspec], Some(&mut push_options))?;

            Ok(format!("Successfully pushed to {}", branch))
        })
        .await
    }

    /// Get commit history
    pub async fn get_log(&self, repo_path: &Path, limit: usize) -> Result<Vec<CommitInfo>> {
        run_blocking("read the commit log", repo_path, move |repo_path| {
            let repo = Repository::open(&repo_path)?;
            let mut revwalk = repo.revwalk()?;
            revwalk.push_head()?;
            revwalk.set_sorting(git2::Sort::TIME)?;

            let mut commits = Vec::new();

            for (count, oid_result) in revwalk.enumerate() {
                if count >= limit {
                    break;
                }

                let oid = oid_result?;
                let commit = repo.find_commit(oid)?;

                let hash = format!("{}", oid);
                let abbreviated_hash = format!("{:.7}", oid);
                let message = commit.message().unwrap_or("").to_string();
                let author = commit.author();
                let author_name = author.name().unwrap_or("").to_string();
                let author_email = author.email().unwrap_or("").to_string();
                let timestamp = author.when();
                let date = chrono::DateTime::<chrono::Utc>::from_timestamp(timestamp.seconds(), 0)
                    .unwrap_or_default()
                    .to_rfc3339();

                commits.push(CommitInfo {
                    hash,
                    abbreviated_hash,
                    message,
                    author_name,
                    author_email,
                    date,
                });
            }

            Ok(commits)
        })
        .await
    }

    /// Get list of branches
    pub async fn get_branches(&self, repo_path: &Path) -> Result<Vec<BranchInfo>> {
        run_blocking("list branches", repo_path, move |repo_path| {
            let repo = Repository::open(&repo_path)?;
            let mut branches = Vec::new();

            // Get current branch
            let current_branch = repo.head()?.shorthand().unwrap_or("").to_string();

            // Local branches
            let local_branches = repo.branches(Some(BranchType::Local))?;
            for branch_result in local_branches {
                let (branch, _) = branch_result?;
                if let Some(name) = branch.name()? {
                    branches.push(BranchInfo {
                        name: name.to_string(),
                        is_current: name == current_branch,
                        is_remote: false,
                    });
                }
            }

            // Remote branches
            let remote_branches = repo.branches(Some(BranchType::Remote))?;
            for branch_result in remote_branches {
                let (branch, _) = branch_result?;
                if let Some(name) = branch.name()? {
                    branches.push(BranchInfo {
                        name: name.to_string(),
                        is_current: false,
                        is_remote: true,
                    });
                }
            }

            Ok(branches)
        })
        .await
    }

    /// Create a new branch
    pub async fn create_branch(
        &self,
        repo_path: &Path,
        branch_name: &str,
        from_branch: Option<&str>,
    ) -> Result<String> {
        let branch_name = branch_name.to_string();
        let from_branch = from_branch.map(|s| s.to_string());

        run_blocking("create a branch", repo_path, move |repo_path| {
            let repo = Repository::open(&repo_path)?;

            // Get the commit to branch from
            let target_commit = if let Some(from) = from_branch {
                // Branch from specified branch
                let from_ref = repo.find_reference(&format!("refs/heads/{}", from))?;
                from_ref.peel_to_commit()?
            } else {
                // Branch from HEAD
                repo.head()?.peel_to_commit()?
            };

            // Create the branch
            let branch = repo.branch(&branch_name, &target_commit, false)?;

            // Switch to the new branch
            let branch_ref =
                branch
                    .get()
                    .name()
                    .map_err(|e| GitTaskError::InvalidBranchReference {
                        reason: e.to_string(),
                    })?;
            repo.set_head(branch_ref)?;
            repo.checkout_head(None)?;

            Ok(format!("Created and switched to branch: {}", branch_name))
        })
        .await
    }

    /// Switch to a different branch
    pub async fn switch_branch(&self, repo_path: &Path, branch_name: &str) -> Result<String> {
        let branch_name = branch_name.to_string();

        run_blocking("switch branch", repo_path, move |repo_path| {
            let repo = Repository::open(&repo_path)?;

            // Find the branch
            let branch_ref = format!("refs/heads/{}", branch_name);
            let _reference = repo.find_reference(&branch_ref)?;

            // Switch to the branch
            repo.set_head(&branch_ref)?;
            repo.checkout_head(None)?;

            Ok(format!("Switched to branch: {}", branch_name))
        })
        .await
    }

    // Helper function to get ahead/behind count
    fn get_ahead_behind_count(
        repo: &Repository,
        branch: &str,
    ) -> std::result::Result<(u32, u32), GitTaskError> {
        let local_ref = format!("refs/heads/{}", branch);
        let remote_ref = format!("refs/remotes/origin/{}", branch);

        let local_oid = match repo.find_reference(&local_ref) {
            Ok(reference) => reference
                .target()
                .ok_or_else(|| GitTaskError::MissingTarget {
                    reference: local_ref.clone(),
                })?,
            Err(_) => return Ok((0, 0)),
        };

        let remote_oid = match repo.find_reference(&remote_ref) {
            Ok(reference) => reference
                .target()
                .ok_or_else(|| GitTaskError::MissingTarget {
                    reference: remote_ref.clone(),
                })?,
            Err(_) => return Ok((0, 0)),
        };

        let (ahead, behind) = repo.graph_ahead_behind(local_oid, remote_oid)?;
        Ok((ahead as u32, behind as u32))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repository() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let path = dir.path().to_path_buf();
        Repository::init(&path).expect("init");
        (dir, path)
    }

    async fn commit_file(service: &GitService, path: &Path, name: &str) {
        std::fs::write(path.join(name), "content").expect("write");
        service
            .add_files(path, Some(vec![name.to_string()]))
            .await
            .expect("stage");
        service
            .commit(path, "a commit", "Test", "test@example.com")
            .await
            .expect("commit");
    }

    #[tokio::test]
    async fn local_operations_still_work_through_the_blocking_wrapper() {
        let (_dir, path) = repository();
        let service = GitService::new();
        commit_file(&service, &path, "a.txt").await;

        let status = service.get_status(&path).await.expect("status");
        assert!(!status.has_uncommitted_changes);
        assert_eq!(service.get_log(&path, 10).await.expect("log").len(), 1);

        service
            .create_branch(&path, "feature", None)
            .await
            .expect("create branch");
        let branches = service.get_branches(&path).await.expect("branches");
        assert!(branches.iter().any(|b| b.name == "feature" && b.is_current));
    }

    #[tokio::test]
    async fn a_missing_repository_is_not_found_and_names_the_path() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let missing = dir.path().join("no-such-repository");
        let error = GitService::new()
            .get_status(&missing)
            .await
            .expect_err("nothing there");
        assert!(
            matches!(
                &error,
                GitServiceError::NotFound { operation: "read the status", repo_path, .. }
                    if *repo_path == missing
            ),
            "{error:?}"
        );
        assert!(!error.is_retryable());
    }

    #[tokio::test]
    async fn a_missing_branch_is_not_found() {
        let (_dir, path) = repository();
        let service = GitService::new();
        commit_file(&service, &path, "a.txt").await;
        let error = service
            .switch_branch(&path, "no-such-branch")
            .await
            .expect_err("the branch does not exist");
        assert!(
            matches!(error, GitServiceError::NotFound { .. }),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn a_file_that_cannot_be_deleted_is_named() {
        let (_dir, path) = repository();
        let service = GitService::new();
        commit_file(&service, &path, "a.txt").await;
        // A directory in place of the tracked file makes the delete fail.
        std::fs::remove_file(path.join("a.txt")).expect("remove");
        std::fs::create_dir(path.join("a.txt")).expect("mkdir");

        let error = service
            .remove_files(&path, vec!["a.txt".to_string()], true)
            .await
            .expect_err("a directory is not removed as a file");
        assert!(
            matches!(&error, GitServiceError::FileRemoval { file, .. } if file == "a.txt"),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn a_panicking_git_task_is_reported_with_the_operation() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let error = run_blocking::<(), _>("read the status", dir.path(), |_| {
            panic!("synthetic task panic")
        })
        .await
        .expect_err("the task panicked");
        assert!(
            matches!(
                error,
                GitServiceError::TaskFailed {
                    operation: "read the status",
                    ..
                }
            ),
            "{error:?}"
        );
    }

    #[test]
    fn libgit2_errors_classify_into_what_a_caller_acts_on() {
        let classify = |code, class| {
            GitServiceError::from_git2(
                "push",
                PathBuf::from("/repo"),
                git2::Error::new(code, class, "synthetic"),
            )
        };

        let refused = classify(ErrorCode::Auth, ErrorClass::Http);
        assert!(matches!(refused, GitServiceError::AuthFailed { .. }));
        assert!(!refused.is_retryable());

        let missing = classify(ErrorCode::NotFound, ErrorClass::Reference);
        assert!(matches!(missing, GitServiceError::NotFound { .. }));
        assert!(!missing.is_retryable());

        for class in [
            ErrorClass::Net,
            ErrorClass::Http,
            ErrorClass::Ssl,
            ErrorClass::Ssh,
        ] {
            let network = classify(ErrorCode::GenericError, class);
            assert!(matches!(network, GitServiceError::Network { .. }));
            assert!(network.is_retryable());
        }

        let other = classify(ErrorCode::Conflict, ErrorClass::Checkout);
        assert!(matches!(other, GitServiceError::Git { .. }));
        assert!(!other.is_retryable());
    }

    #[test]
    fn git_messages_lose_url_credentials() {
        let error = GitServiceError::from_git2(
            "push",
            PathBuf::from("/repo"),
            git2::Error::new(
                ErrorCode::GenericError,
                ErrorClass::Net,
                "failed to connect to https://x-access-token:hunter2@github.example.test/o/r.git: refused",
            ),
        );
        let message = error.to_string();
        assert!(!message.contains("hunter2"), "{message}");
        assert!(
            message.contains("https://***@github.example.test/o/r.git: refused"),
            "{message}"
        );

        assert_eq!(
            scrub_url_credentials("no url, and an@address.example.test"),
            "no url, and an@address.example.test"
        );
        assert_eq!(
            scrub_url_credentials("'https://token@a.test' then ssh://b.test/x"),
            "'https://***@a.test' then ssh://b.test/x"
        );
    }

    #[test]
    fn typed_messages_name_the_repository() {
        let repo_path = PathBuf::from("/repo");
        assert_eq!(
            GitServiceError::MergeRequired {
                repo_path: repo_path.clone(),
                branch: "main".to_string(),
            }
            .to_string(),
            "Pulling branch 'main' into repository '/repo' needs a normal merge, which is not implemented"
        );
        assert_eq!(
            GitServiceError::MissingTarget {
                repo_path,
                reference: "refs/heads/main".to_string(),
            }
            .to_string(),
            "Reference 'refs/heads/main' in repository '/repo' has no target"
        );
    }
}
