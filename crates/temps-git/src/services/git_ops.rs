// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Git operations using libgit2 (git2 crate).
//!
//! Provides safe, typed wrappers around common git operations
//! to replace raw `Command::new("git")` shell calls.

use base64::Engine;
use git2::{build::RepoBuilder, Cred, FetchOptions, RemoteCallbacks, Repository};
use std::path::Path;
use std::process::Stdio;
use thiserror::Error;

#[cfg(unix)]
use tokio::io::AsyncReadExt;

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

    #[error("Sparse checkout of '{subdirectory}' from {url} failed: {reason}")]
    SparseCloneFailed {
        url: String,
        subdirectory: String,
        reason: String,
    },
}

fn clone_failed(url: &str, reason: String) -> GitOpsError {
    GitOpsError::CloneFailed {
        url: temps_core::url_validation::redact_url_password(url),
        reason,
    }
}

/// Snapshot of a clone's network transfer, surfaced from libgit2's
/// `transfer_progress` callback. All counts are cumulative for the fetch.
#[derive(Debug, Clone, Copy)]
pub struct CloneProgress {
    /// Objects received so far.
    pub received_objects: usize,
    /// Total objects the remote advertised (0 until known).
    pub total_objects: usize,
    /// Objects already indexed locally.
    pub indexed_objects: usize,
    /// Bytes received over the wire so far.
    pub received_bytes: usize,
}

/// A progress sink invoked from libgit2's (synchronous) transfer callback.
/// Must be cheap and non-blocking — typically it pushes onto a channel.
pub type ProgressCallback<'a> = dyn FnMut(CloneProgress) + Send + 'a;

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
    clone_repo_inner(url, target_dir, branch, true, None)
}

/// Like [`clone_repo`], but reports network transfer progress via `progress`.
pub fn clone_repo_with_progress(
    url: &str,
    target_dir: &Path,
    branch: Option<&str>,
    progress: &mut ProgressCallback<'_>,
) -> Result<Repository, GitOpsError> {
    clone_repo_inner(url, target_dir, branch, true, Some(progress))
}

fn clone_repo_inner(
    url: &str,
    target_dir: &Path,
    branch: Option<&str>,
    shallow: bool,
    progress: Option<&mut ProgressCallback<'_>>,
) -> Result<Repository, GitOpsError> {
    let mut builder = RepoBuilder::new();

    let mut fetch_opts = FetchOptions::new();

    if let Some(progress) = progress {
        let mut callbacks = RemoteCallbacks::new();
        install_progress_callback(&mut callbacks, progress);
        fetch_opts.remote_callbacks(callbacks);
    }

    if let Some(branch) = branch {
        builder.branch(branch);
        if shallow {
            fetch_opts.depth(1);
        }
    }

    builder.fetch_options(fetch_opts);

    builder
        .clone(url, target_dir)
        .map_err(|e| clone_failed(url, e.message().to_string()))
}

/// Wire libgit2's `transfer_progress` callback to a [`ProgressCallback`].
/// Returning `true` keeps the transfer going (we never cancel from here;
/// cancellation/timeout is enforced by the async wrapper around the clone).
fn install_progress_callback<'cb>(
    callbacks: &mut RemoteCallbacks<'cb>,
    progress: &'cb mut ProgressCallback<'cb>,
) {
    callbacks.transfer_progress(move |stats| {
        progress(CloneProgress {
            received_objects: stats.received_objects(),
            total_objects: stats.total_objects(),
            indexed_objects: stats.indexed_objects(),
            received_bytes: stats.received_bytes(),
        });
        true
    });
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
    clone_repo_with_credentials_inner(url, target_dir, "x-access-token", token, branch, None)
}

/// Like [`clone_repo_with_token`], but reports transfer progress via `progress`.
pub fn clone_repo_with_token_and_progress(
    url: &str,
    target_dir: &Path,
    token: &str,
    branch: Option<&str>,
    progress: &mut ProgressCallback<'_>,
) -> Result<Repository, GitOpsError> {
    clone_repo_with_credentials_inner(
        url,
        target_dir,
        "x-access-token",
        token,
        branch,
        Some(progress),
    )
}

/// Clone a repository with custom username + token authentication.
pub fn clone_repo_with_credentials(
    url: &str,
    target_dir: &Path,
    username: &str,
    token: &str,
    branch: Option<&str>,
) -> Result<Repository, GitOpsError> {
    clone_repo_with_credentials_inner(url, target_dir, username, token, branch, None)
}

