use super::git_provider::{
    AuthMethod, Branch, Commit, FileContent, GitProviderError, GitProviderService, GitProviderTag,
    GitProviderType, PullRequest, Repository, RepositoryPage, User, WebhookConfig,
};
use async_trait::async_trait;

/// Generic / Manual git provider.
///
/// Implements `GitProviderService` for any git server not natively supported
/// by Temps. This is the "Other Git Providers" tier in the sidebar, covering
/// Azure DevOps, AWS CodeCommit, SourceHut, Gogs, Gitblit, Rhodecode, Forgejo
/// (when full native integration is not desired), and any other accessible
/// git host.
///
/// # Connection modes
/// - **Mode A — Public repository:** No `AuthMethod` credentials
///   → cloned via `git_ops::clone_repo` (no credentials).
/// - **Mode B — Private HTTPS token:** `AuthMethod::PersonalAccessToken { token }`
///   (username defaults to `x-access-token`) or
///   `AuthMethod::BasicAuth { username, password }` (password = token)
///   → cloned via `git_ops::clone_repo_with_credentials`.
///
/// # Security (MUST-FIX 4)
/// The `clone_url` is re-validated with the HTTPS-only `validate_git_url`
/// inside `clone_repository` before every clone, so a later metadata edit
/// cannot bypass the create-time validation.
pub struct GenericProvider {
    /// Optional display-only base URL. Not used for API calls.
    #[allow(dead_code)]
    base_url: Option<String>,
    auth_method: AuthMethod,
}

impl GenericProvider {
    /// Create a new Generic/Manual git provider.
    ///
    /// # Arguments
    /// * `base_url` — Optional, display-only. Not validated or used for cloning.
    /// * `auth_method` — Determines the clone mode (A/B).
    pub fn new(base_url: Option<String>, auth_method: AuthMethod) -> Self {
        Self {
            base_url,
            auth_method,
        }
    }

    /// Clone username for HTTPS token auth.
    fn clone_username(&self) -> &str {
        match &self.auth_method {
            AuthMethod::PersonalAccessToken { .. } => "x-access-token",
            AuthMethod::BasicAuth { username, .. } => username,
            _ => "x-access-token",
        }
    }

    /// Credential string for HTTPS token auth, if any.
    fn credential_string(&self) -> Option<String> {
        match &self.auth_method {
            AuthMethod::PersonalAccessToken { token } => Some(token.clone()),
            AuthMethod::BasicAuth { password, .. } => Some(password.clone()),
            _ => None,
        }
    }

    /// Returns true when no credentials are present (Mode A — public repo).
    #[cfg(test)]
    fn is_public(&self) -> bool {
        self.credential_string().is_none()
    }
}

// ── GitProviderService impl ───────────────────────────────────────────────────

#[async_trait]
impl GitProviderService for GenericProvider {
    fn provider_type(&self) -> GitProviderType {
        GitProviderType::Generic
    }

    /// Generic providers do not support OAuth.
    async fn authenticate(&self, _code: Option<String>) -> Result<String, GitProviderError> {
        match &self.auth_method {
            AuthMethod::PersonalAccessToken { token } => Ok(token.clone()),
            AuthMethod::BasicAuth { password, .. } => Ok(password.clone()),
            _ => Err(GitProviderError::AuthenticationFailed(
                "Generic providers do not support OAuth. \
                 Configure a clone URL and an optional HTTPS token instead."
                    .to_string(),
            )),
        }
    }

    /// Generic providers do not have an OAuth flow.
    async fn get_auth_url(&self, _state: &str) -> Result<String, GitProviderError> {
        Err(GitProviderError::InvalidConfiguration(
            "Manual/Generic git providers do not support OAuth. \
             Configure the clone URL and an optional HTTPS token instead."
                .to_string(),
        ))
    }

    /// Token refresh is not applicable to Generic providers.
    async fn token_needs_refresh(&self, _access_token: &str) -> bool {
        false
    }

