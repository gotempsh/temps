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

    #[error("Failed to read the checked-out commit in {repo_path}: {reason}")]
    ReadHeadFailed { repo_path: String, reason: String },

    #[error("Ref '{ref_name}' not found in {url}: {detail}")]
    RefNotFound {
        url: String,
        ref_name: String,
        detail: String,
    },

    #[error("Cannot check out ref '{ref_name}' from {url}: {reason}")]
    InvalidRef {
        url: String,
        ref_name: String,
        reason: String,
    },
}

/// The commit a working copy has checked out, read from its own `.git`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadCommit {
    /// Full hexadecimal object id of the commit.
    pub sha: String,
    /// Commit message, if it is valid UTF-8.
    pub message: Option<String>,
    /// Author name, if it is valid UTF-8.
    pub author: Option<String>,
}

/// Read the commit `HEAD` points at in the working copy rooted at `repo_dir`.
///
/// Returns `Ok(None)` when `repo_dir` is not itself a git working copy (for
/// example a provider archive download, which carries no `.git`). Parent
/// directories are deliberately not searched: a checkout extracted somewhere
/// inside another repository must not report that repository's commit.
pub fn read_head_commit(repo_dir: &Path) -> Result<Option<HeadCommit>, GitOpsError> {
    let read_failed = |reason: String| GitOpsError::ReadHeadFailed {
        repo_path: repo_dir.display().to_string(),
        reason,
    };

    let repo = match Repository::open_ext(
        repo_dir,
        git2::RepositoryOpenFlags::NO_SEARCH,
        std::iter::empty::<&std::ffi::OsStr>(),
    ) {
        Ok(repo) => repo,
        Err(e) if e.code() == git2::ErrorCode::NotFound => return Ok(None),
        Err(e) => return Err(read_failed(e.message().to_string())),
    };

    let commit = repo
        .head()
        .and_then(|head| head.peel_to_commit())
        .map_err(|e| read_failed(e.message().to_string()))?;

    let author = commit.author().name().ok().map(str::to_string);
    Ok(Some(HeadCommit {
        sha: commit.id().to_string(),
        message: commit.message().ok().map(|m| m.trim_end().to_string()),
        author,
    }))
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

/// What kind of ref [`clone_ref`] resolved a name to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClonedRefKind {
    Branch,
    Tag,
    Commit,
    /// A fully qualified ref outside `refs/heads/` and `refs/tags/`
    /// (for example `refs/pull/1/head`), checked out detached.
    Other,
}

impl ClonedRefKind {
    /// Lowercase noun for logs.
    pub fn as_str(self) -> &'static str {
        match self {
            ClonedRefKind::Branch => "branch",
            ClonedRefKind::Tag => "tag",
            ClonedRefKind::Commit => "commit",
            ClonedRefKind::Other => "ref",
        }
    }
}

/// The result of [`clone_ref`]: what was checked out and the commit it is at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClonedRef {
    pub kind: ClonedRefKind,
    /// Full object id `HEAD` points at after the checkout.
    pub commit_sha: String,
}

/// Reject a ref that could be read as an option or smuggle a second refspec
/// into the `git fetch` it is interpolated into.
fn validate_clone_ref(url: &str, reference: &str) -> Result<(), GitOpsError> {
    let invalid = |reason: &str| GitOpsError::InvalidRef {
        url: temps_core::url_validation::redact_url_password(url),
        ref_name: reference.chars().take(200).collect(),
        reason: reason.to_string(),
    };
    if reference.is_empty() {
        return Err(invalid("the ref is empty"));
    }
    // The bare name after `refs/heads/` or `refs/tags/` reaches `git checkout`
    // as an argument, so it is held to the same rule as the whole ref.
    let bare = reference
        .strip_prefix("refs/heads/")
        .or_else(|| reference.strip_prefix("refs/tags/"))
        .unwrap_or(reference);
    if reference.starts_with('-') || bare.starts_with('-') || reference.starts_with('/') {
        return Err(invalid("a ref cannot start with '-' or '/'"));
    }
    if reference.chars().any(|c| {
        c.is_control() || c.is_whitespace() || matches!(c, '~' | '^' | ':' | '?' | '*' | '[' | '\\')
    }) {
        return Err(invalid(
            "a ref cannot contain whitespace, control characters or any of ~ ^ : ? * [ \\",
        ));
    }
    if reference.contains("..")
        || reference.contains("@{")
        || reference.contains("//")
        || reference.ends_with('/')
        || reference.ends_with('.')
        || reference.ends_with(".lock")
    {
        return Err(invalid("not a valid git ref name"));
    }
    Ok(())
}