/// Like [`clone_repo_with_credentials`], but reports transfer progress.
pub fn clone_repo_with_credentials_and_progress(
    url: &str,
    target_dir: &Path,
    username: &str,
    token: &str,
    branch: Option<&str>,
    progress: &mut ProgressCallback<'_>,
) -> Result<Repository, GitOpsError> {
    clone_repo_with_credentials_inner(url, target_dir, username, token, branch, Some(progress))
}

fn clone_repo_with_credentials_inner(
    url: &str,
    target_dir: &Path,
    username: &str,
    token: &str,
    branch: Option<&str>,
    progress: Option<&mut ProgressCallback<'_>>,
) -> Result<Repository, GitOpsError> {
    let username = username.to_string();
    let token = token.to_string();
    let mut builder = RepoBuilder::new();

    let mut callbacks = RemoteCallbacks::new();
    callbacks.credentials(move |_url, _username_from_url, _allowed_types| {
        Cred::userpass_plaintext(&username, &token)
    });
    if let Some(progress) = progress {
        install_progress_callback(&mut callbacks, progress);
    }

    let mut fetch_opts = FetchOptions::new();
    fetch_opts.remote_callbacks(callbacks);

    if let Some(branch) = branch {
        builder.branch(branch);
        fetch_opts.depth(1);
    }

    builder.fetch_options(fetch_opts);

    builder
        .clone(url, target_dir)
        .map_err(|e| clone_failed(url, e.message().to_string()))
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

/// Normalize and reject unsafe sparse-checkout paths.
pub fn validate_sparse_subdirectory(subdirectory: &str) -> Result<String, GitOpsError> {
    let normalized = subdirectory
        .trim()
        .replace('\\', "/")
        .trim_start_matches('/')
        .trim_end_matches('/')
        .trim_start_matches("./")
        .to_string();
    if normalized.is_empty() || normalized == "." {
        return Err(GitOpsError::SparseCloneFailed {
            url: String::new(),
            subdirectory: subdirectory.to_string(),
            reason: "subdirectory must be a path inside the repository, not the root".to_string(),
        });
    }
    let path = Path::new(&normalized);
    if normalized.contains('\n')
        || normalized.contains('\0')
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        return Err(GitOpsError::SparseCloneFailed {
            url: String::new(),
            subdirectory: subdirectory.to_string(),
            reason: "subdirectory must be a relative path inside the repository".to_string(),
        });
    }
    Ok(normalized)
}