    /// For token modes, return `Ok(true)` — there is no generic user API to
    /// probe. For public repos (Mode A), return `Ok(true)` as well.
    ///
    /// Health is recorded as `"unknown"` by the health service (ADR Decision 10).
    async fn validate_token(&self, _access_token: &str) -> Result<bool, GitProviderError> {
        // There is no generic API endpoint to validate against.
        // Return Ok(true) to indicate "no known reason to think it's invalid."
        Ok(true)
    }

    /// Generic PAT / basic auth credentials do not support refresh.
    async fn validate_and_refresh_token(
        &self,
        access_token: &str,
        _refresh_token: Option<&str>,
    ) -> Result<(String, Option<String>), GitProviderError> {
        Ok((access_token.to_string(), None))
    }

    /// Not implemented — Generic providers have no REST API.
    async fn get_user(&self, _access_token: &str) -> Result<User, GitProviderError> {
        Err(GitProviderError::NotImplemented)
    }

    /// Not implemented — Generic providers have no REST API.
    async fn list_repositories(
        &self,
        _access_token: &str,
        _organization: Option<&str>,
    ) -> Result<Vec<Repository>, GitProviderError> {
        Err(GitProviderError::NotImplemented)
    }

    /// Not implemented — Generic providers have no REST API.
    async fn list_repositories_page(
        &self,
        _access_token: &str,
        _organization: Option<&str>,
        _page: u32,
    ) -> Result<RepositoryPage, GitProviderError> {
        Err(GitProviderError::NotImplemented)
    }

    /// Not implemented — Generic providers have no REST API.
    async fn get_repository(
        &self,
        _access_token: &str,
        _owner: &str,
        _repo: &str,
    ) -> Result<Repository, GitProviderError> {
        Err(GitProviderError::NotImplemented)
    }

    /// Not implemented — Generic providers have no REST API.
    async fn list_branches(
        &self,
        _access_token: &str,
        _owner: &str,
        _repo: &str,
    ) -> Result<Vec<Branch>, GitProviderError> {
        Err(GitProviderError::NotImplemented)
    }

    /// Not implemented — Generic providers have no REST API.
    async fn list_tags(
        &self,
        _access_token: &str,
        _owner: &str,
        _repo: &str,
    ) -> Result<Vec<GitProviderTag>, GitProviderError> {
        Err(GitProviderError::NotImplemented)
    }

    /// Not implemented — Generic providers have no REST API.
    async fn get_file_content(
        &self,
        _access_token: &str,
        _owner: &str,
        _repo: &str,
        _path: &str,
        _branch: Option<&str>,
    ) -> Result<FileContent, GitProviderError> {
        Err(GitProviderError::NotImplemented)
    }

    /// Not implemented — Generic providers have no REST API.
    async fn get_latest_commit(
        &self,
        _access_token: &str,
        _owner: &str,
        _repo: &str,
        _branch: &str,
    ) -> Result<Commit, GitProviderError> {
        Err(GitProviderError::NotImplemented)
    }

    /// Not implemented — Generic providers have no REST API.
    async fn get_commit(
        &self,
        _access_token: &str,
        _owner: &str,
        _repo: &str,
        _reference: &str,
    ) -> Result<Commit, GitProviderError> {
        Err(GitProviderError::NotImplemented)
    }

    /// Not implemented — Generic providers have no REST API.
    async fn check_commit_exists(
        &self,
        _access_token: &str,
        _owner: &str,
        _repo: &str,
        _commit_sha: &str,
    ) -> Result<bool, GitProviderError> {
        Err(GitProviderError::NotImplemented)
    }

    /// Not implemented — Generic providers have no REST API.
    async fn list_commits(
        &self,
        _access_token: &str,
        _owner: &str,
        _repo: &str,
        _branch: &str,
        _per_page: u32,
    ) -> Result<Vec<Commit>, GitProviderError> {
        Err(GitProviderError::NotImplemented)
    }

    /// Not implemented — Generic providers have no REST API for webhook registration.
    ///
    /// The webhook URL and token are surfaced in the UI for manual configuration.
    async fn create_webhook(
        &self,
        _access_token: &str,
        _owner: &str,
        _repo: &str,
        _config: WebhookConfig,
    ) -> Result<String, GitProviderError> {
        Err(GitProviderError::NotImplemented)
    }

