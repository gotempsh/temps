// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Which code path serves a node's sandboxes (ADR-055 phase 6).
//!
//! Stored per node (`nodes.sandbox_driver_mode`; the control plane, node 0,
//! uses `network_config.control_plane_sandbox_driver_mode`), never read from
//! an environment variable, so an administrator moves one node at a time
//! through the API, every change is audit-logged, and setting `legacy` again
//! is the rollback.
//!
//! - [`SandboxDriverMode::Legacy`]: sandbox calls go to the sandbox providers
//!   exactly as before ADR-055. Remote sandboxes keep refusing the
//!   operations they refused before.
//! - [`SandboxDriverMode::Shadow`]: the old providers still serve every
//!   call; the driver facade also plans each create (driver choice,
//!   capability filter, limit conditions) and logs where it would differ.
//! - [`SandboxDriverMode::Enforce`]: the driver facade serves the node's
//!   sandboxes, and remote sandboxes forward terminals, snapshots, disk
//!   resize and the relays to the worker's driver.

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
pub enum SandboxDriverMode {
    /// The old sandbox providers serve every call.
    #[default]
    #[sea_orm(string_value = "legacy")]
    Legacy,
    /// The old providers serve every call; the driver facade plans and logs.
    #[sea_orm(string_value = "shadow")]
    Shadow,
    /// The driver facade serves every call.
    #[sea_orm(string_value = "enforce")]
    Enforce,
}

impl SandboxDriverMode {
    pub const ALL: [SandboxDriverMode; 3] = [
        SandboxDriverMode::Legacy,
        SandboxDriverMode::Shadow,
        SandboxDriverMode::Enforce,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            SandboxDriverMode::Legacy => "legacy",
            SandboxDriverMode::Shadow => "shadow",
            SandboxDriverMode::Enforce => "enforce",
        }
    }

    /// Whether the driver facade serves calls (only in `enforce`).
    pub fn uses_drivers(&self) -> bool {
        matches!(self, SandboxDriverMode::Enforce)
    }
}

impl std::fmt::Display for SandboxDriverMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A mode string that is not `legacy`, `shadow` or `enforce`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown sandbox driver mode '{value}'; expected one of legacy, shadow, enforce")]
pub struct UnknownSandboxDriverMode {
    pub value: String,
}

impl std::str::FromStr for SandboxDriverMode {
    type Err = UnknownSandboxDriverMode;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "legacy" => Ok(SandboxDriverMode::Legacy),
            "shadow" => Ok(SandboxDriverMode::Shadow),
            "enforce" => Ok(SandboxDriverMode::Enforce),
            other => Err(UnknownSandboxDriverMode {
                value: other.to_string(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_round_trip_through_their_string_form() {
        for mode in SandboxDriverMode::ALL {
            assert_eq!(mode.as_str().parse::<SandboxDriverMode>(), Ok(mode));
            assert_eq!(
                serde_json::to_value(mode).unwrap(),
                serde_json::Value::String(mode.as_str().to_string())
            );
        }
    }

    #[test]
    fn the_default_is_legacy_and_only_enforce_uses_drivers() {
        assert_eq!(SandboxDriverMode::default(), SandboxDriverMode::Legacy);
        assert!(!SandboxDriverMode::Legacy.uses_drivers());
        assert!(!SandboxDriverMode::Shadow.uses_drivers());
        assert!(SandboxDriverMode::Enforce.uses_drivers());
    }

    #[test]
    fn an_unknown_mode_names_the_value() {
        let error = "enforced".parse::<SandboxDriverMode>().unwrap_err();
        assert_eq!(
            error.to_string(),
            "unknown sandbox driver mode 'enforced'; expected one of legacy, shadow, enforce"
        );
    }
}
