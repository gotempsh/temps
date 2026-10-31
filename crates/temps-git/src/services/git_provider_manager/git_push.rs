// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Native git commit and push: stage files onto a branch's tree in memory,
//! refuse stale or unsafe paths, and push with the connection's credentials.

use super::*;

const COMMIT_RUNNING: u8 = 0;

const COMMIT_CANCELLED: u8 = 1;

const COMMIT_PUSHING: u8 = 2;

/// Shared between a blocking commit worker and the caller that may stop
/// waiting for it.
///
/// The worker checks [`Self::is_cancelled`] while cloning and must win
/// [`Self::begin_push`] before it pushes; the caller calls [`Self::cancel`]
/// when its deadline passes. Both transitions are a single compare-exchange,
/// so the caller always knows whether the push could still land.
#[derive(Debug, Default)]
pub(crate) struct CommitCancellation {
    state: std::sync::atomic::AtomicU8,
}

impl CommitCancellation {
    /// Whether the caller has given up on this commit.
    fn is_cancelled(&self) -> bool {
        self.state.load(std::sync::atomic::Ordering::Acquire) == COMMIT_CANCELLED
    }

    /// Claim the right to push. `false` when the caller has already given up,
    /// in which case the worker must not push.
    fn begin_push(&self) -> bool {
        self.state
            .compare_exchange(
                COMMIT_RUNNING,
                COMMIT_PUSHING,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .is_ok()
    }

    /// Give up on the commit. Returns `true` when the push had already
    /// started, so it may still complete on the remote.
    pub(super) fn cancel(&self) -> bool {
        match self.state.compare_exchange(
            COMMIT_RUNNING,
            COMMIT_CANCELLED,
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
        ) {
            Ok(_) => false,
            Err(current) => current == COMMIT_PUSHING,
        }
    }
}

/// Everything [`GitProviderManager::push_files_with_git`] needs to know about
/// where the commit goes and who makes it.
pub(super) struct GitPushRequest<'a> {
    /// Connection the push is made through, for error context.
    pub(super) connection_id: i32,
    /// `owner/name` of the target repository, for error context.
    pub(super) repository: &'a str,
    pub(super) clone_url: &'a str,
    /// HTTPS username the provider expects alongside the token
    /// (`x-access-token`, GitLab's `oauth2`, Bitbucket's `x-token-auth`).
    pub(super) clone_username: &'a str,
    pub(super) access_token: &'a str,
    pub(super) branch: &'a str,
    /// Branch a `branch` that does not exist yet starts from; `None`: the
    /// repository's default branch.
    pub(super) base_branch: Option<&'a str>,
    /// Revision the files' new content was written against. When set, the
    /// commit is refused with [`GitProviderManagerError::FilesChangedSince`]
    /// if any of the files differs between it and the tree the commit is
    /// built on.
    pub(super) unchanged_since: Option<&'a str>,
    pub(super) commit_message: &'a str,
    pub(super) author_name: &'a str,
    pub(super) author_email: &'a str,
}

/// `clone_url` with the provider's HTTPS `username` and `token` as its
/// userinfo, replacing any user the provider's clone link already carries
/// (Bitbucket's `https://user@bitbucket.org/...`). Credentials are
/// percent-encoded. A URL that is not HTTPS is returned unchanged.
fn authenticated_clone_url(clone_url: &str, username: &str, token: &str) -> String {
    match url::Url::parse(clone_url) {
        Ok(mut url) if url.scheme() == "https" => {
            if url.set_username(username).is_ok() && url.set_password(Some(token)).is_ok() {
                url.to_string()
            } else {
                clone_url.to_string()
            }
        }
        _ => clone_url.to_string(),
    }
}

/// Why a requested file could not be staged onto the branch's tree.
#[derive(Error, Debug)]
enum CommitStagingError {
    #[error("'{path}' {reason}")]
    Rejected { path: String, reason: String },

    #[error("Failed to stage '{path}': {source}")]
    Git {
        path: String,
        #[source]
        source: git2::Error,
    },
}

const GIT_MODE_TREE: i32 = 0o040000;

const GIT_MODE_BLOB: i32 = 0o100644;

const GIT_MODE_BLOB_EXECUTABLE: i32 = 0o100755;

const GIT_MODE_SYMLINK: i32 = 0o120000;

const GIT_MODE_SUBMODULE: i32 = 0o160000;

/// Why [`paths_changed_since`] could not compare.
#[derive(Debug)]
enum ChangedSinceError {
    /// `revision` is not a commit in the repository.
    RevisionNotFound,
    Git(git2::Error),
}

/// The paths of `files` whose entry in `tree` (the tree a commit is about to
/// be built on; `None` for an empty repository) differs from their entry at
/// commit `revision`: other content, another mode, or present in one and not
/// the other. Empty when committing the files whole replaces exactly what
/// `revision` had.
fn paths_changed_since(
    repo: &git2::Repository,
    revision: &str,
    tree: Option<&git2::Tree<'_>>,
    files: &[(String, Vec<u8>)],
) -> Result<Vec<String>, ChangedSinceError> {
    let oid = git2::Oid::from_str(revision).map_err(|_| ChangedSinceError::RevisionNotFound)?;
    let since = match repo.find_commit(oid) {
        Ok(commit) => commit.tree().map_err(ChangedSinceError::Git)?,
        Err(e) if e.code() == git2::ErrorCode::NotFound => {
            return Err(ChangedSinceError::RevisionNotFound)
        }
        Err(e) => return Err(ChangedSinceError::Git(e)),
    };
    let entry = |tree: &git2::Tree<'_>,
                 path: &str|
     -> Result<Option<(git2::Oid, i32)>, ChangedSinceError> {
        match tree.get_path(std::path::Path::new(path)) {
            Ok(entry) => Ok(Some((entry.id(), entry.filemode()))),
            Err(e) if e.code() == git2::ErrorCode::NotFound => Ok(None),
            Err(e) => Err(ChangedSinceError::Git(e)),
        }
    };
    let mut changed = Vec::new();
    for (path, _) in files {
        let now = match tree {
            Some(tree) => entry(tree, path)?,
            None => None,
        };
        if now != entry(&since, path)? {
            changed.push(path.clone());
        }
    }
    Ok(changed)
}