    /// Not implemented — see `create_webhook`.
    async fn delete_webhook(
        &self,
        _access_token: &str,
        _owner: &str,
        _repo: &str,
        _webhook_id: &str,
    ) -> Result<(), GitProviderError> {
        Err(GitProviderError::NotImplemented)
    }

    /// Not implemented — Generic providers use secret-in-path authentication
    /// instead of HMAC body signing. Validation is performed in the webhook
    /// handler (`handlers/generic.rs`), not here.
    async fn verify_webhook_signature(
        &self,
        _payload: &[u8],
        _signature: &str,
        _secret: &str,
    ) -> Result<bool, GitProviderError> {
        Err(GitProviderError::NotImplemented)
    }

    /// Accessibility check without REST API: not available for Generic.
    async fn check_repository_accessible(
        &self,
        _owner: &str,
        _repo: &str,
    ) -> Result<bool, GitProviderError> {
        Err(GitProviderError::NotImplemented)
    }

    /// Clone the repository using the stored clone URL.
    ///
    /// Dispatches by auth mode:
    /// - **Mode A (public):** `git_ops::clone_repo` — no credentials.
    /// - **Mode B (HTTPS token):** `git_ops::clone_repo_with_credentials`.
    ///
    /// # Security (MUST-FIX 4)
    /// The `clone_url` is re-validated here with
    /// `temps_core::url_validation::validate_git_url` (HTTPS-only) before
    /// every clone, regardless of create-time validation.
    async fn clone_repository(
        &self,
        clone_url: &str,
        target_dir: &str,
        _access_token: Option<&str>,
    ) -> Result<(), GitProviderError> {
        const CLONE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

        // Modes A and B: HTTPS clones.
        // MUST-FIX 4: re-validate clone_url with HTTPS-only validate_git_url
        // at clone time (not just at create time).
        temps_core::url_validation::validate_git_url(clone_url).map_err(|e| {
            GitProviderError::InvalidConfiguration(format!(
                "Generic provider clone URL failed HTTPS validation: {}",
                e
            ))
        })?;

        let clone_url = clone_url.to_string();
        let target_dir = std::path::PathBuf::from(target_dir);
        let username = self.clone_username().to_string();
        let credential = self.credential_string();

        let join = tokio::task::spawn_blocking(move || {
            match credential {
                Some(token) => {
                    // Mode B: HTTPS token auth
                    super::git_ops::clone_repo_with_credentials(
                        &clone_url,
                        &target_dir,
                        &username,
                        &token,
                        None,
                    )
                }
                None => {
                    // Mode A: public repo — no credentials
                    super::git_ops::clone_repo(&clone_url, &target_dir, None)
                }
            }
        });

        match tokio::time::timeout(CLONE_TIMEOUT, join).await {
            Ok(joined) => {
                joined
                    .map_err(|e| {
                        GitProviderError::Other(format!("Git clone task panicked: {}", e))
                    })?
                    .map_err(|e| GitProviderError::Other(format!("Git clone failed: {}", e)))?;
                Ok(())
            }
            Err(_) => Err(GitProviderError::Other(format!(
                "Generic provider git clone timed out after {}s",
                CLONE_TIMEOUT.as_secs()
            ))),
        }
    }

    /// Not implemented — the deployer falls back to clone-then-detect for
    /// Generic providers.
    async fn download_archive(
        &self,
        _access_token: &str,
        _owner: &str,
        _repo: &str,
        _ref_spec: &str,
        _target_path: &std::path::Path,
        _progress: Option<&crate::services::git_provider::ArchiveProgressSender>,
    ) -> Result<(), GitProviderError> {
        Err(GitProviderError::NotImplemented)
    }

    /// Not implemented — framework detection requires cloning for Generic providers.
    /// `GenericProvider` has no REST API to read file content from.
    async fn create_source(
        &self,
        _access_token: &str,
        _owner: &str,
        _repo: &str,
        _reference: &str,
    ) -> Result<Box<dyn temps_presets::source::ProjectSource>, GitProviderError> {
        Err(GitProviderError::NotImplemented)
    }

