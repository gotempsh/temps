// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::sync::Arc;

use axum::{
    extract::{rejection::QueryRejection, Extension, Path, Query, State},
    http::{
        header::{CACHE_CONTROL, COOKIE, REFERRER_POLICY, SET_COOKIE},
        HeaderMap, HeaderValue, StatusCode,
    },
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
    Json, Router,
};
use cookie::Cookie;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use temps_core::problemdetails::Problem;
use temps_core::{AuditContext, CookieCrypto, RequestMetadata, SensitiveAction};
use tracing::{error, warn};
use utoipa::{IntoParams, OpenApi, ToSchema};

use crate::audit::{
    LoginAudit, OidcProviderCreatedAudit, OidcProviderDeletedAudit, OidcProviderUpdatedAudit,
    OidcRoleMappingCreatedAudit, OidcRoleMappingDeletedAudit,
};
use crate::oidc_errors::OidcError;
use crate::oidc_service::{OidcLoginStart, OidcService, LOGIN_STATE_TTL_MINUTES};
use crate::oidc_types::{
    provider_to_response, provider_user_to_response, CreateOidcProviderRequest,
    CreateOidcRoleMappingRequest, OidcProviderResponse, OidcProviderSummary,
    OidcProviderUserResponse, OidcRoleMappingResponse, OidcTestConnectionResponse,
    UpdateOidcProviderRequest,
};
use crate::permission_guard;
use crate::state::AuthState;
use crate::RequireAuth;

/// No `Debug`: the query carries the authorization code and the issuer's
/// free-text `error_description`, neither of which belongs in a log line.
///
/// `parameter_in = Query` is explicit because the handler takes
/// `Result<Query<_>, QueryRejection>`, through which utoipa cannot infer the
/// location and would otherwise document these as path parameters.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct OidcCallbackQuery {
    pub code: Option<String>,
    pub state: Option<String>,
    pub error: Option<String>,
    pub error_description: Option<String>,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct OidcLoginQuery {
    pub return_to: Option<String>,
}

/// `/`, like every other auth cookie: a console served behind a reverse proxy
/// under a URL prefix would never be sent a cookie scoped to `/api/auth/oidc`.
/// Each login attempt has its own cookie name, so a wide path costs nothing.
const LOGIN_BINDING_COOKIE_PATH: &str = "/";

/// Name of the cookie that ties one login attempt to the browser that started
/// it. Derived from the attempt's `state`, so logins started in several tabs
/// at once each keep their own cookie. It is a hash, so the name carries no
/// part of the `state` itself.
///
/// Over https the name carries the `__Host-` prefix, which browsers only
/// accept from the exact host that is serving the console: with `Secure`,
/// `Path=/` and no `Domain`, an app deployed on a sibling subdomain cannot
/// plant a cookie of this name (encryption stops a cookie being altered, not
/// being copied from another login). Over plain http browsers refuse that
/// prefix, so the unprefixed name is used there.
fn login_binding_cookie_name(state: &str, secure: bool) -> String {
    let digest = Sha256::digest(state.as_bytes());
    let prefix = if secure {
        "__Host-oidc_login_"
    } else {
        "oidc_login_"
    };
    format!("{prefix}{}", hex::encode(&digest[..8]))
}

fn login_binding_cookie_builder(
    name: String,
    value: String,
    secure: bool,
) -> cookie::CookieBuilder<'static> {
    // `Lax`, not `Strict`: the issuer sends the browser back to the callback
    // with a cross-site top-level navigation, and a `Strict` cookie is not sent
    // on that request.
    Cookie::build((name, value))
        .http_only(true)
        .path(LOGIN_BINDING_COOKIE_PATH)
        .same_site(cookie::SameSite::Lax)
        .secure(secure)
}

fn cookie_header_value(cookie: &Cookie<'_>) -> Result<HeaderValue, String> {
    cookie
        .to_string()
        .parse()
        .map_err(|error| format!("failed to build login binding cookie: {error}"))
}

/// `Set-Cookie` that ties the login attempt `state` to the browser receiving it.
fn login_binding_cookie(
    crypto: &CookieCrypto,
    state: &str,
    secure: bool,
) -> Result<HeaderValue, String> {
    let sealed = crypto
        .encrypt(state)
        .map_err(|error| format!("failed to seal login binding cookie: {error}"))?;
    let cookie =
        login_binding_cookie_builder(login_binding_cookie_name(state, secure), sealed, secure)
            .max_age(cookie::time::Duration::minutes(LOGIN_STATE_TTL_MINUTES))
            .build();
    cookie_header_value(&cookie)
}

/// `Set-Cookie` that removes the binding cookie of the login attempt `state`.
fn clear_login_binding_cookie(state: &str, secure: bool) -> Result<HeaderValue, String> {
    let cookie = login_binding_cookie_builder(
        login_binding_cookie_name(state, secure),
        String::new(),
        secure,
    )
    .max_age(cookie::time::Duration::ZERO)
    .build();
    cookie_header_value(&cookie)
}

/// Login redirects and callback responses carry the authorization `state` (and,
/// on the callback, the code) in their URLs. Keep them out of caches and out of
/// the `Referer` the next page would send.
fn insert_no_store_no_referrer(headers: &mut HeaderMap) {
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// True when the request carries the cookie that was set when the login
/// attempt `state` started, and that cookie was sealed for exactly this `state`.
fn login_is_bound_to_browser(
    crypto: &CookieCrypto,
    headers: &HeaderMap,
    state: &str,
    secure: bool,
) -> bool {
    let name = login_binding_cookie_name(state, secure);
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|header| header.to_str().ok())
        .flat_map(|header| Cookie::split_parse(header.to_owned()).flatten())
        .filter(|cookie| cookie.name() == name)
        .any(|cookie| {
            crypto
                .decrypt(cookie.value())
                .map(|bound| constant_time_eq(bound.as_bytes(), state.as_bytes()))
                .unwrap_or(false)
        })
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct OidcProvidersListResponse {
    pub providers: Vec<OidcProviderSummary>,
}

pub fn configure_oidc_routes() -> Router<Arc<AuthState>> {
    Router::new()
        .route("/auth/oidc/providers", get(list_public_providers))
        // NOTE: `/auth/oidc/login/{slug}` is registered in `handlers.rs`
        // inside the rate-limited /auth/* router group. Do NOT add it here
        // — Axum will panic with "Overlapping method route" at startup.
        .route("/admin/oidc/providers", post(create_oidc_provider))
        .route("/admin/oidc/providers", get(list_oidc_providers))
        .route(
            "/admin/oidc/providers/{provider_id}",
            axum::routing::patch(update_oidc_provider),
        )
        .route(
            "/admin/oidc/providers/{provider_id}",
            axum::routing::delete(delete_oidc_provider),
        )
        .route(
            "/admin/oidc/providers/{provider_id}/test",
            post(test_oidc_provider),
        )
        .route(
            "/admin/oidc/providers/{provider_id}/users",
            get(list_oidc_provider_users),
        )
        .route(
            "/admin/oidc/providers/{provider_id}/role-mappings",
            get(list_oidc_role_mappings),
        )
        .route(
            "/admin/oidc/providers/{provider_id}/role-mappings",
            post(create_oidc_role_mapping),
        )
        .route(
            "/admin/oidc/role-mappings/{mapping_id}",
            axum::routing::delete(delete_oidc_role_mapping),
        )
}

#[utoipa::path(
    get,
    path = "/auth/oidc/providers",
    responses(
        (status = 200, description = "Enabled OIDC providers for login page", body = OidcProvidersListResponse)
    ),
    tag = "Authentication"
)]
pub async fn list_public_providers(
    State(state): State<Arc<AuthState>>,
) -> Result<Json<OidcProvidersListResponse>, Problem> {
    let providers = state.oidc_service.list_enabled_providers().await?;
    Ok(Json(OidcProvidersListResponse { providers }))
}

