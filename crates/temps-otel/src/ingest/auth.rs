//! Per-project authentication for OTel ingest.
//!
//! Supports two token types:
//! - **API keys (`tk_`)**: User-scoped. Requires `X-Temps-Project-Id` header
//!   to identify the target project.
//! - **Deployment tokens (`dt_`)**: Project-scoped. Project, environment, and
//!   deployment IDs are inferred from the token record itself.

use moka::future::Cache;
use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::warn;

use crate::error::OtelError;

/// Authenticated project context after token validation.
#[derive(Debug, Clone)]
pub struct ProjectAuth {
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub deployment_id: Option<i32>,
    /// Identifies the token used. For `tk_` keys this is the api_key id;
    /// for `dt_` tokens it is the deployment_token id.
    pub token_id: i32,
    pub project_name: String,
}

/// Authenticated service context for infrastructure metrics ingest.
#[derive(Debug, Clone)]
pub struct ServiceAuth {
    pub service_id: i32,
    pub service_name: String,
    pub token_id: i32,
}

/// Unified ingest authentication result — either a project or a service context.
#[derive(Debug, Clone)]
pub enum IngestAuth {
    Project(ProjectAuth),
    Service(ServiceAuth),
}

const AUTH_CACHE_TTL: Duration = Duration::from_secs(5);
const AUTH_CACHE_CAPACITY: u64 = 10_000;
const LAST_USED_UPDATE_INTERVAL: Duration = Duration::from_secs(60);
const LAST_USED_TRACKER_CAPACITY: usize = 10_000;

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
enum AuthCacheKey {
    ApiKey { key_hash: String, project_id: i32 },
    DeploymentToken { token_hash: String },
    ServiceToken { key_hash: String },
}

#[derive(Debug, Clone)]
enum CachedAuth {
    Project(ProjectAuth),
    Service(ServiceAuth),
}

#[derive(Debug, Clone)]
enum CachedAuthError {
    InvalidApiKey,
    AuthFailed(String),
    Storage(String),
}

impl From<OtelError> for CachedAuthError {
    fn from(error: OtelError) -> Self {
        match error {
            OtelError::InvalidApiKey => Self::InvalidApiKey,
            OtelError::AuthFailed { reason } => Self::AuthFailed(reason),
            OtelError::Storage { message } => Self::Storage(message),
            other => Self::Storage(other.to_string()),
        }
    }
}

impl From<&CachedAuthError> for OtelError {
    fn from(error: &CachedAuthError) -> Self {
        match error {
            CachedAuthError::InvalidApiKey => Self::InvalidApiKey,
            CachedAuthError::AuthFailed(reason) => Self::AuthFailed {
                reason: reason.clone(),
            },
            CachedAuthError::Storage(message) => Self::Storage {
                message: message.clone(),
            },
        }
    }
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
enum TokenTable {
    ApiKeys,
    DeploymentTokens,
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
struct TokenUsageKey {
    table: TokenTable,
    token_id: i32,
}

#[derive(Default)]
struct LastUsedTracker {
    updates: Mutex<HashMap<TokenUsageKey, Instant>>,
}

impl LastUsedTracker {
    fn mark_if_due(&self, key: TokenUsageKey, now: Instant) -> bool {
        let mut updates = match self.updates.lock() {
            Ok(updates) => updates,
            Err(poisoned) => poisoned.into_inner(),
        };

        if updates.len() >= LAST_USED_TRACKER_CAPACITY && !updates.contains_key(&key) {
            updates.retain(|_, updated_at| {
                now.duration_since(*updated_at) < LAST_USED_UPDATE_INTERVAL
            });
            if updates.len() >= LAST_USED_TRACKER_CAPACITY {
                return false;
            }
        }

        if updates
            .get(&key)
            .is_some_and(|updated_at| now.duration_since(*updated_at) < LAST_USED_UPDATE_INTERVAL)
        {
            return false;
        }

        updates.insert(key, now);
        true
    }