    /// Not implemented — Generic providers have no scoped token API.
    async fn mint_scoped_repo_token(
        &self,
        _access_token: Option<&str>,
        _owner: &str,
        _repo: &str,
        _operation: super::git_provider::ScopedTokenOp,
    ) -> Result<super::git_provider::ScopedTokenGrant, GitProviderError> {
        Err(GitProviderError::NotImplemented)
    }

    /// Not implemented — Generic providers have no repository creation API.
    async fn create_repository(
        &self,
        _access_token: &str,
        _name: &str,
        _owner: Option<&str>,
        _description: Option<&str>,
        _private: bool,
    ) -> Result<Repository, GitProviderError> {
        Err(GitProviderError::NotImplemented)
    }

    /// Not implemented — Generic providers have no file push API.
    async fn push_files_to_repository(
        &self,
        _access_token: &str,
        _owner: &str,
        _repo: &str,
        _branch: &str,
        _files: Vec<(String, Vec<u8>)>,
        _commit_message: &str,
    ) -> Result<Commit, GitProviderError> {
        Err(GitProviderError::NotImplemented)
    }

    /// Not implemented — Generic providers have no pull request API.
    async fn create_pull_request(
        &self,
        _access_token: &str,
        _owner: &str,
        _repo: &str,
        _title: &str,
        _body: &str,
        _head_branch: &str,
        _base_branch: &str,
    ) -> Result<PullRequest, GitProviderError> {
        Err(GitProviderError::NotImplemented)
    }
}

// ── Token generation helper ───────────────────────────────────────────────────

