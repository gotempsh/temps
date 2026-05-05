//! Per-operation git credential issuance for in-sandbox daemons.
//!
//! ## Why this exists
//! The credential daemon running inside every workspace sandbox container
//! mints a fresh, narrowly-scoped token for every single `git clone` /
//! `git push` operation. It has no long-lived credentials of its own — it
//! authenticates to this endpoint with the workspace's deployment token
//! (which carries `project_id`), tells us "I want to fetch repo X owned
//! by owner Y on host Z", and we hand back a single-repo single-permission
//! installation token that lives for under an hour.
//!
//! ## Trust boundary
//! Anything user code inside the sandbox could request, the daemon itself
//! could request — they share a uid in the sandbox's filesystem-only
//! sense (the daemon is on a separate uid, but the deployment token
//! used to call us *is* in the sandbox). What this endpoint guarantees:
//!
//! 1. The caller can only request tokens for the project that issued
//!    their deployment token. Cross-project requests are 403'd.
//! 2. Within their project, they can only request tokens for the *one*
//!    repository the project is configured against. Asking for "any other
//!    repo this GitHub App can access" is also 403'd.
//! 3. Tokens are minted with the minimum permission for the operation
//!    (`contents:read` for fetch, `contents:write` for push) — never the
//!    full installation scope.
//!
//! Combined, a fully-compromised sandbox can do exactly what `git clone`
//! and `git push` of its *own repo* could do, for under an hour, with no
//! ability to escalate to other repos or other projects.

use std::sync::Arc;

use sea_orm::{DatabaseConnection, EntityTrait};
use temps_entities::projects;
use temps_git::services::git_provider_manager_trait::{
    GitProviderManagerError as TraitError, GitProviderManagerTrait, ScopedTokenGrant, ScopedTokenOp,
};

use crate::error::WorkspaceError;

/// Operation a credential is requested for. Mirrors
/// [`temps_git::ScopedTokenOp`] but lives in the workspace crate so the
/// HTTP DTO doesn't pull in temps-git as a public dependency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitCredentialOperation {
    Fetch,
    Push,
}

impl From<GitCredentialOperation> for ScopedTokenOp {
    fn from(op: GitCredentialOperation) -> Self {
        match op {
            GitCredentialOperation::Fetch => ScopedTokenOp::Fetch,
            GitCredentialOperation::Push => ScopedTokenOp::Push,
        }
    }
}

/// Mints per-op scoped git credentials for workspace sandboxes.
///
/// Held as an `Arc` in the workspace plugin and used by exactly one
/// handler (`POST /workspace/git-credential`). The service stays small on
/// purpose — all the policy lives in one place where it can be audited.
pub struct GitCredentialService {
    db: Arc<DatabaseConnection>,
    git_provider_manager: Arc<dyn GitProviderManagerTrait>,
}

impl GitCredentialService {
    pub fn new(
        db: Arc<DatabaseConnection>,
        git_provider_manager: Arc<dyn GitProviderManagerTrait>,
    ) -> Self {
        Self {
            db,
            git_provider_manager,
        }
    }

