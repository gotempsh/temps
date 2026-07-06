//! Bearer token authentication middleware for the agent API.

use axum::{
    extract::Request,
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};
use sha2::{Digest, Sha256};
use std::sync::Arc;

/// Shared state holding the expected bearer token hash (SHA-256).
#[derive(Clone)]
pub struct AgentAuth {
    token_hash: String,
}

impl AgentAuth {
    pub fn new(token: &str) -> Self {
        Self {
            token_hash: sha256_hex(token),
        }
    }

    fn verify(&self, provided: &str) -> bool {
        let provided_hash = sha256_hex(provided);
        // Constant-length comparison: both are 64-char hex strings from SHA-256.
        // We iterate all bytes to avoid timing side-channels.
        constant_time_eq(self.token_hash.as_bytes(), provided_hash.as_bytes())
    }
}

/// SHA-256 hash a string and return lowercase hex.
fn sha256_hex(input: &str) -> String {
    let digest = Sha256::digest(input.as_bytes());
    hex::encode(digest)
}

/// Constant-time byte comparison to prevent timing attacks.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Axum middleware that validates the Authorization: Bearer <token> header.
pub async fn require_agent_auth(request: Request, next: Next) -> Response {
    let auth = request.extensions().get::<Arc<AgentAuth>>().cloned();

    let auth = match auth {
        Some(a) => a,
        None => {
            return (StatusCode::INTERNAL_SERVER_ERROR, "Auth not configured").into_response();
        }
    };

    let auth_header = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok());

    match auth_header {
        Some(header) if header.starts_with("Bearer ") => {
            let token = &header[7..];
            if auth.verify(token) {
                next.run(request).await
            } else {
                (StatusCode::UNAUTHORIZED, "Invalid token").into_response()
            }
        }
        _ => (
            StatusCode::UNAUTHORIZED,
            "Missing or invalid Authorization header",
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_auth_verify_correct_token() {
        let auth = AgentAuth::new("test-secret-token");
        assert!(auth.verify("test-secret-token"));
    }

    #[test]
    fn test_agent_auth_verify_wrong_token() {
        let auth = AgentAuth::new("test-secret-token");
        assert!(!auth.verify("wrong-token"));
    }

    #[test]
    fn test_agent_auth_verify_empty_token() {
        let auth = AgentAuth::new("test-secret-token");
        assert!(!auth.verify(""));
    }

    #[test]
    fn test_sha256_hex_produces_64_char_hex() {
        let hash = sha256_hex("hello");
        assert_eq!(hash.len(), 64);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_sha256_hex_deterministic() {
        assert_eq!(sha256_hex("token-a"), sha256_hex("token-a"));
    }

    #[test]
    fn test_sha256_hex_different_inputs() {
        assert_ne!(sha256_hex("token-a"), sha256_hex("token-b"));
    }

    #[test]
    fn test_constant_time_eq_equal() {
        assert!(constant_time_eq(b"hello", b"hello"));
    }

    #[test]
    fn test_constant_time_eq_not_equal() {
        assert!(!constant_time_eq(b"hello", b"world"));
    }

    #[test]
    fn test_constant_time_eq_different_lengths() {
        assert!(!constant_time_eq(b"short", b"longer"));
    }
}