/// `git fetch origin <refspecs>` into `target`, optionally shallow.
async fn fetch_refspecs(
    target: &Path,
    credentials: Option<(&str, &str)>,
    shallow: bool,
    refspecs: &[String],
) -> Result<(), String> {
    let mut command = git_command();
    command
        .env("GIT_TERMINAL_PROMPT", "0")
        // Fall-through below keys on git's "couldn't find remote ref" text.
        .env("LC_ALL", "C")
        .arg("-C")
        .arg(target)
        .args(["fetch", "--no-tags"]);
    if shallow {
        command.arg("--depth=1");
    }
    command.arg("origin").args(refspecs);
    apply_git_http_credentials(&mut command, credentials);
    run_git(command, "fetch checkout ref").await
}

/// Whether a failed fetch means the remote has no such ref (as opposed to the
/// remote being unreachable or refusing access).
fn is_missing_remote_ref(fetch_error: &str) -> bool {
    fetch_error.contains("couldn't find remote ref")
}

/// Resolve `git rev-parse --verify <rev>` in `target`.
async fn rev_parse(target: &Path, rev: &str) -> Result<String, String> {
    let mut command = git_command();
    command
        .arg("-C")
        .arg(target)
        .args(["rev-parse", "--verify", "--quiet"])
        .arg(rev);
    run_git_stdout(command, &format!("resolve {rev}")).await
}

