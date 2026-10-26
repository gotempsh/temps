// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Direct worker ingress settings and what the agent reported (ADR 058 D7).

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "node_public_ingress")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub node_id: i32,
    /// Local address to bind, for a `delegated` worker. `None` means
    /// automatic: the address of the interface holding the default route.
    pub listen_address: Option<String>,
    pub http_port: i32,
    pub https_port: i32,
    /// What DNS points at and what the reachability check dials. `None`
    /// means the listen address, when that is publicly routable.
    pub public_address: Option<String>,
    /// Host addresses the agent last reported, `[{name, address, default_route}]`.
    pub interfaces: Option<Json>,
    /// What the agent actually bound, `{http, https}`, or `None`.
    pub listening: Option<Json>,
    /// What the worker allows: `off`, `fixed`, `delegated` or `private`, as it last
    /// reported. `None` from agents that predate ingress modes.
    pub mode: Option<String>,
    /// Outcome of the last reachability check.
    pub last_check: Option<Json>,
    /// The worker's last inside checks of its private addresses (ADR 064
    /// D7), `[{address_id, hostname, outcome, message, ...}]`.
    #[serde(default)]
    pub private_checks: Option<Json>,
    pub checked_at: Option<DBDateTime>,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
