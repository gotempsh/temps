// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The isolation an environment's plane-run deployments request (ADR-055
//! Phase 10, Firecracker for deployments).
//!
//! Stored on the environment row (`environments.workload_isolation`) rather
//! than read from an environment variable, so an operator opts one
//! environment in at a time through the audited admin API and undoes it with
//! one more write.
//!
//! [`Model`] is a narrow view of `environments`: only the columns the
//! isolation setting reads and writes. Kept apart from
//! [`super::environments`] so adding the column does not change every place
//! that builds a full environment model.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;
use utoipa::ToSchema;

use crate::environment_workload_mode::EnvironmentWorkloadMode;

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
pub enum EnvironmentWorkloadIsolation {
    /// A container sharing the host kernel: every deployment today.
    #[default]
    #[sea_orm(string_value = "process")]
    Process,
    /// A Firecracker microVM with its own kernel. Only placed on a node whose
    /// runtime driver provides it; refused with the scheduler's reason
    /// otherwise.
    #[sea_orm(string_value = "micro_vm")]
    MicroVm,
}

impl EnvironmentWorkloadIsolation {
    pub fn as_str(&self) -> &'static str {
        match self {
            EnvironmentWorkloadIsolation::Process => "process",
            EnvironmentWorkloadIsolation::MicroVm => "micro_vm",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "environments")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub project_id: i32,
    pub workload_mode: EnvironmentWorkloadMode,
    /// `process | micro_vm`; default `process`.
    pub workload_isolation: EnvironmentWorkloadIsolation,
    pub deleted_at: Option<DBDateTime>,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isolations_serialise_as_their_column_values() {
        for isolation in [
            EnvironmentWorkloadIsolation::Process,
            EnvironmentWorkloadIsolation::MicroVm,
        ] {
            assert_eq!(
                serde_json::to_string(&isolation).unwrap(),
                format!("\"{}\"", isolation.as_str())
            );
        }
        assert_eq!(
            EnvironmentWorkloadIsolation::default(),
            EnvironmentWorkloadIsolation::Process
        );
    }
}