#[utoipa::path(
    get,
    path = "/auth/oidc/login/{slug}",
    params(
        ("slug" = String, Path, description = "OIDC provider slug (from /email-status or /auth/oidc/providers)"),
        OidcLoginQuery
    ),
    responses(
        (status = 302, description = "Redirect to IdP authorize URL"),
        (status = 404, description = "Provider not found"),
        (status = 503, description = "OIDC provider unreachable")
    ),
    tag = "Authentication"
)]
pub async fn start_oidc_login_by_slug(
    State(state): State<Arc<AuthState>>,
    Path(slug): Path<String>,
    Query(query): Query<OidcLoginQuery>,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<Response, Problem> {
    let provider = state.oidc_service.get_provider_by_slug(&slug).await?;
    let redirect_uri = format!(
        "{}/api/auth/oidc/callback",
        metadata.base_url.trim_end_matches('/')
    );
    let login = state
        .oidc_service
        .start_login(provider.id, &redirect_uri, query.return_to)
        .await?;
    redirect_to_issuer(&state, &metadata, &login)
}

/// Start a login against the Cloud-managed console-access provider without
/// knowing its slug.
///
/// The slug embeds the provider's row id, which Cloud does not know and the
/// instance may regenerate, so Cloud's "Open console" button needs a fixed
/// address it can link to: this one. A browser that already holds a Cloud
/// session completes the round trip without seeing a login page, which is
/// what makes the hosted console feel signed-in from Cloud. Anything else
/// (the interactive login, the callback, `return_to` sanitising) is exactly
/// the slug route; only the lookup differs. 404 when no managed provider is
/// installed, so a self-hosted instance that never enrolled has nothing to
/// probe here.
#[utoipa::path(
    get,
    path = "/auth/oidc/cloud/login",
    params(OidcLoginQuery),
    responses(
        (status = 302, description = "Redirect to the Temps Cloud authorize URL"),
        (status = 404, description = "This instance has no Cloud-managed console-access provider"),
        (status = 503, description = "OIDC provider unreachable")
    ),
    tag = "Authentication"
)]
pub async fn start_managed_cloud_login(
    State(state): State<Arc<AuthState>>,
    Query(query): Query<OidcLoginQuery>,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<Response, Problem> {
    let provider = state
        .oidc_service
        .managed_cloud_provider()
        .await?
        .ok_or(OidcError::ProviderNotFound { provider_id: 0 })?;
    let redirect_uri = format!(
        "{}/api/auth/oidc/callback",
        metadata.base_url.trim_end_matches('/')
    );
    let login = state
        .oidc_service
        .start_login(provider.id, &redirect_uri, query.return_to)
        .await?;
    redirect_to_issuer(&state, &metadata, &login)
}

/// Redirect the browser to the issuer, tying this login attempt to it.
fn redirect_to_issuer(
    state: &AuthState,
    metadata: &RequestMetadata,
    login: &OidcLoginStart,
) -> Result<Response, Problem> {
    // A local failure, not the provider's: don't report it as an unreachable issuer.
    let cookie = login_binding_cookie(&state.cookie_crypto, &login.state, metadata.is_secure)
        .map_err(|reason| {
            error!(%reason, "could not start OIDC login");
            temps_core::problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Sign-in Failed")
                .with_detail("Could not start sign-in. Try again.")
        })?;
    let mut headers = HeaderMap::new();
    headers.insert(SET_COOKIE, cookie);
    insert_no_store_no_referrer(&mut headers);
    Ok((headers, Redirect::temporary(&login.authorize_url)).into_response())
}

#[utoipa::path(
    get,
    path = "/auth/oidc/callback",
    params(OidcCallbackQuery),
    responses(
        (status = 302, description = "Redirect to app with session cookie or login error"),
    ),
    tag = "Authentication"
)]
pub async fn oidc_callback(
    State(state): State<Arc<AuthState>>,
    query: Result<Query<OidcCallbackQuery>, QueryRejection>,
    Extension(metadata): Extension<RequestMetadata>,
    headers: HeaderMap,
) -> Response {
    // A query that does not deserialize (a repeated `state`, say) still carries
    // the code and state in its URL: answer it like any other malformed
    // callback, with the same no-store / no-referrer headers, instead of
    // letting the extractor's plain 400 go out without them.
    let query = match query {
        Ok(Query(query)) => query,
        Err(_) => {
            warn!(
                target: "temps_auth::oidc",
                reason_code = "callback_invalid",
                "OIDC callback query could not be parsed"
            );
            let mut response = redirect_login_error("callback_invalid");
            insert_no_store_no_referrer(response.headers_mut());
            return response;
        }
    };
    let state_param = query.state.clone();

    // A callback that carries a code and a state must come from the browser
    // that started that login. This is checked before the state is consumed,
    // so a refused request leaves the real login untouched. The link itself
    // is not enough: whoever starts a login at the issuer also holds a valid
    // callback link, and without this check another browser that follows it
    // would be signed in as them (login CSRF, OAuth 2.0 Security BCP).
    let unbound = match (&query.code, &query.state) {
        (Some(_), Some(candidate)) => !login_is_bound_to_browser(
            &state.cookie_crypto,
            &headers,
            candidate,
            metadata.is_secure,
        ),
        _ => false,
    };

    let mut response = if unbound {
        warn!(
            target: "temps_auth::oidc::abuse",
            ip = %metadata.ip_address,
            user_agent = %metadata.user_agent,
            "OIDC callback without this browser's login binding cookie (login started in another browser, or cookies dropped)"
        );
        redirect_login_error("state_invalid")
    } else {
        complete_callback(&state, &metadata, query).await
    };
    insert_no_store_no_referrer(response.headers_mut());

    // The cookie has done its job whatever the outcome.
    if let Some(state_param) = state_param {
        if let Ok(clear) = clear_login_binding_cookie(&state_param, metadata.is_secure) {
            response.headers_mut().append(SET_COOKIE, clear);
        }
    }
    response
}

async fn complete_callback(
    state: &Arc<AuthState>,
    metadata: &RequestMetadata,
    query: OidcCallbackQuery,
) -> Response {
    let OidcCallbackQuery {
        code,
        state: callback_state,
        error,
        error_description,
    } = query;
    // The IdP's `error` + `error_description` are free text chosen by the
    // issuer (or by whoever crafted the callback URL) and can carry account
    // data, internal addresses or protocol secrets. Echoing them into the
    // redirect's `?reason=` would put them in the address bar, history and
    // `Referer`; logging them would put attacker-chosen text in the operator
    // log. Accept the fields and drop them: the browser gets the opaque
    // `idp_error` code and the log gets a fixed reason code.
    drop(error_description);
    if let Some(error) = error {
        warn!(
            target: "temps_auth::oidc",
            reason_code = "idp_error",
            provider_error = provider_error_code(&error),
            "OIDC provider declined authorization"
        );
        return redirect_login_error("idp_error");
    }

    let (code, state_param) = match (code, callback_state) {
        (Some(code), Some(state_param)) => (code, state_param),
        _ => {
            // Same opaque-code policy as the rest of this handler:
            // never let internal details (here, "what query params
            // arrived from the IdP") reach the browser URL bar.
            warn!(
                target: "temps_auth::oidc",
                "OIDC callback missing code or state query parameter"
            );
            return redirect_login_error("callback_invalid");
        }
    };

    match complete_oidc_login(state, metadata, &code, &state_param).await {
        Ok(response) => response,
        Err(err) => {
            // Distinguish potentially-abusive probes from ordinary failures.
            // `StateNotFound` means an attacker guessed (or replayed) a state
            // token that never existed; `StateExpired` is the same shape but
            // for stale tokens. Both are interesting to a SOC even though we
            // return the same generic error to the browser.
            match &err {
                OidcError::StateNotFound { .. } => {
                    warn!(
                        target: "temps_auth::oidc::abuse",
                        ip = %metadata.ip_address,
                        user_agent = %metadata.user_agent,
                        "OIDC callback with unknown state token (possible probe / replay): {}",
                        err
                    );
                }
                OidcError::StateExpired { age_secs, .. } => {
                    warn!(
                        target: "temps_auth::oidc::abuse",
                        ip = %metadata.ip_address,
                        user_agent = %metadata.user_agent,
                        age_secs = age_secs,
                        "OIDC callback with expired state token: {}",
                        err
                    );
                }
                OidcError::InsufficientRole {
                    provider_id,
                    resolved_role,
                } => {
                    // ADR-045 §4: this reached a real IdP identity and was
                    // refused by the instance-side role gate -- a materially
                    // different, more interesting failure than "credentials
                    // never resolved to a user at all"
                    // (`LoginAudit{success: false}`), so it gets its own
                    // audit row rather than being folded into that one.
                    warn!(
                        target: "temps_auth::oidc::abuse",
                        provider_id = provider_id,
                        resolved_role = %resolved_role,
                        ip = %metadata.ip_address,
                        user_agent = %metadata.user_agent,
                        "OIDC login denied: provider requires an admin-level role"
                    );
                    let provider_name = state
                        .oidc_service
                        .get_provider(*provider_id)
                        .await
                        .map(|provider| provider.name)
                        .unwrap_or_else(|_| format!("provider {provider_id}"));
                    if let Err(audit_error) = state
                        .audit_service
                        .create_audit_log(&crate::audit::OidcLoginDeniedAudit {
                            user_id: None,
                            ip_address: Some(metadata.ip_address.to_string()),
                            user_agent: metadata.user_agent.as_str().to_string(),
                            provider_id: *provider_id,
                            provider_name,
                            reason: "insufficient_role",
                        })
                        .await
                    {
                        error!(%audit_error, "Failed to create OIDC login-denied audit log");
                    }
                }
                // Operator-side failures keep their actionable detail: the
                // issuer is operator configuration and the reason comes from
                // this instance's own HTTP client or database.
                OidcError::DiscoveryFailed { issuer, reason } => {
                    warn!(
                        target: "temps_auth::oidc",
                        reason_code = login_error_code_for(&err),
                        %issuer,
                        %reason,
                        "OIDC callback failed: issuer discovery failed"
                    );
                }
                OidcError::Database(db_error) => {
                    warn!(
                        target: "temps_auth::oidc",
                        reason_code = login_error_code_for(&err),
                        error = %db_error,
                        "OIDC callback failed: database error"
                    );
                }
                // The token-exchange response body and ID-token verification
                // details are issuer-controlled and can echo protocol data
                // (codes, tokens, claims): log the HTTP status at most.
                OidcError::TokenExchangeFailed { status, .. } => {
                    warn!(
                        target: "temps_auth::oidc",
                        reason_code = login_error_code_for(&err),
                        status = *status,
                        "OIDC callback failed: issuer refused the code exchange"
                    );
                }
                _ => {
                    warn!(
                        target: "temps_auth::oidc",
                        reason_code = login_error_code_for(&err),
                        "OIDC callback failed"
                    );
                }
            }
            // Don't reflect the raw error text into the browser URL.
            // Diagnostics above retain only safe reason codes; the
            // user sees a short, stable code that the login page can
            // translate into a friendly message (and that won't leak
            // IdP response bodies / discovery details into history,
            // referrers, or shared screenshots).
            redirect_login_error(login_error_code_for(&err))
        }
    }
}

