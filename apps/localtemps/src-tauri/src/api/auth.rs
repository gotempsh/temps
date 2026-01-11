//! Simple authentication middleware for LocalTemps
//!
//! Accepts a fixed local development token: `localtemps-dev-token`

use axum::{
    extract::Request,
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;

use crate::context::LOCAL_TOKEN;

/// Error response for authentication failures
#[derive(Serialize)]
pub struct AuthError {
    pub error: AuthErrorDetails,
}

#[derive(Serialize)]
pub struct AuthErrorDetails {
    pub message: String,
    pub code: String,
}

/// Authentication middleware
///
/// Validates the Bearer token in the Authorization header.
/// Only accepts the fixed local development token.
pub async fn auth_middleware(request: Request, next: Next) -> Response {
    let auth_header = request
        .headers()
        .get("authorization")
        .and_then(|h| h.to_str().ok());

    match auth_header {
        Some(header) if header.starts_with("Bearer ") => {
            let token = &header[7..]; // Skip "Bearer "
            if token == LOCAL_TOKEN {
                next.run(request).await
            } else {
                unauthorized_response("Invalid token")
            }
        }
        Some(_) => {
            unauthorized_response("Invalid authorization header format. Use: Bearer <token>")
        }
        None => unauthorized_response("Missing authorization header"),
    }
}

fn unauthorized_response(message: &str) -> Response {
    let error = AuthError {
        error: AuthErrorDetails {
            message: message.to_string(),
            code: "UNAUTHORIZED".to_string(),
        },
    };

    (StatusCode::UNAUTHORIZED, Json(error)).into_response()
}
