// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! TempsMiddleware implementation for authentication
//!
//! This module provides the TempsMiddleware trait implementation for authentication,
//! allowing the auth middleware to integrate properly with the plugin system while
//! maintaining access to the AuthState services.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use axum::{
    extract::{MatchedPath, Request},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};
use temps_core::plugin::{
    MiddlewareCondition, MiddlewarePriority, PluginContext, PluginError, TempsMiddleware,
};

use crate::permission_denial_recorder::{
    AuthSourceKind, PermissionDenialEvent, PermissionDenialRecorder, SafePrincipal,
};
use crate::{
    apikey_service::ApiKeyService, auth_service::AuthService,
    deployment_token_service::DeploymentTokenValidationService, user_service::UserService,
};

/// Permission-denial events are attacker-triggerable. Keep the queued and
/// persisted origin metadata small even when the HTTP stack accepts a large
/// request header block.
const MAX_AUDIT_USER_AGENT_BYTES: usize = 512;
use temps_core::CookieCrypto;
use tracing::warn;

/// Authentication middleware that implements TempsMiddleware
pub struct AuthMiddleware {
    api_key_service: Arc<ApiKeyService>,
    auth_service: Arc<AuthService>,
    user_service: Arc<UserService>,
    cookie_crypto: Arc<CookieCrypto>,
    deployment_token_service: DeploymentTokenValidationService,
    permission_denial_recorder: Arc<PermissionDenialRecorder>,
}

impl AuthMiddleware {
    pub fn new(
        api_key_service: Arc<ApiKeyService>,
        auth_service: Arc<AuthService>,
        user_service: Arc<UserService>,
        cookie_crypto: Arc<CookieCrypto>,
        db: Arc<sea_orm::DatabaseConnection>,
        permission_denial_recorder: Arc<PermissionDenialRecorder>,
    ) -> Self {
        let deployment_token_service = DeploymentTokenValidationService::new(db);
        Self {
            api_key_service,
            auth_service,
            user_service,
            cookie_crypto,
            deployment_token_service,
            permission_denial_recorder,
        }
    }
}

// Note: Default implementation removed since AuthState is required