/// Check out a branch, a tag or a commit of `url` into `target`, fetching as
/// little history as the remote allows.
///
/// A plain name is tried as a branch first, then as a tag, then (when it is
/// 7 to 40 hexadecimal characters) as a commit, so a branch always wins over
/// a tag or abbreviated id of the same spelling. `refs/heads/x` and
/// `refs/tags/x` pin the kind; any other `refs/...` is fetched as given and
/// checked out detached.
///
/// * Branch: shallow (depth 1) fetch, left on a local branch of that name.
/// * Tag: shallow fetch, detached at the tag's commit (annotated tags are
///   peeled).
/// * Commit: a full id is fetched shallow when the server allows fetching it
///   by id (GitHub and GitLab do); an abbreviated id, or a server that
///   refuses, falls back to fetching every branch and tag in full, because a
///   short id cannot be asked for by name.
///
/// Only "the remote has no such ref" moves on to the next kind; transport and
/// authentication failures stop at once so they are not retried three times
/// and misreported as a missing ref. When nothing matches the error is
/// [`GitOpsError::RefNotFound`]. After the checkout, `HEAD` is read back and
/// compared with the commit the ref resolved to.
///
/// Credentials are HTTP Basic via a process-local `http.extraHeader`, never
/// argv or repository config. Dropping the future kills the `git` process
/// group, so callers can bound it with `tokio::time::timeout`.
pub async fn clone_ref(
    url: &str,
    target: &Path,
    credentials: Option<(&str, &str)>,
    reference: &str,
) -> Result<ClonedRef, GitOpsError> {
    validate_clone_ref(url, reference)?;
    if url.starts_with('-') {
        return Err(clone_failed(
            url,
            "a remote URL cannot start with '-'".to_string(),
        ));
    }
    let redacted_url = temps_core::url_validation::redact_url_password(url);
    let not_found = |detail: String| GitOpsError::RefNotFound {
        url: redacted_url.clone(),
        ref_name: reference.to_string(),
        detail,
    };

    let mut init = git_command();
    init.args(["init", "--quiet", "--"]).arg(target);
    run_git(init, "initialize shallow checkout")
        .await
        .map_err(|e| clone_failed(url, e))?;
    let mut remote = git_command();
    remote
        .arg("-C")
        .arg(target)
        .args(["remote", "add", "origin", url]);
    run_git(remote, "configure shallow checkout remote")
        .await
        .map_err(|e| clone_failed(url, e))?;

    let (pinned, name) = if let Some(name) = reference.strip_prefix("refs/heads/") {
        (Some(ClonedRefKind::Branch), name)
    } else if let Some(name) = reference.strip_prefix("refs/tags/") {
        (Some(ClonedRefKind::Tag), name)
    } else if reference.starts_with("refs/") {
        (Some(ClonedRefKind::Other), reference)
    } else {
        (None, reference)
    };

    let mut resolved: Option<(ClonedRefKind, String)> = None;

    if pinned.is_none_or(|kind| kind == ClonedRefKind::Branch) {
        let remote_branch = format!("refs/remotes/origin/{name}");
        let refspec = format!("+refs/heads/{name}:{remote_branch}");
        match fetch_refspecs(target, credentials, true, &[refspec]).await {
            Ok(()) => {
                let sha = rev_parse(target, &format!("{remote_branch}^{{commit}}"))
                    .await
                    .map_err(|e| clone_failed(url, e))?;
                let mut checkout = git_command();
                checkout.arg("-C").arg(target).args([
                    "checkout",
                    "--quiet",
                    "-B",
                    name,
                    &remote_branch,
                ]);
                run_git(checkout, "checkout branch")
                    .await
                    .map_err(|e| clone_failed(url, e))?;
                resolved = Some((ClonedRefKind::Branch, sha));
            }
            Err(e) if is_missing_remote_ref(&e) => {}
            Err(e) => return Err(clone_failed(url, e)),
        }
    }

    if resolved.is_none() && pinned.is_none_or(|kind| kind == ClonedRefKind::Tag) {
        let local_tag = format!("refs/tags/{name}");
        let refspec = format!("+{local_tag}:{local_tag}");
        match fetch_refspecs(target, credentials, true, &[refspec]).await {
            Ok(()) => {
                let sha = rev_parse(target, &format!("{local_tag}^{{commit}}"))
                    .await
                    .map_err(|e| clone_failed(url, e))?;
                let mut checkout = git_command();
                checkout
                    .arg("-C")
                    .arg(target)
                    .args(["checkout", "--quiet", "--detach", &sha]);
                run_git(checkout, "checkout tag")
                    .await
                    .map_err(|e| clone_failed(url, e))?;
                resolved = Some((ClonedRefKind::Tag, sha));
            }
            Err(e) if is_missing_remote_ref(&e) => {}
            Err(e) => return Err(clone_failed(url, e)),
        }
    }

    if resolved.is_none() && pinned == Some(ClonedRefKind::Other) {
        let refspec = format!("+{name}:refs/temps/fetched");
        match fetch_refspecs(target, credentials, true, &[refspec]).await {
            Ok(()) => {
                let sha = rev_parse(target, "refs/temps/fetched^{commit}")
                    .await
                    .map_err(|e| clone_failed(url, e))?;
                let mut checkout = git_command();
                checkout
                    .arg("-C")
                    .arg(target)
                    .args(["checkout", "--quiet", "--detach", &sha]);
                run_git(checkout, "checkout ref")
                    .await
                    .map_err(|e| clone_failed(url, e))?;
                resolved = Some((ClonedRefKind::Other, sha));
            }
            Err(e) if is_missing_remote_ref(&e) => {}
            Err(e) => return Err(clone_failed(url, e)),
        }
    }

    let is_commit_like =
        (7..=40).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_hexdigit());
    if resolved.is_none() && pinned.is_none() && is_commit_like {
        // A full id can be requested directly, which keeps the fetch shallow.
        // Anything else (an abbreviated id, or a server that will not serve
        // unadvertised objects) needs every advertised ref to find the commit.
        let direct = if name.len() == 40 {
            fetch_refspecs(target, credentials, true, &[name.to_string()]).await
        } else {
            Err("abbreviated commit id".to_string())
        };
        if direct.is_err() {
            let everything = [
                "+refs/heads/*:refs/remotes/origin/*".to_string(),
                "+refs/tags/*:refs/tags/*".to_string(),
            ];
            fetch_refspecs(target, credentials, false, &everything)
                .await
                .map_err(|e| clone_failed(url, e))?;
        }
        match rev_parse(target, &format!("{name}^{{commit}}")).await {
            Ok(sha) if !sha.is_empty() => {
                let mut checkout = git_command();
                checkout
                    .arg("-C")
                    .arg(target)
                    .args(["checkout", "--quiet", "--detach", &sha]);
                run_git(checkout, "checkout commit")
                    .await
                    .map_err(|e| clone_failed(url, e))?;
                resolved = Some((ClonedRefKind::Commit, sha));
            }
            // `--quiet` makes an unknown id exit non-zero with no output.
            _ => {}
        }
    }

    let Some((kind, expected)) = resolved else {
        return Err(not_found(
            "it is not a branch, tag or commit of the repository".to_string(),
        ));
    };

    let head = rev_parse(target, "HEAD")
        .await
        .map_err(|e| clone_failed(url, e))?;
    if head != expected {
        return Err(clone_failed(
            url,
            format!(
                "checked out {head} for {} '{reference}', expected {expected}",
                kind.as_str()
            ),
        ));
    }
    Ok(ClonedRef {
        kind,
        commit_sha: head,
    })
}

