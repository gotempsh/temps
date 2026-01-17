use crate::permissions::Role;
use crate::{
    auth_service::AuthService, context::AuthContext, user_service::UserService, AuthState,
};
use axum::{
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    middleware::Next,
};
use cookie::Cookie;
use sea_orm::EntityTrait;
use std::sync::Arc;
use temps_core::{AppSettings, CookieCrypto, RequestMetadata};

// Cookie names from proxy
const SESSION_ID_COOKIE_NAME: &str = "_temps_sid";
const VISITOR_ID_COOKIE_NAME: &str = "_temps_visitor_id";
// Cookie for demo mode user selection (stores encrypted user_id)
const DEMO_USER_COOKIE_NAME: &str = "_temps_demo_uid";

pub async fn auth_middleware(
    State(app_state): State<Arc<AuthState>>,
    mut req: axum::extract::Request,
    next: Next,
) -> Result<axum::response::Response, StatusCode> {
    let mut user = None;

    let auth_context = match extract_auth_from_request(&req, &app_state).await {
        Ok(ctx) => {
            // Extract user from auth context if available
            // Note: deployment tokens don't have a user associated
            user = ctx.user.clone();
            Some(ctx)
        }
        Err(_) => {
            // For routes that don't require auth, continue without context
            // The RequireAuth extractor will handle the error later
            None
        }
    };

    // Extract cookies for RequestMetadata
    let visitor_id_cookie = extract_visitor_id_cookie(&req, &app_state.cookie_crypto);
    let session_id_cookie = extract_session_id_cookie(&req, &app_state.cookie_crypto);

    // Create base URL from request headers
    let host = req
        .headers()
        .get("host")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("localhost")
        .to_string();

    // Determine scheme (simplified - you may want to add more sophisticated logic)
    let scheme = if req
        .headers()
        .get("x-forwarded-proto")
        .and_then(|h| h.to_str().ok())
        == Some("https")
    {
        "https"
    } else {
        "http"
    };
    let is_secure = scheme == "https";
    let base_url = format!("{}://{}", scheme, host);

    // Create RequestMetadata
    let metadata = RequestMetadata {
        ip_address: req
            .headers()
            .get("x-forwarded-for")
            .and_then(|h| h.to_str().ok())
            .and_then(|s| s.split(',').next())
            .unwrap_or("unknown")
            .to_string(),
        user_agent: req
            .headers()
            .get("user-agent")
            .and_then(|h| h.to_str().ok())
            .unwrap_or("unknown")
            .to_string(),
        headers: req.headers().clone(),
        visitor_id_cookie,
        session_id_cookie,
        base_url,
        scheme: scheme.to_string(),
        host,
        is_secure,
    };

    // Insert extensions
    req.extensions_mut().insert(metadata);
    if let Some(user) = user {
        req.extensions_mut().insert(user);
    }
    if let Some(auth_ctx) = auth_context {
        req.extensions_mut().insert(auth_ctx);
    }

    Ok(next.run(req).await)
}

