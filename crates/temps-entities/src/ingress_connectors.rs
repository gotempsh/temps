// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! An ingress connector (ADR 058): one tunnel at its provider. See
//! `m20261020_000004_ingress_connectors`.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "ingress_connectors")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub name: String,
    /// Tunnel provider kind, e.g. `cloudflare_tunnel`.
    pub provider: String,
    /// The DNS provider whose credential manages the tunnel and its records.
    pub dns_provider_id: Option<i32>,
    /// The managed zone hostnames are published under.
    pub domain: String,
    /// The tunnel's id at the provider, once created.
    pub external_id: Option<String>,
    /// Provider-specific settings (account id, tunnel name, ...).
    pub settings: Json,
    /// The secret a node runs the connector with, encrypted.
    #[serde(skip_serializing)]
    pub secret_encrypted: Option<String>,
    /// Loopback port of the node's tunnel origin listener.
    pub edge_port: i32,
    /// Publish every worker-served hostname under `domain` (true), or only
    /// the hostnames chosen for this tunnel (false).
    pub auto_publish: bool,
    /// `pending`, `ready` or `error`.
    pub status: String,
    pub last_error: Option<String>,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
