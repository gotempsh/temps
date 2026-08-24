use async_trait::async_trait;
use axum::http::StatusCode;
use chrono::{Duration, Utc};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set, TransactionTrait};
use std::sync::Arc;
use temps_core::{
    problemdetails::Problem, SensitiveAction, SensitiveActionAuthorizationError,
    SensitiveActionAuthorizer, SensitiveActionDecision, SensitiveActionPrincipal,
};
use thiserror::Error;

use crate::{context::AuthContext, user_service::UserService};

pub const STEP_UP_TTL_MINUTES: i64 = 5;

/// Default policy: interactive browser sessions whose user has **enrolled**
/// MFA need recent verification. Machine credentials (API keys, CLI device
/// tokens, deployment tokens) skip step-up entirely rather than being denied
/// — see "Machine credentials" below. Installations that need a stricter
/// posture for automation can provide a different policy through
/// [`SensitiveActionAuthorizer`].
///
/// # Machine credentials
///
/// API keys, CLI tokens, and deployment tokens are not challenged and are
/// not denied — they pass straight through to [`SensitiveActionDecision::Allow`].
/// This is a deliberate product decision, not an oversight: step-up is an
/// interactive TOTP/recovery-code prompt, which has no equivalent a script or
/// CI job can satisfy. Denying these principals outright (the original
/// policy) does not add security for a stolen key — the key already grants
/// whatever `permission_guard!` allows for its scope — it just silently
/// breaks any existing automation that calls a newly-gated endpoint, which
/// is a worse outcome than the exposure being closed. If a stronger posture
/// is needed for machine credentials specifically (e.g. a confirmation
/// header, or short-lived scoped tokens for destructive calls), that is a
/// distinct mechanism to design deliberately, not a side effect of this
/// authorizer's session-based step-up.
///
/// # Users without MFA
///
/// A user who has not enrolled MFA is **allowed through**, not blocked. Step-up
/// is a re-authentication of a factor you already have — asking for one you
/// never enrolled is not a check, it is a dead end. Previously such a user got
/// `RequireVerification { mfa_setup_required: true }`, which on a fresh
/// self-hosted instance meant the first admin could not create an API key or
/// complete a CLI device login at all: every route to a token is gated by
/// `SensitiveAction::CreateApiKey`, and the only way out was to enrol MFA they
/// may not want.
///
/// The tradeoff is explicit and was accepted deliberately: because the gate now
/// applies only to enrolled users, it can be avoided by not enrolling. It is
/// therefore a protection for accounts that opted into MFA — limiting the blast
/// radius of a stolen session cookie for those users — not a barrier an
/// attacker who controls account settings must clear.
///
/// What it is *not* weakened by: a stolen session cannot unenrol to escape the
/// challenge, because disabling MFA itself requires a valid TOTP code.
///
/// # Why this is not the loss it looks like — and what would change that
///
/// The gate this replaces was never a control for unenrolled users. Enrolment
/// itself is guarded only by `permission_guard!(auth, UsersWrite)`:
/// `POST /users/me/mfa/setup` and `/users/me/mfa/verify` ask for no password
/// and no step-up. An attacker holding a stolen session for an unenrolled
/// account could therefore enrol *their own* authenticator and satisfy the old
/// challenge in two calls — while leaving their TOTP secret on the victim's
/// account, which is both a persistence mechanism and a permanent lockout for
/// the real owner. Removing the challenge for those users removes that
/// incentive; it does not remove a barrier the attacker could not already
/// cross.
///
/// **Update:** `POST /users/me/mfa/setup` now requires the caller's current
/// password when the account has one set (see `UserService::setup_mfa`), so
/// the attack this section describes is closed for password-backed accounts
/// — a stolen session alone can no longer self-enrol without also knowing
/// the password. SSO-only accounts (no local password) still skip that
/// check, so the reasoning above still applies to them, and revisiting this
/// policy for password-backed accounts (e.g. requiring step-up before
/// unenrolled principals can act) is a reasonable follow-up now that the
/// premise has changed.
///
/// # Operators who want the stricter posture
///
/// `AppSettings::require_mfa_for_admins` is **not** sufficient on its own: it
/// gates the password-login path only, and OIDC logins are intentionally
/// unaffected. On an SSO instance, admins keep `users.mfa_enabled == false`
/// and so pass this gate without a challenge. Closing that requires either
/// enforcing enrolment in the identity provider, or registering a custom
/// [`SensitiveActionAuthorizer`] that denies unenrolled principals.
pub struct DefaultSensitiveActionAuthorizer {
    db: Arc<sea_orm::DatabaseConnection>,
}

