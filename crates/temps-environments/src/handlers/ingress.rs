// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Ingress settings and `TempsLogin` sign-in (ADR-055 §11).
//!
//! - `GET/PUT /projects/{project_id}/environments/{env_id}/ingress`: which
//!   evaluator decides the environment's requests at the proxy, and whether
//!   visitors must sign in to Temps.
//! - `POST /ingress/login-grants`: called by the console's `/ingress/login`
//!   page, which the proxy sends a visitor to when an environment, or a
//!   sandbox preview in `temps_login` access, requires a Temps login. Checks
//!   the signed request and the user's permission (for a sandbox: being its
//!   owner), records a grant and answers with the URL that hands an exchange
//!   grant back to the host.

use std::sync::Arc;

use axum::extract::{Extension, Path, State};
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use temps_auth::{project_permission_guard, project_scope_guard, RequireAuth};
use temps_core::problemdetails::Problem;
use temps_core::{AuditContext, AuditLogger, CookieCrypto, RequestMetadata};
use temps_entities::ingress_mode::{IngressMode, SandboxPreviewAccess};
use temps_workload_ingress::grant::{self, GrantClaims, GrantKey, GrantUse, LoginScheme};
use temps_workload_ingress::{ProductKind, ProductRef};
use tracing::{error, info};
use utoipa::{OpenApi, ToSchema};

use super::audit::{
    IngressLoginGrantIssuedAudit, IngressSettingsUpdatedAudit, SandboxPreviewLoginGrantIssuedAudit,
};
use crate::services::environment_service::EnvironmentService;
use crate::services::ingress_service::{
    IngressError, IngressService, IngressSettings, IngressSettingsUpdate, SandboxLoginTarget,
};

pub struct IngressAppState {
    pub ingress_service: Arc<IngressService>,
    pub environment_service: Arc<EnvironmentService>,
    pub cookie_crypto: Arc<CookieCrypto>,
    pub audit_service: Arc<dyn AuditLogger>,
    pub project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
}

