//! Git operations using libgit2 (git2 crate).
//!
//! Provides safe, typed wrappers around common git operations
//! to replace raw `Command::new("git")` shell calls.

use git2::{build::RepoBuilder, Cred, FetchOptions, RemoteCallbacks, Repository};
use std::path::Path;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum GitOpsError {
    #[error("Failed to clone repository from {url}: {reason}")]
    CloneFailed { url: String, reason: String },

    #[error("Failed to checkout ref '{ref_name}' in {repo_path}: {reason}")]
    CheckoutFailed {
        ref_name: String,
        repo_path: String,
        reason: String,
    },

    #[error("Failed to create branch '{branch_name}' in {repo_path}: {reason}")]
    CreateBranchFailed {
        branch_name: String,
        repo_path: String,
        reason: String,
    },
}

/// Clone a repository (public, no auth) into `target_dir`.
///
/// If `branch` is provided, clones only that branch.
/// If `shallow` is true, clones with depth=1 (not supported by local transport).
/// If `branch` is None, clones full history (needed for commit SHA checkout).
pub fn clone_repo(
    url: &str,
    target_dir: &Path,
    branch: Option<&str>,
) -> Result<Repository, GitOpsError> {
    clone_repo_inner(url, target_dir, branch, true)
}

fn clone_repo_inner(
    url: &str,
    target_dir: &Path,
    branch: Option<&str>,
    shallow: bool,
) -> Result<Repository, GitOpsError> {
    let mut builder = RepoBuilder::new();

    let mut fetch_opts = FetchOptions::new();

    if let Some(branch) = branch {
        builder.branch(branch);
        if shallow {
            fetch_opts.depth(1);
        }
    }

    builder.fetch_options(fetch_opts);

    builder
        .clone(url, target_dir)
        .map_err(|e| GitOpsError::CloneFailed {
            url: url.to_string(),
            reason: e.message().to_string(),
        })
}

/// Clone a repository with HTTPS token authentication.
///
/// The token is injected via git2's credential callback rather than
/// modifying the URL, which is safer and avoids leaking tokens in logs.
///
/// `username` controls the HTTPS auth username:
/// - GitHub: "x-access-token"
/// - GitLab: "oauth2"
/// - Generic: any username your provider expects
pub fn clone_repo_with_token(
    url: &str,
    target_dir: &Path,
    token: &str,
    branch: Option<&str>,
) -> Result<Repository, GitOpsError> {
    clone_repo_with_credentials(url, target_dir, "x-access-token", token, branch)
}

/// Clone a repository with custom username + token authentication.
pub fn clone_repo_with_credentials(
    url: &str,
    target_dir: &Path,
    username: &str,
    token: &str,
    branch: Option<&str>,
) -> Result<Repository, GitOpsError> {
    let username = username.to_string();
    let token = token.to_string();
    let mut builder = RepoBuilder::new();

    let mut callbacks = RemoteCallbacks::new();
    callbacks.credentials(move |_url, _username_from_url, _allowed_types| {
        Cred::userpass_plaintext(&username, &token)
    });

    let mut fetch_opts = FetchOptions::new();
    fetch_opts.remote_callbacks(callbacks);

    if let Some(branch) = branch {
        builder.branch(branch);
        fetch_opts.depth(1);
    }

    builder.fetch_options(fetch_opts);

    builder
        .clone(url, target_dir)
        .map_err(|e| GitOpsError::CloneFailed {
            url: url.to_string(),
            reason: e.message().to_string(),
        })
}

/// Create a new local branch at HEAD and check it out. Equivalent to
/// `git checkout -b <branch_name>`. Used by workspace sessions to fork a
/// new branch off a base branch (typically `main`) without touching the
/// remote — the branch is purely local until something pushes it.
///
/// Fails if a branch with the same name already exists locally.
pub fn create_and_checkout_branch(repo: &Repository, branch_name: &str) -> Result<(), GitOpsError> {
    let repo_path = repo
        .path()
        .parent()
        .map(|p| p.display().to_string())
        .unwrap_or_default();

    // Resolve HEAD to a commit
    let head = repo.head().map_err(|e| GitOpsError::CreateBranchFailed {
        branch_name: branch_name.to_string(),
        repo_path: repo_path.clone(),
        reason: format!("could not resolve HEAD: {}", e.message()),
    })?;
    let commit = head
        .peel_to_commit()
        .map_err(|e| GitOpsError::CreateBranchFailed {
            branch_name: branch_name.to_string(),
            repo_path: repo_path.clone(),
            reason: format!("HEAD does not point to a commit: {}", e.message()),
        })?;

    // Create the branch (force=false: error if it already exists)
    repo.branch(branch_name, &commit, false)
        .map_err(|e| GitOpsError::CreateBranchFailed {
            branch_name: branch_name.to_string(),
            repo_path: repo_path.clone(),
            reason: e.message().to_string(),
        })?;

    // Point HEAD at the new branch
    let ref_name = format!("refs/heads/{}", branch_name);
    repo.set_head(&ref_name)
        .map_err(|e| GitOpsError::CreateBranchFailed {
            branch_name: branch_name.to_string(),
            repo_path: repo_path.clone(),
            reason: format!("could not set HEAD to new branch: {}", e.message()),
        })?;

    Ok(())
}