/// Build the tree for a commit: `base` (the branch tip's tree, if any) with
/// every file in `files` written as a regular blob.
///
/// Nothing touches the filesystem: content goes straight into the object
/// database and an in-memory index, so no path in the repository, symlink or
/// otherwise, can redirect a write onto the host. A path is refused when the
/// branch has a symlink, file or submodule where one of its directories would
/// be, or a symlink, directory or submodule at the path itself, and when
/// another requested file needs it to be a directory. An existing executable
/// file keeps its executable mode.
fn stage_files_onto(
    repo: &git2::Repository,
    base: Option<&git2::Tree<'_>>,
    files: &[(String, Vec<u8>)],
) -> Result<git2::Oid, CommitStagingError> {
    let rejected = |path: &str, reason: String| CommitStagingError::Rejected {
        path: path.to_string(),
        reason,
    };
    let git_error = |path: &str| {
        let path = path.to_string();
        move |source: git2::Error| CommitStagingError::Git { path, source }
    };
    // What the branch holds at `path`, or `None` when nothing is there.
    let base_mode = |path: &str| -> Result<Option<i32>, CommitStagingError> {
        let Some(tree) = base else {
            return Ok(None);
        };
        match tree.get_path(std::path::Path::new(path)) {
            Ok(entry) => Ok(Some(entry.filemode())),
            Err(e) if e.code() == git2::ErrorCode::NotFound => Ok(None),
            Err(e) => Err(git_error(path)(e)),
        }
    };

    let requested: std::collections::HashSet<&str> =
        files.iter().map(|(path, _)| path.as_str()).collect();
    if requested.len() != files.len() {
        let mut seen = std::collections::HashSet::with_capacity(files.len());
        if let Some((path, _)) = files.iter().find(|(path, _)| !seen.insert(path.as_str())) {
            return Err(rejected(path, "is requested more than once".to_string()));
        }
    }

    let mut index = git2::Index::new().map_err(git_error("<index>"))?;
    if let Some(tree) = base {
        index.read_tree(tree).map_err(git_error("<branch tree>"))?;
    }

    for (path, content) in files {
        let bad_segment = |segment: &str| {
            segment.is_empty()
                || segment == "."
                || segment == ".."
                || segment.eq_ignore_ascii_case(".git")
        };
        if path.starts_with('/')
            || path.contains('\\')
            || path.contains('\0')
            || path.split('/').any(bad_segment)
        {
            return Err(rejected(
                &path.escape_debug().to_string(),
                "is not a relative path of non-empty segments without '.', '..' or '.git'"
                    .to_string(),
            ));
        }

        // Every directory on the way must be a real directory, or not exist.
        let mut base_has_prefix = base.is_some();
        for (end, _) in path.match_indices('/') {
            let prefix = &path[..end];
            if requested.contains(prefix) {
                return Err(rejected(
                    path,
                    format!("needs '{prefix}' to be a directory, but '{prefix}' is also requested as a file"),
                ));
            }
            if !base_has_prefix {
                continue;
            }
            match base_mode(prefix)? {
                None => base_has_prefix = false,
                Some(GIT_MODE_TREE) => {}
                Some(GIT_MODE_SYMLINK) => {
                    return Err(rejected(
                        path,
                        format!("goes through '{prefix}', which is a symbolic link on the branch; commit to the link's target path instead"),
                    ))
                }
                Some(GIT_MODE_SUBMODULE) => {
                    return Err(rejected(
                        path,
                        format!("goes through '{prefix}', which is a submodule on the branch"),
                    ))
                }
                Some(_) => {
                    return Err(rejected(
                        path,
                        format!("goes through '{prefix}', which is a file on the branch, not a directory"),
                    ))
                }
            }
        }

        let mode = match if base_has_prefix {
            base_mode(path)?
        } else {
            None
        } {
            None | Some(GIT_MODE_BLOB) => GIT_MODE_BLOB,
            Some(GIT_MODE_BLOB_EXECUTABLE) => GIT_MODE_BLOB_EXECUTABLE,
            Some(GIT_MODE_SYMLINK) => return Err(rejected(
                path,
                "is a symbolic link on the branch; it cannot be overwritten through this endpoint"
                    .to_string(),
            )),
            Some(GIT_MODE_TREE) => {
                return Err(rejected(path, "is a directory on the branch".to_string()))
            }
            Some(GIT_MODE_SUBMODULE) => {
                return Err(rejected(path, "is a submodule on the branch".to_string()))
            }
            Some(other) => {
                return Err(rejected(
                    path,
                    format!("has unsupported mode {other:o} on the branch"),
                ))
            }
        };

        let blob = repo.blob(content).map_err(git_error(path))?;
        let entry = git2::IndexEntry {
            ctime: git2::IndexTime::new(0, 0),
            mtime: git2::IndexTime::new(0, 0),
            dev: 0,
            ino: 0,
            // Both constants are positive octal file modes.
            mode: mode.unsigned_abs(),
            uid: 0,
            gid: 0,
            // Only a stat cache hint; nothing reads it for an in-memory index.
            file_size: u32::try_from(content.len()).unwrap_or(u32::MAX),
            id: blob,
            flags: 0,
            flags_extended: 0,
            path: path.as_bytes().to_vec(),
        };
        index.add(&entry).map_err(git_error(path))?;
    }

    index.write_tree_to(repo).map_err(git_error("<tree>"))
}