impl TempsMiddleware for AuthMiddleware {
    fn name(&self) -> &'static str {
        "auth_middleware"
    }

    fn plugin_name(&self) -> &'static str {
        "auth"
    }

    fn priority(&self) -> MiddlewarePriority {
        MiddlewarePriority::Security
    }

    fn condition(&self) -> MiddlewareCondition {
        MiddlewareCondition::Always
    }

    fn initialize(&mut self, _context: &PluginContext) -> Result<(), PluginError> {
        // AuthState is already provided in constructor, nothing to initialize
        Ok(())
    }

    fn execute<'a>(
        &'a self,
        req: Request,
        next: Next,
    ) -> Pin<Box<dyn Future<Output = Result<Response, StatusCode>> + Send + 'a>> {
        Box::pin(async move {
            // Use the AuthState directly

            // Call the simplified auth middleware
            match self.execute_auth_middleware_logic(req, next).await {
                Ok(response) => Ok(response),
                Err(status) => Ok(status.into_response()),
            }
        })
    }
}
impl AuthMiddleware {
    /// Simplified auth middleware that replicates the core logic without Send issues
    async fn execute_auth_middleware_logic(
        &self,
        mut req: Request,
        next: Next,
    ) -> Result<Response, StatusCode> {
        let mut user = None;
        // Set only when the credential for this request was a session cookie
        // read off the wire, which is the only credential a browser attaches
        // on its own. It gates the same-origin check below; a bearer token
        // and an `AuthContext` injected by an in-process caller are both
        // unreachable from a hostile page, so neither is affected.
        let mut cookie_session = false;

        // Extract auth context. Each lookup answers valid, rejected or
        // unavailable: a credential the database could not be asked about is
        // not a wrong credential, and must not be answered with a 401 the
        // client reads as "your key is revoked" (a 401 is not retryable).
        let auth_context = if let Some(auth_header) = req.headers().get("authorization") {
            if let Ok(auth_str) = auth_header.to_str() {
                if auth_str.starts_with("Bearer ") {
                    let token = auth_str.trim_start_matches("Bearer ");

                    // Try API key first (they have a specific format: tk_...)
                    if token.starts_with("tk_") || token.starts_with("tkp_") {
                        let lookup =
                            bounded_lookup(self.api_key_service.validate_api_key_with_scope(token))
                                .await;
                        match classify_api_key_lookup(lookup) {
                            CredentialLookup::Valid(validated) => {
                                user = Some(validated.user.clone());
                                Some(validated.into_auth_context())
                            }
                            CredentialLookup::Rejected => None,
                            CredentialLookup::Unavailable(reason) => {
                                return Ok(credential_check_unavailable(
                                    CredentialKind::ApiKey,
                                    &reason,
                                ));
                            }
                        }
                    } else if token.starts_with("dt_") {
                        // Try deployment token (they have a specific format: dt_...)
                        let lookup =
                            bounded_lookup(self.deployment_token_service.validate_token(token))
                                .await;
                        match classify_deployment_token_lookup(lookup) {
                            CredentialLookup::Valid(validated) => {
                                Some(crate::context::AuthContext::new_deployment_token(
                                    validated.project_id,
                                    validated.environment_id,
                                    validated.deployment_id,
                                    validated.token_id,
                                    validated.name,
                                    validated.permissions,
                                ))
                            }
                            CredentialLookup::Rejected => None,
                            CredentialLookup::Unavailable(reason) => {
                                return Ok(credential_check_unavailable(
                                    CredentialKind::DeploymentToken,
                                    &reason,
                                ));
                            }
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            // Try session cookie (only "session" cookie is used for user authentication)
            // Whether this request's cookies carry `Secure` decides which
            // cookie name may authenticate it. It comes from
            // `RequestMetadata` (trusted server-side state, not a client
            // header); with no metadata the strict choice is the safe one,
            // so only the host-only name is read.
            let secure_cookies = req
                .extensions()
                .get::<temps_core::RequestMetadata>()
                .is_none_or(|metadata| metadata.is_secure);
            if let Some(session_token) = self.extract_user_session_from_cookies(
                req.headers(),
                secure_cookies,
                &self.cookie_crypto,
            ) {
                let lookup =
                    bounded_lookup(self.auth_service.verify_session_details(&session_token)).await;
                match classify_session_lookup(lookup) {
                    CredentialLookup::Valid(verified) => {
                        let session_user = verified.user;
                        // Resolved from the database on every request (never
                        // cached on the session), so a role removal applies to
                        // this session's very next request. A lookup failure
                        // degrades to the least-privileged session role.
                        let user_role = self
                            .user_service
                            .current_session_role(session_user.id)
                            .await
                            .unwrap_or(crate::permissions::Role::User);
                        user = Some(session_user.clone());
                        cookie_session = true;

                        Some(crate::context::AuthContext::new_persisted_session(
                            session_user,
                            user_role,
                            verified.session_id,
                        ))
                    }
                    CredentialLookup::Rejected => None,
                    CredentialLookup::Unavailable(reason) => {
                        return Ok(credential_check_unavailable(
                            CredentialKind::Session,
                            &reason,
                        ));
                    }
                }
            } else {
                None
            }
        };

        // `RequestMetadata` is injected by
        // `temps_core::RequestMetadataMiddleware` (Observability priority),
        // which runs before this middleware on both the admin and public
        // routers. Auth used to build it inline; that responsibility moved
        // out so public ingest endpoints get metadata without auth.

        // Insert authenticated user and context. Anonymous requests stay
        // anonymous: there is no implicit promotion to a Reader role for
        // unauthenticated callers. Issue an authenticated reader API key
        // if you want read-only programmatic access.
        if let Some(auth) = &auth_context {
            if let Err(problem) = temps_authz::enforce_api_key_path(auth, req.uri().path()) {
                return Ok(problem.into_response());
            }
        }

        if let Some(user) = user {
            req.extensions_mut().insert(user);
        }
        let principal = safe_principal_for_request(&req, auth_context.as_ref());
        if let Some(auth_ctx) = auth_context {
            req.extensions_mut().insert(auth_ctx);
        }

        let method = req.method().as_str().to_string();
        let route = matched_route_template(&req);
        let (ip_address, user_agent) = req
            .extensions()
            .get::<temps_core::RequestMetadata>()
            .map(|metadata| {
                (
                    Some(metadata.ip_address.clone()),
                    bounded_audit_user_agent(&metadata.user_agent),
                )
            })
            .unwrap_or_else(|| (None, "unknown".to_string()));

        // The console's CSRF boundary, enforced once here rather than in each
        // of the handlers that change state. `SameSite=Strict` keeps the
        // session cookie off a *cross-site* request but not off a
        // sibling-subdomain one, and a bodyless, `multipart/form-data` or
        // plain-text request needs no preflight to ask for. So a request that
        // authenticated with the session cookie must also prove it came from
        // this origin before it reaches a handler.
        if cookie_session
            && temps_core::same_origin::is_state_changing_method(req.method())
            && !temps_core::same_origin::request_is_same_origin(req.headers())
        {
            warn!(
                method = %req.method(),
                route = %route,
                "Refused a cookie-authenticated request from another origin"
            );
            let response = cross_origin_rejection().into_response();
            record_permission_denial_response(
                self.permission_denial_recorder.as_ref(),
                &response,
                principal,
                method,
                route,
                ip_address,
                user_agent,
            );
            return Ok(response);
        }

        // Run the next middleware/handler
        let response = next.run(req).await;
        record_permission_denial_response(
            self.permission_denial_recorder.as_ref(),
            &response,
            principal,
            method,
            route,
            ip_address,
            user_agent,
        );
        Ok(response)
    }

    /// The session token from the session cookie, host-only name first.
    ///
    /// Only this cookie authenticates a user. Preferring `__Host-session`
    /// over the unprefixed name is what stops a sibling app's planted
    /// parent-domain cookie standing in for the console's own; see
    /// [`crate::auth_cookies`].
    fn extract_user_session_from_cookies(
        &self,
        headers: &axum::http::HeaderMap,
        secure: bool,
        crypto: &temps_core::CookieCrypto,
    ) -> Option<String> {
        crate::auth_cookies::read_sealed(headers, crate::auth_cookies::SESSION, secure, crypto)
    }
}

/// How long one credential lookup may take before the request is answered
/// with a 503. The lookups are a few indexed reads that take milliseconds; a
/// lookup still waiting after this long is queued on a database that cannot
/// be reached, and holding the request for the pool's acquire timeout on
/// every query (minutes in total) only delays the same answer.
const CREDENTIAL_LOOKUP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Seconds a client is asked to wait before retrying a credential check the
/// server could not complete.
const CREDENTIAL_CHECK_RETRY_AFTER_SECS: u64 = 5;

/// Problem `type` of the 503 answered when a credential could not be checked.
pub const CREDENTIAL_CHECK_UNAVAILABLE_PROBLEM_TYPE: &str =
    "https://temps.sh/probs/credential-check-unavailable";

/// The outcome of looking one credential up.
#[derive(Debug)]
enum CredentialLookup<T> {
    /// The credential is valid.
    Valid(T),
    /// The credential is unknown, revoked, expired or otherwise refused: the
    /// request is anonymous, and a protected route answers 401.
    Rejected,
    /// The lookup itself failed (the database could not be reached or timed
    /// out): nothing is known about the credential.
    Unavailable(String),
}

/// A lookup that timed out, as distinct from one that returned an error.
#[derive(Debug)]
struct LookupTimedOut;

async fn bounded_lookup<T, E>(
    lookup: impl Future<Output = Result<T, E>>,
) -> Result<Result<T, E>, LookupTimedOut> {
    tokio::time::timeout(CREDENTIAL_LOOKUP_TIMEOUT, lookup)
        .await
        .map_err(|_| LookupTimedOut)
}

fn timed_out_reason() -> String {
    format!(
        "the credential lookup did not finish within {}s",
        CREDENTIAL_LOOKUP_TIMEOUT.as_secs()
    )
}

fn classify_api_key_lookup<T>(
    lookup: Result<Result<T, crate::apikey_service::ApiKeyServiceError>, LookupTimedOut>,
) -> CredentialLookup<T> {
    use crate::apikey_service::ApiKeyServiceError as E;
    match lookup {
        Ok(Ok(validated)) => CredentialLookup::Valid(validated),
        Ok(Err(E::DatabaseError(error))) => CredentialLookup::Unavailable(error.to_string()),
        // Unknown, inactive, expired or mis-scoped keys, and a key whose
        // stored permissions cannot be read: the key itself is refused.
        Ok(Err(
            E::NotFound(_)
            | E::ValidationError(_)
            | E::Unauthorized(_)
            | E::Conflict(_)
            | E::InternalServerError(_)
            | E::InvalidName { .. }
            | E::ExpiryInPast { .. }
            | E::PermissionsNeedCustomRole { .. }
            | E::PermissionsImmutable { .. }
            | E::ExpiryRequired { .. },
        )) => CredentialLookup::Rejected,
        Err(LookupTimedOut) => CredentialLookup::Unavailable(timed_out_reason()),
    }
}

fn classify_deployment_token_lookup<T>(
    lookup: Result<
        Result<T, crate::deployment_token_service::DeploymentTokenValidationError>,
        LookupTimedOut,
    >,
) -> CredentialLookup<T> {
    use crate::deployment_token_service::DeploymentTokenValidationError as E;
    match lookup {
        Ok(Ok(validated)) => CredentialLookup::Valid(validated),
        Ok(Err(E::DatabaseError(error))) => CredentialLookup::Unavailable(error.to_string()),
        Ok(Err(E::InvalidToken | E::TokenExpired | E::TokenInactive)) => CredentialLookup::Rejected,
        Err(LookupTimedOut) => CredentialLookup::Unavailable(timed_out_reason()),
    }
}

fn classify_session_lookup<T>(
    lookup: Result<Result<T, crate::auth_service::AuthError>, LookupTimedOut>,
) -> CredentialLookup<T> {
    use crate::auth_service::AuthError as E;
    match lookup {
        Ok(Ok(verified)) => CredentialLookup::Valid(verified),
        Ok(Err(E::DatabaseError { reason })) => CredentialLookup::Unavailable(reason),
        Ok(Err(E::DatabaseConnectionError(reason))) => CredentialLookup::Unavailable(reason),
        // Session verification only reads two rows; every other variant
        // (not found, expired, deleted user) means the session is refused.
        Ok(Err(_)) => CredentialLookup::Rejected,
        Err(LookupTimedOut) => CredentialLookup::Unavailable(timed_out_reason()),
    }
}

#[derive(Debug, Clone, Copy)]
enum CredentialKind {
    ApiKey,
    DeploymentToken,
    Session,
}

impl CredentialKind {
    fn label(self) -> &'static str {
        match self {
            CredentialKind::ApiKey => "API key",
            CredentialKind::DeploymentToken => "deployment token",
            CredentialKind::Session => "session",
        }
    }
}

/// The 503 for a credential the server could not check. The database
/// error stays in the log; the client is told only that the check, not its
/// credential, failed, and when to try again.
fn credential_check_unavailable(kind: CredentialKind, reason: &str) -> Response {
    tracing::error!(
        credential = kind.label(),
        reason = %reason,
        "Could not check the request's credential: the database lookup failed; \
         answering 503 instead of treating the request as unauthenticated"
    );
    temps_core::problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
        .with_type(CREDENTIAL_CHECK_UNAVAILABLE_PROBLEM_TYPE)
        .with_title("Credential Check Unavailable")
        .with_detail(format!(
            "The server could not check this request's {} because its database is \
             unavailable. The credential was not rejected; retry after {} seconds",
            kind.label(),
            CREDENTIAL_CHECK_RETRY_AFTER_SECS
        ))
        .with_retry_after(CREDENTIAL_CHECK_RETRY_AFTER_SECS)
        .into_response()
}

/// The console's answer to a cookie-authenticated request from another
/// origin. Says what to do instead, because a legitimate non-browser caller
/// that trips this has a supported way through: a bearer token, which no
/// browser attaches on its own.
fn cross_origin_rejection() -> temps_core::problemdetails::Problem {
    temps_core::error_builder::ErrorBuilder::new(StatusCode::FORBIDDEN)
        .title("Cross-Origin Request Refused")
        .detail(
            "This request changes state, authenticated with the session cookie, and did not \
             come from this origin. Browsers must call the API from the console itself; \
             programmatic clients should authenticate with an API key or token instead.",
        )
        .permission_denial(
            temps_core::problemdetails::PermissionDenialKind::CrossOriginRequest,
            None,
        )
        .build()
}

fn record_permission_denial_response(
    recorder: &PermissionDenialRecorder,
    response: &Response,
    principal: SafePrincipal,
    method: String,
    route: String,
    ip_address: Option<String>,
    user_agent: String,
) {
    if let Some(event) =
        permission_denial_event(response, principal, method, route, ip_address, user_agent)
    {
        recorder.record(event);
    }
}

fn bounded_audit_user_agent(user_agent: &str) -> String {
    if user_agent.len() <= MAX_AUDIT_USER_AGENT_BYTES {
        return user_agent.to_string();
    }

    let mut end = MAX_AUDIT_USER_AGENT_BYTES;
    while !user_agent.is_char_boundary(end) {
        end -= 1;
    }
    user_agent[..end].to_string()
}

fn safe_principal(auth: &crate::context::AuthContext) -> SafePrincipal {
    let (source, credential_id) = match &auth.source {
        crate::context::AuthSource::Session { .. } => (AuthSourceKind::Session, None),
        crate::context::AuthSource::CliToken { .. } => (AuthSourceKind::CliToken, None),
        crate::context::AuthSource::ApiKey { key_id, .. } => {
            (AuthSourceKind::ApiKey, Some(*key_id))
        }
        crate::context::AuthSource::DeploymentToken { token_id, .. } => {
            (AuthSourceKind::DeploymentToken, Some(*token_id))
        }
    };

    SafePrincipal {
        user_id: auth.user_id_opt(),
        source,
        credential_id,
    }
}

/// Resolve audit attribution from the credential authenticated here or from a
/// trusted context injected by an in-process caller before the router runs.
///
/// Normal HTTP credentials take precedence. A request extension cannot be
/// supplied by an external HTTP caller, while delegated bridges such as the
/// external-plugin host API use it to carry their already-verified actor.
fn safe_principal_for_request(
    req: &Request,
    authenticated: Option<&crate::context::AuthContext>,
) -> SafePrincipal {
    authenticated
        .or_else(|| req.extensions().get::<crate::context::AuthContext>())
        .map(safe_principal)
        .unwrap_or_else(SafePrincipal::anonymous)
}

fn matched_route_template(req: &Request) -> String {
    req.extensions()
        .get::<MatchedPath>()
        .map(|matched| matched.as_str().to_string())
        .unwrap_or_else(|| "unmatched".to_string())
}

fn permission_denial_event(
    response: &Response,
    principal: SafePrincipal,
    method: String,
    route: String,
    ip_address: Option<String>,
    user_agent: String,
) -> Option<PermissionDenialEvent> {
    if response.status() != StatusCode::FORBIDDEN {
        return None;
    }

    let marker = response
        .extensions()
        .get::<temps_core::problemdetails::PermissionDenialMarker>()?;
    Some(PermissionDenialEvent {
        principal,
        method,
        route,
        denial_kind: marker.kind().as_str().to_string(),
        required_permission: marker.required_permission().map(str::to_string),
        ip_address,
        user_agent,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::extract::Extension;
    use axum::http::StatusCode;
    use axum::middleware::from_fn;
    use axum::routing::get;
    use axum::Router;
    use chrono::Utc;
    use temps_core::problemdetails::PermissionDenialKind;
    use temps_core::{AuditLogger, AuditOperation};
    use temps_entities::deployment_tokens::DeploymentTokenPermission;
    use temps_entities::users;
    use tower::ServiceExt;

    use super::*;
    use crate::permissions::{Permission, Role};

    #[derive(Default)]
    struct WorkflowRecordingLogger {
        records: Mutex<Vec<serde_json::Value>>,
    }

    #[async_trait::async_trait]
    impl AuditLogger for WorkflowRecordingLogger {
        async fn create_audit_log(
            &self,
            operation: &dyn AuditOperation,
        ) -> temps_core::AuditResult<()> {
            self.records
                .lock()
                .map_err(|_| {
                    temps_core::AuditError::write(
                        operation.operation_type(),
                        "workflow logger lock poisoned",
                    )
                })?
                .push(
                    serde_json::from_str(&operation.serialize()?)
                        .expect("audit operation serializes to valid JSON"),
                );
            Ok(())
        }
    }

    fn test_user() -> users::Model {
        let now = Utc::now();
        users::Model {
            id: 42,
            name: "User-chosen name must not leak".to_string(),
            email: "private@example.com".to_string(),
            password_hash: None,
            email_verified: true,
            email_verification_token: None,
            email_verification_expires: None,
            password_reset_token: None,
            password_reset_expires: None,
            must_change_password: false,
            deleted_at: None,
            mfa_secret: None,
            mfa_enabled: false,
            mfa_recovery_codes: None,
            oidc_subject: None,
            oidc_provider_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn event_from(response: &Response) -> Option<PermissionDenialEvent> {
        permission_denial_event(
            response,
            SafePrincipal::anonymous(),
            "GET".to_string(),
            "/projects/{project_id}".to_string(),
            Some("203.0.113.5".to_string()),
            "test-agent".to_string(),
        )
    }

    #[test]
    fn denial_user_agent_is_utf8_safely_byte_bounded() {
        let oversized = format!("{}{}", "a".repeat(MAX_AUDIT_USER_AGENT_BYTES - 1), "éé");
        let bounded = bounded_audit_user_agent(&oversized);

        assert!(bounded.len() <= MAX_AUDIT_USER_AGENT_BYTES);
        assert!(bounded.is_char_boundary(bounded.len()));
        assert_eq!(bounded, "a".repeat(MAX_AUDIT_USER_AGENT_BYTES - 1));
    }

    #[test]
    fn marked_guard_403_becomes_an_audit_event() {
        let response = temps_core::error_builder::ErrorBuilder::new(StatusCode::FORBIDDEN)
            .permission_denial(
                PermissionDenialKind::InsufficientPermission,
                Some("projects:write".to_string()),
            )
            .build()
            .into_response();

        let event = event_from(&response).expect("marked guard denial should be audited");
        assert_eq!(event.denial_kind, "insufficient_permission");
        assert_eq!(event.required_permission.as_deref(), Some("projects:write"));
        assert_eq!(event.route, "/projects/{project_id}");
    }

    #[test]
    fn delegated_auth_context_is_used_for_denial_attribution() {
        let auth = crate::context::AuthContext::new_api_key(
            test_user(),
            Some(Role::User),
            Some(vec![Permission::ProjectsRead]),
            "delegated-plugin-key".to_string(),
            99,
        );
        let mut request = Request::new(axum::body::Body::empty());
        request.extensions_mut().insert(auth);

        let principal = safe_principal_for_request(&request, None);
        let response = temps_core::error_builder::ErrorBuilder::new(StatusCode::FORBIDDEN)
            .permission_denial(
                PermissionDenialKind::InsufficientPermission,
                Some("projects:write".to_string()),
            )
            .build()
            .into_response();
        let event = permission_denial_event(
            &response,
            principal,
            "POST".to_string(),
            "/projects".to_string(),
            None,
            "plugin-channel".to_string(),
        )
        .expect("delegated permission denial should be audited");

        assert_eq!(event.principal.user_id, Some(42));
        assert_eq!(event.principal.source, AuthSourceKind::ApiKey);
        assert_eq!(event.principal.credential_id, Some(99));
    }

    #[test]
    fn authenticated_credential_takes_precedence_over_delegated_context() {
        let mut delegated_user = test_user();
        delegated_user.id = 7;
        let delegated = crate::context::AuthContext::new_api_key(
            delegated_user,
            Some(Role::User),
            Some(vec![Permission::ProjectsRead]),
            "delegated-key".to_string(),
            70,
        );
        let authenticated = crate::context::AuthContext::new_session(test_user(), Role::Admin);
        let mut request = Request::new(axum::body::Body::empty());
        request.extensions_mut().insert(delegated);

        let principal = safe_principal_for_request(&request, Some(&authenticated));

        assert_eq!(principal.user_id, Some(42));
        assert_eq!(principal.source, AuthSourceKind::Session);
        assert_eq!(principal.credential_id, None);
    }

    #[tokio::test(start_paused = true)]
    async fn guard_response_flows_through_middleware_recorder_to_audit_logger() {
        async fn guarded_handler(
            Extension(auth): Extension<crate::context::AuthContext>,
        ) -> Result<StatusCode, temps_core::problemdetails::Problem> {
            crate::permission_guard!(auth, ProjectsWrite);
            Ok(StatusCode::OK)
        }

        let auth = crate::context::AuthContext::new_api_key(
            test_user(),
            Some(Role::User),
            Some(vec![Permission::ProjectsRead]),
            "workflow-key".to_string(),
            99,
        );
        let principal = safe_principal(&auth);
        let response = Router::new()
            .route("/projects/{project_id}", get(guarded_handler))
            .layer(Extension(auth))
            .oneshot(
                Request::builder()
                    .uri("/projects/7")
                    .body(axum::body::Body::empty())
                    .expect("workflow request should build"),
            )
            .await
            .expect("guarded router should respond");
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        let logger = Arc::new(WorkflowRecordingLogger::default());
        let recorder = PermissionDenialRecorder::new(logger.clone());
        record_permission_denial_response(
            recorder.as_ref(),
            &response,
            principal,
            "GET".to_string(),
            "/projects/{project_id}".to_string(),
            Some("203.0.113.5".to_string()),
            "workflow-agent".to_string(),
        );

        // Let the worker arm its aggregation window, then advance to its flush.
        tokio::task::yield_now().await;
        tokio::time::advance(std::time::Duration::from_secs(61)).await;
        tokio::task::yield_now().await;

        let records = logger.records.lock().expect("workflow logger lock");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["denial_kind"], "insufficient_permission");
        assert_eq!(records[0]["required_permission"], "projects:write");
        assert_eq!(records[0]["route"], "/projects/{project_id}");
    }

    #[test]
    fn generic_business_403_is_ignored() {
        let response = StatusCode::FORBIDDEN.into_response();
        assert!(event_from(&response).is_none());
    }

    #[test]
    fn marked_non_403_is_ignored() {
        let response = temps_core::problemdetails::new(StatusCode::BAD_REQUEST)
            .with_permission_denial(PermissionDenialKind::ProjectAccess, None)
            .into_response();
        assert!(event_from(&response).is_none());
    }

    #[test]
    fn safe_principal_keeps_only_source_kind_and_opaque_ids() {
        let api_key = crate::context::AuthContext::new_api_key(
            test_user(),
            Some(Role::User),
            Some(vec![Permission::ProjectsRead]),
            "user-chosen key name".to_string(),
            99,
        );
        let principal = safe_principal(&api_key);
        assert_eq!(principal.user_id, Some(42));
        assert_eq!(principal.source, AuthSourceKind::ApiKey);
        assert_eq!(principal.credential_id, Some(99));

        let token = crate::context::AuthContext::new_deployment_token(
            7,
            None,
            None,
            123,
            "user-chosen deployment token name".to_string(),
            vec![DeploymentTokenPermission::AnalyticsRead],
        );
        let principal = safe_principal(&token);
        assert_eq!(principal.user_id, None);
        assert_eq!(principal.source, AuthSourceKind::DeploymentToken);
        assert_eq!(principal.credential_id, Some(123));
    }

    #[test]
    fn missing_matched_path_never_falls_back_to_raw_uri() {
        let request = Request::builder()
            .uri("/projects/secret-project?token=secret")
            .body(axum::body::Body::empty())
            .expect("test request should build");
        assert_eq!(matched_route_template(&request), "unmatched");
    }

    #[tokio::test]
    async fn matched_route_uses_axum_template_instead_of_concrete_uri() {
        #[derive(Clone)]
        struct CapturedRoute(String);

        async fn capture_route(req: Request, next: Next) -> Response {
            let route = matched_route_template(&req);
            let mut response = next.run(req).await;
            response.extensions_mut().insert(CapturedRoute(route));
            response
        }

        let app = Router::new()
            .route("/projects/{project_id}", get(|| async { "ok" }))
            .layer(from_fn(capture_route));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/projects/secret-project?token=secret")
                    .body(axum::body::Body::empty())
                    .expect("test request should build"),
            )
            .await
            .expect("test router should respond");

        assert_eq!(
            response
                .extensions()
                .get::<CapturedRoute>()
                .expect("capture middleware should attach route")
                .0,
            "/projects/{project_id}"
        );
    }

    /// The console's CSRF boundary, driven through the real routes behind
    /// this middleware. `/logout` stands in for the ~150 session-reachable
    /// writes: the decision is made here, before routing, so it does not
    /// depend on the handler, its body or its content type.
    /// A credential the database could not be asked about is a 503, not the
    /// 401 a client reads as "your key is wrong".
    mod database_unavailable {
        use super::super::{
            classify_api_key_lookup, classify_deployment_token_lookup, classify_session_lookup,
            CredentialLookup, LookupTimedOut, CREDENTIAL_CHECK_UNAVAILABLE_PROBLEM_TYPE,
        };
        use crate::http_test_support::{auth_state, router};
        use axum::http::StatusCode;
        use sea_orm::{DatabaseBackend, DbErr, MockDatabase, RuntimeErr};
        use std::sync::Arc;

        fn unreachable_db() -> Arc<sea_orm::DatabaseConnection> {
            let error = || DbErr::Conn(RuntimeErr::Internal("connection refused".to_string()));
            Arc::new(
                MockDatabase::new(DatabaseBackend::Postgres)
                    .append_query_errors(vec![error(), error(), error(), error()])
                    .into_connection(),
            )
        }

        fn empty_db() -> Arc<sea_orm::DatabaseConnection> {
            Arc::new(
                MockDatabase::new(DatabaseBackend::Postgres)
                    .append_query_results(vec![Vec::<temps_entities::api_keys::Model>::new()])
                    .append_query_results(vec![
                        Vec::<temps_entities::deployment_tokens::Model>::new(),
                    ])
                    .append_query_results(vec![Vec::<temps_entities::sessions::Model>::new()])
                    .into_connection(),
            )
        }

        async fn get_me(
            db: Arc<sea_orm::DatabaseConnection>,
            header: (&str, &str),
        ) -> (StatusCode, serde_json::Value, Option<String>) {
            let app = router(auth_state(db.clone()), db);
            let request = axum::http::Request::builder()
                .method("GET")
                .uri("/user/me")
                .header(header.0, header.1)
                .body(axum::body::Body::empty())
                .expect("request");
            let response = tower::ServiceExt::oneshot(app, request)
                .await
                .expect("router answers");
            let status = response.status();
            let retry_after = response
                .headers()
                .get(axum::http::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string);
            let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
                .await
                .expect("body");
            let body = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
            (status, body, retry_after)
        }

        fn assert_unavailable(
            status: StatusCode,
            body: &serde_json::Value,
            retry_after: Option<String>,
            credential: &str,
        ) {
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
            assert_eq!(body["type"], CREDENTIAL_CHECK_UNAVAILABLE_PROBLEM_TYPE);
            let detail = body["detail"].as_str().expect("detail");
            assert!(detail.contains(credential), "{detail}");
            assert!(
                !detail.contains("connection refused"),
                "the database error stays in the log: {detail}"
            );
            assert_eq!(retry_after.as_deref(), Some("5"));
        }

        #[tokio::test]
        async fn an_api_key_the_database_cannot_check_is_a_503_with_retry_after() {
            let (status, body, retry_after) = get_me(
                unreachable_db(),
                (
                    "authorization",
                    "Bearer tk_0123456789abcdef0123456789abcdef",
                ),
            )
            .await;
            assert_unavailable(status, &body, retry_after, "API key");
        }

        #[tokio::test]
        async fn a_deployment_token_the_database_cannot_check_is_a_503() {
            let (status, body, retry_after) = get_me(
                unreachable_db(),
                (
                    "authorization",
                    "Bearer dt_0123456789abcdef0123456789abcdef",
                ),
            )
            .await;
            assert_unavailable(status, &body, retry_after, "deployment token");
        }

        #[tokio::test]
        async fn a_session_the_database_cannot_check_is_a_503() {
            let db = unreachable_db();
            let state = auth_state(db.clone());
            let sealed = state
                .cookie_crypto
                .encrypt("some-session-token")
                .expect("seal");
            let (status, body, retry_after) =
                get_me(db, ("cookie", &format!("session={sealed}"))).await;
            assert_unavailable(status, &body, retry_after, "session");
        }

        #[tokio::test]
        async fn an_unknown_api_key_is_still_a_401() {
            let (status, _, retry_after) = get_me(
                empty_db(),
                (
                    "authorization",
                    "Bearer tk_0123456789abcdef0123456789abcdef",
                ),
            )
            .await;
            assert_eq!(status, StatusCode::UNAUTHORIZED);
            assert_eq!(retry_after, None);
        }

        #[test]
        fn refused_credentials_are_rejected_not_unavailable() {
            use crate::apikey_service::ApiKeyServiceError;
            use crate::auth_service::AuthError;
            use crate::deployment_token_service::DeploymentTokenValidationError;

            assert!(matches!(
                classify_api_key_lookup::<()>(Ok(Err(ApiKeyServiceError::Unauthorized(
                    "API key has expired".into()
                )))),
                CredentialLookup::Rejected
            ));
            assert!(matches!(
                classify_deployment_token_lookup::<()>(Ok(Err(
                    DeploymentTokenValidationError::TokenInactive
                ))),
                CredentialLookup::Rejected
            ));
            assert!(matches!(
                classify_session_lookup::<()>(Ok(Err(AuthError::NotFound(
                    "Session not found or expired".into()
                )))),
                CredentialLookup::Rejected
            ));
        }

        #[test]
        fn a_lookup_that_timed_out_is_unavailable() {
            assert!(matches!(
                classify_api_key_lookup::<()>(Err(LookupTimedOut)),
                CredentialLookup::Unavailable(_)
            ));
            assert!(matches!(
                classify_session_lookup::<()>(Err(LookupTimedOut)),
                CredentialLookup::Unavailable(_)
            ));
            assert!(matches!(
                classify_session_lookup::<()>(Ok(Err(
                    crate::auth_service::AuthError::DatabaseError {
                        reason: "pool timed out".into()
                    }
                ))),
                CredentialLookup::Unavailable(_)
            ));
        }
    }

    mod same_origin_http {
        use crate::apikey_service::CreateApiKeyRequest;
        use crate::http_test_support::{
            auth_state, insert_user, router, send_with_headers, session_cookie,
        };
        use axum::http::StatusCode;
        use temps_test_utils::TestDatabase;

        async fn test_db() -> Option<TestDatabase> {
            match TestDatabase::with_migrations().await {
                Ok(db) => Some(db),
                Err(error)
                    if temps_test_utils::is_container_runtime_unavailable(&error.to_string()) =>
                {
                    println!("Docker not available, skipping test: {error}");
                    None
                }
                Err(error) => panic!("test database setup failed: {error}"),
            }
        }

        /// `Cookie` header for a fresh session, and the router to send it to.
        async fn session(db: &TestDatabase) -> (axum::Router, String) {
            let connection = db.connection_arc();
            let state = auth_state(connection.clone());
            let user = insert_user(
                connection.as_ref(),
                &format!("origin-{}@example.com", uuid::Uuid::new_v4()),
            )
            .await;
            let cookie = session_cookie(&state, user.id).await;
            (router(state, connection), cookie)
        }

        #[tokio::test]
        async fn a_same_site_sibling_cannot_post_with_the_session_cookie() {
            let Some(db) = test_db().await else { return };
            let (router, cookie) = session(&db).await;

            // A page on a sibling subdomain of the console's base domain is
            // same-site, so the browser attaches the session cookie — and
            // says so in a header it cannot forge.
            let (status, body) = send_with_headers(
                &router,
                "POST",
                "/logout",
                &[
                    ("cookie", &cookie),
                    ("host", "console.example"),
                    ("sec-fetch-site", "same-site"),
                    ("origin", "https://app.console.example"),
                ],
                None,
            )
            .await;

            assert_eq!(status, StatusCode::FORBIDDEN);
            assert_eq!(body["title"], "Cross-Origin Request Refused");
        }

        #[tokio::test]
        async fn a_foreign_origin_cannot_post_with_the_session_cookie() {
            let Some(db) = test_db().await else { return };
            let (router, cookie) = session(&db).await;

            // No Fetch Metadata (an older browser): the `Origin` decides.
            let (status, _) = send_with_headers(
                &router,
                "POST",
                "/logout",
                &[
                    ("cookie", &cookie),
                    ("host", "console.example"),
                    ("origin", "https://attacker.example"),
                ],
                None,
            )
            .await;

            assert_eq!(status, StatusCode::FORBIDDEN);
        }

        #[tokio::test]
        async fn a_multipart_post_is_refused_without_reaching_the_handler() {
            let Some(db) = test_db().await else { return };
            let (router, cookie) = session(&db).await;

            // `multipart/form-data` is a "simple" content type: a hostile
            // page can send it with no preflight to ask permission for.
            let (status, _) = send_with_headers(
                &router,
                "POST",
                "/logout",
                &[
                    ("cookie", &cookie),
                    ("host", "console.example"),
                    ("sec-fetch-site", "same-site"),
                    ("content-type", "multipart/form-data; boundary=x"),
                ],
                None,
            )
            .await;

            assert_eq!(status, StatusCode::FORBIDDEN);
        }

        #[tokio::test]
        async fn the_console_itself_still_posts() {
            let Some(db) = test_db().await else { return };
            let (router, cookie) = session(&db).await;

            let (status, _) = send_with_headers(
                &router,
                "POST",
                "/logout",
                &[
                    ("cookie", &cookie),
                    ("host", "console.example"),
                    ("sec-fetch-site", "same-origin"),
                    ("origin", "https://console.example"),
                ],
                None,
            )
            .await;

            assert_eq!(status, StatusCode::OK);
        }

        #[tokio::test]
        async fn a_matching_origin_without_fetch_metadata_still_posts() {
            let Some(db) = test_db().await else { return };
            let (router, cookie) = session(&db).await;

            // TLS terminates in front of Temps, so the scheme the server
            // sees is not the browser's; only the authority is compared.
            let (status, _) = send_with_headers(
                &router,
                "POST",
                "/logout",
                &[
                    ("cookie", &cookie),
                    ("host", "console.example:8443"),
                    ("origin", "https://console.example:8443"),
                ],
                None,
            )
            .await;

            assert_eq!(status, StatusCode::OK);
        }

        #[tokio::test]
        async fn a_get_is_unaffected_by_a_foreign_origin() {
            let Some(db) = test_db().await else { return };
            let (router, cookie) = session(&db).await;

            let (status, body) = send_with_headers(
                &router,
                "GET",
                "/user/me",
                &[
                    ("cookie", &cookie),
                    ("host", "console.example"),
                    ("sec-fetch-site", "cross-site"),
                    ("origin", "https://attacker.example"),
                ],
                None,
            )
            .await;

            assert_eq!(status, StatusCode::OK);
            assert!(body["email"].is_string());
        }

        #[tokio::test]
        async fn a_bearer_token_post_needs_no_origin_headers() {
            let Some(db) = test_db().await else { return };
            let connection = db.connection_arc();
            let state = auth_state(connection.clone());
            let user = insert_user(connection.as_ref(), "token-caller@example.com").await;
            let created = state
                .api_key_service
                .create_api_key(
                    user.id,
                    CreateApiKeyRequest {
                        project_id: None,
                        environment_id: None,
                        name: "cli".to_string(),
                        role_type: "admin".to_string(),
                        permissions: None,
                        expires_at: None,
                    },
                )
                .await
                .expect("create api key");
            let router = router(state, connection);

            // No `Origin`, no `Sec-Fetch-Site`, no cookie: a browser cannot
            // attach this credential, so the check never applies.
            let (status, _) = send_with_headers(
                &router,
                "POST",
                "/logout",
                &[("authorization", &format!("Bearer {}", created.api_key))],
                None,
            )
            .await;

            assert_eq!(status, StatusCode::OK);
        }

        #[tokio::test]
        async fn a_non_browser_cookie_client_with_no_origin_still_posts() {
            let Some(db) = test_db().await else { return };
            let (router, cookie) = session(&db).await;

            // The acceptance suite and the in-process replay path both log
            // in with a cookie and send neither header; browsers always
            // attach `Origin` to a POST, so there is nothing to refuse here.
            let (status, _) =
                send_with_headers(&router, "POST", "/logout", &[("cookie", &cookie)], None).await;

            assert_eq!(status, StatusCode::OK);
        }
    }

    /// Which session cookie authenticates when a browser holds more than
    /// one, driven through the real routes behind this middleware.
    mod session_cookie_name {
        use crate::http_test_support::{
            auth_state, host_session_cookie, insert_user, router, secure_router, send_with_headers,
            session_cookie,
        };
        use axum::extract::Request;
        use axum::http::StatusCode;
        use temps_test_utils::TestDatabase;
        use tower::ServiceExt;

        async fn test_db() -> Option<TestDatabase> {
            match TestDatabase::with_migrations().await {
                Ok(db) => Some(db),
                Err(error)
                    if temps_test_utils::is_container_runtime_unavailable(&error.to_string()) =>
                {
                    println!("Docker not available, skipping test: {error}");
                    None
                }
                Err(error) => panic!("test database setup failed: {error}"),
            }
        }

        #[tokio::test]
        async fn the_host_only_cookie_wins_over_a_planted_one() {
            let Some(db) = test_db().await else { return };
            let connection = db.connection_arc();
            let state = auth_state(connection.clone());

            // The attacker owns an account and a sibling app, and plants
            // their own sealed session on the console's parent domain. The
            // browser sends both cookies; only the host-only one is theirs
            // to set, and that is the one that must count.
            let attacker = insert_user(connection.as_ref(), "planted@example.com").await;
            let owner = insert_user(connection.as_ref(), "owner@example.com").await;
            let planted = session_cookie(&state, attacker.id).await;
            let host_only = host_session_cookie(&state, owner.id).await;
            let router = router(state, connection);

            for cookie in [
                format!("{planted}; {host_only}"),
                format!("{host_only}; {planted}"),
            ] {
                let (status, body) =
                    send_with_headers(&router, "GET", "/user/me", &[("cookie", &cookie)], None)
                        .await;

                assert_eq!(status, StatusCode::OK, "{cookie}");
                assert_eq!(body["email"], "owner@example.com", "{cookie}");
            }
        }

        /// Signing out removes `__Host-session`, but a `Set-Cookie` without
        /// `Domain` cannot remove a cookie that was set with one. So on an
        /// https console the unprefixed name is not read at all: otherwise
        /// the request after sign-out would authenticate as whoever planted
        /// it.
        #[tokio::test]
        async fn an_https_console_never_authenticates_the_unprefixed_cookie() {
            let Some(db) = test_db().await else { return };
            let connection = db.connection_arc();
            let state = auth_state(connection.clone());
            let attacker = insert_user(connection.as_ref(), "planted-https@example.com").await;
            let owner = insert_user(connection.as_ref(), "owner-https@example.com").await;
            let planted = session_cookie(&state, attacker.id).await;
            let host_only = host_session_cookie(&state, owner.id).await;
            let router = secure_router(state, connection);

            // Alone -- the state the browser is in right after sign-out.
            let (status, _) =
                send_with_headers(&router, "GET", "/user/me", &[("cookie", &planted)], None).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED);

            // And alongside the real one, it still counts for nothing.
            let both = format!("{planted}; {host_only}");
            let (status, body) =
                send_with_headers(&router, "GET", "/user/me", &[("cookie", &both)], None).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body["email"], "owner-https@example.com");
        }

        #[tokio::test]
        async fn a_session_issued_before_the_prefix_still_authenticates() {
            let Some(db) = test_db().await else { return };
            let connection = db.connection_arc();
            let state = auth_state(connection.clone());
            let user = insert_user(connection.as_ref(), "upgraded@example.com").await;
            let cookie = session_cookie(&state, user.id).await;
            let router = router(state, connection);

            // The unprefixed name is the only one a plain-HTTP install can
            // issue, which is the sole reason this path still reads it.
            let (status, body) =
                send_with_headers(&router, "GET", "/user/me", &[("cookie", &cookie)], None).await;

            assert_eq!(status, StatusCode::OK);
            assert_eq!(body["email"], "upgraded@example.com");
        }

        #[tokio::test]
        async fn signing_out_clears_the_cookie_under_both_names() {
            let Some(db) = test_db().await else { return };
            let connection = db.connection_arc();
            let state = auth_state(connection.clone());
            let user = insert_user(connection.as_ref(), "signing-out@example.com").await;
            let cookie = host_session_cookie(&state, user.id).await;
            let router = crate::http_test_support::router(state, connection);

            let response = router
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/logout")
                        .header("cookie", &cookie)
                        .body(axum::body::Body::empty())
                        .expect("request builds"),
                )
                .await
                .expect("route");

            assert_eq!(response.status(), StatusCode::OK);
            let cleared: Vec<&str> = response
                .headers()
                .get_all(axum::http::header::SET_COOKIE)
                .iter()
                .filter_map(|value| value.to_str().ok())
                .collect();
            // Leaving either name set is how a logout loop starts: the
            // browser keeps sending a cookie the server has revoked.
            assert!(
                cleared
                    .iter()
                    .any(|value| value.starts_with("__Host-session=;")),
                "{cleared:?}"
            );
            assert!(
                cleared.iter().any(|value| value.starts_with("session=;")),
                "{cleared:?}"
            );
        }
    }
}