/// Convenience wrapper: open the repo at `repo_path` and create+checkout
/// a new local branch off HEAD. This avoids callers having to depend on
/// `git2` directly.
pub fn create_and_checkout_branch_at(
    repo_path: &Path,
    branch_name: &str,
) -> Result<(), GitOpsError> {
    let repo = Repository::open(repo_path).map_err(|e| GitOpsError::CreateBranchFailed {
        branch_name: branch_name.to_string(),
        repo_path: repo_path.display().to_string(),
        reason: format!("could not open repo: {}", e.message()),
    })?;
    create_and_checkout_branch(&repo, branch_name)
}

/// Checkout a specific ref (branch, tag, or commit SHA) in an existing repository.
///
/// For commit SHAs, performs a detached HEAD checkout.
/// For branches/tags, resolves the reference and checks out.
pub fn checkout_ref(repo: &Repository, ref_name: &str) -> Result<(), GitOpsError> {
    let repo_path = repo
        .path()
        .parent()
        .map(|p| p.display().to_string())
        .unwrap_or_default();

    // Reject the all-zeros "null SHA". Git uses it to signal an absent ref
    // (e.g. branch/tag deletion webhooks), so it can never resolve to a commit.
    // Fail with an actionable message instead of an opaque libgit2 error.
    if !ref_name.is_empty() && ref_name.chars().all(|c| c == '0') {
        return Err(GitOpsError::CheckoutFailed {
            ref_name: ref_name.to_string(),
            repo_path,
            reason: "ref is the all-zeros null SHA, which corresponds to a deleted branch/tag and \
                     has no commit to check out"
                .to_string(),
        });
    }

    // Try to resolve as a commit SHA first (full or abbreviated)
    let object = repo
        .revparse_single(ref_name)
        .map_err(|e| GitOpsError::CheckoutFailed {
            ref_name: ref_name.to_string(),
            repo_path: repo_path.clone(),
            reason: e.message().to_string(),
        })?;

    let commit = object
        .peel_to_commit()
        .map_err(|e| GitOpsError::CheckoutFailed {
            ref_name: ref_name.to_string(),
            repo_path: repo_path.clone(),
            reason: format!("ref does not point to a commit: {}", e.message()),
        })?;

    // Checkout the tree
    repo.checkout_tree(commit.as_object(), None)
        .map_err(|e| GitOpsError::CheckoutFailed {
            ref_name: ref_name.to_string(),
            repo_path: repo_path.clone(),
            reason: e.message().to_string(),
        })?;

    // Set HEAD to the commit (detached HEAD)
    repo.set_head_detached(commit.id())
        .map_err(|e| GitOpsError::CheckoutFailed {
            ref_name: ref_name.to_string(),
            repo_path: repo_path.clone(),
            reason: e.message().to_string(),
        })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::Signature;
    use tempfile::TempDir;

    /// Create a local test repo with a commit, useful for testing checkout_ref
    /// without network access.
    fn create_test_repo() -> (TempDir, Repository) {
        let temp_dir = TempDir::new().unwrap();
        let repo = Repository::init(temp_dir.path()).unwrap();
        let sig = Signature::now("Test", "test@test.com").unwrap();

        // Create an initial commit with empty tree
        {
            let tree_id = repo.index().unwrap().write_tree().unwrap();
            let tree = repo.find_tree(tree_id).unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "initial commit", &tree, &[])
                .unwrap();
        }

        // Create a second commit with a file
        std::fs::write(temp_dir.path().join("file.txt"), "hello").unwrap();
        {
            let mut index = repo.index().unwrap();
            index.add_path(std::path::Path::new("file.txt")).unwrap();
            index.write().unwrap();
            let tree_id = index.write_tree().unwrap();
            let tree = repo.find_tree(tree_id).unwrap();
            let head = repo.head().unwrap().peel_to_commit().unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "add file", &tree, &[&head])
                .unwrap();
        }

        (temp_dir, repo)
    }

    #[test]
    fn test_checkout_ref_by_commit_sha() {
        let (_temp_dir, repo) = create_test_repo();

        // Get the first commit SHA
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        let first_commit = head.parent(0).unwrap();
        let sha = first_commit.id().to_string();

        let result = checkout_ref(&repo, &sha);
        assert!(result.is_ok(), "Checkout failed: {:?}", result.err());

        // Verify HEAD is detached at the first commit
        assert!(repo.head_detached().unwrap());
        let new_head = repo.head().unwrap().peel_to_commit().unwrap();
        assert_eq!(new_head.id(), first_commit.id());
    }

    #[test]
    fn test_checkout_ref_by_branch_name() {
        let (temp_dir, repo) = create_test_repo();

        // Create a branch at the first commit
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        let first_commit = head.parent(0).unwrap();
        repo.branch("test-branch", &first_commit, false).unwrap();

        // Checkout the branch by name
        let result = checkout_ref(&repo, "test-branch");
        assert!(result.is_ok(), "Checkout failed: {:?}", result.err());

        // Verify file.txt doesn't exist (first commit had no files)
        assert!(!temp_dir.path().join("file.txt").exists());
    }

    #[test]
    fn test_checkout_invalid_ref_returns_error() {
        let (_temp_dir, repo) = create_test_repo();

        let result = checkout_ref(&repo, "nonexistent-ref-xyz");
        assert!(result.is_err());
        match result.unwrap_err() {
            GitOpsError::CheckoutFailed { ref_name, .. } => {
                assert_eq!(ref_name, "nonexistent-ref-xyz");
            }
            other => panic!("Expected CheckoutFailed, got {:?}", other),
        }
    }

    #[test]
    fn test_checkout_null_sha_returns_descriptive_error() {
        let (_temp_dir, repo) = create_test_repo();

        // The all-zeros null SHA (sent on branch/tag deletion) must fail with a
        // descriptive reason, not an opaque libgit2 "object not found" message.
        let null_sha = "0000000000000000000000000000000000000000";
        let result = checkout_ref(&repo, null_sha);
        assert!(result.is_err());
        match result.unwrap_err() {
            GitOpsError::CheckoutFailed {
                ref_name, reason, ..
            } => {
                assert_eq!(ref_name, null_sha);
                assert!(
                    reason.contains("null SHA"),
                    "reason should explain the null SHA, got: {reason}"
                );
            }
            other => panic!("Expected CheckoutFailed, got {:?}", other),
        }

        // Abbreviated null SHA (the "0000000" the UI displays) is rejected too.
        let result = checkout_ref(&repo, "0000000");
        assert!(matches!(
            result.unwrap_err(),
            GitOpsError::CheckoutFailed { .. }
        ));
    }

    #[test]
    fn test_clone_local_repo() {
        // Create a source repo, then clone it locally (no network needed)
        let (source_dir, _source_repo) = create_test_repo();
        let target_dir = TempDir::new().unwrap();

        let source_url = format!("file://{}", source_dir.path().display());
        let result = clone_repo(&source_url, target_dir.path(), None);
        assert!(result.is_ok(), "Clone failed: {:?}", result.err());

        let cloned_repo = result.unwrap();
        assert!(cloned_repo.head().is_ok());
        assert!(target_dir.path().join("file.txt").exists());
    }

    #[test]
    fn test_clone_local_repo_with_branch() {
        let (source_dir, source_repo) = create_test_repo();

        // Create a branch in the source repo
        let head = source_repo.head().unwrap().peel_to_commit().unwrap();
        source_repo.branch("feature", &head, false).unwrap();

        let target_dir = TempDir::new().unwrap();
        let source_url = format!("file://{}", source_dir.path().display());
        // Use shallow=false since local transport doesn't support shallow fetch
        let result = clone_repo_inner(&source_url, target_dir.path(), Some("feature"), false);
        assert!(result.is_ok(), "Clone failed: {:?}", result.err());
    }

    #[test]
    fn test_clone_invalid_path_returns_error() {
        let target_dir = TempDir::new().unwrap();
        let result = clone_repo("file:///nonexistent/path/to/repo", target_dir.path(), None);
        match result {
            Err(GitOpsError::CloneFailed { url, .. }) => {
                assert!(url.contains("nonexistent"));
            }
            Err(other) => panic!("Expected CloneFailed, got {:?}", other),
            Ok(_) => panic!("Expected error, got Ok"),
        }
    }

    #[test]
    fn test_clone_and_checkout_commit() {
        let (source_dir, _source_repo) = create_test_repo();
        let target_dir = TempDir::new().unwrap();

        let source_url = format!("file://{}", source_dir.path().display());
        let repo = clone_repo(&source_url, target_dir.path(), None).unwrap();

        // Get HEAD and checkout its parent by SHA
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        let first_commit = head.parent(0).unwrap();
        let sha = first_commit.id().to_string();

        let result = checkout_ref(&repo, &sha);
        assert!(result.is_ok(), "Checkout failed: {:?}", result.err());

        // file.txt shouldn't exist after checking out the first commit
        assert!(!target_dir.path().join("file.txt").exists());
    }
}