impl GitProviderManager {
    /// Commit `files` on top of `request.branch` and push it, using native
    /// git operations (git2) rather than one provider API call per blob.
    ///
    /// The repository is cloned bare into a temporary directory: nothing is
    /// checked out and no requested file is written to the filesystem. Each
    /// file becomes a blob in the object database and an entry in an
    /// in-memory index built from the branch tip's tree (see
    /// [`stage_files_onto`]), so a symlink tracked in the repository can never
    /// redirect a write onto the host, and `.gitignore` cannot drop a
    /// requested file.
    ///
    /// A branch that does not exist on the remote is started from the tip of
    /// `base_branch` when one is given (refused with
    /// [`GitProviderManagerError::BaseBranchNotFound`] when the remote has no
    /// such branch), else from the default branch's tip; an empty repository
    /// gets a root commit. A commit that
    /// would not change the branch's tree is refused with
    /// [`GitProviderManagerError::NothingToCommit`], and a ref update the
    /// remote refuses (protected branch, hook, non-fast-forward) with
    /// [`GitProviderManagerError::PushRejected`].
    ///
    /// `cancellation` lets a caller that stopped waiting abort the clone and
    /// guarantee the push does not start afterwards.
    pub(super) fn push_files_with_git(
        request: &GitPushRequest<'_>,
        files: Vec<(String, Vec<u8>)>,
        cancellation: &CommitCancellation,
    ) -> Result<String, GitProviderManagerError> {
        use git2::{
            build::RepoBuilder, Cred, FetchOptions, PushOptions, RemoteCallbacks, Signature,
        };

        let branch = request.branch;
        let failed = |operation: &str, e: &dyn std::fmt::Display| {
            GitProviderManagerError::InvalidConfiguration(format!(
                "Failed to {operation} for branch '{branch}' of '{}' (connection {}): {}",
                request.repository,
                request.connection_id,
                redact_secret(&e.to_string(), request.access_token)
            ))
        };
        let abandoned = || GitProviderManagerError::CommitTimedOut {
            connection_id: request.connection_id,
            repository: request.repository.to_string(),
            branch: branch.to_string(),
            timeout_secs: COMMIT_FILES_TIMEOUT.as_secs(),
            push_may_have_completed: false,
        };

        let temp_dir =
            tempfile::tempdir().map_err(|e| failed("create a temporary directory", &e))?;

        // https://github.com/owner/repo.git -> https://USER:TOKEN@github.com/owner/repo.git
        let authenticated_url = authenticated_clone_url(
            request.clone_url,
            request.clone_username,
            request.access_token,
        );

        info!(
            connection_id = request.connection_id,
            repository = %request.repository,
            branch = %branch,
            "Cloning repository (bare) for a commit"
        );

        // Abort the transfer as soon as the caller gives up.
        let mut fetch_callbacks = RemoteCallbacks::new();
        fetch_callbacks.transfer_progress(|_| !cancellation.is_cancelled());
        let mut fetch_options = FetchOptions::new();
        fetch_options.remote_callbacks(fetch_callbacks);
        let cloned = RepoBuilder::new()
            .bare(true)
            .fetch_options(fetch_options)
            .clone(&authenticated_url, temp_dir.path());
        if cancellation.is_cancelled() {
            return Err(abandoned());
        }
        let repo = cloned.map_err(|e| failed("clone the repository", &e))?;

        // The commit's parent: the branch's remote tip, else the requested
        // base's tip, else the default branch's tip, else none (an empty
        // repository).
        let parent = match repo.find_reference(&format!("refs/remotes/origin/{branch}")) {
            Ok(reference) => Some(
                reference
                    .peel_to_commit()
                    .map_err(|e| failed("resolve the branch tip", &e))?,
            ),
            Err(e) if e.code() == git2::ErrorCode::NotFound && request.base_branch.is_some() => {
                let base = request.base_branch.unwrap_or_default();
                match repo.find_reference(&format!("refs/remotes/origin/{base}")) {
                    Ok(reference) => Some(
                        reference
                            .peel_to_commit()
                            .map_err(|e| failed("resolve the base branch tip", &e))?,
                    ),
                    Err(e) if e.code() == git2::ErrorCode::NotFound => {
                        return Err(GitProviderManagerError::BaseBranchNotFound {
                            connection_id: request.connection_id,
                            repository: request.repository.to_string(),
                            branch: branch.to_string(),
                            base_branch: base.to_string(),
                        });
                    }
                    Err(e) => return Err(failed("look up the base branch", &e)),
                }
            }
            Err(e) if e.code() == git2::ErrorCode::NotFound => {
                match repo.head() {
                    Ok(head) => Some(
                        head.peel_to_commit()
                            .map_err(|e| failed("resolve the default branch tip", &e))?,
                    ),
                    // An empty repository has an unborn HEAD.
                    Err(_) => None,
                }
            }
            Err(e) => return Err(failed("look up the branch", &e)),
        };
        let base_tree = parent
            .as_ref()
            .map(|commit| commit.tree())
            .transpose()
            .map_err(|e| failed("read the branch tree", &e))?;

        if let Some(revision) = request.unchanged_since {
            let changed = paths_changed_since(&repo, revision, base_tree.as_ref(), &files)
                .map_err(|e| match e {
                    ChangedSinceError::RevisionNotFound => {
                        GitProviderManagerError::RevisionNotFound {
                            connection_id: request.connection_id,
                            repository: request.repository.to_string(),
                            branch: branch.to_string(),
                            revision: revision.to_string(),
                        }
                    }
                    ChangedSinceError::Git(e) => {
                        failed(&format!("compare the files with commit {revision}"), &e)
                    }
                })?;
            if !changed.is_empty() {
                return Err(GitProviderManagerError::FilesChangedSince {
                    connection_id: request.connection_id,
                    repository: request.repository.to_string(),
                    branch: branch.to_string(),
                    since_revision: revision.to_string(),
                    paths: changed,
                });
            }
        }

        info!("Staging {} files onto branch '{}'", files.len(), branch);
        let tree_oid =
            stage_files_onto(&repo, base_tree.as_ref(), &files).map_err(|e| match e {
                CommitStagingError::Rejected { path, reason } => {
                    GitProviderManagerError::CommitPathRejected {
                        connection_id: request.connection_id,
                        repository: request.repository.to_string(),
                        branch: branch.to_string(),
                        path,
                        reason,
                    }
                }
                git @ CommitStagingError::Git { .. } => failed("stage files", &git),
            })?;

        if let Some(parent) = &parent {
            if parent.tree_id() == tree_oid {
                return Err(GitProviderManagerError::NothingToCommit {
                    connection_id: request.connection_id,
                    repository: request.repository.to_string(),
                    branch: branch.to_string(),
                    file_count: files.len(),
                    parent_sha: parent.id().to_string(),
                });
            }
        }

        let tree = repo
            .find_tree(tree_oid)
            .map_err(|e| failed("find the staged tree", &e))?;
        let signature = Signature::now(request.author_name, request.author_email)
            .map_err(|e| failed("create the commit signature", &e))?;
        let parents: Vec<&git2::Commit> = parent.iter().collect();
        let commit_oid = repo
            .commit(
                None,
                &signature,
                &signature,
                request.commit_message,
                &tree,
                &parents,
            )
            .map_err(|e| failed("create the commit", &e))?;
        let local_ref = format!("refs/heads/{branch}");
        repo.reference(&local_ref, commit_oid, true, "temps: commit files")
            .map_err(|e| failed("point the local branch at the commit", &e))?;

        let commit_sha = commit_oid.to_string();
        info!("Created local commit: {}", commit_sha);

        let mut remote = repo
            .find_remote("origin")
            .map_err(|e| failed("find the remote", &e))?;

        // git2 reports a server's per-ref refusal only through this callback;
        // `push` itself still returns Ok.
        let rejections: std::cell::RefCell<Vec<(String, String)>> =
            std::cell::RefCell::new(Vec::new());
        let mut callbacks = RemoteCallbacks::new();
        callbacks.credentials(|_url, _username_from_url, _allowed_types| {
            Cred::userpass_plaintext(request.clone_username, request.access_token)
        });
        callbacks.push_update_reference(|refname, status| {
            if let Some(status) = status {
                rejections
                    .borrow_mut()
                    .push((refname.to_string(), status.to_string()));
            }
            Ok(())
        });
        let mut push_options = PushOptions::new();
        push_options.remote_callbacks(callbacks);

        let refspec = format!("{local_ref}:{local_ref}");
        // Past this point the caller can no longer promise the push did not
        // happen; refuse to start it if the caller has already given up.
        if !cancellation.begin_push() {
            return Err(abandoned());
        }
        info!("Pushing to remote with refspec: {}", refspec);
        remote
            .push(&[&refspec], Some(&mut push_options))
            .map_err(|e| failed("push to the remote", &e))?;

        if let Some((refname, status)) = rejections.borrow().first() {
            return Err(GitProviderManagerError::PushRejected {
                connection_id: request.connection_id,
                repository: request.repository.to_string(),
                branch: branch.to_string(),
                refname: refname.clone(),
                status: redact_secret(status, request.access_token),
            });
        }

        info!(
            "Successfully pushed {} files with commit {}",
            files.len(),
            commit_sha
        );

        Ok(commit_sha)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bare_remote() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().expect("temp dir");
        let repo = git2::Repository::init_bare(dir.path()).expect("bare repo");
        // A provider's repository names its default branch in HEAD.
        repo.set_head("refs/heads/main")
            .expect("point HEAD at main");
        let url = dir.path().to_str().expect("utf-8 path").to_string();
        (dir, url)
    }