/// The registered OAuth 2.0 / OpenID Connect authorization error codes
/// (RFC 6749 section 4.1.2.1, OIDC Core section 3.1.2.6, RFC 9101). The
/// callback's `error` is attacker-controllable text, so only a registered code
/// is logged as-is; anything else is logged as `unregistered`.
fn provider_error_code(error: &str) -> &'static str {
    const REGISTERED: &[&str] = &[
        "invalid_request",
        "unauthorized_client",
        "access_denied",
        "unsupported_response_type",
        "invalid_scope",
        "server_error",
        "temporarily_unavailable",
        "interaction_required",
        "login_required",
        "account_selection_required",
        "consent_required",
        "invalid_request_uri",
        "invalid_request_object",
        "request_not_supported",
        "request_uri_not_supported",
        "registration_not_supported",
    ];
    REGISTERED
        .iter()
        .find(|code| **code == error)
        .copied()
        .unwrap_or("unregistered")
}

/// Map an `OidcError` into a short, stable, user-safe code that the
/// login page can render as a friendly message. Anything the operator
/// actually needs to debug is already in the server log via `warn!`
/// in the callback handler; this is the public face of the failure.
fn login_error_code_for(err: &OidcError) -> &'static str {
    match err {
        OidcError::StateNotFound { .. } => "state_invalid",
        OidcError::StateExpired { .. } => "state_expired",
        OidcError::DiscoveryFailed { .. } => "idp_unreachable",
        OidcError::TokenExchangeFailed { .. } => "idp_rejected_code",
        OidcError::IdTokenInvalid { .. } => "id_token_invalid",
        OidcError::EmailClaimMissing => "email_missing",
        OidcError::EmailNotVerified { .. } => "email_not_verified",
        OidcError::UserNotProvisioned { .. } => "user_not_provisioned",
        OidcError::ProviderDisabled { .. } => "provider_disabled",
        OidcError::CredentialsChanged { .. } => "credentials_changed",
        OidcError::ProviderNotFound { .. } => "provider_not_found",
        OidcError::NoProviderConfigured => "no_provider_configured",
        OidcError::InvalidIssuer { .. } => "issuer_invalid",
        OidcError::InvalidReturnTo => "return_to_invalid",
        OidcError::InvalidRole { .. } => "role_invalid",
        OidcError::RoleMappingNotFound { .. } => "role_mapping_not_found",
        OidcError::ProviderAlreadyExists { .. } => "provider_conflict",
        OidcError::ManagedByCloudEdit { .. }
        | OidcError::ManagedByCloudDelete { .. }
        | OidcError::ManagedByCloudRoleMapping { .. } => "provider_managed_by_cloud",
        OidcError::InsufficientRole { .. } => "insufficient_role",
        OidcError::IssuerMatchesManagedCloudProvider { .. }
        | OidcError::ManagedIssuerAlreadyUsed { .. } => "issuer_managed_by_cloud",
        OidcError::ProviderRevokedDuringLogin { .. } => "provider_revoked",
        OidcError::Database(_) => "internal_error",
    }
}