impl DefaultSensitiveActionAuthorizer {
    pub fn new(db: Arc<sea_orm::DatabaseConnection>) -> Self {
        Self { db }
    }
}

#[async_trait]
impl SensitiveActionAuthorizer for DefaultSensitiveActionAuthorizer {
    async fn authorize(
        &self,
        action: &SensitiveAction,
        principal: &SensitiveActionPrincipal,
    ) -> Result<SensitiveActionDecision, SensitiveActionAuthorizationError> {
        let (user_id, session_id, mfa_enabled) = match principal {
            SensitiveActionPrincipal::UserSession {
                user_id,
                session_id,
                mfa_enabled,
            } => (user_id, session_id, mfa_enabled),
            // Machine credentials (API keys, CLI device tokens, deployment
            // tokens used by running containers) have no interactive factor
            // to re-verify, so step-up is not "denied" here, it is a no-op —
            // the same reasoning as the unenrolled-user case below, just for
            // a principal that can never enroll one. Their blast radius is
            // already bounded by the key's own scope/permissions, checked by
            // `permission_guard!` in the handler before this runs. Deliberate
            // product decision (not the original default, which denied these
            // outright): interactive step-up doesn't compose with scripted /
            // CI usage, so gating a destructive action here would silently
            // break existing automation rather than add friction to it.
            SensitiveActionPrincipal::ApiKey { user_id, key_id } => {
                tracing::info!(
                    user_id = *user_id,
                    key_id = *key_id,
                    action = action.as_str(),
                    step_up = "skipped_machine_principal",
                    "Sensitive action allowed without step-up: API key has no interactive factor to re-verify"
                );
                return Ok(SensitiveActionDecision::Allow);
            }
            SensitiveActionPrincipal::CliToken { user_id } => {
                tracing::info!(
                    user_id = *user_id,
                    action = action.as_str(),
                    step_up = "skipped_machine_principal",
                    "Sensitive action allowed without step-up: CLI token has no interactive factor to re-verify"
                );
                return Ok(SensitiveActionDecision::Allow);
            }
            SensitiveActionPrincipal::DeploymentToken { token_id } => {
                tracing::info!(
                    token_id = *token_id,
                    action = action.as_str(),
                    step_up = "skipped_machine_principal",
                    "Sensitive action allowed without step-up: deployment token has no interactive factor to re-verify"
                );
                return Ok(SensitiveActionDecision::Allow);
            }
        };

        // No enrolled factor, nothing to re-verify — allow. See the type-level
        // docs for why this is a deliberate policy choice and what it costs.
        //
        // Logged at INFO, not DEBUG: this is the record that the action ran at
        // a *reduced* assurance level. Without it the audit entry for e.g.
        // `create_api_key` is indistinguishable from one where step-up was
        // actually satisfied, and the difference is exactly what an incident
        // responder needs.
        if !mfa_enabled {
            tracing::info!(
                user_id = *user_id,
                action = action.as_str(),
                step_up = "skipped_no_mfa",
                "Sensitive action allowed without step-up: user has no MFA enrolled"
            );
            return Ok(SensitiveActionDecision::Allow);
        }

        let session = temps_entities::sessions::Entity::find_by_id(*session_id)
            .filter(temps_entities::sessions::Column::UserId.eq(*user_id))
            .filter(temps_entities::sessions::Column::ExpiresAt.gt(Utc::now()))
            .filter(temps_entities::sessions::Column::MfaPending.eq(false))
            .one(self.db.as_ref())
            .await
            .map_err(|error| SensitiveActionAuthorizationError {
                action: action.as_str(),
                reason: format!(
                    "failed to load active session {session_id} for user {user_id}: {error}"
                ),
            })?;

        let Some(session) = session else {
            return Ok(SensitiveActionDecision::Deny {
                reason: format!(
                    "Authenticated session {session_id} for user {user_id} is no longer active"
                ),
            });
        };

        if session
            .step_up_expires_at
            .is_some_and(|expires_at| expires_at > Utc::now())
        {
            return Ok(SensitiveActionDecision::Allow);
        }

        Ok(SensitiveActionDecision::RequireVerification {
            mfa_setup_required: false,
        })
    }
}

