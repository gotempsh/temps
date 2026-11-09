// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Project-bound GitHub credentials stay on the control plane and worker. The
//! policy pins a connection and repository; changing project source cannot
//! transfer an old approval to a different account or repository.
use crate::{
    error::AccessError,
    types::{AccessMethod, GithubAccessProtocol, GithubRepositoryAccess, SandboxAccessPolicy},
};
use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
use std::sync::Arc;
use temps_core::secret_string::SecretString;
use temps_git::services::git_provider_manager_trait::GitProviderManagerTrait;
use tokenless_core::git::GitScope;

fn invalid(reason: &'static str) -> AccessError {
    AccessError::Validation {
        field: "github_repository",
        reason,
    }
}

pub(crate) fn scope(policy: &SandboxAccessPolicy) -> Result<Option<GitScope>, AccessError> {
    let Some(source) = &policy.github_repository else {
        return Ok(None);
    };
    let valid_name = |name: &str| {
        !name.is_empty()
            && name.len() <= 100
            && name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
            && name != "."
            && name != ".."
    };
    if source.connection_id <= 0
        || !valid_name(&source.owner)
        || !valid_name(&source.repository)
        || source.repository.ends_with(".git")
    {
        return Err(invalid("provide a positive connection ID and plain repository owner/name without a .git suffix"));
    }
    let repo = format!("{}/{}", source.owner, source.repository);
    let (origin, paths, methods) = match source.protocol {
        GithubAccessProtocol::Api => (
            "https://api.github.com",
            vec![format!("/repos/{repo}")],
            vec![AccessMethod::Get, AccessMethod::Head],
        ),
        GithubAccessProtocol::Git => (
            "https://github.com",
            vec![format!("/{repo}"), format!("/{repo}.git")],
            vec![AccessMethod::Get, AccessMethod::Post],
        ),
    };
    if policy.origin != origin || policy.path_prefixes != paths || policy.methods != methods {
        return Err(invalid("GitHub scope must match the selected repository and its read-only API or clone/fetch profile"));
    }
    match source.protocol {
        GithubAccessProtocol::Api => Ok(None),
        GithubAccessProtocol::Git => GitScope::new(vec![repo], false)
            .map(Some)
            .map_err(|_| invalid("cannot compile the GitHub repository clone/fetch scope")),
    }
}

/// Metadata-only query: never loads provider credentials or configuration.
/// Used on save, approval and before every issuance, with an active connection.
pub(crate) async fn validate_binding<C: ConnectionTrait>(
    db: &C,
    project_id: i32,
    source: &GithubRepositoryAccess,
) -> Result<(), AccessError> {
    let found = db.query_one(Statement::from_sql_and_values(DatabaseBackend::Postgres,
        "SELECT p.id FROM projects p JOIN git_provider_connections c ON c.id=p.git_provider_connection_id JOIN git_providers v ON v.id=c.provider_id WHERE p.id=$1 AND NOT p.is_deleted AND p.deleted_at IS NULL AND c.id=$2 AND lower(p.repo_owner)=lower($3) AND lower(p.repo_name)=lower($4) AND c.is_active AND NOT c.is_expired AND v.is_active AND v.provider_type='github' AND (v.base_url IS NULL OR v.base_url IN ('https://github.com','https://github.com/')) AND (v.api_url IS NULL OR v.api_url IN ('https://api.github.com','https://api.github.com/'))",
        [project_id.into(),source.connection_id.into(),source.owner.clone().into(),source.repository.clone().into()]))
        .await.map_err(|e| AccessError::database("validate project GitHub credential source",project_id,e))?;
    if found.is_none() {
        return Err(invalid(
            "the active GitHub connection and repository must match this project's current source",
        ));
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
#[error("Cannot resolve GitHub credential for connection {connection_id}: reauthorize or check the Git connection")]
pub struct GithubCredentialError {
    pub connection_id: i32,
}

#[async_trait::async_trait]
pub trait GithubCredentialResolver: Send + Sync {
    async fn resolve(&self, connection_id: i32) -> Result<SecretString, GithubCredentialError>;
}

pub struct ProjectGithubCredentials {
    manager: Arc<dyn GitProviderManagerTrait>,
}
impl ProjectGithubCredentials {
    pub fn new(manager: Arc<dyn GitProviderManagerTrait>) -> Self {
        Self { manager }
    }
}
#[async_trait::async_trait]
impl GithubCredentialResolver for ProjectGithubCredentials {
    async fn resolve(&self, connection_id: i32) -> Result<SecretString, GithubCredentialError> {
        // Manager handles refresh and reauthorization. Never log its error body:
        // provider responses may contain sensitive account details.
        let (token, provider) = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            self.manager.get_connection_access_token(connection_id),
        )
        .await
        .map_err(|_| GithubCredentialError { connection_id })?
        .map_err(|_| GithubCredentialError { connection_id })?;
        let token = zeroize::Zeroizing::new(token);
        if provider != "github"
            || token.is_empty()
            || token.len() > 16384
            || !token.bytes().all(|b| (33..=126).contains(&b))
        {
            return Err(GithubCredentialError { connection_id });
        }
        Ok(SecretString::new(token.as_str()))
    }
}

#[cfg(test)]
pub(crate) struct NoGithub;
#[cfg(test)]
#[async_trait::async_trait]
impl GithubCredentialResolver for NoGithub {
    async fn resolve(&self, connection_id: i32) -> Result<SecretString, GithubCredentialError> {
        Err(GithubCredentialError { connection_id })
    }
}

#[cfg(test)]
pub(crate) mod tests;
