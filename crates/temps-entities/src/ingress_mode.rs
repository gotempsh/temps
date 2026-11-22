// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Which access evaluator decides an environment's requests (ADR-055 §11).
//!
//! Stored on the environment row (`environments.ingress_mode`), not read from
//! an environment variable, so an operator can move one environment at a time
//! through the API, every change is audit-logged, and a bad result is undone
//! with one more write.
//!
//! - [`IngressMode::Legacy`] is what every environment did before ADR-055:
//!   the proxy's hand-written access chain (IP gate, rate limits, attack mode,
//!   password wall) decides, and the compiled endpoint is not even built.
//! - [`IngressMode::Shadow`] keeps the legacy decision and also evaluates the
//!   compiled endpoint, counting every disagreement in a metric and logging a
//!   sample. This is how an operator learns what `Enforce` would change before
//!   it changes anything.
//! - [`IngressMode::Enforce`] lets the compiled endpoint decide.

use sea_orm::{DeriveActiveEnum, EnumIter};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    ToSchema,
    DeriveActiveEnum,
    EnumIter,
    Default,
)]
#[sea_orm(rs_type = "String", db_type = "Text")]
#[serde(rename_all = "snake_case")]
// Named apart from the worker ingress `IngressMode` (off/fixed/delegated)
// so the two do not collapse into one OpenAPI schema.
#[schema(as = EnvironmentIngressMode)]
pub enum IngressMode {
    /// The legacy access chain decides; nothing new runs.
    #[default]
    #[sea_orm(string_value = "legacy")]
    Legacy,
    /// The legacy chain decides; the compiled endpoint is evaluated and
    /// compared.
    #[sea_orm(string_value = "shadow")]
    Shadow,
    /// The compiled endpoint decides.
    #[sea_orm(string_value = "enforce")]
    Enforce,
}

impl IngressMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            IngressMode::Legacy => "legacy",
            IngressMode::Shadow => "shadow",
            IngressMode::Enforce => "enforce",
        }
    }

    /// Whether the compiled endpoint has to be built for this environment.
    pub fn compiles(&self) -> bool {
        !matches!(self, IngressMode::Legacy)
    }
}

/// Who may open a sandbox's preview URLs (ADR-055 §11: sandboxes default to
/// `TempsLogin`, never anonymous by default).
///
/// Stored on the sandbox row (`sandboxes.preview_access`). The sandbox's IP
/// allow-list (`sandboxes.preview_allowed_cidrs`) is checked before any of
/// these, so `Public` plus an allow-list means "anyone from these networks".
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    ToSchema,
    DeriveActiveEnum,
    EnumIter,
    Default,
)]
#[sea_orm(rs_type = "String", db_type = "Text")]
#[serde(rename_all = "snake_case")]
pub enum SandboxPreviewAccess {
    /// A Temps user who may use the sandbox, proven by a grant the console
    /// signs for the preview host. The default.
    #[default]
    #[sea_orm(string_value = "temps_login")]
    TempsLogin,
    /// Whoever knows the preview password. Only valid while a password is
    /// set: setting one selects this mode, clearing it returns to
    /// `TempsLogin`, and the database refuses the mode without a hash.
    #[sea_orm(string_value = "password")]
    Password,
    /// Anyone who can reach the hostname (and passes the IP allow-list).
    #[sea_orm(string_value = "public")]
    Public,
}

impl SandboxPreviewAccess {
    /// The permission a `TempsLogin` grant for a sandbox preview carries:
    /// the console signs one only for a user who may read the sandbox.
    pub const LOGIN_PERMISSION: &'static str = "sandboxes:read";

    pub fn as_str(&self) -> &'static str {
        match self {
            SandboxPreviewAccess::TempsLogin => "temps_login",
            SandboxPreviewAccess::Password => "password",
            SandboxPreviewAccess::Public => "public",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_serialise_as_their_column_values() {
        for mode in [
            IngressMode::Legacy,
            IngressMode::Shadow,
            IngressMode::Enforce,
        ] {
            assert_eq!(
                serde_json::to_string(&mode).unwrap(),
                format!("\"{}\"", mode.as_str())
            );
        }
        assert_eq!(IngressMode::default(), IngressMode::Legacy);
        assert!(!IngressMode::Legacy.compiles());
        assert!(IngressMode::Shadow.compiles());
        assert!(IngressMode::Enforce.compiles());
        for access in [
            SandboxPreviewAccess::TempsLogin,
            SandboxPreviewAccess::Password,
            SandboxPreviewAccess::Public,
        ] {
            assert_eq!(
                serde_json::to_string(&access).unwrap(),
                format!("\"{}\"", access.as_str())
            );
        }
        assert_eq!(
            SandboxPreviewAccess::default(),
            SandboxPreviewAccess::TempsLogin
        );
    }
}