    fn files_at(remote: &str, branch: &str) -> Vec<(String, String)> {
        let repo = git2::Repository::open_bare(remote).expect("open remote");
        let commit = repo
            .find_reference(&format!("refs/heads/{branch}"))
            .expect("branch exists on the remote")
            .peel_to_commit()
            .expect("branch tip");
        let tree = commit.tree().expect("tree");
        let mut found = Vec::new();
        tree.walk(git2::TreeWalkMode::PreOrder, |dir, entry| {
            if let Ok(name) = entry.name() {
                found.push((format!("{dir}{name}"), entry.id().to_string()));
            }
            git2::TreeWalkResult::Ok
        })
        .expect("walk");
        found
    }

    /// Push `files` to `branch` of the local remote at `url` with a fresh,
    /// never-cancelled worker.
    fn push_to(
        url: &str,
        branch: &str,
        files: Vec<(String, Vec<u8>)>,
    ) -> Result<String, GitProviderManagerError> {
        push_to_with(url, branch, files, &CommitCancellation::default())
    }

    fn push_to_with(
        url: &str,
        branch: &str,
        files: Vec<(String, Vec<u8>)>,
        cancellation: &CommitCancellation,
    ) -> Result<String, GitProviderManagerError> {
        push_onto(url, branch, None, files, cancellation)
    }