async fn complete_oidc_login(
    state: &AuthState,
    metadata: &RequestMetadata,
    code: &str,
    state_param: &str,
) -> Result<Response, OidcError> {
    let login_state = state.oidc_service.consume_login_state(state_param).await?;
    let provider = state
        .oidc_service
        .get_provider(login_state.provider_id)
        .await?;

    // Re-check `enabled` on the callback path. `start_login` already
    // checks it before issuing the authorize URL, but an admin can
    // disable a provider while a user has an in-flight login state
    // (up to `LOGIN_STATE_TTL_MINUTES` later). Without this guard,
    // an admin who disables a provider to revoke SSO access during
    // an incident still lets every in-flight session complete.
    if !provider.enabled {
        return Err(OidcError::ProviderDisabled {
            provider_id: login_state.provider_id,
        });
    }

    let redirect_uri = format!(
        "{}/api/auth/oidc/callback",
        metadata.base_url.trim_end_matches('/')
    );

    let claims = state
        .oidc_service
        .exchange_code(&provider, &redirect_uri, code, &login_state)
        .await?;
    let resolved = state
        .oidc_service
        .resolve_user(login_state.provider_id, &claims.claims, &claims.raw_claims)
        .await?;
    let user = resolved.user;

    let return_to = OidcService::sanitize_return_to(login_state.return_to);

    if user.must_change_password {
        let reset_token = state
            .auth_service
            .create_required_password_change_token(&user)
            .await
            .map_err(|error| match error {
                crate::auth_service::UserAuthError::CredentialsChanged { user_id } => {
                    OidcError::CredentialsChanged { user_id }
                }
                error => OidcError::DiscoveryFailed {
                    issuer: provider.issuer_url.clone(),
                    reason: format!(
                        "failed to create required password-change session for user {}: {error}",
                        user.id
                    ),
                },
            })?;
        let encrypted_token = state.cookie_crypto.encrypt(&reset_token).map_err(|error| {
            OidcError::DiscoveryFailed {
                issuer: provider.issuer_url.clone(),
                reason: format!(
                    "failed to encrypt required password-change session for user {}: {error}",
                    user.id
                ),
            }
        })?;
        let password_change_cookie = Cookie::build(("password_change_session", encrypted_token))
            .http_only(true)
            .path("/")
            .max_age(cookie::time::Duration::minutes(15))
            .same_site(cookie::SameSite::Strict)
            .secure(metadata.is_secure)
            .build();
        let cookie_header = password_change_cookie
            .to_string()
            .parse()
            .map_err(|error| OidcError::DiscoveryFailed {
                issuer: provider.issuer_url.clone(),
                reason: format!(
                    "failed to build required password-change cookie for user {}: {error}",
                    user.id
                ),
            })?;
        let mut headers = HeaderMap::new();
        headers.insert(SET_COOKIE, cookie_header);

        if let Err(error) = state
            .audit_service
            .create_audit_log(&LoginAudit {
                context: AuditContext {
                    user_id: user.id,
                    ip_address: Some(metadata.ip_address.to_string()),
                    user_agent: metadata.user_agent.as_str().to_string(),
                },
                success: true,
                login_method: "oidc-password-change-required".to_string(),
            })
            .await
        {
            error!(
                user_id = user.id,
                error = %error,
                "Failed to audit OIDC required password-change redirect"
            );
        }

        return Ok((headers, Redirect::to("/auth/change-password")).into_response());
    }

    if user.mfa_enabled {
        let mfa_token = state
            .auth_service
            .create_mfa_session(user.id, &format!("oidc:{}", provider.id))
            .await
            .map_err(|e| OidcError::DiscoveryFailed {
                issuer: provider.issuer_url.clone(),
                reason: format!("failed to create MFA session: {e}"),
            })?;
        // SECURITY: the pending MFA challenge is a `sessions` row like any
        // other, so it must survive the same check as a full session — a
        // provider revoked mid-login must not leave a challenge behind that
        // `POST /auth/verify-mfa` would later upgrade into a live session.
        state
            .oidc_service
            .assert_provider_live_for_session(provider.id, &mfa_token)
            .await?;
        let encrypted_token =
            state
                .cookie_crypto
                .encrypt(&mfa_token)
                .map_err(|e| OidcError::DiscoveryFailed {
                    issuer: provider.issuer_url.clone(),
                    reason: format!("failed to encrypt MFA token: {e}"),
                })?;

        let mut headers = HeaderMap::new();
        let mfa_cookie = Cookie::build(("mfa_session", encrypted_token))
            .http_only(true)
            .path("/")
            .max_age(cookie::time::Duration::minutes(5))
            .same_site(cookie::SameSite::Strict)
            .secure(metadata.is_secure)
            .build();
        let cookie_header =
            mfa_cookie
                .to_string()
                .parse()
                .map_err(|e| OidcError::DiscoveryFailed {
                    issuer: provider.issuer_url.clone(),
                    reason: format!("failed to build MFA cookie header: {e}"),
                })?;
        headers.insert(SET_COOKIE, cookie_header);

        // Audit the SSO leg of an MFA-gated login here. The MFA-verify
        // endpoint emits its own follow-up audit on success; together they
        // tell the full story (SSO ok → MFA challenge issued → MFA verified).
        // Without this row, an attacker who stops at MFA never appears in
        // the audit log even though they completed a full IdP login.
        if let Err(e) = state
            .audit_service
            .create_audit_log(&LoginAudit {
                context: AuditContext {
                    user_id: user.id,
                    ip_address: Some(metadata.ip_address.to_string()),
                    user_agent: metadata.user_agent.as_str().to_string(),
                },
                success: true,
                login_method: "oidc-mfa-pending".to_string(),
            })
            .await
        {
            error!("Failed to create OIDC MFA-pending audit log: {}", e);
        }

        // Carry the IdP-supplied return_to through the MFA step.
        // `OidcService::sanitize_return_to` already enforced that
        // this is a same-origin relative path, so URL-encoding it
        // as a query param can't be turned into an open redirect.
        // The frontend's `MfaVerify` page prefers this query value
        // over its sessionStorage fallback — important because
        // sessionStorage doesn't survive a tab switch or a private
        // window, both of which are common when the user reaches
        // the IdP through an external link.
        let target = if return_to == "/dashboard" {
            "/mfa-verify".to_string()
        } else {
            format!("/mfa-verify?return_to={}", urlencoding::encode(&return_to))
        };
        return Ok((headers, Redirect::to(&target)).into_response());
    }

    let session_token = state
        .auth_service
        .create_oidc_session(user.id, provider.id)
        .await
        .map_err(|e| OidcError::DiscoveryFailed {
            issuer: provider.issuer_url.clone(),
            reason: format!("failed to create session: {e}"),
        })?;
    // SECURITY: a revocation (Cloud disconnect, provider delete/disable) that
    // landed while this login was talking to the IdP already deleted every
    // session the provider had issued — but not this one, which did not exist
    // yet. This re-checks the provider under the same row lock the revocation
    // holds and deletes the session again if it lost the race, so the cookie
    // below is never handed out for a provider the instance no longer trusts.
    state
        .oidc_service
        .assert_provider_live_for_session(provider.id, &session_token)
        .await?;
    let encrypted_token =
        state
            .cookie_crypto
            .encrypt(&session_token)
            .map_err(|e| OidcError::DiscoveryFailed {
                issuer: provider.issuer_url.clone(),
                reason: format!("failed to encrypt session token: {e}"),
            })?;
    let headers = state
        .auth_service
        .create_session_cookie(&encrypted_token, metadata.is_secure);

    if let Err(e) = state
        .audit_service
        .create_audit_log(&LoginAudit {
            context: AuditContext {
                user_id: user.id,
                ip_address: Some(metadata.ip_address.to_string()),
                user_agent: metadata.user_agent.as_str().to_string(),
            },
            success: true,
            // ADR-045 §4: distinguishes a Temps Cloud managed-provider login
            // from every other OIDC provider's login in the audit trail —
            // previously this was the constant `"oidc"` regardless of which
            // provider template authenticated the user.
            login_method: format!("oidc:{}", provider.template),
        })
        .await
    {
        error!("Failed to create OIDC login audit log: {}", e);
    }

    Ok((headers, Redirect::to(&return_to)).into_response())
}

fn redirect_login_error(reason: &str) -> Response {
    let encoded = urlencoding::encode(reason);
    Redirect::to(&format!("/login?error=oidc_failed&reason={encoded}")).into_response()
}

#[utoipa::path(
    post,
    path = "/admin/oidc/providers",
    request_body = CreateOidcProviderRequest,
    responses(
        (status = 201, description = "OIDC provider created", body = OidcProviderResponse),
        (status = 409, description = "Another OIDC provider already uses that name")
    ),
    tag = "Authentication",
    security(("bearer_auth" = []))
)]
pub async fn create_oidc_provider(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AuthState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<CreateOidcProviderRequest>,
) -> Result<(StatusCode, Json<OidcProviderResponse>), Problem> {
    permission_guard!(auth, SettingsWrite);
    crate::require_sensitive_action(
        state.sensitive_action_authorizer.as_ref(),
        &auth,
        SensitiveAction::CreateOidcProvider,
    )
    .await?;
    let provider = state.oidc_service.create_provider(request).await?;

    if let Err(e) = state
        .audit_service
        .create_audit_log(&OidcProviderCreatedAudit {
            context: AuditContext {
                user_id: auth.user_id(),
                ip_address: Some(metadata.ip_address.to_string()),
                user_agent: metadata.user_agent.as_str().to_string(),
            },
            provider_id: provider.id,
            name: provider.name.clone(),
            issuer_url: provider.issuer_url.clone(),
            template: provider.template.clone(),
            enabled: provider.enabled,
            jit_provisioning: provider.jit_provisioning,
            trust_idp_email: provider.trust_idp_email,
        })
        .await
    {
        error!("Failed to create OIDC provider audit log: {}", e);
    }

    // Emit anonymous telemetry. The `provider` property is allowlisted against
    // known type labels so free-form admin-supplied names are never transmitted.
    const KNOWN_OIDC_TEMPLATES: &[&str] = &[
        "generic", "google", "github", "keycloak", "okta", "auth0", "azure-ad",
    ];
    let provider_label = KNOWN_OIDC_TEMPLATES
        .iter()
        .find(|t| **t == provider.template.as_str())
        .copied();
    state.telemetry.report(
        temps_core::TelemetryEvent::new(temps_core::TelemetryEventKind::OidcProviderConfigured)
            .with_opt("provider", provider_label),
    );

    Ok((StatusCode::CREATED, Json(provider_to_response(&provider))))
}

#[utoipa::path(
    get,
    path = "/admin/oidc/providers",
    responses(
        (status = 200, description = "OIDC providers", body = Vec<OidcProviderResponse>)
    ),
    tag = "Authentication",
    security(("bearer_auth" = []))
)]
pub async fn list_oidc_providers(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AuthState>>,
) -> Result<Json<Vec<OidcProviderResponse>>, Problem> {
    permission_guard!(auth, SettingsWrite);
    let providers = state.oidc_service.list_providers().await?;
    Ok(Json(providers.iter().map(provider_to_response).collect()))
}