/// Escape a literal path so non-cone sparse-checkout treats it as one
/// directory, not a gitignore glob (`*`, `?`, `[`, `]`, `\`).
fn escape_sparse_gitignore_literal(path: &str) -> String {
    let mut escaped = String::with_capacity(path.len());
    for ch in path.chars() {
        if matches!(ch, '\\' | '*' | '?' | '[' | ']') {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

/// Non-cone pattern that includes only `subdirectory` and nothing at the
/// repository root. Cone mode always materializes root files; this flag is
/// for the opposite case.
fn sparse_checkout_pattern(subdirectory: &str) -> String {
    format!("/{}/", escape_sparse_gitignore_literal(subdirectory))
}

/// Clone only `subdirectory` using git sparse-checkout (partial clone).
///
/// libgit2 cannot do `--filter=blob:none` + sparse-checkout, so this
/// shells out to `git`. `file://` remotes skip the filter (unsupported).
/// Uses non-cone patterns so repository-root files are not checked out.
///
/// `credentials` is HTTP Basic (`username`, `token`) via a process-local
/// `http.extraHeader` — the token is not written into the URL.
///
/// Callers that wrap this in `tokio::time::timeout` can drop the future to
/// kill the in-flight `git` child (`kill_on_drop`).
pub async fn sparse_clone_repo(
    url: &str,
    target_dir: &Path,
    subdirectory: &str,
    checkout_ref: Option<&str>,
    credentials: Option<(&str, &str)>,
) -> Result<Repository, GitOpsError> {
    let subdirectory = validate_sparse_subdirectory(subdirectory)?;
    let redacted_url = temps_core::url_validation::redact_url_password(url);
    let target = target_dir.display().to_string();

    let fail = |reason: String| GitOpsError::SparseCloneFailed {
        url: redacted_url.clone(),
        subdirectory: subdirectory.clone(),
        reason,
    };

    let mut clone = git_command();
    clone
        .env("GIT_TERMINAL_PROMPT", "0")
        .arg("clone")
        .arg("--sparse")
        .arg("--no-checkout");
    if !url.starts_with("file:") && !url.starts_with("file://") {
        clone.arg("--filter=blob:none");
    }
    apply_git_http_credentials(&mut clone, credentials);
    clone.arg("--").arg(url).arg(target_dir);

    run_git(clone, &format!("clone {redacted_url} into {target}"))
        .await
        .map_err(fail)?;

    let mut sparse = git_command();
    sparse
        .env("GIT_TERMINAL_PROMPT", "0")
        .arg("-C")
        .arg(target_dir)
        .arg("sparse-checkout")
        .arg("set")
        .arg("--no-cone")
        .arg("--")
        .arg(sparse_checkout_pattern(&subdirectory));
    apply_git_http_credentials(&mut sparse, credentials);
    run_git(sparse, &format!("sparse-checkout set {subdirectory}"))
        .await
        .map_err(fail)?;

    let mut checkout = git_command();
    checkout
        .env("GIT_TERMINAL_PROMPT", "0")
        .arg("-C")
        .arg(target_dir)
        .arg("checkout");
    if let Some(reference) = checkout_ref.filter(|value| !value.is_empty()) {
        checkout.arg(reference);
    }
    apply_git_http_credentials(&mut checkout, credentials);
    run_git(
        checkout,
        &format!("checkout {}", checkout_ref.unwrap_or("HEAD")),
    )
    .await
    .map_err(fail)?;

    Repository::open(target_dir).map_err(|e| fail(e.message().to_string()))
}

fn git_command() -> tokio::process::Command {
    let mut command = tokio::process::Command::new("git");
    command.kill_on_drop(true);
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    #[cfg(unix)]
    {
        command.process_group(0);
    }
    command
}

fn apply_git_http_credentials(
    command: &mut tokio::process::Command,
    credentials: Option<(&str, &str)>,
) {
    let Some((username, token)) = credentials else {
        return;
    };
    let basic = base64::engine::general_purpose::STANDARD.encode(format!("{username}:{token}"));
    command
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "http.extraHeader")
        .env(
            "GIT_CONFIG_VALUE_0",
            format!("Authorization: Basic {basic}"),
        );
}

async fn run_git(mut command: tokio::process::Command, action: &str) -> Result<(), String> {
    let child = command
        .spawn()
        .map_err(|e| format!("failed to run git ({action}): {e}"))?;
    #[cfg(unix)]
    let output = ProcessGroupOutput::new(child, action)?
        .wait_with_output()
        .await;
    #[cfg(not(unix))]
    let output = child.wait_with_output().await;
    let output = output.map_err(|e| format!("failed to run git ({action}): {e}"))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if !stderr.is_empty() {
        stderr
    } else if !stdout.is_empty() {
        stdout
    } else {
        format!("git exited {}", output.status)
    };
    Err(format!("{action}: {detail}"))
}

/// Owns both process-group cleanup and Tokio's child handle.
///
/// On cancellation, `Drop` signals the group before the `Child` field is
/// dropped. This prevents transport subprocesses from surviving while
/// retaining Tokio's normal child reaping.
#[cfg(unix)]
struct ProcessGroupOutput {
    child: tokio::process::Child,
    process_group: nix::unistd::Pid,
}

#[cfg(unix)]
impl ProcessGroupOutput {
    fn new(child: tokio::process::Child, action: &str) -> Result<Self, String> {
        let child_id = child
            .id()
            .ok_or_else(|| format!("failed to track git process group ({action}): missing PID"))?;
        let process_group = i32::try_from(child_id).map_err(|_| {
            format!("failed to track git process group ({action}): invalid PID {child_id}")
        })?;
        Ok(Self {
            child,
            process_group: nix::unistd::Pid::from_raw(process_group),
        })
    }

    async fn wait_with_output(mut self) -> Result<std::process::Output, std::io::Error> {
        let mut stdout = self.child.stdout.take().ok_or_else(|| {
            std::io::Error::other("git stdout was not configured for process cleanup")
        })?;
        let mut stderr = self.child.stderr.take().ok_or_else(|| {
            std::io::Error::other("git stderr was not configured for process cleanup")
        })?;
        let mut stdout_bytes = Vec::new();
        let mut stderr_bytes = Vec::new();
        tokio::try_join!(
            stdout.read_to_end(&mut stdout_bytes),
            stderr.read_to_end(&mut stderr_bytes)
        )?;

        // Reap only after every group member has closed the inherited pipes.
        // Until then the unreaped leader reserves its PID, making it safe for
        // Drop to use that PID as the process-group ID during cancellation.
        let status = self.child.wait().await?;
        Ok(std::process::Output {
            status,
            stdout: stdout_bytes,
            stderr: stderr_bytes,
        })
    }
}

#[cfg(unix)]
impl Drop for ProcessGroupOutput {
    fn drop(&mut self) {
        // Once Tokio reaps the child, its PID may be reused. Avoid signalling
        // the numeric process-group ID after that point.
        if self.child.id().is_some() {
            let _ = nix::sys::signal::killpg(self.process_group, nix::sys::signal::Signal::SIGKILL);
        }
    }
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
        let result = clone_repo_inner(&source_url, target_dir.path(), Some("feature"), false, None);
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
    fn test_clone_error_never_exposes_url_credentials() {
        for url in [
            "https://token:secret@example.com/repo.git",
            "https://token@example.com/repo.git",
        ] {
            let error = clone_failed(url, "connection failed".to_string());
            let rendered = error.to_string();
            assert!(!rendered.contains("token"), "leaked username: {rendered}");
            assert!(!rendered.contains("secret"), "leaked password: {rendered}");
            assert!(rendered.contains("***"));
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

    #[test]
    fn test_validate_sparse_subdirectory_rejects_root_and_escape() {
        assert!(validate_sparse_subdirectory(".").is_err());
        assert!(validate_sparse_subdirectory("./").is_err());
        assert!(validate_sparse_subdirectory("").is_err());
        assert!(validate_sparse_subdirectory("../etc").is_err());
        assert_eq!(
            validate_sparse_subdirectory("./apps/web").unwrap(),
            "apps/web"
        );
        assert_eq!(
            validate_sparse_subdirectory("apps/web/").unwrap(),
            "apps/web"
        );
        assert_eq!(
            validate_sparse_subdirectory("apps/we[b]").unwrap(),
            "apps/we[b]"
        );
    }

    #[test]
    fn test_sparse_checkout_pattern_escapes_gitignore_metacharacters() {
        assert_eq!(sparse_checkout_pattern("apps/web"), "/apps/web/");
        assert_eq!(sparse_checkout_pattern("apps/we[b]"), r"/apps/we\[b\]/");
        assert_eq!(sparse_checkout_pattern("apps/web?"), r"/apps/web\?/");
        assert_eq!(sparse_checkout_pattern("apps/*"), r"/apps/\*/");
        assert_eq!(sparse_checkout_pattern(r"apps/web]"), r"/apps/web\]/");
    }

    #[tokio::test]
    async fn test_sparse_clone_local_repo_keeps_only_subdirectory() {
        let source_dir = TempDir::new().unwrap();
        let repo = Repository::init(source_dir.path()).unwrap();
        let sig = Signature::now("Test", "test@test.com").unwrap();

        std::fs::create_dir_all(source_dir.path().join("apps/web")).unwrap();
        std::fs::create_dir_all(source_dir.path().join("apps/api")).unwrap();
        std::fs::write(source_dir.path().join("apps/web/index.html"), "web").unwrap();
        std::fs::write(source_dir.path().join("apps/api/main.go"), "package main").unwrap();
        std::fs::write(source_dir.path().join("README.md"), "root").unwrap();

        {
            let mut index = repo.index().unwrap();
            index
                .add_path(std::path::Path::new("apps/web/index.html"))
                .unwrap();
            index
                .add_path(std::path::Path::new("apps/api/main.go"))
                .unwrap();
            index.add_path(std::path::Path::new("README.md")).unwrap();
            index.write().unwrap();
            let tree_id = index.write_tree().unwrap();
            let tree = repo.find_tree(tree_id).unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
                .unwrap();
        }

        let target_dir = TempDir::new().unwrap();
        let source_url = format!("file://{}", source_dir.path().display());
        let result =
            sparse_clone_repo(&source_url, target_dir.path(), "apps/web", None, None).await;
        assert!(result.is_ok(), "sparse clone failed: {:?}", result.err());
        assert!(target_dir.path().join("apps/web/index.html").exists());
        assert!(!target_dir.path().join("apps/api/main.go").exists());
        assert!(
            !target_dir.path().join("README.md").exists(),
            "non-cone sparse checkout must not materialize repository-root files"
        );
    }

    #[tokio::test]
    async fn test_sparse_clone_accepts_dash_prefixed_subdirectory() {
        let source_dir = TempDir::new().unwrap();
        let repo = Repository::init(source_dir.path()).unwrap();
        let sig = Signature::now("Test", "test@test.com").unwrap();

        std::fs::create_dir_all(source_dir.path().join("-site/app")).unwrap();
        std::fs::write(source_dir.path().join("-site/app/index.html"), "site").unwrap();

        {
            let mut index = repo.index().unwrap();
            index
                .add_path(std::path::Path::new("-site/app/index.html"))
                .unwrap();
            index.write().unwrap();
            let tree_id = index.write_tree().unwrap();
            let tree = repo.find_tree(tree_id).unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
                .unwrap();
        }

        let target_dir = TempDir::new().unwrap();
        let source_url = format!("file://{}", source_dir.path().display());
        let result =
            sparse_clone_repo(&source_url, target_dir.path(), "-site/app", None, None).await;
        assert!(
            result.is_ok(),
            "dash-prefixed sparse clone failed: {:?}",
            result.err()
        );
        assert!(target_dir.path().join("-site/app/index.html").exists());
    }

    #[tokio::test]
    async fn test_sparse_clone_treats_bracket_directory_as_literal() {
        let source_dir = TempDir::new().unwrap();
        let repo = Repository::init(source_dir.path()).unwrap();
        let sig = Signature::now("Test", "test@test.com").unwrap();

        std::fs::create_dir_all(source_dir.path().join("apps/we[b]")).unwrap();
        std::fs::create_dir_all(source_dir.path().join("apps/web")).unwrap();
        std::fs::write(source_dir.path().join("apps/we[b]/index.html"), "bracket").unwrap();
        std::fs::write(source_dir.path().join("apps/web/index.html"), "plain").unwrap();

        {
            let mut index = repo.index().unwrap();
            index
                .add_path(std::path::Path::new("apps/we[b]/index.html"))
                .unwrap();
            index
                .add_path(std::path::Path::new("apps/web/index.html"))
                .unwrap();
            index.write().unwrap();
            let tree_id = index.write_tree().unwrap();
            let tree = repo.find_tree(tree_id).unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
                .unwrap();
        }

        let target_dir = TempDir::new().unwrap();
        let source_url = format!("file://{}", source_dir.path().display());
        let result =
            sparse_clone_repo(&source_url, target_dir.path(), "apps/we[b]", None, None).await;
        assert!(
            result.is_ok(),
            "literal bracket sparse clone failed: {:?}",
            result.err()
        );
        assert_eq!(
            std::fs::read_to_string(target_dir.path().join("apps/we[b]/index.html")).unwrap(),
            "bracket"
        );
        assert!(
            !target_dir.path().join("apps/web/index.html").exists(),
            "unescaped [b] would match apps/web"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancelling_git_command_kills_transport_process_group() {
        let temp_dir = TempDir::new().unwrap();
        let ready_path = temp_dir.path().join("ready");
        let survivor_path = temp_dir.path().join("survivor");

        let fake_git = temp_dir.path().join("git");
        std::os::unix::fs::symlink("/bin/sh", &fake_git).unwrap();
        let mut command = tokio::process::Command::new(&fake_git);
        command
            .kill_on_drop(true)
            .process_group(0)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .arg("-c")
            .arg("(sleep 1; echo survived > \"$2\") & echo ready > \"$1\"; wait")
            .arg("git")
            .arg(&ready_path)
            .arg(&survivor_path);

        let mut operation = Box::pin(run_git(command, "test cancellation"));
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if ready_path.exists() {
                    break;
                }
                tokio::select! {
                    result = &mut operation => panic!("test command exited early: {result:?}"),
                    () = tokio::time::sleep(std::time::Duration::from_millis(10)) => {}
                }
            }
        })
        .await
        .unwrap();
        drop(operation);

        tokio::time::sleep(std::time::Duration::from_millis(1_200)).await;
        assert!(
            !survivor_path.exists(),
            "a transport subprocess survived cancellation"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_git_preserves_action_and_stderr_on_failure() {
        let mut command = tokio::process::Command::new("/bin/sh");
        command
            .kill_on_drop(true)
            .process_group(0)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .arg("-c")
            .arg("echo transport-failed >&2; exit 7");

        let error = run_git(command, "clone test repository").await.unwrap_err();
        assert_eq!(error, "clone test repository: transport-failed");
    }
}