#[derive(Debug, Error)]
pub enum StepUpError {
    #[error("Sensitive-action verification requires an authenticated browser session")]
    SessionRequired,
    #[error("User {user_id} must configure MFA before sensitive actions can be verified")]
    MfaNotConfigured { user_id: i32 },
    #[error("MFA code is invalid for user {user_id}")]
    InvalidCode { user_id: i32 },
    #[error("User {user_id} was not found while verifying a sensitive action")]
    UserNotFound { user_id: i32 },
    #[error("Session {session_id} for user {user_id} is invalid or expired")]
    SessionNotFound { user_id: i32, session_id: i32 },
    #[error("Failed to verify MFA for user {user_id}: {reason}")]
    Verification { user_id: i32, reason: String },
    #[error("Failed to {operation} for user {user_id}, session {session_id:?}: {reason}")]
    Database {
        operation: &'static str,
        user_id: i32,
        session_id: Option<i32>,
        reason: String,
    },
}

/// Verifies a user's configured MFA method and elevates only the current
/// browser session for a short, bounded window.
pub struct StepUpService {
    db: Arc<sea_orm::DatabaseConnection>,
    user_service: Arc<UserService>,
}

impl StepUpService {
    pub fn new(db: Arc<sea_orm::DatabaseConnection>, user_service: Arc<UserService>) -> Self {
        Self { db, user_service }
    }

    pub async fn verify_and_elevate(
        &self,
        user_id: i32,
        session_id: i32,
        code: &str,
    ) -> Result<chrono::DateTime<Utc>, StepUpError> {
        let transaction = self
            .db
            .begin()
            .await
            .map_err(|error| StepUpError::Database {
                operation: "begin atomic verification",
                user_id,
                session_id: Some(session_id),
                reason: error.to_string(),
            })?;

        let valid = self
            .user_service
            .verify_mfa_code_in_transaction(&transaction, user_id, code)
            .await
            .map_err(|error| match error {
                crate::user_service::UserServiceError::MfaNotSetup(_) => {
                    StepUpError::MfaNotConfigured { user_id }
                }
                crate::user_service::UserServiceError::NotFound(_) => {
                    StepUpError::UserNotFound { user_id }
                }
                crate::user_service::UserServiceError::Database { reason } => {
                    StepUpError::Database {
                        operation: "verify MFA",
                        user_id,
                        session_id: Some(session_id),
                        reason,
                    }
                }
                other => StepUpError::Verification {
                    user_id,
                    reason: other.to_string(),
                },
            })?;
        if !valid {
            return Err(StepUpError::InvalidCode { user_id });
        }

        let session = temps_entities::sessions::Entity::find_by_id(session_id)
            .filter(temps_entities::sessions::Column::UserId.eq(user_id))
            .filter(temps_entities::sessions::Column::ExpiresAt.gt(Utc::now()))
            .filter(temps_entities::sessions::Column::MfaPending.eq(false))
            .one(&transaction)
            .await
            .map_err(|error| StepUpError::Database {
                operation: "load the active session",
                user_id,
                session_id: Some(session_id),
                reason: error.to_string(),
            })?
            .ok_or(StepUpError::SessionNotFound {
                user_id,
                session_id,
            })?;

        let expires_at = Utc::now() + Duration::minutes(STEP_UP_TTL_MINUTES);
        let mut session_update: temps_entities::sessions::ActiveModel = session.into();
        session_update.step_up_expires_at = Set(Some(expires_at));
        session_update
            .update(&transaction)
            .await
            .map_err(|error| StepUpError::Database {
                operation: "persist the elevated session",
                user_id,
                session_id: Some(session_id),
                reason: error.to_string(),
            })?;
        transaction
            .commit()
            .await
            .map_err(|error| StepUpError::Database {
                operation: "commit atomic verification",
                user_id,
                session_id: Some(session_id),
                reason: error.to_string(),
            })?;

        Ok(expires_at)
    }
}