#[utoipa::path(
    patch,
    path = "/admin/oidc/providers/{provider_id}",
    request_body = UpdateOidcProviderRequest,
    responses(
        (status = 200, description = "OIDC provider updated", body = OidcProviderResponse)
    ),
    tag = "Authentication",
    security(("bearer_auth" = []))
)]
pub async fn update_oidc_provider(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AuthState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(provider_id): Path<i32>,
    Json(request): Json<UpdateOidcProviderRequest>,
) -> Result<Json<OidcProviderResponse>, Problem> {
    permission_guard!(auth, SettingsWrite);
    crate::require_sensitive_action(
        state.sensitive_action_authorizer.as_ref(),
        &auth,
        SensitiveAction::UpdateOidcProvider { provider_id },
    )
    .await?;

    // Capture which fields the PATCH touched *before* moving the
    // request into the service. The audit row is most useful when an
    // auditor can answer "was the client_secret rotated?" without
    // diffing the row history themselves. We don't log the new
    // values here; the provider row is the source of truth.
    let mut fields_changed = Vec::new();
    if request.name.is_some() {
        fields_changed.push("name".to_string());
    }
    if request.issuer_url.is_some() {
        fields_changed.push("issuer_url".to_string());
    }
    if request.client_id.is_some() {
        fields_changed.push("client_id".to_string());
    }
    if request.client_secret.is_some() {
        fields_changed.push("client_secret".to_string());
    }
    if request.scopes.is_some() {
        fields_changed.push("scopes".to_string());
    }
    if request.jit_provisioning.is_some() {
        fields_changed.push("jit_provisioning".to_string());
    }
    if request.enabled.is_some() {
        fields_changed.push("enabled".to_string());
    }
    if request.template.is_some() {
        fields_changed.push("template".to_string());
    }
    if request.group_claim.is_some() {
        fields_changed.push("group_claim".to_string());
    }
    if request.role_claim.is_some() {
        fields_changed.push("role_claim".to_string());
    }
    if request.default_role.is_some() {
        fields_changed.push("default_role".to_string());
    }
    if request.trust_idp_email.is_some() {
        fields_changed.push("trust_idp_email".to_string());
    }

    let provider = state
        .oidc_service
        .update_provider(provider_id, request)
        .await?;

    if let Err(e) = state
        .audit_service
        .create_audit_log(&OidcProviderUpdatedAudit {
            context: AuditContext {
                user_id: auth.user_id(),
                ip_address: Some(metadata.ip_address.to_string()),
                user_agent: metadata.user_agent.as_str().to_string(),
            },
            provider_id: provider.id,
            name: provider.name.clone(),
            fields_changed,
        })
        .await
    {
        error!("Failed to create OIDC provider update audit log: {}", e);
    }

    Ok(Json(provider_to_response(&provider)))
}

#[utoipa::path(
    delete,
    path = "/admin/oidc/providers/{provider_id}",
    responses((status = 204, description = "OIDC provider deleted")),
    tag = "Authentication",
    security(("bearer_auth" = []))
)]
pub async fn delete_oidc_provider(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AuthState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(provider_id): Path<i32>,
) -> Result<StatusCode, Problem> {
    permission_guard!(auth, SettingsWrite);

    // Snapshot identity *before* deletion so the audit row carries
    // the provider name + issuer even after the row is gone. We
    // tolerate a 404 here only by letting it propagate through the
    // delete call itself.
    let provider = state.oidc_service.get_provider(provider_id).await?;

    state.oidc_service.delete_provider(provider_id).await?;

    if let Err(e) = state
        .audit_service
        .create_audit_log(&OidcProviderDeletedAudit {
            context: AuditContext {
                user_id: auth.user_id(),
                ip_address: Some(metadata.ip_address.to_string()),
                user_agent: metadata.user_agent.as_str().to_string(),
            },
            provider_id: provider.id,
            name: provider.name,
            issuer_url: provider.issuer_url,
        })
        .await
    {
        error!("Failed to create OIDC provider delete audit log: {}", e);
    }

    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post,
    path = "/admin/oidc/providers/{provider_id}/test",
    responses(
        (status = 200, description = "Connection test result", body = OidcTestConnectionResponse)
    ),
    tag = "Authentication",
    security(("bearer_auth" = []))
)]
pub async fn test_oidc_provider(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AuthState>>,
    Path(provider_id): Path<i32>,
) -> Result<Json<OidcTestConnectionResponse>, Problem> {
    permission_guard!(auth, SettingsWrite);
    match state.oidc_service.test_connection(provider_id).await {
        Ok(message) => Ok(Json(OidcTestConnectionResponse {
            success: true,
            message,
        })),
        Err(err) => {
            // Full error (incl. upstream status / transport text) goes to the
            // server log only; the response carries `client_message`, which
            // does not reflect what the target host answered.
            warn!(
                provider_id,
                error = %err,
                "OIDC provider connection test failed"
            );
            Ok(Json(OidcTestConnectionResponse {
                success: false,
                message: err.client_message(),
            }))
        }
    }
}

#[utoipa::path(
    get,
    path = "/admin/oidc/providers/{provider_id}/users",
    params(
        ("provider_id" = i32, Path, description = "OIDC provider ID")
    ),
    responses(
        (status = 200, description = "Users authenticated via this OIDC provider", body = Vec<OidcProviderUserResponse>),
        (status = 404, description = "Provider not found")
    ),
    tag = "Authentication",
    security(("bearer_auth" = []))
)]
pub async fn list_oidc_provider_users(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AuthState>>,
    Path(provider_id): Path<i32>,
) -> Result<Json<Vec<OidcProviderUserResponse>>, Problem> {
    permission_guard!(auth, SettingsWrite);
    let users = state
        .oidc_service
        .list_users_for_provider(provider_id)
        .await?;
    Ok(Json(users.iter().map(provider_user_to_response).collect()))
}

#[utoipa::path(
    get,
    path = "/admin/oidc/providers/{provider_id}/role-mappings",
    responses(
        (status = 200, description = "OIDC role mappings", body = Vec<OidcRoleMappingResponse>)
    ),
    tag = "Authentication",
    security(("bearer_auth" = []))
)]
pub async fn list_oidc_role_mappings(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AuthState>>,
    Path(provider_id): Path<i32>,
) -> Result<Json<Vec<OidcRoleMappingResponse>>, Problem> {
    permission_guard!(auth, SettingsWrite);
    let mappings = state.oidc_service.list_role_mappings(provider_id).await?;
    Ok(Json(mappings))
}

#[utoipa::path(
    post,
    path = "/admin/oidc/providers/{provider_id}/role-mappings",
    request_body = CreateOidcRoleMappingRequest,
    responses(
        (status = 201, description = "Role mapping created", body = OidcRoleMappingResponse)
    ),
    tag = "Authentication",
    security(("bearer_auth" = []))
)]
pub async fn create_oidc_role_mapping(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AuthState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(provider_id): Path<i32>,
    Json(request): Json<CreateOidcRoleMappingRequest>,
) -> Result<(StatusCode, Json<OidcRoleMappingResponse>), Problem> {
    permission_guard!(auth, SettingsWrite);
    crate::require_sensitive_action(
        state.sensitive_action_authorizer.as_ref(),
        &auth,
        SensitiveAction::CreateOidcRoleMapping { provider_id },
    )
    .await?;
    let mapping = state
        .oidc_service
        .create_role_mapping(provider_id, request)
        .await?;

    if let Err(e) = state
        .audit_service
        .create_audit_log(&OidcRoleMappingCreatedAudit {
            context: AuditContext {
                user_id: auth.user_id(),
                ip_address: Some(metadata.ip_address.to_string()),
                user_agent: metadata.user_agent.as_str().to_string(),
            },
            provider_id,
            mapping_id: mapping.id,
            idp_group: mapping.idp_group.clone(),
            role: mapping.role.clone(),
            priority: mapping.priority,
        })
        .await
    {
        error!("Failed to create OIDC role mapping audit log: {}", e);
    }

    Ok((StatusCode::CREATED, Json(mapping)))
}