    fn push_onto(
        url: &str,
        branch: &str,
        base_branch: Option<&str>,
        files: Vec<(String, Vec<u8>)>,
        cancellation: &CommitCancellation,
    ) -> Result<String, GitProviderManagerError> {
        push_checked(url, branch, base_branch, None, files, cancellation)
    }

    fn push_checked(
        url: &str,
        branch: &str,
        base_branch: Option<&str>,
        unchanged_since: Option<&str>,
        files: Vec<(String, Vec<u8>)>,
        cancellation: &CommitCancellation,
    ) -> Result<String, GitProviderManagerError> {
        let request = GitPushRequest {
            connection_id: 42,
            repository: "acme/app",
            clone_url: url,
            clone_username: "x-access-token",
            access_token: "unused-token",
            branch,
            base_branch,
            unchanged_since,
            commit_message: "Commit files",
            author_name: "Test Author",
            author_email: "bot@example.invalid",
        };
        GitProviderManager::push_files_with_git(&request, files, cancellation)
    }

    /// Commit `entries` (path, git file mode, content) straight into the bare
    /// remote at `url` as the new tip of `branch`, the way a user's own push
    /// would land arbitrary entries, symlinks included.
    fn seed_remote(url: &str, branch: &str, entries: &[(&str, u32, &[u8])]) -> git2::Oid {
        let repo = git2::Repository::open_bare(url).expect("open remote");
        let mut index = git2::Index::new().expect("in-memory index");
        for (path, mode, content) in entries {
            let id = repo.blob(content).expect("blob");
            index
                .add(&git2::IndexEntry {
                    ctime: git2::IndexTime::new(0, 0),
                    mtime: git2::IndexTime::new(0, 0),
                    dev: 0,
                    ino: 0,
                    mode: *mode,
                    uid: 0,
                    gid: 0,
                    file_size: content.len() as u32,
                    id,
                    flags: 0,
                    flags_extended: 0,
                    path: path.as_bytes().to_vec(),
                })
                .expect("add entry");
        }
        let tree_id = index.write_tree_to(&repo).expect("write tree");
        let tree = repo.find_tree(tree_id).expect("tree");
        let signature = git2::Signature::now("Seeder", "seed@example.invalid").expect("sig");
        let parent = repo
            .find_reference(&format!("refs/heads/{branch}"))
            .ok()
            .and_then(|r| r.peel_to_commit().ok());
        let parents: Vec<&git2::Commit> = parent.iter().collect();
        repo.commit(
            Some(&format!("refs/heads/{branch}")),
            &signature,
            &signature,
            "Seed",
            &tree,
            &parents,
        )
        .expect("seed commit")
    }

    fn remote_tip(url: &str, branch: &str) -> git2::Oid {
        git2::Repository::open_bare(url)
            .expect("open remote")
            .find_reference(&format!("refs/heads/{branch}"))
            .expect("branch")
            .peel_to_commit()
            .expect("tip")
            .id()
    }

    fn entry_at(url: &str, branch: &str, path: &str) -> Option<(i32, Vec<u8>)> {
        let repo = git2::Repository::open_bare(url).expect("open remote");
        let tree = repo
            .find_reference(&format!("refs/heads/{branch}"))
            .expect("branch")
            .peel_to_commit()
            .expect("tip")
            .tree()
            .expect("tree");
        let entry = tree.get_path(std::path::Path::new(path)).ok()?;
        let content = repo
            .find_blob(entry.id())
            .map(|blob| blob.content().to_vec())
            .unwrap_or_default();
        Some((entry.filemode(), content))
    }

    fn assert_path_rejected(error: GitProviderManagerError, expected_path: &str, needle: &str) {
        match error {
            GitProviderManagerError::CommitPathRejected {
                connection_id,
                ref repository,
                ref branch,
                ref path,
                ref reason,
            } => {
                assert_eq!(connection_id, 42);
                assert_eq!(repository, "acme/app");
                assert_eq!(branch, "main");
                assert_eq!(path, expected_path);
                assert!(reason.contains(needle), "unexpected reason: {reason}");
            }
            other => panic!("expected CommitPathRejected, got {other:?}"),
        }
    }

    #[test]
    fn a_tracked_symlink_in_the_path_cannot_redirect_a_write_outside_the_checkout() {
        let (_remote_dir, url) = bare_remote();
        let outside = tempfile::tempdir().expect("outside dir");
        let target = outside.path().to_str().expect("utf-8 path").to_string();
        let before = seed_remote(
            &url,
            "main",
            &[
                ("README.md", 0o100644, b"hello"),
                ("x", 0o120000, target.as_bytes()),
                ("nested/link", 0o120000, target.as_bytes()),
            ],
        );

        for (path, through) in [
            ("x/file", "'x'"),
            ("nested/link/deeper/file", "'nested/link'"),
        ] {
            let error = push_to(&url, "main", vec![(path.to_string(), b"pwned".to_vec())])
                .expect_err("a path through a symlink must be refused");
            assert_path_rejected(error, path, through);
        }

        let written: Vec<_> = std::fs::read_dir(outside.path())
            .expect("read outside dir")
            .collect();
        assert!(
            written.is_empty(),
            "nothing may be written through the link"
        );
        assert_eq!(remote_tip(&url, "main"), before, "nothing was pushed");
    }