impl From<IngressError> for Problem {
    fn from(error: IngressError) -> Self {
        match error {
            IngressError::EnvironmentNotFound { .. }
            | IngressError::EnvironmentIdNotFound { .. } => temps_core::error_builder::not_found()
                .title("Environment Not Found")
                .detail(error.to_string())
                .build(),
            IngressError::NotLoginProtected { .. } => temps_core::error_builder::conflict()
                .title("Temps Login Not Required")
                .detail(error.to_string())
                .build(),
            IngressError::EndpointMissing { .. } | IngressError::Database { .. } => {
                temps_core::error_builder::internal_server_error()
                    .title("Ingress Settings Unavailable")
                    .detail(error.to_string())
                    .build()
            }
            IngressError::SandboxNotFound { .. } => temps_core::error_builder::not_found()
                .title("Sandbox Not Found")
                .detail(error.to_string())
                .build(),
            IngressError::SandboxNotLoginProtected { .. } => temps_core::error_builder::conflict()
                .title("Temps Login Not Required")
                .detail(error.to_string())
                .build(),
            IngressError::SandboxDatabase { .. } => {
                temps_core::error_builder::internal_server_error()
                    .title("Sandbox Preview Sign-In Unavailable")
                    .detail(error.to_string())
                    .build()
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct IngressSettingsResponse {
    pub project_id: i32,
    pub environment_id: i32,
    /// `legacy`: the existing access chain decides. `shadow`: it still
    /// decides, and the compiled endpoint is evaluated alongside and every
    /// disagreement counted (`proxy.ingress_shadow_mismatches`). `enforce`:
    /// the compiled endpoint decides.
    pub mode: IngressMode,
    /// Whether visitors must sign in to Temps.
    pub require_login: bool,
    /// The console permission a visitor needs, when a login is required.
    pub login_permission: Option<String>,
    /// Whether the proxy asks for the login right now: only in `enforce`.
    pub login_enforced: bool,
}

impl From<IngressSettings> for IngressSettingsResponse {
    fn from(settings: IngressSettings) -> Self {
        Self {
            project_id: settings.project_id,
            environment_id: settings.environment_id,
            mode: settings.mode,
            require_login: settings.login_permission.is_some(),
            login_enforced: settings.login_enforced(),
            login_permission: settings.login_permission,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct UpdateIngressSettingsRequest {
    /// Leave out to keep the current mode.
    pub mode: Option<IngressMode>,
    /// Leave out to keep the current setting.
    pub require_login: Option<bool>,
}

/// The proxy's signed login request, as the `/ingress/login` page received
/// it in its query string.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CreateIngressLoginGrantRequest {
    /// `environment` or `sandbox` (a sandbox preview host).
    pub owner_kind: String,
    pub owner_id: i64,
    /// `http` or `https`: how the visitor reached the host.
    pub scheme: String,
    pub host: String,
    /// Path on the host to return to.
    pub next: Option<String>,
    /// Unix seconds the proxy signed the request at.
    pub ts: u64,
    pub sig: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct IngressLoginGrantResponse {
    /// Where the browser goes next: the host's exchange path with a
    /// short-lived grant. Contains a credential; do not store it.
    pub redirect_url: String,
    pub grant_id: i64,
    /// When the session this grant can start expires.
    /// ISO 8601, UTC.
    pub expires_at: String,
}

fn now_secs() -> u64 {
    u64::try_from(Utc::now().timestamp()).unwrap_or(0)
}

/// A host the grant can name: lowercase DNS characters only, no port.
fn grant_host(host: &str) -> Option<String> {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    let valid = !host.is_empty()
        && host.len() <= 253
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.');
    valid.then_some(host)
}

/// `next` when it is a path on the host, `/` otherwise.
fn safe_next(next: Option<&str>) -> &str {
    match next {
        Some(next)
            if next.starts_with('/')
                && !next.starts_with("//")
                && next.len() <= 2048
                && !next.bytes().any(|b| b.is_ascii_control() || b == b'\\') =>
        {
            next
        }
        _ => "/",
    }
}

/// A login request that passed [`verify_login_request`].
#[derive(Debug, Clone, PartialEq, Eq)]
struct VerifiedLoginRequest {
    kind: ProductKind,
    /// The environment or sandbox id (internal, not the public id).
    owner_id: i32,
    scheme: LoginScheme,
    host: String,
}

impl VerifiedLoginRequest {
    fn owner(&self) -> ProductRef {
        ProductRef {
            kind: self.kind,
            id: i64::from(self.owner_id),
        }
    }
}

/// The checks that need no database: the owner, the scheme, the host and
/// the proxy's signature.
fn verify_login_request(
    master: &[u8; 32],
    request: &CreateIngressLoginGrantRequest,
    now: u64,
) -> Result<VerifiedLoginRequest, Problem> {
    let kind = match ProductKind::parse(&request.owner_kind) {
        Some(kind @ (ProductKind::Environment | ProductKind::Sandbox)) => kind,
        Some(ProductKind::Database | ProductKind::Function) | None => {
            return Err(temps_core::error_builder::bad_request()
                .title("Unsupported Login Owner")
                .detail(format!(
                    "Temps login grants are issued for environments and sandbox previews only, not '{}'",
                    request.owner_kind
                ))
                .build());
        }
    };
    let what = kind.as_str();
    let owner_id = i32::try_from(request.owner_id).map_err(|_| {
        temps_core::error_builder::bad_request()
            .title("Invalid Login Request")
            .detail(format!("{what} id {} is out of range", request.owner_id))
            .build()
    })?;
    let scheme = LoginScheme::parse(&request.scheme).ok_or_else(|| {
        temps_core::error_builder::bad_request()
            .title("Invalid Login Request")
            .detail(format!(
                "Scheme '{}' for {what} {owner_id} must be http or https",
                request.scheme
            ))
            .build()
    })?;
    let host = grant_host(&request.host).ok_or_else(|| {
        temps_core::error_builder::bad_request()
            .title("Invalid Login Request")
            .detail(format!(
                "Host '{}' for {what} {owner_id} is not a hostname",
                request.host
            ))
            .build()
    })?;
    let verified = VerifiedLoginRequest {
        kind,
        owner_id,
        scheme,
        host,
    };
    if !grant::login_request_valid(
        &GrantKey::derive(master, verified.owner()),
        scheme,
        &verified.host,
        request.ts,
        &request.sig,
        now,
    ) {
        return Err(temps_core::error_builder::forbidden()
            .title("Sign-In Link Invalid Or Expired")
            .detail(format!(
                "The sign-in request for {} ({what} {owner_id}) is not signed by this instance or is older than {} minutes; open the page again to get a new one",
                verified.host,
                grant::LOGIN_REQUEST_TTL_SECS / 60
            ))
            .build());
    }
    Ok(verified)
}

fn redirect_url(
    scheme: LoginScheme,
    host: &str,
    token: &str,
    next: &str,
) -> Result<String, Problem> {
    let mut url = url::Url::parse(&format!(
        "{}://{host}{}",
        scheme.as_str(),
        grant::EXCHANGE_PATH
    ))
    .map_err(|e| {
        temps_core::error_builder::internal_server_error()
            .title("Sign-In Redirect Failed")
            .detail(format!("Cannot build the sign-in redirect to {host}: {e}"))
            .build()
    })?;
    url.query_pairs_mut()
        .append_pair("grant", token)
        .append_pair("next", next);
    Ok(url.into())
}

/// Whether the caller may open the environment: the login's permission and,
/// with team-based access, the project.
async fn may_open(
    state: &IngressAppState,
    auth: &temps_auth::AuthContext,
    permission: &str,
    project_id: i32,
) -> Result<(), Problem> {
    let denied = |detail: String| {
        temps_core::error_builder::forbidden()
            .title("Insufficient Permissions")
            .detail(detail)
            .build()
    };
    let required = temps_auth::Permission::from_str(permission).ok_or_else(|| {
        denied(format!(
            "Project {project_id} requires the unknown permission '{permission}' to sign in"
        ))
    })?;
    if !auth.has_permission(&required) {
        return Err(denied(format!(
            "Opening project {project_id} requires the {permission} permission"
        )));
    }
    if auth.is_admin() || auth.has_role(&temps_auth::Role::PlatformAdmin) {
        return Ok(());
    }
    if let Some(checker) = &state.project_access_checker {
        let allowed = checker
            .user_can_access_project(auth.user_id(), project_id)
            .await
            .unwrap_or(false);
        if !allowed {
            return Err(denied(format!(
                "You do not have access to project {project_id}"
            )));
        }
    }
    Ok(())
}

/// Whether the caller may open a sandbox's preview: the sandbox API's read
/// permission, being the sandbox's owner (every sandbox operation is
/// owner-only), and access to its project when it has one.
async fn may_open_sandbox(
    state: &IngressAppState,
    auth: &temps_auth::AuthContext,
    target: &SandboxLoginTarget,
) -> Result<(), Problem> {
    let denied = |detail: String| {
        temps_core::error_builder::forbidden()
            .title("Insufficient Permissions")
            .detail(detail)
            .build()
    };
    let public_id = &target.public_id;
    if !auth.has_permission(&temps_auth::Permission::SandboxesRead)
        && !auth.has_permission(&temps_auth::Permission::ProjectsRead)
    {
        return Err(denied(format!(
            "Opening the preview of sandbox {public_id} requires the {} permission",
            SandboxPreviewAccess::LOGIN_PERMISSION
        )));
    }
    if target.owner_user_id != Some(auth.user_id()) {
        return Err(denied(format!(
            "The preview of sandbox {public_id} is limited to its owner's Temps login, and you are signed in as another user. The owner can let others in with a preview password (PUT /v1/sandboxes/{public_id}/preview-password) or open it with PUT /v1/sandboxes/{public_id}/preview-access"
        )));
    }
    if let Some(project_id) = target.project_id {
        if auth.is_admin() || auth.has_role(&temps_auth::Role::PlatformAdmin) {
            return Ok(());
        }
        if let Some(checker) = &state.project_access_checker {
            let allowed = checker
                .user_can_access_project(auth.user_id(), project_id)
                .await
                .unwrap_or(false);
            if !allowed {
                return Err(denied(format!(
                    "Sandbox {public_id} belongs to project {project_id}, which you no longer have access to"
                )));
            }
        }
    }
    Ok(())
}

/// When a grant issued now stops buying sessions, and the same instant as a
/// timestamp for the grant row.
fn session_expiry(now: u64) -> (u64, DateTime<Utc>) {
    let session_expires = now + grant::EXCHANGE_GRANT_TTL_SECS + grant::SESSION_GRANT_TTL_SECS;
    let expires_at =
        DateTime::<Utc>::from_timestamp(i64::try_from(session_expires).unwrap_or(i64::MAX), 0)
            .unwrap_or_else(Utc::now);
    (session_expires, expires_at)
}

/// Sign the short-lived exchange grant the browser carries to the host.
fn mint_exchange_grant(
    master: &[u8; 32],
    request: &VerifiedLoginRequest,
    grant_id: i64,
    permission: &str,
    user_id: i32,
    now: u64,
) -> Result<String, Problem> {
    let owner = request.owner();
    grant::mint(
        &GrantKey::derive(master, owner),
        &GrantClaims {
            grant_id,
            usage: GrantUse::Exchange,
            owner,
            host: &request.host,
            permission,
            user_id: i64::from(user_id),
            expires_at: now + grant::EXCHANGE_GRANT_TTL_SECS,
        },
    )
    .map_err(|e| {
        temps_core::error_builder::internal_server_error()
            .title("Sign-In Grant Failed")
            .detail(format!(
                "Cannot sign grant {grant_id} for {} ({owner}): {e}",
                request.host
            ))
            .build()
    })
}

#[utoipa::path(
    get,
    path = "/projects/{project_id}/environments/{env_id}/ingress",
    tag = "Environments",
    responses(
        (status = 200, description = "Ingress settings", body = IngressSettingsResponse),
        (status = 401, description = "Unauthorized", body = temps_core::problemdetails::ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = temps_core::problemdetails::ProblemDetails),
        (status = 404, description = "Environment not found", body = temps_core::problemdetails::ProblemDetails),
        (status = 500, description = "Internal server error", body = temps_core::problemdetails::ProblemDetails)
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("env_id" = i32, Path, description = "Environment ID")
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_ingress_settings(
    State(state): State<Arc<IngressAppState>>,
    Path((project_id, env_id)): Path<(i32, i32)>,
    RequireAuth(auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    project_permission_guard!(
        auth,
        EnvironmentsRead,
        project_id,
        state.project_access_checker
    );
    project_scope_guard!(auth, project_id);
    let settings = state
        .ingress_service
        .get_settings(project_id, env_id)
        .await?;
    Ok(Json(IngressSettingsResponse::from(settings)))
}

#[utoipa::path(
    put,
    path = "/projects/{project_id}/environments/{env_id}/ingress",
    tag = "Environments",
    request_body = UpdateIngressSettingsRequest,
    responses(
        (status = 200, description = "Ingress settings updated", body = IngressSettingsResponse),
        (status = 401, description = "Unauthorized", body = temps_core::problemdetails::ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = temps_core::problemdetails::ProblemDetails),
        (status = 404, description = "Environment not found", body = temps_core::problemdetails::ProblemDetails),
        (status = 500, description = "Internal server error", body = temps_core::problemdetails::ProblemDetails)
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("env_id" = i32, Path, description = "Environment ID")
    ),
    security(("bearer_auth" = []))
)]
pub async fn update_ingress_settings(
    State(state): State<Arc<IngressAppState>>,
    Path((project_id, env_id)): Path<(i32, i32)>,
    RequireAuth(auth): RequireAuth,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<UpdateIngressSettingsRequest>,
) -> Result<impl IntoResponse, Problem> {
    project_permission_guard!(
        auth,
        EnvironmentsWrite,
        project_id,
        state.project_access_checker
    );
    project_scope_guard!(auth, project_id);
    // Who may reach the environment's public URL: the same authority the
    // environment's password and security settings need.
    super::handler::require_granted_project_write_authority(
        &state.environment_service,
        &auth,
        project_id,
        "public access",
    )
    .await?;

    let previous = state
        .ingress_service
        .get_settings(project_id, env_id)
        .await?;
    let settings = state
        .ingress_service
        .update_settings(
            project_id,
            env_id,
            IngressSettingsUpdate {
                mode: request.mode,
                require_login: request.require_login,
            },
            auth.user_id_opt(),
        )
        .await?;
    info!(
        project_id,
        environment_id = env_id,
        mode = settings.mode.as_str(),
        require_login = settings.login_permission.is_some(),
        "Environment ingress settings updated"
    );

    let audit = IngressSettingsUpdatedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        project_id,
        environment_id: env_id,
        previous_mode: previous.mode.as_str(),
        mode: settings.mode.as_str(),
        previous_require_login: previous.login_permission.is_some(),
        require_login: settings.login_permission.is_some(),
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log for ingress settings of environment {env_id}: {e}");
    }
    Ok(Json(IngressSettingsResponse::from(settings)))
}

#[utoipa::path(
    post,
    path = "/ingress/login-grants",
    tag = "Environments",
    request_body = CreateIngressLoginGrantRequest,
    responses(
        (status = 201, description = "Grant issued; send the browser to redirect_url", body = IngressLoginGrantResponse),
        (status = 400, description = "Malformed login request", body = temps_core::problemdetails::ProblemDetails),
        (status = 401, description = "Unauthorized", body = temps_core::problemdetails::ProblemDetails),
        (status = 403, description = "Not a browser session, a bad or expired signature, or insufficient permissions", body = temps_core::problemdetails::ProblemDetails),
        (status = 404, description = "Environment or sandbox not found", body = temps_core::problemdetails::ProblemDetails),
        (status = 409, description = "The environment or sandbox preview does not require a Temps login", body = temps_core::problemdetails::ProblemDetails),
        (status = 500, description = "Internal server error", body = temps_core::problemdetails::ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn create_ingress_login_grant(
    State(state): State<Arc<IngressAppState>>,
    RequireAuth(auth): RequireAuth,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<CreateIngressLoginGrantRequest>,
) -> Result<impl IntoResponse, Problem> {
    // A grant opens a site in this browser; API keys and tokens never
    // need one.
    if !auth.is_session() {
        return Err(temps_core::error_builder::forbidden()
            .title("Browser Session Required")
            .detail("Temps login grants are issued to signed-in console sessions only, not to API keys or tokens")
            .build());
    }
    let now = now_secs();
    let master = state.cookie_crypto.ingress_grant_master_key();
    let verified = verify_login_request(master, &request, now)?;
    let next = safe_next(request.next.as_deref());
    let user_id = auth.user_id();
    let context = AuditContext {
        user_id,
        ip_address: Some(metadata.ip_address.clone()),
        user_agent: metadata.user_agent.clone(),
    };
    let (_, expires_at) = session_expiry(now);

    let (issued, token) = match verified.kind {
        ProductKind::Sandbox => {
            let sandbox_id = verified.owner_id;
            let target = state
                .ingress_service
                .sandbox_login_target(sandbox_id)
                .await?;
            // The signature already binds the host; this keeps a grant for
            // one sandbox from ever naming another sandbox's preview host.
            if !target.owns_preview_host(&verified.host) {
                return Err(temps_core::error_builder::bad_request()
                    .title("Invalid Login Request")
                    .detail(format!(
                        "Host '{}' is not a preview host of sandbox {} (expected {}<port>.<preview domain>)",
                        verified.host,
                        target.public_id,
                        target.preview_label_prefix()
                    ))
                    .build());
            }
            may_open_sandbox(&state, &auth, &target).await?;
            let permission = SandboxPreviewAccess::LOGIN_PERMISSION;
            let issued = state
                .ingress_service
                .record_sandbox_login_grant(
                    user_id,
                    sandbox_id,
                    &verified.host,
                    permission,
                    expires_at,
                )
                .await?;
            let token =
                mint_exchange_grant(master, &verified, issued.grant_id, permission, user_id, now)?;
            info!(
                sandbox_id = %target.public_id,
                grant_id = issued.grant_id,
                user_id,
                "Sandbox preview Temps login grant issued"
            );
            let audit = SandboxPreviewLoginGrantIssuedAudit {
                context,
                sandbox_id: target.public_id.clone(),
                project_id: target.project_id,
                grant_id: issued.grant_id,
                host: verified.host.clone(),
                permission: permission.to_string(),
            };
            if let Err(e) = state.audit_service.create_audit_log(&audit).await {
                error!(
                    "Failed to create audit log for login grant {} of sandbox {}: {e}",
                    issued.grant_id, target.public_id
                );
            }
            (issued, token)
        }
        ProductKind::Environment | ProductKind::Database | ProductKind::Function => {
            let environment_id = verified.owner_id;
            let settings = state
                .ingress_service
                .get_settings_by_environment(environment_id)
                .await?;
            let permission = settings
                .login_permission
                .clone()
                .ok_or(IngressError::NotLoginProtected { environment_id })?;
            may_open(&state, &auth, &permission, settings.project_id).await?;
            let issued = state
                .ingress_service
                .record_login_grant(
                    user_id,
                    environment_id,
                    &verified.host,
                    &permission,
                    expires_at,
                )
                .await?;
            let token = mint_exchange_grant(
                master,
                &verified,
                issued.grant_id,
                &permission,
                user_id,
                now,
            )?;
            info!(
                environment_id,
                project_id = settings.project_id,
                grant_id = issued.grant_id,
                user_id,
                "Temps login grant issued"
            );
            let audit = IngressLoginGrantIssuedAudit {
                context,
                project_id: settings.project_id,
                environment_id,
                grant_id: issued.grant_id,
                host: verified.host.clone(),
                permission: permission.clone(),
            };
            if let Err(e) = state.audit_service.create_audit_log(&audit).await {
                error!(
                    "Failed to create audit log for login grant {} of environment {environment_id}: {e}",
                    issued.grant_id
                );
            }
            (issued, token)
        }
    };
    let redirect_url = redirect_url(verified.scheme, &verified.host, &token, next)?;

    Ok((
        StatusCode::CREATED,
        [(header::CACHE_CONTROL, "no-store")],
        Json(IngressLoginGrantResponse {
            redirect_url,
            grant_id: issued.grant_id,
            expires_at: issued
                .expires_at
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        }),
    ))
}

pub fn configure_routes() -> Router<Arc<IngressAppState>> {
    Router::new()
        .route(
            "/projects/{project_id}/environments/{env_id}/ingress",
            get(get_ingress_settings).put(update_ingress_settings),
        )
        .route("/ingress/login-grants", post(create_ingress_login_grant))
}

#[derive(OpenApi)]
#[openapi(
    paths(
        get_ingress_settings,
        update_ingress_settings,
        create_ingress_login_grant
    ),
    components(schemas(
        IngressMode,
        IngressSettingsResponse,
        UpdateIngressSettingsRequest,
        CreateIngressLoginGrantRequest,
        IngressLoginGrantResponse,
    ))
)]
pub struct IngressApiDoc;

#[cfg(test)]
mod tests {
    use super::*;

    const MASTER: [u8; 32] = [5u8; 32];
    const NOW: u64 = 1_800_000_000;

    fn signed_request(host: &str, scheme: LoginScheme, ts: u64) -> CreateIngressLoginGrantRequest {
        let owner = ProductRef {
            kind: ProductKind::Environment,
            id: 7,
        };
        CreateIngressLoginGrantRequest {
            owner_kind: "environment".into(),
            owner_id: 7,
            scheme: scheme.as_str().into(),
            host: host.into(),
            next: Some("/dashboard".into()),
            ts,
            sig: grant::sign_login_request(&GrantKey::derive(&MASTER, owner), scheme, host, ts),
        }
    }

    fn status(problem: Problem) -> StatusCode {
        problem.into_response().status()
    }

    fn signed_sandbox_request(
        sandbox_id: i64,
        host: &str,
        ts: u64,
    ) -> CreateIngressLoginGrantRequest {
        let owner = ProductRef {
            kind: ProductKind::Sandbox,
            id: sandbox_id,
        };
        CreateIngressLoginGrantRequest {
            owner_kind: "sandbox".into(),
            owner_id: sandbox_id,
            scheme: "https".into(),
            host: host.into(),
            next: Some("/".into()),
            ts,
            sig: grant::sign_login_request(
                &GrantKey::derive(&MASTER, owner),
                LoginScheme::Https,
                host,
                ts,
            ),
        }
    }

    #[test]
    fn a_signed_login_request_names_its_environment_scheme_and_host() {
        let request = signed_request("app.example.test", LoginScheme::Https, NOW);
        let verified = verify_login_request(&MASTER, &request, NOW + 5).expect("valid");
        assert_eq!(
            verified,
            VerifiedLoginRequest {
                kind: ProductKind::Environment,
                owner_id: 7,
                scheme: LoginScheme::Https,
                host: "app.example.test".into(),
            }
        );
    }

    #[test]
    fn a_signed_sandbox_login_request_is_bound_to_the_sandbox_key() {
        let host = "ws-0123456789abcdef-3000.preview.example.test";
        let request = signed_sandbox_request(11, host, NOW);
        let verified = verify_login_request(&MASTER, &request, NOW).expect("valid");
        assert_eq!(verified.kind, ProductKind::Sandbox);
        assert_eq!(verified.owner_id, 11);
        assert_eq!(verified.host, host);

        // The same signature does not open another sandbox, nor an
        // environment with the same id.
        let mut other = signed_sandbox_request(11, host, NOW);
        other.owner_id = 12;
        assert_eq!(
            status(verify_login_request(&MASTER, &other, NOW).unwrap_err()),
            StatusCode::FORBIDDEN
        );
        let mut as_environment = signed_sandbox_request(11, host, NOW);
        as_environment.owner_kind = "environment".into();
        assert_eq!(
            status(verify_login_request(&MASTER, &as_environment, NOW).unwrap_err()),
            StatusCode::FORBIDDEN
        );
    }

    fn target(owner: Option<i32>, project_id: Option<i32>) -> SandboxLoginTarget {
        SandboxLoginTarget {
            sandbox_id: 11,
            public_id: "sbx_0123456789abcdef".into(),
            owner_user_id: owner,
            project_id,
        }
    }

    #[tokio::test]
    async fn only_the_owner_may_open_a_temps_login_sandbox_preview() {
        let open = state(None);
        // `user()` is user 1.
        assert!(
            may_open_sandbox(&open, &user(temps_auth::Role::User), &target(Some(1), None))
                .await
                .is_ok()
        );
        let problem = may_open_sandbox(
            &open,
            &user(temps_auth::Role::Admin),
            &target(Some(2), None),
        )
        .await
        .unwrap_err();
        let response = problem.into_response();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            status(
                may_open_sandbox(&open, &user(temps_auth::Role::User), &target(None, None))
                    .await
                    .unwrap_err()
            ),
            StatusCode::FORBIDDEN
        );
        // A role without sandbox or project read access is refused.
        assert_eq!(
            status(
                may_open_sandbox(
                    &open,
                    &user(temps_auth::Role::MetricsIngest),
                    &target(Some(1), None)
                )
                .await
                .unwrap_err()
            ),
            StatusCode::FORBIDDEN
        );
        // Losing the project removes the preview too.
        let teams = state(Some(Arc::new(NoProjects)));
        assert_eq!(
            status(
                may_open_sandbox(
                    &teams,
                    &user(temps_auth::Role::User),
                    &target(Some(1), Some(3))
                )
                .await
                .unwrap_err()
            ),
            StatusCode::FORBIDDEN
        );
        assert!(may_open_sandbox(
            &teams,
            &user(temps_auth::Role::User),
            &target(Some(1), None)
        )
        .await
        .is_ok());
    }

    #[test]
    fn sandbox_ingress_errors_map_to_their_status() {
        assert_eq!(
            status(IngressError::SandboxNotFound { sandbox_id: 3 }.into()),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            status(
                IngressError::SandboxNotLoginProtected {
                    public_id: "sbx_0123456789abcdef".into(),
                    access: "public"
                }
                .into()
            ),
            StatusCode::CONFLICT
        );
        assert_eq!(
            status(
                IngressError::SandboxDatabase {
                    sandbox_id: 3,
                    operation: "sandbox lookup",
                    source: sea_orm::DbErr::Custom("down".into()),
                }
                .into()
            ),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[test]
    fn an_exchange_grant_is_bound_to_owner_host_and_expiry() {
        let host = "ws-0123456789abcdef-3000.preview.example.test";
        let request = verify_login_request(&MASTER, &signed_sandbox_request(11, host, NOW), NOW)
            .expect("valid");
        let token = mint_exchange_grant(
            &MASTER,
            &request,
            41,
            SandboxPreviewAccess::LOGIN_PERMISSION,
            1,
            NOW,
        )
        .expect("mint");
        let key = GrantKey::derive(&MASTER, request.owner());
        let mut buffer = [0u8; grant::MAX_GRANT_PAYLOAD];
        let claims = grant::verify(&key, &token, &mut buffer).expect("verifies for its owner");
        assert_eq!(claims.grant_id, 41);
        assert_eq!(claims.usage, GrantUse::Exchange);
        assert_eq!(claims.host, host);
        assert_eq!(claims.permission, "sandboxes:read");
        assert_eq!(claims.user_id, 1);
        let expect = |now: u64| grant::GrantExpectation {
            owner: request.owner(),
            host,
            permission: SandboxPreviewAccess::LOGIN_PERMISSION,
            usage: GrantUse::Exchange,
            now,
        };
        let none = grant::Revocations::default();
        assert!(grant::check(&claims, &expect(NOW + 1), &none).is_ok());
        // Expired after the exchange TTL.
        assert!(matches!(
            grant::check(
                &claims,
                &expect(NOW + grant::EXCHANGE_GRANT_TTL_SECS + 1),
                &none
            ),
            Err(grant::GrantError::Expired { grant_id: 41, .. })
        ));
        // Another sandbox's key does not verify it.
        let other = GrantKey::derive(
            &MASTER,
            ProductRef {
                kind: ProductKind::Sandbox,
                id: 12,
            },
        );
        let mut buffer = [0u8; grant::MAX_GRANT_PAYLOAD];
        assert_eq!(
            grant::verify(&other, &token, &mut buffer).unwrap_err(),
            grant::GrantError::BadSignature
        );
    }

    #[test]
    fn unsigned_expired_or_altered_login_requests_are_refused() {
        // Signed by another instance.
        let mut forged = signed_request("app.example.test", LoginScheme::Https, NOW);
        forged.sig = grant::sign_login_request(
            &GrantKey::derive(
                &[1u8; 32],
                ProductRef {
                    kind: ProductKind::Environment,
                    id: 7,
                },
            ),
            LoginScheme::Https,
            "app.example.test",
            NOW,
        );
        assert_eq!(
            status(verify_login_request(&MASTER, &forged, NOW).unwrap_err()),
            StatusCode::FORBIDDEN
        );
        // Pointed at another host or environment after signing.
        let mut moved = signed_request("app.example.test", LoginScheme::Https, NOW);
        moved.host = "elsewhere.example.test".into();
        assert_eq!(
            status(verify_login_request(&MASTER, &moved, NOW).unwrap_err()),
            StatusCode::FORBIDDEN
        );
        let mut other_env = signed_request("app.example.test", LoginScheme::Https, NOW);
        other_env.owner_id = 8;
        assert_eq!(
            status(verify_login_request(&MASTER, &other_env, NOW).unwrap_err()),
            StatusCode::FORBIDDEN
        );
        // Downgraded to http.
        let mut downgraded = signed_request("app.example.test", LoginScheme::Https, NOW);
        downgraded.scheme = "http".into();
        assert_eq!(
            status(verify_login_request(&MASTER, &downgraded, NOW).unwrap_err()),
            StatusCode::FORBIDDEN
        );
        // Too old.
        let stale = signed_request("app.example.test", LoginScheme::Https, NOW);
        assert_eq!(
            status(
                verify_login_request(&MASTER, &stale, NOW + grant::LOGIN_REQUEST_TTL_SECS + 1)
                    .unwrap_err()
            ),
            StatusCode::FORBIDDEN
        );
    }

    #[test]
    fn malformed_login_requests_are_bad_requests() {
        let mut database = signed_request("app.example.test", LoginScheme::Https, NOW);
        database.owner_kind = "database".into();
        let mut unknown = signed_request("app.example.test", LoginScheme::Https, NOW);
        unknown.owner_kind = "nope".into();
        let mut huge_id = signed_request("app.example.test", LoginScheme::Https, NOW);
        huge_id.owner_id = i64::MAX;
        let mut ftp = signed_request("app.example.test", LoginScheme::Https, NOW);
        ftp.scheme = "ftp".into();
        let mut bad_host = signed_request("app.example.test", LoginScheme::Https, NOW);
        bad_host.host = "app.example.test/evil".into();
        for request in [database, unknown, huge_id, ftp, bad_host] {
            assert_eq!(
                status(verify_login_request(&MASTER, &request, NOW).unwrap_err()),
                StatusCode::BAD_REQUEST
            );
        }
    }

    #[test]
    fn the_redirect_lands_on_the_hosts_exchange_path_with_a_safe_next() {
        let url = redirect_url(
            LoginScheme::Https,
            "app.example.test",
            "tg1.a.b",
            safe_next(Some("/a?b=c")),
        )
        .unwrap();
        assert_eq!(
            url,
            "https://app.example.test/.well-known/temps/ingress/grant?grant=tg1.a.b&next=%2Fa%3Fb%3Dc"
        );
        for unsafe_next in [
            None,
            Some("https://elsewhere.example.test"),
            Some("//elsewhere.example.test"),
            Some("/a\\b"),
        ] {
            assert_eq!(safe_next(unsafe_next), "/");
        }
        assert_eq!(
            grant_host("App.Example.Test."),
            Some("app.example.test".into())
        );
        assert_eq!(grant_host("app.example.test:443"), None);
        assert_eq!(grant_host(""), None);
    }

    #[test]
    fn ingress_errors_map_to_their_status() {
        assert_eq!(
            status(
                IngressError::EnvironmentNotFound {
                    project_id: 1,
                    environment_id: 2
                }
                .into()
            ),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            status(IngressError::NotLoginProtected { environment_id: 2 }.into()),
            StatusCode::CONFLICT
        );
        assert_eq!(
            status(IngressError::EndpointMissing { environment_id: 2 }.into()),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    fn user(role: temps_auth::Role) -> temps_auth::AuthContext {
        let user = temps_entities::users::Model {
            id: 1,
            name: "Operator".to_string(),
            email: "operator@example.test".to_string(),
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
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        temps_auth::AuthContext::new_session(user, role)
    }

    struct NoProjects;

    #[temps_core::async_trait::async_trait]
    impl temps_core::ProjectAccessChecker for NoProjects {
        async fn user_can_access_project(
            &self,
            _user_id: i32,
            _project_id: i32,
        ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
            Ok(false)
        }
    }

    struct NoAudit;

    #[temps_core::async_trait::async_trait]
    impl AuditLogger for NoAudit {
        async fn create_audit_log(
            &self,
            _operation: &dyn temps_core::AuditOperation,
        ) -> anyhow::Result<()> {
            Ok(())
        }
    }

    fn state(checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>) -> IngressAppState {
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
        );
        let config = Arc::new(temps_config::ConfigService::new(
            Arc::new(
                temps_config::ServerConfig::new(
                    "127.0.0.1:3000".into(),
                    "postgres://localhost/test".into(),
                    None,
                    None,
                )
                .unwrap(),
            ),
            db.clone(),
        ));
        IngressAppState {
            ingress_service: Arc::new(IngressService::new(db.clone())),
            environment_service: Arc::new(EnvironmentService::new(db, config)),
            cookie_crypto: Arc::new(CookieCrypto::from_bytes(&MASTER)),
            audit_service: Arc::new(NoAudit),
            project_access_checker: checker,
        }
    }

    #[tokio::test]
    async fn opening_needs_the_login_permission_and_project_access() {
        let open = state(None);
        assert!(
            may_open(&open, &user(temps_auth::Role::User), "projects:read", 3)
                .await
                .is_ok()
        );
        assert_eq!(
            status(
                may_open(&open, &user(temps_auth::Role::User), "no:such", 3)
                    .await
                    .unwrap_err()
            ),
            StatusCode::FORBIDDEN
        );
        let teams = state(Some(Arc::new(NoProjects)));
        assert_eq!(
            status(
                may_open(&teams, &user(temps_auth::Role::User), "projects:read", 3)
                    .await
                    .unwrap_err()
            ),
            StatusCode::FORBIDDEN
        );
        // Instance admins are never narrowed by team membership.
        assert!(
            may_open(&teams, &user(temps_auth::Role::Admin), "projects:read", 3)
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn an_api_key_cannot_get_a_login_grant() {
        let state = Arc::new(state(None));
        let user = user(temps_auth::Role::Admin).user.expect("user");
        let api_key = temps_auth::AuthContext::new_cli_token(user, temps_auth::Role::Admin);
        let metadata = RequestMetadata {
            ip_address: "192.0.2.1".into(),
            user_agent: "test".into(),
            headers: Default::default(),
            visitor_id_cookie: None,
            session_id_cookie: None,
            base_url: "https://console.example.test".into(),
            scheme: "https".into(),
            host: "console.example.test".into(),
            is_secure: true,
        };
        let result = create_ingress_login_grant(
            State(state),
            RequireAuth(api_key),
            Extension(metadata),
            Json(signed_request(
                "app.example.test",
                LoginScheme::Https,
                now_secs(),
            )),
        )
        .await;
        match result {
            Ok(_) => panic!("a CLI token must not get a login grant"),
            Err(problem) => assert_eq!(status(problem), StatusCode::FORBIDDEN),
        }
    }
}