#[utoipa::path(
    delete,
    path = "/admin/oidc/role-mappings/{mapping_id}",
    responses((status = 204, description = "Role mapping deleted")),
    tag = "Authentication",
    security(("bearer_auth" = []))
)]
pub async fn delete_oidc_role_mapping(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AuthState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(mapping_id): Path<i32>,
) -> Result<StatusCode, Problem> {
    permission_guard!(auth, SettingsWrite);
    crate::require_sensitive_action(
        state.sensitive_action_authorizer.as_ref(),
        &auth,
        SensitiveAction::DeleteOidcRoleMapping { mapping_id },
    )
    .await?;
    state.oidc_service.delete_role_mapping(mapping_id).await?;

    if let Err(e) = state
        .audit_service
        .create_audit_log(&OidcRoleMappingDeletedAudit {
            context: AuditContext {
                user_id: auth.user_id(),
                ip_address: Some(metadata.ip_address.to_string()),
                user_agent: metadata.user_agent.as_str().to_string(),
            },
            mapping_id,
        })
        .await
    {
        error!("Failed to create OIDC role mapping delete audit log: {}", e);
    }

    Ok(StatusCode::NO_CONTENT)
}

#[derive(OpenApi)]
#[openapi(
    paths(
        list_public_providers,
        start_oidc_login_by_slug,
        start_managed_cloud_login,
        oidc_callback,
        create_oidc_provider,
        list_oidc_providers,
        update_oidc_provider,
        delete_oidc_provider,
        test_oidc_provider,
        list_oidc_provider_users,
        list_oidc_role_mappings,
        create_oidc_role_mapping,
        delete_oidc_role_mapping,
    ),
    components(
        schemas(
            OidcProvidersListResponse,
            CreateOidcProviderRequest,
            OidcProviderResponse,
            OidcProviderSummary,
            OidcProviderUserResponse,
            UpdateOidcProviderRequest,
            OidcTestConnectionResponse,
            OidcRoleMappingResponse,
            CreateOidcRoleMappingRequest,
        )
    ),
    tags(
        (name = "Authentication", description = "Authentication and authorization endpoints")
    )
)]
pub struct OidcApiDoc;

#[cfg(test)]
mod login_binding_tests {
    use super::*;
    use axum::http::header::COOKIE;

    #[test]
    fn the_callback_documents_its_parameters_in_the_query() {
        let doc = serde_json::to_value(OidcApiDoc::openapi()).expect("spec serializes");
        let params = doc["paths"]["/auth/oidc/callback"]["get"]["parameters"]
            .as_array()
            .expect("the callback documents its parameters");
        let located: Vec<(&str, &str)> = params
            .iter()
            .map(|p| (p["name"].as_str().unwrap(), p["in"].as_str().unwrap()))
            .collect();
        assert_eq!(
            located,
            [
                ("code", "query"),
                ("state", "query"),
                ("error", "query"),
                ("error_description", "query"),
            ]
        );
    }

    #[test]
    fn only_registered_provider_error_codes_are_logged_verbatim() {
        assert_eq!(provider_error_code("access_denied"), "access_denied");
        assert_eq!(provider_error_code("invalid_scope"), "invalid_scope");
        assert_eq!(provider_error_code("login_required"), "login_required");
        for crafted in [
            "",
            "ACCESS_DENIED",
            "access_denied ",
            "account locked for user@example.test",
            "access_denied\nforged log line",
        ] {
            assert_eq!(provider_error_code(crafted), "unregistered", "{crafted:?}");
        }
    }

    fn crypto() -> CookieCrypto {
        CookieCrypto::new("0123456789abcdef0123456789abcdef").unwrap()
    }

    /// The `name=value` pair a browser would send back for a `Set-Cookie`.
    fn sent_back(set_cookie: &HeaderValue) -> String {
        set_cookie
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string()
    }

    fn request_with(cookies: &[String]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for cookie in cookies {
            headers.append(COOKIE, HeaderValue::from_str(cookie).unwrap());
        }
        headers
    }

    #[test]
    fn the_browser_that_started_a_login_is_bound_to_it() {
        let crypto = crypto();
        let set = login_binding_cookie(&crypto, "state-one", false).unwrap();
        let headers = request_with(&[sent_back(&set)]);
        assert!(login_is_bound_to_browser(
            &crypto,
            &headers,
            "state-one",
            false
        ));
    }

    #[test]
    fn a_browser_without_the_cookie_is_not_bound() {
        let crypto = crypto();
        assert!(!login_is_bound_to_browser(
            &crypto,
            &HeaderMap::new(),
            "state-one",
            false
        ));
        let unrelated = request_with(&["session=abc".to_string(), "theme=dark".to_string()]);
        assert!(!login_is_bound_to_browser(
            &crypto,
            &unrelated,
            "state-one",
            false
        ));
    }

    #[test]
    fn another_logins_cookie_does_not_bind_this_state() {
        // Someone else's login (state-two) must not satisfy state-one, even
        // when this browser holds a perfectly valid cookie for that other login.
        let crypto = crypto();
        let other = login_binding_cookie(&crypto, "state-two", false).unwrap();
        let headers = request_with(&[sent_back(&other)]);
        assert!(login_is_bound_to_browser(
            &crypto,
            &headers,
            "state-two",
            false
        ));
        assert!(!login_is_bound_to_browser(
            &crypto,
            &headers,
            "state-one",
            false
        ));
    }

    #[test]
    fn a_cookie_sealed_for_a_different_state_is_refused() {
        // The cookie is found by the name derived from the callback's state, but
        // its sealed value must also be that state.
        let crypto = crypto();
        let sealed_for_two = crypto.encrypt("state-two").unwrap();
        let forged = format!(
            "{}={}",
            login_binding_cookie_name("state-one", false),
            sealed_for_two
        );
        let headers = request_with(&[forged]);
        assert!(!login_is_bound_to_browser(
            &crypto,
            &headers,
            "state-one",
            false
        ));
    }

    #[test]
    fn a_tampered_or_foreign_value_is_refused() {
        let crypto = crypto();
        let name = login_binding_cookie_name("state-one", false);
        for value in ["", "not-sealed", "state-one", "AAAA"] {
            let headers = request_with(&[format!("{name}={value}")]);
            assert!(!login_is_bound_to_browser(
                &crypto,
                &headers,
                "state-one",
                false
            ));
        }
        // Sealed under a different key (another instance, or an attacker).
        let other = CookieCrypto::new("fedcba9876543210fedcba9876543210").unwrap();
        let foreign = other.encrypt("state-one").unwrap();
        let headers = request_with(&[format!("{name}={foreign}")]);
        assert!(!login_is_bound_to_browser(
            &crypto,
            &headers,
            "state-one",
            false
        ));
    }

    #[test]
    fn logins_in_several_tabs_do_not_overwrite_each_other() {
        let crypto = crypto();
        let first = sent_back(&login_binding_cookie(&crypto, "state-one", false).unwrap());
        let second = sent_back(&login_binding_cookie(&crypto, "state-two", false).unwrap());
        assert_ne!(first.split('=').next(), second.split('=').next());
        let headers = request_with(&[format!("{first}; {second}")]);
        assert!(login_is_bound_to_browser(
            &crypto,
            &headers,
            "state-one",
            false
        ));
        assert!(login_is_bound_to_browser(
            &crypto,
            &headers,
            "state-two",
            false
        ));
        // HTTP/2 clients may send each cookie in its own header.
        let split = request_with(&[first, second]);
        assert!(login_is_bound_to_browser(
            &crypto,
            &split,
            "state-one",
            false
        ));
        assert!(login_is_bound_to_browser(
            &crypto,
            &split,
            "state-two",
            false
        ));
    }

    #[test]
    fn the_cookie_name_does_not_reveal_the_state() {
        let name = login_binding_cookie_name("a-very-recognisable-state-value", false);
        assert!(name.starts_with("oidc_login_"));
        assert!(!name.contains("recognisable"));
        assert_eq!(name.len(), "oidc_login_".len() + 16);
        assert_ne!(name, login_binding_cookie_name("another-state", false));
    }

    #[test]
    fn the_binding_cookie_has_the_intended_attributes() {
        let crypto = crypto();
        let plain = login_binding_cookie(&crypto, "state-one", false).unwrap();
        let text = plain.to_str().unwrap();
        assert!(text.contains("HttpOnly"), "{text}");
        assert!(
            text.contains("Path=/") && !text.contains("Path=/api"),
            "{text}"
        );
        assert!(text.contains("Max-Age=600"), "{text}");
        // Lax so it is sent when the issuer redirects back; never Strict, which
        // browsers withhold on that cross-site navigation.
        assert!(text.contains("SameSite=Lax"), "{text}");
        assert!(!text.contains("Strict"), "{text}");
        assert!(!text.contains("Secure"), "{text}");
        assert!(!text.contains("state-one"), "{text}");

        let secure = login_binding_cookie(&crypto, "state-one", true).unwrap();
        assert!(secure.to_str().unwrap().contains("Secure"));
    }

