// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! [`GitProviderManagerError`], the manager's typed error, and its message helpers.

use super::*;

#[derive(Error, Debug)]
pub enum GitProviderManagerError {
    #[error("Database error: {0}")]
    DatabaseError(#[from] sea_orm::DbErr),

    #[error("Provider error: {0}")]
    ProviderError(#[from] GitProviderError),

    #[error("Provider not found: {0}")]
    ProviderNotFound(String),

    #[error("Connection not found: {0}")]
    ConnectionNotFound(String),

    #[error("Invalid configuration: {0}")]
    InvalidConfiguration(String),
    #[error("Compose preview for '{path}' could not be rendered: {source}")]
    ComposePreview {
        path: String,
        #[source]
        source: temps_presets::ComposeParseError,
    },
    #[error("Repository not found: {0}")]
    RepositoryNotFound(String),

    #[error("JSON error: {0}")]
    JsonError(#[from] serde_json::Error),

    #[error("Sync already in progress")]
    SyncInProgress,

    #[error(
        "Sync for connection {connection_id} exceeded hard deadline of {deadline_secs}s and was aborted"
    )]
    SyncTimeout {
        connection_id: i32,
        deadline_secs: u64,
    },

    #[error("Connection token expired for connection ID {connection_id}. Please update your access token.")]
    ConnectionTokenExpired { connection_id: i32 },

    #[error(
        "Failed to create repository '{repository_name}' through connection {connection_id}: {source}"
    )]
    RepositoryCreation {
        connection_id: i32,
        repository_name: String,
        #[source]
        source: GitProviderError,
    },

    #[error(
        "Failed to {operation} in '{repository}' through connection {connection_id}: {reason}"
    )]
    RepositoryOperation {
        connection_id: i32,
        repository: String,
        operation: &'static str,
        reason: String,
    },

    #[error("Queue error: {0}")]
    QueueError(String),

    #[error("OAuth state not found or expired: {0}")]
    OAuthStateInvalid(String),

    /// A requested file cannot be committed where it was asked for, given
    /// what the branch already holds (a symlink or file in its way, a
    /// directory or submodule at that path).
    #[error(
        "Cannot commit '{path}' to branch '{branch}' of '{repository}' (connection {connection_id}): {reason}"
    )]
    CommitPathRejected {
        connection_id: i32,
        repository: String,
        branch: String,
        path: String,
        reason: String,
    },

    /// The branch does not exist yet and the base it was asked to start
    /// from does not exist on the remote either.
    #[error(
        "Cannot start branch '{branch}' of '{repository}' (connection {connection_id}) from '{base_branch}': the remote has no branch '{base_branch}'"
    )]
    BaseBranchNotFound {
        connection_id: i32,
        repository: String,
        branch: String,
        base_branch: String,
    },

    /// Files the commit would replace were changed on the branch since the
    /// revision the new content was written against, so committing them
    /// whole would undo those changes.
    #[error(
        "Cannot commit to branch '{branch}' of '{repository}' (connection {connection_id}): {} changed on the branch since commit {since_revision}, and committing the whole files would undo those changes",
        .paths.join(", ")
    )]
    FilesChangedSince {
        connection_id: i32,
        repository: String,
        branch: String,
        since_revision: String,
        paths: Vec<String>,
    },

    /// The revision the files were checked against is not in the remote's
    /// history (rewritten or never pushed).
    #[error(
        "Cannot check branch '{branch}' of '{repository}' (connection {connection_id}) against commit {revision}: the remote has no such commit"
    )]
    RevisionNotFound {
        connection_id: i32,
        repository: String,
        branch: String,
        revision: String,
    },

    /// Every requested file already has the requested content on the branch.
    #[error(
        "Nothing to commit to branch '{branch}' of '{repository}' (connection {connection_id}): the {file_count} requested file(s) already match commit {parent_sha}"
    )]
    NothingToCommit {
        connection_id: i32,
        repository: String,
        branch: String,
        file_count: usize,
        parent_sha: String,
    },

    /// The remote accepted the connection but refused to update the ref
    /// (protected branch, declined by a hook, non-fast-forward).
    #[error(
        "The remote rejected the update of '{refname}' (branch '{branch}') in '{repository}' (connection {connection_id}): {status}"
    )]
    PushRejected {
        connection_id: i32,
        repository: String,
        branch: String,
        refname: String,
        status: String,
    },

    #[error(
        "Commit to branch '{branch}' of '{repository}' (connection {connection_id}) timed out after {timeout_secs}s; {}",
        commit_timeout_outcome(*.push_may_have_completed, .branch)
    )]
    CommitTimedOut {
        connection_id: i32,
        repository: String,
        branch: String,
        timeout_secs: u64,
        /// Whether the push had already started when the deadline passed,
        /// in which case it may still land on the remote.
        push_may_have_completed: bool,
    },
}

/// The honest second half of a commit-timeout message: whether retrying is
/// safe or could produce the same commit twice.
fn commit_timeout_outcome(push_may_have_completed: bool, branch: &str) -> String {
    if push_may_have_completed {
        format!(
            "the push to '{branch}' had already started and may still have completed, check the branch before retrying"
        )
    } else {
        format!("the commit was abandoned before anything was pushed to '{branch}', so it is safe to retry")
    }
}

/// A connection the caller may not use: the same answer whether it is missing
/// or another user's, so this does not reveal which connections exist.
pub(super) fn foreign_or_missing_connection(connection_id: i32) -> GitProviderManagerError {
    GitProviderManagerError::ConnectionNotFound(format!(
        "Git connection {connection_id} does not exist or is not one of yours"
    ))
}

impl GitProviderManagerError {
    /// Whether the provider reported that the requested file does not exist,
    /// as opposed to failing to answer.
    pub fn is_file_not_found(&self) -> bool {
        matches!(
            self,
            Self::ProviderError(GitProviderError::FileNotFound { .. })
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_commit_timeout_says_whether_a_retry_is_safe() {
        let timed_out = |push_may_have_completed| {
            GitProviderManagerError::CommitTimedOut {
                connection_id: 3,
                repository: "acme/app".to_string(),
                branch: "feature/x".to_string(),
                timeout_secs: 120,
                push_may_have_completed,
            }
            .to_string()
        };
        let safe = timed_out(false);
        assert!(safe.contains("connection 3") && safe.contains("120s"));
        assert!(safe.contains("safe to retry"), "{safe}");
        let unsure = timed_out(true);
        assert!(
            unsure.contains("may still have completed")
                && unsure.contains("check the branch before retrying")
                && unsure.contains("'feature/x'"),
            "{unsure}"
        );
    }
}
