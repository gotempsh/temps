//! Error types for the plugin SDK.

use thiserror::Error;

/// Errors that can occur during plugin operation.
#[derive(Error, Debug)]
pub enum PluginSdkError {
    #[error("Failed to parse CLI arguments: {message}")]
    Args { message: String },

    #[error("Failed to bind Unix socket at '{path}': {reason}")]
    SocketBind { path: String, reason: String },

    #[error("Handshake failed for plugin '{plugin_name}': {reason}")]
    Handshake { plugin_name: String, reason: String },

    #[error("Plugin initialization failed for '{plugin_name}': {reason}")]
    Initialization { plugin_name: String, reason: String },

    #[error("Server error: {0}")]
    Server(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Platform channel closed unexpectedly")]
    ChannelClosed,

    #[error("Platform returned error ({code}): {message}")]
    PlatformError { code: String, message: String },

    #[error("Failed to deserialize platform response: {reason}")]
    Deserialization { reason: String },

    /// The platform answered a different call than the one made.
    ///
    /// Distinct from `Deserialization` on purpose: this one always means the
    /// plugin and the platform were built against different versions of the
    /// channel protocol, and the fix is to rebuild, not to check the data.
    #[error("{reason}")]
    ProtocolMismatch { reason: String },

    /// This plugin has no actor token, so it cannot act for a user.
    ///
    /// Either the request being served was not made by a signed-in user (a
    /// self-authenticating `public_paths` route, or a background task with
    /// no request behind it), or the manifest declares no API capability so
    /// the platform never minted one.
    #[error(
        "No platform actor is available for this call. Platform API calls must be made while \
         serving a request from a signed-in user, and the plugin manifest must declare an \
         api_read or api_write capability."
    )]
    NoActor,

    /// The request body is larger than the channel will carry.
    ///
    /// Rejected before transport rather than after, and named as a limit
    /// rather than a generic failure, because the fix is always the same:
    /// send less, or use an endpoint that streams.
    #[error(
        "Cannot send a {len}-byte body for {method} {path} over the platform channel \
         (limit {limit} bytes). Large artifacts must be uploaded another way."
    )]
    BodyTooLarge {
        len: usize,
        limit: usize,
        method: &'static str,
        path: String,
    },

    /// The platform API answered, but not with success.
    ///
    /// The status and body are the API's own, so a plugin can tell a 403
    /// from a 404 and show the operator what the platform actually said.
    #[error("Platform API returned {status} for {method} {path}: {body}")]
    ApiStatus {
        status: u16,
        method: &'static str,
        path: String,
        body: String,
    },
}