    #[test]
    fn clearing_removes_exactly_this_logins_cookie() {
        let clear = clear_login_binding_cookie("state-one", true).unwrap();
        let text = clear.to_str().unwrap();
        assert!(
            text.starts_with(&format!(
                "{}=;",
                login_binding_cookie_name("state-one", true)
            )),
            "{text}"
        );
        assert!(text.contains("Max-Age=0"), "{text}");
        assert!(
            text.contains("Path=/") && !text.contains("Path=/api"),
            "{text}"
        );
        assert!(
            text.contains("SameSite=Lax") && text.contains("Secure"),
            "{text}"
        );
    }

    #[test]
    fn over_https_the_cookie_is_host_only_and_a_planted_one_is_ignored() {
        let crypto = crypto();
        let set = login_binding_cookie(&crypto, "state-one", true).unwrap();
        let text = set.to_str().unwrap();
        // Browsers enforce these together for a `__Host-` cookie, which is what
        // keeps an app on a sibling subdomain from setting one.
        assert!(text.starts_with("__Host-oidc_login_"), "{text}");
        assert!(text.contains("Secure") && text.contains("Path=/"), "{text}");
        assert!(!text.contains("Domain"), "{text}");
        let headers = request_with(&[sent_back(&set)]);
        assert!(login_is_bound_to_browser(
            &crypto,
            &headers,
            "state-one",
            true
        ));

        // A sibling app can set an unprefixed cookie holding a sealed value it
        // copied from its own login. It must not count over https.
        let planted = format!(
            "{}={}",
            login_binding_cookie_name("state-one", false),
            crypto.encrypt("state-one").unwrap()
        );
        let headers = request_with(&[planted]);
        assert!(!login_is_bound_to_browser(
            &crypto,
            &headers,
            "state-one",
            true
        ));
    }

    #[test]
    fn over_http_the_name_has_no_host_prefix() {
        // Browsers refuse the `__Host-` prefix without `Secure`.
        let plain = login_binding_cookie_name("state-one", false);
        assert!(plain.starts_with("oidc_login_"), "{plain}");
        assert_ne!(plain, login_binding_cookie_name("state-one", true));
    }

    #[test]
    fn constant_time_eq_matches_equality() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert!(constant_time_eq(b"", b""));
    }
}

/// The binding wired into the real handlers: the start redirect, and the
/// callback route over a migrated test database.
#[cfg(test)]
mod login_binding_http_tests {
    use super::*;
    use crate::http_test_support::{auth_state, insert_request_metadata};
    use crate::oidc_types::CreateOidcProviderRequest;
    use axum::{body::Body, http::Request, routing::get};
    use chrono::Utc;
    use openidconnect::core::{
        CoreJwsSigningAlgorithm, CoreProviderMetadata, CoreResponseType, CoreSubjectIdentifierType,
    };
    use openidconnect::{
        AuthUrl, EmptyAdditionalProviderMetadata, IssuerUrl, JsonWebKeySetUrl, ResponseTypes,
    };
    use sea_orm::{ActiveModelTrait, ActiveValue::Set, ColumnTrait, EntityTrait, QueryFilter};
    use temps_database::test_utils::TestDatabase;
    use temps_entities::{oidc_login_states, oidc_providers};
    use tower::ServiceExt;

    struct Fixture {
        _db: TestDatabase,
        state: Arc<AuthState>,
        app: Router,
    }

    async fn test_db() -> Option<TestDatabase> {
        match TestDatabase::with_migrations().await {
            Ok(db) => Some(db),
            Err(error)
                if temps_database::test_utils::is_container_runtime_unavailable(
                    &error.to_string(),
                ) =>
            {
                println!("Docker not available, skipping test: {error}");
                None
            }
            Err(error) => panic!("test database setup failed: {error}"),
        }
    }

    fn discovery_metadata(issuer: &str) -> CoreProviderMetadata {
        CoreProviderMetadata::new(
            IssuerUrl::new(issuer.to_string()).unwrap(),
            AuthUrl::new(format!("{issuer}/authorize")).unwrap(),
            JsonWebKeySetUrl::new(format!("{issuer}/jwks")).unwrap(),
            vec![ResponseTypes::new(vec![CoreResponseType::Code])],
            vec![CoreSubjectIdentifierType::Public],
            vec![CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha256],
            EmptyAdditionalProviderMetadata {},
        )
    }

    fn provider_request(name: &str, issuer: &str, enabled: bool) -> CreateOidcProviderRequest {
        CreateOidcProviderRequest {
            name: name.to_string(),
            issuer_url: issuer.to_string(),
            client_id: "client".to_string(),
            client_secret: "secret".to_string().into(),
            scopes: "openid email profile".to_string(),
            jit_provisioning: true,
            enabled,
            template: "generic".to_string(),
            group_claim: "groups".to_string(),
            role_claim: "roles".to_string(),
            default_role: "user".to_string(),
            trust_idp_email: false,
        }
    }

    fn router(state: &Arc<AuthState>) -> Router {
        Router::new()
            .route("/auth/oidc/login/{slug}", get(start_oidc_login_by_slug))
            .route("/auth/oidc/cloud/login", get(start_managed_cloud_login))
            .route("/auth/oidc/callback", get(oidc_callback))
            .layer(axum::middleware::from_fn(insert_request_metadata))
            .with_state(state.clone())
    }

    async fn fixture(pending_states: &[&str]) -> Option<Fixture> {
        let db = test_db().await?;
        let state = auth_state(db.db.clone());
        // Disabled, so a callback that passes the binding check stops right
        // after consuming its state instead of reaching out to an issuer.
        let provider = state
            .oidc_service
            .create_provider(provider_request(
                "Binding Test",
                "https://issuer.example.com",
                false,
            ))
            .await
            .expect("create provider");
        for pending in pending_states {
            oidc_login_states::ActiveModel {
                state: Set(pending.to_string()),
                nonce: Set("nonce".to_string()),
                pkce_verifier: Set("verifier".to_string()),
                provider_id: Set(provider.id),
                return_to: Set(None),
                expires_at: Set(Utc::now() + chrono::Duration::minutes(10)),
                ..Default::default()
            }
            .insert(db.db.as_ref())
            .await
            .expect("insert login state");
        }
        let app = router(&state);
        Some(Fixture {
            _db: db,
            state,
            app,
        })
    }

    fn metadata() -> RequestMetadata {
        RequestMetadata {
            ip_address: "127.0.0.1".to_string(),
            user_agent: "temps-auth-test".to_string(),
            headers: HeaderMap::new(),
            visitor_id_cookie: None,
            session_id_cookie: None,
            base_url: "http://temps.test".to_string(),
            scheme: "http".to_string(),
            host: "temps.test".to_string(),
            is_secure: false,
        }
    }

    async fn callback(
        app: &Router,
        query: &str,
        cookie: Option<&str>,
    ) -> (StatusCode, String, Vec<String>) {
        get_path(app, &format!("/auth/oidc/callback?{query}"), cookie).await
    }

    /// `(status, Location, Set-Cookie values)` of a GET request.
    async fn get_path(
        app: &Router,
        uri: &str,
        cookie: Option<&str>,
    ) -> (StatusCode, String, Vec<String>) {
        let mut request = Request::builder().uri(uri);
        if let Some(cookie) = cookie {
            request = request.header(COOKIE, cookie);
        }
        let response = app
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let location = response
            .headers()
            .get("location")
            .map(|value| value.to_str().unwrap().to_string())
            .unwrap_or_default();
        let set_cookies = response
            .headers()
            .get_all(SET_COOKIE)
            .iter()
            .map(|value| value.to_str().unwrap().to_string())
            .collect();
        (response.status(), location, set_cookies)
    }

    async fn is_pending(fixture: &Fixture, state: &str) -> bool {
        oidc_login_states::Entity::find()
            .filter(oidc_login_states::Column::State.eq(state))
            .one(fixture._db.db.as_ref())
            .await
            .unwrap()
            .is_some()
    }

    fn clears_binding_cookie(set_cookies: &[String], state: &str) -> bool {
        let name = login_binding_cookie_name(state, false);
        set_cookies
            .iter()
            .any(|cookie| cookie.starts_with(&format!("{name}=;")) && cookie.contains("Max-Age=0"))
    }

