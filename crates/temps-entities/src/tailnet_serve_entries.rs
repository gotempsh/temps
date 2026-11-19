// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What a worker must configure in tailscaled's Serve config for one address
//! (ADR 065 D4). See `m20261026_000001_tailnet_serve`.
//!
//! One row per (node, address), shared by every run that needs it through
//! `tailnet_serve_refs`. Every change to a row's references, and the
//! last-reference check that removes it, happens in one transaction holding
//! the advisory lock on (node, address).

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

/// The worker should serve the entry.
pub const STATE_ACTIVE: &str = "active";
/// No run references it any more: the worker takes it off, then reports.
pub const STATE_REMOVING: &str = "removing";

/// The agent wrote the entry, at `applied_etag`.
pub const REPORT_APPLIED: &str = "applied";
/// The slot holds something the agent did not write; it wrote nothing.
pub const REPORT_CONFLICT: &str = "conflict";
/// The write failed, and may have landed anyway.
pub const REPORT_ERROR: &str = "error";

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "tailnet_serve_entries")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    /// No foreign key: a worker that left the cluster may still serve it.
    pub node_id: i32,
    /// The worker's name when the entry was made, for messages after it left.
    pub node_name: String,
    /// `temps_core::worker_ingress::private_address_id` of the address.
    pub address_id: String,
    pub hostname: String,
    /// `tailscale_serve` or `tailscale_service`.
    pub private_via: String,
    /// The `svc:` name, for a Service.
    pub service_name: Option<String>,
    /// The tailnet the Service is in, for its advisory lock.
    pub tailnet: Option<String>,
    /// The Service's addresses as Tailscale assigned them, a JSON array of
    /// strings: what the worker's inside check accepts for its name.
    pub service_addrs: Option<Json>,
    /// `active` or `removing`.
    pub state: String,
    /// What the agent last reported: `applied`, `conflict` or `error`.
    pub report: Option<String>,
    /// The agent's words with the report: what holds the slot, or the error.
    pub report_detail: Option<String>,
    /// The Serve config `Etag` the agent's write produced.
    pub applied_etag: Option<String>,
    pub reported_at: Option<DBDateTime>,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::tailnet_serve_refs::Entity")]
    Refs,
}

impl Related<super::tailnet_serve_refs::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Refs.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