    fn forget(&self, key: TokenUsageKey) {
        let mut updates = match self.updates.lock() {
            Ok(updates) => updates,
            Err(poisoned) => poisoned.into_inner(),
        };
        updates.remove(&key);
    }
}

/// Service for authenticating OTel ingest requests.
pub struct OtelAuthService {
    db: Arc<DatabaseConnection>,
    auth_cache: Cache<AuthCacheKey, CachedAuth>,
    last_used_tracker: LastUsedTracker,
    /// ADR-028 team-based project access checker, injected after plugin
    /// registration (see `set_project_access_checker`). `tk_` keys carry a
    /// user identity, so ingest must honour the same project-access grants
    /// as the query-side `project_access_guard!` — otherwise any valid key
    /// could ingest telemetry into any project by ID. Absent (plain OSS
    /// binary, no checker plugin) → allow, matching the guard's no-op
    /// semantics.
    project_access_checker: std::sync::OnceLock<Arc<dyn temps_core::ProjectAccessChecker>>,
}

impl OtelAuthService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self {
            db,
            auth_cache: Cache::builder()
                .max_capacity(AUTH_CACHE_CAPACITY)
                .time_to_live(AUTH_CACHE_TTL)
                .build(),
            last_used_tracker: LastUsedTracker::default(),
            project_access_checker: std::sync::OnceLock::new(),
        }
    }

    /// Inject the ADR-028 project access checker once plugins have registered.
    /// Later calls are no-ops (first registration wins).
    pub fn set_project_access_checker(&self, checker: Arc<dyn temps_core::ProjectAccessChecker>) {
        let _ = self.project_access_checker.set(checker);
    }

    /// Authenticate any ingest token — project (`tk_`/`dt_`) or service (`si_`).
    ///
    /// Routes `si_` tokens to `authenticate_service_token`; all others fall
    /// through to the existing `authenticate` method.
    pub async fn authenticate_any(
        &self,
        token: &str,
        header_project_id: Option<i32>,
    ) -> Result<IngestAuth, OtelError> {
        if token.starts_with("si_") {
            self.authenticate_service_token(token)
                .await
                .map(IngestAuth::Service)
        } else {
            self.authenticate(token, header_project_id)
                .await
                .map(IngestAuth::Project)
        }
    }

    /// Authenticate a service ingest token (`si_`).
    ///
    /// The token must exist in `api_keys` with `role_type = 'metrics_ingest'`
    /// and have a non-NULL `service_id` linking it to `external_services`.
    async fn authenticate_service_token(&self, token: &str) -> Result<ServiceAuth, OtelError> {
        if token.len() < 10 {
            return Err(OtelError::InvalidApiKey);
        }

        let key_hash = hash_token(token);
        let cache_key = AuthCacheKey::ServiceToken {
            key_hash: key_hash.clone(),
        };
        let cached = self
            .auth_cache
            .try_get_with(cache_key, async {
                self.lookup_service_token(key_hash)
                    .await
                    .map(CachedAuth::Service)
                    .map_err(CachedAuthError::from)
            })
            .await
            .map_err(|error| OtelError::from(error.as_ref()))?;

        // Unreachable in practice: `AuthCacheKey::ServiceToken` is a distinct
        // enum discriminant from `ApiKey`/`DeploymentToken`, and this method
        // only ever inserts `CachedAuth::Service` under that key, so a
        // `Project` value can never be read back here. Kept as a defensive
        // fallback (mapped to a generic 500, no cache internals leaked)
        // rather than `unreachable!()`, in case that invariant ever changes.
        let CachedAuth::Service(auth) = cached else {
            return Err(OtelError::Internal {
                message: "Project auth found in service-token cache entry".into(),
            });
        };
        self.touch_last_used(TokenUsageKey {
            table: TokenTable::ApiKeys,
            token_id: auth.token_id,
        })
        .await;
        Ok(auth)
    }

