// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The exact single-branch lookup through a connection, and the rule for
//! when the provider's negative answer may be trusted.
//!
//! A branch listing is capped, so a name absent from it proves nothing; this
//! lookup reads the branch by name instead. Its 404 is still not proof of
//! absence on its own: GitHub, GitLab, Gitea and Bitbucket answer 404 for
//! every branch of a private repository the credential cannot see (a
//! fine-grained token whose repository selection no longer includes it, a
//! revoked membership), and the connection's token check passes regardless.
//! So a branch is only reported missing once the repository itself has been
//! read through the same connection.

use super::*;
use crate::services::git_provider::Branch;
use std::fmt;

/// What a connection's provider said about one branch, once checked.
#[derive(Debug, Clone)]
pub enum BranchLookupOutcome {
    /// The provider returned the branch. `is_default` follows the provider's
    /// live default branch, or the synced copy when that could not be read.
    Found { branch: Branch, is_default: bool },
    /// The provider has no such branch, and the repository is readable
    /// through the connection, so the absence is real.
    Missing,
}

/// Which provider request of a branch lookup failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchLookupStep {
    /// Reading the branch itself.
    ReadBranch,
    /// Reading the repository after the branch came back 404, to tell a
    /// missing branch from a repository the connection cannot see.
    ConfirmRepository,
}

impl fmt::Display for BranchLookupStep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::ReadBranch => "reading the branch",
            Self::ConfirmRepository => "reading the repository to confirm the branch is missing",
        })
    }
}

/// A branch lookup that produced no answer about the branch.
#[derive(Debug, Error)]
pub enum BranchLookupError {
    /// The synced repository, the connection, its provider or its token
    /// could not be resolved, so no provider request about the branch was
    /// made.
    #[error(
        "Could not look up branches of {owner}/{repo} through connection {connection_id}: {source}"
    )]
    Connection {
        connection_id: i32,
        owner: String,
        repo: String,
        #[source]
        source: Box<GitProviderManagerError>,
    },
    /// The provider failed at `step`. Never a statement that the branch is
    /// missing.
    #[error("Git provider failed while {step} for branch '{branch}' of {owner}/{repo} (connection {connection_id}): {source}")]
    Provider {
        step: BranchLookupStep,
        connection_id: i32,
        owner: String,
        repo: String,
        branch: String,
        #[source]
        source: Box<GitProviderError>,
    },
}

impl GitProviderManager {
    /// Look up `branch` of `owner/repo` by its exact name through
    /// `connection_id`.
    ///
    /// Makes two or three provider requests: the token check (for a PAT),
    /// the branch, and the repository, which gives a found branch its live
    /// default flag and confirms a 404 is a missing branch rather than a
    /// repository the credential can no longer see.
    pub async fn look_up_branch(
        &self,
        connection_id: i32,
        owner: &str,
        repo: &str,
        branch: &str,
    ) -> Result<BranchLookupOutcome, BranchLookupError> {
        let connection_error = |source| BranchLookupError::Connection {
            connection_id,
            owner: owner.to_string(),
            repo: repo.to_string(),
            source: Box::new(source),
        };
        let provider_error = |step, source| BranchLookupError::Provider {
            step,
            connection_id,
            owner: owner.to_string(),
            repo: repo.to_string(),
            branch: branch.to_string(),
            source: Box::new(source),
        };

        let repository = self
            .get_repository_by_owner_and_name_in_connection(owner, repo, connection_id)
            .await
            .map_err(connection_error)?;
        let connection = self
            .get_connection(connection_id)
            .await
            .map_err(connection_error)?;
        let provider_service = self
            .get_provider_service(connection.provider_id)
            .await
            .map_err(connection_error)?;
        let access_token = self
            .get_connection_token(connection_id)
            .await
            .map_err(connection_error)?;

        match provider_service
            .get_branch(&access_token, owner, repo, branch)
            .await
        {
            Ok(Some(found)) => {
                let default_branch = match provider_service
                    .get_repository(&access_token, owner, repo)
                    .await
                {
                    Ok(info) => info.default_branch,
                    Err(error) => {
                        debug!(
                            repository_id = repository.id,
                            connection_id,
                            error = %error,
                            "Live default branch unavailable; using the synced one"
                        );
                        repository.default_branch.clone()
                    }
                };
                Ok(BranchLookupOutcome::Found {
                    is_default: found.name == default_branch,
                    branch: found,
                })
            }
            Ok(None) => match provider_service
                .get_repository(&access_token, owner, repo)
                .await
            {
                Ok(_) => Ok(BranchLookupOutcome::Missing),
                Err(source) => {
                    warn!(
                        repository_id = repository.id,
                        connection_id,
                        owner = %owner,
                        repository = %repo,
                        branch = %branch,
                        error = %source,
                        "Branch not found, and the repository could not be read to confirm it"
                    );
                    Err(provider_error(BranchLookupStep::ConfirmRepository, source))
                }
            },
            Err(source) => {
                warn!(
                    repository_id = repository.id,
                    connection_id,
                    owner = %owner,
                    repository = %repo,
                    branch = %branch,
                    error = %source,
                    "Git provider branch lookup failed"
                );
                Err(provider_error(BranchLookupStep::ReadBranch, source))
            }
        }
    }
}

#[cfg(test)]
mod tests;
