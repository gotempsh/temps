// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Authorization primitives shared by every handler crate.
//!
//! `AuthContext`, `Role` and `Permission`, the `RequireAuth` extractor and the
//! `permission_guard!` family of macros live here so a crate that only needs
//! to authorize requests does not pull in the authentication stack (sessions,
//! OIDC, MFA, API-key services). `temps-auth` re-exports everything here, so
//! existing `temps_auth::...` paths keep working.

pub mod context;
mod decorators;
mod macros;
mod permission_attribute;
mod permission_decorator;
mod permission_guard;
pub mod permissions;

pub use context::*;
pub use decorators::*;
pub use macros::*;
pub use permission_attribute::*;
pub use permissions::*;

/// Paths the exported macros expand to, so a caller that depends only on
/// `temps-authz` does not also need `temps-core`, `axum` or `tracing` in
/// scope. Not part of the public API.
#[doc(hidden)]
pub mod __private {
    pub use axum;
    pub use temps_core;
    pub use tracing;
}
