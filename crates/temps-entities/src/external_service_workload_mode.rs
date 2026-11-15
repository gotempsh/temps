// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! How a managed database's containers are created (ADR-055 Phase 4).
//!
//! Stored on the service row (`external_services.workload_mode`), not read
//! from an environment variable, so an administrator moves one database at a
//! time through the API, every change is audit-logged, and a bad result is
//! undone with one more write.
//!
//! - [`ServiceWorkloadMode::Legacy`]: what every database did before
//!   ADR-055. The control-plane engines and the worker agent build their own
//!   container-create requests, and nothing new runs.
//! - [`ServiceWorkloadMode::Shadow`]: the old path still creates the
//!   container. A bounded background pass also computes the database's
//!   workload spec and logs where the running container differs from it,
//!   without changing anything.
//! - [`ServiceWorkloadMode::Enforce`]: both creation paths build the
//!   container from the admitted spec through the one Docker driver. A
//!   running container only changes when it is next recreated.

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
pub enum ServiceWorkloadMode {
    /// The engine's (or the agent's) own create request; nothing new runs.
    #[default]
    #[sea_orm(string_value = "legacy")]
    Legacy,
    /// The old path creates; the spec is computed and compared in the
    /// background.
    #[sea_orm(string_value = "shadow")]
    Shadow,
    /// The admitted spec, through the Docker driver, creates.
    #[sea_orm(string_value = "enforce")]
    Enforce,
}

impl ServiceWorkloadMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            ServiceWorkloadMode::Legacy => "legacy",
            ServiceWorkloadMode::Shadow => "shadow",
            ServiceWorkloadMode::Enforce => "enforce",
        }
    }

    /// Whether the database's containers are created from the spec.
    pub fn creates_from_spec(&self) -> bool {
        matches!(self, ServiceWorkloadMode::Enforce)
    }
}

impl std::fmt::Display for ServiceWorkloadMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_serialise_as_their_column_values() {
        for mode in [
            ServiceWorkloadMode::Legacy,
            ServiceWorkloadMode::Shadow,
            ServiceWorkloadMode::Enforce,
        ] {
            assert_eq!(
                serde_json::to_string(&mode).unwrap(),
                format!("\"{}\"", mode.as_str())
            );
        }
        assert_eq!(ServiceWorkloadMode::default(), ServiceWorkloadMode::Legacy);
        assert!(!ServiceWorkloadMode::Legacy.creates_from_spec());
        assert!(!ServiceWorkloadMode::Shadow.creates_from_spec());
        assert!(ServiceWorkloadMode::Enforce.creates_from_spec());
    }
}