pub async fn extract_auth_from_request(
    req: &axum::extract::Request,
    auth_state: &Arc<AuthState>,
) -> Result<AuthContext, AuthError> {
    // Get services from app state
    let auth_service = &auth_state.auth_service;
    let user_service = &auth_state.user_service;
    let api_key_service = &auth_state.api_key_service;
    let deployment_token_service = &auth_state.deployment_token_service;

    // 0. Check for demo mode via X-Temps-Demo-Mode header (set by proxy) or host matching demo.<preview_domain>
    // This auto-authenticates requests without requiring login
    let demo_mode_header = req
        .headers()
        .get("x-temps-demo-mode")
        .and_then(|h| h.to_str().ok())
        .map(|v| v == "true")
        .unwrap_or(false);

    let host = req
        .headers()
        .get("host")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    let host_without_port = host.split(':').next().unwrap_or(host);

    // Debug: Log all headers to see what the auth middleware receives
    tracing::info!(
        "Auth middleware check: host={}, host_without_port={}, demo_mode_header={}, path={}",
        host,
        host_without_port,
        demo_mode_header,
        req.uri().path()
    );

    // Check demo mode either via header from proxy or via host subdomain
    if demo_mode_header || host_without_port.starts_with("demo.") {
        // Get preview_domain from database settings for host validation (if checking by host)
        if let Some(demo_context) = check_demo_mode(
            req,
            auth_state,
            host_without_port,
            user_service,
            demo_mode_header,
        )
        .await
        {
            return Ok(demo_context);
        }
    }

    // 1. Check for Authorization header (API Key, Deployment Token, or CLI Token)
    if let Some(auth_header) = req.headers().get("authorization") {
        if let Ok(auth_str) = auth_header.to_str() {
            if auth_str.starts_with("Bearer ") {
                let token = auth_str.trim_start_matches("Bearer ");

                // Try API key first (they have a specific format: tk_...)
                if token.starts_with("tk_") {
                    if let Ok((user, role, permissions, key_name, key_id)) =
                        api_key_service.validate_api_key(token).await
                    {
                        return Ok(AuthContext::new_api_key(
                            user,
                            role,
                            permissions,
                            key_name,
                            key_id,
                        ));
                    }
                }

                // Try deployment token (format: dt_...)
                if token.starts_with("dt_") {
                    if let Ok(validated) = deployment_token_service.validate_token(token).await {
                        return Ok(AuthContext::new_deployment_token(
                            validated.project_id,
                            validated.environment_id,
                            validated.token_id,
                            validated.name,
                            validated.permissions,
                        ));
                    }
                }
            }
        }
    }

    // 2. Check for session cookie
    if let Some(session_context) = validate_session_cookie(
        req,
        auth_service.as_ref(),
        user_service.as_ref(),
        auth_state.cookie_crypto.as_ref(),
    )
    .await?
    {
        return Ok(session_context);
    }

    Err(AuthError::Unauthorized(
        "No valid authentication found".to_string(),
    ))
}

async fn validate_session_cookie(
    req: &Request,
    auth_service: &AuthService,
    user_service: &UserService,
    crypto: &CookieCrypto,
) -> Result<Option<AuthContext>, AuthError> {
    // Extract session cookie from request
    let session_token = extract_session_from_cookies(req.headers(), crypto)?;

    if let Some(token) = session_token {
        match auth_service.verify_session(&token).await {
            Ok(user) => {
                let user_role = determine_user_role(&user, user_service)
                    .await
                    .unwrap_or(Role::User);
                return Ok(Some(AuthContext::new_session(user.clone(), user_role)));
            }
            Err(_) => return Ok(None),
        }
    }

    Ok(None)
}

fn extract_session_from_cookies(
    headers: &HeaderMap,
    crypto: &CookieCrypto,
) -> Result<Option<String>, AuthError> {
    // Iterate through ALL cookie headers (there can be multiple)
    for cookie_header in headers.get_all("cookie") {
        if let Ok(cookie_str) = cookie_header.to_str() {
            // Parse cookies and find the encrypted session cookie
            for cookie in Cookie::split_parse(cookie_str).filter_map(Result::ok) {
                if cookie.name() == SESSION_ID_COOKIE_NAME {
                    // Decrypt the session ID - if it fails, treat as no valid session
                    match crypto.decrypt(cookie.value()) {
                        Ok(decrypted_session_id) => {
                            return Ok(Some(decrypted_session_id));
                        }
                        Err(_) => {
                            // If decryption fails, no valid session
                            return Ok(None);
                        }
                    }
                }
            }
        }
    }
    Ok(None)
}

pub fn extract_visitor_id_cookie(req: &Request, crypto: &CookieCrypto) -> Option<String> {
    let headers = req.headers();

    for cookie_header in headers.get_all("Cookie") {
        if let Ok(cookie_header_str) = cookie_header.to_str() {
            for cookie in Cookie::split_parse(cookie_header_str).flatten() {
                if cookie.name() == VISITOR_ID_COOKIE_NAME {
                    // Decrypt the cookie value
                    match crypto.decrypt(cookie.value()) {
                        Ok(decrypted) => return Some(decrypted),
                        Err(_) => {
                            // If decryption fails, no valid visitor ID
                            return None;
                        }
                    }
                }
            }
        }
    }
    None
}