    async fn lookup_service_token(&self, key_hash: String) -> Result<ServiceAuth, OtelError> {
        let sql = r#"
            SELECT ak.id AS key_id, ak.service_id, es.name AS service_name
            FROM api_keys ak
            JOIN external_services es ON es.id = ak.service_id
            WHERE ak.key_hash = $1
              AND ak.is_active = true
              AND ak.role_type = 'metrics_ingest'
              AND (ak.expires_at IS NULL OR ak.expires_at > NOW())
            LIMIT 1
        "#;

        let result = self
            .db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                sql,
                vec![key_hash.into()],
            ))
            .await
            .map_err(|e| OtelError::Storage {
                message: format!("Database error during service token auth: {}", e),
            })?;

        match result {
            Some(row) => {
                let key_id: i32 = row
                    .try_get("", "key_id")
                    .map_err(|_| OtelError::AuthFailed {
                        reason: "Failed to parse key_id".into(),
                    })?;
                let service_id: i32 =
                    row.try_get("", "service_id")
                        .map_err(|_| OtelError::AuthFailed {
                            reason: "Failed to parse service_id".into(),
                        })?;
                let service_name: String =
                    row.try_get("", "service_name")
                        .map_err(|_| OtelError::AuthFailed {
                            reason: "Failed to parse service_name".into(),
                        })?;

                Ok(ServiceAuth {
                    service_id,
                    service_name,
                    token_id: key_id,
                })
            }
            None => Err(OtelError::AuthFailed {
                reason: "Invalid or expired service ingest token".into(),
            }),
        }
    }

    /// Authenticate an OTel ingest request.
    ///
    /// For `tk_` (API key) tokens the caller must supply `project_id` via
    /// the `X-Temps-Project-Id` header so we can verify access.
    ///
    /// For `dt_` (deployment token) tokens the project/environment/deployment
    /// are resolved from the token record; `header_project_id` is ignored.
    pub async fn authenticate(
        &self,
        token: &str,
        header_project_id: Option<i32>,
    ) -> Result<ProjectAuth, OtelError> {
        if token.starts_with("dt_") {
            self.authenticate_deployment_token(token).await
        } else if token.starts_with("tk_") {
            self.authenticate_api_key(token, header_project_id).await
        } else {
            Err(OtelError::InvalidApiKey)
        }
    }

    /// Authenticate using an API key (`tk_`).
    ///
    /// The key identifies a user. We require `X-Temps-Project-Id` to know
    /// which project to associate telemetry with, and verify the user has
    /// access to that project.
    async fn authenticate_api_key(
        &self,
        api_key: &str,
        header_project_id: Option<i32>,
    ) -> Result<ProjectAuth, OtelError> {
        if api_key.len() < 10 {
            return Err(OtelError::InvalidApiKey);
        }

        let project_id = header_project_id.ok_or_else(|| OtelError::AuthFailed {
            reason:
                "X-Temps-Project-Id header is required when using an API key (tk_) for OTel ingest"
                    .into(),
        })?;

        let key_hash = hash_token(api_key);
        let cache_key = AuthCacheKey::ApiKey {
            key_hash: key_hash.clone(),
            project_id,
        };
        let cached = self
            .auth_cache
            .try_get_with(cache_key, async {
                self.lookup_api_key(key_hash, project_id)
                    .await
                    .map(CachedAuth::Project)
                    .map_err(CachedAuthError::from)
            })
            .await
            .map_err(|error| OtelError::from(error.as_ref()))?;

        // Unreachable in practice: see the matching comment in
        // `authenticate_service_token` — `AuthCacheKey::ApiKey` never shares
        // a cache slot with a `Service`-typed value.
        let CachedAuth::Project(auth) = cached else {
            return Err(OtelError::Internal {
                message: "Service auth found in API-key cache entry".into(),
            });
        };
        self.touch_last_used(TokenUsageKey {
            table: TokenTable::ApiKeys,
            token_id: auth.token_id,
        })
        .await;
        Ok(auth)
    }

    async fn lookup_api_key(
        &self,
        key_hash: String,
        project_id: i32,
    ) -> Result<ProjectAuth, OtelError> {
        // Look up the key and confirm the target project exists. The JOIN on a
        // bound constant only proves project existence — the user→project
        // access check happens below via the ADR-028 ProjectAccessChecker,
        // mirroring `project_access_guard!` (admin bypass, fail-closed on
        // infrastructure errors, allow when no checker is registered).
        let sql = r#"
            SELECT ak.id AS key_id, ak.user_id, ak.role_type, p.name AS project_name
            FROM api_keys ak
            JOIN projects p ON p.id = $2
            WHERE ak.key_hash = $1
              AND ak.is_active = true
              AND (ak.expires_at IS NULL OR ak.expires_at > NOW())
            LIMIT 1
        "#;

        let result = self
            .db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                sql,
                vec![key_hash.into(), project_id.into()],
            ))
            .await
            .map_err(|e| OtelError::Storage {
                message: format!("Database error during OTel auth: {}", e),
            })?;

        match result {
            Some(row) => {
                let key_id: i32 = row
                    .try_get("", "key_id")
                    .map_err(|_| OtelError::AuthFailed {
                        reason: "Failed to parse auth result".into(),
                    })?;
                let user_id: i32 =
                    row.try_get("", "user_id")
                        .map_err(|_| OtelError::AuthFailed {
                            reason: "Failed to parse auth result".into(),
                        })?;
                let role_type: String =
                    row.try_get("", "role_type")
                        .map_err(|_| OtelError::AuthFailed {
                            reason: "Failed to parse auth result".into(),
                        })?;
                let project_name: String =
                    row.try_get("", "project_name")
                        .map_err(|_| OtelError::AuthFailed {
                            reason: "Failed to parse project name".into(),
                        })?;

                self.check_project_access(user_id, &role_type, project_id)
                    .await?;

                Ok(ProjectAuth {
                    project_id,
                    environment_id: None,
                    deployment_id: None,
                    token_id: key_id,
                    project_name,
                })
            }
            None => Err(OtelError::AuthFailed {
                reason: "Invalid or expired API key, or project not found".into(),
            }),
        }
    }

    /// Enforce ADR-028 team-based project access for a `tk_` key's user,
    /// mirroring `project_access_guard!` semantics exactly:
    ///
    /// - Instance `admin` / `platform_admin` roles bypass the check.
    /// - No checker registered (plain OSS binary) → allow.
    /// - `Ok(false)` → deny.
    /// - `Err` → deny (**fail-closed**): a broken check must never silently
    ///   grant cross-project ingest.
    ///
    /// (`dt_` tokens never reach this path — they are confined to their bound
    /// project by construction and carry no user identity.)
    async fn check_project_access(
        &self,
        user_id: i32,
        role_type: &str,
        project_id: i32,
    ) -> Result<(), OtelError> {
        use temps_auth::permissions::Role;

        let is_instance_admin = matches!(
            Role::from_str(role_type),
            Some(Role::Admin) | Some(Role::PlatformAdmin)
        );
        if is_instance_admin {
            return Ok(());
        }

        let Some(checker) = self.project_access_checker.get() else {
            return Ok(());
        };

        match checker.user_can_access_project(user_id, project_id).await {
            Ok(true) => Ok(()),
            Ok(false) => Err(OtelError::AuthFailed {
                reason: format!("API key user does not have access to project {project_id}"),
            }),
            Err(e) => {
                warn!(
                    user_id,
                    project_id,
                    error = %e,
                    "ProjectAccessChecker infrastructure failure during OTel \
                     ingest auth — denying (fail-closed)"
                );
                Err(OtelError::AuthFailed {
                    reason: "Project access check failed".into(),
                })
            }
        }
    }

    /// Authenticate using a deployment token (`dt_`).
    ///
    /// The token record contains `project_id`, `environment_id`, and
    /// `deployment_id`, so no extra headers are needed.
    async fn authenticate_deployment_token(&self, token: &str) -> Result<ProjectAuth, OtelError> {
        if token.len() < 10 {
            return Err(OtelError::InvalidApiKey);
        }

        let token_hash = hash_token(token);
        let cache_key = AuthCacheKey::DeploymentToken {
            token_hash: token_hash.clone(),
        };
        let cached = self
            .auth_cache
            .try_get_with(cache_key, async {
                self.lookup_deployment_token(token_hash)
                    .await
                    .map(CachedAuth::Project)
                    .map_err(CachedAuthError::from)
            })
            .await
            .map_err(|error| OtelError::from(error.as_ref()))?;

        // Unreachable in practice: see the matching comment in
        // `authenticate_service_token` — `AuthCacheKey::DeploymentToken`
        // never shares a cache slot with a `Service`-typed value.
        let CachedAuth::Project(auth) = cached else {
            return Err(OtelError::Internal {
                message: "Service auth found in deployment-token cache entry".into(),
            });
        };
        self.touch_last_used(TokenUsageKey {
            table: TokenTable::DeploymentTokens,
            token_id: auth.token_id,
        })
        .await;
        Ok(auth)
    }

    async fn lookup_deployment_token(&self, token_hash: String) -> Result<ProjectAuth, OtelError> {
        let sql = r#"
            SELECT dt.id AS token_id,
                   dt.project_id,
                   dt.environment_id,
                   dt.deployment_id,
                   p.name AS project_name
            FROM deployment_tokens dt
            JOIN projects p ON p.id = dt.project_id
            WHERE dt.token_hash = $1
              AND dt.is_active = true
              AND (dt.expires_at IS NULL OR dt.expires_at > NOW())
            LIMIT 1
        "#;

        let result = self
            .db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                sql,
                vec![token_hash.into()],
            ))
            .await
            .map_err(|e| OtelError::Storage {
                message: format!("Database error during OTel auth: {}", e),
            })?;

        match result {
            Some(row) => {
                let token_id: i32 =
                    row.try_get("", "token_id")
                        .map_err(|_| OtelError::AuthFailed {
                            reason: "Failed to parse deployment token ID".into(),
                        })?;
                let project_id: i32 =
                    row.try_get("", "project_id")
                        .map_err(|_| OtelError::AuthFailed {
                            reason: "Failed to parse project ID".into(),
                        })?;
                let environment_id: Option<i32> =
                    row.try_get("", "environment_id")
                        .map_err(|_| OtelError::AuthFailed {
                            reason: "Failed to parse environment ID".into(),
                        })?;
                let deployment_id: Option<i32> =
                    row.try_get("", "deployment_id")
                        .map_err(|_| OtelError::AuthFailed {
                            reason: "Failed to parse deployment ID".into(),
                        })?;
                let project_name: String =
                    row.try_get("", "project_name")
                        .map_err(|_| OtelError::AuthFailed {
                            reason: "Failed to parse project name".into(),
                        })?;

                Ok(ProjectAuth {
                    project_id,
                    environment_id,
                    deployment_id,
                    token_id,
                    project_name,
                })
            }
            None => Err(OtelError::AuthFailed {
                reason: "Invalid or expired deployment token".into(),
            }),
        }
    }

    async fn touch_last_used(&self, key: TokenUsageKey) {
        if !self.last_used_tracker.mark_if_due(key, Instant::now()) {
            return;
        }

        let sql = match key.table {
            TokenTable::ApiKeys => "UPDATE api_keys SET last_used_at = NOW() WHERE id = $1",
            TokenTable::DeploymentTokens => {
                "UPDATE deployment_tokens SET last_used_at = NOW() WHERE id = $1"
            }
        };
        if let Err(error) = self
            .db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                sql,
                vec![key.token_id.into()],
            ))
            .await
        {
            self.last_used_tracker.forget(key);
            warn!(
                token_id = key.token_id,
                table = ?key.table,
                error = %error,
                "Failed to update OTel token last_used_at"
            );
        }
    }
}

