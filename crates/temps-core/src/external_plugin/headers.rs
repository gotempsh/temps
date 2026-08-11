//! Request headers Temps attaches when proxying to an external plugin.
//!
//! Both ends of the proxy need these names, so they live here rather than in
//! either crate that uses them: `temps-external-plugins` injects them in
//! `forward_to_unix_socket`, and `temps-plugin-sdk` re-exports them from
//! `protocol::headers` for the `TempsAuth` extractor plugins build on.
//!
//! # Trust model
//!
//! Everything under the [`PREFIX`] namespace is **asserted by Temps**, never
//! by the caller. The proxy strips every inbound `x-temps-*` header before
//! injecting its own, so a client cannot present `x-temps-user-role: admin`
//! and have it survive to the plugin. A plugin may therefore treat these as
//! authoritative for requests carrying the staged process secret. This does
//! not isolate installed plugins from one another under the current shared-UID
//! host model; installed plugin binaries are trusted host code.

/// Namespace for every header the proxy asserts. The proxy removes all
/// inbound headers with this prefix before adding its own.
pub const PREFIX: &str = "x-temps-";

/// Name of the plugin the request was routed to.
pub const PLUGIN_NAME: &str = "x-temps-plugin";

/// Authenticated user's database id. Absent when the caller authenticated
/// with a credential that is not bound to a user (a deployment token).
pub const USER_ID: &str = "x-temps-user-id";

/// Authenticated user's email address.
pub const USER_EMAIL: &str = "x-temps-user-email";

/// Caller's effective role, in `Role`'s wire form (`admin`, `user`,
/// `reader`, …). Always present on a proxied request.
pub const USER_ROLE: &str = "x-temps-user-role";

/// Comma-separated effective permissions for this request.
///
/// This is the already-resolved permission set, not merely the permissions
/// implied by [`USER_ROLE`]. API keys may narrow a role or carry a custom
/// permission set, so plugins must authorize against this header rather than
/// reconstructing permissions from the role name.
pub const USER_PERMISSIONS: &str = "x-temps-user-permissions";

/// Per-request correlation id, so plugin logs can be joined to Temps' own.
pub const REQUEST_ID: &str = "x-temps-request-id";

/// The plugin's handshake secret, echoed back so the plugin can confirm the
/// request came from the Temps process that spawned it rather than from
/// something else that found its socket.
pub const AUTH_SIGNATURE: &str = "x-temps-auth-signature";

/// Short-lived proof the plugin echoes back on channel API calls to act as
/// the caller. Present only when the caller is a real user and the plugin
/// declared an API capability. See
/// [`crate::external_plugin::actor`].
pub const ACTOR_TOKEN: &str = "x-temps-actor-token";
