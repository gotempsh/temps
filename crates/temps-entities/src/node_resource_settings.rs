// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A node's operator-controlled scheduling settings (ADR-055 §5, Phase 5):
//! `nodes.resource_policy` and `nodes.system_reserved`.
//!
//! These are columns of the `nodes` table, mapped here as their own narrow
//! entity rather than added to [`super::nodes::Model`]: they are written only
//! by the admin API and read only by the scheduler, never by registration or
//! heartbeats, and keeping them out of the wide node model leaves every
//! existing `nodes::Model` construction untouched.
//!
//! The control plane (node 0) is not a row; its values are stored with its
//! other node-0 settings (`temps_core::AppSettings::control_plane_resources`).

use sea_orm::entity::prelude::*;
use sea_orm::{DeriveActiveEnum, EnumIter, FromJsonQueryResult};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Whether the scheduler counts reservations against a node's capacity.
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
pub enum NodeResourcePolicy {
    /// Placed by observed load with the 0.90 threshold, over the capacity
    /// left after `system_reserved`.
    #[default]
    #[sea_orm(string_value = "best_effort")]
    BestEffort,
    /// A placement is refused when the node's reservations would exceed its
    /// capacity minus `system_reserved`.
    #[sea_orm(string_value = "enforced")]
    Enforced,
}

impl NodeResourcePolicy {
    pub fn as_str(&self) -> &'static str {
        match self {
            NodeResourcePolicy::BestEffort => "best_effort",
            NodeResourcePolicy::Enforced => "enforced",
        }
    }
}

/// The capacity Temps keeps for itself on a node: a scheduling deduction,
/// not a CPU priority (ADR-055 §5).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, FromJsonQueryResult,
)]
pub struct SystemReserved {
    pub cpu_millis: u32,
    pub memory_mb: u32,
}

impl SystemReserved {
    /// §5 estimate for a worker: agent, resolver, egress proxy.
    pub const WORKER: SystemReserved = SystemReserved {
        cpu_millis: 250,
        memory_mb: 256,
    };
    /// §5 estimate for a control plane with its database on the same host.
    pub const CONTROL_PLANE_LOCAL_DATABASE: SystemReserved = SystemReserved {
        cpu_millis: 500,
        memory_mb: 1024,
    };
    /// §5 estimate for a control plane with an external database.
    pub const CONTROL_PLANE_EXTERNAL_DATABASE: SystemReserved = SystemReserved {
        cpu_millis: 250,
        memory_mb: 512,
    };
    /// No reserve: what `NodeScheduler` assumed.
    pub const NONE: SystemReserved = SystemReserved {
        cpu_millis: 0,
        memory_mb: 0,
    };

    /// The default for a `nodes.role` value, as the migration backfills it.
    pub fn default_for_role(role: &str) -> SystemReserved {
        match role {
            "control" | "control-plane" => SystemReserved::CONTROL_PLANE_LOCAL_DATABASE,
            _ => SystemReserved::WORKER,
        }
    }
}

impl Default for SystemReserved {
    fn default() -> Self {
        SystemReserved::WORKER
    }
}

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "nodes")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub name: String,
    pub role: String,
    pub resource_policy: NodeResourcePolicy,
    #[sea_orm(column_type = "JsonBinary")]
    pub system_reserved: SystemReserved,
    pub updated_at: temps_core::DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_follow_the_adr_table() {
        assert_eq!(
            NodeResourcePolicy::default(),
            NodeResourcePolicy::BestEffort
        );
        assert_eq!(
            SystemReserved::default_for_role("worker"),
            SystemReserved::WORKER
        );
        assert_eq!(
            SystemReserved::default_for_role("control-plane"),
            SystemReserved::CONTROL_PLANE_LOCAL_DATABASE
        );
        let json = serde_json::to_value(SystemReserved::WORKER).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"cpu_millis": 250, "memory_mb": 256})
        );
        assert_eq!(
            serde_json::to_string(&NodeResourcePolicy::Enforced).unwrap(),
            "\"enforced\""
        );
    }
}