    #[test]
    fn a_new_branch_starts_from_the_requested_base_not_the_default_branch() {
        let (_remote_dir, url) = bare_remote();
        seed_remote(&url, "main", &[("app.rs", 0o100644, b"newer on main")]);
        let production = seed_remote(&url, "production", &[("app.rs", 0o100644, b"live")]);

        push_onto(
            &url,
            "autofix/7",
            Some("production"),
            vec![("fix.txt".to_string(), b"fix".to_vec())],
            &CommitCancellation::default(),
        )
        .expect("push onto production");

        let remote = git2::Repository::open_bare(&url).expect("open remote");
        let tip = remote
            .find_reference("refs/heads/autofix/7")
            .expect("branch")
            .peel_to_commit()
            .expect("tip");
        assert_eq!(tip.parent_id(0).expect("parent"), production);
        assert_eq!(
            entry_at(&url, "autofix/7", "app.rs").map(|(_, content)| content),
            Some(b"live".to_vec()),
            "main's newer content must not leak into a branch based on production"
        );
    }

    #[test]
    fn files_edited_on_the_branch_since_the_revision_refuse_the_commit() {
        let (_remote_dir, url) = bare_remote();
        let crashing = seed_remote(
            &url,
            "main",
            &[
                ("app.rs", 0o100644, b"crashing"),
                ("lib.rs", 0o100644, b"lib"),
            ],
        );
        // A teammate edits app.rs and adds new.rs after the crash.
        let newer = seed_remote(
            &url,
            "main",
            &[
                ("app.rs", 0o100644, b"teammate's edit"),
                ("lib.rs", 0o100644, b"lib"),
                ("new.rs", 0o100644, b"added later"),
            ],
        );

        let error = push_checked(
            &url,
            "autofix/9",
            Some("main"),
            Some(&crashing.to_string()),
            vec![
                ("app.rs".to_string(), b"fixed".to_vec()),
                ("lib.rs".to_string(), b"lib, fixed".to_vec()),
                ("new.rs".to_string(), b"fixed".to_vec()),
            ],
            &CommitCancellation::default(),
        )
        .expect_err("replacing files edited since the crash must be refused");
        match error {
            GitProviderManagerError::FilesChangedSince {
                connection_id,
                ref branch,
                ref since_revision,
                ref paths,
                ..
            } => {
                assert_eq!(connection_id, 42);
                assert_eq!(branch, "autofix/9");
                assert_eq!(since_revision, &crashing.to_string());
                assert_eq!(paths, &["app.rs".to_string(), "new.rs".to_string()]);
            }
            other => panic!("expected FilesChangedSince, got {other:?}"),
        }
        assert!(git2::Repository::open_bare(&url)
            .expect("open remote")
            .find_reference("refs/heads/autofix/9")
            .is_err());
        assert_eq!(remote_tip(&url, "main"), newer);
    }

    #[test]
    fn edits_to_other_files_since_the_revision_do_not_block_the_commit() {
        let (_remote_dir, url) = bare_remote();
        let crashing = seed_remote(
            &url,
            "main",
            &[
                ("app.rs", 0o100644, b"crashing"),
                ("lib.rs", 0o100644, b"lib"),
            ],
        );
        let newer = seed_remote(
            &url,
            "main",
            &[
                ("app.rs", 0o100644, b"crashing"),
                ("lib.rs", 0o100644, b"lib, edited later"),
            ],
        );

        push_checked(
            &url,
            "autofix/10",
            Some("main"),
            Some(&crashing.to_string()),
            vec![("app.rs".to_string(), b"fixed".to_vec())],
            &CommitCancellation::default(),
        )
        .expect("only app.rs is replaced and it is unchanged since the crash");

        let remote = git2::Repository::open_bare(&url).expect("open remote");
        let tip = remote
            .find_reference("refs/heads/autofix/10")
            .expect("branch")
            .peel_to_commit()
            .expect("tip");
        assert_eq!(tip.parent_id(0).expect("parent"), newer);
        assert_eq!(
            entry_at(&url, "autofix/10", "lib.rs").map(|(_, content)| content),
            Some(b"lib, edited later".to_vec()),
            "the later edit to another file is kept"
        );
        assert_eq!(
            entry_at(&url, "autofix/10", "app.rs").map(|(_, content)| content),
            Some(b"fixed".to_vec())
        );
    }

    #[test]
    fn a_revision_missing_from_the_history_is_refused() {
        let (_remote_dir, url) = bare_remote();
        seed_remote(&url, "main", &[("app.rs", 0o100644, b"main")]);
        let missing = "0123456789abcdef0123456789abcdef01234567";

        let error = push_checked(
            &url,
            "autofix/11",
            Some("main"),
            Some(missing),
            vec![("app.rs".to_string(), b"fixed".to_vec())],
            &CommitCancellation::default(),
        )
        .expect_err("a revision the remote does not have cannot be checked");
        match error {
            GitProviderManagerError::RevisionNotFound {
                ref branch,
                ref revision,
                ..
            } => {
                assert_eq!(branch, "autofix/11");
                assert_eq!(revision, missing);
            }
            other => panic!("expected RevisionNotFound, got {other:?}"),
        }
    }