/// Enforce the active sensitive-action policy and map its stable decisions to
/// Problem Details responses shared by every guarded endpoint.
pub async fn require_sensitive_action(
    authorizer: &dyn SensitiveActionAuthorizer,
    auth: &AuthContext,
    action: SensitiveAction,
) -> Result<(), Problem> {
    let action_name = action.as_str();
    let principal = auth.sensitive_action_principal().ok_or_else(|| {
        temps_core::error_builder::unauthorized()
            .title("Browser Session Required")
            .detail(format!(
                "Sensitive action '{action_name}' requires an authenticated browser session"
            ))
            .value("error_code", "SENSITIVE_ACTION_SESSION_REQUIRED")
            .value("action", action_name)
            .build()
    })?;

    match authorizer.authorize(&action, &principal).await {
        Ok(SensitiveActionDecision::Allow) => Ok(()),
        Ok(SensitiveActionDecision::RequireVerification { mfa_setup_required }) => Err(
            temps_core::error_builder::ErrorBuilder::new(StatusCode::PRECONDITION_REQUIRED)
                .type_("https://temps.sh/probs/step-up-required")
                .title("Additional Verification Required")
                .detail(format!(
                    "Sensitive action '{action_name}' requires recent MFA verification"
                ))
                .value("error_code", "STEP_UP_REQUIRED")
                .value("action", action_name)
                .value("mfa_setup_required", mfa_setup_required)
                .build(),
        ),
        Ok(SensitiveActionDecision::Deny { reason }) => {
            tracing::warn!(
                action = action_name,
                reason,
                "Sensitive action denied by policy"
            );
            Err(temps_core::error_builder::forbidden()
                .title("Sensitive Action Denied")
                .detail("This authentication method cannot perform the requested sensitive action")
                .value("error_code", "SENSITIVE_ACTION_DENIED")
                .value("action", action_name)
                .build())
        }
        Err(error) => {
            tracing::error!(action = action_name, %error, "Sensitive action authorization failed");
            Err(temps_core::error_builder::internal_server_error()
                .title("Sensitive Action Authorization Failed")
                .detail("The sensitive-action policy could not be evaluated")
                .value("error_code", "SENSITIVE_ACTION_AUTHORIZATION_FAILED")
                .value("action", action_name)
                .build())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};

    struct FixedDecisionAuthorizer(SensitiveActionDecision);

    #[async_trait]
    impl SensitiveActionAuthorizer for FixedDecisionAuthorizer {
        async fn authorize(
            &self,
            _action: &SensitiveAction,
            _principal: &SensitiveActionPrincipal,
        ) -> Result<SensitiveActionDecision, SensitiveActionAuthorizationError> {
            Ok(self.0.clone())
        }
    }

    fn authorizer_with_sessions(
        sessions: Vec<temps_entities::sessions::Model>,
    ) -> DefaultSensitiveActionAuthorizer {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([sessions])
            .into_connection();
        DefaultSensitiveActionAuthorizer::new(Arc::new(db))
    }

    fn browser_principal(mfa_enabled: bool) -> SensitiveActionPrincipal {
        SensitiveActionPrincipal::UserSession {
            user_id: 7,
            session_id: 11,
            mfa_enabled,
        }
    }

    fn session(
        step_up_expires_at: Option<chrono::DateTime<Utc>>,
    ) -> temps_entities::sessions::Model {
        temps_entities::sessions::Model {
            id: 11,
            user_id: 7,
            session_token: "redacted".to_string(),
            expires_at: Utc::now() + Duration::days(1),
            mfa_pending: false,
            step_up_expires_at,
        }
    }

    fn user(mfa_enabled: bool) -> temps_entities::users::Model {
        let now = Utc::now();
        temps_entities::users::Model {
            id: 7,
            name: "Sensitive Action User".to_string(),
            email: "sensitive-action@example.com".to_string(),
            password_hash: None,
            email_verified: true,
            email_verification_token: None,
            email_verification_expires: None,
            password_reset_token: None,
            password_reset_expires: None,
            must_change_password: false,
            deleted_at: None,
            mfa_secret: mfa_enabled.then(|| "JBSWY3DPEHPK3PXPJBSWY3DPEHPK3PXP".to_string()),
            mfa_enabled,
            mfa_recovery_codes: None,
            oidc_subject: None,
            oidc_provider_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn current_totp_code() -> String {
        let secret = base32::decode(
            base32::Alphabet::Rfc4648 { padding: true },
            "JBSWY3DPEHPK3PXPJBSWY3DPEHPK3PXP",
        )
        .expect("test MFA secret is valid base32");
        // In totp-rs 6.0.0: Secret::Raw is gone (Secret is now a struct, not an
        // enum). Raw bytes become Secret via From<Vec<u8>>. TOTP::new is gone;
        // use Builder. generate_current() returns Token (Display), not Result.
        totp_rs::Builder::new()
            .with_algorithm(totp_rs::Algorithm::SHA1)
            .with_digits(6)
            .with_skew(1)
            .with_step_duration(30)
            .with_secret(secret)
            .build_noncompliant()
            .generate_current()
            .to_string()
    }

    #[tokio::test]
    async fn recently_elevated_session_is_allowed() {
        let authorizer =
            authorizer_with_sessions(vec![session(Some(Utc::now() + Duration::minutes(1)))]);
        let decision = authorizer
            .authorize(&SensitiveAction::CreateApiKey, &browser_principal(true))
            .await
            .unwrap();
        assert_eq!(decision, SensitiveActionDecision::Allow);
    }

    #[tokio::test]
    async fn expired_elevation_requires_verification() {
        let authorizer =
            authorizer_with_sessions(vec![session(Some(Utc::now() - Duration::seconds(1)))]);
        let decision = authorizer
            .authorize(&SensitiveAction::CreateApiKey, &browser_principal(true))
            .await
            .unwrap();
        assert_eq!(
            decision,
            SensitiveActionDecision::RequireVerification {
                mfa_setup_required: false
            }
        );
    }

    #[tokio::test]
    async fn unenrolled_session_is_allowed_without_step_up() {
        // Step-up re-verifies a factor you already have. Demanding one from a
        // user who never enrolled is a dead end, not a check — and it made the
        // first admin of a fresh self-hosted instance unable to create an API
        // key or complete a CLI device login at all.
        let authorizer = authorizer_with_sessions(vec![session(None)]);
        let decision = authorizer
            .authorize(&SensitiveAction::CreateApiKey, &browser_principal(false))
            .await
            .unwrap();
        assert_eq!(decision, SensitiveActionDecision::Allow);
    }

    #[tokio::test]
    async fn unenrolled_session_is_allowed_for_every_action_kind() {
        // The policy is per-principal, not per-action: nothing should
        // reintroduce the dead end for one action but not another.
        let authorizer = authorizer_with_sessions(vec![session(None)]);
        for action in [
            SensitiveAction::CreateApiKey,
            SensitiveAction::DeleteEnvironment {
                project_id: 3,
                environment_id: 7,
            },
        ] {
            let decision = authorizer
                .authorize(&action, &browser_principal(false))
                .await
                .unwrap();
            assert_eq!(
                decision,
                SensitiveActionDecision::Allow,
                "action {} must not demand step-up from an unenrolled user",
                action.as_str()
            );
        }
    }

    #[tokio::test]
    async fn enrolled_session_without_recent_elevation_still_requires_verification() {
        // The other half of the contract: opting into MFA must still buy you
        // the protection. A user WITH a factor and no recent elevation is
        // challenged exactly as before.
        let authorizer = authorizer_with_sessions(vec![session(None)]);
        let decision = authorizer
            .authorize(&SensitiveAction::CreateApiKey, &browser_principal(true))
            .await
            .unwrap();
        assert_eq!(
            decision,
            SensitiveActionDecision::RequireVerification {
                mfa_setup_required: false
            }
        );
    }

    #[tokio::test]
    async fn machine_principals_bypass_step_up() {
        // Machine credentials have no interactive factor to re-verify, and
        // denying them outright silently breaks any automation (e.g. the
        // CLI) that legitimately calls a gated endpoint with a scoped key.
        // See the "Machine credentials" section of the type-level docs.
        let authorizer = authorizer_with_sessions(vec![]);
        for principal in [
            SensitiveActionPrincipal::ApiKey {
                user_id: 7,
                key_id: 9,
            },
            SensitiveActionPrincipal::CliToken { user_id: 7 },
            SensitiveActionPrincipal::DeploymentToken { token_id: 9 },
        ] {
            let decision = authorizer
                .authorize(
                    &SensitiveAction::DeleteEnvironment {
                        project_id: 3,
                        environment_id: 4,
                    },
                    &principal,
                )
                .await
                .unwrap();
            assert_eq!(
                decision,
                SensitiveActionDecision::Allow,
                "principal {:?} must bypass step-up, not be denied",
                principal
            );
        }
    }

    #[tokio::test]
    async fn verification_decision_maps_to_actionable_428_problem() {
        let auth = AuthContext::new_persisted_session(user(true), crate::Role::Admin, 11);
        let error = require_sensitive_action(
            &FixedDecisionAuthorizer(SensitiveActionDecision::RequireVerification {
                mfa_setup_required: false,
            }),
            &auth,
            SensitiveAction::CreateApiKey,
        )
        .await
        .expect_err("verification must interrupt the mutation");

        assert_eq!(error.status_code, StatusCode::PRECONDITION_REQUIRED);
        assert_eq!(
            error.body.get("error_code"),
            Some(&serde_json::json!("STEP_UP_REQUIRED"))
        );
        assert_eq!(
            error.body.get("action"),
            Some(&serde_json::json!("create_api_key"))
        );
        assert_eq!(
            error.body.get("mfa_setup_required"),
            Some(&serde_json::json!(false))
        );
    }

    #[tokio::test]
    async fn policy_denial_does_not_disclose_implementation_reason() {
        let auth = AuthContext::new_persisted_session(user(true), crate::Role::Admin, 11);
        let error = require_sensitive_action(
            &FixedDecisionAuthorizer(SensitiveActionDecision::Deny {
                reason: "private policy detail".to_string(),
            }),
            &auth,
            SensitiveAction::CreateApiKey,
        )
        .await
        .expect_err("denied policy must interrupt the mutation");

        assert_eq!(error.status_code, StatusCode::FORBIDDEN);
        assert!(!format!("{:?}", error.body).contains("private policy detail"));
    }

    #[tokio::test]
    async fn verification_elevates_only_the_selected_session() {
        let test_user = user(true);
        let active_session = session(None);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![test_user]])
            .append_query_results([vec![active_session]])
            .append_query_results([vec![session(Some(
                Utc::now() + Duration::minutes(STEP_UP_TTL_MINUTES),
            ))]])
            .into_connection();
        let db = Arc::new(db);
        let service = StepUpService::new(db.clone(), Arc::new(UserService::new(db.clone())));

        let expires_at = service
            .verify_and_elevate(7, 11, &current_totp_code())
            .await
            .expect("valid MFA should elevate the active session");

        assert!(expires_at > Utc::now() + Duration::minutes(4));

        drop(service);
        let log = Arc::try_unwrap(db)
            .expect("service released the database")
            .into_transaction_log();
        assert_eq!(log.len(), 1, "verification and elevation must be atomic");
        let statements = log[0].statements();
        assert!(
            statements
                .iter()
                .any(|statement| statement.sql.contains("FOR UPDATE")),
            "the MFA user row must remain locked until elevation commits"
        );
        assert!(statements.iter().any(|statement| {
            statement.sql.starts_with("UPDATE \"sessions\"")
                && statement.sql.contains("step_up_expires_at")
        }));
    }

    #[tokio::test]
    async fn verification_requires_mfa_enrollment() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![user(false)]])
            .into_connection();
        let db = Arc::new(db);
        let service = StepUpService::new(db.clone(), Arc::new(UserService::new(db)));

        let result = service.verify_and_elevate(7, 11, "123456").await;
        assert!(matches!(
            result,
            Err(StepUpError::MfaNotConfigured { user_id: 7 })
        ));
    }

    #[tokio::test]
    async fn invalid_code_does_not_elevate_the_session() {
        let test_user = user(true);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![test_user]])
            .into_connection();
        let db = Arc::new(db);
        let service = StepUpService::new(db.clone(), Arc::new(UserService::new(db)));

        let result = service.verify_and_elevate(7, 11, "not-a-code").await;
        assert!(matches!(
            result,
            Err(StepUpError::InvalidCode { user_id: 7 })
        ));
    }

    #[tokio::test]
    async fn expired_session_is_rejected_without_committing_elevation() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![user(true)]])
            .append_query_results([Vec::<temps_entities::sessions::Model>::new()])
            .into_connection();
        let db = Arc::new(db);
        let service = StepUpService::new(db.clone(), Arc::new(UserService::new(db)));

        let result = service
            .verify_and_elevate(7, 11, &current_totp_code())
            .await;
        assert!(matches!(
            result,
            Err(StepUpError::SessionNotFound {
                user_id: 7,
                session_id: 11
            })
        ));
    }
}
