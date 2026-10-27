// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The console's half of the Temps gate (ADR 065 D8).
//!
//! - `GET /_temps/gate/authorize` (mounted at the console's root by the
//!   server, not under `/api`): a signed-in console user is sent back to the
//!   address with a single-use code. See [`authorize_response`].
//! - `POST /gate/sign-out-of-apps`: "Sign out of apps", every gate session
//!   of the current user.
//! - `POST /projects/{project_id}/gate/addresses/{address_id}/sign-everyone-out`:
//!   refuse every token issued for one address so far.

use std::sync::Arc;

use axum::{
    extract::{Extension, Path, State},
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use serde::Serialize;
use temps_auth::{project_permission_guard, AuthContext, RequireAuth};
use temps_core::gate;
use temps_core::problemdetails::{self, Problem};
use temps_core::{
    AuditContext, AuditLogger, AuditOperation, ProjectAccessChecker, RequestMetadata,
};
use temps_routes::gate::{AuthorizeRequest, GateService, GateServiceError};
use tracing::{error, info};
use utoipa::{OpenApi, ToSchema};

pub struct GateConsoleState {
    pub gate: Arc<GateService>,
    pub audit_service: Arc<dyn AuditLogger>,
    pub project_access_checker: Option<Arc<dyn ProjectAccessChecker>>,
}

#[derive(OpenApi)]
#[openapi(
    paths(sign_out_of_apps, sign_everyone_out),
    components(schemas(SignOutOfAppsResponse)),
    tags((name = "Signed-in addresses", description = "The Temps sign-in in front of signed-in addresses (ADR 065)"))
)]
pub struct GateApiDoc;

pub fn configure_routes() -> Router<Arc<GateConsoleState>> {
    Router::new()
        .route("/gate/sign-out-of-apps", post(sign_out_of_apps))
        .route(
            "/projects/{project_id}/gate/addresses/{address_id}/sign-everyone-out",
            post(sign_everyone_out),
        )
}

/// The console's answer for a sign-in service error. A function rather than
/// a `From` impl: both types live in other crates.
fn gate_problem(error: GateServiceError) -> Problem {
    let (status, title) = match &error {
        GateServiceError::InvalidCode { .. }
        | GateServiceError::SessionEnded { .. }
        | GateServiceError::InvalidToken { .. }
        | GateServiceError::Validation { .. }
        | GateServiceError::BadAddressId { .. } => {
            (StatusCode::BAD_REQUEST, "Invalid Sign-in Request")
        }
        GateServiceError::NotAllowed { .. } => (StatusCode::FORBIDDEN, "Sign-in Not Allowed"),
        GateServiceError::AddressNotFound { .. } => (StatusCode::NOT_FOUND, "Address Not Found"),
        GateServiceError::NotGated { .. } => (StatusCode::CONFLICT, "Address Not Signed In"),
        GateServiceError::Database { .. }
        | GateServiceError::Key { .. }
        | GateServiceError::Gate(_) => (StatusCode::INTERNAL_SERVER_ERROR, "Sign-in Error"),
    };
    problemdetails::new(status)
        .with_title(title)
        .with_detail(error.to_string())
}

#[derive(Debug, Serialize)]
struct GateAudit {
    #[serde(skip)]
    action: &'static str,
    context: AuditContext,
    address_id: Option<String>,
    project_id: Option<i32>,
    /// Sessions revoked, for "Sign out of apps".
    revoked: Option<u64>,
}

impl AuditOperation for GateAudit {
    fn operation_type(&self) -> String {
        self.action.to_string()
    }
    fn user_id(&self) -> Option<i32> {
        Some(self.context.user_id)
    }
    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }
    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }
    fn serialize(&self) -> anyhow::Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation: {e}"))
    }
}

async fn audit(state: &GateConsoleState, record: GateAudit) {
    if let Err(error) = state.audit_service.create_audit_log(&record).await {
        error!(%error, action = record.action, "gate change made but audit record failed");
    }
}

fn audit_context(auth: &AuthContext, metadata: Option<&RequestMetadata>) -> AuditContext {
    AuditContext {
        user_id: auth.user_id(),
        ip_address: metadata.map(|m| m.ip_address.clone()),
        user_agent: metadata.map(|m| m.user_agent.clone()).unwrap_or_default(),
    }
}

