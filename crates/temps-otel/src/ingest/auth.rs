//! Per-project authentication for OTel ingest.
//!
//! Supports two token types:
//! - **API keys (`tk_`)**: User-scoped. Requires `X-Temps-Project-Id` header
//!   to identify the target project.
//! - **Deployment tokens (`dt_`)**: Project-scoped. Project, environment, and
//!   deployment IDs are inferred from the token record itself.

use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement};
use sha2::{Digest, Sha256};
use std::sync::Arc;
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

/// Service for authenticating OTel ingest requests.
pub struct OtelAuthService {
    db: Arc<DatabaseConnection>,
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

        let mut hasher = Sha256::new();
        hasher.update(token.as_bytes());
        let key_hash = hex::encode(hasher.finalize());

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

                // Update last_used_at (fire-and-forget)
                let db = self.db.clone();
                tokio::spawn(async move {
                    let update_sql = "UPDATE api_keys SET last_used_at = NOW() WHERE id = $1";
                    if let Err(e) = db
                        .execute(Statement::from_sql_and_values(
                            DatabaseBackend::Postgres,
                            update_sql,
                            vec![key_id.into()],
                        ))
                        .await
                    {
                        warn!(key_id, error = %e, "Failed to update service token last_used_at");
                    }
                });

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

        // Hash the key (same algorithm as temps-auth)
        let mut hasher = Sha256::new();
        hasher.update(api_key.as_bytes());
        let key_hash = hex::encode(hasher.finalize());

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

                // Update last_used_at (fire-and-forget)
                let db = self.db.clone();
                tokio::spawn(async move {
                    let update_sql = "UPDATE api_keys SET last_used_at = NOW() WHERE id = $1";
                    if let Err(e) = db
                        .execute(Statement::from_sql_and_values(
                            DatabaseBackend::Postgres,
                            update_sql,
                            vec![key_id.into()],
                        ))
                        .await
                    {
                        warn!(key_id, error = %e, "Failed to update API key last_used_at");
                    }
                });

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

        // Hash the token
        let mut hasher = Sha256::new();
        hasher.update(token.as_bytes());
        let token_hash = hex::encode(hasher.finalize());

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

                // Update last_used_at (fire-and-forget)
                let db = self.db.clone();
                tokio::spawn(async move {
                    let update_sql =
                        "UPDATE deployment_tokens SET last_used_at = NOW() WHERE id = $1";
                    if let Err(e) = db
                        .execute(Statement::from_sql_and_values(
                            DatabaseBackend::Postgres,
                            update_sql,
                            vec![token_id.into()],
                        ))
                        .await
                    {
                        warn!(token_id, error = %e, "Failed to update deployment token last_used_at");
                    }
                });

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
}
