//! Communication protocol between Temps and external plugins.
//!
//! The protocol uses simple JSON-over-stdout for the initial handshake,
//! then HTTP-over-Unix-socket for all subsequent communication.
//!
//! ## Handshake Sequence
//!
//! ```text
//! Temps                           Plugin Binary
//!   │                                  │
//!   │── spawn with args ──────────────>│
//!   │   --socket-path /tmp/x.sock      │
//!   │   --database-url postgres://...   │
//!   │   --auth-secret <hmac_key>       │
//!   │   --data-dir ~/.temps/plugins/x/ │
//!   │                                  │
//!   │<── stdout: manifest JSON ────────│  (plugin writes manifest)
//!   │                                  │
//!   │                                  │── starts axum on socket
//!   │                                  │
//!   │<── stdout: ready JSON ───────────│  (plugin signals ready)
//!   │                                  │
//!   │── GET /health ──────────────────>│  (Temps verifies health)
//!   │<── 200 OK ───────────────────────│
//!   │                                  │
//!   │   ┌──── normal operation ────┐   │
//!   │   │ Temps proxies requests   │   │
//!   │   │ to plugin over socket    │   │
//!   │   └──────────────────────────┘   │
//!   │                                  │
//!   │── SIGTERM ──────────────────────>│  (graceful shutdown)
//!   │                                  │── cleanup + exit
//! ```
//!
//! ## Request Headers
//!
//! Temps adds these headers to proxied requests:
//! - `X-Temps-Plugin`: plugin name
//! - `X-Temps-User-Id`: authenticated user ID (if available)
//! - `X-Temps-User-Email`: authenticated user email (if available)
//! - `X-Temps-User-Role`: effective role (admin, user, reader, etc.)
//! - `X-Temps-Request-Id`: unique request ID for tracing
//! - `X-Temps-Auth-Signature`: HMAC signature of the request

use serde::{Deserialize, Serialize};

/// CLI arguments that Temps passes to the plugin binary.
#[derive(Debug, Clone, clap::Parser)]
pub struct PluginArgs {
    /// Path to the Unix domain socket the plugin should listen on
    #[arg(long)]
    pub socket_path: String,

    /// PostgreSQL database URL
    #[arg(long)]
    pub database_url: String,

    /// HMAC secret for authenticating requests from Temps
    #[arg(long)]
    pub auth_secret: String,

    /// Directory for plugin-specific data files
    #[arg(long)]
    pub data_dir: String,
}

/// User context extracted from Temps proxy headers.
///
/// This is available in handlers via the `TempsAuth` extractor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TempsUserContext {
    /// User ID (None for unauthenticated or system requests)
    pub user_id: Option<i32>,
    /// User email
    pub user_email: Option<String>,
    /// Effective role
    pub role: String,
    /// Request ID for tracing
    pub request_id: String,
}

/// Header names used in the Temps-to-plugin protocol.
pub mod headers {
    pub const PLUGIN_NAME: &str = "x-temps-plugin";
    pub const USER_ID: &str = "x-temps-user-id";
    pub const USER_EMAIL: &str = "x-temps-user-email";
    pub const USER_ROLE: &str = "x-temps-user-role";
    pub const REQUEST_ID: &str = "x-temps-request-id";
    pub const AUTH_SIGNATURE: &str = "x-temps-auth-signature";
}

/// Axum extractor that reads Temps auth headers from proxied requests.
///
/// # Usage
/// ```rust,no_run
/// use temps_plugin_sdk::protocol::TempsAuth;
///
/// async fn my_handler(TempsAuth(user): TempsAuth) -> String {
///     format!("Hello, user {:?}", user.user_email)
/// }
/// ```
#[derive(Debug, Clone)]
pub struct TempsAuth(pub TempsUserContext);

impl<S> axum::extract::FromRequestParts<S> for TempsAuth
where
    S: Send + Sync,
{
    type Rejection = axum::http::StatusCode;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        let user_id = parts
            .headers
            .get(headers::USER_ID)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<i32>().ok());

        let user_email = parts
            .headers
            .get(headers::USER_EMAIL)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.to_string());

        let role = parts
            .headers
            .get(headers::USER_ROLE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("reader")
            .to_string();

        let request_id = parts
            .headers
            .get(headers::REQUEST_ID)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        Ok(TempsAuth(TempsUserContext {
            user_id,
            user_email,
            role,
            request_id,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_user_context_serialization() {
        let ctx = TempsUserContext {
            user_id: Some(1),
            user_email: Some("admin@example.com".into()),
            role: "admin".into(),
            request_id: "req-123".into(),
        };
        let json = serde_json::to_string(&ctx).unwrap();
        let deser: TempsUserContext = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.user_id, Some(1));
        assert_eq!(deser.role, "admin");
    }
}