    #[test]
    fn a_missing_base_branch_is_refused_instead_of_falling_back_to_the_default() {
        let (_remote_dir, url) = bare_remote();
        seed_remote(&url, "main", &[("app.rs", 0o100644, b"main")]);

        let error = push_onto(
            &url,
            "autofix/8",
            Some("production"),
            vec![("fix.txt".to_string(), b"fix".to_vec())],
            &CommitCancellation::default(),
        )
        .expect_err("a base the remote lacks must be refused");
        match error {
            GitProviderManagerError::BaseBranchNotFound {
                connection_id,
                ref branch,
                ref base_branch,
                ..
            } => {
                assert_eq!(connection_id, 42);
                assert_eq!(branch, "autofix/8");
                assert_eq!(base_branch, "production");
            }
            other => panic!("expected BaseBranchNotFound, got {other:?}"),
        }
        assert!(git2::Repository::open_bare(&url)
            .expect("open remote")
            .find_reference("refs/heads/autofix/8")
            .is_err());
    }

    #[test]
    fn the_clone_url_carries_the_providers_username_and_drops_its_own() {
        assert_eq!(
            authenticated_clone_url("https://github.com/acme/app.git", "x-access-token", "tok"),
            "https://x-access-token:tok@github.com/acme/app.git"
        );
        assert_eq!(
            authenticated_clone_url("https://gitlab.com/acme/app.git", "oauth2", "tok"),
            "https://oauth2:tok@gitlab.com/acme/app.git"
        );
        // Bitbucket's API clone link names the account in its authority.
        assert_eq!(
            authenticated_clone_url(
                "https://someone@bitbucket.org/acme/app.git",
                "x-token-auth",
                "tok"
            ),
            "https://x-token-auth:tok@bitbucket.org/acme/app.git"
        );
        assert_eq!(
            authenticated_clone_url("https://host/acme/app.git", "oauth2", "a/b@c"),
            "https://oauth2:a%2Fb%40c@host/acme/app.git"
        );
        assert_eq!(
            authenticated_clone_url("/srv/git/app.git", "oauth2", "tok"),
            "/srv/git/app.git"
        );
        assert_eq!(
            GitProviderManager::clone_username_for_provider_type("gitlab"),
            "oauth2"
        );
        assert_eq!(
            GitProviderManager::clone_username_for_provider_type("bitbucket"),
            "x-token-auth"
        );
    }

    #[test]
    fn a_tracked_symlink_as_the_final_component_cannot_be_overwritten() {
        let (_remote_dir, url) = bare_remote();
        let before = seed_remote(&url, "main", &[("x", 0o120000, b"/etc/hosts")]);

        let error = push_to(&url, "main", vec![("x".to_string(), b"data".to_vec())])
            .expect_err("overwriting a symlink must be refused");
        assert_path_rejected(error, "x", "symbolic link");
        assert_eq!(remote_tip(&url, "main"), before);
    }

    #[test]
    fn a_path_that_conflicts_with_the_branch_or_the_request_is_refused() {
        let (_remote_dir, url) = bare_remote();
        seed_remote(
            &url,
            "main",
            &[
                ("README.md", 0o100644, b"hello"),
                ("docs/guide.md", 0o100644, b"# Guide"),
            ],
        );

        let through_file = push_to(
            &url,
            "main",
            vec![("README.md/x".to_string(), b"x".to_vec())],
        )
        .expect_err("a file cannot become a directory");
        assert_path_rejected(through_file, "README.md/x", "is a file on the branch");

        let onto_dir = push_to(&url, "main", vec![("docs".to_string(), b"x".to_vec())])
            .expect_err("a directory cannot become a file");
        assert_path_rejected(onto_dir, "docs", "is a directory");

        let self_conflict = push_to(
            &url,
            "main",
            vec![
                ("a".to_string(), b"file".to_vec()),
                ("a/b".to_string(), b"nested".to_vec()),
            ],
        )
        .expect_err("one request cannot use a path as both file and directory");
        assert_path_rejected(self_conflict, "a/b", "also requested as a file");
    }

    #[test]
    fn nested_paths_ignored_files_and_executable_modes_are_committed_as_requested() {
        let (_remote_dir, url) = bare_remote();
        seed_remote(
            &url,
            "main",
            &[
                (".gitignore", 0o100644, b"dist/\n*.log\n"),
                ("bin/run.sh", 0o100755, b"#!/bin/sh\n"),
            ],
        );

        push_to(
            &url,
            "main",
            vec![
                ("dist/app.js".to_string(), b"console.log(1)".to_vec()),
                ("deep/nested/dir/file.txt".to_string(), b"nested".to_vec()),
                ("bin/run.sh".to_string(), b"#!/bin/sh\necho hi\n".to_vec()),
            ],
        )
        .expect("commit files");

        assert_eq!(
            entry_at(&url, "main", "dist/app.js"),
            Some((0o100644, b"console.log(1)".to_vec())),
            "a .gitignore'd path is still committed when asked for"
        );
        assert_eq!(
            entry_at(&url, "main", "deep/nested/dir/file.txt"),
            Some((0o100644, b"nested".to_vec()))
        );
        assert_eq!(
            entry_at(&url, "main", "bin/run.sh"),
            Some((0o100755, b"#!/bin/sh\necho hi\n".to_vec())),
            "an executable keeps its mode"
        );
        assert!(
            entry_at(&url, "main", ".gitignore").is_some(),
            "untouched files stay"
        );
    }