fn hash_token(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_invalid_api_key_format() {
        // Key too short
        assert!(matches!(
            validate_key_format("tk_ab"),
            Err(OtelError::InvalidApiKey)
        ));

        // Wrong prefix — neither tk_ nor dt_
        assert!(matches!(
            validate_key_format("xx_abcdefghij"),
            Err(OtelError::InvalidApiKey)
        ));

        // Valid tk_ format
        assert!(validate_key_format("tk_abcdefghij").is_ok());

        // Valid dt_ format
        assert!(validate_key_format("dt_abcdefghij").is_ok());

        // Valid si_ format
        assert!(validate_key_format("si_abcdefghij").is_ok());
    }

    fn validate_key_format(key: &str) -> Result<(), OtelError> {
        if key.len() < 10 {
            return Err(OtelError::InvalidApiKey);
        }
        if !key.starts_with("tk_") && !key.starts_with("dt_") && !key.starts_with("si_") {
            return Err(OtelError::InvalidApiKey);
        }
        Ok(())
    }

    // ── Token format validation ──────────────────────────────────────────────

    #[test]
    fn test_si_token_prefix_recognized() {
        // si_ prefix should route to service auth path in authenticate_any
        let token = "si_abcdefghij1234567890";
        assert!(token.starts_with("si_"));
        assert!(token.len() >= 10);
    }

    #[test]
    fn test_si_token_too_short_rejected() {
        // Token under 10 chars should fail length check
        let token = "si_abc";
        assert!(token.len() < 10);
    }

    #[test]
    fn test_tk_prefix_still_routes_to_project_auth() {
        let token = "tk_abcdefghij";
        assert!(!token.starts_with("si_"));
        assert!(token.starts_with("tk_"));
    }

    #[test]
    fn test_dt_prefix_still_routes_to_deployment_auth() {
        let token = "dt_abcdefghij";
        assert!(!token.starts_with("si_"));
        assert!(token.starts_with("dt_"));
    }

    #[test]
    fn test_unknown_prefix_rejected() {
        let token = "xx_abcdefghij";
        assert!(!token.starts_with("si_"));
        assert!(!token.starts_with("tk_"));
        assert!(!token.starts_with("dt_"));
    }

    // ── check_project_access (ADR-028 enforcement on tk_ ingest) ─────────────

    /// Stub checker: `Some(bool)` answers, `None` simulates infrastructure
    /// failure. Counts calls so admin-bypass tests can assert it was skipped.
    struct StubChecker {
        allow: Option<bool>,
        calls: std::sync::atomic::AtomicU32,
    }

    #[async_trait::async_trait]
    impl temps_core::ProjectAccessChecker for StubChecker {
        async fn user_can_access_project(
            &self,
            _user_id: i32,
            _project_id: i32,
        ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            match self.allow {
                Some(b) => Ok(b),
                None => Err("checker database unreachable".into()),
            }
        }
    }

    fn service_with_checker(allow: Option<bool>) -> (OtelAuthService, Arc<StubChecker>) {
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
        );
        let svc = OtelAuthService::new(db);
        let checker = Arc::new(StubChecker {
            allow,
            calls: std::sync::atomic::AtomicU32::new(0),
        });
        svc.set_project_access_checker(checker.clone());
        (svc, checker)
    }

    #[tokio::test]
    async fn access_allowed_without_registered_checker() {
        // Plain OSS binary: no checker plugin → allow (guard no-op parity).
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
        );
        let svc = OtelAuthService::new(db);
        assert!(svc.check_project_access(1, "user", 42).await.is_ok());
    }

    #[tokio::test]
    async fn admin_roles_bypass_checker() {
        let (svc, checker) = service_with_checker(Some(false)); // would deny
        assert!(svc.check_project_access(1, "admin", 42).await.is_ok());
        assert!(svc
            .check_project_access(1, "platform_admin", 42)
            .await
            .is_ok());
        assert_eq!(
            checker.calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "instance admins must not consult the checker"
        );
    }

    #[tokio::test]
    async fn checker_verdict_is_enforced_for_non_admins() {
        let (svc, _) = service_with_checker(Some(true));
        assert!(svc.check_project_access(1, "user", 42).await.is_ok());

        let (svc, _) = service_with_checker(Some(false));
        assert!(matches!(
            svc.check_project_access(1, "user", 42).await,
            Err(OtelError::AuthFailed { .. })
        ));

        // Unknown/custom role strings get no bypass either.
        let (svc, _) = service_with_checker(Some(false));
        assert!(svc.check_project_access(1, "custom", 42).await.is_err());
    }

    #[tokio::test]
    async fn checker_infrastructure_failure_is_fail_closed() {
        let (svc, _) = service_with_checker(None); // simulates Err from checker
        assert!(matches!(
            svc.check_project_access(1, "user", 42).await,
            Err(OtelError::AuthFailed { .. })
        ));
    }

    // ── ServiceAuth struct ───────────────────────────────────────────────────

    #[test]
    fn test_service_auth_fields() {
        let auth = ServiceAuth {
            service_id: 42,
            service_name: "my-rustfs".to_string(),
            token_id: 7,
        };
        assert_eq!(auth.service_id, 42);
        assert_eq!(auth.service_name, "my-rustfs");
        assert_eq!(auth.token_id, 7);
    }

    // ── IngestAuth enum ──────────────────────────────────────────────────────

    #[test]
    fn test_ingest_auth_service_variant() {
        let auth = IngestAuth::Service(ServiceAuth {
            service_id: 1,
            service_name: "rustfs-prod".to_string(),
            token_id: 99,
        });
        match auth {
            IngestAuth::Service(sa) => assert_eq!(sa.service_id, 1),
            IngestAuth::Project(_) => panic!("expected Service variant"),
        }
    }

    #[test]
    fn test_ingest_auth_project_variant() {
        let auth = IngestAuth::Project(ProjectAuth {
            project_id: 5,
            environment_id: None,
            deployment_id: None,
            token_id: 3,
            project_name: "my-project".to_string(),
        });
        match auth {
            IngestAuth::Project(pa) => assert_eq!(pa.project_id, 5),
            IngestAuth::Service(_) => panic!("expected Project variant"),
        }
    }

    #[test]
    fn last_used_tracker_coalesces_updates_until_interval_elapses() {
        let tracker = LastUsedTracker::default();
        let key = TokenUsageKey {
            table: TokenTable::ApiKeys,
            token_id: 7,
        };
        let now = Instant::now();

        assert!(tracker.mark_if_due(key, now));
        assert!(!tracker.mark_if_due(key, now + Duration::from_secs(59)));
        assert!(tracker.mark_if_due(key, now + Duration::from_secs(60)));
    }

    #[test]
    fn last_used_tracker_retries_after_failed_update() {
        let tracker = LastUsedTracker::default();
        let key = TokenUsageKey {
            table: TokenTable::DeploymentTokens,
            token_id: 11,
        };
        let now = Instant::now();

        assert!(tracker.mark_if_due(key, now));
        tracker.forget(key);
        assert!(tracker.mark_if_due(key, now));
    }

    #[tokio::test]
    async fn deployment_auth_cache_avoids_repeated_database_queries_and_updates() {
        use sea_orm::{MockDatabase, MockExecResult};
        use std::collections::BTreeMap;

        let mut row: BTreeMap<&str, sea_orm::Value> = BTreeMap::new();
        row.insert("token_id", 17_i32.into());
        row.insert("project_id", 23_i32.into());
        row.insert("environment_id", Option::<i32>::None.into());
        row.insert("deployment_id", Option::<i32>::None.into());
        row.insert("project_name", "cached-project".into());

        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![row]])
                .append_exec_results(vec![MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 1,
                }])
                .into_connection(),
        );
        let service = OtelAuthService::new(db);

        let first = service.authenticate("dt_abcdefghij", None).await;
        let second = service.authenticate("dt_abcdefghij", None).await;

        assert!(first.is_ok());
        assert!(second.is_ok());
        assert_eq!(second.unwrap().project_id, 23);
    }

    #[tokio::test]
    async fn api_key_cache_does_not_reuse_access_verdict_across_projects() {
        use sea_orm::{MockDatabase, MockExecResult};
        use std::collections::BTreeMap;

        struct ProjectSpecificChecker;

        #[async_trait::async_trait]
        impl temps_core::ProjectAccessChecker for ProjectSpecificChecker {
            async fn user_can_access_project(
                &self,
                _user_id: i32,
                project_id: i32,
            ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
                Ok(project_id == 23)
            }
        }

        fn auth_row(project_name: &'static str) -> BTreeMap<&'static str, sea_orm::Value> {
            let mut row = BTreeMap::new();
            row.insert("key_id", 17_i32.into());
            row.insert("user_id", 19_i32.into());
            row.insert("role_type", "user".into());
            row.insert("project_name", project_name.into());
            row
        }

        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![
                    vec![auth_row("allowed-project")],
                    vec![auth_row("denied-project")],
                ])
                .append_exec_results(vec![MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 1,
                }])
                .into_connection(),
        );
        let service = OtelAuthService::new(db);
        service.set_project_access_checker(Arc::new(ProjectSpecificChecker));

        assert!(service
            .authenticate("tk_abcdefghij", Some(23))
            .await
            .is_ok());
        assert!(matches!(
            service.authenticate("tk_abcdefghij", Some(24)).await,
            Err(OtelError::AuthFailed { .. })
        ));
    }

    #[tokio::test]
    async fn deployment_auth_cache_does_not_cache_failed_lookups() {
        use sea_orm::{MockDatabase, MockExecResult};
        use std::collections::BTreeMap;

        let mut row: BTreeMap<&str, sea_orm::Value> = BTreeMap::new();
        row.insert("token_id", 17_i32.into());
        row.insert("project_id", 23_i32.into());
        row.insert("environment_id", Option::<i32>::None.into());
        row.insert("deployment_id", Option::<i32>::None.into());
        row.insert("project_name", "recovered-project".into());

        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![
                    // First lookup: no matching row (revoked/never existed).
                    Vec::<BTreeMap<&str, sea_orm::Value>>::new(),
                    // Second lookup, same token: now succeeds. If the first
                    // failure had been cached (moka's `try_get_with` is
                    // documented to only persist `Ok` values, never `Err`),
                    // this second call would incorrectly return the same
                    // cached failure instead of re-querying.
                    vec![row],
                ])
                .append_exec_results(vec![MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 1,
                }])
                .into_connection(),
        );
        let service = OtelAuthService::new(db);

        let first = service.authenticate("dt_abcdefghij", None).await;
        assert!(matches!(first, Err(OtelError::AuthFailed { .. })));

        let second = service.authenticate("dt_abcdefghij", None).await;
        assert_eq!(second.unwrap().project_id, 23);
    }

    /// NOTE on what this test does and does not prove: `sea_orm::MockDatabase`
    /// resolves synchronously (it never actually yields the executor), so
    /// under `tokio::join!` the first `authenticate` future typically runs to
    /// completion — including the moka cache insert — before the second is
    /// ever polled. This test therefore exercises "a second call for the same
    /// key issued immediately after the first reuses the cached result and
    /// does not re-query", not a genuine mid-flight race between two
    /// in-flight lookups. The stronger single-flight guarantee (concurrent
    /// callers for the same key share one in-flight computation rather than
    /// each starting their own) comes from moka's documented `try_get_with`
    /// semantics and isn't independently re-verified here. Kept as a
    /// regression guard: if cache-sharing were accidentally removed, this
    /// would fail (the mock has only one query result queued).
    #[tokio::test]
    async fn deployment_auth_cache_reuses_result_for_joined_calls_to_same_token() {
        use sea_orm::{MockDatabase, MockExecResult};
        use std::collections::BTreeMap;

        let mut row: BTreeMap<&str, sea_orm::Value> = BTreeMap::new();
        row.insert("token_id", 17_i32.into());
        row.insert("project_id", 23_i32.into());
        row.insert("environment_id", Option::<i32>::None.into());
        row.insert("deployment_id", Option::<i32>::None.into());
        row.insert("project_name", "cached-project".into());

        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                // Only ONE query result is queued. If the second call above
                // issued its own DB lookup instead of reusing the cached
                // verdict, it would find the mock's result queue exhausted
                // and fail/panic.
                .append_query_results(vec![vec![row]])
                .append_exec_results(vec![MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 1,
                }])
                .into_connection(),
        );
        let service = OtelAuthService::new(db);

        let (first, second) = tokio::join!(
            service.authenticate("dt_abcdefghij", None),
            service.authenticate("dt_abcdefghij", None)
        );

        assert_eq!(first.unwrap().project_id, 23);
        assert_eq!(second.unwrap().project_id, 23);
    }
}