    /// Mint a per-operation, single-repo, narrow-permission credential for
    /// the project identified by `project_id`. The caller MUST already have
    /// authenticated with a deployment token for that project — this method
    /// trusts `project_id` as ground truth.
    ///
    /// Authorization checks performed here:
    /// 1. Project exists.
    /// 2. Project has a `git_provider_connection_id`.
    /// 3. Requested `(owner, repo)` matches the project's `(repo_owner,
    ///    repo_name)`. `host` is validated against an allow-list
    ///    (`github.com`, `gitlab.com`, plus the corresponding API hosts)
    ///    rather than against the project record — the project entity
    ///    doesn't store provider host, only provider connection.
    pub async fn mint_for_project(
        &self,
        project_id: i32,
        host: &str,
        owner: &str,
        repo: &str,
        operation: GitCredentialOperation,
    ) -> Result<ScopedTokenGrant, WorkspaceError> {
        // 1. Host allow-list. We deliberately don't read this from
        // platform settings: the credential daemon should *only* be asked
        // for credentials for hosts we know how to mint scoped tokens
        // against. Anything else is either a bug in the daemon or an
        // attempt to smuggle creds for an unrelated host.
        if !is_allowed_host(host) {
            return Err(WorkspaceError::GitCredentialRepoMismatch {
                project_id,
                requested_host: host.to_string(),
                requested_owner: owner.to_string(),
                requested_repo: repo.to_string(),
                project_repo: "<host not on allow-list>".to_string(),
            });
        }

        // 2. Project lookup.
        let project = projects::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(WorkspaceError::ProjectNotFound { project_id })?;

        // 3. Repo match. Compare case-insensitively because GitHub itself
        // is case-insensitive on owner/repo (and `acme/Web` resolves the
        // same as `acme/web`); rejecting based on case alone would be a
        // false positive at best, a confusing UX at worst.
        if !owner.eq_ignore_ascii_case(&project.repo_owner)
            || !repo.eq_ignore_ascii_case(&project.repo_name)
        {
            return Err(WorkspaceError::GitCredentialRepoMismatch {
                project_id,
                requested_host: host.to_string(),
                requested_owner: owner.to_string(),
                requested_repo: repo.to_string(),
                project_repo: format!("{}/{}", project.repo_owner, project.repo_name),
            });
        }

        // 4. Connection presence.
        let connection_id = project
            .git_provider_connection_id
            .ok_or(WorkspaceError::GitCredentialNoConnection { project_id })?;

        // 5. Mint via provider manager. Use the project's canonical
        // owner/repo (not the request's) so the GitHub API gets the exact
        // case-correct name — case-insensitive match above is for the
        // authz check, but the mint call needs canonical input.
        let grant = self
            .git_provider_manager
            .mint_scoped_repo_token(
                connection_id,
                &project.repo_owner,
                &project.repo_name,
                operation.into(),
            )
            .await
            .map_err(|e| match e {
                TraitError::ConnectionNotFound(_) => {
                    WorkspaceError::GitCredentialNoConnection { project_id }
                }
                other => WorkspaceError::GitCredentialMintFailed {
                    project_id,
                    owner: project.repo_owner.clone(),
                    repo: project.repo_name.clone(),
                    reason: other.to_string(),
                },
            })?;

        Ok(grant)
    }
}