    #[tokio::test]
    async fn a_callback_from_another_browser_is_refused_and_leaves_the_login_pending() {
        let Some(fixture) = fixture(&["state-one", "state-two"]).await else {
            return;
        };
        let crypto = &fixture.state.cookie_crypto;
        let other_browser_cookie = {
            let set = login_binding_cookie(crypto, "state-two", false).unwrap();
            set.to_str().unwrap().split(';').next().unwrap().to_string()
        };
        let tampered = format!("{}=AAAA", login_binding_cookie_name("state-one", false));

        let attempts = [
            ("no cookie", None),
            (
                "a cookie for a different login",
                Some(other_browser_cookie.as_str()),
            ),
            ("a tampered cookie", Some(tampered.as_str())),
        ];
        for (label, cookie) in attempts {
            let (status, location, set_cookies) =
                callback(&fixture.app, "code=abc&state=state-one", cookie).await;
            assert_eq!(status, StatusCode::SEE_OTHER, "{label}");
            assert!(
                location.contains("reason=state_invalid"),
                "{label}: {location}"
            );
            assert!(clears_binding_cookie(&set_cookies, "state-one"), "{label}");
            assert!(
                is_pending(&fixture, "state-one").await,
                "{label}: a refused callback must not consume the login"
            );
            assert!(is_pending(&fixture, "state-two").await, "{label}");
        }
    }

    #[tokio::test]
    async fn the_start_redirect_sets_the_cookie_the_callback_accepts() {
        let Some(fixture) = fixture(&["state-one"]).await else {
            return;
        };
        let response = redirect_to_issuer(
            &fixture.state,
            &metadata(),
            &OidcLoginStart {
                authorize_url: "https://issuer.example.com/authorize?state=state-one".to_string(),
                state: "state-one".to_string(),
            },
        )
        .unwrap();
        assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
        assert_eq!(
            response.headers().get("location").unwrap(),
            "https://issuer.example.com/authorize?state=state-one"
        );
        assert_eq!(response.headers()[CACHE_CONTROL], "no-store");
        assert_eq!(response.headers()[REFERRER_POLICY], "no-referrer");
        let set_cookies: Vec<_> = response.headers().get_all(SET_COOKIE).iter().collect();
        assert_eq!(set_cookies.len(), 1);
        let cookie = set_cookies[0]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string();

        let (status, location, set_cookies) =
            callback(&fixture.app, "code=abc&state=state-one", Some(&cookie)).await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        // The binding passed: the login was consumed and failed later (the
        // provider is disabled), not at the binding check.
        assert!(!location.contains("reason=state_invalid"), "{location}");
        assert!(!is_pending(&fixture, "state-one").await);
        assert!(clears_binding_cookie(&set_cookies, "state-one"));

        // The same link and cookie cannot be used twice.
        let (_, location, _) =
            callback(&fixture.app, "code=abc&state=state-one", Some(&cookie)).await;
        assert!(location.contains("reason=state_invalid"), "{location}");
    }

    fn query_param(url: &str, key: &str) -> String {
        let query = url.split_once('?').expect("a query string").1;
        query
            .split('&')
            .find_map(|pair| pair.strip_prefix(&format!("{key}=")))
            .unwrap_or_else(|| panic!("no {key} in {url}"))
            .to_string()
    }

    /// Both real start routes, with the `state` they actually issue.
    #[tokio::test]
    async fn both_start_routes_bind_the_login_to_the_browser_that_started_it() {
        let Some(db) = test_db().await else {
            return;
        };
        let state = auth_state(db.db.clone());
        let service = &state.oidc_service;
        let ordinary = service
            .create_provider(provider_request(
                "Start Test",
                "https://issuer.example.com",
                true,
            ))
            .await
            .unwrap();
        let managed = service
            .upsert_managed_cloud_provider(crate::oidc_service::ManagedCloudOidcConfig {
                issuer: "https://cloud.example.com".to_string(),
                client_id: "cloud-client".to_string(),
                client_secret: "cloud-secret".to_string(),
            })
            .await
            .unwrap();
        service
            .seed_discovery_for_test(
                &ordinary,
                discovery_metadata("https://issuer.example.com"),
                "secret",
            )
            .await;
        service
            .seed_discovery_for_test(
                &managed,
                discovery_metadata("https://cloud.example.com"),
                "cloud-secret",
            )
            .await;
        let fixture = Fixture {
            _db: db,
            app: router(&state),
            state: state.clone(),
        };

        let slug = crate::oidc_types::derive_provider_slug(ordinary.id, &ordinary.name);
        let routes = [
            (
                format!("/auth/oidc/login/{slug}"),
                "https://issuer.example.com/authorize",
                ordinary.id,
            ),
            (
                "/auth/oidc/cloud/login".to_string(),
                "https://cloud.example.com/authorize",
                managed.id,
            ),
        ];
        for (path, authorize, provider_id) in routes {
            let (status, location, set_cookies) = get_path(&fixture.app, &path, None).await;
            assert_eq!(status, StatusCode::TEMPORARY_REDIRECT, "{path}");
            assert!(location.starts_with(authorize), "{path}: {location}");
            let login_state = query_param(&location, "state");
            assert!(is_pending(&fixture, &login_state).await, "{path}");

            assert_eq!(set_cookies.len(), 1, "{path}: {set_cookies:?}");
            let cookie = set_cookies[0].split(';').next().unwrap().to_string();
            assert!(
                cookie.starts_with(&login_binding_cookie_name(&login_state, false)),
                "{path}: {cookie}"
            );

            // Another browser (no cookie) follows this login's callback.
            let query = format!("code=abc&state={login_state}");
            let (_, location, _) = callback(&fixture.app, &query, None).await;
            assert!(
                location.contains("reason=state_invalid"),
                "{path}: {location}"
            );
            assert!(is_pending(&fixture, &login_state).await, "{path}");

            // The browser that started it. The provider is switched off first
            // so that, once the binding passes, the login stops without
            // reaching an issuer.
            let mut row: oidc_providers::ActiveModel =
                oidc_providers::Entity::find_by_id(provider_id)
                    .one(fixture._db.db.as_ref())
                    .await
                    .unwrap()
                    .unwrap()
                    .into();
            row.enabled = Set(false);
            row.update(fixture._db.db.as_ref()).await.unwrap();
            let (status, location, _) = callback(&fixture.app, &query, Some(&cookie)).await;
            assert_eq!(status, StatusCode::SEE_OTHER, "{path}");
            assert!(
                !location.contains("reason=state_invalid"),
                "{path}: {location}"
            );
            assert!(!is_pending(&fixture, &login_state).await, "{path}");
        }
    }

    #[tokio::test]
    async fn a_provider_error_is_still_reported_as_one() {
        let Some(fixture) = fixture(&[]).await else {
            return;
        };
        let (status, location, _) = callback(
            &fixture.app,
            "error=access_denied&error_description=no",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        assert!(location.contains("reason=idp_error"), "{location}");
        let (_, location, _) = callback(&fixture.app, "code=abc", None).await;
        assert!(location.contains("reason=callback_invalid"), "{location}");
    }

    /// Every callback outcome carries the code and state in its URL, so none
    /// of them may be cached or leak through `Referer`.
    #[tokio::test]
    async fn every_callback_outcome_is_no_store_and_no_referrer() {
        let Some(fixture) = fixture(&["state-one"]).await else {
            return;
        };
        for query in [
            // Refused at the browser binding.
            "code=abc&state=state-one",
            // Provider error.
            "error=access_denied&error_description=account%20locked",
            // Malformed.
            "code=abc",
            // Does not deserialize at all: a repeated field.
            "code=abc&state=state-one&state=state-two",
        ] {
            let response = fixture
                .app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!("/auth/oidc/callback?{query}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::SEE_OTHER, "{query}");
            assert_eq!(response.headers()[CACHE_CONTROL], "no-store", "{query}");
            assert_eq!(
                response.headers()[REFERRER_POLICY],
                "no-referrer",
                "{query}"
            );
            let location = response.headers()["location"].to_str().unwrap();
            assert!(!location.contains("locked"), "{location}");
        }
    }
}

#[cfg(test)]
mod callback_schema_tests {
    use super::*;

    #[test]
    fn callback_parameters_are_query_parameters() {
        let parameters = OidcCallbackQuery::into_params(|| None);
        assert_eq!(parameters.len(), 4);
        for parameter in parameters {
            let json = serde_json::to_value(parameter).unwrap();
            assert_eq!(json["in"], "query");
        }
    }
}