/// An HTML answer from the authorize endpoint: a person reads it.
fn page(status: StatusCode, title: &str, message: &str) -> Response {
    let mut response = (status, gate::page(title, message)).into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// `GET /_temps/gate/authorize?address=…&state=…&code_challenge=…&code_challenge_method=S256`
/// for an authenticated request (the server serves the console's sign-in
/// page to anyone else, which returns here after signing in).
///
/// Requires a full console session: an API key, CLI token or deployment
/// token is refused, and a session still waiting for MFA never gets an
/// `AuthContext` at all. The redirect goes to the address's own hostname,
/// looked up from `address`; nothing in the request names where to go.
pub async fn authorize_response(
    state: &GateConsoleState,
    auth: &AuthContext,
    metadata: Option<&RequestMetadata>,
    query: Option<&str>,
) -> Response {
    let (Some(user_id), Some(console_session_id)) = (auth.user_id_opt(), auth.session_id()) else {
        return page(
            StatusCode::FORBIDDEN,
            "Sign-in needs the console",
            "Sign in to the Temps console in this browser, then open the address again.",
        );
    };
    if !auth.is_session() {
        return page(
            StatusCode::FORBIDDEN,
            "Sign-in needs the console",
            "Sign in to the Temps console in this browser, then open the address again.",
        );
    }
    let (Some(address_id), Some(state_value), Some(challenge)) = (
        gate::query_param(query, "address"),
        gate::query_param(query, "state"),
        gate::query_param(query, "code_challenge"),
    ) else {
        return page(
            StatusCode::BAD_REQUEST,
            "Sign-in failed",
            "This sign-in link is incomplete. Open the address again to sign in.",
        );
    };
    if gate::query_param(query, "code_challenge_method").as_deref() != Some("S256") {
        return page(
            StatusCode::BAD_REQUEST,
            "Sign-in failed",
            "This sign-in link is not from a Temps address. Open the address again to sign in.",
        );
    }
    let request = AuthorizeRequest {
        address_id: &address_id,
        user_id,
        console_session_id,
        is_admin: auth.is_admin(),
        state: &state_value,
        code_challenge: &challenge,
    };
    match state
        .gate
        .authorize(&request, state.project_access_checker.as_deref())
        .await
    {
        Ok(location) => {
            info!(user_id, address_id = %address_id, "sign-in to a gated address authorized");
            audit(
                state,
                GateAudit {
                    action: "GATE_SIGN_IN_AUTHORIZED",
                    context: audit_context(auth, metadata),
                    address_id: Some(address_id.clone()),
                    project_id: None,
                    revoked: None,
                },
            )
            .await;
            let mut response = StatusCode::FOUND.into_response();
            let headers = response.headers_mut();
            match HeaderValue::from_str(&location) {
                Ok(value) => {
                    headers.insert(header::LOCATION, value);
                }
                Err(_) => {
                    error!(address_id = %address_id, "callback location is not a valid header");
                    return page(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Sign-in failed",
                        "Sign-in failed on the control plane. Try again in a moment.",
                    );
                }
            }
            headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            // The code is in the URL; keep it out of the next page's Referer.
            headers.insert(
                header::REFERRER_POLICY,
                HeaderValue::from_static("no-referrer"),
            );
            response
        }
        Err(error) => {
            let status = StatusCode::from_u16(error.http_status())
                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            if status.is_server_error() {
                error!(user_id, address_id = %address_id, %error, "gate authorize failed");
            } else {
                info!(user_id, address_id = %address_id, %error, "gate authorize refused");
            }
            page(status, "Sign-in failed", &error.visitor_message())
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SignOutOfAppsResponse {
    /// Gate sessions revoked.
    pub revoked: u64,
}

/// Sign out of every signed-in address
///
/// Revokes all of the current user's gate sessions. Listeners refuse them
/// as soon as they take the next route snapshot.
#[utoipa::path(
    tag = "Signed-in addresses",
    post,
    path = "/gate/sign-out-of-apps",
    operation_id = "sign_out_of_apps",
    responses(
        (status = 200, body = SignOutOfAppsResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Not a user"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
async fn sign_out_of_apps(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<GateConsoleState>>,
    metadata: Option<Extension<RequestMetadata>>,
) -> Result<impl IntoResponse, Problem> {
    let Some(user_id) = auth.user_id_opt() else {
        return Err(problemdetails::new(StatusCode::FORBIDDEN)
            .with_title("Not a User")
            .with_detail("Only a user can sign out of signed-in addresses"));
    };
    let revoked = state
        .gate
        .revoke_user(user_id)
        .await
        .map_err(gate_problem)?;
    audit(
        &state,
        GateAudit {
            action: "GATE_SIGN_OUT_OF_APPS",
            context: audit_context(&auth, metadata.as_ref().map(|m| &m.0)),
            address_id: None,
            project_id: None,
            revoked: Some(revoked),
        },
    )
    .await;
    Ok(Json(SignOutOfAppsResponse { revoked }))
}

/// Sign everyone out of a signed-in address
///
/// Every gate token issued for the address so far is refused; people sign
/// in again (and are checked again) on their next visit.
#[utoipa::path(
    tag = "Signed-in addresses",
    post,
    path = "/projects/{project_id}/gate/addresses/{address_id}/sign-everyone-out",
    operation_id = "sign_everyone_out_of_address",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("address_id" = String, Path, description = "`custom:<id>` or `environment:<id>`")
    ),
    responses(
        (status = 204, description = "Everyone signed out"),
        (status = 400, description = "Malformed address id"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Address not found in this project"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
async fn sign_everyone_out(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<GateConsoleState>>,
    metadata: Option<Extension<RequestMetadata>>,
    Path((project_id, address_id)): Path<(i32, String)>,
) -> Result<impl IntoResponse, Problem> {
    project_permission_guard!(
        auth,
        ProjectsWrite,
        project_id,
        state.project_access_checker
    );
    let address = state
        .gate
        .address(&address_id)
        .await
        .map_err(gate_problem)?;
    if address.project_id != project_id {
        return Err(gate_problem(GateServiceError::AddressNotFound {
            address_id,
        }));
    }
    state
        .gate
        .sign_everyone_out(&address_id)
        .await
        .map_err(gate_problem)?;
    audit(
        &state,
        GateAudit {
            action: "GATE_SIGN_EVERYONE_OUT",
            context: audit_context(&auth, metadata.as_ref().map(|m| &m.0)),
            address_id: Some(address_id),
            project_id: Some(project_id),
            revoked: None,
        },
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_auth::permissions::Role;

    struct NoAudit;
    #[async_trait::async_trait]
    impl AuditLogger for NoAudit {
        async fn create_audit_log(&self, _: &dyn AuditOperation) -> anyhow::Result<()> {
            Ok(())
        }
    }

    fn state() -> GateConsoleState {
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection();
        GateConsoleState {
            gate: Arc::new(GateService::new(
                Arc::new(db),
                Arc::new(temps_core::EncryptionService::new_from_password(
                    "gate-test",
                )),
            )),
            audit_service: Arc::new(NoAudit),
            project_access_checker: None,
        }
    }

    fn user() -> temps_entities::users::Model {
        let now = chrono::Utc::now();
        temps_entities::users::Model {
            id: 5,
            name: "Member".into(),
            email: "member@example.test".into(),
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

    #[tokio::test]
    async fn api_keys_and_unpersisted_sessions_cannot_authorize() {
        let state = state();
        let query = Some("address=custom%3A1&state=s&code_challenge=c&code_challenge_method=S256");
        let cli = AuthContext::new_cli_token(user(), Role::User);
        assert_eq!(
            authorize_response(&state, &cli, None, query).await.status(),
            StatusCode::FORBIDDEN
        );
        let unpersisted = AuthContext::new_session(user(), Role::User);
        assert_eq!(
            authorize_response(&state, &unpersisted, None, query)
                .await
                .status(),
            StatusCode::FORBIDDEN
        );
    }

    #[tokio::test]
    async fn incomplete_or_non_pkce_requests_are_refused_before_any_lookup() {
        // The mock database has nothing queued, so any lookup would be a 500.
        let state = state();
        let auth = AuthContext::new_persisted_session(user(), Role::User, 77);
        for query in [
            None,
            Some("address=custom%3A1&state=s"),
            Some("address=custom%3A1&state=s&code_challenge=c"),
            Some("address=custom%3A1&state=s&code_challenge=c&code_challenge_method=plain"),
            // Duplicated parameters are refused rather than picked from.
            Some("address=custom%3A1&address=custom%3A2&state=s&code_challenge=c&code_challenge_method=S256"),
        ] {
            assert_eq!(
                authorize_response(&state, &auth, None, query).await.status(),
                StatusCode::BAD_REQUEST,
                "{query:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_malformed_state_is_refused_without_a_redirect() {
        let state = state();
        let auth = AuthContext::new_persisted_session(user(), Role::User, 77);
        let challenge = gate::code_challenge(&gate::new_verifier().unwrap());
        let query = format!(
            "address=custom%3A1&state=short&code_challenge={challenge}&code_challenge_method=S256"
        );
        let response = authorize_response(&state, &auth, None, Some(&query)).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(response.headers().get(header::LOCATION).is_none());
    }

    #[test]
    fn service_errors_map_to_problem_statuses() {
        let problem = gate_problem(GateServiceError::NotAllowed {
            address_id: "custom:1".into(),
            user_id: 5,
            reason: "no access",
        });
        assert_eq!(problem.status_code, StatusCode::FORBIDDEN);
        let problem = gate_problem(GateServiceError::AddressNotFound {
            address_id: "custom:1".into(),
        });
        assert_eq!(problem.status_code, StatusCode::NOT_FOUND);
    }
}
