// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Recent queries answered by the internal DNS resolvers (ADR-095).
//!
//! Internal-zone names only: a forwarded query is logged only when it
//! failed, with `qname` NULL. Kept for 24 hours and at most 50,000 rows.
//! `project_id`, `environment_id` and `service_id` attribute the asking
//! container when its address matched a published endpoint on that node;
//! they are not foreign keys (the rows age out within a day).

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "dns_query_log")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// The node whose resolver answered; 0 is the control plane.
    pub node_id: i32,
    /// When it was answered, by the control plane's clock.
    pub observed_at: DBDateTime,
    pub qname: Option<String>,
    pub qtype: String,
    /// `internal` or `external`.
    pub zone: String,
    /// `noerror`, `nodata`, `nxdomain`, `servfail`, `refused` or `error`.
    pub outcome: String,
    pub latency_us: i64,
    pub answer_count: i32,
    pub source_ip: String,
    pub project_id: Option<i32>,
    pub environment_id: Option<i32>,
    pub service_id: Option<i32>,
    pub created_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
