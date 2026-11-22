// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Which scheduler places an environment's deployments (ADR-055 Phase 5).
//!
//! Stored on the environment row (`environments.workload_mode`), not read from
//! an environment variable, so an operator moves one environment at a time
//! through the API, every change is audit-logged, and a bad result is undone
//! with one more write (the rollback the ADR names).
//!
//! - [`EnvironmentWorkloadMode::Legacy`]: `NodeScheduler` places replicas and
//!   the deployment job creates containers exactly as before ADR-055.
//! - [`EnvironmentWorkloadMode::Shadow`]: as `Legacy`, and the unified
//!   scheduler also runs and every disagreement is logged.
//! - [`EnvironmentWorkloadMode::Enforce`]: the unified scheduler places
//!   replicas (strictly: a pin that cannot be met fails the deployment), and
//!   control-plane replicas are submitted to the workload plane.

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
pub enum EnvironmentWorkloadMode {
    /// `NodeScheduler` and the legacy container path; nothing new runs.
    #[default]
    #[sea_orm(string_value = "legacy")]
    Legacy,
    /// Legacy decides; the unified scheduler runs beside it and is compared.
    #[sea_orm(string_value = "shadow")]
    Shadow,
    /// The unified scheduler decides and the plane runs the replicas.
    #[sea_orm(string_value = "enforce")]
    Enforce,
}

impl EnvironmentWorkloadMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            EnvironmentWorkloadMode::Legacy => "legacy",
            EnvironmentWorkloadMode::Shadow => "shadow",
            EnvironmentWorkloadMode::Enforce => "enforce",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_serialise_as_their_column_values() {
        for mode in [
            EnvironmentWorkloadMode::Legacy,
            EnvironmentWorkloadMode::Shadow,
            EnvironmentWorkloadMode::Enforce,
        ] {
            assert_eq!(
                serde_json::to_string(&mode).unwrap(),
                format!("\"{}\"", mode.as_str())
            );
        }
        assert_eq!(
            EnvironmentWorkloadMode::default(),
            EnvironmentWorkloadMode::Legacy
        );
    }
}