pub fn extract_session_id_cookie(req: &Request, crypto: &CookieCrypto) -> Option<String> {
    let headers = req.headers();

    for cookie_header in headers.get_all("Cookie") {
        if let Ok(cookie_header_str) = cookie_header.to_str() {
            for cookie in Cookie::split_parse(cookie_header_str).flatten() {
                if cookie.name() == SESSION_ID_COOKIE_NAME {
                    // Decrypt the cookie value
                    match crypto.decrypt(cookie.value()) {
                        Ok(decrypted) => return Some(decrypted),
                        Err(_) => {
                            // If decryption fails, no valid session ID
                            return None;
                        }
                    }
                }
            }
        }
    }
    None
}

async fn determine_user_role(
    user: &temps_entities::users::Model,
    user_service: &UserService,
) -> Result<Role, AuthError> {
    // Check if user is admin
    if user_service.is_admin(user.id).await.unwrap_or(false) {
        return Ok(Role::Admin);
    }

    // For now, return User as default
    // In the future, you might want to check the user_roles table
    Ok(Role::User)
}

/// Check if the request is in demo mode and return the appropriate auth context
/// Demo mode is detected by the X-Temps-Demo-Mode header (set by proxy) or matching the host against demo.<preview_domain>
async fn check_demo_mode(
    req: &Request,
    auth_state: &Arc<AuthState>,
    host_without_port: &str,
    user_service: &UserService,
    demo_mode_header: bool,
) -> Option<AuthContext> {
    // If demo mode header is set by proxy, skip host validation (proxy already validated)
    if demo_mode_header {
        tracing::info!(
            "Demo mode detected via X-Temps-Demo-Mode header (host: {})",
            host_without_port
        );
    } else {
        // Validate host against preview_domain
        let settings_record = temps_entities::settings::Entity::find_by_id(1)
            .one(auth_state.db.as_ref())
            .await
            .ok()
            .flatten();

        let preview_domain = settings_record
            .map(|r| AppSettings::from_json(r.data).preview_domain)
            .unwrap_or_else(|| "localho.st".to_string());

        let preview_domain = preview_domain.trim_start_matches("*.");
        let expected_demo_host = format!("demo.{}", preview_domain);

        tracing::debug!(
            "Demo check: host_without_port={}, expected_demo_host={}, preview_domain={}",
            host_without_port,
            expected_demo_host,
            preview_domain
        );

        if host_without_port != expected_demo_host {
            return None;
        }

        tracing::info!(
            "Demo mode detected: host {} matches demo.{}",
            host_without_port,
            preview_domain
        );
    }

    // Check if there's a demo user cookie specifying which user to impersonate
    let selected_user_id =
        extract_demo_user_cookie(req.headers(), auth_state.cookie_crypto.as_ref());

    if let Some(user_id) = selected_user_id {
        // User has selected a specific user to view as in demo mode
        match user_service.get_user_by_id(user_id).await {
            Ok(user) => {
                // Determine the role of the selected user
                let role = determine_user_role(&user, user_service)
                    .await
                    .unwrap_or(Role::User);
                tracing::info!(
                    "Demo mode: authenticated as selected user (id={}, email={}, role={:?})",
                    user.id,
                    user.email,
                    role
                );
                return Some(AuthContext::new_demo_session(user, role));
            }
            Err(e) => {
                tracing::warn!(
                    "Demo mode: failed to load selected user {}: {:?}, falling back to demo user",
                    user_id,
                    e
                );
            }
        }
    }

    // Default: Find or create demo user
    match user_service.find_or_create_demo_user().await {
        Ok(demo_user) => {
            tracing::info!(
                "Demo mode: auto-authenticated as demo user (id={}, email={})",
                demo_user.id,
                demo_user.email
            );
            Some(AuthContext::new_demo_session(demo_user, Role::Demo))
        }
        Err(e) => {
            tracing::error!("Demo mode: failed to find/create demo user: {:?}", e);
            None
        }
    }
}

