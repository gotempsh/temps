// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! One allow/deny rule of a workload's network policy (ADR-052 §4).
//!
//! Keyed by workload identity, never by address. `origin = 'link'` rows are
//! owned by a `project_services` / `sandbox_services` row and are written by
//! database triggers; the API writes `operator` rows only.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "network_policy_rules")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// `deployment | sandbox | service | function | job`.
    pub subject_kind: String,
    /// Environment id for deployments; the row id for the other kinds.
    pub subject_id: i64,
    /// `egress | ingress`.
    pub direction: String,
    /// `service | workload | cidr | host`.
    pub peer_kind: String,
    /// Service id, `<kind>/<id>`, an address block, or a host pattern.
    pub peer_ref: String,
    pub port_start: Option<i32>,
    pub port_end: Option<i32>,
    /// `tcp | udp | any`.
    pub protocol: String,
    /// `allow | deny`.
    pub action: String,
    /// `link | operator | system`.
    pub origin: String,
    pub link_table: Option<String>,
    pub link_id: Option<i64>,
    pub description: Option<String>,
    pub created_by: Option<i32>,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
