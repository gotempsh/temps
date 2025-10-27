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
use std::sync::Arc;
use temps_core::{CookieCrypto, RequestMetadata};

// Cookie names from proxy
const SESSION_ID_COOKIE_NAME: &str = "_temps_sid";
const VISITOR_ID_COOKIE_NAME: &str = "_temps_visitor_id";

pub async fn auth_middleware(
    State(app_state): State<Arc<AuthState>>,
    mut req: axum::extract::Request,
    next: Next,
) -> Result<axum::response::Response, StatusCode> {
    let mut user = None;

    let auth_context = match extract_auth_from_request(&req, &app_state).await {
        Ok(ctx) => {
            // Extract user from auth context if available
            user = Some(ctx.user.clone());
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

    // 1. Check for Authorization header (API Key or CLI Token)
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
