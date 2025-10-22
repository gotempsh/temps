use anyhow::{anyhow, Result};
use git2::{BranchType, Cred, CredentialType, PushOptions, RemoteCallbacks, Repository, Signature};
use serde::{Deserialize, Serialize};
use std::path::Path;
use tokio::task;
use tracing::debug;

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
        let repo_path = repo_path.to_path_buf();

        task::spawn_blocking(move || {
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
        .await?
    }

    /// Add files to staging area
    pub async fn add_files(&self, repo_path: &Path, files: Option<Vec<String>>) -> Result<String> {
        let repo_path = repo_path.to_path_buf();

        task::spawn_blocking(move || {
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
        .await?
    }

    /// Remove files from staging area (unstage)
    pub async fn unstage_files(
        &self,
        repo_path: &Path,
        files: Option<Vec<String>>,
    ) -> Result<String> {
        let repo_path = repo_path.to_path_buf();

        task::spawn_blocking(move || {
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
        .await?
    }

    /// Remove files from repository
    pub async fn remove_files(
        &self,
        repo_path: &Path,
        files: Vec<String>,
        delete_from_disk: bool,
    ) -> Result<String> {
        let repo_path = repo_path.to_path_buf();

        task::spawn_blocking(move || {
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
                        std::fs::remove_file(&full_path)
                            .map_err(|e| anyhow!("Failed to delete file {}: {}", file, e))?;
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
        .await?
    }

    /// Commit changes
    pub async fn commit(
        &self,
        repo_path: &Path,
        message: &str,
        author_name: &str,
        author_email: &str,
    ) -> Result<String> {
        let repo_path = repo_path.to_path_buf();
        let message = message.to_string();
        let author_name = author_name.to_string();
        let author_email = author_email.to_string();

        task::spawn_blocking(move || {
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
        .await?
    }

    /// Pull latest changes from remote
    pub async fn pull(
        &self,
        repo_path: &Path,
        branch: &str,
        credentials: GitCredentials,
    ) -> Result<String> {
        let repo_path = repo_path.to_path_buf();
        let branch = branch.to_string();

        task::spawn_blocking(move || {
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
                    String::from_utf8_lossy(remote_url)
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
                Err(anyhow!("Normal merge required - not implemented yet"))
            } else {
                Ok("Already up to date".to_string())
            }
        })
        .await?
    }

    /// Push changes to remote
    pub async fn push(
        &self,
        repo_path: &Path,
        branch: &str,
        credentials: GitCredentials,
    ) -> Result<String> {
        let repo_path = repo_path.to_path_buf();
        let branch = branch.to_string();

        task::spawn_blocking(move || {
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
        .await?
    }

    /// Get commit history
    pub async fn get_log(&self, repo_path: &Path, limit: usize) -> Result<Vec<CommitInfo>> {
        let repo_path = repo_path.to_path_buf();

        task::spawn_blocking(move || {
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
        .await?
    }

    /// Get list of branches
    pub async fn get_branches(&self, repo_path: &Path) -> Result<Vec<BranchInfo>> {
        let repo_path = repo_path.to_path_buf();

        task::spawn_blocking(move || {
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
        .await?
    }

    /// Create a new branch
    pub async fn create_branch(
        &self,
        repo_path: &Path,
        branch_name: &str,
        from_branch: Option<&str>,
    ) -> Result<String> {
        let repo_path = repo_path.to_path_buf();
        let branch_name = branch_name.to_string();
        let from_branch = from_branch.map(|s| s.to_string());

        task::spawn_blocking(move || {
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
            let branch_ref = branch
                .get()
                .name()
                .ok_or_else(|| anyhow!("Invalid branch reference"))?;
            repo.set_head(branch_ref)?;
            repo.checkout_head(None)?;

            Ok(format!("Created and switched to branch: {}", branch_name))
        })
        .await?
    }

    /// Switch to a different branch
    pub async fn switch_branch(&self, repo_path: &Path, branch_name: &str) -> Result<String> {
        let repo_path = repo_path.to_path_buf();
        let branch_name = branch_name.to_string();

        task::spawn_blocking(move || {
            let repo = Repository::open(&repo_path)?;

            // Find the branch
            let branch_ref = format!("refs/heads/{}", branch_name);
            let _reference = repo.find_reference(&branch_ref)?;

            // Switch to the branch
            repo.set_head(&branch_ref)?;
            repo.checkout_head(None)?;

            Ok(format!("Switched to branch: {}", branch_name))
        })
        .await?
    }

    // Helper function to get ahead/behind count
    fn get_ahead_behind_count(repo: &Repository, branch: &str) -> Result<(u32, u32)> {
        let local_ref = format!("refs/heads/{}", branch);
        let remote_ref = format!("refs/remotes/origin/{}", branch);

        let local_oid = match repo.find_reference(&local_ref) {
            Ok(reference) => reference
                .target()
                .ok_or_else(|| anyhow!("No target for local branch"))?,
            Err(_) => return Ok((0, 0)),
        };

        let remote_oid = match repo.find_reference(&remote_ref) {
            Ok(reference) => reference
                .target()
                .ok_or_else(|| anyhow!("No target for remote branch"))?,
            Err(_) => return Ok((0, 0)),
        };

        let (ahead, behind) = repo.graph_ahead_behind(local_oid, remote_oid)?;
        Ok((ahead as u32, behind as u32))
    }
}
