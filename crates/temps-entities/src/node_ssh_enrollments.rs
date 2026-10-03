// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! One attempt at adding a server over SSH (ADR 048 D2c). The credentials
//! used are never stored.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "node_ssh_enrollments")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    /// Name the node registers under.
    pub name: String,
    /// The host as the operator entered it.
    pub host: String,
    /// `ip:port` connected to.
    pub ssh_address: String,
    pub ssh_user: String,
    /// `password`, `private_key` or `agent`.
    pub auth_method: String,
    /// `SHA256:…` host key the operator confirmed.
    pub host_key_fingerprint: String,
    /// The pairing run on the server.
    pub pairing_id: Option<i32>,
    /// `running`, `succeeded` or `failed`.
    pub status: String,
    /// What it is doing, or was doing when it stopped.
    pub step: String,
    /// What it did, with the servers' output.
    pub log: String,
    pub error: Option<String>,
    /// `service` (systemd unit) or `detached` (no service manager).
    pub agent_mode: Option<String>,
    pub node_id: Option<i32>,
    pub created_by_user_id: Option<i32>,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
    /// When the process running it last reported that it still is. A
    /// running row with an old heartbeat belongs to a process that is gone.
    pub heartbeat_at: DBDateTime,
    pub finished_at: Option<DBDateTime>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