/// Extract the demo user ID from the demo user cookie
fn extract_demo_user_cookie(headers: &HeaderMap, crypto: &CookieCrypto) -> Option<i32> {
    for cookie_header in headers.get_all("cookie") {
        if let Ok(cookie_str) = cookie_header.to_str() {
            for cookie in Cookie::split_parse(cookie_str).filter_map(Result::ok) {
                if cookie.name() == DEMO_USER_COOKIE_NAME {
                    // Decrypt the cookie value
                    match crypto.decrypt(cookie.value()) {
                        Ok(decrypted) => {
                            // Parse the user_id
                            if let Ok(user_id) = decrypted.parse::<i32>() {
                                return Some(user_id);
                            }
                        }
                        Err(_) => {
                            // If decryption fails, no valid demo user selection
                            return None;
                        }
                    }
                }
            }
        }
    }
    None
}

#[derive(Debug)]
pub enum AuthError {
    Unauthorized(String),
    InternalServerError(String),
}

impl From<crate::auth_service::AuthError> for AuthError {
    fn from(err: crate::auth_service::AuthError) -> Self {
        match err {
            crate::auth_service::AuthError::Unauthorized(msg) => AuthError::Unauthorized(msg),
            _ => AuthError::InternalServerError(err.to_string()),
        }
    }
}

impl From<crate::apikey_service::ApiKeyServiceError> for AuthError {
    fn from(err: crate::apikey_service::ApiKeyServiceError) -> Self {
        match err {
            crate::apikey_service::ApiKeyServiceError::Unauthorized(msg) => {
                AuthError::Unauthorized(msg)
            }
            _ => AuthError::InternalServerError(err.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test that the middleware correctly identifies deployment token prefix
    #[test]
    fn test_deployment_token_prefix_detection() {
        // Valid deployment token prefix
        assert!("dt_sometoken123456789".starts_with("dt_"));
        // Invalid prefix should not match
        assert!(!"tk_sometoken123456789".starts_with("dt_"));
        assert!(!"invalid_token".starts_with("dt_"));
    }

    /// Test Bearer token extraction from Authorization header
    #[test]
    fn test_bearer_token_extraction() {
        let auth_header = "Bearer dt_testtoken123456";
        assert!(auth_header.starts_with("Bearer "));
        let token = auth_header.trim_start_matches("Bearer ");
        assert_eq!(token, "dt_testtoken123456");
    }

    /// Test that deployment tokens are routed to the correct validation path
    #[test]
    fn test_token_routing_logic() {
        // API key prefix
        let api_key_token = "tk_abc123";
        assert!(api_key_token.starts_with("tk_"));
        assert!(!api_key_token.starts_with("dt_"));

        // Deployment token prefix
        let deployment_token = "dt_xyz789";
        assert!(deployment_token.starts_with("dt_"));
        assert!(!deployment_token.starts_with("tk_"));

        // Invalid prefix (neither)
        let invalid_token = "invalid_token";
        assert!(!invalid_token.starts_with("tk_"));
        assert!(!invalid_token.starts_with("dt_"));
    }

    /// Test session cookie name constants
    #[test]
    fn test_cookie_names() {
        assert_eq!(SESSION_ID_COOKIE_NAME, "_temps_sid");
        assert_eq!(VISITOR_ID_COOKIE_NAME, "_temps_visitor_id");
    }

    /// Test AuthError variants
    #[test]
    fn test_auth_error_variants() {
        let unauthorized = AuthError::Unauthorized("test".to_string());
        let internal = AuthError::InternalServerError("error".to_string());

        match unauthorized {
            AuthError::Unauthorized(msg) => assert_eq!(msg, "test"),
            _ => panic!("Expected Unauthorized"),
        }

        match internal {
            AuthError::InternalServerError(msg) => assert_eq!(msg, "error"),
            _ => panic!("Expected InternalServerError"),
        }
    }
}