/// Hosts the daemon may legitimately request credentials for.
///
/// Both the user-facing host (`github.com`) and the API host
/// (`api.github.com`) are accepted because the git credential helper
/// protocol passes the same `host=` field for HTTPS clones to either.
/// GitLab's self-hosted instances are deliberately *not* allow-listed
/// here — projects pointing at a self-hosted GitLab need an explicit
/// platform-settings change first; the goal of this list is "fail
/// loudly rather than silently mint creds for an unexpected host".
fn is_allowed_host(host: &str) -> bool {
    matches!(
        host,
        "github.com" | "api.github.com" | "gitlab.com" | "api.gitlab.com"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use chrono::{Duration, Utc};
    use sea_orm::{DatabaseBackend, MockDatabase};
    use std::sync::Mutex;
    use temps_git::services::git_provider_manager_trait::{
        GitProviderManagerError, PullRequest, RepositoryInfo,
    };

    /// Records what was asked of it so tests can assert exact
    /// owner/repo/operation values reached the manager.
    struct RecordingManager {
        calls: Mutex<Vec<(i32, String, String, ScopedTokenOp)>>,
        result: Mutex<Option<Result<ScopedTokenGrant, GitProviderManagerError>>>,
    }

    impl RecordingManager {
        fn new(result: Result<ScopedTokenGrant, GitProviderManagerError>) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                result: Mutex::new(Some(result)),
            }
        }
    }

    #[async_trait]
    impl GitProviderManagerTrait for RecordingManager {
        async fn clone_repository(
            &self,
            _: i32,
            _: &str,
            _: &str,
            _: &std::path::Path,
            _: Option<&str>,
        ) -> Result<(), GitProviderManagerError> {
            unimplemented!()
        }
        async fn get_repository_info(
            &self,
            _: i32,
            _: &str,
            _: &str,
        ) -> Result<RepositoryInfo, GitProviderManagerError> {
            unimplemented!()
        }
        async fn download_archive(
            &self,
            _: i32,
            _: &str,
            _: &str,
            _: &str,
            _: &std::path::Path,
        ) -> Result<(), GitProviderManagerError> {
            unimplemented!()
        }
        async fn get_connection_access_token(
            &self,
            _: i32,
        ) -> Result<(String, String), GitProviderManagerError> {
            unimplemented!()
        }
        async fn push_files_and_create_pr(
            &self,
            _: i32,
            _: &str,
            _: &str,
            _: &str,
            _: &str,
            _: Vec<(String, Vec<u8>)>,
            _: &str,
            _: &str,
            _: &str,
        ) -> Result<PullRequest, GitProviderManagerError> {
            unimplemented!()
        }
        async fn mint_scoped_repo_token(
            &self,
            connection_id: i32,
            owner: &str,
            repo: &str,
            operation: ScopedTokenOp,
        ) -> Result<ScopedTokenGrant, GitProviderManagerError> {
            self.calls.lock().unwrap().push((
                connection_id,
                owner.to_string(),
                repo.to_string(),
                operation,
            ));
            self.result.lock().unwrap().take().unwrap_or_else(|| {
                Err(GitProviderManagerError::Other(
                    "test result already consumed".into(),
                ))
            })
        }
    }

    fn project_model(repo_owner: &str, repo_name: &str, conn_id: Option<i32>) -> projects::Model {
        let now = Utc::now();
        projects::Model {
            id: 42,
            name: "p".into(),
            repo_name: repo_name.into(),
            repo_owner: repo_owner.into(),
            directory: ".".into(),
            main_branch: "main".into(),
            preset: temps_entities::preset::Preset::NodeJs,
            preset_config: None,
            deployment_config: None,
            created_at: now,
            updated_at: now,
            slug: "p".into(),
            is_deleted: false,
            deleted_at: None,
            last_deployment: None,
            is_public_repo: false,
            git_url: None,
            git_provider_connection_id: conn_id,
            attack_mode: false,
            enable_preview_environments: false,
            source_type: temps_entities::source_type::SourceType::Git,
            gitlab_webhook_id: None,
            gitlab_webhook_signing_token: None,
        }
    }

    fn ok_grant() -> ScopedTokenGrant {
        ScopedTokenGrant {
            username: "x-access-token".into(),
            password: "ghs_xxx".into(),
            expires_at: Some(Utc::now() + Duration::minutes(55)),
        }
    }

    /// Happy path: project exists, connection set, repo matches → mints.
    /// Asserts the mint call propagates canonical owner/repo from the
    /// project record, NOT whatever case the caller used.
    #[tokio::test]
    async fn mints_token_when_repo_matches() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![project_model("Acme", "Web", Some(7))]])
            .into_connection();
        let manager = Arc::new(RecordingManager::new(Ok(ok_grant())));

        let svc = GitCredentialService::new(Arc::new(db), manager.clone());
        let grant = svc
            .mint_for_project(
                42,
                "github.com",
                "acme",
                "web",
                GitCredentialOperation::Fetch,
            )
            .await
            .expect("expected ok");

        assert_eq!(grant.username, "x-access-token");
        let calls = manager.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        // Canonical case from project, not the caller's lowercase.
        assert_eq!(calls[0].0, 7);
        assert_eq!(calls[0].1, "Acme");
        assert_eq!(calls[0].2, "Web");
        assert_eq!(calls[0].3, ScopedTokenOp::Fetch);
    }

    /// Push → manager receives Push (not Fetch).
    #[tokio::test]
    async fn push_operation_propagates_to_manager() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![project_model("acme", "web", Some(7))]])
            .into_connection();
        let manager = Arc::new(RecordingManager::new(Ok(ok_grant())));

        let svc = GitCredentialService::new(Arc::new(db), manager.clone());
        svc.mint_for_project(
            42,
            "github.com",
            "acme",
            "web",
            GitCredentialOperation::Push,
        )
        .await
        .unwrap();

        assert_eq!(manager.calls.lock().unwrap()[0].3, ScopedTokenOp::Push);
    }

    /// Cross-repo request inside the same project → 403, manager not called.
    /// This is the core security guarantee: a daemon (or compromised
    /// workspace) cannot mint creds for any repo other than its own.
    #[tokio::test]
    async fn rejects_cross_repo_request() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![project_model("acme", "web", Some(7))]])
            .into_connection();
        let manager = Arc::new(RecordingManager::new(Ok(ok_grant())));

        let svc = GitCredentialService::new(Arc::new(db), manager.clone());
        let err = svc
            .mint_for_project(
                42,
                "github.com",
                "acme",
                "secret-internal-repo",
                GitCredentialOperation::Fetch,
            )
            .await
            .expect_err("expected refusal");

        match err {
            WorkspaceError::GitCredentialRepoMismatch {
                project_id,
                requested_repo,
                project_repo,
                ..
            } => {
                assert_eq!(project_id, 42);
                assert_eq!(requested_repo, "secret-internal-repo");
                assert_eq!(project_repo, "acme/web");
            }
            other => panic!("expected RepoMismatch, got {other:?}"),
        }
        assert!(manager.calls.lock().unwrap().is_empty());
    }

    /// Cross-owner request → also 403. Even if the GitHub App has access
    /// to multiple orgs, the daemon's project is pinned to one of them.
    #[tokio::test]
    async fn rejects_cross_owner_request() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![project_model("acme", "web", Some(7))]])
            .into_connection();
        let manager = Arc::new(RecordingManager::new(Ok(ok_grant())));

        let svc = GitCredentialService::new(Arc::new(db), manager);
        let err = svc
            .mint_for_project(
                42,
                "github.com",
                "victim-org",
                "web",
                GitCredentialOperation::Fetch,
            )
            .await
            .expect_err("expected refusal");

        assert!(matches!(
            err,
            WorkspaceError::GitCredentialRepoMismatch { .. }
        ));
    }

    /// Disallowed host → 403, no DB lookup wasted.
    #[tokio::test]
    async fn rejects_unknown_host() {
        // No DB query expected, so don't seed the mock with anything.
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let manager = Arc::new(RecordingManager::new(Ok(ok_grant())));

        let svc = GitCredentialService::new(Arc::new(db), manager.clone());
        let err = svc
            .mint_for_project(
                42,
                "evil.example.com",
                "acme",
                "web",
                GitCredentialOperation::Fetch,
            )
            .await
            .expect_err("expected refusal");

        assert!(matches!(
            err,
            WorkspaceError::GitCredentialRepoMismatch { .. }
        ));
        assert!(manager.calls.lock().unwrap().is_empty());
    }

    /// Project exists but has no git connection → 409 Conflict via
    /// `GitCredentialNoConnection`.
    #[tokio::test]
    async fn rejects_when_project_has_no_connection() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![project_model("acme", "web", None)]])
            .into_connection();
        let manager = Arc::new(RecordingManager::new(Ok(ok_grant())));

        let svc = GitCredentialService::new(Arc::new(db), manager);
        let err = svc
            .mint_for_project(
                42,
                "github.com",
                "acme",
                "web",
                GitCredentialOperation::Fetch,
            )
            .await
            .expect_err("expected refusal");

        assert!(matches!(
            err,
            WorkspaceError::GitCredentialNoConnection { project_id: 42 }
        ));
    }

    /// Provider says "scoped tokens unsupported" (PAT/OAuth) → propagate as
    /// `GitCredentialMintFailed`, not as a misleading "no connection".
    #[tokio::test]
    async fn surfaces_provider_unsupported_as_mint_failed() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![project_model("acme", "web", Some(7))]])
            .into_connection();
        let manager = Arc::new(RecordingManager::new(Err(
            GitProviderManagerError::ScopedTokensUnsupported {
                connection_id: 7,
                reason: "PAT".into(),
            },
        )));

        let svc = GitCredentialService::new(Arc::new(db), manager);
        let err = svc
            .mint_for_project(
                42,
                "github.com",
                "acme",
                "web",
                GitCredentialOperation::Fetch,
            )
            .await
            .expect_err("expected refusal");

        match err {
            WorkspaceError::GitCredentialMintFailed { project_id, .. } => {
                assert_eq!(project_id, 42);
            }
            other => panic!("expected MintFailed, got {other:?}"),
        }
    }
}
