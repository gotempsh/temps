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
//!   │   --data-dir ~/.temps/plugins/x/ │
//!   │                                  │
//!   │<── stdout: protocol + manifest ──│
//!   │── stdin: typed launch config ───>│  (requested values only)
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
//! - `X-Temps-Auth-Signature`: per-process assertion secret

use serde::{Deserialize, Serialize};

/// CLI arguments that Temps passes to the plugin binary.
#[derive(Debug, Clone, clap::Parser)]
pub struct PluginArgs {
    /// Path to the Unix domain socket the plugin should listen on
    #[arg(long)]
    pub socket_path: String,

    /// Database URL populated from staged launch configuration for plugins
    /// that declare direct DB access.
    /// Most plugins should use the WebSocket channel instead of direct database access.
    #[arg(skip)]
    pub database_url: Option<String>,

    /// Request-assertion secret populated internally from the host's one-shot
    /// stdin pipe. It is not accepted as a CLI argument, so it does not appear
    /// in ordinary process listings.
    #[arg(skip)]
    pub auth_secret: Option<String>,

    /// Directory for plugin-specific data files
    #[arg(long)]
    pub data_dir: String,

    /// The instance's own data root, of which `data_dir` is a subdirectory.
    ///
    /// Present only when the manifest explicitly requests the privileged
    /// `requires_host_data_access` capability. This can expose encryption
    /// keys and platform-owned state; ordinary plugins must use `data_dir`.
    #[arg(skip)]
    pub host_data_dir: Option<String>,

    /// Base URL at which the instance's API answers, e.g.
    /// `http://127.0.0.1:8080`.
    ///
    /// A plugin's own routes live under `<this>/api/x/<plugin-name>`. Needed
    /// whenever a plugin must hand a URL to something that is not the caller
    /// — a sandboxed process, an external webhook — since such a client
    /// cannot reach the Unix socket the plugin actually listens on.
    #[arg(long)]
    pub host_api_url: Option<String>,
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
    /// Short-lived proof this plugin may act as the caller on the platform
    /// channel.
    ///
    /// `None` when the route authenticates itself (`public_paths`, so there
    /// is no platform user to act as) or when the manifest declares no API
    /// capability, in which case the platform never minted one. Pass it to
    /// [`crate::context::PluginContext::api_for`] to make platform API
    /// calls attributed to this caller.
    pub actor_token: Option<String>,
}

/// Header names used in the Temps-to-plugin protocol.
///
/// Re-exported from `temps-core` so the injecting side (the platform's
/// proxy) and the reading side (this SDK) cannot drift apart. See
/// [`temps_core::external_plugin::headers`] for the trust model.
pub use temps_core::external_plugin::headers;

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

        let actor_token = parts
            .headers
            .get(headers::ACTOR_TOKEN)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);

        Ok(TempsAuth(TempsUserContext {
            user_id,
            user_email,
            role,
            request_id,
            actor_token,
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
            actor_token: Some("signed".into()),
        };
        let json = serde_json::to_string(&ctx).unwrap();
        let deser: TempsUserContext = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.user_id, Some(1));
        assert_eq!(deser.role, "admin");
        assert_eq!(deser.actor_token.as_deref(), Some("signed"));
    }

    /// A request with no actor header must still parse — that is the normal
    /// shape for a `public_paths` route, and for any plugin whose manifest
    /// declares no API capability.
    #[test]
    fn a_context_without_an_actor_token_is_valid() {
        let ctx: TempsUserContext =
            serde_json::from_str(r#"{"user_id":1,"role":"user","request_id":"r"}"#).unwrap();
        assert!(ctx.actor_token.is_none());
    }
}