/// Clone a named branch or tag shallowly, with cancellable transport processes.
/// Credentials use process-local Git configuration, never argv or repository config.
///
/// Thin wrapper over [`clone_ref`] with HTTP Basic credentials.
pub async fn clone_named_ref_with_credentials(
    url: &str,
    target: &Path,
    username: &str,
    token: &str,
    reference: &str,
) -> Result<(), GitOpsError> {
    clone_ref(url, target, Some((username, token)), reference)
        .await
        .map(|_| ())
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

async fn run_git(command: tokio::process::Command, action: &str) -> Result<(), String> {
    run_git_stdout(command, action).await.map(|_| ())
}

/// Run `command` to completion and return its trimmed standard output.
async fn run_git_stdout(
    mut command: tokio::process::Command,
    action: &str,
) -> Result<String, String> {
    command.stdin(Stdio::null());
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
        return Ok(String::from_utf8_lossy(&output.stdout).trim().to_string());
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
    fn read_head_commit_returns_the_checked_out_commit() {
        let (temp_dir, repo) = create_test_repo();
        let head = repo.head().unwrap().peel_to_commit().unwrap();

        let commit = read_head_commit(temp_dir.path())
            .expect("reading HEAD must succeed")
            .expect("a working copy has a HEAD commit");

        assert_eq!(commit.sha, head.id().to_string());
        assert_eq!(commit.sha.len(), 40);
        assert_eq!(commit.message.as_deref(), Some("add file"));
        assert_eq!(commit.author.as_deref(), Some("Test"));
    }

    #[test]
    fn read_head_commit_follows_a_detached_checkout() {
        let (temp_dir, repo) = create_test_repo();
        let first = repo
            .head()
            .unwrap()
            .peel_to_commit()
            .unwrap()
            .parent(0)
            .unwrap()
            .id()
            .to_string();
        checkout_ref(&repo, &first).unwrap();

        let commit = read_head_commit(temp_dir.path()).unwrap().unwrap();

        assert_eq!(commit.sha, first);
        assert_eq!(commit.message.as_deref(), Some("initial commit"));
    }

    #[test]
    fn read_head_commit_is_none_without_git_metadata() {
        // An archive download has the files but no `.git`.
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("main.go"), "package main").unwrap();

        assert_eq!(read_head_commit(dir.path()).unwrap(), None);
    }

    #[test]
    fn read_head_commit_does_not_report_an_enclosing_repository() {
        let (temp_dir, _repo) = create_test_repo();
        let nested = temp_dir.path().join("extracted");
        std::fs::create_dir(&nested).unwrap();

        assert_eq!(read_head_commit(&nested).unwrap(), None);
    }

    #[test]
    fn read_head_commit_errors_on_a_repository_without_commits() {
        let dir = TempDir::new().unwrap();
        Repository::init(dir.path()).unwrap();

        let error = read_head_commit(dir.path()).unwrap_err();

        assert!(matches!(error, GitOpsError::ReadHeadFailed { .. }));
        assert!(error
            .to_string()
            .contains(&dir.path().display().to_string()));
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

    #[tokio::test]
    async fn authenticated_branch_clone_is_shallow_and_keeps_head_metadata() {
        struct Daemon(std::process::Child);
        impl Drop for Daemon {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let (source, repo) = create_test_repo();
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        repo.branch("selected", &head, false).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let child = std::process::Command::new("git")
            .args([
                "daemon",
                "--reuseaddr",
                "--export-all",
                "--listen=127.0.0.1",
            ])
            .arg(format!("--port={port}"))
            .arg(format!(
                "--base-path={}",
                source.path().parent().unwrap().display()
            ))
            .arg(source.path())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("git CLI must be available for the clone regression test");
        let mut daemon = Daemon(child);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
                break;
            }
            assert!(daemon.0.try_wait().unwrap().is_none(), "git daemon exited");
            assert!(
                std::time::Instant::now() < deadline,
                "git daemon did not become ready"
            );
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        let target = TempDir::new().unwrap();
        let url = format!(
            "git://127.0.0.1:{port}/{}",
            source.path().file_name().unwrap().to_string_lossy()
        );
        let cloned =
            clone_repo_with_credentials(&url, target.path(), "example", "unused", Some("selected"))
                .unwrap();
        assert!(
            cloned.is_shallow(),
            "branch clone must not download full history"
        );
        assert_eq!(cloned.head().unwrap().target(), Some(head.id()));
        assert_eq!(
            read_head_commit(target.path()).unwrap().unwrap().sha,
            head.id().to_string()
        );
        let mut walk = cloned.revwalk().unwrap();
        walk.push_head().unwrap();
        assert_eq!(walk.count(), 1);
        repo.tag_lightweight("release-only", head.as_object(), false)
            .unwrap();
        let first = head.parent(0).unwrap();
        repo.tag_lightweight("selected", first.as_object(), false)
            .unwrap();
        for reference in [
            "selected",
            "refs/heads/selected",
            "refs/tags/release-only",
            "refs/tags/selected",
        ] {
            let target = TempDir::new().unwrap();
            clone_named_ref_with_credentials(&url, target.path(), "example", "unused", reference)
                .await
                .unwrap();
            let cloned = Repository::open(target.path()).unwrap();
            let expected = if reference == "refs/tags/selected" {
                first.id()
            } else {
                head.id()
            };
            assert!(cloned.is_shallow(), "{reference}");
            assert_eq!(
                cloned.head().unwrap().target(),
                Some(expected),
                "{reference}"
            );
            if !reference.starts_with("refs/tags/") {
                assert!(cloned.head().unwrap().is_branch(), "{reference}");
                assert_eq!(cloned.head().unwrap().shorthand().unwrap(), "selected");
            } else {
                assert!(cloned.head_detached().unwrap());
            }
            assert_eq!(
                target.path().join("file.txt").exists(),
                expected == head.id(),
                "{reference}"
            );
        }
    }

    /// A bare-bones origin built with the `git` CLI, addressed over `file://`:
    /// `master` (three commits), `feature` (one extra commit off the second),
    /// an annotated tag `v1.0.0` and a lightweight tag `light` on earlier
    /// commits, and a branch and a tag that share the name `dual`.
    struct OriginFixture {
        _dir: TempDir,
        url: String,
        commits: [String; 3],
        feature_commit: String,
        annotated_tag_object: String,
    }

    fn git_available() -> bool {
        std::process::Command::new("git")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn origin_fixture() -> Option<OriginFixture> {
        if !git_available() {
            println!("git CLI not available, skipping");
            return None;
        }
        let dir = TempDir::new().ok()?;
        let path = dir.path().join("origin");
        std::fs::create_dir_all(&path).ok()?;
        let git = |args: &[&str]| -> String {
            let output = std::process::Command::new("git")
                .arg("-C")
                .arg(&path)
                .args([
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@example.invalid",
                    "-c",
                    "commit.gpgsign=false",
                    "-c",
                    "tag.gpgsign=false",
                ])
                .args(args)
                .output()
                .expect("run git");
            assert!(
                output.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        };
        let commit = |content: &str, message: &str| -> String {
            std::fs::write(path.join("file.txt"), content).unwrap();
            git(&["add", "file.txt"]);
            git(&["commit", "--quiet", "-m", message]);
            git(&["rev-parse", "HEAD"])
        };
        git(&["init", "--quiet", "--initial-branch=master"]);
        let first = commit("one", "first");
        git(&["tag", "light"]);
        git(&["tag", "dual"]);
        let second = commit("two", "second");
        git(&["tag", "-a", "-m", "release", "v1.0.0"]);
        let annotated_tag_object = git(&["rev-parse", "refs/tags/v1.0.0"]);
        git(&["checkout", "--quiet", "-b", "feature"]);
        std::fs::write(path.join("feature.txt"), "feature").unwrap();
        git(&["add", "feature.txt"]);
        git(&["commit", "--quiet", "-m", "feature work"]);
        let feature_commit = git(&["rev-parse", "HEAD"]);
        git(&["checkout", "--quiet", "master"]);
        let third = commit("three", "third");
        git(&["branch", "dual"]);
        Some(OriginFixture {
            url: format!("file://{}", path.display()),
            _dir: dir,
            commits: [first, second, third],
            feature_commit,
            annotated_tag_object,
        })
    }

    async fn checkout(origin: &OriginFixture, reference: &str) -> (TempDir, ClonedRef) {
        let target = TempDir::new().unwrap();
        let cloned = clone_ref(&origin.url, target.path(), None, reference)
            .await
            .unwrap_or_else(|e| panic!("clone_ref({reference}) failed: {e}"));
        (target, cloned)
    }

    fn file(target: &TempDir, name: &str) -> Option<String> {
        std::fs::read_to_string(target.path().join(name)).ok()
    }

    #[tokio::test]
    async fn clone_ref_checks_out_a_branch_shallow_and_on_that_branch() {
        let Some(origin) = origin_fixture() else {
            return;
        };
        // `master` is also git's default initial branch, which a fetch into
        // the checked-out branch would refuse.
        for (branch, expected_commit, expected_file) in [
            ("master", &origin.commits[2], "three"),
            ("feature", &origin.feature_commit, "two"),
            ("refs/heads/master", &origin.commits[2], "three"),
        ] {
            let (target, cloned) = checkout(&origin, branch).await;
            assert_eq!(cloned.kind, ClonedRefKind::Branch, "{branch}");
            assert_eq!(&cloned.commit_sha, expected_commit, "{branch}");
            assert_eq!(file(&target, "file.txt").as_deref(), Some(expected_file));
            let repo = Repository::open(target.path()).unwrap();
            assert!(repo.is_shallow(), "{branch} must be a shallow checkout");
            assert!(repo.head().unwrap().is_branch(), "{branch}");
            assert_eq!(
                read_head_commit(target.path()).unwrap().unwrap().sha,
                *expected_commit
            );
        }
        let (target, _) = checkout(&origin, "feature").await;
        assert_eq!(file(&target, "feature.txt").as_deref(), Some("feature"));
    }

    #[tokio::test]
    async fn clone_ref_checks_out_annotated_and_lightweight_tags_at_their_commit() {
        let Some(origin) = origin_fixture() else {
            return;
        };
        let (target, cloned) = checkout(&origin, "v1.0.0").await;
        assert_eq!(cloned.kind, ClonedRefKind::Tag);
        // The commit, not the tag object.
        assert_eq!(cloned.commit_sha, origin.commits[1]);
        assert_ne!(cloned.commit_sha, origin.annotated_tag_object);
        assert_eq!(file(&target, "file.txt").as_deref(), Some("two"));
        let repo = Repository::open(target.path()).unwrap();
        assert!(repo.head_detached().unwrap());
        assert!(repo.is_shallow());

        let (target, cloned) = checkout(&origin, "light").await;
        assert_eq!(cloned.kind, ClonedRefKind::Tag);
        assert_eq!(cloned.commit_sha, origin.commits[0]);
        assert_eq!(file(&target, "file.txt").as_deref(), Some("one"));

        let (_, cloned) = checkout(&origin, "refs/tags/v1.0.0").await;
        assert_eq!(cloned.kind, ClonedRefKind::Tag);
        assert_eq!(cloned.commit_sha, origin.commits[1]);
    }

    #[tokio::test]
    async fn clone_ref_checks_out_full_and_abbreviated_commits() {
        let Some(origin) = origin_fixture() else {
            return;
        };
        // The first commit is no branch tip, so only a by-id fetch (or the
        // full fallback) can reach it.
        let (target, cloned) = checkout(&origin, &origin.commits[0]).await;
        assert_eq!(cloned.kind, ClonedRefKind::Commit);
        assert_eq!(cloned.commit_sha, origin.commits[0]);
        assert_eq!(file(&target, "file.txt").as_deref(), Some("one"));
        assert!(Repository::open(target.path())
            .unwrap()
            .head_detached()
            .unwrap());

        for length in [7, 12] {
            let short = &origin.commits[1][..length];
            let (target, cloned) = checkout(&origin, short).await;
            assert_eq!(cloned.kind, ClonedRefKind::Commit, "{short}");
            assert_eq!(cloned.commit_sha, origin.commits[1], "{short}");
            assert_eq!(file(&target, "file.txt").as_deref(), Some("two"));
        }

        // A commit only reachable from another branch.
        let short = &origin.feature_commit[..10];
        let (target, cloned) = checkout(&origin, short).await;
        assert_eq!(cloned.commit_sha, origin.feature_commit);
        assert_eq!(file(&target, "feature.txt").as_deref(), Some("feature"));
    }

    #[tokio::test]
    async fn clone_ref_prefers_a_branch_over_a_tag_of_the_same_name() {
        let Some(origin) = origin_fixture() else {
            return;
        };
        let (target, cloned) = checkout(&origin, "dual").await;
        assert_eq!(cloned.kind, ClonedRefKind::Branch);
        assert_eq!(cloned.commit_sha, origin.commits[2]);
        assert_eq!(file(&target, "file.txt").as_deref(), Some("three"));

        let (target, cloned) = checkout(&origin, "refs/tags/dual").await;
        assert_eq!(cloned.kind, ClonedRefKind::Tag);
        assert_eq!(cloned.commit_sha, origin.commits[0]);
        assert_eq!(file(&target, "file.txt").as_deref(), Some("one"));
    }

    #[tokio::test]
    async fn clone_ref_names_the_repository_and_ref_when_nothing_matches() {
        let Some(origin) = origin_fixture() else {
            return;
        };
        for reference in [
            "no-such-ref",
            "v9.9.9",
            "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
            "deadbee",
            "refs/tags/master",
        ] {
            let target = TempDir::new().unwrap();
            let error = clone_ref(&origin.url, target.path(), None, reference)
                .await
                .expect_err(reference);
            assert!(
                matches!(&error, GitOpsError::RefNotFound { ref_name, .. } if ref_name == reference),
                "{reference}: {error:?}"
            );
            let message = error.to_string();
            assert!(message.contains(reference), "{message}");
            assert!(message.contains(&origin.url), "{message}");
        }
    }

    #[tokio::test]
    async fn clone_ref_rejects_refs_that_could_inject_options_or_refspecs() {
        for reference in [
            "",
            "--upload-pack=touch /tmp/pwned",
            "main:refs/heads/other",
            "a b",
            "a..b",
            "x^{commit}",
            "refs/heads/",
            "refs/heads/--detach",
        ] {
            let target = TempDir::new().unwrap();
            let error = clone_ref("file:///nonexistent", target.path(), None, reference)
                .await
                .expect_err(reference);
            assert!(
                matches!(error, GitOpsError::InvalidRef { .. }),
                "{reference:?}: {error:?}"
            );
            assert!(
                std::fs::read_dir(target.path()).unwrap().next().is_none(),
                "an invalid ref must be refused before anything touches the target"
            );
        }
    }

    #[tokio::test]
    async fn clone_ref_reports_an_unreachable_remote_as_a_clone_failure_not_a_missing_ref() {
        if !git_available() {
            return;
        }
        let target = TempDir::new().unwrap();
        let error = clone_ref("file:///nonexistent/origin", target.path(), None, "main")
            .await
            .expect_err("no such remote");
        assert!(
            matches!(error, GitOpsError::CloneFailed { .. }),
            "{error:?}"
        );
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

    #[cfg(unix)]
    #[tokio::test]
    async fn run_git_closes_inherited_stdin() {
        let temp_dir = TempDir::new().unwrap();
        let input_path = temp_dir.path().join("server-stdin");
        std::fs::write(&input_path, "unexpected input\n").unwrap();
        let input = std::fs::File::open(input_path).unwrap();

        let mut command = tokio::process::Command::new("/bin/sh");
        command
            .kill_on_drop(true)
            .process_group(0)
            .stdin(Stdio::from(input))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .arg("-c")
            .arg("if IFS= read -r line; then echo \"read inherited stdin: $line\" >&2; exit 9; fi");

        let result = run_git(command, "test detached stdin").await;
        assert!(result.is_ok(), "git command read server stdin: {result:?}");
    }
}