    #[test]
    fn committing_unchanged_content_is_refused_instead_of_pushing_an_empty_commit() {
        let (_remote_dir, url) = bare_remote();
        let tip = seed_remote(&url, "main", &[("README.md", 0o100644, b"hello")]);

        let error = push_to(
            &url,
            "main",
            vec![("README.md".to_string(), b"hello".to_vec())],
        )
        .expect_err("no change means no commit");
        match error {
            GitProviderManagerError::NothingToCommit {
                connection_id,
                ref branch,
                file_count,
                ref parent_sha,
                ..
            } => {
                assert_eq!(connection_id, 42);
                assert_eq!(branch, "main");
                assert_eq!(file_count, 1);
                assert_eq!(parent_sha, &tip.to_string());
            }
            other => panic!("expected NothingToCommit, got {other:?}"),
        }
        assert_eq!(remote_tip(&url, "main"), tip);
    }

    #[test]
    fn a_ref_update_the_remote_refuses_is_reported_not_returned_as_a_commit() {
        let (remote_dir, url) = bare_remote();
        let tip = seed_remote(&url, "main", &[("README.md", 0o100644, b"hello")]);
        // A held ref lock makes the remote refuse this ref while the push
        // itself still completes, the way a protected branch or a declining
        // hook does on a hosted provider.
        std::fs::write(remote_dir.path().join("refs/heads/main.lock"), b"")
            .expect("hold the ref lock");

        let error = push_to(
            &url,
            "main",
            vec![("README.md".to_string(), b"new".to_vec())],
        )
        .expect_err("a refused ref update is not a success");
        match error {
            GitProviderManagerError::PushRejected {
                connection_id,
                ref branch,
                ref refname,
                ref status,
                ..
            } => {
                assert_eq!(connection_id, 42);
                assert_eq!(branch, "main");
                assert_eq!(refname, "refs/heads/main");
                assert!(!status.is_empty());
            }
            other => panic!("expected PushRejected, got {other:?}"),
        }
        assert_eq!(remote_tip(&url, "main"), tip);
    }

    #[test]
    fn a_cancelled_commit_never_pushes() {
        let (_remote_dir, url) = bare_remote();
        let tip = seed_remote(&url, "main", &[("README.md", 0o100644, b"hello")]);
        let cancellation = CommitCancellation::default();
        assert!(!cancellation.cancel(), "nothing was pushing yet");

        let error = push_to_with(
            &url,
            "main",
            vec![("README.md".to_string(), b"late".to_vec())],
            &cancellation,
        )
        .expect_err("a cancelled worker must not push");
        assert!(matches!(
            error,
            GitProviderManagerError::CommitTimedOut {
                push_may_have_completed: false,
                ..
            }
        ));
        assert_eq!(remote_tip(&url, "main"), tip);
    }

    #[test]
    fn cancellation_reports_whether_the_push_had_started() {
        let before_push = CommitCancellation::default();
        assert!(!before_push.is_cancelled());
        assert!(!before_push.cancel(), "cancelled before the push started");
        assert!(before_push.is_cancelled());
        assert!(!before_push.begin_push(), "a cancelled worker may not push");

        let during_push = CommitCancellation::default();
        assert!(during_push.begin_push());
        assert!(during_push.cancel(), "the push was under way");
        assert!(!during_push.is_cancelled(), "a started push is not aborted");
    }

    #[test]
    fn pushing_files_populates_an_empty_repository_and_then_a_new_branch() {
        let (remote_dir, url) = bare_remote();

        // An empty repository has no HEAD commit: the first push creates the branch.
        let first = push_to(
            &url,
            "main",
            vec![
                ("README.md".to_string(), b"hello".to_vec()),
                ("cmd/app/main.go".to_string(), b"package main".to_vec()),
            ],
        )
        .expect("push into an empty repository");
        let main_files = files_at(&url, "main");
        assert!(main_files.iter().any(|(path, _)| path == "README.md"));
        assert!(main_files.iter().any(|(path, _)| path == "cmd/app/main.go"));

        // A branch that does not exist yet is started from the default branch.
        let second = push_to(
            &url,
            "feature/add-docs",
            vec![("docs/guide.md".to_string(), b"# Guide".to_vec())],
        )
        .expect("push to a new branch");
        assert_ne!(first, second);
        let feature_files = files_at(&url, "feature/add-docs");
        assert!(feature_files.iter().any(|(path, _)| path == "README.md"));
        assert!(feature_files
            .iter()
            .any(|(path, _)| path == "docs/guide.md"));
        // The default branch is untouched by the feature branch.
        assert!(!files_at(&url, "main")
            .iter()
            .any(|(path, _)| path == "docs/guide.md"));

        // Committing to an existing branch stacks on top of it.
        push_to(
            &url,
            "main",
            vec![("README.md".to_string(), b"hello again".to_vec())],
        )
        .expect("push to an existing branch");
        let repo = git2::Repository::open_bare(remote_dir.path()).expect("open remote");
        let tip = repo
            .find_reference("refs/heads/main")
            .expect("main")
            .peel_to_commit()
            .expect("tip");
        assert_eq!(tip.parent_count(), 1, "history is extended, not rewritten");
        assert_eq!(tip.parent_id(0).expect("parent").to_string(), first);
    }
}