/// Generate a cryptographically random 64-character hex token (32 bytes) for
/// use as a Generic webhook delivery URL path token.
///
/// Uses `rand::rngs::OsRng` (MUST-FIX 1 — not `thread_rng`).
///
/// The token is embedded in the webhook callback URL:
/// `{temps_url}/api/webhook/git/generic/events/{token}`.
/// It is stored encrypted in `projects.generic_webhook_token` and compared
/// in constant time in the webhook handler (MUST-FIX 3).
pub fn generate_generic_webhook_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_public_provider() -> GenericProvider {
        GenericProvider::new(
            None,
            AuthMethod::OAuth {
                client_id: String::new(),
                client_secret: String::new(),
                redirect_uri: String::new(),
            },
        )
    }

    fn make_pat_provider() -> GenericProvider {
        GenericProvider::new(
            None,
            AuthMethod::PersonalAccessToken {
                token: "my-token".to_string(),
            },
        )
    }

    fn make_basic_provider() -> GenericProvider {
        GenericProvider::new(
            Some("https://git.internal.example.com".to_string()),
            AuthMethod::BasicAuth {
                username: "myuser".to_string(),
                password: "my-pat".to_string(),
            },
        )
    }

    // ── provider_type ─────────────────────────────────────────────────────────

    #[test]
    fn test_provider_type_is_generic() {
        assert!(matches!(
            make_pat_provider().provider_type(),
            GitProviderType::Generic
        ));
    }

    // ── is_public ─────────────────────────────────────────────────────────────

    #[test]
    fn test_is_public_when_no_credentials() {
        // OAuth / other non-token methods have no credential_string → public mode
        let p = make_public_provider();
        assert!(
            p.is_public(),
            "provider with no credential should be public"
        );
    }

    #[test]
    fn test_is_not_public_when_pat() {
        assert!(!make_pat_provider().is_public());
    }

    #[test]
    fn test_is_not_public_when_basic() {
        assert!(!make_basic_provider().is_public());
    }

    // ── clone_username ────────────────────────────────────────────────────────

    #[test]
    fn test_clone_username_pat_is_x_access_token() {
        assert_eq!(make_pat_provider().clone_username(), "x-access-token");
    }

    #[test]
    fn test_clone_username_basic_is_user_supplied() {
        assert_eq!(make_basic_provider().clone_username(), "myuser");
    }

    // ── credential_string ─────────────────────────────────────────────────────

    #[test]
    fn test_credential_string_pat() {
        assert_eq!(
            make_pat_provider().credential_string(),
            Some("my-token".to_string())
        );
    }

    #[test]
    fn test_credential_string_basic() {
        assert_eq!(
            make_basic_provider().credential_string(),
            Some("my-pat".to_string())
        );
    }

    #[test]
    fn test_credential_string_no_cred() {
        assert_eq!(make_public_provider().credential_string(), None);
    }

    // ── validate_token ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_validate_token_always_ok_true() {
        let p = make_pat_provider();
        assert!(matches!(p.validate_token("any").await, Ok(true)));
    }

    // ── token_needs_refresh ───────────────────────────────────────────────────

    #[tokio::test]
    async fn test_token_needs_refresh_always_false() {
        let p = make_pat_provider();
        assert!(!p.token_needs_refresh("any").await);
    }

    // ── NotImplemented methods ────────────────────────────────────────────────

    #[tokio::test]
    async fn test_get_user_not_implemented() {
        let p = make_pat_provider();
        assert!(matches!(
            p.get_user("tok").await,
            Err(GitProviderError::NotImplemented)
        ));
    }

    #[tokio::test]
    async fn test_list_repositories_not_implemented() {
        let p = make_pat_provider();
        assert!(matches!(
            p.list_repositories("tok", None).await,
            Err(GitProviderError::NotImplemented)
        ));
    }

    #[tokio::test]
    async fn test_create_webhook_not_implemented() {
        let p = make_pat_provider();
        assert!(matches!(
            p.create_webhook(
                "tok",
                "owner",
                "repo",
                WebhookConfig {
                    url: String::new(),
                    secret: None,
                    events: vec![],
                }
            )
            .await,
            Err(GitProviderError::NotImplemented)
        ));
    }

    #[tokio::test]
    async fn test_create_source_not_implemented() {
        let p = make_pat_provider();
        assert!(matches!(
            p.create_source("tok", "owner", "repo", "main").await,
            Err(GitProviderError::NotImplemented)
        ));
    }

    #[tokio::test]
    async fn test_mint_scoped_repo_token_not_implemented() {
        use crate::services::git_provider::ScopedTokenOp;
        let p = make_pat_provider();
        assert!(matches!(
            p.mint_scoped_repo_token(None, "owner", "repo", ScopedTokenOp::Fetch)
                .await,
            Err(GitProviderError::NotImplemented)
        ));
    }

    // ── clone URL validation (MUST-FIX 4) ────────────────────────────────────

    #[tokio::test]
    async fn test_clone_rejects_http_url() {
        let p = make_pat_provider();
        let result = p
            .clone_repository("http://git.example.com/user/repo.git", "/tmp/test", None)
            .await;
        assert!(
            result.is_err(),
            "HTTP clone URL must be rejected (MUST-FIX 4)"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("HTTPS") || msg.contains("http"),
            "error message should mention HTTPS: {msg}"
        );
    }

    #[tokio::test]
    async fn test_clone_rejects_localhost_url() {
        let p = make_pat_provider();
        let result = p
            .clone_repository("https://localhost/user/repo.git", "/tmp/test", None)
            .await;
        assert!(result.is_err(), "Localhost URL must be rejected");
    }

    // ── generate_generic_webhook_token ────────────────────────────────────────

    #[test]
    fn test_generate_generic_webhook_token_is_64_hex_chars() {
        let tok = generate_generic_webhook_token();
        assert_eq!(tok.len(), 64, "32 bytes => 64 hex chars");
        assert!(tok.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_generate_generic_webhook_token_is_unique() {
        let t1 = generate_generic_webhook_token();
        let t2 = generate_generic_webhook_token();
        assert_ne!(t1, t2, "tokens must be unique per generation");
    }

    // ── authenticate ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_authenticate_returns_pat_token() {
        let p = make_pat_provider();
        let result = p.authenticate(None).await;
        assert_eq!(result.unwrap(), "my-token");
    }

    #[tokio::test]
    async fn test_authenticate_returns_basic_password() {
        let p = make_basic_provider();
        let result = p.authenticate(None).await;
        assert_eq!(result.unwrap(), "my-pat");
    }

    #[tokio::test]
    async fn test_authenticate_non_token_returns_error() {
        let p = make_public_provider();
        let result = p.authenticate(None).await;
        assert!(
            matches!(result, Err(GitProviderError::AuthenticationFailed(_))),
            "non-token auth should return AuthenticationFailed"
        );
    }
}
