// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Bearer token authentication middleware for the agent API.

use axum::{
    extract::{FromRequestParts, Request},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use temps_auth::permissions::Permission;
use temps_core::problemdetails::{self, Problem};

/// The operator-issued node token authorizes the control plane to build and
/// read images on this node. This is NOT a user/session principal and must not
/// inherit admin, secret-reading, or other control-plane API permissions.
pub struct AgentPrincipal {
    pub(crate) effective_role: &'static str,
    permissions: &'static [Permission],
}

impl AgentPrincipal {
    pub fn has_permission(&self, permission: &Permission) -> bool {
        self.permissions.contains(permission)
    }
}

/// Authenticate at the handler boundary too: mounting a handler without the
/// shared middleware must not accidentally make it public. Never accept a
/// principal/role/permission supplied by HTTP headers or the request body.
pub struct RequireAgentAuth(pub AgentPrincipal);

impl<S: Send + Sync> FromRequestParts<S> for RequireAgentAuth {
    type Rejection = Problem;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        let auth = parts.extensions.get::<Arc<AgentAuth>>().ok_or_else(|| {
            problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Agent authentication unavailable")
                .with_detail("This worker has no configured agent authentication state")
        })?;
        let token = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|header| header.to_str().ok())
            .and_then(|header| header.strip_prefix("Bearer "));
        if !token.is_some_and(|token| auth.verify(token)) {
            return Err(problemdetails::new(StatusCode::UNAUTHORIZED)
                .with_title("Agent authentication required")
                .with_detail("A valid operator-issued token for this worker is required"));
        }
        Ok(Self(AgentPrincipal {
            effective_role: "control-plane-agent",
            permissions: &[Permission::DeploymentsCreate, Permission::DeploymentsRead],
        }))
    }
}

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

    #[tokio::test]
    async fn node_principal_has_only_image_operation_permissions() {
        let request = Request::builder()
            .header("authorization", "Bearer test-token")
            .extension(Arc::new(AgentAuth::new("test-token")))
            .body(axum::body::Body::empty())
            .unwrap();
        let (mut parts, _) = request.into_parts();
        let RequireAgentAuth(principal) = RequireAgentAuth::from_request_parts(&mut parts, &())
            .await
            .unwrap();
        assert!(principal.has_permission(&Permission::DeploymentsCreate));
        assert!(principal.has_permission(&Permission::DeploymentsRead));
        assert!(!principal.has_permission(&Permission::SystemAdmin));
        assert!(!principal.has_permission(&Permission::SecretsRead));
    }

    #[test]
    fn node_permission_guard_denies_insufficient_capability() {
        fn guarded_build(auth: AgentPrincipal) -> Result<(), Problem> {
            temps_auth::permission_guard!(auth, DeploymentsCreate);
            Ok(())
        }
        let error = guarded_build(AgentPrincipal {
            effective_role: "test-reader",
            permissions: &[Permission::DeploymentsRead],
        })
        .unwrap_err();
        assert_eq!(error.into_response().status(), StatusCode::FORBIDDEN);
    }

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
