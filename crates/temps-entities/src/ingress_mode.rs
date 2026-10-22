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
/// `TempsLogin`).
///
/// Stored on the sandbox row (`sandboxes.preview_access`). Rows that existed
/// before the column are `Legacy`, so no running sandbox changes behaviour;
/// rows created afterwards get the database default, `TempsLogin`.
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
    /// Before ADR-055: the preview password when one is set, otherwise
    /// anyone who can reach the unguessable hostname.
    #[sea_orm(string_value = "legacy")]
    Legacy,
    /// The preview password when one is set, otherwise a Temps login by a
    /// user who may use the sandbox.
    #[default]
    #[sea_orm(string_value = "temps_login")]
    TempsLogin,
    /// Anyone who can reach the hostname, even with no password set.
    #[sea_orm(string_value = "public")]
    Public,
}

impl SandboxPreviewAccess {
    pub fn as_str(&self) -> &'static str {
        match self {
            SandboxPreviewAccess::Legacy => "legacy",
            SandboxPreviewAccess::TempsLogin => "temps_login",
            SandboxPreviewAccess::Public => "public",
        }
    }

    /// The value of a row from before the column: `Legacy`, so reading an
    /// older serialized row never changes who may open its preview.
    pub fn unchanged() -> Self {
        SandboxPreviewAccess::Legacy
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
            SandboxPreviewAccess::Legacy,
            SandboxPreviewAccess::TempsLogin,
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
